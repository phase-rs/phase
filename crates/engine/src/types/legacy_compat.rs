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

/// The documented soft window: `+14` releases after the version that SHIPS the
/// shim, which is the catch-up room `docs/LEGACY-COMPAT.md` gives downstream
/// clients before a compat path must go.
///
/// Counted in RELEASES, and this workspace releases by bumping the MINOR component
/// (`v0.121.0`, `v0.122.0`, …) — it has never shipped a patch release. Counting the
/// window in patch positions therefore collapses it to the single next release:
/// a shim installed at 0.122.0 would carry the boundary 0.122.14, and 0.123.0 would
/// cross it, breaking `main` and every open PR at each release. See
/// [`legacy_removal_boundary`].
pub(crate) const LEGACY_REMOVAL_WINDOW_RELEASES: u32 = 14;

/// The last version that may still carry a shim that ships in `shipped_in`.
///
/// Releases bump the minor component, so the window advances minor and resets
/// patch: a shim shipping in 0.123.0 stays legal through 0.137.0 and its tripwire
/// fires at 0.138.0.
pub(crate) const fn legacy_removal_boundary(shipped_in: (u32, u32, u32)) -> (u32, u32, u32) {
    let (major, minor, _patch) = shipped_in;
    (major, minor + LEGACY_REMOVAL_WINDOW_RELEASES, 0)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_version_components() {
        assert_eq!(parse_version_component("0"), 0);
        assert_eq!(parse_version_component("122"), 122);
        assert_eq!(parse_version_component("14"), 14);
    }

    /// The window must survive the release that follows the one the shim ships in.
    /// Counting it in patch positions instead put the boundary at `0.122.14`, so
    /// `main`'s next release crossed it and every open PR failed to compile.
    #[test]
    fn window_spans_more_than_the_next_release() {
        let shipped_in = (0u32, 123u32, 0u32);
        let boundary = legacy_removal_boundary(shipped_in);
        assert_eq!(boundary, (0, 123 + LEGACY_REMOVAL_WINDOW_RELEASES, 0));
        assert!(
            within_legacy_window(boundary, (0, 124, 0)),
            "a shim shipping in 0.123.0 must still be legal at 0.124.0, boundary {boundary:?}"
        );
        assert!(within_legacy_window(boundary, (0, 137, 0)));
        assert!(!within_legacy_window(boundary, (0, 138, 0)));
    }
}
