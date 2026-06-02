use engine::game::game_object::GameObject;
use engine::types::ability::{TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;

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
