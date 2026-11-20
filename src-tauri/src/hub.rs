//! The app's brain: per-service state (loaded or asleep, unread counts, signed-out, ...) and the scheduler that drives
//! lazy loading, polling, wake cycles, "show as online" and the daily greeting. A direct port of the Swift
//! `ServiceModel` + `AppState`.
//!
//! Locking rule: mutate `Core` under the lock, collect `Action`s, release the lock, then run the actions. Nothing that
//! can block on the main thread (webview calls, HTTP) ever runs while the lock is held.
use crate::{
    native, poller_actor::{PollerHandle, Report}, pollers,
    services::{self, Service, SERVICES},
    settings::Settings,
    types::*,
    ui_thread::UiThread,
    webviews,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::{AppHandle, Emitter};

/// At most this many banners per check; more than that collapses into one "N new …" summary.
pub const MAX_BANNERS: usize = 3;
const TICK: Duration = Duration::from_millis(250);

fn env_secs(name: &str, default: u64) -> Duration {
    Duration::from_secs(std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default))
}
fn now_unix() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

pub struct Svc {
    pub def: &'static Service,
    pub enabled: bool,
    /// The page's web view exists.
    pub loaded: bool,
    /// The page has drawn its content (the "waking up" cover can go).
    pub ready: bool,
    pub page_finished: bool,
    pub load_started: Instant,
    pub error: Option<String>,
    pub error_since: Option<Instant>,
    pub unread: u32,
    pub quiet: u32,
    pub signed_out: bool,
    pub last_checked: Option<Instant>,
    /// This service has an unread poller (Slack, Gmail).
    pub has_poller: bool,
    /// The poller before its thread is started (set up in `Svc::new`; `Hub::new` starts the thread and takes it).
    pub pending_poller: Option<Box<dyn Poller>>,
    /// The running poller. Every operation on it is a non-blocking message handled in order (see `poller_actor.rs`), so the
    /// core never waits for it, and nothing can reorder "reset" and the next "poll".
    pub poller: Option<PollerHandle>,
    /// The poller still needs cookies / a token from the page. Updated only from the poller's own reports, in order.
    pub needs_creds: bool,
    /// What the poller reads out of the page (a fixed property of the poller).
    pub harvest_js: Option<&'static str>,
    /// A poll has been requested and its report hasn't come back yet.
    pub poll_inflight: bool,
    /// Bumped whenever the poller is reconfigured; a poll's answer is only applied if it was requested under the current one.
    pub poll_gen: u64,
    pub poller_healthy: bool,
    pub next_poll: Instant,
    pub cookies: Vec<Cookie>,
    pub idle_deadline: Option<Instant>,
    pub next_render_probe: Instant,
    pub next_loggedout_probe: Instant,
    // sleep cycle (WhatsApp)
    pub next_wake: Option<Instant>,
    pub did_first_wake: bool,
    pub wake_session: bool,
    pub ready_fired: bool,
    pub wake_started: Instant,
    pub wake_deadline: Option<Instant>,
    pub next_ready_probe: Instant,
    pub unread_before_wake: u32,
    pub page_notified_this_wake: bool,
    pub primed_wake: bool,
    // credentials (Slack / Gmail): load the page once to pick up the token and cookies
    pub cred_wake: bool,
    pub cred_tries: u32,
    pub next_cred_check: Instant,
    pub last_cred_wake: Option<Instant>,
    // Slack "show as online"
    pub keeping_online: bool,
    pub last_presence_ping: Option<Instant>,
}

pub struct Core {
    pub settings: Settings,
    pub order: Vec<String>,
    pub selected: String,
    pub window_visible: bool,
    pub window_focused: bool,
    pub svcs: HashMap<&'static str, Svc>,
    pub acknowledged: HashMap<String, u32>,
    pub last_tick: Instant,
    pub next_presence_tick: Instant,
    pub greeting_since: Option<(Instant, Duration, Duration)>,
    pub idle_after: Duration,
}

pub enum Action {
    Load(&'static str),
    Unload(&'static str),
    /// Destroy the page's web view without any of the unload bookkeeping.
    Close(&'static str),
    Probe(&'static str, &'static str, String),
    Layout,
    Poll(&'static str, u64),
    Presence(&'static str),
    Harvest(&'static str),
    Notify(&'static Service, NotificationItem),
    State,
    /// The menu bar icon and hover summary need refreshing.
    Banner,
}

pub struct Hub {
    pub app: AppHandle,
    pub core: Mutex<Core>,
    /// All window and web view work runs here, never on the main thread (see `ui_thread.rs`).
    pub ui: UiThread,
    /// A layout is already queued (so a burst of requests collapses into one).
    pub layout_pending: AtomicBool,
    watchdog: Watchdog,
}

/// Notices when the main thread stops answering (a hang), and says so in the log together with what the UI thread was doing.
struct Watchdog {
    started: Instant,
    ping_inflight: AtomicBool,
    ping_sent_ms: AtomicU64,
    stalled: AtomicBool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    name: &'static str,
    icon: &'static str,
    enabled: bool,
    unread: u32,
    quiet: u32,
    signed_out: bool,
    muted: bool,
    waking: bool,
    error: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StateView {
    order: Vec<String>,
    selected: String,
    paused: bool,
    services: HashMap<&'static str, ServiceView>,
}

impl Svc {
    fn new(def: &'static Service, settings: &Settings) -> Svc {
        let now = Instant::now();
        let mut poller = if def.has_poller { pollers::make(def.id) } else { None };
        if let Some(p) = poller.as_mut() {
            if def.id == "gmail" {
                p.set_option("source", &settings.gmail_source);
            }
        }
        Svc {
            def,
            enabled: !settings.disabled.iter().any(|d| d == def.id),
            loaded: false,
            ready: false,
            page_finished: false,
            load_started: now,
            error: None,
            error_since: None,
            unread: 0,
            quiet: 0,
            signed_out: false,
            last_checked: None,
            needs_creds: poller.as_ref().map(|p| p.needs_credentials()).unwrap_or(false),
            harvest_js: poller.as_ref().and_then(|p| p.harvest_js()),
            has_poller: poller.is_some(),
            pending_poller: poller,
            poller: None,
            poll_inflight: false,
            poll_gen: 0,
            poller_healthy: false,
            next_poll: now,
            cookies: vec![],
            idle_deadline: None,
            next_render_probe: now,
            next_loggedout_probe: now,
            next_wake: None,
            did_first_wake: false,
            wake_session: false,
            ready_fired: false,
            wake_started: now,
            wake_deadline: None,
            next_ready_probe: now,
            unread_before_wake: 0,
            page_notified_this_wake: false,
            primed_wake: false,
            cred_wake: false,
            cred_tries: 0,
            next_cred_check: now,
            last_cred_wake: None,
            keeping_online: false,
            last_presence_ping: None,
        }
    }
    fn lazy(&self) -> bool { self.def.lazy && (self.has_poller || self.def.sleep_cycle.is_some()) }
    fn needs_credentials(&self) -> bool { self.needs_creds }
    fn waking(&self) -> bool { self.loaded && !self.ready && self.error.is_none() }
}

impl Core {
    pub fn new(settings: Settings, idle_after: Duration) -> Core {
        let ids: Vec<&str> = SERVICES.iter().map(|s| s.id).collect();
        let order = settings.resolved_order(&ids);
        let selected = settings.last_selected.clone().filter(|s| ids.contains(&s.as_str())).unwrap_or_else(|| order[0].clone());
        let svcs = SERVICES.iter().map(|d| (d.id, Svc::new(d, &settings))).collect();
        let now = Instant::now();
        Core {
            settings, order, selected, window_visible: false, window_focused: false, svcs, acknowledged: HashMap::new(),
            last_tick: now, next_presence_tick: now + Duration::from_secs(10), greeting_since: None, idle_after,
        }
    }

    pub fn is_visible(&self, id: &str) -> bool { self.window_visible && self.selected == id }
    pub fn is_paused(&self) -> bool { self.settings.paused_until > now_unix() }
    pub fn is_muted(&self, id: &str) -> bool { self.settings.muted_until.get(id).map(|t| *t > now_unix()).unwrap_or(false) }
    pub fn tray_count(&self) -> u32 {
        self.svcs.values().map(|s| s.unread.saturating_sub(*self.acknowledged.get(s.def.id).unwrap_or(&0))).sum()
    }
    pub fn status_text(&self, s: &Svc) -> String {
        if !s.enabled { return "turned off".into(); }
        if s.signed_out { return "signed out".into(); }
        let mut parts = vec![];
        if s.unread > 0 { parts.push(format!("{} {}", s.unread, s.def.unread_wording)); }
        if s.quiet > 0 { parts.push(format!("{} unread channel{}", s.quiet, if s.quiet == 1 { "" } else { "s" })); }
        if parts.is_empty() { "all caught up".into() } else { parts.join(", ") }
    }
    pub fn summary_text(&self) -> String {
        let mut lines: Vec<String> = self.order.iter().filter_map(|id| self.svcs.get(id.as_str()))
            .map(|s| format!("{}: {}", s.def.name, self.status_text(s))).collect();
        if self.is_paused() { lines.push("Notifications paused".into()); }
        lines.join("\n")
    }
    pub fn view(&self) -> StateView {
        StateView {
            order: self.order.clone(),
            selected: self.selected.clone(),
            paused: self.is_paused(),
            services: self.svcs.iter().map(|(id, s)| (*id, ServiceView {
                name: s.def.name, icon: s.def.icon, enabled: s.enabled, unread: s.unread, quiet: s.quiet,
                signed_out: s.signed_out, muted: self.is_muted(id), waking: s.waking() && s.enabled, error: s.error.clone(),
            })).collect(),
        }
    }
    fn enabled_ids(&self) -> Vec<&'static str> {
        self.order.iter().filter_map(|id| self.svcs.get(id.as_str())).filter(|s| s.enabled).map(|s| s.def.id).collect()
    }

    // MARK: visibility / loading decisions

    fn set_visible(&mut self, id: &'static str, visible: bool, acts: &mut Vec<Action>) {
        let idle_after = self.idle_after;
        let Some(s) = self.svcs.get_mut(id) else { return };
        if !s.enabled || !s.lazy() { return; }
        if visible {
            s.idle_deadline = None;
            s.wake_deadline = None;
            s.wake_session = false;
            s.cred_wake = false;
            if !s.loaded { acts.push(Action::Load(id)); }
        } else if s.loaded && !s.wake_session && !s.cred_wake && s.idle_deadline.is_none() {
            s.idle_deadline = Some(Instant::now() + idle_after);
        }
    }

    pub fn refresh_visibility(&mut self, acts: &mut Vec<Action>) {
        for id in SERVICES.iter().map(|s| s.id) {
            let v = self.is_visible(id);
            self.set_visible(id, v, acts);
        }
        acts.push(Action::Layout);
        acts.push(Action::State);
    }

    /// New activity keeps an unloading-soon page alive a little longer, so live conversations stay snappy.
    fn note_activity(&mut self, id: &str) {
        let idle_after = self.idle_after;
        if let Some(s) = self.svcs.get_mut(id) {
            if s.wake_session {
                self.extend_wake(id);
            } else if s.idle_deadline.is_some() {
                s.idle_deadline = Some(Instant::now() + idle_after);
            }
        }
    }

    fn extend_wake(&mut self, id: &str) {
        let settle = env_secs("HUB_WAKE_SETTLE", 6);
        if let Some(s) = self.svcs.get_mut(id) {
            if let Some(c) = s.def.sleep_cycle.as_ref() {
                let awake = env_secs("HUB_WAKE_AWAKE", c.awake_for);
                s.wake_deadline = Some(Instant::now() + if s.ready_fired { settle } else { awake });
            }
        }
    }

    pub fn set_signed_out(&mut self, id: &'static str, v: bool, acts: &mut Vec<Action>) {
        let Some(s) = self.svcs.get_mut(id) else { return };
        if s.signed_out == v { return; }
        s.signed_out = v;
        crate::log(&format!("{id} {}", if v { "signed out" } else { "signed in again" }));
        if v {
            acts.push(Action::Notify(s.def, NotificationItem {
                title: format!("Signed out of {}", s.def.name),
                body: "Open Hub to sign in again.".into(),
                ..Default::default()
            }));
        }
        acts.push(Action::Banner);
    }

    /// Turns a service on or off. Off means nothing is loaded and nothing is checked in the background; the login is kept.
    pub fn set_enabled(&mut self, def: &'static Service, on: bool, a: &mut Vec<Action>) {
        let sel = self.selected.clone();
        let Some(s) = self.svcs.get_mut(def.id) else { return };
        if s.enabled == on { return; }
        s.enabled = on;
        if on {
            // sent while the core lock is held, so no later poll can start before it (they're handled in order)
            s.poll_gen += 1;   // answers from polls started before this are stale
            if let Some(p) = &s.poller { p.reconfigure(None, true); }
            s.did_first_wake = false; s.primed_wake = false; s.next_poll = Instant::now(); s.next_wake = None;
            self.settings.disabled.retain(|d| d != def.id);
        } else {
            if s.loaded { a.push(Action::Close(def.id)); }
            s.loaded = false; s.ready = false;
            s.idle_deadline = None; s.wake_session = false; s.cred_wake = false; s.unread = 0; s.quiet = 0;
            s.last_checked = None; s.error = None; s.signed_out = false; s.keeping_online = false;
            s.poll_gen += 1;
            if let Some(p) = &s.poller { p.reconfigure(None, true); }
            self.settings.disabled.push(def.id.to_string());
            if sel == def.id {
                if let Some(next) = self.enabled_ids().first() { self.selected = next.to_string(); }
            }
        }
        self.settings.save();
        crate::log(&format!("{} turned {}", def.id, if on { "on" } else { "off" }));
        self.refresh_visibility(a);
        a.push(Action::Banner);
    }

    /// A poll answered. Returns the banners to show. An answer is only applied if the poller hasn't been reconfigured since
    /// the poll was requested (`generation`): otherwise it was computed with the old options or the old "what has been
    /// seen" and would announce stale items as new.
    pub fn poll_finished(&mut self, id: &'static str, generation: u64, result: Option<PollResult>, signed_out: bool, needs_creds: bool, acts: &mut Vec<Action>) -> Vec<NotificationItem> {
        let Some(s) = self.svcs.get_mut(id) else { return vec![] };
        s.poll_inflight = false;
        s.needs_creds = needs_creds;   // the poller's state at that moment; reports arrive in order, so this is never stale
        if generation != s.poll_gen || !s.enabled { return vec![]; }
        self.set_signed_out(id, signed_out, acts);
        let Some(s) = self.svcs.get_mut(id) else { return vec![] };
        let Some(r) = result else { return vec![] };
        if s.loaded && !s.def.poll_while_loaded { return vec![]; }
        s.poller_healthy = true;
        s.last_checked = Some(Instant::now());
        s.quiet = r.quiet_unread;
        let grew = r.unread > s.unread;
        s.unread = r.unread;
        // While the page is open it posts its own notifications, so only notify for what the poller finds while unloaded
        let mut notifications = vec![];
        if !s.loaded {
            let mut items = r.new_items;
            if items.len() > MAX_BANNERS {
                let noun = if id == "gmail" { "emails" } else { "messages" };
                items = vec![NotificationItem { title: s.def.name.into(), body: format!("{} new {noun}", items.len()), ..Default::default() }];
            }
            notifications = items;
        }
        if grew { self.note_activity(id); }
        acts.push(Action::Banner);
        acts.push(Action::State);
        notifications
    }

    // MARK: scheduler

    fn tick(&mut self, now: Instant, acts: &mut Vec<Action>) {
        // Did the computer sleep? (a long gap between ticks) -> first wake of the day greets you
        if now.duration_since(self.last_tick) > Duration::from_secs(60) {
            self.greeting_since = Some((now, Duration::from_secs(25), Duration::from_secs(60)));
        }
        self.last_tick = now;

        for id in self.enabled_ids() {
            self.tick_service(id, now, acts);
        }
        if now >= self.next_presence_tick {
            self.next_presence_tick = now + Duration::from_secs(60);
            self.tick_presence(now, acts);
        }
    }

    fn tick_service(&mut self, id: &'static str, now: Instant, acts: &mut Vec<Action>) {
        let visible = self.is_visible(id);
        let Some(s) = self.svcs.get_mut(id) else { return };

        // Waking cover: lift it as soon as the page has drawn, or after a grace period
        if s.loaded && !s.ready && s.error.is_none() {
            let waited = now.duration_since(s.load_started);
            if !s.page_finished && waited > Duration::from_secs(25) {
                s.error = Some(format!("Can't reach {}", s.def.name));
                s.error_since = Some(now);
                crate::log(&format!("{id} page failed to load"));
                acts.push(Action::State);
                acts.push(Action::Layout);
            } else if waited > Duration::from_secs(25) || (s.page_finished && s.def.rendered_js.is_none() && waited > Duration::from_millis(400)) {
                s.ready = true;
                acts.push(Action::State);
                acts.push(Action::Layout);
            } else if let Some(js) = s.def.rendered_js {
                if now >= s.next_render_probe {
                    s.next_render_probe = now + Duration::from_millis(500);
                    acts.push(Action::Probe(id, "rendered", js.to_string()));
                }
            }
        }
        // Showing as signed out while the page is open: keep looking for the login to appear (a sign-in doesn't reload the page)
        if let (true, true, Some(js)) = (s.loaded, s.signed_out, s.def.logged_out_js) {
            if now >= s.next_loggedout_probe {
                s.next_loggedout_probe = now + Duration::from_secs(5);
                acts.push(Action::Probe(id, "loggedout", js.to_string()));
            }
        }
        // Can't reach: try again every so often (there is no "network is back" hook)
        if let (Some(since), true) = (s.error_since, s.error.is_some()) {
            if now.duration_since(since) > Duration::from_secs(30) {
                s.error = None;
                s.error_since = None;
                s.ready = false;
                s.page_finished = false;
                s.load_started = now;
                acts.push(Action::Unload(id));
                acts.push(Action::Load(id));
            }
        }

        // Idle unload: a lazy page nobody is looking at is destroyed (its memory freed); the poller takes over
        if let Some(deadline) = s.idle_deadline {
            if now >= deadline {
                s.idle_deadline = None;
                if !s.keeping_online && !visible && s.loaded {
                    acts.push(Action::Unload(id));
                }
            }
        }

        // Polling: while asleep, or all the time for services whose poller is the source of truth
        if s.has_poller {
            if !s.loaded && s.needs_credentials() {
                // Signed out: don't reload the page every poll just to look for a login that isn't there
                let recently = s.last_cred_wake.map(|t| now.duration_since(t) < Duration::from_secs(600)).unwrap_or(false);
                if !s.cred_wake && !(s.signed_out && recently) && now >= s.next_poll {
                    s.last_cred_wake = Some(now);
                    s.cred_wake = true;
                    s.cred_tries = 0;
                    s.next_cred_check = now + Duration::from_secs(2);
                    crate::log(&format!("{id} loading once to pick up credentials"));
                    acts.push(Action::Load(id));
                }
            } else if (!s.loaded || s.def.poll_while_loaded) && !s.poll_inflight && now >= s.next_poll && !s.needs_credentials() {
                s.next_poll = now + Duration::from_secs(s.def.poll_interval);
                s.poll_inflight = true;
                acts.push(Action::Poll(id, s.poll_gen));
            }
        }
        if s.cred_wake && now >= s.next_cred_check {
            s.next_cred_check = now + Duration::from_secs(2);
            s.cred_tries += 1;
            let needs = s.needs_credentials();
            if !needs || s.cred_tries > 30 {
                s.cred_wake = false;
                if needs {
                    s.signed_out = true;
                    acts.push(Action::State);
                }
                if !visible && s.loaded && !s.wake_session { acts.push(Action::Unload(id)); }
            }
        }

        // Sleep cycle (no cheap poller): wake the page now and then to sync
        if let Some(c) = s.def.sleep_cycle.as_ref() {
            if !s.loaded && !s.wake_session {
                let interval = env_secs("HUB_WAKE_INTERVAL", c.interval);
                let due = *s.next_wake.get_or_insert_with(|| now + if s.did_first_wake { interval } else { Duration::from_secs(5) });
                if now >= due {
                    s.did_first_wake = true;
                    s.next_wake = None;
                    s.unread_before_wake = s.unread;
                    s.page_notified_this_wake = false;
                    s.wake_session = true;
                    s.ready_fired = false;
                    s.wake_started = now;
                    s.next_ready_probe = now + Duration::from_secs(2);
                    crate::log(&format!("{id} waking for background sync"));
                    acts.push(Action::Load(id));
                    let awake = env_secs("HUB_WAKE_AWAKE", c.awake_for);
                    s.wake_deadline = Some(now + awake);
                }
            } else if s.wake_session {
                if let (Some(js), false) = (s.def.wake_ready_js, s.ready_fired) {
                    if s.loaded && now >= s.next_ready_probe {
                        s.next_ready_probe = now + Duration::from_secs(2);
                        acts.push(Action::Probe(id, "wake_ready", js.to_string()));
                    }
                }
                if let Some(d) = s.wake_deadline {
                    if now >= d {
                        s.wake_deadline = None;
                        s.wake_session = false;
                        s.last_checked = Some(now);
                        let fresh = s.unread.saturating_sub(s.unread_before_wake);
                        crate::log(&format!("{id} background sync done, unread {} -> {}", s.unread_before_wake, s.unread));
                        let already_primed = s.primed_wake;
                        s.primed_wake = true;
                        // The page normally posts its own notification; only add one if it didn't
                        if already_primed && fresh > 0 && !s.page_notified_this_wake {
                            acts.push(Action::Notify(s.def, NotificationItem {
                                title: s.def.name.into(),
                                body: format!("{fresh} new unread message{}", if fresh == 1 { "" } else { "s" }),
                                ..Default::default()
                            }));
                        }
                        if let Some(js) = s.def.logged_out_js {
                            acts.push(Action::Probe(id, "loggedout_then_unload", js.to_string()));
                        } else if !visible {
                            acts.push(Action::Unload(id));
                        }
                    }
                }
            }
        }
    }

    fn tick_presence(&mut self, now: Instant, acts: &mut Vec<Action>) {
        let mode = self.settings.slack_online_mode.clone();
        let on = mode == "always" || (mode == "work" && is_work_time());
        let Some(s) = self.svcs.get_mut("slack") else { return };
        if !s.enabled { return; }
        if on {
            if !s.keeping_online { s.keeping_online = true; crate::log("slack staying online: on"); }
            s.idle_deadline = None;
            if !s.loaded { acts.push(Action::Load("slack")); }
            let due = s.last_presence_ping.map(|t| now.duration_since(t) >= Duration::from_secs(240)).unwrap_or(true);
            if due && s.loaded { acts.push(Action::Presence("slack")); }
        } else if s.keeping_online {
            s.keeping_online = false;
            s.last_presence_ping = None;
            crate::log("slack staying online: off");
            let visible = self.window_visible && self.selected == "slack";
            if let Some(s) = self.svcs.get_mut("slack") {
                if s.loaded && !visible && !s.wake_session { s.idle_deadline = Some(now + self.idle_after); }
            }
        }
    }
}

/// Weekdays 8:00-18:00 local time.
pub fn is_work_time() -> bool {
    use chrono::{Datelike, Timelike};
    let t = chrono::Local::now();
    (1..=5).contains(&t.weekday().number_from_monday()) && (8..18).contains(&t.hour())
}

pub fn day_key() -> String { chrono::Local::now().format("%Y-%m-%d").to_string() }

impl Hub {
    pub fn new(app: AppHandle) -> Arc<Hub> {
        let hub = Arc::new(Hub {
            app,
            core: Mutex::new(Core::new(Settings::load(), env_secs("HUB_IDLE_SECONDS", 300))),
            ui: UiThread::start(),
            layout_pending: AtomicBool::new(false),
            watchdog: Watchdog { started: Instant::now(), ping_inflight: AtomicBool::new(false), ping_sent_ms: AtomicU64::new(0), stalled: AtomicBool::new(false) },
        });
        hub.attach_pollers();
        hub
    }

    /// Starts the scheduler thread.
    pub fn start(self: &Arc<Self>) {
        {
            let mut c = self.core.lock().unwrap();
            let last = c.settings.last_greeting_day.clone();
            if last != day_key() { c.greeting_since = Some((Instant::now(), Duration::ZERO, Duration::from_secs(60))); }
        }
        let hub = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(TICK);
            hub.tick();
        });
    }

    /// Asks the main thread to answer a tiny job every tick; if it hasn't for several seconds, the app is hung, and the log
    /// says so (once), with what the UI thread was doing, so a freeze leaves evidence.
    fn check_main_thread(self: &Arc<Self>) {
        const STALL_AFTER_MS: u64 = 8000;
        let now_ms = self.watchdog.started.elapsed().as_millis() as u64;
        if !self.watchdog.ping_inflight.swap(true, Ordering::SeqCst) {
            self.watchdog.ping_sent_ms.store(now_ms, Ordering::SeqCst);
            let hub = self.clone();
            let _ = self.app.run_on_main_thread(move || {
                hub.watchdog.ping_inflight.store(false, Ordering::SeqCst);
                if hub.watchdog.stalled.swap(false, Ordering::SeqCst) {
                    crate::log("the main thread is answering again");
                }
            });
        } else {
            let waited = now_ms.saturating_sub(self.watchdog.ping_sent_ms.load(Ordering::SeqCst));
            if waited > STALL_AFTER_MS && !self.watchdog.stalled.swap(true, Ordering::SeqCst) {
                crate::log(&format!("the main thread has not answered for {}s. {}", waited / 1000, self.ui.describe()));
            }
        }
    }

    pub fn tick(self: &Arc<Self>) {
        self.check_main_thread();
        let mut acts = vec![];
        let greet = {
            let mut c = self.core.lock().unwrap();
            c.tick(Instant::now(), &mut acts);
            c.greeting_ready()
        };
        if greet { self.greet(false); }
        self.run(acts);
    }

    /// Runs `f` on the core under the lock, then performs whatever actions it asked for.
    pub fn with<R>(self: &Arc<Self>, f: impl FnOnce(&mut Core, &mut Vec<Action>) -> R) -> R {
        let mut acts = vec![];
        let r = { let mut c = self.core.lock().unwrap(); f(&mut c, &mut acts) };
        self.run(acts);
        r
    }

    pub fn run(self: &Arc<Self>, acts: Vec<Action>) {
        for a in acts {
            match a {
                Action::Load(id) => self.load(id),
                Action::Unload(id) => self.unload(id),
                Action::Close(id) => webviews::close_service(self, id),
                Action::Layout => webviews::layout(self),
                Action::State => self.emit_state(),
                Action::Banner => self.update_tray(),
                Action::Probe(id, kind, js) => webviews::eval(self, id, &services::probe_js(id, kind, &js)),
                Action::Poll(id, generation) => self.spawn_poll(id, generation),
                Action::Presence(id) => self.spawn_presence(id),
                Action::Harvest(id) => self.harvest(id),
                Action::Notify(svc, item) => self.notify(svc, item),
            }
        }
    }

    // MARK: load / unload

    pub fn load(self: &Arc<Self>, id: &'static str) {
        let go = self.with(|c, _| {
            let Some(s) = c.svcs.get_mut(id) else { return false };
            if !s.enabled || s.loaded { return false; }
            s.loaded = true;
            s.ready = false;
            s.page_finished = false;
            s.error = None;
            s.error_since = None;
            s.load_started = Instant::now();
            true
        });
        if go {
            webviews::create_service(self, id);
            self.with(|_, a| { a.push(Action::Layout); a.push(Action::State); });
        }
    }

    pub fn unload(self: &Arc<Self>, id: &'static str) {
        let was = self.with(|c, _| {
            let Some(s) = c.svcs.get_mut(id) else { return false };
            let was = s.loaded;
            s.loaded = false;
            s.ready = false;
            s.idle_deadline = None;
            s.wake_deadline = None;
            s.wake_session = false;
            s.cred_wake = false;
            was
        });
        if was {
            webviews::close_service(self, id);
            crate::log(&format!("{id} unloaded"));
            self.with(|_, a| a.push(Action::State));
        }
    }

    // MARK: polling

    fn spawn_poll(self: &Arc<Self>, id: &'static str, generation: u64) {
        let Some((poller, loaded)) = self.core.lock().unwrap().svcs.get(id).and_then(|s| s.poller.clone().map(|p| (p, s.loaded))) else { return };
        if !loaded {
            poller.poll(generation);   // the answer comes back as a report, see `poller_report`
            return;
        }
        // While the page is open its cookies are readable: use the freshest ones (the services rotate them). Reading them
        // waits for the main thread, so it happens on a worker thread; the poller's own thread does the network work.
        let hub = self.clone();
        std::thread::spawn(move || {
            let cookies = webviews::cookies(&hub, id);
            if !cookies.is_empty() {
                poller.credentials(Credentials { cookies, page_data: None });
            }
            poller.poll(generation);
        });
    }

    /// What a poller reported after an operation. They arrive in the order the operations were sent.
    fn poller_report(self: &Arc<Self>, id: &'static str, report: Report) {
        match report {
            Report::Polled { generation, result, signed_out, needs_creds } => self.poll_done(id, generation, result, signed_out, needs_creds),
            Report::Credentials { needs_creds } => self.with(|c, _| { if let Some(s) = c.svcs.get_mut(id) { s.needs_creds = needs_creds; } }),
        }
    }

    /// Starts each poller's thread. Called once, right after the `Hub` exists (the threads report back to it).
    fn attach_pollers(self: &Arc<Self>) {
        let mut c = self.core.lock().unwrap();
        for s in c.svcs.values_mut() {
            if let Some(poller) = s.pending_poller.take() {
                let hub = self.clone();
                s.poller = Some(PollerHandle::spawn(s.def.id, poller, move |id, report| hub.poller_report(id, report)));
            }
        }
    }

    fn poll_done(self: &Arc<Self>, id: &'static str, generation: u64, result: Option<PollResult>, signed_out: bool, needs_creds: bool) {
        let notifications = self.with(|c, acts| c.poll_finished(id, generation, result, signed_out, needs_creds, acts));
        if let Some(svc) = services::service(id) {
            for item in notifications { self.notify(svc, item); }
        }
    }

    fn spawn_presence(self: &Arc<Self>, id: &'static str) {
        let Some(poller) = self.core.lock().unwrap().svcs.get(id).and_then(|s| s.poller.clone()) else { return };
        let hub = self.clone();
        poller.keep_online(move |sent| {
            if sent {
                hub.with(|c, _| { if let Some(s) = c.svcs.get_mut(id) { s.last_presence_ping = Some(Instant::now()); } });
            }
        });
    }

    /// The page finished loading: pick up the cookies and whatever the poller reads out of the page.
    fn harvest(self: &Arc<Self>, id: &'static str) {
        let (has_poller, js) = {
            let c = self.core.lock().unwrap();
            let Some(s) = c.svcs.get(id) else { return };
            (s.has_poller, s.harvest_js)
        };
        if !has_poller { return; }
        let hub = self.clone();
        std::thread::spawn(move || {
            let cookies = webviews::cookies(&hub, id);   // waits for the main thread, so on a worker thread
            let poller = hub.with(|c, _| {
                let s = c.svcs.get_mut(id)?;
                if !cookies.is_empty() { s.cookies = cookies.clone(); }
                crate::log(&format!("{id}: picked up {} cookies from the page", cookies.len()));
                s.poller.clone()
            });
            match (js, poller) {
                (None, Some(p)) => p.credentials(Credentials { cookies, page_data: None }),
                (Some(js), _) => webviews::eval(&hub, id, &services::probe_js(id, "harvest", js)),
                _ => {}
            }
        });
    }

    // MARK: events from pages

    pub fn page_started(self: &Arc<Self>, id: &'static str) {
        self.with(|c, a| {
            if let Some(s) = c.svcs.get_mut(id) { s.page_finished = false; if s.error.take().is_some() { a.push(Action::State); } }
        });
    }

    pub fn page_finished(self: &Arc<Self>, id: &'static str) {
        self.with(|c, a| {
            let visible = c.is_visible(id);
            if let Some(s) = c.svcs.get_mut(id) {
                s.page_finished = true;
                a.push(Action::Harvest(id));
                if let (Some(js), true) = (s.def.logged_out_js, visible) {
                    // while you're looking at it, notice a sign-in screen (or its absence) once it has drawn
                    a.push(Action::Probe(id, "loggedout", js.to_string()));
                }
            }
        });
    }

    pub fn page_title(self: &Arc<Self>, id: &'static str, title: &str) {
        self.with(|c, a| {
            let Some(s) = c.svcs.get_mut(id) else { return };
            // Slack and Gmail: the poller's count is the source of truth once it works
            if s.def.poll_while_loaded && s.poller_healthy { return; }
            let n = (s.def.title_parser)(title);
            if n != s.unread {
                if n > s.unread { c.note_activity(id); }
                if let Some(s) = c.svcs.get_mut(id) { s.unread = n; s.last_checked = Some(Instant::now()); }
                a.push(Action::Banner);
                a.push(Action::State);
            }
        });
    }

    pub fn page_probe(self: &Arc<Self>, id: &'static str, kind: &str, value: serde_json::Value) {
        let Some(def) = services::service(id) else { return };
        match kind {
            "rendered" if value == serde_json::Value::Bool(true) => {
                self.with(|c, a| {
                    if let Some(s) = c.svcs.get_mut(id) { if s.loaded && !s.ready { s.ready = true; a.push(Action::Layout); a.push(Action::State); } }
                });
            }
            "wake_ready" if value == serde_json::Value::Bool(true) => {
                self.with(|c, _| {
                    if let Some(s) = c.svcs.get_mut(id) {
                        if s.wake_session && !s.ready_fired {
                            s.ready_fired = true;
                            crate::log(&format!("{id} ready after {}s", s.wake_started.elapsed().as_secs()));
                            c.extend_wake(id);
                        }
                    }
                });
            }
            "loggedout" | "loggedout_then_unload" => {
                let then_unload = kind == "loggedout_then_unload";
                self.with(|c, a| {
                    if let Some(b) = value.as_bool() { c.set_signed_out(def.id, b, a); }
                    if then_unload && !c.is_visible(id) { a.push(Action::Unload(def.id)); }
                });
            }
            "harvest" => {
                let page_data = value.as_str().map(|s| s.to_string());
                let (poller, cookies) = {
                    let c = self.core.lock().unwrap();
                    match c.svcs.get(id) { Some(s) => (s.poller.clone(), s.cookies.clone()), None => return }
                };
                // sending is a non-blocking message, so this is safe on the main thread (this is a command)
                if let Some(p) = poller { p.credentials(Credentials { cookies, page_data }); }
            }
            "test" => crate::log(&format!("{id} page bridge ok: {value}")),
            _ => {}
        }
    }

    pub fn page_notify(self: &Arc<Self>, id: &'static str, title: String, body: String) {
        self.with(|c, _| {
            if let Some(s) = c.svcs.get_mut(id) { s.page_notified_this_wake = true; }
            c.note_activity(id);
        });
        if let Some(svc) = services::service(id) {
            self.notify(svc, NotificationItem { title: if title.is_empty() { svc.name.into() } else { title }, body, ..Default::default() });
        }
    }

    // MARK: user actions

    pub fn select(self: &Arc<Self>, id: &str) {
        self.with(|c, a| {
            let Some(def) = services::service(id) else { return };
            c.selected = def.id.to_string();
            c.settings.last_selected = Some(def.id.to_string());
            c.settings.save();
            c.refresh_visibility(a);
        });
    }

    pub fn select_index(self: &Arc<Self>, i: usize) {
        let id = self.core.lock().unwrap().order.get(i).cloned();
        if let Some(id) = id { self.select(&id); }
    }

    pub fn set_window_visible(self: &Arc<Self>, visible: bool) {
        self.with(|c, a| {
            c.window_visible = visible;
            c.refresh_visibility(a);
        });
    }

    /// On opening Hub: jump to whatever needs you most (DMs/mentions, then unread channels); otherwise stay on the last tab.
    pub fn select_most_urgent(self: &Arc<Self>) {
        let pick = {
            let c = self.core.lock().unwrap();
            let ids = c.enabled_ids();
            ids.iter().find(|i| c.svcs[*i].unread > 0).or_else(|| ids.iter().find(|i| c.svcs[*i].quiet > 0)).copied()
        };
        if let Some(id) = pick { crate::log(&format!("opening on {id}: it has something unread")); self.select(id); }
    }

    pub fn reorder(self: &Arc<Self>, order: Vec<String>) {
        self.with(|c, a| {
            let ids: Vec<&str> = SERVICES.iter().map(|s| s.id).collect();
            let tmp = Settings { order, ..Default::default() };
            c.order = tmp.resolved_order(&ids);
            c.settings.order = c.order.clone();
            c.settings.save();
            a.push(Action::State);
        });
    }

    pub fn set_enabled(self: &Arc<Self>, id: &str, on: bool) {
        let Some(def) = services::service(id) else { return };
        self.with(|c, a| c.set_enabled(def, on, a));
    }

    pub fn reload(self: &Arc<Self>, id: &str) {
        let Some(def) = services::service(id) else { return };
        let loaded = self.with(|c, a| {
            let Some(s) = c.svcs.get_mut(def.id) else { return false };
            if !s.enabled { return false; }
            s.error = None;
            s.error_since = None;
            if s.loaded {
                s.ready = false;
                s.page_finished = false;
                s.load_started = Instant::now();
                a.push(Action::State);
            }
            s.loaded
        });
        if loaded { webviews::reload(self, def.id); } else {
            self.load(def.id);   // asleep: wake it
            self.with(|c, _| { let v = c.is_visible(id); let idle = c.idle_after; if let Some(s) = c.svcs.get_mut(def.id) { if !v { s.idle_deadline = Some(Instant::now() + idle); } } });
        }
    }

    /// Opens a specific page (a Slack conversation, a Gmail message), loading the service first if it was asleep.
    pub fn navigate(self: &Arc<Self>, id: &str, url: &str) {
        let Some(def) = services::service(id) else { return };
        self.load(def.id);
        webviews::navigate(self, def.id, url);
    }

    pub fn mute(self: &Arc<Self>, id: &str, until: Option<f64>) {
        self.with(|c, a| {
            match until { Some(t) => { c.settings.muted_until.insert(id.to_string(), t); } None => { c.settings.muted_until.remove(id); } }
            c.settings.save();
            crate::log(&format!("{id} notifications {}", if until.is_some() { "muted" } else { "unmuted" }));
            a.push(Action::State);
            a.push(Action::Banner);
        });
    }

    pub fn pause(self: &Arc<Self>, until: Option<f64>) {
        self.with(|c, a| {
            c.settings.paused_until = until.unwrap_or(0.0);
            c.settings.save();
            crate::log(&format!("notifications {}", if until.is_some() { "paused" } else { "resumed" }));
            a.push(Action::State);
            a.push(Action::Banner);
        });
    }

    pub fn dismiss_tray_badge(self: &Arc<Self>) {
        self.with(|c, a| {
            let pairs: Vec<(String, u32)> = c.svcs.values().filter(|s| s.unread > 0).map(|s| (s.def.id.to_string(), s.unread)).collect();
            for (id, n) in pairs { c.acknowledged.insert(id, n); }
            crate::log("menu bar red dot dismissed");
            a.push(Action::Banner);
        });
    }

    pub fn set_gmail_source(self: &Arc<Self>, source: &str) {
        self.with(|c, a| {
            c.settings.gmail_source = source.to_string();
            c.settings.save();
            if let Some(s) = c.svcs.get_mut("gmail") {
                s.poll_gen += 1;
                if let Some(p) = &s.poller { p.reconfigure(Some(("source", source)), true); }   // one message, before the poll below
                s.next_poll = Instant::now();   // re-read the count straight away
            }
            crate::log(&format!("gmail count source: {source}"));
            a.push(Action::State);
        });
    }

    pub fn set_slack_online_mode(self: &Arc<Self>, mode: &str) {
        self.with(|c, a| {
            c.settings.slack_online_mode = mode.to_string();
            c.settings.save();
            crate::log(&format!("slack show as online: {mode}"));
            c.next_presence_tick = Instant::now();
            a.push(Action::State);
        });
    }

    /// A notification button was pressed ("reply", "markRead").
    pub fn handle_action(self: &Arc<Self>, action: String, info: HashMap<String, String>, text: Option<String>) {
        let Some(id) = info.get("service").cloned() else { return };
        let Some(def) = services::service(&id) else { return };
        let Some(poller) = self.core.lock().unwrap().svcs.get(def.id).and_then(|s| s.poller.clone()) else { return };
        let hub = self.clone();
        let performed = action.clone();
        poller.perform(action, info, text, move |ok| {
            crate::log(&format!("{id} banner action {performed}: {}", if ok { "done" } else { "failed" }));
            if !ok {
                let failure = if performed == "reply" { "Couldn't send your reply" } else { "Couldn't mark it as read" };
                hub.notify(def, NotificationItem { title: failure.into(), body: format!("Open {} in Hub to do it there.", def.name), ..Default::default() });
            } else {
                hub.with(|c, _| { if let Some(s) = c.svcs.get_mut(def.id) { s.next_poll = Instant::now(); } });
            }
        });
    }

    // MARK: notifications, badge, tray

    pub fn notify(self: &Arc<Self>, svc: &'static Service, item: NotificationItem) {
        let skip = {
            let c = self.core.lock().unwrap();
            c.is_paused() || c.is_muted(svc.id) || (c.window_visible && c.window_focused && c.selected == svc.id)
        };
        if skip { return; }
        native::post_banner(self, svc, &item);
    }

    pub fn emit_state(self: &Arc<Self>) {
        let v = self.core.lock().unwrap().view();
        let _ = self.app.emit("state", v);
    }

    pub fn update_tray(self: &Arc<Self>) {
        let (total, summary) = {
            let mut c = self.core.lock().unwrap();
            // If something was read, forget the matching part of the dismissal so the next new message lights the dot again
            let pairs: Vec<(String, u32)> = c.svcs.values().map(|s| (s.def.id.to_string(), s.unread)).collect();
            for (id, unread) in pairs {
                if let Some(a) = c.acknowledged.get(&id).copied() {
                    if unread < a { if unread > 0 { c.acknowledged.insert(id, unread); } else { c.acknowledged.remove(&id); } }
                }
            }
            (c.tray_count(), c.summary_text())
        };
        let hub = self.clone();
        self.ui.post("update tray", move || crate::tray::update(&hub, total, &summary));
    }

    // MARK: daily greeting

    fn greet(self: &Arc<Self>, force: bool) {
        let (title, body) = {
            let mut c = self.core.lock().unwrap();
            if !force && c.settings.last_greeting_day == day_key() { return; }
            c.settings.last_greeting_day = day_key();
            c.settings.save();
            let ids = c.enabled_ids();
            // no point if you're already in Hub
            if c.is_paused() || (c.window_visible && c.window_focused) || ids.is_empty() { return; }
            use chrono::Timelike;
            let hour = chrono::Local::now().hour();
            let title = match hour { 5..=11 => "Good morning", 12..=16 => "Good afternoon", 17..=21 => "Good evening", _ => "Hello" };
            let waiting: Vec<String> = ids.iter().filter_map(|i| c.svcs.get(i)).filter(|s| s.unread > 0 || s.quiet > 0 || s.signed_out)
                .map(|s| format!("{}: {}", s.def.name, c.status_text(s))).collect();
            let body = if waiting.is_empty() { "Everything's caught up.".to_string() } else { waiting.join(" · ") };
            (title.to_string(), body)
        };
        crate::log("greeting sent");
        native::post_greeting(self, &title, &body);
    }

    pub fn greet_now(self: &Arc<Self>) { self.greet(true); }
}

impl Core {
    /// True once the first checks are back (or we've waited long enough), so the greeting's numbers are real.
    fn greeting_ready(&mut self) -> bool {
        let Some((since, min_wait, max_wait)) = self.greeting_since else { return false };
        let waited = since.elapsed();
        let ready = self.enabled_ids().iter().all(|i| { let s = &self.svcs[i]; s.last_checked.is_some() || s.signed_out || s.error.is_some() });
        if waited >= min_wait && (ready || waited > max_wait) {
            self.greeting_since = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> Core { Core::new(Settings::default(), Duration::from_millis(50)) }
    fn has_load(acts: &[Action], id: &str) -> bool { acts.iter().any(|a| matches!(a, Action::Load(i) if *i == id)) }
    fn has_unload(acts: &[Action], id: &str) -> bool { acts.iter().any(|a| matches!(a, Action::Unload(i) if *i == id)) }

    #[test]
    fn selecting_a_service_loads_it_and_leaving_unloads_it_after_the_idle_time() {
        let mut c = core();
        let mut acts = vec![];
        c.window_visible = true;
        c.selected = "slack".into();
        c.refresh_visibility(&mut acts);
        assert!(has_load(&acts, "slack"));
        assert!(!has_load(&acts, "gmail"), "only the visible service loads");

        // pretend it loaded, then look at another service
        c.svcs.get_mut("slack").unwrap().loaded = true;
        c.selected = "gmail".into();
        let mut acts = vec![];
        c.refresh_visibility(&mut acts);
        assert!(c.svcs["slack"].idle_deadline.is_some(), "unload is scheduled, not immediate");
        assert!(!has_unload(&acts, "slack"));

        std::thread::sleep(Duration::from_millis(80));
        let mut acts = vec![];
        c.tick_service("slack", Instant::now(), &mut acts);
        assert!(has_unload(&acts, "slack"));
    }

    #[test]
    fn a_service_you_come_back_to_is_not_unloaded() {
        let mut c = core();
        c.window_visible = true;
        c.selected = "gmail".into();
        c.svcs.get_mut("slack").unwrap().loaded = true;
        let mut acts = vec![];
        c.refresh_visibility(&mut acts);
        assert!(c.svcs["slack"].idle_deadline.is_some());
        c.selected = "slack".into();
        c.refresh_visibility(&mut acts);
        assert!(c.svcs["slack"].idle_deadline.is_none());
    }

    #[test]
    fn staying_online_keeps_the_page_from_unloading() {
        let mut c = core();
        let s = c.svcs.get_mut("slack").unwrap();
        s.loaded = true;
        s.keeping_online = true;
        s.idle_deadline = Some(Instant::now() - Duration::from_secs(1));
        let mut acts = vec![];
        c.tick_service("slack", Instant::now(), &mut acts);
        assert!(!has_unload(&acts, "slack"));
        assert!(c.svcs["slack"].idle_deadline.is_none());
    }

    #[test]
    fn a_poller_that_has_no_credentials_loads_the_page_once_to_get_them() {
        let mut c = core();
        let mut acts = vec![];
        c.tick_service("slack", Instant::now(), &mut acts);
        assert!(has_load(&acts, "slack"));
        assert!(c.svcs["slack"].cred_wake);
        // and doesn't do it again straight away
        let mut acts = vec![];
        c.svcs.get_mut("slack").unwrap().loaded = true;
        c.tick_service("slack", Instant::now(), &mut acts);
        assert!(!has_load(&acts, "slack"));
    }

    #[test]
    fn a_signed_out_page_keeps_checking_whether_you_have_signed_in() {
        let mut c = core();
        let s = c.svcs.get_mut("whatsapp").unwrap();
        s.loaded = true;
        s.signed_out = true;
        let t0 = Instant::now();
        let probes = |acts: &[Action]| acts.iter().filter(|a| matches!(a, Action::Probe("whatsapp", "loggedout", _))).count();
        let mut acts = vec![];
        c.tick_service("whatsapp", t0, &mut acts);
        assert_eq!(probes(&acts), 1);
        let mut acts = vec![];
        c.tick_service("whatsapp", t0 + Duration::from_secs(1), &mut acts);
        assert_eq!(probes(&acts), 0, "not every tick");
        let mut acts = vec![];
        c.tick_service("whatsapp", t0 + Duration::from_secs(6), &mut acts);
        assert_eq!(probes(&acts), 1);
        // once signed in again it stops
        c.set_signed_out("whatsapp", false, &mut vec![]);
        let mut acts = vec![];
        c.tick_service("whatsapp", t0 + Duration::from_secs(20), &mut acts);
        assert_eq!(probes(&acts), 0);
    }

    #[test]
    fn a_signed_out_service_is_not_reloaded_every_poll() {
        let mut c = core();
        let s = c.svcs.get_mut("gmail").unwrap();
        s.signed_out = true;
        s.last_cred_wake = Some(Instant::now());
        let mut acts = vec![];
        c.tick_service("gmail", Instant::now(), &mut acts);
        assert!(!has_load(&acts, "gmail"));
    }

    #[test]
    fn whatsapp_wakes_on_a_cycle_syncs_then_goes_back_to_sleep() {
        let mut c = core();
        let now = Instant::now();
        let mut acts = vec![];
        c.tick_service("whatsapp", now, &mut acts);   // sets the first wake for a few seconds ahead
        assert!(!has_load(&acts, "whatsapp"));
        let later = now + Duration::from_secs(6);
        let mut acts = vec![];
        c.tick_service("whatsapp", later, &mut acts);
        assert!(has_load(&acts, "whatsapp"));
        assert!(c.svcs["whatsapp"].wake_session);

        // the page loaded and the sync window is over: it unloads (after checking whether you're signed out)
        let s = c.svcs.get_mut("whatsapp").unwrap();
        s.loaded = true;
        s.unread = 2;
        let after = later + Duration::from_secs(60);
        let mut acts = vec![];
        c.tick_service("whatsapp", after, &mut acts);
        assert!(!c.svcs["whatsapp"].wake_session);
        assert!(acts.iter().any(|a| matches!(a, Action::Probe("whatsapp", "loggedout_then_unload", _))));
    }

    #[test]
    fn only_new_unread_after_the_first_sync_notifies() {
        let mut c = core();
        let s = c.svcs.get_mut("whatsapp").unwrap();
        s.loaded = true;
        s.wake_session = true;
        s.unread_before_wake = 0;
        s.unread = 3;
        s.wake_deadline = Some(Instant::now() - Duration::from_secs(1));
        let mut acts = vec![];
        c.tick_service("whatsapp", Instant::now(), &mut acts);
        assert!(!acts.iter().any(|a| matches!(a, Action::Notify(..))), "first sync only records what's already unread");

        let s = c.svcs.get_mut("whatsapp").unwrap();
        s.wake_session = true;
        s.unread_before_wake = 3;
        s.unread = 5;
        s.wake_deadline = Some(Instant::now() - Duration::from_secs(1));
        let mut acts = vec![];
        c.tick_service("whatsapp", Instant::now(), &mut acts);
        assert!(acts.iter().any(|a| matches!(a, Action::Notify(_, n) if n.body == "2 new unread messages")));
    }

    #[test]
    fn red_dot_counts_what_has_not_been_waved_away() {
        let mut c = core();
        c.svcs.get_mut("slack").unwrap().unread = 4;
        c.svcs.get_mut("gmail").unwrap().unread = 2;
        assert_eq!(c.tray_count(), 6);
        c.acknowledged.insert("slack".into(), 4);
        assert_eq!(c.tray_count(), 2);
        c.svcs.get_mut("slack").unwrap().unread = 5;   // something new arrived
        assert_eq!(c.tray_count(), 3);
    }

    #[test]
    fn plain_language_status_lines() {
        let mut c = core();
        assert_eq!(c.status_text(&c.svcs["gmail"]), "all caught up");
        c.svcs.get_mut("gmail").unwrap().unread = 3;
        assert_eq!(c.status_text(&c.svcs["gmail"]), "3 unread emails");
        c.svcs.get_mut("slack").unwrap().quiet = 1;
        assert_eq!(c.status_text(&c.svcs["slack"]), "1 unread channel");
        c.svcs.get_mut("slack").unwrap().signed_out = true;
        assert_eq!(c.status_text(&c.svcs["slack"]), "signed out");
        c.svcs.get_mut("whatsapp").unwrap().enabled = false;
        assert_eq!(c.status_text(&c.svcs["whatsapp"]), "turned off");
    }

    #[test]
    fn signing_out_notifies_once() {
        let mut c = core();
        let mut acts = vec![];
        c.set_signed_out("gmail", true, &mut acts);
        c.set_signed_out("gmail", true, &mut acts);
        assert_eq!(acts.iter().filter(|a| matches!(a, Action::Notify(..))).count(), 1);
    }

    #[test]
    fn turning_a_service_off_closes_it_clears_its_counts_and_moves_selection_on() {
        let dir = std::env::temp_dir().join(format!("hub-test-{}", std::process::id()));
        std::env::set_var("HUB_SETTINGS_DIR", &dir);
        let mut c = core();
        c.window_visible = true;
        c.selected = "slack".into();
        let s = c.svcs.get_mut("slack").unwrap();
        s.loaded = true;
        s.unread = 3;
        let mut acts = vec![];
        c.set_enabled(services::service("slack").unwrap(), false, &mut acts);
        assert!(acts.iter().any(|a| matches!(a, Action::Close("slack"))));
        assert_eq!(c.svcs["slack"].unread, 0);
        assert!(!c.svcs["slack"].loaded);
        assert_ne!(c.selected, "slack", "selection moves to a service that is still on");
        assert!(c.settings.disabled.contains(&"slack".to_string()));
        let _ = std::fs::remove_dir_all(dir);
    }


    /// Records what it is asked to do and can be made slow, standing in for a poller busy on the network.
    struct TestPoller {
        calls: Arc<Mutex<Vec<String>>>,
        poll_takes: Duration,
    }
    impl Poller for TestPoller {
        fn poll(&mut self) -> Option<PollResult> {
            std::thread::sleep(self.poll_takes);
            self.calls.lock().unwrap().push("poll".into());
            None
        }
        fn needs_credentials(&self) -> bool { false }
        fn signed_out(&self) -> bool { false }
        fn set_credentials(&mut self, _: Credentials) {}
        fn reset(&mut self) { self.calls.lock().unwrap().push("reset".into()); }
    }

    fn test_poller(poll_takes: Duration) -> (PollerHandle, Arc<Mutex<Vec<String>>>) {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let handle = PollerHandle::spawn("test", Box::new(TestPoller { calls: calls.clone(), poll_takes }), |_, _| {});
        (handle, calls)
    }

    #[test]
    fn a_poller_busy_on_the_network_never_blocks_the_core() {
        // A poll can take many seconds. Everything the app's main thread does goes through the core, so if the core ever
        // waited for a busy poller, a slow request would freeze every click and menu.
        let mut c = core();
        let (poller, calls) = test_poller(Duration::from_secs(4));
        c.svcs.get_mut("slack").unwrap().poller = Some(poller.clone());
        c.svcs.get_mut("slack").unwrap().needs_creds = false;
        poller.poll(0);   // now busy for 4 seconds

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let slack = services::service("slack").unwrap();
            let mut acts = vec![];
            c.tick(Instant::now(), &mut acts);
            c.set_enabled(slack, false, &mut acts);
            c.set_enabled(slack, true, &mut acts);
            let _ = c.view();
            c.refresh_visibility(&mut acts);
            done_tx.send(()).unwrap();
        });
        done_rx.recv_timeout(Duration::from_secs(2)).expect("the core waited for a busy poller");
        drop(calls);
    }

    #[test]
    fn turning_a_service_back_on_resets_its_poller_before_any_later_poll() {
        // Re-enabling must forget what was seen before the next poll runs, or existing unread items would be announced as
        // new. The reset is sent under the core lock, so the poll that follows (which the scheduler can only request after
        // that) is handled after it.
        let mut c = core();
        let (poller, calls) = test_poller(Duration::ZERO);
        c.svcs.get_mut("gmail").unwrap().poller = Some(poller.clone());
        let gmail = services::service("gmail").unwrap();
        let mut acts = vec![];
        c.set_enabled(gmail, false, &mut acts);
        c.set_enabled(gmail, true, &mut acts);
        assert!(c.svcs["gmail"].next_poll <= Instant::now(), "a poll is due straight away");
        poller.poll(c.svcs["gmail"].poll_gen);   // what the scheduler does next
        let until = Instant::now() + Duration::from_secs(3);
        while calls.lock().unwrap().len() < 3 && Instant::now() < until { std::thread::sleep(Duration::from_millis(5)); }
        assert_eq!(*calls.lock().unwrap(), vec!["reset", "reset", "poll"]);
        assert_eq!(c.svcs["gmail"].poll_gen, 2, "each change bumps the generation");
    }

    #[test]
    fn an_answer_from_before_a_reconfiguration_is_ignored() {
        let mut c = core();
        let s = c.svcs.get_mut("gmail").unwrap();
        s.poll_inflight = true;
        s.poll_gen = 3;   // reconfigured since the poll below was requested under generation 2
        let item = NotificationItem { title: "Someone".into(), body: "hello".into(), ..Default::default() };
        let stale = PollResult { unread: 5, new_items: vec![item.clone()], quiet_unread: 0 };
        let mut acts = vec![];
        let shown = c.poll_finished("gmail", 2, Some(stale), false, false, &mut acts);
        assert!(shown.is_empty(), "stale items must not become banners");
        assert_eq!(c.svcs["gmail"].unread, 0, "stale counts are not applied");
        assert!(!c.svcs["gmail"].poll_inflight, "but the service can be polled again");

        let fresh = PollResult { unread: 5, new_items: vec![item.clone()], quiet_unread: 0 };
        let shown = c.poll_finished("gmail", 3, Some(fresh), false, false, &mut acts);
        assert_eq!(shown, vec![item]);
        assert_eq!(c.svcs["gmail"].unread, 5);
    }

    #[test]
    fn turned_off_services_do_nothing() {
        let mut c = core();
        c.svcs.get_mut("slack").unwrap().enabled = false;
        let mut acts = vec![];
        c.tick(Instant::now(), &mut acts);
        assert!(!has_load(&acts, "slack"));
        assert!(!c.enabled_ids().contains(&"slack"));
    }

    #[test]
    fn a_page_that_never_loads_shows_cant_reach() {
        let mut c = core();
        let s = c.svcs.get_mut("gmail").unwrap();
        s.loaded = true;
        s.load_started = Instant::now() - Duration::from_secs(30);
        let mut acts = vec![];
        c.tick_service("gmail", Instant::now(), &mut acts);
        assert_eq!(c.svcs["gmail"].error.as_deref(), Some("Can't reach Gmail"));
    }
}
