use std::cell::{Cell, RefCell};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use draft_core::pack_generator::PackGenerator;
use draft_core::session;
use draft_core::set_pool::LimitedSetPool;
use draft_core::types::*;
use draft_core::view::filter_for_player;
use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;

mod bot_ai;
mod suggest;

thread_local! {
    /// Draft session state uses Cell<Option<T>> with take/set to avoid RefCell
    /// borrow poisoning — same panic-resilient pattern as engine-wasm.
    static DRAFT_SESSION: Cell<Option<DraftSession>> = const { Cell::new(None) };
    static PACK_GEN: Cell<Option<PackGenerator>> = const { Cell::new(None) };
    static DIFFICULTY: Cell<AiDifficulty> = const { Cell::new(AiDifficulty::Medium) };
    static RNG: Cell<Option<ChaCha20Rng>> = const { Cell::new(None) };
    /// Per RESEARCH Pitfall 2: draft-wasm has its own CardDatabase, separate
    /// from engine-wasm's thread-local. The frontend loads card-data.json into
    /// draft-wasm independently for Hard/VeryHard bot evaluation.
    static CARD_DB: RefCell<Option<CardDatabase>> = const { RefCell::new(None) };
}

/// Serialize a Rust value to a JS object via JSON.
/// Same pattern as engine-wasm: serde_json -> JSON.parse.
fn to_js<T: Serialize + ?Sized>(value: &T) -> JsValue {
    let json = serde_json::to_string(value)
        .unwrap_or_else(|e| panic!("serde_json serialization failed: {e}"));
    js_sys::JSON::parse(&json).unwrap_or_else(|e| panic!("JSON.parse failed: {e:?}"))
}

/// Take the draft session out of the Cell, pass it to a closure, then put it back.
fn with_draft<R>(f: impl FnOnce(&DraftSession) -> R) -> Result<R, JsValue> {
    DRAFT_SESSION.with(|cell| {
        let session = cell
            .take()
            .ok_or_else(|| JsValue::from_str("Draft not initialized"))?;
        let result = f(&session);
        cell.set(Some(session));
        Ok(result)
    })
}

/// Take the draft session out of the Cell, pass it mutably, then put it back.
fn with_draft_mut<R>(f: impl FnOnce(&mut DraftSession) -> Result<R, JsValue>) -> Result<R, JsValue> {
    DRAFT_SESSION.with(|cell| {
        let mut session = cell
            .take()
            .ok_or_else(|| JsValue::from_str("Draft not initialized"))?;
        let result = f(&mut session);
        cell.set(Some(session));
        result
    })
}

/// Map a u8 difficulty value to AiDifficulty.
/// Per T-55-02: clamp to 0..=4, default to Medium for out-of-range.
fn map_difficulty(val: u8) -> AiDifficulty {
    match val {
        0 => AiDifficulty::VeryEasy,
        1 => AiDifficulty::Easy,
        2 => AiDifficulty::Medium,
        3 => AiDifficulty::Hard,
        4 => AiDifficulty::VeryHard,
        _ => AiDifficulty::Medium,
    }
}

/// Initialize panic hook for better error messages in WASM.
#[wasm_bindgen(start)]
pub fn init_panic_hook() {
    console_error_panic_hook::set_once();
}

/// Load the card database from a JSON string (card-data.json contents).
/// Required for Hard/VeryHard bot AI evaluation and accurate deck suggestion.
/// Returns the number of cards loaded.
#[wasm_bindgen]
pub fn load_card_database(json_str: &str) -> Result<u32, JsValue> {
    let db = CardDatabase::from_json_str(json_str)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse card database: {}", e)))?;
    let count = db.card_count() as u32;
    CARD_DB.with(|cell| {
        *cell.borrow_mut() = Some(db);
    });
    Ok(count)
}

/// Start a Quick Draft session: 1 human + 7 bots.
///
/// - `set_pool_json`: serialized LimitedSetPool from draft-pools.json
/// - `difficulty`: 0=VeryEasy, 1=Easy, 2=Medium, 3=Hard, 4=VeryHard
/// - `seed`: RNG seed for deterministic pack generation
///
/// Returns the initial DraftPlayerView as a JS object.
#[wasm_bindgen]
pub fn start_quick_draft(
    set_pool_json: &str,
    difficulty: u8,
    seed: u32,
) -> Result<JsValue, JsValue> {
    let set_pool: LimitedSetPool = serde_json::from_str(set_pool_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse set pool: {}", e)))?;

    let ai_difficulty = map_difficulty(difficulty);
    let set_code = set_pool.code.clone();

    let config = DraftConfig {
        set_code,
        kind: DraftKind::Quick,
        cards_per_pack: 14,
        pack_count: 3,
        rng_seed: seed as u64,
        tournament_format: TournamentFormat::Swiss,
        pod_policy: PodPolicy::Competitive,
    };

    let mut seats = vec![DraftSeat::Human {
        player_id: engine::types::player::PlayerId(0),
        display_name: "Player".to_string(),
        connected: true,
    }];
    for i in 1..8u8 {
        seats.push(DraftSeat::Bot {
            name: format!("Bot {i}"),
        });
    }

    let mut draft_session = DraftSession::new(config, seats, "quick-draft".to_string());
    let pack_gen = PackGenerator::new(set_pool);

    // Apply StartDraft to generate packs and transition to Drafting
    session::apply(&mut draft_session, DraftAction::StartDraft, Some(&pack_gen))
        .map_err(|e| JsValue::from_str(&format!("Failed to start draft: {}", e)))?;

    let view = filter_for_player(&draft_session, 0);

    // Store state in thread-locals
    DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
    PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
    DIFFICULTY.with(|cell| cell.set(ai_difficulty));
    RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

    Ok(to_js(&view))
}

/// Submit the human player's pick and resolve all bot picks synchronously.
///
/// Per Arena Quick Draft model: bots pick instantly after the human.
/// Returns the updated DraftPlayerView.
#[wasm_bindgen]
pub fn submit_pick(card_instance_id: &str) -> Result<JsValue, JsValue> {
    let card_id = card_instance_id.to_string();

    with_draft_mut(|draft_session| {
        // 1. Apply human pick (seat 0)
        session::apply(
            draft_session,
            DraftAction::Pick {
                seat: 0,
                card_instance_id: card_id,
            },
            None,
        )
        .map_err(|e| JsValue::from_str(&format!("Human pick failed: {}", e)))?;

        // 2. Resolve bot picks for seats 1..8
        let difficulty = DIFFICULTY.with(|cell| cell.get());
        let mut rng = RNG.with(|cell| cell.take())
            .ok_or_else(|| JsValue::from_str("RNG not initialized"))?;

        CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();

            for seat in 1..8u8 {
                let has_pack = draft_session
                    .current_pack
                    .get(seat as usize)
                    .is_some_and(|p| p.is_some());

                if has_pack {
                    let pack = draft_session.current_pack[seat as usize]
                        .as_ref()
                        .unwrap();
                    let pool = &draft_session.pools[seat as usize];

                    let pick_idx = bot_ai::bot_pick(
                        &pack.0,
                        difficulty,
                        pool,
                        card_db,
                        &mut rng,
                    );
                    let pick_id = pack.0[pick_idx].instance_id.clone();

                    session::apply(
                        draft_session,
                        DraftAction::Pick {
                            seat,
                            card_instance_id: pick_id,
                        },
                        None,
                    )
                    .map_err(|e| JsValue::from_str(&format!("Bot {seat} pick failed: {}", e)))?;
                }
            }

            Ok::<(), JsValue>(())
        })?;

        RNG.with(|cell| cell.set(Some(rng)));

        Ok(to_js(&filter_for_player(draft_session, 0)))
    })
}

/// Get the current DraftPlayerView without mutation.
#[wasm_bindgen]
pub fn get_view() -> Result<JsValue, JsValue> {
    with_draft(|session| to_js(&filter_for_player(session, 0)))
}

/// Submit the human player's deck for limited play.
///
/// `main_deck_json`: JSON array of card instance ID strings.
/// The deck is validated against the pool via LimitedDeckValidator.
#[wasm_bindgen]
pub fn submit_deck(main_deck_json: &str) -> Result<JsValue, JsValue> {
    let main_deck: Vec<String> = serde_json::from_str(main_deck_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse deck: {}", e)))?;

    with_draft_mut(|session| {
        session::apply(
            session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
            },
            None,
        )
        .map_err(|e| JsValue::from_str(&format!("Deck submission failed: {}", e)))?;

        Ok(to_js(&filter_for_player(session, 0)))
    })
}

/// Auto-suggest a playable Limited deck from the human's pool.
///
/// Returns a SuggestedDeck with ~23 spells + ~17 lands, using AI evaluation
/// at the current difficulty level. Per D-12: "Suggest deck" auto-build.
#[wasm_bindgen]
pub fn suggest_deck() -> Result<JsValue, JsValue> {
    with_draft(|session| {
        let pool = &session.pools[0];
        let difficulty = DIFFICULTY.with(|cell| cell.get());

        CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();
            let result = suggest::suggest_deck(pool, difficulty, card_db);
            to_js(&result)
        })
    })
}

/// Suggest land counts for a given set of spells.
///
/// `spells_json`: JSON array of card name strings from the pool.
/// Returns a map of land name -> count (e.g. {"Plains": 4, "Island": 6}).
/// Per D-11: auto-suggest land counts based on color distribution.
#[wasm_bindgen]
pub fn suggest_lands(spells_json: &str) -> Result<JsValue, JsValue> {
    let spell_names: Vec<String> = serde_json::from_str(spells_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse spells: {}", e)))?;

    with_draft(|session| {
        let pool = &session.pools[0];
        let lands = suggest::suggest_lands(&spell_names, pool);
        to_js(&lands)
    })
}

// ── Multi-seat draft API (P2P Tournament Host) ─────────────────────────
//
// These exports support the P2P draft host running an authoritative
// DraftSession for 8 human players. Unlike Quick Draft (single human +
// bots), the host calls `start_multiplayer_draft` with human seat names,
// then proxies picks/decks per-seat as guests submit them over the
// DataChannel.

/// Start a multiplayer draft session (Premier or Traditional).
///
/// - `set_pool_json`: serialized LimitedSetPool
/// - `kind`: "Premier" or "Traditional"
/// - `seat_names_json`: JSON array of display names, one per seat (length = pod size)
/// - `seed`: RNG seed for deterministic pack generation
///
/// Returns the DraftPlayerView for seat 0 (the host).
#[wasm_bindgen]
pub fn start_multiplayer_draft(
    set_pool_json: &str,
    kind: &str,
    seat_names_json: &str,
    seed: u32,
) -> Result<JsValue, JsValue> {
    let set_pool: LimitedSetPool = serde_json::from_str(set_pool_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse set pool: {e}")))?;

    let seat_names: Vec<String> = serde_json::from_str(seat_names_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse seat names: {e}")))?;

    let draft_kind = match kind {
        "Premier" => DraftKind::Premier,
        "Traditional" => DraftKind::Traditional,
        other => return Err(JsValue::from_str(&format!("Unknown draft kind: {other}"))),
    };

    let set_code = set_pool.code.clone();
    let config = DraftConfig {
        set_code,
        kind: draft_kind,
        cards_per_pack: 14,
        pack_count: 3,
        rng_seed: seed as u64,
        tournament_format: TournamentFormat::default(),
        pod_policy: PodPolicy::default(),
    };

    let seats: Vec<DraftSeat> = seat_names
        .iter()
        .enumerate()
        .map(|(i, name)| DraftSeat::Human {
            player_id: engine::types::player::PlayerId(i as u8),
            display_name: name.clone(),
            connected: true,
        })
        .collect();

    let draft_code = format!("draft-{seed:08x}");
    let mut draft_session = DraftSession::new(config, seats, draft_code);
    let pack_gen = PackGenerator::new(set_pool);

    session::apply(&mut draft_session, DraftAction::StartDraft, Some(&pack_gen))
        .map_err(|e| JsValue::from_str(&format!("Failed to start draft: {e}")))?;

    let view = filter_for_player(&draft_session, 0);

    DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
    PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
    RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

    Ok(to_js(&view))
}

/// Submit a pick for any seat (host proxies guest picks).
///
/// Returns the DraftPlayerView for the specified seat after the pick.
#[wasm_bindgen]
pub fn submit_pick_for_seat(seat: u8, card_instance_id: &str) -> Result<JsValue, JsValue> {
    let card_id = card_instance_id.to_string();

    with_draft_mut(|draft_session| {
        session::apply(
            draft_session,
            DraftAction::Pick {
                seat,
                card_instance_id: card_id,
            },
            None,
        )
        .map_err(|e| JsValue::from_str(&format!("Pick failed for seat {seat}: {e}")))?;

        Ok(to_js(&filter_for_player(draft_session, seat)))
    })
}

/// Submit a deck for any seat.
///
/// `main_deck_json`: JSON array of card name strings.
/// Returns the DraftPlayerView for the specified seat.
#[wasm_bindgen]
pub fn submit_deck_for_seat(seat: u8, main_deck_json: &str) -> Result<JsValue, JsValue> {
    let main_deck: Vec<String> = serde_json::from_str(main_deck_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse deck: {e}")))?;

    with_draft_mut(|session| {
        session::apply(
            session,
            DraftAction::SubmitDeck {
                seat,
                main_deck,
            },
            None,
        )
        .map_err(|e| JsValue::from_str(&format!("Deck submission failed for seat {seat}: {e}")))?;

        Ok(to_js(&filter_for_player(session, seat)))
    })
}

/// Get the filtered DraftPlayerView for any seat.
#[wasm_bindgen]
pub fn get_view_for_seat(seat: u8) -> Result<JsValue, JsValue> {
    with_draft(|session| to_js(&filter_for_player(session, seat)))
}

/// Serialize the full DraftSession to JSON for host persistence.
///
/// The host persists this after every authoritative mutation so a
/// crashed/reloaded host can restore the draft state.
#[wasm_bindgen]
pub fn export_draft_session() -> Result<String, JsValue> {
    with_draft(|session| {
        serde_json::to_string(session)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize draft session: {e}")))
    })?
}

/// Restore a DraftSession from a persisted JSON snapshot.
///
/// After calling this, subsequent `submit_pick_for_seat`, `get_view_for_seat`,
/// etc. operate on the restored session.
#[wasm_bindgen]
pub fn import_draft_session(json: &str) -> Result<JsValue, JsValue> {
    let session: DraftSession = serde_json::from_str(json)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize draft session: {e}")))?;

    let view = filter_for_player(&session, 0);
    DRAFT_SESSION.with(|cell| cell.set(Some(session)));

    Ok(to_js(&view))
}

/// Check whether all seats with pending packs have submitted their picks.
///
/// Returns true when the draft can advance (all seats picked or no packs pending).
/// The P2P host uses this to know when to broadcast state updates after a round.
#[wasm_bindgen]
pub fn all_picks_submitted() -> Result<bool, JsValue> {
    with_draft(|session| {
        if session.status != DraftStatus::Drafting {
            return true;
        }
        // A pick round is "complete" when every seat's current_pack is None
        // (all picks have been applied and packs passed).
        session.current_pack.iter().all(|p| p.is_none())
    })
}

/// Get a bot's auto-built deck for match play.
///
/// `bot_seat`: seat index 1-7 for the bot opponent.
/// Returns a SuggestedDeck built from the bot's drafted pool.
#[wasm_bindgen]
pub fn get_bot_deck(bot_seat: u8) -> Result<JsValue, JsValue> {
    if bot_seat == 0 || bot_seat > 7 {
        return Err(JsValue::from_str("bot_seat must be 1-7"));
    }

    with_draft(|session| {
        let pool = &session.pools[bot_seat as usize];
        let difficulty = DIFFICULTY.with(|cell| cell.get());

        CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();
            let result = suggest::suggest_deck(pool, difficulty, card_db);
            to_js(&result)
        })
    })
}

// ── Host-role exports for multiplayer (P2P) draft coordination ─────────

/// Seat descriptor for multiplayer draft creation.
/// JSON: `{ "type": "Human", "player_id": 0, "display_name": "Alice" }`
///    or `{ "type": "Bot", "name": "Bot 1" }`
#[derive(Deserialize)]
#[serde(tag = "type")]
enum SeatDescriptor {
    Human {
        player_id: u8,
        display_name: String,
    },
    Bot {
        name: String,
    },
}

/// Create a multiplayer draft session. Used by the P2P host to initialize a
/// Premier or Traditional draft with human + bot seats.
///
/// - `set_pool_json`: serialized LimitedSetPool from draft-pools.json
/// - `seats_json`: JSON array of SeatDescriptors
/// - `kind`: 0=Quick, 1=Premier, 2=Traditional
/// - `seed`: RNG seed for deterministic pack generation
/// - `draft_code`: unique room identifier
///
/// Stores the session in the same thread-local as Quick Draft (one active
/// draft at a time per WASM instance). Returns the initial DraftPlayerView
/// for seat 0.
#[wasm_bindgen]
pub fn create_multiplayer_draft(
    set_pool_json: &str,
    seats_json: &str,
    kind: u8,
    seed: u32,
    draft_code: &str,
) -> Result<JsValue, JsValue> {
    let set_pool: LimitedSetPool = serde_json::from_str(set_pool_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse set pool: {}", e)))?;

    let seat_descriptors: Vec<SeatDescriptor> = serde_json::from_str(seats_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse seats: {}", e)))?;

    let draft_kind = match kind {
        0 => DraftKind::Quick,
        1 => DraftKind::Premier,
        2 => DraftKind::Traditional,
        _ => return Err(JsValue::from_str("kind must be 0 (Quick), 1 (Premier), or 2 (Traditional)")),
    };

    let set_code = set_pool.code.clone();

    let seats: Vec<DraftSeat> = seat_descriptors
        .into_iter()
        .map(|desc| match desc {
            SeatDescriptor::Human {
                player_id,
                display_name,
            } => DraftSeat::Human {
                player_id: engine::types::player::PlayerId(player_id),
                display_name,
                connected: true,
            },
            SeatDescriptor::Bot { name } => DraftSeat::Bot { name },
        })
        .collect();

    let config = DraftConfig {
        set_code,
        kind: draft_kind,
        cards_per_pack: 14,
        pack_count: 3,
        rng_seed: seed as u64,
        tournament_format: TournamentFormat::default(),
        pod_policy: PodPolicy::default(),
    };

    let mut draft_session = DraftSession::new(config, seats, draft_code.to_string());
    let pack_gen = PackGenerator::new(set_pool);

    session::apply(&mut draft_session, DraftAction::StartDraft, Some(&pack_gen))
        .map_err(|e| JsValue::from_str(&format!("Failed to start draft: {}", e)))?;

    let view = filter_for_player(&draft_session, 0);

    DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
    PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
    RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

    Ok(to_js(&view))
}

/// Apply a draft action from any seat. Used by the P2P host to forward
/// picks from connected guests.
///
/// `action_json`: serialized DraftAction, e.g.:
///   `{ "type": "Pick", "data": { "seat": 2, "card_instance_id": "abc-123" } }`
///
/// Returns the list of DraftDeltas produced (serialized as a JS array).
#[wasm_bindgen]
pub fn apply_draft_action(action_json: &str) -> Result<JsValue, JsValue> {
    let action: DraftAction = serde_json::from_str(action_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse action: {}", e)))?;

    with_draft_mut(|draft_session| {
        let deltas = session::apply(draft_session, action, None)
            .map_err(|e| JsValue::from_str(&format!("Draft action failed: {}", e)))?;
        Ok(to_js(&deltas))
    })
}

/// Get a filtered draft view for a specific seat. The P2P host calls this
/// after each action to produce per-player state snapshots to send over
/// the P2P channel.
///
/// `seat_index`: 0-based seat index.
#[wasm_bindgen]
pub fn get_draft_view_for_seat(seat_index: u8) -> Result<JsValue, JsValue> {
    with_draft(|session| to_js(&filter_for_player(session, seat_index)))
}

/// Get the full draft status. Lightweight check so the host can decide
/// whether to broadcast updates or transition phases.
#[wasm_bindgen]
pub fn get_draft_status() -> Result<JsValue, JsValue> {
    with_draft(|session| to_js(&session.status))
}
