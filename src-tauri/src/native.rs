//! Per-platform bits: where files live, opening links in the default browser, banners, start-at-login, the Dock icon.
//! Everything that differs between macOS and Windows is here (or in `banner.rs`).
use crate::{hub::Hub, services::Service, types::NotificationItem};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

// MARK: where things live

/// The user's home folder.
pub fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    PathBuf::from(std::env::var(var).unwrap_or_default())
}

/// Settings live here (`HUB_SETTINGS_DIR` overrides it, for tests).
/// macOS: ~/Library/Application Support/Huburt. Windows: %APPDATA%\Huburt.
pub fn settings_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HUB_SETTINGS_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(windows) {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_default()).join("Huburt")
    } else {
        home_dir().join("Library/Application Support/Huburt")
    }
}

/// The log file. macOS: ~/Library/Logs/Huburt.log. Windows: %LOCALAPPDATA%\Huburt\Huburt.log. `HUB_LOG_FILE` overrides it
/// (for testing a second copy without mixing its log with the one you're using).
pub fn log_path() -> PathBuf {
    if let Ok(p) = std::env::var("HUB_LOG_FILE") {
        return PathBuf::from(p);
    }
    if cfg!(windows) {
        PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join("Huburt").join("Huburt.log")
    } else {
        home_dir().join("Library/Logs/Huburt.log")
    }
}

pub fn downloads_dir() -> PathBuf { home_dir().join("Downloads") }

/// True when running from an installed location (not a build folder): first run there turns on start-at-login.
pub fn is_installed() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    installed_location(&exe, &std::env::var("LOCALAPPDATA").unwrap_or_default(), &std::env::var("ProgramFiles").unwrap_or_default())
}

fn installed_location(exe: &Path, local_app_data: &str, program_files: &str) -> bool {
    if cfg!(windows) {
        let under = |dir: &str| !dir.is_empty() && exe.starts_with(dir);
        under(program_files) || (!local_app_data.is_empty() && exe.starts_with(Path::new(local_app_data).join("Huburt")))
    } else {
        exe.starts_with("/Applications/")
    }
}

// MARK: Dock / taskbar

/// macOS: Huburt shows in the Dock while its window is open and lives only in the menu bar otherwise.
/// Windows has no equivalent (the taskbar button follows the window), so this does nothing there.
pub fn set_dock_icon_visible(app: &tauri::AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.set_activation_policy(if visible { tauri::ActivationPolicy::Regular } else { tauri::ActivationPolicy::Accessory });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, visible);
    }
}

// MARK: links

/// Only these kinds of link are ever handed to the operating system.
fn is_openable(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")
}

/// Opens a link in the default browser.
pub fn open_url(url: &str) {
    if !is_openable(url) {
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
    #[cfg(windows)]
    {
        // rundll32 takes the URL as one argument (no shell, so characters like & in the URL are safe)
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", url]).creation_flags(CREATE_NO_WINDOW).spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn post_banner(_hub: &Arc<Hub>, svc: &Service, item: &NotificationItem) { crate::notify_banner(svc, item); }

pub fn post_greeting(_hub: &Arc<Hub>, title: &str, body: &str) { crate::notify_greeting(title, body); }

// MARK: start at login

#[cfg(any(target_os = "macos", test))]
const LOGIN_LABEL: &str = "local.huburt.login";

/// macOS: a LaunchAgent that starts Huburt hidden in the menu bar.
#[cfg(any(target_os = "macos", test))]
fn login_plist(bundle: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LOGIN_LABEL}</string>
<key>ProgramArguments</key><array><string>/usr/bin/open</string><string>-a</string><string>{bundle}</string><string>--args</string><string>--hidden</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
"#
    )
}

/// Windows: the value of the Run registry key that starts Huburt hidden in the notification area.
#[cfg(any(windows, test))]
fn run_key_value(exe: &str) -> String { format!("\"{exe}\" --hidden") }

#[cfg(target_os = "macos")]
mod login {
    use super::*;

    fn plist_path() -> PathBuf { home_dir().join(format!("Library/LaunchAgents/{LOGIN_LABEL}.plist")) }

    pub fn enabled() -> bool { plist_path().exists() }

    pub fn set(on: bool) {
        let path = plist_path();
        if on {
            let bundle = std::env::current_exe().ok().and_then(|p| p.ancestors().nth(3).map(|a| a.to_path_buf()));
            let Some(bundle) = bundle else { return };
            if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
            let _ = std::fs::write(&path, login_plist(&bundle.display().to_string()));   // picked up at next login; not started now
        } else {
            let uid = std::process::Command::new("/usr/bin/id").arg("-u").output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
            let _ = std::process::Command::new("/bin/launchctl").args(["bootout", &format!("gui/{uid}/{LOGIN_LABEL}")]).status();
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(windows)]
mod login {
    use super::*;
    use std::os::windows::process::CommandExt;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    fn reg(args: &[&str]) -> bool {
        std::process::Command::new("reg.exe").args(args).creation_flags(CREATE_NO_WINDOW).output().map(|o| o.status.success()).unwrap_or(false)
    }

    pub fn enabled() -> bool { reg(&["query", RUN_KEY, "/v", "Huburt"]) }

    pub fn set(on: bool) {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            reg(&["add", RUN_KEY, "/v", "Huburt", "/t", "REG_SZ", "/d", &run_key_value(&exe.display().to_string()), "/f"]);
        } else {
            reg(&["delete", RUN_KEY, "/v", "Huburt", "/f"]);
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod login {
    pub fn enabled() -> bool { false }
    pub fn set(_on: bool) {}
}

pub fn login_item_enabled() -> bool { login::enabled() }

pub fn set_login_item(on: bool) {
    login::set(on);
    crate::log(&format!("start at login: {}", if on { "on" } else { "off" }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_links_are_handed_to_the_system() {
        assert!(is_openable("https://github.com/a?b=1&c=2"));
        assert!(is_openable("mailto:someone@example.com"));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("ms-settings:privacy"));
        assert!(!is_openable("C:\\Windows\\System32\\calc.exe"));
    }

    #[test]
    fn login_entries_start_huburt_hidden() {
        let plist = login_plist("/Applications/Huburt.app");
        assert!(plist.contains("<string>/Applications/Huburt.app</string>") && plist.contains("--hidden") && plist.contains("RunAtLoad"));
        assert_eq!(run_key_value(r"C:\Users\me\AppData\Local\Huburt\huburt.exe"), r#""C:\Users\me\AppData\Local\Huburt\huburt.exe" --hidden"#);
    }

    #[test]
    fn build_folders_are_not_installed_locations() {
        if cfg!(windows) {
            assert!(installed_location(Path::new(r"C:\Users\me\AppData\Local\Huburt\huburt.exe"), r"C:\Users\me\AppData\Local", r"C:\Program Files"));
            assert!(installed_location(Path::new(r"C:\Program Files\Huburt\huburt.exe"), r"C:\Users\me\AppData\Local", r"C:\Program Files"));
            assert!(!installed_location(Path::new(r"C:\src\huburt\target\release\huburt.exe"), r"C:\Users\me\AppData\Local", r"C:\Program Files"));
        } else {
            assert!(installed_location(Path::new("/Applications/Huburt.app/Contents/MacOS/huburt"), "", ""));
            assert!(!installed_location(Path::new("/Users/me/src/huburt/target/release/huburt"), "", ""));
        }
    }
}

// MARK: macOS keyboard handling in child web views

/// Tauri puts each page in a child web view, and on macOS two things go wrong with the keyboard. Both are fixed here by
/// adding or replacing a method on Tauri's own classes, because Tauri doesn't offer a hook for either:
///
/// - **A hidden page takes the keyboard.** A page that is only doing a background check (WhatsApp waking up, Gmail) runs its
///   own script, which focuses a box on its page; WebKit then asks the window to make that page the keyboard's owner, and the
///   text cursor vanishes from the page you are typing in (and the window is left with nothing focused once that page closes).
///   The window now refuses to give the keyboard to a page that is hidden or parked off-screen (`webviews::OFFSCREEN_X`).
/// - **An unhandled key becomes text.** A key the page doesn't handle (Right Arrow while a macOS inline suggestion is showing)
///   falls through to the window's own view (tao), which types it into the page as the control character U+001D, shown as an
///   empty square. That view now ignores keys. Menu shortcuts don't go through it, so they are unaffected.
///
/// Safe to call repeatedly; it patches once, as soon as the classes exist (they are created with the first web view).
#[cfg(target_os = "macos")]
pub fn patch_key_handling() {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use objc2::{msg_send, sel};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    extern "C" {
        fn class_addMethod(cls: *const AnyClass, name: Sel, imp: *const (), types: *const std::ffi::c_char) -> Bool;
    }

    static DONE: AtomicBool = AtomicBool::new(false);
    static NSWINDOW_MAKE_FIRST_RESPONDER: AtomicUsize = AtomicUsize::new(0);
    if DONE.load(Ordering::SeqCst) { return; }

    extern "C-unwind" fn ignore_key(_: *mut AnyObject, _: Sel, _: *mut AnyObject) {}

    extern "C-unwind" fn make_first_responder(this: *mut AnyObject, cmd: Sel, responder: *mut AnyObject) -> Bool {
        if !responder.is_null() {
            if let Some(page_class) = AnyClass::get(c"WKWebView") {
                let is_page: bool = unsafe { msg_send![&*responder, isKindOfClass: page_class] };
                if is_page {
                    let hidden: bool = unsafe { msg_send![&*responder, isHiddenOrHasHiddenAncestor] };
                    let frame: objc2_foundation::NSRect = unsafe { msg_send![&*responder, frame] };
                    if hidden || frame.origin.x < -1000.0 { return Bool::NO; }
                }
            }
        }
        let imp = NSWINDOW_MAKE_FIRST_RESPONDER.load(Ordering::SeqCst);
        if imp == 0 { return Bool::NO; }
        let original: extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool = unsafe { std::mem::transmute(imp) };
        original(this, cmd, responder)
    }

    let (Some(window), Some(ns_window), Some(view)) =
        (AnyClass::get(c"TaoWindow"), AnyClass::get(c"NSWindow"), AnyClass::get(c"TaoView")) else { return };
    let make_first = sel!(makeFirstResponder:);
    let Some(original) = ns_window.instance_method(make_first) else { return };
    NSWINDOW_MAKE_FIRST_RESPONDER.store(original.implementation() as usize, Ordering::SeqCst);
    let guard = unsafe { class_addMethod(window, make_first, make_first_responder as *const (), c"B@:@".as_ptr()) }.as_bool();

    let key_down = view.instance_method(sel!(keyDown:)).map(|m| unsafe {
        m.set_implementation(std::mem::transmute::<*const (), objc2::runtime::Imp>(ignore_key as *const ()));
    }).is_some();

    DONE.store(guard && key_down, Ordering::SeqCst);
    crate::log(&format!("macOS keyboard patches: hidden pages refused the keyboard={guard}, unhandled keys ignored={key_down}"));
}

#[cfg(not(target_os = "macos"))]
pub fn patch_key_handling() {}
