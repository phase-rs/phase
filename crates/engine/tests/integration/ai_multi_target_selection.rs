//! Regression coverage for multi-target AI candidates and selection validation.

use engine::ai_support::{candidate_actions_broad, legal_actions, AiDecisionContract};
use engine::game::engine::apply_as_current;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CostResume, GameState, ManaAbilityResume, PayCostKind, PendingManaAbility, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

fn exact_thirteen_targets_state() -> (GameState, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(42);
    let targets: Vec<ObjectId> = (0..14)
        .map(|index| {
            create_object(
                &mut state,
                CardId(100 + index),
                P0,
                format!("Target {index}"),
                Zone::Battlefield,
            )
        })
        .collect();
    let ability = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        Vec::new(),
        targets[0],
        P0,
    );
    state.waiting_for = WaitingFor::MultiTargetSelection {
        player: P0,
        legal_targets: targets.clone(),
        min_targets: 13,
        max_targets: 13,
        pending_ability: Box::new(ability),
    };
    (state, targets)
}

fn mana_ability_resume() -> CostResume {
    CostResume::ManaAbility {
        mana_ability: Box::new(PendingManaAbility {
            player: P0,
            source_id: ObjectId(900),
            ability_index: None,
            rules_execution_node: None,
            ability_snapshot: None,
            color_override: None,
            resume: ManaAbilityResume::Priority,
            cost_move_resume: None,
            chosen_tappers: Vec::new(),
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            chosen_counter_count: None,
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            batch_siblings: Vec::new(),
        }),
    }
}

#[test]
fn multi_target_selection_offers_and_applies_an_exact_oversized_choice() {
    let (mut state, targets) = exact_thirteen_targets_state();

    let actions = legal_actions(&state);
    let action = actions
        .into_iter()
        .find(|action| matches!(action, GameAction::SelectCards { cards } if cards.len() == 13))
        .expect("the public legal-action set must include an exact 13-target selection");
    let GameAction::SelectCards { cards } = &action else {
        unreachable!("selected multi-target action must select cards");
    };
    assert_eq!(cards.len(), 13);
    assert!(cards.iter().all(|id| targets.contains(id)));
    assert!(
        AiDecisionContract::issue(&state, P0).contains_action(&state, &action),
        "the exact selection must remain in the AI contract"
    );

    apply_as_current(&mut state, action).expect("the exact multi-target selection must apply");
    assert_eq!(state.players[P0.0 as usize].life, 21);
}

#[test]
fn multi_target_selection_rejects_duplicates_before_mutating_state() {
    let (mut state, targets) = exact_thirteen_targets_state();
    let before = serde_json::to_value(&state).expect("game state serializes");

    let error = apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![targets[0]; 13],
        },
    )
    .expect_err("one target cannot fill all thirteen slots");
    assert!(error.to_string().contains("more than once"));
    assert_eq!(
        serde_json::to_value(&state).expect("game state serializes"),
        before,
        "rejected duplicates must not mutate game state"
    );

    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: targets.into_iter().take(13).collect(),
        },
    )
    .expect("a distinct exact-size selection remains legal");
}

#[test]
fn mana_ability_sacrifice_candidates_are_exact_while_generic_costs_keep_their_range() {
    let choices = vec![ObjectId(1), ObjectId(2)];
    let mut state = GameState::new_two_player(42);
    state.waiting_for = WaitingFor::PayCost {
        player: P0,
        kind: PayCostKind::Sacrifice,
        choices,
        count: 2,
        min_count: 0,
        resume: mana_ability_resume(),
    };

    let exact_sizes: Vec<_> = candidate_actions_broad(&state)
        .into_iter()
        .filter_map(|candidate| match candidate.action {
            GameAction::SelectCards { cards } => Some(cards.len()),
            _ => None,
        })
        .collect();
    assert_eq!(exact_sizes, vec![2]);

    let WaitingFor::PayCost { resume, .. } = &mut state.waiting_for else {
        unreachable!("fixture must remain a payment prompt");
    };
    *resume = CostResume::Resolution;
    let generic_sizes: Vec<_> = candidate_actions_broad(&state)
        .into_iter()
        .filter_map(|candidate| match candidate.action {
            GameAction::SelectCards { cards } => Some(cards.len()),
            _ => None,
        })
        .collect();
    assert_eq!(generic_sizes, vec![0, 1, 1, 2]);
}
