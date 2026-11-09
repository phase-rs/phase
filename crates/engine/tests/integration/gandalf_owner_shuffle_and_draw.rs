use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GANDALF: &str = "Ward {3} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {3}.)\n{6}: Gandalf's owner shuffles him into their library and draws three cards.";
const COSI: &str =
    "Whenever an opponent shuffles their library, you may put a +1/+1 counter on this creature.";

fn gandalf_owner_shuffle_and_draw(owner: PlayerId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gandalf = scenario
        .add_creature(owner, "Gandalf, Wandering Wizard", 4, 5)
        .from_oracle_text_with_keywords(&["Ward"], GANDALF)
        .controlled_by(P0)
        .id();
    let observer = if owner == P0 { P1 } else { P0 };
    let trickster = scenario
        .add_creature(observer, "Cosi's Trickster", 1, 1)
        .from_oracle_text(COSI)
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
    let outcome = runner.activate(gandalf, index).accept_optional().resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0, "P0 pays all six mana");
    outcome.assert_hand_drawn(owner, 3);
    let other = if owner == P0 { P1 } else { P0 };
    outcome.assert_hand_drawn(other, 0);
    outcome.assert_counters(trickster, CounterType::Plus1Plus1, 1);
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
    assert_eq!(
        shuffles.len(),
        1,
        "the printed instruction shuffles exactly once"
    );
    let first_draw = outcome
        .events()
        .iter()
        .position(
            |event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == owner),
        )
        .expect("the owner draws after the shuffle");
    for (index, player) in shuffles {
        assert_eq!(player, owner, "only the owner's library is shuffled");
        assert!(
            moved < index,
            "Gandalf enters the library before it is shuffled"
        );
        assert!(index < first_draw, "the shuffle precedes the owner's draws");
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

#[test]
fn unavailable_stolen_gandalf_still_shuffles_once_before_its_owner_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gandalf = scenario
        .add_creature(P1, "Gandalf, Wandering Wizard", 4, 5)
        .from_oracle_text_with_keywords(&["Ward"], GANDALF)
        .controlled_by(P0)
        .id();
    let trickster = scenario
        .add_creature(P0, "Cosi's Trickster", 1, 1)
        .from_oracle_text(COSI)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.with_library_top(P0, &["P0 A", "P0 B", "P0 C", "P0 D", "P0 E"]);
    scenario.with_library_top(P1, &["P1 A", "P1 B", "P1 C", "P1 D", "P1 E"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 6],
    );
    let mut runner = scenario.build();
    let index = runner.state().objects[&gandalf]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("Gandalf has the printed six-mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: gandalf,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: island,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // Its controller responds to their own ability, so Ward is not involved.
    let outcome = runner
        .cast(unsummon)
        .target_object(gandalf)
        .accept_optional()
        .resolve();
    assert_eq!(outcome.state().objects[&gandalf].zone, Zone::Hand);
    assert_eq!(outcome.state().objects[&gandalf].owner, P1);
    // The bounced Gandalf also enters P1's hand; count actual draw events,
    // not the hand-size delta across Unsummon and the activated ability.
    for (player, count) in [(P0, 0), (P1, 3)] {
        assert_eq!(
            outcome
                .events()
                .iter()
                .filter(|event| matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == player))
                .count(),
            count,
            "only the owner draws the printed three cards"
        );
    }
    outcome.assert_counters(trickster, CounterType::Plus1Plus1, 1);
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
    assert_eq!(shuffles.len(), 1, "CR 701.24c still requires one shuffle");
    assert_eq!(shuffles[0].1, P1);
    let first_draw = outcome
        .events()
        .iter()
        .position(|event| matches!(event, GameEvent::CardDrawn { player_id: P1, .. }))
        .expect("the owner draws after the unavailable-object shuffle");
    assert!(shuffles[0].0 < first_draw);
    assert!(!outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, to: Zone::Library, .. } if *object_id == gandalf
    )));
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
}
