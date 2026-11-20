//! Thada Adel, Acquisitor (#8066): "Whenever Thada Adel deals combat damage to a player, search
//! that player's library for an artifact card and exile it." The CONTROLLER searches the DAMAGED
//! player's library (CR 701.23a), not their own.

use super::rules::{run_combat, GameScenario, Phase, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::zones::Zone;

const THADA_TRIGGER: &str = "Whenever Thada Adel deals combat damage to a player, search that player's library for an artifact card and exile it. Then that player shuffles. Until end of turn, you may play that card.";

#[test]
fn thada_adel_searches_the_damaged_players_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let thada = {
        let mut b = scenario.add_creature(P0, "Thada Adel, Acquisitor", 2, 2);
        b.from_oracle_text(THADA_TRIGGER);
        b.id()
    };
    // The controller's own artifact is inserted LAST (top of library), so a search that
    // wrongly reads the controller's library cannot pass by candidate ordering.
    let opp_artifact = scenario.add_card_to_library_top(P1, "Opponent Artifact");
    let own_artifact = scenario.add_card_to_library_top(P0, "Controller Artifact");

    let mut runner = scenario.build();
    for id in [opp_artifact, own_artifact] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact];
    }
    run_combat(&mut runner, vec![thada], vec![]);

    for _ in 0..10 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }

    let WaitingFor::SearchChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!(
            "expected SearchChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0, "the controller is the searcher");
    assert_eq!(
        cards.to_vec(),
        vec![opp_artifact],
        "only the damaged player's artifact is a candidate"
    );

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![opp_artifact],
        })
        .expect("select the found artifact");
    // The damaged player, not the searcher, shuffles their library.
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id: P1,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )),
        "the damaged player shuffles their library"
    );
    assert_eq!(
        runner.state().objects[&opp_artifact].zone,
        Zone::Exile,
        "the found artifact is exiled"
    );
    assert_eq!(
        runner.state().objects[&own_artifact].zone,
        Zone::Library,
        "the controller's own library is untouched"
    );
}
