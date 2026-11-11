//! Runtime witness for Flamewar, Brash Veteran's intel-counter return, which
//! this PR's owner/counter grammar newly supports. Printed Oracle text is
//! quoted from the card data.
//!
//! Infrastructure exception: intel counters are placed on the exiled cards
//! directly, isolating the activation and its resolution under test.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FLAMEWAR: &str = "More Than Meets the Eye {B}{R} (You may cast this card converted for {B}{R}.)\nSacrifice another artifact: Put a +1/+1 counter on Flamewar and convert it. Activate only as a sorcery.\n{1}, Discard your hand: Put all exiled cards you own with intel counters on them into your hand.";

fn intel() -> CounterType {
    CounterType::Generic("intel".to_string())
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// CR 602.2b + CR 701.9a + CR 108.3 + CR 122.1: "{1}, Discard your hand: Put
/// all exiled cards you own with intel counters on them into your hand." The
/// whole hand is discarded as the cost, then exactly the exiled cards P0 owns
/// that carry an intel counter return to P0's hand: not P0's exiled card
/// without one, and not P1's exiled card with one.
#[test]
fn flamewar_returns_only_own_exiled_cards_with_intel_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flamewar = scenario
        .add_creature_from_oracle(P0, "Flamewar, Brash Veteran", 3, 2, FLAMEWAR)
        .as_legendary()
        .with_subtypes(vec!["Robot"])
        .id();
    scenario.add_basic_land(P0, ManaColor::Red);
    let discard_a = scenario.add_card_to_hand(P0, "Spare Card A");
    let discard_b = scenario.add_card_to_hand(P0, "Spare Card B");
    let intel_bear = scenario.add_creature_to_exile(P0, "Exiled Bear", 2, 2).id();
    let intel_spell = scenario.add_spell_to_exile(P0, "Exiled Shock", true).id();
    let plain = scenario
        .add_creature_to_exile(P0, "Exiled Without Intel", 2, 2)
        .id();
    let theirs = scenario
        .add_creature_to_exile(P1, "Opponent's Intel Card", 2, 2)
        .id();
    for id in [intel_bear, intel_spell, theirs] {
        scenario.with_counter(id, intel(), 1);
    }
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&flamewar]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::ChangeZoneAll { .. }))
        .expect("the intel return parses as a mass move");
    runner
        .act(GameAction::ActivateAbility {
            source_id: flamewar,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Flamewar's intel ability is activated");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                player: P0,
                kind: PayCostKind::Discard,
                count: 2,
                ..
            }
        ),
        "the cost discards the whole hand: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![discard_a, discard_b],
        })
        .expect("the hand is discarded");
    runner.advance_until_stack_empty();

    assert_eq!(zone_of(&runner, discard_a), Zone::Graveyard);
    assert_eq!(zone_of(&runner, discard_b), Zone::Graveyard);
    let mut hand: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .collect();
    hand.sort();
    let mut expected = vec![intel_bear, intel_spell];
    expected.sort();
    assert_eq!(
        hand, expected,
        "exactly the exiled cards P0 owns with intel counters"
    );
    assert_eq!(zone_of(&runner, plain), Zone::Exile, "no intel counter");
    assert_eq!(zone_of(&runner, theirs), Zone::Exile, "owned by P1");
    assert!(runner.state().players[P1.0 as usize].hand.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
