//! Issue #3672: Midnight Reaper — "Whenever a nontoken creature you control dies, this
//! creature deals 1 damage to you and you draw a card." Sacrificing a creature puts it into
//! its owner's graveyard (CR 701.21a), so it dies (CR 700.4). The Reaper sees its own death
//! through last-known information (CR 603.10a), including when it is the creature
//! sacrificed, whether as an activation cost or as a spell's additional cost.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MIDNIGHT_REAPER: &str = "Whenever a nontoken creature you control dies, this creature deals 1 damage to you and you draw a card.";
const VISCERA_SEER: &str =
    "Sacrifice a creature: Scry 1. (Look at the top card of your library. You may put that card on the bottom.)";
const VILLAGE_RITES: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";

fn hand_size(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.owner == P0 && obj.zone == Zone::Hand)
        .count()
}

fn assert_settled(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "everything has resolved and P0 has priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "the stack is empty");
}

/// Board: P0 controls Midnight Reaper, Viscera Seer and a Grizzly Bears, with cards to draw.
fn board() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reaper = scenario
        .add_creature_from_oracle(P0, "Midnight Reaper", 3, 2, MIDNIGHT_REAPER)
        .id();
    let seer = scenario
        .add_creature_from_oracle(P0, "Viscera Seer", 1, 1, VISCERA_SEER)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_library_top(P0, &["Card"; 6]);
    (scenario, reaper, seer, bears)
}

/// The reported case: Midnight Reaper sacrificed to Viscera Seer's cost sees its own death.
#[test]
fn reaper_sacrificed_as_an_activation_cost_triggers_itself() {
    let (scenario, reaper, seer, _) = board();
    let mut runner = scenario.build();
    let life = runner.life(P0);
    let hand = hand_size(&runner);

    runner.activate(seer, 0).pay_with(&[reaper]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&reaper].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), life - 1, "the Reaper deals 1 damage to P0");
    assert_eq!(hand_size(&runner), hand + 1, "and P0 draws a card");
    assert_settled(&runner);
}

/// The Reaper also triggers for another nontoken creature sacrificed the same way.
#[test]
fn another_creature_sacrificed_as_a_cost_triggers_the_reaper() {
    let (scenario, reaper, seer, bears) = board();
    let mut runner = scenario.build();
    let life = runner.life(P0);
    let hand = hand_size(&runner);

    runner.activate(seer, 0).pay_with(&[bears]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&reaper].zone, Zone::Battlefield);
    assert_eq!(runner.life(P0), life - 1);
    assert_eq!(hand_size(&runner), hand + 1);
    assert_settled(&runner);
}

/// Sacrificed as Village Rites' additional cost (CR 601.2h): the Reaper triggers, and the
/// spell's own two draws still happen.
#[test]
fn reaper_sacrificed_as_a_spells_additional_cost_triggers_itself() {
    let (mut scenario, reaper, _, _) = board();
    let rites = scenario
        .add_spell_to_hand_from_oracle(P0, "Village Rites", true, VILLAGE_RITES)
        .id();
    let mut runner = scenario.build();
    let life = runner.life(P0);
    let hand = hand_size(&runner);

    runner.cast(rites).sacrifice_with(&[reaper]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&reaper].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), life - 1, "the Reaper deals 1 damage to P0");
    // Village Rites leaves the hand (-1), draws two (+2), and the Reaper draws one (+1).
    assert_eq!(hand_size(&runner), hand + 2);
    assert_settled(&runner);
}
