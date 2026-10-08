//! Real-card regression for the reducer-backed prospective fetchland route.

use engine::ai_support::{
    certify_fetch_then_cast, validated_candidate_actions_for_semantic_owner, CandidateAction,
};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::identifiers::{ObjectId, ObjectIdentityBinding, ObjectIncarnationRef};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn fetchland_state() -> (
    engine::types::game_state::GameState,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let misty = scenario.add_real_card(P0, "Misty Rainforest", Zone::Battlefield, db);
    // Phantom Monster needs three generic mana in addition to the fetched
    // blue source. These preserve the Forest-first baseline while making the
    // Island branch's normal payment continuation reducer-certifiable.
    for _ in 0..3 {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Library, db);
    // These earlier hand cards ensure certification is not limited by hand
    // order: the beneficial Phantom route must remain reachable after other
    // castable alternatives have been considered.
    scenario.add_real_card(P0, "Goblin Piker", Zone::Hand, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let phantom = scenario.add_real_card(P0, "Phantom Monster", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner.state().clone(), misty, forest, island, phantom)
}

fn misty_candidate(
    state: &engine::types::game_state::GameState,
    misty: ObjectId,
) -> CandidateAction {
    validated_candidate_actions_for_semantic_owner(state, P0)
        .into_iter()
        .find(|candidate| {
            matches!(
                candidate.action,
                GameAction::ActivateAbility { source_id, .. } if source_id == misty
            )
        })
        .expect("Misty Rainforest's real activated ability must be a validated root candidate")
}

#[test]
fn prospective_fetchland_certificate_prefers_island_and_commits_phantom_monster() {
    let (state, misty, forest, island, phantom) = fetchland_state();
    let fetch = misty_candidate(&state, misty);
    let cast = ObjectIdentityBinding::new(
        ObjectIncarnationRef::from_object(&state.objects[&phantom]),
        Zone::Hand,
    );

    let GameAction::ActivateAbility { ability_index, .. } = &fetch.action else {
        panic!("the validated Misty root must be an activation");
    };

    // The generic deterministic tutor route takes the first actual search
    // card, Forest, which cannot pay Phantom Monster's printed {3}{U} cost.
    let mut generic = GameRunner::from_state(state.clone());
    generic
        .activate(misty, *ability_index)
        .search_first_legal()
        .resolve();
    assert_eq!(
        generic.state().objects[&forest].zone,
        Zone::Battlefield,
        "the generic SearchChoice policy takes the first legal Forest"
    );
    assert_eq!(
        generic.state().objects[&island].zone,
        Zone::Library,
        "the generic SearchChoice policy leaves Island unfetched"
    );

    // The scored certificate follows that same frozen root through the reducer,
    // evaluates each real SearchChoice, and picks Island because it can commit
    // Phantom Monster to the stack.
    let casts: Vec<_> = state.players[P0.0 as usize]
        .hand
        .iter()
        .map(|object_id| {
            ObjectIdentityBinding::new(
                ObjectIncarnationRef::from_object(&state.objects[object_id]),
                Zone::Hand,
            )
        })
        .collect();
    assert_eq!(casts.last(), Some(&cast));
    let (prompt, _) = certify_fetch_then_cast(&state, &fetch, &casts, |terminal, selected_cast| {
        if terminal.objects[&island].zone == Zone::Battlefield
            && terminal.objects[&phantom].zone == Zone::Stack
            && *selected_cast == cast
        {
            1.0
        } else {
            0.0
        }
    })
    .expect("the real fetch-then-cast route must be reducer-certifiable");
    let mut real = GameRunner::from_state(state.clone());
    real.activate(misty, *ability_index).resolve();
    let scored_pick = prompt
        .action_for(real.state(), P0)
        .expect("the certified token must match the real SearchChoice");
    let follow_up = prompt.follow_up();
    assert_eq!(
        scored_pick,
        GameAction::SelectCards {
            cards: vec![island]
        }
    );
    real.act(scored_pick)
        .expect("certified Island selection applies");
    let cast_action = follow_up
        .action_for(real.state(), P0)
        .expect("the exact post-search state unlocks the certified Phantom Monster cast");
    assert!(matches!(cast_action, GameAction::CastSpell { object_id, .. } if object_id == phantom));
    real.act(cast_action)
        .expect("ordinary cast commits Phantom Monster");
    assert_eq!(real.state().objects[&phantom].zone, Zone::Stack);
}
