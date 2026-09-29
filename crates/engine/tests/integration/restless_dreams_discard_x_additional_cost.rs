//! Restless Dreams {B}: "As an additional cost to cast this spell, discard X
//! cards. Return X target creature cards from your graveyard to your hand."
//!
//! The card has no {X} in its mana cost: X is announced for the variable
//! discard cost (CR 107.3a + CR 601.2b), the same X sizes the target set
//! (CR 601.2c), and X cards are discarded while paying (CR 601.2h + CR 701.9a).
//! The parser used to read "discard x cards" as a FIXED 0 — the spell was
//! never offered as castable, and even if it were, it would discard nothing.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "As an additional cost to cast this spell, discard X cards.\n\
                      Return X target creature cards from your graveyard to your hand.";

fn add_black(runner: &mut engine::game::scenario::GameRunner, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

struct Board {
    runner: engine::game::scenario::GameRunner,
    spell: ObjectId,
    graveyard_creatures: Vec<ObjectId>,
    hand_cards: Vec<ObjectId>,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Restless Dreams", false, ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        });
        b.id()
    };
    let graveyard_creatures = (0..3)
        .map(|i| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Dead Bear {i}"), 2, 2)
                .id()
        })
        .collect();
    let hand_cards = (0..3)
        .map(|i| scenario.add_card_to_hand(P0, &format!("Spare Card {i}")))
        .collect();
    let mut runner = scenario.build();
    add_black(&mut runner, 1);
    Board {
        runner,
        spell,
        graveyard_creatures,
        hand_cards,
    }
}

#[test]
fn discard_x_additional_cost_parses_as_variable_x_not_zero() {
    let b = board();
    let cost = b.runner.state().objects[&b.spell].additional_cost.clone();
    match cost {
        Some(AdditionalCost::Required(AbilityCost::Discard { count, .. })) => assert!(
            count.contains_x(),
            "CR 107.3a: \"discard X cards\" must keep X variable, got {count:?}"
        ),
        other => panic!("expected a required discard cost, got {other:?}"),
    }
}

#[test]
fn restless_dreams_is_offered_as_castable() {
    let b = board();
    let actions = legal_actions(b.runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == b.spell
        )),
        "Restless Dreams with {{B}} available and creatures in the graveyard must be castable; \
         legal actions: {actions:?}"
    );
}

#[test]
fn restless_dreams_discards_x_and_returns_x_creatures() {
    let mut b = board();
    let returned = [b.graveyard_creatures[0], b.graveyard_creatures[2]];
    let discarded = [b.hand_cards[0], b.hand_cards[1]];
    let outcome = b
        .runner
        .cast(b.spell)
        .x(2)
        .target_objects(&returned)
        .pay_cost_with(&discarded)
        .resolve();

    // CR 701.9a: exactly X = 2 chosen cards were discarded as the cost.
    outcome.assert_zone(&discarded, Zone::Graveyard);
    outcome.assert_zone(&[b.hand_cards[2]], Zone::Hand);
    // CR 400.7: the X = 2 targeted creature cards returned to hand; the third stays.
    outcome.assert_zone(&returned, Zone::Hand);
    outcome.assert_zone(&[b.graveyard_creatures[1]], Zone::Graveyard);
    outcome.assert_zone(&[b.spell], Zone::Graveyard);
}
