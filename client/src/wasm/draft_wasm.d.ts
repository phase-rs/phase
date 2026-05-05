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
 *   `{ "type": "Pick", "data": { "seat": 2, "card_instance_id": "abc-123" } }`
 *
 * Returns the list of DraftDeltas produced (serialized as a JS array).
 */
export function apply_draft_action(action_json: string): any;

/**
 * Create a multiplayer draft session. Used by the P2P host to initialize a
 * Premier or Traditional draft with human + bot seats.
 *
 * - `set_pool_json`: serialized LimitedSetPool from draft-pools.json
 * - `seats_json`: JSON array of SeatDescriptors
 * - `kind`: 0=Quick, 1=Premier, 2=Traditional
 * - `seed`: RNG seed for deterministic pack generation
 * - `draft_code`: unique room identifier
 *
 * Stores the session in the same thread-local as Quick Draft (one active
 * draft at a time per WASM instance). Returns the initial DraftPlayerView
 * for seat 0.
 */
export function create_multiplayer_draft(set_pool_json: string, seats_json: string, kind: number, seed: number, draft_code: string): any;

/**
 * Serialize the full DraftSession to JSON for host persistence.
 *
 * The host persists this after every authoritative mutation so a
 * crashed/reloaded host can restore the draft state.
 */
export function export_draft_session(): string;

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
 * Load the card database from a JSON string (card-data.json contents).
 * Required for Hard/VeryHard bot AI evaluation and accurate deck suggestion.
 * Returns the number of cards loaded.
 */
export function load_card_database(json_str: string): number;

/**
 * Start a multiplayer draft session (Premier or Traditional).
 *
 * - `set_pool_json`: serialized LimitedSetPool
 * - `kind`: "Premier" or "Traditional"
 * - `seat_names_json`: JSON array of display names, one per seat (length = pod size)
 * - `seed`: RNG seed for deterministic pack generation
 *
 * Returns the DraftPlayerView for seat 0 (the host).
 */
export function start_multiplayer_draft(set_pool_json: string, kind: string, seat_names_json: string, seed: number): any;

/**
 * Start a Quick Draft session: 1 human + 7 bots.
 *
 * - `set_pool_json`: serialized LimitedSetPool from draft-pools.json
 * - `difficulty`: 0=VeryEasy, 1=Easy, 2=Medium, 3=Hard, 4=VeryHard
 * - `seed`: RNG seed for deterministic pack generation
 *
 * Returns the initial DraftPlayerView as a JS object.
 */
export function start_quick_draft(set_pool_json: string, difficulty: number, seed: number): any;

/**
 * Submit the human player's deck for limited play.
 *
 * `main_deck_json`: JSON array of card instance ID strings.
 * The deck is validated against the pool via LimitedDeckValidator.
 */
export function submit_deck(main_deck_json: string): any;

/**
 * Submit a deck for any seat.
 *
 * `main_deck_json`: JSON array of card name strings.
 * Returns the DraftPlayerView for the specified seat.
 */
export function submit_deck_for_seat(seat: number, main_deck_json: string): any;

/**
 * Submit the human player's pick and resolve all bot picks synchronously.
 *
 * Per Arena Quick Draft model: bots pick instantly after the human.
 * Returns the updated DraftPlayerView.
 */
export function submit_pick(card_instance_id: string): any;

/**
 * Submit a pick for any seat (host proxies guest picks).
 *
 * Returns the DraftPlayerView for the specified seat after the pick.
 */
export function submit_pick_for_seat(seat: number, card_instance_id: string): any;

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

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly all_picks_submitted: () => [number, number, number];
    readonly apply_draft_action: (a: number, b: number) => [number, number, number];
    readonly create_multiplayer_draft: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number];
    readonly export_draft_session: () => [number, number, number, number];
    readonly get_bot_deck: (a: number) => [number, number, number];
    readonly get_draft_status: () => [number, number, number];
    readonly get_draft_view_for_seat: (a: number) => [number, number, number];
    readonly get_view: () => [number, number, number];
    readonly get_view_for_seat: (a: number) => [number, number, number];
    readonly import_draft_session: (a: number, b: number, c: number) => [number, number, number];
    readonly load_card_database: (a: number, b: number) => [number, number, number];
    readonly start_multiplayer_draft: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number];
    readonly start_quick_draft: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly submit_deck: (a: number, b: number) => [number, number, number];
    readonly submit_deck_for_seat: (a: number, b: number, c: number) => [number, number, number];
    readonly submit_pick: (a: number, b: number) => [number, number, number];
    readonly submit_pick_for_seat: (a: number, b: number, c: number) => [number, number, number];
    readonly suggest_deck: () => [number, number, number];
    readonly suggest_lands: (a: number, b: number) => [number, number, number];
    readonly init_panic_hook: () => void;
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
