use tauri::{Manager, WebviewWindowBuilder};

mod migration;
mod native_engine;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            migration::stash_legacy_storage,
            migration::set_channel_preference,
            migration::take_legacy_storage,
            migration::confirm_legacy_import,
            migration::mark_remote_load_ok,
            native_engine::ensure_native_engine,
            native_engine::stop_native_engine
        ])
        .setup(|app| {
            // `create: false` on the "main" window in tauri.conf.json defers
            // window creation to here so we can pin an explicit, always-writable
            // `data_directory` on Windows. WebView2 otherwise derives its
            // user-data folder from the install path; on a read-only per-machine
            // install (e.g. under Program Files) that folder can't be written, so
            // WebView2 falls back to a throwaway profile that's discarded every
            // launch and the Supabase session in localStorage never survives a
            // restart even though `persistSession: true` is set. Pinning it to the
            // per-user local-data dir keeps it stable and writable regardless of
            // install location.
            //
            // Windows-only: WKWebView (macOS) ignores `data_directory`, and
            // webkit2gtk (Linux) already persists under the user's profile by
            // default — overriding it there would only relocate existing storage
            // and force a one-time re-login, so we leave those platforms on their
            // defaults and just build the window straight from config.
            let main_config = &app.config().app.windows[0];
            let builder = WebviewWindowBuilder::from_config(app, main_config)?;
            #[cfg(target_os = "windows")]
            let builder = {
                let data_dir = app.path().app_local_data_dir()?.join("webview");
                builder.data_directory(data_dir)
            };
            builder.build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running phase.rs");
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            native_engine::stop_native_engine_on_exit(app);
        }
    });
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
