//! Production death-trigger coverage for source-bound owner-library shuffles.

use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::StackEntryKind;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Full Oracle verified against https://api.scryfall.com/cards/named?exact=Alabaster%20Dragon
const ALABASTER_DRAGON: &str =
    "Flying\nWhen this creature dies, shuffle it into its owner's library.";
// Full Oracle verified against https://api.scryfall.com/cards/named?exact=Murder
const MURDER: &str = "Destroy target creature.";

#[test]
fn alabaster_dragon_death_follows_the_source_into_its_owners_shuffled_library() {
    for controller in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dragon = scenario
            .add_creature(P0, "Alabaster Dragon", 4, 4)
            .controlled_by(controller)
            .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
            .id();
        let other_dragon = scenario
            .add_creature(P0, "Alabaster Dragon", 4, 4)
            .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
            .id();
        let unrelated_graveyard_card = scenario
            .add_creature_to_graveyard(P0, "Unrelated Graveyard Card", 1, 1)
            .id();
        scenario.with_library_top(P0, &["Owner A", "Owner B", "Owner C", "Owner D"]);
        scenario.with_library_top(P1, &["Opponent A", "Opponent B", "Opponent C"]);
        let murder = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let mut runner = scenario.build();
        assert_eq!(runner.state().objects[&dragon].owner, P0);
        assert_eq!(runner.state().objects[&dragon].controller, controller);
        let opponent_library = runner.state().players[P1.0 as usize].library.clone();

        // The public spell action creates the death event and the untargeted
        // trigger; no trigger or remembered successor is supplied by the fixture.
        let outcome = runner.cast(murder).target_object(dragon).resolve();

        // CR 400.7e: the zone-change trigger finds the new object in the public
        // graveyard. Both transitions must occur, not a direct battlefield return.
        let dragon_transitions: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from,
                    to,
                    ..
                } if *object_id == dragon => Some((*from, *to)),
                _ => None,
            })
            .collect();
        assert_eq!(
            dragon_transitions,
            vec![
                (Some(Zone::Battlefield), Zone::Graveyard),
                (Some(Zone::Graveyard), Zone::Library),
            ],
            "controller={controller:?}; waiting={:?}; stack={}",
            outcome.final_waiting_for(),
            outcome.state().stack.len()
        );
        outcome.assert_zone(&[dragon], Zone::Library);
        outcome.assert_zone(&[other_dragon], Zone::Battlefield);
        outcome.assert_zone(&[unrelated_graveyard_card, murder], Zone::Graveyard);

        let state = outcome.state();
        // CR 400.3: the source goes to its owner's library even when the
        // controller of its death trigger is the opponent.
        assert!(state.players[P0.0 as usize].library.contains(&dragon));
        assert!(!state.players[P0.0 as usize].graveyard.contains(&dragon));
        assert_eq!(state.players[P0.0 as usize].library.len(), 5);
        assert_eq!(state.players[P1.0 as usize].library, opponent_library);

        // CR 701.24a + CR 701.24c: moving the source alone is not completion;
        // the engine's shuffle-action authority must report the owner's shuffle.
        let shuffled_players: Vec<_> = outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } => Some(*player_id),
                _ => None,
            })
            .collect();
        assert!(
            !shuffled_players.is_empty(),
            "the owner must actually shuffle"
        );
        assert!(shuffled_players.iter().all(|player| *player == P0));

        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
        assert!(state.deferred_triggers.is_empty());
        assert!(state.pending_trigger.is_none());
        assert!(state.pending_trigger_order.is_none());
        assert!(state.resolving_stack_entry.is_none());
        assert!(state.pending_resolution_completion.is_none());
    }
}

#[test]
fn ceased_stolen_dragon_token_still_shuffles_its_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Rite of Replication's verified Oracle creates a token copy of a target
    // creature. Seed that copy with Alabaster Dragon's full printed abilities.
    let dragon = scenario
        .add_creature(P0, "Alabaster Dragon", 4, 4)
        .controlled_by(P1)
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    scenario.with_library_top(P0, &["Owner A", "Owner B", "Owner C"]);
    scenario.with_library_top(P1, &["Controller A", "Controller B", "Controller C"]);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&dragon)
        .unwrap()
        .is_token = true;
    let controller_library = runner.state().players[P1.0 as usize].library.clone();
    let outcome = runner.cast(murder).target_object(dragon).resolve();

    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            ..
        } if *object_id == dragon
    )));
    // CR 111.7 + CR 111.8: the token ceases before its trigger resolves and
    // cannot move from the graveyard into a library.
    let state = outcome.state();
    assert!(!state.objects.contains_key(&dragon));
    assert_eq!(state.players[P0.0 as usize].library.len(), 3);
    assert!(!state.players[P0.0 as usize].library.contains(&dragon));
    assert_eq!(state.players[P1.0 as usize].library, controller_library);

    // CR 111.2 + CR 701.24c: ownership survives the source row's disappearance;
    // the obligation is still the token creator's shuffle, not its controller's.
    let shuffled_players: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(shuffled_players, vec![P0]);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

// CR 400.7e + CR 730.3c: a merged permanent that dies is one departure, and the
// dies trigger's "shuffle it" finds every object that merged permanent became.
// Both the survivor and its absorbed component must arrive before the one shuffle.
#[test]
fn alabaster_dragon_death_shuffles_the_whole_merged_pile_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature(P0, "Alabaster Dragon", 4, 4)
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    let component = scenario.add_creature(P0, "Gemrazer", 4, 4).id();
    scenario.with_library_top(P0, &["Owner A", "Owner B", "Owner C"]);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        // The mutating spell resolved off the stack, never listed on the battlefield.
        state.battlefield.retain(|&id| id != component);
        let mut merge_events = vec![];
        merge_object_onto(state, component, dragon, MergeSide::Top, &mut merge_events);
    }

    let outcome = runner.cast(murder).target_object(dragon).resolve();

    // Reach guard: the trigger fired from the merged permanent's death, and the
    // component split out of that same departure.
    outcome.assert_zone(&[dragon, component], Zone::Library);
    let state = outcome.state();
    assert!(state.players[P0.0 as usize].library.contains(&dragon));
    assert!(
        state.players[P0.0 as usize].library.contains(&component),
        "the absorbed component is part of the object the trigger found"
    );
    let shuffled_players: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shuffled_players,
        vec![P0],
        "the pile shuffles exactly once into its owner's library"
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

// CR 400.7e + CR 730.3c: an activated "return this card from your graveyard"
// names the object that activated it, not the departure that split a former
// merged permanent. Its split sibling must stay in the graveyard.
#[test]
fn activated_graveyard_return_does_not_sweep_the_former_merge_sibling() {
    const SKELETON: &str =
        "{1}{B}: Return this card from your graveyard to the battlefield tapped.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let skeleton = scenario
        .add_creature(P0, "Reassembling Skeleton", 1, 1)
        .from_oracle_text(SKELETON)
        .id();
    let component = scenario.add_creature(P0, "Migratory Greathorn", 3, 4).id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        merge_object_onto(state, component, skeleton, MergeSide::Top, &mut events);
        engine::game::zones::move_to_zone(state, skeleton, Zone::Graveyard, &mut events);
    }
    assert_eq!(runner.state().objects[&skeleton].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&component].zone, Zone::Graveyard);
    assert!(runner.state().objects[&component]
        .split_from_merge_survivor
        .is_some());

    let index = runner.state().objects[&skeleton]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("the graveyard source keeps its activated return");
    runner
        .act(GameAction::ActivateAbility {
            source_id: skeleton,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the graveyard self-return is activatable");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&skeleton].zone, Zone::Battlefield);
    assert!(runner.state().objects[&skeleton].tapped);
    assert_eq!(
        runner.state().objects[&component].zone,
        Zone::Graveyard,
        "a non-triggered self-return has no recorded departure to expand"
    );
}

// CR 400.7e + CR 730.3c + CR 609.3: responding to the dies trigger by exiling the
// survivor alone must not erase the sibling that is still the same public-zone
// successor. The effect acts on the successor it can still find.
#[test]
fn alabaster_death_after_the_survivor_is_exiled_still_shuffles_the_sibling() {
    const CREMATE: &str = "Exile target card from a graveyard. Draw a card.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature(P0, "Alabaster Dragon", 4, 4)
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    let component = scenario.add_creature(P0, "Migratory Greathorn", 3, 4).id();
    scenario.with_library_top(P0, &["Owner A", "Owner B", "Owner C"]);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let cremate = scenario
        .add_spell_to_hand_from_oracle(P0, "Cremate", true, CREMATE)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        merge_object_onto(state, component, dragon, MergeSide::Top, &mut events);
    }

    // Destroy the merged permanent, then stop as soon as its dies trigger is the
    // top of the stack so the response window is available.
    runner.cast(murder).target_object(dragon).commit();
    let top_is_trigger = |runner: &engine::game::scenario::GameRunner| {
        runner
            .state()
            .stack
            .last()
            .is_some_and(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
    };
    let mut events = Vec::new();
    for _ in 0..16 {
        if top_is_trigger(&runner) {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve the removal")
                    .events,
            );
        } else {
            break;
        }
    }
    assert_eq!(
        runner.state().objects[&dragon].zone,
        Zone::Graveyard,
        "reach guard: the removal put the merged permanent into the graveyard; {:?}",
        runner.state().waiting_for
    );
    assert!(
        top_is_trigger(&runner),
        "reach guard: the dies trigger is on the stack"
    );

    // Respond by exiling only the survivor.
    runner.cast(cremate).target_object(dragon).commit();
    for _ in 0..24 {
        if runner.state().stack.is_empty() {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve the response and the trigger")
                    .events,
            );
        } else {
            break;
        }
    }
    let state = runner.state();
    assert_eq!(
        state.objects[&dragon].zone,
        Zone::Exile,
        "Cremate moved the survivor out of the public zone it departed into"
    );
    assert_eq!(
        state.objects[&component].zone,
        Zone::Library,
        "the still-findable successor receives the trigger's action"
    );
    assert!(state.players[P0.0 as usize].library.contains(&component));
    assert!(!state.players[P0.0 as usize].library.contains(&dragon));

    // CR 701.24c: the library is shuffled even though the survivor itself is no
    // longer in the zone it was expected in. Exactly one owner shuffle, after the
    // component actually arrived.
    let arrival = events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Library,
                    ..
                } if *object_id == component
            )
        })
        .expect("the trigger moves the surviving component into the library");
    let shuffles: Vec<_> = events
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
        "the owner shuffles exactly once, after the delivery: {shuffles:?}"
    );
    assert_eq!(shuffles[0].1, P0);
    assert!(
        shuffles[0].0 > arrival,
        "the shuffle must follow the component's library arrival"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(state.deferred_triggers.is_empty());
}

// Full Oracle verified against https://api.scryfall.com/cards/named?exact=Avenging%20Angel
const AVENGING_ANGEL: &str =
    "Flying\nWhen this creature dies, you may put it on top of its owner's library.";

/// CR 400.7e + CR 701.24a control: a recorded merged departure widens only the
/// move component of a terminal shuffle (`LibraryShuffleMode::TerminalShuffle`).
/// An ordered "put it on top" keeps the base placement: the dies trigger places
/// the one object it found and the absorbed component stays where the split
/// left it. Placing every component in an owner-chosen order (CR 730.3a +
/// CR 730.3c) is not implemented for this path (known gap, #10759).
#[test]
fn merged_ordered_placement_keeps_the_single_chosen_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let angel = scenario
        .add_creature(P0, "Avenging Angel", 3, 3)
        .from_oracle_text_with_keywords(&["Flying"], AVENGING_ANGEL)
        .id();
    let component = scenario.add_creature(P0, "Gemrazer", 2, 2).id();
    scenario.with_library_top(P0, &["Top A", "Top B"]);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        merge_object_onto(state, component, angel, MergeSide::Top, &mut events);
    }

    runner
        .cast(murder)
        .target_object(angel)
        .accept_optional()
        .resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&angel].zone, Zone::Library);
    assert_eq!(
        state.players[P0.0 as usize].library[0], angel,
        "the survivor is the single chosen card of the ordered placement"
    );
    assert!(
        !state.players[P0.0 as usize].library.contains(&component),
        "the absorbed component is not swept into an ordered placement"
    );
    assert_eq!(
        state.objects[&component].zone,
        Zone::Graveyard,
        "the component stays where the split left it"
    );
    assert_eq!(
        state.players[P0.0 as usize].library.len(),
        3,
        "exactly one card was added to the two-card library"
    );
}

/// SYNTHETIC: Avenging Angel's printed trigger followed by an unrelated
/// "Then shuffle your library." No printed card pairs a recorded merged
/// departure's ordered placement with a later shuffle; Avenging Angel is the
/// nearest printed analogue. CR 701.24a: the later instruction shuffles the
/// controller's library on its own; it does not make the ordered placement the
/// move component of a terminal shuffle. Only the move's own
/// `LibraryShuffleMode` widens a departure (CR 730.3c), so the component stays
/// in the graveyard and the shuffle happens once.
const ANGEL_THEN_UNRELATED_SHUFFLE: &str = "Flying\nWhen this creature dies, you may put it on top of its owner's library. Then shuffle your library.";

#[test]
fn unrelated_later_shuffle_does_not_widen_an_ordered_departure_placement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let angel = scenario
        .add_creature(P0, "Synthetic Shuffling Angel", 3, 3)
        .from_oracle_text_with_keywords(&["Flying"], ANGEL_THEN_UNRELATED_SHUFFLE)
        .id();
    let component = scenario.add_creature(P0, "Gemrazer", 2, 2).id();
    scenario.with_library_top(P0, &["Top A", "Top B"]);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        merge_object_onto(state, component, angel, MergeSide::Top, &mut events);
    }

    let outcome = runner
        .cast(murder)
        .target_object(angel)
        .accept_optional()
        .resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let state = outcome.state();
    assert!(state.stack.is_empty());
    assert_eq!(state.objects[&angel].zone, Zone::Library);
    assert_eq!(
        state.objects[&component].zone,
        Zone::Graveyard,
        "an unrelated later shuffle does not sweep the component into the placement"
    );
    assert_eq!(state.players[P0.0 as usize].library.len(), 3);
    let shuffles = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == P0
            )
        })
        .count();
    assert_eq!(shuffles, 1, "the printed shuffle still happens once");
}

// Oracle text from the repo's card data export.
const BUTCHER_GHOUL: &str = "Undying (When this creature dies, if it had no +1/+1 counters on it, return it to the battlefield under its owner's control with a +1/+1 counter on it.)";

/// Base-behavior control, not a rules claim. A recorded departure widens only
/// the move component of a terminal shuffle (CR 701.24a). Undying's return is
/// not one, so it keeps the merge-base population: the card the trigger names
/// returns and the absorbed component stays in the graveyard. CR 730.3c would
/// return each card; that gap predates this branch and is not claimed here.
/// This test passes unchanged at the merge base.
#[test]
fn undying_return_of_a_merged_permanent_keeps_the_base_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ghoul = scenario
        .add_creature(P0, "Butcher Ghoul", 1, 1)
        .from_oracle_text_with_keywords(&["Undying"], BUTCHER_GHOUL)
        .id();
    let component = scenario.add_creature(P0, "Gemrazer", 4, 4).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        merge_object_onto(state, component, ghoul, MergeSide::Top, &mut events);
    }

    let outcome = runner.cast(murder).target_object(ghoul).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    let state = outcome.state();
    assert!(state.stack.is_empty());
    assert_eq!(
        state.objects[&ghoul].zone,
        Zone::Battlefield,
        "reach guard: the undying trigger resolved and returned the card it names"
    );
    assert_eq!(
        state.objects[&component].zone,
        Zone::Graveyard,
        "a non-shuffle return keeps the merge-base population"
    );
}

// Full Oracle verified against https://api.scryfall.com/cards/named?exact=Tel-Jilad%20Stylus
const TEL_JILAD_STYLUS: &str = "{T}: Put target permanent you own on the bottom of your library.";

/// Base-behavior pin, not a rules claim (known gap, tracked in #10759). Tel-Jilad
/// Stylus is an ordinary placement, not a terminal shuffle, so this branch does
/// not change how a live merged permanent's cards are delivered: as at
/// `main`, each physical card (CR 730.3) is delivered at the library's zone
/// default, which shuffles the owner's library once per card, and no owner
/// arrangement (CR 730.3a) is offered. Only the move component of a terminal
/// shuffle carries its position to every component. The relative order of the
/// cards is not asserted. This test passes unchanged at the merge base.
#[test]
fn stylus_on_a_live_merged_permanent_keeps_the_base_component_delivery() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Existing A", "Existing B"]);
    let stylus = scenario
        .add_artifact_from_oracle(P0, "Tel-Jilad Stylus", TEL_JILAD_STYLUS)
        .id();
    let host = scenario.add_creature(P0, "Host Creature", 2, 2).id();
    let component = scenario.add_creature(P0, "Migratory Greathorn", 3, 4).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        merge_object_onto(state, component, host, MergeSide::Top, &mut events);
    }
    assert_eq!(runner.state().players[P0.0 as usize].library.len(), 2);

    let outcome = runner.activate(stylus, 0).target_object(host).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no arrangement prompt is offered: {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[host, component], Zone::Library);
    let state = outcome.state();
    assert_eq!(state.players[P0.0 as usize].library.len(), 4);
    let shuffled_players: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shuffled_players,
        vec![P0, P0],
        "each card takes the zone-default delivery, as at the merge base"
    );
    assert!(state.stack.is_empty());
}

// Oracle text from the repo's card data export.
const DEBT_TO_THE_KAMI: &str = "Choose one —\n• Target opponent exiles a creature they control.\n• Target opponent exiles an enchantment they control.";

/// Base-behavior pin, not a rules claim. Debt to the Kami's exile is a
/// resolution-time choice that is not a terminal shuffle, so this branch does
/// not change its candidate population: as on `main`, the scan still offers
/// the merged creature's absorbed component beside the permanent itself
/// (CR 730.2 would offer only the permanent; tracked with the merged-arrival
/// work in #10759). Only a terminal-shuffle move's scan excludes absorbed
/// components. This test passes unchanged at the merge base.
#[test]
fn debt_to_the_kami_scan_of_a_merged_creature_keeps_the_base_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Host Creature", 2, 2).id();
    let component = scenario.add_creature(P1, "Migratory Greathorn", 3, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Debt to the Kami", true, DEBT_TO_THE_KAMI)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = vec![];
        merge_object_onto(state, component, host, MergeSide::Top, &mut events);
    }

    runner.cast(spell).modes(&[0]).target_player(P1).commit();
    let mut passes = 0;
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes until the exile choice");
        passes += 1;
        assert!(passes < 6, "the exile choice must surface");
    }
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "the targeted opponent chooses");
            let mut offered = cards.clone();
            offered.sort_by_key(|id| id.0);
            let mut expected = vec![host, component];
            expected.sort_by_key(|id| id.0);
            assert_eq!(offered, expected, "the merge-base population is offered");
        }
        other => panic!("expected the exile choice, got {other:?}"),
    }
}
