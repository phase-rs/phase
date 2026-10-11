use engine::game::companion::can_activate_companion;
use engine::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use engine::game::match_flow::{handle_choose_play_draw, handle_submit_sideboard};
use engine::game::scenario::GameRunner;
use engine::game::{apply, start_game_with_starting_player};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CompanionChoiceSource, CompanionDeclaration, CompanionRevealChoice, GameState, WaitingFor,
};
use engine::types::keywords::{CompanionCondition, Keyword};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::match_config::{DeckCardCount, MatchPhase};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{ObjectId, PlayerId};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn creature(name: &str, mv: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![],
                generic: mv,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string()],
            },
            ..Default::default()
        },
        count: 1,
    }
}

fn land(name: &str, count: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Land],
                subtypes: vec!["Plains".to_string()],
            },
            ..Default::default()
        },
        count,
    }
}

fn lurrus_companion() -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: "Lurrus of the Dream-Den".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![],
                generic: 3,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Cat".to_string(), "Nightmare".to_string()],
            },
            keywords: vec![Keyword::Companion(
                CompanionCondition::MaxPermanentManaValue(2),
            )],
            ..Default::default()
        },
        count: 1,
    }
}

fn gyruda_companion() -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: "Gyruda, Doom of Depths".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![],
                generic: 6,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Demon".to_string(), "Kraken".to_string()],
            },
            keywords: vec![Keyword::Companion(CompanionCondition::EvenManaValues)],
            ..Default::default()
        },
        count: 1,
    }
}

/// CR 702.139a: Limited matches support a designated companion loaded via
/// `PlayerDeckPayload.companion`. The companion is offered during production
/// pregame initialization via `start_game_with_starting_player`, declared through
/// `apply(GameAction::DeclareCompanion)`, and put to hand for {3} special action.
#[test]
fn limited_companion_match_launch_hydrates_dedicated_companion_and_allows_reveal_and_payment() {
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);

    let mut main_deck: Vec<DeckEntry> = Vec::new();
    // 23 creatures with mana value <= 2
    for i in 0..23 {
        main_deck.push(creature(&format!("Small Creature {i}"), 2));
    }
    // 17 lands
    main_deck.push(land("Plains", 17));

    let companion_entry = lurrus_companion();

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            companion: vec![companion_entry],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck,
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    // Verify deck pool loaded the dedicated companion for Limited format
    let p0_pool = state
        .deck_pools
        .iter()
        .find(|pool| pool.player == P0)
        .expect("P0 pool must exist");
    assert_eq!(p0_pool.registered_companion.len(), 1);
    assert_eq!(p0_pool.current_companion.len(), 1);
    assert_eq!(
        p0_pool.current_companion[0].card.name,
        "Lurrus of the Dream-Den"
    );

    // Production start_game_with_starting_player must offer Lurrus from Dedicated source
    let start_result = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start_result.waiting_for;

    let eligible = match &state.waiting_for {
        WaitingFor::CompanionReveal {
            player,
            eligible_companions,
        } => {
            assert_eq!(*player, P0);
            eligible_companions.clone()
        }
        other => panic!("expected CompanionReveal waiting state from start_game, got {other:?}"),
    };

    assert_eq!(eligible.len(), 1);
    assert_eq!(eligible[0].name, "Lurrus of the Dream-Den");
    assert_eq!(eligible[0].source, CompanionChoiceSource::Dedicated);

    // Reveal companion through production apply()
    let choice = CompanionDeclaration::Reveal(CompanionRevealChoice {
        name: "Lurrus of the Dream-Den".to_string(),
        source: CompanionChoiceSource::Dedicated,
    });
    let action_result = apply(&mut state, P0, GameAction::DeclareCompanion { choice })
        .expect("valid dedicated companion reveal via apply must succeed");

    // Companion must now be assigned to P0
    assert!(state.players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|c| c.card.card.name == "Lurrus of the Dream-Den" && !c.used));

    // Current companion pool is cleared after reveal
    let p0_pool_after = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    assert_eq!(p0_pool_after.current_companion.len(), 0);

    // Verify reveal event was emitted
    assert!(action_result.events.iter().any(|e| matches!(
        e,
        GameEvent::CompanionRevealed {
            player,
            card_name,
        } if *player == P0 && card_name == "Lurrus of the Dream-Den"
    )));

    // Setup precombat main phase with 3 mana in pool to pay {3}
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.priority_player = P0;
    state.active_player = P0;

    for _ in 0..3 {
        state.players[P0.0 as usize].mana_pool.add(ManaUnit::new(
            ManaType::White,
            ObjectId(999),
            false,
            vec![],
        ));
    }

    assert!(
        can_activate_companion(&state, P0),
        "P0 must be able to activate companion special action with 3 mana in pool"
    );

    let mut runner = GameRunner::from_state(state);
    let outcome = runner
        .act(GameAction::CompanionToHand)
        .expect("companion special action payment must succeed");

    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|c| c.used));

    assert!(runner.state().objects.values().any(|obj| {
        obj.owner == P0 && obj.zone == Zone::Hand && obj.name == "Lurrus of the Dream-Den"
    }));

    assert!(outcome.events.iter().any(|event| matches!(
        event,
        GameEvent::CompanionMovedToHand { player, card_name }
            if *player == P0 && card_name == "Lurrus of the Dream-Den"
    )));
}

/// CR 702.139a: If a Limited deck violates the companion's restriction (e.g.
/// Lurrus with a 3+ mana value permanent in the deck), the companion is loaded into
/// the deck pool with its keyword, but is rejected by pregame reveal check.
/// A legal companion deck (e.g. Gyruda with even-MV cards) is offered.
#[test]
fn limited_companion_match_launch_rejects_invalid_companion_condition() {
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);

    let mut invalid_main: Vec<DeckEntry> = Vec::new();
    // 22 creatures with mana value <= 2
    for i in 0..22 {
        invalid_main.push(creature(&format!("Small Creature {i}"), 2));
    }
    // 1 creature with mana value 3 (violates Lurrus restriction!)
    invalid_main.push(creature("Expensive Creature", 3));
    // 17 lands
    invalid_main.push(land("Plains", 17));

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: invalid_main.clone(),
            companion: vec![lurrus_companion()],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: invalid_main,
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    // Verify Lurrus was loaded into the deck pool and retains its Companion keyword
    let p0_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    assert_eq!(p0_pool.registered_companion.len(), 1);
    assert!(p0_pool.registered_companion[0]
        .card
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Companion(_))));

    // Production start_game_with_starting_player must NOT offer Lurrus
    let start_result = start_game_with_starting_player(&mut state, P0);
    match start_result.waiting_for {
        WaitingFor::CompanionReveal {
            eligible_companions,
            ..
        } => {
            assert!(
                !eligible_companions
                    .iter()
                    .any(|c| c.name == "Lurrus of the Dream-Den"),
                "Lurrus must not be offered when deck contains a 3-MV permanent"
            );
        }
        WaitingFor::MulliganDecision { .. } => {
            // No companion offered, advanced directly to mulligans
        }
        other => panic!("unexpected waiting state: {other:?}"),
    }
}

/// CR 702.139a: An unbacked companion declaration (e.g. declaring a companion not offered)
/// is rejected by `apply(GameAction::DeclareCompanion)`.
#[test]
fn limited_companion_match_launch_rejects_unbacked_companion_selection() {
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);

    let mut main_deck: Vec<DeckEntry> = Vec::new();
    for i in 0..23 {
        main_deck.push(creature(&format!("Small Creature {i}"), 2));
    }
    main_deck.push(land("Plains", 17));

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            companion: vec![], // No companion registered!
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck,
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    state.waiting_for = WaitingFor::CompanionReveal {
        player: P0,
        eligible_companions: vec![],
    };

    let forged_choice = CompanionDeclaration::Reveal(CompanionRevealChoice {
        name: "Lurrus of the Dream-Den".to_string(),
        source: CompanionChoiceSource::Dedicated,
    });

    let result = apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: forged_choice,
        },
    );
    assert!(
        result.is_err(),
        "unoffered/unbacked companion declaration must be rejected by apply"
    );
}

/// CR 100.4b + CR 702.139a/b: A single drafted companion copy is not duplicated
/// between dedicated companion and sideboard pool at launch, during reveal,
/// or across between-games BO3 sideboarding transitions.
/// When the single drafted copy moves into the main deck for Game 2, the production
/// ChoosePlayDraw/next-game rebuild conserves the 45-card pool (45 cards) and suppresses
/// the outside-game companion designation.
#[test]
fn test_one_drafted_copy_bo3_move_to_main_suppresses_outside_companion() {
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);

    let mut main_deck: Vec<DeckEntry> = Vec::new();
    // 23 creatures with even mana value (e.g. 2)
    for i in 0..23 {
        main_deck.push(creature(&format!("Even Creature {i}"), 2));
    }
    main_deck.push(land("Plains", 17));

    // Sideboard has 1 Gyruda + 4 Plains (constructed from 45-card pool minus 40 main deck cards)
    let sideboard = vec![gyruda_companion(), land("Plains", 4)];
    let companion = vec![gyruda_companion()];

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: sideboard.clone(),
            companion: companion.clone(),
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: vec![land("Plains", 5)],
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    let p0_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    // Registered companion has exactly 1 Gyruda
    assert_eq!(p0_pool.registered_companion.len(), 1);
    assert_eq!(
        p0_pool.registered_companion[0].card.name,
        "Gyruda, Doom of Depths"
    );

    // Dedicated companion was NETTED out of registered sideboard so total pool count is 45
    assert_eq!(
        p0_pool.registered_sideboard.len(),
        1,
        "Sideboard should only contain the 4 Plains after netting Gyruda"
    );
    assert_eq!(p0_pool.registered_sideboard[0].card.name, "Plains");
    assert_eq!(p0_pool.registered_sideboard[0].count, 4);

    let total_registered_cards: u32 = p0_pool.registered_main.iter().map(|e| e.count).sum::<u32>()
        + p0_pool
            .registered_sideboard
            .iter()
            .map(|e| e.count)
            .sum::<u32>()
        + p0_pool
            .registered_companion
            .iter()
            .map(|e| e.count)
            .sum::<u32>();
    assert_eq!(
        total_registered_cards, 45,
        "Total registered card pool must be exactly 45 cards"
    );

    // Start game 1 and reveal Gyruda
    let start_result = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start_result.waiting_for;

    let reveal_choice = CompanionDeclaration::Reveal(CompanionRevealChoice {
        name: "Gyruda, Doom of Depths".to_string(),
        source: CompanionChoiceSource::Dedicated,
    });
    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: reveal_choice,
        },
    )
    .expect("Gyruda reveal must succeed");

    assert!(state.players[0]
        .companion
        .as_ref()
        .is_some_and(|c| c.card.card.name == "Gyruda, Doom of Depths"));

    // Transition to BetweenGames BO3 sideboarding
    state.match_phase = MatchPhase::BetweenGames;
    state.game_number = 1;
    state.next_game_chooser = Some(P0);

    // Player moves Gyruda into main deck for Game 2 (replacing Even Creature 0)
    let mut new_main: Vec<DeckCardCount> = main_deck[1..]
        .iter()
        .map(|e| DeckCardCount {
            name: e.card.name.clone(),
            count: e.count,
        })
        .collect();
    new_main.push(DeckCardCount {
        name: "Gyruda, Doom of Depths".to_string(),
        count: 1,
    });

    let new_sideboard = vec![
        DeckCardCount {
            name: main_deck[0].card.name.clone(),
            count: 1,
        },
        DeckCardCount {
            name: "Plains".to_string(),
            count: 4,
        },
    ];

    let mut events = Vec::new();
    let p0_submit = handle_submit_sideboard(&mut state, P0, new_main, new_sideboard, &mut events);
    assert!(
        p0_submit.is_ok(),
        "P0 sideboard submission must conserve the 45-card pool"
    );

    // Opponent submits sideboard to complete the sideboarding phase
    let p1_main: Vec<DeckCardCount> = main_deck
        .iter()
        .map(|e| DeckCardCount {
            name: e.card.name.clone(),
            count: e.count,
        })
        .collect();
    let p1_sideboard = vec![DeckCardCount {
        name: "Plains".to_string(),
        count: 5,
    }];
    let p1_submit = handle_submit_sideboard(&mut state, P1, p1_main, p1_sideboard, &mut events);
    assert!(p1_submit.is_ok(), "P1 sideboard submission must succeed");

    assert!(matches!(
        state.waiting_for,
        WaitingFor::BetweenGamesChoosePlayDraw { .. }
    ));

    // Execute production ChoosePlayDraw and next-game rebuild
    let choose_result = handle_choose_play_draw(&mut state, P0, true, &mut events);
    assert!(choose_result.is_ok(), "ChoosePlayDraw restart must succeed");

    // In Game 2:
    let p0_game2_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    let total_game2_pool: u32 = p0_game2_pool
        .registered_main
        .iter()
        .map(|e| e.count)
        .sum::<u32>()
        + p0_game2_pool
            .registered_sideboard
            .iter()
            .map(|e| e.count)
            .sum::<u32>()
        + p0_game2_pool
            .registered_companion
            .iter()
            .map(|e| e.count)
            .sum::<u32>();
    assert_eq!(
        total_game2_pool, 45,
        "Game 2 card pool must remain exactly 45 cards (no duplicate card created)"
    );

    // Main deck contains Gyruda
    assert!(p0_game2_pool
        .current_main
        .iter()
        .any(|e| e.card.name == "Gyruda, Doom of Depths" && e.count == 1));

    // Sideboard does NOT contain Gyruda
    assert!(!p0_game2_pool
        .current_sideboard
        .iter()
        .any(|e| e.card.name == "Gyruda, Doom of Depths"));

    // Outside-game companion is empty (suppressed because the sole copy is in main deck)
    assert_eq!(p0_game2_pool.current_companion.len(), 0);

    // Companion reveal is NOT offered (proceeds to mulligans/gameplay)
    match &state.waiting_for {
        WaitingFor::CompanionReveal {
            eligible_companions,
            ..
        } => {
            assert!(
                !eligible_companions
                    .iter()
                    .any(|c| c.name == "Gyruda, Doom of Depths"),
                "Gyruda must not be offered as companion when the sole copy is in the main deck"
            );
        }
        WaitingFor::MulliganDecision { .. } => {
            // Correctly skipped companion reveal
        }
        other => panic!("unexpected waiting state in game 2: {other:?}"),
    }

    assert!(state.players[0].companion.is_none());
}

/// CR 100.4b + CR 702.139a/b: When a player submits all 45 registered pool cards into their main
/// deck with an empty sideboard for Game 2, the production ChoosePlayDraw/next-game rebuild
/// conserves the 45-card pool (45 cards) and does not recreate or offer the sole companion outside.
#[test]
fn test_one_drafted_copy_bo3_all_cards_in_main_empty_sideboard_suppresses_outside_companion() {
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);

    let mut main_deck: Vec<DeckEntry> = Vec::new();
    // 23 creatures with even mana value
    for i in 0..23 {
        main_deck.push(creature(&format!("Even Creature {i}"), 2));
    }
    main_deck.push(land("Plains", 17));

    // Sideboard has 1 Gyruda + 4 Plains = 5 cards
    let sideboard = vec![gyruda_companion(), land("Plains", 4)];
    let companion = vec![gyruda_companion()];

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: sideboard.clone(),
            companion: companion.clone(),
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: vec![land("Plains", 5)],
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    // Start Game 1 and reveal Gyruda
    let start_result = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start_result.waiting_for;

    let reveal_choice = CompanionDeclaration::Reveal(CompanionRevealChoice {
        name: "Gyruda, Doom of Depths".to_string(),
        source: CompanionChoiceSource::Dedicated,
    });
    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: reveal_choice,
        },
    )
    .expect("Game 1 Gyruda reveal must succeed");

    // Transition to BetweenGames BO3 sideboarding
    state.match_phase = MatchPhase::BetweenGames;
    state.game_number = 1;
    state.next_game_chooser = Some(P0);

    // Player submits ALL 45 registered cards into the main deck with an EMPTY sideboard
    // 23 creatures + 1 Gyruda + 21 Plains = 45 cards
    let mut new_main: Vec<DeckCardCount> = main_deck
        .iter()
        .map(|e| DeckCardCount {
            name: e.card.name.clone(),
            count: e.count,
        })
        .collect();
    new_main.push(DeckCardCount {
        name: "Gyruda, Doom of Depths".to_string(),
        count: 1,
    });
    // Add the 4 Plains from sideboard to main deck (making 21 Plains total)
    if let Some(plains_entry) = new_main.iter_mut().find(|e| e.name == "Plains") {
        plains_entry.count += 4;
    }

    let new_sideboard: Vec<DeckCardCount> = Vec::new();

    let mut events = Vec::new();
    let p0_submit = handle_submit_sideboard(&mut state, P0, new_main, new_sideboard, &mut events);
    assert!(
        p0_submit.is_ok(),
        "P0 45-card main / 0-card sideboard submission must conserve the 45-card pool"
    );

    let p1_main: Vec<DeckCardCount> = main_deck
        .iter()
        .map(|e| DeckCardCount {
            name: e.card.name.clone(),
            count: e.count,
        })
        .collect();
    let p1_sideboard = vec![DeckCardCount {
        name: "Plains".to_string(),
        count: 5,
    }];
    let p1_submit = handle_submit_sideboard(&mut state, P1, p1_main, p1_sideboard, &mut events);
    assert!(p1_submit.is_ok(), "P1 sideboard submission must succeed");

    // Execute ChoosePlayDraw rebuild
    let choose_result = handle_choose_play_draw(&mut state, P0, true, &mut events);
    assert!(choose_result.is_ok(), "Game 2 rebuild must succeed");

    // In Game 2:
    let p0_game2_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    let total_game2_pool: u32 = p0_game2_pool
        .registered_main
        .iter()
        .map(|e| e.count)
        .sum::<u32>()
        + p0_game2_pool
            .registered_sideboard
            .iter()
            .map(|e| e.count)
            .sum::<u32>()
        + p0_game2_pool
            .registered_companion
            .iter()
            .map(|e| e.count)
            .sum::<u32>();
    assert_eq!(
        total_game2_pool, 45,
        "Game 2 pool must remain exactly 45 cards when all cards are in main deck"
    );

    // Main deck contains all 45 cards (including Gyruda)
    let main_deck_total: u32 = p0_game2_pool.current_main.iter().map(|e| e.count).sum();
    assert_eq!(main_deck_total, 45);
    assert!(p0_game2_pool
        .current_main
        .iter()
        .any(|e| e.card.name == "Gyruda, Doom of Depths" && e.count == 1));

    // Sideboard is empty
    assert_eq!(p0_game2_pool.current_sideboard.len(), 0);

    // Dedicated companion is empty
    assert_eq!(p0_game2_pool.current_companion.len(), 0);

    // Companion reveal is NOT offered
    match &state.waiting_for {
        WaitingFor::CompanionReveal {
            eligible_companions,
            ..
        } => {
            assert!(
                !eligible_companions
                    .iter()
                    .any(|c| c.name == "Gyruda, Doom of Depths"),
                "Gyruda must not be offered as companion when the sole copy is in the main deck"
            );
        }
        WaitingFor::MulliganDecision { .. } => {}
        other => panic!("unexpected waiting state in game 2: {other:?}"),
    }

    assert!(state.players[0].companion.is_none());
}

/// CR 100.4b + CR 702.139a/b: When two physical copies of a companion exist in the pool,
/// one copy may be in the main deck while the other is designated and revealed as companion
/// outside the game in Game 1 and across BO3 next-game rebuild.
#[test]
fn test_two_drafted_copies_bo3_one_in_main_one_outside_preserves_companion() {
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);

    let mut main_deck: Vec<DeckEntry> = Vec::new();
    // 22 creatures with even mana value + 1 Gyruda = 23 even creatures
    for i in 0..22 {
        main_deck.push(creature(&format!("Even Creature {i}"), 2));
    }
    main_deck.push(gyruda_companion());
    main_deck.push(land("Plains", 17));

    // Sideboard has the second Gyruda + 4 Plains = 5 cards
    let sideboard = vec![gyruda_companion(), land("Plains", 4)];
    let companion = vec![gyruda_companion()];

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: sideboard.clone(),
            companion: companion.clone(),
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: vec![land("Plains", 5)],
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    let p0_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    // Main deck has 1 Gyruda, Companion has 1 Gyruda, Sideboard has 4 Plains
    assert_eq!(p0_pool.registered_companion.len(), 1);
    assert_eq!(
        p0_pool.registered_companion[0].card.name,
        "Gyruda, Doom of Depths"
    );
    assert_eq!(p0_pool.registered_sideboard.len(), 1);
    assert_eq!(p0_pool.registered_sideboard[0].count, 4); // 4 Plains

    let total_registered_cards: u32 = p0_pool.registered_main.iter().map(|e| e.count).sum::<u32>()
        + p0_pool
            .registered_sideboard
            .iter()
            .map(|e| e.count)
            .sum::<u32>()
        + p0_pool
            .registered_companion
            .iter()
            .map(|e| e.count)
            .sum::<u32>();
    assert_eq!(
        total_registered_cards, 45,
        "Total registered pool must be exactly 45 cards (2 Gyrudas)"
    );

    // Start Game 1: companion is offered and revealed
    let start_result = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start_result.waiting_for;

    let reveal_choice = CompanionDeclaration::Reveal(CompanionRevealChoice {
        name: "Gyruda, Doom of Depths".to_string(),
        source: CompanionChoiceSource::Dedicated,
    });
    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: reveal_choice.clone(),
        },
    )
    .expect("Game 1 Gyruda reveal must succeed");

    assert!(state.players[0]
        .companion
        .as_ref()
        .is_some_and(|c| c.card.card.name == "Gyruda, Doom of Depths"));

    // Transition to BetweenGames BO3 sideboarding
    state.match_phase = MatchPhase::BetweenGames;
    state.game_number = 1;
    state.next_game_chooser = Some(P0);

    // Submit Game 2: 1 Gyruda in main, 1 Gyruda in sideboard
    let p0_main: Vec<DeckCardCount> = main_deck
        .iter()
        .map(|e| DeckCardCount {
            name: e.card.name.clone(),
            count: e.count,
        })
        .collect();
    let p0_sideboard = vec![
        DeckCardCount {
            name: "Gyruda, Doom of Depths".to_string(),
            count: 1,
        },
        DeckCardCount {
            name: "Plains".to_string(),
            count: 4,
        },
    ];

    let mut events = Vec::new();
    let p0_submit = handle_submit_sideboard(&mut state, P0, p0_main, p0_sideboard, &mut events);
    assert!(
        p0_submit.is_ok(),
        "P0 2-copy sideboard submission must succeed"
    );

    let p1_main: Vec<DeckCardCount> = main_deck
        .iter()
        .map(|e| DeckCardCount {
            name: e.card.name.clone(),
            count: e.count,
        })
        .collect();
    let p1_sideboard = vec![DeckCardCount {
        name: "Plains".to_string(),
        count: 5,
    }];
    let p1_submit = handle_submit_sideboard(&mut state, P1, p1_main, p1_sideboard, &mut events);
    assert!(p1_submit.is_ok(), "P1 sideboard submission must succeed");

    // Execute ChoosePlayDraw rebuild
    let choose_result = handle_choose_play_draw(&mut state, P0, true, &mut events);
    assert!(choose_result.is_ok(), "Game 2 rebuild must succeed");

    // In Game 2:
    let p0_game2_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    let total_game2_pool: u32 = p0_game2_pool
        .registered_main
        .iter()
        .map(|e| e.count)
        .sum::<u32>()
        + p0_game2_pool
            .registered_sideboard
            .iter()
            .map(|e| e.count)
            .sum::<u32>()
        + p0_game2_pool
            .registered_companion
            .iter()
            .map(|e| e.count)
            .sum::<u32>();
    assert_eq!(total_game2_pool, 45);

    // 1 Gyruda in main deck, 1 Gyruda as companion, 4 Plains in sideboard
    assert!(p0_game2_pool
        .current_main
        .iter()
        .any(|e| e.card.name == "Gyruda, Doom of Depths" && e.count == 1));
    assert_eq!(p0_game2_pool.current_companion.len(), 1);
    assert_eq!(
        p0_game2_pool.current_companion[0].card.name,
        "Gyruda, Doom of Depths"
    );
    assert_eq!(p0_game2_pool.current_sideboard.len(), 1);
    assert_eq!(p0_game2_pool.current_sideboard[0].count, 4);

    // Game 2 offers companion reveal for the outside-the-game copy
    assert!(matches!(
        state.waiting_for,
        WaitingFor::CompanionReveal { .. }
    ));
    let reveal_g2 = apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: reveal_choice,
        },
    );
    assert!(
        reveal_g2.is_ok(),
        "Game 2 Gyruda reveal must succeed for 2-copy pool"
    );
}

/// Test that state with declared companion cleanly serializes/deserializes (replay/reload).
#[test]
fn limited_companion_state_roundtrip_preserves_companion_declaration() {
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);
    state.players[P0.0 as usize].companion = Some(engine::types::player::CompanionInfo {
        card: lurrus_companion(),
        used: false,
    });

    let serialized = serde_json::to_string(&state).expect("state must serialize");
    let restored: GameState = serde_json::from_str(&serialized).expect("state must deserialize");

    assert!(restored.players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|c| c.card.card.name == "Lurrus of the Dream-Den" && !c.used));
}
