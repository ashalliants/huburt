//! The services Hub hosts, and the JavaScript injected into their pages.
use regex::Regex;

/// Slack blocks Safari/WebKit's own identity with "Please update your browser"; it accepts Chrome's.
/// (macOS only: on Windows the web view is Edge-based, and its own identity is already accepted by every service.)
#[cfg(target_os = "macos")]
pub const CHROME_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

/// What a real Safari sends. WebKit's own default has no "Version/x Safari/605.1.15" part, and Gmail answers that with "This
/// browser version is no longer supported", so Gmail and WhatsApp are told they are talking to Safari.
#[cfg(any(target_os = "macos", test))]
fn safari_ua_for(version: &str) -> String {
    format!("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/{version} Safari/605.1.15")
}

/// A Safari version number ("27.2") from whatever was read from Safari's Info.plist, or None if it doesn't look like one.
#[cfg(any(target_os = "macos", test))]
fn clean_safari_version(raw: &str) -> Option<String> {
    let v = raw.trim();
    let ok = !v.is_empty() && v.len() <= 10 && v.chars().all(|c| c.is_ascii_digit() || c == '.') && v.chars().next().is_some_and(|c| c.is_ascii_digit());
    ok.then(|| v.to_string())
}

/// The installed Safari's version, read once. Presenting the Mac's own current Safari keeps this correct through macOS
/// updates, where a number written into the code would go stale. Falls back to a recent one if Safari can't be read.
#[cfg(target_os = "macos")]
fn safari_ua() -> String {
    static UA: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    UA.get_or_init(|| {
        let read = std::process::Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-", "/Applications/Safari.app/Contents/Info.plist"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| clean_safari_version(&String::from_utf8_lossy(&o.stdout)));
        safari_ua_for(&read.unwrap_or_else(|| "26.0".to_string()))
    })
    .clone()
}

#[cfg(target_os = "macos")]
fn ua_safari() -> Option<String> { Some(safari_ua()) }
#[cfg(not(target_os = "macos"))]
fn ua_safari() -> Option<String> { None }
#[cfg(target_os = "macos")]
fn ua_chrome() -> Option<String> { Some(CHROME_UA.to_string()) }
#[cfg(not(target_os = "macos"))]
fn ua_chrome() -> Option<String> { None }

/// Hosts that always stay inside Hub (single sign-on providers), on top of each service's own hosts.
pub const SIGN_IN_HOSTS: &[&str] = &[
    "google.com", "gstatic.com", "microsoft.com", "microsoftonline.com", "live.com", "okta.com", "oktapreview.com",
    "apple.com", "github.com", "auth0.com", "onelogin.com", "duosecurity.com", "slack.com", "slack-edge.com",
];

pub struct SleepCycle {
    pub interval: u64,
    pub awake_for: u64,
}

pub struct Service {
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    pub url: &'static str,
    /// Links to these stay in the app; everything else opens in the browser.
    pub hosts: &'static [&'static str],
    /// Fixed, so each service keeps its own login across launches (macOS 14+ persistent data store).
    pub store: [u8; 16],
    /// What the page is told its browser is (None: the web view's own identity). A function, because the right Safari version
    /// is read from the Mac when it is needed.
    pub user_agent: fn() -> Option<String>,
    /// Page is only loaded while you look at it; a poller (or sleep cycle) keeps the badge fresh.
    pub lazy: bool,
    pub has_poller: bool,
    /// Keep polling (and prefer its count over the page title) even while the page is open.
    pub poll_while_loaded: bool,
    pub poll_interval: u64,
    pub unread_wording: &'static str,
    /// For services with no cheap poller: while unloaded, wake the page every `interval` seconds for `awake_for` seconds.
    pub sleep_cycle: Option<SleepCycle>,
    /// JavaScript expression true once the page has connected and shown its content (a wake can then finish early).
    pub wake_ready_js: Option<&'static str>,
    /// JavaScript expression: true when showing a sign-in screen, false when signed in, null when unsure.
    pub logged_out_js: Option<&'static str>,
    /// Sites without a dark theme following the system: darken with a colour filter in dark mode.
    pub darken_with_filter: bool,
    /// A class the site puts on <body> when already dark (no filter then).
    pub native_dark_class: Option<&'static str>,
    /// JavaScript expression true once the page has drawn its real content.
    pub rendered_js: Option<&'static str>,
    /// While the signed-in app itself is showing (`is_app_page`), only these hosts stay in Hub; every other link opens in
    /// the browser, even sign-in providers (those are only needed on a sign-in screen).
    pub app_hosts: &'static [&'static str],
    pub is_app_page: fn(host: &str, path: &str) -> bool,
    /// The same test as `is_app_page`, as a JavaScript expression over `location` (the link guard runs in the page).
    pub app_page_js: &'static str,
    pub title_parser: fn(&str) -> u32,
}

fn host_matches(host: &str, domains: &[&str]) -> bool {
    domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

impl Service {
    /// Should a link to `host`, followed from the page at `current` (host, path), stay inside Hub?
    /// On a sign-in screen the service's own sites and sign-in providers stay; once you're in the app itself, only the
    /// app's own sites do, so a GitHub link posted in a Slack message opens in the browser.
    pub fn stays_in_app(&self, host: &str, current: Option<(&str, &str)>) -> bool {
        let host = host.to_lowercase();
        if let Some((ch, cp)) = current {
            if (self.is_app_page)(ch, cp) {
                return host_matches(&host, self.app_hosts);
            }
        }
        host_matches(&host, self.hosts) || host_matches(&host, SIGN_IN_HOSTS)
    }
}

fn first_number(title: &str, pattern: &str) -> u32 {
    Regex::new(pattern).ok().and_then(|re| re.captures(title)).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok()).unwrap_or(0)
}

fn slack_title(title: &str) -> u32 {
    let n = first_number(title, r"[\(\[](\d+)[\)\]]");
    if n > 0 {
        return n;
    }
    // Slack marks unread with a leading "*" and mentions with "!"
    if title.starts_with('*') || title.starts_with('!') { 1 } else { 0 }
}
fn gmail_title(title: &str) -> u32 { first_number(title, r"\((\d+)\)") }
fn whatsapp_title(title: &str) -> u32 { first_number(title, r"^\((\d+)\)") }

const fn store(last: u8) -> [u8; 16] {
    [0x6F, 0x1B, 0x0A, 0x10, 0, 0, 0x40, 0, 0x80, 0, 0, 0, 0, 0, 1, last]
}

pub static SERVICES: &[Service] = &[
    Service {
        id: "slack", name: "Slack", icon: "slack",
        url: "https://app.slack.com/client",
        hosts: &["slack.com", "slack-edge.com"],
        store: store(2), user_agent: ua_chrome,
        lazy: true, has_poller: true, poll_while_loaded: true, poll_interval: 30,
        unread_wording: "DMs/mentions",
        sleep_cycle: None, wake_ready_js: None, logged_out_js: None,
        darken_with_filter: false, native_dark_class: None,
        rendered_js: Some("!!document.querySelector('.ql-editor, [data-qa=\"message_input\"], .p-workspace-layout')"),
        app_hosts: &["slack.com"],
        is_app_page: |h, p| h == "app.slack.com" && p.starts_with("/client"),
        app_page_js: "location.hostname === 'app.slack.com' && location.pathname.indexOf('/client') === 0",
        title_parser: slack_title,
    },
    Service {
        id: "gmail", name: "Gmail", icon: "gmail",
        url: "https://mail.google.com/mail/u/0/",
        hosts: &["google.com", "gstatic.com", "googleusercontent.com"],
        store: store(1), user_agent: ua_safari,
        lazy: true, has_poller: true, poll_while_loaded: true, poll_interval: 60,
        unread_wording: "unread emails",
        sleep_cycle: None, wake_ready_js: None, logged_out_js: None,
        darken_with_filter: true, native_dark_class: None,
        rendered_js: Some("document.querySelectorAll('tr.zA').length > 0 || !!document.querySelector('.T-I.T-I-KE')"),
        app_hosts: &["mail.google.com"],
        is_app_page: |h, _| h == "mail.google.com",
        app_page_js: "location.hostname === 'mail.google.com'",
        title_parser: gmail_title,
    },
    Service {
        id: "whatsapp", name: "WhatsApp", icon: "whatsapp",
        url: "https://web.whatsapp.com",
        hosts: &["whatsapp.com", "whatsapp.net"],
        store: store(3), user_agent: ua_safari,
        lazy: true, has_poller: false, poll_while_loaded: false, poll_interval: 60,
        unread_wording: "unread chats",
        sleep_cycle: Some(SleepCycle { interval: 300, awake_for: 45 }),
        wake_ready_js: Some("!!document.querySelector('#pane-side')"),
        logged_out_js: Some("(function(){ if (document.querySelector('#pane-side')) return false; if (document.querySelector('canvas[aria-label*=\"QR\" i], div[data-ref]')) return true; return null; })()"),
        darken_with_filter: true, native_dark_class: Some("dark"),
        rendered_js: Some("!!document.querySelector('#pane-side, div[data-ref]')"),
        app_hosts: &["whatsapp.com", "whatsapp.net"],
        is_app_page: |_, _| true,
        app_page_js: "true",
        title_parser: whatsapp_title,
    },
];

pub fn service(id: &str) -> Option<&'static Service> { SERVICES.iter().find(|s| s.id == id) }

// MARK: injected JavaScript

/// Darkens a light-only site when the system is in dark mode: invert the page, then flip images and video back.
/// Follows the system setting live because it's a CSS media query. Sites with their own dark theme add a class to
/// <body>; the filter then steps aside entirely (page AND images) so nothing is inverted with nothing to undo it.
pub fn dark_filter_js(native_dark_class: Option<&str>) -> String {
    let target = native_dark_class.map(|c| format!("html:has(body:not(.{c}))")).unwrap_or_else(|| "html".into());
    let media = ["img", "picture", "video", "canvas", "svg image", "[style*=\\\"background-image\\\"]"]
        .iter().map(|s| format!("{target} {s}")).collect::<Vec<_>>().join(", ");
    format!(
        r#"(function () {{
  var css = '@media (prefers-color-scheme: dark) {{' +
    '{target} {{ background: #fff !important; filter: invert(0.92) hue-rotate(180deg); }}' +
    '{media} {{ filter: invert(1) hue-rotate(180deg); }}' +
    '}}';
  function add() {{
    if (window.__hubDark) return;
    window.__hubDark = true;
    try {{
      var sheet = new CSSStyleSheet(); sheet.replaceSync(css);
      document.adoptedStyleSheets = document.adoptedStyleSheets.concat([sheet]);
    }} catch (e) {{
      var st = document.createElement('style'); st.textContent = css;
      (document.head || document.documentElement).appendChild(st);
    }}
  }}
  add();
  document.addEventListener('DOMContentLoaded', function () {{ window.__hubDark = false; add(); }});
}})();"#
    )
}

/// Replaces the page's Notification API so web notifications become Hub banners.
pub fn notification_shim_js(service_id: &str) -> String {
    format!(
        r#"(function () {{
  function N(title, opts) {{
    try {{ window.__TAURI_INTERNALS__.invoke('page_notify', {{ service: '{service_id}', title: String(title), body: (opts && opts.body) ? String(opts.body) : '' }}); }} catch (e) {{}}
  }}
  N.prototype.close = function () {{}};
  N.prototype.addEventListener = function () {{}};
  N.requestPermission = function (cb) {{ if (cb) cb('granted'); return Promise.resolve('granted'); }};
  Object.defineProperty(N, 'permission', {{ get: function () {{ return 'granted'; }} }});
  window.Notification = N;
}})();"#
    )
}

/// Ctrl+1..9 (switch service), Ctrl+R (reload), Ctrl+W / Ctrl+H (hide to the tray). Injected into service pages where
/// there is no app menu to own these shortcuts (Windows).
#[cfg(any(not(target_os = "macos"), test))]
pub fn shortcuts_js() -> &'static str {
    r#"(function () {
  function send(kind, n) { try { window.__TAURI_INTERNALS__.invoke('shortcut', { kind: kind, n: n || null }); } catch (_) {} }
  window.addEventListener('keydown', function (e) {
    if (!e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return;
    var k = e.key.toLowerCase();
    if (/^[1-9]$/.test(k)) { e.preventDefault(); e.stopPropagation(); send('select', Number(k)); }
    else if (k === 'r') { e.preventDefault(); e.stopPropagation(); send('reload'); }
    else if (k === 'w' || k === 'h') { e.preventDefault(); e.stopPropagation(); send('hide'); }
  }, true);
})();"#
}

/// Evaluates `expr` in the page and reports the result back to Hub (`kind` says what the answer is for).
pub fn probe_js(service_id: &str, kind: &str, expr: &str) -> String {
    // The expression goes inside parentheses, so a trailing ";" (as in the Slack token script) would be a syntax error
    let expr = expr.trim().trim_end_matches(';').trim_end();
    format!(
        "(function(){{ var v = null; try {{ v = ({expr}); }} catch (e) {{}} try {{ window.__TAURI_INTERNALS__.invoke('page_probe', {{ service: '{service_id}', kind: '{kind}', value: v === undefined ? null : v }}); }} catch (e) {{}} }})();"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_parsers() {
        assert_eq!(slack_title("(3) Slack | team"), 3);
        assert_eq!(slack_title("[2] foo"), 2);
        assert_eq!(slack_title("* general | team"), 1);
        assert_eq!(slack_title("! dm"), 1);
        assert_eq!(slack_title("Slack"), 0);
        assert_eq!(gmail_title("Inbox (12) - me@x.com - Gmail"), 12);
        assert_eq!(whatsapp_title("(4) WhatsApp"), 4);
        assert_eq!(whatsapp_title("WhatsApp (4)"), 0);
    }

    #[test]
    fn links_in_the_signed_in_app_leave_for_the_browser_but_sign_in_flows_stay() {
        let slack = service("slack").unwrap();
        let in_app = Some(("app.slack.com", "/client/T1/C1"));
        assert!(!slack.stays_in_app("github.com", in_app), "a GitHub link posted in a message opens in the browser");
        assert!(!slack.stays_in_app("docs.google.com", in_app));
        assert!(!slack.stays_in_app("example.com", in_app));
        assert!(!slack.stays_in_app("notslack.com", in_app));
        assert!(slack.stays_in_app("acme.slack.com", in_app), "message permalinks stay");
        // sign in
        assert!(slack.stays_in_app("github.com", Some(("acme.slack.com", "/sign_in_with_password"))));
        assert!(slack.stays_in_app("accounts.google.com", Some(("slack.com", "/signin"))));
        assert!(slack.stays_in_app("github.com", Some(("github.com", "/login"))));
        assert!(slack.stays_in_app("accounts.google.com", None));
        let gmail = service("gmail").unwrap();
        assert!(!gmail.stays_in_app("docs.google.com", Some(("mail.google.com", "/mail/u/0/"))));
        assert!(gmail.stays_in_app("accounts.google.com", Some(("accounts.google.com", "/"))));
        let wa = service("whatsapp").unwrap();
        assert!(!wa.stays_in_app("github.com", Some(("web.whatsapp.com", "/"))));
    }

    #[test]
    fn the_safari_identity_has_the_parts_google_looks_for() {
        let ua = safari_ua_for("27.2");
        assert!(ua.contains("AppleWebKit/605.1.15") && ua.contains("Version/27.2 Safari/605.1.15"), "{ua}");
        assert!(ua.starts_with("Mozilla/5.0 (Macintosh;"));
    }

    #[test]
    fn only_a_real_looking_version_is_used() {
        assert_eq!(clean_safari_version("27.2\n").as_deref(), Some("27.2"));
        assert_eq!(clean_safari_version("18").as_deref(), Some("18"));
        for bad in ["", "  ", "Safari 27", "27.2; rm -rf", "x27.2", ".5", "27.2.1.4.5.6.7.8"] {
            assert_eq!(clean_safari_version(bad), None, "{bad:?}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_macos_every_service_presents_as_a_browser_it_knows() {
        // WebKit's own default (no UA override) has no "Safari/" part, and Gmail refuses it as an unsupported browser.
        for svc in SERVICES {
            let ua = (svc.user_agent)().unwrap_or_else(|| panic!("{} has no user agent override on macOS", svc.id));
            assert!(ua.contains("Safari/"), "{}: {ua}", svc.id);
        }
    }

    #[test]
    fn shortcut_script_reports_the_expected_kinds() {
        let js = shortcuts_js();
        for kind in ["'select'", "'reload'", "'hide'"] {
            assert!(js.contains(kind), "{kind}");
        }
        assert!(js.contains("e.ctrlKey") && js.contains("invoke('shortcut'"));
    }

    #[test]
    fn probes_accept_expressions_that_end_with_a_semicolon() {
        let js = probe_js("slack", "harvest", "(function () { return 1; })();");
        assert!(js.contains("v = ((function () { return 1; })());"), "{js}");
        assert!(!js.contains(";);"));
    }

    #[test]
    fn dark_filter_flips_images_only_when_page_is_inverted() {
        let js = dark_filter_js(Some("dark"));
        assert!(js.contains("html:has(body:not(.dark)) img"));
        assert!(!js.contains("'img, picture"));
        assert!(dark_filter_js(None).contains("html img"));
    }
}
