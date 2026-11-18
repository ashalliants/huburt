//! Native menus: the menu bar icon's right-click menu, the service buttons' right-click menu, and the app menu.
//! Every item id is dispatched by `handle` (prefix says which kind of item it is).
use crate::{hub::Hub, native, services, webviews};
use std::sync::Arc;
use tauri::{
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
    AppHandle, LogicalPosition, Manager,
};

fn now_unix() -> f64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0) }

fn tomorrow_morning() -> f64 {
    use chrono::{Duration, Local, TimeZone};
    let now = Local::now();
    let eight = now.date_naive().and_hms_opt(8, 0, 0).and_then(|t| Local.from_local_datetime(&t).single());
    let target = match eight { Some(t) if t > now => t, Some(t) => t + Duration::days(1), None => now + Duration::hours(12) };
    target.timestamp() as f64
}

fn sep(app: &AppHandle) -> tauri::Result<PredefinedMenuItem<tauri::Wry>> { PredefinedMenuItem::separator(app) }

/// The menu bar icon's right-click menu.
pub fn tray_menu(hub: &Arc<Hub>) -> tauri::Result<Menu<tauri::Wry>> {
    let app = &hub.app;
    let (lines, has_badge, paused, login) = {
        let c = hub.core.lock().unwrap();
        let lines: Vec<(String, String)> = c.order.iter().filter_map(|id| c.svcs.get(id.as_str())).filter(|s| s.enabled)
            .map(|s| (s.def.id.to_string(), format!("{}  —  {}", s.def.name, c.status_text(s)))).collect();
        (lines, c.tray_count() > 0, c.is_paused(), native::login_item_enabled())
    };
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = vec![];
    // Quick switch: jump straight to a service, with what's waiting in it
    for (id, text) in lines { items.push(Box::new(MenuItem::with_id(app, format!("tray:open:{id}"), text, true, None::<&str>)?)); }
    items.push(Box::new(MenuItem::with_id(app, "tray:open", "Open Huburt", true, None::<&str>)?));
    if has_badge {
        items.push(Box::new(sep(app)?));
        items.push(Box::new(MenuItem::with_id(app, "tray:dismiss", "Hide Red Dot Until Something New", true, None::<&str>)?));
    }
    items.push(Box::new(sep(app)?));
    if paused {
        items.push(Box::new(MenuItem::with_id(app, "tray:resume", "Resume Notifications", true, None::<&str>)?));
    } else {
        let a = MenuItem::with_id(app, "tray:pause1h", "For 1 hour", true, None::<&str>)?;
        let b = MenuItem::with_id(app, "tray:pauseam", "Until tomorrow morning (8:00)", true, None::<&str>)?;
        items.push(Box::new(Submenu::with_items(app, "Pause Notifications", true, &[&a, &b])?));
    }
    items.push(Box::new(sep(app)?));
    items.push(Box::new(CheckMenuItem::with_id(app, "tray:login", "Start Huburt at Login", true, login, None::<&str>)?));
    items.push(Box::new(sep(app)?));
    items.push(Box::new(MenuItem::with_id(app, "tray:quit", "Quit Huburt", true, None::<&str>)?));
    let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Menu::with_items(app, &refs)
}

/// Right-click on a service button.
pub fn show_service_menu(hub: &Arc<Hub>, id: &str, x: f64, y: f64) {
    let Some(def) = services::service(id) else { return };
    let app = &hub.app;
    let (enabled, loaded, muted, gmail_src, slack_mode) = {
        let c = hub.core.lock().unwrap();
        let s = &c.svcs[def.id];
        (s.enabled, s.loaded, c.is_muted(def.id), c.settings.gmail_source.clone(), c.settings.slack_online_mode.clone())
    };
    let build = || -> tauri::Result<Menu<tauri::Wry>> {
        let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = vec![];
        if enabled {
            let label = if loaded { format!("Reload {}", def.name) } else { format!("Wake Up {}", def.name) };
            items.push(Box::new(MenuItem::with_id(app, format!("svc:reload:{id}"), label, true, None::<&str>)?));
            if loaded {
                items.push(Box::new(MenuItem::with_id(app, format!("svc:home:{id}"), format!("Back to {} Home", def.name), true, None::<&str>)?));
            }
            if muted {
                items.push(Box::new(MenuItem::with_id(app, format!("svc:unmute:{id}"), "Unmute Notifications", true, None::<&str>)?));
            } else {
                let a = MenuItem::with_id(app, format!("svc:mute1h:{id}"), "For 1 hour", true, None::<&str>)?;
                let b = MenuItem::with_id(app, format!("svc:muteam:{id}"), "Until tomorrow morning (8:00)", true, None::<&str>)?;
                items.push(Box::new(Submenu::with_items(app, "Mute Notifications", true, &[&a, &b])?));
            }
            if id == "slack" {
                let opts = [("off", "Off (Slack decides)"), ("work", "During work hours (Mon–Fri, 8am–6pm)"), ("always", "Always")];
                let ms: Vec<CheckMenuItem<tauri::Wry>> = opts.iter()
                    .map(|(k, t)| CheckMenuItem::with_id(app, format!("svc:online:{k}"), *t, true, slack_mode == *k, None::<&str>)).collect::<Result<_, _>>()?;
                let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = ms.iter().map(|m| m as &dyn IsMenuItem<tauri::Wry>).collect();
                items.push(Box::new(Submenu::with_items(app, "Show as Online", true, &refs)?));
            }
            if id == "gmail" {
                let opts = [("important", "Important unread (matches Gmail's Inbox number)"), ("all", "All unread (includes promotions and updates)")];
                let ms: Vec<CheckMenuItem<tauri::Wry>> = opts.iter()
                    .map(|(k, t)| CheckMenuItem::with_id(app, format!("svc:gmailsrc:{k}"), *t, true, gmail_src == *k, None::<&str>)).collect::<Result<_, _>>()?;
                let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = ms.iter().map(|m| m as &dyn IsMenuItem<tauri::Wry>).collect();
                items.push(Box::new(Submenu::with_items(app, "Number on the Badge", true, &refs)?));
            }
            items.push(Box::new(sep(app)?));
            items.push(Box::new(MenuItem::with_id(app, format!("svc:off:{id}"), format!("Turn Off {}", def.name), true, None::<&str>)?));
        } else {
            items.push(Box::new(MenuItem::with_id(app, format!("svc:on:{id}"), format!("Turn On {}", def.name), true, None::<&str>)?));
        }
        let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|b| b.as_ref()).collect();
        Menu::with_items(app, &refs)
    };
    let (Ok(menu), Some(win)) = (build(), app.get_window("main")) else { return };
    let _ = tauri::menu::ContextMenu::popup_at(&menu, win, LogicalPosition::new(x, y));
}

/// The app menu (shown while the window is open): keyboard shortcuts and the Edit commands.
pub fn app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let hide = MenuItem::with_id(app, "app:hide", "Hide Huburt to Menu Bar", true, Some("CmdOrCtrl+H"))?;
    let quit = MenuItem::with_id(app, "tray:quit", "Quit Huburt", true, None::<&str>)?;
    let hub_menu = Submenu::with_items(app, "Huburt", true, &[&hide, &sep(app)?, &quit])?;
    let edit = Submenu::with_items(app, "Edit", true, &[
        &PredefinedMenuItem::undo(app, None)?, &PredefinedMenuItem::redo(app, None)?, &sep(app)?,
        &PredefinedMenuItem::cut(app, None)?, &PredefinedMenuItem::copy(app, None)?, &PredefinedMenuItem::paste(app, None)?,
        &PredefinedMenuItem::select_all(app, None)?,
    ])?;
    let mut svc_items: Vec<MenuItem<tauri::Wry>> = vec![];
    for (i, s) in services::SERVICES.iter().enumerate() {
        svc_items.push(MenuItem::with_id(app, format!("app:select:{i}"), s.name, true, Some(format!("CmdOrCtrl+{}", i + 1)))?);
    }
    let reload = MenuItem::with_id(app, "app:reload", "Reload", true, Some("CmdOrCtrl+R"))?;
    let close = MenuItem::with_id(app, "app:hide", "Close Window", true, Some("CmdOrCtrl+W"))?;
    let mut refs: Vec<&dyn IsMenuItem<tauri::Wry>> = svc_items.iter().map(|m| m as &dyn IsMenuItem<tauri::Wry>).collect();
    let sp = sep(app)?;
    refs.push(&sp);
    refs.push(&reload);
    refs.push(&close);
    let view = Submenu::with_items(app, "Services", true, &refs)?;
    Menu::with_items(app, &[&hub_menu, &edit, &view])
}

/// Every menu click lands here.
pub fn handle(hub: &Arc<Hub>, e: &MenuEvent) {
    let id = e.id().0.as_str();
    let parts: Vec<&str> = id.splitn(3, ':').collect();
    match parts.as_slice() {
        ["tray", "open"] => webviews::show_window(hub, true),
        ["tray", "open", sid] => { hub.select(sid); webviews::show_window(hub, false); }
        ["tray", "dismiss"] => hub.dismiss_tray_badge(),
        ["tray", "pause1h"] => hub.pause(Some(now_unix() + 3600.0)),
        ["tray", "pauseam"] => hub.pause(Some(tomorrow_morning())),
        ["tray", "resume"] => hub.pause(None),
        ["tray", "login"] => native::set_login_item(!native::login_item_enabled()),
        ["tray", "quit"] => hub.app.exit(0),
        ["app", "hide"] => webviews::hide_window(hub),
        ["app", "select", i] => { if let Ok(i) = i.parse() { hub.select_index(i); } }
        ["app", "reload"] => { let sel = hub.core.lock().unwrap().selected.clone(); hub.reload(&sel); }
        ["svc", "reload", sid] => hub.reload(sid),
        ["svc", "home", sid] => { if let Some(d) = services::service(sid) { hub.navigate(sid, d.url); } }
        ["svc", "mute1h", sid] => hub.mute(sid, Some(now_unix() + 3600.0)),
        ["svc", "muteam", sid] => hub.mute(sid, Some(tomorrow_morning())),
        ["svc", "unmute", sid] => hub.mute(sid, None),
        ["svc", "off", sid] => hub.set_enabled(sid, false),
        ["svc", "on", sid] => hub.set_enabled(sid, true),
        ["svc", "online", mode] => hub.set_slack_online_mode(mode),
        ["svc", "gmailsrc", src] => hub.set_gmail_source(src),
        _ => {}
    }
}
