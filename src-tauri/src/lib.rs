//! Huburt: Slack, Gmail and WhatsApp in one window, with a tiny memory footprint (Tauri port of the Swift app).
//! This is the real app, built as `libhuburt_core.dylib` and loaded by the tiny launcher in `main.rs`.

mod banner;
mod gmail;
mod http;
mod hub;
mod menus;
mod native;
mod poller_actor;
mod pollers;
mod services;
mod settings;
mod slack;
mod tray;
mod types;
mod ui_thread;
mod webviews;

use hub::Hub;
use std::{io::Write, sync::Arc, time::Duration};
use tauri::{Manager, State};

pub use hub::MAX_BANNERS;

/// The log file, see `native::log_path` (counts and status only; never message text or tokens)
pub fn log(msg: &str) {
    if cfg!(test) { return; }   // tests must never write to the real log
    let path = native::log_path();
    if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        // one write per line (append mode keeps it whole), so lines from different threads can't be mixed together
        let line = format!("{} {msg}\n", chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"));
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn notify_banner(svc: &services::Service, item: &types::NotificationItem) {
    let mut info = item.info.clone();
    info.insert("service".into(), svc.id.into());
    if let Some(u) = &item.url { info.insert("url".into(), u.clone()); }
    banner::post(banner::Banner {
        title: item.title.clone(),
        subtitle: svc.name.into(),
        body: item.body.clone(),
        category: item.category.clone(),
        user_info: info,
        identifier: None,
    });
}
pub fn notify_greeting(title: &str, body: &str) {
    banner::post(banner::Banner {
        title: title.into(), body: body.into(),
        user_info: [("greeting".to_string(), "1".to_string())].into(), ..Default::default()
    });
}

/// A banner was clicked, or one of its buttons pressed.
fn banner_event(hub: &Arc<Hub>, e: banner::Event) {
    match e {
        banner::Event::Action { action, info, text } => hub.handle_action(action, info, text),
        banner::Event::Open { info } => {
            // greeting: open on whatever needs you most; otherwise on the service the banner was about
            if info.contains_key("greeting") { webviews::show_window(hub, true); }
            if let Some(id) = info.get("service") {
                hub.select(id);
                webviews::show_window(hub, false);
                // Jump straight to the message or conversation the banner was about
                if let Some(u) = info.get("url") { hub.navigate(id, u); }
            }
        }
    }
}

// MARK: commands the UI calls

type H<'a> = State<'a, Arc<Hub>>;

#[tauri::command]
fn get_state(hub: H) -> hub::StateView { hub.core.lock().unwrap().view() }
#[tauri::command]
fn select(hub: H, id: String) { hub.select(&id); }
#[tauri::command]
fn reorder(hub: H, order: Vec<String>) { hub.reorder(order); }
#[tauri::command]
fn show_menu(hub: H, id: String, x: f64, y: f64) { menus::show_service_menu(&hub, &id, x, y); }
#[tauri::command]
fn reload(hub: H, id: String) { hub.reload(&id); }
#[tauri::command]
fn set_enabled(hub: H, id: String, enabled: bool) { hub.set_enabled(&id, enabled); }

// MARK: commands the service pages call (allowed for remote origins in capabilities/remote.json)

#[tauri::command]
fn page_notify(hub: H, service: String, title: String, body: String) {
    if let Some(def) = services::service(&service) { hub.page_notify(def.id, title, body); }
}
#[tauri::command]
fn page_probe(hub: H, service: String, kind: String, value: serde_json::Value) {
    if let Some(def) = services::service(&service) { hub.page_probe(def.id, &kind, value); }
}
#[tauri::command]
fn open_external(url: String) { native::open_url(&url); }

/// Keyboard shortcuts on platforms where there's no app menu to own them (Windows): the pages report them here.
/// kind: "select" (n = 1-based position in the sidebar), "reload" (the selected service) or "hide" (to the tray).
#[tauri::command]
fn shortcut(hub: H, kind: String, n: Option<usize>) {
    match kind.as_str() {
        "select" => { if let Some(n) = n.filter(|n| *n >= 1) { hub.select_index(n - 1); } }
        "reload" => { let sel = hub.core.lock().unwrap().selected.clone(); hub.reload(&sel); }
        "hide" => webviews::hide_window(&hub),
        _ => {}
    }
}

/// Entry point, called by the launcher.
#[no_mangle]
pub extern "C" fn huburt_main() {
    run();
}

pub fn run() {
    let hidden = std::env::args().any(|a| a == "--hidden");
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_state, select, reorder, show_menu, reload, set_enabled, page_notify, page_probe, open_external, shortcut
        ])
        .on_menu_event(|app, e| {
            if let Some(hub) = app.try_state::<Arc<Hub>>() { menus::handle(&hub, &e); }
        })
        .setup(move |app| {
            let hub = Hub::new(app.handle().clone());
            app.manage(hub.clone());
            native::set_dock_icon_visible(app.handle(), false);   // menu bar only until the window opens
            webviews::ensure_window(&hub);
            let bh = hub.clone();
            banner::init(Box::new(move |e| banner_event(&bh, e)));
            // macOS: the app menu owns the keyboard shortcuts. Elsewhere there is no menu bar in the window; see `shortcut`.
            #[cfg(target_os = "macos")]
            app.set_menu(menus::app_menu(app.handle())?)?;
            tray::init(&hub)?;
            hub.start();
            hub.update_tray();
            if !hidden { webviews::show_window(&hub, true); }
            // First run from /Applications: start at login by default (can be switched off from the menu bar icon)
            let first_run = native::is_installed() && {
                let mut c = hub.core.lock().unwrap();
                let first = !c.settings.login_item_configured;
                if first { c.settings.login_item_configured = true; }
                first
            };
            if first_run { hub.save_settings(); }
            if first_run { native::set_login_item(true); }
            spawn_test_hooks(hub);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error running hub");
}

/// Test aids: HUB_TEST_STEPS="select:slack:20,hide:20,show:5" walks through states for memory measurements;
/// HUB_TEST_GREETING=1 sends the daily greeting now.
fn spawn_test_hooks(hub: Arc<Hub>) {
    if std::env::var("HUB_TEST_GREETING").is_ok() {
        let h = hub.clone();
        std::thread::spawn(move || { std::thread::sleep(Duration::from_secs(8)); h.greet_now(); });
    }
    let Ok(steps) = std::env::var("HUB_TEST_STEPS") else { return };
    std::thread::spawn(move || {
        for step in steps.split(',') {
            let parts: Vec<&str> = step.split(':').collect();
            let secs: u64 = parts.last().and_then(|s| s.parse().ok()).unwrap_or(10);
            let h = hub.clone();
            let p: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
            let _ = hub.app.run_on_main_thread(move || match p[0].as_str() {
                "hide" => webviews::hide_window(&h),
                "show" => webviews::show_window(&h, false),
                "select" => h.select(&p[1]),
                "online" => h.set_slack_online_mode(&p[1]),
                "probe" => { if let Some(d) = services::service(&p[1]) { webviews::eval(&h, d.id, &services::probe_js(d.id, "test", "1 + 1")); } }
                "disable" => h.set_enabled(&p[1], false),
                "enable" => h.set_enabled(&p[1], true),
                _ => {}
            });
            log(&format!("TEST step {step}"));
            std::thread::sleep(Duration::from_secs(secs));
        }
    });
}
