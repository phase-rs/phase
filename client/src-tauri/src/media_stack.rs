//! Boot-time preflight for the Linux media runtime WebKitGTK plays audio with.
//!
//! WebKitGTK has no audio stack of its own: `AudioContext`, `decodeAudioData`
//! and `<audio>` are all GStreamer pipelines it assembles at runtime out of
//! plugin libraries found on GStreamer's plugin search path. When an element
//! is missing, WebKit does not fail the call — it logs `GStreamer element
//! <name> not found` to stderr, builds a half-wired pipeline, and the decode
//! promise driving it never settles. A page that awaits that promise waits
//! forever, which is how a missing plugin set turns into a frozen loading
//! screen rather than silent audio.
//!
//! Two shapes of that fault are detectable before the webview exists, and both
//! are reported here as a diagnostic only — the page boots either way, because
//! `startup/preloadAssets.ts` bounds the audio phase on its side:
//!
//! 1. A bundle that ships its own GStreamer core but no plugins. The bundled
//!    core rejects plugin libraries built against a different GStreamer
//!    version, so pointing it at the host's plugin directory registers
//!    nothing at all — every element lookup misses. This is the reported
//!    AppImage failure (issue #6744); `bundleMediaFramework` in
//!    `tauri.conf.json` is the fix, and this arm catches a bundle built
//!    without it.
//! 2. A plugin directory that is genuinely missing libraries the app needs —
//!    the `.deb`/source case on a system without the GStreamer plugin sets
//!    installed. `bundle.linux.deb.depends` declares them for apt; this arm
//!    names them for everyone else.

use std::path::{Path, PathBuf};

/// One GStreamer plugin library WebKitGTK needs to play this app's audio: what
/// it registers, and the Debian/Ubuntu package that ships it.
///
/// `provides` is prose rather than an element list because not every required
/// library registers elements — `typefindfunctions` contributes only container
/// typefinders, which is why a plugin set can look complete by element lookup
/// and still fail to identify an `.m4a`.
#[derive(Debug, PartialEq, Eq)]
pub struct RequiredPlugin {
    pub library: &'static str,
    pub provides: &'static str,
    pub debian_package: &'static str,
}

/// The plugin set behind one `decodeAudioData` of an `.m4a` SFX buffer plus one
/// `AudioContext` destination — the two pipelines boot actually builds. Every
/// element named in issue #6744's log appears here.
pub const REQUIRED_PLUGINS: &[RequiredPlugin] = &[
    RequiredPlugin {
        library: "libgstapp.so",
        provides: "appsrc, appsink",
        debian_package: "gstreamer1.0-plugins-base",
    },
    RequiredPlugin {
        library: "libgstplayback.so",
        provides: "decodebin",
        debian_package: "gstreamer1.0-plugins-base",
    },
    RequiredPlugin {
        library: "libgstgio.so",
        provides: "giostreamsrc",
        debian_package: "gstreamer1.0-plugins-base",
    },
    RequiredPlugin {
        library: "libgstcoreelements.so",
        provides: "typefind, queue, capsfilter",
        debian_package: "libgstreamer1.0-0",
    },
    RequiredPlugin {
        library: "libgsttypefindfunctions.so",
        provides: "the container typefinders decodebin identifies .m4a with",
        debian_package: "gstreamer1.0-plugins-base",
    },
    RequiredPlugin {
        library: "libgstaudioconvert.so",
        provides: "audioconvert",
        debian_package: "gstreamer1.0-plugins-base",
    },
    RequiredPlugin {
        library: "libgstaudioresample.so",
        provides: "audioresample",
        debian_package: "gstreamer1.0-plugins-base",
    },
    RequiredPlugin {
        library: "libgstautodetect.so",
        provides: "autoaudiosink",
        debian_package: "gstreamer1.0-plugins-good",
    },
    RequiredPlugin {
        library: "libgstisomp4.so",
        provides: "qtdemux (the .m4a container every bundled track uses)",
        debian_package: "gstreamer1.0-plugins-good",
    },
    RequiredPlugin {
        library: "libgstlibav.so",
        provides: "avdec_aac (the .m4a codec every bundled track uses)",
        debian_package: "gstreamer1.0-libav",
    },
];

/// Everything the verdict depends on besides the filesystem, so the decision is
/// a pure function the tests can drive without touching process env.
#[derive(Debug, Default)]
pub struct MediaStackEnv {
    /// `$APPDIR` — set by an AppImage's `AppRun` for the app and every child
    /// process, including WebKit's.
    pub appdir: Option<PathBuf>,
    /// `GST_PLUGIN_SYSTEM_PATH_1_0` / `GST_PLUGIN_SYSTEM_PATH`. When set these
    /// *replace* the distro directories rather than adding to them.
    pub system_path: Option<Vec<PathBuf>>,
    /// `GST_PLUGIN_PATH_1_0` / `GST_PLUGIN_PATH` — always searched as well.
    /// Unset and set-but-empty both mean "no extra directories" here, because
    /// this list only ever adds to the search path.
    pub extra_path: Vec<PathBuf>,
    /// The distro's own plugin directories, searched when `system_path` is unset.
    pub default_dirs: Vec<PathBuf>,
}

impl MediaStackEnv {
    /// The directories GStreamer will scan, in the order it scans them.
    fn search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = self
            .system_path
            .clone()
            .unwrap_or_else(|| self.default_dirs.clone());
        dirs.extend(self.extra_path.iter().cloned());
        dirs
    }
}

/// What WebKit's first audio pipeline will find when it looks for its elements.
#[derive(Debug, PartialEq, Eq)]
pub enum MediaStackVerdict {
    Usable,
    /// The bundle carries its own `libgstreamer-1.0` but every directory it
    /// will search for plugins lies outside the bundle. Version-mismatched
    /// plugins register nothing, so *no* element resolves — the failure is
    /// total rather than partial, which is why it gets its own variant.
    BundledCoreUnbundledPlugins,
    /// The searched directories are missing these libraries.
    MissingPlugins(Vec<&'static RequiredPlugin>),
}

/// Whether `appdir` ships a GStreamer core of its own.
fn bundles_gstreamer_core(appdir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(appdir.join("usr/lib")) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("libgstreamer-1.0.so"))
    })
}

/// Classify the media runtime. Filesystem reads only — never spawns, never
/// blocks, and never changes what the app does.
pub fn verdict(env: &MediaStackEnv) -> MediaStackVerdict {
    let dirs: Vec<PathBuf> = env
        .search_dirs()
        .into_iter()
        .filter(|dir| dir.is_dir())
        .collect();

    if let Some(appdir) = &env.appdir {
        if bundles_gstreamer_core(appdir) && !dirs.iter().any(|dir| dir.starts_with(appdir)) {
            return MediaStackVerdict::BundledCoreUnbundledPlugins;
        }
    }

    let missing: Vec<&'static RequiredPlugin> = REQUIRED_PLUGINS
        .iter()
        .filter(|plugin| !dirs.iter().any(|dir| dir.join(plugin.library).exists()))
        .collect();

    if missing.is_empty() {
        MediaStackVerdict::Usable
    } else {
        MediaStackVerdict::MissingPlugins(missing)
    }
}

/// Split a `PATH`-style variable into directories, dropping empty segments.
fn split_path_var(value: &str) -> Vec<PathBuf> {
    value
        .split(':')
        .filter(|segment| !segment.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The first of `names` that is *set*, split into directories.
///
/// Set-but-empty is a real answer, not a missing one: GStreamer picks the
/// variable by presence (`g_getenv` returning non-NULL) and only then splits
/// it, so `GST_PLUGIN_SYSTEM_PATH_1_0=""` means "no system plugin directories"
/// and must neither fall through to the unversioned variable nor restore the
/// distro defaults. Matching that exactly is what keeps the verdict a
/// prediction of what GStreamer will do rather than a guess.
///
/// Takes `lookup` instead of reading the environment so the precedence rule
/// has an env-independent seam to test against — mutating process env in a
/// shared-process test harness is unsound (see
/// `crates/engine/tests/integration/issue_4365_msh_legality.rs`).
fn path_var_from(names: &[&str], lookup: impl Fn(&str) -> Option<String>) -> Option<Vec<PathBuf>> {
    names
        .iter()
        .find_map(|name| lookup(name))
        .map(|value| split_path_var(&value))
}

/// Read the process environment into [`MediaStackEnv`].
fn process_env() -> MediaStackEnv {
    let env = |name: &str| std::env::var(name).ok();
    MediaStackEnv {
        appdir: std::env::var_os("APPDIR").map(PathBuf::from),
        system_path: path_var_from(
            &["GST_PLUGIN_SYSTEM_PATH_1_0", "GST_PLUGIN_SYSTEM_PATH"],
            env,
        ),
        extra_path: path_var_from(&["GST_PLUGIN_PATH_1_0", "GST_PLUGIN_PATH"], env)
            .unwrap_or_default(),
        default_dirs: default_plugin_dirs(),
    }
}

/// Where distributions install GStreamer plugins. All plausible layouts are
/// listed rather than probed: a directory that does not exist is filtered out
/// by [`verdict`], and listing them is cheaper than detecting the distro. The
/// Debian multiarch triple is built from `ARCH`, which is exact for the
/// desktop targets (`x86_64`, `aarch64`); the plain `/usr/lib` entry covers
/// the architectures whose triple carries an ABI suffix.
fn default_plugin_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from(format!(
            "/usr/lib/{}-linux-gnu/gstreamer-1.0",
            std::env::consts::ARCH
        )),
        PathBuf::from("/usr/lib64/gstreamer-1.0"),
        PathBuf::from("/usr/lib/gstreamer-1.0"),
    ]
}

/// Print an actionable diagnostic when the media runtime cannot serve audio.
///
/// Stderr, not a dialog: the app still starts and stays fully playable without
/// audio, so this is for the terminal the user reaches for when they wonder why
/// it is silent — the same place issue #6744 was diagnosed from.
pub fn report_to_stderr() {
    match verdict(&process_env()) {
        MediaStackVerdict::Usable => {}
        MediaStackVerdict::BundledCoreUnbundledPlugins => {
            eprintln!(
                "phase.rs: this bundle ships its own GStreamer core but no GStreamer \
                 plugins, so WebKit inside it will find no audio elements at all \
                 (appsrc, appsink, decodebin, autoaudiosink, giostreamsrc). Audio will \
                 be unavailable; the app still starts. Fix: use a newer build, or the \
                 .deb / your distribution's package instead of this bundle."
            );
        }
        MediaStackVerdict::MissingPlugins(missing) => {
            eprintln!(
                "phase.rs: GStreamer plugins needed for audio are missing. Audio will \
                 be unavailable; the app still starts. Install the packages below (or \
                 your distribution's equivalents) and restart:"
            );
            for plugin in missing {
                eprintln!(
                    "  {} — provides {} — Debian/Ubuntu package: {}",
                    plugin.library, plugin.provides, plugin.debian_package
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin directory holding `libraries`, inside `root`.
    fn plugin_dir(root: &Path, relative: &str, libraries: &[&str]) -> PathBuf {
        let dir = root.join(relative);
        std::fs::create_dir_all(&dir).unwrap();
        for library in libraries {
            std::fs::write(dir.join(library), b"").unwrap();
        }
        dir
    }

    fn all_libraries() -> Vec<&'static str> {
        REQUIRED_PLUGINS.iter().map(|p| p.library).collect()
    }

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("phase-media-stack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn complete_plugin_directory_is_usable() {
        let root = temp_root("complete");
        let dir = plugin_dir(&root, "gstreamer-1.0", &all_libraries());
        let env = MediaStackEnv {
            default_dirs: vec![dir],
            ..MediaStackEnv::default()
        };
        assert_eq!(verdict(&env), MediaStackVerdict::Usable);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn missing_libraries_are_named_with_their_package() {
        let root = temp_root("missing");
        let present: Vec<&str> = all_libraries()
            .into_iter()
            .filter(|library| *library != "libgstautodetect.so")
            .collect();
        let dir = plugin_dir(&root, "gstreamer-1.0", &present);
        let env = MediaStackEnv {
            default_dirs: vec![dir],
            ..MediaStackEnv::default()
        };
        match verdict(&env) {
            MediaStackVerdict::MissingPlugins(missing) => {
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].library, "libgstautodetect.so");
                assert_eq!(missing[0].debian_package, "gstreamer1.0-plugins-good");
            }
            other => panic!("expected MissingPlugins, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `GST_PLUGIN_SYSTEM_PATH*` replaces the distro directories, so a complete
    /// system directory must NOT rescue an override that points somewhere empty.
    #[test]
    fn system_path_override_replaces_the_default_directories() {
        let root = temp_root("override");
        let complete = plugin_dir(&root, "default", &all_libraries());
        let empty = plugin_dir(&root, "override", &[]);
        let env = MediaStackEnv {
            system_path: Some(vec![empty]),
            default_dirs: vec![complete],
            ..MediaStackEnv::default()
        };
        assert!(matches!(
            verdict(&env),
            MediaStackVerdict::MissingPlugins(_)
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// An explicitly empty `GST_PLUGIN_SYSTEM_PATH_1_0` means "no system plugin
    /// directories at all". Treating it as unset would restore the distro
    /// defaults and report a host as usable that GStreamer will find bare.
    #[test]
    fn an_empty_system_path_override_is_not_an_unset_one() {
        let root = temp_root("empty-override");
        let complete = plugin_dir(&root, "default", &all_libraries());
        let env = MediaStackEnv {
            system_path: Some(Vec::new()),
            default_dirs: vec![complete],
            ..MediaStackEnv::default()
        };
        match verdict(&env) {
            MediaStackVerdict::MissingPlugins(missing) => {
                assert_eq!(missing.len(), REQUIRED_PLUGINS.len());
            }
            other => panic!("expected every plugin missing, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The env reader itself must preserve set-versus-unset, not just the
    /// verdict: precedence is by EXISTENCE, so an empty versioned variable
    /// wins over a populated unversioned one instead of falling through to it.
    /// Driven through the env-independent seam — no process env is mutated.
    #[test]
    fn path_var_stops_at_the_first_variable_that_exists() {
        let names = ["VERSIONED", "UNVERSIONED"];
        let lookup = |set: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                set.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_string())
            }
        };

        assert_eq!(
            path_var_from(
                &names,
                lookup(&[("VERSIONED", ""), ("UNVERSIONED", "/host")])
            ),
            Some(Vec::new()),
            "an empty versioned variable is an answer, not a fall-through"
        );
        assert_eq!(
            path_var_from(&names, lookup(&[("UNVERSIONED", "/host")])),
            Some(vec![PathBuf::from("/host")]),
            "with the versioned variable absent, the unversioned one applies"
        );
        assert_eq!(path_var_from(&names, lookup(&[])), None);
    }

    /// Issue #6744: an AppImage carrying `libgstreamer-1.0.so` but pointed at the
    /// host's plugin directory. The host directory is complete, so only the
    /// bundled-core arm can catch this — a plain file-presence check calls it
    /// healthy and the app freezes on the loading screen.
    #[test]
    fn bundled_core_pointed_at_host_plugins_is_diagnosed() {
        let root = temp_root("bundled");
        let appdir = root.join("AppDir");
        plugin_dir(&appdir, "usr/lib", &["libgstreamer-1.0.so.0"]);
        let host = plugin_dir(&root, "host-gstreamer-1.0", &all_libraries());
        let env = MediaStackEnv {
            appdir: Some(appdir),
            default_dirs: vec![host],
            ..MediaStackEnv::default()
        };
        assert_eq!(
            verdict(&env),
            MediaStackVerdict::BundledCoreUnbundledPlugins
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Control arm for the test above: the same bundle, now with its plugins
    /// bundled too (what `bundleMediaFramework` produces). Only the search path
    /// moved, so the bundled-core assertion above is discriminating.
    #[test]
    fn bundled_core_with_bundled_plugins_is_usable() {
        let root = temp_root("bundled-ok");
        let appdir = root.join("AppDir");
        plugin_dir(&appdir, "usr/lib", &["libgstreamer-1.0.so.0"]);
        let bundled = plugin_dir(&appdir, "usr/lib/gstreamer-1.0", &all_libraries());
        let env = MediaStackEnv {
            appdir: Some(appdir),
            system_path: Some(vec![bundled]),
            ..MediaStackEnv::default()
        };
        assert_eq!(verdict(&env), MediaStackVerdict::Usable);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A bundle with no GStreamer core of its own loads the host's core, so the
    /// host's plugins are the right ones and nothing is wrong.
    #[test]
    fn bundle_without_a_gstreamer_core_uses_host_plugins() {
        let root = temp_root("no-core");
        let appdir = root.join("AppDir");
        plugin_dir(&appdir, "usr/lib", &["libsomethingelse.so"]);
        let host = plugin_dir(&root, "host-gstreamer-1.0", &all_libraries());
        let env = MediaStackEnv {
            appdir: Some(appdir),
            default_dirs: vec![host],
            ..MediaStackEnv::default()
        };
        assert_eq!(verdict(&env), MediaStackVerdict::Usable);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn empty_path_segments_are_dropped() {
        assert_eq!(split_path_var(""), Vec::<PathBuf>::new());
        assert_eq!(
            split_path_var("/a::/b"),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }
}
