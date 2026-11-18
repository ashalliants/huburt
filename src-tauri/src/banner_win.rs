//! Windows toast notifications with a Reply box and Mark as Read button (the same banners as macOS).
//!
//! Toasts are built from XML (`toast_xml`) and shown through the WinRT toast API. The activation handler is attached to
//! each toast, so a click or button press is routed back while Huburt is running. A toast left in the Action Center from
//! a previous run can't be routed (there is no COM activator registered), so it does nothing when clicked.
use super::{toast_xml, Banner, Event};
use std::{
    collections::HashMap,
    sync::{mpsc, Arc, Mutex, OnceLock},
};
use windows::{
    core::{Interface, HSTRING},
    Data::Xml::Dom::XmlDocument,
    Foundation::{IPropertyValue, TypedEventHandler},
    UI::Notifications::{IToastActivatedEventArgs2, ToastActivatedEventArgs, ToastNotification, ToastNotificationManager},
    Win32::{
        System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
        UI::Shell::SetCurrentProcessExplicitAppUserModelID,
    },
};

/// Identifies Huburt to Windows (also the bundle identifier), so toasts say "Huburt" and group under its icon.
const APP_ID: &str = "local.huburt.app";
const KEEP_ALIVE: usize = 64;

static HANDLER: OnceLock<Arc<dyn Fn(Event) + Send + Sync>> = OnceLock::new();
static SENDER: OnceLock<Mutex<mpsc::Sender<Banner>>> = OnceLock::new();

/// Call once at startup. Registers Huburt's name for toasts and starts the thread that shows them.
pub fn init(handler: Box<dyn Fn(Event) + Send + Sync + 'static>) {
    if HANDLER.set(Arc::from(handler)).is_err() {
        eprintln!("banner: init called twice");
        return;
    }
    register_app_id();
    let (tx, rx) = mpsc::channel::<Banner>();
    let _ = SENDER.set(Mutex::new(tx));
    std::thread::spawn(move || {
        // Toasts are created on one thread that lives in the multithreaded apartment, so events can arrive on any thread
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        let mut alive: Vec<ToastNotification> = vec![];   // keeps each toast (and its event handlers) alive
        for banner in rx {
            match show(&banner) {
                Ok(toast) => {
                    alive.push(toast);
                    if alive.len() > KEEP_ALIVE {
                        alive.remove(0);
                    }
                }
                Err(e) => eprintln!("banner: couldn't show a toast: {e}"),
            }
        }
    });
}

pub fn post(banner: Banner) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.lock().map(|tx| tx.send(banner));
    }
}

/// Gives Huburt a display name for toasts without needing a Start menu shortcut (a per-user registry key).
fn register_app_id() {
    use std::os::windows::process::CommandExt;
    let key = format!(r"HKCU\Software\Classes\AppUserModelId\{APP_ID}");
    let _ = std::process::Command::new("reg.exe")
        .args(["add", &key, "/v", "DisplayName", "/t", "REG_SZ", "/d", "Huburt", "/f"])
        .creation_flags(0x0800_0000)
        .output();
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_ID));
    }
}

fn show(banner: &Banner) -> windows::core::Result<ToastNotification> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(toast_xml(banner)))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    let info = banner.user_info.clone();
    toast.Activated(&TypedEventHandler::new(move |_, args: &Option<windows::core::IInspectable>| {
        if let Some(handler) = HANDLER.get() {
            handler(activation_event(args, &info));
        }
        Ok(())
    }))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)?;
    Ok(toast)
}

/// Turns what Windows reports about a click into an `Event`: the button's argument, and the text typed in the reply box.
fn activation_event(args: &Option<windows::core::IInspectable>, info: &HashMap<String, String>) -> Event {
    let action = args
        .as_ref()
        .and_then(|a| a.cast::<ToastActivatedEventArgs>().ok())
        .and_then(|a| a.Arguments().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    match action.as_str() {
        "reply" | "markRead" => {
            let text = if action == "reply" { typed_text(args) } else { None };
            Event::Action { action, info: info.clone(), text }
        }
        _ => Event::Open { info: info.clone() },
    }
}

/// What was typed into the reply box, if anything.
fn typed_text(args: &Option<windows::core::IInspectable>) -> Option<String> {
    let input = args.as_ref()?.cast::<IToastActivatedEventArgs2>().ok()?.UserInput().ok()?;
    let value = input.Lookup(&HSTRING::from("reply")).ok()?;
    let text = value.cast::<IPropertyValue>().ok()?.GetString().ok()?.to_string();
    if text.is_empty() { None } else { Some(text) }
}
