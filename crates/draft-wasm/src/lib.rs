use std::cell::{Cell, RefCell};

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use draft_core::cube::{
    cube_cards_from_entries, parse_cube_list, resolve_addable_cards, CubePackSource,
};
use draft_core::pack_generator::PackGenerator;
use draft_core::pack_source::PackSource;
use draft_core::session;
use draft_core::set_pool::LimitedSetPool;
use draft_core::types::*;
use draft_core::view::{filter_for_player, DraftPlayerView};
use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;
use session_cell::LocalOwner;

mod bot_ai;
mod session_cell;
mod suggest;

thread_local! {
    static PACK_GEN: Cell<Option<PackGenerator>> = const { Cell::new(None) };
    static DIFFICULTY: Cell<AiDifficulty> = const { Cell::new(AiDifficulty::Medium) };
    /// Whether bots (and auto-pick) apply the built-in draft guides. Opt-in and
    /// set by the caller via [`set_draft_pick_enhancements`]; defaults off.
    static PICK_ENHANCEMENT: Cell<bot_ai::PickEnhancement> =
        const { Cell::new(bot_ai::PickEnhancement::Off) };
    static RNG: Cell<Option<ChaCha20Rng>> = const { Cell::new(None) };
    /// Whether the local runtime's bot seats are LLM drafters. Every local
    /// install sets it; it is never part of the session.
    static LOCAL_LLM_DRAFTING: Cell<LocalLlmDrafting> =
        const { Cell::new(LocalLlmDrafting::EngineBots) };
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

/// The installed session's flow is checked before any closure runs, so the dormant
/// runtime never serializes, mutates or consumes RNG for a FIFO session.
fn admit_legacy_runtime(session: &DraftSession) -> Result<(), String> {
    session
        .validate_legacy_admission()
        .map_err(|_| FIFO_UNSUPPORTED_BY_RUNTIME.to_string())
}

fn with_draft_inner<R>(f: impl FnOnce(&DraftSession) -> Result<R, String>) -> Result<R, String> {
    session_cell::with_draft_inner(|session| {
        admit_legacy_runtime(session).and_then(|()| f(session))
    })
}

fn with_draft_mut_inner<R>(
    f: impl FnOnce(&mut DraftSession) -> Result<R, String>,
) -> Result<R, String> {
    session_cell::with_draft_mut_inner(|session| {
        admit_legacy_runtime(session).and_then(|()| f(session))
    })
}

fn with_draft<R>(f: impl FnOnce(&DraftSession) -> R) -> Result<R, JsValue> {
    session_cell::with_draft(|session| admit_legacy_runtime(session).map(|()| f(session)))?
        .map_err(|error| JsValue::from_str(&error))
}

fn with_draft_mut<R>(
    f: impl FnOnce(&mut DraftSession) -> Result<R, JsValue>,
) -> Result<R, JsValue> {
    session_cell::with_draft_mut(|session| {
        admit_legacy_runtime(session).map_err(|error| JsValue::from_str(&error))?;
        f(session)
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

/// Opt bots and auto-pick in or out of the built-in draft-guide pick
/// enhancements. Independent of difficulty; the frontend sets it from the
/// player's setting before starting or resuming a draft.
#[wasm_bindgen]
pub fn set_draft_pick_enhancements(enabled: bool) {
    let enhancement = if enabled {
        bot_ai::PickEnhancement::DraftGuides
    } else {
        bot_ai::PickEnhancement::Off
    };
    PICK_ENHANCEMENT.with(|cell| cell.set(enhancement));
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

fn enforce_procedure_minimum_deck_size(mut config: DraftConfig) -> DraftConfig {
    config.min_deck_size = config
        .kind
        .procedure()
        .effective_cube_min_deck_size(config.min_deck_size);
    config
}

/// The local FIFO Quick configuration of a cube draft: the cube's settings
/// under the untimed Quick clock and the default selection policy, with the
/// procedure's minimum-deck floor applied.
fn build_quick_cube_config(
    cube_name: &str,
    settings: &CubeDraftSettings,
    addable_cards: DeckAddableCards,
    seed: u64,
) -> DraftConfig {
    enforce_procedure_minimum_deck_size(DraftConfig {
        flow: DraftFlow::FifoAsync,
        selection_policy: draft_core::types::DraftSelectionPolicy::default(),
        clock_policy: DraftClockPolicy::quick_default(),
        source: DraftSource::Cube {
            id: "custom-cube".to_string(),
            name: cube_name.to_string(),
        },
        set_code: "custom-cube".to_string(),
        kind: DraftKind::Quick,
        pod_size: settings.pod_size,
        cards_per_pack: settings.cards_per_pack,
        pack_count: settings.pack_count,
        min_deck_size: settings.min_deck_size,
        addable_cards,
        rng_seed: seed,
        tournament_format: TournamentFormat::Swiss,
        pod_policy: PodPolicy::Competitive,
        spectator_visibility: SpectatorVisibility::default(),
    })
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

/// Start a Quick Draft on the local FIFO runtime: one human and seven bots.
///
/// - `selection_json`: serialized [`SetPackSequence`] — the distinct set pools
///   from draft-pools.json plus the set filling each booster, in pack order.
///   The sequence length is the draft's pack count, and a set may repeat.
/// - `difficulty`: 0=VeryEasy, 1=Easy, 2=Medium, 3=Hard, 4=VeryHard
/// - `seed`: RNG seed for deterministic pack generation
/// - `drafting_json`: `"EngineBots"` or `"LlmSeats"`, parsed before anything
///   else, so a refused call installs nothing.
/// - `now_ms`: the caller's time; the draft starts at it.
///
/// Returns `{ owner, view }`: the owner the caller presents on every later
/// local call, and seat 0's view.
#[wasm_bindgen]
pub fn start_quick_draft(
    selection_json: &str,
    difficulty: u8,
    seed: u32,
    drafting_json: &str,
    now_ms: u64,
) -> Result<JsValue, JsValue> {
    start_quick_draft_inner(selection_json, difficulty, seed, drafting_json, now_ms)
        .map(|local| to_js(&local))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`start_quick_draft`].
fn start_quick_draft_inner(
    selection_json: &str,
    difficulty: u8,
    seed: u32,
    drafting_json: &str,
    now_ms: u64,
) -> Result<LocalFifoSession, String> {
    let drafting = parse_local_llm_drafting(drafting_json)?;
    // The procedure table is the single authority for how many boosters this
    // kind opens; the selection only decides which set fills each of them.
    let quick_procedure = DraftKind::Quick.procedure();
    let selection =
        ResolvedSetSelection::parse(selection_json, Some(quick_procedure.packs_per_player))?;

    let config = enforce_procedure_minimum_deck_size(DraftConfig {
        flow: DraftFlow::FifoAsync,
        selection_policy: draft_core::types::DraftSelectionPolicy::default(),
        clock_policy: DraftClockPolicy::quick_default(),
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
    });

    start_local_fifo_draft(
        config,
        local_quick_seats(quick_procedure.pod_size),
        "quick-draft".to_string(),
        &selection.generator,
        map_difficulty(difficulty),
        drafting,
        now_ms,
    )
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
        flow: draft_core::types::DraftFlow::LegacyBarrier,
        selection_policy: draft_core::types::DraftSelectionPolicy::default(),
        clock_policy: draft_core::types::DraftClockPolicy::default(),
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

    session_cell::install(draft_session);
    PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
    DIFFICULTY.with(|cell| cell.set(map_difficulty(difficulty)));
    RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(seed as u64))));

    Ok(to_js(&view))
}

/// Start a Quick Cube Draft on the local FIFO runtime from a counted cube
/// list: one human and a bot at every other seat of the settings' pod.
///
/// - `drafting_json`: `"EngineBots"` or `"LlmSeats"`, parsed before anything
///   else, so a refused call installs nothing.
/// - `now_ms`: the caller's time; the draft starts at it.
///
/// Returns `{ owner, view }`: the owner the caller presents on every later
/// local call, and seat 0's view.
#[wasm_bindgen]
pub fn start_quick_cube_draft(
    cube_list_text: &str,
    cube_name: &str,
    settings_json: &str,
    difficulty: u8,
    seed: u32,
    drafting_json: &str,
    now_ms: u64,
) -> Result<JsValue, JsValue> {
    start_quick_cube_draft_inner(
        cube_list_text,
        cube_name,
        settings_json,
        difficulty,
        seed,
        drafting_json,
        now_ms,
    )
    .map(|local| to_js(&local))
    .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`start_quick_cube_draft`].
fn start_quick_cube_draft_inner(
    cube_list_text: &str,
    cube_name: &str,
    settings_json: &str,
    difficulty: u8,
    seed: u32,
    drafting_json: &str,
    now_ms: u64,
) -> Result<LocalFifoSession, String> {
    let drafting = parse_local_llm_drafting(drafting_json)?;
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
            .map_err(|e| format!("Failed to parse cube settings: {e}"))?
    };
    if settings.pod_size == 0 {
        return Err("Cube draft pod size must be at least 1".to_string());
    }

    let entries = parse_cube_list(cube_list_text).map_err(|errors| {
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
                serde_json::to_string(&errors).unwrap_or_else(|_| "unknown cards".to_string())
            )
        })?;
        let addable_cards =
            resolve_addable_cards(&settings.addable_cards, db).map_err(|errors| {
                format!(
                    "Failed to resolve addable cards: {}",
                    serde_json::to_string(&errors).unwrap_or_else(|_| "unknown cards".to_string())
                )
            })?;
        Ok::<_, String>((cards, addable_cards))
    })?;

    let pod_size = settings.pod_size;
    let config = build_quick_cube_config(cube_name, &settings, addable_cards, seed as u64);

    start_local_fifo_draft(
        config,
        local_quick_seats(pod_size),
        "quick-cube-draft".to_string(),
        &CubePackSource::new(cards),
        map_difficulty(difficulty),
        drafting,
        now_ms,
    )
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
) -> Result<(), String> {
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
) -> Result<(), String> {
    apply_human_pick_and_resolve_bots_with_overrides(
        draft_session,
        human_action,
        &std::collections::BTreeMap::new(),
    )
}

/// Resolve the bot seats, letting `overrides` supply the pick for any seat an
/// LLM drafter already chose for.
///
/// An override is a list of `instance_id`s, never indices: the ids were resolved
/// against the same pack this loop reads, and `session::apply` re-validates them
/// against the live pack regardless. A seat with no override — or whose override
/// the reducer refuses — falls through to the heuristic bot in the same pass, so
/// a failed LLM call costs a seat its flavour, never its pick.
fn apply_human_pick_and_resolve_bots_with_overrides(
    draft_session: &mut DraftSession,
    human_action: DraftAction,
    overrides: &std::collections::BTreeMap<u8, Vec<String>>,
) -> Result<(), String> {
    if !matches!(draft_session.config.kind, DraftKind::Quick) {
        return Err("apply_human_pick_and_resolve_bots is only valid for Quick Draft".to_string());
    }

    session::apply(draft_session, human_action, None)
        .map_err(|e| format!("Human pick failed: {e}"))?;

    let difficulty = DIFFICULTY.with(|cell| cell.get());
    let mut rng = RNG
        .with(|cell| cell.take())
        .ok_or_else(|| "RNG not initialized".to_string())?;

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

            // CR 905.1a: a bot owes its kind's one-card Quick Draft pick step. This loop is
            // `Quick`-gated above, so `cards_per_pick` is 1 here today; reading
            // it from the procedure is what keeps that true by construction
            // rather than by coincidence.
            let cards_per_pick =
                usize::from(draft_session.config.kind.procedure().cards_per_pick).min(pack.0.len());
            let pack_shape = bot_ai::PackShape::current(draft_session);
            let chosen = overrides
                .get(&seat)
                .filter(|ids| {
                    // Only honour an override that names the exact step and
                    // cards this pack still holds; anything else is stale.
                    ids.len() == cards_per_pick
                        && ids
                            .iter()
                            .all(|id| pack.0.iter().any(|card| &card.instance_id == id))
                })
                .cloned();
            let card_instance_ids = chosen.unwrap_or_else(|| {
                let pick_indices = bot_ai::bot_picks(
                    &pack.0,
                    cards_per_pick,
                    difficulty,
                    &draft_session.pools[seat as usize],
                    card_db,
                    PICK_ENHANCEMENT.with(|cell| cell.get()),
                    pack_shape,
                    &mut rng,
                );
                // Map indices to ids BEFORE applying — the apply mutates the pack
                // the indices refer to.
                pick_indices
                    .into_iter()
                    .map(|index| pack.0[index].instance_id.clone())
                    .collect()
            });

            session::apply(
                draft_session,
                DraftAction::Pick {
                    seat,
                    card_instance_ids,
                },
                None,
            )
            .map_err(|e| format!("Bot {seat} pick failed: {e}"))?;
        }

        Ok::<(), String>(())
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
        apply_human_pick_and_resolve_bots(draft_session, card_id)
            .map_err(|e| JsValue::from_str(&e))?;
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
        )
        .map_err(|e| JsValue::from_str(&e))?;
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
        let pack_shape = bot_ai::PackShape::current(draft_session);
        let card_id = CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let pick_idx = bot_ai::bot_pick(
                &pack.0,
                difficulty,
                &draft_session.pools[0],
                db_borrow.as_ref(),
                PICK_ENHANCEMENT.with(|cell| cell.get()),
                pack_shape,
                &mut rng,
            );
            pack.0[pick_idx].instance_id.clone()
        });
        RNG.with(|cell| cell.set(Some(rng)));

        apply_human_pick_and_resolve_bots(draft_session, card_id)
            .map_err(|e| JsValue::from_str(&e))?;
        Ok(to_js(&filter_for_player(draft_session, 0)))
    })
}

// ── LLM-driven draft seats ───────────────────────────────────────────────────
//
// Strictly opt-in, exactly as in `engine-wasm`: with no configured endpoint the
// heuristic bot in `bot_ai` drives every seat, unchanged.
//
// The two calls bracket the network round trip. `buildLlmDraftPickRequests`
// renders each LLM seat's own `DraftPlayerView` — the same per-seat projection
// the heuristic bot is handed, which is what keeps an LLM drafter blind to other
// seats' pools — and stamps the pack with a fingerprint.
// `submitPickWithLlmBotPicks` applies the human's pick and then hands each
// resolved pick to the bot loop as an override, falling back per seat.

/// The per-seat inputs a caller supplies to resolve LLM picks: which seat, the
/// pack fingerprint the prompt was built over, which provider answered, and the
/// raw response body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlmDraftResponse {
    seat: u8,
    fingerprint: String,
    provider: String,
    /// HTTP status of the provider response. Carried so the engine can refuse a
    /// non-2xx reply whatever its body looks like.
    status: u16,
    body: String,
}

/// What happened to one seat's LLM pick, so the UI can report a misconfigured
/// endpoint instead of silently drafting like a bot forever.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LlmDraftOutcome {
    seat: u8,
    used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// What the provider reported spending on this reply, used or not.
    /// Diagnostic only.
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<phase_llm::TokenUsage>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LlmDraftPickRequest {
    seat: u8,
    fingerprint: String,
    option_count: usize,
    required_pick_count: usize,
    /// Size of the prompt this request carries, for measuring prompt growth.
    prompt_chars: usize,
    request: phase_llm::HttpRequestSpec,
}

/// The seats an LLM drafter may act for: the BOT seats of this pod, and nothing
/// else.
///
/// Derived from the session's own roster rather than filtering a caller's list,
/// so there is exactly one answer to "which seats may an LLM draft for" and no
/// way to ask for a different one. `DraftSeat::Human` is legal at ANY index — an
/// 8-player Premier or Traditional pod seats humans at 1..7 — so a seat number
/// carries no permission on its own. Rendering a seat's `filter_for_player`
/// view builds a prompt out of that seat's private pool and unpassed pack and
/// ships it to a third-party provider; for a human seat that would disclose
/// another player's hidden information to an outside service.
fn llm_eligible_bot_seats(draft_session: &DraftSession) -> Vec<u8> {
    draft_session
        .seats
        .iter()
        .enumerate()
        .filter(|(_, seat)| matches!(seat, DraftSeat::Bot { .. }))
        .filter_map(|(index, _)| u8::try_from(index).ok())
        .collect()
}

/// Build one LLM pick request per eligible bot seat.
///
/// Takes NO seat list. Which seats an LLM may draft for is an authority
/// question this crate already owns ([`llm_eligible_bot_seats`]), and accepting
/// a caller's list made the display layer a second classifier of the same
/// thing -- one free to drift toward naming a human seat, whose private pool
/// and unpassed pack would then be rendered into a third-party prompt.
///
/// `set_names_json` is an optional set-code -> name map so the format brief
/// reads "Triple Mirrodin" rather than "Triple MRD"; codes are used verbatim
/// when it is absent.
#[wasm_bindgen(js_name = buildLlmDraftPickRequests)]
pub fn build_llm_draft_pick_requests(
    endpoint_json: &str,
    set_names_json: &str,
) -> Result<JsValue, JsValue> {
    let requests = build_llm_draft_pick_requests_inner(endpoint_json, set_names_json)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&requests))
}

fn build_llm_draft_pick_requests_inner(
    endpoint_json: &str,
    set_names_json: &str,
) -> Result<Vec<LlmDraftPickRequest>, String> {
    let endpoint: phase_llm::LlmEndpointConfig = serde_json::from_str(endpoint_json)
        .map_err(|e| format!("Invalid LLM endpoint config: {e}"))?;
    // A missing or unparsable name map degrades the brief to set codes; it is
    // never a reason to refuse a pick.
    let set_names: std::collections::BTreeMap<String, String> =
        serde_json::from_str(set_names_json).unwrap_or_default();
    let set_names = phase_llm::draft_decision::set_names_from_pairs(set_names);
    let difficulty = DIFFICULTY.with(|cell| cell.get());

    with_draft_inner(|draft_session| {
        let requests = CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();
            llm_eligible_bot_seats(draft_session)
                .into_iter()
                .filter_map(|seat| {
                    let view = filter_for_player(draft_session, seat);
                    // A seat with no prompt asks nothing: notably a forced pick
                    // (the last card of a pack), which the bot path takes and
                    // records without a provider round trip.
                    let request = phase_llm::build_draft_pick_prompt(
                        seat, &view, difficulty, card_db, &set_names,
                    )
                    .ok()?;
                    let http = phase_llm::build_chat_request(&endpoint, &request.prompt).ok()?;
                    Some(LlmDraftPickRequest {
                        seat,
                        fingerprint: request.fingerprint,
                        option_count: request.option_count,
                        required_pick_count: request.required_pick_count,
                        prompt_chars: request.prompt.char_count(),
                        request: http,
                    })
                })
                .collect()
        });
        Ok(requests)
    })
}

/// Submit the human's pick, resolving any LLM seat's pick from its response.
///
/// Returns `{ view, llmOutcomes }`: the same `DraftPlayerView` `submit_pick`
/// returns, plus a per-seat record of whether the LLM pick was used.
#[wasm_bindgen(js_name = submitPickWithLlmBotPicks)]
pub fn submit_pick_with_llm_bot_picks(
    card_instance_id: &str,
    responses_json: &str,
) -> Result<JsValue, JsValue> {
    let (view, outcomes) = submit_pick_with_llm_bot_picks_inner(card_instance_id, responses_json)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(
        &serde_json::json!({ "view": view, "llmOutcomes": outcomes }),
    ))
}

fn submit_pick_with_llm_bot_picks_inner(
    card_instance_id: &str,
    responses_json: &str,
) -> Result<(DraftPlayerView, Vec<LlmDraftOutcome>), String> {
    let responses: Vec<LlmDraftResponse> = serde_json::from_str(responses_json)
        .map_err(|e| format!("Invalid LLM draft responses: {e}"))?;
    let card_id = card_instance_id.to_string();

    with_draft_mut_inner(|draft_session| {
        let mut overrides: std::collections::BTreeMap<u8, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut outcomes: Vec<LlmDraftOutcome> = Vec::with_capacity(responses.len());

        for response in &responses {
            let usage = phase_llm::token_usage(
                phase_llm::LlmProvider::from_label(&response.provider),
                &response.body,
            );
            match resolve_llm_draft_pick(draft_session, response) {
                Ok(selection) => {
                    overrides.insert(response.seat, selection.card_instance_ids);
                    outcomes.push(LlmDraftOutcome {
                        seat: response.seat,
                        used: true,
                        reasoning: selection.reasoning,
                        error: None,
                        usage,
                    });
                }
                Err(error) => outcomes.push(LlmDraftOutcome {
                    seat: response.seat,
                    used: false,
                    reasoning: None,
                    error: Some(error.to_string()),
                    usage,
                }),
            }
        }

        apply_human_pick_and_resolve_bots_with_overrides(
            draft_session,
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec![card_id],
            },
            &overrides,
        )?;

        Ok((filter_for_player(draft_session, 0), outcomes))
    })
}

/// Decode one seat's LLM response against the pack it is actually holding.
fn resolve_llm_draft_pick(
    draft_session: &DraftSession,
    response: &LlmDraftResponse,
) -> Result<phase_llm::LlmPickSelection, phase_llm::LlmError> {
    // Same authority as the request side. A response naming a human seat cannot
    // have come from a request this bridge issued, so it is refused rather than
    // used to pick for that player.
    if !matches!(
        draft_session.seats.get(usize::from(response.seat)),
        Some(DraftSeat::Bot { .. })
    ) {
        return Err(phase_llm::LlmError::StaleDecision);
    }
    // The pack is read through the same per-seat projection the request was
    // built from, never from `session.current_pack`. The projection can reorder
    // a pack for presentation (a set draft lists it by rarity), and the prompt's
    // numbered options, the fingerprint and the model's reply all speak in THAT
    // order. Fingerprinting or indexing the raw pack instead would refuse every
    // reply as stale, and where it did match would resolve the model's option
    // number to a different card.
    let view = filter_for_player(draft_session, response.seat);
    let Some(pack) = view.current_pack.as_deref().filter(|pack| !pack.is_empty()) else {
        return Err(phase_llm::LlmError::StaleDecision);
    };
    let provider = phase_llm::LlmProvider::from_label(&response.provider);
    // CR 905.1a / CR 903.13b: the configured procedure supplies the ordinary or Commander Draft pick-step count.
    let required = usize::from(draft_session.config.kind.procedure().cards_per_pick);
    // The reply is read against the option lines the REQUEST rendered: the same
    // pack, card database and difficulty `build_llm_draft_pick_requests` used.
    let difficulty = DIFFICULTY.with(|cell| cell.get());
    CARD_DB.with(|cell| {
        let db_borrow = cell.borrow();
        phase_llm::select_picks_from_response(
            response.seat,
            pack,
            required,
            &response.fingerprint,
            (db_borrow.as_ref(), difficulty),
            phase_llm::LlmReply {
                provider,
                status: response.status,
                body: &response.body,
            },
        )
    })
}

/// The engine-owned LLM provider catalog, mirrored here so a draft-only client
/// surface does not have to load the game engine to render the settings UI.
#[wasm_bindgen(js_name = llmProviderCatalog)]
pub fn llm_provider_catalog() -> JsValue {
    to_js(phase_llm::catalog::provider_catalog())
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
    let (main_deck, commanders) = parse_deck_payload(main_deck_json, commanders_json)?;

    with_draft_mut_inner(|session| {
        session::apply(
            session,
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck,
                commanders,
                // Dedicated local/P2P wrappers remain name-only.
                printings: Vec::new(),
            },
            None,
        )
        .map_err(deck_submission_message)?;

        Ok(filter_for_player(session, 0))
    })
}

/// Parse a deck submission's two payloads, before any session is touched. The
/// two parse failures carry DISTINCT texts so a caller can tell which payload
/// was bad.
fn parse_deck_payload(
    main_deck_json: &str,
    commanders_json: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let main_deck: Vec<String> =
        serde_json::from_str(main_deck_json).map_err(|e| format!("Failed to parse deck: {e}"))?;
    let commanders: Vec<String> = serde_json::from_str(commanders_json)
        .map_err(|e| format!("Failed to parse commanders: {e}"))?;
    Ok((main_deck, commanders))
}

/// Auto-suggest a playable Limited deck from the human's pool.
///
/// Returns a SuggestedDeck with ~23 spells + ~17 lands, using AI evaluation
/// at the current difficulty level. Per D-12: "Suggest deck" auto-build.
#[wasm_bindgen]
pub fn suggest_deck() -> Result<JsValue, JsValue> {
    with_draft(|session| to_js(&human_deck_suggestion(session)))
}

/// The deck suggestion for the human at seat 0, shared by [`suggest_deck`] and
/// the local runtime's suggestion.
fn human_deck_suggestion(session: &DraftSession) -> suggest::SuggestedDeck {
    let pool = &session.pools[0];
    let difficulty = DIFFICULTY.with(|cell| cell.get());

    CARD_DB.with(|cell| {
        let db_borrow = cell.borrow();
        let card_db = db_borrow.as_ref();
        // `0`: the HUMAN designates their commander in the deck builder, so
        // the suggester must not pre-empt that choice. A decision, not a
        // default.
        suggest::suggest_deck(
            pool,
            difficulty,
            card_db,
            session.config.min_deck_size,
            0,
            &session.config.addable_cards,
        )
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

    with_draft(|session| to_js(&human_land_suggestion(session, &spell_names)))
}

/// The land suggestion for `spell_names` against the human's pool at seat 0,
/// shared by [`suggest_lands`] and the local runtime's land suggestion.
fn human_land_suggestion(
    session: &DraftSession,
    spell_names: &[String],
) -> std::collections::HashMap<String, u8> {
    let pool = &session.pools[0];
    suggest::suggest_lands(spell_names, pool, session.config.min_deck_size)
}

/// Suggest land counts for spells in a specific multiplayer seat's pool.
///
/// The spells payload is parsed before the active session is accessed, so a
/// malformed request cannot observe or depend on the current draft state.
#[wasm_bindgen]
pub fn suggest_lands_for_seat(seat: u8, spells_json: &str) -> Result<JsValue, JsValue> {
    let lands = suggest_lands_for_seat_inner(seat, spells_json)
        .map_err(|error| JsValue::from_str(&error))?;
    Ok(to_js(&lands))
}

/// Native-testable core for the multiplayer Auto Lands boundary.
fn suggest_lands_for_seat_inner(
    seat: u8,
    spells_json: &str,
) -> Result<std::collections::HashMap<String, u8>, String> {
    let spell_names: Vec<String> = serde_json::from_str(spells_json)
        .map_err(|error| format!("Failed to parse spells: {error}"))?;

    with_draft_inner(|session| {
        let pool = session
            .pools
            .get(seat as usize)
            .ok_or_else(|| format!("Seat {seat} has no pool"))?;
        Ok(suggest::suggest_lands(
            &spell_names,
            pool,
            session.config.min_deck_size,
        ))
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
/// final pick. `Winston` has NO PICK STEP AT ALL and never reaches this
/// function: a shared-stack turn is a whole-pile
/// `DraftAction::SharedStackDecision`.
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
    let (main_deck, commanders) = parse_deck_payload(main_deck_json, commanders_json)?;

    with_draft_mut_inner(|session| {
        session::apply(
            session,
            DraftAction::SubmitDeck {
                seat,
                main_deck,
                commanders,
                printings: Vec::new(),
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

/// Why a saved session cannot be restored under the legacy admission.
enum RestoreRefusal {
    /// The text does not deserialize as a draft session.
    Unreadable(serde_json::Error),
    /// The session deserializes and the legacy admission refuses it, with the
    /// admission's own message.
    Incompatible(String),
}

/// The first two steps of restoring a saved session, shared by the restore and
/// the check: deserialize the text as a draft session, then apply the legacy
/// admission.
fn admitted_session_from_json(json: &str) -> Result<DraftSession, RestoreRefusal> {
    let session: DraftSession = serde_json::from_str(json).map_err(RestoreRefusal::Unreadable)?;
    admit_legacy_runtime(&session).map_err(RestoreRefusal::Incompatible)?;
    Ok(session)
}

/// Decode only snapshot shapes this client can safely present after a restore.
///
/// Player and spectator projections own the privacy boundary for a Chaos
/// layout: the persisted snapshot remains host-local, while all ordinary views
/// expose only `DraftSourceView`'s redacted metadata.
fn restorable_draft_session_from_json(json: &str) -> Result<DraftSession, String> {
    let mut session = admitted_session_from_json(json).map_err(|refusal| match refusal {
        RestoreRefusal::Unreadable(error) => {
            format!("Failed to deserialize draft session: {error}")
        }
        RestoreRefusal::Incompatible(error) => error,
    })?;
    session.config = enforce_procedure_minimum_deck_size(session.config);
    session
        .validate_persisted_snapshot()
        .map_err(|error| format!("Invalid draft snapshot: {error}"))?;
    Ok(session)
}

/// The draft engine's answer to whether a saved session could be restored
/// under its legacy admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
enum RestorableDraftSessionVerdict {
    /// The session deserializes and the legacy admission accepts it.
    Admitted,
    /// The session deserializes and the legacy admission refuses it.
    Incompatible,
    /// The text does not deserialize as a draft session.
    Unreadable,
}

/// Pure-Rust core for `check_restorable_draft_session`, so the verdict is
/// reachable from `cargo test` without `js_sys::JSON::parse`. It takes the
/// restore's first two steps from `admitted_session_from_json` and nothing
/// else: the procedure minimum and the snapshot validation stay the import's.
fn restorable_draft_session_verdict(json: &str) -> RestorableDraftSessionVerdict {
    match admitted_session_from_json(json) {
        Ok(_) => RestorableDraftSessionVerdict::Admitted,
        Err(RestoreRefusal::Incompatible(_)) => RestorableDraftSessionVerdict::Incompatible,
        Err(RestoreRefusal::Unreadable(_)) => RestorableDraftSessionVerdict::Unreadable,
    }
}

/// Check a saved draft session against the draft engine's legacy admission
/// without restoring it.
///
/// Returns `{ type: "Admitted" }` when the session deserializes and the
/// admission accepts it, `{ type: "Incompatible" }` when it deserializes and
/// the admission refuses it, and `{ type: "Unreadable" }` when it does not
/// deserialize as a draft session. It never throws, and it reads and writes no
/// installed session, random state, pack generator, difficulty or card
/// database.
#[wasm_bindgen]
pub fn check_restorable_draft_session(json: &str) -> JsValue {
    to_js(&restorable_draft_session_verdict(json))
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
    import_draft_session_inner(json, difficulty)
        .map(|view| to_js(&view))
        .map_err(|error| JsValue::from_str(&error))
}

fn import_draft_session_inner(
    json: &str,
    difficulty: u8,
) -> Result<draft_core::view::DraftPlayerView, String> {
    let session = restorable_draft_session_from_json(json)?;

    // The resume-seed offset is derived from `cards_in_pack` /
    // `current_pack_number` / `pick_number`, all of which stay `0` for a
    // shared-stack session (it moves none of them). That is harmless: this RNG
    // seeds the PICK-AND-PASS bot, and a shared-stack bot seat never reaches
    // it. `winston_decision` is deliberately RNG-free -- "same projection =>
    // same decision" is meant literally -- so a resumed Winston pod's bot
    // behaviour does not depend on this stream at all.
    let offset = u64::from(session.cards_in_pack(session.current_pack_number))
        * u64::from(session.current_pack_number)
        + u64::from(session.pick_number);
    let resume_seed = session.config.rng_seed.wrapping_add(offset);

    DIFFICULTY.with(|cell| cell.set(map_difficulty(difficulty)));
    RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(resume_seed))));

    let view = filter_for_player(&session, 0);
    session_cell::install(session);

    Ok(view)
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
    with_draft_inner(|session| bot_deck_for_seat(session, bot_seat))
}

/// A bot seat's auto-built deck from its drafted pool, shared by
/// [`get_bot_deck_inner`] and the local runtime's bot deck.
fn bot_deck_for_seat(
    session: &DraftSession,
    bot_seat: u8,
) -> Result<suggest::SuggestedDeck, String> {
    if bot_seat == 0 || bot_seat as usize >= session.seats.len() {
        return Err("bot_seat is out of range".to_string());
    }
    let pool = &session.pools[bot_seat as usize];
    let difficulty = DIFFICULTY.with(|cell| cell.get());

    CARD_DB.with(|cell| {
        let db_borrow = cell.borrow();
        let card_db = db_borrow.as_ref();

        // CR 407.3 + CR 903.3: Cube addables and Commander designations
        // require typed faces. Ordinary set-backed pods can propose a
        // deck without this optional database; game admission still
        // checks every submitted name and excludes ante cards.
        let requires_card_db = match &session.config.source {
            DraftSource::Set { .. } => session.config.kind.commanders_required() > 0,
            DraftSource::Cube { .. } => true,
        };
        if requires_card_db && card_db.is_none() {
            return Err("Card database must be loaded before a draft bot deck".to_string());
        }

        let deck = suggest::suggest_deck(
            pool,
            difficulty,
            card_db,
            session.config.min_deck_size,
            session.config.kind.commanders_required(),
            &session.config.addable_cards,
        );

        // Enforce the session-configured floor for the bot, as
        // `validate_limited_deck` does for human decks. CR 100.2b sets
        // ordinary Limited at 40 cards; CR 903.13f(1) sets Commander Draft
        // at 60. Custom Cube floors are configuration, not CR 100.2b.
        // Never publish an undersized proposal if the bot runs out of cards.
        let deck_total: usize =
            deck.main_deck.len() + deck.lands.values().map(|&n| n as usize).sum::<usize>();
        if deck_total < session.config.min_deck_size {
            return Err(format!(
                "Draft bot deck reached {deck_total} cards, minimum is {}",
                session.config.min_deck_size
            ));
        }

        Ok(deck)
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
    /// Which set-layout shapes this kind admits. The setup page renders exactly
    /// this list; it does not derive layout legality from `distribution`.
    allowed_set_layouts: Vec<draft_core::types::SetLayoutKind>,
    min_deck_size: usize,
    cube_min_deck_size: usize,
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
        allowed_set_layouts: procedure.allowed_set_layouts().to_vec(),
        min_deck_size: procedure.min_deck_size,
        cube_min_deck_size: procedure.cube_min_deck_size,
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
        // The sixth kind. No CR: Winston Draft has no Comprehensive Rules
        // section -- see `PackDistribution::SharedStackPiles`.
        DraftKind::Winston => 5,
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
/// multiplayer draft of any `DraftKind` with a wire number, with human + bot
/// seats from a Set pool, host-local Chaos candidate pools, or a custom Cube
/// list. A shared-stack kind admits bot seats like any other; their turns are
/// driven by `resolve_shared_stack_bot_turns`, which the host calls after each
/// human decision.
///
/// - `pool_input_json`: serialized `PoolInput` discriminated union
///   (`{ "type": "Set" | "Chaos" | "Cube", "data": { ... } }`)
/// - `seats_json`: JSON array of SeatDescriptors
/// - `kind`: the wire number for a `DraftKind`. The mapping's single authority
///   is `draft_kind_wire_number` — read it there rather than restating it here,
///   which is what keeps a widening from leaving this list stale. Flows through
///   to `DraftConfig.kind` unchanged. Tournament match format is identical to
///   set drafts.
/// - `seed`: RNG seed for deterministic pack generation
/// - `draft_code`: unique room identifier
/// - `difficulty`: the bot strength this pod's bot seats play at, through
///   `map_difficulty` (0..=4, anything else is `Medium`). APPENDED LAST, and it
///   must stay last: the client's call sites and their test mocks read this
///   boundary positionally.
///
///   It is not cosmetic. `DIFFICULTY` is a per-thread `Cell` with no reset that
///   outlives the draft that set it, and until now this entry point never wrote
///   it — so a player who finished a Quick draft at `VeryHard` and then hosted
///   a pod in the same tab got a `VeryHard` pod bot, silently, with no UI
///   saying so. Every other entry point that creates a session writes this cell
///   (`start_quick_draft`, `start_sealed_draft`, `start_quick_cube_draft`,
///   `import_draft_session`); this one now does too, so the strength a pod
///   plays at is the strength its host asked for.
///
/// Stores the session in the same thread-local as Quick Draft (one active
/// draft at a time per WASM instance). Returns the initial DraftPlayerView
/// for seat 0.
// The wasm boundary is positional by construction: `#[wasm_bindgen]` maps each
// parameter to one JS argument, and the host's `.d.ts` and its tests read this
// call by position. Bundling these eight into a struct would mean serializing a
// payload across the boundary just to satisfy an arity lint, and would move
// every existing argument — exactly what appending `difficulty` LAST avoids.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen]
pub fn create_multiplayer_draft(
    pool_input_json: &str,
    seats_json: &str,
    kind: u8,
    seed: u32,
    draft_code: &str,
    tournament_format: &str,
    pod_policy: &str,
    difficulty: u8,
) -> Result<JsValue, JsValue> {
    let view = create_multiplayer_draft_inner(
        pool_input_json,
        seats_json,
        kind,
        seed,
        draft_code,
        tournament_format,
        pod_policy,
        difficulty,
    )
    .map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&view))
}

/// Return the host-only original cube multiset for the game launched after a
/// draft. This deliberately bypasses `DraftPlayerView`: players and spectators
/// must never receive undealt cube entries or their duplicate counts.
#[wasm_bindgen]
pub fn booster_pack_pool_for_game() -> Result<JsValue, JsValue> {
    let pool = booster_pack_pool_for_game_inner().map_err(|error| JsValue::from_str(&error))?;
    Ok(to_js(&pool))
}

fn booster_pack_pool_for_game_inner() -> Result<Option<Vec<String>>, String> {
    with_draft_inner(|session| Ok(session.booster_pack_pool_for_game().map(<[String]>::to_vec)))
}

/// Pure-Rust core for `create_multiplayer_draft`. Returns a typed
/// `DraftPlayerView` so this branch is reachable from `cargo test` without
/// going through `js_sys::JSON::parse`. The WASM export wraps this with
/// `to_js` and `JsValue::from_str` error mapping.
///
/// `difficulty` is written to the `DIFFICULTY` thread-local beside
/// `session_cell::install` in each pool arm, exactly where `start_sealed_draft`
/// and the other session-creating entry points write it — ON THE SUCCESS PATH
/// ONLY. A failed creation leaves any previously installed session in place, so
/// writing the cell earlier would hand THAT session's bots a strength nobody
/// chose for it, which is the same cross-draft contamination this parameter
/// exists to end.
///
/// The parameter list mirrors the export's one-for-one; see the note there for
/// why the arity lint is allowed rather than designed around.
#[allow(clippy::too_many_arguments)]
fn create_multiplayer_draft_inner(
    pool_input_json: &str,
    seats_json: &str,
    kind: u8,
    seed: u32,
    draft_code: &str,
    tournament_format: &str,
    pod_policy: &str,
    difficulty: u8,
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
                flow: draft_core::types::DraftFlow::LegacyBarrier,
                selection_policy: draft_core::types::DraftSelectionPolicy::default(),
                clock_policy: draft_core::types::DraftClockPolicy::default(),
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

            session_cell::install(draft_session);
            PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
            DIFFICULTY.with(|cell| cell.set(map_difficulty(difficulty)));
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
                flow: draft_core::types::DraftFlow::LegacyBarrier,
                selection_policy: draft_core::types::DraftSelectionPolicy::default(),
                clock_policy: draft_core::types::DraftClockPolicy::default(),
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

            session_cell::install(draft_session);
            PACK_GEN.with(|cell| cell.set(Some(pack_gen)));
            DIFFICULTY.with(|cell| cell.set(map_difficulty(difficulty)));
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
                // A shared stack is built by shuffling every opened pack
                // together, and a cube source generates packs just as a set
                // source does — so Winston-from-cube is permitted and this arm
                // falls through with the pick-and-pass one.
                PackDistribution::PickAndPass | PackDistribution::SharedStackPiles { .. } => {}
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
            let config = enforce_procedure_minimum_deck_size(DraftConfig {
                flow: draft_core::types::DraftFlow::LegacyBarrier,
                selection_policy: draft_core::types::DraftSelectionPolicy::default(),
                clock_policy: draft_core::types::DraftClockPolicy::default(),
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
            });

            let mut draft_session = DraftSession::new(config, seats, draft_code.to_string());
            let pack_source = CubePackSource::new(cards);

            session::apply(
                &mut draft_session,
                DraftAction::StartDraft,
                Some(&pack_source),
            )
            .map_err(|e| format!("Failed to start cube draft: {}", e))?;

            let view = filter_for_player(&draft_session, 0);

            session_cell::install(draft_session);
            PACK_GEN.with(|cell| cell.set(None));
            DIFFICULTY.with(|cell| cell.set(map_difficulty(difficulty)));
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
    apply_draft_action_inner(action_json)
        .map(|deltas| to_js(&deltas))
        .map_err(|error| JsValue::from_str(&error))
}

fn apply_draft_action_inner(action_json: &str) -> Result<Vec<DraftDelta>, String> {
    let action: DraftAction =
        serde_json::from_str(action_json).map_err(|e| format!("Failed to parse action: {}", e))?;
    if action.is_fifo_only() {
        return Err(FIFO_UNSUPPORTED_BY_RUNTIME.to_string());
    }

    with_draft_mut_inner(|draft_session| {
        session::apply(draft_session, action, None)
            .map_err(|e| format!("Draft action failed: {}", e))
    })
}

/// The termination bound for one shared-stack bot run, DERIVED from the state
/// rather than chosen, so it tracks the real board instead of a magic constant.
///
/// With `U` undrafted cards (main stack plus every pile) and `n` piles, the
/// bound is `U * (n + 1) + 1`. The proof it comes from, read off
/// `shared_stack::apply_shared_stack_decision`:
///
/// * `Phi = U * (n + 1) + (n - c)` is a non-negative integer (`c <= n - 1`, so
///   `n - c >= 1`) and STRICTLY DECREASES on every applied decision. A `Take` or
///   a final-pile `Decline` drops `U` by at least 1, costing at least `n + 1`,
///   while `(n - c)` can rise by at most `n - 1`; a non-final `Decline` leaves
///   `U` alone and drops `(n - c)` by exactly 1. So the loop terminates.
/// * The `Err` branch is unreachable, by a tighter per-turn count: a turn's
///   cursor starts at 0 and advances by exactly 1 per non-final `Decline`, which
///   requires `cursor + 1 < n`, so a turn holds at most `n - 1` non-final
///   declines plus exactly one turn-ender -- **at most `n` decisions per turn**.
///   Every turn-ender drops `U` by at least 1 and the session leaves `Drafting`
///   the instant `U` reaches 0, so there are at most `U` turns. Therefore
///   `applied <= U * n < U * (n + 1) + 1`, with slack `U + 1`.
///
/// MEASURED, for scale: a 2-seat 90-card pod gives `U = 90`, `n = 3`, a bound of
/// 361, a theoretical ceiling of 270 and 74-77 actual decisions; a 4-seat pod's
/// longest consecutive bot run was 9 decisions against a bound of 720.
///
/// Returns 0 for a session with no shared stack -- there is nothing to drive,
/// and the loop's own `break` handles that case before the bound is consulted.
fn shared_stack_bot_turn_bound(session: &DraftSession) -> usize {
    let Some(state) = session.shared_stack.as_ref() else {
        return 0;
    };
    let undrafted = state.main_stack.len() + state.piles.iter().map(Vec::len).sum::<usize>();
    undrafted
        .saturating_mul(state.piles.len() + 1)
        .saturating_add(1)
}

/// Drive every consecutive shared-stack turn that belongs to a bot seat, from
/// whatever state the session is in, and stop at the first turn that does not.
///
/// A bot's turn is up to `pile_count` decisions and a 3-4 seat pod can have
/// several bot turns back to back (MEASURED: the longest consecutive run in a
/// 4-seat/3-bot pod was 9 decisions across three turns), so this re-reads
/// `active_seat` every iteration. A one-decision-per-call driver would strand
/// the pod mid-run.
///
/// `bound` is a parameter rather than an inline expression ONLY so
/// `the_loop_fails_loudly_rather_than_spinning` can drive the failure branch
/// without mutating the production arithmetic; the production caller passes
/// [`shared_stack_bot_turn_bound`], computed once at entry from the state.
///
/// **Loud on violation, never silent.** Three distinct failures get three
/// distinct `Err`s and none of them is a `break`:
///
/// * the bound trips -- an engine invariant broke, and the termination proof
///   above says this is unreachable from any legal state;
/// * `winston_decision` returns `None` for the active seat while drafting,
///   which `some_decision_is_always_legal_for_the_active_seat_while_drafting`
///   proves cannot happen, so it is a defect rather than a state;
/// * `session::apply` refuses the decision -- the bot disagreed with the single
///   legality authority, which `winston_decision_never_returns_a_refused_decision`
///   says it cannot.
///
/// The `break`s are for the three LEGITIMATE exits only: the session is not
/// drafting, it has no shared stack, or the active seat is human. A `break` on
/// the bound would strand the pod on a bot's turn with no error anywhere.
fn drive_shared_stack_bot_turns(
    session: &mut DraftSession,
    difficulty: AiDifficulty,
    card_db: Option<&CardDatabase>,
    bound: usize,
) -> Result<Vec<DraftDelta>, String> {
    let mut deltas = Vec::new();
    let mut applied = 0usize;

    while session.status == DraftStatus::Drafting {
        let Some(state) = session.shared_stack.as_ref() else {
            break;
        };
        let seat = state.active_seat;
        if !matches!(
            session.seats.get(usize::from(seat)),
            Some(DraftSeat::Bot { .. })
        ) {
            break;
        }

        applied += 1;
        if applied > bound {
            return Err(format!(
                "shared-stack bot loop exceeded its termination bound of {bound} decisions \
                 (seat {seat}); the session's undrafted count is not decreasing"
            ));
        }

        // THE ONLY INPUT the bot gets. Not the session, not the cell: the same
        // projection the human seat across the table receives.
        let view = filter_for_player(session, seat);
        let (pile, decision) = bot_ai::winston_decision(&view, difficulty, card_db)
            .ok_or_else(|| format!("no legal shared-stack decision for bot seat {seat}"))?;

        deltas.extend(
            session::apply(
                session,
                DraftAction::SharedStackDecision {
                    seat,
                    pile,
                    decision,
                },
                None,
            )
            .map_err(|e| format!("bot seat {seat} decision refused: {e}"))?,
        );
    }

    Ok(deltas)
}

/// Pure-Rust core for `resolve_shared_stack_bot_turns`, so the loop, its bound
/// and its three `Err`s are reachable from `cargo test` without wasm-bindgen --
/// the same `_inner` split `create_multiplayer_draft_inner` and
/// `booster_pack_pool_for_game_inner` use.
fn resolve_shared_stack_bot_turns_inner() -> Result<Vec<DraftDelta>, String> {
    let difficulty = DIFFICULTY.with(|cell| cell.get());
    with_draft_mut_inner(|session| {
        let bound = shared_stack_bot_turn_bound(session);
        CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            drive_shared_stack_bot_turns(session, difficulty, db_borrow.as_ref(), bound)
        })
    })
}

/// Resolve every consecutive shared-stack turn owned by a bot seat, and return
/// the `DraftDelta`s produced (an empty array when the active seat is human, the
/// draft is over, or the session has no shared stack).
///
/// The host calls this after applying a human seat's decision and after starting
/// a pod whose first seat is a bot. It is a WASM export because the loop, its
/// bound and its termination proof are engine concerns: the client calls it and
/// renders what comes back, and computes nothing.
#[wasm_bindgen]
pub fn resolve_shared_stack_bot_turns() -> Result<JsValue, JsValue> {
    let deltas = resolve_shared_stack_bot_turns_inner().map_err(|e| JsValue::from_str(&e))?;
    Ok(to_js(&deltas))
}

/// The decision the ENGINE would apply for a seat whose turn must be resolved
/// without that seat choosing — a pick-timer expiry, or a disconnect.
///
/// `None` when there is no shared stack, or when the seat has no legal move
/// (which for the ACTIVE seat while drafting is unreachable, and proved so by
/// `some_decision_is_always_legal_for_the_active_seat_while_drafting`).
///
/// WHY THIS EXISTS AS AN EXPORT. The host used to scan the published
/// `legality` vector itself — `legality.find(entry => entry.refusal === null)`
/// — and take the first entry with no refusal. That is the same algorithm
/// `shared_stack::forced_decision` runs, but over a DIFFERENT ordering source:
/// the engine folds `SharedStackPileDecision::ALL` in declaration order, while
/// the client folded whatever order the view happened to serialize. The two
/// agreed by coincidence rather than by construction, and a reordering of
/// either would have silently changed which move a timed-out seat makes.
///
/// Choosing a rules outcome is the reducer's job. The host may ASK for the
/// forced resolution — that is a timeout, which is a host concern — but the
/// answer comes from here, and the host only dispatches it through the ordinary
/// decision path so the timed-out turn is persisted, acknowledged, broadcast and
/// re-armed by exactly the code a player-driven one is.
#[wasm_bindgen]
pub fn shared_stack_forced_decision(seat_index: u8) -> Result<JsValue, JsValue> {
    with_draft(|session| {
        let decision = session
            .shared_stack
            .as_ref()
            .and_then(|state| draft_core::shared_stack::forced_decision(state, seat_index));
        to_js(&decision)
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

// ── Local FIFO runtime ───────────────────────────────────────────────────────
//
// A FIFO Quick draft this runtime owns locally: one human at seat 0 and an
// engine-run bot at every other seat. The general gates above refuse every FIFO
// session, and they still do; the local gates below admit only this shape, and
// only for the caller holding the installed session's live local owner
// (`session_cell::LocalOwner`). Each operation is one export over a
// native-testable `_inner` core, and every mutating one takes the host's
// explicit time.

/// The seat the local human occupies, and the only seat the local runtime
/// projects.
const LOCAL_HUMAN_SEAT: u8 = 0;

/// The prefix of every session id the local runtime issues.
const LOCAL_SESSION_ID_PREFIX: &str = "local-";

/// The local gates' refusal for a session they do not admit.
const NOT_A_LOCAL_FIFO_SESSION: &str = "This draft is not a local FIFO Quick draft";

/// Whether `id` has the one shape the local runtime issues:
/// [`LOCAL_SESSION_ID_PREFIX`] followed by exactly 16 lowercase hex digits.
fn is_canonical_local_session_id(id: &DraftSessionId) -> bool {
    id.0.strip_prefix(LOCAL_SESSION_ID_PREFIX)
        .is_some_and(|hex| {
            hex.len() == 16
                && hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Admit only a session the local runtime can own: the FIFO flow, the Quick
/// kind, a human at seat 0 and a bot at every other seat, under a canonical
/// local session id. Every refusal is [`NOT_A_LOCAL_FIFO_SESSION`].
///
/// CR 905.1c: during the draft a player can look only at the booster pack they
/// are drafting from, the cards they have drafted, and cards revealed or
/// drafted face up. The runtime projects one seat, so exactly one seat may be a
/// person, and it is seat 0. The admission indexes no seat: a session with no
/// seats has no human at seat 0 and is refused.
fn admit_local_fifo_runtime(session: &DraftSession) -> Result<(), String> {
    let refuse = || Err(NOT_A_LOCAL_FIFO_SESSION.to_string());
    if session.config.flow != DraftFlow::FifoAsync {
        return refuse();
    }
    match session.config.kind {
        DraftKind::Quick => {}
        DraftKind::Premier
        | DraftKind::Traditional
        | DraftKind::Sealed
        | DraftKind::CommanderDraft
        | DraftKind::Winston => return refuse(),
    }
    let human_at_local_seat = matches!(
        session.seats.get(usize::from(LOCAL_HUMAN_SEAT)),
        Some(DraftSeat::Human { .. })
    );
    let every_other_seat_is_a_bot = session
        .seats
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != usize::from(LOCAL_HUMAN_SEAT))
        .all(|(_, seat)| matches!(seat, DraftSeat::Bot { .. }));
    let canonical_id = session
        .fifo
        .as_ref()
        .is_some_and(|fifo| is_canonical_local_session_id(&fifo.session_id));
    if human_at_local_seat && every_other_seat_is_a_bot && canonical_id {
        Ok(())
    } else {
        refuse()
    }
}

/// The seats of a local Quick pod of `pod_size`: the human at seat 0
/// ([`LOCAL_HUMAN_SEAT`]), then a bot at every other seat, the one shape
/// [`admit_local_fifo_runtime`] admits.
fn local_quick_seats(pod_size: u8) -> Vec<DraftSeat> {
    std::iter::once(DraftSeat::Human {
        player_id: engine::types::player::PlayerId(0),
        display_name: "Player".to_string(),
    })
    .chain((1..pod_size).map(|index| DraftSeat::Bot {
        name: format!("Bot {index}"),
    }))
    .collect()
}

/// The local read gate: the live owner, then [`admit_local_fifo_runtime`], both
/// before `f` sees the session. A parallel of [`with_draft_inner`], not a
/// widening of it.
fn with_local_fifo_inner<R>(
    owner: LocalOwner,
    f: impl FnOnce(&DraftSession) -> Result<R, String>,
) -> Result<R, String> {
    session_cell::with_local_draft_inner(owner, |session| {
        admit_local_fifo_runtime(session).and_then(|()| f(session))
    })
}

/// The local mutable gate: the live owner, then [`admit_local_fifo_runtime`],
/// both before `f` sees the session. A parallel of [`with_draft_mut_inner`],
/// not a widening of it.
fn with_local_fifo_mut_inner<R>(
    owner: LocalOwner,
    f: impl FnOnce(&mut DraftSession) -> Result<R, String>,
) -> Result<R, String> {
    session_cell::with_local_draft_mut_inner(owner, |session| {
        admit_local_fifo_runtime(session).and_then(|()| f(session))
    })
}

/// What a local install returns: the owner the caller presents on every later
/// call, and seat 0's view.
#[derive(Serialize)]
struct LocalFifoSession {
    owner: LocalOwner,
    view: DraftPlayerView,
}

/// Who decides a local bot seat's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum LocalLlmDrafting {
    /// The engine bot drafts every bot seat as soon as it holds a head.
    EngineBots,
    /// Each bot seat's head waits for that seat's LLM reply, applied or fallen
    /// back; a forced head is taken by the engine bot.
    LlmSeats,
}

/// Install a local session with its difficulty, the bots' random state and
/// its LLM drafting, and issue its owner. The one tail every local install
/// shares, so none can leave an earlier install's runtime state behind. No
/// pack generator is kept: every FIFO pack is dealt at `StartDraft`.
fn install_local_fifo(
    session: DraftSession,
    difficulty: AiDifficulty,
    rng: ChaCha20Rng,
    drafting: LocalLlmDrafting,
) -> LocalOwner {
    let owner = session_cell::install_local(session);
    DIFFICULTY.with(|cell| cell.set(difficulty));
    RNG.with(|cell| cell.set(Some(rng)));
    PACK_GEN.with(|cell| cell.set(None));
    LOCAL_LLM_DRAFTING.with(|cell| cell.set(drafting));
    owner
}

/// Create, start and install a local FIFO Quick draft, and issue its owner.
/// The Quick constructors, [`start_quick_draft`] and
/// [`start_quick_cube_draft`], are its callers.
///
/// Everything fallible runs before any thread-local is written. The session is
/// constructed under a session id the runtime issues itself, never one a caller
/// supplies; it is admitted by [`admit_local_fifo_runtime`] BEFORE `StartDraft`
/// deals a single pack; it starts at `now_ms`; and its bots run to quiescence
/// under `drafting`. Only then is it installed, with its difficulty, the bots'
/// random state and `drafting`.
///
/// CR 905.2a: during a draft there is no active player or system of priority,
/// so a bot seat drafts as soon as it holds a pack, and the start batch makes
/// every such pick before the human's first view; under
/// [`LocalLlmDrafting::LlmSeats`] a seat's choice waits for its own drafter.
fn start_local_fifo_draft(
    config: DraftConfig,
    seats: Vec<DraftSeat>,
    draft_code: String,
    pack_source: &dyn PackSource,
    difficulty: AiDifficulty,
    drafting: LocalLlmDrafting,
    now_ms: u64,
) -> Result<LocalFifoSession, String> {
    let session_id = DraftSessionId(format!(
        "{LOCAL_SESSION_ID_PREFIX}{:016x}",
        rand::rng().random::<u64>()
    ));
    let mut session = DraftSession::new_fifo(config, seats, draft_code, session_id)
        .map_err(|e| format!("Failed to create local draft: {e}"))?;
    admit_local_fifo_runtime(&session)?;
    session::apply_at(
        &mut session,
        DraftAction::StartDraft,
        Some(pack_source),
        now_ms,
    )
    .map_err(|e| format!("Failed to start local draft: {e}"))?;

    let mut rng = ChaCha20Rng::seed_from_u64(session.config.rng_seed);
    let enhancement = PICK_ENHANCEMENT.with(|cell| cell.get());
    let bound = local_fifo_bot_batch_bound(&session);
    CARD_DB.with(|cell| {
        let db_borrow = cell.borrow();
        drive_local_fifo_bot_batch(
            &mut session,
            &LocalBotSettings {
                difficulty,
                card_db: db_borrow.as_ref(),
                enhancement,
                drafting,
            },
            &mut rng,
            now_ms,
            bound,
        )
    })?;
    let view = filter_for_player(&session, LOCAL_HUMAN_SEAT);

    let owner = install_local_fifo(session, difficulty, rng, drafting);
    Ok(LocalFifoSession { owner, view })
}

/// The local draft's view: seat 0's projection, which publishes every seat's
/// FIFO progress.
///
/// `owner`: the owner the local start or import returned. Any other owner is
/// refused before the session is read.
#[wasm_bindgen]
pub fn local_fifo_view(owner: u32) -> Result<JsValue, JsValue> {
    local_fifo_view_inner(LocalOwner::from_raw(owner))
        .map(|view| to_js(&view))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_view`].
fn local_fifo_view_inner(owner: LocalOwner) -> Result<DraftPlayerView, String> {
    with_local_fifo_inner(owner, |session| {
        Ok(filter_for_player(session, LOCAL_HUMAN_SEAT))
    })
}

/// The local human's composed selection, bound to the prompt it answers.
///
/// It names no seat: the seat is always the owner's, seat 0, and a payload
/// that names one is refused as an unknown field before the session is read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalFifoSelection {
    prompt_id: DraftPromptId,
    picked_card_instance_ids: Vec<String>,
    burned_card_instance_ids: Vec<String>,
    /// The draft-effect cards this selection uses, empty for none, in the
    /// order they go back into the pack.
    effect_card_instance_ids: Vec<String>,
}

/// The selection the engine makes for `seat` on the prompt `prompt_id`: the
/// bot's picks, the burns the policy requires, and no draft-effect card.
/// `None` when the seat holds no head pack or owes no pick.
///
/// CR 905.1c: a player can look only at the pack they are drafting from, the
/// cards they have drafted, and cards revealed or drafted face up, so `view`
/// (that seat's own projection) is all this reads. The counts are the engine's
/// published `requirements`, never
/// literals: a kind or policy that takes more than one card, or burns, per
/// step is honoured as published. The pick indices are mapped to instance ids
/// before anything moves, and the burns are the head's remaining cards in the
/// projection's order.
fn fifo_ai_selection(
    view: &DraftPlayerView,
    seat: u8,
    prompt_id: DraftPromptId,
    difficulty: AiDifficulty,
    card_db: Option<&CardDatabase>,
    enhancement: bot_ai::PickEnhancement,
    rng: &mut ChaCha20Rng,
) -> Option<DraftAction> {
    let fifo = view.fifo.as_ref()?;
    let head = fifo.head.as_ref()?;
    let pack = view.current_pack.as_deref()?;
    let requirements = fifo.requirements;
    if requirements.picks == 0 {
        return None;
    }
    let picked_card_instance_ids: Vec<String> = bot_ai::bot_picks(
        pack,
        requirements.picks,
        difficulty,
        &view.pool,
        card_db,
        enhancement,
        bot_ai::PackShape {
            pack_number: head.round,
            cards_in_pack: head.original_size,
        },
        rng,
    )
    .into_iter()
    .map(|index| pack[index].instance_id.clone())
    .collect();
    let burned_card_instance_ids = fifo_burns(pack, &picked_card_instance_ids, requirements.burns);
    Some(DraftAction::Select {
        seat,
        prompt_id,
        picked_card_instance_ids,
        burned_card_instance_ids,
        effect_card_instance_ids: Vec::new(),
    })
}

/// The cards an engine-composed selection burns: the head's remaining cards
/// in the projection's order, `burns` of them. Shared by the engine bot and an
/// LLM drafter's selection, so both burn by the same rule.
fn fifo_burns(pack: &[DraftCardInstance], picked: &[String], burns: usize) -> Vec<String> {
    pack.iter()
        .map(|card| &card.instance_id)
        .filter(|id| !picked.contains(*id))
        .take(burns)
        .cloned()
        .collect()
}

/// The termination bound for one local bot batch, DERIVED from the state: the
/// undrafted cards (every queued pack plus every unopened pack), plus one.
///
/// Every bot application removes at least one card from that total: the
/// engine's requirements take at least one pick from a non-empty head, and a
/// bot returns no draft-effect card to the pack. Opening a round moves cards
/// from unopened packs into queues without changing the total. So a batch makes
/// at most as many applications as there are undrafted cards, and the bound
/// cannot trip from a legal state.
fn local_fifo_bot_batch_bound(session: &DraftSession) -> usize {
    let queued: usize = session.fifo.as_ref().map_or(0, |fifo| {
        fifo.seats
            .iter()
            .flat_map(|seat| &seat.queue)
            .map(|queued| queued.pack.0.len())
            .sum()
    });
    let unopened: usize = session
        .packs_by_seat
        .iter()
        .flatten()
        .map(|pack| pack.0.len())
        .sum();
    queued.saturating_add(unopened).saturating_add(1)
}

/// The engine bot's settings for one local bot batch, and who decides the bot
/// seats' heads.
struct LocalBotSettings<'a> {
    difficulty: AiDifficulty,
    card_db: Option<&'a CardDatabase>,
    enhancement: bot_ai::PickEnhancement,
    drafting: LocalLlmDrafting,
}

/// Whether `seat`'s head waits for its LLM drafter's reply rather than being
/// drafted by the engine bot: under [`LocalLlmDrafting::LlmSeats`], for an
/// LLM-eligible seat ([`llm_eligible_bot_seats`]) whose step is a real choice.
///
/// CR 905.1c: `view` is that seat's own projection, the only thing it may look
/// at. CR 905.1a: a step that takes every card left is no choice, so a forced
/// head never waits; the prompt builder's own classification
/// (`draft_pick_step`) decides it, so a head waits exactly when a prompt can be
/// built for it.
fn llm_holds_head(
    session: &DraftSession,
    seat: u8,
    view: &DraftPlayerView,
    drafting: LocalLlmDrafting,
) -> bool {
    match drafting {
        LocalLlmDrafting::EngineBots => false,
        LocalLlmDrafting::LlmSeats => {
            llm_eligible_bot_seats(session).contains(&seat)
                && matches!(
                    phase_llm::draft_decision::draft_pick_step(view),
                    phase_llm::draft_decision::DraftPickStep::Choice { .. }
                )
        }
    }
}

/// Run the local bot seats to quiescence: while the session is drafting, the
/// lowest-indexed bot seat holding a head pack the engine drafts makes the
/// selection [`fifo_ai_selection`] chooses from its own projection, at
/// `now_ms`, until no bot seat holds a head the engine drafts. A head that
/// [`llm_holds_head`] holds under `settings.drafting` is left for its seat's
/// LLM reply.
///
/// CR 905.2a: during a draft there is no active player or system of priority;
/// when several players would act at once and cannot agree on an order, the
/// order is random. The engine runs every bot seat, and they act in seat order,
/// an order they agree on, which makes the batch deterministic for a given
/// random state. Which packs each seat ends up holding does not depend on that
/// order: a seat receives packs only from its one neighbour, in the order that
/// neighbour passes them, so only the order of the random draws does.
///
/// Atomic per application: each selection commits whole through
/// `session::apply_at` or not at all, and `rng` advances only when it commits.
///
/// `bound` is a parameter so `the_bot_batch_fails_loudly_rather_than_spinning`
/// can drive the failure branch; production passes
/// [`local_fifo_bot_batch_bound`]. Loud on violation, never silent: the bound
/// tripping, a bot seat with no selection, and a refused selection are each an
/// `Err`. The legitimate exits are the session leaving `Drafting` and no bot
/// seat holding a head the engine drafts.
fn drive_local_fifo_bot_batch(
    session: &mut DraftSession,
    settings: &LocalBotSettings<'_>,
    rng: &mut ChaCha20Rng,
    now_ms: u64,
    bound: usize,
) -> Result<(), String> {
    let mut applied = 0usize;
    while session.status == DraftStatus::Drafting {
        // THE ONLY INPUT the bot gets: the projection a person in that seat
        // would see. It is built once per seat holding a head, and the same
        // projection decides the hold and the selection.
        let Some((seat, view)) = session
            .seats
            .iter()
            .enumerate()
            .filter(|(index, seat)| {
                *index != usize::from(LOCAL_HUMAN_SEAT) && matches!(seat, DraftSeat::Bot { .. })
            })
            .filter_map(|(index, _)| u8::try_from(index).ok())
            .filter(|&seat| session.head_pack(seat).is_some())
            .map(|seat| (seat, filter_for_player(session, seat)))
            .find(|(seat, view)| !llm_holds_head(session, *seat, view, settings.drafting))
        else {
            break;
        };

        applied += 1;
        if applied > bound {
            return Err(format!(
                "local FIFO bot batch exceeded its termination bound of {bound} selections \
                 (seat {seat}); the undrafted card count is not decreasing"
            ));
        }

        let mut trial = rng.clone();
        let action = view
            .fifo
            .as_ref()
            .and_then(|fifo| fifo.prompt_id.clone())
            .and_then(|prompt_id| {
                fifo_ai_selection(
                    &view,
                    seat,
                    prompt_id,
                    settings.difficulty,
                    settings.card_db,
                    settings.enhancement,
                    &mut trial,
                )
            })
            .ok_or_else(|| format!("no FIFO selection for bot seat {seat}"))?;
        session::apply_at(session, action, None, now_ms)
            .map_err(|e| format!("bot seat {seat} selection refused: {e}"))?;
        *rng = trial;
    }
    Ok(())
}

/// [`drive_local_fifo_bot_batch`] over the runtime's difficulty, pick
/// enhancement, LLM drafting, random state and card database, with the derived
/// bound. The random state goes back on every exit, an error included: it then
/// holds the state after the last committed application.
fn run_local_fifo_bot_batch(session: &mut DraftSession, now_ms: u64) -> Result<(), String> {
    let difficulty = DIFFICULTY.with(|cell| cell.get());
    let enhancement = PICK_ENHANCEMENT.with(|cell| cell.get());
    let drafting = LOCAL_LLM_DRAFTING.with(Cell::get);
    let mut rng = RNG
        .with(|cell| cell.take())
        .ok_or_else(|| "RNG not initialized".to_string())?;
    let bound = local_fifo_bot_batch_bound(session);
    let result = CARD_DB.with(|cell| {
        let db_borrow = cell.borrow();
        drive_local_fifo_bot_batch(
            session,
            &LocalBotSettings {
                difficulty,
                card_db: db_borrow.as_ref(),
                enhancement,
                drafting,
            },
            &mut rng,
            now_ms,
            bound,
        )
    });
    RNG.with(|cell| cell.set(Some(rng)));
    result
}

/// Apply the local human's composed selection at `now_ms`, then run the bots to
/// quiescence.
///
/// `selection_json`: `{ prompt_id, picked_card_instance_ids,
/// burned_card_instance_ids, effect_card_instance_ids }`, with the prompt id
/// copied from the view. It names no seat; the seat is the owner's. The payload
/// is parsed before the session is touched, and a stale prompt changes nothing.
/// Returns seat 0's view.
#[wasm_bindgen]
pub fn local_fifo_select(
    owner: u32,
    selection_json: &str,
    now_ms: u64,
) -> Result<JsValue, JsValue> {
    local_fifo_select_inner(LocalOwner::from_raw(owner), selection_json, now_ms)
        .map(|view| to_js(&view))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_select`].
fn local_fifo_select_inner(
    owner: LocalOwner,
    selection_json: &str,
    now_ms: u64,
) -> Result<DraftPlayerView, String> {
    let selection: LocalFifoSelection = serde_json::from_str(selection_json)
        .map_err(|e| format!("Failed to parse selection: {e}"))?;
    with_local_fifo_mut_inner(owner, |session| {
        session::apply_at(
            session,
            DraftAction::Select {
                seat: LOCAL_HUMAN_SEAT,
                prompt_id: selection.prompt_id,
                picked_card_instance_ids: selection.picked_card_instance_ids,
                burned_card_instance_ids: selection.burned_card_instance_ids,
                effect_card_instance_ids: selection.effect_card_instance_ids,
            },
            None,
            now_ms,
        )
        .map_err(|e| format!("Selection failed: {e}"))?;
        run_local_fifo_bot_batch(session, now_ms)?;
        Ok(filter_for_player(session, LOCAL_HUMAN_SEAT))
    })
}

/// Let the engine make the local human's selection on the prompt
/// `prompt_id_json` at `now_ms`, with the bots' own choice at the active
/// difficulty, then run the bots to quiescence.
///
/// The auto-pick is bound to the caller's prompt, so a stale prompt is refused
/// and changes nothing, the random state included. It spends no draft-effect
/// card. Returns seat 0's view.
#[wasm_bindgen]
pub fn local_fifo_auto_select(
    owner: u32,
    prompt_id_json: &str,
    now_ms: u64,
) -> Result<JsValue, JsValue> {
    local_fifo_auto_select_inner(LocalOwner::from_raw(owner), prompt_id_json, now_ms)
        .map(|view| to_js(&view))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_auto_select`]. The selection is drawn from a
/// copy of the random state, which replaces the stored one only when the
/// selection commits; on every refusal the stored state is put back untouched.
fn local_fifo_auto_select_inner(
    owner: LocalOwner,
    prompt_id_json: &str,
    now_ms: u64,
) -> Result<DraftPlayerView, String> {
    let prompt_id: DraftPromptId = serde_json::from_str(prompt_id_json)
        .map_err(|e| format!("Failed to parse prompt id: {e}"))?;
    with_local_fifo_mut_inner(owner, |session| {
        let view = filter_for_player(session, LOCAL_HUMAN_SEAT);
        let difficulty = DIFFICULTY.with(|cell| cell.get());
        let enhancement = PICK_ENHANCEMENT.with(|cell| cell.get());
        let stored = RNG
            .with(|cell| cell.take())
            .ok_or_else(|| "RNG not initialized".to_string())?;
        let mut trial = stored.clone();
        let applied = CARD_DB
            .with(|cell| {
                let db_borrow = cell.borrow();
                fifo_ai_selection(
                    &view,
                    LOCAL_HUMAN_SEAT,
                    prompt_id,
                    difficulty,
                    db_borrow.as_ref(),
                    enhancement,
                    &mut trial,
                )
            })
            .ok_or_else(|| "No pack to pick from".to_string())
            .and_then(|action| {
                session::apply_at(session, action, None, now_ms)
                    .map_err(|e| format!("Auto-pick failed: {e}"))
            });
        let kept = if applied.is_ok() { trial } else { stored };
        RNG.with(|cell| cell.set(Some(kept)));
        applied?;
        run_local_fifo_bot_batch(session, now_ms)?;
        Ok(filter_for_player(session, LOCAL_HUMAN_SEAT))
    })
}

/// Submit the local human's deck at `now_ms`.
///
/// `main_deck_json`: JSON array of card name strings. `commanders_json`: JSON
/// array of the card names designated as commander(s), `[]` for a Quick
/// draft. Both are parsed before the session is touched. Returns seat 0's
/// view.
#[wasm_bindgen]
pub fn local_fifo_submit_deck(
    owner: u32,
    main_deck_json: &str,
    commanders_json: &str,
    now_ms: u64,
) -> Result<JsValue, JsValue> {
    local_fifo_submit_deck_inner(
        LocalOwner::from_raw(owner),
        main_deck_json,
        commanders_json,
        now_ms,
    )
    .map(|view| to_js(&view))
    .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_submit_deck`].
fn local_fifo_submit_deck_inner(
    owner: LocalOwner,
    main_deck_json: &str,
    commanders_json: &str,
    now_ms: u64,
) -> Result<DraftPlayerView, String> {
    let (main_deck, commanders) = parse_deck_payload(main_deck_json, commanders_json)?;
    with_local_fifo_mut_inner(owner, |session| {
        // CR 100.2b: a limited deck has a minimum deck size of 40 cards, built
        // from the product plus basic lands. The engine's deck validation
        // enforces the session's floor inside the FIFO transaction, so a
        // refused deck leaves the session unchanged.
        session::apply_at(
            session,
            DraftAction::SubmitDeck {
                seat: LOCAL_HUMAN_SEAT,
                main_deck,
                commanders,
                printings: Vec::new(),
            },
            None,
            now_ms,
        )
        .map_err(deck_submission_message)?;
        Ok(filter_for_player(session, LOCAL_HUMAN_SEAT))
    })
}

/// Auto-suggest a playable Limited deck from the local human's pool, as
/// [`suggest_deck`] does for the legacy runtime.
#[wasm_bindgen]
pub fn local_fifo_suggest_deck(owner: u32) -> Result<JsValue, JsValue> {
    local_fifo_suggest_deck_inner(LocalOwner::from_raw(owner))
        .map(|deck| to_js(&deck))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_suggest_deck`].
fn local_fifo_suggest_deck_inner(owner: LocalOwner) -> Result<suggest::SuggestedDeck, String> {
    with_local_fifo_inner(owner, |session| Ok(human_deck_suggestion(session)))
}

/// Suggest land counts for `spells_json` (a JSON array of card name strings)
/// against the local human's pool, as [`suggest_lands`] does for the legacy
/// runtime. The payload is parsed before the session is touched.
#[wasm_bindgen]
pub fn local_fifo_suggest_lands(owner: u32, spells_json: &str) -> Result<JsValue, JsValue> {
    local_fifo_suggest_lands_inner(LocalOwner::from_raw(owner), spells_json)
        .map(|lands| to_js(&lands))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_suggest_lands`].
fn local_fifo_suggest_lands_inner(
    owner: LocalOwner,
    spells_json: &str,
) -> Result<std::collections::HashMap<String, u8>, String> {
    let spell_names: Vec<String> =
        serde_json::from_str(spells_json).map_err(|e| format!("Failed to parse spells: {e}"))?;
    with_local_fifo_inner(owner, |session| {
        Ok(human_land_suggestion(session, &spell_names))
    })
}

/// A local bot seat's auto-built deck for match play, as [`get_bot_deck`]
/// builds one for the legacy runtime.
///
/// `bot_seat`: a bot seat's index, 1 or more.
#[wasm_bindgen]
pub fn local_fifo_bot_deck(owner: u32, bot_seat: u8) -> Result<JsValue, JsValue> {
    local_fifo_bot_deck_inner(LocalOwner::from_raw(owner), bot_seat)
        .map(|deck| to_js(&deck))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_bot_deck`].
fn local_fifo_bot_deck_inner(
    owner: LocalOwner,
    bot_seat: u8,
) -> Result<suggest::SuggestedDeck, String> {
    with_local_fifo_inner(owner, |session| bot_deck_for_seat(session, bot_seat))
}

/// The local draft's original cube multiset for the game launched after it,
/// as [`booster_pack_pool_for_game`] returns for the legacy runtime: `null`
/// for a set draft.
#[wasm_bindgen]
pub fn local_fifo_booster_pack_pool_for_game(owner: u32) -> Result<JsValue, JsValue> {
    local_fifo_booster_pack_pool_for_game_inner(LocalOwner::from_raw(owner))
        .map(|pool| to_js(&pool))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_booster_pack_pool_for_game`].
fn local_fifo_booster_pack_pool_for_game_inner(
    owner: LocalOwner,
) -> Result<Option<Vec<String>>, String> {
    with_local_fifo_inner(owner, |session| {
        Ok(session.booster_pack_pool_for_game().map(<[String]>::to_vec))
    })
}

/// Serialize the local draft's full session to JSON for local persistence.
///
/// The trusted local export, like [`export_draft_session`]: it is the whole
/// session, every seat's pool and every unopened pack included, so it is for
/// persistence and never for display. It carries no owner;
/// [`import_local_fifo_session`] issues a new one.
#[wasm_bindgen]
pub fn export_local_fifo_session(owner: u32) -> Result<String, JsValue> {
    export_local_fifo_session_inner(LocalOwner::from_raw(owner))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`export_local_fifo_session`].
fn export_local_fifo_session_inner(owner: LocalOwner) -> Result<String, String> {
    with_local_fifo_inner(owner, |session| {
        serde_json::to_string(session)
            .map_err(|e| format!("Failed to serialize draft session: {e}"))
    })
}

/// Restore a local draft from [`export_local_fifo_session`]'s JSON, and issue
/// it a new owner.
///
/// The text is deserialized, admitted by the local admission, and validated as
/// a snapshot, all before anything is written; on any failure nothing is
/// installed and the current draft keeps its owner. The session id and its
/// prompts are kept. The owner is re-issued on every import, so every earlier
/// owner is refused afterwards. The bots' random state resumes from the
/// session's seed offset by its revision, so each save point resumes its own
/// stream.
///
/// `difficulty`: 0=VeryEasy, 1=Easy, 2=Medium, 3=Hard, 4=VeryHard.
/// Returns `{ owner, view }`.
#[wasm_bindgen]
pub fn import_local_fifo_session(json: &str, difficulty: u8) -> Result<JsValue, JsValue> {
    import_local_fifo_session_inner(json, difficulty)
        .map(|local| to_js(&local))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`import_local_fifo_session`].
fn import_local_fifo_session_inner(json: &str, difficulty: u8) -> Result<LocalFifoSession, String> {
    let session: DraftSession = serde_json::from_str(json)
        .map_err(|e| format!("Failed to deserialize draft session: {e}"))?;
    admit_local_fifo_runtime(&session)?;
    session
        .validate_persisted_snapshot()
        .map_err(|e| format!("Invalid draft snapshot: {e}"))?;
    // The revision moves on every committed application, so the seed offset
    // differs between save points of one draft.
    let revision = session
        .fifo
        .as_ref()
        .ok_or_else(|| NOT_A_LOCAL_FIFO_SESSION.to_string())?
        .revision;
    let rng = ChaCha20Rng::seed_from_u64(session.config.rng_seed.wrapping_add(revision));
    let view = filter_for_player(&session, LOCAL_HUMAN_SEAT);

    // The import runs no bot and keeps no LLM drafting: the caller declares
    // it with `local_fifo_set_llm_drafting`, which also runs any bot the saved
    // session left waiting.
    let owner = install_local_fifo(
        session,
        map_difficulty(difficulty),
        rng,
        LocalLlmDrafting::EngineBots,
    );
    Ok(LocalFifoSession { owner, view })
}

// ── LLM seats on the local runtime ───────────────────────────────────────────
//
// Under `LocalLlmDrafting::LlmSeats` the bot batch leaves each LLM seat's
// choice head for that seat's own drafter. The caller builds the requests for
// those heads in one call, runs each provider call with no engine call open,
// and hands each reply back in a later call, where the engine binds it to the
// seat's live head or falls back to the engine bot for that seat alone.

/// One held head's LLM request, or why none could be built for it. Every held
/// head is listed, so no head waits on a request that was never sent.
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum LocalLlmPickRequest {
    Request {
        seat: u8,
        /// The prompt the reply must answer; the resolution re-checks it live.
        prompt_id: DraftPromptId,
        fingerprint: String,
        option_count: usize,
        required_pick_count: usize,
        /// Size of the prompt this request carries, for measuring prompt growth.
        prompt_chars: usize,
        request: phase_llm::HttpRequestSpec,
    },
    Unbuildable {
        seat: u8,
        prompt_id: DraftPromptId,
        error: String,
    },
}

/// One LLM seat's reply, bound to the prompt it answers. It names no seat: the
/// seat is the prompt's, so a payload cannot name one seat and answer another
/// seat's prompt.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalLlmPickResolution {
    prompt_id: DraftPromptId,
    /// `None` when no reply came back: a timeout, a transport failure, or a
    /// request that could not be built.
    reply: Option<LocalLlmReply>,
}

/// A provider's reply as the transport received it, with the fingerprint of
/// the pack the request was built over.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalLlmReply {
    fingerprint: String,
    provider: String,
    /// HTTP status of the provider response, so a non-2xx reply is refused
    /// whatever its body looks like.
    status: u16,
    body: String,
}

/// The refusal for a resolution whose seat holds no head an LLM drafter is
/// answering: not an LLM-eligible seat, or a seat with no live prompt.
const NOT_AN_LLM_HEAD: &str = "This seat holds no head its LLM drafter is answering";

/// The outcome error when no reply came back for a held head.
const NO_LLM_REPLY: &str = "the LLM drafter returned no reply";

/// Declare whether the local runtime's bot seats are LLM drafters at `now_ms`,
/// then run the bots the declaration leaves to the engine. Returns seat 0's
/// view.
///
/// `drafting_json`: `"EngineBots"` or `"LlmSeats"`, parsed before the session
/// is touched. Only the live owner declares it. An import installs
/// `"EngineBots"`; a Quick constructor installs the drafting it is given.
#[wasm_bindgen]
pub fn local_fifo_set_llm_drafting(
    owner: u32,
    drafting_json: &str,
    now_ms: u64,
) -> Result<JsValue, JsValue> {
    local_fifo_set_llm_drafting_inner(LocalOwner::from_raw(owner), drafting_json, now_ms)
        .map(|view| to_js(&view))
        .map_err(|error| JsValue::from_str(&error))
}

/// Parse a client's LLM drafting declaration: `"EngineBots"` or `"LlmSeats"`.
/// The one parser of every export that takes a drafting.
fn parse_local_llm_drafting(drafting_json: &str) -> Result<LocalLlmDrafting, String> {
    serde_json::from_str(drafting_json).map_err(|e| format!("Invalid LLM drafting: {e}"))
}

/// Pure-Rust core for [`local_fifo_set_llm_drafting`].
fn local_fifo_set_llm_drafting_inner(
    owner: LocalOwner,
    drafting_json: &str,
    now_ms: u64,
) -> Result<DraftPlayerView, String> {
    let drafting = parse_local_llm_drafting(drafting_json)?;
    with_local_fifo_mut_inner(owner, |session| {
        LOCAL_LLM_DRAFTING.with(|cell| cell.set(drafting));
        run_local_fifo_bot_batch(session, now_ms)?;
        Ok(filter_for_player(session, LOCAL_HUMAN_SEAT))
    })
}

/// Build one LLM pick request for each head an LLM seat's drafter is
/// answering. Read-only.
///
/// Takes NO seat list, for the same reason [`build_llm_draft_pick_requests`]
/// takes none: which seats an LLM may draft for is
/// [`llm_eligible_bot_seats`]'s question, and which heads wait for a reply is
/// [`llm_holds_head`]'s. CR 905.1c: each prompt is built from that seat's own
/// projection. Every held head is listed: an endpoint that cannot carry a
/// request makes each entry `unbuildable` with the reason, never a refused
/// call.
///
/// `set_names_json` is an optional set-code -> name map, as for
/// [`build_llm_draft_pick_requests`].
#[wasm_bindgen]
pub fn local_fifo_build_llm_requests(
    owner: u32,
    endpoint_json: &str,
    set_names_json: &str,
) -> Result<JsValue, JsValue> {
    local_fifo_build_llm_requests_inner(LocalOwner::from_raw(owner), endpoint_json, set_names_json)
        .map(|requests| to_js(&requests))
        .map_err(|error| JsValue::from_str(&error))
}

/// Pure-Rust core for [`local_fifo_build_llm_requests`].
fn local_fifo_build_llm_requests_inner(
    owner: LocalOwner,
    endpoint_json: &str,
    set_names_json: &str,
) -> Result<Vec<LocalLlmPickRequest>, String> {
    // An endpoint that does not parse is every entry's reason, never the
    // call's: the heads are held either way, and each must be answered.
    let endpoint: Result<phase_llm::LlmEndpointConfig, String> =
        serde_json::from_str(endpoint_json)
            .map_err(|e| format!("Invalid LLM endpoint config: {e}"));
    // A missing or unparsable name map degrades the brief to set codes.
    let set_names: std::collections::BTreeMap<String, String> =
        serde_json::from_str(set_names_json).unwrap_or_default();
    let set_names = phase_llm::draft_decision::set_names_from_pairs(set_names);
    let difficulty = DIFFICULTY.with(|cell| cell.get());
    let drafting = LOCAL_LLM_DRAFTING.with(Cell::get);

    with_local_fifo_inner(owner, |session| {
        let requests = CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();
            llm_eligible_bot_seats(session)
                .into_iter()
                .filter_map(|seat| {
                    let view = filter_for_player(session, seat);
                    let prompt_id = view.fifo.as_ref()?.prompt_id.clone()?;
                    if !llm_holds_head(session, seat, &view, drafting) {
                        return None;
                    }
                    let built = phase_llm::build_draft_pick_prompt(
                        seat, &view, difficulty, card_db, &set_names,
                    )
                    .map_err(|e| e.to_string())
                    .and_then(|request| {
                        let endpoint = endpoint.as_ref().map_err(Clone::clone)?;
                        let http = phase_llm::build_chat_request(endpoint, &request.prompt)
                            .map_err(|e| e.to_string())?;
                        Ok((request, http))
                    });
                    Some(match built {
                        Ok((request, http)) => LocalLlmPickRequest::Request {
                            seat,
                            prompt_id,
                            fingerprint: request.fingerprint,
                            option_count: request.option_count,
                            required_pick_count: request.required_pick_count,
                            prompt_chars: request.prompt.char_count(),
                            request: http,
                        },
                        Err(error) => LocalLlmPickRequest::Unbuildable {
                            seat,
                            prompt_id,
                            error,
                        },
                    })
                })
                .collect()
        });
        Ok(requests)
    })
}

/// Resolve one LLM seat's head with its reply at `now_ms`, then run the bots
/// that resolution releases.
///
/// `resolution_json`: `{ promptId, reply }`, with the prompt id the request
/// carried and `reply` the provider's `{ fingerprint, provider, status, body }`,
/// or `null` when no reply came back. It names no seat: the seat is the
/// prompt's, and a seat that is not an LLM seat holding a live prompt is
/// refused with no change. The engine alone decides whether the reply is used:
/// only on the seat's live prompt, against the pack the request was built
/// over, when it decodes to a selection the engine accepts. Otherwise the
/// engine bot drafts that seat's live head. Returns `{ view, llmOutcome }`:
/// seat 0's view and what happened to the reply.
#[wasm_bindgen]
pub fn local_fifo_resolve_llm_pick(
    owner: u32,
    resolution_json: &str,
    now_ms: u64,
) -> Result<JsValue, JsValue> {
    let (view, outcome) =
        local_fifo_resolve_llm_pick_inner(LocalOwner::from_raw(owner), resolution_json, now_ms)
            .map_err(|error| JsValue::from_str(&error))?;
    Ok(to_js(
        &serde_json::json!({ "view": view, "llmOutcome": outcome }),
    ))
}

/// Pure-Rust core for [`local_fifo_resolve_llm_pick`].
///
/// CR 905.2a: there is no priority in a draft, so one seat's choice never
/// waits on another seat's; this resolves its seat alone and then runs only
/// the bots it releases. CR 905.1c: the reply is bound to that seat's own
/// projection. Each application commits whole through `session::apply_at` or
/// not at all, and the fallback's random draw is kept only when it commits.
fn local_fifo_resolve_llm_pick_inner(
    owner: LocalOwner,
    resolution_json: &str,
    now_ms: u64,
) -> Result<(DraftPlayerView, LlmDraftOutcome), String> {
    let resolution: LocalLlmPickResolution = serde_json::from_str(resolution_json)
        .map_err(|e| format!("Invalid LLM pick resolution: {e}"))?;
    with_local_fifo_mut_inner(owner, |session| {
        let seat = resolution.prompt_id.seat;
        if !llm_eligible_bot_seats(session).contains(&seat) {
            return Err(NOT_AN_LLM_HEAD.to_string());
        }
        let view = filter_for_player(session, seat);
        let Some(live) = view.fifo.as_ref().and_then(|fifo| fifo.prompt_id.clone()) else {
            return Err(NOT_AN_LLM_HEAD.to_string());
        };
        let usage = resolution.reply.as_ref().and_then(|reply| {
            phase_llm::token_usage(
                phase_llm::LlmProvider::from_label(&reply.provider),
                &reply.body,
            )
        });
        let difficulty = DIFFICULTY.with(|cell| cell.get());
        let enhancement = PICK_ENHANCEMENT.with(|cell| cell.get());

        let (used, reasoning, error) = CARD_DB.with(|cell| {
            let db_borrow = cell.borrow();
            let card_db = db_borrow.as_ref();
            let (used, reasoning, error) =
                match local_llm_selection(&view, &live, &resolution, (card_db, difficulty)) {
                    Ok((action, reasoning)) => {
                        match session::apply_at(session, action, None, now_ms) {
                            Ok(_) => (true, reasoning, None),
                            Err(refusal) => (
                                false,
                                None,
                                Some(format!("LLM selection refused: {refusal}")),
                            ),
                        }
                    }
                    Err(error) => (false, None, Some(error)),
                };
            if !used {
                let stored = RNG
                    .with(|cell| cell.take())
                    .ok_or_else(|| "RNG not initialized".to_string())?;
                let mut trial = stored.clone();
                let applied = fifo_ai_selection(
                    &view,
                    seat,
                    live.clone(),
                    difficulty,
                    card_db,
                    enhancement,
                    &mut trial,
                )
                .ok_or_else(|| format!("no FIFO selection for bot seat {seat}"))
                .and_then(|action| {
                    session::apply_at(session, action, None, now_ms)
                        .map_err(|e| format!("bot seat {seat} selection refused: {e}"))
                });
                let kept = if applied.is_ok() { trial } else { stored };
                RNG.with(|cell| cell.set(Some(kept)));
                applied?;
            }
            Ok::<_, String>((used, reasoning, error))
        })?;

        run_local_fifo_bot_batch(session, now_ms)?;
        Ok((
            filter_for_player(session, LOCAL_HUMAN_SEAT),
            LlmDraftOutcome {
                seat,
                used,
                reasoning,
                error,
                usage,
            },
        ))
    })
}

/// The selection an LLM seat's reply makes on that seat's live head, with the
/// model's stated reason, or why it makes none: no reply, a reply for a prompt
/// that is not live, or a reply that does not bind to the pack the request was
/// built over. The burns follow the engine's published requirements by the
/// same rule as the engine bot's ([`fifo_burns`]), and an LLM selection spends
/// no draft-effect card.
fn local_llm_selection(
    view: &DraftPlayerView,
    live: &DraftPromptId,
    resolution: &LocalLlmPickResolution,
    render: (Option<&CardDatabase>, AiDifficulty),
) -> Result<(DraftAction, Option<String>), String> {
    let stale = || phase_llm::LlmError::StaleDecision.to_string();
    let reply = resolution
        .reply
        .as_ref()
        .ok_or_else(|| NO_LLM_REPLY.to_string())?;
    if resolution.prompt_id != *live {
        return Err(stale());
    }
    let requirements = view.fifo.as_ref().ok_or_else(stale)?.requirements;
    let (pack, required) = match phase_llm::draft_decision::draft_pick_step(view) {
        phase_llm::draft_decision::DraftPickStep::Choice { pack, required } => (pack, required),
        phase_llm::draft_decision::DraftPickStep::NoPack
        | phase_llm::draft_decision::DraftPickStep::Forced => return Err(stale()),
    };
    let selection = phase_llm::select_picks_from_response(
        live.seat,
        pack,
        required,
        &reply.fingerprint,
        render,
        phase_llm::LlmReply {
            provider: phase_llm::LlmProvider::from_label(&reply.provider),
            status: reply.status,
            body: &reply.body,
        },
    )
    .map_err(|e| e.to_string())?;
    let burned_card_instance_ids =
        fifo_burns(pack, &selection.card_instance_ids, requirements.burns);
    Ok((
        DraftAction::Select {
            seat: live.seat,
            prompt_id: live.clone(),
            picked_card_instance_ids: selection.card_instance_ids,
            burned_card_instance_ids,
            effect_card_instance_ids: Vec::new(),
        },
        selection.reasoning,
    ))
}

#[cfg(test)]
mod shared_stack_bot_loop_tests {
    use super::*;
    use draft_core::pack_source::FixturePackSource;
    use engine::types::player::PlayerId;

    const CARDS_PER_PACK: u8 = 15;

    /// A pod of `pod_size` seats where `bot_seats` are bots, started and dealt.
    ///
    /// `FixturePackSource`'s cards carry no colours and no database face, which
    /// is deliberate here: these tests are about the LOOP -- its span, its
    /// bound and its failure mode -- and the valuation is exercised by
    /// `bot_ai`'s own tests against a real card database.
    fn started_pod(pod_size: u8, bot_seats: &[u8], rng_seed: u64) -> DraftSession {
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Winston,
            pod_size,
            cards_per_pack: CARDS_PER_PACK,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..pod_size)
            .map(|i| {
                if bot_seats.contains(&i) {
                    DraftSeat::Bot {
                        name: format!("Bot {i}"),
                    }
                } else {
                    DraftSeat::Human {
                        player_id: PlayerId(i),
                        display_name: format!("Player {i}"),
                    }
                }
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "WIN-LOOP".to_string());
        session::apply(
            &mut session,
            DraftAction::StartDraft,
            Some(&FixturePackSource {
                set_code: "TST".to_string(),
                cards_per_pack: CARDS_PER_PACK,
            }),
        )
        .expect("a Winston pod with bot seats starts");
        session
    }

    fn active_seat(session: &DraftSession) -> u8 {
        session
            .shared_stack
            .as_ref()
            .expect("a live pile turn")
            .active_seat
    }

    /// Hand the human seat's turn back to the bots, so the fixture is the shape
    /// production is in when the host calls the driver: a human decision has
    /// just been applied and the next seat is a bot.
    fn end_the_humans_turn(session: &mut DraftSession) {
        let seat = active_seat(session);
        let pile = session
            .shared_stack
            .as_ref()
            .expect("a live pile turn")
            .cursor;
        session::apply(
            session,
            DraftAction::SharedStackDecision {
                seat,
                pile,
                decision: SharedStackPileDecision::Take,
            },
            None,
        )
        .expect("taking a dealt pile is legal at turn start");
    }

    fn decisions_in(deltas: &[DraftDelta]) -> Vec<u8> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                DraftDelta::SharedStackDecisionApplied { seat, .. } => Some(*seat),
                _ => None,
            })
            .collect()
    }

    /// One call spans EVERY consecutive bot turn, not one decision and not one
    /// turn: a 4-seat pod with three bots hands the driver several turns back to
    /// back, and a one-decision-per-call driver would strand the pod.
    ///
    /// Driven through `resolve_shared_stack_bot_turns_inner`, the production
    /// core, so the thread-local read and the bound derivation are covered too.
    #[test]
    fn the_loop_spans_consecutive_bot_turns_and_terminates() {
        let mut session = started_pod(4, &[1, 2, 3], 20_260_913);
        if active_seat(&session) == 0 {
            end_the_humans_turn(&mut session);
        }
        assert_ne!(active_seat(&session), 0, "the fixture must start on a bot");

        session_cell::install(session);
        let deltas = resolve_shared_stack_bot_turns_inner().expect("the loop runs");
        let seats = decisions_in(&deltas);

        assert!(
            seats.len() > 3,
            "one call must cross a turn boundary; it applied {} decisions",
            seats.len()
        );
        let mut distinct: Vec<u8> = seats.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() >= 2,
            "the run must span more than one seat, got {distinct:?}"
        );
        assert!(
            !seats.contains(&0),
            "the driver must never act for the human seat"
        );

        // It STOPPED, and stopped for the right reason: the human's turn.
        let stopped_on_human = session_cell::with_installed(|session| {
            session.status != DraftStatus::Drafting || active_seat(session) == 0
        });
        assert!(stopped_on_human);

        // A second call from the same state produces nothing at all.
        let again = resolve_shared_stack_bot_turns_inner().expect("a second call is a no-op");
        assert!(decisions_in(&again).is_empty());

        session_cell::clear();
    }

    /// The bound trip is an `Err`, never a `break` and never a hang. A silent
    /// `break` would strand the pod mid-run with no error anywhere.
    #[test]
    fn the_loop_fails_loudly_rather_than_spinning() {
        let mut session = started_pod(4, &[1, 2, 3], 20_260_913);
        if active_seat(&session) == 0 {
            end_the_humans_turn(&mut session);
        }

        // The bound is DERIVED: `U * (n + 1) + 1` over the undrafted cards.
        let state = session.shared_stack.as_ref().expect("a live pile turn");
        let undrafted = state.main_stack.len() + state.piles.iter().map(Vec::len).sum::<usize>();
        let piles = state.piles.len();
        assert_eq!(
            shared_stack_bot_turn_bound(&session),
            undrafted * (piles + 1) + 1
        );

        let mut starved = session.clone();
        let error = drive_shared_stack_bot_turns(&mut starved, AiDifficulty::Medium, None, 1)
            .expect_err("a bound of 1 must trip on a multi-decision run");
        assert!(
            error.contains("termination bound"),
            "the error must name the bound: {error}"
        );

        // Paired positive: the SAME state runs clean under the derived bound,
        // so the `Err` above is the bound tripping and not a broken fixture.
        let mut healthy = session;
        let bound = shared_stack_bot_turn_bound(&healthy);
        let deltas = drive_shared_stack_bot_turns(&mut healthy, AiDifficulty::Medium, None, bound)
            .expect("the derived bound is not reachable from a legal state");
        assert!(decisions_in(&deltas).len() > 1);
    }
}

#[cfg(test)]
mod llm_draft_resolution_tests {
    use super::*;
    use draft_core::pack_source::FixturePackSource;
    use engine::types::player::PlayerId;

    /// A Quick pod (seat 0 human, seats 1..8 bots), started and dealt.
    fn started_quick_pod() -> DraftSession {
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Quick,
            pod_size: 8,
            cards_per_pack: 15,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 20_261_004,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..8)
            .map(|i| {
                if i == 0 {
                    DraftSeat::Human {
                        player_id: PlayerId(0),
                        display_name: "Player".to_string(),
                    }
                } else {
                    DraftSeat::Bot {
                        name: format!("Bot {i}"),
                    }
                }
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "LLM-RES".to_string());
        session::apply(
            &mut session,
            DraftAction::StartDraft,
            Some(&FixturePackSource {
                set_code: "TST".to_string(),
                cards_per_pack: 15,
            }),
        )
        .expect("a Quick pod starts");
        session
    }

    /// A well-formed OpenAI-compatible reply choosing option `choice`.
    fn reply_choosing(choice: usize) -> String {
        let content = format!(r#"{{"choice": {choice}, "reason": "test"}}"#);
        serde_json::json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
            .to_string()
    }

    /// The request side's own recipe: the seat's projected view, rendered into a
    /// prompt whose fingerprint the reply will carry back.
    fn issued_fingerprint(session: &DraftSession, seat: u8) -> String {
        let view = filter_for_player(session, seat);
        phase_llm::build_draft_pick_prompt(
            seat,
            &view,
            AiDifficulty::Medium,
            None,
            &phase_llm::draft_decision::set_names_from_pairs(Vec::new()),
        )
        .expect("a dealt pack renders")
        .fingerprint
    }

    fn response(session: &DraftSession, seat: u8, choice: usize) -> LlmDraftResponse {
        LlmDraftResponse {
            seat,
            fingerprint: issued_fingerprint(session, seat),
            provider: "OpenAiCompatible".to_string(),
            status: 200,
            body: reply_choosing(choice),
        }
    }

    /// A set draft presents each pack by rarity, so the order the model reads
    /// its options in is not the order the session stores the pack in. A reply
    /// to a request this bridge issued must resolve against the order the
    /// prompt used: refused as stale, or bound to a different card, would mean
    /// the LLM never drafts for a set pod at all.
    #[test]
    fn a_reply_resolves_against_the_pack_in_the_order_the_prompt_presented() {
        let mut session = started_quick_pod();
        // Store seat 1's pack in the reverse of its presented (rarity) order.
        session.current_pack[1]
            .as_mut()
            .expect("seat 1 holds a pack")
            .0
            .reverse();
        let view_pack = filter_for_player(&session, 1)
            .current_pack
            .expect("seat 1 is shown its pack");
        let raw_pack = session.current_pack[1].as_ref().unwrap().0.clone();
        assert_ne!(
            view_pack.iter().map(|c| &c.instance_id).collect::<Vec<_>>(),
            raw_pack.iter().map(|c| &c.instance_id).collect::<Vec<_>>(),
            "fixture must make the presented order differ from the stored order"
        );

        let selection = resolve_llm_draft_pick(&session, &response(&session, 1, 0))
            .expect("a reply to an issued request is not stale");

        assert_eq!(
            selection.card_instance_ids,
            vec![view_pack[0].instance_id.clone()],
            "option 0 is the first card the model was shown"
        );
    }

    #[test]
    fn a_reply_for_a_pack_that_has_moved_on_is_refused_as_stale() {
        let mut session = started_quick_pod();
        let stale = response(&session, 1, 0);
        // The seat's pack changes under the in-flight request.
        session.current_pack[1].as_mut().unwrap().0.pop();

        assert!(matches!(
            resolve_llm_draft_pick(&session, &stale),
            Err(phase_llm::LlmError::StaleDecision)
        ));
    }

    #[test]
    fn an_emptied_pack_is_refused_rather_than_resolved() {
        let mut session = started_quick_pod();
        let reply = response(&session, 1, 0);
        session.current_pack[1].as_mut().unwrap().0.clear();

        assert!(matches!(
            resolve_llm_draft_pick(&session, &reply),
            Err(phase_llm::LlmError::StaleDecision)
        ));
    }

    /// CR 905.1a: the last card of a pack is not a choice. No seat holding one
    /// is put to a provider, and the bot path still takes and records it.
    #[test]
    fn a_forced_last_pick_builds_no_request_and_is_still_recorded() {
        let mut session = started_quick_pod();
        for pack in session.current_pack.iter_mut().flatten() {
            pack.0.truncate(1);
        }
        let human_card = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        let pools_before: Vec<usize> = session.pools.iter().map(Vec::len).collect();

        session_cell::install(session.clone());
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        let requests = build_llm_draft_pick_requests_inner(
            r#"{"provider":"OpenAiCompatible","baseUrl":"https://provider.test/v1","model":"test"}"#,
            "{}",
        )
        .unwrap();
        session_cell::clear();
        assert!(requests.is_empty(), "a forced pick was put to a provider");

        RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(42))));
        apply_human_pick_and_resolve_bots(&mut session, human_card).unwrap();
        for (seat, before) in pools_before.iter().enumerate() {
            assert_eq!(session.pools[seat].len(), before + 1, "seat {seat}");
        }
    }

    /// The production response-to-pick path for Jev: the reply is read against
    /// the criterion names the REQUEST carried for this seat's pack, so a valid
    /// option number with a label the pack never offered is not that card.
    #[test]
    fn a_jev_reply_resolves_only_through_the_criteria_the_request_issued() {
        let session = started_quick_pod();
        session_cell::install(session.clone());
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        let requests = build_llm_draft_pick_requests_inner(
            r#"{"provider":"Jev","baseUrl":"https://relay.test","apiKey":"k","model":"jev-latest"}"#,
            "{}",
        )
        .unwrap();
        session_cell::clear();
        let issued = requests.iter().find(|request| request.seat == 1).unwrap();
        let envelope: serde_json::Value = serde_json::from_str(&issued.request.body).unwrap();
        let keys: Vec<String> = envelope["request"]["questions"]["pick"]["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys.len(), issued.option_count);

        let reply = |choice: &str, probabilities: serde_json::Value| LlmDraftResponse {
            seat: 1,
            fingerprint: issued.fingerprint.clone(),
            provider: "Jev".to_string(),
            status: 200,
            body: serde_json::json!({
                "answers": { "pick": {
                    "type": "choice",
                    "choice": choice,
                    "probabilities": probabilities,
                    "confidence": 0.9,
                }},
            })
            .to_string(),
        };
        let projected = filter_for_player(&session, 1).current_pack.unwrap();

        let good = reply(
            &keys[1],
            serde_json::json!({ &keys[0]: 0.1, &keys[1]: 0.9 }),
        );
        let selection = resolve_llm_draft_pick(&session, &good).unwrap();
        assert_eq!(
            selection.card_instance_ids,
            vec![projected[1].instance_id.clone()]
        );

        let unissued_label = "01: A card that is not in this pack";
        let wrong_choice = reply(
            unissued_label,
            serde_json::json!({ &keys[0]: 0.1, unissued_label: 0.9 }),
        );
        let wrong_probability = reply(
            &keys[1],
            serde_json::json!({ &keys[1]: 0.8, "00: Invented": 0.2 }),
        );
        for bad in [wrong_choice, wrong_probability] {
            assert!(matches!(
                resolve_llm_draft_pick(&session, &bad),
                Err(phase_llm::LlmError::MalformedResponse { .. })
            ));
        }
    }

    #[test]
    fn a_reply_naming_the_human_seat_is_refused() {
        let session = started_quick_pod();
        assert!(matches!(
            resolve_llm_draft_pick(&session, &response(&session, 0, 0)),
            Err(phase_llm::LlmError::StaleDecision)
        ));
    }

    #[test]
    fn issued_rarity_ordered_request_selects_the_specific_bot_pool_card() {
        let mut start = started_quick_pod();
        start.current_pack[1].as_mut().unwrap().0.reverse();
        let projected = filter_for_player(&start, 1).current_pack.unwrap();
        let stored = &start.current_pack[1].as_ref().unwrap().0;
        assert_ne!(projected[0].instance_id, stored[0].instance_id);
        let human_id = start.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(42))));
        let mut ordinary = start.clone();
        apply_human_pick_and_resolve_bots(&mut ordinary, human_id.clone()).unwrap();
        let ordinary_id = ordinary.pools[1][0].instance_id.clone();
        let (choice, chosen) = projected
            .iter()
            .enumerate()
            .find(|(_, card)| card.instance_id != ordinary_id)
            .expect("the displayed pack offers a non-heuristic option");

        session_cell::install(start);
        RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(42))));
        let requests = build_llm_draft_pick_requests_inner(
            r#"{"provider":"OpenAiCompatible","baseUrl":"https://provider.test/v1","model":"test"}"#,
            "{}",
        )
        .unwrap();
        assert_eq!(requests.len(), 7);
        let issued = requests.iter().find(|request| request.seat == 1).unwrap();
        assert_eq!(issued.option_count, projected.len());
        assert_eq!(issued.required_pick_count, 1);
        assert!(issued
            .request
            .body
            .contains(&format!("[{choice}] {}", chosen.name)));
        let wire = serde_json::to_value(issued).unwrap();
        assert_eq!(wire["seat"].as_u64(), Some(1));
        assert_eq!(
            wire["fingerprint"].as_str(),
            Some(issued.fingerprint.as_str())
        );
        assert_eq!(wire["optionCount"].as_u64(), Some(projected.len() as u64));
        assert_eq!(wire["requiredPickCount"].as_u64(), Some(1));
        assert_eq!(wire["request"]["method"].as_str(), Some("POST"));
        let responses = serde_json::json!([{
            "seat": 1,
            "fingerprint": issued.fingerprint,
            "provider": "OpenAiCompatible",
            "status": 200,
            "body": reply_choosing(choice),
        }]);
        let (view, outcomes) =
            submit_pick_with_llm_bot_picks_inner(&human_id, &responses.to_string())
                .expect("the issued response submits through the installed draft");
        assert!(outcomes[0].used);
        assert!(view.pool.iter().any(|card| card.instance_id == human_id));
        session_cell::with_installed(|session| {
            assert_eq!(session.pools[1][0].instance_id, chosen.instance_id);
            assert_ne!(session.pools[1][0].instance_id, ordinary_id);
            assert!(session.pools[0]
                .iter()
                .any(|card| card.instance_id == human_id));
        });
        session_cell::clear();
    }

    #[test]
    fn submit_core_refuses_human_and_stale_responses_while_valid_bot_and_fallback_advance() {
        let start = started_quick_pod();
        let human_id = start.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        session_cell::install(start);
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(43))));
        let requests = build_llm_draft_pick_requests_inner(
            r#"{"provider":"OpenAiCompatible","baseUrl":"https://provider.test/v1","model":"test"}"#,
            "{}",
        )
        .unwrap();
        assert!(requests.iter().all(|request| request.seat != 0));
        let stale = requests.iter().find(|request| request.seat == 1).unwrap();
        let valid = requests.iter().find(|request| request.seat == 2).unwrap();
        session_cell::with_installed_mut(|session| {
            session.current_pack[1].as_mut().unwrap().0.pop();
        });
        let responses = serde_json::json!([
            { "seat": 0, "fingerprint": "forged", "provider": "OpenAiCompatible", "status": 200, "body": reply_choosing(0) },
            { "seat": 1, "fingerprint": stale.fingerprint, "provider": "OpenAiCompatible", "status": 200, "body": reply_choosing(0) },
            { "seat": 2, "fingerprint": valid.fingerprint, "provider": "OpenAiCompatible", "status": 200, "body": reply_choosing(0) },
        ]);
        let (view, outcomes) =
            submit_pick_with_llm_bot_picks_inner(&human_id, &responses.to_string())
                .expect("invalid LLM responses fall back without blocking the human pick");
        assert_eq!(
            outcomes
                .iter()
                .map(|outcome| outcome.used)
                .collect::<Vec<_>>(),
            vec![false, false, true]
        );
        assert_eq!(view.pool.len(), 1);
        session_cell::with_installed(|session| {
            assert_eq!(session.pools[0].len(), 1);
            assert_eq!(session.pools[0][0].instance_id, human_id);
            assert_eq!(session.pools[1].len(), 1);
            assert_eq!(session.pools[2].len(), 1);
        });
        session_cell::clear();
    }

    struct MixedBoosterPackSource;

    impl draft_core::pack_source::PackSource for MixedBoosterPackSource {
        fn generate_pack(
            &self,
            _rng: &mut dyn rand::RngCore,
            seat: u8,
            pack_number: u8,
        ) -> draft_core::types::DraftPack {
            let size = match pack_number {
                0 => 2,
                1 => 3,
                _ => 2,
            };
            let cards = (0..size)
                .map(|i| DraftCardInstance {
                    instance_id: format!("card-{seat}-{pack_number}-{i}"),
                    name: format!("Card {i}"),
                    set_code: "TST".to_string(),
                    collector_number: format!("{i}"),
                    rarity: "common".to_string(),
                    colors: Vec::new(),
                    cmc: 1,
                    type_line: String::new(),
                    draft_effect: None,
                })
                .collect();
            draft_core::types::DraftPack(cards)
        }
    }

    #[test]
    fn session_with_mixed_booster_sizes_wires_cards_in_pack_through_bot_resolution() {
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Quick,
            pod_size: 8,
            cards_per_pack: 15, // uniform default overridden by pack source
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 20_261_004,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..8)
            .map(|i| {
                if i == 0 {
                    DraftSeat::Human {
                        player_id: PlayerId(0),
                        display_name: "Player".to_string(),
                    }
                } else {
                    DraftSeat::Bot {
                        name: format!("Bot {i}"),
                    }
                }
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "MIXED-SIZES".to_string());
        session::apply(
            &mut session,
            DraftAction::StartDraft,
            Some(&MixedBoosterPackSource),
        )
        .expect("mixed-size Quick pod starts");

        assert_eq!(session.pack_sizes, vec![2, 3, 2]);
        assert_eq!(session.current_pack_number, 0);
        assert_eq!(session.cards_in_pack(0), 2);

        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(42))));

        // The shape every pick site builds, and the pick-in-pack it yields for
        // the pack seat 0 is holding. The uniform `cards_per_pack` (15) would put
        // these picks deep in the late stage; the recorded sizes must win.
        let shape_and_pick = |session: &DraftSession| {
            let shape = bot_ai::PackShape::current(session);
            let remaining = session.current_pack[0].as_ref().unwrap().0.len();
            (shape, shape.position(remaining))
        };
        let expect = |pack_number: u8, cards_in_pack: u8, pick_in_pack: usize| {
            (
                bot_ai::PackShape {
                    pack_number,
                    cards_in_pack,
                },
                phase_ai::draft_profile::PackPosition {
                    pack_number: usize::from(pack_number),
                    pick_in_pack,
                },
            )
        };

        // Pack 0, pick 1 of 2.
        assert_eq!(shape_and_pick(&session), expect(0, 2, 1));
        let human_card_0 = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        apply_human_pick_and_resolve_bots(&mut session, human_card_0).unwrap();
        assert_eq!(session.current_pack_number, 0);
        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 1);
        assert_eq!(shape_and_pick(&session), expect(0, 2, 2));

        // Pack 0, pick 2 of 2. Exhausts pack 0 and transitions to pack 1.
        let human_card_1 = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        apply_human_pick_and_resolve_bots(&mut session, human_card_1).unwrap();

        // Now in pack 1: size is 3, cards_in_pack(1) is 3.
        assert_eq!(session.current_pack_number, 1);
        assert_eq!(session.cards_in_pack(session.current_pack_number), 3);
        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 3);
        assert_eq!(shape_and_pick(&session), expect(1, 3, 1));

        // Pack 1, pick 1 of 3: bots resolve using PackShape with cards_in_pack = 3.
        let human_card_2 = session.current_pack[0].as_ref().unwrap().0[0]
            .instance_id
            .clone();
        apply_human_pick_and_resolve_bots(&mut session, human_card_2).unwrap();

        for seat in 0..8 {
            assert_eq!(
                session.pools[seat].len(),
                3,
                "seat {seat} took 2 cards from pack 0 and 1 from pack 1"
            );
        }
        assert_eq!(session.current_pack[0].as_ref().unwrap().0.len(), 2);
        assert_eq!(shape_and_pick(&session), expect(1, 3, 2));
    }
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

    pub(super) fn selection_json(pools: &[(&str, u8)], sequence: &[&str]) -> String {
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

    pub(super) fn install_fixture_db() {
        let db = CardDatabase::from_json_str(fixture_card_db_json()).unwrap();
        CARD_DB.with(|cell| *cell.borrow_mut() = Some(db));
    }

    pub(super) fn clear_state() {
        session_cell::clear();
        PACK_GEN.with(|cell| cell.set(None));
        RNG.with(|cell| cell.set(None));
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        LOCAL_LLM_DRAFTING.with(|cell| cell.set(LocalLlmDrafting::EngineBots));
    }

    fn persisted_premier_session(layout: SetLayout) -> DraftSession {
        let source = DraftSource::Set { layout };
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
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

    fn colored_spell(name: &str, color: &str) -> DraftCardInstance {
        DraftCardInstance {
            instance_id: format!("{name}-id"),
            name: name.to_string(),
            set_code: "TST".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: vec![color.to_string()],
            cmc: 1,
            type_line: "Creature".to_string(),
            draft_effect: None,
        }
    }

    #[test]
    fn suggest_lands_for_seat_uses_the_requested_seat_pool() {
        clear_state();
        let mut session = persisted_premier_session(SetLayout::UniformByRound {
            codes: vec!["TST".to_string()],
        });
        session.pools[0] = vec![colored_spell("Host Blue", "U")];
        session.pools[1] = vec![colored_spell("Guest Red", "R")];
        session_cell::install(session);

        let lands = suggest_lands_for_seat_inner(1, r#"["Guest Red"]"#)
            .expect("seat-one suggestion should succeed");
        assert_eq!(lands.get("Mountain"), Some(&18));
        assert!(!lands.contains_key("Island"));

        clear_state();
    }

    #[test]
    fn suggest_lands_for_seat_parses_before_accessing_the_session() {
        clear_state();
        let error = suggest_lands_for_seat_inner(1, "not JSON")
            .expect_err("malformed spells must be rejected");
        assert!(
            error.contains("Failed to parse spells"),
            "unexpected error: {error}"
        );
        assert!(
            !error.contains("Draft not initialized"),
            "parse must precede session access"
        );
    }

    #[test]
    fn suggest_lands_for_seat_rejects_a_missing_pool() {
        clear_state();
        let session = persisted_premier_session(SetLayout::UniformByRound {
            codes: vec!["TST".to_string()],
        });
        session_cell::install(session);

        let error =
            suggest_lands_for_seat_inner(8, "[]").expect_err("out-of-range seat must be rejected");
        assert!(
            error.contains("Seat 8 has no pool"),
            "unexpected error: {error}"
        );

        clear_state();
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
        assert!(!session_cell::is_installed());
    }

    /// A started, drafting 2-seat Winston session, built through the REAL
    /// reducer so its `shared_stack` is the shape the reducer produces rather
    /// than one this test invented.
    /// A booster shaped like a real one: five colours, a mana curve, and type
    /// lines. `FixturePackSource` makes colourless, typeless, mana-value-zero
    /// cards, which is right for the reducer's conservation and legality tests
    /// and useless here — a deck suggested from that pool has no colours to
    /// choose between and no curve to build, so a playability claim over it
    /// would assert nothing.
    struct WinstonDeckFixtureSource;

    impl draft_core::pack_source::PackSource for WinstonDeckFixtureSource {
        fn generate_pack(
            &self,
            _rng: &mut dyn rand::RngCore,
            seat: u8,
            pack_number: u8,
        ) -> draft_core::types::DraftPack {
            // Five colours plus COLOURLESS, because every real booster has
            // artifacts and a pool with none of them is a worst case no set
            // produces: with nothing colourless to play, a two-colour build can
            // never reach the suggester's 23-playable target and its documented
            // top-up splashes the whole pool. Roughly one in five here, which is
            // the low end of a real booster's artifact count.
            const COLORS: [Option<&str>; 6] =
                [Some("W"), Some("U"), Some("B"), Some("R"), Some("G"), None];
            let cards = (0..15u8)
                .map(|i| {
                    let color = COLORS[usize::from(i) % COLORS.len()];
                    // A curve, not a flat cost: 1..=5, so the land count the
                    // suggester derives is a real answer rather than a constant.
                    let cmc: u8 = 1 + (i % 5);
                    let creature = i % 3 != 0;
                    DraftCardInstance {
                        instance_id: format!("FIX-{seat}-{pack_number}-{i}"),
                        // The colour is IN THE NAME on purpose: `SuggestedDeck`
                        // carries names only, so this is how the assertions read
                        // a finished deck's colours back.
                        name: format!(
                            "Fixture {} {cmc} {seat}-{pack_number}-{i}",
                            color.unwrap_or("C")
                        ),
                        set_code: "FIX".to_string(),
                        collector_number: format!("{}", i + 1),
                        rarity: if i == 0 { "rare" } else { "common" }.to_string(),
                        colors: color.map(|c| vec![c.to_string()]).unwrap_or_default(),
                        cmc,
                        type_line: match (color, creature) {
                            (None, _) => "Artifact".to_string(),
                            (Some(_), true) => "Creature — Fixture".to_string(),
                            (Some(_), false) => "Instant".to_string(),
                        },
                        draft_effect: None,
                    }
                })
                .collect();
            draft_core::types::DraftPack(cards)
        }
    }

    /// A complete two-BOT Winston draft, driven by the same loop production
    /// uses, returning the finished session.
    fn winston_drafted_by_bots(card_db: Option<&CardDatabase>) -> DraftSession {
        let source = DraftSource::single_set("FIX".to_string());
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            set_code: source.set_code(),
            source,
            kind: DraftKind::Winston,
            pod_size: 2,
            cards_per_pack: 15,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 20_260_913,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        // BOTH seats are bots, so the driver runs the whole draft rather than
        // stopping at the first human turn.
        let seats = (0..2)
            .map(|seat| DraftSeat::Bot {
                name: format!("Bot {seat}"),
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "winston-bots".to_string());
        draft_core::session::apply(
            &mut session,
            draft_core::types::DraftAction::StartDraft,
            Some(&WinstonDeckFixtureSource),
        )
        .expect("a Winston pod starts");
        drive_shared_stack_bot_turns(&mut session, AiDifficulty::Medium, card_db, 10_000)
            .expect("the bot loop drives a whole draft");
        assert_eq!(
            session.status,
            DraftStatus::Deckbuilding,
            "the draft must actually finish before a deck can be judged"
        );
        session
    }

    /// The colour a fixture card's name encodes.
    fn fixture_color(name: &str) -> Option<&'static str> {
        ["W", "U", "B", "R", "G"]
            .into_iter()
            .find(|color| name.starts_with(&format!("Fixture {color} ")))
    }

    /// THE BAR: A BOT MUST END UP WITH A DECK IT COULD ACTUALLY PLAY.
    ///
    /// Not "a bot that drafts like a human" — it is not expected to. This is the
    /// weaker, and the only load-bearing, claim: a bot seat drafts a pool, and
    /// the suggester turns that pool into a legal, castable, sensibly shaped
    /// deck. Everything the draft heuristics do above that is quality of play.
    ///
    /// Run WITHOUT a card database, which is the worst case and now a reachable
    /// one: `resumeDraftingAfterRestore` fails open on a card-data fetch, so a
    /// pod whose fetch failed drafts in exactly this degraded mode. If the deck
    /// is playable here it is playable with the database too.
    ///
    /// DELIBERATELY NOT ASSERTED HERE: that the deck is focused on two colours,
    /// and that the same pool builds the same deck twice. Both hold only once
    /// `find_best_colors` sorts on a total order -- it sorts on score alone, and
    /// `rarity_prior` scores a common at 0.0, so without a card database every
    /// colour holding no rare or uncommon ties at exactly 0.0 and the second
    /// colour falls out of `HashMap` iteration order. That is a defect in the
    /// deckbuilding suggester, which is shared by every draft kind and is not
    /// Winston's to fix; it is carried in its own change. The three claims
    /// below are the ones that hold either way, measured green over 13 runs.
    #[test]
    fn a_bot_that_drafted_a_whole_winston_pod_can_build_a_playable_deck() {
        let session = winston_drafted_by_bots(None);

        for seat in 0..2usize {
            let pool = &session.pools[seat];
            // Reach guard: the draft really dealt this seat a pool to build from.
            assert!(
                pool.len() >= 40,
                "seat {seat} drafted {} cards, which is not a limited pool",
                pool.len()
            );

            let deck = suggest::suggest_deck(
                pool,
                AiDifficulty::Medium,
                None,
                session.config.min_deck_size,
                0,
                &session.config.addable_cards,
            );

            let basics: u32 = deck.lands.values().map(|count| u32::from(*count)).sum();
            let spells = deck.main_deck.len() as u32;

            // (1) LEGAL. CR 100.2b: a limited deck is at least 40 cards, and the
            // engine's own `validate_limited_deck` counts spells plus basics.
            assert!(
                spells + basics >= session.config.min_deck_size as u32,
                "seat {seat}: {spells} spells + {basics} basics is short of 40"
            );

            // (2) CASTABLE. Every colour the chosen spells need is produced by a
            // basic the suggester actually added. A deck with red spells and no
            // Mountains is legal and unplayable, which is the distinction this
            // whole test exists to draw.
            let needed: std::collections::HashSet<&str> = deck
                .main_deck
                .iter()
                .filter_map(|name| fixture_color(name))
                .collect();
            let produced: std::collections::HashSet<&str> = deck
                .lands
                .iter()
                .filter(|(_, count)| **count > 0)
                .filter_map(|(name, _)| match name.as_str() {
                    "Plains" => Some("W"),
                    "Island" => Some("U"),
                    "Swamp" => Some("B"),
                    "Mountain" => Some("R"),
                    "Forest" => Some("G"),
                    _ => None,
                })
                .collect();
            assert!(
                needed.is_subset(&produced),
                "seat {seat}: spells need {needed:?} but the manabase produces {produced:?}"
            );

            // (3) A DECK, not a pile of lands. A 40-card limited deck is about
            // seventeen lands and twenty-three spells; this floor is deliberately
            // well below that, because the claim is playability and not quality.
            assert!(
                spells >= 15,
                "seat {seat}: {spells} spells is a land pile, not a deck"
            );
        }
    }

    fn started_winston_session() -> DraftSession {
        let source = DraftSource::single_set("TST".to_string());
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            set_code: source.set_code(),
            source,
            kind: DraftKind::Winston,
            pod_size: 2,
            cards_per_pack: 15,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats = (0..2)
            .map(|seat| DraftSeat::Human {
                player_id: engine::types::player::PlayerId(seat),
                display_name: format!("Player {seat}"),
            })
            .collect();
        let mut session = DraftSession::new(config, seats, "winston-draft".to_string());
        let pack_source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 15,
        };
        draft_core::session::apply(
            &mut session,
            draft_core::types::DraftAction::StartDraft,
            Some(&pack_source),
        )
        .expect("a human-only Winston pod starts");
        session
    }

    pub(super) fn dormant_fixture(flow: DraftFlow, status: DraftStatus) -> DraftSession {
        use draft_core::pack_source::FixturePackSource;
        let template = persisted_premier_session(SetLayout::UniformByRound {
            codes: vec!["TST".into()],
        });
        let mut config = template.config;
        config.pod_size = 4;
        config.cards_per_pack = 2;
        config.flow = flow;
        if flow == DraftFlow::FifoAsync {
            config.clock_policy = DraftClockPolicy {
                pick: PickClockPolicy::Fixed { duration_ms: 1000 },
                review_duration_ms: 1000,
            };
        }
        let seats = template.seats.into_iter().take(4).collect();
        let mut draft = match flow {
            DraftFlow::LegacyBarrier => DraftSession::new(config, seats, "guard-fixture".into()),
            DraftFlow::FifoAsync => DraftSession::new_fifo(
                config,
                seats,
                "guard-fixture".into(),
                DraftSessionId("guard-incarnation".into()),
            )
            .unwrap(),
        };
        session::apply_at(
            &mut draft,
            DraftAction::StartDraft,
            Some(&FixturePackSource {
                set_code: "TST".into(),
                cards_per_pack: 2,
            }),
            1000,
        )
        .unwrap();
        if status == DraftStatus::Review || status == DraftStatus::Paused {
            for _ in 0..2 {
                for seat in 0..4 {
                    let action = draft_core::pick_pass::legal_auto_selection(&draft, seat).unwrap();
                    session::apply_at(&mut draft, action, None, 1100).unwrap();
                }
            }
            assert_eq!(draft.status, DraftStatus::Review);
            if status == DraftStatus::Paused {
                session::apply_at(
                    &mut draft,
                    DraftAction::Control {
                        control: DraftControl::Pause {
                            reason: DraftPauseReason::PausedByHost,
                        },
                    },
                    None,
                    1500,
                )
                .unwrap();
            }
        }
        draft.validate_persisted_snapshot().unwrap();
        assert_eq!(draft.status, status);
        if flow == DraftFlow::FifoAsync && status == DraftStatus::Drafting {
            let mut positive = draft.clone();
            let action = draft_core::pick_pass::legal_auto_selection(&positive, 0).unwrap();
            session::apply_at(&mut positive, action, None, 1100).unwrap();
            assert_eq!(positive.pools[0].len(), 1);
        }
        draft
    }

    fn install_guard_cells(draft: DraftSession) {
        clear_state();
        install_fixture_db();
        session_cell::install(draft);
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::VeryHard));
        RNG.with(|cell| cell.set(Some(ChaCha20Rng::seed_from_u64(8765))));
        let pool: LimitedSetPool = serde_json::from_str(&pod_pool_json("TST")).unwrap();
        PACK_GEN.with(|cell| cell.set(Some(PackGenerator::new(pool))));
    }

    pub(super) fn guard_cells_snapshot() -> serde_json::Value {
        use draft_core::pack_source::PackSource;
        use rand::RngCore;
        let session = if session_cell::is_installed() {
            session_cell::with_installed(|draft| serde_json::to_value(draft).unwrap())
        } else {
            serde_json::Value::Null
        };
        let rng = RNG.with(|cell| {
            let value = cell.take();
            let words = value.as_ref().map(|rng| {
                let mut copy = rng.clone();
                (0..4).map(|_| copy.next_u64()).collect::<Vec<_>>()
            });
            cell.set(value);
            words
        });
        let generated = PACK_GEN.with(|cell| {
            let value = cell.take();
            let cards = value.as_ref().map(|generator| {
                generator.generate_pack(&mut ChaCha20Rng::seed_from_u64(42), 0, 0)
            });
            cell.set(value);
            cards
        });
        let database = CARD_DB.with(|cell| { let value=cell.borrow();value.as_ref().map(|db| serde_json::json!({"count":db.card_count(),"alpha":db.get_face_by_name("Alpha")})) });
        serde_json::json!({"session":session,"difficulty":DIFFICULTY.with(|cell|format!("{:?}",cell.get())),"rng":rng,"generator":generated,"database":database,"local_owner":session_cell::local_owner(),"local_llm_drafting":LOCAL_LLM_DRAFTING.with(|cell|format!("{:?}",cell.get()))})
    }

    #[test]
    fn dormant_wasm_live_fifo_read_guard_precedes_serialization() {
        for status in [
            DraftStatus::Drafting,
            DraftStatus::Review,
            DraftStatus::Paused,
        ] {
            install_guard_cells(dormant_fixture(DraftFlow::FifoAsync, status));
            let before = guard_cells_snapshot();
            let entered = Cell::new(0);
            let result = with_draft_inner(|draft| {
                entered.set(entered.get() + 1);
                serde_json::to_value(filter_for_player(draft, 0)).map_err(|error| error.to_string())
            });
            assert_eq!(
                result,
                Err("FIFO drafting is not supported by this runtime".into())
            );
            assert_eq!(entered.get(), 0);
            assert_eq!(guard_cells_snapshot(), before);
        }
        install_guard_cells(dormant_fixture(
            DraftFlow::LegacyBarrier,
            DraftStatus::Drafting,
        ));
        let before = guard_cells_snapshot();
        let entered = Cell::new(0);
        let result = with_draft_inner(|draft| {
            entered.set(entered.get() + 1);
            serde_json::to_value(filter_for_player(draft, 0)).map_err(|error| error.to_string())
        })
        .unwrap();
        assert_eq!(entered.get(), 1);
        assert_eq!(result["current_pack"].as_array().unwrap().len(), 2);
        assert_eq!(guard_cells_snapshot(), before);
        assert_eq!(
            with_draft_inner::<()>(|_| Err("operation error".into())),
            Err("operation error".into())
        );
        assert_eq!(guard_cells_snapshot(), before);
        clear_state();
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
    }

    #[test]
    fn dormant_wasm_live_fifo_mutable_guard_precedes_effects() {
        use rand::RngCore;
        for status in [
            DraftStatus::Drafting,
            DraftStatus::Review,
            DraftStatus::Paused,
        ] {
            install_guard_cells(dormant_fixture(DraftFlow::FifoAsync, status));
            let before = guard_cells_snapshot();
            let entered = Cell::new(0);
            let result = with_draft_mut_inner(|draft| {
                entered.set(entered.get() + 1);
                if status == DraftStatus::Drafting {
                    let action = draft_core::pick_pass::legal_auto_selection(draft, 0).unwrap();
                    session::apply_at(draft, action, None, 1100)
                        .map_err(|error| error.to_string())?;
                }
                RNG.with(|cell| {
                    let mut rng = cell.take().unwrap();
                    rng.next_u64();
                    cell.set(Some(rng));
                });
                Ok(())
            });
            assert_eq!(
                result,
                Err("FIFO drafting is not supported by this runtime".into())
            );
            assert_eq!(entered.get(), 0);
            assert_eq!(guard_cells_snapshot(), before);
            assert_eq!(
                submit_pick_for_seat_inner(0, "[\"TST-0-0-0\"]").unwrap_err(),
                "FIFO drafting is not supported by this runtime"
            );
            assert_eq!(guard_cells_snapshot(), before);
        }
        install_guard_cells(dormant_fixture(
            DraftFlow::LegacyBarrier,
            DraftStatus::Drafting,
        ));
        let entered = Cell::new(0);
        with_draft_mut_inner(|draft| {
            entered.set(entered.get() + 1);
            session::apply(
                draft,
                DraftAction::Pick {
                    seat: 0,
                    card_instance_ids: vec!["TST-0-0-0".into()],
                },
                None,
            )
            .map_err(|error| error.to_string())
        })
        .unwrap();
        assert_eq!(entered.get(), 1);
        assert_eq!(
            guard_cells_snapshot()["session"]["pools"][0]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        install_guard_cells(dormant_fixture(
            DraftFlow::LegacyBarrier,
            DraftStatus::Drafting,
        ));
        assert_eq!(
            submit_pick_for_seat_inner(0, "[\"TST-0-0-0\"]")
                .unwrap()
                .pool
                .len(),
            1
        );
        let before = guard_cells_snapshot();
        let entered = Cell::new(0);
        assert_eq!(
            with_draft_mut_inner::<()>(|_| {
                entered.set(entered.get() + 1);
                Err("operation error".into())
            }),
            Err("operation error".into())
        );
        assert_eq!(entered.get(), 1);
        assert_eq!(guard_cells_snapshot(), before);
        clear_state();
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
    }

    #[test]
    fn dormant_transports_refuse_fifo_before_install() {
        let legacy = dormant_fixture(DraftFlow::LegacyBarrier, DraftStatus::Drafting);
        install_guard_cells(legacy.clone());
        let legacy_json = serde_json::to_string(&legacy).unwrap();
        assert!(import_draft_session_inner(&legacy_json, 4).is_ok());
        assert_eq!(
            submit_pick_for_seat_inner(0, "[\"TST-0-0-0\"]")
                .unwrap()
                .pool
                .len(),
            1
        );
        for status in [
            DraftStatus::Drafting,
            DraftStatus::Review,
            DraftStatus::Paused,
        ] {
            let draft = dormant_fixture(DraftFlow::FifoAsync, status);
            let before = guard_cells_snapshot();
            assert_eq!(
                import_draft_session_inner(&serde_json::to_string(&draft).unwrap(), 0).unwrap_err(),
                "FIFO drafting is not supported by this runtime"
            );
            assert_eq!(guard_cells_snapshot(), before);
        }
        let fifo = dormant_fixture(DraftFlow::FifoAsync, DraftStatus::Drafting);
        for malformed in ["missing-flow", "unknown-flow", "unknown-status"] {
            let mut json = serde_json::to_value(&fifo).unwrap();
            match malformed {
                "missing-flow" => {
                    json["config"].as_object_mut().unwrap().remove("flow");
                }
                "unknown-flow" => json["config"]["flow"] = "FutureFlow".into(),
                "unknown-status" => json["status"] = "FutureStatus".into(),
                _ => unreachable!(),
            }
            let before = guard_cells_snapshot();
            let error = import_draft_session_inner(&json.to_string(), 0).unwrap_err();
            if malformed == "missing-flow" {
                assert_eq!(error, "FIFO drafting is not supported by this runtime");
            } else {
                assert!(error.contains("unknown variant"));
            }
            assert_eq!(guard_cells_snapshot(), before);
        }
        for action in [
            draft_core::pick_pass::legal_auto_selection(&fifo, 0).unwrap(),
            DraftAction::Control {
                control: DraftControl::Resume,
            },
            DraftAction::AdvanceClock,
        ] {
            let before = guard_cells_snapshot();
            assert_eq!(
                apply_draft_action_inner(&serde_json::to_string(&action).unwrap()).unwrap_err(),
                "FIFO drafting is not supported by this runtime"
            );
            assert_eq!(guard_cells_snapshot(), before);
        }
        clear_state();
        let before = guard_cells_snapshot();
        assert!(import_draft_session_inner(&serde_json::to_string(&fifo).unwrap(), 0).is_err());
        assert_eq!(guard_cells_snapshot(), before);
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
    }

    /// A legacy session, as draft-core serializes it, passes the check.
    #[test]
    fn restorable_session_check_admits_a_legacy_session() {
        let legacy = dormant_fixture(DraftFlow::LegacyBarrier, DraftStatus::Drafting);
        let json = serde_json::to_string(&legacy).unwrap();
        // Reach: the text deserializes to the same session, which the legacy
        // admission itself accepts.
        assert_eq!(serde_json::from_str::<DraftSession>(&json).unwrap(), legacy);
        assert!(legacy.validate_legacy_admission().is_ok());
        assert_eq!(
            restorable_draft_session_verdict(&json),
            RestorableDraftSessionVerdict::Admitted
        );
    }

    /// Every session the legacy admission refuses, as draft-core serializes
    /// it, is reported incompatible rather than unreadable.
    #[test]
    fn restorable_session_check_refuses_what_the_legacy_admission_refuses() {
        let legacy = dormant_fixture(DraftFlow::LegacyBarrier, DraftStatus::Drafting);
        let mut legacy_review = legacy.clone();
        legacy_review.status = DraftStatus::Review;
        let mut burn_policy = legacy.clone();
        burn_policy.config.selection_policy = DraftSelectionPolicy {
            burns_per_pick: 1,
            discard_threshold: 0,
        };
        let mut pick_clock = legacy.clone();
        pick_clock.config.clock_policy = DraftClockPolicy {
            pick: PickClockPolicy::Fixed { duration_ms: 1000 },
            review_duration_ms: 0,
        };
        let mut review_duration = legacy;
        review_duration.config.clock_policy = DraftClockPolicy {
            pick: PickClockPolicy::Off,
            review_duration_ms: 1000,
        };
        let refused = [
            dormant_fixture(DraftFlow::FifoAsync, DraftStatus::Drafting),
            dormant_fixture(DraftFlow::FifoAsync, DraftStatus::Review),
            legacy_review,
            burn_policy,
            pick_clock,
            review_duration,
        ];
        let verdicts: Vec<_> = refused
            .iter()
            .map(|session| {
                let json = serde_json::to_string(session).unwrap();
                // Reach: the text deserializes to the same session, which the
                // legacy admission itself refuses.
                assert_eq!(
                    &serde_json::from_str::<DraftSession>(&json).unwrap(),
                    session
                );
                assert!(session.validate_legacy_admission().is_err());
                restorable_draft_session_verdict(&json)
            })
            .collect();
        assert_eq!(verdicts, [RestorableDraftSessionVerdict::Incompatible; 6]);
    }

    /// Text that does not deserialize as a draft session is unreadable, not a
    /// refusal.
    #[test]
    fn restorable_session_check_reports_text_that_is_not_a_session_as_unreadable() {
        let legacy = serde_json::to_value(dormant_fixture(
            DraftFlow::LegacyBarrier,
            DraftStatus::Drafting,
        ))
        .unwrap();
        // Paired positive: the unedited session deserializes, so the refusal
        // below is about the unknown flow.
        assert!(serde_json::from_value::<DraftSession>(legacy.clone()).is_ok());
        let mut unknown_flow = legacy;
        unknown_flow["config"]["flow"] = "FutureFlow".into();
        let unreadable = ["{".to_string(), "[]".to_string(), unknown_flow.to_string()];
        let verdicts: Vec<_> = unreadable
            .iter()
            .map(|json| {
                assert!(serde_json::from_str::<DraftSession>(json).is_err());
                restorable_draft_session_verdict(json)
            })
            .collect();
        assert_eq!(verdicts, [RestorableDraftSessionVerdict::Unreadable; 3]);
    }

    #[test]
    fn restorable_session_check_verdicts_are_type_tagged_objects() {
        for (verdict, tag) in [
            (RestorableDraftSessionVerdict::Admitted, "Admitted"),
            (RestorableDraftSessionVerdict::Incompatible, "Incompatible"),
            (RestorableDraftSessionVerdict::Unreadable, "Unreadable"),
        ] {
            assert_eq!(
                serde_json::to_value(verdict).unwrap(),
                serde_json::json!({ "type": tag })
            );
        }
    }

    /// The check installs nothing, and leaves an installed session, the random
    /// state, the pack generator, the difficulty and the card database as they
    /// were.
    #[test]
    fn restorable_session_check_reads_and_writes_no_thread_local() {
        let installed = dormant_fixture(DraftFlow::LegacyBarrier, DraftStatus::Drafting);
        let mut checked = installed.clone();
        checked.draft_code = "checked-draft".into();
        let inputs = [
            serde_json::to_string(&checked).unwrap(),
            serde_json::to_string(&dormant_fixture(
                DraftFlow::FifoAsync,
                DraftStatus::Drafting,
            ))
            .unwrap(),
            "{".to_string(),
        ];
        let check_inputs = || {
            let verdicts: Vec<_> = inputs
                .iter()
                .map(String::as_str)
                .map(restorable_draft_session_verdict)
                .collect();
            // Reach: the legacy text is admitted, the FIFO text reaches the
            // admission, and the malformed text is unreadable.
            assert_eq!(verdicts[0], RestorableDraftSessionVerdict::Admitted);
            assert_ne!(verdicts[1], RestorableDraftSessionVerdict::Unreadable);
            assert_eq!(verdicts[2], RestorableDraftSessionVerdict::Unreadable);
        };

        clear_state();
        let empty = guard_cells_snapshot();
        check_inputs();
        assert!(!session_cell::is_installed());
        assert_eq!(guard_cells_snapshot(), empty);

        install_guard_cells(installed);
        let export = session_cell::with_installed(|draft| serde_json::to_string(draft).unwrap());
        let before = guard_cells_snapshot();
        check_inputs();
        assert_eq!(
            session_cell::with_installed(|draft| serde_json::to_string(draft).unwrap()),
            export
        );
        assert_eq!(guard_cells_snapshot(), before);
        clear_state();
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
    }

    /// The shared fixture's saved sessions, which the client's saved-host tests
    /// read with the draft engine's recorded answers.
    fn shared_saved_sessions() -> serde_json::Value {
        let shared: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/draft-contract/fifo_legacy_admission.json"
        ))
        .unwrap();
        shared["saved_sessions"].clone()
    }

    /// Each saved session in the shared fixture is draft-core's serialization of
    /// a session built here, or of one with a field set to a value draft-core
    /// cannot read, and the check gives it the verdict the fixture records.
    #[test]
    fn shared_fixture_saved_sessions_are_draft_core_sessions_with_their_verdicts() {
        let legacy = dormant_fixture(DraftFlow::LegacyBarrier, DraftStatus::Drafting);
        let fifo = dormant_fixture(DraftFlow::FifoAsync, DraftStatus::Drafting);
        let mut legacy_review = legacy.clone();
        legacy_review.status = DraftStatus::Review;
        let mut legacy_carrying_fifo = legacy;
        legacy_carrying_fifo.fifo = fifo.fifo.clone();
        let mut fifo_complete = fifo.clone();
        fifo_complete.status = DraftStatus::Complete;
        let mut fifo_abandoned = fifo.clone();
        fifo_abandoned.status = DraftStatus::Abandoned;
        let value = |session: &DraftSession| serde_json::to_value(session).unwrap();
        let mut legacy_review_unknown_flow = value(&legacy_review);
        legacy_review_unknown_flow["config"]["flow"] = "FutureFlow".into();
        let built = [
            ("fifo_drafting", value(&fifo)),
            ("legacy_review", value(&legacy_review)),
            ("legacy_carrying_fifo", value(&legacy_carrying_fifo)),
            ("fifo_complete", value(&fifo_complete)),
            ("fifo_abandoned", value(&fifo_abandoned)),
            ("legacy_review_unknown_flow", legacy_review_unknown_flow),
        ];
        let shared = shared_saved_sessions();
        // Reach: the fixture holds exactly these saved sessions.
        let mut names: Vec<&str> = shared
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        let mut expected: Vec<&str> = built.iter().map(|(name, _)| *name).collect();
        expected.sort_unstable();
        assert_eq!(names, expected);
        for (name, session) in &built {
            let saved = &shared[*name];
            assert_eq!(&saved["session"], session, "{name}");
            let verdict = restorable_draft_session_verdict(&saved["session"].to_string());
            assert_eq!(
                serde_json::to_value(verdict).unwrap(),
                saved["verdict"],
                "{name}"
            );
        }
    }

    /// V26. A public backup strips `shared_stack` at the trust boundary, and
    /// the result is DELIBERATELY not a restorable Winston snapshot -- the same
    /// discipline `import_rejects_redacted_chaos_snapshot_that_claims_a_uniform_layout`
    /// applies to a redacted Chaos layout. The authoritative, resumable copy is
    /// the host's IndexedDB snapshot.
    #[test]
    fn import_rejects_redacted_winston_snapshot_missing_its_stack() {
        clear_state();

        let session = started_winston_session();
        assert_eq!(session.status, draft_core::types::DraftStatus::Drafting);
        let mut snapshot = serde_json::to_value(&session).expect("serialize Winston snapshot");
        assert!(
            snapshot.get("shared_stack").is_some(),
            "the unredacted snapshot carries its stack"
        );

        // Paired positive FIRST, so the refusal below is demonstrably about the
        // redaction and not about Winston.
        restorable_draft_session_from_json(&snapshot.to_string())
            .expect("an unredacted Winston snapshot restores");

        snapshot
            .as_object_mut()
            .expect("a session serializes to an object")
            .remove("shared_stack");
        let error = restorable_draft_session_from_json(&snapshot.to_string())
            .expect_err("a public redaction is not a restorable Winston snapshot");
        assert!(
            error.contains("must carry its stack"),
            "unexpected error: {error}"
        );
        assert!(!session_cell::is_installed());
    }

    /// V20's own leg. `draft_kind_wire_numbers_round_trip` and
    /// `draft_procedure_dto_copies_every_axis_unmoved` carry the rest.
    #[test]
    fn winston_round_trips_through_the_wire_number() {
        assert_eq!(draft_kind_wire_number(DraftKind::Winston), 5);
        assert_eq!(draft_kind_from_wire(5), Ok(DraftKind::Winston));
        // Negative sibling: an unmapped number is an `Err` naming every known
        // kind, never a default.
        let error = draft_kind_from_wire(6).expect_err("6 is unmapped");
        assert!(error.contains("unknown draft kind 6"), "{error}");
        for kind in DraftKind::ALL {
            assert!(
                error.contains(&format!("{kind:?}")),
                "{error} omits {kind:?}"
            );
        }
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
        assert!(!session_cell::is_installed());
    }

    #[test]
    fn import_normalizes_legacy_commander_deck_floors_for_human_and_bot_decks() {
        clear_state();
        install_commander_fixture_db();
        start_commander_pod(4);
        seat_into_deckbuilding(0, 60);

        let mut snapshot = session_cell::with_installed(|session| {
            serde_json::to_value(session).expect("serialize Commander snapshot")
        });
        snapshot["config"]["min_deck_size"] = serde_json::json!(59);
        let restored = restorable_draft_session_from_json(&snapshot.to_string())
            .expect("a legacy Commander snapshot restores");
        assert_eq!(restored.config.min_deck_size, 60);
        session_cell::install(restored);

        let human_deck = seat_deck(0, 59);
        let error = submit_deck_inner(&json(&human_deck), &json(&["Seat 0 Card 0".to_string()]))
            .expect_err("a restored 59-card Commander deck is illegal");
        assert!(
            error.contains("59") && error.contains("60"),
            "error = {error}"
        );

        let bot_deck = get_bot_deck_inner(1).expect("a restored Commander bot deck builds");
        let bot_total = bot_deck.main_deck.len()
            + bot_deck
                .lands
                .values()
                .map(|&count| count as usize)
                .sum::<usize>();
        assert!(bot_total >= 60, "bot deck has {bot_total} cards");

        clear_state();
    }

    #[test]
    fn cube_pool_input_drives_drafting_status_and_pack_size() {
        clear_state();
        install_fixture_db();

        // Four dealt cards, plus duplicate and undealt source occurrences.
        let pool_input_json = r#"{
            "type": "Cube",
            "data": {
                "cube_list_text": "2 Alpha\n1 Beta\n1 Gamma\n2 Delta\n",
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
        )
        .expect("cube draft should start");

        let expected = vec!["Alpha", "Alpha", "Beta", "Gamma", "Delta", "Delta"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(serde_json::to_value(&view)
            .unwrap()
            .get("booster_pack_pool")
            .is_none());
        assert_eq!(
            booster_pack_pool_for_game_inner(),
            Ok(Some(expected.clone()))
        );

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
        session_cell::with_installed_mut(|session| {
            let deltas = session::apply(session, action, None).expect("pick applies");
            assert!(!deltas.is_empty(), "pick should produce deltas");
        });

        // After the human pick, seat 0's pack should no longer contain the picked card
        // (it has been passed; pack will not be visible again until the rotation lands).
        let post_view = session_cell::with_installed(|session| {
            let json = serde_json::to_string(session).unwrap();
            let restored = restorable_draft_session_from_json(&json).unwrap();
            filter_for_player(&restored, 0)
        });
        assert_eq!(post_view.pool.len(), 1);
        assert!(serde_json::to_value(&post_view)
            .unwrap()
            .get("booster_pack_pool")
            .is_none());
        assert_eq!(booster_pack_pool_for_game_inner(), Ok(Some(expected)));
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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

    #[test]
    fn unglued_set_pool_loads_ten_cards_per_pack_for_draft_and_sealed() {
        let set_pool_json = r#"{
            "code": "UGL",
            "name": "Unglued",
            "release_date": null,
            "pack_variants": [
                {
                    "contents": [
                        { "slot": "basic", "count": 1, "choices": [{ "sheet": "basic", "weight": 1 }] },
                        { "slot": "common", "count": 6, "choices": [{ "sheet": "common", "weight": 1 }] },
                        { "slot": "rare", "count": 1, "choices": [{ "sheet": "rare", "weight": 1 }] },
                        { "slot": "uncommon", "count": 2, "choices": [{ "sheet": "uncommon", "weight": 1 }] }
                    ],
                    "weight": 3
                },
                {
                    "contents": [
                        { "slot": "basic", "count": 1, "choices": [{ "sheet": "basic", "weight": 1 }] },
                        { "slot": "common", "count": 6, "choices": [{ "sheet": "common", "weight": 1 }] },
                        { "slot": "rare", "count": 1, "choices": [{ "sheet": "rare", "weight": 1 }] },
                        { "slot": "uncommon", "count": 1, "choices": [{ "sheet": "uncommon", "weight": 1 }] }
                    ],
                    "weight": 2
                }
            ],
            "pack_variants_total_weight": 5,
            "sheets": {
                "basic": {
                    "cards": [
                        { "name": "Forest", "set_code": "UGL", "collector_number": "1", "rarity": "common", "weight": 1 }
                    ],
                    "total_weight": 1,
                    "foil": false,
                    "balance_colors": false
                },
                "common": {
                    "cards": [
                        { "name": "C1", "set_code": "UGL", "collector_number": "2", "rarity": "common", "weight": 1 },
                        { "name": "C2", "set_code": "UGL", "collector_number": "3", "rarity": "common", "weight": 1 },
                        { "name": "C3", "set_code": "UGL", "collector_number": "4", "rarity": "common", "weight": 1 },
                        { "name": "C4", "set_code": "UGL", "collector_number": "5", "rarity": "common", "weight": 1 },
                        { "name": "C5", "set_code": "UGL", "collector_number": "6", "rarity": "common", "weight": 1 },
                        { "name": "C6", "set_code": "UGL", "collector_number": "7", "rarity": "common", "weight": 1 }
                    ],
                    "total_weight": 6,
                    "foil": false,
                    "balance_colors": false
                },
                "uncommon": {
                    "cards": [
                        { "name": "U1", "set_code": "UGL", "collector_number": "8", "rarity": "uncommon", "weight": 1 },
                        { "name": "U2", "set_code": "UGL", "collector_number": "9", "rarity": "uncommon", "weight": 1 }
                    ],
                    "total_weight": 2,
                    "foil": false,
                    "balance_colors": false
                },
                "rare": {
                    "cards": [
                        { "name": "R1", "set_code": "UGL", "collector_number": "10", "rarity": "rare", "weight": 1 }
                    ],
                    "total_weight": 1,
                    "foil": false,
                    "balance_colors": false
                }
            },
            "prints": [],
            "basic_lands": ["Forest"]
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
            2,
        )
        .expect("Unglued draft should start");
        assert_eq!(draft_view.cards_per_pack, 10);
        assert_eq!(draft_view.current_pack.expect("current pack").len(), 10);

        clear_state();
        let sealed_view = create_multiplayer_draft_inner(
            &pool_input_json,
            seats_json,
            3,
            42,
            "test-room",
            "Swiss",
            "Competitive",
            2,
        )
        .expect("Unglued sealed should start");
        assert_eq!(sealed_view.cards_per_pack, 10);
        assert_eq!(sealed_view.pool.len(), 60);
        assert!(sealed_view
            .sealed_packs
            .expect("sealed pack boundaries")
            .iter()
            .all(|pack| pack.len() == 10));

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
                2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            // THE RULE, stated durably so the next widening does not repeat
            // this: the unmapped-kind hostile fixture must name a number BEYOND
            // the widened table. `5` is `DraftKind::Winston` now.
            6,
            42,
            "test-room",
            "Swiss",
            "Competitive",
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
        )
        .expect_err("kind 6 is unmapped");
        assert!(
            err.contains("unknown draft kind 6"),
            "unexpected error: {err}"
        );

        clear_state();
    }

    /// Phase D's discriminating test: a pod bot plays at the strength THIS pod
    /// asked for, not at whatever the last draft in this tab left behind.
    ///
    /// `DIFFICULTY` is a per-thread `Cell` with no reset, and
    /// `create_multiplayer_draft_inner` never wrote it before this change. So a
    /// player who ran a Quick draft at `VeryHard` and then hosted a pod in the
    /// same worker got a `VeryHard` pod bot, silently. That is the contamination
    /// this asserts against, seeded here exactly as `start_quick_draft` would
    /// have seeded it.
    ///
    /// TWO legs, because one is satisfiable by a function that always writes
    /// `Medium`: the second call asks for `Hard` from the same contaminated
    /// starting point and must get it.
    #[test]
    fn a_pod_bot_ignores_a_previous_drafts_difficulty() {
        clear_state();
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::VeryHard));
        // Positive control that the seeding took. Without it, a test whose
        // seeding silently failed would pass on a cell that was already
        // `Medium` by default.
        assert_eq!(DIFFICULTY.with(|cell| cell.get()), AiDifficulty::VeryHard);

        create_multiplayer_draft_inner(
            &commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            4, // CommanderDraft
            42,
            "test-room",
            "Swiss",
            "Competitive",
            2, // Medium — what this pod's host asked for.
        )
        .expect("the pod should start");

        // REVERT-FAILING: delete the `DIFFICULTY.with(..set..)` line from the
        // pool arm and this reads `VeryHard`, the previous draft's answer.
        assert_eq!(DIFFICULTY.with(|cell| cell.get()), AiDifficulty::Medium);

        // The paired leg, from the same contaminated start, asking for a
        // DIFFERENT rung: an implementation that hardcodes `Medium` reds here.
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::VeryHard));
        create_multiplayer_draft_inner(
            &commander_pool_input_json(),
            COMMANDER_SEATS_JSON,
            4, // CommanderDraft
            42,
            "test-room",
            "Swiss",
            "Competitive",
            3, // Hard
        )
        .expect("the pod should start");
        assert_eq!(DIFFICULTY.with(|cell| cell.get()), AiDifficulty::Hard);

        clear_state();
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
    /// transposition reddens here. Recomputed with the `Winston` row:
    /// `commanders_required` is `[0, 0, 0, 0, 1, 0]` over `DraftKind::ALL` and
    /// equals no other column (`pod_size` `[8, 8, 8, 8, 4, 2]`, `human_seats`
    /// `[1, 8, 8, 8, 1, 2]`, `min_pod_size` `[2, 2, 2, 2, 3, 2]`,
    /// `packs_per_player` `[3, 3, 3, 6, 3, 3]`, `cards_per_pick`
    /// `[1, 1, 1, 1, 2, 1]`), so the pairwise-distinctness conclusion holds and
    /// the argument survives the sixth kind.
    ///
    /// The `Winston` row has `pod_size == human_seats == min_pod_size == 2`, so
    /// a single-kind test on that row could not catch a transposition among
    /// those three. The fold over `ALL` is what keeps that within-row
    /// transposition red — the same reason the Commander row gave.
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
                dto.cube_min_deck_size, procedure.cube_min_deck_size,
                "cube_min_deck_size ({kind:?})"
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
        session_cell::with_installed_mut(|session| {
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
        });
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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

        session_cell::with_installed(|session| {
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
            assert!(submission.printings.is_empty());
            assert!(serde_json::to_value(submission)
                .unwrap()
                .get("printings")
                .is_none());
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

        session_cell::with_installed(|session| {
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
        session_cell::with_installed(|session| {
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
        session_cell::with_installed(|session| {
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

        session_cell::with_installed(|session| {
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
            assert!(seat2.printings.is_empty());
            assert!(serde_json::to_value(seat2)
                .unwrap()
                .get("printings")
                .is_none());

            let seat0 = session
                .submitted_decks
                .get(&engine::types::player::PlayerId(0))
                .expect("seat 0's submission survives seat 2's");
            assert_eq!(seat0.seat, 0);
            assert_eq!(
                seat0.commanders, seat0_commanders,
                "a later seat's designation must not be attributed to an earlier one"
            );
            assert!(seat0.printings.is_empty());
            assert!(serde_json::to_value(seat0)
                .unwrap()
                .get("printings")
                .is_none());
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

    fn install_ante_fixture_db() {
        let mut cards: serde_json::Value =
            serde_json::from_str(&commander_fixture_db_json()).unwrap();
        let mut contract = cards["beta"].clone();
        contract["name"] = serde_json::json!("Contract from Below");
        contract["card_type"]["core_types"] = serde_json::json!(["Sorcery"]);
        contract["oracle_text"] = serde_json::json!(
            "Remove this card from your deck before playing if you're not playing for ante.\nDiscard your hand, ante the top card of your library, then draw seven cards."
        );
        cards["contract from below"] = contract;
        let mut forest = cards["plains"].clone();
        forest["name"] = serde_json::json!("Forest");
        forest["color_identity"] = serde_json::json!(["Green"]);
        cards["forest"] = forest;
        let db = CardDatabase::from_json_str(&cards.to_string()).unwrap();
        CARD_DB.with(|cell| *cell.borrow_mut() = Some(db));
    }

    #[test]
    fn quick_cube_config_raises_zero_to_one_without_lowering_higher_requests() {
        let settings = |min_deck_size| CubeDraftSettings {
            pod_size: 8,
            pack_count: 3,
            cards_per_pack: 15,
            min_deck_size,
            addable_cards: DeckAddableCards::standard_basics(),
        };
        let config = |min_deck_size| {
            build_quick_cube_config(
                "Test Cube",
                &settings(min_deck_size),
                DeckAddableCards::standard_basics(),
                42,
            )
        };

        assert_eq!(config(0).min_deck_size, 1);
        assert_eq!(config(4).min_deck_size, 4);
        assert_eq!(config(4).flow, DraftFlow::FifoAsync);
        assert_eq!(config(4).clock_policy, DraftClockPolicy::quick_default());
        install_fixture_db();
        let cards = CARD_DB.with(|cell| {
            let db = cell.borrow();
            cube_cards_from_entries(
                &parse_cube_list("400 Alpha\n1 Delta").unwrap(),
                db.as_ref().unwrap(),
            )
            .unwrap()
        });
        let local = start_local_fifo_draft(
            config(4),
            local_quick_seats(8),
            "quick-cube-draft".to_string(),
            &CubePackSource::new(cards),
            AiDifficulty::Medium,
            LocalLlmDrafting::EngineBots,
            1_000,
        )
        .expect("the cube configuration starts on the local FIFO runtime");
        assert_eq!(
            local
                .view
                .fifo
                .as_ref()
                .and_then(|fifo| fifo.head.as_ref())
                .map(|head| head.original_size),
            Some(15)
        );
        let pool = local_fifo_booster_pack_pool_for_game_inner(local.owner)
            .unwrap()
            .unwrap();
        assert_eq!(pool.len(), 401);
        assert_eq!(pool.last().unwrap(), "Delta");
        clear_state();
    }

    #[test]
    fn multiplayer_commander_cube_stores_and_publishes_the_procedure_floor() {
        clear_state();
        install_commander_fixture_db();
        let pool_input = serde_json::json!({
            "type": "Cube",
            "data": {
                "cube_list_text": "200 Alpha",
                "cube_name": "Commander Cube",
                "cube_draft_settings": {
                    "pod_size": 4,
                    "pack_count": 3,
                    "cards_per_pack": 15,
                    "min_deck_size": 1,
                    "addable_cards": {
                        "policy": "StandardBasics",
                        "custom": []
                    }
                }
            }
        })
        .to_string();

        let view = create_multiplayer_draft_inner(
            &pool_input,
            COMMANDER_SEATS_JSON,
            4,
            42,
            "commander-cube",
            "Swiss",
            "Competitive",
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
        )
        .expect("Commander cube should start");

        assert_eq!(view.min_deck_size, 60);
        assert!(serde_json::to_value(&view)
            .unwrap()
            .get("booster_pack_pool")
            .is_none());
        assert_eq!(
            booster_pack_pool_for_game_inner(),
            Ok(Some(vec!["Alpha".to_string(); 200]))
        );
        session_cell::with_installed(|session| {
            assert_eq!(session.config.min_deck_size, 60);
        });
        clear_state();
    }

    /// `DraftSession.pools` is `pub`, and seeding a bot seat's pool directly is
    /// the same shape `draft-core`'s own session tests use.
    fn seed_bot_pool(seat: usize, pool: Vec<DraftCardInstance>) {
        session_cell::with_installed_mut(|session| {
            session.pools[seat] = pool;
        });
    }

    /// `DraftConfig.addable_cards` is `pub`. In PRODUCTION this field is written
    /// only by `create_multiplayer_draft_inner`'s Cube arm, from the host's cube
    /// settings; the Set arm hardcodes `DeckAddableCards::standard_basics()` and
    /// cannot reach a `CustomOnly` policy at all. The fixture below sets it
    /// directly on a Set session, so it must not be read as implying the Set
    /// path can produce this configuration.
    fn set_addable_cards(addable: DeckAddableCards) {
        session_cell::with_installed_mut(|session| {
            session.config.addable_cards = addable;
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
            2, // Medium; see `a_pod_bot_ignores_a_previous_drafts_difficulty`
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

    /// VM-4e — the source/kind gate when the host has no `CARD_DB`.
    ///
    /// `install_commander_fixture_db()` is deliberately NOT called. Do not add
    /// it back as an oversight — the whole subject of this row is the database's
    /// absence, and installing one silences it. `commander_pool_input_json()` is
    /// a SET pool, so session creation itself needs no database, which is what
    /// makes the fixture constructible.
    #[test]
    fn get_bot_deck_inner_requires_faces_for_commander_and_cube_but_not_set_premier() {
        clear_state();
        start_commander_pod(4);

        let err = get_bot_deck_inner(1).expect_err("CR 903.3 cannot be judged with no database");
        assert!(
            err.contains("Card database"),
            "the message must name the card database: {err}"
        );

        // The normal Set-backed Premier host does not fetch CARD_DB. Its bot
        // still proposes a complete deck from the drafted pool and basics.
        clear_state();
        start_commander_pod(1);
        let deck = get_bot_deck_inner(1).expect("Set Premier builds without CARD_DB");
        assert!(!deck.main_deck.is_empty(), "deck = {:?}", deck.main_deck);
        assert_eq!(
            deck.main_deck.len() + deck.lands.values().map(|&n| n as usize).sum::<usize>(),
            40
        );
        assert!(deck.commander.is_empty());

        assert!(get_bot_deck_inner(8).unwrap_err().contains("out of range"));

        // Positive reach: the same Premier seat builds with its database.
        install_commander_fixture_db();
        let deck = get_bot_deck_inner(1).expect("Premier builds with typed faces");
        assert!(!deck.main_deck.is_empty(), "deck = {:?}", deck.main_deck);

        // Cube's host loads faces at creation. Clearing them after creation
        // models a resumed Cube whose database is missing at bot proposal.
        clear_state();
        install_fixture_db();
        let cube_input = serde_json::json!({
            "type": "Cube",
            "data": {
                "cube_list_text": "20 Alpha\n20 Beta",
                "cube_name": "Test Cube",
                "cube_draft_settings": {
                    "pod_size": 2,
                    "pack_count": 1,
                    "cards_per_pack": 2,
                    "min_deck_size": 4,
                    "addable_cards": { "policy": "StandardBasics", "custom": [] }
                }
            }
        });
        let seats = serde_json::json!([
            { "type": "Human", "player_id": 0, "display_name": "Host" },
            { "type": "Bot", "name": "Bot 1" }
        ]);
        create_multiplayer_draft_inner(
            &cube_input.to_string(),
            &seats.to_string(),
            1,
            42,
            "cube-room",
            "Swiss",
            "Competitive",
            2,
        )
        .expect("Cube session must be installed before testing missing faces");
        seed_bot_pool(1, mono_white_bot_pool());
        CARD_DB.with(|cell| *cell.borrow_mut() = None);
        let err = get_bot_deck_inner(1).expect_err("Cube requires typed faces");
        assert!(err.contains("Card database"), "{err}");

        clear_state();
    }

    #[test]
    fn limited_bot_entry_admits_only_a_typed_ante_free_full_deck() {
        clear_state();
        install_ante_fixture_db();
        let cube_input = serde_json::json!({
            "type": "Cube",
            "data": {
                "cube_list_text": "100 Alpha\n100 Beta\n1 Contract from Below",
                "cube_name": "Ante Cube",
                "cube_draft_settings": {
                    "pod_size": 2,
                    "pack_count": 1,
                    "cards_per_pack": 2,
                    "min_deck_size": 40,
                    "addable_cards": {
                        "policy": "CustomOnly",
                        "custom": ["Contract from Below", "Forest"]
                    }
                }
            }
        });
        let seats = serde_json::json!([
            { "type": "Human", "player_id": 0, "display_name": "Host" },
            { "type": "Bot", "name": "Bot 1" }
        ]);
        create_multiplayer_draft_inner(
            &cube_input.to_string(),
            &seats.to_string(),
            1,
            42,
            "ante-cube",
            "Swiss",
            "Competitive",
            2,
        )
        .expect("Cube with typed addable faces must start");
        let mut pool = mono_white_bot_pool();
        pool.insert(
            0,
            DraftCardInstance {
                instance_id: "ante-first".to_string(),
                name: "Contract from Below".to_string(),
                set_code: "TST".to_string(),
                collector_number: "0".to_string(),
                rarity: "rare".to_string(),
                colors: vec!["B".to_string()],
                cmc: 1,
                type_line: "Sorcery".to_string(),
                draft_effect: None,
            },
        );
        seed_bot_pool(1, pool.clone());
        CARD_DB.with(|cell| {
            let db = cell.borrow();
            let db = db.as_ref().unwrap();
            assert!(db
                .get_face_by_name("Contract from Below")
                .is_some_and(engine::game::face_uses_ante));
            assert!(db
                .get_face_by_name("Forest")
                .is_some_and(|face| !engine::game::face_uses_ante(face)));
        });

        let deck = get_bot_deck_inner(1).expect("the legal land fills the proposal");
        assert!(deck.main_deck.contains(&"Alpha".to_string()));
        assert!(!deck.main_deck.contains(&"Contract from Below".to_string()));
        assert_eq!(deck.lands.get("Forest"), Some(&36));
        assert_eq!(
            deck.main_deck.len() + deck.lands.values().map(|&n| n as usize).sum::<usize>(),
            40
        );

        set_addable_cards(DeckAddableCards {
            policy: DeckAddableCardPolicy::CustomOnly,
            custom: vec!["Contract from Below".to_string()],
        });
        let err = get_bot_deck_inner(1).expect_err("ante-only fill is short");
        assert!(err.contains("minimum is 40"), "{err}");

        seed_bot_pool(1, vec![pool[0].clone()]);
        set_addable_cards(DeckAddableCards::standard_basics());
        let err = get_bot_deck_inner(1).expect_err("all-ante pool is exhausted");
        assert!(err.contains("reached 0 cards"), "{err}");
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

#[cfg(test)]
mod llm_seat_authority_tests {
    use super::*;

    /// A pod whose seat roster is explicit about which indices are bots, so the
    /// test never leans on the "seat 0 is the human" convention that the
    /// authority check exists to stop trusting.
    fn pod(bot_seats: &[u8], pod_size: u8) -> DraftSession {
        let config = DraftConfig {
            flow: draft_core::types::DraftFlow::LegacyBarrier,
            selection_policy: draft_core::types::DraftSelectionPolicy::default(),
            clock_policy: draft_core::types::DraftClockPolicy::default(),
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size,
            cards_per_pack: 15,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 7,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..pod_size)
            .map(|index| {
                if bot_seats.contains(&index) {
                    DraftSeat::Bot {
                        name: format!("Bot {index}"),
                    }
                } else {
                    DraftSeat::Human {
                        player_id: engine::types::player::PlayerId(index),
                        display_name: format!("Player {index}"),
                    }
                }
            })
            .collect();
        DraftSession::new(config, seats, "LLM-AUTH".to_string())
    }

    /// The motivating case: an 8-human Premier pod. Every index is in range, so
    /// a bounds check would admit all of them, but none may have its private
    /// pool and pack rendered into a third-party prompt.
    #[test]
    fn a_pod_of_humans_yields_no_llm_seat() {
        assert_eq!(llm_eligible_bot_seats(&pod(&[], 8)), Vec::<u8>::new());
    }

    /// A human at a NONZERO index is excluded while its bot neighbours are
    /// admitted. This is the disclosure the seat-number convention allowed:
    /// seat 3 is in range and is not seat 0.
    #[test]
    fn only_bot_seats_are_eligible_whatever_their_index() {
        assert_eq!(llm_eligible_bot_seats(&pod(&[1, 2, 4], 5)), vec![1, 2, 4]);
    }

    /// Seat 0 carries no special status in either direction: eligible when it
    /// is a bot, excluded when it is not.
    #[test]
    fn seat_zero_is_governed_by_its_role_not_its_number() {
        assert_eq!(llm_eligible_bot_seats(&pod(&[0], 2)), vec![0]);
        assert_eq!(llm_eligible_bot_seats(&pod(&[1], 2)), vec![1]);
        assert!(!llm_eligible_bot_seats(&pod(&[1], 2)).contains(&0));
    }

    /// There is no caller-supplied list to disagree with, so the roster is the
    /// only answer: a pod of all bots yields exactly its seats, in order.
    #[test]
    fn the_roster_is_the_only_source_of_eligibility() {
        assert_eq!(llm_eligible_bot_seats(&pod(&[0, 1, 2], 3)), vec![0, 1, 2]);
    }
}

#[cfg(test)]
mod local_fifo_runtime_tests {
    use super::create_multiplayer_draft_tests::{
        clear_state, dormant_fixture, guard_cells_snapshot, install_fixture_db,
    };
    use super::*;
    use crate::session_cell::STALE_LOCAL_OWNER;
    use draft_core::pack_source::FixturePackSource;
    use engine::types::card::DraftEffect;
    use engine::types::player::PlayerId;
    use phase_llm::draft_decision::{draft_pick_step, DraftPickStep};
    use rand::RngCore;

    /// The instance id [`LibrarianSource`] deals Cogwork Librarian under.
    const LIBRARIAN: &str = "cogwork-librarian";

    /// A canonical local session id, for sessions built by draft-core directly.
    const CANONICAL_ID: &str = "local-0123456789abcdef";

    /// An endpoint that parses, so the session gate, not a parse error, is what
    /// refuses the LLM request builder.
    const ENDPOINT: &str =
        r#"{"provider":"OpenAiCompatible","baseUrl":"https://provider.test/v1","model":"test"}"#;

    /// A local Quick FIFO configuration: three boosters, the 40-card floor, the
    /// untimed Quick clock, no discard threshold, and `burns_per_pick` burns.
    /// The seed only makes the bots' random draws repeatable.
    fn local_quick_config(
        source: DraftSource,
        pod_size: u8,
        cards_per_pack: u8,
        burns_per_pick: u8,
    ) -> DraftConfig {
        DraftConfig {
            flow: DraftFlow::FifoAsync,
            selection_policy: DraftSelectionPolicy {
                burns_per_pick,
                discard_threshold: 0,
            },
            clock_policy: DraftClockPolicy::quick_default(),
            set_code: source.set_code(),
            source,
            kind: DraftKind::Quick,
            pod_size,
            cards_per_pack,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 20_261_106,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        }
    }

    fn set_source() -> DraftSource {
        DraftSource::single_set("TST")
    }

    /// Three textless 3-card `TST` boosters per seat. With 8 seats a booster
    /// reaches three seats before it empties (CR 905.1a), so every seat drafts
    /// 3 cards a round and 9 in all. After the start batch the human's queue
    /// holds its own booster (3 cards), seat 7's (2) and seat 6's (1), in that
    /// order, because the first round passes left (CR 905.1b).
    fn fixture_source() -> FixturePackSource {
        FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 3,
        }
    }

    /// [`fixture_source`]'s boosters, except that the first card of seat 0's
    /// first booster is Cogwork Librarian, so it is in the local human's first
    /// head. Its Oracle text, verbatim: "Draft this card face up. / As you draft
    /// a card, you may draft an additional card from that booster pack. If you
    /// do, put this card into that booster pack." The parser reads it as
    /// `DraftEffect::AdditionalPick`, a card drafted face up (CR 905.2c).
    struct LibrarianSource;

    impl PackSource for LibrarianSource {
        fn generate_pack(
            &self,
            rng: &mut dyn rand::RngCore,
            seat: u8,
            pack_number: u8,
        ) -> DraftPack {
            let mut pack = fixture_source().generate_pack(rng, seat, pack_number);
            if seat == 0 && pack_number == 0 {
                pack.0[0] = DraftCardInstance {
                    instance_id: LIBRARIAN.to_string(),
                    name: "Cogwork Librarian".to_string(),
                    set_code: "CNS".to_string(),
                    collector_number: "58".to_string(),
                    rarity: "common".to_string(),
                    colors: Vec::new(),
                    cmc: 4,
                    type_line: "Artifact Creature — Construct".to_string(),
                    draft_effect: Some(DraftEffect::AdditionalPick),
                };
            }
            pack
        }
    }

    /// [`fixture_source`], counting every booster it is asked to generate.
    struct CountingSource {
        calls: Cell<usize>,
    }

    impl PackSource for CountingSource {
        fn generate_pack(
            &self,
            rng: &mut dyn rand::RngCore,
            seat: u8,
            pack_number: u8,
        ) -> DraftPack {
            self.calls.set(self.calls.get() + 1);
            fixture_source().generate_pack(rng, seat, pack_number)
        }
    }

    fn start(
        config: DraftConfig,
        seats: Vec<DraftSeat>,
        source: &dyn PackSource,
        difficulty: AiDifficulty,
    ) -> Result<LocalFifoSession, String> {
        start_local_fifo_draft(
            config,
            seats,
            "quick-draft".to_string(),
            source,
            difficulty,
            LocalLlmDrafting::EngineBots,
            1_000,
        )
    }

    /// The standard local session: 8 seats over [`fixture_source`].
    fn start_fixture(difficulty: AiDifficulty) -> LocalFifoSession {
        start(
            local_quick_config(set_source(), 8, 3, 0),
            local_quick_seats(8),
            &fixture_source(),
            difficulty,
        )
        .expect("the local Quick shape starts")
    }

    /// The standard local session over [`LibrarianSource`].
    fn start_librarian() -> LocalFifoSession {
        start(
            local_quick_config(set_source(), 8, 3, 0),
            local_quick_seats(8),
            &LibrarianSource,
            AiDifficulty::Medium,
        )
        .expect("the local Quick shape starts")
    }

    /// A Cogwork Librarian session after the human drafts the Librarian with
    /// their first pick: the owner, and the view that selection returned.
    fn librarian_drafted() -> (LocalOwner, DraftPlayerView) {
        let local = start_librarian();
        let payload = selection_json(
            &live_prompt(&local.view),
            &[LIBRARIAN.to_string()],
            &[],
            &[],
        );
        let view = local_fifo_select_inner(local.owner, &payload, 2_000)
            .expect("drafting the Librarian is a legal selection");
        (local.owner, view)
    }

    /// A session in the local shape built by draft-core's own constructors under
    /// `id`, started at 1000 ms with no bot batch run.
    fn core_session(config: DraftConfig, seats: Vec<DraftSeat>, id: &str) -> DraftSession {
        let mut session = DraftSession::new_fifo(
            config,
            seats,
            "quick-draft".to_string(),
            DraftSessionId(id.to_string()),
        )
        .expect("draft-core admits the configuration");
        session::apply_at(
            &mut session,
            DraftAction::StartDraft,
            Some(&fixture_source()),
            1_000,
        )
        .expect("the session starts");
        session
    }

    fn installed() -> DraftSession {
        session_cell::with_installed(DraftSession::clone)
    }

    /// Not drafting, or no bot seat holds a head pack while the human does.
    fn bots_quiescent(session: &DraftSession) -> bool {
        let seat_holds_a_head =
            |index: usize| session.head_pack(u8::try_from(index).unwrap()).is_some();
        session.status != DraftStatus::Drafting
            || (session.seats.iter().enumerate().all(|(index, seat)| {
                !matches!(seat, DraftSeat::Bot { .. }) || !seat_holds_a_head(index)
            }) && seat_holds_a_head(usize::from(LOCAL_HUMAN_SEAT)))
    }

    /// Every bot seat's completed steps, as the view publishes them.
    fn bot_steps(view: &DraftPlayerView) -> u64 {
        view.seats
            .iter()
            .filter(|seat| seat.is_bot)
            .map(|seat| {
                seat.fifo_progress
                    .as_ref()
                    .expect("a FIFO view publishes every seat's progress")
                    .completed_steps
            })
            .sum()
    }

    fn live_prompt(view: &DraftPlayerView) -> DraftPromptId {
        view.fifo
            .as_ref()
            .and_then(|fifo| fifo.prompt_id.clone())
            .expect("the human holds a live prompt")
    }

    fn ids(cards: &[DraftCardInstance]) -> Vec<String> {
        cards.iter().map(|card| card.instance_id.clone()).collect()
    }

    fn names(cards: &[DraftCardInstance]) -> Vec<String> {
        cards.iter().map(|card| card.name.clone()).collect()
    }

    fn head_ids(view: &DraftPlayerView) -> Vec<String> {
        ids(view
            .current_pack
            .as_deref()
            .expect("the human holds a head pack"))
    }

    fn selection_json(
        prompt_id: &DraftPromptId,
        picked: &[String],
        burned: &[String],
        effects: &[String],
    ) -> String {
        serde_json::json!({
            "prompt_id": prompt_id,
            "picked_card_instance_ids": picked,
            "burned_card_instance_ids": burned,
            "effect_card_instance_ids": effects,
        })
        .to_string()
    }

    fn prompt_json(prompt_id: &DraftPromptId) -> String {
        serde_json::to_string(prompt_id).unwrap()
    }

    fn value(view: &DraftPlayerView) -> serde_json::Value {
        serde_json::to_value(view).unwrap()
    }

    /// The runtime never issues owner 0, so it is never the live owner.
    fn retired_owner() -> LocalOwner {
        LocalOwner::from_raw(0)
    }

    fn deck_total(deck: &suggest::SuggestedDeck) -> usize {
        deck.main_deck.len() + deck.lands.values().map(|&n| usize::from(n)).sum::<usize>()
    }

    /// Auto-select on the live prompt until the draft leaves `Drafting`,
    /// handing every returned view to `after_each`. The human drafts 9 cards, so
    /// 27 calls is a safety cap, not an expectation.
    fn draft_every_pick(
        owner: LocalOwner,
        mut view: DraftPlayerView,
        mut after_each: impl FnMut(&DraftPlayerView),
    ) -> DraftPlayerView {
        let mut calls: u64 = 0;
        while view.status == DraftStatus::Drafting {
            assert!(calls < 27, "the draft did not finish within 27 selections");
            calls += 1;
            view = local_fifo_auto_select_inner(
                owner,
                &prompt_json(&live_prompt(&view)),
                1_000 + calls,
            )
            .expect("an auto-pick on the live prompt is accepted");
            after_each(&view);
        }
        view
    }

    fn reset() {
        clear_state();
        DIFFICULTY.with(|cell| cell.set(AiDifficulty::Medium));
    }

    /// Once a second local session is installed, the first one's owner neither
    /// reads nor acts, and a legacy install retires the second one's owner.
    #[test]
    fn a_stale_local_owner_neither_reads_nor_acts() {
        reset();
        let a = start_fixture(AiDifficulty::Medium).owner;
        assert!(
            local_fifo_view_inner(a).is_ok(),
            "reach: the live owner reads"
        );

        let b = start_fixture(AiDifficulty::Medium);
        assert_ne!(a, b.owner);
        // B's live prompt and a card of B's live head: only the owner check can
        // refuse this payload, so a stale prompt cannot mask the gate.
        let payload = selection_json(&live_prompt(&b.view), &head_ids(&b.view)[..1], &[], &[]);
        let before = guard_cells_snapshot();
        assert_eq!(local_fifo_view_inner(a).unwrap_err(), STALE_LOCAL_OWNER);
        assert_eq!(guard_cells_snapshot(), before);
        assert_eq!(
            local_fifo_select_inner(a, &payload, 2_000).unwrap_err(),
            STALE_LOCAL_OWNER
        );
        assert_eq!(guard_cells_snapshot(), before);
        assert_eq!(
            export_local_fifo_session_inner(a).unwrap_err(),
            STALE_LOCAL_OWNER
        );
        assert_eq!(guard_cells_snapshot(), before);

        assert!(local_fifo_view_inner(b.owner).is_ok());
        assert!(
            local_fifo_select_inner(b.owner, &payload, 2_000).is_ok(),
            "the same payload is accepted under the live owner"
        );

        let legacy = dormant_fixture(DraftFlow::LegacyBarrier, DraftStatus::Drafting);
        import_draft_session_inner(&serde_json::to_string(&legacy).unwrap(), 2)
            .expect("a legacy save imports");
        assert_eq!(
            local_fifo_view_inner(b.owner).unwrap_err(),
            STALE_LOCAL_OWNER
        );
        reset();
    }

    #[test]
    fn local_sessions_carry_distinct_canonical_ids() {
        reset();
        let installed_id = || {
            session_cell::with_installed(|session| {
                session
                    .fifo
                    .as_ref()
                    .expect("a FIFO session")
                    .session_id
                    .clone()
            })
        };
        start_fixture(AiDifficulty::Medium);
        let first = installed_id();
        start_fixture(AiDifficulty::Medium);
        let second = installed_id();
        for id in [&first, &second] {
            let hex =
                id.0.strip_prefix("local-")
                    .unwrap_or_else(|| panic!("{id:?} does not carry the local prefix"));
            assert_eq!(id.0.len(), 22, "{id:?}");
            assert!(
                hex.bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{id:?}"
            );
        }
        assert_ne!(first, second);
        reset();
    }

    /// Each refused id differs from the accepted one in exactly one property.
    #[test]
    fn canonical_local_session_ids_have_one_shape() {
        reset();
        let id = |text: &str| DraftSessionId(text.to_string());
        assert!(is_canonical_local_session_id(&id(CANONICAL_ID)));
        assert!(
            !is_canonical_local_session_id(&id("local-0123456789ABCDEF")),
            "uppercase digits"
        );
        assert!(
            !is_canonical_local_session_id(&id("local-0123")),
            "too few digits"
        );
        assert!(
            !is_canonical_local_session_id(&id("lokal-0123456789abcdef")),
            "another prefix"
        );
        assert!(
            !is_canonical_local_session_id(&id("local-0123456789abcdeg")),
            "one non-hex digit"
        );
        reset();
    }

    /// Every general-runtime gate refuses an installed local session while its
    /// owner is live, before any closure runs and with no effect.
    #[test]
    fn the_general_runtime_refuses_an_owned_local_session() {
        reset();
        let owner = start_fixture(AiDifficulty::Medium).owner;
        install_fixture_db();
        let before = guard_cells_snapshot();
        let refused = Err(FIFO_UNSUPPORTED_BY_RUNTIME.to_string());

        let entered = Cell::new(0);
        let read = with_draft_inner(|_| {
            entered.set(entered.get() + 1);
            Ok(())
        });
        let write = with_draft_mut_inner(|_| {
            entered.set(entered.get() + 1);
            Ok(())
        });
        assert_eq!(read, refused);
        assert_eq!(write, refused);
        assert_eq!(entered.get(), 0);
        assert_eq!(guard_cells_snapshot(), before);

        let pick = serde_json::to_string(&DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["TST-0-0-0".to_string()],
        })
        .unwrap();
        let assert_refused = |label: &str, result: Result<(), String>| {
            assert_eq!(result, refused, "{label}");
            assert_eq!(guard_cells_snapshot(), before, "{label}");
        };
        assert_refused(
            "submit_pick_for_seat",
            submit_pick_for_seat_inner(0, "[\"TST-0-0-0\"]").map(|_| ()),
        );
        assert_refused("submit_deck", submit_deck_inner("[]", "[]").map(|_| ()));
        assert_refused(
            "submit_deck_for_seat",
            submit_deck_for_seat_inner(0, "[]", "[]").map(|_| ()),
        );
        assert_refused(
            "suggest_lands_for_seat",
            suggest_lands_for_seat_inner(0, "[]").map(|_| ()),
        );
        assert_refused("get_bot_deck", get_bot_deck_inner(1).map(|_| ()));
        assert_refused(
            "booster_pack_pool_for_game",
            booster_pack_pool_for_game_inner().map(|_| ()),
        );
        assert_refused(
            "build_llm_draft_pick_requests",
            build_llm_draft_pick_requests_inner(ENDPOINT, "{}").map(|_| ()),
        );
        assert_refused(
            "submit_pick_with_llm_bot_picks",
            submit_pick_with_llm_bot_picks_inner("TST-0-0-0", "[]").map(|_| ()),
        );
        assert_refused(
            "resolve_shared_stack_bot_turns",
            resolve_shared_stack_bot_turns_inner().map(|_| ()),
        );
        assert_refused(
            "apply_draft_action",
            apply_draft_action_inner(&pick).map(|_| ()),
        );

        assert!(
            local_fifo_view_inner(owner).is_ok(),
            "the same install is reachable through the local gate"
        );
        reset();
    }

    /// The local gates admit before their closures: the live owner alone does
    /// not reach a session the local admission refuses.
    #[test]
    fn the_local_gate_admits_before_its_closure() {
        reset();
        let owner = start_fixture(AiDifficulty::Medium).owner;
        let entered = Cell::new(0);
        let count = |_: &DraftSession| {
            entered.set(entered.get() + 1);
            Ok(())
        };

        session_cell::with_installed_mut(|session| session.config.kind = DraftKind::Premier);
        let refused = Err(NOT_A_LOCAL_FIFO_SESSION.to_string());
        assert_eq!(with_local_fifo_inner(owner, count), refused);
        assert_eq!(
            with_local_fifo_mut_inner(owner, |session| count(&*session)),
            refused
        );
        assert_eq!(entered.get(), 0);

        session_cell::with_installed_mut(|session| session.config.kind = DraftKind::Quick);
        assert_eq!(with_local_fifo_inner(owner, count), Ok(()));
        assert_eq!(
            with_local_fifo_mut_inner(owner, |session| count(&*session)),
            Ok(())
        );
        assert_eq!(entered.get(), 2);
        reset();
    }

    /// Construction admits before it deals and installs nothing on a refusal.
    #[test]
    fn local_construction_refuses_before_dealing_packs() {
        reset();
        let previous = start_fixture(AiDifficulty::Medium).owner;
        let before = guard_cells_snapshot();

        let premier = DraftConfig {
            kind: DraftKind::Premier,
            ..local_quick_config(set_source(), 8, 3, 0)
        };
        let mut second_human = local_quick_seats(8);
        second_human[3] = DraftSeat::Human {
            player_id: PlayerId(3),
            display_name: "Player 3".to_string(),
        };
        let legacy = DraftConfig {
            flow: DraftFlow::LegacyBarrier,
            ..local_quick_config(set_source(), 8, 3, 0)
        };
        // A zero-length pick clock is draft-core's own configuration refusal.
        let zero_clock = DraftConfig {
            clock_policy: DraftClockPolicy {
                pick: PickClockPolicy::Fixed { duration_ms: 0 },
                review_duration_ms: 0,
            },
            ..local_quick_config(set_source(), 8, 3, 0)
        };
        let refusals = [
            ("Premier kind", premier, local_quick_seats(8)),
            (
                "a second human seat",
                local_quick_config(set_source(), 8, 3, 0),
                second_human,
            ),
            ("legacy flow", legacy, local_quick_seats(8)),
            ("zero pick clock", zero_clock, local_quick_seats(8)),
        ];
        for (label, config, seats) in refusals {
            let source = CountingSource {
                calls: Cell::new(0),
            };
            assert!(
                start(config, seats, &source, AiDifficulty::VeryHard).is_err(),
                "{label}"
            );
            assert_eq!(source.calls.get(), 0, "{label}: no booster is dealt");
            assert_eq!(guard_cells_snapshot(), before, "{label}");
            assert!(local_fifo_view_inner(previous).is_ok(), "{label}");
        }

        let source = CountingSource {
            calls: Cell::new(0),
        };
        assert!(start(
            local_quick_config(set_source(), 8, 3, 0),
            local_quick_seats(8),
            &source,
            AiDifficulty::Medium,
        )
        .is_ok());
        // 8 seats x 3 boosters, one generation each.
        assert_eq!(source.calls.get(), 24);
        reset();
    }

    /// The local view is seat 0's projection and publishes every seat's FIFO
    /// progress.
    #[test]
    fn the_local_view_is_seat_zero_and_publishes_every_seats_progress() {
        reset();
        let local = start_fixture(AiDifficulty::Medium);
        let session = installed();
        let view = local_fifo_view_inner(local.owner).expect("the live owner reads");

        assert_eq!(value(&view), value(&filter_for_player(&session, 0)));
        assert_ne!(value(&view), value(&filter_for_player(&session, 1)));
        assert!(bot_steps(&view) > 0, "reach: the bots have drafted");
        // CR 905.1a: a Quick seat drafts one card per step, and the bots spend
        // no effect card, so a seat's completed steps are its pool size.
        for (index, seat) in view.seats.iter().enumerate() {
            let progress = seat
                .fifo_progress
                .as_ref()
                .expect("every seat's progress is published");
            assert_eq!(
                progress.completed_steps,
                u64::try_from(session.pools[index].len()).unwrap(),
                "seat {index}"
            );
        }
        assert_eq!(
            view.fifo
                .as_ref()
                .expect("a FIFO view")
                .progress
                .completed_steps,
            0
        );
        reset();
    }

    #[test]
    fn construction_runs_the_bots_to_quiescence() {
        reset();
        let local = start_fixture(AiDifficulty::Medium);
        let session = installed();
        assert!(bots_quiescent(&session));
        let steps = bot_steps(&local.view);
        assert!(steps > 0);
        // One commit for `StartDraft`, then one per bot application.
        assert_eq!(
            session.fifo.as_ref().expect("a FIFO session").revision,
            1 + steps
        );
        reset();
    }

    #[test]
    fn a_composed_selection_applies_then_the_bots_reach_quiescence() {
        reset();
        let local = start_librarian();
        let fifo = local.view.fifo.as_ref().expect("a FIFO view");
        // Reach: under the default policy a Quick step takes one card and burns
        // none.
        assert_eq!(
            fifo.requirements,
            DraftSelectionRequirements { picks: 1, burns: 0 }
        );
        assert!(head_ids(&local.view).contains(&LIBRARIAN.to_string()));
        let steps_before = bot_steps(&local.view);

        let payload = selection_json(
            &live_prompt(&local.view),
            &[LIBRARIAN.to_string()],
            &[],
            &[],
        );
        let view = local_fifo_select_inner(local.owner, &payload, 2_000)
            .expect("drafting the Librarian is a legal selection");
        assert_eq!(ids(&view.pool), vec![LIBRARIAN.to_string()]);
        assert!(bots_quiescent(&installed()));
        assert!(bot_steps(&view) > steps_before);
        reset();
    }

    /// Cogwork Librarian: "As you draft a card, you may draft an additional card
    /// from that booster pack. If you do, put this card into that booster pack."
    #[test]
    fn a_composed_selection_carries_its_effect_list() {
        reset();
        let (owner, view) = librarian_drafted();
        let fifo = view.fifo.as_ref().expect("a FIFO view");
        let effects = fifo
            .effects
            .as_ref()
            .expect("reach: the Librarian can be used on this head");
        assert_eq!(effects.card_instance_ids, vec![LIBRARIAN.to_string()]);
        assert_eq!(
            effects.requirements,
            vec![DraftSelectionRequirements { picks: 2, burns: 0 }]
        );
        let picks = effects.requirements[0].picks;
        let head = head_ids(&view);
        assert_eq!(
            head.len(),
            picks,
            "the head is the 2-card booster seat 7 passed"
        );
        let picked: Vec<String> = head.into_iter().take(picks).collect();

        let payload = selection_json(&live_prompt(&view), &picked, &[], &[LIBRARIAN.to_string()]);
        let after = local_fifo_select_inner(owner, &payload, 3_000)
            .expect("two picks with the Librarian's effect are legal");
        let pool = ids(&after.pool);
        assert!(!pool.contains(&LIBRARIAN.to_string()));
        assert!(picked.iter().all(|id| pool.contains(id)));

        let session = installed();
        let in_a_bot_pool = session
            .seats
            .iter()
            .zip(&session.pools)
            .filter(|(seat, _)| matches!(seat, DraftSeat::Bot { .. }))
            .flat_map(|(_, pool)| pool)
            .any(|card| card.instance_id == LIBRARIAN);
        let in_a_queue = session
            .fifo
            .as_ref()
            .expect("a FIFO session")
            .seats
            .iter()
            .flat_map(|seat| &seat.queue)
            .flat_map(|queued| &queued.pack.0)
            .any(|card| card.instance_id == LIBRARIAN);
        assert!(
            in_a_bot_pool || in_a_queue,
            "the Librarian went back into the booster"
        );
        reset();
    }

    #[test]
    fn auto_select_is_prompt_bound_and_spends_no_effect_card() {
        reset();
        let (owner, view) = librarian_drafted();
        assert!(
            view.fifo.as_ref().expect("a FIFO view").effects.is_some(),
            "reach: the Librarian could be spent on this head"
        );
        let after = local_fifo_auto_select_inner(owner, &prompt_json(&live_prompt(&view)), 3_000)
            .expect("an auto-pick on the live prompt is accepted");
        assert_eq!(after.pool.len(), 2);
        assert!(ids(&after.pool).contains(&LIBRARIAN.to_string()));
        reset();
    }

    /// `VeryEasy` is the one difficulty whose bot draws from the random state,
    /// so it is the one under which a draw before the refusal would show.
    #[test]
    fn a_stale_prompt_changes_nothing() {
        reset();
        let local = start_fixture(AiDifficulty::VeryEasy);
        let stale = live_prompt(&local.view);

        let before = guard_cells_snapshot();
        let view = local_fifo_auto_select_inner(local.owner, &prompt_json(&stale), 2_000)
            .expect("an auto-pick on the live prompt is accepted");
        let after = guard_cells_snapshot();
        assert_ne!(
            after["rng"], before["rng"],
            "reach: the snapshot sees the random state move"
        );
        let head = head_ids(&view);
        assert_eq!(head.len(), 2, "reach: a refused auto-pick would have drawn");

        let assert_stale = |label: &str, result: Result<DraftPlayerView, String>| {
            let error = result.expect_err(label);
            assert!(error.contains("stale draft prompt"), "{label}: {error}");
            assert_eq!(guard_cells_snapshot(), after, "{label}");
        };
        assert_stale(
            "consumed prompt",
            local_fifo_select_inner(
                local.owner,
                &selection_json(&stale, &head[..1], &[], &[]),
                3_000,
            ),
        );
        let other_seat = DraftPromptId {
            seat: 1,
            ..live_prompt(&view)
        };
        assert_stale(
            "another seat's prompt",
            local_fifo_select_inner(
                local.owner,
                &selection_json(&other_seat, &head[..1], &[], &[]),
                3_000,
            ),
        );
        assert_stale(
            "consumed prompt, auto-pick",
            local_fifo_auto_select_inner(local.owner, &prompt_json(&stale), 3_000),
        );
        reset();
    }

    #[test]
    fn a_selection_payload_naming_a_seat_is_refused_before_the_session() {
        reset();
        let local = start_librarian();
        let mut payload: serde_json::Value = serde_json::from_str(&selection_json(
            &live_prompt(&local.view),
            &[LIBRARIAN.to_string()],
            &[],
            &[],
        ))
        .unwrap();
        payload["seat"] = serde_json::json!(3);
        let before = guard_cells_snapshot();
        let error = local_fifo_select_inner(local.owner, &payload.to_string(), 2_000)
            .expect_err("a payload naming a seat is refused");
        assert!(error.contains("unknown field `seat`"), "{error}");
        assert_eq!(guard_cells_snapshot(), before);

        payload.as_object_mut().unwrap().remove("seat");
        assert!(local_fifo_select_inner(local.owner, &payload.to_string(), 2_000).is_ok());
        reset();
    }

    #[test]
    fn bots_select_by_the_engine_requirements() {
        reset();
        // A policy burn: every bot step on a 3- or 2-card head must burn one.
        start(
            local_quick_config(set_source(), 8, 3, 1),
            local_quick_seats(8),
            &fixture_source(),
            AiDifficulty::Medium,
        )
        .expect("the bots burn what the policy requires");
        let session = installed();
        let removed = &session.fifo.as_ref().expect("a FIFO session").removed;
        assert!(!removed.is_empty());
        assert!(removed
            .iter()
            .all(|card| card.reason == DraftRemovalReason::Burn));
        reset();

        // CR 903.13b: a Commander Draft step takes two cards, a count no Quick
        // head can publish. This session is only a projection input; the local
        // gates never admit it.
        let config = DraftConfig {
            flow: DraftFlow::FifoAsync,
            selection_policy: DraftSelectionPolicy::default(),
            clock_policy: DraftClockPolicy::default(),
            source: set_source(),
            set_code: "TST".to_string(),
            kind: DraftKind::CommanderDraft,
            pod_size: 4,
            cards_per_pack: 6,
            pack_count: 2,
            min_deck_size: DraftKind::CommanderDraft.procedure().min_deck_size,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats = (0..4)
            .map(|seat| DraftSeat::Human {
                player_id: PlayerId(seat),
                display_name: format!("Player {seat}"),
            })
            .collect();
        let mut commander = DraftSession::new_fifo(
            config,
            seats,
            "commander-draft".to_string(),
            DraftSessionId("commander-draft".to_string()),
        )
        .expect("a Commander Draft FIFO pod is admitted");
        session::apply_at(
            &mut commander,
            DraftAction::StartDraft,
            Some(&FixturePackSource {
                set_code: "TST".to_string(),
                cards_per_pack: 6,
            }),
            1_000,
        )
        .expect("the pod starts");
        let view = filter_for_player(&commander, 1);
        let fifo = view.fifo.as_ref().expect("a FIFO view");
        assert_eq!(fifo.requirements.picks, 2);
        let action = fifo_ai_selection(
            &view,
            1,
            live_prompt(&view),
            AiDifficulty::Medium,
            None,
            bot_ai::PickEnhancement::Off,
            &mut ChaCha20Rng::seed_from_u64(7),
        )
        .expect("seat 1 holds a head");
        let DraftAction::Select {
            picked_card_instance_ids,
            ..
        } = &action
        else {
            panic!("the engine's selection is a Select, got {action:?}");
        };
        assert_eq!(picked_card_instance_ids.len(), 2);
        session::apply_at(&mut commander, action, None, 1_100)
            .expect("the engine accepts its own selection");
        reset();
    }

    #[test]
    fn the_bot_batch_fails_loudly_rather_than_spinning() {
        reset();
        let session = core_session(
            local_quick_config(set_source(), 8, 3, 0),
            local_quick_seats(8),
            CANONICAL_ID,
        );
        assert!(!bots_quiescent(&session), "reach: the bots hold heads");
        // 8 seats x 3 boosters x 3 cards undrafted, plus one.
        assert_eq!(local_fifo_bot_batch_bound(&session), 73);

        let settings = LocalBotSettings {
            difficulty: AiDifficulty::Medium,
            card_db: None,
            enhancement: bot_ai::PickEnhancement::Off,
            drafting: LocalLlmDrafting::EngineBots,
        };
        let mut starved = session.clone();
        let error = drive_local_fifo_bot_batch(
            &mut starved,
            &settings,
            &mut ChaCha20Rng::seed_from_u64(1),
            1_000,
            1,
        )
        .expect_err("a bound of 1 trips on a multi-selection batch");
        assert!(error.contains("termination bound"), "{error}");

        // Paired positive: the same state runs clean under the derived bound.
        let mut healthy = session;
        let bound = local_fifo_bot_batch_bound(&healthy);
        drive_local_fifo_bot_batch(
            &mut healthy,
            &settings,
            &mut ChaCha20Rng::seed_from_u64(1),
            1_000,
            bound,
        )
        .expect("the derived bound is not reachable from a legal state");
        assert!(bots_quiescent(&healthy));
        reset();
    }

    /// 14-card boosters give the start batch enough random draws on large heads
    /// that two equal outcomes from an unseeded state would be a coincidence.
    #[test]
    fn the_bot_batch_is_deterministic() {
        reset();
        let source = FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let construct = || {
            start(
                local_quick_config(set_source(), 8, 14, 0),
                local_quick_seats(8),
                &source,
                AiDifficulty::VeryEasy,
            )
            .expect("the local Quick shape starts");
            installed()
        };
        let first = construct();
        let second = construct();
        let pools = |session: &DraftSession| -> Vec<Vec<String>> {
            session.pools.iter().map(|pool| ids(pool)).collect()
        };
        let progress = |session: &DraftSession| -> Vec<u64> {
            session
                .fifo
                .as_ref()
                .expect("a FIFO session")
                .seats
                .iter()
                .map(|seat| seat.completed_steps)
                .collect()
        };
        assert!(
            progress(&first).iter().sum::<u64>() > 0,
            "reach: the bots drafted"
        );
        assert_eq!(pools(&first), pools(&second));
        assert_eq!(progress(&first), progress(&second));
        reset();
    }

    /// CR 905.1a + CR 905.1b: the human drafts every round while the bots keep
    /// up after every selection.
    #[test]
    fn the_human_drafts_every_round_while_bots_stay_quiescent() {
        reset();
        let local = start_fixture(AiDifficulty::Medium);
        let mut rounds = std::collections::BTreeSet::new();
        let end = draft_every_pick(local.owner, local.view, |view| {
            assert!(bots_quiescent(&installed()));
            if view.status == DraftStatus::Drafting {
                rounds.insert(view.current_pack_number);
            }
        });
        assert_eq!(rounds, std::collections::BTreeSet::from([0, 1, 2]));
        assert_eq!(end.status, DraftStatus::Deckbuilding);
        assert!(installed().pools.iter().all(|pool| pool.len() == 9));
        reset();
    }

    /// CR 100.2b: a limited deck has a minimum deck size of 40 cards, built
    /// from the product plus basic lands.
    #[test]
    fn deck_submission_runs_at_explicit_time() {
        reset();
        let local = start_fixture(AiDifficulty::Medium);
        let owner = local.owner;
        let end = draft_every_pick(owner, local.view, |_| {});
        assert_eq!(end.status, DraftStatus::Deckbuilding);
        let pool = names(&end.pool);
        assert_eq!(pool.len(), 9);
        let with_plains = |count: usize| {
            let mut deck = pool.clone();
            deck.extend(std::iter::repeat_n("Plains".to_string(), count));
            serde_json::to_string(&deck).unwrap()
        };

        let before = guard_cells_snapshot();
        let short = local_fifo_submit_deck_inner(owner, &with_plains(30), "[]", 9_000_000)
            .expect_err("a 39-card deck is refused");
        assert!(short.contains("minimum is 40"), "{short}");
        assert_eq!(guard_cells_snapshot(), before);
        let unreadable =
            local_fifo_submit_deck_inner(owner, &with_plains(31), "not json", 9_000_000)
                .expect_err("an unreadable designation is refused");
        assert!(
            unreadable.contains("Failed to parse commanders"),
            "{unreadable}"
        );
        assert_eq!(guard_cells_snapshot(), before);
        assert_eq!(
            local_fifo_submit_deck_inner(retired_owner(), &with_plains(31), "[]", 9_000_000)
                .unwrap_err(),
            STALE_LOCAL_OWNER
        );
        assert_eq!(guard_cells_snapshot(), before);

        // 9_000_000 exceeds every earlier time in this draft, so only the
        // caller's time can put it in the session.
        let view = local_fifo_submit_deck_inner(owner, &with_plains(31), "[]", 9_000_000)
            .expect("40 cards from the pool and basic lands are accepted");
        assert_eq!(view.status, DraftStatus::Complete);
        let session = installed();
        assert_eq!(
            session.fifo.as_ref().expect("a FIFO session").last_now_ms,
            9_000_000
        );
        session
            .validate_persisted_snapshot()
            .expect("the completed session is a valid snapshot");
        reset();
    }

    /// Suggestions are not card-derived: a suggested deck is exactly the 40-card
    /// floor (CR 100.2b) from the pool plus basics, and a land suggestion is
    /// clamped to 16-18 lands.
    #[test]
    fn suggestions_and_bot_decks_reach_the_local_owner() {
        reset();
        let local = start_fixture(AiDifficulty::Medium);
        let owner = local.owner;
        let end = draft_every_pick(owner, local.view, |_| {});
        let pool = names(&end.pool);

        let suggested = local_fifo_suggest_deck_inner(owner).expect("the owner's suggestion");
        assert!(suggested.main_deck.iter().all(|name| pool.contains(name)));
        assert_eq!(deck_total(&suggested), 40);
        assert!(suggested.commander.is_empty());

        let lands = local_fifo_suggest_lands_inner(owner, &serde_json::to_string(&pool).unwrap())
            .expect("the owner's land suggestion");
        // clamp(40 - 9, 16, 18)
        assert_eq!(lands.values().map(|&n| usize::from(n)).sum::<usize>(), 18);

        let bot = local_fifo_bot_deck_inner(owner, 1).expect("seat 1's deck");
        let bot_pool = names(&installed().pools[1]);
        assert_eq!(deck_total(&bot), 40);
        assert!(bot.commander.is_empty());
        assert!(bot.main_deck.iter().all(|name| bot_pool.contains(name)));
        for seat in [0, 8] {
            let error = local_fifo_bot_deck_inner(owner, seat).unwrap_err();
            assert!(error.contains("out of range"), "seat {seat}: {error}");
        }

        let stale = retired_owner();
        assert_eq!(
            local_fifo_suggest_deck_inner(stale).unwrap_err(),
            STALE_LOCAL_OWNER
        );
        assert_eq!(
            local_fifo_suggest_lands_inner(stale, "[]").unwrap_err(),
            STALE_LOCAL_OWNER
        );
        assert_eq!(
            local_fifo_bot_deck_inner(stale, 1).unwrap_err(),
            STALE_LOCAL_OWNER
        );
        reset();
    }

    /// Not card-derived: a cube draft's game uses the cube's original multiset,
    /// and a set draft has none.
    #[test]
    fn the_booster_pack_pool_reaches_the_local_owner() {
        reset();
        let set = start_fixture(AiDifficulty::Medium);
        assert_eq!(
            local_fifo_booster_pack_pool_for_game_inner(set.owner).unwrap(),
            None
        );

        // 2 seats x 3 boosters x 2 cards: the cube's whole multiset is dealt.
        let cube_cards: Vec<DraftCardInstance> = (0..12)
            .map(|index| DraftCardInstance {
                instance_id: format!("cube-source-{index}"),
                name: format!("Cube Card {index}"),
                set_code: "CUBE".to_string(),
                collector_number: format!("{index}"),
                rarity: "cube".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect();
        let cube_names = names(&cube_cards);
        let cube = start(
            local_quick_config(
                DraftSource::Cube {
                    id: "custom-cube".to_string(),
                    name: "Fixture Cube".to_string(),
                },
                2,
                2,
                0,
            ),
            local_quick_seats(2),
            &CubePackSource::new(cube_cards),
            AiDifficulty::Medium,
        )
        .expect("a two-seat Quick cube draft starts");
        assert_eq!(
            local_fifo_booster_pack_pool_for_game_inner(cube.owner).unwrap(),
            Some(cube_names)
        );
        assert_eq!(
            local_fifo_booster_pack_pool_for_game_inner(retired_owner()).unwrap_err(),
            STALE_LOCAL_OWNER
        );
        reset();
    }

    #[test]
    fn a_local_export_imports_and_is_re_owned() {
        reset();
        let local = start_fixture(AiDifficulty::Medium);
        let a = local.owner;
        let mut view = local.view;
        for now_ms in [2_000, 3_000] {
            view = local_fifo_auto_select_inner(a, &prompt_json(&live_prompt(&view)), now_ms)
                .expect("an auto-pick on the live prompt is accepted");
        }

        let json = export_local_fifo_session_inner(a).expect("the owner exports");
        assert_eq!(
            restorable_draft_session_verdict(&json),
            RestorableDraftSessionVerdict::Incompatible
        );
        let exported: DraftSession = serde_json::from_str(&json).unwrap();

        let imported = import_local_fifo_session_inner(&json, 0).expect("the export imports");
        let c = imported.owner;
        assert_ne!(c, a);
        assert_eq!(installed(), exported);
        assert_eq!(
            value(&imported.view),
            value(&filter_for_player(&exported, 0))
        );
        let revision = exported.fifo.as_ref().expect("a FIFO session").revision;
        let mut resumed =
            ChaCha20Rng::seed_from_u64(exported.config.rng_seed.wrapping_add(revision));
        let words: Vec<u64> = (0..4).map(|_| resumed.next_u64()).collect();
        assert_eq!(guard_cells_snapshot()["rng"], serde_json::json!(words));
        assert_eq!(DIFFICULTY.with(|cell| cell.get()), AiDifficulty::VeryEasy);

        assert_eq!(local_fifo_view_inner(a).unwrap_err(), STALE_LOCAL_OWNER);
        let payload = selection_json(
            &live_prompt(&imported.view),
            &head_ids(&imported.view)[..1],
            &[],
            &[],
        );
        assert!(local_fifo_select_inner(c, &payload, 4_000).is_ok());
        reset();
    }

    #[test]
    fn a_local_import_refuses_before_installing() {
        reset();
        let a = start_fixture(AiDifficulty::Medium).owner;
        let quick = || local_quick_config(set_source(), 8, 3, 0);
        let positive = core_session(quick(), local_quick_seats(8), CANONICAL_ID);
        let as_json = |session: &DraftSession| serde_json::to_string(session).unwrap();
        let mut second_human = local_quick_seats(8);
        second_human[3] = DraftSeat::Human {
            player_id: PlayerId(3),
            display_name: "Player 3".to_string(),
        };
        let all_bots = (0..8)
            .map(|index| DraftSeat::Bot {
                name: format!("Bot {index}"),
            })
            .collect();
        let mut no_seats = serde_json::to_value(&positive).unwrap();
        no_seats["seats"] = serde_json::json!([]);
        let mut overflow: serde_json::Value =
            serde_json::from_str(&export_local_fifo_session_inner(a).unwrap()).unwrap();
        overflow["fifo"]["revision"] = serde_json::json!(MAX_DRAFT_JSON_INTEGER + 1);

        let hostiles = [
            ("unreadable text", "{".to_string()),
            (
                "a legacy save",
                as_json(&dormant_fixture(
                    DraftFlow::LegacyBarrier,
                    DraftStatus::Drafting,
                )),
            ),
            (
                "the Premier kind",
                as_json(&core_session(
                    DraftConfig {
                        kind: DraftKind::Premier,
                        ..quick()
                    },
                    local_quick_seats(8),
                    CANONICAL_ID,
                )),
            ),
            (
                "a short session id",
                as_json(&core_session(quick(), local_quick_seats(8), "local-0123")),
            ),
            (
                "a second human seat",
                as_json(&core_session(quick(), second_human, CANONICAL_ID)),
            ),
            ("no seats", no_seats.to_string()),
            (
                "no human seat",
                as_json(&core_session(quick(), all_bots, CANONICAL_ID)),
            ),
            (
                "a revision past the JSON integer range",
                overflow.to_string(),
            ),
        ];
        let before = guard_cells_snapshot();
        for (label, json) in hostiles {
            assert!(
                import_local_fifo_session_inner(&json, 4).is_err(),
                "{label}"
            );
            assert_eq!(guard_cells_snapshot(), before, "{label}");
            assert!(local_fifo_view_inner(a).is_ok(), "{label}");
        }
        assert!(import_local_fifo_session_inner(&as_json(&positive), 4).is_ok());
        reset();
    }

    #[test]
    fn legacy_import_still_refuses_a_local_export() {
        reset();
        let a = start_fixture(AiDifficulty::Medium).owner;
        let json = export_local_fifo_session_inner(a).expect("the owner exports");
        let before = guard_cells_snapshot();
        assert_eq!(
            import_draft_session_inner(&json, 4).unwrap_err(),
            FIFO_UNSUPPORTED_BY_RUNTIME
        );
        assert_eq!(guard_cells_snapshot(), before);
        assert!(local_fifo_view_inner(a).is_ok());
        reset();
    }

    // ── LLM seats ────────────────────────────────────────────────────────────

    /// The standard local shape, 8 seats over `cards_per_pack`-card textless
    /// `TST` boosters burning `burns_per_pick` per step, whose bot seats are
    /// LLM drafters from the start.
    fn start_llm(cards_per_pack: u8, burns_per_pick: u8) -> LocalFifoSession {
        start_local_fifo_draft(
            local_quick_config(set_source(), 8, cards_per_pack, burns_per_pick),
            local_quick_seats(8),
            "quick-draft".to_string(),
            &FixturePackSource {
                set_code: "TST".to_string(),
                cards_per_pack,
            },
            AiDifficulty::Medium,
            LocalLlmDrafting::LlmSeats,
            1_000,
        )
        .expect("the local Quick shape starts with LLM seats")
    }

    /// A well-formed OpenAI-compatible reply body choosing option `choice`.
    fn reply_choosing(choice: usize) -> String {
        let content = format!(r#"{{"choice": {choice}, "reason": "test"}}"#);
        serde_json::json!({ "choices": [{ "message": { "role": "assistant", "content": content } }] })
            .to_string()
    }

    /// A transport's reply payload: an OpenAI-compatible provider's `status`
    /// and `body`, carrying the request's `fingerprint`.
    fn reply(fingerprint: &str, status: u16, body: String) -> serde_json::Value {
        serde_json::json!({
            "fingerprint": fingerprint,
            "provider": "OpenAiCompatible",
            "status": status,
            "body": body,
        })
    }

    /// The resolution payload for `prompt_id`, with `reply` or `null`.
    fn resolution_json(prompt_id: &DraftPromptId, reply: Option<serde_json::Value>) -> String {
        serde_json::json!({ "promptId": prompt_id, "reply": reply }).to_string()
    }

    /// `seat`'s completed steps in `session`.
    fn steps(session: &DraftSession, seat: u8) -> u64 {
        session.fifo.as_ref().expect("a FIFO session").seats[usize::from(seat)].completed_steps
    }

    /// `seat`'s projected head pack.
    fn projected_head(view: &DraftPlayerView) -> Vec<DraftCardInstance> {
        view.current_pack
            .clone()
            .expect("the seat holds a head pack")
    }

    /// The card the engine bot drafts for `seat` on `view`, drawn from a copy
    /// of the stored random state as the fallback draws it.
    fn engine_bot_choice(view: &DraftPlayerView, seat: u8) -> String {
        let mut rng = RNG
            .with(|cell| {
                let stored = cell.take();
                let copy = stored.clone();
                cell.set(stored);
                copy
            })
            .expect("the random state is installed");
        let action = fifo_ai_selection(
            view,
            seat,
            live_prompt(view),
            DIFFICULTY.with(Cell::get),
            None,
            PICK_ENHANCEMENT.with(Cell::get),
            &mut rng,
        )
        .expect("the seat holds a head");
        let DraftAction::Select {
            picked_card_instance_ids,
            ..
        } = &action
        else {
            panic!("the engine's selection is a Select, got {action:?}");
        };
        picked_card_instance_ids[0].clone()
    }

    /// The index of the first projected card that is not `avoid`.
    fn option_other_than(projected: &[DraftCardInstance], avoid: &str) -> usize {
        projected
            .iter()
            .position(|card| card.instance_id != avoid)
            .expect("a choice head holds more than one card")
    }

    /// CR 905.2a: there is no priority in a draft, so the human's selection
    /// never waits on another seat, and under LLM seats no bot seat's choice is
    /// made by the engine while its drafter has not answered. A pack arriving
    /// behind a seat's head leaves that seat's prompt as it was.
    #[test]
    fn llm_seats_hold_their_heads_while_the_human_selects() {
        reset();
        // Paired positive: the same start under engine bots drafts the bots.
        let engine = start_fixture(AiDifficulty::Medium);
        assert!(bot_steps(&engine.view) > 0);

        let local = start_llm(3, 0);
        let session = installed();
        for seat in 1..8 {
            let view = filter_for_player(&session, seat);
            assert_eq!(
                view.current_pack.as_ref().map(Vec::len),
                Some(3),
                "seat {seat}"
            );
            assert!(
                view.fifo
                    .as_ref()
                    .and_then(|fifo| fifo.prompt_id.as_ref())
                    .is_some(),
                "reach: seat {seat} holds a live prompt"
            );
        }
        assert_eq!(bot_steps(&local.view), 0);
        // The `StartDraft` commit alone: no bot application.
        assert_eq!(session.fifo.as_ref().expect("a FIFO session").revision, 1);

        let seat_one_prompt = live_prompt(&filter_for_player(&session, 1));
        let picked = head_ids(&local.view)[0].clone();
        let view = local_fifo_select_inner(
            local.owner,
            &selection_json(
                &live_prompt(&local.view),
                std::slice::from_ref(&picked),
                &[],
                &[],
            ),
            2_000,
        )
        .expect("the human's selection applies while every drafter is out");
        assert_eq!(ids(&view.pool), vec![picked]);
        assert_eq!(bot_steps(&view), 0);
        let seat_one = filter_for_player(&installed(), 1);
        let fifo = seat_one.fifo.as_ref().expect("a FIFO projection");
        assert_eq!(fifo.queue_count, 2);
        assert_eq!(fifo.prompt_id.as_ref(), Some(&seat_one_prompt));
        reset();
    }

    /// CR 905.1a: a step that takes every card left is no choice. Two-card
    /// boosters make one: after seats 1 and 2 each select from their own
    /// booster, seat 2's next head is seat 1's last card, while seat 3 still
    /// holds a real choice.
    #[test]
    fn a_forced_head_is_taken_by_the_engine_bot_while_choices_wait() {
        reset();
        let mut session = DraftSession::new_fifo(
            local_quick_config(set_source(), 8, 2, 0),
            local_quick_seats(8),
            "quick-draft".to_string(),
            DraftSessionId(CANONICAL_ID.to_string()),
        )
        .expect("draft-core admits the configuration");
        session::apply_at(
            &mut session,
            DraftAction::StartDraft,
            Some(&FixturePackSource {
                set_code: "TST".to_string(),
                cards_per_pack: 2,
            }),
            1_000,
        )
        .expect("the session starts");
        for seat in [1, 2] {
            let view = filter_for_player(&session, seat);
            let first = projected_head(&view)[0].instance_id.clone();
            session::apply_at(
                &mut session,
                DraftAction::Select {
                    seat,
                    prompt_id: live_prompt(&view),
                    picked_card_instance_ids: vec![first],
                    burned_card_instance_ids: Vec::new(),
                    effect_card_instance_ids: Vec::new(),
                },
                None,
                1_100,
            )
            .expect("a selection from the seat's own booster is legal");
        }

        let forced = filter_for_player(&session, 2);
        assert_eq!(forced.current_pack.as_ref().map(Vec::len), Some(1));
        assert!(matches!(draft_pick_step(&forced), DraftPickStep::Forced));
        let choice = filter_for_player(&session, 3);
        assert_eq!(choice.current_pack.as_ref().map(Vec::len), Some(2));
        assert!(matches!(
            draft_pick_step(&choice),
            DraftPickStep::Choice { .. }
        ));
        assert_eq!(steps(&session, 2), 1);

        let bound = local_fifo_bot_batch_bound(&session);
        drive_local_fifo_bot_batch(
            &mut session,
            &LocalBotSettings {
                difficulty: AiDifficulty::Medium,
                card_db: None,
                enhancement: bot_ai::PickEnhancement::Off,
                drafting: LocalLlmDrafting::LlmSeats,
            },
            &mut ChaCha20Rng::seed_from_u64(1),
            1_200,
            bound,
        )
        .expect("the batch takes the forced head");
        assert_eq!(steps(&session, 2), 2);
        for seat in 3..8 {
            assert_eq!(steps(&session, seat), 0, "seat {seat}");
        }
        reset();
    }

    /// CR 905.1c: each request is built from its seat's own projection, only
    /// for heads that wait for a drafter, and building one changes nothing.
    #[test]
    fn requests_are_built_for_held_heads_only_and_change_nothing() {
        reset();
        let local = start_llm(3, 0);
        let session = installed();
        assert!(
            filter_for_player(&session, 1)
                .fifo
                .as_ref()
                .and_then(|fifo| fifo.prompt_id.as_ref())
                .is_some(),
            "reach: seat 1 holds a live prompt"
        );

        let before = guard_cells_snapshot();
        let requests = local_fifo_build_llm_requests_inner(local.owner, ENDPOINT, "{}")
            .expect("the owner builds the requests");
        assert_eq!(guard_cells_snapshot(), before);
        let seats: Vec<u8> = requests
            .iter()
            .map(|entry| match entry {
                LocalLlmPickRequest::Request { seat, .. } => *seat,
                LocalLlmPickRequest::Unbuildable { seat, error, .. } => {
                    panic!("seat {seat} is unbuildable: {error}")
                }
            })
            .collect();
        assert_eq!(seats, (1..8).collect::<Vec<u8>>());
        for entry in &requests {
            let LocalLlmPickRequest::Request {
                seat,
                prompt_id,
                fingerprint,
                option_count,
                required_pick_count,
                ..
            } = entry
            else {
                panic!("{entry:?}");
            };
            let view = filter_for_player(&session, *seat);
            assert_eq!(
                Some(prompt_id),
                view.fifo.as_ref().and_then(|fifo| fifo.prompt_id.as_ref()),
                "seat {seat}"
            );
            assert_eq!(
                *fingerprint,
                phase_llm::pick_fingerprint(*seat, &projected_head(&view)),
                "seat {seat}"
            );
            assert_eq!(*option_count, 3, "seat {seat}");
            assert_eq!(*required_pick_count, 1, "seat {seat}");
        }
        let serialized = serde_json::to_value(&requests[0]).unwrap();
        assert_eq!(serialized["kind"], "request");
        assert!(serialized.get("promptId").is_some(), "{serialized}");
        assert!(serialized.get("optionCount").is_some(), "{serialized}");

        // Paired: the same draft re-imported holds the same heads, but an
        // import keeps no LLM drafting and runs no bot, so none is requested.
        let json = export_local_fifo_session_inner(local.owner).expect("the owner exports");
        let imported = import_local_fifo_session_inner(&json, 2).expect("the export imports");
        let resumed = installed();
        for seat in 1..8 {
            let view = filter_for_player(&resumed, seat);
            assert!(
                view.fifo
                    .as_ref()
                    .and_then(|fifo| fifo.prompt_id.as_ref())
                    .is_some(),
                "reach: seat {seat} holds a live prompt"
            );
            assert_eq!(
                view.current_pack.as_ref().map(Vec::len),
                Some(3),
                "seat {seat}"
            );
        }
        assert!(
            local_fifo_build_llm_requests_inner(imported.owner, ENDPOINT, "{}")
                .expect("the new owner builds")
                .is_empty()
        );
        reset();
    }

    #[test]
    fn an_endpoint_that_cannot_carry_a_request_names_every_held_seat() {
        reset();
        let local = start_llm(3, 0);
        let built = local_fifo_build_llm_requests_inner(local.owner, ENDPOINT, "{}")
            .expect("the owner builds the requests");
        assert_eq!(built.len(), 7);
        assert!(
            built
                .iter()
                .all(|entry| matches!(entry, LocalLlmPickRequest::Request { .. })),
            "reach: a usable endpoint builds every held head's request"
        );

        let session = installed();
        let before = guard_cells_snapshot();
        let entries = local_fifo_build_llm_requests_inner(local.owner, "{}", "{}")
            .expect("an endpoint that does not parse refuses no call");
        assert_eq!(guard_cells_snapshot(), before);
        assert_eq!(entries.len(), 7);
        for (entry, expected) in entries.iter().zip(1u8..) {
            let LocalLlmPickRequest::Unbuildable {
                seat,
                prompt_id,
                error,
            } = entry
            else {
                panic!("{entry:?}");
            };
            assert_eq!(*seat, expected);
            assert!(error.contains("Invalid LLM endpoint config"), "{error}");
            assert_eq!(
                Some(prompt_id),
                filter_for_player(&session, *seat)
                    .fifo
                    .as_ref()
                    .and_then(|fifo| fifo.prompt_id.as_ref()),
                "seat {seat}"
            );
        }
        reset();
    }

    #[test]
    fn a_valid_reply_drafts_the_card_the_model_chose() {
        reset();
        let local = start_llm(3, 0);
        let view = filter_for_player(&installed(), 1);
        let projected = projected_head(&view);
        let bot_choice = engine_bot_choice(&view, 1);
        let choice = option_other_than(&projected, &bot_choice);
        assert_ne!(projected[choice].instance_id, bot_choice);

        let payload = resolution_json(
            &live_prompt(&view),
            Some(reply(
                &phase_llm::pick_fingerprint(1, &projected),
                200,
                reply_choosing(choice),
            )),
        );
        let (_, outcome) = local_fifo_resolve_llm_pick_inner(local.owner, &payload, 2_000)
            .expect("the owner resolves seat 1's head");
        assert!(outcome.used, "{:?}", outcome.error);
        let session = installed();
        assert_eq!(
            ids(&session.pools[1]),
            vec![projected[choice].instance_id.clone()]
        );
        assert_eq!(steps(&session, 1), 1);
        reset();
    }

    #[test]
    fn a_reply_for_a_prompt_that_is_not_live_falls_back_for_that_seat_only() {
        reset();
        let local = start_llm(3, 0);
        let session = installed();
        let view = filter_for_player(&session, 1);
        let projected = projected_head(&view);
        let bot_choice = engine_bot_choice(&view, 1);
        let choice = option_other_than(&projected, &bot_choice);
        let fingerprint = phase_llm::pick_fingerprint(1, &projected);
        // Reach: on the live projection this fingerprint and body decode to
        // the model's card, so only the prompt can refuse the reply.
        let decoded = phase_llm::select_picks_from_response(
            1,
            &projected,
            1,
            &fingerprint,
            (None, DIFFICULTY.with(Cell::get)),
            phase_llm::LlmReply {
                provider: phase_llm::LlmProvider::from_label("OpenAiCompatible"),
                status: 200,
                body: &reply_choosing(choice),
            },
        )
        .expect("the reply decodes on the live projection");
        assert_eq!(
            decoded.card_instance_ids,
            vec![projected[choice].instance_id.clone()]
        );

        let live = live_prompt(&view);
        let not_live = DraftPromptId {
            generation: live.generation + 1,
            ..live
        };
        let payload = resolution_json(
            &not_live,
            Some(reply(&fingerprint, 200, reply_choosing(choice))),
        );
        let (_, outcome) = local_fifo_resolve_llm_pick_inner(local.owner, &payload, 2_000)
            .expect("a reply for a prompt that is not live falls back");
        assert!(!outcome.used);
        assert_eq!(
            outcome.error,
            Some(phase_llm::LlmError::StaleDecision.to_string())
        );
        let after = installed();
        assert_eq!(ids(&after.pools[1]), vec![bot_choice]);
        for seat in 2..8 {
            assert_eq!(steps(&after, seat), steps(&session, seat), "seat {seat}");
        }
        reset();
    }

    #[test]
    fn a_reply_for_another_pack_falls_back() {
        reset();
        let local = start_llm(3, 0);
        let session = installed();
        let view = filter_for_player(&session, 1);
        let bot_choice = engine_bot_choice(&view, 1);
        let seat_two = filter_for_player(&session, 2);
        let another_pack = phase_llm::pick_fingerprint(2, &projected_head(&seat_two));
        let live = live_prompt(&view);
        assert_eq!(
            Some(&live),
            filter_for_player(&installed(), 1)
                .fifo
                .as_ref()
                .and_then(|fifo| fifo.prompt_id.as_ref()),
            "reach: the resolution answers seat 1's live prompt"
        );

        let payload = resolution_json(&live, Some(reply(&another_pack, 200, reply_choosing(0))));
        let (_, outcome) = local_fifo_resolve_llm_pick_inner(local.owner, &payload, 2_000)
            .expect("a reply for another pack falls back");
        assert!(!outcome.used);
        assert_eq!(
            outcome.error,
            Some(phase_llm::LlmError::StaleDecision.to_string())
        );
        assert_eq!(ids(&installed().pools[1]), vec![bot_choice]);
        reset();
    }

    #[test]
    fn a_missing_or_refused_reply_falls_back() {
        const REFUSAL: &str = r#"{"error":{"message":"Incorrect API key provided"}}"#;
        for refused in [false, true] {
            reset();
            let local = start_llm(3, 0);
            let session = installed();
            let view = filter_for_player(&session, 1);
            assert_eq!(steps(&session, 1), 0, "reach: seat 1 has not drafted");
            let bot_choice = engine_bot_choice(&view, 1);
            let sent = refused.then(|| {
                reply(
                    &phase_llm::pick_fingerprint(1, &projected_head(&view)),
                    401,
                    REFUSAL.to_string(),
                )
            });

            let payload = resolution_json(&live_prompt(&view), sent);
            let (_, outcome) = local_fifo_resolve_llm_pick_inner(local.owner, &payload, 2_000)
                .expect("a missing or refused reply falls back");
            assert!(!outcome.used, "refused: {refused}");
            let error = outcome.error.expect("the outcome says why");
            if refused {
                assert!(error.contains("Incorrect API key provided"), "{error}");
            } else {
                assert_eq!(error, NO_LLM_REPLY);
            }
            let after = installed();
            assert_eq!(steps(&after, 1), 1, "refused: {refused}");
            assert_eq!(ids(&after.pools[1]), vec![bot_choice], "refused: {refused}");
        }
        reset();
    }

    /// CR 905.1c: an LLM drafter answers only for its own bot seat's head, so a
    /// resolution naming the human's prompt, or a bot seat that holds no head,
    /// is refused with no change.
    #[test]
    fn a_resolution_for_a_seat_with_no_llm_head_changes_nothing() {
        reset();
        let local = start_llm(2, 0);
        // Paired positive: seat 1's own live head resolves.
        let seat_one_prompt = live_prompt(&filter_for_player(&installed(), 1));
        local_fifo_resolve_llm_pick_inner(
            local.owner,
            &resolution_json(&seat_one_prompt, None),
            2_000,
        )
        .expect("seat 1's live head resolves");

        let session = installed();
        let human = filter_for_player(&session, LOCAL_HUMAN_SEAT);
        let human_prompt = live_prompt(&human);
        assert!(
            session.head_pack(1).is_none(),
            "reach: seat 1 holds no head once it resolved"
        );
        let human_reply = reply(
            &phase_llm::pick_fingerprint(LOCAL_HUMAN_SEAT, &projected_head(&human)),
            200,
            reply_choosing(0),
        );

        let before = guard_cells_snapshot();
        assert_eq!(
            local_fifo_resolve_llm_pick_inner(
                local.owner,
                &resolution_json(&human_prompt, Some(human_reply)),
                3_000,
            )
            .unwrap_err(),
            NOT_AN_LLM_HEAD
        );
        assert_eq!(guard_cells_snapshot(), before);
        assert_eq!(
            local_fifo_resolve_llm_pick_inner(
                local.owner,
                &resolution_json(&seat_one_prompt, None),
                3_000,
            )
            .unwrap_err(),
            NOT_AN_LLM_HEAD
        );
        assert_eq!(guard_cells_snapshot(), before);
        reset();
    }

    /// CR 905.1a: after seat 2 drafts from its own two-card booster, its next
    /// head is seat 1's last card, a forced step the same call takes.
    #[test]
    fn a_resolution_runs_the_bots_it_releases() {
        reset();
        let local = start_llm(2, 0);
        let seat_one_prompt = live_prompt(&filter_for_player(&installed(), 1));
        local_fifo_resolve_llm_pick_inner(
            local.owner,
            &resolution_json(&seat_one_prompt, None),
            2_000,
        )
        .expect("seat 1's head resolves");

        let session = installed();
        let seat_two = filter_for_player(&session, 2);
        assert_eq!(
            seat_two
                .fifo
                .as_ref()
                .expect("a FIFO projection")
                .queue_count,
            2
        );
        assert_eq!(seat_two.current_pack.as_ref().map(Vec::len), Some(2));
        let revision = session.fifo.as_ref().expect("a FIFO session").revision;

        local_fifo_resolve_llm_pick_inner(
            local.owner,
            &resolution_json(&live_prompt(&seat_two), None),
            3_000,
        )
        .expect("seat 2's head resolves");
        let after = installed();
        assert_eq!(steps(&after, 2), 2);
        // Seat 2's own step, then its forced head: two commits.
        assert_eq!(
            after.fifo.as_ref().expect("a FIFO session").revision,
            revision + 2
        );
        reset();
    }

    #[test]
    fn declaring_engine_bots_drafts_every_waiting_head() {
        reset();
        let local = start_llm(3, 0);
        assert_eq!(bot_steps(&local.view), 0);
        let view = local_fifo_set_llm_drafting_inner(local.owner, "\"LlmSeats\"", 2_000)
            .expect("reach: the owner declares");
        assert_eq!(bot_steps(&view), 0);

        let before = guard_cells_snapshot();
        assert!(local_fifo_set_llm_drafting_inner(local.owner, "\"Bots\"", 2_000).is_err());
        assert_eq!(guard_cells_snapshot(), before);

        let view = local_fifo_set_llm_drafting_inner(local.owner, "\"EngineBots\"", 3_000)
            .expect("the owner declares");
        assert!(bot_steps(&view) > 0);
        assert!(bots_quiescent(&installed()));
        reset();
    }

    #[test]
    fn a_stale_owner_cannot_change_the_llm_drafting() {
        reset();
        let a = start_llm(3, 0).owner;
        let b = start_llm(3, 0);
        assert!(local_fifo_view_inner(b.owner).is_ok());

        let before = guard_cells_snapshot();
        assert_eq!(before["local_llm_drafting"], "LlmSeats");
        assert_eq!(
            local_fifo_set_llm_drafting_inner(a, "\"EngineBots\"", 2_000).unwrap_err(),
            STALE_LOCAL_OWNER
        );
        assert_eq!(guard_cells_snapshot(), before);
        let view = local_fifo_select_inner(
            b.owner,
            &selection_json(&live_prompt(&b.view), &head_ids(&b.view)[..1], &[], &[]),
            2_000,
        )
        .expect("B's human selects");
        assert_eq!(bot_steps(&view), 0);

        // Paired positive: B's own declaration releases the heads.
        let view = local_fifo_set_llm_drafting_inner(b.owner, "\"EngineBots\"", 3_000)
            .expect("the live owner declares");
        assert!(bot_steps(&view) > 0);
        reset();
    }

    #[test]
    fn every_local_install_resets_the_llm_drafting() {
        reset();
        let local = start_llm(3, 0);
        assert_eq!(bot_steps(&local.view), 0);
        assert_eq!(
            LOCAL_LLM_DRAFTING.with(Cell::get),
            LocalLlmDrafting::LlmSeats
        );
        assert_eq!(
            head_ids(&local.view).len(),
            3,
            "reach: the human's own booster"
        );
        live_prompt(&local.view);

        let json = export_local_fifo_session_inner(local.owner).expect("the owner exports");
        let imported = import_local_fifo_session_inner(&json, 2).expect("the export imports");
        let view = local_fifo_select_inner(
            imported.owner,
            &selection_json(
                &live_prompt(&imported.view),
                &head_ids(&imported.view)[..1],
                &[],
                &[],
            ),
            2_000,
        )
        .expect("the human selects after the import");
        assert!(bot_steps(&view) > 0);
        reset();
    }

    /// An LLM seat's selection, like the engine bot's, burns what the
    /// selection policy requires and spends no draft-effect card: the
    /// selection it applies carries an empty effect list.
    #[test]
    fn an_llm_selection_burns_what_the_policy_requires() {
        reset();
        let local = start_llm(3, 1);
        let session = installed();
        let view = filter_for_player(&session, 1);
        let fifo = view.fifo.as_ref().expect("a FIFO projection");
        assert_eq!(
            fifo.requirements,
            DraftSelectionRequirements { picks: 1, burns: 1 }
        );
        let pack_id = fifo.head.as_ref().expect("seat 1 holds a head").pack_id;
        let removed_before = session.fifo.as_ref().expect("a FIFO session").removed.len();
        let projected = projected_head(&view);

        let payload = resolution_json(
            &live_prompt(&view),
            Some(reply(
                &phase_llm::pick_fingerprint(1, &projected),
                200,
                reply_choosing(0),
            )),
        );
        let (_, outcome) = local_fifo_resolve_llm_pick_inner(local.owner, &payload, 2_000)
            .expect("the owner resolves seat 1's head");
        assert!(outcome.used, "{:?}", outcome.error);
        let after = installed();
        let removed = &after.fifo.as_ref().expect("a FIFO session").removed;
        assert_eq!(removed.len(), removed_before + 1);
        let burned = removed.last().expect("one card was burned");
        assert_eq!(burned.reason, DraftRemovalReason::Burn);
        assert_eq!(burned.pack_id, pack_id);
        reset();
    }

    /// CR 905.2a: there is no priority in a draft, so a FIFO seat drafts at
    /// its own pace and the request states the steps that seat has taken in
    /// the round, not the pod's step counter.
    #[test]
    fn a_request_states_the_seats_own_fifo_position() {
        reset();
        let local = start_llm(3, 0);
        local_fifo_select_inner(
            local.owner,
            &selection_json(
                &live_prompt(&local.view),
                &head_ids(&local.view)[..1],
                &[],
                &[],
            ),
            2_000,
        )
        .expect("the human selects");
        let seat_one_prompt = live_prompt(&filter_for_player(&installed(), 1));
        local_fifo_resolve_llm_pick_inner(
            local.owner,
            &resolution_json(&seat_one_prompt, None),
            3_000,
        )
        .expect("seat 1's head resolves");

        let view = filter_for_player(&installed(), 1);
        assert_eq!(view.pick_number, 0);
        assert_eq!(
            view.fifo
                .as_ref()
                .expect("a FIFO projection")
                .progress
                .round_completed_steps,
            1
        );
        assert_eq!(view.current_pack.as_ref().map(Vec::len), Some(2));
        assert!(matches!(
            draft_pick_step(&view),
            DraftPickStep::Choice { .. }
        ));

        let requests = local_fifo_build_llm_requests_inner(local.owner, ENDPOINT, "{}")
            .expect("the owner builds the requests");
        let body = requests
            .iter()
            .find_map(|entry| match entry {
                LocalLlmPickRequest::Request {
                    seat: 1, request, ..
                } => Some(request.body.clone()),
                LocalLlmPickRequest::Request { .. } | LocalLlmPickRequest::Unbuildable { .. } => {
                    None
                }
            })
            .expect("seat 1's head is requested");
        assert!(body.contains("Pack 1 of 3, pick 2"), "{body}");
        reset();
    }

    // ── The Quick constructors ───────────────────────────────────────────────

    /// A time no other fixture starts at, so the threaded time is visible.
    const QUICK_START_MS: u64 = 1_700_000_000_000;

    /// The cube list the Quick cube constructor resolves against
    /// [`install_fixture_db`]: 400 Alpha and one Delta.
    const QUICK_CUBE_LIST: &str = "400 Alpha\n1 Delta";

    /// The Quick set constructor's selection: one 15-card textless `AAA` set
    /// filling every booster.
    fn quick_set_selection() -> String {
        super::set_selection_tests::selection_json(&[("AAA", 15)], &["AAA"])
    }

    /// Cube settings unlike the defaults: a 4-seat pod of two 10-card
    /// boosters, asking for no minimum deck, so the procedure floor raising it
    /// to 1 is visible.
    fn quick_cube_settings() -> String {
        serde_json::json!({
            "pod_size": 4,
            "pack_count": 2,
            "cards_per_pack": 10,
            "min_deck_size": 0,
        })
        .to_string()
    }

    /// The human at seat 0 and a bot at every other seat of `pod_size`.
    fn assert_local_quick_seats(session: &DraftSession, pod_size: usize) {
        assert_eq!(session.seats.len(), pod_size);
        assert!(matches!(session.seats[0], DraftSeat::Human { .. }));
        assert!(session.seats[1..]
            .iter()
            .all(|seat| matches!(seat, DraftSeat::Bot { .. })));
    }

    /// The Quick set constructor starts a local FIFO draft: the Quick kind,
    /// one human and seven bots, the untimed Quick clock, the default
    /// selection policy, an issued canonical id, the caller's time and
    /// difficulty, and the bots quiescent. The legacy runtime refuses it, and
    /// the start retires the earlier draft's owner.
    #[test]
    fn the_quick_set_constructor_starts_a_local_fifo_draft() {
        reset();
        let a = start_fixture(AiDifficulty::Medium).owner;
        assert!(local_fifo_view_inner(a).is_ok(), "reach: owner A reads");

        let local = start_quick_draft_inner(
            &quick_set_selection(),
            3,
            7,
            "\"EngineBots\"",
            QUICK_START_MS,
        )
        .expect("the Quick set constructor starts");
        assert!(
            local_fifo_view_inner(local.owner).is_ok(),
            "reach: the started owner reads"
        );
        let session = installed();
        assert_eq!(session.config.flow, DraftFlow::FifoAsync);
        assert_eq!(session.config.kind, DraftKind::Quick);
        assert_eq!(
            session.config.clock_policy,
            DraftClockPolicy::quick_default()
        );
        assert!(session.config.selection_policy.is_default());
        assert_local_quick_seats(&session, 8);
        let fifo = session.fifo.as_ref().expect("a FIFO session");
        assert!(
            is_canonical_local_session_id(&fifo.session_id),
            "{:?}",
            fifo.session_id
        );
        assert_eq!(fifo.last_now_ms, QUICK_START_MS);
        assert_eq!(DIFFICULTY.with(Cell::get), AiDifficulty::Hard);
        assert!(bots_quiescent(&session));
        assert_eq!(
            with_draft_inner(|_| Ok(())),
            Err(FIFO_UNSUPPORTED_BY_RUNTIME.to_string())
        );
        assert_eq!(local_fifo_view_inner(a).unwrap_err(), STALE_LOCAL_OWNER);
        reset();
    }

    /// Not card-derived: the Quick cube constructor starts a local FIFO draft
    /// with the cube's own pod size, raises a zero minimum deck to the
    /// procedure floor, and keeps the cube's original multiset for the game.
    #[test]
    fn the_quick_cube_constructor_starts_a_local_fifo_draft() {
        reset();
        install_fixture_db();
        let local = start_quick_cube_draft_inner(
            QUICK_CUBE_LIST,
            "Cube",
            &quick_cube_settings(),
            2,
            11,
            "\"EngineBots\"",
            QUICK_START_MS,
        )
        .expect("the Quick cube constructor starts");
        assert!(
            local_fifo_view_inner(local.owner).is_ok(),
            "reach: the started owner reads"
        );
        let session = installed();
        assert_eq!(session.config.flow, DraftFlow::FifoAsync);
        assert_local_quick_seats(&session, 4);
        assert_eq!(local.view.min_deck_size, 1);
        let pool = local_fifo_booster_pack_pool_for_game_inner(local.owner)
            .expect("the owner reads the pool")
            .expect("a cube draft keeps its original multiset");
        assert_eq!(pool.len(), 401);
        assert_eq!(pool.last().map(String::as_str), Some("Delta"));
        reset();
    }

    /// Under LLM seats every bot seat holds its head for its own drafter and a
    /// request is built for each; under engine bots the start batch drafts
    /// them and none is requested.
    fn assert_takes_the_drafting(
        label: &str,
        pod_size: u8,
        start: impl Fn(&str) -> LocalFifoSession,
    ) {
        let llm = start("\"LlmSeats\"");
        assert_eq!(bot_steps(&llm.view), 0, "{label}");
        assert_eq!(
            LOCAL_LLM_DRAFTING.with(Cell::get),
            LocalLlmDrafting::LlmSeats,
            "{label}"
        );
        let requested: Vec<u8> = local_fifo_build_llm_requests_inner(llm.owner, ENDPOINT, "{}")
            .expect("the owner builds the requests")
            .iter()
            .map(|entry| match entry {
                LocalLlmPickRequest::Request { seat, .. } => *seat,
                LocalLlmPickRequest::Unbuildable { seat, error, .. } => {
                    panic!("{label}: seat {seat} is unbuildable: {error}")
                }
            })
            .collect();
        assert_eq!(requested, (1..pod_size).collect::<Vec<u8>>(), "{label}");

        let engine = start("\"EngineBots\"");
        assert!(bot_steps(&engine.view) > 0, "{label}");
        assert_eq!(
            LOCAL_LLM_DRAFTING.with(Cell::get),
            LocalLlmDrafting::EngineBots,
            "{label}"
        );
        assert!(
            local_fifo_build_llm_requests_inner(engine.owner, ENDPOINT, "{}")
                .expect("the owner builds the requests")
                .is_empty(),
            "{label}"
        );
    }

    /// Each Quick constructor installs the LLM drafting it is given.
    #[test]
    fn the_quick_constructors_take_the_llm_drafting_they_are_given() {
        reset();
        assert_takes_the_drafting("set", 8, |drafting| {
            start_quick_draft_inner(&quick_set_selection(), 2, 7, drafting, QUICK_START_MS)
                .expect("the Quick set constructor starts")
        });
        reset();
        install_fixture_db();
        assert_takes_the_drafting("cube", 4, |drafting| {
            start_quick_cube_draft_inner(
                QUICK_CUBE_LIST,
                "Cube",
                &quick_cube_settings(),
                2,
                11,
                drafting,
                QUICK_START_MS,
            )
            .expect("the Quick cube constructor starts")
        });
        reset();
    }

    /// `start` is refused with an error naming `expected`, no runtime cell
    /// changes, and `owner` still reads its session.
    fn assert_refused_before_install(
        label: &str,
        expected: &str,
        owner: LocalOwner,
        start: impl FnOnce() -> Result<LocalFifoSession, String>,
    ) {
        let before = guard_cells_snapshot();
        let Err(error) = start() else {
            panic!("{label}: the constructor started");
        };
        assert!(error.contains(expected), "{label}: {error}");
        assert_eq!(guard_cells_snapshot(), before, "{label}");
        assert!(local_fifo_view_inner(owner).is_ok(), "{label}");
    }

    /// A Quick constructor refuses before it installs: on each refusal no
    /// runtime cell changes and the earlier owner keeps its session.
    #[test]
    fn a_quick_constructor_refuses_before_it_installs() {
        reset();
        let a = start_fixture(AiDifficulty::Medium).owner;
        let selection = quick_set_selection();
        assert_refused_before_install("an unparsable drafting", "Invalid LLM drafting", a, || {
            start_quick_draft_inner(&selection, 2, 7, "\"Sometimes\"", QUICK_START_MS)
        });
        let missing_set = super::set_selection_tests::selection_json(&[("AAA", 15)], &["ZZZ"]);
        assert_refused_before_install(
            "a set with no pool",
            "No pool data was supplied for set 'ZZZ'",
            a,
            || start_quick_draft_inner(&missing_set, 2, 7, "\"EngineBots\"", QUICK_START_MS),
        );
        assert_refused_before_install(
            "a time beyond the engine's bound",
            "Failed to start local draft",
            a,
            || start_quick_draft_inner(&selection, 2, 7, "\"EngineBots\"", u64::MAX),
        );
        assert_refused_before_install(
            "a cube with no card database",
            "Card database must be loaded before cube draft",
            a,
            || {
                start_quick_cube_draft_inner(
                    QUICK_CUBE_LIST,
                    "Cube",
                    &quick_cube_settings(),
                    2,
                    11,
                    "\"EngineBots\"",
                    QUICK_START_MS,
                )
            },
        );

        assert!(
            start_quick_draft_inner(&selection, 2, 7, "\"EngineBots\"", QUICK_START_MS).is_ok(),
            "a valid start succeeds"
        );
        assert_eq!(local_fifo_view_inner(a).unwrap_err(), STALE_LOCAL_OWNER);
        reset();
    }
}
