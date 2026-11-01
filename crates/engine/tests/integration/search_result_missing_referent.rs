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
use engine::types::zones::Zone;

const INVENTOR: &str = "Vigilance\nWhen this creature enters, you may search your library for an artifact card, reveal it, then shuffle. Put that card into your hand if you control an Assassin. Otherwise, put that card on top of your library.";
const VERDANT: &str = "Search your library for a basic land card and put it onto the battlefield tapped. Search your library and graveyard for a card named Nissa, Nature's Artisan, reveal it, put it into your hand, then shuffle.";
const BRIBERY: &str = "Search target opponent's library for a creature card and put that card onto the battlefield under your control. Then that player shuffles.";

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
                .act(GameAction::DecideOptionalCost { pay })
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
