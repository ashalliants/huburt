//! The window and its web views: one local UI web view (sidebar + covers) and one child web view per loaded service.
use crate::{
    hub::Hub,
    native,
    services::{self, SIGN_IN_HOSTS},
    types::Cookie,
};
use std::{path::PathBuf, sync::Arc};
use tauri::{
    webview::{DownloadEvent, NewWindowResponse, PageLoadEvent, WebviewBuilder},
    window::WindowBuilder,
    LogicalPosition, LogicalSize, Manager, WebviewUrl, WindowEvent,
};

pub const SIDEBAR_W: f64 = 80.0;
const OFFSCREEN_X: f64 = -20000.0;

fn label(id: &str) -> String { format!("svc-{id}") }

pub fn ensure_window(hub: &Arc<Hub>) -> tauri::Window {
    if let Some(w) = hub.app.get_window("main") {
        return w;
    }
    let win = WindowBuilder::new(&hub.app, "main")
        .title("Huburt")
        .inner_size(1100.0, 760.0)
        .min_inner_size(520.0, 360.0)
        .center()
        .build()
        .expect("window");
    let h = hub.clone();
    win.on_window_event(move |e| match e {
        WindowEvent::Resized(_) => layout(&h),
        WindowEvent::Focused(f) => {
            h.core.lock().unwrap().window_focused = *f;
        }
        WindowEvent::CloseRequested { api, .. } => {
            // The red cross hides the window; only Quit really quits
            api.prevent_close();
            hide_window(&h);
        }
        _ => {}
    });
    win
}

/// Window open: Huburt shows in the Dock. Window hidden: it lives only in the menu bar.
pub fn show_window(hub: &Arc<Hub>, pick_urgent: bool) {
    let was_visible = hub.core.lock().unwrap().window_visible;
    if pick_urgent && !was_visible {
        hub.select_most_urgent();
    }
    native::set_dock_icon_visible(&hub.app, true);
    let win = ensure_window(hub);
    let _ = win.show();
    let _ = win.set_focus();
    ensure_ui(hub, &win);
    hub.set_window_visible(true);
    // The UI web view was just (re)created and sits above any service web views that survived: bring them back on top
    for id in services::SERVICES.iter().map(|s| s.id) {
        if let Some(wv) = hub.app.get_webview(&label(id)) {
            let _ = wv.reparent(&win);
        }
    }
    layout(hub);
}

pub fn hide_window(hub: &Arc<Hub>) {
    if let Some(w) = hub.app.get_window("main") {
        let _ = w.hide();
    }
    native::set_dock_icon_visible(&hub.app, false);
    hub.set_window_visible(false);
    // Free the sidebar's web view too: while hidden, only the pages that must stay alive use memory
    if let Some(ui) = hub.app.get_webview("ui") {
        let _ = ui.close();
    }
}

pub fn toggle_window(hub: &Arc<Hub>) {
    let (visible, focused) = { let c = hub.core.lock().unwrap(); (c.window_visible, c.window_focused) };
    if visible && focused { hide_window(hub) } else { show_window(hub, true) }
}

fn ensure_ui(hub: &Arc<Hub>, win: &tauri::Window) {
    if hub.app.get_webview("ui").is_some() {
        return;
    }
    let _ = win.add_child(
        WebviewBuilder::new("ui", WebviewUrl::App("index.html".into())),
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(1100.0, 760.0),
    );
}

/// Puts every web view where it belongs: the UI fills the window; the selected service sits over the content area once
/// its page has drawn (until then it renders off-screen so the "waking up" cover shows); other services are hidden.
pub fn layout(hub: &Arc<Hub>) {
    let Some(win) = hub.app.get_window("main") else { return };
    let Ok(size) = win.inner_size() else { return };
    let scale = win.scale_factor().unwrap_or(1.0);
    let (w, h) = (size.width as f64 / scale, size.height as f64 / scale);
    let (window_visible, selected, states) = {
        let c = hub.core.lock().unwrap();
        let st: Vec<(&'static str, bool, bool)> = c.svcs.values().map(|s| (s.def.id, s.ready, s.error.is_some() || !s.enabled)).collect();
        (c.window_visible, c.selected.clone(), st)
    };
    if let Some(ui) = hub.app.get_webview("ui") {
        let _ = ui.set_position(LogicalPosition::new(0.0, 0.0));
        let _ = ui.set_size(LogicalSize::new(w, h));
    }
    let content = LogicalSize::new((w - SIDEBAR_W).max(1.0), h);
    for (id, ready, covered) in states {
        let Some(wv) = hub.app.get_webview(&label(id)) else { continue };
        if !window_visible || selected != id || covered {
            let _ = wv.hide();
        } else {
            let x = if ready { SIDEBAR_W } else { OFFSCREEN_X };
            let _ = wv.set_position(LogicalPosition::new(x, 0.0));
            let _ = wv.set_size(content);
            let _ = wv.show();
        }
    }
}

/// Clicks on links to other sites open in the default browser (like the Swift app's `linkActivated` handling).
/// The decision mirrors `Service::stays_in_app`: sign-in providers stay in the app only on a sign-in screen.
fn link_guard_js(def: &services::Service) -> String {
    let service_hosts: Vec<&str> = def.hosts.iter().chain(SIGN_IN_HOSTS.iter()).copied().collect();
    let (service_hosts, app_hosts) = (serde_json::to_string(&service_hosts).unwrap(), serde_json::to_string(def.app_hosts).unwrap());
    let on_app = def.app_page_js;
    format!(
        r#"(function () {{
  var serviceHosts = {service_hosts}, appHosts = {app_hosts};
  function matches(h, list) {{ h = (h || '').toLowerCase(); return list.some(function (d) {{ return h === d || h.endsWith('.' + d); }}); }}
  function onApp() {{ try {{ return {on_app}; }} catch (_) {{ return false; }} }}
  function internal(h) {{ return onApp() ? matches(h, appHosts) : matches(h, serviceHosts); }}
  function handle(e) {{
    var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
    if (!a) return;
    var u; try {{ u = new URL(a.href, location.href); }} catch (_) {{ return; }}
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return;
    if (internal(u.hostname)) return;
    e.preventDefault(); e.stopPropagation();
    try {{ window.__TAURI_INTERNALS__.invoke('open_external', {{ url: u.href }}); }} catch (_) {{}}
  }}
  document.addEventListener('click', handle, true);
  document.addEventListener('auxclick', handle, true);
}})();"#
    )
}

pub fn create_service(hub: &Arc<Hub>, id: &'static str) {
    let Some(def) = services::service(id) else { return };
    let win = ensure_window(hub);
    let lbl = label(id);
    if hub.app.get_webview(&lbl).is_some() {
        return;
    }
    let url: tauri::Url = def.url.parse().unwrap();
    let mut b = WebviewBuilder::new(&lbl, WebviewUrl::External(url));
    // Each service keeps its own login, separate from the others
    #[cfg(target_os = "macos")]
    {
        b = b.data_store_identifier(def.store);
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Ok(dir) = hub.app.path().app_data_dir() {
            b = b.data_directory(dir.join("stores").join(def.id));
        }
    }
    if let Some(ua) = def.user_agent {
        b = b.user_agent(ua);
    }
    b = b.initialization_script_for_all_frames(services::notification_shim_js(id));
    b = b.initialization_script(link_guard_js(def));
    if def.darken_with_filter {
        b = b.initialization_script(services::dark_filter_js(def.native_dark_class));
    }
    let h = hub.clone();
    b = b.on_page_load(move |_wv, p| match p.event() {
        PageLoadEvent::Started => h.page_started(id),
        PageLoadEvent::Finished => h.page_finished(id),
    });
    let h = hub.clone();
    b = b.on_document_title_changed(move |_wv, title| h.page_title(id, &title));
    let h = hub.clone();
    b = b.on_new_window(move |url, _| {
        // window.open / target=_blank: stay in the app for the service itself and sign-in providers, otherwise the browser
        let current = h.app.get_webview(&label(id)).and_then(|wv| wv.url().ok());
        let internal = match url.host_str() {
            Some(host) => def.stays_in_app(host, current.as_ref().and_then(|u| u.host_str().map(|h| (h, u.path())))),
            None => true,
        };
        crate::log(&format!("{id} new window internal={internal}"));
        if internal { h.navigate(id, url.as_str()); } else { crate::native::open_url(url.as_str()); }
        NewWindowResponse::Deny
    });
    b = b.on_download(|_wv, event| {
        // Downloads go to ~/Downloads
        if let DownloadEvent::Requested { destination, .. } = event {
            let name = destination.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "download".into());
            *destination = unique_download_path(&name);
        }
        true
    });
    let _ = win.add_child(b, LogicalPosition::new(OFFSCREEN_X, 0.0), LogicalSize::new(800.0, 700.0));
}

fn unique_download_path(name: &str) -> PathBuf {
    let dir = native::downloads_dir();
    let p = std::path::Path::new(name);
    let (stem, ext) = (p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), p.extension().map(|e| e.to_string_lossy().to_string()));
    let mut dest = dir.join(name);
    let mut n = 1;
    while dest.exists() {
        dest = dir.join(match &ext { Some(e) => format!("{stem} {n}.{e}"), None => format!("{stem} {n}") });
        n += 1;
    }
    dest
}

pub fn close_service(hub: &Arc<Hub>, id: &str) {
    if let Some(wv) = hub.app.get_webview(&label(id)) {
        let _ = wv.close();
    }
}

pub fn eval(hub: &Arc<Hub>, id: &str, js: &str) {
    if let Some(wv) = hub.app.get_webview(&label(id)) {
        let _ = wv.eval(js);
    }
}

pub fn reload(hub: &Arc<Hub>, id: &str) {
    if let Some(wv) = hub.app.get_webview(&label(id)) {
        let _ = wv.reload();
    }
}

pub fn navigate(hub: &Arc<Hub>, id: &str, url: &str) {
    if let (Some(wv), Ok(u)) = (hub.app.get_webview(&label(id)), url.parse::<tauri::Url>()) {
        let _ = wv.navigate(u);
    }
}

/// The service's saved cookies (the whole store: `cookies_for_url` only returns cookies set for that exact host and misses
/// the domain-wide login cookies). Needs the page's web view, so only works while it's loaded.
pub fn cookies(hub: &Arc<Hub>, id: &str) -> Vec<Cookie> {
    let (Some(wv), Some(def)) = (hub.app.get_webview(&label(id)), services::service(id)) else { return vec![] };
    let host = def.url.parse::<tauri::Url>().ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
    let Ok(all) = wv.cookies() else { return vec![] };
    if std::env::var("HUB_DEBUG_COOKIES").is_ok() {
        crate::log(&format!("DEBUG {id}: {} cookies in the store", all.len()));
    }
    all.iter().map(|c| Cookie::from_parts(c.name(), c.value(), c.domain(), &host)).collect()
}
