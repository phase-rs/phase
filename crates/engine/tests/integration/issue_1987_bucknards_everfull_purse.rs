//! Regression (issue #1987): Bucknard's Everfull Purse activated ability must
//! roll a d4, create that many Treasure tokens, and pass control to the player
//! on the controller's right.
//!
//! CR 706.2: "create a number of Treasure tokens equal to the result."
//! CR 102.1 + CR 103.1: "the player to your right" is seating-relative.

use engine::game::players::previous_player;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, SeatDirection, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);

const BUCKNARDS_ORACLE: &str = "{1}, {T}: Roll a d4 and create a number of Treasure tokens equal to the result. The player to your right gains control of this artifact.";

fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn treasure_count_for_controller(state: &GameState, controller: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| {
            o.card_types.subtypes.contains(&"Treasure".to_string()) && o.controller == controller
        })
        .count()
}

#[test]
fn issue_1987_bucknards_parsed_oracle_creates_treasures_and_passes_right() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let purse_id = scenario
        .add_creature(P0, "Bucknard's Everfull Purse", 0, 0)
        .as_artifact()
        .from_oracle_text(BUCKNARDS_ORACLE)
        .id();

    scenario.with_mana_pool(P0, floating_colorless(1));

    let mut runner = scenario.build();
    let ability = &runner.state().objects.get(&purse_id).unwrap().abilities[0];
    let Effect::RollDie { .. } = ability.effect.as_ref() else {
        panic!("head must be RollDie, got {:?}", ability.effect);
    };
    let token = ability.sub_ability.as_ref().expect("RollDie sub = Token");
    let give = token.sub_ability.as_ref().expect("Token sub = GiveControl");
    let Effect::GiveControl { target, recipient } = give.effect.as_ref() else {
        panic!("Token sub must be GiveControl, got {:?}", give.effect);
    };
    assert_eq!(target, &TargetFilter::SelfRef);
    assert_eq!(
        recipient,
        &TargetFilter::Neighbor {
            direction: SeatDirection::Right
        }
    );

    let outcome = runner.activate(purse_id, 0).resolve();
    let state = outcome.state();

    let treasures = treasure_count_for_controller(state, P0);
    assert!(
        (1..=4).contains(&treasures),
        "must create between 1 and 4 Treasure tokens (d4 roll), got {treasures}; waiting_for={:?}",
        state.waiting_for
    );

    assert_eq!(
        previous_player(state, P0),
        P2,
        "right neighbor of P0 in a 3-player seat is P2"
    );
    assert_eq!(
        state.objects.get(&purse_id).unwrap().controller,
        P2,
        "Purse must transfer to the player on the controller's right"
    );
    assert!(
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.card_types.subtypes.contains(&"Treasure".to_string()))
            .all(|o| o.card_types.core_types.contains(&CoreType::Artifact) && o.color.is_empty()),
        "Treasure tokens must be colorless artifacts"
    );
}
