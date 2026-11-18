//! Headless full-game proof for Horde Magic (the Zombies Horde).
//!
//! This is the end-to-end integration path automated tests hadn't yet covered:
//! the AI driving BOTH seats of a real Horde game through to `GameOver` —
//!   - seat 0 = the Horde AI (whose precombat main fires the engine's
//!     reveal-and-cast wave; its creatures have haste + must-attack), and
//!   - seat 1 = a survivor AI playing a normal green deck.
//!
//! Uses the ZOMBIES Horde deck, not Cyberman: every card in it is fully
//! implemented, so this sim validates the Horde SPINE and AI end to end without
//! being hostage to incomplete cards. (The Cyberman deck's Universes Beyond
//! cards are known-incomplete — e.g. Missy's villainous-choice draw branch —
//! and a long enough game trips them; that is a card-fidelity gap, not a spine
//! bug, and it does not belong in a spine regression test.)
//!
//! The construction mirrors the production init path exactly:
//!   `GameState::new(FormatConfig::horde(..))` → `load_and_hydrate_decks`
//!   (which injects the ~300-card Cyberman library on the Horde seat and grants
//!   the Horde game-start emblem) → `engine::game::engine::start_game` → the
//!   `run_ai_actions` drive loop (the same loop `ai_duel` / `greasefang_bounded`
//!   use).
//!
//! `#[ignore]` because it loads a card DB and plays a full game (heavy); opt in
//! with `cargo test -p phase-ai --test horde_full_game -- --ignored --nocapture`.
//! It prefers the real `client/public/card-data.json` and falls back to the
//! committed `crates/engine/tests/fixtures/who_horde_cards.json` subset.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, DeckEntry, DeckPayload, PlayerDeckPayload,
};
use engine::types::card_type::CoreType;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use rand::rngs::StdRng;
use rand::SeedableRng;

const HORDE: PlayerId = PlayerId(0);
const SURVIVOR: PlayerId = PlayerId(1);

/// Total AI-action safety bound. A full Horde beatdown terminates well within
/// this; the bound only guards against a non-terminating wave / priority loop.
const MAX_TOTAL_ACTIONS: usize = 60_000;
/// Turn safety cap — a Horde game that hasn't ended by here is a stall.
const MAX_TURN: u32 = 400;

/// Load the real card DB, or fall back to the committed WHO fixture subset.
/// Returns `(db, source_label)`.
fn load_db() -> (CardDatabase, &'static str) {
    let real = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("client")
        .join("public")
        .join("card-data.json");
    if real.exists() {
        match CardDatabase::from_export(&real) {
            Ok(db) => return (db, "client/public/card-data.json"),
            Err(e) => eprintln!(
                "real card DB {} failed to load ({e}); falling back to fixture",
                real.display()
            ),
        }
    }
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("engine")
        .join("tests")
        .join("fixtures")
        .join("who_horde_cards.json");
    let db = CardDatabase::from_export(&fixture)
        .unwrap_or_else(|e| panic!("load WHO fixture {}: {e}", fixture.display()));
    (db, "fixtures/who_horde_cards.json")
}

/// A 2-player Horde payload: seat 0 (Horde) submits nothing — its library is
/// engine-supplied — and seat 1 (survivor) brings a simple green deck of
/// Forests plus whatever cheap creatures resolve in the loaded DB.
fn horde_payload(db: &CardDatabase) -> DeckPayload {
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest must resolve in the card DB")
        .clone();

    // Cheap green creatures that (a) exist widely and (b) resolve without
    // targeting requirements the survivor AI could stall on. Only the ones
    // present in the loaded DB are added — the fixture may have none, in which
    // case the survivor plays a mono-Forest deck (still a valid, terminating
    // game: the Horde beats it down).
    let creature_candidates = [
        "Grizzly Bears",
        "Llanowar Elves",
        "Elvish Mystic",
        "Centaur Courser",
        "Runeclaw Bear",
        "Alpine Grizzly",
    ];
    let mut main_deck = vec![DeckEntry {
        card: forest,
        count: 30,
    }];
    let mut creatures_added = 0u32;
    for name in creature_candidates {
        if let Some(face) = db.get_face_by_name(name) {
            main_deck.push(DeckEntry {
                card: face.clone(),
                count: 3,
            });
            creatures_added += 3;
            if creatures_added >= 12 {
                break;
            }
        }
    }
    // Trim Forests so the deck is ~40 cards.
    if let Some(first) = main_deck.first_mut() {
        first.count = 40u32.saturating_sub(creatures_added).max(20);
    }

    DeckPayload {
        player: PlayerDeckPayload::default(),
        opponent: PlayerDeckPayload {
            main_deck,
            ..Default::default()
        },
        ai_decks: Vec::new(),
        ai_difficulties: Vec::new(),
        booster_pack_pool: None,
    }
}

/// Evidence that the Horde mechanics actually fired under AI control.
#[derive(Default, Debug)]
struct Evidence {
    max_horde_tokens: usize,
    max_horde_nontoken_creatures: usize,
    horde_ever_attacked: bool,
    max_horde_attackers: usize,
    horde_graveyard_peak: usize,
    horde_library_start: usize,
    horde_library_min: usize,
}

fn horde_battlefield_tokens(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == HORDE && o.is_token)
        })
        .count()
}

fn horde_battlefield_nontoken_creatures(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.controller == HORDE
                    && !o.is_token
                    && o.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .count()
}

fn horde_attacker_count(state: &GameState) -> usize {
    let Some(combat) = state.combat.as_ref() else {
        return 0;
    };
    combat
        .attackers
        .iter()
        .filter(|a| {
            state
                .objects
                .get(&a.object_id)
                .is_some_and(|o| o.controller == HORDE)
        })
        .count()
}

fn observe(ev: &mut Evidence, state: &GameState) {
    ev.max_horde_tokens = ev.max_horde_tokens.max(horde_battlefield_tokens(state));
    ev.max_horde_nontoken_creatures = ev
        .max_horde_nontoken_creatures
        .max(horde_battlefield_nontoken_creatures(state));
    let attackers = horde_attacker_count(state);
    if attackers > 0 {
        ev.horde_ever_attacked = true;
        ev.max_horde_attackers = ev.max_horde_attackers.max(attackers);
    }
    let gy = state.players[HORDE.0 as usize].graveyard.len();
    ev.horde_graveyard_peak = ev.horde_graveyard_peak.max(gy);
    let lib = state.players[HORDE.0 as usize].library.len();
    ev.horde_library_min = ev.horde_library_min.min(lib);
}

#[test]
#[ignore = "loads card-data.json + plays a full Horde game; opt in via --ignored --nocapture"]
fn ai_plays_full_horde_game_to_completion() {
    let seed = 42u64;
    let (db, db_source) = load_db();
    eprintln!("card DB source: {db_source}");
    // The Zombies Horde deck is built from real Magic cards across many sets; the
    // committed WHO fixture only holds Cyberman-deck cards, so it cannot serve
    // this deck. Skip (rather than fall back) when the full DB is absent — the
    // WHO fixture is only useful for a WHO deck.
    if db_source != "client/public/card-data.json" {
        eprintln!("skipping: Zombies Horde needs the full card DB, not the WHO fixture");
        return;
    }

    // --- Construction: exactly the production init path. ---
    let mut state = GameState::new(
        FormatConfig::horde(ChallengeDeck::ZombiesHorde.default_ruleset()),
        2,
        seed,
    );
    // FormatConfig::horde already pins archenemy_player = seat 0 (the Horde).
    assert_eq!(
        state.format_config.archenemy_player(),
        Some(HORDE),
        "the Horde must occupy the archenemy seat (seat 0)"
    );

    // The format's own policy: in Horde, survivors take the first turn(s) to set
    // up before the Horde (CR 805-style setup turns, PR6). Record it so we can
    // compare against what `start_game` actually does.
    let format_starting_player = state.format_config.starting_player();
    let setup_turns = state
        .format_config
        .horde_ruleset
        .as_ref()
        .map(|r| r.survivor_setup_turns)
        .unwrap_or(0);

    let payload = horde_payload(&db);
    load_and_hydrate_decks(&mut state, &payload, Some(&db));
    engine::game::engine::start_game(&mut state);

    let post_start_active = state.active_player;
    let post_start_turn = state.turn_number;
    eprintln!(
        "format_config.starting_player() = seat {} (survivor setup_turns = {setup_turns}); \
         after start_game: turn {post_start_turn}, active seat {}",
        format_starting_player.0, post_start_active.0
    );

    let horde_lib_start = state.players[HORDE.0 as usize].library.len();
    eprintln!(
        "Horde library injected: {horde_lib_start} cards; survivor library: {} cards",
        state.players[SURVIVOR.0 as usize].library.len()
    );
    assert!(
        horde_lib_start > 0,
        "the Horde seat must have an engine-supplied library"
    );

    // --- Both seats are AI. ---
    let ai_players: HashSet<PlayerId> = [HORDE, SURVIVOR].into_iter().collect();
    // Easy + measurement mode: deterministic, node-bounded search (fast, no
    // wall-clock variance) — mirrors ai_duel's regression configuration.
    let horde_cfg =
        create_config_for_players(AiDifficulty::Easy, Platform::Native, 2).into_measurement(seed);
    let survivor_cfg = create_config_for_players(AiDifficulty::Easy, Platform::Native, 2)
        .into_measurement(seed.wrapping_add(1));
    let ai_configs: HashMap<PlayerId, _> = [(HORDE, horde_cfg), (SURVIVOR, survivor_cfg)]
        .into_iter()
        .collect();

    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);

    let mut ev = Evidence {
        horde_library_start: horde_lib_start,
        horde_library_min: horde_lib_start,
        ..Default::default()
    };

    let mut total_actions: usize = 0;
    let mut last_turn: u32 = 0;
    let mut stalled_waiting: Option<String> = None;

    observe(&mut ev, &state);

    loop {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            break;
        }
        if state.turn_number != last_turn {
            last_turn = state.turn_number;
            eprintln!(
                "=== Turn {last_turn} (active seat {}) — horde bf tokens={}, horde nontoken creatures={}, horde lib={}, actions={total_actions} ===",
                state.active_player.0,
                horde_battlefield_tokens(&state),
                horde_battlefield_nontoken_creatures(&state),
                state.players[HORDE.0 as usize].library.len(),
            );
        }

        let results = run_ai_actions(
            &mut state,
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
        );
        observe(&mut ev, &state);

        if results.is_empty() {
            if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
                break;
            }
            // The AI produced no action and the game is not over — a stuck
            // WaitingFor the AI cannot answer. This is the highest-value bug.
            stalled_waiting = Some(format!("{:?}", state.waiting_for));
            break;
        }
        total_actions += results.len();

        if total_actions >= MAX_TOTAL_ACTIONS {
            stalled_waiting = Some(format!(
                "ACTION BOUND {MAX_TOTAL_ACTIONS} hit at turn {} — waiting_for={:?}",
                state.turn_number, state.waiting_for
            ));
            break;
        }
        if state.turn_number >= MAX_TURN {
            stalled_waiting = Some(format!(
                "TURN CAP {MAX_TURN} hit — waiting_for={:?}",
                state.waiting_for
            ));
            break;
        }
    }

    let winner = match &state.waiting_for {
        WaitingFor::GameOver { winner } => *winner,
        _ => None,
    };

    // --- Report. ---
    eprintln!("\n================ HORDE FULL-GAME REPORT ================");
    eprintln!("card DB source        : {db_source}");
    eprintln!(
        "reached GameOver      : {}",
        winner.is_some() || matches!(state.waiting_for, WaitingFor::GameOver { .. })
    );
    eprintln!(
        "winner                : {}",
        match winner {
            Some(HORDE) => "HORDE (seat 0)".to_string(),
            Some(p) => format!("SURVIVOR (seat {})", p.0),
            None => "none / aborted".to_string(),
        }
    );
    eprintln!("final turn number     : {}", state.turn_number);
    eprintln!("--- setup-turn check ---");
    eprintln!(
        "format starting_player: seat {} (survivors-first by design; setup_turns={setup_turns})",
        format_starting_player.0
    );
    eprintln!(
        "start_game gave turn {post_start_turn} to seat {} {}",
        post_start_active.0,
        if post_start_active == format_starting_player {
            "(matches format policy)"
        } else {
            "(!! BYPASSES survivor setup turns — start_game forces the archenemy first)"
        }
    );
    eprintln!("total AI actions      : {total_actions}");
    eprintln!(
        "final waiting_for     : {:?}",
        std::mem::discriminant(&state.waiting_for)
    );
    eprintln!("--- Horde wave evidence ---");
    eprintln!("horde library start   : {}", ev.horde_library_start);
    eprintln!("horde library min     : {}", ev.horde_library_min);
    eprintln!(
        "horde library consumed: {}",
        ev.horde_library_start.saturating_sub(ev.horde_library_min)
    );
    eprintln!(
        "horde graveyard peak  : {} (mill evidence)",
        ev.horde_graveyard_peak
    );
    eprintln!("max horde tokens on bf: {}", ev.max_horde_tokens);
    eprintln!(
        "max horde nontoken crt: {} (free-cast evidence)",
        ev.max_horde_nontoken_creatures
    );
    eprintln!("horde ever attacked   : {}", ev.horde_ever_attacked);
    eprintln!("max horde attackers   : {}", ev.max_horde_attackers);
    if let Some(reason) = &stalled_waiting {
        eprintln!("!!! STALL/ABORT       : {reason}");
    }
    eprintln!("=======================================================\n");

    // --- Assertions. ---
    assert!(
        stalled_waiting.is_none(),
        "the AI-driven Horde game did NOT play to completion: {}",
        stalled_waiting.unwrap()
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "the game must reach GameOver (waiting_for={:?}, turn={})",
        state.waiting_for,
        state.turn_number
    );
    // Evidence the wave ran under AI control: the Horde must have put at least
    // one permanent onto the battlefield (token or free-cast nontoken).
    assert!(
        ev.max_horde_tokens > 0 || ev.max_horde_nontoken_creatures > 0,
        "the Horde reveal-and-cast wave must have produced at least one permanent \
         (tokens={}, nontoken creatures={})",
        ev.max_horde_tokens,
        ev.max_horde_nontoken_creatures,
    );
}

/// Outcome of one headless Horde game (for the multi-seed robustness sweep).
struct Outcome {
    reached_game_over: bool,
    winner: Option<PlayerId>,
    turn: u32,
    actions: usize,
    evidence: Evidence,
    stall: Option<String>,
}

/// Construct + AI-drive one Horde game to completion (or stall). Same production
/// init path as the detailed test above; no per-turn logging.
fn play_once(db: &CardDatabase, seed: u64) -> Outcome {
    let mut state = GameState::new(
        FormatConfig::horde(ChallengeDeck::ZombiesHorde.default_ruleset()),
        2,
        seed,
    );
    let payload = horde_payload(db);
    load_and_hydrate_decks(&mut state, &payload, Some(db));
    engine::game::engine::start_game(&mut state);

    let horde_lib_start = state.players[HORDE.0 as usize].library.len();
    let ai_players: HashSet<PlayerId> = [HORDE, SURVIVOR].into_iter().collect();
    let ai_configs: HashMap<PlayerId, _> = [
        (
            HORDE,
            create_config_for_players(AiDifficulty::Easy, Platform::Native, 2)
                .into_measurement(seed),
        ),
        (
            SURVIVOR,
            create_config_for_players(AiDifficulty::Easy, Platform::Native, 2)
                .into_measurement(seed.wrapping_add(1)),
        ),
    ]
    .into_iter()
    .collect();
    let mut ai_rng = StdRng::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);

    let mut ev = Evidence {
        horde_library_start: horde_lib_start,
        horde_library_min: horde_lib_start,
        ..Default::default()
    };
    let mut total_actions = 0usize;
    let mut stall = None;
    observe(&mut ev, &state);

    loop {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            break;
        }
        let results = run_ai_actions(
            &mut state,
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
        );
        observe(&mut ev, &state);
        if results.is_empty() {
            if !matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
                stall = Some(format!("stuck waiting_for={:?}", state.waiting_for));
            }
            break;
        }
        total_actions += results.len();
        if total_actions >= MAX_TOTAL_ACTIONS {
            stall = Some(format!("action bound at turn {}", state.turn_number));
            break;
        }
        if state.turn_number >= MAX_TURN {
            stall = Some("turn cap".to_string());
            break;
        }
    }

    let winner = match &state.waiting_for {
        WaitingFor::GameOver { winner } => *winner,
        _ => None,
    };
    Outcome {
        reached_game_over: matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        winner,
        turn: state.turn_number,
        actions: total_actions,
        evidence: ev,
        stall,
    }
}

/// Robustness sweep: every seed in a small set must play to completion with no
/// stall/deadlock and with the Horde wave firing (a permanent produced under AI
/// control). Proves the single-seed result above is not a lucky RNG path.
#[test]
#[ignore = "loads card-data.json + plays several full Horde games; opt in via --ignored --nocapture"]
fn ai_plays_horde_games_across_seeds_without_stalling() {
    let (db, db_source) = load_db();
    eprintln!("card DB source: {db_source}");
    // The Zombies Horde deck is built from real Magic cards across many sets; the
    // committed WHO fixture only holds Cyberman-deck cards, so it cannot serve
    // this deck. Skip (rather than fall back) when the full DB is absent — the
    // WHO fixture is only useful for a WHO deck.
    if db_source != "client/public/card-data.json" {
        eprintln!("skipping: Zombies Horde needs the full card DB, not the WHO fixture");
        return;
    }
    let seeds: [u64; 6] = [1, 7, 42, 99, 2024, 31337];
    let mut all_ok = true;
    for seed in seeds {
        let o = play_once(&db, seed);
        let produced_permanent =
            o.evidence.max_horde_tokens > 0 || o.evidence.max_horde_nontoken_creatures > 0;
        eprintln!(
            "seed {seed:>6}: game_over={} winner={:?} turn={} actions={} horde_tokens={} horde_nontoken={} attacked={} milled_gy={} stall={:?}",
            o.reached_game_over,
            o.winner.map(|p| p.0),
            o.turn,
            o.actions,
            o.evidence.max_horde_tokens,
            o.evidence.max_horde_nontoken_creatures,
            o.evidence.horde_ever_attacked,
            o.evidence.horde_graveyard_peak,
            o.stall,
        );
        if !o.reached_game_over || o.stall.is_some() || !produced_permanent {
            all_ok = false;
        }
    }
    assert!(
        all_ok,
        "at least one seed failed to play a complete Horde game (see per-seed lines above)"
    );
}
