//! The staleness guard shared by the game and draft LLM flows.
//!
//! An LLM decision is inherently two-phase: the engine builds a request, a
//! transport performs a network round trip, and the engine resolves the reply.
//! Between those halves the position can move on — the player conceded, a
//! trigger resolved, the pack passed. The option indices in the reply would then
//! name different options than the ones the model was shown.
//!
//! A fingerprint taken over the exact option domain at request time and checked
//! again at resolve time closes that window: a mismatch is
//! [`LlmError::StaleDecision`](crate::error::LlmError::StaleDecision) and the
//! caller falls back, rather than applying an action nobody chose.

/// FNV-1a over the option domain. A hash, not a cryptographic commitment: the
/// engine re-validates every resolved action against a freshly issued authority
/// contract regardless, so this only has to catch honest drift.
pub fn fingerprint_of<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for part in parts {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        // Length-delimit so ["ab","c"] and ["a","bc"] cannot collide.
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_domain_always_fingerprints_the_same() {
        assert_eq!(
            fingerprint_of(["a", "b", "c"]),
            fingerprint_of(["a", "b", "c"])
        );
    }

    #[test]
    fn reordering_or_changing_the_domain_changes_the_fingerprint() {
        let base = fingerprint_of(["a", "b"]);
        assert_ne!(base, fingerprint_of(["b", "a"]));
        assert_ne!(base, fingerprint_of(["a", "b", "c"]));
        assert_ne!(base, fingerprint_of(["a", "B"]));
    }

    #[test]
    fn part_boundaries_are_not_collapsible() {
        assert_ne!(fingerprint_of(["ab", "c"]), fingerprint_of(["a", "bc"]));
    }
}
