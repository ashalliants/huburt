//! macOS-specific bits: opening links in the default browser, banners, start-at-login.
use crate::{hub::Hub, services::Service, types::NotificationItem};
use std::sync::Arc;

/// Opens a link in the default browser.
pub fn open_url(url: &str) {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
    }
}

pub fn post_banner(_hub: &Arc<Hub>, svc: &Service, item: &NotificationItem) {
    crate::notify_banner(svc, item);
}

pub fn post_greeting(_hub: &Arc<Hub>, title: &str, body: &str) {
    crate::notify_greeting(title, body);
}

// MARK: start at login (a LaunchAgent that starts Hub hidden in the menu bar)

const LOGIN_LABEL: &str = "local.huburt.login";

fn login_plist_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(format!("Library/LaunchAgents/{LOGIN_LABEL}.plist"))
}

pub fn login_item_enabled() -> bool { login_plist_path().exists() }

pub fn set_login_item(on: bool) {
    let path = login_plist_path();
    if on {
        let bundle = std::env::current_exe().ok().and_then(|p| p.ancestors().nth(3).map(|a| a.to_path_buf()));
        let Some(bundle) = bundle else { return };
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LOGIN_LABEL}</string>
<key>ProgramArguments</key><array><string>/usr/bin/open</string><string>-a</string><string>{}</string><string>--args</string><string>--hidden</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
"#,
            bundle.display()
        );
        if let Some(dir) = path.parent() { let _ = std::fs::create_dir_all(dir); }
        let _ = std::fs::write(&path, plist);   // picked up at next login; not started now
    } else {
        let uid = unsafe_uid();
        let _ = std::process::Command::new("/bin/launchctl").args(["bootout", &format!("gui/{uid}/{LOGIN_LABEL}")]).status();
        let _ = std::fs::remove_file(&path);
    }
    crate::log(&format!("start at login: {}", if on { "on" } else { "off" }));
}

fn unsafe_uid() -> String {
    std::process::Command::new("/usr/bin/id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}
