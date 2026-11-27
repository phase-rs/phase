use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::game::zones::create_object;
use engine::types::ability::{EffectKind, SearchSelectionConstraint};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const INVENTOR: &str = "Vigilance\nWhen this creature enters, you may search your library for an artifact card, reveal it, then shuffle. Put that card into your hand if you control an Assassin. Otherwise, put that card on top of your library.";
const VERDANT: &str = "Search your library for a basic land card and put it onto the battlefield tapped. Search your library and graveyard for a card named Nissa, Nature's Artisan, reveal it, put it into your hand, then shuffle.";
const BRIBERY: &str = "Search target opponent's library for a creature card and put that card onto the battlefield under your control. Then that player shuffles.";
const ORESKOS: &str = "When this creature enters, search your library for up to X Plains cards, where X is the number of players who control more lands than you. Reveal those cards, put them into your hand, then shuffle.";
const PRISMATIC: &str = "Vivid — When this enchantment enters, search your library for up to X basic land cards, where X is the number of colors among permanents you control. Reveal those cards, put them into your hand, then shuffle.\nYou may play an additional land on each of your turns.";

#[test]
fn oreskos_explorer_delivers_selected_plains_once() {
    for (response, cap, pick) in [(true, 2, 2), (false, 1, 0), (false, 0, 0)] {
        let mut scenario = GameScenario::new_n_player(4, 217);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand_from_oracle(P0, "Oreskos Explorer", 2, 2, ORESKOS)
            .with_mana_cost(ManaCost::zero())
            .id();
        let opposing_land = (cap != 0).then(|| scenario.add_basic_land(P1, ManaColor::Blue));
        let response = response.then(|| {
            for (player, lands) in [(PlayerId(2), 2), (PlayerId(3), 3)] {
                for _ in 0..lands {
                    scenario.add_basic_land(player, ManaColor::Blue);
                }
            }
            let spell = scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Boomerang",
                    true,
                    "Return target permanent to its owner's hand.",
                )
                .with_mana_cost(ManaCost::zero())
                .id();
            (
                spell,
                opposing_land.expect("Boomerang has an opposing land"),
            )
        });
        let nonbasic = scenario
            .add_land_to_library_top(P0, "Nonbasic Plains")
            .with_subtypes(vec!["Plains", "Island"])
            .id();
        let cover = scenario
            .add_land_to_library_top(P0, "Island cover")
            .with_subtypes(vec!["Island"])
            .id();
        let plains = [
            scenario.add_basic_land(P0, ManaColor::White),
            scenario.add_basic_land(P0, ManaColor::White),
        ];
        let mut runner = scenario.build();
        move_test_basics_to_library(&mut runner, &plains);
        let candidates = [nonbasic, plains[0], plains[1]];
        let events = finish_plural_search(&mut runner, source, response, cap, &candidates, pick);
        assert_selected_delivery(&runner, source, &events, &candidates[..pick]);
        for card in &candidates[pick..] {
            assert_eq!(runner.state().objects[card].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
    }
}

#[test]
fn prismatic_undercurrents_delivers_selected_basics_once() {
    for (respond, have_basics, pick) in [(true, true, 2), (false, true, 0), (false, false, 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_enchantment_to_hand_from_oracle(P0, "Prismatic Undercurrents", PRISMATIC)
            .with_mana_cost(ManaCost::zero())
            .with_color(vec![ManaColor::Green])
            .id();
        scenario
            .add_creature(P0, "White and blue permanent", 1, 1)
            .with_color(vec![ManaColor::White, ManaColor::Blue]);
        scenario
            .add_creature(P0, "Second blue permanent", 1, 1)
            .with_color(vec![ManaColor::Blue]);
        scenario.add_basic_land(P0, ManaColor::Green);
        let response = respond.then(|| {
            let red = scenario
                .add_creature(P0, "Unique red permanent", 1, 1)
                .with_color(vec![ManaColor::Red])
                .id();
            let unsummon = scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Unsummon",
                    true,
                    "Return target creature to its owner's hand.",
                )
                .with_mana_cost(ManaCost::zero())
                .id();
            (unsummon, red)
        });
        let nonbasic = scenario
            .add_land_to_library_top(P0, "Nonbasic Plains")
            .with_subtypes(vec!["Plains", "Island"])
            .id();
        let basics = if have_basics {
            [
                ManaColor::White,
                ManaColor::Blue,
                ManaColor::Red,
                ManaColor::Green,
            ]
            .map(|color| scenario.add_basic_land(P0, color))
            .to_vec()
        } else {
            Vec::new()
        };
        let mut runner = scenario.build();
        move_test_basics_to_library(&mut runner, &basics);
        let events = finish_plural_search(&mut runner, source, response, 3, &basics, pick);
        assert_selected_delivery(&runner, source, &events, &basics[..pick]);
        for card in &basics[pick..] {
            assert_eq!(runner.state().objects[card].zone, Zone::Library);
        }
        assert_eq!(runner.state().objects[&nonbasic].zone, Zone::Library);
    }
}

fn move_test_basics_to_library(runner: &mut GameRunner, cards: &[ObjectId]) {
    for card in cards {
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(*card, Zone::Library),
            &mut Vec::new(),
        ));
    }
}

fn finish_plural_search(
    runner: &mut GameRunner,
    source: ObjectId,
    response: Option<(ObjectId, ObjectId)>,
    cap: usize,
    candidates: &[ObjectId],
    pick: usize,
) -> Vec<GameEvent> {
    runner.cast(source).commit();
    let player_count = runner.state().players.len();
    for _ in 0..player_count {
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().stack.len(), 1, "the ETB is on the stack");
    if let Some((spell, target)) = response {
        runner.cast(spell).target_object(target).commit();
        assert_eq!(runner.state().stack.len(), 2);
        for _ in 0..player_count {
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().objects[&target].zone, Zone::Hand);
        assert_eq!(runner.state().stack.len(), 1, "the response leaves the ETB");
    }
    let mut events = Vec::new();
    for _ in 0..player_count {
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    if !candidates.is_empty() {
        let WaitingFor::SearchChoice {
            player,
            library_owner,
            cards,
            count,
            up_to,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("eligible cards must open the search prompt");
        };
        assert_eq!(
            (*player, *library_owner, *count, *up_to),
            (P0, Some(P0), cap, true)
        );
        let mut actual = cards.clone();
        actual.sort_unstable();
        let mut expected = candidates.to_vec();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: candidates[..pick].to_vec(),
                })
                .unwrap()
                .events,
        );
    }
    assert_complete(runner);
    events
}

fn assert_selected_delivery(
    runner: &GameRunner,
    source: ObjectId,
    events: &[GameEvent],
    selected: &[ObjectId],
) {
    let search_actions: Vec<_> = events.iter().enumerate().filter_map(|(index, event)| match event {
        GameEvent::PlayerPerformedAction {
            player_id,
            action: action @ (PlayerActionKind::SearchedLibrary | PlayerActionKind::ShuffledLibrary),
            ..
        } => Some((index, *player_id, action)),
        _ => None,
    }).collect();
    let [(search, P0, PlayerActionKind::SearchedLibrary), (shuffle, P0, PlayerActionKind::ShuffledLibrary)] =
        search_actions.as_slice()
    else {
        panic!("controller must search and then shuffle: {search_actions:?}");
    };
    let reveals: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::CardsRevealed {
                player, card_ids, ..
            } => Some((index, *player, card_ids)),
            _ => None,
        })
        .collect();
    let deliveries: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Hand,
                ..
            } => Some((index, *object_id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        deliveries.iter().map(|(_, id)| *id).collect::<Vec<_>>(),
        selected
    );
    if selected.is_empty() {
        assert!(reveals.is_empty());
        return;
    }
    let [(reveal, P0, revealed)] = reveals.as_slice() else {
        panic!("one public reveal: {reveals:?}");
    };
    assert_eq!(revealed.as_slice(), selected);
    assert!(search < reveal);
    for (index, card) in deliveries {
        assert!(reveal < &index && &index < shuffle);
        assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    }
    assert_eq!(
        events[*reveal + 1..*shuffle].iter().filter(|event| matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::ChangeZone, source_id, .. } if *source_id == source
        )).count(),
        1,
        "exactly one delivery dispatch, including any empty repeated attempts",
    );
}

fn assert_complete(runner: &GameRunner) {
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    assert!(state.pending_search_found_batch.is_none());
    assert!(state.active_library_searches.is_empty());
    assert!(state.active_search_decision_controls.is_empty());
}

/// CR 701.23b + CR 608.2c + CR 609.3: An accepted search's empty result
/// supplies no object for either destination. The old set is deliberately
/// injected stored state, not a claim about an earlier printed producer action.
#[test]
fn inventor_empty_and_found_searches_bind_only_the_delivered_artifact() {
    for (candidate, assassin, choose_found) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (true, true, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let inventor = scenario
            .add_creature_to_hand_from_oracle(P0, "Loyal Inventor", 2, 3, INVENTOR)
            .with_mana_cost(ManaCost::zero())
            .id();
        if assassin {
            scenario
                .add_creature(P0, "Assassin fixture", 1, 1)
                .with_subtypes(vec!["Assassin"]);
        }
        let cover = scenario.add_land_to_library_top(P0, "Covering Forest").id();
        let mut runner = scenario.build();
        let artifact = create_object(
            runner.state_mut(),
            CardId(99001),
            P0,
            "Lower search candidate".into(),
            Zone::Library,
        );
        if candidate {
            let object = runner.state_mut().objects.get_mut(&artifact).unwrap();
            object.card_types.core_types.push(CoreType::Artifact);
            object.base_card_types = object.card_types.clone();
        }
        assert_eq!(runner.state().players[0].library.front(), Some(&cover));
        assert_ne!(cover, artifact);
        let stale = create_object(
            runner.state_mut(),
            CardId(99002),
            P0,
            "Unrelated earlier result".into(),
            Zone::Exile,
        );
        runner
            .state_mut()
            .tracked_object_sets
            .insert(TrackedSetId(991), vec![stale]);
        runner.state_mut().next_tracked_set_id = 992;
        runner.cast(inventor).commit();
        let mut events = Vec::new();
        // Advance only priority; the optional ETB response must be explicit.
        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap()
                .events,
        );
        if candidate {
            let WaitingFor::SearchChoice { player, cards, .. } = &runner.state().waiting_for else {
                panic!("accepted search must offer the artifact");
            };
            assert_eq!(*player, P0);
            assert_eq!(cards, &vec![artifact]);
            let paused_continuation = runner
                .state()
                .active_ability_continuation()
                .expect("the search parks its printed destination")
                .clone();
            assert_eq!(paused_continuation.chain.source_id, inventor);
            assert!(!runner.state().active_library_searches.is_empty());
            assert!(!runner.state().active_search_decision_controls.is_empty());
            let paused_searches = runner.state().active_library_searches.clone();
            let paused_controls = runner.state().active_search_decision_controls.clone();
            // Exercise the actual paused-search persistence boundary before resume.
            let saved = serde_json::to_string(runner.state()).unwrap();
            runner = GameRunner::from_state(serde_json::from_str(&saved).unwrap());
            let WaitingFor::SearchChoice { player, cards, .. } = &runner.state().waiting_for else {
                panic!("restored search must preserve the actual prompt");
            };
            assert_eq!(*player, P0);
            assert_eq!(cards, &vec![artifact]);
            assert_eq!(
                runner.state().active_ability_continuation(),
                Some(&paused_continuation)
            );
            assert_eq!(runner.state().active_library_searches, paused_searches);
            assert_eq!(
                runner.state().active_search_decision_controls,
                paused_controls
            );
            assert!(runner.state().pending_search_found_batch.is_none());
            events.extend(
                runner
                    .act(GameAction::SelectCards {
                        cards: if choose_found { vec![artifact] } else { vec![] },
                    })
                    .unwrap()
                    .events,
            );
        } else {
            assert!(!matches!(
                runner.state().waiting_for,
                WaitingFor::SearchChoice { .. }
            ));
        }
        assert_complete(&runner);
        assert_eq!(runner.state().objects[&inventor].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert_eq!(
            runner.state().objects[&artifact].zone,
            if choose_found {
                Zone::Hand
            } else {
                Zone::Library
            }
        );
        assert_eq!(runner.state().objects[&stale].zone, Zone::Exile);
        assert_eq!(
            runner.state().tracked_object_sets[&TrackedSetId(991)],
            vec![stale]
        );
        assert_eq!(
            runner.state().players[0]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            if choose_found { vec![artifact] } else { vec![] }
        );
        let set = runner
            .state()
            .chain_tracked_set_id
            .expect("ordinary result binds a fresh set");
        assert_ne!(set, TrackedSetId(991));
        assert_eq!(
            runner.state().tracked_object_sets[&set],
            if choose_found { vec![artifact] } else { vec![] }
        );
        assert!(events.iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::SearchLibrary, source_id, .. } if *source_id == inventor)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                player_id: P0,
                action: PlayerActionKind::SearchedLibrary,
                ..
            }
        )));
        let shuffle = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    GameEvent::PlayerPerformedAction {
                        player_id: P0,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
            })
            .expect("accepted search shuffles");
        let destination = events.iter().position(|e| matches!(e, GameEvent::EffectResolved { kind, source_id, .. } if *source_id == inventor && *kind == if assassin { EffectKind::ChangeZone } else { EffectKind::PutAtLibraryPosition })).expect("the printed destination is reached");
        assert!(shuffle < destination);
        let revealed: Vec<ObjectId> = events
            .iter()
            .filter_map(|e| {
                if let GameEvent::CardsRevealed { card_ids, .. } = e {
                    Some(card_ids.clone())
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        assert_eq!(revealed, if choose_found { vec![artifact] } else { vec![] });
        if choose_found {
            let moved = events.iter().position(|e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == artifact)).unwrap();
            assert!(shuffle < moved);
        }
    }
}

/// CR 608.2c + CR 701.23a: The second search cannot reuse the first land.
#[test]
fn verdant_crescendo_second_empty_search_preserves_the_first_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Verdant Crescendo", false, VERDANT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::pregame(land, Zone::Library),
        &mut Vec::new(),
    ));
    assert_eq!(
        runner.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![land]
    );
    assert_eq!(runner.state().objects[&land].zone, Zone::Library);
    for types in [
        &runner.state().objects[&land].card_types,
        &runner.state().objects[&land].base_card_types,
    ] {
        assert!(types.supertypes.contains(&Supertype::Basic));
        assert!(types.core_types.contains(&CoreType::Land));
        assert_eq!(types.subtypes, vec!["Forest"]);
    }
    let outcome = runner.cast(spell).resolve();
    let WaitingFor::SearchChoice {
        player: P0, cards, ..
    } = outcome.final_waiting_for()
    else {
        panic!("first search must prompt");
    };
    assert_eq!(cards, &vec![land]);
    let mut events = outcome.events().to_vec();
    let result = runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .unwrap();
    events.extend(result.events);
    assert_complete(&runner);
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(runner.state().objects[&land].tapped);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(runner.state().players[0].hand.is_empty());
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::SearchLibrary,
                    source_id,
                    ..
                } if *source_id == spell
            ))
            .count(),
        2
    );
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::PlayerPerformedAction {
            action: PlayerActionKind::ShuffledLibrary,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::CardsRevealed { .. })),
        "neither Verdant search has a result it may reveal here: {events:?}"
    );
    assert!(!events.iter().any(|e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == land)));
}

/// CR 608.2c + CR 701.23b: Missing objects do not cancel a named player's shuffle.
#[test]
fn bribery_empty_search_keeps_the_opponents_shuffle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bribery", false, BRIBERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let creature = scenario
        .add_spell_to_library_top(P1, "Opponent creature", false)
        .as_creature()
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice {
            player: P0,
            library_owner: Some(P1),
            ..
        }
    ));
    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .unwrap();
    assert_complete(&runner);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Library);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::PlayerPerformedAction {
            player_id: P1,
            action: PlayerActionKind::ShuffledLibrary,
            ..
        }
    )));
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: EffectKind::ChangeZone,
            ..
        }
    )));
}

/// CR 800.4a + CR 608.2c: An eliminated zone owner's stale candidates cannot deliver.
#[test]
fn bribery_search_owner_concession_settles_the_spell() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bribery", false, BRIBERY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let creature = scenario
        .add_spell_to_library_top(P1, "Departing owner's creature", false)
        .as_creature()
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::SearchChoice {
            player: P0,
            library_owner: Some(P1),
            ..
        }
    ));
    let result = runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    assert_complete(&runner);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&creature].owner, P1);
    assert!(result.events.iter().any(|e| matches!(e,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Exile, record }
            if *object_id == creature && record.owner == P1
    )));
    assert!(!result.events.iter().any(|e| matches!(e,
        GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == creature
    )));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, GameEvent::PlayerEliminated { player_id: P1, .. })));
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::EffectResolved {
            kind: EffectKind::ChangeZone,
            ..
        }
    )));
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, GameEvent::CardsRevealed { .. })));
}

/// CR 701.20a + CR 701.23b: the separate reveal names only selected lands.
#[test]
fn shard_convergence_separate_reveal_binds_the_search_result() {
    for choose_found in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Shard Convergence", false,
            "Search your library for a Plains card, an Island card, a Swamp card, and a Mountain card. Reveal those cards, put them into your hand, then shuffle.")
            .with_mana_cost(ManaCost::zero()).id();
        let land = scenario.add_basic_land(P0, ManaColor::Blue);
        let cover = scenario
            .add_spell_to_library_top(P0, "Unmatched cover", false)
            .id();
        let mut runner = scenario.build();
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(land, Zone::Library),
            &mut Vec::new()
        ));
        assert_eq!(runner.state().players[0].library.front(), Some(&cover));
        for types in [
            &runner.state().objects[&land].card_types,
            &runner.state().objects[&land].base_card_types,
        ] {
            assert!(types.supertypes.contains(&Supertype::Basic));
            assert!(types.core_types.contains(&CoreType::Land));
            assert_eq!(types.subtypes, vec!["Island"]);
        }
        let outcome = runner.cast(spell).resolve();
        let WaitingFor::SearchChoice {
            player: P0,
            cards,
            count,
            constraint,
            reveal,
            allows_partial_find,
            ..
        } = outcome.final_waiting_for()
        else {
            panic!("Shard must offer its matching land");
        };
        assert_eq!(cards, &vec![land]);
        assert_eq!(*count, 1, "public count is capped by the sole candidate");
        let SearchSelectionConstraint::MatchEachFilter { filters } = constraint else {
            panic!("Shard retains its per-land-type selection constraint");
        };
        assert_eq!(filters.len(), 4);
        assert!(*reveal);
        assert!(*allows_partial_find);
        let mut events = outcome.events().to_vec();
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: if choose_found { vec![land] } else { vec![] },
                })
                .unwrap()
                .events,
        );
        assert_complete(&runner);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert_eq!(
            runner.state().objects[&land].zone,
            if choose_found {
                Zone::Hand
            } else {
                Zone::Library
            }
        );
        assert_eq!(
            runner.state().players[0]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            if choose_found { vec![land] } else { vec![] }
        );
        assert!(events.iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::ChangeZone, source_id, .. } if *source_id == spell)));
        let reveals: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            reveals,
            if choose_found {
                vec![vec![land]]
            } else {
                vec![]
            }
        );
        let shuffle = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    GameEvent::PlayerPerformedAction {
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
            })
            .expect("printed shuffle completes");
        assert!(events.iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::SearchLibrary, source_id, .. } if *source_id == spell)));
        if choose_found {
            let reveal = events
                .iter()
                .position(|e| matches!(e, GameEvent::CardsRevealed { .. }))
                .unwrap();
            let delivery = events.iter().position(|e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == land)).unwrap();
            assert!(reveal < delivery && delivery < shuffle);
        }
    }
}

/// CR 118.8b + CR 701.59a + CR 701.59c: the public cost selects the search;
/// either alternative reveals its own result before delivery and shuffle.
#[test]
fn analyze_the_pollen_folded_searches_reveal_their_own_result() {
    for pay in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Analyze the Pollen", false,
            "As an additional cost to cast this spell, you may collect evidence 8. (Exile cards with total mana value 8 or greater from your graveyard.)\nSearch your library for a basic land card. If evidence was collected, instead search your library for a creature or land card. Reveal that card, put it into your hand, then shuffle.")
            .with_mana_cost(ManaCost::zero()).id();
        let evidence = scenario
            .add_spell_to_graveyard(P0, "Evidence value eight", false)
            .with_mana_cost(ManaCost::generic(8))
            .id();
        let creature = scenario
            .add_spell_to_library_top(P0, "Creature alternative", false)
            .as_creature()
            .id();
        let cover = scenario
            .add_spell_to_library_top(P0, "Unmatched cover", false)
            .id();
        let land = scenario.add_basic_land(P0, ManaColor::Green);
        let mut runner = scenario.build();
        assert!(!move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::pregame(land, Zone::Library),
            &mut Vec::new()
        ));
        assert_eq!(runner.state().players[0].library.front(), Some(&cover));
        let card_id = runner.state().objects[&spell].card_id;
        let mut events = runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap()
            .events;
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { player: P0, .. }
        ));
        events.extend(
            runner
                .act(GameAction::DecideOptionalCost {
                    index: if pay { 0 } else { 1 },
                })
                .unwrap()
                .events,
        );
        if pay {
            let WaitingFor::CollectEvidenceChoice {
                player: P0,
                minimum_mana_value,
                cards,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("evidence must be charged publicly");
            };
            assert_eq!(*minimum_mana_value, 8);
            assert_eq!(cards, &vec![evidence]);
            events.extend(
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![evidence],
                    })
                    .unwrap()
                    .events,
            );
        }
        assert_eq!(
            runner.state().objects[&evidence].zone,
            if pay { Zone::Exile } else { Zone::Graveyard }
        );
        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        let WaitingFor::SearchChoice {
            player: P0,
            cards,
            reveal,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the chosen search must prompt");
        };
        assert!(*reveal);
        assert!(cards.contains(&land));
        assert_eq!(cards.contains(&creature), pay);
        assert!(!cards.contains(&cover));
        assert_eq!(cards.len(), if pay { 2 } else { 1 });
        let selected = if pay { creature } else { land };
        events.extend(
            runner
                .act(GameAction::SelectCards {
                    cards: vec![selected],
                })
                .unwrap()
                .events,
        );
        assert_complete(&runner);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&selected].zone, Zone::Hand);
        assert_eq!(runner.state().objects[&cover].zone, Zone::Library);
        assert_eq!(
            runner.state().objects[&if pay { land } else { creature }].zone,
            Zone::Library
        );
        assert_eq!(
            runner.state().players[0]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![selected]
        );
        assert!(events.iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::SearchLibrary, source_id, .. } if *source_id == spell)));
        assert!(events.iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::ChangeZone, source_id, .. } if *source_id == spell)));
        let reveals: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(reveals, vec![vec![selected]]);
        let reveal = events
            .iter()
            .position(|e| matches!(e, GameEvent::CardsRevealed { .. }))
            .unwrap();
        let delivery = events.iter().position(|e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == selected)).unwrap();
        let shuffle = events
            .iter()
            .position(|e| {
                matches!(
                    e,
                    GameEvent::PlayerPerformedAction {
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
            })
            .expect("printed shuffle completes");
        assert!(reveal < delivery && delivery < shuffle);
    }
}
