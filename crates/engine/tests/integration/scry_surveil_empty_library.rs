//! CR 701.22b/CR 701.22d + CR 701.25c/CR 701.25d: an instructed scry or
//! surveil of 1 or more against an EMPTY library still completes — no prompt
//! is offered (nothing to look at), but the `PlayerPerformedAction` event
//! still publishes, so "whenever you scry"/"whenever you surveil" triggers
//! fire and `player_actions_this_turn` records the action. An instructed
//! count of exactly 0 remains a true no-op (CR 701.22b/CR 701.25c): no event,
//! no ledger entry.
//!
//! Every row asserts the spell resolved (stack empty) as its reach-guard —
//! failing to reach that point (a stuck prompt) would satisfy the negative
//! assertions vacuously.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::PlayerActionKind;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// Kenessos, Priest of Thassa's static line, placed on a plain
/// (non-legendary) creature: "If you would scry a number of cards, scry that
/// many cards plus one instead."
const KENESSOS_SCRY_STATIC: &str =
    "If you would scry a number of cards, scry that many cards plus one instead.";

const CHANCE_MET_ELVES: &str =
    "Whenever you scry, put a +1/+1 counter on this creature. This ability triggers only once each turn.";

const DIMIR_SPYBUG: &str =
    "Flying\nMenace\nWhenever you surveil, put a +1/+1 counter on this creature.";

fn assert_spell_resolved(runner: &engine::game::scenario::GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the spell must have resolved off the stack"
    );
}

fn scry_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(player, action)| *player == P0 && *action == PlayerActionKind::Scry)
        .count()
}

fn surveil_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .player_actions_this_turn
        .iter()
        .filter(|(player, action)| *player == P0 && *action == PlayerActionKind::Surveil)
        .count()
}

/// `Scry 1.` against an empty library — no `ScryChoice` offered, but the
/// scry is still recorded once.
#[test]
fn scry_against_empty_library_records_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry Spell", true, "Scry 1.")
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().players[0].library.is_empty());

    runner.cast(spell).resolve();

    assert_spell_resolved(&runner);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ScryChoice { .. }),
        "an empty library must not offer a ScryChoice prompt"
    );
    assert_eq!(
        scry_count(&runner),
        1,
        "CR 701.22d: an instructed scry against an empty library still completes"
    );
}

/// The same scenario with Chance-Met Elves on the battlefield — its
/// "whenever you scry" trigger must fire off the published event.
#[test]
fn scry_against_empty_library_still_fires_whenever_you_scry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elves = scenario
        .add_creature_from_oracle(P0, "Chance-Met Elves", 3, 2, CHANCE_MET_ELVES)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry Spell", true, "Scry 1.")
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().players[0].library.is_empty());

    runner.cast(spell).resolve();

    assert_spell_resolved(&runner);
    assert_eq!(
        *runner.state().objects[&elves]
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0),
        1,
        "CR 701.22d authorizes the trigger even though nothing was looked at"
    );
}

/// `Surveil 1.` against an empty library — no `SurveilChoice` offered,
/// but the surveil is still recorded once.
#[test]
fn surveil_against_empty_library_records_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Surveil Spell", true, "Surveil 1.")
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().players[0].library.is_empty());

    runner.cast(spell).resolve();

    assert_spell_resolved(&runner);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::SurveilChoice { .. }),
        "an empty library must not offer a SurveilChoice prompt"
    );
    assert_eq!(
        surveil_count(&runner),
        1,
        "CR 701.25d: an instructed surveil against an empty library still completes"
    );
}

/// The same scenario with Dimir Spybug on the battlefield — its
/// "whenever you surveil" trigger must fire off the published event.
#[test]
fn surveil_against_empty_library_still_fires_whenever_you_surveil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spybug = scenario
        .add_creature_from_oracle(P0, "Dimir Spybug", 1, 1, DIMIR_SPYBUG)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Surveil Spell", true, "Surveil 1.")
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().players[0].library.is_empty());

    runner.cast(spell).resolve();

    assert_spell_resolved(&runner);
    assert_eq!(
        *runner.state().objects[&spybug]
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0),
        1,
        "CR 701.25d authorizes the trigger even though nothing was looked at"
    );
}

/// (Hostile, CR 701.22b): "Scry X, where X is the number of cards in your
/// graveyard." with an EMPTY graveyard never raises an instructed count above
/// 0 in the first place — no event, no ledger entry, no trigger. Paired
/// reach-guard in the same test: with graveyard fuel present, the same text
/// reaches `ScryChoice`, proving the negative isn't just an unparsed effect.
#[test]
fn scry_x_graveyard_count_of_zero_is_not_an_instructed_scry() {
    const SCRY_X_GRAVEYARD: &str = "Scry X, where X is the number of cards in your graveyard.";

    // Negative: empty graveyard, one-card library, Chance-Met Elves present.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elves = scenario
        .add_creature_from_oracle(P0, "Chance-Met Elves", 3, 2, CHANCE_MET_ELVES)
        .id();
    scenario.add_card_to_library_top(P0, "Library Card");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry X Spell", true, SCRY_X_GRAVEYARD)
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).resolve();

    assert_spell_resolved(&runner);
    assert_eq!(
        scry_count(&runner),
        0,
        "CR 701.22b: an instructed count of 0 is not a scry event at all"
    );
    assert_eq!(
        *runner.state().objects[&elves]
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0),
        0
    );

    // Reach-guard: with graveyard fuel, the same text reaches ScryChoice.
    let mut scenario2 = GameScenario::new();
    scenario2.at_phase(Phase::PreCombatMain);
    scenario2.add_creature_to_graveyard(P0, "Fuel A", 1, 1);
    scenario2.add_creature_to_graveyard(P0, "Fuel B", 1, 1);
    scenario2.add_card_to_library_top(P0, "Library Card 1");
    scenario2.add_card_to_library_top(P0, "Library Card 2");
    let spell2 = scenario2
        .add_spell_to_hand_from_oracle(P0, "Test Scry X Spell", true, SCRY_X_GRAVEYARD)
        .id();
    let mut runner2 = scenario2.build();
    runner2
        .act(GameAction::CastSpell {
            object_id: spell2,
            card_id: runner2.state().objects[&spell2].card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Scry X cast accepted");
    for _ in 0..10 {
        if matches!(runner2.state().waiting_for, WaitingFor::Priority { .. }) {
            if runner2.act(GameAction::PassPriority).is_err() {
                break;
            }
        } else {
            break;
        }
    }
    assert!(
        matches!(runner2.state().waiting_for, WaitingFor::ScryChoice { .. }),
        "reach-guard: with graveyard fuel present, Scry X must reach ScryChoice; got {:?}",
        runner2.state().waiting_for
    );
}

/// (Hostile, CR 701.25c): the surveil analogue of
/// `scry_x_graveyard_count_of_zero_is_not_an_instructed_scry` — "Surveil X, where X
/// is the number of cards in your graveyard." with an empty graveyard is not
/// an instructed surveil. Reach-guard: with graveyard fuel, the same text
/// reaches `SurveilChoice`.
#[test]
fn surveil_x_graveyard_count_of_zero_is_not_an_instructed_surveil() {
    const SURVEIL_X_GRAVEYARD: &str =
        "Surveil X, where X is the number of cards in your graveyard.";

    // Negative: empty graveyard, one-card library, Dimir Spybug present.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spybug = scenario
        .add_creature_from_oracle(P0, "Dimir Spybug", 1, 1, DIMIR_SPYBUG)
        .id();
    scenario.add_card_to_library_top(P0, "Library Card");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Surveil X Spell", true, SURVEIL_X_GRAVEYARD)
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).resolve();

    assert_spell_resolved(&runner);
    assert_eq!(
        surveil_count(&runner),
        0,
        "CR 701.25c: an instructed count of 0 is not a surveil event at all"
    );
    assert_eq!(
        *runner.state().objects[&spybug]
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0),
        0
    );

    // Reach-guard: with graveyard fuel, the same text reaches SurveilChoice.
    let mut scenario2 = GameScenario::new();
    scenario2.at_phase(Phase::PreCombatMain);
    scenario2.add_creature_to_graveyard(P0, "Fuel A", 1, 1);
    scenario2.add_creature_to_graveyard(P0, "Fuel B", 1, 1);
    scenario2.add_card_to_library_top(P0, "Library Card 1");
    scenario2.add_card_to_library_top(P0, "Library Card 2");
    let spell2 = scenario2
        .add_spell_to_hand_from_oracle(P0, "Test Surveil X Spell", true, SURVEIL_X_GRAVEYARD)
        .id();
    let mut runner2 = scenario2.build();
    runner2
        .act(GameAction::CastSpell {
            object_id: spell2,
            card_id: runner2.state().objects[&spell2].card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Surveil X cast accepted");
    for _ in 0..10 {
        if matches!(runner2.state().waiting_for, WaitingFor::Priority { .. }) {
            if runner2.act(GameAction::PassPriority).is_err() {
                break;
            }
        } else {
            break;
        }
    }
    assert!(
        matches!(
            runner2.state().waiting_for,
            WaitingFor::SurveilChoice { .. }
        ),
        "reach-guard: with graveyard fuel present, Surveil X must reach SurveilChoice; got {:?}",
        runner2.state().waiting_for
    );
}

/// (Multi-authority, replacement-choice path): two Kenessos-line static
/// abilities compete to apply to the same `Scry 1.` — a `ReplacementChoice`
/// prompt is offered (reach-guard: the test fails if it is not) even though
/// the library is empty. `engine_replacement.rs::handle_replacement_choice_
/// inner` is the only site that records the resulting event, since it runs
/// outside `resolve_chain_body`'s window.
#[test]
fn two_scry_plus_one_replacements_on_empty_library_via_replacement_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Scry Plus One A", 2, 2, KENESSOS_SCRY_STATIC);
    scenario.add_creature_from_oracle(P0, "Scry Plus One B", 2, 2, KENESSOS_SCRY_STATIC);
    let elves = scenario
        .add_creature_from_oracle(P0, "Chance-Met Elves", 3, 2, CHANCE_MET_ELVES)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry Spell", true, "Scry 1.")
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().players[0].library.is_empty());

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Scry 1 cast accepted");

    let mut answered_replacement_choice = false;
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                answered_replacement_choice = true;
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("replacement choice accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        answered_replacement_choice,
        "reach-guard: two competing scry-plus-one replacements must offer a ReplacementChoice"
    );

    assert_spell_resolved(&runner);
    assert_eq!(
        scry_count(&runner),
        1,
        "CR 701.22d: the replacement-choice path must record the scry exactly once"
    );
    assert_eq!(
        *runner.state().objects[&elves]
            .counters
            .get(&CounterType::Plus1Plus1)
            .unwrap_or(&0),
        1
    );
}

/// (Single replacement, chain path): one Kenessos-line static applied to an
/// empty-library `Scry 1.` — a single applicable replacement auto-applies
/// with no `ReplacementChoice` prompt, so the event stays inside
/// `resolve_chain_body`'s own window and is recorded there.
#[test]
fn one_scry_plus_one_replacement_on_empty_library_records_in_chain_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Scry Plus One", 2, 2, KENESSOS_SCRY_STATIC);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry Spell", true, "Scry 1.")
        .id();
    let mut runner = scenario.build();
    assert!(runner.state().players[0].library.is_empty());

    runner.cast(spell).resolve();

    assert_spell_resolved(&runner);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "a single applicable replacement must not prompt"
    );
    assert_eq!(
        scry_count(&runner),
        1,
        "a single-replacement empty-library scry must still be recorded exactly once"
    );
}
