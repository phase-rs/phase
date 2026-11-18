//! Regression for issue #2372: Nourishing Shoal must offer and execute its
//! pitch alternative cost (exile a green card with mana value X), binding X to
//! the exiled card's mana value for the life-gain effect.
//!
//! https://github.com/phase-rs/phase/issues/2372

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AdditionalCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Nourishing Shoal and Centaur Courser (green, mana value 3) in P0's hand; Thalia, Guardian of
/// Thraben and one floating {C} when `with_thalia`.
fn setup_shoal_scenario(
    with_thalia: bool,
) -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    CardId,
    ObjectId,
) {
    let db = crate::support::shared_card_db().expect("the integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let shoal = scenario.add_real_card(P0, "Nourishing Shoal", Zone::Hand, db);
    let courser = scenario.add_real_card(P0, "Centaur Courser", Zone::Hand, db);

    if with_thalia {
        scenario.add_real_card(P1, "Thalia, Guardian of Thraben", Zone::Battlefield, db);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            )],
        );
    }

    let runner = scenario.build();
    let card_id = runner.state().objects[&shoal].card_id;
    (runner, shoal, card_id, courser)
}

#[test]
fn nourishing_shoal_cast_surfaces_pitch_alternative_choice() {
    let (mut runner, shoal, card_id, _) = setup_shoal_scenario(false);
    let life_before = runner.life(P0);

    let result = runner
        .act(GameAction::CastSpell {
            object_id: shoal,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Nourishing Shoal");

    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::OptionalCostChoice {
                cost: AdditionalCost::Choice(_),
                ..
            }
        ),
        "expected OptionalCostChoice for pitch alt cost, got {:?}",
        result.waiting_for
    );
    assert_eq!(life_before, runner.life(P0));
}

#[test]
fn nourishing_shoal_pitch_exile_binds_x_and_gains_life() {
    let (mut runner, shoal, card_id, courser) = setup_shoal_scenario(false);
    let life_before = runner.life(P0);

    runner
        .act(GameAction::CastSpell {
            object_id: shoal,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");

    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept pitch cost");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: engine::types::game_state::PayCostKind::ExileFromZone {
                    zone: engine::types::zones::ExileCostSourceZone::Hand,
                },
                ..
            }
        ),
        "expected exile-from-hand payment, got {:?}",
        runner.state().waiting_for
    );

    let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
        unreachable!();
    };
    assert!(choices.contains(&courser));

    runner
        .act(GameAction::SelectCards {
            cards: vec![courser],
        })
        .expect("exile green card for pitch");

    assert!(
        !runner.state().stack.is_empty(),
        "Nourishing Shoal must reach the stack after paying the pitch cost"
    );
    if let StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &runner.state().stack.last().unwrap().kind
    {
        assert_eq!(
            ability.chosen_x,
            Some(3),
            "X must be bound from the exiled card's mana value before resolution"
        );
    } else {
        panic!("expected spell on stack after pitch payment");
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 3,
        "X must equal the exiled card's mana value (3)"
    );
    assert_eq!(
        runner.state().objects[&courser].zone,
        Zone::Exile,
        "pitched card must be exiled"
    );
}

/// CR 107.3c + CR 118.9d: under Thalia, Guardian of Thraben the pitch cast also pays {1}, and X
/// is still the exiled card's mana value.
#[test]
fn nourishing_shoal_pitch_under_a_cost_increase_binds_x_and_pays_it() {
    let (mut runner, shoal, card_id, courser) = setup_shoal_scenario(true);
    let life_before = runner.life(P0);

    runner
        .act(GameAction::CastSpell {
            object_id: shoal,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalCostChoice {
            cost: AdditionalCost::Choice(_),
            ..
        }
    ));
    runner
        .act(GameAction::DecideOptionalCost { index: 0 })
        .expect("accept pitch cost");
    runner
        .act(GameAction::SelectCards {
            cards: vec![courser],
        })
        .expect("exile green card for pitch");

    let Some(StackEntryKind::Spell {
        ability: Some(ability),
        ..
    }) = runner.state().stack.last().map(|entry| &entry.kind)
    else {
        panic!("expected Nourishing Shoal on the stack");
    };
    assert_eq!(ability.chosen_x, Some(3));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), life_before + 3);
    assert_eq!(runner.state().objects[&courser].zone, Zone::Exile);
}
