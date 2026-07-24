//! Runtime coverage for the PR #6540 parse-diff set (issue #4240).
//!
//! The parser fix that taught `parse_damage_source_subject` the article-less
//! Aura/Equipment determiners ("enchanted creature" / "equipped creature")
//! changed six cards' parse output. Two moved in the TARGET position
//! (Sigil of Sleep, Hammer of Ruin) and four in the QUANTITY position
//! (Quietus Spike, Scytheclaw, Sword of War and Peace, Sword of Fire and Ice
//! and War and Peace). Every one of them resolves the same anaphor: the
//! `"that player"` / `"their"` in the effect body is the player the equipped
//! or enchanted creature just damaged.
//!
//! CR 120.3a + CR 301.5a + CR 603.2: damage dealt to a player by the equipped
//! creature (CR 301.5a defines "equipped creature") identifies that player as
//! the triggering event's player (CR 603.2), so `"that player"` / `"their"`
//! binds to the DAMAGED player — never to the Equipment's controller.
//!
//! Every test is deliberately DISCRIMINATING: the two players are given
//! different life totals / hand sizes, so binding the anaphor to the
//! controller instead of the damaged player would produce a different,
//! asserted-against number. Sigil of Sleep's own regression lives in
//! `issue_4240_sigil_of_sleep_target_scope`.
//!
//! # The two classes of parse-diff change, and what these tests prove
//!
//! Running this module against the parser fix REVERTED separates them:
//!
//! * **Behavioral (regression tests — they FAIL when the fix is reverted).**
//!   `hammer_of_ruin_*` fails with `target_slots.len() == 2`: the pre-fix
//!   `ControllerRef::TargetPlayer` binding surfaces a phantom companion player
//!   slot, which is the softlock reported in issue #4240.
//!
//! * **Representation-only (invariance tests — they PASS either way).**
//!   `quietus_spike_*`, `scytheclaw_*` and `sword_of_war_and_peace_*` pass
//!   identically with and without the fix. During trigger resolution
//!   `QuantityContext::scoped_player` is already bound to the damaged player,
//!   so the pre-fix `PlayerScope::ScopedPlayer` and the post-fix
//!   `PlayerScope::Target` resolve to the SAME player. Their entries in the
//!   PR's `coverage-parse-diff` are therefore a change of representation with
//!   no change in behavior. These tests are kept precisely to pin that
//!   invariance: they are the evidence that the shared-grammar edit did not
//!   alter what these cards do.

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const HAMMER_OF_RUIN_ORACLE: &str = "Whenever equipped creature deals combat damage to a player, you may destroy target Equipment that player controls.";

/// Quietus Spike and Scytheclaw print the identical triggered ability.
const HALF_LIFE_ORACLE: &str = "Whenever equipped creature deals combat damage to a player, that player loses half their life, rounded up.";

const SWORD_OF_WAR_AND_PEACE_ORACLE: &str = "Whenever equipped creature deals combat damage to a player, this Equipment deals damage to that player equal to the number of cards in their hand and you gain 1 life for each card in your hand.";

/// Drive the pending combat-damage trigger to full resolution.
///
/// Handles the two interactive windows a `"you may … target …"` trigger can
/// raise: CR 603.3d target declaration (skipped by the engine when exactly one
/// legal target exists) and the CR 603.3c optional decision, which is accepted
/// so the destroy actually happens. Returns the legal targets observed at the
/// target-selection window, if one was presented.
fn resolve_trigger(runner: &mut GameRunner) -> Option<Vec<TargetRef>> {
    let mut observed: Option<Vec<TargetRef>> = None;
    for _ in 0..30 {
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                let slots = target_slots.clone();
                assert_eq!(
                    slots.len(),
                    1,
                    "the trigger must create exactly one target slot, not a phantom player slot"
                );
                let chosen = slots[0]
                    .legal_targets
                    .first()
                    .cloned()
                    .expect("the trigger must have at least one legal target");
                observed = Some(slots[0].legal_targets.clone());
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![chosen],
                    })
                    .expect("selecting the only legal target must succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the optional trigger must let it resolve");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return observed;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must advance the combat-damage trigger");
            }
            other => panic!("unexpected state while resolving the trigger: {other:?}"),
        }
    }
    panic!("trigger never finished resolving");
}

/// CR 301.5a + CR 603.2: "target Equipment **that player** controls" must offer
/// only Equipment controlled by the DAMAGED player. Before the fix the anaphor
/// bound to `ControllerRef::TargetPlayer`, which surfaces a phantom companion
/// player slot and admits the wrong controller's permanents.
#[test]
fn hammer_of_ruin_targets_only_the_damaged_players_equipment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let equipped_creature = scenario.add_creature(P0, "Hammer Bearer", 2, 2).id();
    let hammer = scenario
        .add_creature(P0, "Hammer of Ruin", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(HAMMER_OF_RUIN_ORACLE)
        .id();
    let damaged_players_equipment = scenario
        .add_creature(P1, "Damaged Player's Equipment", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let controllers_equipment = scenario
        .add_creature(P0, "Controller's Equipment", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), hammer, equipped_creature);
    evaluate_layers(runner.state_mut());

    run_combat(&mut runner, vec![equipped_creature], vec![]);
    let observed_targets = resolve_trigger(&mut runner);

    // When a target-selection window is presented, the damaged player's
    // Equipment must be the ONLY legal choice. (With a single legal target the
    // engine may auto-select and skip the window entirely, so this is asserted
    // conditionally; the destroyed-zone assertions below are unconditional.)
    if let Some(legal) = observed_targets {
        assert_eq!(
            legal,
            vec![TargetRef::Object(damaged_players_equipment)],
            "only the damaged player's Equipment may be targeted"
        );
        assert!(
            !legal.contains(&TargetRef::Object(controllers_equipment)),
            "the controller's own Equipment must not be legal for 'that player controls'"
        );
    }

    // Behavioral discriminator: a controller-bound anaphor would have destroyed
    // the Equipment controller's own Equipment (or the Hammer) instead.
    assert_eq!(
        runner.state().objects[&damaged_players_equipment].zone,
        Zone::Graveyard,
        "Hammer of Ruin must destroy the DAMAGED player's Equipment"
    );
    assert_eq!(
        runner.state().objects[&controllers_equipment].zone,
        Zone::Battlefield,
        "the Equipment controller's own Equipment must survive"
    );
    assert_eq!(
        runner.state().objects[&hammer].zone,
        Zone::Battlefield,
        "Hammer of Ruin must not destroy itself"
    );
}

/// Shared driver for the Quietus Spike / Scytheclaw printed ability.
///
/// CR 119.3 + CR 603.2: "that player loses half **their** life" halves the
/// DAMAGED player's life. Controller life is set far apart from the damaged
/// player's so a controller-bound anaphor yields a different total.
///
/// INVARIANCE test (see module docs): this passes both with and without the
/// parser fix, because `scoped_player` is already the damaged player during
/// trigger resolution. It pins that the PR's parse-diff entry for these cards
/// is representation-only and changes no behavior.
fn assert_half_life_uses_damaged_player(card_name: &str) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Deliberately divergent: half of the controller's 40 is 20, half of the
    // damaged player's post-combat 28 is 14 — the two readings cannot collide.
    scenario.with_life(P0, 40);
    scenario.with_life(P1, 30);

    let equipped_creature = scenario.add_creature(P0, "Spike Bearer", 2, 2).id();
    let equipment = scenario
        .add_creature(P0, card_name, 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(HALF_LIFE_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), equipment, equipped_creature);
    evaluate_layers(runner.state_mut());

    run_combat(&mut runner, vec![equipped_creature], vec![]);
    runner.advance_until_stack_empty();

    // 30 life − 2 combat damage = 28; half of 28 rounded up = 14 lost → 14 left.
    // A controller-bound reading would halve 40 → lose 20 → 8 left.
    let damaged_life = runner.state().players[P1.0 as usize].life;
    assert_eq!(
        damaged_life, 14,
        "{card_name} must halve the DAMAGED player's life (28 → lose 14 → 14); \
         got {damaged_life} (8 would mean it halved the Equipment controller's 40)"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        40,
        "{card_name} must not touch the Equipment controller's life"
    );
}

#[test]
fn quietus_spike_halves_the_damaged_players_life() {
    assert_half_life_uses_damaged_player("Quietus Spike");
}

#[test]
fn scytheclaw_halves_the_damaged_players_life() {
    assert_half_life_uses_damaged_player("Scytheclaw");
}

/// CR 120.3a + CR 603.2: Sword of War and Peace deals damage equal to the
/// number of cards in **their** hand (the damaged player's), while the life
/// gain counts **your** hand (the controller's). Asserting both halves in one
/// test proves the damaged-player and controller-side references stay
/// correctly distinct.
///
/// INVARIANCE test (see module docs): passes with and without the parser fix.
/// It pins that the PR's `DealDamage.amount` parse-diff entry for this card
/// (and for Sword of Fire and Ice and War and Peace, which prints the same
/// "cards in their hand" clause) is representation-only.
#[test]
fn sword_of_war_and_peace_damage_uses_damaged_players_hand_and_life_gain_uses_yours() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // Divergent hand sizes: damaged player 5, controller 2.
    for i in 0..5 {
        scenario.add_card_to_hand(P1, &format!("Damaged Player Card {i}"));
    }
    for i in 0..2 {
        scenario.add_card_to_hand(P0, &format!("Controller Card {i}"));
    }

    let equipped_creature = scenario.add_creature(P0, "Sword Bearer", 1, 1).id();
    let sword = scenario
        .add_creature(P0, "Sword of War and Peace", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(SWORD_OF_WAR_AND_PEACE_ORACLE)
        .id();

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), sword, equipped_creature);
    evaluate_layers(runner.state_mut());

    run_combat(&mut runner, vec![equipped_creature], vec![]);
    runner.advance_until_stack_empty();

    // 20 − 1 combat − 5 (damaged player's hand) = 14.
    // A controller-bound reading would deal 2 instead of 5 → 17.
    let damaged_life = runner.state().players[P1.0 as usize].life;
    assert_eq!(
        damaged_life, 14,
        "damage must equal the DAMAGED player's hand size (5), leaving 14; \
         got {damaged_life} (17 would mean it counted the controller's 2-card hand)"
    );
    // Life gain still counts the CONTROLLER's hand (2) — unchanged by the fix.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        22,
        "life gain must count the controller's own hand (2)"
    );
}
