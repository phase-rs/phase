use std::collections::BTreeMap;

use engine::ai_support::legal_actions;
use engine::database::CardDatabase;
use engine::game::companion::can_activate_companion;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckEntry, DeckList, DeckPayload, PlayerDeckList,
    PlayerDeckPayload,
};
use engine::game::deck_validation::{evaluate_deck_compatibility, DeckCompatibilityRequest};
use engine::game::match_flow::{handle_choose_play_draw, handle_submit_sideboard};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::{apply, start_game_with_starting_player};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, CompanionChoiceSource, CompanionDeclaration, CompanionRevealChoice, GameState,
    WaitingFor,
};
use engine::types::keywords::{CompanionCondition, Keyword};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::match_config::{DeckCardCount, MatchPhase, MatchType};
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

/// Ends game 1 of a best-of-three the way a real match ends it: P0 concedes through `apply`
/// (CR 104.3a), so the production game-over transition and its between-games projection run.
/// The loser, P0, chooses play/draw for game 2.
fn concede_game_one(state: &mut GameState) {
    state.match_config.match_type = MatchType::Bo3;
    apply(state, P0, GameAction::Concede { player_id: P0 }).expect("conceding ends game 1");
    assert_eq!(state.match_phase, MatchPhase::BetweenGames);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::BetweenGamesSideboard { player: P0, .. }
        ),
        "game 1 must end into P0's sideboard prompt, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.next_game_chooser, Some(P0));
}

fn pool_total(state: &GameState, player: PlayerId) -> u32 {
    let pool = state
        .deck_pools
        .iter()
        .find(|p| p.player == player)
        .unwrap();
    pool.registered_main
        .iter()
        .chain(pool.registered_sideboard.iter())
        .chain(pool.registered_companion.iter())
        .map(|entry| entry.count)
        .sum()
}

/// The submission a player makes by keeping the engine-published between-games partitions
/// unchanged: what the sideboard modal sends when the player edits nothing.
fn published_submission(state: &GameState, player: PlayerId) -> GameAction {
    let pool = state
        .deck_pools
        .iter()
        .find(|p| p.player == player)
        .unwrap();
    let counts = |entries: &[DeckEntry]| -> Vec<DeckCardCount> {
        let mut by_name: BTreeMap<String, u32> = BTreeMap::new();
        for entry in entries {
            *by_name.entry(entry.card.name.clone()).or_insert(0) += entry.count;
        }
        by_name
            .into_iter()
            .map(|(name, count)| DeckCardCount { name, count })
            .collect()
    };
    GameAction::SubmitSideboard {
        main: counts(&pool.current_main),
        sideboard: counts(&pool.current_sideboard),
    }
}

/// CR 100.4b + CR 702.139a: plays a Limited Bo3 with a dedicated Lurrus from game 1 to game 2
/// without editing either deck. Game 1 ends naturally (a concession), both seats submit the
/// published `current_main` / `current_sideboard` (the AI's candidate is checked to be that
/// same submission), and game 2 must rebuild the same pool and offer Lurrus again.
fn natural_bo3_round_trip_keeps_dedicated_companion(
    label: &str,
    p0_sideboard: Vec<DeckEntry>,
    declaration: CompanionDeclaration,
) {
    let mut main_deck: Vec<DeckEntry> = (0..23)
        .map(|i| creature(&format!("Small Creature {i}"), 2))
        .collect();
    main_deck.push(land("Plains", 17));
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: p0_sideboard,
            companion: vec![lurrus_companion()],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut state = GameState::new(FormatConfig::limited(), 2, 42);
    load_deck_into_state(&mut state, &payload);
    let registered_total = pool_total(&state, P0);

    let start = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start.waiting_for;
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::CompanionReveal { player: P0, .. }
        ),
        "{label}: game 1 must offer the dedicated Lurrus"
    );
    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: declaration,
        },
    )
    .unwrap_or_else(|e| panic!("{label}: companion declaration failed: {e}"));

    concede_game_one(&mut state);

    // The dedicated copy is back in the published sideboard (CR 100.4b), exactly once.
    let p0_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    let lurrus_in_sideboard: u32 = p0_pool
        .current_sideboard
        .iter()
        .filter(|entry| entry.card.name == "Lurrus of the Dream-Den")
        .map(|entry| entry.count)
        .sum();
    assert_eq!(lurrus_in_sideboard, 1, "{label}: published sideboard");
    assert!(
        p0_pool.current_companion.is_empty(),
        "{label}: no second copy outside the partitions"
    );

    for player in [P0, P1] {
        let submission = published_submission(&state, player);
        assert!(
            legal_actions(&state).contains(&submission),
            "{label}: the AI's sideboard candidate for {player:?} is the published partitions"
        );
        apply(&mut state, player, submission).unwrap_or_else(|e| {
            panic!("{label}: unchanged published partitions for {player:?} rejected: {e}")
        });
    }

    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::BetweenGamesChoosePlayDraw { player: P0, .. }
        ),
        "{label}: both submissions accepted"
    );
    apply(
        &mut state,
        P0,
        GameAction::ChoosePlayDraw { play_first: true },
    )
    .unwrap_or_else(|e| panic!("{label}: game 2 restart failed: {e}"));

    assert_eq!(state.game_number, 2, "{label}");
    assert_eq!(
        pool_total(&state, P0),
        registered_total,
        "{label}: pool conserved"
    );
    let game2 = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    assert_eq!(game2.registered_companion.len(), 1, "{label}");
    assert_eq!(game2.registered_companion[0].count, 1, "{label}");
    assert!(
        !game2
            .registered_sideboard
            .iter()
            .any(|entry| entry.card.name == "Lurrus of the Dream-Den"),
        "{label}: the designated copy is netted out of the sideboard, not duplicated"
    );
    match &state.waiting_for {
        WaitingFor::CompanionReveal {
            player,
            eligible_companions,
        } => {
            assert_eq!(*player, P0, "{label}");
            assert_eq!(
                eligible_companions,
                &vec![CompanionRevealChoice {
                    name: "Lurrus of the Dream-Den".to_string(),
                    source: CompanionChoiceSource::Dedicated,
                }],
                "{label}"
            );
        }
        other => panic!("{label}: game 2 must offer Lurrus again, got {other:?}"),
    }
}

fn reveal_lurrus() -> CompanionDeclaration {
    CompanionDeclaration::Reveal(CompanionRevealChoice {
        name: "Lurrus of the Dream-Den".to_string(),
        source: CompanionChoiceSource::Dedicated,
    })
}

/// The quick-draft / pod payload: the designated copy is also listed in the sideboard, and the
/// Limited loader nets it out into the dedicated slot.
#[test]
fn limited_dedicated_companion_bo3_netted_sideboard_payload_survives_unchanged_sideboarding() {
    for (label, declaration) in [
        ("netted payload, revealed", reveal_lurrus()),
        ("netted payload, declined", CompanionDeclaration::Decline),
    ] {
        natural_bo3_round_trip_keeps_dedicated_companion(
            label,
            vec![lurrus_companion(), land("Plains", 4)],
            declaration,
        );
    }
}

/// The server payload: `sideboard` is empty and the companion travels only in its own slot.
#[test]
fn limited_dedicated_companion_bo3_empty_sideboard_payload_survives_unchanged_sideboarding() {
    for (label, declaration) in [
        ("server payload, revealed", reveal_lurrus()),
        ("server payload, declined", CompanionDeclaration::Decline),
    ] {
        natural_bo3_round_trip_keeps_dedicated_companion(label, Vec::new(), declaration);
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

    // End game 1 through the production game-over transition.
    concede_game_one(&mut state);

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

    // End game 1 through the production game-over transition.
    concede_game_one(&mut state);

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

    // End game 1 through the production game-over transition.
    concede_game_one(&mut state);

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
        printing: Default::default(),
    });

    let serialized = serde_json::to_string(&state).expect("state must serialize");
    let restored: GameState = serde_json::from_str(&serialized).expect("state must deserialize");

    assert!(restored.players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|c| c.card.card.name == "Lurrus of the Dream-Den" && !c.used));
}

/// Yorion, Sky Nomad's printed Oracle text (verified against Scryfall).
const YORION_ORACLE: &str = "Companion \u{2014} Your starting deck contains at least twenty cards more than the minimum deck size. (If this card is your chosen companion, you may put it into your hand from outside the game for {3} as a sorcery.)\nFlying\nWhen Yorion enters, exile any number of other nonland permanents you own and control. Return those cards to the battlefield at the beginning of the next end step.";

/// Yorion built from its real Oracle text through the production parser, so these tests read
/// the condition the card-data pipeline exports rather than a hand-picked variant.
fn yorion_from_oracle() -> DeckEntry {
    let parsed = engine::parser::oracle::parse_oracle_text(
        YORION_ORACLE,
        "Yorion, Sky Nomad",
        &["Companion".into(), "Flying".into()],
        &["Creature".into()],
        &["Bird".into(), "Serpent".into()],
    );
    let keywords: Vec<Keyword> = parsed
        .extracted_keywords
        .into_iter()
        .filter(|keyword| matches!(keyword, Keyword::Companion(_)))
        .collect();
    assert_eq!(
        keywords,
        vec![Keyword::Companion(CompanionCondition::MinDeckSizeOver(20))],
        "Yorion's companion condition is the deck-size surplus"
    );
    DeckEntry {
        card: CardFace {
            name: "Yorion, Sky Nomad".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![],
                generic: 5,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Bird".to_string(), "Serpent".to_string()],
            },
            keywords,
            ..Default::default()
        },
        count: 1,
    }
}

/// CR 702.139a + CR 702.139b: in 40-card Limited, Yorion needs a starting deck of at least 60
/// cards. Pregame offers it at 60 and not at 59 (paired boundary).
#[test]
fn limited_yorion_from_oracle_is_offered_only_at_twenty_over_the_limited_minimum() {
    for (deck_size, offered) in [(60, true), (59, false)] {
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![land("Plains", deck_size)],
                companion: vec![yorion_from_oracle()],
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![land("Plains", 40)],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::limited(), 2, 42);
        load_deck_into_state(&mut state, &payload);
        let p0_pool = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
        assert_eq!(
            p0_pool.current_companion.len(),
            1,
            "{deck_size}: Yorion reached the pool"
        );

        let start = start_game_with_starting_player(&mut state, P0);
        let yorion_offered = matches!(
            &start.waiting_for,
            WaitingFor::CompanionReveal { player, eligible_companions }
                if *player == P0
                    && eligible_companions.iter().any(|c| c.name == "Yorion, Sky Nomad")
        );
        assert_eq!(
            yorion_offered, offered,
            "{deck_size}-card deck: {:?}",
            start.waiting_for
        );
    }
}

/// CR 702.139a + CR 702.139b: the engine's Limited compatibility check (what the deck builder
/// shows and what admission enforces) rejects the real Yorion with 59 cards and accepts 60.
#[test]
fn limited_yorion_from_oracle_compatibility_requires_twenty_over_the_limited_minimum() {
    let faces = [yorion_from_oracle().card, land("Plains", 1).card];
    let export: serde_json::Map<String, serde_json::Value> = faces
        .iter()
        .map(|face| {
            (
                face.name.to_lowercase(),
                serde_json::to_value(face).expect("CardFace serializes"),
            )
        })
        .collect();
    let db = CardDatabase::from_json_str(&serde_json::Value::Object(export).to_string())
        .expect("synthetic export parses");

    for (deck_size, compatible) in [(60usize, true), (59, false)] {
        let request: DeckCompatibilityRequest = serde_json::from_value(serde_json::json!({
            "main_deck": vec!["Plains"; deck_size],
            "companion": ["Yorion, Sky Nomad"],
            "selected_format": "Limited",
        }))
        .expect("request deserializes");
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.unknown_cards.is_empty(),
            "{deck_size}: every card resolves"
        );
        assert_eq!(
            result.selected_format_compatible,
            Some(compatible),
            "{deck_size}-card deck: {:?}",
            result.selected_format_reasons
        );
        assert_eq!(
            result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("not a legal companion")),
            !compatible,
            "{deck_size}-card deck: {:?}",
            result.selected_format_reasons
        );
    }
}

fn move_one(from: &mut Vec<DeckCardCount>, to: &mut Vec<DeckCardCount>, name: &str) {
    let source = from
        .iter_mut()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("{name} is in the partition"));
    source.count -= 1;
    from.retain(|entry| entry.count > 0);
    match to.iter_mut().find(|entry| entry.name == name) {
        Some(entry) => entry.count += 1,
        None => to.push(DeckCardCount {
            name: name.to_string(),
            count: 1,
        }),
    }
}

/// CR 702.139a + CR 100.4b: a Limited designation is one card from outside the game, not
/// the only one. Game 1 reveals the designated Lurrus; between games P0 boards in a four-drop,
/// so in game 2 Lurrus (still designated, since a copy stays outside the main deck) no longer
/// qualifies, while the sideboard Gyruda does. Game 2 must offer Gyruda from the sideboard,
/// and once the window closes the unrevealed Lurrus is a sideboard card again.
#[test]
fn limited_game_two_offers_an_eligible_sideboard_companion_when_the_designation_fails() {
    let mut main_deck: Vec<DeckEntry> = (0..23)
        .map(|i| creature(&format!("Small Creature {i}"), 2))
        .collect();
    main_deck.push(land("Plains", 17));
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: vec![
                lurrus_companion(),
                gyruda_companion(),
                creature("Big Creature", 4),
            ],
            companion: vec![lurrus_companion()],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);
    load_deck_into_state(&mut state, &payload);
    let registered_total = pool_total(&state, P0);

    let start = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start.waiting_for;
    match &state.waiting_for {
        WaitingFor::CompanionReveal {
            player,
            eligible_companions,
        } => {
            assert_eq!(*player, P0);
            let offered: Vec<(&str, bool)> = eligible_companions
                .iter()
                .map(|c| {
                    (
                        c.name.as_str(),
                        c.source == CompanionChoiceSource::Dedicated,
                    )
                })
                .collect();
            assert_eq!(
                offered,
                vec![
                    ("Lurrus of the Dream-Den", true),
                    ("Gyruda, Doom of Depths", false)
                ],
                "game 1 offers the designation and the eligible sideboard companion"
            );
        }
        other => panic!("game 1 must offer companions, got {other:?}"),
    }
    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: reveal_lurrus(),
        },
    )
    .expect("Lurrus reveal");

    concede_game_one(&mut state);

    let GameAction::SubmitSideboard {
        mut main,
        mut sideboard,
    } = published_submission(&state, P0)
    else {
        unreachable!("published_submission builds a sideboard submission");
    };
    move_one(&mut sideboard, &mut main, "Big Creature");
    move_one(&mut main, &mut sideboard, "Small Creature 0");
    apply(
        &mut state,
        P0,
        GameAction::SubmitSideboard { main, sideboard },
    )
    .expect("boarding in the four-drop keeps the pool");
    let p1_submission = published_submission(&state, P1);
    apply(&mut state, P1, p1_submission).expect("P1 keeps its deck");
    apply(
        &mut state,
        P0,
        GameAction::ChoosePlayDraw { play_first: true },
    )
    .expect("game 2 starts");

    assert_eq!(state.game_number, 2);
    let game2 = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    assert_eq!(
        game2
            .current_companion
            .iter()
            .map(|e| e.card.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Lurrus of the Dream-Den"],
        "reach guard: Lurrus is still the designation in game 2"
    );
    let gyruda_choice = match &state.waiting_for {
        WaitingFor::CompanionReveal {
            player,
            eligible_companions,
        } => {
            assert_eq!(*player, P0);
            assert_eq!(eligible_companions.len(), 1, "{eligible_companions:?}");
            assert_eq!(eligible_companions[0].name, "Gyruda, Doom of Depths");
            assert!(matches!(
                eligible_companions[0].source,
                CompanionChoiceSource::Sideboard { .. }
            ));
            eligible_companions[0].clone()
        }
        other => panic!("game 2 must offer the sideboard Gyruda, got {other:?}"),
    };
    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: CompanionDeclaration::Reveal(gyruda_choice),
        },
    )
    .expect("sideboard Gyruda reveal");

    assert!(state.players[0]
        .companion
        .as_ref()
        .is_some_and(|c| c.card.card.name == "Gyruda, Doom of Depths"));
    let after = state.deck_pools.iter().find(|p| p.player == P0).unwrap();
    assert!(
        after.current_companion.is_empty(),
        "the window closed and released the unrevealed designation"
    );
    let sideboard_count = |name: &str| -> u32 {
        after
            .current_sideboard
            .iter()
            .filter(|e| e.card.name == name)
            .map(|e| e.count)
            .sum()
    };
    assert_eq!(sideboard_count("Lurrus of the Dream-Den"), 1);
    assert_eq!(sideboard_count("Gyruda, Doom of Depths"), 0);
    assert_eq!(pool_total(&state, P0), registered_total, "pool conserved");
}

/// Living Wish's printed Oracle text (verified against Scryfall).
const LIVING_WISH: &str = "You may reveal a creature or land card you own from outside the game and put it into your hand. Exile Living Wish.";

/// Casts Living Wish for P0 against `pools` and returns the names it offers from outside
/// the game.
fn living_wish_offers(pools: Vec<engine::types::game_state::PlayerDeckPool>) -> Vec<String> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wish = scenario
        .add_spell_to_hand_from_oracle(P0, "Living Wish", false, LIVING_WISH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().deck_pools = pools;
    let card_id = runner.state().objects[&wish].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: wish,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Living Wish is castable");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("reveal from outside the game");
            }
            _ => break,
        }
    }
    match &runner.state().waiting_for {
        WaitingFor::OutsideGameChoice { choices, .. } => {
            choices.iter().map(|choice| choice.name.clone()).collect()
        }
        other => panic!("Living Wish must reach its outside-the-game choice, got {other:?}"),
    }
}

/// Plays a Limited game 1 to the end of the companion reveal window and returns the pools.
fn limited_pools_after_reveal_window(
    main_deck: Vec<DeckEntry>,
    declaration: Option<CompanionDeclaration>,
) -> Vec<engine::types::game_state::PlayerDeckPool> {
    limited_state_after_reveal_window(main_deck, declaration).deck_pools
}

/// Plays a Limited game 1 to the end of the companion reveal window and returns the state.
fn limited_state_after_reveal_window(
    main_deck: Vec<DeckEntry>,
    declaration: Option<CompanionDeclaration>,
) -> GameState {
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: main_deck.clone(),
            sideboard: vec![land("Plains", 3), lurrus_companion()],
            companion: vec![lurrus_companion()],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut state = GameState::new(FormatConfig::limited(), 2, 42);
    load_deck_into_state(&mut state, &payload);
    let pool = state
        .deck_pools
        .iter()
        .find(|pool| pool.player == P0)
        .unwrap();
    assert_eq!(
        pool.current_companion.len(),
        1,
        "the sideboard backs the designation"
    );
    assert_eq!(
        pool.current_companion[0].card.name,
        "Lurrus of the Dream-Den"
    );
    assert!(pool.current_companion[0]
        .card
        .keywords
        .contains(&Keyword::Companion(
            CompanionCondition::MaxPermanentManaValue(2),
        )));
    let start = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start.waiting_for;
    if let Some(choice) = declaration {
        assert!(
            matches!(&state.waiting_for,
            WaitingFor::CompanionReveal { player, eligible_companions }
                if *player == P0 && eligible_companions.contains(&CompanionRevealChoice {
                    name: "Lurrus of the Dream-Den".to_string(),
                    source: CompanionChoiceSource::Dedicated,
                })),
            "reach guard: the designated Lurrus is offered before declaring: {:?}",
            state.waiting_for
        );
        apply(&mut state, P0, GameAction::DeclareCompanion { choice })
            .expect("companion declaration");
    }
    assert!(
        matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }),
        "the reveal window is closed, got {:?}",
        state.waiting_for
    );
    state
}

/// CR 100.4b + CR 400.11a: once the reveal window closes, a Limited designation that was
/// not revealed is a sideboard card, so an outside-the-game effect can find it, whether the
/// player declined it or it was never offered. A revealed one is the companion instead and
/// is not in the sideboard.
#[test]
fn limited_unrevealed_designation_is_a_sideboard_card_for_outside_the_game_effects() {
    let mut eligible: Vec<DeckEntry> = (0..23)
        .map(|i| creature(&format!("Small Creature {i}"), 2))
        .collect();
    eligible.push(land("Plains", 17));
    let mut ineligible: Vec<DeckEntry> = (0..22)
        .map(|i| creature(&format!("Small Creature {i}"), 2))
        .collect();
    ineligible.push(creature("Expensive Creature", 3));
    ineligible.push(land("Plains", 17));

    let lurrus = "Lurrus of the Dream-Den".to_string();
    let declined = living_wish_offers(limited_pools_after_reveal_window(
        eligible.clone(),
        Some(CompanionDeclaration::Decline),
    ));
    assert!(declined.contains(&lurrus), "declined: {declined:?}");

    let never_offered = living_wish_offers(limited_pools_after_reveal_window(ineligible, None));
    assert!(
        never_offered.contains(&lurrus),
        "never offered: {never_offered:?}"
    );

    // The revealed companion leaves the SIDEBOARD pool; it is offered separately, as a card
    // still outside the game (see the Living Wish tests below).
    let revealed = living_wish_offers(limited_pools_after_reveal_window(
        eligible,
        Some(reveal_lurrus()),
    ));
    assert!(
        !revealed.contains(&lurrus),
        "the revealed companion is not a sideboard card: {revealed:?}"
    );
    assert!(
        revealed.iter().any(|i| i == "Plains"),
        "reach guard: the wish saw the sideboard: {revealed:?}"
    );
}

/// CR 702.139a + CR 702.139b: in 60-card Constructed, Yorion (parsed from its Oracle text)
/// needs an 80-card starting deck. It is offered from the sideboard at 80 and not at 79.
#[test]
fn constructed_yorion_from_oracle_is_offered_only_at_eighty_cards() {
    for (deck_size, offered) in [(80, true), (79, false)] {
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![land("Plains", deck_size)],
                sideboard: vec![yorion_from_oracle()],
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![land("Plains", 60)],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        load_deck_into_state(&mut state, &payload);
        let start = start_game_with_starting_player(&mut state, P0);
        let yorion_offered = matches!(
            &start.waiting_for,
            WaitingFor::CompanionReveal { player, eligible_companions }
                if *player == P0
                    && eligible_companions.iter().any(|c| c.name == "Yorion, Sky Nomad"
                        && matches!(c.source, CompanionChoiceSource::Sideboard { .. }))
        );
        assert_eq!(
            yorion_offered, offered,
            "{deck_size}-card deck: {:?}",
            start.waiting_for
        );
    }
}

/// CR 702.139a + CR 702.139b + CR 903.5a: a Commander deck is exactly 100 cards, so Yorion's
/// "twenty cards more than the minimum deck size" can never hold there. The same fixture
/// with Gyruda in the slot is offered, so the slot itself was reached.
#[test]
fn commander_family_never_offers_yorion() {
    let commander = DeckEntry {
        card: CardFace {
            name: "Plain Commander".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![],
                generic: 2,
            },
            card_type: CardType {
                supertypes: vec![engine::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string()],
            },
            ..Default::default()
        },
        count: 1,
    };
    for (companion, offered) in [(yorion_from_oracle(), false), (gyruda_companion(), true)] {
        let name = companion.card.name.clone();
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![land("Plains", 99)],
                commander: vec![commander.clone()],
                companion: vec![companion],
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![land("Plains", 99)],
                commander: vec![commander.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        load_deck_into_state(&mut state, &payload);
        let start = start_game_with_starting_player(&mut state, P0);
        let was_offered = matches!(
            &start.waiting_for,
            WaitingFor::CompanionReveal { player, eligible_companions }
                if *player == P0
                    && eligible_companions.iter().any(|c| c.name == name
                        && c.source == CompanionChoiceSource::Dedicated)
        );
        assert_eq!(was_offered, offered, "{name}: {:?}", start.waiting_for);
    }
}

/// CR 702.139b + CR 903.5a: a Commander Draft pod submits its commander in the main list as
/// well as the command-zone slot, and the loader nets that double listing to one card. At the
/// 60-card minimum Yorion therefore needs an 80-card deck COUNTING THE COMMANDER ONCE: it is
/// offered at 80 and not at 79 (paired boundary).
#[test]
fn commander_draft_pregame_counts_a_double_listed_commander_once_for_yorion() {
    let commander = DeckEntry {
        card: CardFace {
            name: "Draft Commander".to_string(),
            card_type: CardType {
                supertypes: vec![engine::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string()],
            },
            ..Default::default()
        },
        count: 1,
    };
    for (deck_size, offered) in [(80u32, true), (79, false)] {
        let main_deck = vec![commander.clone(), land("Plains", deck_size - 1)];
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: main_deck.clone(),
                commander: vec![commander.clone()],
                companion: vec![yorion_from_oracle()],
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck,
                commander: vec![commander.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(FormatConfig::commander_draft(), 2, 42);
        load_deck_into_state(&mut state, &payload);
        let start = start_game_with_starting_player(&mut state, P0);
        let yorion_offered = matches!(
            &start.waiting_for,
            WaitingFor::CompanionReveal { player, eligible_companions }
                if *player == P0
                    && eligible_companions.iter().any(|c| c.name == "Yorion, Sky Nomad")
        );
        assert_eq!(
            yorion_offered, offered,
            "{deck_size}-card deck: {:?}",
            start.waiting_for
        );
    }
}

/// Lutri, the Spellchaser's printed Oracle text (verified against Scryfall).
const LUTRI_ORACLE: &str = "Companion \u{2014} Each nonland card in your starting deck has a different name. (If this card is your chosen companion, you may put it into your hand from outside the game for {3} as a sorcery.)\nFlash\nWhen Lutri enters, if you cast it, copy target instant or sorcery spell you control. You may choose new targets for the copy.";

/// CR 702.139a: Lutri built from its real Oracle text through the production parser reads as
/// the singleton condition, and Limited admission measures NONLAND names only: a repeated
/// nonland name is refused, while distinct nonland names with repeated basic lands pass.
#[test]
fn limited_lutri_from_oracle_admission_requires_distinct_nonland_names() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        LUTRI_ORACLE,
        "Lutri, the Spellchaser",
        &["Companion".into(), "Flash".into()],
        &["Creature".into()],
        &["Elemental".into(), "Otter".into()],
    );
    let keywords: Vec<Keyword> = parsed
        .extracted_keywords
        .into_iter()
        .filter(|keyword| matches!(keyword, Keyword::Companion(_)))
        .collect();
    assert_eq!(
        keywords,
        vec![Keyword::Companion(CompanionCondition::Singleton)],
        "Lutri's companion condition is the singleton condition"
    );

    let mut faces = vec![
        CardFace {
            name: "Lutri, the Spellchaser".to_string(),
            keywords,
            ..Default::default()
        },
        land("Plains", 1).card,
    ];
    faces.extend((0..23).map(|i| creature(&format!("Distinct Spell {i}"), 2).card));
    let export: serde_json::Map<String, serde_json::Value> = faces
        .iter()
        .map(|face| {
            (
                face.name.to_lowercase(),
                serde_json::to_value(face).expect("CardFace serializes"),
            )
        })
        .collect();
    let db = CardDatabase::from_json_str(&serde_json::Value::Object(export).to_string())
        .expect("synthetic export parses");

    let distinct: Vec<String> = (0..23).map(|i| format!("Distinct Spell {i}")).collect();
    let mut repeated = distinct.clone();
    repeated[1] = repeated[0].clone();
    for (spells, compatible) in [(distinct, true), (repeated, false)] {
        let mut main_deck = spells;
        main_deck.extend(vec!["Plains".to_string(); 17]);
        let request: DeckCompatibilityRequest = serde_json::from_value(serde_json::json!({
            "main_deck": main_deck,
            "companion": ["Lutri, the Spellchaser"],
            "selected_format": "Limited",
        }))
        .expect("request deserializes");
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(result.unknown_cards.is_empty(), "every card resolves");
        assert_eq!(
            result.selected_format_compatible,
            Some(compatible),
            "{:?}",
            result.selected_format_reasons
        );
        assert_eq!(
            result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("not a legal companion")),
            !compatible,
            "{:?}",
            result.selected_format_reasons
        );
    }
}

/// A game-1 Limited deck Lurrus accepts: 23 two-drops and 17 Plains.
fn lurrus_eligible_deck() -> Vec<DeckEntry> {
    let mut deck: Vec<DeckEntry> = (0..23)
        .map(|i| creature(&format!("Small Creature {i}"), 2))
        .collect();
    deck.push(land("Plains", 17));
    deck
}

/// A scenario with Living Wish in P0's hand and P0's pools and revealed companion copied from
/// `prepared`, a game that went through the real reveal window.
fn living_wish_runner(prepared: &GameState) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wish = scenario
        .add_spell_to_hand_from_oracle(P0, "Living Wish", false, LIVING_WISH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().deck_pools = prepared.deck_pools.clone();
    runner.state_mut().players[P0.0 as usize].companion =
        prepared.players[P0.0 as usize].companion.clone();
    (runner, wish)
}

/// Casts Living Wish and accepts its optional reveal, stopping at the outside-the-game choice,
/// whose offered entries are returned.
fn cast_living_wish_to_choice(
    runner: &mut GameRunner,
    wish: ObjectId,
) -> Vec<engine::types::game_state::OutsideGameChoiceEntry> {
    let card_id = runner.state().objects[&wish].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: wish,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Living Wish is castable");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("reveal from outside the game");
            }
            _ => break,
        }
    }
    match &runner.state().waiting_for {
        WaitingFor::OutsideGameChoice { choices, .. } => choices.clone(),
        other => panic!("Living Wish must reach its outside-the-game choice, got {other:?}"),
    }
}

fn companion_entries(
    choices: &[engine::types::game_state::OutsideGameChoiceEntry],
) -> Vec<&engine::types::game_state::OutsideGameChoiceEntry> {
    choices
        .iter()
        .filter(|choice| {
            matches!(
                choice.source,
                engine::types::game_state::OutsideGameChoiceSource::Companion { .. }
            )
        })
        .collect()
}

fn add_white_mana(runner: &mut GameRunner, amount: usize) {
    for _ in 0..amount {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(ManaType::White, ObjectId(999), false, vec![]));
    }
}

/// CR 702.139a + CR 400.11 + CR 100.4b: a revealed Limited companion is still a card outside
/// the game until it is put into hand, so Living Wish ("a creature or land card you own from
/// outside the game") can find it even though the reveal took it out of the sideboard pool.
/// Taking it through the wish brings it into the game (CR 702.139c) and uses the companion up:
/// the special action is then refused.
#[test]
fn living_wish_finds_the_revealed_limited_companion_and_uses_it_up() {
    let prepared = limited_state_after_reveal_window(lurrus_eligible_deck(), Some(reveal_lurrus()));
    assert!(
        prepared.players[P0.0 as usize]
            .companion
            .as_ref()
            .is_some_and(|companion| !companion.used),
        "reach guard: Lurrus was revealed and is not yet in hand"
    );

    let (mut runner, wish) = living_wish_runner(&prepared);
    let choices = cast_living_wish_to_choice(&mut runner, wish);
    let offered = companion_entries(&choices);
    assert_eq!(
        offered.len(),
        1,
        "the companion is offered once: {choices:?}"
    );
    assert_eq!(offered[0].name, "Lurrus of the Dream-Den");
    assert_eq!(offered[0].count, 1);
    assert_eq!(
        choices
            .iter()
            .filter(|choice| choice.name == "Lurrus of the Dream-Den")
            .count(),
        1,
        "the companion is not ALSO a sideboard entry: {choices:?}"
    );

    runner
        .act(GameAction::ChooseOutsideGameCards {
            selections: vec![engine::types::actions::OutsideGameSelection::Companion],
        })
        .expect("the revealed companion can be brought in from outside the game");
    assert!(
        runner.state().objects.values().any(|obj| {
            obj.owner == P0 && obj.zone == Zone::Hand && obj.name == "Lurrus of the Dream-Den"
        }),
        "the wish put the companion into hand"
    );
    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| companion.used));

    // It is in the game now: the special action cannot put a second copy into hand.
    add_white_mana(&mut runner, 3);
    assert!(!can_activate_companion(runner.state(), P0));
    assert!(runner.act(GameAction::CompanionToHand).is_err());
}

/// CR 702.139c: once the special action has put the companion into hand it is in the game, no
/// longer a card outside it, so a later Living Wish cannot find it. Paired with the test above,
/// which finds the same unused companion before the special action.
#[test]
fn living_wish_cannot_find_a_companion_already_put_into_hand() {
    let prepared = limited_state_after_reveal_window(lurrus_eligible_deck(), Some(reveal_lurrus()));
    let (mut runner, wish) = living_wish_runner(&prepared);
    add_white_mana(&mut runner, 3);
    runner
        .act(GameAction::CompanionToHand)
        .expect("the companion special action pays {3} and puts Lurrus into hand");
    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| companion.used));

    let choices = cast_living_wish_to_choice(&mut runner, wish);
    assert!(
        companion_entries(&choices).is_empty(),
        "the companion is in the game: {choices:?}"
    );
    assert!(
        choices.iter().any(|choice| choice.name == "Plains"),
        "reach guard: the wish still saw the sideboard: {choices:?}"
    );
    assert!(runner
        .act(GameAction::ChooseOutsideGameCards {
            selections: vec![engine::types::actions::OutsideGameSelection::Companion],
        })
        .is_err());
}

/// A Commander game 1 taken through the real reveal window with Gyruda revealed as P0's
/// dedicated companion (even mana values: Plains and a two-mana commander).
fn commander_state_with_revealed_gyruda() -> GameState {
    let commander = DeckEntry {
        card: CardFace {
            name: "Plain Commander".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![],
                generic: 2,
            },
            card_type: CardType {
                supertypes: vec![engine::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string()],
            },
            ..Default::default()
        },
        count: 1,
    };
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: vec![land("Plains", 99)],
            commander: vec![commander.clone()],
            companion: vec![gyruda_companion()],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: vec![land("Plains", 99)],
            commander: vec![commander],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut state = GameState::new(FormatConfig::commander(), 2, 42);
    load_deck_into_state(&mut state, &payload);
    let start = start_game_with_starting_player(&mut state, P0);
    state.waiting_for = start.waiting_for;
    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: CompanionDeclaration::Reveal(CompanionRevealChoice {
                name: "Gyruda, Doom of Depths".to_string(),
                source: CompanionChoiceSource::Dedicated,
            }),
        },
    )
    .expect("Gyruda is revealed in Commander");
    state
}

/// CR 903.11 + CR 702.139d: Commander admits a card from outside the game only through
/// rules, special actions and effects that specifically bring cards in. Living Wish merely
/// finds "a card you own from outside the game", which does not function in Commander, so the
/// revealed Gyruda is not offered to it. The companion special action is the way it enters and
/// still works there. Paired with `living_wish_finds_the_revealed_limited_companion_and_uses_it_up`,
/// where the same Wish does find the companion in Limited.
#[test]
fn living_wish_does_not_offer_the_revealed_companion_in_commander() {
    let prepared = commander_state_with_revealed_gyruda();
    assert!(
        prepared.players[P0.0 as usize]
            .companion
            .as_ref()
            .is_some_and(
                |companion| !companion.used && companion.card.card.name == "Gyruda, Doom of Depths"
            ),
        "reach guard: Gyruda was revealed and is not yet in hand"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wish = scenario
        .add_spell_to_hand_from_oracle(P0, "Living Wish", false, LIVING_WISH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().format_config = FormatConfig::commander();
    runner.state_mut().deck_pools = prepared.deck_pools.clone();
    runner.state_mut().players[P0.0 as usize].companion =
        prepared.players[P0.0 as usize].companion.clone();

    let card_id = runner.state().objects[&wish].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: wish,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Living Wish is castable");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("reveal from outside the game");
            }
            _ => break,
        }
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OutsideGameChoice { .. }
        ),
        "Living Wish offers nothing from outside the game in Commander: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Living Wish resolved"
    );
    assert!(
        !runner.state().objects.values().any(|obj| {
            obj.owner == P0 && obj.zone == Zone::Hand && obj.name == "Gyruda, Doom of Depths"
        }),
        "the wish did not bring the companion into the game"
    );
    assert!(runner.state().players[P0.0 as usize]
        .companion
        .as_ref()
        .is_some_and(|companion| !companion.used));

    // CR 702.139d: the special action still brings it in.
    add_white_mana(&mut runner, 3);
    assert!(can_activate_companion(runner.state(), P0));
    runner
        .act(GameAction::CompanionToHand)
        .expect("the companion special action works in Commander");
    assert!(runner.state().objects.values().any(|obj| {
        obj.owner == P0 && obj.zone == Zone::Hand && obj.name == "Gyruda, Doom of Depths"
    }));
}

/// CR 100.2b + CR 702.139b + CR 903.13f(1): a Commander Cube configured for 65 launches its
/// match under that minimum, so the pregame reveal offers Yorion at 85 cards (twenty over) and
/// not at 84 or at the registry's 80. A launch under the registry minimum keeps the 80 threshold
/// (paired control), so the single configured minimum is what moves it.
#[test]
fn commander_cube_pregame_offers_yorion_at_the_session_configured_minimum() {
    let commander = DeckEntry {
        card: CardFace {
            name: "Draft Commander".to_string(),
            card_type: CardType {
                supertypes: vec![engine::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string()],
            },
            ..Default::default()
        },
        count: 1,
    };
    let offered_at = |config: FormatConfig, deck_size: u32| {
        let main_deck = vec![commander.clone(), land("Plains", deck_size - 1)];
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: main_deck.clone(),
                commander: vec![commander.clone()],
                companion: vec![yorion_from_oracle()],
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck,
                commander: vec![commander.clone()],
                ..Default::default()
            },
            ..Default::default()
        };
        let mut state = GameState::new(config, 2, 42);
        load_deck_into_state(&mut state, &payload);
        let start = start_game_with_starting_player(&mut state, P0);
        matches!(
            &start.waiting_for,
            WaitingFor::CompanionReveal { player, eligible_companions }
                if *player == P0
                    && eligible_companions.iter().any(|c| c.name == "Yorion, Sky Nomad")
        )
    };
    let cube = || {
        FormatConfig::commander_draft()
            .with_draft_session_minimum(65)
            .expect("a Commander Cube may configure a minimum above 60")
    };
    for (deck_size, offered) in [(85u32, true), (84, false), (80, false)] {
        assert_eq!(
            offered_at(cube(), deck_size),
            offered,
            "Cube minimum 65, {deck_size}-card deck"
        );
    }
    for (deck_size, offered) in [(80u32, true), (79, false)] {
        assert_eq!(
            offered_at(FormatConfig::commander_draft(), deck_size),
            offered,
            "registry minimum 60, {deck_size}-card deck"
        );
    }
}

/// CR 702.139a: Limited admission refuses a printed companion whose condition the starting deck
/// does not fulfil. Jegantha ("no card repeats a mana symbol") counts hybrid symbols, so Boros
/// Reckoner's {R/W}{R/W}{R/W} fails it; Zirda ("each permanent card has an activated ability")
/// covers land cards, so Glacial Chasm (no activated ability) fails it. Each has a legal control:
/// non-repeating hybrid symbols, and a deck of basic lands (CR 305.6 intrinsic mana ability).
#[test]
fn limited_admission_checks_jegantha_hybrid_symbols_and_zirda_land_cards() {
    let companion = |name: &str, condition| DeckEntry {
        card: CardFace {
            name: name.to_string(),
            keywords: vec![Keyword::Companion(condition)],
            ..Default::default()
        },
        count: 1,
    };
    let spell_with = |name: &str, shards: Vec<ManaCostShard>| DeckEntry {
        card: CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::Cost { shards, generic: 0 },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            },
            ..Default::default()
        },
        count: 1,
    };
    let utility_land = DeckEntry {
        card: CardFace {
            name: "Glacial Chasm".to_string(),
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Land],
                subtypes: vec![],
            },
            ..Default::default()
        },
        count: 1,
    };
    let entries = [
        companion(
            "Jegantha, the Wellspring",
            CompanionCondition::NoRepeatedManaSymbols,
        ),
        companion(
            "Zirda, the Dawnwaker",
            CompanionCondition::PermanentsHaveActivatedAbilities,
        ),
        spell_with(
            "Boros Reckoner",
            vec![
                ManaCostShard::RedWhite,
                ManaCostShard::RedWhite,
                ManaCostShard::RedWhite,
            ],
        ),
        spell_with(
            "Distinct Hybrids",
            vec![ManaCostShard::RedWhite, ManaCostShard::GreenWhite],
        ),
        utility_land,
        land("Plains", 1),
    ];
    let export: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .map(|entry| {
            (
                entry.card.name.to_lowercase(),
                serde_json::to_value(&entry.card).expect("CardFace serializes"),
            )
        })
        .collect();
    let db = CardDatabase::from_json_str(&serde_json::Value::Object(export).to_string())
        .expect("synthetic export parses");

    let cases: [(&str, &str, bool); 4] = [
        ("Jegantha, the Wellspring", "Boros Reckoner", false),
        ("Jegantha, the Wellspring", "Distinct Hybrids", true),
        ("Zirda, the Dawnwaker", "Glacial Chasm", false),
        ("Zirda, the Dawnwaker", "Plains", true),
    ];
    for (companion_name, odd_card, compatible) in cases {
        let mut main_deck = vec![odd_card.to_string()];
        main_deck.extend(vec!["Plains".to_string(); 39]);
        let request: DeckCompatibilityRequest = serde_json::from_value(serde_json::json!({
            "main_deck": main_deck,
            "companion": [companion_name],
            "selected_format": "Limited",
        }))
        .expect("request deserializes");
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(result.unknown_cards.is_empty(), "every card resolves");
        assert_eq!(
            result.selected_format_compatible,
            Some(compatible),
            "{companion_name} with {odd_card}: {:?}",
            result.selected_format_reasons
        );
        assert_eq!(
            result
                .selected_format_reasons
                .iter()
                .any(|reason| reason.contains("not a legal companion")),
            !compatible,
            "{companion_name} with {odd_card}: {:?}",
            result.selected_format_reasons
        );
    }
}

/// CR 709.4b + CR 702.139a: Jegantha sees the separate symbols of every half of a split card, so
/// Fire // Ice ({1}{R} and {1}{U}) contains {1} twice and fails it, while Wear // Tear ({1}{R} and
/// {W}) shares no symbol and passes. Both the public deck-compatibility check and the pregame
/// reveal of a two-player Limited game must agree.
#[test]
fn limited_jegantha_reads_every_split_half_at_admission_and_reveal() {
    const JEGANTHA: &str = "Jegantha, the Wellspring";
    let face =
        |name: &str, shard: Option<ManaCostShard>, generic: u32, types: &[CoreType]| CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::Cost {
                shards: shard.into_iter().collect(),
                generic,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: types.to_vec(),
                subtypes: vec![],
            },
            ..Default::default()
        };
    let mut jegantha = face(JEGANTHA, None, 5, &[CoreType::Creature]);
    jegantha.keywords = vec![Keyword::Companion(
        CompanionCondition::NoRepeatedManaSymbols,
    )];
    let mut mountain = face("Mountain", None, 0, &[CoreType::Land]);
    mountain.mana_cost = ManaCost::NoCost;
    mountain.card_type.subtypes = vec!["Mountain".to_string()];

    // (face, split oracle id, position within the split card)
    let halves = [
        ("Fire", Some(ManaCostShard::Red), 1, "fire-ice", 0),
        ("Ice", Some(ManaCostShard::Blue), 1, "fire-ice", 1),
        ("Wear", Some(ManaCostShard::Red), 1, "wear-tear", 0),
        ("Tear", Some(ManaCostShard::White), 0, "wear-tear", 1),
    ];
    let mut export = serde_json::Map::new();
    for single in [&jegantha, &mountain] {
        export.insert(
            single.name.to_lowercase(),
            serde_json::to_value(single).expect("CardFace serializes"),
        );
    }
    for (name, shard, generic, oracle_id, order) in halves {
        let mut half = face(name, shard, generic, &[CoreType::Instant]);
        half.scryfall_oracle_id = Some(oracle_id.to_string());
        let mut value = serde_json::to_value(&half).expect("CardFace serializes");
        value["layout"] = "split".into();
        value["face_index"] = order.into();
        export.insert(name.to_lowercase(), value);
    }
    let db = CardDatabase::from_json_str(&serde_json::Value::Object(export).to_string())
        .expect("synthetic export parses");

    for (split_card, legal) in [("Fire // Ice", false), ("Wear // Tear", true)] {
        let mut main_deck = vec![split_card.to_string()];
        main_deck.extend(vec!["Mountain".to_string(); 39]);

        // Public deck submission.
        let request: DeckCompatibilityRequest = serde_json::from_value(serde_json::json!({
            "main_deck": main_deck,
            "companion": [JEGANTHA],
            "selected_format": "Limited",
        }))
        .expect("request deserializes");
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(result.unknown_cards.is_empty(), "every card resolves");
        assert_eq!(
            result.selected_format_compatible,
            Some(legal),
            "{split_card}: {:?}",
            result.selected_format_reasons
        );

        // Pregame reveal of the same deck through the production loader.
        let list = DeckList {
            player: PlayerDeckList {
                main_deck,
                companion: vec![JEGANTHA.to_string()],
                ..Default::default()
            },
            opponent: PlayerDeckList {
                main_deck: vec!["Mountain".to_string(); 40],
                ..Default::default()
            },
            ..Default::default()
        };
        let payload = resolve_deck_list(&db, &engine::database::ComboTable::default(), &list);
        let mut state = GameState::new(FormatConfig::limited(), 2, 42);
        load_deck_into_state(&mut state, &payload);
        let start = start_game_with_starting_player(&mut state, P0);
        let offered: Vec<String> = match start.waiting_for {
            WaitingFor::CompanionReveal {
                eligible_companions,
                ..
            } => eligible_companions.into_iter().map(|c| c.name).collect(),
            WaitingFor::MulliganDecision { .. } => Vec::new(),
            other => panic!("unexpected waiting state: {other:?}"),
        };
        assert_eq!(
            offered.iter().any(|name| name == JEGANTHA),
            legal,
            "{split_card}: offered {offered:?}"
        );
    }
}

/// CR 110.4a: "permanent card" means an artifact, battle, creature, enchantment, land, or
/// planeswalker card, so a battle counts for the permanent-card companion conditions. Invasion
/// of Zendikar ({3}{G}, Battle -- Siege) has no activated ability and mana value 4, so it fails
/// both Zirda, the Dawnwaker ("each permanent card in your starting deck has an activated
/// ability") and Lurrus of the Dream-Den ("each permanent card in your starting deck has mana
/// value 2 or less"). Explosive Vegetation, a {3}{G} sorcery with the same effect, is the legal
/// control: it is not a permanent card, so neither condition reads it. Both the public deck
/// submission and the pregame reveal of a two-player Limited game must agree.
#[test]
fn limited_companion_permanent_card_conditions_count_battles() {
    const ZIRDA: &str = "Zirda, the Dawnwaker";
    const LURRUS: &str = "Lurrus of the Dream-Den";
    let face =
        |name: &str, shards: Vec<ManaCostShard>, generic: u32, types: &[CoreType]| CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::Cost { shards, generic },
            card_type: CardType {
                supertypes: vec![],
                core_types: types.to_vec(),
                subtypes: vec![],
            },
            ..Default::default()
        };
    let mut zirda = face(
        ZIRDA,
        vec![ManaCostShard::RedWhite, ManaCostShard::RedWhite],
        1,
        &[CoreType::Creature],
    );
    zirda.keywords = vec![Keyword::Companion(
        CompanionCondition::PermanentsHaveActivatedAbilities,
    )];
    let mut lurrus = face(
        LURRUS,
        vec![ManaCostShard::WhiteBlack, ManaCostShard::WhiteBlack],
        1,
        &[CoreType::Creature],
    );
    lurrus.keywords = vec![Keyword::Companion(
        CompanionCondition::MaxPermanentManaValue(2),
    )];
    let mut invasion = face(
        "Invasion of Zendikar",
        vec![ManaCostShard::Green],
        3,
        &[CoreType::Battle],
    );
    invasion.card_type.subtypes = vec!["Siege".to_string()];
    let vegetation = face(
        "Explosive Vegetation",
        vec![ManaCostShard::Green],
        3,
        &[CoreType::Sorcery],
    );
    // CR 305.6: a basic Mountain has an intrinsic mana ability, so it satisfies Zirda.
    let mut mountain = face("Mountain", vec![], 0, &[CoreType::Land]);
    mountain.mana_cost = ManaCost::NoCost;
    mountain.card_type.subtypes = vec!["Mountain".to_string()];

    let export: serde_json::Map<String, serde_json::Value> =
        [&zirda, &lurrus, &invasion, &vegetation, &mountain]
            .into_iter()
            .map(|card| {
                (
                    card.name.to_lowercase(),
                    serde_json::to_value(card).expect("CardFace serializes"),
                )
            })
            .collect();
    let db = CardDatabase::from_json_str(&serde_json::Value::Object(export).to_string())
        .expect("synthetic export parses");

    for companion in [ZIRDA, LURRUS] {
        for (odd_card, legal) in [
            ("Invasion of Zendikar", false),
            ("Explosive Vegetation", true),
        ] {
            let mut main_deck = vec![odd_card.to_string()];
            main_deck.extend(vec!["Mountain".to_string(); 39]);

            // Public deck submission.
            let request: DeckCompatibilityRequest = serde_json::from_value(serde_json::json!({
                "main_deck": main_deck,
                "companion": [companion],
                "selected_format": "Limited",
            }))
            .expect("request deserializes");
            let result = evaluate_deck_compatibility(&db, &request);
            assert!(result.unknown_cards.is_empty(), "every card resolves");
            assert_eq!(
                result.selected_format_compatible,
                Some(legal),
                "{companion} with {odd_card}: {:?}",
                result.selected_format_reasons
            );
            assert_eq!(
                result
                    .selected_format_reasons
                    .iter()
                    .any(|reason| reason.contains("not a legal companion")),
                !legal,
                "{companion} with {odd_card}: {:?}",
                result.selected_format_reasons
            );

            // Pregame reveal of the same deck through the production loader.
            let list = DeckList {
                player: PlayerDeckList {
                    main_deck,
                    companion: vec![companion.to_string()],
                    ..Default::default()
                },
                opponent: PlayerDeckList {
                    main_deck: vec!["Mountain".to_string(); 40],
                    ..Default::default()
                },
                ..Default::default()
            };
            let payload = resolve_deck_list(&db, &engine::database::ComboTable::default(), &list);
            let mut state = GameState::new(FormatConfig::limited(), 2, 42);
            load_deck_into_state(&mut state, &payload);
            let start = start_game_with_starting_player(&mut state, P0);
            let offered: Vec<String> = match start.waiting_for {
                WaitingFor::CompanionReveal {
                    eligible_companions,
                    ..
                } => eligible_companions.into_iter().map(|c| c.name).collect(),
                WaitingFor::MulliganDecision { .. } => Vec::new(),
                other => panic!("unexpected waiting state: {other:?}"),
            };
            assert_eq!(
                offered.iter().any(|name| name == companion),
                legal,
                "{companion} with {odd_card}: offered {offered:?}"
            );
        }
    }
}
