/* tslint:disable */
/* eslint-disable */

/**
 * Check whether all seats with pending packs have submitted their picks.
 *
 * Returns true when the draft can advance (all seats picked or no packs pending).
 * The P2P host uses this to know when to broadcast state updates after a round.
 */
export function all_picks_submitted(): boolean;

/**
 * Apply a draft action from any seat. Used by the P2P host to forward
 * picks from connected guests.
 *
 * `action_json`: serialized DraftAction, e.g.:
 *   `{ "type": "Pick", "data": { "seat": 2, "card_instance_ids": ["abc-123"] } }`
 *
 * Returns the list of DraftDeltas produced (serialized as a JS array).
 */
export function apply_draft_action(action_json: string): any;

/**
 * Auto-pick the best card from the human's current pack using the same AI the
 * bots use (at the active difficulty), then resolve all bot picks.
 *
 * Returns the updated DraftPlayerView.
 */
export function auto_pick(): any;

/**
 * Return the host-only original cube multiset for the game launched after a
 * draft. This deliberately bypasses `DraftPlayerView`: players and spectators
 * must never receive undealt cube entries or their duplicate counts.
 */
export function booster_pack_pool_for_game(): any;

/**
 * Build one LLM pick request per eligible bot seat.
 *
 * Takes NO seat list. Which seats an LLM may draft for is an authority
 * question this crate already owns ([`llm_eligible_bot_seats`]), and accepting
 * a caller's list made the display layer a second classifier of the same
 * thing -- one free to drift toward naming a human seat, whose private pool
 * and unpassed pack would then be rendered into a third-party prompt.
 *
 * `set_names_json` is an optional set-code -> name map so the format brief
 * reads "Triple Mirrodin" rather than "Triple MRD"; codes are used verbatim
 * when it is absent.
 */
export function buildLlmDraftPickRequests(endpoint_json: string, set_names_json: string): any;

/**
 * Create a multiplayer draft session. Used by the P2P host to initialize a
 * multiplayer draft of any `DraftKind` with a wire number, with human + bot
 * seats from a Set pool, host-local Chaos candidate pools, or a custom Cube
 * list. A shared-stack kind admits bot seats like any other; their turns are
 * driven by `resolve_shared_stack_bot_turns`, which the host calls after each
 * human decision.
 *
 * - `pool_input_json`: serialized `PoolInput` discriminated union
 *   (`{ "type": "Set" | "Chaos" | "Cube", "data": { ... } }`)
 * - `seats_json`: JSON array of SeatDescriptors
 * - `kind`: the wire number for a `DraftKind`. The mapping's single authority
 *   is `draft_kind_wire_number` — read it there rather than restating it here,
 *   which is what keeps a widening from leaving this list stale. Flows through
 *   to `DraftConfig.kind` unchanged. Tournament match format is identical to
 *   set drafts.
 * - `seed`: RNG seed for deterministic pack generation
 * - `draft_code`: unique room identifier
 * - `difficulty`: the bot strength this pod's bot seats play at, through
 *   `map_difficulty` (0..=4, anything else is `Medium`). APPENDED LAST, and it
 *   must stay last: the client's call sites and their test mocks read this
 *   boundary positionally.
 *
 *   It is not cosmetic. `DIFFICULTY` is a per-thread `Cell` with no reset that
 *   outlives the draft that set it, and until now this entry point never wrote
 *   it — so a player who finished a Quick draft at `VeryHard` and then hosted
 *   a pod in the same tab got a `VeryHard` pod bot, silently, with no UI
 *   saying so. Every other entry point that creates a session writes this cell
 *   (`start_quick_draft`, `start_sealed_draft`, `start_quick_cube_draft`,
 *   `import_draft_session`); this one now does too, so the strength a pod
 *   plays at is the strength its host asked for.
 *
 * Stores the session in the same thread-local as Quick Draft (one active
 * draft at a time per WASM instance). Returns the initial DraftPlayerView
 * for seat 0.
 */
export function create_multiplayer_draft(pool_input_json: string, seats_json: string, kind: number, seed: number, draft_code: string, tournament_format: string, pod_policy: string, difficulty: number): any;

/**
 * The engine-owned per-kind axes for a numeric draft kind. The display layer
 * reads these; it never re-derives them (CLAUDE.md: the frontend is a display
 * layer, not a logic layer).
 */
export function draft_procedure(kind: number, tournament_format: string): any;

/**
 * Serialize the full DraftSession to JSON for host persistence.
 *
 * The host persists this after every authoritative mutation so a
 * crashed/reloaded host can restore the draft state. This is the trusted
 * authority export: unlike `DraftSourceView`, it intentionally retains a
 * Chaos layout's complete assignment matrix and must not be sent to guests.
 */
export function export_draft_session(): string;

/**
 * Narrow a limited-pool listing through the ENGINE's filtering authority
 * (#7546 review): the display sends the listing and a typed `PoolFilter`;
 * it renders exactly the returned instance ids. Each instance is classified
 * inside draft-core, so wire-delivered groups (of any protocol vintage) are
 * not an input. Stateless — usable by P2P guests.
 */
export function filter_pool_listing(listing_json: string, filter_json: string): any;

/**
 * Get a bot's auto-built deck for match play.
 *
 * `bot_seat`: seat index 1-7 for the bot opponent.
 * Returns a SuggestedDeck built from the bot's drafted pool.
 */
export function get_bot_deck(bot_seat: number): any;

/**
 * Get the full draft status. Lightweight check so the host can decide
 * whether to broadcast updates or transition phases.
 */
export function get_draft_status(): any;

/**
 * Get a filtered draft view for a specific seat. The P2P host calls this
 * after each action to produce per-player state snapshots to send over
 * the P2P channel.
 *
 * `seat_index`: 0-based seat index.
 */
export function get_draft_view_for_seat(seat_index: number): any;

/**
 * Get the current DraftPlayerView without mutation.
 */
export function get_view(): any;

/**
 * Get the filtered DraftPlayerView for any seat.
 */
export function get_view_for_seat(seat: number): any;

/**
 * Restore a DraftSession from a persisted JSON snapshot.
 *
 * Also re-initializes RNG and difficulty from the session config so that
 * `submit_pick` (which runs bot picks) works after resume.  The RNG is
 * re-seeded from the config seed offset by the current pick progress —
 * bot pick quality remains reasonable but won't be identical to the
 * original session's RNG stream, which is fine.
 */
export function import_draft_session(json: string, difficulty: number): any;

/**
 * Initialize panic hook for better error messages in WASM.
 */
export function init_panic_hook(): void;

/**
 * The engine-owned LLM provider catalog, mirrored here so a draft-only client
 * surface does not have to load the game engine to render the settings UI.
 */
export function llmProviderCatalog(): any;

/**
 * Load the card database from a JSON string (card-data.json contents).
 * Required for Hard/VeryHard bot AI evaluation and accurate deck suggestion.
 * Returns the number of cards loaded.
 */
export function load_card_database(json_str: string): number;

/**
 * The complete engine-owned filter option lists for a pool, computed from
 * the instances alone (review round 5): the stateless path a display uses
 * when its delivered view predates the option fields, so legacy controls
 * never come from the lossy exclusive presentation buckets.
 */
export function pool_filter_options(pool_json: string): any;

/**
 * Resolve every consecutive shared-stack turn owned by a bot seat, and return
 * the `DraftDelta`s produced (an empty array when the active seat is human, the
 * draft is over, or the session has no shared stack).
 *
 * The host calls this after applying a human seat's decision and after starting
 * a pod whose first seat is a bot. It is a WASM export because the loop, its
 * bound and its termination proof are engine concerns: the client calls it and
 * renders what comes back, and computes nothing.
 */
export function resolve_shared_stack_bot_turns(): any;

/**
 * Mark a human seat as connected or disconnected. The host adapter calls
 * this on guest disconnect/reconnect so `DraftPlayerView.seats[*].connected`
 * reflects the runtime state. Rejects bot seats with `SeatIsBot`.
 *
 * Returns the DraftPlayerView for seat 0 (the host) after the update.
 */
export function set_seat_connected(seat: number, connected: boolean): any;

/**
 * The decision the ENGINE would apply for a seat whose turn must be resolved
 * without that seat choosing — a pick-timer expiry, or a disconnect.
 *
 * `None` when there is no shared stack, or when the seat has no legal move
 * (which for the ACTIVE seat while drafting is unreachable, and proved so by
 * `some_decision_is_always_legal_for_the_active_seat_while_drafting`).
 *
 * WHY THIS EXISTS AS AN EXPORT. The host used to scan the published
 * `legality` vector itself — `legality.find(entry => entry.refusal === null)`
 * — and take the first entry with no refusal. That is the same algorithm
 * `shared_stack::forced_decision` runs, but over a DIFFERENT ordering source:
 * the engine folds `SharedStackPileDecision::ALL` in declaration order, while
 * the client folded whatever order the view happened to serialize. The two
 * agreed by coincidence rather than by construction, and a reordering of
 * either would have silently changed which move a timed-out seat makes.
 *
 * Choosing a rules outcome is the reducer's job. The host may ASK for the
 * forced resolution — that is a timeout, which is a host concern — but the
 * answer comes from here, and the host only dispatches it through the ordinary
 * decision path so the timed-out turn is persisted, acknowledged, broadcast and
 * re-armed by exactly the code a player-driven one is.
 */
export function shared_stack_forced_decision(seat_index: number): any;

/**
 * Start a Quick Cube Draft session from a counted cube list.
 */
export function start_quick_cube_draft(cube_list_text: string, cube_name: string, settings_json: string, difficulty: number, seed: number): any;

/**
 * Start a Quick Draft session: 1 human + 7 bots.
 *
 * - `selection_json`: serialized [`SetPackSequence`] — the distinct set pools
 *   from draft-pools.json plus the set filling each booster, in pack order.
 *   The sequence length is the draft's pack count, and a set may repeat.
 * - `difficulty`: 0=VeryEasy, 1=Easy, 2=Medium, 3=Hard, 4=VeryHard
 * - `seed`: RNG seed for deterministic pack generation
 *
 * Returns the initial DraftPlayerView as a JS object.
 */
export function start_quick_draft(selection_json: string, difficulty: number, seed: number): any;

/**
 * Start a local Sealed event: one human and seven bots each open six packs,
 * then the human proceeds directly to deckbuilding.
 */
export function start_sealed_draft(selection_json: string, difficulty: number, seed: number): any;

/**
 * Submit the human's pick, resolving any LLM seat's pick from its response.
 *
 * Returns `{ view, llmOutcomes }`: the same `DraftPlayerView` `submit_pick`
 * returns, plus a per-seat record of whether the LLM pick was used.
 */
export function submitPickWithLlmBotPicks(card_instance_id: string, responses_json: string): any;

/**
 * Submit the human player's deck for limited play.
 *
 * `main_deck_json`: JSON array of card name strings.
 * `commanders_json`: JSON array of the card names this seat designates as its
 * commander(s) (CR 903.3 / CR 702.124h). CR 903.1 puts the designation inside
 * the Commander variant, so `[]` is the correct and meaningful value for every
 * non-Commander kind.
 * The deck is validated against the pool via LimitedDeckValidator.
 */
export function submit_deck(main_deck_json: string, commanders_json: string): any;

/**
 * Submit a deck for any seat.
 *
 * `main_deck_json`: JSON array of card name strings.
 * `commanders_json`: JSON array of the card names this seat designates as its
 * commander(s) (CR 903.3 / CR 702.124h). CR 903.1 puts the designation inside
 * the Commander variant, so `[]` is the correct and meaningful value for every
 * non-Commander kind.
 * Returns the DraftPlayerView for the specified seat.
 */
export function submit_deck_for_seat(seat: number, main_deck_json: string, commanders_json: string): any;

/**
 * Submit the human player's pick and resolve all bot picks synchronously.
 *
 * Returns the updated DraftPlayerView.
 */
export function submit_pick(card_instance_id: string): any;

/**
 * Submit one whole CR 903.13b pick step for any seat (host proxies guest
 * picks): every card the seat drafts this step, as a JSON array of instance
 * ids. `apply_pick_inner` owns the count contract — one id for the four CR
 * 905.1a kinds, two for CommanderDraft, dropping to the remainder on an odd
 * final pick. `Winston` has NO PICK STEP AT ALL and never reaches this
 * function: a shared-stack turn is a whole-pile
 * `DraftAction::SharedStackDecision`.
 *
 * The JSON encoding mirrors `submit_pick_with_draft_effect_for_seat` below
 * byte for byte. It is deliberately NOT tolerant of a bare id: a bare string
 * is a parse `Err` here, which is what keeps a half-applied caller loud
 * instead of silently picking one card.
 *
 * Returns the DraftPlayerView for the specified seat after the pick.
 */
export function submit_pick_for_seat(seat: number, card_instance_ids_json: string): any;

/**
 * Submit an additional pick using a drafted card's draft-time effect, then
 * resolve all bot picks.
 */
export function submit_pick_with_draft_effect(effect_card_instance_id: string, card_instance_ids_json: string): any;

/**
 * Submit a draft-effect pick for any seat (host proxies guest picks).
 *
 * Returns the filtered DraftPlayerView for the specified seat after the pick.
 */
export function submit_pick_with_draft_effect_for_seat(seat: number, effect_card_instance_id: string, card_instance_ids_json: string): any;

/**
 * Auto-suggest a playable Limited deck from the human's pool.
 *
 * Returns a SuggestedDeck with ~23 spells + ~17 lands, using AI evaluation
 * at the current difficulty level. Per D-12: "Suggest deck" auto-build.
 */
export function suggest_deck(): any;

/**
 * Suggest land counts for a given set of spells.
 *
 * `spells_json`: JSON array of card name strings from the pool.
 * Returns a map of land name -> count (e.g. {"Plains": 4, "Island": 6}).
 * Per D-11: auto-suggest land counts based on color distribution.
 */
export function suggest_lands(spells_json: string): any;

/**
 * Suggest land counts for spells in a specific multiplayer seat's pool.
 *
 * The spells payload is parsed before the active session is accessed, so a
 * malformed request cannot observe or depend on the current draft state.
 */
export function suggest_lands_for_seat(seat: number, spells_json: string): any;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly all_picks_submitted: () => [number, number, number];
    readonly apply_draft_action: (a: number, b: number) => [number, number, number];
    readonly auto_pick: () => [number, number, number];
    readonly booster_pack_pool_for_game: () => [number, number, number];
    readonly buildLlmDraftPickRequests: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly create_multiplayer_draft: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number) => [number, number, number];
    readonly draft_procedure: (a: number, b: number, c: number) => [number, number, number];
    readonly export_draft_session: () => [number, number, number, number];
    readonly filter_pool_listing: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly get_bot_deck: (a: number) => [number, number, number];
    readonly get_draft_status: () => [number, number, number];
    readonly get_draft_view_for_seat: (a: number) => [number, number, number];
    readonly get_view: () => [number, number, number];
    readonly get_view_for_seat: (a: number) => [number, number, number];
    readonly import_draft_session: (a: number, b: number, c: number) => [number, number, number];
    readonly load_card_database: (a: number, b: number) => [number, number, number];
    readonly pool_filter_options: (a: number, b: number) => [number, number, number];
    readonly resolve_shared_stack_bot_turns: () => [number, number, number];
    readonly set_seat_connected: (a: number, b: number) => [number, number, number];
    readonly shared_stack_forced_decision: (a: number) => [number, number, number];
    readonly start_quick_cube_draft: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number];
    readonly start_quick_draft: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly start_sealed_draft: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly submitPickWithLlmBotPicks: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly submit_deck: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly submit_deck_for_seat: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly submit_pick: (a: number, b: number) => [number, number, number];
    readonly submit_pick_for_seat: (a: number, b: number, c: number) => [number, number, number];
    readonly submit_pick_with_draft_effect: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly submit_pick_with_draft_effect_for_seat: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly suggest_deck: () => [number, number, number];
    readonly suggest_lands: (a: number, b: number) => [number, number, number];
    readonly suggest_lands_for_seat: (a: number, b: number, c: number) => [number, number, number];
    readonly init_panic_hook: () => void;
    readonly llmProviderCatalog: () => any;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
