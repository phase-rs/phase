//! Tomb Tyrant activation restriction: legal iff there are at least three
//! Zombie creature cards in the activator's graveyard and it is their turn.
//!
//! Parser-only fix; runtime already evaluates `RequiresCondition` +
//! `ZoneCardCount` + `DuringYourTurn`. This file is the discriminating
//! production-path test (CR 602.5).

use engine::game::casting::can_activate_ability_now;
use engine::game::restrictions::check_activation_restrictions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, ActivationRestriction, Comparator, CountScope, Effect, ParsedCondition,
    QuantityExpr, QuantityRef, TargetFilter, TypeFilter, TypedFilter, ZoneRef,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle (2026-09-11).
const TOMB_TYRANT_ORACLE: &str = "\
Other Zombies you control get +1/+1.\n\
{2}{B}, {T}, Sacrifice a creature: Return a Zombie creature card at random from your graveyard to the battlefield. Activate only during your turn and only if there are at least three Zombie creature cards in your graveyard.";

struct Board {
    p0_zombie_gy: usize,
    p0_human_gy: usize,
    p1_zombie_gy: usize,
    p0_zombie_battlefield: usize,
    active: PlayerId,
    stage_costs: bool,
}

impl Default for Board {
    fn default() -> Self {
        Self {
            p0_zombie_gy: 0,
            p0_human_gy: 0,
            p1_zombie_gy: 0,
            p0_zombie_battlefield: 0,
            active: P0,
            stage_costs: false,
        }
    }
}

fn pool_two_generic_and_black() -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..2 {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    pool.push(ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]));
    pool
}

fn build_board(board: Board) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tyrant = scenario
        .add_creature_from_oracle(P0, "Tomb Tyrant", 3, 3, TOMB_TYRANT_ORACLE)
        .id();
    for i in 0..board.p0_zombie_gy {
        scenario
            .add_creature_to_graveyard(P0, &format!("Yard Zombie {i}"), 2, 2)
            .with_subtypes(vec!["Zombie"]);
    }
    for i in 0..board.p0_human_gy {
        scenario
            .add_creature_to_graveyard(P0, &format!("Yard Human {i}"), 2, 2)
            .with_subtypes(vec!["Human"]);
    }
    for i in 0..board.p1_zombie_gy {
        scenario
            .add_creature_to_graveyard(P1, &format!("Opp Zombie {i}"), 2, 2)
            .with_subtypes(vec!["Zombie"]);
    }
    for i in 0..board.p0_zombie_battlefield {
        scenario
            .add_creature(P0, &format!("Field Zombie {i}"), 2, 2)
            .with_subtypes(vec!["Zombie"]);
    }
    if board.stage_costs {
        scenario.with_mana_pool(P0, pool_two_generic_and_black());
        let _ = scenario.add_vanilla(P0, 1, 1);
    }
    let mut runner = scenario.build();
    if board.active != P0 {
        let state = runner.state_mut();
        state.active_player = board.active;
        state.priority_player = board.active;
    }
    (runner, tyrant)
}

fn has_unimplemented(definition: &AbilityDefinition) -> bool {
    matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })
        || definition
            .sub_ability
            .as_deref()
            .is_some_and(has_unimplemented)
}

fn tyrant_activate(state: &GameState, tyrant: ObjectId) -> &AbilityDefinition {
    state
        .objects
        .get(&tyrant)
        .expect("Tomb Tyrant exists")
        .abilities
        .iter()
        .find(|ability| {
            matches!(
                ability.effect.as_ref(),
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    ..
                }
            )
        })
        .expect("Tomb Tyrant GY→battlefield activate")
}

fn tyrant_activate_index(state: &GameState, tyrant: ObjectId) -> usize {
    state
        .objects
        .get(&tyrant)
        .expect("Tomb Tyrant exists")
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.effect.as_ref(),
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    ..
                }
            )
        })
        .expect("Tomb Tyrant activate index")
}

/// Reach-guard: restrictions are populated and the Activate sentence is not
/// Unimplemented. Without this, empty restrictions make every `check_*.is_err()`
/// vacuous on a reverted parser.
fn assert_restriction_ast(state: &GameState, tyrant: ObjectId) {
    let ability = tyrant_activate(state, tyrant);
    assert!(
        !has_unimplemented(ability),
        "Activate sentence must not remain Unimplemented: {:#?}",
        ability
    );
    // CR 602.1b: activation instructions are not the effect.
    assert!(
        ability.condition.is_none(),
        "activation gate must live on restrictions, not resolution condition"
    );
    let restrictions = &ability.activation_restrictions;
    assert!(
        restrictions.len() >= 2,
        "reach-guard: restrictions populated, got {restrictions:?}"
    );
    assert!(
        restrictions.contains(&ActivationRestriction::DuringYourTurn),
        "must carry DuringYourTurn: {restrictions:?}"
    );
    assert!(
        restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Graveyard,
                            card_types,
                            filter: Some(TargetFilter::Typed(TypedFilter {
                                type_filters,
                                controller: None,
                                properties,
                            })),
                            scope: CountScope::Controller,
                        }
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                })
            } if card_types.is_empty()
                && properties.is_empty()
                && type_filters.as_slice()
                    == [
                        TypeFilter::Creature,
                        TypeFilter::Subtype("Zombie".to_string()),
                    ]
        )),
        "CR 205.1 + CR 205.3m + CR 107.1 + CR 404.1: GE 3 Zombie creature cards: {restrictions:?}"
    );
}

fn restriction_ok(state: &GameState, tyrant: ObjectId) -> bool {
    let ability = tyrant_activate(state, tyrant);
    check_activation_restrictions(
        state,
        P0,
        tyrant,
        tyrant_activate_index(state, tyrant),
        &ability.activation_restrictions,
    )
    .is_ok()
}

fn legal_three_zombie_board() -> (GameRunner, ObjectId) {
    build_board(Board {
        p0_zombie_gy: 3,
        ..Board::default()
    })
}

#[test]
fn three_zombie_creature_cards_in_controller_graveyard_is_legal() {
    let (runner, tyrant) = legal_three_zombie_board();
    let state = runner.state();
    assert_restriction_ast(state, tyrant);
    // CR 602.5 + CR 107.1 + CR 404.1: three matching cards meet the threshold.
    assert!(
        restriction_ok(state, tyrant),
        "three Zombie creature cards in P0 GY on P0's turn must be legal"
    );

    let (runner, tyrant) = build_board(Board {
        p0_zombie_gy: 3,
        stage_costs: true,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    let idx = tyrant_activate_index(runner.state(), tyrant);
    assert!(
        can_activate_ability_now(runner.state(), P0, tyrant, idx),
        "production entry must be legal with costs staged, three GY Zombies, P0's turn"
    );
}

#[test]
fn two_zombie_creature_cards_in_controller_graveyard_is_illegal() {
    let (legal, tyrant) = legal_three_zombie_board();
    assert!(restriction_ok(legal.state(), tyrant));

    let (runner, tyrant) = build_board(Board {
        p0_zombie_gy: 2,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    assert!(
        !restriction_ok(runner.state(), tyrant),
        "CR 107.1: two matching cards must not satisfy GE 3"
    );

    let (runner, tyrant) = build_board(Board {
        p0_zombie_gy: 2,
        stage_costs: true,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    let idx = tyrant_activate_index(runner.state(), tyrant);
    assert!(
        !can_activate_ability_now(runner.state(), P0, tyrant, idx),
        "production entry must refuse two GY Zombies even with costs staged"
    );
}

#[test]
fn empty_graveyard_is_illegal() {
    let (legal, tyrant) = legal_three_zombie_board();
    assert!(restriction_ok(legal.state(), tyrant));

    let (runner, tyrant) = build_board(Board::default());
    assert_restriction_ast(runner.state(), tyrant);
    assert!(
        !restriction_ok(runner.state(), tyrant),
        "CR 404.1: an empty graveyard must not satisfy GE 3"
    );
}

#[test]
fn three_human_creatures_in_graveyard_are_not_zombies() {
    let (legal, tyrant) = legal_three_zombie_board();
    assert!(restriction_ok(legal.state(), tyrant));

    let (runner, tyrant) = build_board(Board {
        p0_human_gy: 3,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    assert!(
        !restriction_ok(runner.state(), tyrant),
        "CR 205.3m: Human creatures must not satisfy the Zombie conjunct"
    );
}

#[test]
fn two_zombies_and_one_human_do_not_meet_threshold() {
    let (legal, tyrant) = legal_three_zombie_board();
    assert!(restriction_ok(legal.state(), tyrant));

    let (runner, tyrant) = build_board(Board {
        p0_zombie_gy: 2,
        p0_human_gy: 1,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    assert!(
        !restriction_ok(runner.state(), tyrant),
        "mixed 2 Zombie + 1 Human must not satisfy GE 3 Zombie creatures"
    );
}

#[test]
fn opponent_graveyard_zombies_do_not_count() {
    let (legal, tyrant) = legal_three_zombie_board();
    assert!(restriction_ok(legal.state(), tyrant));

    let (runner, tyrant) = build_board(Board {
        p1_zombie_gy: 3,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    assert!(
        !restriction_ok(runner.state(), tyrant),
        "CR 404.1: CountScope::Controller is the activator's graveyard, not P1's"
    );
}

#[test]
fn battlefield_zombies_do_not_count() {
    let (legal, tyrant) = legal_three_zombie_board();
    assert!(restriction_ok(legal.state(), tyrant));

    let (runner, tyrant) = build_board(Board {
        p0_zombie_battlefield: 3,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    assert!(
        !restriction_ok(runner.state(), tyrant),
        "CR 404.1: battlefield Zombies are not graveyard cards"
    );
}

#[test]
fn opponent_turn_is_illegal_even_with_three_zombies() {
    let (legal, tyrant) = legal_three_zombie_board();
    assert!(restriction_ok(legal.state(), tyrant));

    let (runner, tyrant) = build_board(Board {
        p0_zombie_gy: 3,
        active: P1,
        ..Board::default()
    });
    assert_restriction_ast(runner.state(), tyrant);
    assert!(
        !restriction_ok(runner.state(), tyrant),
        "CR 602.5: DuringYourTurn fails on P1's turn"
    );
}
