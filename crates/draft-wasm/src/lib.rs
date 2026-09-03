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

/// `with_draft_mut` for the pure-Rust `_inner` cores: identical take/run/put
/// dance, but `String` errors so the core is callable from `cargo test` on a
/// native target, where every `JsValue` operation is unavailable.
fn with_draft_mut_inner<R>(
    f: impl FnOnce(&mut DraftSession) -> Result<R, String>,
) -> Result<R, String> {
    DRAFT_SESSION.with(|cell| {
        let mut session = cell.take().ok_or("Draft not initialized")?;
        let result = f(&mut session);
        cell.set(Some(session));
        result
    })
}

/// `with_draft` for the pure-Rust `_inner` cores: identical take/run/put dance
/// over a SHARED borrow, but `String` errors so the core is callable from
/// `cargo test` on a native target.
///
/// The shared sibling of `with_draft_mut_inner`. A read-only `_inner` core must
/// not reach for the `&mut` helper instead: taking `&mut` for a body that only
/// reads is the kind of borrow the type system is there to state honestly.
fn with_draft_inner<R>(f: impl FnOnce(&DraftSession) -> Result<R, String>) -> Result<R, String> {
    DRAFT_SESSION.with(|cell| {
        let session = cell.take().ok_or("Draft not initialized")?;
        let result = f(&session);
        cell.set(Some(session));
        result
    })
}

/// Preserve Limited-deck validation details across the WASM boundary so the
/// deck builder can tell the player what needs correction.
fn deck_submission_message(error: DraftError) -> String {
    match error {
        DraftError::ValidationFailed { errors } => errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
        error => error.to_string(),
    }
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

/// Derive the session pack size from the selected MTGJSON booster product.
/// Every supported set currently has uniformly sized variants; rejecting mixed
/// data keeps UI progress and sealed-pool validation aligned with actual pulls.
fn set_cards_per_pack(set_pool: &LimitedSetPool) -> Result<u8, String> {
    set_pool.cards_per_pack().ok_or_else(|| {
        format!(
            "Set {} has no single MTGJSON pack size across its booster variants",
            set_pool.code
        )
    })
}

/// The sets backing a draft and the order their boosters are opened in.
///
/// `pools` carries each distinct set once; `sequence` names which of them fills
/// each booster, in pack order, so a set may be drafted more than once without
/// its pool data crossing the WASM boundary more than once. A one-element
/// sequence is a single-set draft.
///
/// JSON shape:
///   `{ "pools": [<LimitedSetPool>, ...], "sequence": ["isd", "dka", "avr"] }`
#[derive(Deserialize)]
struct SetPackSequence {
    pools: Vec<LimitedSetPool>,
    sequence: Vec<String>,
}

/// A set-backed draft's source, pack shape, and generator, resolved from the
/// selection the client sent. Single authority for turning a `SetPackSequence`
/// into the three things every set-backed entry point needs.
struct ResolvedSetSelection {
    source: DraftSource,
    /// Nominal booster size (the first pack's). Per-pack sizes are recorded on
    /// the session at `StartDraft` from the packs this generator produces.
    cards_per_pack: u8,
    pack_count: u8,
    generator: PackGenerator,
}

impl ResolvedSetSelection {
    /// Parse and validate a client selection. `expected_packs` constrains the
    /// sequence length for event kinds the engine fixes (Sealed opens exactly
    /// six boosters); `None` lets the selection decide how many packs to open.
    fn parse(selection_json: &str, expected_packs: Option<u8>) -> Result<Self, String> {
        let selection: SetPackSequence = serde_json::from_str(selection_json)
            .map_err(|e| format!("Failed to parse set selection: {e}"))?;
        Self::resolve(selection, expected_packs)
    }

    /// Validate an already-deserialized selection. The pod boundary reaches
    /// this directly, since its sequence arrives inside a `PoolInput` frame
    /// rather than as a JSON string of its own.
    fn resolve(selection: SetPackSequence, expected_packs: Option<u8>) -> Result<Self, String> {
        let named = u8::try_from(selection.sequence.len())
            .ok()
            .filter(|count| (1..=MAX_PACK_COUNT).contains(count))
            .ok_or_else(|| format!("A draft must name between 1 and {MAX_PACK_COUNT} sets"))?;

        // A sequence SHORTER than the event's pack count repeats its last entry
        // (`entry_for_pack`) — that is the rule `DraftSource::Set` is defined
        // by, and it is how a single-set draft stays a one-element sequence
        // instead of the same code copied once per booster. A LONGER sequence
        // names boosters the event never opens, so it is the caller's error
        // rather than a silent truncation.
        let pack_count = expected_packs.unwrap_or(named);
        if named > pack_count {
            return Err(format!(
                "This event opens {pack_count} packs, but {named} sets were named"
            ));
        }

        // Resolve every named set against the supplied pools up front, so a set
        // with no pool data names itself here instead of surfacing as a short
        // pack mid-draft.
        let indices = selection
            .sequence
            .iter()
            .map(|code| {
                selection
                    .pools
                    .iter()
                    .position(|pool| pool.code.eq_ignore_ascii_case(code))
                    .ok_or_else(|| format!("No pool data was supplied for set '{code}'"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Every booster must have a size MTGJSON agrees on; pack 1's is the
        // session's nominal one.
        for &index in &indices {
            set_cards_per_pack(&selection.pools[index])?;
        }
        let cards_per_pack = set_cards_per_pack(&selection.pools[indices[0]])?;

        // Carry the pools' own casing into the source, so the per-pack set
        // codes the view publishes match the codes on the cards it deals.
        let codes: Vec<String> = indices
            .iter()
            .map(|&index| selection.pools[index].code.clone())
            .collect();
        let generator =
            PackGenerator::for_sequence(selection.pools, &codes).map_err(|e| e.to_string())?;

        Ok(Self {
            source: DraftSource::Set {
                layout: SetLayout::UniformByRound { codes },
            },
            cards_per_pack,
            pack_count,
            generator,
        })
    }
}

/// Preflight every set a Chaos draft can select. Pack passing requires every
/// seat's booster in a round to exhaust on the same pick count, so a variable
/// MTGJSON variant or a different declared total is rejected before assignment.
fn preflight_chaos_pools(
    pools: &[LimitedSetPool],
    candidate_codes: &[String],
) -> Result<(Vec<String>, u8), String> {
    let mut canonical_codes = Vec::with_capacity(candidate_codes.len());
    let mut cards_per_pack = None;
    for candidate in candidate_codes {
        let pool = pools
            .iter()
            .find(|pool| pool.code.eq_ignore_ascii_case(candidate))
            .ok_or_else(|| format!("No pool data was supplied for set '{candidate}'"))?;
        let size = set_cards_per_pack(pool)?;
        if let Some(expected) = cards_per_pack {
            if expected != size {
                return Err(format!(
                    "Chaos candidate sets must share one MTGJSON pack size; {} has {size}, expected {expected}",
                    pool.code
                ));
            }
        } else {
            cards_per_pack = Some(size);
        }
        canonical_codes.push(pool.code.clone());
    }
    let cards_per_pack = cards_per_pack
        .ok_or_else(|| "A Chaos draft must name at least one candidate set".to_string())?;
    Ok((canonical_codes, cards_per_pack))
}

/// Host-local Chaos input. The host names only the candidate pools; the WASM
/// boundary derives the exact seat-by-round assignment from its private seed
/// and persists it in the session. Guests never receive this input.
#[derive(Deserialize)]
struct ChaosPackSelection {
    pools: Vec<LimitedSetPool>,
    candidate_codes: Vec<String>,
}

/// A resolved Chaos source and generator. Unlike `ResolvedSetSelection`, this
/// records one set assignment for each seat and round.
struct ResolvedChaosSelection {
    source: DraftSource,
    cards_per_pack: u8,
    pack_count: u8,
    generator: PackGenerator,
}

impl ResolvedChaosSelection {
    fn resolve(
        selection: ChaosPackSelection,
        seat_count: u8,
        pack_count: u8,
        seed: u64,
    ) -> Result<Self, String> {
        let (candidate_codes, cards_per_pack) =
            preflight_chaos_pools(&selection.pools, &selection.candidate_codes)?;
        let assignments =
            PackGenerator::chaos_assignments(&candidate_codes, seat_count, pack_count, seed)
                .map_err(|error| error.to_string())?;
        let generator = PackGenerator::for_chaos(
            selection.pools,
            &candidate_codes,
            &assignments,
            seat_count,
            pack_count,
        )
        .map_err(|error| error.to_string())?;

        Ok(Self {
            source: DraftSource::Set {
                layout: SetLayout::Chaos {
                    candidate_codes,
                    assignments,
                },
            },
            cards_per_pack,
            pack_count,
            generator,
        })
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
/// - `selection_json`: serialized [`SetPackSequence`] — the distinct set pools
///   from draft-pools.json plus the set filling each booster, in pack order.
///   The sequence length is the draft's pack count, and a set may repeat.
/// - `difficulty`: 0=VeryEasy, 1=Easy, 2=Medium, 3=Hard, 4=VeryHard
/// - `seed`: RNG seed for deterministic pack generation
///
/// Returns the initial DraftPlayerView as a JS object.
#[wasm_bindgen]
pub fn start_quick_draft(
    selection_json: &str,
    difficulty: u8,
    seed: u32,
) -> Result<JsValue, JsValue> {
    // The procedure table is the single authority for how many boosters this
    // kind opens; the selection only decides which set fills each of them.
    let quick_procedure = DraftKind::Quick.procedure();
    let selection =
        ResolvedSetSelection::parse(selection_json, Some(quick_procedure.packs_per_player))
            .map_err(|e| JsValue::from_str(&e))?;

    let ai_difficulty = map_difficulty(difficulty);

    let config = DraftConfig {
        set_code: selection.source.set_code(),
        source: selection.source,
        kind: DraftKind::Quick,
        pod_size: quick_procedure.pod_size,
        cards_per_pack: selection.cards_per_pack,
        pack_count: selection.pack_count,
        min_deck_size: quick_procedure.min_deck_size,
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
    let pack_gen = selection.generator;

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

/// Start a local Sealed event: one human and seven bots each open six packs,
/// then the human proceeds directly to deckbuilding.
#[wasm_bindgen]
pub fn start_sealed_draft(
    selection_json: &str,
    difficulty: u8,
    seed: u32,
) -> Result<JsValue, JsValue> {
    // CR-independent event rule: the engine fixes sealed at the procedure's
    // booster count (`apply_start_draft` rejects any other), so the selection
    // must name exactly that many — mixed sets are fine, a different pack count
    // is not.
    let sealed_procedure = DraftKind::Sealed.procedure();
    let selection =
        ResolvedSetSelection::parse(selection_json, Some(sealed_procedure.packs_per_player))
            .map_err(|e| JsValue::from_str(&e))?;
    let config = DraftConfig {
        set_code: selection.source.set_code(),
        source: selection.source,
        kind: DraftKind::Sealed,
        pod_size: sealed_procedure.pod_size,
        cards_per_pack: selection.cards_per_pack,
        pack_count: selection.pack_count,
        min_deck_size: sealed_procedure.min_deck_size,
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

    let mut draft_session = DraftSession::new(config, seats, "sealed-draft".to_string());
    let pack_gen = selection.generator;
    session::apply(&mut draft_session, DraftAction::StartDraft, Some(&pack_gen))
        .map_err(|e| JsValue::from_str(&format!("Failed to start sealed event: {e}")))?;
    let view = filter_for_player(&draft_session, 0);

    DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
    PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
    DIFFICULTY.with(|cell| cell.set(map_difficulty(difficulty)));
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
    apply_human_pick_and_resolve_bots_with_action(
        draft_session,
        DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec![human_card_id],
        },
    )
}

fn apply_human_pick_and_resolve_bots_with_action(
    draft_session: &mut DraftSession,
    human_action: DraftAction,
) -> Result<(), JsValue> {
    if !matches!(draft_session.config.kind, DraftKind::Quick) {
        return Err(JsValue::from_str(
            "apply_human_pick_and_resolve_bots is only valid for Quick Draft",
        ));
    }

    session::apply(draft_session, human_action, None)
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

            // CR 903.13b: a bot owes its kind's whole pick step. This loop is
            // `Quick`-gated above, so `cards_per_pick` is 1 here today; reading
            // it from the procedure is what keeps that true by construction
            // rather than by coincidence.
            let cards_per_pick =
                usize::from(draft_session.config.kind.procedure().cards_per_pick).min(pack.0.len());
            let pick_indices = bot_ai::bot_picks(
                &pack.0,
                cards_per_pick,
                difficulty,
                &draft_session.pools[seat as usize],
                card_db,
                &mut rng,
            );
            // Map indices to ids BEFORE applying — the apply mutates the pack
            // the indices refer to.
            let card_instance_ids: Vec<String> = pick_indices
                .into_iter()
                .map(|index| pack.0[index].instance_id.clone())
                .collect();

            session::apply(
                draft_session,
                DraftAction::Pick {
                    seat,
                    card_instance_ids,
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

/// Submit an additional pick using a drafted card's draft-time effect, then
/// resolve all bot picks.
#[wasm_bindgen]
pub fn submit_pick_with_draft_effect(
    effect_card_instance_id: &str,
    card_instance_ids_json: &str,
) -> Result<JsValue, JsValue> {
    let effect_card_instance_id = effect_card_instance_id.to_string();
    let card_instance_ids: Vec<String> = serde_json::from_str(card_instance_ids_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse draft-effect cards: {e}")))?;
    with_draft_mut(|draft_session| {
        apply_human_pick_and_resolve_bots_with_action(
            draft_session,
            DraftAction::PickWithDraftEffect {
                seat: 0,
                effect_card_instance_id,
                card_instance_ids,
            },
        )?;
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

/// Narrow a limited-pool listing through the ENGINE's filtering authority
/// (#7546 review): the display sends the listing and a typed `PoolFilter`;
/// it renders exactly the returned instance ids. Each instance is classified
/// inside draft-core, so wire-delivered groups (of any protocol vintage) are
/// not an input. Stateless — usable by P2P guests.
#[wasm_bindgen]
pub fn filter_pool_listing(listing_json: &str, filter_json: &str) -> Result<JsValue, JsValue> {
    let listing: Vec<draft_core::types::DraftCardInstance> = serde_json::from_str(listing_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse pool listing: {}", e)))?;
    let filter: draft_core::view::PoolFilter = serde_json::from_str(filter_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse pool filter: {}", e)))?;
    Ok(to_js(&draft_core::view::filter_pool_listing(
        &listing, &filter,
    )))
}

/// The complete engine-owned filter option lists for a pool, computed from
/// the instances alone (review round 5): the stateless path a display uses
/// when its delivered view predates the option fields, so legacy controls
/// never come from the lossy exclusive presentation buckets.
#[wasm_bindgen]
pub fn pool_filter_options(pool_json: &str) -> Result<JsValue, JsValue> {
    let pool: Vec<draft_core::types::DraftCardInstance> = serde_json::from_str(pool_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse pool: {}", e)))?;
    Ok(to_js(&draft_core::view::pool_filter_options(&pool)))
}

/// Submit the human player's deck for limited play.
///
/// `main_deck_json`: JSON array of card name strings.
/// `commanders_json`: JSON array of the card names this seat designates as its
/// commander(s) (CR 903.3 / CR 702.124h). CR 903.1 puts the designation inside
/// the Commander variant, so `[]` is the correct and meaningful value for every
/// non-Commander kind.
/// The deck is validated against the pool via LimitedDeckValidator.
#[wasm_bindgen]
pub fn submit_deck(main_deck_json: &str, commanders_json: &str) -> Result<JsValue, JsValue> {
    let view =
        submit_deck_inner(main_deck_json, commanders_json).map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&view))
}

/// Pure-Rust core for `submit_deck`, so both halves of the payload boundary are
/// reachable from `cargo test` without `js_sys::JSON::parse` -- the same reason
/// `submit_pick_for_seat_inner` exists.
///
/// Both `serde_json::from_str` calls deliberately precede `with_draft_mut_inner`:
/// a malformed payload fails before any session is mutated. The two parse
/// failures carry DISTINCT texts so a caller can tell which payload was bad.
fn submit_deck_inner(
    main_deck_json: &str,
    commanders_json: &str,
) -> Result<draft_core::view::DraftPlayerView, String> {
    let main_deck: Vec<String> =
        serde_json::from_str(main_deck_json).map_err(|e| format!("Failed to parse deck: {e}"))?;
    let commanders: Vec<String> = serde_json::from_str(commanders_json)
        .map_err(|e| format!("Failed to parse commanders: {e}"))?;

    with_draft_mut_inner(|session| {
        session::apply(
            session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders,
            },
            None,
        )
        .map_err(deck_submission_message)?;

        Ok(filter_for_player(session, 0))
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
            // `0`: the HUMAN designates their commander in the deck builder, so
            // the suggester must not pre-empt that choice. A decision, not a
            // default.
            let result = suggest::suggest_deck(
                pool,
                difficulty,
                card_db,
                session.config.min_deck_size,
                0,
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
// bots), the host calls `create_multiplayer_draft` with seat descriptors,
// then proxies picks/decks per-seat as guests submit them over the
// DataChannel.

/// Submit one whole CR 903.13b pick step for any seat (host proxies guest
/// picks): every card the seat drafts this step, as a JSON array of instance
/// ids. `apply_pick_inner` owns the count contract — one id for the four CR
/// 905.1a kinds, two for CommanderDraft, dropping to the remainder on an odd
/// final pick.
///
/// The JSON encoding mirrors `submit_pick_with_draft_effect_for_seat` below
/// byte for byte. It is deliberately NOT tolerant of a bare id: a bare string
/// is a parse `Err` here, which is what keeps a half-applied caller loud
/// instead of silently picking one card.
///
/// Returns the DraftPlayerView for the specified seat after the pick.
#[wasm_bindgen]
pub fn submit_pick_for_seat(seat: u8, card_instance_ids_json: &str) -> Result<JsValue, JsValue> {
    let view = submit_pick_for_seat_inner(seat, card_instance_ids_json)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&view))
}

/// Pure-Rust core for `submit_pick_for_seat`, so both halves of the payload
/// boundary are reachable from `cargo test` without `js_sys::JSON::parse` —
/// the same reason `create_multiplayer_draft_inner` exists.
///
/// The `serde_json::from_str` deliberately precedes `with_draft_mut`: a
/// malformed payload fails before any session is mutated.
fn submit_pick_for_seat_inner(
    seat: u8,
    card_instance_ids_json: &str,
) -> Result<draft_core::view::DraftPlayerView, String> {
    let card_instance_ids: Vec<String> = serde_json::from_str(card_instance_ids_json)
        .map_err(|e| format!("Failed to parse pick cards: {e}"))?;

    with_draft_mut_inner(|draft_session| {
        session::apply(
            draft_session,
            DraftAction::Pick {
                seat,
                card_instance_ids,
            },
            None,
        )
        .map_err(|e| format!("Pick failed for seat {seat}: {e}"))?;

        Ok(filter_for_player(draft_session, seat))
    })
}

/// Submit a draft-effect pick for any seat (host proxies guest picks).
///
/// Returns the filtered DraftPlayerView for the specified seat after the pick.
#[wasm_bindgen]
pub fn submit_pick_with_draft_effect_for_seat(
    seat: u8,
    effect_card_instance_id: &str,
    card_instance_ids_json: &str,
) -> Result<JsValue, JsValue> {
    let effect_card_instance_id = effect_card_instance_id.to_string();
    let card_instance_ids: Vec<String> = serde_json::from_str(card_instance_ids_json)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse draft-effect cards: {e}")))?;

    with_draft_mut(|draft_session| {
        session::apply(
            draft_session,
            DraftAction::PickWithDraftEffect {
                seat,
                effect_card_instance_id,
                card_instance_ids,
            },
            None,
        )
        .map_err(|e| {
            JsValue::from_str(&format!("Draft-effect pick failed for seat {seat}: {e}"))
        })?;

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
/// `commanders_json`: JSON array of the card names this seat designates as its
/// commander(s) (CR 903.3 / CR 702.124h). CR 903.1 puts the designation inside
/// the Commander variant, so `[]` is the correct and meaningful value for every
/// non-Commander kind.
/// Returns the DraftPlayerView for the specified seat.
#[wasm_bindgen]
pub fn submit_deck_for_seat(
    seat: u8,
    main_deck_json: &str,
    commanders_json: &str,
) -> Result<JsValue, JsValue> {
    let view = submit_deck_for_seat_inner(seat, main_deck_json, commanders_json)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&view))
}

/// Pure-Rust core for `submit_deck_for_seat`, mirroring `submit_deck_inner`:
/// both parses precede `with_draft_mut_inner`, so a malformed payload fails
/// before any session is mutated.
fn submit_deck_for_seat_inner(
    seat: u8,
    main_deck_json: &str,
    commanders_json: &str,
) -> Result<draft_core::view::DraftPlayerView, String> {
    let main_deck: Vec<String> =
        serde_json::from_str(main_deck_json).map_err(|e| format!("Failed to parse deck: {e}"))?;
    let commanders: Vec<String> = serde_json::from_str(commanders_json)
        .map_err(|e| format!("Failed to parse commanders: {e}"))?;

    with_draft_mut_inner(|session| {
        session::apply(
            session,
            DraftAction::SubmitDeck {
                seat,
                main_deck,
                commanders,
            },
            None,
        )
        .map_err(deck_submission_message)?;

        Ok(filter_for_player(session, seat))
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
/// crashed/reloaded host can restore the draft state. This is the trusted
/// authority export: unlike `DraftSourceView`, it intentionally retains a
/// Chaos layout's complete assignment matrix and must not be sent to guests.
#[wasm_bindgen]
pub fn export_draft_session() -> Result<String, JsValue> {
    with_draft(|session| {
        serde_json::to_string(session)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize draft session: {e}")))
    })?
}

/// Decode only snapshot shapes this client can safely present after a restore.
///
/// Player and spectator projections own the privacy boundary for a Chaos
/// layout: the persisted snapshot remains host-local, while all ordinary views
/// expose only `DraftSourceView`'s redacted metadata.
fn restorable_draft_session_from_json(json: &str) -> Result<DraftSession, String> {
    let session: DraftSession = serde_json::from_str(json)
        .map_err(|error| format!("Failed to deserialize draft session: {error}"))?;
    session
        .validate_persisted_snapshot()
        .map_err(|error| format!("Invalid draft snapshot: {error}"))?;
    Ok(session)
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
    let session =
        restorable_draft_session_from_json(json).map_err(|error| JsValue::from_str(&error))?;

    let offset = u64::from(session.cards_in_pack(session.current_pack_number))
        * u64::from(session.current_pack_number)
        + u64::from(session.pick_number);
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
    get_bot_deck_inner(bot_seat)
        .map(|deck| to_js(&deck))
        .map_err(|e| JsValue::from_str(&e))
}

/// Pure-Rust core for `get_bot_deck`, so the CR 903.3 designation argument is
/// reachable from `cargo test` without `js_sys` — the same reason
/// `create_multiplayer_draft_inner`, `submit_pick_for_seat_inner` and
/// `draft_procedure_dto` exist.
fn get_bot_deck_inner(bot_seat: u8) -> Result<suggest::SuggestedDeck, String> {
    with_draft_inner(|session| {
        if bot_seat == 0 || bot_seat as usize >= session.seats.len() {
            return Err("bot_seat is out of range".to_string());
        }
        let pool = &session.pools[bot_seat as usize];
        let difficulty = DIFFICULTY.with(|cell| cell.get());

        CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();

            // CR 903.3 + CR 903.6: eligibility and colour identity are both read
            // off a `CardFace`, so with no card database this crate cannot
            // designate a commander -- and a 60-card pile with no commander is
            // not a Commander deck. Refuse rather than return a deck whose
            // legality was never judged: the caller loads the database at host
            // setup, and a silent empty designation would put three of four seats
            // into a game CR 903.6 cannot start. The four CR 905.1a kinds report
            // `0` here and are unaffected.
            if session.config.kind.commanders_required() > 0 && card_db.is_none() {
                return Err(
                    "Card database must be loaded before a Commander Draft bot deck".to_string(),
                );
            }

            let deck = suggest::suggest_deck(
                pool,
                difficulty,
                card_db,
                session.config.min_deck_size,
                session.config.kind.commanders_required(),
                &session.config.addable_cards,
            );

            // CR 903.13f(1): "A player's deck must contain at least 60 cards".
            // This check enforces THIS SESSION'S configured floor,
            // `min_deck_size`, not that literal 60 -- and on the only pod shape
            // that can reach it, the two are not the same number.
            // `DeckAddableCardPolicy::CustomOnly` is written only by
            // `create_multiplayer_draft_inner`'s Cube arm, which also takes
            // `min_deck_size` from the host's cube settings, where the Set arm
            // takes it from the procedure table and hardcodes
            // `standard_basics()`. The host's control is
            // `client/src/components/draft/CubeSetupPanel.tsx`, range 1..=100,
            // default 40. So on a cube-hosted Commander pod this refuses a deck
            // short of the SESSION's floor; a 40..=59-card deck at the default
            // floor is still short of CR 903.13f(1) and is NOT caught here.
            // Making the floor itself CR-correct is a separate, pre-existing gap
            // and is deliberately out of this phase's scope.
            //
            // `min_deck_size` is also the same value `apply_submit_deck` hands
            // `validate_limited_deck` for the human on this pod
            // (`draft-core/src/session.rs`), which is what makes the two
            // authorities on this session agree. That validator rejects a short
            // deck with `LimitedDeckError::TooFewCards`
            // (`draft-core/src/validation.rs`, CR 100.2b); a bot deck reaches no
            // such gate, so the postcondition is asserted here instead. It fires
            // when `suggest_addable_cards`'s CustomOnly arm finds no addable card
            // inside the commander's colour identity (CR 903.5c) and returns
            // nothing to fill the land slots with. Refuse rather than ship a deck
            // this engine would refuse from a human on the same session: the
            // alternative is a CR 903.13f(1) violation nobody can see without
            // counting the bot's cards. The four CR 905.1a kinds report `0` here
            // and are unaffected -- without that gate this would change their
            // behaviour, which is outside this phase's scope; the general case
            // belongs to `validate_limited_deck`, which already owns it on every
            // path a human deck takes.
            let deck_total: usize =
                deck.main_deck.len() + deck.lands.values().map(|&n| n as usize).sum::<usize>();
            if session.config.kind.commanders_required() > 0
                && deck_total < session.config.min_deck_size
            {
                return Err(format!(
                    "Commander Draft bot deck reached {deck_total} cards, minimum is {}",
                    session.config.min_deck_size
                ));
            }

            Ok(deck)
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
    Human { player_id: u8, display_name: String },
    Bot { name: String },
}

/// Pool source for multiplayer draft creation.
/// Discriminated union mirroring the TS `PoolInput` type. The `data` payload
/// uses snake_case field names matching `CubeDraftSettings` and the existing
/// TS↔Rust mirror convention in `draft-adapter.ts`.
///
/// JSON examples:
///   `{ "type": "Set",  "data": { "pools": [<LimitedSetPool>, ...],
///                                 "sequence": ["isd", "dka", "avr"] } }`
///   `{ "type": "Chaos", "data": { "pools": [<LimitedSetPool>, ...],
///                                  "candidate_codes": ["isd", "dka"] } }`
///   `{ "type": "Cube", "data": { "cube_list_text": "...", "cube_name": "My Cube",
///                                 "cube_draft_settings": { ... } } }`
#[derive(Deserialize)]
#[serde(tag = "type", content = "data")]
enum PoolInput {
    Set(SetPoolInput),
    Chaos(ChaosPackSelection),
    Cube {
        cube_list_text: String,
        cube_name: String,
        cube_draft_settings: CubeDraftSettings,
    },
}

/// A pod's set-backed pool, in either spelling a host may have written.
///
/// The live shape is the same [`SetPackSequence`] the single-player entry
/// points take, so one pod boundary and one local boundary describe a pack
/// sequence identically. Hosts that predate multi-set pods persisted a single
/// serialized [`LimitedSetPool`] under `set_pool_json`; that snapshot restores
/// here as the one-pack, one-pool sequence it always meant. Same contract as
/// `DraftSource`'s `code`/`codes` alias, at the boundary rather than in the
/// snapshot.
#[derive(Deserialize)]
#[serde(untagged)]
enum SetPoolInput {
    Sequence(SetPackSequence),
    Legacy { set_pool_json: String },
}

impl SetPoolInput {
    /// Resolve to a pack sequence, promoting the legacy single-pool spelling.
    fn into_sequence(self) -> Result<SetPackSequence, String> {
        match self {
            SetPoolInput::Sequence(sequence) => Ok(sequence),
            SetPoolInput::Legacy { set_pool_json } => {
                let pool: LimitedSetPool = serde_json::from_str(&set_pool_json)
                    .map_err(|e| format!("Failed to parse set pool: {e}"))?;
                Ok(SetPackSequence {
                    sequence: vec![pool.code.clone()],
                    pools: vec![pool],
                })
            }
        }
    }
}

/// Boundary mirror of `draft_core::types::DraftProcedure` for the JS bridge.
///
/// A DTO rather than `#[derive(Serialize)]` on `DraftProcedure` itself:
/// `DraftProcedure` derives no `Serialize`, so this keeps the external procedure
/// surface intentionally explicit. `PostDraftPlay` and `PackDistribution` are
/// serialized here because a host must know whether the procedure runs in-session
/// tournament pairings; the reducer remains the authority for every constraint.
#[derive(Serialize)]
struct DraftProcedureDto {
    pod_size: u8,
    human_seats: u8,
    min_pod_size: u8,
    max_pod_size: u8,
    allowed_pod_sizes: Vec<u8>,
    packs_per_player: u8,
    cards_per_pick: u8,
    pick_selection_mode: draft_core::types::PickSelectionMode,
    distribution: draft_core::types::PackDistribution,
    min_deck_size: usize,
    commanders_required: u8,
    post_draft_play: draft_core::types::PostDraftPlay,
    launch_capability: draft_core::types::DraftLaunchCapability,
    match_config: engine::types::match_config::MatchConfig,
}

/// The engine-owned per-kind axes for a numeric draft kind. The display layer
/// reads these; it never re-derives them (CLAUDE.md: the frontend is a display
/// layer, not a logic layer).
#[wasm_bindgen]
pub fn draft_procedure(kind: u8, tournament_format: &str) -> Result<JsValue, JsValue> {
    let tournament_format =
        parse_tournament_format(tournament_format).map_err(|e| JsValue::from_str(&e))?;
    let dto = draft_procedure_dto(kind, tournament_format).map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&dto))
}

/// Pure-Rust core for `draft_procedure`, so the field mapping is reachable from
/// `cargo test` without `js_sys::JSON::parse` — the same reason
/// `create_multiplayer_draft_inner` and `submit_pick_for_seat_inner` exist.
///
/// The mapping is hand-written and its `u8` columns are interchangeable to the
/// compiler, so transposing any two of them compiles clean and passes clippy.
/// Nothing in the type system catches that;
/// `draft_procedure_dto_copies_every_axis_unmoved` is the substitute for the
/// missing type error.
fn draft_procedure_dto(
    kind: u8,
    tournament_format: TournamentFormat,
) -> Result<DraftProcedureDto, String> {
    let procedure = draft_kind_from_wire(kind)?.procedure();
    Ok(DraftProcedureDto {
        pod_size: procedure.pod_size,
        human_seats: procedure.human_seats,
        min_pod_size: procedure.min_pod_size,
        max_pod_size: procedure.max_pod_size,
        allowed_pod_sizes: procedure.allowed_pod_sizes(tournament_format),
        packs_per_player: procedure.packs_per_player,
        cards_per_pick: procedure.cards_per_pick,
        pick_selection_mode: procedure.pick_selection_mode,
        distribution: procedure.distribution,
        min_deck_size: procedure.min_deck_size,
        commanders_required: procedure.commanders_required,
        post_draft_play: procedure.post_draft_play,
        launch_capability: procedure.launch_capability(),
        match_config: procedure.match_config,
    })
}

fn parse_tournament_format(tournament_format: &str) -> Result<TournamentFormat, String> {
    match tournament_format {
        "Swiss" => Ok(TournamentFormat::Swiss),
        "SingleElimination" => Ok(TournamentFormat::SingleElimination),
        _ => Err("tournament_format must be Swiss or SingleElimination".to_string()),
    }
}

/// The numeric `kind` the JS bridge sends, and the single authority for it.
///
/// Wildcard-free on the ENUM side: a sixth `DraftKind` is an `E0004` HERE, so a
/// new kind cannot reach the bridge without a wire number. The base's
/// `match kind: u8` could not do that — `DraftKind::CommanderDraft` existed for
/// two phases while this decode still stopped at 3.
fn draft_kind_wire_number(kind: DraftKind) -> u8 {
    match kind {
        DraftKind::Quick => 0,
        DraftKind::Premier => 1,
        DraftKind::Traditional => 2,
        DraftKind::Sealed => 3,
        // CR 903.13a: the fifth kind.
        DraftKind::CommanderDraft => 4,
    }
}

/// Decode, derived from the encode over `DraftKind::ALL` — one numeric table,
/// not two. There is deliberately no arm that yields a `DraftKind` from an
/// unmapped input: an unknown number is an `Err`, never a default.
fn draft_kind_from_wire(kind: u8) -> Result<DraftKind, String> {
    DraftKind::ALL
        .into_iter()
        .find(|k| draft_kind_wire_number(*k) == kind)
        .ok_or_else(|| {
            let known = DraftKind::ALL
                .into_iter()
                .map(|k| format!("{} ({k:?})", draft_kind_wire_number(k)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("unknown draft kind {kind}; expected one of {known}")
        })
}

/// Create a multiplayer draft session. Used by the P2P host to initialize a
/// Premier, Traditional, Sealed, or Commander draft with human + bot seats from
/// a Set pool, host-local Chaos candidate pools, or a custom Cube list.
///
/// - `pool_input_json`: serialized `PoolInput` discriminated union
///   (`{ "type": "Set" | "Chaos" | "Cube", "data": { ... } }`)
/// - `seats_json`: JSON array of SeatDescriptors
/// - `kind`: 0=Quick, 1=Premier, 2=Traditional, 3=Sealed, 4=CommanderDraft
///   (CR 903.13a). The mapping's single authority is `draft_kind_wire_number`.
///   Flows through to `DraftConfig.kind` unchanged. Tournament match format is
///   identical to set drafts.
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

    let draft_kind = draft_kind_from_wire(kind)?;

    let tournament_format = parse_tournament_format(tournament_format)?;

    let pod_policy = match pod_policy {
        "Competitive" => PodPolicy::Competitive,
        "Casual" => PodPolicy::Casual,
        _ => {
            return Err("pod_policy must be Competitive or Casual".to_string());
        }
    };

    // This entry point hosts a draft for remote peers. It deliberately uses
    // the public procedure range, not Quick Draft's local-cube allowance, so
    // an arbitrary seat payload cannot allocate an unbounded remote pod.
    let procedure = draft_kind.procedure();
    let allowed = procedure.allowed_pod_size_range(tournament_format);
    let pod_size = u8::try_from(seat_descriptors.len()).map_err(|_| {
        format!(
            "pod_size must be between {} and {}",
            allowed.start(),
            allowed.end()
        )
    })?;
    if !procedure.allows_pod_size(tournament_format, pod_size) {
        return Err(format!(
            "pod_size must be between {} and {}",
            allowed.start(),
            allowed.end()
        ));
    }

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
        PoolInput::Set(set_pool_input) => {
            // The procedure table is the single authority for how many boosters
            // this kind opens; the host's selection only decides which set fills
            // each of them. Same contract as the single-player entry points.
            let selection = ResolvedSetSelection::resolve(
                set_pool_input.into_sequence()?,
                Some(procedure.packs_per_player),
            )?;

            let config = DraftConfig {
                set_code: selection.source.set_code(),
                source: selection.source,
                kind: draft_kind,
                pod_size,
                cards_per_pack: selection.cards_per_pack,
                pack_count: selection.pack_count,
                min_deck_size: procedure.min_deck_size,
                addable_cards: DeckAddableCards::standard_basics(),
                rng_seed: seed as u64,
                tournament_format,
                pod_policy,
                spectator_visibility: SpectatorVisibility::default(),
            };

            let mut draft_session = DraftSession::new(config, seats, draft_code.to_string());
            let pack_gen = selection.generator;

            session::apply(&mut draft_session, DraftAction::StartDraft, Some(&pack_gen))
                .map_err(|e| format!("Failed to start draft: {}", e))?;

            let view = filter_for_player(&draft_session, 0);

            DRAFT_SESSION.with(|cell| cell.set(Some(draft_session)));
            PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
            RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

            Ok(view)
        }
        PoolInput::Chaos(chaos_selection) => {
            let selection = ResolvedChaosSelection::resolve(
                chaos_selection,
                pod_size,
                procedure.packs_per_player,
                seed as u64,
            )?;

            let config = DraftConfig {
                set_code: selection.source.set_code(),
                source: selection.source,
                kind: draft_kind,
                pod_size,
                cards_per_pack: selection.cards_per_pack,
                pack_count: selection.pack_count,
                min_deck_size: procedure.min_deck_size,
                addable_cards: DeckAddableCards::standard_basics(),
                rng_seed: seed as u64,
                tournament_format,
                pod_policy,
                spectator_visibility: SpectatorVisibility::default(),
            };

            let mut draft_session = DraftSession::new(config, seats, draft_code.to_string());
            let pack_gen = selection.generator;

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
            // A cube pool has no unopened-pack distribution, so an all-at-once
            // kind cannot be run from one.
            match draft_kind.procedure().distribution {
                PackDistribution::AllAtOnce => {
                    return Err("Sealed events require a Set pool".to_string());
                }
                PackDistribution::PickAndPass => {}
            }
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
                pod_size,
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
///   `{ "type": "Pick", "data": { "seat": 2, "card_instance_ids": ["abc-123"] } }`
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
    use draft_core::validation::LimitedDeckError;

    #[test]
    fn deck_submission_message_includes_validation_details() {
        let message = deck_submission_message(DraftError::ValidationFailed {
            errors: vec![LimitedDeckError::NotInPool {
                name: "Watery Grave".to_string(),
            }],
        });

        assert_eq!(message, "card 'Watery Grave' is not in the drafted pool");
    }

    /// The live pod spelling: pools plus the pack-ordered sequence naming which
    /// of them fills each booster — the same shape the single-player entry
    /// points take.
    #[test]
    fn pool_input_set_round_trip() {
        let json = r#"{"type":"Set","data":{
            "pools":[{"code":"foo","name":"Foo","release_date":null,
                      "pack_variants":[],"pack_variants_total_weight":0,
                      "sheets":{},"prints":[],"basic_lands":[]}],
            "sequence":["foo","foo","foo"]}}"#;
        let parsed: PoolInput = serde_json::from_str(json).unwrap();
        match parsed {
            PoolInput::Set(input) => {
                let sequence = input.into_sequence().expect("already a sequence");
                assert_eq!(sequence.sequence, vec!["foo".to_string(); 3]);
                assert_eq!(sequence.pools.len(), 1);
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn pool_input_chaos_carries_candidates_without_assignments() {
        let json = r#"{"type":"Chaos","data":{"pools":[],"candidate_codes":["foo"]}}"#;
        let parsed: PoolInput = serde_json::from_str(json).unwrap();
        match parsed {
            PoolInput::Chaos(input) => {
                assert_eq!(input.candidate_codes, vec!["foo".to_string()]);
                assert!(input.pools.is_empty());
            }
            _ => panic!("expected Chaos"),
        }
    }

    /// The pre-multi-set spelling one pod host may still have persisted:
    /// a single serialized pool, promoted to the one-element sequence it meant.
    #[test]
    fn pool_input_set_accepts_the_legacy_single_pool() {
        let pool = r#"{"code":"foo","name":"Foo","release_date":null,
            "pack_variants":[],"pack_variants_total_weight":0,
            "sheets":{},"prints":[],"basic_lands":[]}"#;
        let json = serde_json::json!({
            "type": "Set",
            "data": { "set_pool_json": pool }
        })
        .to_string();

        let parsed: PoolInput = serde_json::from_str(&json).unwrap();
        match parsed {
            PoolInput::Set(input) => {
                let sequence = input.into_sequence().expect("the legacy pool parses");
                assert_eq!(sequence.sequence, vec!["foo".to_string()]);
                assert_eq!(sequence.pools.len(), 1);
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
mod set_selection_tests {
    use std::collections::BTreeMap;

    use super::*;

    fn declared_pool(code: &str, variant_sizes: &[u8]) -> LimitedSetPool {
        LimitedSetPool {
            code: code.to_string(),
            name: format!("{code} Set"),
            release_date: None,
            pack_variants: variant_sizes
                .iter()
                .map(|size| draft_core::set_pool::PackVariant {
                    contents: vec![draft_core::set_pool::PackSlot {
                        slot: "common".to_string(),
                        count: *size,
                        choices: Vec::new(),
                    }],
                    weight: 1,
                })
                .collect(),
            pack_variants_total_weight: variant_sizes.len() as u32,
            sheets: BTreeMap::new(),
            prints: Vec::new(),
            basic_lands: Vec::new(),
        }
    }

    /// A minimal `LimitedSetPool` whose single variant holds `size` commons.
    fn pool_json(code: &str, size: u8) -> String {
        let cards: Vec<String> = (0..40)
            .map(|i| {
                format!(
                    r#"{{"name":"{code} Card {i}","set_code":"{code}","collector_number":"{n}","rarity":"common","weight":1}}"#,
                    n = i + 1
                )
            })
            .collect();
        format!(
            r#"{{
                "code": "{code}",
                "name": "{code} Set",
                "release_date": null,
                "pack_variants": [{{
                    "contents": [{{
                        "slot": "common",
                        "count": {size},
                        "choices": [{{ "sheet": "common", "weight": 1 }}]
                    }}],
                    "weight": 1
                }}],
                "pack_variants_total_weight": 1,
                "sheets": {{
                    "common": {{
                        "cards": [{cards}],
                        "total_weight": 40,
                        "foil": false,
                        "balance_colors": false
                    }}
                }},
                "prints": [],
                "basic_lands": []
            }}"#,
            cards = cards.join(",")
        )
    }

    fn selection_json(pools: &[(&str, u8)], sequence: &[&str]) -> String {
        let pools: Vec<String> = pools
            .iter()
            .map(|(code, size)| pool_json(code, *size))
            .collect();
        let sequence: Vec<String> = sequence.iter().map(|c| format!("\"{c}\"")).collect();
        format!(
            r#"{{ "pools": [{}], "sequence": [{}] }}"#,
            pools.join(","),
            sequence.join(",")
        )
    }

    #[test]
    fn a_selection_sets_the_pack_count_from_its_sequence_and_repeats_sets() {
        let selection = ResolvedSetSelection::parse(
            &selection_json(&[("AAA", 15), ("BBB", 14)], &["AAA", "BBB", "AAA"]),
            None,
        )
        .expect("both named sets have pool data");

        assert_eq!(selection.pack_count, 3);
        // The nominal size follows pack 1; per-pack sizes are recorded on the
        // session from the packs the generator produces.
        assert_eq!(selection.cards_per_pack, 15);
        assert_eq!(
            selection.source,
            DraftSource::Set {
                layout: SetLayout::UniformByRound {
                    codes: vec!["AAA".to_string(), "BBB".to_string(), "AAA".to_string()],
                },
            }
        );
        assert_eq!(selection.source.set_code(), "AAA+BBB");
    }

    #[test]
    fn chaos_preflight_checks_every_selectable_pool_and_each_variant_shape() {
        let mismatch = preflight_chaos_pools(
            &[declared_pool("AAA", &[15]), declared_pool("BBB", &[14])],
            &["AAA".to_string(), "BBB".to_string()],
        )
        .expect_err("all selectable Chaos pools must share a total");
        assert!(mismatch.contains("BBB"), "unexpected error: {mismatch}");

        let variable =
            preflight_chaos_pools(&[declared_pool("AAA", &[15, 14])], &["AAA".to_string()])
                .expect_err("variable MTGJSON variants are invalid for Chaos");
        assert!(
            variable.contains("no single MTGJSON pack size"),
            "unexpected error: {variable}"
        );

        assert_eq!(
            preflight_chaos_pools(
                &[declared_pool("AAA", &[15]), declared_pool("BBB", &[15])],
                &["AAA".to_string(), "BBB".to_string()],
            )
            .unwrap(),
            (vec!["AAA".to_string(), "BBB".to_string()], 15)
        );
    }

    #[test]
    fn private_chaos_construction_persists_deterministic_assignments() {
        let first = ResolvedChaosSelection::resolve(
            ChaosPackSelection {
                pools: vec![declared_pool("AAA", &[15]), declared_pool("BBB", &[15])],
                candidate_codes: vec!["AAA".to_string(), "BBB".to_string()],
            },
            2,
            3,
            42,
        )
        .unwrap();
        let replay = ResolvedChaosSelection::resolve(
            ChaosPackSelection {
                pools: vec![declared_pool("AAA", &[15]), declared_pool("BBB", &[15])],
                candidate_codes: vec!["AAA".to_string(), "BBB".to_string()],
            },
            2,
            3,
            42,
        )
        .unwrap();

        assert_eq!(first.source, replay.source);
        assert!(matches!(
            first.source,
            DraftSource::Set {
                layout: SetLayout::Chaos { assignments, .. }
            } if assignments.len() == 2 && assignments.iter().all(|rounds| rounds.len() == 3)
        ));
    }

    #[test]
    fn a_selection_naming_a_set_it_did_not_supply_is_rejected() {
        let error =
            ResolvedSetSelection::parse(&selection_json(&[("AAA", 15)], &["AAA", "MISSING"]), None)
                .err()
                .expect("the sequence names a set with no pool");

        assert!(error.contains("MISSING"), "unexpected error: {error}");
    }

    #[test]
    fn an_empty_selection_is_rejected() {
        assert!(ResolvedSetSelection::parse(&selection_json(&[("AAA", 15)], &[]), None).is_err());
    }

    /// A fixed-length event opens its own booster count regardless of how many
    /// sets the selection names. Naming fewer repeats the last entry, so the
    /// one-element single-set shorthand still fills all six; naming MORE would
    /// silently drop boosters, so it is refused.
    #[test]
    fn sealed_opens_six_boosters_from_a_shorter_or_exact_selection() {
        let one = selection_json(&[("AAA", 15)], &["AAA"]);
        let selection = ResolvedSetSelection::parse(&one, Some(SEALED_PACK_COUNT))
            .expect("a one-element sequence fills every booster");
        assert_eq!(selection.pack_count, SEALED_PACK_COUNT);
        assert_eq!(
            selection.source.set_code_for_pack(SEALED_PACK_COUNT - 1),
            "AAA"
        );

        let six = selection_json(
            &[("AAA", 15), ("BBB", 14)],
            &["AAA", "AAA", "AAA", "BBB", "BBB", "BBB"],
        );
        let selection = ResolvedSetSelection::parse(&six, Some(SEALED_PACK_COUNT))
            .expect("six boosters is a valid sealed selection");
        assert_eq!(selection.pack_count, SEALED_PACK_COUNT);
        assert_eq!(selection.source.set_code_for_pack(5), "BBB");

        let seven = selection_json(
            &[("AAA", 15)],
            &["AAA", "AAA", "AAA", "AAA", "AAA", "AAA", "AAA"],
        );
        let error = ResolvedSetSelection::parse(&seven, Some(SEALED_PACK_COUNT))
            .err()
            .expect("seven sets name a booster sealed never opens");
        assert!(error.contains('7'), "unexpected error: {error}");
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

    fn persisted_premier_session(layout: SetLayout) -> DraftSession {
        let source = DraftSource::Set { layout };
        let config = DraftConfig {
            set_code: source.set_code(),
            source,
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats = (0..8)
            .map(|seat| DraftSeat::Human {
                player_id: engine::types::player::PlayerId(seat),
                display_name: format!("Player {seat}"),
            })
            .collect();
        DraftSession::new(config, seats, "persisted-draft".to_string())
    }

    #[test]
    fn import_accepts_persisted_chaos_only_through_a_redacted_player_view() {
        clear_state();

        let uniform = persisted_premier_session(SetLayout::UniformByRound {
            codes: vec!["TST".to_string()],
        });
        let uniform_json = serde_json::to_string(&uniform).expect("serialize uniform snapshot");
        assert!(
            restorable_draft_session_from_json(&uniform_json).is_ok(),
            "existing Uniform snapshots must continue to restore"
        );

        let chaos = persisted_premier_session(SetLayout::Chaos {
            candidate_codes: vec!["TST".to_string()],
            assignments: vec![vec!["TST".to_string(); 3]; 8],
        });
        let chaos_json = serde_json::to_string(&chaos).expect("serialize Chaos snapshot");
        let restored = restorable_draft_session_from_json(&chaos_json)
            .expect("redacted player views make persisted Chaos snapshots restorable");
        let view = filter_for_player(&restored, 0);

        let serialized = serde_json::to_value(view).expect("serialize redacted view");
        assert!(
            serialized
                .pointer("/source/data/layout/Chaos/assignments")
                .is_none(),
            "a player view must never serialize the host-only assignments"
        );
        assert_eq!(
            serialized.pointer("/source/data/layout/Chaos/candidate_codes"),
            Some(&serde_json::json!(["TST"])),
        );
        DRAFT_SESSION.with(|cell| assert!(cell.take().is_none()));
    }

    #[test]
    fn import_rejects_redacted_chaos_snapshot_that_claims_a_uniform_layout() {
        clear_state();

        let chaos = persisted_premier_session(SetLayout::Chaos {
            candidate_codes: vec!["TST".to_string()],
            assignments: vec![vec!["TST".to_string(); 3]; 8],
        });
        let mut snapshot = serde_json::to_value(chaos).expect("serialize Chaos snapshot");
        let layout = snapshot
            .pointer_mut("/config/source/data")
            .expect("canonical DraftSource layout");
        *layout = serde_json::json!({
            "candidate_codes": ["TST"],
            "codes": ["TST"],
        });

        let error = restorable_draft_session_from_json(&snapshot.to_string())
            .expect_err("a public redaction is not a restorable Chaos snapshot");
        assert!(error.contains("Failed to deserialize draft session"));
        DRAFT_SESSION.with(|cell| assert!(cell.take().is_none()));
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
            card_instance_ids: vec![picked.clone()],
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

    #[test]
    fn set_branch_uses_the_mtgjson_pack_size_for_draft_and_sealed() {
        let set_pool_json = r#"{
            "code": "TST",
            "name": "Test Set",
            "release_date": null,
            "pack_variants": [{
                "contents": [{ "slot": "common", "count": 3, "choices": [{ "sheet": "common", "weight": 1 }] }],
                "weight": 1
            }],
            "pack_variants_total_weight": 1,
            "sheets": {
                "common": {
                    "cards": [
                        { "name": "Alpha", "set_code": "TST", "collector_number": "1", "rarity": "common", "weight": 1 },
                        { "name": "Beta", "set_code": "TST", "collector_number": "2", "rarity": "common", "weight": 1 },
                        { "name": "Gamma", "set_code": "TST", "collector_number": "3", "rarity": "common", "weight": 1 }
                    ],
                    "total_weight": 3,
                    "foil": false,
                    "balance_colors": false
                }
            },
            "prints": [],
            "basic_lands": []
        }"#;
        let pool_input_json = serde_json::json!({
            "type": "Set",
            "data": { "set_pool_json": set_pool_json }
        })
        .to_string();
        let seats_json = r#"[
            { "type": "Human", "player_id": 0, "display_name": "Host" },
            { "type": "Human", "player_id": 1, "display_name": "Guest" }
        ]"#;

        clear_state();
        let draft_view = create_multiplayer_draft_inner(
            &pool_input_json,
            seats_json,
            1,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("set draft should start");
        assert_eq!(draft_view.cards_per_pack, 3);
        assert_eq!(draft_view.current_pack.expect("current pack").len(), 3);

        clear_state();
        let sealed_view = create_multiplayer_draft_inner(
            &pool_input_json,
            seats_json,
            3,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("set sealed should start");
        assert_eq!(sealed_view.cards_per_pack, 3);
        assert_eq!(sealed_view.pool.len(), 18);
        assert!(sealed_view
            .sealed_packs
            .expect("sealed pack boundaries")
            .iter()
            .all(|pack| pack.len() == 3));

        clear_state();
    }

    /// One set pool with `code`, three commons, a fixed 3-card booster.
    fn pod_pool_json(code: &str) -> String {
        format!(
            r#"{{
            "code": "{code}",
            "name": "Set {code}",
            "release_date": null,
            "pack_variants": [{{
                "contents": [{{ "slot": "common", "count": 3, "choices": [{{ "sheet": "common", "weight": 1 }}] }}],
                "weight": 1
            }}],
            "pack_variants_total_weight": 1,
            "sheets": {{
                "common": {{
                    "cards": [
                        {{ "name": "Alpha", "set_code": "{code}", "collector_number": "1", "rarity": "common", "weight": 1 }},
                        {{ "name": "Beta", "set_code": "{code}", "collector_number": "2", "rarity": "common", "weight": 1 }},
                        {{ "name": "Gamma", "set_code": "{code}", "collector_number": "3", "rarity": "common", "weight": 1 }}
                    ],
                    "total_weight": 3,
                    "foil": false,
                    "balance_colors": false
                }}
            }},
            "prints": [],
            "basic_lands": []
        }}"#
        )
    }

    /// A pod's `PoolInput::Set` in the live sequence spelling.
    fn pod_sequence_pool_input(pools: &[&str], sequence: &[&str]) -> String {
        let pools: Vec<serde_json::Value> = pools
            .iter()
            .map(|code| serde_json::from_str(&pod_pool_json(code)).expect("pool fixture"))
            .collect();
        serde_json::json!({
            "type": "Set",
            "data": { "pools": pools, "sequence": sequence }
        })
        .to_string()
    }

    fn pod_chaos_pool_input(candidates: &[&str]) -> String {
        let pools: Vec<serde_json::Value> = candidates
            .iter()
            .map(|code| serde_json::from_str(&pod_pool_json(code)).expect("pool fixture"))
            .collect();
        serde_json::json!({
            "type": "Chaos",
            "data": { "pools": pools, "candidate_codes": candidates }
        })
        .to_string()
    }

    fn two_human_seats_json() -> &'static str {
        r#"[
            { "type": "Human", "player_id": 0, "display_name": "Host" },
            { "type": "Human", "player_id": 1, "display_name": "Guest" }
        ]"#
    }

    #[test]
    fn remote_quick_draft_rejects_pods_outside_the_public_procedure_range() {
        for seat_count in [1, 9] {
            let seats_json = format!(
                "[{}]",
                (0..seat_count)
                    .map(|seat| {
                        format!(
                            r#"{{ "type": "Human", "player_id": {seat}, "display_name": "P{seat}" }}"#
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            );

            let err = create_multiplayer_draft_inner(
                &pod_sequence_pool_input(&["AAA"], &["AAA"]),
                &seats_json,
                0, // Quick
                42,
                "test-room",
                "Swiss",
                "Competitive",
            )
            .expect_err("remote Quick Draft must use its public 2..=8 pod range");

            assert!(err.contains("pod_size must be between 2 and 8"), "{err}");
        }
    }

    /// THE multiplayer multi-set claim: a pod host names one set per booster,
    /// and the pod that starts opens those sets in that order.
    ///
    /// Asserts the ORDER, not just the set membership — a pod that resolved
    /// only its first code, or that deduped the sequence, would deal every
    /// later booster from the wrong set while still reporting the right sets.
    #[test]
    fn a_pod_opens_each_booster_from_its_own_set_in_pack_order() {
        clear_state();
        let view = create_multiplayer_draft_inner(
            &pod_sequence_pool_input(&["AAA", "BBB"], &["AAA", "BBB", "AAA"]),
            two_human_seats_json(),
            1,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("a three-booster Premier pod is a valid multi-set selection");

        assert_eq!(
            view.pack_set_codes,
            vec!["AAA".to_string(), "BBB".to_string(), "AAA".to_string()]
        );
        // Pack 1 is open, so every card in hand comes from the sequence's first
        // entry — the pool the generator resolved for that booster.
        let pack = view.current_pack.expect("current pack");
        assert_eq!(pack.len(), 3);
        assert!(
            pack.iter().all(|card| card.set_code == "AAA"),
            "pack 1 must be dealt from the sequence's first set: {:?}",
            pack.iter().map(|c| &c.set_code).collect::<Vec<_>>()
        );
        clear_state();
    }

    #[test]
    fn a_chaos_pod_derives_and_persists_assignments_from_host_candidates() {
        clear_state();
        let view = create_multiplayer_draft_inner(
            &pod_chaos_pool_input(&["AAA", "BBB"]),
            two_human_seats_json(),
            1,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("a Chaos pod should resolve host-local assignments");

        let source = serde_json::to_value(&view.source).expect("view source serializes");
        assert_eq!(
            source.pointer("/data/layout/Chaos/candidate_codes"),
            Some(&serde_json::json!(["AAA", "BBB"])),
        );
        assert!(
            source.pointer("/data/layout/Chaos/assignments").is_none(),
            "player views must not expose host-local assignments"
        );

        let persisted = export_draft_session().expect("host session exports");
        let persisted: serde_json::Value = serde_json::from_str(&persisted).expect("session JSON");
        let assignments = persisted
            .pointer("/config/source/data/assignments")
            .expect("host session persists exact Chaos assignments");
        assert_eq!(assignments.as_array().map(Vec::len), Some(2));
        assert!(assignments
            .as_array()
            .expect("assignment rows")
            .iter()
            .all(|row| row.as_array().map(Vec::len) == Some(3)));
        clear_state();
    }

    /// The kind's procedure fixes how many boosters a pod opens. A host who
    /// names MORE sets than that is refused at creation rather than having the
    /// extra boosters silently dropped. Premier opens three; Sealed opens six.
    #[test]
    fn a_pod_refuses_a_sequence_longer_than_its_kinds_pack_count() {
        clear_state();
        let err = create_multiplayer_draft_inner(
            &pod_sequence_pool_input(&["AAA", "BBB"], &["AAA", "BBB", "AAA", "BBB"]),
            two_human_seats_json(),
            1,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect_err("four sets name a booster Premier never opens");
        assert!(err.contains('4'), "unexpected error: {err}");

        let sealed = create_multiplayer_draft_inner(
            &pod_sequence_pool_input(&["AAA", "BBB"], &["AAA", "BBB", "AAA", "BBB", "AAA", "BBB"]),
            two_human_seats_json(),
            3,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("six boosters is a valid Sealed pod selection");
        assert_eq!(sealed.pack_set_codes.len(), 6);
        clear_state();
    }

    /// A sequence naming a set the host shipped no pool for is refused by name,
    /// rather than starting a pod whose later packs come up empty.
    #[test]
    fn a_pod_refuses_a_sequence_naming_an_unsupplied_set() {
        clear_state();
        let err = create_multiplayer_draft_inner(
            &pod_sequence_pool_input(&["AAA"], &["AAA", "MISSING", "AAA"]),
            two_human_seats_json(),
            1,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect_err("the sequence names a set with no pool data");
        assert!(err.contains("MISSING"), "unexpected error: {err}");
        clear_state();
    }

    /// A host that persisted its pod before multi-set pods existed wrote a
    /// single serialized `LimitedSetPool`. Resuming must restore the pod it
    /// always described — every booster from that one set — not fail the frame.
    #[test]
    fn a_pod_resumes_from_the_legacy_single_pool_spelling() {
        clear_state();
        let legacy = serde_json::json!({
            "type": "Set",
            "data": { "set_pool_json": pod_pool_json("AAA") }
        })
        .to_string();

        let view = create_multiplayer_draft_inner(
            &legacy,
            two_human_seats_json(),
            1,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("a legacy single-pool pod still starts");

        assert_eq!(
            view.pack_set_codes,
            vec!["AAA".to_string(), "AAA".to_string(), "AAA".to_string()],
            "a one-element sequence repeats its last entry for every booster"
        );
        clear_state();
    }

    /// A 4-seat Commander pod over a Set pool. Packs are deliberately large
    /// enough that a CR 903.13b two-card step is not the whole pack.
    fn commander_pool_input_json() -> String {
        let set_pool_json = r#"{
            "code": "TST",
            "name": "Test Set",
            "release_date": null,
            "pack_variants": [{
                "contents": [{ "slot": "common", "count": 4, "choices": [{ "sheet": "common", "weight": 1 }] }],
                "weight": 1
            }],
            "pack_variants_total_weight": 1,
            "sheets": {
                "common": {
                    "cards": [
                        { "name": "Alpha", "set_code": "TST", "collector_number": "1", "rarity": "common", "weight": 1 },
                        { "name": "Beta", "set_code": "TST", "collector_number": "2", "rarity": "common", "weight": 1 },
                        { "name": "Gamma", "set_code": "TST", "collector_number": "3", "rarity": "common", "weight": 1 },
                        { "name": "Delta", "set_code": "TST", "collector_number": "4", "rarity": "common", "weight": 1 }
                    ],
                    "total_weight": 4,
                    "foil": false,
                    "balance_colors": false
                }
            },
            "prints": [],
            "basic_lands": []
        }"#;
        serde_json::json!({
            "type": "Set",
            "data": { "set_pool_json": set_pool_json }
        })
        .to_string()
    }

    const COMMANDER_SEATS_JSON: &str = r#"[
        { "type": "Human", "player_id": 0, "display_name": "Host" },
        { "type": "Human", "player_id": 1, "display_name": "G1" },
        { "type": "Human", "player_id": 2, "display_name": "G2" },
        { "type": "Human", "player_id": 3, "display_name": "G3" }
    ]"#;

    /// U10's discriminating test.
    ///
    /// The NEGATIVE half is the point: assert the resulting session's KIND,
    /// never merely that the call returned `Ok`. A decode that silently
    /// resolved 4 to `Quick` would satisfy an `is_ok()` assertion perfectly.
    #[test]
    fn create_multiplayer_draft_inner_accepts_commander_kind() {
        clear_state();
        let view = create_multiplayer_draft_inner(
            &commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            4, // CommanderDraft
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("commander draft should start");

        assert!(matches!(view.kind, DraftKind::CommanderDraft));
        // CR 903.13f(1) and CR 903.13b reach the view through the procedure, so
        // this also proves the config was built from `DraftProcedure` and not
        // from the 40-card/3-pack literals the four older kinds share.
        assert_eq!(view.min_deck_size, 60);
        assert_eq!(view.pack_count, 3);

        clear_state();
    }

    /// Hostile fixture, paired with the positive above so it cannot pass
    /// vacuously: an id BEYOND the table must be an `Err`, never a default.
    #[test]
    fn create_multiplayer_draft_inner_refuses_an_unmapped_kind() {
        clear_state();
        let err = create_multiplayer_draft_inner(
            &commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            5,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect_err("kind 5 is unmapped");
        assert!(
            err.contains("unknown draft kind 5"),
            "unexpected error: {err}"
        );

        clear_state();
    }

    /// The numeric table is total and injective over every kind, and the decode
    /// is the encode's inverse. Folds `DraftKind::ALL`, so it moves with the
    /// enum.
    #[test]
    fn draft_kind_wire_numbers_round_trip() {
        for kind in DraftKind::ALL {
            assert_eq!(
                draft_kind_from_wire(draft_kind_wire_number(kind)).unwrap(),
                kind
            );
        }
        let mut numbers: Vec<u8> = DraftKind::ALL
            .into_iter()
            .map(draft_kind_wire_number)
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(
            numbers.len(),
            DraftKind::ALL.len(),
            "wire numbers must be distinct"
        );
    }

    /// Boundary D, positive half: the multi-seat pick export carries one WHOLE
    /// CR 903.13b step, and a Commander pod's step is two cards.
    ///
    /// This is also the paired reach-guard for the two negatives below — it
    /// proves the export works on a live session, so their `Err`s cannot be
    /// satisfied by a wholesale failure of the call.
    #[test]
    fn submit_pick_for_seat_takes_a_whole_commander_pick_step() {
        clear_state();
        let view = create_multiplayer_draft_inner(
            &commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            4,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("commander draft should start");
        let pack = view.current_pack.as_ref().expect("seat 0 has a pack");
        let two = serde_json::json!([pack[0].instance_id, pack[1].instance_id]).to_string();

        let after = submit_pick_for_seat_inner(0, &two).expect("a two-card step applies");
        assert_eq!(
            after.pool.len(),
            2,
            "CR 903.13b: a Commander pick step drafts two cards"
        );

        clear_state();
    }

    /// The count negative: a ONE-id payload is not a whole Commander step, so
    /// the reducer refuses it (CR 903.13b). This is the assertion that would
    /// flip if `submit_pick_for_seat` went back to wrapping a single id, and
    /// its paired positive reach-guard is the two-id test above.
    #[test]
    fn submit_pick_for_seat_refuses_a_one_card_commander_step() {
        clear_state();
        let view = create_multiplayer_draft_inner(
            &commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            4,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("commander draft should start");
        let pack = view.current_pack.as_ref().expect("seat 0 has a pack");
        let one = serde_json::json!([pack[0].instance_id]).to_string();

        let err =
            submit_pick_for_seat_inner(0, &one).expect_err("one id is not a whole CR 903.13b step");
        assert!(
            err.contains("Pick failed for seat 0"),
            "unexpected error: {err}"
        );

        clear_state();
    }

    /// Boundary D, negative half: a BARE id is not a payload.
    ///
    /// Nothing in the compiler catches a half-applied caller here — both sides
    /// of the boundary are `string` and the call is positional — so this
    /// assertion is the substitute for the missing type error. It also pins the
    /// ordering: the parse fails BEFORE any session is entered.
    #[test]
    fn submit_pick_for_seat_refuses_a_bare_id() {
        clear_state();
        let err =
            submit_pick_for_seat_inner(0, "card-abc").expect_err("a bare id is not a JSON array");
        assert!(
            err.contains("Failed to parse pick cards"),
            "unexpected error: {err}"
        );
        // Reached the parse, not the session: no draft is initialized here, so
        // a decoder that ran AFTER `with_draft_mut_inner` would have reported
        // "Draft not initialized" instead.
        assert!(
            !err.contains("Draft not initialized"),
            "parse must precede the session: {err}"
        );

        clear_state();
    }

    /// The `DraftProcedureDto` mapping is a hand-written copy whose `u8` columns
    /// are interchangeable to the compiler, so transposing any two of them
    /// compiles clean and passes clippy while publishing a wrong axis to the
    /// display layer. Nothing in the type system catches that; this is the
    /// substitute for the missing type error.
    ///
    /// Folded over `DraftKind::ALL` rather than asserted on `CommanderDraft`
    /// alone, and that is load-bearing: in the Commander row `min_pod_size` and
    /// `packs_per_player` are BOTH `3`, so a single-kind test — field by field
    /// or not — stays green with exactly those two swapped. Over the whole
    /// table the `u8` columns are pairwise distinct AS COLUMNS, so every
    /// transposition reddens here. `commanders_required` is `[0, 0, 0, 0, 1]`
    /// over `DraftKind::ALL` and equals no other column (`pod_size`
    /// `[8, 8, 8, 8, 4]`, `human_seats` `[1, 8, 8, 8, 1]`, `min_pod_size`
    /// `[2, 2, 2, 2, 3]`, `packs_per_player` `[3, 3, 3, 6, 3]`, `cards_per_pick`
    /// `[1, 1, 1, 1, 2]`), so the argument survives its addition.
    #[test]
    fn draft_procedure_dto_copies_every_axis_unmoved() {
        for kind in DraftKind::ALL {
            let procedure = kind.procedure();
            let dto = draft_procedure_dto(draft_kind_wire_number(kind), TournamentFormat::Swiss)
                .unwrap_or_else(|e| panic!("{kind:?} has a wire number: {e}"));

            assert_eq!(dto.pod_size, procedure.pod_size, "pod_size ({kind:?})");
            assert_eq!(
                dto.human_seats, procedure.human_seats,
                "human_seats ({kind:?})"
            );
            assert_eq!(
                dto.min_pod_size, procedure.min_pod_size,
                "min_pod_size ({kind:?})"
            );
            assert_eq!(
                dto.max_pod_size, procedure.max_pod_size,
                "max_pod_size ({kind:?})"
            );
            assert_eq!(
                dto.allowed_pod_sizes,
                procedure.allowed_pod_sizes(TournamentFormat::Swiss),
                "allowed_pod_sizes ({kind:?})"
            );
            assert_eq!(
                dto.packs_per_player, procedure.packs_per_player,
                "packs_per_player ({kind:?})"
            );
            assert_eq!(
                dto.cards_per_pick, procedure.cards_per_pick,
                "cards_per_pick ({kind:?})"
            );
            assert_eq!(
                dto.pick_selection_mode, procedure.pick_selection_mode,
                "pick_selection_mode ({kind:?})"
            );
            assert_eq!(
                dto.distribution, procedure.distribution,
                "distribution ({kind:?})"
            );
            assert_eq!(
                dto.min_deck_size, procedure.min_deck_size,
                "min_deck_size ({kind:?})"
            );
            assert_eq!(
                dto.commanders_required, procedure.commanders_required,
                "commanders_required ({kind:?})"
            );
            assert_eq!(
                dto.post_draft_play, procedure.post_draft_play,
                "post_draft_play ({kind:?})"
            );
            assert_eq!(
                dto.launch_capability,
                procedure.launch_capability(),
                "launch_capability ({kind:?})"
            );
            assert_eq!(
                dto.match_config, procedure.match_config,
                "match_config ({kind:?})"
            );
        }
    }

    #[test]
    fn draft_procedure_dto_publishes_the_engine_single_elimination_seat_set() {
        let dto = draft_procedure_dto(
            draft_kind_wire_number(DraftKind::Premier),
            TournamentFormat::SingleElimination,
        )
        .expect("Premier has a wire number");

        assert_eq!(dto.allowed_pod_sizes, vec![8]);
    }

    // ── V-RS: the CR 903.3 designation's channel, seat by seat ─────────────
    //
    // Seam: `submit_deck_inner` / `submit_deck_for_seat_inner` -> `session::apply`
    // -> `apply_submit_deck` -> `validate_limited_deck`.
    //
    // Every row asserts on `session.submitted_decks[..].commanders`, NEVER on
    // the returned view: the stored value is what
    // `crates/server-core/src/draft_session.rs`'s phase-9 deferral promises
    // that phase it will find, and a view assertion would not see it. (The
    // marker itself is deliberately NOT reproduced here -- this phase's
    // completion gate is a census of those literals, and a cross-reference
    // that reproduced one would move a count it does not own.)

    /// The CR 903.13e granting twin of `commander_pool_input_json()`.
    ///
    /// Identical but for the set code, which is the only thing that makes the
    /// grant fire: `session_concessions` latches `SetLayout::UniformByRound`
    /// and matches each of them case-insensitively against the engine's
    /// `DRAFT_SET_CONCESSIONS` table, where "CMM" grants up to two copies of
    /// The Prismatic Piper. The grant is therefore LATCHED from what the draft
    /// contained rather than hand-set on the session, which is what makes the
    /// rows below channel tests instead of validator tests.
    ///
    /// `commander_pool_input_json()` is deliberately left alone --
    /// `create_multiplayer_draft_inner_accepts_commander_kind` and
    /// `create_multiplayer_draft_inner_refuses_an_unmapped_kind` consume it.
    fn granting_commander_pool_input_json() -> String {
        let set_pool_json = r#"{
            "code": "CMM",
            "name": "Test Set",
            "release_date": null,
            "pack_variants": [{
                "contents": [{ "slot": "common", "count": 4, "choices": [{ "sheet": "common", "weight": 1 }] }],
                "weight": 1
            }],
            "pack_variants_total_weight": 1,
            "sheets": {
                "common": {
                    "cards": [
                        { "name": "Alpha", "set_code": "CMM", "collector_number": "1", "rarity": "common", "weight": 1 },
                        { "name": "Beta", "set_code": "CMM", "collector_number": "2", "rarity": "common", "weight": 1 },
                        { "name": "Gamma", "set_code": "CMM", "collector_number": "3", "rarity": "common", "weight": 1 },
                        { "name": "Delta", "set_code": "CMM", "collector_number": "4", "rarity": "common", "weight": 1 }
                    ],
                    "total_weight": 4,
                    "foil": false,
                    "balance_colors": false
                }
            },
            "prints": [],
            "basic_lands": []
        }"#;
        serde_json::json!({
            "type": "Set",
            "data": { "set_pool_json": set_pool_json }
        })
        .to_string()
    }

    /// Put the installed session into Deckbuilding and seed ONE seat's pool.
    ///
    /// The wasm-seam mirror of draft-core's landed `deckbuilding_commander_draft`,
    /// PARAMETERIZED ON THE SEAT -- and the parameter is required, not a
    /// generalisation for its own sake. `apply_submit_deck` validates against
    /// the SUBMITTING seat's own pool (`session.pools[seat as usize]`), and
    /// `DraftSession::new` builds `pools: vec![vec![]; pod_size]`, so a
    /// seat-0-only helper leaves `pools[2]` present and EMPTY: a seat-2
    /// submission then dies at `validate_limited_deck` step 4 with a
    /// `NotInPool` per deck name, `apply_submit_deck` returns `Err` before its
    /// `insert`, and the routing the seat-routing row asserts is never reached.
    ///
    /// The names are per-seat distinct for the same reason: seat 2's deck must
    /// not be validatable against seat 0's pool, so a wrong-pool read is a hard
    /// `Err` rather than a silent pass.
    fn seat_into_deckbuilding(seat: u8, pool_size: usize) {
        DRAFT_SESSION.with(|cell| {
            let mut session = cell.take().expect("a draft session is installed");
            session.status = DraftStatus::Deckbuilding;
            session.pools[seat as usize] = (0..pool_size)
                .map(|i| DraftCardInstance {
                    instance_id: format!("seat-{seat}-card-{i}"),
                    name: format!("Seat {seat} Card {i}"),
                    set_code: "CMM".to_string(),
                    collector_number: format!("{i}"),
                    rarity: "common".to_string(),
                    colors: Vec::new(),
                    cmc: 0,
                    type_line: String::new(),
                    draft_effect: None,
                })
                .collect();
            cell.set(Some(session));
        });
    }

    /// Read the installed session without disturbing it -- the same take/put
    /// dance `with_draft_mut_inner` runs.
    fn with_installed_session<R>(f: impl FnOnce(&DraftSession) -> R) -> R {
        DRAFT_SESSION.with(|cell| {
            let session = cell.take().expect("a draft session is installed");
            let out = f(&session);
            cell.set(Some(session));
            out
        })
    }

    /// Start a granting 4-seat Commander pod and put `seat` into deckbuilding.
    fn granting_commander_pod(seat: u8, pool_size: usize) {
        create_multiplayer_draft_inner(
            &granting_commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            4, // CommanderDraft
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("commander draft should start");
        seat_into_deckbuilding(seat, pool_size);
    }

    /// Start a 4-seat PREMIER pod and put `seat` into deckbuilding.
    ///
    /// The non-Commander sibling of `granting_commander_pod`, for rows whose
    /// subject is a deck OUTSIDE the Commander variant. CR 903.3's designation
    /// floor is `0` for the four CR 905.1a kinds, so an empty designation is
    /// legal here and illegal in a Commander pod -- which is the whole reason
    /// this helper exists rather than the Commander one being edited (four
    /// other rows depend on that helper's kind).
    ///
    /// Premier reaches `StartDraft`: `min_pod_size` is 2 and Swiss admits
    /// `2..=8`, so a 4-seat pod passes both guards.
    fn premier_pod(seat: u8, pool_size: usize) {
        create_multiplayer_draft_inner(
            &granting_commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            1, // Premier
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("premier draft should start");
        seat_into_deckbuilding(seat, pool_size);
    }

    /// The `pool_size`-card deck a seat can legally submit from its own pool.
    fn seat_deck(seat: u8, size: usize) -> Vec<String> {
        (0..size).map(|i| format!("Seat {seat} Card {i}")).collect()
    }

    fn json(value: &[String]) -> String {
        serde_json::to_string(value).expect("a Vec<String> serializes")
    }

    /// V-RS (i) -- DELIVERY. The designation reaches the session, IN ORDER.
    ///
    /// Discriminates against a channel that drops the designation or delivers
    /// it reordered. Revert the threading of the parsed `commanders` into the
    /// `DraftAction::SubmitDeck` literal (back to `Vec::new()`) and the stored
    /// value is `[]` against an assertion naming an ordered, non-empty list.
    ///
    /// The designation is deliberately NOT in the deck's own order: a channel
    /// that sorted, deduped or re-derived it from the deck would red here.
    #[test]
    fn submit_deck_inner_carries_the_designation_to_the_session() {
        clear_state();
        granting_commander_pod(0, 60);

        let deck = seat_deck(0, 60);
        let commanders = vec!["Seat 0 Card 7".to_string(), "Seat 0 Card 3".to_string()];
        submit_deck_inner(&json(&deck), &json(&commanders)).expect("a legal deck submits");

        with_installed_session(|session| {
            // Paired positive reach-guard: the submission INSERTED. A refusal
            // cannot satisfy the assertion below vacuously.
            assert_eq!(
                session.submitted_decks.len(),
                1,
                "the submission must have reached `submitted_decks.insert`"
            );
            let submission = session
                .submitted_decks
                .get(&engine::types::player::PlayerId(0))
                .expect("seat 0's submission is keyed by its player id");
            assert_eq!(
                submission.commanders, commanders,
                "CR 903.3: the designation arrives verbatim and in order"
            );
        });

        clear_state();
    }

    /// V-RS (ii) -- ANTI-FABRICATION. An empty designation is STORED empty.
    ///
    /// Discriminates against a world in which something on the empty path
    /// synthesises a designation -- the core defaulting an empty parse,
    /// `session::apply` substituting, or `apply_submit_deck` inventing an entry
    /// rather than storing the empty one.
    ///
    /// This row is NOT revert-failing, and that is deliberate rather than an
    /// oversight: the value-level reversion (`commanders: Vec::new()` in the
    /// `SubmitDeck` literal) hands `apply_submit_deck` an empty `Vec` either
    /// way, so the assertion holds identically. What it buys is a pin on the
    /// empty path -- it is the only row that carries an empty designation
    /// through to a SUCCESSFUL insert and then reads what was stored. Do NOT
    /// "strengthen" it with a non-empty input: that converts it into (i) and
    /// deletes this seam's only empty-path row.
    #[test]
    fn submit_deck_inner_stores_an_empty_designation_without_synthesising_one() {
        clear_state();
        // A PREMIER pod, not a Commander one: CR 903.3's floor is `0` outside
        // the Commander variant, so an empty designation is legal here. The
        // row's own assertion message already reads "a deck outside the
        // Commander variant designates none" -- it was always about this case
        // and was merely borrowing the Commander pod helper. The INPUT stays
        // `"[]"`, exactly as the docstring above requires.
        premier_pod(0, 60);

        let deck = seat_deck(0, 60);
        submit_deck_inner(&json(&deck), "[]").expect("an undesignated deck submits");

        with_installed_session(|session| {
            assert_eq!(
                session.submitted_decks.len(),
                1,
                "the submission must have reached `submitted_decks.insert`"
            );
            let submission = session
                .submitted_decks
                .get(&engine::types::player::PlayerId(0))
                .expect("seat 0's submission is keyed by its player id");
            assert!(
                submission.commanders.is_empty(),
                "CR 903.1: a deck outside the Commander variant designates none, and \
                 the empty list must be stored rather than filled in: {:?}",
                submission.commanders
            );
        });

        clear_state();
    }

    /// V-RS (iii) -- LOUD REFUSAL. A designation the deck does not back is
    /// refused, and the ENGINE'S OWN `CommanderNotInDeck` text reaches the
    /// caller.
    ///
    /// Discriminates against a generic `format!` wrapper in place of
    /// `deck_submission_message`: `DraftError::ValidationFailed`'s own
    /// `#[error]` is the bare "deck validation failed", so a wrapper's string
    /// would carry NONE of the text asserted here.
    ///
    /// Revert-failing against two distinct lines -- revert the threading and no
    /// name is designated at all, so `*designated > in_deck` never fires and
    /// the call SUCCEEDS where this row demands `expect_err`; revert
    /// `.map_err(deck_submission_message)` to a generic wrapper and the text
    /// half fails. Its paired positive is the same call with the name IN the
    /// deck, immediately below.
    #[test]
    fn submit_deck_inner_carries_the_engines_refusal_text() {
        clear_state();
        granting_commander_pod(0, 60);

        let deck = seat_deck(0, 60);
        let absent = vec!["Seat 0 Card 99".to_string()];
        let err = submit_deck_inner(&json(&deck), &json(&absent))
            .expect_err("CR 702.124h: a designation must be backed by a copy in the deck");
        assert!(
            err.contains("is designated as commander"),
            "the engine's own CommanderNotInDeck text must survive the boundary: {err}"
        );
        assert!(
            !err.contains("deck validation failed"),
            "a generic DraftError wrapper would have replaced the details: {err}"
        );

        // Paired positive: the same call with a name the deck DOES back
        // succeeds, so this row cannot pass because everything is refused.
        let present = vec!["Seat 0 Card 4".to_string()];
        submit_deck_inner(&json(&deck), &json(&present)).expect("a backed designation submits");

        clear_state();
    }

    /// V-RS (iv) -- PARSE ORDER. Both decodes precede the session.
    ///
    /// The landed `submit_pick_for_seat_refuses_a_bare_id` shape, verbatim: run
    /// after `clear_state()` with NO session installed, so the two candidate
    /// orderings produce DIFFERENT strings. `main_deck_json` is well-formed, so
    /// only the `commanders` decode can fire.
    ///
    /// This row is insensitive to the threading reversion the other rows catch;
    /// what it protects is the parse ORDERING and the distinct
    /// "Failed to parse commanders" text. Seeding a session here would destroy
    /// its discrimination -- against a seeded session BOTH orderings produce
    /// the same string.
    #[test]
    fn submit_deck_inner_parses_the_designation_before_the_session() {
        clear_state();
        let err = submit_deck_inner("[]", "kenrith").expect_err("a bare word is not a JSON array");
        assert!(
            err.contains("Failed to parse commanders"),
            "the commanders decode is the one that fired: {err}"
        );
        // Reached the parse, not the session: no draft is initialized here, so
        // a decoder that ran AFTER `with_draft_mut_inner` would have reported
        // "Draft not initialized" instead.
        assert!(
            !err.contains("Draft not initialized"),
            "parse must precede the session: {err}"
        );

        // Paired positive reach-guard: the same core against a seeded session
        // DOES reach `submitted_decks.insert`, so a core that refused
        // everything cannot satisfy the negative above.
        granting_commander_pod(0, 60);
        let deck = seat_deck(0, 60);
        // CR 903.3: a Commander pod requires a designation, and this guard's
        // job is only to show the insert IS reached. `Seat 0 Card 0` is backed
        // by the deck and the pool, so nothing but the floor changes.
        submit_deck_inner(&json(&deck), &json(&["Seat 0 Card 0".to_string()]))
            .expect("a well-formed payload applies");
        with_installed_session(|session| {
            assert_eq!(session.submitted_decks.len(), 1);
        });

        clear_state();
    }

    /// V-RS (v-a) -- CR 903.13e, the commanders-DEPENDENT arm.
    ///
    /// "each player may add up to two cards named The Prismatic Piper to their
    /// card pool, but only if those cards are used as the player's
    /// commander(s)". The two halves differ in EXACTLY ONE input: the same deck
    /// submits cleanly when the added copies are designated and is refused by
    /// `FillerNotUsedAsCommander` when they are not.
    ///
    /// Revert-failing: under the threading reversion the accepted half receives
    /// `designated = 0`, `added > designated` fires, and the submission this
    /// row asserts is clean is refused.
    #[test]
    fn submit_deck_inner_feeds_the_filler_designation_arm() {
        clear_state();
        granting_commander_pod(0, 60);

        let filler = "The Prismatic Piper".to_string();
        let mut deck = seat_deck(0, 58);
        deck.push(filler.clone());
        deck.push(filler.clone());
        let designated = vec![filler.clone(), filler.clone()];

        // Accepted: two added copies, both designated.
        submit_deck_inner(&json(&deck), &json(&designated))
            .expect("CR 903.13e: added filler copies designated as commanders are legal");
        with_installed_session(|session| {
            let submission = session
                .submitted_decks
                .get(&engine::types::player::PlayerId(0))
                .expect("the accepted submission inserted");
            assert_eq!(submission.commanders, designated);
        });

        // Refused: the SAME deck with no designation.
        let err = submit_deck_inner(&json(&deck), "[]")
            .expect_err("CR 903.13e: undesignated added filler is not legal");
        assert!(
            err.contains("designated as commander(s)"),
            "expected the engine's FillerNotUsedAsCommander text: {err}"
        );

        clear_state();
    }

    /// V-RS (v-b) -- CR 903.13e, the commanders-INDEPENDENT arm.
    ///
    /// Three added copies exceed the grant of two, and `FillerExceedsGrant`
    /// is the error that fires, with the engine's own text through
    /// `deck_submission_message`.
    ///
    /// This row is NOT revert-failing and is not padding: `added >
    /// filler.max_copies` reads `commanders` not at all, so no line this phase
    /// writes sits under it. It is here because phase 8 routed the filler's cap
    /// affordance forward in prose, and the refusal must be shown REACHABLE
    /// through this channel and SPECIFIC to this error rather than merely
    /// asserted. Its paired positive is (v-a)'s accepted two-copy half.
    #[test]
    fn submit_deck_inner_reaches_the_filler_cap() {
        clear_state();
        granting_commander_pod(0, 60);

        let filler = "The Prismatic Piper".to_string();
        let mut deck = seat_deck(0, 57);
        deck.push(filler.clone());
        deck.push(filler.clone());
        deck.push(filler.clone());
        // Two designations, not three: CR 702.124g caps the designation at two,
        // and `apply_submit_deck` would return TooManyCommanders BEFORE the
        // validator on a third -- which would test the wrong arm.
        let designated = vec![filler.clone(), filler.clone()];

        let err = submit_deck_inner(&json(&deck), &json(&designated))
            .expect_err("CR 903.13e: at most two copies may be added");
        assert!(
            err.contains("but at most 2 may be added"),
            "expected the engine's FillerExceedsGrant text: {err}"
        );

        clear_state();
    }

    /// V-RS (vi) -- SEAT ROUTING, with a prior producer.
    ///
    /// There is no separate routing path to test: both submissions enter the
    /// SAME function, so nothing but the `seat` argument can carry the routing.
    /// The fixture seeds TWO seats and submits from both, because attribution
    /// needs a first producer -- a one-seat fixture would catch "the seat
    /// parameter is ignored" only by refusal, and would catch neither a
    /// submission attributed to the wrong player nor a `submitted_decks`
    /// replaced rather than inserted into.
    ///
    /// Per-seat-distinct pool names make `pools[seat]`'s index load-bearing,
    /// and per-seat-distinct DESIGNATIONS make a wrong-keyed submission visible
    /// in the value and not only in the count.
    ///
    /// Revert-failing: under the threading reversion BOTH entries store `[]`
    /// and both `.commanders` assertions fail, independently of anything this
    /// row claims about seats.
    #[test]
    fn submit_deck_for_seat_inner_routes_each_seat_to_its_own_submission() {
        clear_state();
        granting_commander_pod(0, 60);
        seat_into_deckbuilding(2, 60);

        let seat0_deck = seat_deck(0, 60);
        let seat0_commanders = vec!["Seat 0 Card 0".to_string()];
        submit_deck_for_seat_inner(0, &json(&seat0_deck), &json(&seat0_commanders))
            .expect("seat 0 submits from its own pool");

        let seat2_deck = seat_deck(2, 60);
        let seat2_commanders = vec!["Seat 2 Card 1".to_string()];
        submit_deck_for_seat_inner(2, &json(&seat2_deck), &json(&seat2_commanders))
            .expect("seat 2 submits from its own pool");

        with_installed_session(|session| {
            // Paired positive reach-guard: BOTH submissions inserted. A refused
            // seat-2 call cannot satisfy the assertions below.
            assert_eq!(
                session.submitted_decks.len(),
                2,
                "`submitted_decks` is inserted into, never replaced"
            );

            let seat2 = session
                .submitted_decks
                .get(&engine::types::player::PlayerId(2))
                .expect("seat 2's submission is keyed by its own player id");
            assert_eq!(seat2.seat, 2);
            assert_eq!(seat2.commanders, seat2_commanders);

            let seat0 = session
                .submitted_decks
                .get(&engine::types::player::PlayerId(0))
                .expect("seat 0's submission survives seat 2's");
            assert_eq!(seat0.seat, 0);
            assert_eq!(
                seat0.commanders, seat0_commanders,
                "a later seat's designation must not be attributed to an earlier one"
            );
        });

        clear_state();
    }

    // ── U21: the PRODUCTION seam for the CR 903.3 designation ──────────────
    //
    // These three rows sit together and read as one argument about
    // `get_bot_deck_inner`: VM-4c is the argument (`Ok` with a designation),
    // VM-4e is the PRECONDITION refusal (before `suggest_deck`), VM-4h is the
    // POSTCONDITION refusal (after it). They must not be folded into each
    // other: VM-4c needs a database and VM-4e needs none, and VM-4e's fixture
    // would never reach the postcondition VM-4h asserts.

    /// A card database whose `commander_pool_input_json` cards are commander-
    /// judgeable: `Alpha` is a Legendary Creature, all four are mono-white, and
    /// the basics are present so a containment check never measures a missing
    /// row (PROBE C'').
    ///
    /// The module's `fixture_card_db_json` makes all four NON-legendary, so a
    /// designation test needs its own variant rather than reusing it.
    fn commander_fixture_db_json() -> String {
        let card = |name: &str, supertypes: &str, core: &str, identity: &str| {
            format!(
                r#""{}": {{ "name": "{name}", "mana_cost": {{ "type": "NoCost" }},
                "card_type": {{ "supertypes": [{supertypes}], "core_types": ["{core}"], "subtypes": [] }},
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "color_identity": [{identity}],
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "legalities": {{ "commander": "legal" }} }}"#,
                name.to_lowercase()
            )
        };
        format!(
            "{{ {} }}",
            [
                card("Alpha", "\"Legendary\"", "Creature", "\"White\""),
                card("Beta", "", "Creature", "\"White\""),
                card("Gamma", "", "Creature", "\"White\""),
                card("Delta", "", "Creature", "\"White\""),
                card("Blue Addable", "", "Creature", "\"Blue\""),
                card("White Addable", "", "Creature", "\"White\""),
                card("Plains", "\"Basic\"", "Land", "\"White\""),
                card("Island", "\"Basic\"", "Land", "\"Blue\""),
            ]
            .join(", ")
        )
    }

    fn install_commander_fixture_db() {
        let db = CardDatabase::from_json_str(&commander_fixture_db_json()).unwrap();
        CARD_DB.with(|cell| *cell.borrow_mut() = Some(db));
    }

    /// `DraftSession.pools` is `pub`, and seeding a bot seat's pool directly is
    /// the same shape `draft-core`'s own session tests use.
    fn seed_bot_pool(seat: usize, pool: Vec<DraftCardInstance>) {
        DRAFT_SESSION.with(|cell| {
            let mut session = cell.take().expect("a session must be installed");
            session.pools[seat] = pool;
            cell.set(Some(session));
        });
    }

    /// `DraftConfig.addable_cards` is `pub`. In PRODUCTION this field is written
    /// only by `create_multiplayer_draft_inner`'s Cube arm, from the host's cube
    /// settings; the Set arm hardcodes `DeckAddableCards::standard_basics()` and
    /// cannot reach a `CustomOnly` policy at all. The fixture below sets it
    /// directly on a Set session, so it must not be read as implying the Set
    /// path can produce this configuration.
    fn set_addable_cards(addable: DeckAddableCards) {
        DRAFT_SESSION.with(|cell| {
            let mut session = cell.take().expect("a session must be installed");
            session.config.addable_cards = addable;
            cell.set(Some(session));
        });
    }

    fn mono_white_bot_pool() -> Vec<DraftCardInstance> {
        ["Alpha", "Beta", "Gamma", "Delta"]
            .into_iter()
            .map(|name| DraftCardInstance {
                instance_id: format!("id-{name}"),
                name: name.to_string(),
                set_code: "TST".to_string(),
                collector_number: "1".to_string(),
                rarity: "common".to_string(),
                colors: vec!["W".to_string()],
                cmc: 2,
                type_line: if name == "Alpha" {
                    "Legendary Creature — Human".to_string()
                } else {
                    "Creature — Human".to_string()
                },
                draft_effect: None,
            })
            .collect()
    }

    fn start_commander_pod(kind_wire: u8) {
        create_multiplayer_draft_inner(
            &commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            kind_wire,
            42,
            "test-room",
            "Swiss",
            "Competitive",
        )
        .expect("the pod should start");
        seed_bot_pool(1, mono_white_bot_pool());
    }

    /// VM-4c — the PRODUCTION argument at `get_bot_deck_inner`'s `suggest_deck`
    /// call: `session.config.kind.commanders_required()`, not a literal.
    ///
    /// This row is BLIND to database ABSENCE, and that is disclosed rather than
    /// worked around: it installs its own fixture database, so it constructs the
    /// input whose absence is the production failure mode. VM-4e is its
    /// complement.
    #[test]
    fn get_bot_deck_inner_designates_a_commander_for_a_commander_pod() {
        clear_state();
        install_commander_fixture_db();
        start_commander_pod(4);

        let deck = get_bot_deck_inner(1).expect("a Commander bot deck should build");
        // Reach guard first: an empty deck cannot satisfy the claim below.
        assert!(!deck.main_deck.is_empty(), "deck = {:?}", deck.main_deck);
        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);

        // Paired control: the same pool and pod under `Premier` designates
        // nothing, so the argument is provably read from the kind.
        clear_state();
        install_commander_fixture_db();
        start_commander_pod(1);

        let deck = get_bot_deck_inner(1).expect("a Premier bot deck should build");
        assert!(!deck.main_deck.is_empty(), "deck = {:?}", deck.main_deck);
        assert!(
            deck.commander.is_empty(),
            "commander = {:?}",
            deck.commander
        );

        clear_state();
    }

    /// VM-4e — [B1] the PRECONDITION refusal: a Commander pod whose host never
    /// loaded `CARD_DB`.
    ///
    /// `install_commander_fixture_db()` is deliberately NOT called. Do not add
    /// it back as an oversight — the whole subject of this row is the database's
    /// absence, and installing one silences it. `commander_pool_input_json()` is
    /// a SET pool, so session creation itself needs no database, which is what
    /// makes the fixture constructible.
    #[test]
    fn get_bot_deck_inner_refuses_a_commander_pod_with_no_card_database() {
        clear_state();
        start_commander_pod(4);

        let err = get_bot_deck_inner(1).expect_err("CR 903.3 cannot be judged with no database");
        assert!(
            err.contains("Card database"),
            "the message must name the card database: {err}"
        );

        // Paired control ON THE SAME no-database state: `Premier` still builds a
        // deck. This is the reach guard — it proves the fixture reaches
        // `suggest_deck` at all — and it isolates the `commanders_required() > 0`
        // conjunct, so the four CR 905.1a kinds provably keep today's behaviour.
        clear_state();
        start_commander_pod(1);

        let deck = get_bot_deck_inner(1).expect("Premier needs no designation");
        assert!(!deck.main_deck.is_empty(), "deck = {:?}", deck.main_deck);

        clear_state();
    }

    /// VM-4h — [M2] the POSTCONDITION refusal: a Commander bot deck that did not
    /// reach `min_deck_size` is not shipped.
    ///
    /// CR 903.13f(1). The cause lives in `suggest.rs`
    /// (`custom_only_with_no_in_identity_entry_yields_a_short_deck`, which pins
    /// that the CR 903.5c filter reached the shortfall deliberately); this row
    /// pins the DISPOSITION at the seam, and each would still pass if the
    /// other's subject regressed.
    #[test]
    fn get_bot_deck_inner_refuses_a_commander_bot_deck_under_the_floor() {
        // An off-identity custom list: the CR 903.5c filter admits nothing, so
        // `lands` is empty and the deck is short of the session's floor.
        clear_state();
        install_commander_fixture_db();
        start_commander_pod(4);
        set_addable_cards(DeckAddableCards {
            policy: DeckAddableCardPolicy::CustomOnly,
            custom: vec!["Blue Addable".to_string()],
        });

        let err = get_bot_deck_inner(1).expect_err("a deck under the floor must be refused");
        assert!(
            err.contains("minimum is 60") && err.contains("reached 4 cards"),
            "the message must name the reached count and the minimum: {err}"
        );

        // Control (i) — the reach guard, one field varied. An IN-identity custom
        // name builds a full deck, so the `Err` above is the postcondition
        // refusing rather than a broken fixture. This is also the row that reds
        // if the comparison is inverted or the `lands` term is dropped from the
        // sum.
        clear_state();
        install_commander_fixture_db();
        start_commander_pod(4);
        set_addable_cards(DeckAddableCards {
            policy: DeckAddableCardPolicy::CustomOnly,
            custom: vec!["White Addable".to_string()],
        });

        let deck = get_bot_deck_inner(1).expect("an in-identity custom card fills the deck");
        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);
        let land_total: usize = deck.lands.values().map(|&n| n as usize).sum();
        assert_eq!(
            deck.main_deck.len() + land_total,
            60,
            "lands = {:?}",
            deck.lands
        );

        // Control (ii) — isolates the `commanders_required() > 0` conjunct: the
        // identical off-identity `CustomOnly` list under `Premier` returns `Ok`.
        clear_state();
        install_commander_fixture_db();
        start_commander_pod(1);
        set_addable_cards(DeckAddableCards {
            policy: DeckAddableCardPolicy::CustomOnly,
            custom: vec!["Blue Addable".to_string()],
        });

        let deck = get_bot_deck_inner(1).expect("the four CR 905.1a kinds are unaffected");
        assert!(!deck.main_deck.is_empty(), "deck = {:?}", deck.main_deck);

        clear_state();
    }
}
