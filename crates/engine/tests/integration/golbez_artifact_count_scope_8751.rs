//! Regression for GitHub issue #8751 — Golbez, Crystal Collector's second
//! sentence, "Then if you control eight or more artifacts, each opponent loses
//! life equal to that card's power", checks the artifact count for Golbez's
//! controller (CR 109.5: "you" is the ability's controller), not for each
//! opponent the life loss iterates over (CR 608.2c). The amount is the returned
//! card's power (CR 608.2c + CR 608.2h), not an opponent-scoped value.
//!
//! The Oracle text below is Golbez's printed text from the repo's card data;
//! the card is built through `add_creature_from_oracle` (1/4 legendary
//! creature, {U}{B}), the same shape as the printed card for this trigger.
//! Arterial Flow is the printed sibling with the same "If you control ...,
//! each opponent ..." shape (a spell, not a trigger).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GOLBEZ_ORACLE: &str = "Whenever an artifact you control enters, surveil 1.\n\
At the beginning of your end step, if you control four or more artifacts, return target creature card from your graveyard to your hand. Then if you control eight or more artifacts, each opponent loses life equal to that card's power.";

const RETURNED_POWER: i32 = 3;

/// Builds Golbez plus `artifacts` artifacts for P0, one creature card with
/// `RETURNED_POWER` power in P0's graveyard, and runs the end-step trigger to
/// completion. Returns the runner and the graveyard card.
fn run_golbez_end_step(artifacts: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Golbez, Crystal Collector", 1, 4, GOLBEZ_ORACLE);
    for i in 0..artifacts {
        scenario.add_artifact_from_oracle(P0, &format!("Test Artifact {i}"), "");
    }
    let card = scenario
        .add_creature_to_graveyard(P0, "Returned Bear", RETURNED_POWER, 3)
        .id();
    let mut runner = scenario.build();
    // P0 is the active player; the opponent controls no artifacts. Walk the
    // turn to the end step (declaring no attackers), then resolve the trigger.
    let mut reached_end = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                assert_eq!(runner.state().phase, Phase::End);
                reached_end = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(card)),
                    })
                    .expect("choose the graveyard creature");
            }
            WaitingFor::Priority { .. }
                if runner.state().phase == Phase::End && runner.state().stack.is_empty() =>
            {
                reached_end = true;
                break;
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    assert!(reached_end, "the turn must reach the end step");
    assert!(
        runner.state().stack.is_empty(),
        "trigger must have resolved"
    );
    (runner, card)
}

/// CR 109.5 + CR 608.2c: eight artifacts → the opponent loses life equal to the
/// returned card's power.
#[test]
fn golbez_eight_artifacts_opponent_loses_returned_cards_power() {
    let (runner, card) = run_golbez_end_step(8);
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    assert_eq!(runner.life(P1), 20 - RETURNED_POWER);
    assert_eq!(runner.life(P0), 20, "the controller loses nothing");
}

/// Seven artifacts: the second sentence's "eight or more" is false for the
/// controller, so the creature returns and nobody loses life.
#[test]
fn golbez_seven_artifacts_returns_without_life_loss() {
    let (runner, card) = run_golbez_end_step(7);
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    assert_eq!(runner.life(P1), 20);
}

/// Four artifacts: only the intervening-if of the first sentence holds.
#[test]
fn golbez_four_artifacts_returns_without_life_loss() {
    let (runner, card) = run_golbez_end_step(4);
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    assert_eq!(runner.life(P1), 20);
}

const ARTERIAL_FLOW_ORACLE: &str = "Each opponent discards two cards. If you control a Vampire, each opponent loses 2 life and you gain 2 life.";

/// Casts Arterial Flow ({1}{B}{B}, printed sorcery) with P0 controlling a
/// Vampire iff `vampire`, and returns the runner after resolution.
fn cast_arterial_flow(vampire: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Arterial Flow", false, ARTERIAL_FLOW_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    if vampire {
        scenario
            .add_creature(P0, "Test Vampire", 2, 2)
            .with_subtypes(vec!["Vampire"]);
    }
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    runner
}

/// CR 109.5 + CR 608.2c: "If you control a Vampire" is checked for the caster,
/// so the opponent (who controls no Vampire) loses 2 life. The caster's "you gain 2 life" is not asserted: the parse leaves that conjunct unconditional, a separate defect outside this fix.
#[test]
fn arterial_flow_with_vampire_drains_each_opponent() {
    let runner = cast_arterial_flow(true);
    assert_eq!(runner.life(P1), 18);
}

/// Negative control: no Vampire for the caster, no drain.
#[test]
fn arterial_flow_without_vampire_does_not_drain() {
    let runner = cast_arterial_flow(false);
    assert_eq!(runner.life(P1), 20);
}

const LILIANAS_TRIUMPH_ORACLE: &str = "Each opponent sacrifices a creature of their choice. If you control a Liliana planeswalker, each opponent also discards a card.";

/// Casts Liliana's Triumph ({1}{B}, printed instant) with P0 controlling a
/// Liliana planeswalker iff `liliana`; P1 holds one card and no creatures.
/// Returns the runner and P1's hand card.
fn cast_lilianas_triumph(liliana: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Liliana's Triumph", true, LILIANAS_TRIUMPH_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    if liliana {
        scenario.add_planeswalker_from_oracle(P0, "Test Liliana", "Liliana", 3, "");
    }
    let opponent_card = scenario.add_card_to_hand(P1, "Opponent's Card");
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    (runner, opponent_card)
}

/// CR 109.5: "If you control a Liliana planeswalker" is checked for the caster,
/// so the opponent (who controls none) discards.
#[test]
fn lilianas_triumph_with_liliana_discards_from_each_opponent() {
    let (runner, card) = cast_lilianas_triumph(true);
    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
}

/// Negative control: no Liliana for the caster, no discard.
#[test]
fn lilianas_triumph_without_liliana_does_not_discard() {
    let (runner, card) = cast_lilianas_triumph(false);
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
}
