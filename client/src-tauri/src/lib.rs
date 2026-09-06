#[cfg(desktop)]
use tauri::{Manager, WebviewWindowBuilder};

mod audio_probe;
mod host_platform;
#[cfg(target_os = "linux")]
mod media_stack;
mod migration;
mod mobile_compat;
#[cfg(desktop)]
mod native_bridge;
#[cfg(desktop)]
mod native_engine;
mod native_engine_contract;
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
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
            native_bridge::native_engine_bridge_close
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
                let builder =
                    WebviewWindowBuilder::from_config(app, main_config)?.on_navigation(|_| {
                        native_engine::abort_native_engine_bridges_on_navigation();
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
            ])
        );
        for capability in [common_local, common_remote] {
            let permissions = capability_permissions(capability);
            assert!(!permissions.contains("core:window:allow-set-fullscreen"));
            assert!(!permissions.contains("process:allow-exit"));
            assert!(!permissions.contains("process:allow-restart"));
            assert!(!permissions.contains("updater:default"));
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
