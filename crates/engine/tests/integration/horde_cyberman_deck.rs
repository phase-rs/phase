//! Horde Magic PR5 — the Doctor Who "Cyberman Horde" deck + `UntilNonToken` wave.
//!
//! Two seams are exercised:
//!
//!   1. `WaveTermination::UntilNonToken` (synthetic-library tests, no card DB):
//!      a wave reveals-and-resolves cards until it casts the FIRST non-token
//!      card, which ENDS the wave. All tokens revealed before it enter the
//!      battlefield; the next card stays in the library.
//!   2. Seat-scoped deck injection (full-card-DB tests): loading a Horde game
//!      through `load_and_hydrate_decks` puts the ~300-card Cyberman library on
//!      the Horde seat (real non-token cards + `is_horde_deck_token` tokens), all
//!      owned by the Horde, while survivors keep their submitted decks.
//!
//! Seat 0 = Horde, seat 1 = survivor. The full-DB tests skip gracefully when
//! `client/public/card-data.json` is absent (CI without the card-data pipeline).

use std::path::Path;

use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, DeckEntry, DeckPayload, PlayerDeckPayload,
};
use engine::game::decks::cyberman_horde::CYBERMAN_HORDE_NONTOKEN_CARDS;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HORDE: PlayerId = PlayerId(0);
const SURVIVOR: PlayerId = PlayerId(1);

// ---------------------------------------------------------------------------
// UntilNonToken wave semantics (synthetic library — no card DB needed).
// ---------------------------------------------------------------------------

/// A Horde game whose ruleset uses the default (`UntilNonToken`) wave, parked at
/// the Horde's Upkeep with priority on a mid-game turn (turn 4 avoids any
/// first-turn draw-skip interaction).
fn horde_until_nontoken_game() -> GameState {
    let mut state = GameState::new(
        FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
        2,
        42,
    );
    state.turn_number = 4;
    state.active_player = HORDE;
    state.phase = Phase::Upkeep;
    state.priority_player = HORDE;
    state.waiting_for = WaitingFor::Priority { player: HORDE };
    state
}

/// Place a creature token on top of `owner`'s library, flagged `is_horde_deck_token`.
fn add_library_token(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    p: i32,
    t: i32,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.is_token = true;
    obj.is_horde_deck_token = true;
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(p);
    obj.toughness = Some(t);
    obj.base_power = Some(p);
    obj.base_toughness = Some(t);
    id
}

/// Place a vanilla castable (non-token) creature on top of `owner`'s library.
fn add_library_creature(state: &mut GameState, owner: PlayerId, name: &str, mv: u32) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: mv,
    };
    obj.power = Some(mv as i32);
    obj.toughness = Some(mv as i32);
    obj.base_power = Some(mv as i32);
    obj.base_toughness = Some(mv as i32);
    id
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

/// Run the Horde's precombat-main wave to completion (bounded).
fn run_wave(runner: &mut GameRunner) {
    runner.advance_to_phase(Phase::PreCombatMain);
    for _ in 0..80 {
        if runner.state().horde_wave_remaining == 0 && runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// PRIMARY DISCRIMINATING TEST for `UntilNonToken`. An `UntilNonToken` wave over
/// `[token, token, nontoken, nontoken]` reveals BOTH tokens onto the battlefield
/// and casts EXACTLY ONE non-token (the first), then stops — the second
/// non-token stays in the library.
///
/// Revert guard: dropping the `horde_wave_remaining = 0` set in the non-token
/// branch of `maybe_reveal_next` lets the wave continue (seed = library size), so
/// the SECOND non-token is also cast — `nontoken creatures == 1` flips to 2 and
/// the "still in library" assertion flips too.
#[test]
fn until_nontoken_reveals_tokens_then_one_nontoken_and_stops() {
    let mut state = horde_until_nontoken_game();
    // Reveal order = creation order (front of library first).
    add_library_token(&mut state, HORDE, "Cyberman", 2, 2);
    add_library_token(&mut state, HORDE, "Dalek", 3, 3);
    let _first_nontoken = add_library_creature(&mut state, HORDE, "First Nontoken", 2);
    let second_nontoken = add_library_creature(&mut state, HORDE, "Second Nontoken", 2);

    let mut runner = GameRunner::from_state(state);
    run_wave(&mut runner);

    assert_eq!(
        horde_battlefield_tokens(runner.state()),
        2,
        "both tokens revealed before the non-token must enter the battlefield"
    );
    assert_eq!(
        horde_battlefield_nontoken_creatures(runner.state()),
        1,
        "exactly ONE non-token must be cast — the first non-token ends the wave"
    );
    assert!(
        runner.state().players[HORDE.0 as usize]
            .library
            .contains(&second_nontoken),
        "the second non-token must stay in the library (the wave ended before it)"
    );
    assert_eq!(
        runner.state().horde_wave_remaining,
        0,
        "the wave counter must be cleared once the wave ends"
    );
}

/// An all-token `UntilNonToken` library reveals EVERY token, then ends when the
/// library empties (the safety bound = library size).
///
/// Revert guard: if `UntilNonToken` seeded `0` (like a missing policy) instead of
/// the library size, no token would be revealed — the count flips from 3 to 0.
#[test]
fn until_nontoken_all_token_library_reveals_all_then_ends() {
    let mut state = horde_until_nontoken_game();
    add_library_token(&mut state, HORDE, "Cyberman", 2, 2);
    add_library_token(&mut state, HORDE, "Dalek", 3, 3);
    add_library_token(&mut state, HORDE, "Cyberman", 2, 2);

    let mut runner = GameRunner::from_state(state);
    run_wave(&mut runner);

    assert_eq!(
        horde_battlefield_tokens(runner.state()),
        3,
        "every token in an all-token library must enter the battlefield"
    );
    assert!(
        runner.state().players[HORDE.0 as usize].library.is_empty(),
        "the library must be empty once every token has been revealed"
    );
    assert_eq!(
        runner.state().horde_wave_remaining,
        0,
        "the wave must end cleanly when the library empties"
    );
}

// ---------------------------------------------------------------------------
// Seat-scoped deck injection (full card DB).
// ---------------------------------------------------------------------------

/// Load the Cyberman-Horde card fixture (`tests/fixtures/who_horde_cards.json`),
/// a subset of the full export carrying exactly the WHO cards this deck needs
/// plus a basic land for the survivor. The shared `integration_cards.json`
/// fixture does not include the WHO cards, and the full 92 MB export cannot be
/// deserialized on this branch (it is produced by a newer engine that emits
/// `FilterProp` variants this branch predates), so a curated subset is used.
/// Returns `None` when the fixture is absent so CI without the card-data pipeline
/// skips these tests gracefully.
fn full_card_db() -> Option<CardDatabase> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/who_horde_cards.json");
    if !path.exists() {
        eprintln!("skipping: tests/fixtures/who_horde_cards.json not generated");
        return None;
    }
    Some(CardDatabase::from_export(&path).expect("WHO card fixture should load"))
}

/// A 2-player Horde deck payload: seat 0 (Horde) submits nothing — its library is
/// engine-supplied — and seat 1 (survivor) brings a minimal real deck.
fn horde_payload(db: &CardDatabase) -> DeckPayload {
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest must resolve")
        .clone();
    DeckPayload {
        player: PlayerDeckPayload::default(),
        opponent: PlayerDeckPayload {
            main_deck: vec![DeckEntry {
                card: forest,
                count: 40,
            }],
            ..Default::default()
        },
        ai_decks: Vec::new(),
        ai_difficulties: Vec::new(),
        booster_pack_pool: None,
    }
}

fn new_horde_state() -> GameState {
    GameState::new(
        FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
        2,
        42,
    )
}

fn horde_library_ids(state: &GameState) -> Vec<ObjectId> {
    state.players[HORDE.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

/// Loading a Horde game through the real deck-load path puts a 300-card library
/// on the Horde seat: 100 real (non-token) card objects + 200
/// `is_token` + `is_horde_deck_token` tokens, all owned by the Horde. Survivors keep
/// their submitted deck (seat-scoped, not Momir's all-seat overwrite).
///
/// Revert guard: dropping the `GameFormat::Horde` arm in `load_and_hydrate_decks`
/// leaves the Horde library empty — every count assertion flips to 0.
#[test]
fn cyberman_horde_deck_loads_seat_scoped_300_card_library() {
    let Some(db) = full_card_db() else {
        return;
    };
    let mut state = new_horde_state();
    let payload = horde_payload(&db);
    load_and_hydrate_decks(&mut state, &payload, Some(&db));

    let lib = horde_library_ids(&state);
    assert_eq!(
        lib.len(),
        300,
        "the Horde library must be exactly 300 cards"
    );

    let mut nontokens = 0usize;
    let mut tokens = 0usize;
    for id in &lib {
        let obj = state.objects.get(id).expect("library object exists");
        assert_eq!(
            obj.owner, HORDE,
            "every Horde library object is owned by the Horde"
        );
        assert_eq!(
            obj.zone,
            Zone::Library,
            "every injected object is in the library"
        );
        if obj.is_token {
            tokens += 1;
            assert!(
                obj.is_horde_deck_token,
                "a Horde library token must carry the cease-to-exist exemption flag"
            );
        } else {
            nontokens += 1;
            assert!(
                obj.printed_ref.is_some(),
                "a non-token Horde library card must resolve to a real printed card ({})",
                obj.name
            );
        }
    }
    assert_eq!(nontokens, 100, "100 non-token real cards");
    assert_eq!(tokens, 200, "200 predefined tokens");

    // Seat-scoping: the survivor's submitted deck is untouched (40 Forests).
    assert_eq!(
        state.players[SURVIVOR.0 as usize].library.len(),
        40,
        "the survivor keeps their submitted deck — the Horde inject is seat-scoped"
    );
}

/// Coverage: every distinct non-token name in the Cyberman decklist resolves to a
/// real `CardFace` in the card DB. This is the explicit form of the panic guard
/// inside `load_horde_library`.
#[test]
fn cyberman_horde_every_nontoken_name_resolves() {
    let Some(db) = full_card_db() else {
        return;
    };
    for (_, name) in CYBERMAN_HORDE_NONTOKEN_CARDS {
        assert!(
            db.get_face_by_name(name).is_some(),
            "Cyberman Horde non-token card '{name}' must resolve in the card DB"
        );
    }
}

/// End-to-end: a token from the REAL loaded deck reveals onto the battlefield, and
/// the `UntilNonToken` wave ends after the first non-token. The Horde library is
/// reordered so its top three cards are `[token, token, "Cyberman Patrol"]`
/// (Cyberman Patrol is a vanilla-bodied 2/2 with only a static ability — no ETB
/// target requirement), making the reveal deterministic despite the shuffle.
///
/// Revert guard: reverting the token reveal branch leaves 0 battlefield tokens;
/// reverting the `UntilNonToken` termination casts past the first non-token, so
/// the library would shrink by more than 3.
#[test]
fn cyberman_horde_wave_reveals_real_tokens_and_one_nontoken() {
    let Some(db) = full_card_db() else {
        return;
    };
    let mut state = new_horde_state();
    let payload = horde_payload(&db);
    load_and_hydrate_decks(&mut state, &payload, Some(&db));

    // Pick two library tokens and one Cyberman Patrol (non-token creature).
    let lib = horde_library_ids(&state);
    let token_ids: Vec<ObjectId> = lib
        .iter()
        .copied()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
        .take(2)
        .collect();
    assert_eq!(
        token_ids.len(),
        2,
        "the deck must contain at least two tokens"
    );
    let patrol = lib
        .iter()
        .copied()
        .find(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| !o.is_token && o.name == "Cyberman Patrol")
        })
        .expect("the deck must contain a Cyberman Patrol");

    // Reorder: put [token, token, Cyberman Patrol] on top (front = revealed first).
    let front = [token_ids[0], token_ids[1], patrol];
    {
        let library = &mut state.players[HORDE.0 as usize].library;
        library.retain(|id| !front.contains(id));
        for id in front.iter().rev() {
            library.push_front(*id);
        }
    }
    let library_before = state.players[HORDE.0 as usize].library.len();

    // Park the Horde's turn and drive the wave.
    state.turn_number = 4;
    state.active_player = HORDE;
    state.phase = Phase::Upkeep;
    state.priority_player = HORDE;
    state.waiting_for = WaitingFor::Priority { player: HORDE };
    let mut runner = GameRunner::from_state(state);
    run_wave(&mut runner);

    assert_eq!(
        horde_battlefield_tokens(runner.state()),
        2,
        "both revealed library tokens must enter the battlefield under the Horde"
    );
    assert!(
        runner.state().battlefield.iter().any(|id| *id == patrol),
        "the first non-token (Cyberman Patrol) must resolve onto the battlefield"
    );
    assert_eq!(
        runner.state().horde_wave_remaining,
        0,
        "the wave must end after casting the first non-token"
    );
    assert_eq!(
        runner.state().players[HORDE.0 as usize].library.len(),
        library_before - 3,
        "exactly three cards (2 tokens + 1 non-token) leave the library this wave"
    );
}
