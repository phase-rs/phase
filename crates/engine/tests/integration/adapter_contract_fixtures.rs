use engine::game::game_object::GameObject;
use engine::game::interaction::{bind_interaction_authority, submit_interaction};
use engine::types::ability::{TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::interaction::{
    InteractionResponse, InteractionSessionId, InteractionShortcutDecision, InteractionSubmission,
};

/// U8 byte-linkage: the Rust end of the one fixture the TS row also reads. The
/// variant is asserted before the values, so a fixture that parsed into the
/// wrong response variant cannot pass on the values alone.
///
/// The same payload is then driven through the production submission entry
/// point on the board it was curated from — `i-1` is that board's interaction
/// session, so every id in the fixture is one the engine itself minted. Parsing
/// alone accepted a payload production refuses, which is the gap this leg
/// closes: mutate an id, a group, or an `amount` and the submission is refused
/// rather than silently pinning a contract no adapter can use.
#[test]
fn shortcut_allocation_submission_fixture_matches_curated_client_contract() {
    let parsed: InteractionSubmission = serde_json::from_str(include_str!(
        "../../../../fixtures/adapter-contract/shortcut_allocation_submission.json"
    ))
    .unwrap();
    assert_eq!(parsed.interaction_id.0, "i-1.0.1");
    let InteractionResponse::Shortcut { decision, pins } = &parsed.response else {
        panic!("wrong variant: {:?}", parsed.response);
    };
    assert_eq!(
        *decision,
        InteractionShortcutDecision::Fixed { iterations: 18 }
    );
    let allocation = pins
        .iter()
        .find(|pin| pin.group == 2)
        .unwrap_or_else(|| panic!("the allocation pin is stated over group 2: {pins:?}"));
    assert_eq!(
        allocation
            .choice_ids
            .iter()
            .map(|id| id.0.as_str())
            .collect::<Vec<_>>(),
        ["i-1.0.1.k4", "i-1.0.1.k5", "i-1.0.1.k6"]
    );
    assert_eq!(
        allocation
            .amounts
            .iter()
            .map(|assignment| (assignment.choice_id.0.as_str(), assignment.amount))
            .collect::<Vec<_>>(),
        [("i-1.0.1.k4", 6), ("i-1.0.1.k5", 6), ("i-1.0.1.k6", 6)]
    );

    let (mut state, proposer) = crate::interaction_contract::f4_offer_board();
    bind_interaction_authority(&mut state, InteractionSessionId("i-1".to_string()))
        .expect("the fixture's own session binds over the live offer");
    submit_interaction(&mut state, proposer, parsed)
        .expect("the curated submission is one the production entry point accepts");
    assert!(
        !matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
        "an accepted submission moves the pending interaction off the offer, so the leg above \
         is acceptance rather than an inert payload"
    );
}

#[test]
fn game_action_fixture_matches_curated_client_contract() {
    let parsed: GameAction = serde_json::from_str(include_str!(
        "../../../../fixtures/adapter-contract/game_action.json"
    ))
    .unwrap();
    match parsed {
        GameAction::ChooseLegend { keep } => assert_eq!(keep.0, 1),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn waiting_for_fixture_matches_curated_client_contract() {
    let parsed: WaitingFor = serde_json::from_str(include_str!(
        "../../../../fixtures/adapter-contract/waiting_for.json"
    ))
    .unwrap();
    match parsed {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            source_id,
            ..
        } => {
            assert_eq!(player.0, 0);
            assert_eq!(cards.len(), 2);
            assert_eq!(count, 1);
            assert_eq!(source_id.0, 99);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn waiting_for_priority_fixture_matches_curated_client_contract() {
    let parsed: WaitingFor = serde_json::from_str(include_str!(
        "../../../../fixtures/adapter-contract/waiting_for_priority.json"
    ))
    .unwrap();
    match parsed {
        WaitingFor::Priority { player } => assert_eq!(player.0, 0),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn waiting_for_category_choice_fixture_matches_curated_client_contract() {
    let parsed: WaitingFor = serde_json::from_str(include_str!(
        "../../../../fixtures/adapter-contract/waiting_for_category_choice.json"
    ))
    .unwrap();
    match parsed {
        WaitingFor::CategoryChoice {
            player,
            target_player,
            categories,
            choose_filter,
            sacrifice_filter,
            source_controller,
            eligible_per_category,
            remaining_players,
            all_kept,
            scoped_players,
            ..
        } => {
            assert_eq!(player.0, 0);
            assert_eq!(target_player.0, 0);
            assert_eq!(categories.len(), 2);
            assert!(filter_contains_nonland(&choose_filter));
            assert!(filter_contains_nonland(&sacrifice_filter));
            assert_eq!(source_controller.0, 0);
            assert_eq!(eligible_per_category[0][0].0, 10);
            assert_eq!(remaining_players[0].0, 1);
            assert!(all_kept.is_empty());
            assert_eq!(scoped_players.len(), 2);
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

fn filter_contains_nonland(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .type_filters
            .iter()
            .any(|type_filter| matches!(type_filter, TypeFilter::Non(inner) if **inner == TypeFilter::Land)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_contains_nonland)
        }
        TargetFilter::Not { filter } => filter_contains_nonland(filter),
        _ => false,
    }
}

#[test]
fn game_object_fixture_matches_curated_client_contract() {
    let parsed: GameObject = serde_json::from_str(include_str!(
        "../../../../fixtures/adapter-contract/game_object.json"
    ))
    .unwrap();
    assert_eq!(parsed.name, "Fixture Bear");
    assert_eq!(parsed.id.0, 1);
    assert_eq!(parsed.card_id.0, 100);
    assert_eq!(parsed.power, Some(2));
    assert_eq!(parsed.toughness, Some(2));
}
