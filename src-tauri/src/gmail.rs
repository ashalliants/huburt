//! Gmail: reads the Atom feed using the login cookies the app hands over. Only counts are logged.
use crate::http::{host_of, Http, HttpRequest, UreqHttp};
use crate::log;
use crate::types::{Credentials, NotificationItem, PollResult, Poller};
use regex::Regex;
use std::collections::HashSet;

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
const FEED_HOST: &str = "mail.google.com";

pub struct GmailPoller {
    http: Box<dyn Http>,
    creds: Credentials,
    seen: HashSet<String>,
    /// First poll just records what's already unread; no notifications for it.
    primed: bool,
    signed_out: bool,
    /// "important" (Priority Inbox count) or "all".
    source: String,
}

struct Entry {
    id: String,
    sender: String,
    subject: String,
    link: Option<String>,
}

impl GmailPoller {
    pub fn new() -> Self {
        Self::with_http(Box::new(UreqHttp::new()))
    }

    pub fn with_http(http: Box<dyn Http>) -> Self {
        Self {
            http,
            creds: Credentials::default(),
            seen: HashSet::new(),
            primed: false,
            signed_out: false,
            source: "important".into(),
        }
    }

    fn feed(&self) -> String {
        let tail = if self.source == "all" {
            ""
        } else {
            "/%5Eio_im"
        };
        format!("https://{FEED_HOST}/mail/u/0/feed/atom{tail}")
    }
}

impl Default for GmailPoller {
    fn default() -> Self {
        Self::new()
    }
}

fn capture(pattern: &str, text: &str) -> Option<String> {
    let re = Regex::new(&format!("(?s){pattern}")).ok()?;
    re.captures(text).map(|c| c[1].to_string())
}

fn unescape(t: &str) -> String {
    t.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_value(url: &str, name: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    let q = q.split('#').next().unwrap_or("");
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (percent_decode(k) == name).then(|| percent_decode(v))
    })
}

fn entries(xml: &str) -> Vec<Entry> {
    let re = Regex::new("(?s)<entry>(.*?)</entry>").unwrap();
    re.captures_iter(xml)
        .filter_map(|m| {
            let e = &m[1];
            let id = capture("<id>(.*?)</id>", e)?;
            // The feed's own link goes through a redirect Gmail can't open directly; the message_id form opens the email.
            let feed_link = capture("<link[^>]*href=\"([^\"]+)\"", e).map(|l| unescape(&l));
            let message_id = feed_link
                .as_deref()
                .and_then(|l| query_value(l, "message_id"));
            let link = message_id
                .map(|m| format!("https://mail.google.com/mail/u/0/#all/{m}"))
                .or(feed_link);
            Some(Entry {
                id,
                sender: unescape(
                    &capture("<name>(.*?)</name>", e).unwrap_or_else(|| "New email".into()),
                ),
                subject: unescape(&capture("<title>(.*?)</title>", e).unwrap_or_default()),
                link,
            })
        })
        .collect()
}

impl Poller for GmailPoller {
    fn poll(&mut self) -> Option<PollResult> {
        let Some(cookie) = self.creds.cookie_header(FEED_HOST) else {
            log("gmail poll: no usable cookies (not signed in?)");
            self.signed_out = true;
            return None;
        };
        let req = HttpRequest {
            method: "GET".into(),
            url: self.feed(),
            headers: vec![
                ("Cookie".into(), cookie),
                ("User-Agent".into(), USER_AGENT.into()),
            ],
            body: None,
        };
        let (status, bounced, body, err) = match self.http.request(req) {
            Ok(r) => {
                let bounced = r.status == 401 || host_of(&r.url).contains("accounts.google.com");
                (
                    i32::from(r.status),
                    bounced,
                    Some(r.body),
                    "none".to_string(),
                )
            }
            Err(e) => (-1, false, None, e),
        };
        let count = body
            .as_deref()
            .filter(|_| status == 200)
            .and_then(|x| capture(r"<fullcount>(\d+)</fullcount>", x))
            .and_then(|c| c.parse::<u32>().ok());
        let (Some(xml), Some(count)) = (body, count) else {
            log(&format!("gmail poll: failed status={status} error={err}"));
            if bounced {
                self.signed_out = true;
            }
            return None;
        };
        let entries = entries(&xml);
        let ids: HashSet<String> = entries.iter().map(|e| e.id.clone()).collect();
        let fresh: Vec<&Entry> = if self.primed {
            entries
                .iter()
                .filter(|e| !self.seen.contains(&e.id))
                .collect()
        } else {
            vec![]
        };
        let new_items: Vec<NotificationItem> = fresh
            .iter()
            .map(|e| NotificationItem {
                title: e.sender.clone(),
                body: e.subject.clone(),
                url: e.link.clone(),
                ..Default::default()
            })
            .collect();
        log(&format!(
            "gmail poll: ok unread={count} new={} entriesWithLink={}/{}",
            new_items.len(),
            entries.iter().filter(|e| e.link.is_some()).count(),
            entries.len()
        ));
        self.seen = ids;
        self.primed = true;
        self.signed_out = false;
        Some(PollResult {
            unread: count,
            new_items,
            quiet_unread: 0,
        })
    }

    fn needs_credentials(&self) -> bool {
        self.creds.cookie_header(FEED_HOST).is_none()
    }

    fn signed_out(&self) -> bool {
        self.signed_out
    }

    fn set_credentials(&mut self, creds: Credentials) {
        self.creds = creds;
    }

    fn reset(&mut self) {
        self.primed = false;
        self.seen.clear();
    }

    fn set_option(&mut self, key: &str, value: &str) {
        if key == "source" {
            self.source = value.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::{ok, MockHttp};
    use crate::http::HttpResponse;
    use crate::types::Cookie;
    use std::sync::{Arc, Mutex};

    fn entry(id: &str, name: &str, title: &str, link: &str) -> String {
        format!("<entry><title>{title}</title><link rel=\"alternate\" href=\"{link}\"/><id>{id}</id><author><name>{name}</name></author></entry>")
    }
    fn feed(count: u32, entries: &[String]) -> String {
        format!("<?xml version=\"1.0\"?><feed><title>Inbox</title><fullcount>{count}</fullcount>{}</feed>", entries.concat())
    }
    fn creds(domain: &str) -> Credentials {
        Credentials {
            cookies: vec![
                Cookie {
                    name: "SID".into(),
                    value: "abc".into(),
                    domain: domain.into(),
                },
                Cookie {
                    name: "X".into(),
                    value: "no".into(),
                    domain: ".slack.com".into(),
                },
            ],
            page_data: None,
        }
    }
    type Body = Arc<Mutex<Result<HttpResponse, String>>>;
    fn poller(
        initial: Result<HttpResponse, String>,
    ) -> (GmailPoller, Body, crate::http::mock::Log) {
        let body: Body = Arc::new(Mutex::new(initial));
        let b = body.clone();
        let (m, log) = MockHttp::new(move |_| b.lock().unwrap().clone());
        let mut p = GmailPoller::with_http(Box::new(m));
        p.set_credentials(creds(".mail.google.com"));
        (p, body, log)
    }
    const L1: &str = "https://mail.google.com/mail?account_id=a&amp;message_id=18c%2Fx1&amp;view=conv&amp;extsrc=atom";

    #[test]
    fn needs_credentials_until_cookies() {
        let (m, _) = MockHttp::new(|_| ok("", ""));
        let mut p = GmailPoller::with_http(Box::new(m));
        assert!(p.needs_credentials());
        assert!(p.harvest_js().is_none());
        p.set_credentials(creds(".google.com"));
        assert!(!p.needs_credentials());
        p.set_credentials(creds(".slack.com"));
        assert!(p.needs_credentials());
    }

    #[test]
    fn no_cookies_means_signed_out() {
        let (m, log) = MockHttp::new(|_| ok("", ""));
        let mut p = GmailPoller::with_http(Box::new(m));
        assert!(p.poll().is_none());
        assert!(p.signed_out());
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn parses_feed_primes_then_reports_new() {
        let e1 = entry(
            "tag:1",
            "Ann &amp; Co",
            "Hi &lt;you&gt; &quot;x&quot; it&#39;s",
            L1,
        );
        let e2 = entry(
            "tag:2",
            "Bob",
            "Second",
            "https://mail.google.com/mail?foo=1",
        );
        let (mut p, body, log) = poller(ok(
            "https://mail.google.com/x",
            &feed(2, &[e1.clone(), e2.clone()]),
        ));
        let first = p.poll().unwrap();
        assert_eq!(first.unread, 2);
        assert!(first.new_items.is_empty(), "first poll primes");
        let req = log.lock().unwrap()[0].clone();
        assert_eq!(
            req.url,
            "https://mail.google.com/mail/u/0/feed/atom/%5Eio_im"
        );
        assert_eq!(req.header("Cookie"), Some("SID=abc"));
        assert!(req.header("User-Agent").unwrap().contains("Safari"));

        let e3 = entry(
            "tag:3",
            "Ann &amp; Co",
            "Hi &lt;you&gt; &quot;x&quot; it&#39;s",
            L1,
        );
        *body.lock().unwrap() = ok("https://mail.google.com/x", &feed(3, &[e3, e1, e2]));
        let second = p.poll().unwrap();
        assert_eq!(second.unread, 3);
        assert_eq!(second.new_items.len(), 1);
        let it = &second.new_items[0];
        assert_eq!(it.title, "Ann & Co");
        assert_eq!(it.body, "Hi <you> \"x\" it's");
        assert_eq!(
            it.url.as_deref(),
            Some("https://mail.google.com/mail/u/0/#all/18c/x1")
        );
        assert!(it.category.is_none() && it.info.is_empty());
        // Nothing new the third time.
        assert!(p.poll().unwrap().new_items.is_empty());
    }

    #[test]
    fn link_without_message_id_falls_back_to_feed_link() {
        let e = entry(
            "tag:9",
            "Bob",
            "T",
            "https://mail.google.com/mail?foo=1&amp;bar=2",
        );
        let (mut p, body, _) = poller(ok("u", &feed(0, &[])));
        p.poll().unwrap();
        *body.lock().unwrap() = ok("u", &feed(1, &[e]));
        let r = p.poll().unwrap();
        assert_eq!(
            r.new_items[0].url.as_deref(),
            Some("https://mail.google.com/mail?foo=1&bar=2")
        );
    }

    #[test]
    fn entry_defaults_when_name_and_title_missing() {
        let xml = feed(1, &["<entry><id>tag:1</id></entry>".to_string()]);
        let es = entries(&xml);
        assert_eq!(es[0].sender, "New email");
        assert_eq!(es[0].subject, "");
        assert!(es[0].link.is_none());
    }

    #[test]
    fn reset_reprimes() {
        let e1 = entry("tag:1", "A", "S", L1);
        let (mut p, _, _) = poller(ok("u", &feed(1, &[e1])));
        p.poll().unwrap();
        p.reset();
        assert!(p.poll().unwrap().new_items.is_empty());
    }

    #[test]
    fn source_option_selects_feed() {
        let (mut p, _, log) = poller(ok("u", &feed(0, &[])));
        p.set_option("source", "all");
        p.poll().unwrap();
        assert_eq!(
            log.lock().unwrap()[0].url,
            "https://mail.google.com/mail/u/0/feed/atom"
        );
        p.set_option("source", "important");
        assert!(p.feed().ends_with("/atom/%5Eio_im"));
    }

    #[test]
    fn signed_out_on_401_and_bounce() {
        let (mut p, body, _) = poller(Ok(HttpResponse {
            status: 401,
            url: "https://mail.google.com/".into(),
            body: "".into(),
        }));
        assert!(p.poll().is_none());
        assert!(p.signed_out());
        // Recovers on success.
        *body.lock().unwrap() = ok("https://mail.google.com/", &feed(1, &[]));
        assert!(p.poll().is_some());
        assert!(!p.signed_out());
        // Bounced to the sign-in page (200 HTML, final URL on accounts.google.com).
        *body.lock().unwrap() = ok(
            "https://accounts.google.com/ServiceLogin?x=1",
            "<html>sign in</html>",
        );
        assert!(p.poll().is_none());
        assert!(p.signed_out());
    }

    #[test]
    fn transport_error_or_bad_body_is_not_signed_out() {
        let (mut p, body, _) = poller(Err("timeout".into()));
        assert!(p.poll().is_none());
        assert!(!p.signed_out());
        *body.lock().unwrap() = ok("https://mail.google.com/", "<feed>no count</feed>");
        assert!(p.poll().is_none());
        assert!(!p.signed_out());
        *body.lock().unwrap() = Ok(HttpResponse {
            status: 500,
            url: "https://mail.google.com/".into(),
            body: feed(1, &[]),
        });
        assert!(p.poll().is_none());
    }

    #[test]
    fn helpers() {
        assert_eq!(
            host_of("https://accounts.google.com/a?b"),
            "accounts.google.com"
        );
        assert_eq!(percent_decode("a%2Fb%zz%"), "a/b%zz%");
    }
}
