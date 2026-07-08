//! Issue #4252 — Court of Cunning: "any number of target players each mill two
//! cards. If you're the monarch, each of those players mills ten cards
//! instead." The reported symptom was that only the FIRST selected target
//! actually milled. This is the dedicated regression deliverable the issue
//! asks for: it drives the REAL upkeep-trigger pipeline (multi-target player
//! selection → fan-out → mill), asserting every targeted player mills, and
//! covering the monarch "mills ten instead" branch.
//!
//! CR 603.3d + CR 601.2c: the upkeep trigger's controller chooses any number
//! of target players as the ability goes on the stack.
//! CR 101.4 + CR 608.2c: the effect resolves once per chosen player.
//! CR 614.1 + CR 608.2c: the "mills ten instead" rider replaces the mill
//! during resolution. CR 725: monarch designation gates that rider.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::PlayerId;

/// Court of Cunning's printed Oracle text — byte-identical to
/// `client/public/card-data.json` and MTGJSON `AtomicCards.json`.
const COURT_OF_CUNNING: &str = "When Court of Cunning enters, you become the monarch.\n\
     At the beginning of your upkeep, any number of target players each mill two cards. \
     If you're the monarch, each of those players mills ten cards instead.";

fn graveyard_count(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.graveyard.len())
        .expect("player exists")
}

/// Build a 2-player game with Court of Cunning already on P0's battlefield and
/// a 15-card library for every player. `monarch` seeds the monarch designation
/// directly (the ETB "you become the monarch" trigger does not fire for a
/// scenario-seeded permanent).
fn build_runner(monarch: Option<PlayerId>) -> engine::game::scenario::GameRunner {
    let library: Vec<&str> = (0..15).map(|_| "Library Card").collect();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &library);
    }
    scenario
        .add_creature(P0, "Court of Cunning", 0, 0)
        .as_enchantment()
        .from_oracle_text(COURT_OF_CUNNING);

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().monarch = monarch;
    runner
}

/// Drive to P0's upkeep so the trigger fires, then select both players as
/// targets. Returns once the stack has emptied and targets were selected.
fn fire_and_target_both(runner: &mut engine::game::scenario::GameRunner) {
    runner.advance_to_upkeep();

    let mut targets_selected = false;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 50, "stuck at {:?}", runner.state().waiting_for);
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
                    })
                    .expect("selecting two player targets must succeed");
                targets_selected = true;
                runner.advance_until_stack_empty();
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && targets_selected => {
                return;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority at upkeep");
            }
            _ if targets_selected && runner.state().stack.is_empty() => return,
            other => panic!(
                "unexpected state while driving Court of Cunning upkeep (selected={targets_selected}): {other:?}"
            ),
        }
    }
}

/// CR 101.4: With no monarch, each of the two targeted players mills two cards.
/// Before the fix, only the first selected target milled.
#[test]
fn court_of_cunning_non_monarch_mills_two_for_each_targeted_player() {
    let mut runner = build_runner(None);
    fire_and_target_both(&mut runner);

    assert_eq!(
        graveyard_count(&runner, P0),
        2,
        "P0 (targeted) must mill two cards"
    );
    assert_eq!(
        graveyard_count(&runner, P1),
        2,
        "P1 (targeted) must mill two cards — not just the first target"
    );
}

/// CR 614.1 + CR 725: When the controller is the monarch, every targeted player
/// mills ten cards instead — and the "instead" replacement applies to each
/// fanned-out player, not only the first.
#[test]
fn court_of_cunning_monarch_mills_ten_for_each_targeted_player() {
    let mut runner = build_runner(Some(P0));
    fire_and_target_both(&mut runner);

    assert_eq!(
        graveyard_count(&runner, P0),
        10,
        "P0 (targeted, controller is monarch) must mill ten cards"
    );
    assert_eq!(
        graveyard_count(&runner, P1),
        10,
        "P1 (targeted) must also mill ten cards — not just the first target"
    );
}
