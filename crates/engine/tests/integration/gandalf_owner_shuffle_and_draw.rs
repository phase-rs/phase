use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GANDALF: &str = "Ward {3} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {3}.)\n{6}: Gandalf's owner shuffles him into their library and draws three cards.";

fn gandalf_owner_shuffle_and_draw(owner: PlayerId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gandalf = scenario
        .add_creature(owner, "Gandalf, Wandering Wizard", 4, 5)
        .from_oracle_text_with_keywords(&["Ward"], GANDALF)
        .controlled_by(P0)
        .id();
    scenario.with_library_top(P0, &["P0 A", "P0 B", "P0 C", "P0 D", "P0 E"]);
    scenario.with_library_top(P1, &["P1 A", "P1 B", "P1 C", "P1 D", "P1 E"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 6],
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&gandalf].owner, owner);
    assert_eq!(runner.state().objects[&gandalf].controller, P0);
    let index = runner.state().objects[&gandalf]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("Gandalf has a costed activated ability");

    // No target intent: the printed ability must resolve without announcement.
    let outcome = runner.activate(gandalf, index).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0, "P0 pays all six mana");
    outcome.assert_hand_drawn(owner, 3);
    let other = if owner == P0 { P1 } else { P0 };
    outcome.assert_hand_drawn(other, 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());

    let moved = outcome
        .events()
        .iter()
        .position(|event| {
            matches!(event, GameEvent::ZoneChanged {
            object_id, from: Some(Zone::Battlefield), to: Zone::Library, ..
        } if *object_id == gandalf)
        })
        .expect("Gandalf moves from the battlefield into his owner's library");
    let shuffles: Vec<_> = outcome
        .events()
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some((index, *player_id)),
            _ => None,
        })
        .collect();
    assert!(
        !shuffles.is_empty(),
        "the owner actually shuffles a library"
    );
    for (index, player) in shuffles {
        assert_eq!(player, owner, "only the owner's library is shuffled");
        assert!(
            moved < index,
            "Gandalf enters the library before it is shuffled"
        );
    }
    // Gandalf may be among the three drawn cards; his final zone is not fixed.
    assert_ne!(outcome.state().objects[&gandalf].zone, Zone::Battlefield);
}

#[test]
fn stolen_gandalf_shuffles_and_draws_for_owner_not_controller() {
    gandalf_owner_shuffle_and_draw(P1);
}

#[test]
fn own_controlled_gandalf_shuffles_and_draws_for_owner() {
    gandalf_owner_shuffle_and_draw(P0);
}
