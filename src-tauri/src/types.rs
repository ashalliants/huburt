//! Types shared between the service model, the pollers and the UI.
use std::collections::HashMap;

/// One banner to show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NotificationItem {
    pub title: String,
    pub body: String,
    /// Clicking the banner opens this inside Hub.
    pub url: Option<String>,
    /// Which quick-action buttons the banner gets ("slack.message").
    pub category: Option<String>,
    /// What those buttons need to act on.
    pub info: HashMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PollResult {
    pub unread: u32,
    pub new_items: Vec<NotificationItem>,
    /// Unread content that doesn't deserve a red badge or banner (e.g. channels without mentions).
    pub quiet_unread: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// As stored, possibly with a leading dot (".slack.com").
    pub domain: String,
}

/// What the app hands a poller after the service's page has been open: its cookies, and whatever the poller's
/// `harvest_js` returned from the page (JSON text).
#[derive(Clone, Debug, Default)]
pub struct Credentials {
    pub cookies: Vec<Cookie>,
    pub page_data: Option<String>,
}

impl Cookie {
    /// Browsers report host-only cookies (set for exactly one host, not a whole domain) with no domain at all; those
    /// belong to the host the cookies were asked for.
    pub fn from_parts(name: &str, value: &str, domain: Option<&str>, host: &str) -> Cookie {
        let domain = domain.filter(|d| !d.is_empty()).unwrap_or(host);
        Cookie { name: name.to_string(), value: value.to_string(), domain: domain.to_string() }
    }
}

impl Credentials {
    /// `Cookie:` header value for requests to `host` (only cookies whose domain matches), or None if there are none.
    pub fn cookie_header(&self, host: &str) -> Option<String> {
        let parts: Vec<String> = self
            .cookies
            .iter()
            .filter(|c| {
                let d = c.domain.trim_start_matches('.');
                host == d || host.ends_with(&format!(".{d}"))
            })
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        if parts.is_empty() { None } else { Some(parts.join("; ")) }
    }
}

/// A light-weight "how many unread?" check that works without the service's page being loaded.
/// All methods block (they make HTTP requests), so the app calls them from a worker thread.
pub trait Poller: Send {
    /// None means "couldn't tell", so the old count is kept.
    fn poll(&mut self) -> Option<PollResult>;
    /// True while the poller can't work until the page has been opened once (to pick up a token or cookies).
    fn needs_credentials(&self) -> bool;
    /// True when the service says you're no longer signed in.
    fn signed_out(&self) -> bool;
    fn set_credentials(&mut self, creds: Credentials);
    /// JavaScript (an expression returning a JSON string) that reads what the poller needs out of the open page.
    fn harvest_js(&self) -> Option<&'static str> { None }
    /// Forget what has been seen, so the next poll only records the starting point (used when a service is turned back on).
    fn reset(&mut self) {}
    /// A banner button was pressed ("reply", "markRead"); true if it worked.
    fn perform(&mut self, _action: &str, _info: &HashMap<String, String>, _text: Option<&str>) -> bool { false }
    /// Slack only: tell the service you're active ("show as online"). False if it couldn't be sent yet, or was called off.
    /// `keep_going` is asked before each request it makes (a ping is several requests); once it says no, it stops and returns
    /// false. A request already in flight can't be taken back, so that is the limit of how late it can be cancelled.
    fn keep_online(&mut self, _keep_going: &dyn Fn() -> bool) -> bool { false }
    /// Per-service options (Gmail: "source" = "important" | "all").
    fn set_option(&mut self, _key: &str, _value: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_only_cookies_are_kept_for_their_own_host() {
        let creds = Credentials {
            cookies: vec![
                Cookie::from_parts("SID", "a", Some(".google.com"), "mail.google.com"),
                Cookie::from_parts("OSID", "b", None, "mail.google.com"),
                Cookie::from_parts("X", "c", Some(""), "mail.google.com"),
            ],
            page_data: None,
        };
        assert_eq!(creds.cookie_header("mail.google.com").as_deref(), Some("SID=a; OSID=b; X=c"));
        assert_eq!(creds.cookie_header("accounts.google.com").as_deref(), Some("SID=a"), "host-only ones don't leak to other hosts");
    }
}
