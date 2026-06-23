use tauri::{Manager, WebviewWindowBuilder};

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            // `create: false` on the "main" window in tauri.conf.json defers window
            // creation to here so we can pin an explicit, always-writable
            // `data_directory`. Without it, WebView2 (Windows) falls back to a
            // folder derived from the install path; when that path is read-only
            // (e.g. a per-machine install under Program Files) WebView2 silently
            // uses a temp profile that's discarded every launch, so the Supabase
            // session in localStorage never survives a restart even though
            // `persistSession: true` is set. Pointing it at the app's per-user
            // local-data dir keeps it stable and writable regardless of where the
            // app is installed. No-op on macOS/Linux, where the webview already
            // persists storage under the user's profile.
            let data_dir = app.path().app_local_data_dir()?.join("webview");
            let main_config = &app.config().app.windows[0];
            WebviewWindowBuilder::from_config(app, main_config)?
                .data_directory(data_dir)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running phase.rs");
}

#[cfg(test)]
mod tests {
    /// `run()` indexes `app.config().app.windows[0]` and assumes it is the
    /// "main" window with `create: false`, so the setup hook is the sole
    /// place that creates it (with the `data_directory` override applied).
    /// If `tauri.conf.json` ever grows a second window or flips `create`
    /// back to `true`, that assumption breaks silently — either panicking on
    /// the index or duplicating the window with two competing webview data
    /// directories. Pin the config shape here so a drift fails loudly.
    #[test]
    fn main_window_config_defers_to_setup_hook() {
        let raw = include_str!("../tauri.conf.json");
        let config: serde_json::Value = serde_json::from_str(raw).unwrap();
        let windows = config["app"]["windows"].as_array().unwrap();
        assert_eq!(
            windows.len(),
            1,
            "run() assumes exactly one window at index 0"
        );
        assert_eq!(windows[0]["label"], "main");
        assert_eq!(
            windows[0]["create"], false,
            "must stay false so run()'s setup hook is the only window creator"
        );
    }
}
