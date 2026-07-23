//! God-Pharaoh's Gift (#2350) — a second same-turn resolution must copy the
//! creature THIS resolution exiled, not the one a prior same-turn resolution
//! exiled.
//!
//! Oracle (the relevant triggered ability): "you may exile a creature card from
//! your graveyard. If you do, create a token that's a copy of that card, except
//! it's a 4/4 black Zombie ... with haste."
//!
//! The "If you do, create a token that's a copy of that card" rider is a
//! tracked-set CONSUMER: `CopyTokenOf { target: TrackedSet }` whose "that card"
//! anaphor (CR 707.2a — a copy acquires the copiable values of the object it's
//! copying) names the very card the gating exile published into the chain's
//! tracked set during THIS resolution.
//!
//! CR 608.2c / CR 603.7: the rider resolves in the same instruction chain as
//! the gating exile, so "that card" must bind to the chain-local
//! `chain_tracked_set_id`, not the turn-global fallback.
//!
//! Pre-fix bug: the "If you do" boundary unconditionally reset
//! `chain_tracked_set_id = None` (added for Party Thrasher's PRODUCER rider,
//! #1977). That orphaned GPG's consumer rider to the turn-global
//! `latest_tracked_set_id` fallback. When GPG resolved twice in one turn, both
//! exiled cards' tracked sets persisted, so the second resolution's copy bound
//! to the FIRST resolution's exiled card — copying the wrong creature.
//!
//! The fix gates the reset behind `!effect_references_tracked_set(&sub.effect)`:
//! consumer riders (GPG) keep the chain-local set; producer riders (Party
//! Thrasher's `ExileTop`) still get the reset. This test fails on revert: the
//! second token would copy "Grizzly Bears" instead of "Walking Corpse".

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use std::collections::HashSet;

/// The effect body of God-Pharaoh's Gift's end-step triggered ability. The
/// "At the beginning of your end step," trigger prefix is the trigger
/// condition; `parse_effect_chain` parses the EFFECT chain it gates, so we pass
/// only that body (mirrors how the trigger parser threads the effect through
/// `resolve_ability_chain`).
const ORACLE: &str = "You may exile a creature card from your graveyard. If you do, create a token that's a copy of that card, except it's a 4/4 black Zombie with haste.";

/// Returns the set of copy-token object ids currently on the battlefield. A
/// copy token is a battlefield object that is NOT one of the originally placed
/// non-token objects we track ourselves.
fn battlefield_object_ids(state: &engine::types::game_state::GameState) -> HashSet<ObjectId> {
    state.battlefield.iter().copied().collect()
}

/// GPG's top instruction is the optional "you may exile a creature card from
/// your graveyard." `resolve_ability_chain` parks at
/// `WaitingFor::OptionalEffectChoice` (stashing the chain in
/// optional-effect frame) instead of performing the exile. Accept the
/// optional through the real `apply` pipeline so the gating exile and its
/// `CopyTokenOf` "If you do" rider actually execute — mirroring how the engine
/// resumes a suspended "you may" decision in production (CR 117.3a: "you may"
/// prompts the acting player; CR 608.2c: the rider resolves in the same chain).
/// The accept re-enters the chain at depth 1, so the resolution's chain-local
/// tracked set (established by the now-performed exile) is exactly what the
/// rider's "that card" anaphor must bind to.
fn accept_pending_optional(runner: &mut engine::game::scenario::GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "expected the optional 'you may exile' to pause at OptionalEffectChoice, \
         got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional exile must succeed");
}

#[test]
fn second_instance_copies_its_own_exiled_card_not_the_first() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two DISTINCT creature cards in P0's graveyard. Resolution #1 exiles gy_a,
    // resolution #2 exiles gy_b. Distinct names let us discriminate which card
    // each created token copied.
    let gy_a = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let gy_b = scenario
        .add_creature_to_graveyard(P0, "Walking Corpse", 2, 2)
        .id();

    // GPG itself is on the battlefield; use a land as a stand-in source object
    // so the source is never part of any creature/copy filter.
    let source = scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();

    // Parse the real Oracle text into the exile -> CopyTokenOf -> Haste chain.
    let def = parse_effect_chain(ORACLE, AbilityKind::Spell);

    // --- Resolution #1: exile Grizzly Bears (gy_a). ---
    let before_res1 = battlefield_object_ids(runner.state());
    let ability1 =
        build_resolved_from_def_with_targets(&def, source, P0, vec![TargetRef::Object(gy_a)]);
    let mut events1 = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability1, &mut events1, 0)
        .expect("GPG resolution #1 must resolve");
    // Accept the "you may exile" optional so the gating exile + copy rider run.
    accept_pending_optional(&mut runner);

    // The gating exile moved gy_a out of the graveyard.
    assert_eq!(
        runner.state().objects[&gy_a].zone,
        Zone::Exile,
        "resolution #1 must exile Grizzly Bears from the graveyard"
    );

    // Exactly one new battlefield object: the copy token created by res #1.
    let after_res1 = battlefield_object_ids(runner.state());
    let new_after_res1: Vec<ObjectId> = after_res1.difference(&before_res1).copied().collect();
    assert_eq!(
        new_after_res1.len(),
        1,
        "resolution #1 must create exactly one copy token, got {new_after_res1:?}"
    );
    let token1 = new_after_res1[0];
    assert_eq!(
        runner.state().objects[&token1].name,
        "Grizzly Bears",
        "resolution #1's token must copy the card IT exiled (Grizzly Bears)"
    );

    // --- Resolution #2: exile Walking Corpse (gy_b). ---
    let before_res2 = battlefield_object_ids(runner.state());
    let ability2 =
        build_resolved_from_def_with_targets(&def, source, P0, vec![TargetRef::Object(gy_b)]);
    let mut events2 = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability2, &mut events2, 0)
        .expect("GPG resolution #2 must resolve");
    // Accept the second resolution's "you may exile" optional.
    accept_pending_optional(&mut runner);

    assert_eq!(
        runner.state().objects[&gy_b].zone,
        Zone::Exile,
        "resolution #2 must exile Walking Corpse from the graveyard"
    );

    // The token created by res #2 is the one new battlefield object since res #1.
    let after_res2 = battlefield_object_ids(runner.state());
    let new_after_res2: Vec<ObjectId> = after_res2.difference(&before_res2).copied().collect();
    assert_eq!(
        new_after_res2.len(),
        1,
        "resolution #2 must create exactly one copy token, got {new_after_res2:?}"
    );
    let token2 = new_after_res2[0];

    // THE DISCRIMINATING ASSERTION: the second token must copy the card THIS
    // resolution exiled (Walking Corpse), NOT the card the first resolution
    // exiled (Grizzly Bears). On revert, the unconditional reset orphans the
    // anaphor to the turn-global fallback (max-by-id non-empty set = gy_a's set),
    // so token2.name would be "Grizzly Bears" and this flips.
    assert_eq!(
        runner.state().objects[&token2].name,
        "Walking Corpse",
        "resolution #2's token must copy the card IT exiled (Walking Corpse), \
         not the first resolution's exiled card (Grizzly Bears)"
    );

    // Exactly two copy tokens exist overall, one per distinct exiled card —
    // no duplicate-named copy of Grizzly Bears.
    let copy_token_names: Vec<&str> = [token1, token2]
        .iter()
        .map(|id| runner.state().objects[id].name.as_str())
        .collect();
    assert!(
        copy_token_names.contains(&"Grizzly Bears"),
        "one copy token must be Grizzly Bears, got {copy_token_names:?}"
    );
    assert!(
        copy_token_names.contains(&"Walking Corpse"),
        "one copy token must be Walking Corpse, got {copy_token_names:?}"
    );
    assert_ne!(
        token1, token2,
        "the two resolutions must create distinct tokens"
    );
}
