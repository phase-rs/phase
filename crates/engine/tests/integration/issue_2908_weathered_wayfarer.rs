//! Issue #2908: Weathered Wayfarer activation restriction must gate on existential
//! opponent land counts. Regression for card-database hydration (production path)
//! and the related "at least N more [type] than you" variant (Isolated Watchtower).
//!
//! Note: the parsed AST's top-level `condition` field is intentionally `None` —
//! activation gates live in `activation_restrictions` as
//! `RequiresCondition { condition: Some(...) }` per CR 602.5b.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::restrictions::check_activation_restrictions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    ActivationRestriction, Comparator, ParsedCondition, PlayerFilter, PlayerRelation, QuantityExpr,
    QuantityRef,
};
use engine::types::actions::GameAction;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const P2: PlayerId = PlayerId(2);

fn assert_opponent_controls_more_lands_restriction(
    restrictions: &[ActivationRestriction],
) -> usize {
    let idx = restrictions
        .iter()
        .position(|r| {
            matches!(
                r,
                ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::QuantityComparison {
                        lhs: QuantityExpr::Ref {
                            qty: QuantityRef::PlayerCount {
                                filter: PlayerFilter::ControlsCount {
                                    relation: PlayerRelation::Opponent,
                                    comparator: Comparator::GT,
                                    ..
                                },
                            },
                        },
                        comparator: Comparator::GE,
                        rhs: QuantityExpr::Fixed { value: 1 },
                    })
                }
            )
        })
        .expect("Weathered Wayfarer must carry existential opponent land ControlsCount GT gate");
    idx
}

#[test]
fn card_database_weathered_wayfarer_has_activation_restriction_not_resolution_condition() {
    let Some(db) = load_db() else {
        return;
    };

    let face = db
        .get_face_by_name("Weathered Wayfarer")
        .expect("Weathered Wayfarer must exist in card database");
    assert_eq!(face.abilities.len(), 1);
    let ability = &face.abilities[0];
    assert!(
        ability.condition.is_none(),
        "activation gate must not be stored on resolution `condition`; \
         check activation_restrictions instead"
    );
    assert_opponent_controls_more_lands_restriction(&ability.activation_restrictions);
}

#[test]
fn card_database_weathered_wayfarer_blocked_when_land_counts_tied() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wayfarer = scenario.add_real_card(P0, "Weathered Wayfarer", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Plains", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Island", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert!(
        !can_activate_ability_now(runner.state(), P0, wayfarer, 0),
        "card-database Weathered Wayfarer must not activate when land counts are tied"
    );

    let actions = legal_actions(runner.state());
    assert!(
        !actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility { source_id, .. } if *source_id == wayfarer
        )),
        "legal_actions must not offer Weathered Wayfarer when restriction fails"
    );
}

#[test]
fn card_database_weathered_wayfarer_activates_when_opponent_has_more_lands() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wayfarer = scenario.add_real_card(P0, "Weathered Wayfarer", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Plains", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Island", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Mountain", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert!(
        can_activate_ability_now(runner.state(), P0, wayfarer, 0),
        "card-database Weathered Wayfarer must activate when an opponent has more lands"
    );
}

#[test]
fn card_database_weathered_wayfarer_existential_not_aggregate_in_three_player() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new_n_player(3, 99);
    scenario.at_phase(Phase::PreCombatMain);

    let wayfarer = scenario.add_real_card(P0, "Weathered Wayfarer", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Plains", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Island", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Forest", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert!(
        !can_activate_ability_now(runner.state(), P0, wayfarer, 0),
        "combined opponent land totals must not satisfy existential restriction"
    );
}

#[test]
fn isolated_watchtower_at_least_two_more_lands_gate() {
    // Isolated Watchtower — card-data export may lag the parser; exercise the
    // fresh Oracle parse path for this variant.
    const ISOLATED_WATCHTOWER: &str = "\
{3}, {T}: Draw a card. Activate only if an opponent controls at least two more \
lands than you.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let watchtower = scenario
        .add_creature_from_oracle(P0, "Isolated Watchtower", 0, 0, ISOLATED_WATCHTOWER)
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Green);

    let runner = scenario.build();
    assert!(
        !can_activate_ability_now(runner.state(), P0, watchtower, 0),
        "one-land lead must not satisfy 'at least two more lands than you'"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let watchtower = scenario
        .add_creature_from_oracle(P0, "Isolated Watchtower", 0, 0, ISOLATED_WATCHTOWER)
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_basic_land(P1, ManaColor::Green);
    scenario.add_basic_land(P1, ManaColor::Red);

    let runner = scenario.build();
    let state = runner.state();
    let restrictions =
        &state.objects.get(&watchtower).unwrap().abilities[0].activation_restrictions;
    assert!(
        check_activation_restrictions(state, P0, watchtower, 0, restrictions).is_ok(),
        "two-land lead must satisfy 'at least two more lands than you'"
    );
}
