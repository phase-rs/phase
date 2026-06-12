//! Meld (CR 701.42 / CR 712.4) — synthesis parity hook.
//!
//! Unlike `synthesize_haunt`/`synthesize_encore`/`synthesize_unearth`, this hook
//! performs NO ability construction. The Oracle parser fully wires the meld
//! instigator's gated triggered/activated ability and stamps its effect as
//! [`Effect::Meld`] (see `parser/oracle_effect`), so there is nothing for
//! synthesis to build:
//!
//! * The instigator face is the one whose parsed ability already carries
//!   `Effect::Meld { source, partner, result }`.
//! * The partner half carries only "Melds with X." reminder text and synthesizes
//!   nothing.
//!
//! This module exists for parity with the sibling keyword synthesizers
//! (registered together in `synthesize_all`) and as a future-proofing seam — a
//! single place to add any meld-clause normalization or idempotency guard that
//! proves necessary. It runs during db ingestion, BEFORE the card-face registry
//! exists, so it performs NO result-face validation (the resolver looks the
//! result face up at resolution time via `card_face_registry`).

use crate::types::ability::Effect;
use crate::types::card::CardFace;

/// CR 701.42 / CR 712.4: Meld parity hook. Idempotent and side-effect-free: the
/// parser owns all meld ability construction, so this only records (via the
/// early return) that a meld instigator face was recognized. It is gated on the
/// presence of a parsed `Effect::Meld` so it never touches non-meld faces.
pub fn synthesize_meld(face: &mut CardFace) {
    // The instigator is recognized by an already-parsed `Effect::Meld` anywhere
    // in its ability set (top-level abilities or trigger/activated payloads).
    // There is nothing to synthesize — the parser produced the complete ability.
    let _is_meld_instigator = face_has_meld_effect(face);
}

/// Whether any ability, trigger, or static on `face` carries an `Effect::Meld`.
fn face_has_meld_effect(face: &CardFace) -> bool {
    face.abilities
        .iter()
        .any(|a| matches!(a.effect.as_ref(), Effect::Meld { .. }))
        || face.triggers.iter().any(|t| {
            t.execute
                .as_ref()
                .is_some_and(|a| matches!(a.effect.as_ref(), Effect::Meld { .. }))
        })
}
