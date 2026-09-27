//! Issues #6373 / #1486 / #1533 — Obeka, Splitter of Seconds.
//!
//! Oracle (Obeka, Splitter of Seconds):
//! > Menace
//! > Whenever Obeka deals combat damage to a player, you get that many
//! > additional upkeep steps after this phase.
//!
//! Rulings: each additional upkeep step gets its own created beginning phase
//! with the untap and draw steps skipped, all of them come after the combat
//! phase ends, "at the beginning of your upkeep" abilities trigger in each one,
//! and the turn then continues to the postcombat main phase.
//!
//! CR 500.10: a step added after a phase first creates the phase that normally
//! holds it. CR 500.11: its other steps are skipped. CR 510.2: combat damage is
//! dealt. CR 503.1a: upkeep triggers go on the stack before the active player
//! gets priority. CR 511.3: after end of combat, the postcombat main phase
//! begins.
//!
//! These tests drive an unblocked Obeka into P1 and pass priority through the
//! rest of the turn, recording every step entered, with an upkeep life-gain
//! trigger on the battlefield as the witness that each created upkeep ran.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use super::rules::run_combat;

const OBEKA_ORACLE: &str = "Menace\nWhenever Obeka deals combat damage to a player, \
you get that many additional upkeep steps after this phase.";

/// What the turn looked like from Obeka's trigger resolving to the start of the
/// postcombat main phase.
struct AfterCombat {
    /// The step in progress when the trigger finished resolving, then every
    /// step entered afterwards, ending with `PostCombatMain`.
    steps: Vec<Phase>,
    life_gained: i32,
    cards_drawn: i64,
    obeka_tapped: bool,
    runner: GameRunner,
}

fn attack_with_obeka(power: i32) -> AfterCombat {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let obeka = scenario
        .add_creature_from_oracle(P0, "Obeka, Splitter of Seconds", power, 5, OBEKA_ORACLE)
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Upkeep Gainer",
        1,
        1,
        "At the beginning of your upkeep, you gain 1 life.",
    );
    scenario.add_card_to_library_top(P0, "Island");
    scenario.add_card_to_library_top(P0, "Island");
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![obeka], vec![]);
    runner.advance_until_stack_empty();

    let life_before = runner.life(P0);
    let hand_before = runner.state().players[0].hand.len() as i64;
    let turn = runner.state().turn_number;
    let mut steps = vec![runner.state().phase];
    for _ in 0..40 {
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority should succeed");
        steps.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
        assert_eq!(
            runner.state().turn_number,
            turn,
            "the turn must not end before the postcombat main phase: {steps:?}"
        );
        if steps.last() == Some(&Phase::PostCombatMain) {
            let state = runner.state();
            return AfterCombat {
                life_gained: runner.life(P0) - life_before,
                cards_drawn: state.players[0].hand.len() as i64 - hand_before,
                obeka_tapped: object_tapped(&runner, obeka),
                steps,
                runner,
            };
        }
    }
    panic!("did not reach the postcombat main phase within 40 passes: {steps:?}");
}

fn object_tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

/// The steps from the end of combat onwards.
fn from_end_of_combat(steps: &[Phase]) -> &[Phase] {
    let start = steps
        .iter()
        .position(|phase| *phase == Phase::EndCombat)
        .unwrap_or_else(|| panic!("end of combat never ran: {steps:?}"));
    &steps[start..]
}

/// CR 500.10 + CR 500.11 + CR 503.1a: two combat damage from Obeka gives two
/// created upkeep steps after combat, each triggering the upkeep ability, with
/// untap and draw skipped, and then the postcombat main phase.
#[test]
fn obeka_two_damage_runs_two_upkeeps_after_combat_then_postcombat_main() {
    let after = attack_with_obeka(2);

    assert_eq!(
        from_end_of_combat(&after.steps),
        [
            Phase::EndCombat,
            Phase::Upkeep,
            Phase::Upkeep,
            Phase::PostCombatMain
        ],
        "full sequence: {:?}",
        after.steps
    );
    // One upkeep trigger per created upkeep. Also the reach guard for the
    // skipped-step negatives below: the created phases did run.
    assert_eq!(after.life_gained, 2);
    assert_eq!(
        after.cards_drawn, 0,
        "CR 500.11: the draw steps are skipped"
    );
    assert!(after.obeka_tapped, "CR 500.11: the untap steps are skipped");
    let state = after.runner.state();
    assert!(state.extra_phases.is_empty(), "{:?}", state.extra_phases);
    assert!(
        state.extra_phase_resume.is_empty(),
        "{:?}",
        state.extra_phase_resume
    );
}

/// CR 500.10 + CR 510.2: one combat damage gives exactly one created upkeep.
#[test]
fn obeka_one_damage_runs_one_upkeep_after_combat_then_postcombat_main() {
    let after = attack_with_obeka(1);

    assert_eq!(
        from_end_of_combat(&after.steps),
        [Phase::EndCombat, Phase::Upkeep, Phase::PostCombatMain],
        "full sequence: {:?}",
        after.steps
    );
    assert_eq!(after.life_gained, 1);
    assert_eq!(after.cards_drawn, 0);
    assert!(after.obeka_tapped);
}
