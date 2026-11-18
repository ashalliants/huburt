//! Slack: asks the same "client.counts" endpoint the web app uses. The workspace token is read from the open
//! Slack page and kept in memory only (never written to disk or logged); the login cookie comes from the app.
use crate::http::{host_of, Http, HttpRequest, UreqHttp};
use crate::types::{Credentials, NotificationItem, PollResult, Poller};
use crate::{log, MAX_BANNERS};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const MUTED_REFRESH_EVERY: Duration = Duration::from_secs(600);
const AUTH_ERRORS: [&str; 5] = [
    "invalid_auth",
    "not_authed",
    "token_revoked",
    "token_expired",
    "account_inactive",
];
const HARVEST_JS: &str = "(function () { try {
  var c = JSON.parse(localStorage.getItem('localConfig_v2') || '{}'), out = [];
  for (var k in (c.teams || {})) { var t = c.teams[k]; if (t && t.token) out.push({ id: k, url: t.url || '', token: t.token }); }
  return JSON.stringify(out);
} catch (e) { return '[]'; } })();";

#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    pub id: String,
    pub url: String,
    token: String,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Channel,
    Mpim,
    Im,
}

struct Convo {
    id: String,
    kind: Kind,
    mentions: i64,
    last_read: Option<String>,
}

#[derive(Default)]
struct TeamCounts {
    mentions: i64,
    quiet: i64,
    items: Vec<NotificationItem>,
}

pub struct SlackPoller {
    http: Box<dyn Http>,
    creds: Credentials,
    teams: Vec<Team>,
    last_total: Option<i64>,
    logged_no_creds: bool,
    logged_shape: bool,
    /// per workspace token: muted channel ids and when they were fetched
    muted: HashMap<String, (HashSet<String>, Instant)>,
    /// "workspace/conversation" -> mentions last time
    prev_mentions: HashMap<String, i64>,
    /// first poll per workspace only records what's already unread
    primed_teams: HashSet<String>,
    /// "team/ID" -> display name
    names: HashMap<String, String>,
    /// Test hook (HUB_TEST_SLACK_REPLAY): pretend already-unread mentions just arrived.
    replay_existing: bool,
    signed_out: bool,
    rejections: u32,
}

fn enc(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for b in t.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `api/<path>` resolved against the workspace URL (like Swift's URL(string:relativeTo:)).
fn api_url(base: &str, path: &str) -> String {
    let after_scheme = base.find("://").map_or(0, |i| i + 3);
    match base[after_scheme..].rfind('/') {
        Some(i) => format!("{}api/{path}", &base[..after_scheme + i + 1]),
        None => format!("{base}/api/{path}"),
    }
}

fn matches(pattern: &str, text: &str) -> Vec<String> {
    Regex::new(pattern)
        .map(|re| re.captures_iter(text).map(|c| c[1].to_string()).collect())
        .unwrap_or_default()
}

fn unique(v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    v.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

/// Turns Slack's message markup (<@U123>, <#C123|name>, <https://x|label>, &amp;) into plain text.
fn clean(text: &str, user_name: impl Fn(&str) -> Option<String>) -> String {
    let mut t = text.to_string();
    for id in unique(matches(r"<@([UW][A-Z0-9]+)(?:\|[^>]*)?>", &t)) {
        t = t.replace(
            &format!("<@{id}>"),
            &format!("@{}", user_name(&id).unwrap_or_else(|| "someone".into())),
        );
    }
    let subs = [
        (r"<@[UW][A-Z0-9]+(?:\|([^>]*))?>", "@${1}"),
        (r"<#C[A-Z0-9]+\|([^>]*)>", "#${1}"),
        (r"<!(here|channel|everyone)(?:\|[^>]*)?>", "@${1}"),
        (r"<(?:https?|mailto):[^|>]*\|([^>]+)>", "${1}"),
        (r"<((?:https?|mailto):[^>]+)>", "${1}"),
    ];
    for (pat, rep) in subs {
        if let Ok(re) = Regex::new(pat) {
            t = re.replace_all(&t, rep).into_owned();
        }
    }
    t.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

/// Unmuted channel with new content and no mention. Slack also flags channels you never joined (last_read of zero);
/// those aren't unread to you, so they're ignored.
fn is_quiet_unread(d: &Value, muted: &HashSet<String>) -> bool {
    if d["has_unreads"].as_bool() != Some(true)
        || d["mention_count"].as_i64().unwrap_or(0) != 0
        || muted.contains(d["id"].as_str().unwrap_or(""))
    {
        return false;
    }
    d["last_read"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
        > 0.0
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

impl SlackPoller {
    pub fn new() -> Self {
        Self::with_http(Box::new(UreqHttp::new()))
    }

    pub fn with_http(http: Box<dyn Http>) -> Self {
        Self {
            http,
            creds: Credentials::default(),
            teams: vec![],
            last_total: None,
            logged_no_creds: false,
            logged_shape: false,
            muted: HashMap::new(),
            prev_mentions: HashMap::new(),
            primed_teams: HashSet::new(),
            names: HashMap::new(),
            replay_existing: std::env::var_os("HUB_TEST_SLACK_REPLAY").is_some(),
            signed_out: false,
            rejections: 0,
        }
    }

    #[cfg(test)]
    pub fn teams(&self) -> &[Team] {
        &self.teams
    }

    fn cookie_header(&self) -> Option<String> {
        self.creds.cookie_header("slack.com")
    }

    fn call(&self, path: &str, team: &Team, cookie: &str, body: &str) -> Option<Value> {
        self.call_raw(path, team, cookie, body)
            .ok()
            .filter(|j| j["ok"].as_bool() == Some(true))
    }

    /// Err: transport failure or non-JSON-object response (message has no secrets).
    fn call_raw(&self, path: &str, team: &Team, cookie: &str, body: &str) -> Result<Value, String> {
        let req = HttpRequest {
            method: "POST".into(),
            url: api_url(&team.url, path),
            headers: vec![
                ("Cookie".into(), cookie.to_string()),
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ),
                ("Origin".into(), "https://app.slack.com".into()),
            ],
            body: Some(format!("token={}{body}", enc(&team.token))),
        };
        let res = self
            .http
            .request(req)
            .map_err(|e| format!("status=-1 error={e}"))?;
        match serde_json::from_str::<Value>(&res.body) {
            Ok(v) if v.is_object() => Ok(v),
            _ => Err(format!("status={} error=none", res.status)),
        }
    }

    fn muted_channels(&mut self, team: &Team, cookie: &str) -> HashSet<String> {
        if let Some((ids, fetched)) = self.muted.get(&team.token) {
            if fetched.elapsed() < MUTED_REFRESH_EVERY {
                return ids.clone();
            }
        }
        let json = self.call("users.prefs.get", team, cookie, "");
        let mut ids = self
            .muted
            .get(&team.token)
            .map(|m| m.0.clone())
            .unwrap_or_default(); // keep the old list if this fails
        if let Some(prefs) = json
            .as_ref()
            .and_then(|j| j.get("prefs"))
            .filter(|p| p.is_object())
        {
            let legacy: HashSet<String> = prefs["muted_channels"]
                .as_str()
                .unwrap_or("")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            // Newer Slack keeps per-channel muting inside a JSON string: {"channels": {"C123": {"muted": true}}}
            let mut modern = HashSet::new();
            if let Some(obj) = prefs["all_notifications_prefs"]
                .as_str()
                .and_then(|r| serde_json::from_str::<Value>(r).ok())
            {
                if let Some(chans) = obj["channels"].as_object() {
                    for (id, v) in chans {
                        if v["muted"].as_bool() == Some(true) {
                            modern.insert(id.clone());
                        }
                    }
                }
            }
            log(&format!(
                "slack: muted channels legacy={} modern={}",
                legacy.len(),
                modern.len()
            ));
            ids = legacy.union(&modern).cloned().collect();
        } else {
            log("slack: couldn't read muted channels; muted ones may show in the quiet indicator");
        }
        self.muted
            .insert(team.token.clone(), (ids.clone(), Instant::now()));
        ids
    }

    /// Returns the team's counts (None if it couldn't be read) and whether Slack rejected the token.
    fn counts(&mut self, team: &Team, cookie: &str) -> (Option<TeamCounts>, bool) {
        let muted = self.muted_channels(team, cookie);
        let json = match self.call_raw(
            "client.counts",
            team,
            cookie,
            "&thread_counts_by_channel=true&include_file_channels=true",
        ) {
            Ok(j) => j,
            Err(e) => {
                log(&format!("slack poll: bad response {e}"));
                return (None, false);
            }
        };
        if json["ok"].as_bool() != Some(true) {
            let err = json["error"].as_str().unwrap_or("unknown");
            log(&format!("slack poll: api error={err}"));
            return (None, AUTH_ERRORS.contains(&err));
        }
        (Some(self.process(&json, team, cookie, &muted)), false)
    }

    fn process(
        &mut self,
        json: &Value,
        team: &Team,
        cookie: &str,
        muted: &HashSet<String>,
    ) -> TeamCounts {
        let list = |key: &str| -> Vec<Value> { json[key].as_array().cloned().unwrap_or_default() };
        let convos = |key: &str, kind: Kind| -> Vec<Convo> {
            list(key)
                .iter()
                .filter_map(|d| {
                    Some(Convo {
                        id: d["id"].as_str()?.to_string(),
                        kind,
                        mentions: d["mention_count"].as_i64().unwrap_or(0),
                        last_read: d["last_read"].as_str().map(String::from),
                    })
                })
                .collect()
        };
        let mut all = convos("channels", Kind::Channel);
        all.extend(convos("mpims", Kind::Mpim));
        all.extend(convos("ims", Kind::Im));
        let thread_info = &json["threads"];
        let thread_mentions = thread_info["mention_count"].as_i64().unwrap_or(0);
        let quiet_channels = list("channels")
            .iter()
            .filter(|d| is_quiet_unread(d, muted))
            .count() as i64;
        let quiet_threads =
            i64::from(thread_info["has_unreads"].as_bool() == Some(true) && thread_mentions == 0);
        let mut result = TeamCounts {
            mentions: all.iter().map(|c| c.mentions).sum::<i64>() + thread_mentions,
            quiet: quiet_channels + quiet_threads,
            items: vec![],
        };

        if !self.logged_shape {
            self.logged_shape = true;
            let raw = list("channels")
                .iter()
                .filter(|d| d["has_unreads"].as_bool() == Some(true))
                .count();
            log(&format!(
                "slack counts shape: channels={} ims={} mpims={} unreadChannels(raw)={raw} unreadChannels(unmuted)={quiet_channels}",
                list("channels").len(),
                list("ims").len(),
                list("mpims").len()
            ));
        }

        // Which conversations just gained mentions? (The very first poll only records the starting point.)
        let host = if team.id.is_empty() {
            host_of(&team.url)
        } else {
            team.id.clone()
        };
        let primed = self.primed_teams.contains(&host) || self.replay_existing;
        self.primed_teams.insert(host.clone());
        let prefix = format!("{host}/");
        let prev_before = self.prev_mentions.clone();
        let rising: Vec<&Convo> = all
            .iter()
            .filter(|c| {
                c.mentions
                    > *self
                        .prev_mentions
                        .get(&format!("{prefix}{}", c.id))
                        .unwrap_or(&0)
            })
            .collect();
        self.prev_mentions.retain(|k, _| !k.starts_with(&prefix));
        for c in all.iter().filter(|c| c.mentions > 0) {
            self.prev_mentions
                .insert(format!("{prefix}{}", c.id), c.mentions);
        }

        if !primed || rising.is_empty() {
            return result;
        }
        // A burst: one summary instead of a banner per conversation
        if rising.len() > MAX_BANNERS {
            let n: i64 = rising
                .iter()
                .map(|c| {
                    1.max(
                        c.mentions
                            - prev_before
                                .get(&format!("{prefix}{}", c.id))
                                .copied()
                                .unwrap_or(0),
                    )
                })
                .sum();
            log(&format!(
                "slack: {} conversations with new mentions, sending one summary",
                rising.len()
            ));
            result.items = vec![NotificationItem {
                title: "Slack".into(),
                body: format!("{n} new direct messages or mentions"),
                ..Default::default()
            }];
            return result;
        }
        for c in rising.iter().take(MAX_BANNERS) {
            if let Some(item) = self.describe(c, team, cookie) {
                result.items.push(item);
            }
        }
        log(&format!(
            "slack: described {} of {} new mention(s)",
            result.items.len(),
            rising.len().min(3)
        ));
        result
    }

    fn user_name(&mut self, id: &str, team: &Team, cookie: &str) -> Option<String> {
        let key = format!("{}/{id}", team.id);
        if let Some(n) = self.names.get(&key) {
            return Some(n.clone());
        }
        let json = self.call("users.info", team, cookie, &format!("&user={id}"));
        let user = json.as_ref().map(|j| &j["user"]);
        let profile = user.map(|u| &u["profile"]);
        let name = [
            profile.map(|p| &p["display_name"]),
            profile.map(|p| &p["real_name"]),
            user.map(|u| &u["real_name"]),
            user.map(|u| &u["name"]),
        ]
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .find(|s| !s.is_empty())
        .map(String::from);
        if let Some(n) = &name {
            self.names.insert(key, n.clone());
        }
        name
    }

    fn channel_name(&mut self, id: &str, team: &Team, cookie: &str) -> Option<String> {
        let key = format!("{}/{id}", team.id);
        if let Some(n) = self.names.get(&key) {
            return Some(n.clone());
        }
        let json = self.call(
            "conversations.info",
            team,
            cookie,
            &format!("&channel={id}"),
        )?;
        let name = json["channel"]["name"].as_str()?.to_string();
        self.names.insert(key, name.clone());
        Some(name)
    }

    /// Fetches the newest message in a conversation and turns it into "Sender in #channel: text".
    fn describe(&mut self, c: &Convo, team: &Team, cookie: &str) -> Option<NotificationItem> {
        let oldest = c
            .last_read
            .as_ref()
            .map(|l| format!("&oldest={l}"))
            .unwrap_or_default();
        let json = self.call(
            "conversations.history",
            team,
            cookie,
            &format!("&channel={}&limit=5{oldest}", c.id),
        )?;
        let m = json["messages"]
            .as_array()?
            .iter()
            .find(|m| {
                !matches!(
                    m["subtype"].as_str().unwrap_or(""),
                    "channel_join" | "channel_leave"
                )
            })?
            .clone();
        let raw = m["text"].as_str().unwrap_or("").to_string();
        let has_file = m["files"].as_array().is_some_and(|f| !f.is_empty());
        let bot_name = m["bot_profile"]["name"]
            .as_str()
            .or_else(|| m["username"].as_str())
            .map(String::from);

        let mut sender = bot_name.unwrap_or_else(|| "Someone".into());
        if let Some(sid) = m["user"].as_str() {
            if let Some(n) = self.user_name(sid, team, cookie) {
                sender = n;
            }
        }
        let channel = if c.kind == Kind::Channel {
            self.channel_name(&c.id, team, cookie)
        } else {
            None
        };
        for id in unique(matches(r"<@([UW][A-Z0-9]+)", &raw))
            .into_iter()
            .take(4)
        {
            self.user_name(&id, team, cookie);
        }
        let names = &self.names;
        let mut text = clean(&raw, |id| names.get(&format!("{}/{id}", team.id)).cloned());
        if text.is_empty() {
            text = if has_file {
                "Sent an attachment"
            } else {
                "New message"
            }
            .into();
        }
        if text.chars().count() > 160 {
            text = text.chars().take(157).collect::<String>() + "…";
        }
        let title = match c.kind {
            Kind::Im => sender,
            Kind::Mpim => format!("{sender} in a group chat"),
            Kind::Channel => channel.map_or(sender.clone(), |ch| format!("{sender} in #{ch}")),
        };
        let url = (!team.id.is_empty())
            .then(|| format!("https://app.slack.com/client/{}/{}", team.id, c.id));
        let mut info = HashMap::from([
            ("team".to_string(), team.id.clone()),
            ("channel".to_string(), c.id.clone()),
            ("ts".to_string(), m["ts"].as_str().unwrap_or("").to_string()),
            (
                "kind".to_string(),
                if c.kind == Kind::Channel {
                    "channel"
                } else {
                    "dm"
                }
                .to_string(),
            ),
        ]);
        if let Some(t) = m["thread_ts"].as_str() {
            info.insert("thread".into(), t.to_string());
        }
        Some(NotificationItem {
            title,
            body: text,
            url,
            category: Some("slack.message".into()),
            info,
        })
    }
}

impl Default for SlackPoller {
    fn default() -> Self {
        Self::new()
    }
}

impl Poller for SlackPoller {
    fn poll(&mut self) -> Option<PollResult> {
        if self.teams.is_empty() {
            if !self.logged_no_creds {
                log("slack poll: no credentials yet (open the Slack tab once)");
                self.logged_no_creds = true;
            }
            return None;
        }
        let Some(cookie) = self.cookie_header() else {
            log("slack poll: no cookies");
            self.signed_out = true;
            return None;
        };
        let teams = self.teams.clone();
        let mut results = vec![];
        let mut invalid = false;
        for team in &teams {
            let (r, auth_failed) = self.counts(team, &cookie);
            results.extend(r);
            invalid |= auth_failed;
        }
        if invalid {
            self.teams.clear();
            self.logged_no_creds = false;
            self.rejections += 1;
            if self.rejections >= 2 {
                self.signed_out = true; // a fresh token from the page was rejected too
            }
            log("slack poll: token rejected, will re-read it from the page");
        }
        if results.is_empty() {
            return None;
        }
        self.rejections = 0;
        self.signed_out = false;
        let total: i64 = results.iter().map(|r| r.mentions).sum();
        let quiet: i64 = results.iter().map(|r| r.quiet).sum();
        let mut items: Vec<NotificationItem> = results.into_iter().flat_map(|r| r.items).collect();
        // Couldn't describe what arrived (e.g. a thread reply): fall back to a plain count
        if let (true, Some(last)) = (items.is_empty(), self.last_total) {
            if total > last {
                let n = total - last;
                items = vec![NotificationItem {
                    title: "Slack".into(),
                    body: format!(
                        "{n} new direct message{} or mention{}",
                        plural(n),
                        plural(n)
                    ),
                    ..Default::default()
                }];
            }
        }
        self.last_total = Some(total);
        log(&format!(
            "slack poll: ok unread={total} quiet={quiet} notifications={}",
            items.len()
        ));
        Some(PollResult {
            unread: total as u32,
            new_items: items,
            quiet_unread: quiet as u32,
        })
    }

    fn needs_credentials(&self) -> bool {
        self.teams.is_empty()
    }

    fn signed_out(&self) -> bool {
        self.signed_out
    }

    fn set_credentials(&mut self, creds: Credentials) {
        self.creds = creds.clone();
        let Some(data) = creds.page_data.as_deref() else {
            return;
        };
        let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let found: Vec<Team> = arr
            .iter()
            .filter_map(|d| {
                let token = d["token"].as_str().filter(|t| t.starts_with("xoxc-"))?;
                let url = d["url"]
                    .as_str()
                    .filter(|u| u.starts_with("http://") || u.starts_with("https://"));
                Some(Team {
                    id: d["id"].as_str().unwrap_or("").to_string(),
                    url: url.unwrap_or("https://slack.com/").to_string(),
                    token: token.to_string(),
                })
            })
            .collect();
        let tokens = |ts: &[Team]| ts.iter().map(|t| t.token.clone()).collect::<Vec<_>>();
        if !found.is_empty() && tokens(&found) != tokens(&self.teams) {
            log(&format!(
                "slack: picked up credentials for {} workspace(s)",
                found.len()
            ));
            self.teams = found;
        }
    }

    fn harvest_js(&self) -> Option<&'static str> {
        Some(HARVEST_JS)
    }

    fn reset(&mut self) {
        self.primed_teams.clear();
        self.last_total = None;
        self.prev_mentions.clear();
    }

    fn perform(
        &mut self,
        action: &str,
        info: &HashMap<String, String>,
        text: Option<&str>,
    ) -> bool {
        let Some(channel) = info.get("channel").filter(|c| !c.is_empty()) else {
            return false;
        };
        let Some(team) = self
            .teams
            .iter()
            .find(|t| Some(&t.id) == info.get("team"))
            .or(self.teams.first())
            .cloned()
        else {
            return false;
        };
        let Some(cookie) = self.cookie_header() else {
            return false;
        };
        match action {
            "markRead" => {
                let Some(ts) = info.get("ts").filter(|t| !t.is_empty()) else {
                    return false;
                };
                self.call(
                    "conversations.mark",
                    &team,
                    &cookie,
                    &format!("&channel={}&ts={}", enc(channel), enc(ts)),
                )
                .is_some()
            }
            "reply" => {
                let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
                    return false;
                };
                // Channels: reply in a thread under the message you were pinged in (less noise). Chats: just reply.
                let mut body = format!("&channel={}&text={}", enc(channel), enc(text));
                if info.get("kind").map(String::as_str) == Some("channel") {
                    if let Some(thread) = info
                        .get("thread")
                        .or(info.get("ts"))
                        .filter(|t| !t.is_empty())
                    {
                        body.push_str(&format!("&thread_ts={}", enc(thread)));
                    }
                }
                self.call("chat.postMessage", &team, &cookie, &body)
                    .is_some()
            }
            _ => false,
        }
    }

    /// Tells Slack you're active ("auto" clears a manual away) and logs what Slack now reports for you.
    /// False if there's no token yet.
    fn keep_online(&mut self) -> bool {
        if self.teams.is_empty() {
            return false;
        }
        let Some(cookie) = self.cookie_header() else {
            return true;
        };
        let ok = |o: &Option<Value>| if o.is_some() { "ok" } else { "failed" };
        for team in self.teams.clone() {
            let set = self.call("users.setPresence", &team, &cookie, "&presence=auto");
            let active = self.call("users.setActive", &team, &cookie, "");
            let head = format!(
                "slack presence: setPresence {}, setActive {}",
                ok(&set),
                ok(&active)
            );
            let me = self.call("auth.test", &team, &cookie, "");
            let Some(id) = me.as_ref().and_then(|m| m["user_id"].as_str()) else {
                log(&head);
                continue;
            };
            let p = self.call("users.getPresence", &team, &cookie, &format!("&user={id}"));
            let now = p
                .as_ref()
                .and_then(|p| p["presence"].as_str())
                .unwrap_or("unknown");
            log(&format!("{head}, now {now}"));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::{ok, Log, MockHttp};
    use crate::types::Cookie;
    use std::sync::{Arc, Mutex};

    type Routes = Arc<Mutex<HashMap<String, String>>>;

    const TOKEN: &str = "xoxc-123-abc/def";
    const NOW: &str = "1700000000.000100";

    fn creds() -> Credentials {
        Credentials {
            cookies: vec![
                Cookie {
                    name: "d".into(),
                    value: "cookieval".into(),
                    domain: ".slack.com".into(),
                },
                Cookie {
                    name: "g".into(),
                    value: "no".into(),
                    domain: ".google.com".into(),
                },
            ],
            page_data: Some(format!(
                r#"[{{"id":"T1","url":"https://acme.slack.com/","token":"{TOKEN}"}},{{"id":"T2","url":"","token":"not-a-token"}}]"#
            )),
        }
    }

    /// A poller wired to canned responses keyed by API method; unrouted methods answer `ok:false`.
    fn setup() -> (SlackPoller, Routes, Log) {
        let routes: Routes = Arc::default();
        let r = routes.clone();
        let (m, log) = MockHttp::new(move |req| {
            let method = req.url.rsplit("/api/").next().unwrap().to_string();
            let body = r
                .lock()
                .unwrap()
                .get(&method)
                .cloned()
                .unwrap_or_else(|| r#"{"ok":false,"error":"unrouted"}"#.into());
            ok(&req.url, &body)
        });
        let mut p = SlackPoller::with_http(Box::new(m));
        p.set_credentials(creds());
        (p, routes, log)
    }
    fn route(r: &Routes, method: &str, body: &str) {
        r.lock().unwrap().insert(method.into(), body.into());
    }
    fn methods(log: &Log) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .map(|r| r.url.rsplit("/api/").next().unwrap().to_string())
            .collect()
    }
    fn counts_json(im_mentions: u32, extra: &str) -> String {
        format!(
            r#"{{"ok":true,"channels":[{extra}],"mpims":[],"ims":[{{"id":"D1","mention_count":{im_mentions},"has_unreads":{},"last_read":"{NOW}"}}],"threads":{{"has_unreads":false,"mention_count":0}}}}"#,
            im_mentions > 0
        )
    }
    fn history(text: &str, user: &str) -> String {
        format!(
            r#"{{"ok":true,"messages":[{{"type":"message","user":"{user}","text":"{text}","ts":"1700000100.000200"}}]}}"#
        )
    }
    fn user(name: &str) -> String {
        format!(
            r#"{{"ok":true,"user":{{"name":"login","real_name":"Real {name}","profile":{{"display_name":"{name}","real_name":"Real {name}"}}}}}}"#
        )
    }

    #[test]
    fn credentials_only_accept_xoxc_tokens() {
        let mut p = SlackPoller::with_http(Box::new(MockHttp::new(|_| ok("", "")).0));
        assert!(p.needs_credentials());
        assert!(p.harvest_js().unwrap().contains("localConfig_v2"));
        p.set_credentials(Credentials {
            cookies: vec![],
            page_data: Some("garbage".into()),
        });
        assert!(p.needs_credentials());
        p.set_credentials(creds());
        assert!(!p.needs_credentials());
        assert_eq!(p.teams().len(), 1);
        assert_eq!(p.teams()[0].id, "T1");
        // Empty / bad URL falls back to slack.com.
        p.set_credentials(Credentials {
            cookies: vec![],
            page_data: Some(r#"[{"id":"T3","url":"","token":"xoxc-9"}]"#.into()),
        });
        assert_eq!(p.teams()[0].url, "https://slack.com/");
    }

    #[test]
    fn no_credentials_or_cookies() {
        let (m, log) = MockHttp::new(|_| ok("", ""));
        let mut p = SlackPoller::with_http(Box::new(m));
        assert!(p.poll().is_none());
        assert!(!p.signed_out());
        assert!(!p.keep_online());
        p.set_credentials(Credentials {
            cookies: vec![],
            page_data: Some(
                r#"[{"id":"T1","url":"https://a.slack.com/","token":"xoxc-1"}]"#.into(),
            ),
        });
        assert!(p.poll().is_none());
        assert!(p.signed_out());
        assert!(log.lock().unwrap().is_empty());
        assert!(!p.perform(
            "markRead",
            &HashMap::from([("channel".into(), "C1".into()), ("ts".into(), "1".into())]),
            None
        ));
    }

    #[test]
    fn counts_mentions_and_quiet_unread() {
        let (mut p, r, log) = setup();
        route(
            &r,
            "users.prefs.get",
            r#"{"ok":true,"prefs":{"muted_channels":"C3,","all_notifications_prefs":"{\"channels\":{\"C4\":{\"muted\":true},\"C5\":{\"muted\":false}}}"}}"#,
        );
        let chans = [
            r#"{"id":"C1","has_unreads":true,"mention_count":0,"last_read":"1700000000.000100"}"#, // quiet
            r#"{"id":"C2","has_unreads":true,"mention_count":0,"last_read":"0000000000.000000"}"#, // never joined
            r#"{"id":"C3","has_unreads":true,"mention_count":0,"last_read":"1700000000.000100"}"#, // legacy muted
            r#"{"id":"C4","has_unreads":true,"mention_count":0,"last_read":"1700000000.000100"}"#, // modern muted
            r#"{"id":"C5","has_unreads":true,"mention_count":0,"last_read":"1700000000.000100"}"#, // quiet (muted:false)
            r#"{"id":"C6","has_unreads":false,"mention_count":0,"last_read":"1700000000.000100"}"#, // read
            r#"{"id":"C7","has_unreads":true,"mention_count":2,"last_read":"1700000000.000100"}"#, // mentions
            r#"{"id":"C8","has_unreads":true,"mention_count":0}"#, // no last_read
        ]
        .join(",");
        let json = counts_json(3, &chans).replace(
            r#""threads":{"has_unreads":false,"mention_count":0}"#,
            r#""threads":{"has_unreads":true,"mention_count":0}"#,
        );
        route(&r, "client.counts", &json);
        let res = p.poll().unwrap();
        assert_eq!(res.unread, 5); // 2 + 3 DM
        assert_eq!(res.quiet_unread, 3); // C1, C5, quiet threads
        assert!(res.new_items.is_empty(), "first poll primes");
        // muted list fetched once, then cached
        p.poll().unwrap();
        let m = methods(&log);
        assert_eq!(m.iter().filter(|x| *x == "users.prefs.get").count(), 1);
        assert_eq!(m.iter().filter(|x| *x == "client.counts").count(), 2);
    }

    #[test]
    fn thread_mentions_count_and_suppress_quiet_thread() {
        let (mut p, r, _) = setup();
        route(
            &r,
            "client.counts",
            r#"{"ok":true,"channels":[],"mpims":[],"ims":[],"threads":{"has_unreads":true,"mention_count":4}}"#,
        );
        let res = p.poll().unwrap();
        assert_eq!((res.unread, res.quiet_unread), (4, 0));
    }

    #[test]
    fn request_is_built_like_the_web_app() {
        let (mut p, r, log) = setup();
        route(&r, "client.counts", &counts_json(0, ""));
        p.poll().unwrap();
        let reqs = log.lock().unwrap();
        let c = reqs
            .iter()
            .find(|q| q.url.ends_with("/api/client.counts"))
            .unwrap();
        assert_eq!(c.url, "https://acme.slack.com/api/client.counts");
        assert_eq!(c.method, "POST");
        assert_eq!(c.header("Origin"), Some("https://app.slack.com"));
        assert_eq!(
            c.header("Content-Type"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(c.header("Cookie"), Some("d=cookieval"));
        assert_eq!(
            c.body.as_deref(),
            Some(
                "token=xoxc-123-abc%2Fdef&thread_counts_by_channel=true&include_file_channels=true"
            )
        );
    }

    #[test]
    fn mention_rising_notifies_once_with_im_title() {
        let (mut p, r, log) = setup();
        route(&r, "client.counts", &counts_json(0, ""));
        assert!(p.poll().unwrap().new_items.is_empty());
        route(&r, "client.counts", &counts_json(1, ""));
        route(
            &r,
            "conversations.history",
            &history(
                "Hello &lt;3 <@U9> see <#C1|general> <!here> <https://x.io|docs> <https://y.io>",
                "U2",
            ),
        );
        route(&r, "users.info", &user("Ann"));
        let res = p.poll().unwrap();
        assert_eq!(res.unread, 1);
        assert_eq!(res.new_items.len(), 1);
        let it = &res.new_items[0];
        assert_eq!(it.title, "Ann");
        assert_eq!(
            it.body,
            "Hello <3 @Ann see #general @here docs https://y.io"
        );
        assert_eq!(
            it.url.as_deref(),
            Some("https://app.slack.com/client/T1/D1")
        );
        assert_eq!(it.category.as_deref(), Some("slack.message"));
        assert_eq!(it.info["team"], "T1");
        assert_eq!(it.info["channel"], "D1");
        assert_eq!(it.info["ts"], "1700000100.000200");
        assert_eq!(it.info["kind"], "dm");
        assert!(!it.info.contains_key("thread"));
        // history was fetched with the last_read as oldest
        let hist = log
            .lock()
            .unwrap()
            .iter()
            .find(|q| q.url.ends_with("conversations.history"))
            .cloned()
            .unwrap();
        assert!(hist
            .body
            .unwrap()
            .ends_with(&format!("&channel=D1&limit=5&oldest={NOW}")));
        // same count again: nothing new; names are cached (one users.info per user id)
        assert!(p.poll().unwrap().new_items.is_empty());
        assert_eq!(
            methods(&log).iter().filter(|x| *x == "users.info").count(),
            2
        ); // U2 and U9
    }

    #[test]
    fn titles_for_channel_and_group_chats() {
        let (mut p, r, _) = setup();
        let base = |m: u32, k: &str| {
            format!(
                r#"{{"ok":true,"channels":[{}],"mpims":[{}],"ims":[],"threads":{{}}}}"#,
                if k == "c" {
                    format!(r#"{{"id":"C1","mention_count":{m},"last_read":"{NOW}"}}"#)
                } else {
                    String::new()
                },
                if k == "m" {
                    format!(r#"{{"id":"G1","mention_count":{m},"last_read":"{NOW}"}}"#)
                } else {
                    String::new()
                },
            )
        };
        route(&r, "client.counts", &base(0, "c"));
        p.poll().unwrap();
        route(&r, "client.counts", &base(1, "c"));
        route(
            &r,
            "conversations.history",
            r#"{"ok":true,"messages":[{"subtype":"channel_join","user":"U5","text":"joined","ts":"1"},{"user":"U2","text":"ping","ts":"2","thread_ts":"1.5","files":[{"id":"F"}]}]}"#,
        );
        route(&r, "users.info", &user("Ann"));
        route(
            &r,
            "conversations.info",
            r#"{"ok":true,"channel":{"name":"general"}}"#,
        );
        let it = p.poll().unwrap().new_items.remove(0);
        assert_eq!(it.title, "Ann in #general");
        assert_eq!(it.info["kind"], "channel");
        assert_eq!(it.info["thread"], "1.5");
        assert_eq!(it.info["ts"], "2");

        // group chat, and an attachment-only message from a bot
        route(&r, "client.counts", &base(0, "m"));
        p.poll().unwrap();
        route(&r, "client.counts", &base(2, "m"));
        route(
            &r,
            "conversations.history",
            r#"{"ok":true,"messages":[{"bot_profile":{"name":"Deploybot"},"text":"","files":[{"id":"F"}],"ts":"3"}]}"#,
        );
        let it = p.poll().unwrap().new_items.remove(0);
        assert_eq!(it.title, "Deploybot in a group chat");
        assert_eq!(it.body, "Sent an attachment");
    }

    #[test]
    fn channel_title_falls_back_to_sender_and_long_text_truncates() {
        let (mut p, r, _) = setup();
        let c = |m: u32| {
            format!(
                r#"{{"ok":true,"channels":[{{"id":"C1","mention_count":{m},"last_read":"{NOW}"}}],"mpims":[],"ims":[]}}"#
            )
        };
        route(&r, "client.counts", &c(0));
        p.poll().unwrap();
        route(&r, "client.counts", &c(1));
        route(
            &r,
            "conversations.history",
            &history(&"x".repeat(200), "U2"),
        );
        // users.info and conversations.info fail
        let it = p.poll().unwrap().new_items.remove(0);
        assert_eq!(it.title, "Someone");
        assert_eq!(it.body.chars().count(), 158);
        assert!(it.body.ends_with('…'));
    }

    #[test]
    fn burst_collapses_to_one_summary() {
        let (mut p, r, log) = setup();
        let ims = |m: u32| {
            let v: Vec<String> = (1..=4)
                .map(|i| format!(r#"{{"id":"D{i}","mention_count":{m},"last_read":"{NOW}"}}"#))
                .collect();
            format!(
                r#"{{"ok":true,"channels":[],"mpims":[],"ims":[{}]}}"#,
                v.join(",")
            )
        };
        route(&r, "client.counts", &ims(0));
        p.poll().unwrap();
        route(&r, "client.counts", &ims(2));
        let res = p.poll().unwrap();
        assert_eq!(res.unread, 8);
        assert_eq!(res.new_items.len(), 1);
        assert_eq!(res.new_items[0].title, "Slack");
        assert_eq!(res.new_items[0].body, "8 new direct messages or mentions");
        assert!(res.new_items[0].category.is_none());
        assert!(!methods(&log).contains(&"conversations.history".to_string()));
    }

    #[test]
    fn falls_back_to_plain_count_when_nothing_describable() {
        let (mut p, r, _) = setup();
        route(&r, "client.counts", &counts_json(1, ""));
        p.poll().unwrap();
        route(
            &r,
            "client.counts",
            &counts_json(1, "").replace(
                r#""threads":{"has_unreads":false,"mention_count":0}"#,
                r#""threads":{"mention_count":1}"#,
            ),
        );
        let res = p.poll().unwrap();
        assert_eq!(res.new_items.len(), 1);
        assert_eq!(res.new_items[0].body, "1 new direct message or mention");
        route(
            &r,
            "client.counts",
            &counts_json(1, "").replace(
                r#""threads":{"has_unreads":false,"mention_count":0}"#,
                r#""threads":{"mention_count":4}"#,
            ),
        );
        assert_eq!(
            p.poll().unwrap().new_items[0].body,
            "3 new direct messages or mentions"
        );
    }

    #[test]
    fn reset_reprimes() {
        let (mut p, r, _) = setup();
        route(&r, "client.counts", &counts_json(1, ""));
        p.poll().unwrap();
        p.reset();
        assert!(p.poll().unwrap().new_items.is_empty());
    }

    #[test]
    fn clean_markup() {
        let names = |id: &str| (id == "U123").then(|| "Ann".to_string());
        assert_eq!(
            clean("hi <@U123> and <@W9|bob> and <@U777>", names),
            "hi @Ann and @bob and @someone"
        );
        assert_eq!(
            clean(
                "<#C1|general> <!channel> <!everyone|everyone> <!here|here>",
                names
            ),
            "#general @channel @everyone @here"
        );
        assert_eq!(
            clean(
                "<https://a.b/c?d=1|label> <mailto:x@y.z|mail me> <mailto:x@y.z> <http://q>",
                names
            ),
            "label mail me mailto:x@y.z http://q"
        );
        assert_eq!(
            clean("  a &lt;b&gt; &amp;amp; c \n", names),
            "a <b> &amp; c"
        );
        assert_eq!(clean("", names), "");
    }

    #[test]
    fn api_url_resolution() {
        assert_eq!(
            api_url("https://a.slack.com/", "x"),
            "https://a.slack.com/api/x"
        );
        assert_eq!(
            api_url("https://a.slack.com", "x"),
            "https://a.slack.com/api/x"
        );
        assert_eq!(
            api_url("https://a.slack.com/ws/sub/", "x"),
            "https://a.slack.com/ws/sub/api/x"
        );
        assert_eq!(enc("a b&c~é"), "a%20b%26c~%C3%A9");
    }

    #[test]
    fn invalid_auth_twice_means_signed_out() {
        let (mut p, r, _) = setup();
        route(
            &r,
            "client.counts",
            r#"{"ok":false,"error":"invalid_auth"}"#,
        );
        assert!(p.poll().is_none());
        assert!(!p.signed_out());
        assert!(
            p.needs_credentials(),
            "token dropped so it's re-read from the page"
        );
        assert!(p.poll().is_none()); // no creds: nothing sent, not a second rejection
        assert!(!p.signed_out());
        p.set_credentials(creds());
        assert!(p.poll().is_none());
        assert!(p.signed_out());
        // a good poll after a fresh token recovers
        p.set_credentials(creds());
        route(&r, "client.counts", &counts_json(0, ""));
        assert!(p.poll().is_some());
        assert!(!p.signed_out());
    }

    #[test]
    fn other_errors_keep_old_state() {
        let (mut p, r, _) = setup();
        route(&r, "client.counts", r#"{"ok":false,"error":"ratelimited"}"#);
        assert!(p.poll().is_none());
        assert!(!p.needs_credentials() && !p.signed_out());
        route(&r, "client.counts", "<html>502</html>");
        assert!(p.poll().is_none());
        assert!(!p.needs_credentials() && !p.signed_out());
    }

    #[test]
    fn markread_requests() {
        let (mut p, r, log) = setup();
        route(&r, "conversations.mark", r#"{"ok":true}"#);
        let info = HashMap::from([
            ("team".to_string(), "T1".to_string()),
            ("channel".to_string(), "C 1".to_string()),
            ("ts".to_string(), "1700000100.000200".to_string()),
        ]);
        assert!(p.perform("markRead", &info, None));
        let q = log.lock().unwrap()[0].clone();
        assert_eq!(q.url, "https://acme.slack.com/api/conversations.mark");
        assert_eq!(
            q.body.unwrap(),
            "token=xoxc-123-abc%2Fdef&channel=C%201&ts=1700000100.000200"
        );
        let mut no_ts = info.clone();
        no_ts.remove("ts");
        assert!(!p.perform("markRead", &no_ts, None));
        assert!(!p.perform("bogus", &info, None));
        let mut no_ch = info.clone();
        no_ch.insert("channel".into(), String::new());
        assert!(!p.perform("markRead", &no_ch, None));
        route(
            &r,
            "conversations.mark",
            r#"{"ok":false,"error":"channel_not_found"}"#,
        );
        assert!(!p.perform("markRead", &info, None));
    }

    #[test]
    fn reply_requests() {
        let (mut p, r, log) = setup();
        route(&r, "chat.postMessage", r#"{"ok":true}"#);
        let mut info = HashMap::from([
            ("team".to_string(), "T-unknown".to_string()), // unknown team falls back to the first
            ("channel".to_string(), "C1".to_string()),
            ("ts".to_string(), "1.5".to_string()),
            ("kind".to_string(), "channel".to_string()),
        ]);
        assert!(p.perform("reply", &info, Some("a & b")));
        assert!(log.lock().unwrap()[0]
            .body
            .as_deref()
            .unwrap()
            .ends_with("&channel=C1&text=a%20%26%20b&thread_ts=1.5"));
        info.insert("thread".into(), "1.0".into());
        assert!(p.perform("reply", &info, Some("x")));
        assert!(log.lock().unwrap()[1]
            .body
            .as_deref()
            .unwrap()
            .ends_with("&thread_ts=1.0"));
        info.insert("kind".into(), "dm".into());
        assert!(p.perform("reply", &info, Some("x")));
        assert!(!log.lock().unwrap()[2]
            .body
            .as_deref()
            .unwrap()
            .contains("thread_ts"));
        assert!(!p.perform("reply", &info, Some("   \n")));
        assert!(!p.perform("reply", &info, None));
        assert_eq!(log.lock().unwrap().len(), 3);
    }

    #[test]
    fn keep_online_sequence() {
        let (mut p, r, log) = setup();
        route(&r, "users.setPresence", r#"{"ok":true}"#);
        route(&r, "users.setActive", r#"{"ok":true}"#);
        route(&r, "auth.test", r#"{"ok":true,"user_id":"U42"}"#);
        route(
            &r,
            "users.getPresence",
            r#"{"ok":true,"presence":"active"}"#,
        );
        assert!(p.keep_online());
        assert_eq!(
            methods(&log),
            [
                "users.setPresence",
                "users.setActive",
                "auth.test",
                "users.getPresence"
            ]
        );
        let reqs = log.lock().unwrap();
        assert!(reqs[0].body.as_deref().unwrap().ends_with("&presence=auto"));
        assert_eq!(reqs[1].body.as_deref(), Some("token=xoxc-123-abc%2Fdef"));
        assert!(reqs[3].body.as_deref().unwrap().ends_with("&user=U42"));
    }

    #[test]
    fn keep_online_stops_when_auth_test_fails() {
        let (mut p, _r, log) = setup();
        assert!(p.keep_online());
        assert_eq!(
            methods(&log),
            ["users.setPresence", "users.setActive", "auth.test"]
        );
    }
}
