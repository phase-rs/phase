//! Legacy on-disk compatibility: the shared version arithmetic behind the
//! `LEGACY_*` tripwires indexed in `docs/LEGACY-COMPAT.md`.
//!
//! A compat path accepts an OLDER on-disk shape beside the current one. Each such
//! path carries a `LEGACY_*` tripwire const beside the branch it guards, whose
//! `assert!` fails to COMPILE once the workspace version crosses the documented
//! removal boundary — so a compat path cannot outlive its deadline by inattention,
//! and the index and the source cannot drift apart.
//!
//! Only the arithmetic lives here; the tripwires themselves sit with the branches
//! they guard (`ResolvedRulesJournal::node_settled`,
//! `PendingManaAbility::activation_producer_evidence`).

/// The documented soft window: `+14` patch releases after the landing version, which
/// is the catch-up room `docs/LEGACY-COMPAT.md` gives downstream clients before a
/// compat path must go.
pub(crate) const LEGACY_REMOVAL_WINDOW_PATCH: u32 = 14;

/// One numeric component of `CARGO_PKG_VERSION_*` as a `u32`.
///
/// `env!` yields a `&'static str` at compile time, so this consumes ASCII digits in a
/// `const fn` — the only place a tripwire can compare versions while building.
pub(crate) const fn parse_version_component(component: &str) -> u32 {
    let bytes = component.as_bytes();
    let mut value: u32 = 0;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        assert!(
            byte >= b'0' && byte <= b'9',
            "non-digit in version component"
        );
        value = value * 10 + (byte - b'0') as u32;
        index += 1;
    }
    value
}

/// `true` while `now` is still at or below `boundary`.
///
/// `boundary` is the LAST version that may still carry the shim, so a tripwire built
/// on it fires the moment a release ABOVE it lands. Both tuples are
/// `(major, minor, patch)`.
pub(crate) const fn within_legacy_window(boundary: (u32, u32, u32), now: (u32, u32, u32)) -> bool {
    let (boundary_major, boundary_minor, boundary_patch) = boundary;
    let (now_major, now_minor, now_patch) = now;
    !(now_major > boundary_major
        || (now_major == boundary_major
            && (now_minor > boundary_minor
                || (now_minor == boundary_minor && now_patch > boundary_patch))))
}
