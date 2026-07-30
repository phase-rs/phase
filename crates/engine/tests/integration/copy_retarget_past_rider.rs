//! CR 707.10c: end-to-end regression coverage for the copy-retarget building
//! block — "You may choose new targets for the copy/copies" must attach the
//! `MayChooseNewTargets` permission to every reachable `CopySpell`, even when a
//! clause splits the copy from the retarget sentence, and even when a card makes
//! more than one copy. These drive the full parser via `parse_oracle_text`
//! (complementing the in-module unit tests in `oracle_effect/tests.rs`):
//!   - Narset's Reversal — a Bounce rider ("... then return it to its owner's
//!     hand") sits between the copy and the retarget clause.
//!   - Spinerock's Tyrant — a static-grant rider ("those spells gain wither")
//!     sits between the copy and the retarget clause.
//!   - Increasing Vengeance — the flashback "copy that spell twice instead"
//!     nests a second `CopySpell`, so both the base copy and the instead copy
//!     must gain `MayChooseNewTargets`.
//!   - Twincast — the no-rider control (copy immediately followed by the
//!     retarget clause) stays correct.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, CopyRetargetPermission, Effect};

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Walk a def tree (effect + `sub_ability` chain + `else_ability`) collecting
/// every effect, so assertions don't depend on the exact chain depth.
fn collect_effects<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
    out.push(&def.effect);
    if let Some(sub) = def.sub_ability.as_deref() {
        collect_effects(sub, out);
    }
    if let Some(els) = def.else_ability.as_deref() {
        collect_effects(els, out);
    }
}

fn copy_retarget(effects: &[&Effect]) -> Option<CopyRetargetPermission> {
    effects.iter().find_map(|e| match e {
        Effect::CopySpell { retarget, .. } => Some(retarget.clone()),
        _ => None,
    })
}

fn has_unimplemented(effects: &[&Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::Unimplemented { .. }))
}

#[test]
fn increasing_vengeance_retargets_both_base_and_instead_copies() {
    // CR 707.10c: Increasing Vengeance copies the target spell once normally, or
    // "twice instead" when cast from a graveyard (a `ConditionInstead` override
    // whose `CopySpell` lives in `sub_ability`). The single "You may choose new
    // targets for the copies" clause must retarget BOTH copy branches. Before
    // patching every reachable copy (and dropping the early break), only the
    // base copy gained `MayChooseNewTargets` while the instead copy kept
    // `KeepOriginalTargets`.
    let parsed = parse_oracle_text(
        "Copy target instant or sorcery spell you control. If this spell was cast from a \
         graveyard, copy that spell twice instead. You may choose new targets for the copies.",
        "Increasing Vengeance",
        &strs(&[]),
        &strs(&["Instant"]),
        &strs(&[]),
    );

    let mut effects = Vec::new();
    for a in &parsed.abilities {
        collect_effects(a, &mut effects);
    }

    let copies: Vec<&CopyRetargetPermission> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::CopySpell { retarget, .. } => Some(retarget),
            _ => None,
        })
        .collect();

    // Both the base copy and the "copy twice instead" copy must be present.
    assert_eq!(
        copies.len(),
        2,
        "expected the base copy and the instead copy, got {effects:#?}"
    );
    // Every copy branch must allow choosing new targets — not just the first.
    assert!(
        copies
            .iter()
            .all(|r| matches!(r, CopyRetargetPermission::MayChooseNewTargets)),
        "every copy branch must gain MayChooseNewTargets, got {copies:#?}"
    );
}

#[test]
fn narsets_reversal_copy_carries_retarget_and_preserves_bounce_rider() {
    let parsed = parse_oracle_text(
        "Copy target instant or sorcery spell, then return it to its owner's hand. \
         You may choose new targets for the copy.",
        "Narset's Reversal",
        &strs(&["Copy"]),
        &strs(&["Instant"]),
        &strs(&[]),
    );

    let mut effects = Vec::new();
    for a in &parsed.abilities {
        collect_effects(a, &mut effects);
    }

    // The copy must carry MayChooseNewTargets (was KeepOriginalTargets before fix).
    assert_eq!(
        copy_retarget(&effects),
        Some(CopyRetargetPermission::MayChooseNewTargets),
        "Narset's copy must gain MayChooseNewTargets",
    );
    // The "return it to its owner's hand" rider must NOT be clobbered.
    assert!(
        effects.iter().any(|e| matches!(e, Effect::Bounce { .. })),
        "return-to-hand rider must be preserved",
    );
    // The retarget clause must have been absorbed, not left as Unimplemented.
    assert!(
        !has_unimplemented(&effects),
        "retarget clause must not lower to Unimplemented: {effects:#?}",
    );
}

#[test]
fn spinerocks_tyrant_copy_carries_retarget_and_preserves_wither_rider() {
    let parsed = parse_oracle_text(
        "Whenever you cast a spell, you may copy it. If you do, those spells gain wither. \
         You may choose new targets for the copy.",
        "Spinerock's Tyrant",
        &strs(&[]),
        &strs(&["Creature"]),
        &strs(&["Dragon"]),
    );

    let mut effects = Vec::new();
    for t in &parsed.triggers {
        if let Some(exec) = t.execute.as_deref() {
            collect_effects(exec, &mut effects);
        }
    }

    assert_eq!(
        copy_retarget(&effects),
        Some(CopyRetargetPermission::MayChooseNewTargets),
        "Spinerock's copy must gain MayChooseNewTargets",
    );
    // The "those spells gain wither" rider (a GenericEffect static grant) must
    // remain present alongside the copy.
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::GenericEffect { .. })),
        "gain-wither rider must be preserved: {effects:#?}",
    );
    assert!(
        !has_unimplemented(&effects),
        "retarget clause must not lower to Unimplemented: {effects:#?}",
    );
}

#[test]
fn adjacent_copy_retarget_no_rider_unchanged() {
    // No-regression: copy immediately followed by the retarget clause (no rider).
    let parsed = parse_oracle_text(
        "Copy target instant or sorcery spell. You may choose new targets for the copy.",
        "Twincast",
        &strs(&["Copy"]),
        &strs(&["Instant"]),
        &strs(&[]),
    );

    let mut effects = Vec::new();
    for a in &parsed.abilities {
        collect_effects(a, &mut effects);
    }

    assert_eq!(
        copy_retarget(&effects),
        Some(CopyRetargetPermission::MayChooseNewTargets),
        "adjacent retarget must still attach to the copy",
    );
    assert!(!has_unimplemented(&effects));
}
