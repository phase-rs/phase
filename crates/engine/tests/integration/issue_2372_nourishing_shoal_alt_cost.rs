//! Regression for issue #2372: Nourishing Shoal must offer and execute its
//! pitch alternative cost (exile a green card with mana value X), binding X to
//! the exiled card's mana value for the life-gain effect.
//!
//! https://github.com/phase-rs/phase/issues/2372

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::oracle_cost::parse_oracle_cost;
use engine::types::ability::{
    AdditionalCost, Effect, QuantityExpr, SpellCastingOption, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Nourishing Shoal with a green mana value 3 card in P0's hand; Thalia, Guardian of Thraben
/// and one floating {C} when `with_thalia`.
fn setup_shoal_scenario(
    with_thalia: bool,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    CardId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let alt_cost = parse_oracle_cost("exile a green card with mana value X from your hand");

    let shoal = scenario
        .add_creature_to_hand(P0, "Nourishing Shoal", 0, 0)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        })
        .with_ability(Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: engine::types::ability::QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            player: TargetFilter::Controller,
        })
        .id();

    let green_filler = scenario.add_creature_to_hand(P0, "Green Filler", 2, 2).id();

    if with_thalia {
        let db = crate::support::shared_card_db().expect("the integration card fixture loads");
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

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        let shoal_obj = s.objects.get_mut(&shoal).unwrap();
        shoal_obj
            .casting_options
            .push(SpellCastingOption::alternative_cost(alt_cost));
        shoal_obj.color.push(ManaColor::Green);

        let green = s.objects.get_mut(&green_filler).unwrap();
        green.card_types.core_types.push(CoreType::Creature);
        green.color.push(ManaColor::Green);
        green.mana_cost = ManaCost::generic(3);
    }

    let card_id = runner.state().objects[&shoal].card_id;
    (runner, shoal, card_id, green_filler)
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
                cost: AdditionalCost::Choice(_, _),
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
    let (mut runner, shoal, card_id, green_filler) = setup_shoal_scenario(false);
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
        .act(GameAction::DecideOptionalCost { pay: true })
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
    assert!(choices.contains(&green_filler));

    runner
        .act(GameAction::SelectCards {
            cards: vec![green_filler],
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
        runner.state().objects[&green_filler].zone,
        Zone::Exile,
        "pitched card must be exiled"
    );
}

/// CR 107.3a + CR 118.9d: under Thalia, Guardian of Thraben the pitch cast also pays {1}, and X
/// is still the exiled card's mana value.
#[test]
fn nourishing_shoal_pitch_under_a_cost_increase_binds_x_and_pays_it() {
    let (mut runner, shoal, card_id, green_filler) = setup_shoal_scenario(true);
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
            cost: AdditionalCost::Choice(_, _),
            ..
        }
    ));
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept pitch cost");
    runner
        .act(GameAction::SelectCards {
            cards: vec![green_filler],
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
    assert_eq!(runner.state().objects[&green_filler].zone, Zone::Exile);
}
