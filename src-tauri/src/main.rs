//! Huburt's launcher (macOS). It is the signed app executable and it NEVER changes, so macOS keeps the approvals tied to it
//! (e.g. "Always Allow" for WhatsApp's encryption key in the keychain) across rebuilds. The real app is
//! `huburt_core` (src/lib.rs). The launcher looks for it, in order: `$HUBURT_CORE`; inside the app bundle
//! (Contents/Frameworks, how downloadable builds ship it); then ~/Library/Application Support/Huburt/ (how local
//! `./build.sh` builds update it without touching the signed app). Don't edit this file casually: changing it, the
//! Info.plist or the icon changes the app's hash and costs one more "Always Allow".
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_int, c_void, CStr, CString};

#[cfg(target_os = "macos")]
extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

#[cfg(target_os = "macos")]
fn fail(msg: &str) -> ! {
    eprintln!("huburt launcher: {msg}");
    std::process::exit(1);
}

/// Windows (and anywhere else): there is no keychain approval to protect, so this is just the program.
#[cfg(not(target_os = "macos"))]
fn main() {
    huburt_core::run();
}

/// Where to find the core library. `bundled_exists` says whether a file exists (a parameter so it can be tested).
#[cfg(target_os = "macos")]
fn core_path(env_override: Option<String>, exe: &std::path::Path, home: &str, bundled_exists: impl Fn(&std::path::Path) -> bool) -> String {
    if let Some(p) = env_override {
        return p;
    }
    // exe is <app>/Contents/MacOS/huburt, so the bundle's Frameworks folder is two levels up
    if let Some(contents) = exe.parent().and_then(|p| p.parent()) {
        let bundled = contents.join("Frameworks/libhuburt_core.dylib");
        if bundled_exists(&bundled) {
            return bundled.display().to_string();
        }
    }
    format!("{home}/Library/Application Support/Huburt/libhuburt_core.dylib")
}

#[cfg(target_os = "macos")]
fn main() {
    let path = core_path(
        std::env::var("HUBURT_CORE").ok(),
        &std::env::current_exe().unwrap_or_default(),
        &std::env::var("HOME").unwrap_or_default(),
        |p| p.exists(),
    );
    let c_path = CString::new(path.clone()).unwrap_or_else(|_| fail("bad path"));
    unsafe {
        let handle = dlopen(c_path.as_ptr(), 2); // RTLD_NOW
        if handle.is_null() {
            let err = dlerror();
            let detail = if err.is_null() { String::new() } else { CStr::from_ptr(err).to_string_lossy().into_owned() };
            fail(&format!("can't load {path}: {detail} (building from source? run ./build.sh)"));
        }
        let entry = dlsym(handle, c"huburt_main".as_ptr());
        if entry.is_null() {
            fail("huburt_main not found in the core library");
        }
        let entry: extern "C" fn() = std::mem::transmute(entry);
        entry();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::Path;

    const EXE: &str = "/Applications/Huburt.app/Contents/MacOS/huburt";

    #[test]
    fn the_environment_override_wins() {
        assert_eq!(core_path(Some("/tmp/x.dylib".into()), Path::new(EXE), "/Users/me", |_| true), "/tmp/x.dylib");
    }

    #[test]
    fn a_core_inside_the_bundle_is_used_when_present() {
        let p = core_path(None, Path::new(EXE), "/Users/me", |p| p == Path::new("/Applications/Huburt.app/Contents/Frameworks/libhuburt_core.dylib"));
        assert_eq!(p, "/Applications/Huburt.app/Contents/Frameworks/libhuburt_core.dylib");
    }

    #[test]
    fn local_builds_without_a_bundled_core_use_application_support() {
        assert_eq!(
            core_path(None, Path::new(EXE), "/Users/me", |_| false),
            "/Users/me/Library/Application Support/Huburt/libhuburt_core.dylib"
        );
    }
}
