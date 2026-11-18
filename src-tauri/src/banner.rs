//! Native macOS notification banners with action buttons (Reply / Mark as Read).
//!
//! On non-macOS targets every function is a no-op.

use std::collections::HashMap;

/// A banner to post.
#[derive(Debug, Clone, Default)]
pub struct Banner {
    pub title: String,
    pub subtitle: String,
    pub body: String,
    pub category: Option<String>,
    pub user_info: HashMap<String, String>,
    /// `None` => random UUID.
    pub identifier: Option<String>,
}

/// Something the user did with a banner.
#[derive(Debug, Clone)]
pub enum Event {
    /// A banner button: `action` is "reply" or "markRead"; `text` is the typed reply for "reply".
    Action {
        action: String,
        info: HashMap<String, String>,
        text: Option<String>,
    },
    /// The banner body was clicked.
    Open { info: HashMap<String, String> },
}

/// Call once at startup on any thread. See crate docs; `handler` runs on whatever thread the
/// callback arrives on.
pub fn init(handler: Box<dyn Fn(Event) + Send + Sync + 'static>) {
    #[cfg(target_os = "macos")]
    mac::init(handler);
    #[cfg(windows)]
    win::init(handler);
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = handler;
}

/// Posts immediately (nil trigger) with the default sound.
pub fn post(banner: Banner) {
    #[cfg(target_os = "macos")]
    mac::post(banner);
    #[cfg(windows)]
    win::post(banner);
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = banner;
}

/// The XML for a Windows toast: the message, the service name as its source line, and (for Slack messages) a Reply box
/// with a Send button and a Mark as Read button, like the macOS banners.
#[cfg(any(windows, test))]
fn toast_xml(banner: &Banner) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
    }
    let attribution = if banner.subtitle.is_empty() { String::new() } else { format!("<text placement=\"attribution\">{}</text>", esc(&banner.subtitle)) };
    let actions = if banner.category.as_deref() == Some("slack.message") {
        "<actions><input id=\"reply\" type=\"text\" placeHolderContent=\"Reply…\"/>\
         <action content=\"Send\" arguments=\"reply\" activationType=\"foreground\" hint-inputId=\"reply\"/>\
         <action content=\"Mark as Read\" arguments=\"markRead\" activationType=\"foreground\"/></actions>"
    } else {
        ""
    };
    format!(
        "<toast launch=\"open\" activationType=\"foreground\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text>{attribution}</binding></visual>{actions}<audio src=\"ms-winsoundevent:Notification.Default\"/></toast>",
        esc(&banner.title),
        esc(&banner.body)
    )
}

#[cfg(windows)]
#[path = "banner_win.rs"]
mod win;

#[cfg(target_os = "macos")]
mod mac {
    use super::{Banner, Event};
    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{define_class, AnyThread};
    use objc2_foundation::{
        NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSOperationQueue,
        NSSet, NSString, NSThread, NSUUID,
    };
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
        UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
        UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
        UNNotificationSettings, UNNotificationSound, UNTextInputNotificationAction,
        UNTextInputNotificationResponse, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };
    use std::collections::HashMap;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::ptr::NonNull;
    use std::sync::{Mutex, OnceLock};

    type Handler = Box<dyn Fn(Event) + Send + Sync + 'static>;
    static HANDLER: OnceLock<Handler> = OnceLock::new();

    /// Keeps the delegate alive for the process lifetime (the centre only holds it weakly).
    struct KeepAlive(#[allow(dead_code)] Retained<Delegate>);
    // SAFETY: only stored, never touched after creation.
    unsafe impl Send for KeepAlive {}
    unsafe impl Sync for KeepAlive {}
    static DELEGATE: Mutex<Option<KeepAlive>> = Mutex::new(None);

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "HubNotifyDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _n: &UNNotification,
                done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                done.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::Sound,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                done: &DynBlock<dyn Fn()>,
            ) {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if let Some(h) = HANDLER.get() {
                        h(event_from(response));
                    }
                }));
                done.call(());
            }
        }
    );

    fn event_from(response: &UNNotificationResponse) -> Event {
        let action = response.actionIdentifier().to_string();
        let info = dict_to_map(&response.notification().request().content().userInfo());
        if action == "reply" || action == "markRead" {
            let text = response
                .downcast_ref::<UNTextInputNotificationResponse>()
                .map(|r| r.userText().to_string());
            Event::Action { action, info, text }
        } else {
            Event::Open { info }
        }
    }

    fn dict_to_map(d: &NSDictionary) -> HashMap<String, String> {
        let mut out = HashMap::new();
        let (keys, vals) = d.to_vecs();
        for (k, v) in keys.iter().zip(vals.iter()) {
            if let (Some(k), Some(v)) = (k.downcast_ref::<NSString>(), v.downcast_ref::<NSString>())
            {
                out.insert(k.to_string(), v.to_string());
            }
        }
        out
    }

    fn map_to_dict(m: &HashMap<String, String>) -> Retained<NSDictionary> {
        let keys: Vec<Retained<NSString>> = m.keys().map(|k| NSString::from_str(k)).collect();
        let vals: Vec<Retained<NSString>> = m.values().map(|v| NSString::from_str(v)).collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let d: Retained<NSDictionary<NSString, NSString>> =
            NSDictionary::from_retained_objects(&key_refs, &vals);
        // SAFETY: NSDictionary<K, V> is the same object as the untyped dictionary.
        unsafe { Retained::cast_unchecked(d) }
    }

    fn has_bundle() -> bool {
        NSBundle::mainBundle().bundleIdentifier().is_some()
    }

    /// Run `f` on the main thread (inline if already there). Needs the main run loop to spin
    /// if called from another thread.
    fn on_main(f: impl FnOnce() + Send + 'static) {
        if NSThread::isMainThread_class() {
            f();
            return;
        }
        let cell = Mutex::new(Some(f));
        let block = RcBlock::new(move || {
            if let Some(f) = cell.lock().ok().and_then(|mut c| c.take()) {
                let _ = catch_unwind(AssertUnwindSafe(f));
            }
        });
        // SAFETY: block is `Fn() + 'static` and only touches Send data.
        unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
    }

    pub fn init(handler: Handler) {
        if !has_bundle() {
            eprintln!("hub_notify: no app bundle (bare binary); notifications disabled");
            return;
        }
        if HANDLER.set(handler).is_err() {
            eprintln!("hub_notify: init called twice; ignoring");
            return;
        }
        on_main(|| {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            let delegate: Retained<Delegate> = unsafe { objc2::msg_send![Delegate::alloc(), init] };
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            if let Ok(mut g) = DELEGATE.lock() {
                *g = Some(KeepAlive(delegate));
            }

            // Categories first, so they exist before the first banner arrives.
            let reply = UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
                &NSString::from_str("reply"),
                &NSString::from_str("Reply"),
                UNNotificationActionOptions::empty(),
                &NSString::from_str("Send"),
                &NSString::from_str("Reply\u{2026}"),
            );
            let mark = UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str("markRead"),
                &NSString::from_str("Mark as Read"),
                UNNotificationActionOptions::empty(),
            );
            let actions = NSArray::from_retained_slice(&[Retained::into_super(reply), mark]);
            let category =
                UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                    &NSString::from_str("slack.message"),
                    &actions,
                    &NSArray::new(),
                    UNNotificationCategoryOptions::empty(),
                );
            center.setNotificationCategories(&NSSet::from_retained_slice(&[category]));

            let auth = RcBlock::new(|granted: objc2::runtime::Bool, err: *mut NSError| {
                eprintln!(
                    "hub_notify: authorization granted={} error={:?}",
                    granted.as_bool(),
                    unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string())
                );
                log_settings();
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert
                    | UNAuthorizationOptions::Sound
                    | UNAuthorizationOptions::Badge,
                &auth,
            );
        });
    }

    fn log_settings() {
        let cb = RcBlock::new(|s: NonNull<UNNotificationSettings>| {
            let s = unsafe { s.as_ref() };
            eprintln!(
                "hub_notify: authorizationStatus={} (0=notDetermined 1=denied 2=authorized 3=provisional) alert={} sound={}",
                s.authorizationStatus().0,
                s.alertSetting().0,
                s.soundSetting().0
            );
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&cb);
    }

    pub fn post(b: Banner) {
        if !has_bundle() {
            eprintln!("hub_notify: no app bundle; skipping banner {:?}", b.title);
            return;
        }
        on_main(move || {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&b.title));
            content.setSubtitle(&NSString::from_str(&b.subtitle));
            content.setBody(&NSString::from_str(&b.body));
            content.setSound(Some(&UNNotificationSound::defaultSound()));
            if let Some(c) = &b.category {
                content.setCategoryIdentifier(&NSString::from_str(c));
            }
            unsafe { content.setUserInfo(&map_to_dict(&b.user_info)) };
            let id = match &b.identifier {
                Some(i) => NSString::from_str(i),
                None => NSUUID::new().UUIDString(),
            };
            let req =
                UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
            let done = RcBlock::new(|err: *mut NSError| {
                if let Some(e) = unsafe { err.as_ref() } {
                    eprintln!("hub_notify: post failed: {}", e.localizedDescription());
                } else {
                    eprintln!("hub_notify: banner posted");
                }
            });
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&req, Some(&done));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn banner(category: Option<&str>) -> Banner {
        Banner { title: "Sam in #ops".into(), subtitle: "Slack".into(), body: "deploy <now> & \"check\" it's up".into(), category: category.map(String::from), ..Default::default() }
    }

    #[test]
    fn toast_text_is_escaped() {
        let xml = toast_xml(&banner(None));
        assert!(xml.contains("<text>Sam in #ops</text>"));
        assert!(xml.contains("deploy &lt;now&gt; &amp; &quot;check&quot; it&apos;s up"), "{xml}");
        assert!(xml.contains("<text placement=\"attribution\">Slack</text>"));
        assert!(!xml.contains("<actions>"), "plain banners have no buttons");
    }

    #[test]
    fn slack_messages_get_a_reply_box_and_mark_as_read() {
        let xml = toast_xml(&banner(Some("slack.message")));
        assert!(xml.contains("<input id=\"reply\" type=\"text\""));
        assert!(xml.contains("arguments=\"reply\"") && xml.contains("hint-inputId=\"reply\""));
        assert!(xml.contains("arguments=\"markRead\""));
    }
}
