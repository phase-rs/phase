//! The "discard X cards" additional-cost class beyond the two headline cards
//! (Restless Dreams, Firestorm):
//!
//! - X = 0 on an additional cost: nothing is discarded, no target is chosen
//!   (CR 107.3a — 0 is a legal announcement; CR 601.2c).
//! - A TYPED count (Scorched Earth, "discard X land cards"): the announced
//!   maximum counts only cards that can pay (CR 601.2b), and only they pay.
//! - An ACTIVATED ability (Gix, Yawgmoth Praetor, "{4}{B}{B}{B}, Discard X
//!   cards:"): X is announced for the activation cost (CR 602.2b → 601.2b)
//!   and exactly X cards are discarded (CR 601.2h).
//! - A FLASHBACK cost (Conflagrate, "Flashback—{R}{R}, Discard X cards"):
//!   X is announced from the flashback cost's non-mana part (CR 702.34a +
//!   CR 601.2b) before the X-sized division of damage (CR 601.2d).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastingVariant, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

#[test]
fn restless_dreams_x_zero_discards_nothing_and_returns_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Restless Dreams",
            false,
            "As an additional cost to cast this spell, discard X cards.\n\
             Return X target creature cards from your graveyard to your hand.",
        );
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        });
        b.id()
    };
    let dead = scenario
        .add_creature_to_graveyard(P0, "Dead Bear", 2, 2)
        .id();
    let spare = scenario.add_card_to_hand(P0, "Spare Card");
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Black, 1);

    let outcome = runner.cast(spell).x(0).resolve();

    outcome.assert_zone(&[spare], Zone::Hand);
    outcome.assert_zone(&[dead], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
}

#[test]
fn scorched_earth_typed_discard_x_counts_and_pays_with_land_cards_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Scorched Earth",
            false,
            "As an additional cost to cast this spell, discard X land cards.\n\
             Destroy X target lands.",
        );
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        });
        b.id()
    };
    let hand_lands: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_land_to_hand(P0, &format!("Spare Land {i}"))
                .id()
        })
        .collect();
    let nonland = scenario.add_card_to_hand(P0, "Spare Spell");
    let their_lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P1, ManaColor::Green))
        .collect();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 4);

    // CR 601.2b: the announced maximum is the number of LAND cards in hand (2),
    // not the hand size (3).
    let card_id = runner.state().objects[&spell].card_id;
    let waiting = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: Default::default(),
        })
        .expect("Scorched Earth must be castable")
        .waiting_for;
    match waiting {
        WaitingFor::ChooseXValue { min, max, .. } => {
            assert_eq!((min, max), (0, 2), "X is capped by land cards in hand");
        }
        other => panic!("expected ChooseXValue, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("X = 2 must be accepted");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(their_lands[0]),
                TargetRef::Object(their_lands[1]),
            ],
        })
        .expect("two lands must be accepted as targets");
    // Only the two land cards may pay; a nonland card is not a choice.
    match &runner.state().waiting_for {
        WaitingFor::PayCost { choices, count, .. } => {
            assert_eq!(*count, 2);
            assert!(choices.contains(&hand_lands[0]) && choices.contains(&hand_lands[1]));
            assert!(!choices.contains(&nonland), "a nonland card cannot pay");
        }
        other => panic!("expected the discard PayCost prompt, got {other:?}"),
    }
}

#[test]
fn gix_activated_discard_x_announces_x_and_discards_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gix = scenario
        .add_creature_from_oracle(
            P0,
            "Gix, Yawgmoth Praetor",
            3,
            3,
            "{4}{B}{B}{B}, Discard X cards: Exile the top X cards of target opponent's library.",
        )
        .id();
    let hand: Vec<ObjectId> = (0..3)
        .map(|i| scenario.add_card_to_hand(P0, &format!("Spare Card {i}")))
        .collect();
    let library: Vec<ObjectId> = (0..4)
        .map(|i| scenario.add_card_to_library_top(P1, &format!("Their Card {i}")))
        .collect();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Black, 3);
    add_mana(&mut runner, ManaType::Colorless, 4);

    let outcome = runner
        .activate(gix, 0)
        .x(2)
        .pay_with(&[hand[0], hand[1]])
        .resolve();

    // CR 601.2h: exactly the announced X = 2 cards were discarded.
    outcome.assert_zone(&[hand[0], hand[1]], Zone::Graveyard);
    outcome.assert_zone(&[hand[2]], Zone::Hand);
    // The same X sizes the effect: the top two of four are exiled
    // (`add_card_to_library_top` puts each new card on top).
    outcome.assert_zone(&[library[3], library[2]], Zone::Exile);
    outcome.assert_zone(&[library[1], library[0]], Zone::Library);
}

#[test]
fn conflagrate_flashback_discard_x_announces_x_and_divides_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_spell_to_graveyard(P0, "Conflagrate", false);
        b.from_oracle_text(
            "Conflagrate deals X damage divided as you choose among any number of targets.\n\
             Flashback—{R}{R}, Discard X cards. (You may cast this card from your graveyard \
             for its flashback cost. Then exile it.)",
        );
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        });
        b.id()
    };
    let victim = scenario.add_creature(P1, "Target Ogre", 3, 3).id();
    let hand: Vec<ObjectId> = (0..3)
        .map(|i| scenario.add_card_to_hand(P0, &format!("Spare Card {i}")))
        .collect();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 2);

    let outcome = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .x(3)
        .target_object(victim)
        .distribute_among(&[(TargetRef::Object(victim), 3)])
        .pay_cost_with(&hand)
        .resolve();

    // CR 601.2h: the X = 3 announced cards were discarded once, and the
    // flashback mana ({R}{R}) was paid — not the printed {X}{X}{R}.
    outcome.assert_zone(&hand, Zone::Graveyard);
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    // CR 601.2d + CR 120.3: all 3 damage on the 3/3.
    outcome.assert_zone(&[victim], Zone::Graveyard);
    // CR 702.34a: a flashback spell is exiled as it leaves the stack.
    outcome.assert_zone(&[spell], Zone::Exile);
}
