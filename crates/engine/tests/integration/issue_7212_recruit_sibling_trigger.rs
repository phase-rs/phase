//! GitHub issue #7212 — Recruit must retain its discard provenance while an
//! earlier ETB trigger from the same permanent remains on the stack.

use std::io::Read;

use engine::game::scenario::{GameRunner, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::zones::Zone;

fn load_state() -> GameState {
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        &include_bytes!("fixtures/issue_7212_recruit_with_sibling_trigger.json.gz")[..],
    )
    .read_to_string(&mut json)
    .expect("fixture .json.gz must inflate to UTF-8 JSON");
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("game-state envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract")
}

fn token_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.is_token)
        .count()
}

fn graveyard_count(state: &GameState, player: engine::types::player::PlayerId) -> usize {
    state
        .objects
        .values()
        .filter(|object| object.zone == Zone::Graveyard && object.owner == player)
        .count()
}

fn pass_priority_until_combat(runner: &mut GameRunner) {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "expected priority while resolving the two ETB triggers, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance the stacked ETB triggers");
    }
    panic!(
        "the stacked ETB triggers never settled; final state: {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn recruit_discard_with_a_sibling_etb_trigger_records_its_own_result() {
    let mut runner = GameRunner::from_state(load_state());
    let tokens_before = token_count(runner.state());
    let graveyard_before = graveyard_count(runner.state(), P0);

    pass_priority_until_combat(&mut runner);

    assert_eq!(
        graveyard_count(runner.state(), P0),
        graveyard_before + 1,
        "Recruit's drawn card is automatically discarded from its controller's one-card hand"
    );
    assert_eq!(
        token_count(runner.state()),
        tokens_before + 1,
        "Recruit creates its contingent token after discarding a nonland"
    );
}
