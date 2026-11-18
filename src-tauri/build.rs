fn main() {
    // Commands the web pages may call are listed here so capabilities can grant them individually.
    let commands = &[
        "get_state", "select", "reorder", "show_menu", "reload", "set_enabled", "page_notify", "page_probe", "open_external",
    ];
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(commands)))
        .expect("failed to run tauri-build");
}
