//! Huburt's launcher. It is the signed app executable and it NEVER changes, so macOS keeps the approvals tied to it
//! (e.g. "Always Allow" for WhatsApp's encryption key in the keychain) across rebuilds. The real app is
//! `huburt_core` (src/lib.rs), loaded from ~/Library/Application Support/Huburt/. Don't edit this file casually:
//! changing it, the Info.plist or the icon changes the app's hash and costs one more "Always Allow".
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::{c_char, c_int, c_void, CStr, CString};

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

fn fail(msg: &str) -> ! {
    eprintln!("huburt launcher: {msg}");
    std::process::exit(1);
}

fn main() {
    let path = std::env::var("HUBURT_CORE").unwrap_or_else(|_| {
        format!("{}/Library/Application Support/Huburt/libhuburt_core.dylib", std::env::var("HOME").unwrap_or_default())
    });
    let c_path = CString::new(path.clone()).unwrap_or_else(|_| fail("bad path"));
    unsafe {
        let handle = dlopen(c_path.as_ptr(), 2); // RTLD_NOW
        if handle.is_null() {
            let err = dlerror();
            let detail = if err.is_null() { String::new() } else { CStr::from_ptr(err).to_string_lossy().into_owned() };
            fail(&format!("can't load {path}: {detail} (run ./build.sh --install)"));
        }
        let entry = dlsym(handle, c"huburt_main".as_ptr());
        if entry.is_null() {
            fail("huburt_main not found in the core library");
        }
        let entry: extern "C" fn() = std::mem::transmute(entry);
        entry();
    }
}
