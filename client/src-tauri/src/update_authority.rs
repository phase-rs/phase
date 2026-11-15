//! Which system is responsible for replacing this installation's files.
//!
//! The shell ships in two shapes. Self-managed bundles — `.AppImage`, `.deb`,
//! `.dmg`, `.exe` — are installed by the user and `tauri_plugin_updater`
//! downloads and swaps them in place. A Flatpak is the other shape: `/app` is
//! mounted read-only and `flatpak update` performs upgrades out of band, so the
//! shell replacing its own binaries is both impossible and wrong to attempt.
//!
//! This distinction cannot be expressed in `tauri.conf.json`. `tauri.conf.json`
//! sets `createUpdaterArtifacts`, which only decides whether the *build*
//! produces updater signatures; it has no bearing on whether a running binary
//! checks for updates. The Flatpak also reuses the binary the Linux release job
//! already built, so no Flatpak-specific build config is consulted at all. And
//! the check is driven from the web app — `client/src/pwa/tauriUpdater.ts`,
//! served remotely and shared with every other desktop package — so the
//! frontend cannot gate it either. The shell is the only layer that knows how
//! it was installed, which makes this the seam that owns the policy.
//!
//! Known limit: this is enforced as the plugin's *default* comparator, and
//! `check({ allowDowngrades: true })` replaces it outright with
//! `update.version != current` rather than composing with it (see the plugin's
//! `commands.rs`). No caller passes that option today —
//! `client/src/pwa/tauriUpdater.ts` calls a bare `check()` — but the web app is
//! versioned independently of this binary, so a future frontend could reach
//! past this guard. Closing that would mean withholding the updater's install
//! permission from sandboxed builds rather than answering "no" to every
//! release; it is deliberately not attempted here.

use semver::Version;
use std::path::Path;

/// The canonical marker for "running inside a Flatpak sandbox". `flatpak-run`
/// writes this file into every sandbox, and it is what GLib's own
/// containerisation check looks for. Preferred over the `FLATPAK_ID`
/// environment variable, which child processes inherit and anything can set.
const FLATPAK_INFO: &str = "/.flatpak-info";

/// Who owns replacing this installation's files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateAuthority {
    /// A self-managed bundle: the shell may replace its own installation.
    Shell,
    /// A Flatpak: the sandbox owns a read-only `/app`, and `flatpak update`
    /// performs upgrades.
    Flatpak,
}

impl UpdateAuthority {
    /// Resolves the authority for the running process.
    pub fn detect() -> Self {
        Self::detect_at(Path::new(FLATPAK_INFO))
    }

    /// Test seam for [`detect`](Self::detect); the marker path is a parameter
    /// so both arms are reachable without a sandbox to run under.
    fn detect_at(flatpak_info: &Path) -> Self {
        if flatpak_info.exists() {
            Self::Flatpak
        } else {
            Self::Shell
        }
    }

    /// Whether an available release should be installed by the shell itself.
    ///
    /// This is the whole of `tauri_plugin_updater`'s decision: supplying a
    /// comparator *replaces* the plugin's built-in `candidate > current` test
    /// rather than running alongside it, so the `Shell` arm has to restate it.
    /// Dropping that comparison would make every self-managed install offer
    /// whatever version the manifest happens to name, downgrades included.
    pub fn should_install(self, current: &Version, candidate: &Version) -> bool {
        match self {
            Self::Shell => candidate > current,
            Self::Flatpak => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Version {
        Version::parse(raw).unwrap()
    }

    /// Both arms, driven off paths whose existence is already guaranteed by the
    /// crate layout, so the test needs no temporary files or dev-dependency.
    #[test]
    fn detect_reads_the_flatpak_sandbox_marker() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let present = root.join("Cargo.toml");
        let absent = root.join("this-crate-is-not-a-flatpak-sandbox");

        assert!(present.exists(), "fixture path must exist");
        assert!(!absent.exists(), "fixture path must not exist");
        assert_eq!(
            UpdateAuthority::detect_at(&present),
            UpdateAuthority::Flatpak
        );
        assert_eq!(UpdateAuthority::detect_at(&absent), UpdateAuthority::Shell);
    }

    /// The marker is an absolute path deliberately: resolving it relative to the
    /// working directory would make the verdict depend on where the shell was
    /// launched from. Pin the exact constant — an equality against `detect()`
    /// would restate that function's definition and hold even if it regressed
    /// to returning one arm unconditionally.
    #[test]
    fn sandbox_marker_is_the_absolute_flatpak_run_path() {
        assert_eq!(FLATPAK_INFO, "/.flatpak-info");
        assert!(Path::new(FLATPAK_INFO).is_absolute());
    }

    /// A self-managed bundle keeps the plugin's stock semantics exactly.
    #[test]
    fn shell_installs_only_strictly_newer_releases() {
        let current = v("1.0.12");
        assert!(UpdateAuthority::Shell.should_install(&current, &v("1.0.13")));
        assert!(UpdateAuthority::Shell.should_install(&current, &v("2.0.0")));
        assert!(!UpdateAuthority::Shell.should_install(&current, &current));
        assert!(!UpdateAuthority::Shell.should_install(&current, &v("1.0.11")));
    }

    /// The guard itself: a Flatpak refuses every release, including the newer
    /// one a self-managed bundle would take, so `check()` resolves to "no
    /// update" instead of trying to write to a read-only `/app`.
    #[test]
    fn flatpak_refuses_every_release_the_shell_would_take() {
        let current = v("1.0.12");
        for candidate in ["1.0.13", "2.0.0", "1.0.12", "1.0.11"] {
            assert!(
                !UpdateAuthority::Flatpak.should_install(&current, &v(candidate)),
                "flatpak must not self-install {candidate}"
            );
        }
        assert!(UpdateAuthority::Shell.should_install(&current, &v("1.0.13")));
    }
}
