use std::cell::{Cell, RefCell};

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use draft_core::cube::{
    cube_cards_from_entries, parse_cube_list, resolve_addable_cards, CubePackSource,
};
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
fn with_draft_mut<R>(
    f: impl FnOnce(&mut DraftSession) -> Result<R, JsValue>,
) -> Result<R, JsValue> {
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

#[derive(Deserialize)]
struct CubeDraftSettings {
    #[serde(default = "default_cube_pod_size")]
    pod_size: u8,
    #[serde(default = "default_cube_pack_count")]
    pack_count: u8,
    #[serde(default = "default_cube_cards_per_pack")]
    cards_per_pack: u8,
    #[serde(default = "default_cube_min_deck_size")]
    min_deck_size: usize,
    #[serde(default = "DeckAddableCards::standard_basics")]
    addable_cards: DeckAddableCards,
}

fn default_cube_pod_size() -> u8 {
    8
}

fn default_cube_pack_count() -> u8 {
    3
}

fn default_cube_cards_per_pack() -> u8 {
    15
}

fn default_cube_min_deck_size() -> usize {
    40
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
        source: DraftSource::Set {
            code: set_code.clone(),
        },
        set_code,
        kind: DraftKind::Quick,
        pod_size: 8,
        cards_per_pack: 14,
        pack_count: 3,
        min_deck_size: 40,
        addable_cards: DeckAddableCards::standard_basics(),
        rng_seed: seed as u64,
        tournament_format: TournamentFormat::Swiss,
        pod_policy: PodPolicy::Competitive,
        spectator_visibility: SpectatorVisibility::default(),
    };

    let mut seats = vec![DraftSeat::Human {
        player_id: engine::types::player::PlayerId(0),
        display_name: "Player".to_string(),
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

/// Start a Quick Cube Draft session from a counted cube list.
#[wasm_bindgen]
pub fn start_quick_cube_draft(
    cube_list_text: &str,
    cube_name: &str,
    settings_json: &str,
    difficulty: u8,
    seed: u32,
) -> Result<JsValue, JsValue> {
    let settings: CubeDraftSettings = if settings_json.trim().is_empty() {
        CubeDraftSettings {
            pod_size: default_cube_pod_size(),
            pack_count: default_cube_pack_count(),
            cards_per_pack: default_cube_cards_per_pack(),
            min_deck_size: default_cube_min_deck_size(),
            addable_cards: DeckAddableCards::standard_basics(),
        }
    } else {
        serde_json::from_str(settings_json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse cube settings: {e}")))?
    };
    if settings.pod_size == 0 {
        return Err(JsValue::from_str("Cube draft pod size must be at least 1"));
    }

    let entries = parse_cube_list(cube_list_text).map_err(|errors| {
        JsValue::from_str(&format!(
            "Failed to parse cube list: {}",
            serde_json::to_string(&errors).unwrap_or_else(|_| "invalid lines".to_string())
        ))
    })?;
    let (cards, addable_cards) = CARD_DB.with(|cell| {
        let db_borrow = cell.borrow();
        let db = db_borrow
            .as_ref()
            .ok_or_else(|| JsValue::from_str("Card database must be loaded before cube draft"))?;
        let cards = cube_cards_from_entries(&entries, db).map_err(|errors| {
            JsValue::from_str(&format!(
                "Failed to resolve cube cards: {}",
                serde_json::to_string(&errors).unwrap_or_else(|_| "unknown cards".to_string())
            ))
        })?;
        let addable_cards =
            resolve_addable_cards(&settings.addable_cards, db).map_err(|errors| {
                JsValue::from_str(&format!(
                    "Failed to resolve addable cards: {}",
                    serde_json::to_string(&errors).unwrap_or_else(|_| "unknown cards".to_string())
                ))
            })?;
        Ok::<_, JsValue>((cards, addable_cards))
    })?;

    let ai_difficulty = map_difficulty(difficulty);
    let pod_size = settings.pod_size;
    let config = DraftConfig {
        source: DraftSource::Cube {
            id: "custom-cube".to_string(),
            name: cube_name.to_string(),
        },
        set_code: "custom-cube".to_string(),
        kind: DraftKind::Quick,
        pod_size,
        cards_per_pack: settings.cards_per_pack,
        pack_count: settings.pack_count,
        min_deck_size: settings.min_deck_size,
        addable_cards,
        rng_seed: seed as u64,
        tournament_format: TournamentFormat::Swiss,
        pod_policy: PodPolicy::Competitive,
        spectator_visibility: SpectatorVisibility::default(),
    };

    let mut seats = vec![DraftSeat::Human {
        player_id: engine::types::player::PlayerId(0),
        display_name: "Player".to_string(),
    }];
    for i in 1..pod_size {
        seats.push(DraftSeat::Bot {
            name: format!("Bot {i}"),
        });
    }

    let mut draft_session = DraftSession::new(config, seats, "quick-cube-draft".to_string());
    let pack_source = CubePackSource::new(cards);
    session::apply(
        &mut draft_session,
        DraftAction::StartDraft,
        Some(&pack_source),
    )
    .map_err(|e| JsValue::from_str(&format!("Failed to start cube draft: {e}")))?;

    let view = filter_for_player(&draft_session, 0);

    DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
    PACK_GEN.with(|cell| cell.set(None));
    DIFFICULTY.with(|cell| cell.set(ai_difficulty));
    RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

    Ok(to_js(&view))
}

/// Apply the human player's pick at seat 0, then resolve every bot pick.
///
/// Per Arena Quick Draft model: bots pick instantly after the human. Shared by
/// [`submit_pick`] (player chose a card) and [`auto_pick`] (AI chose for them).
/// Only valid for Quick Draft sessions — in 8-human Premier/Traditional pods,
/// seats 1-7 are real players and the multi-seat API (`submit_pick_for_seat`)
/// must be used instead.
fn apply_human_pick_and_resolve_bots(
    draft_session: &mut DraftSession,
    human_card_id: String,
) -> Result<(), JsValue> {
    if !matches!(draft_session.config.kind, DraftKind::Quick) {
        return Err(JsValue::from_str(
            "apply_human_pick_and_resolve_bots is only valid for Quick Draft",
        ));
    }

    session::apply(
        draft_session,
        DraftAction::Pick {
            seat: 0,
            card_instance_id: human_card_id,
        },
        None,
    )
    .map_err(|e| JsValue::from_str(&format!("Human pick failed: {}", e)))?;

    let difficulty = DIFFICULTY.with(|cell| cell.get());
    let mut rng = RNG
        .with(|cell| cell.take())
        .ok_or_else(|| JsValue::from_str("RNG not initialized"))?;

    let result = CARD_DB.with(|cell| {
        let db_borrow = cell.borrow();
        let card_db = db_borrow.as_ref();

        for seat in 1..draft_session.seats.len() as u8 {
            let Some(Some(pack)) = draft_session.current_pack.get(seat as usize) else {
                continue;
            };
            if pack.0.is_empty() {
                continue;
            }

            let pick_idx = bot_ai::bot_pick(
                &pack.0,
                difficulty,
                &draft_session.pools[seat as usize],
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

        Ok::<(), JsValue>(())
    });

    RNG.with(|cell| cell.set(Some(rng)));
    result
}

/// Submit the human player's pick and resolve all bot picks synchronously.
///
/// Returns the updated DraftPlayerView.
#[wasm_bindgen]
pub fn submit_pick(card_instance_id: &str) -> Result<JsValue, JsValue> {
    let card_id = card_instance_id.to_string();
    with_draft_mut(|draft_session| {
        apply_human_pick_and_resolve_bots(draft_session, card_id)?;
        Ok(to_js(&filter_for_player(draft_session, 0)))
    })
}

/// Auto-pick the best card from the human's current pack using the same AI the
/// bots use (at the active difficulty), then resolve all bot picks.
///
/// Returns the updated DraftPlayerView.
#[wasm_bindgen]
pub fn auto_pick() -> Result<JsValue, JsValue> {
    with_draft_mut(|draft_session| {
        let pack = draft_session
            .current_pack
            .first()
            .and_then(|p| p.as_ref())
            .ok_or_else(|| JsValue::from_str("No pack to pick from"))?;
        if pack.0.is_empty() {
            return Err(JsValue::from_str("Pack is empty"));
        }

        let difficulty = DIFFICULTY.with(|cell| cell.get());
        let mut rng = RNG
            .with(|cell| cell.take())
            .ok_or_else(|| JsValue::from_str("RNG not initialized"))?;
        let card_id = CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let pick_idx = bot_ai::bot_pick(
                &pack.0,
                difficulty,
                &draft_session.pools[0],
                db_borrow.as_ref(),
                &mut rng,
            );
            pack.0[pick_idx].instance_id.clone()
        });
        RNG.with(|cell| cell.set(Some(rng)));

        apply_human_pick_and_resolve_bots(draft_session, card_id)?;
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
            DraftAction::SubmitDeck { seat: 0, main_deck },
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
            let result = suggest::suggest_deck(
                pool,
                difficulty,
                card_db,
                session.config.min_deck_size,
                &session.config.addable_cards,
            );
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
        let lands = suggest::suggest_lands(&spell_names, pool, session.config.min_deck_size);
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
        source: DraftSource::Set {
            code: set_code.clone(),
        },
        set_code,
        kind: draft_kind,
        pod_size: seat_names.len() as u8,
        cards_per_pack: 14,
        pack_count: 3,
        min_deck_size: 40,
        addable_cards: DeckAddableCards::standard_basics(),
        rng_seed: seed as u64,
        tournament_format: TournamentFormat::default(),
        pod_policy: PodPolicy::default(),
        spectator_visibility: SpectatorVisibility::default(),
    };

    let seats: Vec<DraftSeat> = seat_names
        .iter()
        .enumerate()
        .map(|(i, name)| DraftSeat::Human {
            player_id: engine::types::player::PlayerId(i as u8),
            display_name: name.clone(),
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

/// Mark a human seat as connected or disconnected. The host adapter calls
/// this on guest disconnect/reconnect so `DraftPlayerView.seats[*].connected`
/// reflects the runtime state. Rejects bot seats with `SeatIsBot`.
///
/// Returns the DraftPlayerView for seat 0 (the host) after the update.
#[wasm_bindgen]
pub fn set_seat_connected(seat: u8, connected: bool) -> Result<JsValue, JsValue> {
    with_draft_mut(|session| {
        session::apply(
            session,
            DraftAction::SetSeatConnected { seat, connected },
            None,
        )
        .map_err(|e| JsValue::from_str(&format!("SetSeatConnected failed: {e}")))?;

        Ok(to_js(&filter_for_player(session, 0)))
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
        session::apply(session, DraftAction::SubmitDeck { seat, main_deck }, None).map_err(
            |e| JsValue::from_str(&format!("Deck submission failed for seat {seat}: {e}")),
        )?;

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
/// Also re-initializes RNG and difficulty from the session config so that
/// `submit_pick` (which runs bot picks) works after resume.  The RNG is
/// re-seeded from the config seed offset by the current pick progress —
/// bot pick quality remains reasonable but won't be identical to the
/// original session's RNG stream, which is fine.
#[wasm_bindgen]
pub fn import_draft_session(json: &str, difficulty: u8) -> Result<JsValue, JsValue> {
    let session: DraftSession = serde_json::from_str(json)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize draft session: {e}")))?;

    let offset = session.current_pack_number as u64 * session.config.cards_per_pack as u64
        + session.pick_number as u64;
    let resume_seed = session.config.rng_seed.wrapping_add(offset);

    DIFFICULTY.with(|cell| cell.set(map_difficulty(difficulty)));
    RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(resume_seed))));

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
    with_draft(|session| {
        if bot_seat == 0 || bot_seat as usize >= session.seats.len() {
            return Err(JsValue::from_str("bot_seat is out of range"));
        }
        let pool = &session.pools[bot_seat as usize];
        let difficulty = DIFFICULTY.with(|cell| cell.get());

        CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();
            let result = suggest::suggest_deck(
                pool,
                difficulty,
                card_db,
                session.config.min_deck_size,
                &session.config.addable_cards,
            );
            Ok(to_js(&result))
        })
    })?
}

// ── Host-role exports for multiplayer (P2P) draft coordination ─────────

/// Seat descriptor for multiplayer draft creation.
/// JSON: `{ "type": "Human", "player_id": 0, "display_name": "Alice" }`
///    or `{ "type": "Bot", "name": "Bot 1" }`
#[derive(Deserialize)]
#[serde(tag = "type")]
enum SeatDescriptor {
    Human { player_id: u8, display_name: String },
    Bot { name: String },
}

/// Pool source for multiplayer draft creation.
/// Discriminated union mirroring the TS `PoolInput` type. The `data` payload
/// uses snake_case field names matching `CubeDraftSettings` and the existing
/// TS↔Rust mirror convention in `draft-adapter.ts`.
///
/// JSON examples:
///   `{ "type": "Set",  "data": { "set_pool_json": "<serialized LimitedSetPool>" } }`
///   `{ "type": "Cube", "data": { "cube_list_text": "...", "cube_name": "My Cube",
///                                 "cube_draft_settings": { ... } } }`
#[derive(Deserialize)]
#[serde(tag = "type", content = "data")]
enum PoolInput {
    Set {
        set_pool_json: String,
    },
    Cube {
        cube_list_text: String,
        cube_name: String,
        cube_draft_settings: CubeDraftSettings,
    },
}

/// Create a multiplayer draft session. Used by the P2P host to initialize a
/// Premier or Traditional draft with human + bot seats from either a Set pool
/// or a custom Cube list.
///
/// - `pool_input_json`: serialized `PoolInput` discriminated union
///   (`{ "type": "Set" | "Cube", "data": { ... } }`)
/// - `seats_json`: JSON array of SeatDescriptors
/// - `kind`: 0=Quick, 1=Premier, 2=Traditional. The user-selected DraftKind
///   flows through to `DraftConfig.kind` unchanged. Tournament match format
///   (Bo1 for Premier, Bo3 for Traditional) is identical to set drafts.
/// - `seed`: RNG seed for deterministic pack generation
/// - `draft_code`: unique room identifier
///
/// Stores the session in the same thread-local as Quick Draft (one active
/// draft at a time per WASM instance). Returns the initial DraftPlayerView
/// for seat 0.
#[wasm_bindgen]
pub fn create_multiplayer_draft(
    pool_input_json: &str,
    seats_json: &str,
    kind: u8,
    seed: u32,
    draft_code: &str,
    tournament_format: &str,
    pod_policy: &str,
) -> Result<JsValue, JsValue> {
    let view = create_multiplayer_draft_inner(
        pool_input_json,
        seats_json,
        kind,
        seed,
        draft_code,
        tournament_format,
        pod_policy,
    )
    .map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&view))
}

/// Pure-Rust core for `create_multiplayer_draft`. Returns a typed
/// `DraftPlayerView` so this branch is reachable from `cargo test` without
/// going through `js_sys::JSON::parse`. The WASM export wraps this with
/// `to_js` and `JsValue::from_str` error mapping.
fn create_multiplayer_draft_inner(
    pool_input_json: &str,
    seats_json: &str,
    kind: u8,
    seed: u32,
    draft_code: &str,
    tournament_format: &str,
    pod_policy: &str,
) -> Result<draft_core::view::DraftPlayerView, String> {
    let pool_input: PoolInput = serde_json::from_str(pool_input_json)
        .map_err(|e| format!("Failed to parse pool input: {}", e))?;

    let seat_descriptors: Vec<SeatDescriptor> =
        serde_json::from_str(seats_json).map_err(|e| format!("Failed to parse seats: {}", e))?;

    let draft_kind = match kind {
        0 => DraftKind::Quick,
        1 => DraftKind::Premier,
        2 => DraftKind::Traditional,
        _ => {
            return Err("kind must be 0 (Quick), 1 (Premier), or 2 (Traditional)".to_string());
        }
    };

    let tournament_format = match tournament_format {
        "Swiss" => TournamentFormat::Swiss,
        "SingleElimination" => TournamentFormat::SingleElimination,
        _ => {
            return Err("tournament_format must be Swiss or SingleElimination".to_string());
        }
    };

    let pod_policy = match pod_policy {
        "Competitive" => PodPolicy::Competitive,
        "Casual" => PodPolicy::Casual,
        _ => {
            return Err("pod_policy must be Competitive or Casual".to_string());
        }
    };

    let seats: Vec<DraftSeat> = seat_descriptors
        .into_iter()
        .map(|desc| match desc {
            SeatDescriptor::Human {
                player_id,
                display_name,
            } => DraftSeat::Human {
                player_id: engine::types::player::PlayerId(player_id),
                display_name,
            },
            SeatDescriptor::Bot { name } => DraftSeat::Bot { name },
        })
        .collect();

    match pool_input {
        PoolInput::Set { set_pool_json } => {
            let set_pool: LimitedSetPool = serde_json::from_str(&set_pool_json)
                .map_err(|e| format!("Failed to parse set pool: {}", e))?;
            let set_code = set_pool.code.clone();

            let config = DraftConfig {
                source: DraftSource::Set {
                    code: set_code.clone(),
                },
                set_code,
                kind: draft_kind,
                pod_size: seats.len() as u8,
                cards_per_pack: 14,
                pack_count: 3,
                min_deck_size: 40,
                addable_cards: DeckAddableCards::standard_basics(),
                rng_seed: seed as u64,
                tournament_format,
                pod_policy,
                spectator_visibility: SpectatorVisibility::default(),
            };

            let mut draft_session = DraftSession::new(config, seats, draft_code.to_string());
            let pack_gen = PackGenerator::new(set_pool);

            session::apply(&mut draft_session, DraftAction::StartDraft, Some(&pack_gen))
                .map_err(|e| format!("Failed to start draft: {}", e))?;

            let view = filter_for_player(&draft_session, 0);

            DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
            PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
            RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

            Ok(view)
        }
        PoolInput::Cube {
            cube_list_text,
            cube_name,
            cube_draft_settings: settings,
        } => {
            let entries = parse_cube_list(&cube_list_text).map_err(|errors| {
                format!(
                    "Failed to parse cube list: {}",
                    serde_json::to_string(&errors).unwrap_or_else(|_| "invalid lines".to_string())
                )
            })?;
            let (cards, addable_cards) = CARD_DB.with(|cell| {
                let db_borrow = cell.borrow();
                let db = db_borrow
                    .as_ref()
                    .ok_or_else(|| "Card database must be loaded before cube draft".to_string())?;
                let cards = cube_cards_from_entries(&entries, db).map_err(|errors| {
                    format!(
                        "Failed to resolve cube cards: {}",
                        serde_json::to_string(&errors)
                            .unwrap_or_else(|_| "unknown cards".to_string())
                    )
                })?;
                let addable_cards =
                    resolve_addable_cards(&settings.addable_cards, db).map_err(|errors| {
                        format!(
                            "Failed to resolve addable cards: {}",
                            serde_json::to_string(&errors)
                                .unwrap_or_else(|_| "unknown cards".to_string())
                        )
                    })?;
                Ok::<_, String>((cards, addable_cards))
            })?;

            // pod_size from settings is overridden by seats.len() — MP authoritative source
            let config = DraftConfig {
                source: DraftSource::Cube {
                    id: "custom-cube".to_string(),
                    name: cube_name.clone(),
                },
                set_code: "custom-cube".to_string(),
                kind: draft_kind,
                pod_size: seats.len() as u8,
                cards_per_pack: settings.cards_per_pack,
                pack_count: settings.pack_count,
                min_deck_size: settings.min_deck_size,
                addable_cards,
                rng_seed: seed as u64,
                tournament_format,
                pod_policy,
                spectator_visibility: SpectatorVisibility::default(),
            };

            let mut draft_session = DraftSession::new(config, seats, draft_code.to_string());
            let pack_source = CubePackSource::new(cards);

            session::apply(
                &mut draft_session,
                DraftAction::StartDraft,
                Some(&pack_source),
            )
            .map_err(|e| format!("Failed to start cube draft: {}", e))?;

            let view = filter_for_player(&draft_session, 0);

            DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
            PACK_GEN.with(|cell| cell.set(None));
            RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

            Ok(view)
        }
    }
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

#[cfg(test)]
mod pool_input_tests {
    use super::*;

    #[test]
    fn pool_input_set_round_trip() {
        let json = r#"{"type":"Set","data":{"set_pool_json":"{\"code\":\"foo\"}"}}"#;
        let parsed: PoolInput = serde_json::from_str(json).unwrap();
        match parsed {
            PoolInput::Set { set_pool_json } => {
                assert_eq!(set_pool_json, "{\"code\":\"foo\"}");
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn pool_input_cube_round_trip() {
        let json = r#"{
            "type": "Cube",
            "data": {
                "cube_list_text": "1 Lightning Bolt\n",
                "cube_name": "Test Cube",
                "cube_draft_settings": {
                    "pod_size": 8,
                    "pack_count": 3,
                    "cards_per_pack": 15,
                    "min_deck_size": 40,
                    "addable_cards": { "policy": "StandardBasics", "custom": [] }
                }
            }
        }"#;
        let parsed: PoolInput = serde_json::from_str(json).unwrap();
        match parsed {
            PoolInput::Cube {
                cube_list_text,
                cube_name,
                cube_draft_settings,
            } => {
                assert_eq!(cube_list_text, "1 Lightning Bolt\n");
                assert_eq!(cube_name, "Test Cube");
                assert_eq!(cube_draft_settings.cards_per_pack, 15);
                assert_eq!(cube_draft_settings.pack_count, 3);
            }
            _ => panic!("expected Cube"),
        }
    }
}

#[cfg(test)]
mod create_multiplayer_draft_tests {
    use super::*;
    use draft_core::types::DraftStatus;
    use engine::database::CardDatabase;

    /// Minimal card-data JSON with four vanilla cards usable as cube content.
    /// Shape mirrors the production card-data export consumed by
    /// `CardDatabase::from_json_str` (see engine-wasm bracket_estimate_tests).
    fn fixture_card_db_json() -> &'static str {
        r#"{
            "alpha": {
                "name": "Alpha",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": "1", "toughness": "1", "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": { "game_changer": false, "mass_land_denial": false, "extra_turn": false, "efficient_tutor": false }
            },
            "beta": {
                "name": "Beta",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": "1", "toughness": "1", "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": { "game_changer": false, "mass_land_denial": false, "extra_turn": false, "efficient_tutor": false }
            },
            "gamma": {
                "name": "Gamma",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": "1", "toughness": "1", "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": { "game_changer": false, "mass_land_denial": false, "extra_turn": false, "efficient_tutor": false }
            },
            "delta": {
                "name": "Delta",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": "1", "toughness": "1", "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": { "game_changer": false, "mass_land_denial": false, "extra_turn": false, "efficient_tutor": false }
            }
        }"#
    }

    fn install_fixture_db() {
        let db = CardDatabase::from_json_str(fixture_card_db_json()).unwrap();
        CARD_DB.with(|cell| *cell.borrow_mut() = Some(db));
    }

    fn clear_state() {
        DRAFT_SESSION.with(|cell| cell.set(None));
        PACK_GEN.with(|cell| cell.set(None));
        RNG.with(|cell| cell.set(None));
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
    }

    #[test]
    fn cube_pool_input_drives_drafting_status_and_pack_size() {
        clear_state();
        install_fixture_db();

        // 2 seats × 2 cards/pack × 1 pack = 4 cards exactly.
        let pool_input_json = r#"{
            "type": "Cube",
            "data": {
                "cube_list_text": "1 Alpha\n1 Beta\n1 Gamma\n1 Delta\n",
                "cube_name": "Test Cube",
                "cube_draft_settings": {
                    "pod_size": 2,
                    "pack_count": 1,
                    "cards_per_pack": 2,
                    "min_deck_size": 4,
                    "addable_cards": { "policy": "StandardBasics", "custom": [] }
                }
            }
        }"#;
        let seats_json = r#"[
            { "type": "Human", "player_id": 0, "display_name": "Host" },
            { "type": "Human", "player_id": 1, "display_name": "Guest" }
        ]"#;

        let view = create_multiplayer_draft_inner(
            pool_input_json,
            seats_json,
            1, // Premier
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("cube draft should start");

        assert!(
            matches!(view.status, DraftStatus::Drafting),
            "expected Drafting, got {:?}",
            view.status
        );
        assert_eq!(view.cards_per_pack, 2);
        assert_eq!(view.pack_count, 1);
        assert_eq!(view.min_deck_size, 4);
        let pack = view.current_pack.as_ref().expect("seat 0 has a pack");
        assert_eq!(pack.len(), 2);

        // Drive one Pick action by seat 0 and verify a delta is produced.
        let picked = pack[0].instance_id.clone();
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_id: picked.clone(),
        };
        DRAFT_SESSION.with(|cell| {
            let mut session = cell.take().expect("session populated");
            let deltas = session::apply(&mut session, action, None).expect("pick applies");
            assert!(!deltas.is_empty(), "pick should produce deltas");
            cell.set(Some(session));
        });

        // After the human pick, seat 0's pack should no longer contain the picked card
        // (it has been passed; pack will not be visible again until the rotation lands).
        let post_view = DRAFT_SESSION.with(|cell| {
            let session = cell.take().expect("session populated");
            let v = filter_for_player(&session, 0);
            cell.set(Some(session));
            v
        });
        if let Some(pack_after) = &post_view.current_pack {
            assert!(
                !pack_after.iter().any(|c| c.instance_id == picked),
                "picked card must not remain in seat 0's pack"
            );
        }

        clear_state();
    }

    #[test]
    fn cube_branch_uses_settings_cards_per_pack_not_default() {
        clear_state();
        install_fixture_db();

        let pool_input_json = r#"{
            "type": "Cube",
            "data": {
                "cube_list_text": "1 Alpha\n1 Beta\n1 Gamma\n1 Delta\n",
                "cube_name": "C1",
                "cube_draft_settings": {
                    "pod_size": 2,
                    "pack_count": 1,
                    "cards_per_pack": 2,
                    "min_deck_size": 4,
                    "addable_cards": { "policy": "StandardBasics", "custom": [] }
                }
            }
        }"#;
        let seats_json = r#"[
            { "type": "Human", "player_id": 0, "display_name": "Host" },
            { "type": "Human", "player_id": 1, "display_name": "Guest" }
        ]"#;

        let view = create_multiplayer_draft_inner(
            pool_input_json,
            seats_json,
            2, // Traditional
            7,
            "test-room",
            "Swiss",
            "Casual",
        )
        .expect("cube draft should start");

        // C1: cards_per_pack must come from settings, NOT the hardcoded 14 in the Set branch.
        assert_eq!(
            view.cards_per_pack, 2,
            "cards_per_pack must read from CubeDraftSettings"
        );
        // DraftKind flow-through: Traditional flows unchanged.
        assert!(matches!(view.kind, DraftKind::Traditional));

        clear_state();
    }
}
