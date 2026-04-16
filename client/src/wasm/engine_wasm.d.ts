/* tslint:disable */
/* eslint-disable */

/**
 * Apply a seat mutation to a seat state, using the TLS card database for deck
 * resolution. Both arguments are JSON strings; returns `{ state, delta }` as
 * a JS object on success, or a JS error string on failure.
 */
export function apply_seat_mutation(state_json: string, mutation_json: string): any;

/**
 * Clear the game state without dropping the WASM instance or card database.
 *
 * Used by the singleton adapter to reset between game sessions. Any in-flight
 * AI computation that calls `with_state()` after this will return an error
 * immediately rather than running a full search on stale state.
 */
export function clear_game_state(): void;

/**
 * Create a default 2-player game state.
 */
export function create_initial_state(): any;

/**
 * Evaluate deck compatibility and format legality using the loaded card database.
 * Returns strict Standard/Commander checks, BO3 readiness, and selected-format compatibility.
 */
export function evaluate_deck_compatibility_js(request: any): any;

/**
 * Export the current game state as a JSON string.
 * Used by the engine worker to transfer state to AI workers for root parallelism.
 */
export function export_game_state_json(): string;

/**
 * Get the AI's chosen action for the current game state.
 * `difficulty` is one of: "VeryEasy", "Easy", "Medium", "Hard", "VeryHard".
 * `player_id` is the seat index of the AI player (0-based).
 */
export function get_ai_action(difficulty: string, player_id: number): any;

/**
 * Score all candidate actions and return `[GameAction, score]` tuples.
 * Used by AI workers for root parallelism — each worker scores independently,
 * then results are merged on the main thread.
 * `rng_seed` seeds the game state's RNG so each worker's MCTS explores
 * different paths through the search tree, producing diverse score vectors.
 */
export function get_ai_scored_candidates(difficulty: string, player_id: number, rng_seed: bigint): any;

/**
 * Look up a card face by name from the loaded card database.
 * Returns the serialized `CardFace` (keywords, abilities, triggers, static_abilities,
 * replacements, card_type, oracle_text, etc.) or null if not found.
 * Used by the deck builder to display engine-parsed ability data.
 */
export function get_card_face_data(name: string): any;

/**
 * Returns the hierarchical parse tree for a card face, with per-item support status.
 * Each `ParsedItem` contains category, label, source_text, supported (bool), details
 * (key-value pairs), and recursive children (sub-abilities, modal modes, costs).
 * Returns null if the card database is not loaded or the card is not found.
 */
export function get_card_parse_details(name: string): any;

/**
 * Get a filtered view of the current game state for the given player.
 */
export function get_filtered_game_state(viewer: number): any;

/**
 * Get the current game state as JSON.
 * Derived display fields (summoning sickness, devotion, etc.) are computed
 * automatically by the engine in apply()/start_game().
 */
export function get_game_state(): any;

/**
 * Get the legal actions, auto-pass recommendation, and spell costs for the current game state.
 * Returns `{ actions: GameAction[], autoPassRecommended: boolean, spellCosts: Record<ObjectId, ManaCost> }`.
 */
export function get_legal_actions_js(): any;

/**
 * Initialize panic hook for better error messages in WASM.
 * Called automatically on first use — safe to call multiple times.
 */
export function init_panic_hook(): void;

/**
 * Initialize a new game.
 * Accepts deck_data as a DeckList (name-only) or null/undefined for empty libraries.
 * format_config_js: optional FormatConfig JSON — defaults to Standard if null/undefined.
 * match_config_js: optional MatchConfig JSON — defaults to BO1 if null/undefined.
 * player_count: number of players — defaults to 2 if not provided.
 * first_player: 0 = human plays first (CR 103.1), 1 = opponent plays first, None = random.
 * Names are resolved against the card database loaded via load_card_database().
 * Returns the initial ActionResult (events + waiting_for).
 */
export function initialize_game(deck_data: any, seed: number | null | undefined, format_config_js: any, match_config_js: any, player_count?: number | null, first_player?: number | null): any;

/**
 * Read the multiplayer enforcement flag. Exposed primarily for tests and
 * adapters that need to defend their own paths (e.g., skip history pushes).
 */
export function is_multiplayer_mode(): boolean;

/**
 * Load the card database from a JSON string (card-data.json contents).
 * Must be called before initialize_game to enable name-based deck resolution.
 */
export function load_card_database(json_str: string): number;

/**
 * Verify WASM integration works.
 */
export function ping(): string;

/**
 * Restore the game state from a JSON string.
 * Uses serde_json which handles string-keyed maps (from localStorage round-trip)
 * correctly deserializing into HashMap<ObjectId, V>.
 *
 * Refuses when `MULTIPLAYER_MODE` is set — rewriting a single client's
 * state in a multiplayer session would diverge from the authoritative
 * game on the wire. Undo is a single-player affordance only.
 */
export function restore_game_state(json_str: string): void;

/**
 * Resume a multiplayer host session from a persisted `GameState`.
 *
 * Called when a P2P host returns after a crash/reload and needs to restore
 * the authoritative game state from disk so returning guests (still in
 * their reconnect backoff) can re-bind to their seats. Mirrors
 * `server-core::GameSession::from_persisted` — the analogous pattern for
 * the WebSocket-server authority.
 *
 * Differs from `restore_game_state` in two load-bearing ways:
 *
 * 1. **Fresh RNG seed.** `restore_game_state` re-seeds from the saved
 *    `rng_seed`, which rewinds the ChaCha20 stream to position 0 —
 *    correct for undo (replay from origin) but wrong for resume
 *    (subsequent draws would replay the pre-save sequence). This
 *    function stamps a fresh seed so continued play diverges.
 * 2. **Atomic multiplayer-flag flip.** Sets `MULTIPLAYER_MODE` in the
 *    same call that loads state, so there's no window where a stray
 *    `restore_game_state` (undo) would be accepted on the resumed
 *    session.
 *
 * Refuses when the engine is already in use — this is a fresh-instance
 * entry point. Callers must clear any existing state first.
 */
export function resume_multiplayer_host_state(json_str: string): void;

/**
 * Select an action from merged scores using softmax.
 * Called after collecting scored candidates from parallel workers and merging.
 * `scores_json` is a JSON array of `[GameAction, score]` tuples.
 * `difficulty` determines the softmax temperature (engine is the single
 * authority for AI tuning parameters — the frontend never specifies temperature).
 * `rng_seed` provides deterministic randomness.
 */
export function select_action_from_scores(scores_json: string, difficulty: string, rng_seed: bigint): any;

/**
 * Toggle the multiplayer enforcement flag. Called by multiplayer adapters
 * (P2P host/guest, WS) after the engine is initialized so subsequent
 * `restore_game_state` calls fail fast with a clear error instead of
 * silently rewriting the local view.
 */
export function set_multiplayer_mode(enabled: boolean): void;

/**
 * Submit a game action on behalf of `actor` and return the ActionResult
 * (events + waiting_for).
 *
 * **Security contract:** `actor` must be the transport-authenticated
 * `PlayerId` of the caller — either the local human's seat (in local/AI
 * games) or the connection-authenticated seat (in P2P/WebSocket games).
 * It must *never* come from UI or wire payload data. The engine rejects any
 * action whose `actor` does not match `authorized_submitter(state)`, so
 * passing a spoofed value here will fail cleanly rather than silently
 * applying the action as another player.
 */
export function submit_action(actor: number, action: any): any;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly apply_seat_mutation: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly evaluate_deck_compatibility_js: (a: any) => [number, number, number];
    readonly export_game_state_json: () => [number, number, number, number];
    readonly get_ai_action: (a: number, b: number, c: number) => [number, number, number];
    readonly get_ai_scored_candidates: (a: number, b: number, c: number, d: bigint) => [number, number, number];
    readonly get_card_face_data: (a: number, b: number) => any;
    readonly get_card_parse_details: (a: number, b: number) => any;
    readonly get_filtered_game_state: (a: number) => any;
    readonly initialize_game: (a: any, b: number, c: number, d: any, e: any, f: number, g: number) => any;
    readonly is_multiplayer_mode: () => number;
    readonly load_card_database: (a: number, b: number) => [number, number, number];
    readonly ping: () => [number, number];
    readonly restore_game_state: (a: number, b: number) => [number, number];
    readonly resume_multiplayer_host_state: (a: number, b: number) => [number, number];
    readonly select_action_from_scores: (a: number, b: number, c: number, d: number, e: bigint) => [number, number, number];
    readonly set_multiplayer_mode: (a: number) => void;
    readonly submit_action: (a: number, b: any) => any;
    readonly get_game_state: () => any;
    readonly get_legal_actions_js: () => any;
    readonly init_panic_hook: () => void;
    readonly create_initial_state: () => any;
    readonly clear_game_state: () => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
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
