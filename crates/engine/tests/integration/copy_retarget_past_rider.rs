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
//!   - Hive Mind — "each other player copies that spell. Each of those players
//!     may choose new targets for their copy": the retarget permission rides the
//!     per-player `CopySpell`, so every copying player (not the caster) is
//!     offered the retarget choice for the copy they control.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, CopyRetargetPermission, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

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

/// CR 707.10 + CR 707.10c: Hive Mind. P0 casts Lightning Bolt at P0; the trigger has
/// each OTHER player (P1, P2) copy it, and each of them (not P0) decides whether
/// the copy they control gets new targets.
///
/// Before the "their copy" determiner was recognised the second sentence lowered
/// to a free-standing `ChangeTargets { Any }`: the `CopySpell` kept
/// `KeepOriginalTargets`, no copy ever opened a `CopyRetarget` prompt, and the
/// trigger declared a bogus any-target slot. Reverting the fix makes the first
/// `CopyRetarget` wait below never occur (the loop panics on its absence).
///
/// Reach guard: both copies are made (the loop sees one prompt per other player).
/// P1 sends its copy to P2 and P2 keeps the original target, so the final life
/// totals discriminate who decided: P0 takes the original 3 and the P2-copy 3,
/// P2 takes the P1-copy 3, P1 takes nothing.
#[test]
fn hive_mind_each_other_player_chooses_new_targets_for_their_own_copy() {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(
        P0,
        "Hive Mind",
        "Whenever a player casts an instant or sorcery spell, each other player copies that spell. Each of those players may choose new targets for their copy.",
    );
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(bolt).target_player(P0).commit();

    let mut choosers = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CopyRetarget { player, .. } => {
                // The first copying player redirects at P2; the second keeps.
                let target = if choosers.is_empty() {
                    Some(TargetRef::Player(P2))
                } else {
                    None
                };
                choosers.push(player);
                runner
                    .act(GameAction::ChooseTarget { target })
                    .expect("copy controller answers the retarget prompt");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving Hive Mind: {other:?}"),
        }
    }

    assert_eq!(
        {
            let mut sorted = choosers.clone();
            sorted.sort_by_key(|p| p.0);
            sorted
        },
        vec![P1, P2],
        "each other player, and only they, is asked to retarget their own copy: {choosers:?}"
    );
    assert_eq!(runner.life(P1), 20, "P1's copy was redirected away from P0");
    assert_eq!(
        runner.life(P2),
        17,
        "P2 takes the one copy redirected at it"
    );
    assert_eq!(
        runner.life(P0),
        14,
        "P0 takes the original Bolt and the copy whose controller kept the target"
    );
}
