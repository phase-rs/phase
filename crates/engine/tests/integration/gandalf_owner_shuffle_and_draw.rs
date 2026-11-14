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

#[test]
fn gemrazer_under_gandalf_delivers_both_cards_before_one_owner_shuffle_and_three_draws() {
    // Full printed Oracle text, verified against Scryfall.
    const GEMRAZER: &str = "Mutate {1}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nReach, trample\nWhenever this creature mutates, destroy target artifact or enchantment an opponent controls.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gandalf = scenario
        .add_creature(P0, "Gandalf, Wandering Wizard", 4, 5)
        .with_subtypes(vec!["Avatar", "Wizard"])
        .from_oracle_text_with_keywords(&["Ward"], GANDALF)
        .id();
    let gemrazer = scenario
        .add_creature_to_hand_from_oracle(P0, "Gemrazer", 4, 4, GEMRAZER)
        .with_subtypes(vec!["Beast"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        })
        .from_oracle_text_with_keywords(&["Mutate", "Reach", "Trample"], GEMRAZER)
        .id();
    let trickster = scenario
        .add_creature(P1, "Cosi's Trickster", 1, 1)
        .from_oracle_text(COSI)
        .id();
    scenario.with_library_top(P0, &["P0 A", "P0 B", "P0 C", "P0 D", "P0 E"]);
    scenario.with_library_top(P1, &["P1 A", "P1 B", "P1 C", "P1 D", "P1 E"]);
    let mut mana = vec![ManaUnit::new(ManaType::Green, ObjectId(9_999), false, vec![]); 3];
    mana.extend(vec![
        ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9_999),
            false,
            vec![]
        );
        6
    ]);
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    let original_activation = runner.state().objects[&gandalf]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("Gandalf has his printed six-mana activation");

    let mutated = runner
        .cast(gemrazer)
        .alternative_cast(engine::types::actions::AlternativeCastDecision::Alternative)
        .target_object(gandalf)
        .resolve();
    assert_eq!(mutated.mana_pool_total(P0), 6, "pay the mutate cost");
    assert!(
        matches!(
            mutated.final_waiting_for(),
            WaitingFor::MutateMergeChoice { .. }
        ),
        "the real mutate cast must reach its over-or-under choice"
    );
    runner
        .act(GameAction::ChooseMutateMergeSide {
            side: engine::game::merge::MergeSide::Bottom,
        })
        .expect("put Gemrazer under Gandalf");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&gandalf].name,
        "Gandalf, Wandering Wizard"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());

    let outcome = runner
        .activate(gandalf, original_activation)
        .accept_optional()
        .resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0, "pay Gandalf's six mana");
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
        "components must not add their own shuffles"
    );
    let (shuffle_index, shuffler) = shuffles[0];
    assert_eq!(shuffler, P0, "only the owner's library is shuffled");
    for component in [gandalf, gemrazer] {
        let deliveries: Vec<_> = outcome
            .events()
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                matches!(event, GameEvent::ZoneChanged {
                    object_id, to: Zone::Library, ..
                } if *object_id == component)
                .then_some(index)
            })
            .collect();
        assert_eq!(
            deliveries.len(),
            1,
            "each physical card reaches the library"
        );
        assert!(
            deliveries[0] < shuffle_index,
            "both cards arrive before the shuffle"
        );
        // Either card may be drawn by the rest of the printed instruction.
        assert_ne!(outcome.state().objects[&component].zone, Zone::Battlefield);
    }
    let draws: Vec<_> = outcome
        .events()
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::CardDrawn { player_id, .. } => Some((index, *player_id)),
            _ => None,
        })
        .collect();
    assert_eq!(draws.len(), 3, "the owner actually draws three cards");
    for (index, drawer) in draws {
        assert_eq!(drawer, P0);
        assert!(shuffle_index < index, "the one shuffle precedes every draw");
    }
    outcome.assert_counters(trickster, CounterType::Plus1Plus1, 1);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
}
