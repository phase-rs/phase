#[cfg(desktop)]
use std::{
    collections::HashMap,
    path::{Path, PathBuf, MAIN_SEPARATOR},
    sync::Mutex,
};

#[cfg(desktop)]
use tauri::{
    webview::{DownloadEvent, PageLoadEvent},
    Emitter, Manager, Url, WebviewWindowBuilder,
};

#[cfg(desktop)]
use crate::native_engine_contract::{ShellDownload, ShellDownloadOutcome};

/// Carries a finished download's outcome and destination to the page.
#[cfg(desktop)]
const DOWNLOAD_EVENT: &str = "shell-download";

/// What a finished download should report: where it landed, and whether it
/// worked. wry's failure flag lives on the `WebContext` — one per process — and
/// is only ever set, never cleared, so every download after a single failure
/// arrives here with `success = false` and no path even though the file was
/// written. The destination wry named when it requested the download narrows
/// that, but it cannot settle it: a write that dies mid-file (ENOSPC, EIO)
/// leaves a truncated file at that same destination. Nothing here can tell the
/// two apart, so they report `Unknown` rather than claim a corrupt file. A
/// withheld attribution lands in the same place: it is a gap in this handler's
/// bookkeeping, not evidence about the file.
#[cfg(desktop)]
fn finished_download_report(
    wry_path: Option<PathBuf>,
    wry_success: bool,
    attribution: DownloadAttribution,
) -> (Option<PathBuf>, ShellDownloadOutcome) {
    use DownloadAttribution::{Ambiguous, Attributed, Unmatched};
    use ShellDownloadOutcome::{Failed, Saved, Unknown};

    match (wry_success, attribution) {
        // wry's macOS handler reports no path, so the destination it named when
        // it asked for the download is the only one there is.
        (true, Attributed(destination)) => (wry_path.or(Some(destination)), Saved),
        (true, Ambiguous | Unmatched) => (wry_path, Saved),
        (false, Attributed(destination)) if destination.exists() => (Some(destination), Unknown),
        // Nothing reached the destination the request named.
        (false, Attributed(_)) => (wry_path, Failed),
        // No destination was attributable, so there is nothing to check and no
        // grounds to call the download failed.
        (false, Ambiguous) => (wry_path, Unknown),
        (false, Unmatched) => (wry_path, Failed),
    }
}

/// Which request a `Finished` event belongs to, as far as this handler can
/// tell. `Ambiguous` is a completion whose destination was withheld because
/// more than one request for its url was in flight; `Unmatched` is a completion
/// for a url with nothing outstanding at all.
#[cfg(desktop)]
#[derive(Debug, PartialEq, Eq)]
enum DownloadAttribution {
    Attributed(PathBuf),
    Ambiguous,
    Unmatched,
}

/// The downloads wry has requested for one URL and not yet finished. A second
/// request for the same URL while the first is in flight leaves both
/// destinations unattributable — wry uniquifies a destination before this
/// handler sees it, so they name different files, and nothing in a `Finished`
/// event says which request it belongs to. Only the count survives that, and
/// no completion gets a fallback destination until the URL is idle again.
#[cfg(desktop)]
#[derive(Default)]
struct OutstandingDownloads {
    requests: usize,
    destination: Option<PathBuf>,
}

#[cfg(desktop)]
fn stash_download_destination(
    outstanding: &mut HashMap<String, OutstandingDownloads>,
    url: String,
    destination: PathBuf,
) {
    let entry = outstanding.entry(url).or_default();
    entry.requests += 1;
    entry.destination = (entry.requests == 1).then_some(destination);
}

/// Consume one outstanding request for `url`, attributing its destination only
/// when that request was the only one in flight. The last completion drops the
/// URL, so the map holds live downloads and nothing else.
#[cfg(desktop)]
fn take_download_attribution(
    outstanding: &mut HashMap<String, OutstandingDownloads>,
    url: &str,
) -> DownloadAttribution {
    let Some(entry) = outstanding.get_mut(url) else {
        return DownloadAttribution::Unmatched;
    };
    entry.requests -= 1;
    let destination = entry.destination.take();
    if entry.requests == 0 {
        outstanding.remove(url);
    }
    destination.map_or(
        DownloadAttribution::Ambiguous,
        DownloadAttribution::Attributed,
    )
}

/// Compress a leading home directory to `~`, or report nothing. The page is
/// remotely served, so an absolute path hands that origin the user's account
/// name and filesystem layout; home-relative still finds the file. A path
/// outside home — `XDG_DOWNLOAD_DIR` elsewhere, wry's working-directory
/// fallback — and an undeterminable home have nothing to compress, so the page
/// is told no path at all, exactly as a browser download leaves it.
#[cfg(desktop)]
fn reportable_download_path(path: &Path, home: Option<&Path>) -> Option<String> {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(relative) if relative.as_os_str().is_empty() => Some("~".to_owned()),
        Some(relative) => Some(format!("~{MAIN_SEPARATOR}{}", relative.display())),
        None => None,
    }
}

/// Whether a page load means the webview really left the page. The navigation
/// guard has to spare `blob:`, since a file download arrives as a navigation to
/// one; a committed load — wry's `Started` on every backend — is the proof a
/// download never produces.
#[cfg(desktop)]
fn page_load_leaves_the_page(url: &Url, event: PageLoadEvent) -> bool {
    url.scheme() == "blob" && event == PageLoadEvent::Started
}

mod audio_probe;
mod host_platform;
#[cfg(desktop)]
mod lan;
#[cfg(target_os = "linux")]
mod media_stack;
mod migration;
mod mobile_compat;
#[cfg(desktop)]
mod native_bridge;
#[cfg(desktop)]
mod native_engine;
mod native_engine_contract;
#[cfg(desktop)]
mod update_authority;
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's dmabuf renderer renders blank frames when the GPU import
    // path misbehaves (NVIDIA drivers); forcing shared-memory buffers avoids
    // that while keeping the dmabuf renderer (and thus acceleration), unlike
    // WEBKIT_DISABLE_DMABUF_RENDERER which tanks in-game performance. Must be
    // set before the first webview is created. A value already present in the
    // environment wins so users can override.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DMABUF_RENDERER_FORCE_SHM").is_none() {
        std::env::set_var("WEBKIT_DMABUF_RENDERER_FORCE_SHM", "1");
    }

    // WebKitGTK has no audio stack of its own — every AudioContext and every
    // decodeAudioData is a GStreamer pipeline it assembles from plugin
    // libraries. A missing plugin set leaves the decode promise the page
    // awaits unsettled rather than rejected, so the user sees a frozen
    // loading screen with no explanation. Say why here, before the webview
    // exists, so the reason is the first thing in the terminal. Diagnostic
    // only: the page's own audio phase is deadline-bounded and boots anyway.
    #[cfg(target_os = "linux")]
    media_stack::report_to_stderr();

    let builder = tauri::Builder::default().plugin(
        tauri_plugin_opener::Builder::new()
            .open_js_links_on_click(false)
            .build(),
    );

    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        // The plugin stays registered everywhere so the `check()` command the
        // web app calls always exists — an unregistered plugin rejects the
        // call, and client/src/pwa/tauriUpdater.ts surfaces that rejection as a
        // visible update error. Refusing the release instead resolves to "no
        // update available", which that same lifecycle already treats as the
        // quiet, healthy outcome.
        .plugin(
            tauri_plugin_updater::Builder::new()
                .default_version_comparator(|current, candidate| {
                    update_authority::UpdateAuthority::detect()
                        .should_install(&current, &candidate.version)
                })
                .build(),
        )
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            audio_probe::audio_boot_health,
            host_platform::host_platform,
            migration::stash_legacy_storage,
            migration::set_channel_preference,
            migration::take_legacy_storage,
            migration::confirm_legacy_import,
            migration::mark_remote_load_ok,
            native_engine::ensure_native_engine,
            native_engine::native_engine_capabilities,
            native_engine::native_engine_progress,
            native_engine::stop_native_engine,
            native_bridge::connect_native_engine,
            native_bridge::native_engine_bridge_send,
            native_bridge::native_engine_bridge_close,
            lan::lan_capabilities,
            lan::start_lan_server,
            lan::lan_server_status,
            lan::stop_lan_server,
            lan::discover_lan_servers,
            native_bridge::authorize_lan_server,
            native_bridge::connect_lan_server,
            native_bridge::lan_bridge_send,
            native_bridge::lan_bridge_close
        ]);

    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        audio_probe::audio_boot_health,
        host_platform::host_platform,
        migration::stash_legacy_storage,
        migration::set_channel_preference,
        migration::take_legacy_storage,
        migration::confirm_legacy_import,
        migration::mark_remote_load_ok,
        mobile_compat::ensure_native_engine,
        mobile_compat::native_engine_capabilities,
        mobile_compat::native_engine_progress,
        mobile_compat::stop_native_engine,
        mobile_compat::connect_native_engine,
        mobile_compat::native_engine_bridge_send,
        mobile_compat::native_engine_bridge_close
    ]);

    let app = builder
        .setup(|app| {
            #[cfg(desktop)]
            {
                // Kick off the audio-device probe before the webview exists so the
                // verdict is usually cached by the time the page asks for it.
                audio_probe::prewarm();
                let download_destinations: Mutex<HashMap<String, OutstandingDownloads>> =
                    Mutex::default();
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
                let builder = WebviewWindowBuilder::from_config(app, main_config)?
                    .on_navigation(|url| {
                        // An `<a download>` click arrives here as a navigation to its
                        // `blob:` URL, and nothing at this point distinguishes it from
                        // a real `blob:` page, so the live game's bridges have to
                        // survive it and `on_page_load` settles the other case.
                        if url.scheme() != "blob" {
                            native_engine::abort_native_engine_bridges_on_navigation();
                            native_bridge::abort_lan_bridges();
                        }
                        true
                    })
                    .on_page_load(|_window, payload| {
                        if page_load_leaves_the_page(payload.url(), payload.event()) {
                            native_engine::abort_native_engine_bridges_on_navigation();
                            native_bridge::abort_lan_bridges();
                        }
                    })
                    // wry already accepts downloads on its own; what it does not do is
                    // tell anyone where the file went. The page only knows the name it
                    // asked for, so report the destination from here, where it is
                    // known: in full to the log, home-relative or not at all to the
                    // page.
                    .on_download(move |webview, event| {
                        match event {
                            DownloadEvent::Requested { url, destination } => {
                                eprintln!(
                                    "shell download requested: {url} -> {}",
                                    destination.display()
                                );
                                if let Ok(mut destinations) = download_destinations.lock() {
                                    stash_download_destination(
                                        &mut destinations,
                                        url.to_string(),
                                        destination.clone(),
                                    );
                                }
                            }
                            DownloadEvent::Finished { url, path, success } => {
                                // A record that cannot be read attributes
                                // nothing, which is this handler's limitation
                                // rather than evidence about the file.
                                let attribution = download_destinations.lock().map_or(
                                    DownloadAttribution::Ambiguous,
                                    |mut destinations| {
                                        take_download_attribution(&mut destinations, url.as_str())
                                    },
                                );
                                let (path, outcome) =
                                    finished_download_report(path, success, attribution);
                                eprintln!(
                                    "shell download finished: {url} -> {path:?} outcome={outcome:?}"
                                );
                                let _ = webview.emit(
                                    DOWNLOAD_EVENT,
                                    ShellDownload {
                                        url: url.to_string(),
                                        path: path.as_deref().and_then(|path| {
                                            reportable_download_path(
                                                path,
                                                std::env::home_dir().as_deref(),
                                            )
                                        }),
                                        outcome,
                                    },
                                );
                            }
                            // `DownloadEvent` is `#[non_exhaustive]`; a variant added
                            // upstream needs no decision here.
                            _ => {}
                        }
                        true
                    });
                #[cfg(target_os = "windows")]
                let builder = {
                    let data_dir = app.path().app_local_data_dir()?.join("webview");
                    builder.data_directory(data_dir)
                };
                builder.build()?;
            }
            #[cfg(mobile)]
            let _ = app;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running phase.rs");
    app.run(|app, event| {
        #[cfg(desktop)]
        if let tauri::RunEvent::Exit = event {
            native_bridge::abort_lan_bridges();
            let _ = native_engine::stop_lan_server_sync();
            native_engine::stop_native_engine_on_exit(app);
        }
        #[cfg(mobile)]
        let _ = (app, event);
    });
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::Path};

    use serde_json::{json, Value};
    use tauri::utils::{config::parse::read_from, platform::Target};

    type ConfigMutation = Box<dyn Fn(&mut Value)>;

    fn android_overlay_value() -> Value {
        serde_json::from_str(include_str!("../tauri.android.conf.json")).unwrap()
    }

    fn expected_android_overlay() -> Value {
        json!({
            "app": {
                "windows": [{
                    "label": "main",
                    "title": "phase.rs",
                    "create": true,
                    "resizable": true,
                    "maximized": true
                }]
            },
            "bundle": {
                "createUpdaterArtifacts": false,
                "android": {
                    "minSdkVersion": 24,
                    "debugApplicationIdSuffix": ".debug",
                    "autoIncrementVersionCode": false
                }
            }
        })
    }

    fn verify_android_config(root: &Path) -> Result<(), String> {
        let base: Value = serde_json::from_str(
            &fs::read_to_string(root.join("tauri.conf.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let overlay: Value = serde_json::from_str(
            &fs::read_to_string(root.join("tauri.android.conf.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if overlay != expected_android_overlay() {
            return Err("overlay differs from the exact reviewed merge patch".into());
        }
        let (merged, paths) = read_from(Target::Android, root).map_err(|e| e.to_string())?;
        if paths.len() != 2
            || paths[0].file_name().and_then(|name| name.to_str()) != Some("tauri.conf.json")
            || paths[1].file_name().and_then(|name| name.to_str())
                != Some("tauri.android.conf.json")
        {
            return Err("Tauri did not consume exactly the base and Android overlay".into());
        }
        let config: tauri::Config =
            serde_json::from_value(merged.clone()).map_err(|e| e.to_string())?;
        if config.product_name.as_deref() != base["productName"].as_str()
            || config.version.as_deref() != base["version"].as_str()
            || config.identifier != base["identifier"].as_str().unwrap()
        {
            return Err("base product/version/identifier authority was not inherited".into());
        }
        if merged["build"]
            != serde_json::from_str::<Value>(include_str!("../tauri.conf.json")).unwrap()["build"]
            || merged["plugins"]
                != serde_json::from_str::<Value>(include_str!("../tauri.conf.json")).unwrap()
                    ["plugins"]
        {
            return Err("base build/plugin authority changed during merge".into());
        }
        if !config.bundle.active
            || merged["bundle"]["targets"] != "all"
            || merged["bundle"]["icon"]
                != serde_json::from_str::<Value>(include_str!("../tauri.conf.json")).unwrap()
                    ["bundle"]["icon"]
            || config.bundle.create_updater_artifacts != tauri::utils::config::Updater::Bool(false)
        {
            return Err("effective common bundle settings are not exact".into());
        }
        let android = &config.bundle.android;
        if android.min_sdk_version != 24
            || android.version_code.is_some()
            || android.auto_increment_version_code
            || android.debug_application_id_suffix.as_deref() != Some(".debug")
        {
            return Err("effective Android bundle settings are not exact".into());
        }
        if config.app.windows.len() != 1 {
            return Err("Android window array did not replace the desktop array".into());
        }
        let window = &config.app.windows[0];
        if window.label != "main"
            || !window.create
            || window.title != "phase.rs"
            || window.width != 800.0
            || window.height != 600.0
            || !window.resizable
            || !window.maximized
        {
            return Err("effective Android main-window settings are not exact".into());
        }
        Ok(())
    }

    /// `run()` indexes `app.config().app.windows[0]` and assumes it is the
    /// "main" window with `create: false`, so the setup hook is the sole
    /// place that creates it (with the `data_directory` override applied).
    /// If `tauri.conf.json` ever grows a second window or flips `create`
    /// back to `true`, that assumption breaks silently — either panicking on
    /// the index or duplicating the window with two competing webview data
    /// directories. Pin the config shape here so a drift fails loudly.
    #[test]
    fn desktop_config_is_typed_and_preserves_the_base_authority() {
        let raw = include_str!("../tauri.conf.json");
        let config: tauri::Config = serde_json::from_str(raw).unwrap();
        assert_eq!(config.identifier, "rs.phase.app");
        assert_eq!(config.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(
            config.bundle.create_updater_artifacts,
            tauri::utils::config::Updater::Bool(true)
        );
        assert_eq!(config.bundle.android.version_code, None);
        assert_eq!(config.app.windows.len(), 1);
        let window = &config.app.windows[0];
        assert_eq!(window.label, "main");
        assert!(!window.create);
        assert_eq!(window.width, 1280.0);
        assert_eq!(window.height, 800.0);
        assert!(window.resizable);
        assert!(window.maximized);
    }

    /// `update_authority` reaches the running app through exactly one call: the
    /// updater plugin's version comparator. Drop that call and the module still
    /// compiles, its own unit tests still pass, and self-update is silently
    /// restored inside the Flatpak sandbox, where `/app` is read-only. No test
    /// of the module can observe that, so pin the wiring here — the same reason
    /// the generated Android Gradle invariants are pinned below.
    #[test]
    fn updater_plugin_defers_to_the_update_authority() {
        // Only the production half of this file, because the needles below are
        // themselves string literals in this module: matching the whole file
        // would match the test's own array and pass with the wiring deleted.
        let source = include_str!("lib.rs")
            .split("mod tests")
            .next()
            .expect("split always yields a first element");
        for required in [
            "tauri_plugin_updater::Builder::new()",
            ".default_version_comparator(",
            "update_authority::UpdateAuthority::detect()",
            ".should_install(",
        ] {
            assert!(
                source.contains(required),
                "updater registration lost required invariant: {required}"
            );
        }
    }

    /// wry latches its per-`WebContext` failure flag on the first failed
    /// download, so from then on a written file arrives as `(None, false)` —
    /// and a write that died mid-file leaves a truncated file at that same
    /// destination. Neither may be reported as saved.
    #[cfg(desktop)]
    #[test]
    fn finished_download_report_never_calls_a_reported_failure_saved() {
        use super::DownloadAttribution::{Ambiguous, Attributed, Unmatched};
        use super::ShellDownloadOutcome::{Failed, Saved, Unknown};

        let written = std::env::temp_dir().join(format!(
            "phase-rs-finished-download-{}.tmp",
            std::process::id()
        ));
        fs::write(&written, b"x").unwrap();
        let missing = written.with_extension("absent");

        assert_eq!(
            super::finished_download_report(Some(written.clone()), true, Unmatched),
            (Some(written.clone()), Saved)
        );
        // wry's macOS handler reports success with no path at all.
        assert_eq!(
            super::finished_download_report(None, true, Attributed(written.clone())),
            (Some(written.clone()), Saved)
        );
        // Latched flag or truncated file: indistinguishable from here.
        assert_eq!(
            super::finished_download_report(None, false, Attributed(written.clone())),
            (Some(written.clone()), Unknown)
        );
        // Genuine failure: nothing was written to the destination.
        assert_eq!(
            super::finished_download_report(None, false, Attributed(missing)),
            (None, Failed)
        );
        // A `Finished` with no matching `Requested` at all.
        assert_eq!(
            super::finished_download_report(None, false, Unmatched),
            (None, Failed)
        );
        // Attribution withheld between same-url requests: the latched flag may
        // still be reporting a written file.
        assert_eq!(
            super::finished_download_report(None, false, Ambiguous),
            (None, Unknown)
        );

        fs::remove_file(&written).unwrap();
    }

    /// Two downloads of one URL get different destinations from wry and arrive
    /// here as two indistinguishable completions. Lending either completion the
    /// other's destination would report a file the page never asked about, so
    /// an ambiguous completion carries no destination at all.
    #[cfg(desktop)]
    #[test]
    fn a_repeated_download_url_lends_no_completion_another_requests_destination() {
        use std::{collections::HashMap, path::PathBuf};

        use super::DownloadAttribution::{Ambiguous, Attributed, Unmatched};

        let url = "blob:https://phase-rs.dev/a";
        let other = "blob:https://phase-rs.dev/b";
        let first = PathBuf::from("/downloads/game-state.zip");
        let second = PathBuf::from("/downloads/game-state (1).zip");
        let mut outstanding = HashMap::new();

        // One in flight: the destination is this completion's, unambiguously.
        super::stash_download_destination(&mut outstanding, url.to_owned(), first.clone());
        assert_eq!(
            super::take_download_attribution(&mut outstanding, url),
            Attributed(first.clone())
        );
        assert!(outstanding.is_empty(), "a finished url must not be kept");

        // Two in flight for one url: neither completion may claim a destination.
        super::stash_download_destination(&mut outstanding, url.to_owned(), first.clone());
        super::stash_download_destination(&mut outstanding, url.to_owned(), second.clone());
        assert_eq!(
            super::take_download_attribution(&mut outstanding, url),
            Ambiguous
        );
        assert_eq!(
            super::take_download_attribution(&mut outstanding, url),
            Ambiguous
        );
        assert!(
            outstanding.is_empty(),
            "the last completion must drop the url"
        );

        // A third request arriving while those two are unresolved is ambiguous
        // with them, and the url only becomes attributable again once idle.
        super::stash_download_destination(&mut outstanding, url.to_owned(), first.clone());
        super::stash_download_destination(&mut outstanding, url.to_owned(), second.clone());
        super::take_download_attribution(&mut outstanding, url);
        super::stash_download_destination(&mut outstanding, url.to_owned(), first.clone());
        assert_eq!(
            super::take_download_attribution(&mut outstanding, url),
            Ambiguous
        );
        assert_eq!(
            super::take_download_attribution(&mut outstanding, url),
            Ambiguous
        );
        assert!(outstanding.is_empty());

        // Distinct urls never borrow from each other.
        super::stash_download_destination(&mut outstanding, url.to_owned(), first.clone());
        super::stash_download_destination(&mut outstanding, other.to_owned(), second.clone());
        assert_eq!(
            super::take_download_attribution(&mut outstanding, other),
            Attributed(second)
        );
        assert_eq!(
            super::take_download_attribution(&mut outstanding, url),
            Attributed(first)
        );
        assert!(outstanding.is_empty());

        // A completion with nothing outstanding matches no request at all.
        assert_eq!(
            super::take_download_attribution(&mut outstanding, url),
            Unmatched
        );
    }

    /// Withholding a destination is a gap in the bookkeeping above, not a
    /// verdict on the file: under wry's latched failure flag an ambiguous pair
    /// may both have been written. Such a completion may therefore be reported
    /// neither as a failed export nor with a destination it cannot claim.
    #[cfg(desktop)]
    #[test]
    fn withheld_attribution_reports_neither_a_failure_nor_a_destination() {
        use std::collections::HashMap;

        use super::ShellDownloadOutcome::{Failed, Unknown};

        let url = "blob:https://phase-rs.dev/export";
        let written = std::env::temp_dir().join(format!(
            "phase-rs-withheld-attribution-{}.tmp",
            std::process::id()
        ));
        fs::write(&written, b"x").unwrap();
        let missing = written.with_extension("absent");
        let mut outstanding = HashMap::new();

        // Two in flight for one url. The first destination is on disk, so a
        // report of `Failed` here would be wrong about a file that exists, and
        // a report of either destination would name a file this completion was
        // never shown to be.
        super::stash_download_destination(&mut outstanding, url.to_owned(), written.clone());
        super::stash_download_destination(
            &mut outstanding,
            url.to_owned(),
            written.with_extension("1.tmp"),
        );
        for _ in 0..2 {
            let attribution = super::take_download_attribution(&mut outstanding, url);
            assert_eq!(
                super::finished_download_report(None, false, attribution),
                (None, Unknown)
            );
        }

        // One in flight and the file is there: the destination is reported.
        super::stash_download_destination(&mut outstanding, url.to_owned(), written.clone());
        let attribution = super::take_download_attribution(&mut outstanding, url);
        assert_eq!(
            super::finished_download_report(None, false, attribution),
            (Some(written.clone()), Unknown)
        );

        // One in flight and nothing was written there: still a failure.
        super::stash_download_destination(&mut outstanding, url.to_owned(), missing);
        let attribution = super::take_download_attribution(&mut outstanding, url);
        assert_eq!(
            super::finished_download_report(None, false, attribution),
            (None, Failed)
        );

        // The url is idle again, so this completion matches no request at all —
        // the case withheld attribution must not be folded into.
        assert!(outstanding.is_empty());
        let attribution = super::take_download_attribution(&mut outstanding, url);
        assert_eq!(
            super::finished_download_report(None, false, attribution),
            (None, Failed)
        );

        fs::remove_file(&written).unwrap();
    }

    /// The page is remotely served, so the destination it is handed must name
    /// no part of the user's filesystem. Only a home-relative path qualifies;
    /// anything else is withheld rather than handed over absolute.
    #[cfg(desktop)]
    #[test]
    fn emitted_download_path_never_names_the_users_filesystem() {
        let home = Path::new("/home/alice");
        let inside = home.join("Downloads").join("game-state.zip");
        assert_eq!(
            super::reportable_download_path(&inside, Some(home)),
            Some(format!(
                "~{sep}Downloads{sep}game-state.zip",
                sep = std::path::MAIN_SEPARATOR
            ))
        );
        assert_eq!(
            super::reportable_download_path(home, Some(home)).as_deref(),
            Some("~")
        );
        // A neighbour whose name merely starts with the home path is outside it,
        // as is any download directory pointed elsewhere.
        for outside in [
            Path::new("/home/alice-backup/game-state.zip"),
            Path::new("/media/alice-usb/game-state.zip"),
        ] {
            assert_eq!(super::reportable_download_path(outside, Some(home)), None);
        }
        assert_eq!(super::reportable_download_path(&inside, None), None);
    }

    /// The navigation guard cannot see an `<a download>`, so it spares every
    /// `blob:` navigation. A committed page load is the one signal that says
    /// the webview left the page instead of saving a file.
    #[cfg(desktop)]
    #[test]
    fn only_a_committed_blob_page_load_ends_the_session() {
        use super::PageLoadEvent::{Finished, Started};
        use tauri::Url;

        let blob = Url::parse("blob:https://phase-rs.dev/0f8a4c21").unwrap();
        let page = Url::parse("https://phase-rs.dev/play").unwrap();

        assert!(super::page_load_leaves_the_page(&blob, Started));
        // The navigation guard tore these down before the load began.
        assert!(!super::page_load_leaves_the_page(&page, Started));
        // `Started` is the commit, so `Finished` would only repeat it.
        assert!(!super::page_load_leaves_the_page(&blob, Finished));
    }

    /// Nothing automated catches this grant going missing: no CI job builds the
    /// Flatpak, and ci.yml's tauri-check job — the only runner of this crate's
    /// tests — is disabled. This fires only under a local cargo test.
    #[test]
    fn flatpak_manifest_grants_the_download_directory() {
        let manifest = include_str!("../../../packaging/flatpak/rs.phase.app.yml");
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == "- --filesystem=xdg-download:create"),
            "packaging/flatpak/rs.phase.app.yml must grant --filesystem=xdg-download:create"
        );
    }

    /// Flatpak keys the desktop entry, the icons and the AppStream component on
    /// the app-id, and Tauri names the window's WM class from the identifier.
    /// If the two drift the package still builds and installs, but launches
    /// into an unmatched window with no icon, so pin them together.
    #[test]
    fn flatpak_manifest_app_id_matches_the_tauri_identifier() {
        let identifier = serde_json::from_str::<tauri::Config>(include_str!("../tauri.conf.json"))
            .unwrap()
            .identifier;
        let manifest = include_str!("../../../packaging/flatpak/rs.phase.app.yml");
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == format!("app-id: {identifier}")),
            "packaging/flatpak/rs.phase.app.yml must declare app-id: {identifier}"
        );
        for asset in [
            include_str!("../../../packaging/flatpak/rs.phase.app.desktop"),
            include_str!("../../../packaging/flatpak/rs.phase.app.metainfo.xml"),
        ] {
            assert!(
                asset.contains(&identifier),
                "flatpak asset must reference the {identifier} app-id"
            );
        }
        // Flatpak exports a desktop entry, an icon and a metainfo component only
        // when each is installed under the app-id name, so the install
        // destinations are the part that actually decides whether the launcher
        // works. Declaring the right app-id while installing to the old file
        // names silently exports nothing.
        for destination in [
            format!("{identifier}.desktop"),
            format!("{identifier}.metainfo.xml"),
            format!("{identifier}.png"),
        ] {
            assert!(
                manifest.contains(&destination),
                "rs.phase.app.yml must install {destination} for flatpak to export it"
            );
        }
    }

    #[test]
    fn android_config_uses_tauri_rfc7396_merge_and_exact_typed_values() {
        assert_eq!(android_overlay_value(), expected_android_overlay());
        verify_android_config(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    }

    #[test]
    fn installed_android_config_schema_admits_only_the_four_real_properties() {
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../node_modules/@tauri-apps/cli/config.schema.json");
        let Ok(raw) = fs::read_to_string(&schema_path) else {
            eprintln!(
                "skipping installed schema assertions because {} is absent; run the frontend dependency install first",
                schema_path.display()
            );
            return;
        };
        let schema: Value = serde_json::from_str(&raw).unwrap();
        let android = &schema["definitions"]["AndroidConfig"];
        let properties = android["properties"].as_object().unwrap();
        let actual: BTreeSet<_> = properties.keys().map(String::as_str).collect();
        let expected = BTreeSet::from([
            "autoIncrementVersionCode",
            "debugApplicationIdSuffix",
            "minSdkVersion",
            "versionCode",
        ]);
        assert_eq!(actual, expected);
        assert_eq!(properties["minSdkVersion"]["default"], 24);
        assert_eq!(properties["autoIncrementVersionCode"]["default"], false);
        assert!(!properties.contains_key("targetSdkVersion"));
        let overlay = android_overlay_value();
        for key in overlay["bundle"]["android"].as_object().unwrap().keys() {
            assert!(
                properties.contains_key(key),
                "unknown Android overlay key {key}"
            );
        }
        assert!(overlay.get("identifier").is_none());
        assert!(overlay.get("version").is_none());
        assert!(overlay["bundle"]["android"].get("versionCode").is_none());
        assert!(overlay["bundle"]["android"]
            .get("targetSdkVersion")
            .is_none());
    }

    #[test]
    fn generated_android_gradle_keeps_release_invariants() {
        use sha2::{Digest, Sha256};

        let gradle = include_str!("../gen/android/app/build.gradle.kts");
        for required in [
            "fun strictAndroidVersionCode(version: String): Int",
            "val androidVersionCode = strictAndroidVersionCode(androidVersionName)",
            "signingConfigs {",
            "create(\"release\")",
            "signingConfig = signingConfigs.getByName(\"release\")",
            "tasks.configureEach",
            "Missing required Android release signing inputs",
            "PHASE_ANDROID_KEYSTORE_FILE",
            "PHASE_ANDROID_KEYSTORE_PASSWORD",
            "PHASE_ANDROID_KEY_ALIAS",
            "PHASE_ANDROID_KEY_PASSWORD",
        ] {
            assert!(
                gradle.contains(required),
                "generated Android Gradle integration lost required invariant: {required}"
            );
        }

        let wrapper_properties =
            include_str!("../gen/android/gradle/wrapper/gradle-wrapper.properties");
        assert!(wrapper_properties.contains(
            "distributionSha256Sum=bd71102213493060956ec229d946beee57158dbd89d0e62b91bca0fa2c5f3531"
        ));

        let wrapper_jar = include_bytes!("../gen/android/gradle/wrapper/gradle-wrapper.jar");
        assert_eq!(
            format!("{:x}", Sha256::digest(wrapper_jar)),
            "7d3a4ac4de1c32b59bc6a4eb8ecb8e612ccd0cf1ae1e99f66902da64df296172",
            "generated Android Gradle wrapper JAR must match the official 8.14.3 artifact"
        );
    }

    #[test]
    fn generated_android_launcher_uses_exact_phase_brand_assets() {
        fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(&bytes[12..16], b"IHDR");
            (
                u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
                u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
            )
        }

        fn fnv1a64(bytes: &[u8]) -> u64 {
            bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            })
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("gen/android/app/src/main");
        let launchers = [
            ("res/mipmap-mdpi/ic_launcher.png", 48, 0x5c1ab8a4b9388839),
            (
                "res/mipmap-mdpi/ic_launcher_round.png",
                48,
                0x59147ebcc4d03aee,
            ),
            (
                "res/mipmap-mdpi/ic_launcher_foreground.png",
                108,
                0x7475638e0402146d,
            ),
            ("res/mipmap-hdpi/ic_launcher.png", 72, 0x6617db678c75ce93),
            (
                "res/mipmap-hdpi/ic_launcher_round.png",
                72,
                0x4f89bf39172434e8,
            ),
            (
                "res/mipmap-hdpi/ic_launcher_foreground.png",
                162,
                0x117a6af2e6fc0ba6,
            ),
            ("res/mipmap-xhdpi/ic_launcher.png", 96, 0x093ba5f2f7965ec2),
            (
                "res/mipmap-xhdpi/ic_launcher_round.png",
                96,
                0x4b3df36bd3042bc2,
            ),
            (
                "res/mipmap-xhdpi/ic_launcher_foreground.png",
                216,
                0xf53f6d79f95ca531,
            ),
            ("res/mipmap-xxhdpi/ic_launcher.png", 144, 0xff0c47390df3221d),
            (
                "res/mipmap-xxhdpi/ic_launcher_round.png",
                144,
                0xd35d38485496323b,
            ),
            (
                "res/mipmap-xxhdpi/ic_launcher_foreground.png",
                324,
                0x67326e67177dc7d1,
            ),
            (
                "res/mipmap-xxxhdpi/ic_launcher.png",
                192,
                0x8392d43e0239107d,
            ),
            (
                "res/mipmap-xxxhdpi/ic_launcher_round.png",
                192,
                0x1d2a7149b7716eff,
            ),
            (
                "res/mipmap-xxxhdpi/ic_launcher_foreground.png",
                432,
                0x361ec69b50f865b4,
            ),
        ];
        for (relative, expected_size, expected_hash) in launchers {
            let bytes = fs::read(root.join(relative)).unwrap();
            assert_eq!(
                png_dimensions(&bytes),
                (expected_size, expected_size),
                "{relative}"
            );
            assert_eq!(fnv1a64(&bytes), expected_hash, "{relative}");
        }

        let manifest = fs::read_to_string(root.join("AndroidManifest.xml")).unwrap();
        assert!(manifest.contains("android:icon=\"@mipmap/ic_launcher\""));
        assert!(manifest.contains("android:roundIcon=\"@mipmap/ic_launcher_round\""));

        let expected_adaptive_mappings = [
            "<background android:drawable=\"@color/ic_launcher_background\" />",
            "<foreground android:drawable=\"@mipmap/ic_launcher_foreground\" />",
        ];
        for relative in [
            "res/mipmap-anydpi-v26/ic_launcher.xml",
            "res/mipmap-anydpi-v26/ic_launcher_round.xml",
        ] {
            let adaptive = fs::read_to_string(root.join(relative)).unwrap();
            assert!(adaptive.contains("<adaptive-icon"), "{relative}");
            for mapping in expected_adaptive_mappings {
                assert!(adaptive.contains(mapping), "{relative}: missing {mapping}");
            }
        }
        let colors = fs::read_to_string(root.join("res/values/colors.xml")).unwrap();
        assert!(colors.contains("<color name=\"ic_launcher_background\">#FF111827</color>"));

        for obsolete_stock_asset in [
            "res/drawable/ic_launcher_background.xml",
            "res/drawable-v24/ic_launcher_foreground.xml",
        ] {
            assert!(
                !root.join(obsolete_stock_asset).exists(),
                "{obsolete_stock_asset}"
            );
        }
    }

    #[test]
    fn every_android_config_mutation_is_rejected_and_the_positive_is_restored() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let base = include_str!("../tauri.conf.json");
        let mut cases: Vec<(&str, ConfigMutation)> = vec![
            (
                "delete createUpdaterArtifacts",
                Box::new(|v| {
                    v["bundle"]
                        .as_object_mut()
                        .unwrap()
                        .remove("createUpdaterArtifacts");
                }),
            ),
            (
                "change minSdkVersion",
                Box::new(|v| v["bundle"]["android"]["minSdkVersion"] = json!(23)),
            ),
            (
                "change debug suffix",
                Box::new(|v| v["bundle"]["android"]["debugApplicationIdSuffix"] = json!(".other")),
            ),
            (
                "enable auto increment",
                Box::new(|v| v["bundle"]["android"]["autoIncrementVersionCode"] = json!(true)),
            ),
            (
                "append desktop window",
                Box::new(|v| {
                    v["app"]["windows"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"label":"second"}))
                }),
            ),
            (
                "duplicate identifier",
                Box::new(|v| v["identifier"] = json!("rs.phase.app")),
            ),
            (
                "duplicate version",
                Box::new(|v| v["version"] = json!(env!("CARGO_PKG_VERSION"))),
            ),
            (
                "duplicate versionCode",
                Box::new(|v| v["bundle"]["android"]["versionCode"] = json!(1)),
            ),
            (
                "invent targetSdkVersion",
                Box::new(|v| v["bundle"]["android"]["targetSdkVersion"] = json!(36)),
            ),
        ];
        for (index, (name, mutate)) in cases.drain(..).enumerate() {
            let temp = std::env::temp_dir().join(format!(
                "phase-android-config-{}-{index}",
                std::process::id()
            ));
            if temp.exists() {
                fs::remove_dir_all(&temp).unwrap();
            }
            fs::create_dir_all(&temp).unwrap();
            fs::write(temp.join("tauri.conf.json"), base).unwrap();
            let mut overlay = expected_android_overlay();
            mutate(&mut overlay);
            fs::write(
                temp.join("tauri.android.conf.json"),
                serde_json::to_vec_pretty(&overlay).unwrap(),
            )
            .unwrap();
            assert!(
                verify_android_config(&temp).is_err(),
                "mutation unexpectedly passed: {name}"
            );
            fs::remove_dir_all(&temp).unwrap();
            verify_android_config(root).unwrap();
        }
    }

    fn capability_permissions(capability: &Value) -> BTreeSet<&str> {
        capability["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .or_else(|| value["identifier"].as_str())
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn capability_manifest_declares_exact_mobile_and_desktop_authority() {
        let capabilities: Vec<Value> =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert_eq!(capabilities.len(), 4);
        let expected_opener = json!({
            "identifier": "opener:allow-open-url",
            "allow": [{ "url": "http://*" }, { "url": "https://*" }]
        });
        for identifier in ["default", "remote-shell-common"] {
            let capability = capabilities
                .iter()
                .find(|capability| capability["identifier"] == identifier)
                .unwrap();
            let grants: Vec<_> = capability["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|permission| permission["identifier"] == "opener:allow-open-url")
                .collect();
            assert_eq!(grants, vec![&expected_opener]);
        }
        let desktop_remote = capabilities
            .iter()
            .find(|capability| capability["identifier"] == "remote-shell-desktop")
            .unwrap();
        assert!(!capability_permissions(desktop_remote)
            .iter()
            .any(
                |permission| permission.starts_with("opener:") || permission.starts_with("shell:")
            ));
        let identifiers: BTreeSet<_> = capabilities
            .iter()
            .map(|capability| capability["identifier"].as_str().unwrap())
            .collect();
        assert_eq!(
            identifiers,
            BTreeSet::from([
                "default",
                "local-shell-desktop",
                "remote-shell-common",
                "remote-shell-desktop",
            ])
        );
        let find = |identifier: &str| {
            capabilities
                .iter()
                .find(|capability| capability["identifier"] == identifier)
                .unwrap()
        };
        let common_local = find("default");
        let desktop_local = find("local-shell-desktop");
        let common_remote = find("remote-shell-common");
        let desktop_remote = find("remote-shell-desktop");

        for capability in [common_local, desktop_local, common_remote, desktop_remote] {
            assert_eq!(capability["windows"], json!(["main"]));
        }
        assert!(common_local.get("platforms").is_none());
        assert!(common_local.get("local").is_none());
        assert!(desktop_local.get("local").is_none());
        assert_eq!(
            desktop_local["platforms"],
            json!(["linux", "macOS", "windows"])
        );

        let trusted_origins = json!([
            "https://phase-rs.dev/*",
            "https://app.phase-rs.dev/*",
            "https://preview.phase-rs.dev/*"
        ]);
        for capability in [common_remote, desktop_remote] {
            assert_eq!(capability["local"], false);
            assert_eq!(capability["remote"]["urls"], trusted_origins);
        }
        assert!(common_remote.get("platforms").is_none());
        assert_eq!(
            desktop_remote["platforms"],
            json!(["linux", "macOS", "windows"])
        );

        // Self-update, exit and restart belong to the trusted remote origins only.
        assert_eq!(
            capability_permissions(desktop_local),
            BTreeSet::from(["core:window:allow-set-fullscreen"])
        );
        assert_eq!(
            capability_permissions(desktop_remote),
            BTreeSet::from([
                "core:window:allow-set-fullscreen",
                "process:allow-exit",
                "process:allow-restart",
                "updater:default",
                "allow-lan",
            ])
        );
        for capability in [common_local, common_remote] {
            let permissions = capability_permissions(capability);
            assert!(!permissions.contains("core:window:allow-set-fullscreen"));
            assert!(!permissions.contains("process:allow-exit"));
            assert!(!permissions.contains("process:allow-restart"));
            assert!(!permissions.contains("updater:default"));
            assert!(!permissions.contains("allow-lan"));
        }
        for required in [
            "allow-host-platform",
            "allow-ensure-native-engine",
            "allow-connect-native-engine",
        ] {
            assert!(capability_permissions(common_remote).contains(required));
        }

        let acl_manifests: Value =
            serde_json::from_str(include_str!("../gen/schemas/acl-manifests.json")).unwrap();
        let app_permissions = acl_manifests["__app-acl__"]["permissions"]
            .as_object()
            .unwrap();
        assert_eq!(
            app_permissions["allow-ensure-native-engine"]["commands"]["allow"],
            json!(["ensure_native_engine", "native_engine_capabilities"])
        );
        for capability in [common_local, common_remote] {
            for permission in capability_permissions(capability) {
                if permission.starts_with("allow-") {
                    assert!(
                        app_permissions.contains_key(permission),
                        "unknown application permission {permission}"
                    );
                }
            }
        }
    }
}
