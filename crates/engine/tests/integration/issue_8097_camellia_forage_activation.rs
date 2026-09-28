//! Issue #8097: Camellia, the Seedmiser's "{2}, Forage:" activation cost.
//!
//! CR 701.61a: "To forage means 'Exile three cards from your graveyard or
//! sacrifice a Food.'" The disjunction sits inside a `Composite` beside the
//! `{2}` mana leg, and CR 601.2h + CR 602.2b require the TOTAL cost to be
//! payable — each branch is judged with the sibling mana leg, never alone.

use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CAMELLIA_ORACLE: &str = "Menace\nOther Squirrels you control have menace.\nWhenever you sacrifice one or more Foods, create a 1/1 green Squirrel creature token.\n{2}, Forage: Put a +1/+1 counter on each other Squirrel you control. (To forage, exile three cards from your graveyard or sacrifice a Food.)";

struct CamelliaBoard {
    runner: GameRunner,
    camellia: ObjectId,
    other_squirrel: ObjectId,
    bear: ObjectId,
    food: ObjectId,
}

fn colorless(amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// Camellia + another Squirrel + a non-Squirrel for P0, one Food controlled by
/// `food_controller`, `graveyard_cards` cards in P0's graveyard and `mana`
/// colorless mana in P0's pool.
fn camellia_board(food_controller: PlayerId, graveyard_cards: usize, mana: usize) -> CamelliaBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let camellia = scenario
        .add_creature(P0, "Camellia, the Seedmiser", 3, 3)
        .with_subtypes(vec!["Squirrel", "Warlock"])
        .from_oracle_text(CAMELLIA_ORACLE)
        .id();
    let other_squirrel = scenario
        .add_creature(P0, "Other Squirrel", 1, 1)
        .with_subtypes(vec!["Squirrel"])
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let food = scenario
        .add_creature(food_controller, "Food", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Food"])
        .id();
    let names: Vec<String> = (0..graveyard_cards)
        .map(|index| format!("Graveyard Card {index}"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    scenario.with_graveyard(P0, &refs);
    scenario.with_mana_pool(P0, colorless(mana));
    CamelliaBoard {
        runner: scenario.build(),
        camellia,
        other_squirrel,
        bear,
        food,
    }
}

fn plus_one_counters(runner: &GameRunner, object_id: ObjectId) -> u32 {
    runner.state().objects[&object_id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn p0_graveyard_len(runner: &GameRunner) -> usize {
    runner.state().players[0].graveyard.len()
}

fn p0_pool(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

fn p0_tokens_on_battlefield(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &runner.state().objects[*id];
            obj.is_token && obj.controller == P0
        })
        .count()
}

fn activate(runner: &mut GameRunner, source_id: ObjectId) -> Vec<AbilityCost> {
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id,
            ability_index: 0,
        })
        .expect("activation must be legal on this board");
    match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { costs, .. } => costs,
        other => panic!("expected ActivationCostOneOfChoice, got {other:?}"),
    }
}

fn is_graveyard_exile(cost: &AbilityCost) -> bool {
    matches!(
        cost,
        AbilityCost::Exile {
            count: 3,
            zone: Some(Zone::Graveyard),
            ..
        }
    )
}

fn is_sacrifice(cost: &AbilityCost) -> bool {
    matches!(cost, AbilityCost::Sacrifice(_))
}

/// Answer every interactive cost prompt with the first `count` eligible choices.
fn pay_interactive_costs(runner: &mut GameRunner) {
    for _ in 0..8 {
        let WaitingFor::PayCost { choices, count, .. } = runner.state().waiting_for.clone() else {
            return;
        };
        let cards = choices.into_iter().take(count).collect();
        runner
            .act(GameAction::SelectCards { cards })
            .expect("selecting eligible cost choices pays the cost");
    }
    panic!("cost payment did not finish");
}

/// CR 701.61a + CR 601.2h + CR 602.2b: the exile half of forage, paid with the
/// sibling {2}, puts a counter on each OTHER Squirrel only.
#[test]
fn camellia_forage_exile_branch_counters_other_squirrels() {
    let CamelliaBoard {
        mut runner,
        camellia,
        other_squirrel,
        bear,
        food,
    } = camellia_board(P0, 3, 2);

    assert!(can_activate_ability_now(runner.state(), P0, camellia, 0));
    let exiled_before = runner.state().exile.len();

    let costs = activate(&mut runner, camellia);
    let exile_index = costs
        .iter()
        .position(is_graveyard_exile)
        .expect("exile-three branch offered with three graveyard cards");
    runner
        .act(GameAction::ChooseActivationCostBranch { index: exile_index })
        .expect("choosing the exile branch is accepted");
    pay_interactive_costs(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(plus_one_counters(&runner, other_squirrel), 1);
    assert_eq!(
        plus_one_counters(&runner, camellia),
        0,
        "Camellia is not 'other'"
    );
    assert_eq!(
        plus_one_counters(&runner, bear),
        0,
        "non-Squirrel gets nothing"
    );
    assert_eq!(p0_graveyard_len(&runner), 0, "three cards exiled");
    assert_eq!(runner.state().exile.len(), exiled_before + 3);
    assert_eq!(
        runner.state().objects[&food].zone,
        Zone::Battlefield,
        "exile branch leaves the Food alone"
    );
    assert_eq!(p0_pool(&runner), 0, "{{2}} paid as part of the total cost");

    // CR 702.111b + CR 613.1f (T6): Camellia's layer-6 grant reaches other
    // Squirrels only; the non-Squirrel stays without menace.
    assert!(runner.state().objects[&camellia].has_keyword(&Keyword::Menace));
    assert!(runner.state().objects[&other_squirrel].has_keyword(&Keyword::Menace));
    assert!(!runner.state().objects[&bear].has_keyword(&Keyword::Menace));
}

/// CR 701.61a + CR 701.21a: the Food half of forage sacrifices the Food, which
/// also fires Camellia's "Whenever you sacrifice one or more Foods" trigger.
#[test]
fn camellia_forage_food_branch_sacrifices_and_triggers() {
    let CamelliaBoard {
        mut runner,
        camellia,
        other_squirrel,
        food,
        ..
    } = camellia_board(P0, 3, 2);
    let graveyard_before = p0_graveyard_len(&runner);
    assert_eq!(p0_tokens_on_battlefield(&runner), 0);

    let costs = activate(&mut runner, camellia);
    let sacrifice_index = costs
        .iter()
        .position(is_sacrifice)
        .expect("sacrifice-a-Food branch offered with a Food");
    runner
        .act(GameAction::ChooseActivationCostBranch {
            index: sacrifice_index,
        })
        .expect("choosing the Food branch is accepted");
    pay_interactive_costs(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&food].zone, Zone::Graveyard);
    assert_eq!(
        p0_graveyard_len(&runner),
        graveyard_before + 1,
        "only the Food entered the graveyard; no graveyard card was exiled"
    );
    assert_eq!(plus_one_counters(&runner, other_squirrel), 1);
    assert_eq!(p0_tokens_on_battlefield(&runner), 1, "one Squirrel token");
    assert_eq!(p0_pool(&runner), 0);
}

/// CR 601.2h + CR 118.3 + CR 701.21a: only branches payable within the total
/// cost are offered; an opponent's Food never counts; cancelling pays nothing.
#[test]
fn camellia_forage_offers_only_payable_branches() {
    // (a) Two graveyard cards + own Food: only the sacrifice branch.
    let CamelliaBoard {
        mut runner,
        camellia,
        ..
    } = camellia_board(P0, 2, 2);
    let costs = activate(&mut runner, camellia);
    assert_eq!(costs.len(), 1, "only one branch payable: {costs:?}");
    assert!(is_sacrifice(&costs[0]), "offered branch: {costs:?}");

    // (b) No graveyard cards and only an opponent's Food: not activatable.
    // Paired positive: the same builder with three graveyard cards is.
    let CamelliaBoard {
        runner, camellia, ..
    } = camellia_board(P1, 0, 2);
    assert!(!can_activate_ability_now(runner.state(), P0, camellia, 0));
    let CamelliaBoard {
        runner, camellia, ..
    } = camellia_board(P1, 3, 2);
    assert!(can_activate_ability_now(runner.state(), P0, camellia, 0));

    // (c) Both forage halves available but only {1}: the total cost is unpayable.
    let CamelliaBoard {
        runner, camellia, ..
    } = camellia_board(P0, 3, 1);
    assert!(!can_activate_ability_now(runner.state(), P0, camellia, 0));

    // (d) Cancelling at the branch choice pays nothing.
    let CamelliaBoard {
        mut runner,
        camellia,
        food,
        ..
    } = camellia_board(P0, 3, 2);
    let costs = activate(&mut runner, camellia);
    assert_eq!(costs.len(), 2, "both branches payable: {costs:?}");
    runner
        .act(GameAction::CancelCast)
        .expect("cancel at the branch choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(p0_pool(&runner), 2);
    assert_eq!(p0_graveyard_len(&runner), 3);
    assert_eq!(runner.state().objects[&food].zone, Zone::Battlefield);
}

fn sibling_mana_board(mana: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Nested OneOf Witness", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(2),
                    },
                    AbilityCost::OneOf {
                        costs: vec![
                            AbilityCost::Mana {
                                cost: ManaCost::generic(1),
                            },
                            AbilityCost::PayLife {
                                amount: QuantityExpr::Fixed { value: 2 },
                            },
                        ],
                    },
                ],
            }),
        )
        .id();
    // CR 704.5b: keep a library card so the draw cannot deck P0.
    scenario.with_library_top(P0, &["Island"]);
    scenario.with_mana_pool(P0, colorless(mana));
    (scenario.build(), source)
}

/// CR 601.2h + CR 118.3: a nested branch is judged with its sibling mana leg.
/// With {2}, `{2} + {1}` is unpayable, so only the life branch is offered.
#[test]
fn nested_one_of_branch_filtered_against_sibling_mana_leg() {
    // Paired positive: {3} pays either branch together with the sibling {2}.
    let (mut runner, source) = sibling_mana_board(3);
    let costs = activate(&mut runner, source);
    assert_eq!(
        costs.len(),
        2,
        "both branches payable with {{3}}: {costs:?}"
    );

    let (mut runner, source) = sibling_mana_board(2);
    let costs = activate(&mut runner, source);
    assert_eq!(
        costs,
        vec![AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        }],
        "the {{1}} branch must not be offered when only {{2}} is available"
    );

    // Out-of-range branch index is rejected with nothing paid.
    assert!(runner
        .act(GameAction::ChooseActivationCostBranch { index: costs.len() })
        .is_err());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ActivationCostOneOfChoice { .. }
    ));
    assert_eq!(runner.state().players[0].life, 20);
    assert_eq!(p0_pool(&runner), 2);

    let hand_before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ChooseActivationCostBranch { index: 0 })
        .expect("choosing the life branch is accepted");
    pay_interactive_costs(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().players[0].life, 18, "paid 2 life");
    assert_eq!(p0_pool(&runner), 0, "paid the sibling {{2}}");
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
}

fn ai_branch_indices(runner: &GameRunner) -> Vec<usize> {
    let mut indices: Vec<usize> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ChooseActivationCostBranch { index } => Some(index),
            _ => None,
        })
        .collect();
    indices.sort_unstable();
    indices
}

/// CR 601.2h + CR 602.2b: AI enumeration agrees with the engine — the forage
/// activation is legal and exactly the engine-offered branches are choosable.
#[test]
fn camellia_forage_ai_legal_actions_match_engine_branches() {
    let CamelliaBoard {
        mut runner,
        camellia,
        ..
    } = camellia_board(P0, 3, 2);
    let priority_actions = engine::ai_support::legal_actions(runner.state());
    assert!(
        priority_actions.iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == camellia
        )),
        "AI must see Camellia's forage activation: {priority_actions:?}"
    );
    let costs = activate(&mut runner, camellia);
    assert_eq!(costs.len(), 2);
    assert_eq!(ai_branch_indices(&runner), vec![0, 1]);

    let CamelliaBoard {
        mut runner,
        camellia,
        ..
    } = camellia_board(P0, 2, 2);
    let costs = activate(&mut runner, camellia);
    let sacrifice_index = costs
        .iter()
        .position(is_sacrifice)
        .expect("sacrifice branch offered");
    assert_eq!(costs.len(), 1);
    assert_eq!(ai_branch_indices(&runner), vec![sacrifice_index]);

    let (mut runner, source) = sibling_mana_board(2);
    let costs = activate(&mut runner, source);
    assert_eq!(costs.len(), 1);
    assert_eq!(ai_branch_indices(&runner), vec![0]);
}
