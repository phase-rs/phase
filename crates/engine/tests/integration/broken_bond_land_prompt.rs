//! Issue #8123 — Broken Bond's "You may put a land card from your hand onto
//! the battlefield" sub-ability never surfaced a card-selection prompt after
//! the player accepted the optional choice, so no land ever reached the
//! battlefield. Distinct from the already-closed issue #2405 (land drop was
//! incorrectly consumed) — this is "no picker shown at all".
//!
//! Oracle (verbatim, client/public/card-data.json):
//!   Destroy target artifact or enchantment. You may put a land card from
//!   your hand onto the battlefield.
//!
//! CR 608.2d: an optional ("you may") effect prompts the controller before
//! executing; accepting must run the effect (here, an interactive
//! `EffectZoneChoice`, CR 608.2c, when more than one card matches the
//! resolution-time zone scan), and declining must run neither the prompt's
//! effect nor any zone move.
//!
//! Every test below drives the real `apply()` pipeline (GameScenario +
//! GameRunner::cast + CR 601.2 announce/target + CR 608 resolve) and asserts
//! battlefield/hand zone deltas — never AST shape — per the `card-test` skill.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const ORACLE: &str = "Destroy target artifact or enchantment. You may put a land card from your hand onto the battlefield.";

/// Build P0's turn at PreCombatMain with Broken Bond in hand, a legal artifact
/// target on the battlefield, and TWO lands in P0's hand — the multi-eligible
/// shape that forces `WaitingFor::EffectZoneChoice` (CR 608.2c) rather than the
/// single-eligible auto-pick shortcut, so the card-selection prompt itself is
/// exercised, not merely the zone move.
fn setup() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Broken Bond", false, ORACLE)
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Target Artifact", "")
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let island = scenario.add_land_to_hand(P0, "Island").id();
    let runner = scenario.build();
    (runner, spell, artifact, forest, island)
}

/// THE fix assertion: accepting the "may" choice must actually surface a
/// legal land-selection choice and put the selected land onto the
/// battlefield. Revert-failing: with the prompt never wired, `waiting_for`
/// never reaches `Priority` again cleanly and neither land ever leaves hand.
#[test]
fn accepting_the_choice_prompts_and_puts_the_selected_land_into_play() {
    let (mut runner, spell, artifact, forest, island) = setup();

    let outcome = runner
        .cast(spell)
        .target_objects(&[artifact])
        .accept_optional()
        .effect_zone(&[forest])
        .resolve();

    // THE fix assertion — a legal choice was actually offered AND acted on.
    assert_eq!(
        outcome.zone_of(forest),
        Zone::Battlefield,
        "CR 608.2d: accepting the optional land put must move the chosen \
         land onto the battlefield"
    );

    // Reach-guards proving this wasn't a vacuous pass:
    // (a) the spell actually resolved and destroyed its target (CR 701.8a).
    assert_ne!(
        outcome.zone_of(artifact),
        Zone::Battlefield,
        "the targeted artifact must have been destroyed"
    );
    // (b) the OTHER eligible land was left behind — a real choice was made
    // among multiple legal candidates, not an unconditional "move all lands"
    // fallback masquerading as the fix.
    assert_eq!(
        outcome.zone_of(island),
        Zone::Hand,
        "the un-chosen land must remain in hand — the prompt chose ONE card, \
         not every eligible land"
    );
}

/// CR 608.2d: declining the optional effect must run neither the prompt nor
/// any zone move — both lands stay in hand. Paired with a positive reach-guard
/// (the destroy still resolved) so this negative assertion cannot pass merely
/// because the whole ability failed to resolve at all.
#[test]
fn declining_the_choice_leaves_both_lands_in_hand() {
    let (mut runner, spell, artifact, forest, island) = setup();

    let outcome = runner
        .cast(spell)
        .target_objects(&[artifact])
        .decline_optional()
        .resolve();

    assert_eq!(
        outcome.zone_of(forest),
        Zone::Hand,
        "CR 608.2d: declining the optional land put must leave the land in hand"
    );
    assert_eq!(
        outcome.zone_of(island),
        Zone::Hand,
        "CR 608.2d: declining the optional land put must leave the land in hand"
    );

    // Reach-guard: the mandatory head effect still ran, proving the decline
    // path was reached via real resolution rather than a short-circuited cast.
    assert_ne!(
        outcome.zone_of(artifact),
        Zone::Battlefield,
        "the targeted artifact must still have been destroyed on decline"
    );
}
