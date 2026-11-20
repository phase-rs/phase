//! Issue #3672: Midnight Reaper — "Whenever a nontoken creature you control dies, this
//! creature deals 1 damage to you and you draw a card." Sacrificing a creature puts it into
//! its owner's graveyard (CR 701.21a), so it dies (CR 700.4). The Reaper sees its own death
//! through last-known information (CR 603.10a), including when it is the creature
//! sacrificed, whether as an activation cost or as a spell's additional cost.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
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

/// Activate Viscera Seer, assert the sacrifice prompt offers `paid`, and pay with it. Then
/// assert the boundary right after payment: P0 has priority, with Midnight Reaper's trigger
/// above the Seer's ability.
fn sacrifice_to_seer(runner: &mut GameRunner, seer: ObjectId, reaper: ObjectId, paid: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: seer,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Viscera Seer's ability can be activated");
    let WaitingFor::PayCost {
        player, choices, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the sacrifice cost asks for a creature, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        choices.contains(&paid),
        "{paid:?} can be sacrificed; choices were {choices:?}"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![paid] })
        .expect("sacrificing the chosen creature pays the cost");
    assert_trigger_above(runner, reaper, seer);
}

/// P0 has priority, and the stack holds exactly the Reaper's trigger on top of the entry
/// from `below_source`.
fn assert_trigger_above(runner: &GameRunner, reaper: ObjectId, below_source: ObjectId) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "P0 has priority after paying, got {:?}",
        runner.state().waiting_for
    );
    let sources: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .map(|entry| entry.source_id)
        .collect();
    assert_eq!(
        sources,
        vec![below_source, reaper],
        "Midnight Reaper's trigger goes on the stack above it (bottom to top)"
    );
}

/// Resolve the stack, keeping the scried card on top when Viscera Seer's ability asks
/// (CR 701.22a).
fn resolve_all(runner: &mut GameRunner) {
    for _ in 0..8 {
        runner.advance_until_stack_empty();
        let WaitingFor::ScryChoice { cards, .. } = runner.state().waiting_for.clone() else {
            return;
        };
        runner
            .act(GameAction::SelectCards { cards })
            .expect("keeping the scried card on top is legal");
    }
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

    sacrifice_to_seer(&mut runner, seer, reaper, reaper);
    resolve_all(&mut runner);

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

    sacrifice_to_seer(&mut runner, seer, reaper, bears);
    resolve_all(&mut runner);

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
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let life = runner.life(P0);
    let hand = hand_size(&runner);

    runner.cast(rites).sacrifice_with(&[reaper]).commit();
    assert_trigger_above(&runner, reaper, rites);
    resolve_all(&mut runner);

    assert_eq!(runner.state().objects[&reaper].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), life - 1, "the Reaper deals 1 damage to P0");
    // Village Rites leaves the hand (-1), draws two (+2), and the Reaper draws one (+1).
    assert_eq!(hand_size(&runner), hand + 2);
    assert_settled(&runner);
}
