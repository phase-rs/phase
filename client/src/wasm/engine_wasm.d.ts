/* tslint:disable */
/* eslint-disable */

/**
 * Apply a seat mutation to a seat state, using the TLS card database for deck
 * resolution. Both arguments are JSON strings; returns `{ state, delta }` as
 * a JS object on success, or a JS error string on failure.
 */
export function apply_seat_mutation(state_json: string, mutation_json: string): any;

/**
 * Build the bounded card corpus for parallel AI scoring workers. The live
 * main engine remains the only authority that owns the full card database.
 */
export function build_ai_card_subset(): string;

/**
 * Classify a deck's archetype (Aggro / Midrange / Control / Combo / Ramp) using
 * `phase_ai::DeckProfile::analyze`. The engine is the single authority for archetype
 * classification — the frontend must not compute this from card lists itself.
 *
 * Input: a flat list of card names (duplicates allowed — `resolve_player_deck_list`
 * groups them into DeckEntry counts). Unresolvable names are silently skipped.
 * Output: `{ archetype, confidence: "Pure" | "Hybrid", secondary? }`.
 */
export function classify_deck_js(names_js: any): any;

/**
 * Clear the game state without dropping the WASM instance or card database.
 *
 * Used by the singleton adapter to reset between game sessions. Any in-flight
 * AI computation that calls `with_state()` after this will return an error
 * immediately rather than running a full search on stale state.
 */
export function clear_game_state(): void;

/**
 * Discard the loaded replay (if any). Safe to call even when none is loaded.
 */
export function clear_replay_playback(): void;

/**
 * CR 702.124: Of `candidates`, which can legally pair with `first_commander`
 * as a co-commander? Applies the full partner family (generic Partner, Partner
 * with [Name], Friends Forever, Character Select, Doctor's Companion, Choose a
 * Background) via the engine's single-authority `can_pair_commanders`. The
 * frontend must not re-derive partner-pairing rules — it filters its candidate
 * list through this. Returns an empty array if the database isn't loaded.
 */
export function commanderPartnerCandidates(first_commander: string, candidates: any): any;

/**
 * Returns legal Commander-family companion candidates from the main deck.
 */
export function companionCandidates(request: any): any;

/**
 * Create a default 2-player game state.
 */
export function create_initial_state(): any;

/**
 * CR 100.2a / CR 903.5b: The named card's per-card deck-construction copy-limit
 * override, or `null` when the default four-of / singleton limit applies.
 * Serialized as the `DeckCopyLimit` tagged union (`{"type":"Unlimited"}` or
 * `{"type":"UpTo","data":N}`); the frontend must switch on `.type`. The engine
 * is the single authority — the frontend never re-parses Oracle text.
 */
export function deckCopyLimit(name: string): any;

/**
 * Estimates a Commander deck's bracket without touching `GAME_STATE`.
 * Reads `CARD_DB` for bracket signals. Returns `null` (via serde) when the
 * deck has no commander or the card database is not loaded.
 */
export function estimate_bracket_for_deck(deck_js: any): any;

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
 * Serialize the current game's replay recording to a JSON string — the
 * format `load_replay_for_playback` reads back. Errors if no game has been
 * initialized in this worker (or the recording was invalidated by undo).
 */
export function export_replay_log(): string;

/**
 * Return the authoritative list of user-selectable formats as a typed array.
 * The frontend treats this as the single source of truth for rendering
 * format pickers, badges, and default configs — no hand-maintained mirrors.
 */
export function getFormatRegistry(): any;

/**
 * Mint an opaque, authority-bound proposal for the AI's next action.
 *
 * Callers must submit it through [`submit_ai_action_proposal`]. The registry
 * is local to this live WASM instance and is cleared
 * on every successful state mutation, restore, resume, reset, and new game.
 */
export function get_ai_action_proposal(difficulty: string, player_id: number): any;

/**
 * Convert score-only worker output into an authority-bound proposal.
 *
 * The worker state may be old, from another game, or maliciously altered.
 * Consequently this endpoint always derives a new decision contract from the
 * main WASM state, discards every score whose action is not an exact member,
 * and only then mints an opaque proposal. There is intentionally no public
 * score-to-`GameAction` endpoint.
 */
export function get_ai_action_proposal_from_scores(scores_json: string, difficulty: string, player_id: number, rng_seed: bigint): any;

/**
 * Diagnostic counterpart of score-worker proposal rebinding. It preserves the
 * existing authority filter and selector; the returned receipt is local WASM
 * observability data bound to the same opaque token.
 */
export function get_ai_action_proposal_from_scores_with_diagnostics(scores_json: string, difficulty: string, player_id: number, rng_seed: bigint): any;

/**
 * Mint an ordinary opaque proposal together with a local-only diagnostic
 * receipt. The receipt is an observation of the minted capability, never an
 * additional action-selection API.
 */
export function get_ai_action_proposal_with_diagnostics(difficulty: string, player_id: number): any;

/**
 * Score candidates inside an isolated AI worker. These are plain,
 * serializable hints rather than capabilities: they cannot cross the action
 * boundary until the live main engine reissues an exact proposal.
 */
export function get_ai_scored_candidates(difficulty: string, player_id: number, rng_seed: bigint): any;

/**
 * Mint a proposal using the existing tactical floor without entering
 * rollout search. This is the engine-owned escape for a timed-out optional
 * scorer; it still issues and validates the current decision contract.
 */
export function get_ai_tactical_action_proposal(difficulty: string, player_id: number): any;

/**
 * Diagnostic counterpart of [`get_ai_tactical_action_proposal`].
 */
export function get_ai_tactical_action_proposal_with_diagnostics(difficulty: string, player_id: number): any;

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
 * Returns the official WotC rulings for a card as a JS array of `{date, text}`
 * objects. Returns an empty array if the card is not found, the database is
 * not loaded, or the card has no rulings (back faces of multi-face cards
 * inherit empty rulings — they're deduped at export time to the front face).
 */
export function get_card_rulings(name: string): any;

/**
 * Filtered-viewer variant of `get_game_state`. Runs the viewer filter
 * first (hides opponent hand/library per standard multiplayer redaction),
 * then derives views over the filtered state so the wire shape is
 * identical to `get_game_state` regardless of filter path.
 */
export function get_filtered_game_state(viewer: number): any;

/**
 * Get the current game state as a `ClientGameState` wire envelope
 * (`{ state, derived }`). The `derived` block holds engine-authored
 * presentation projections — commander-damage grouping, etc. — so the
 * frontend never computes game logic. Derivation happens just-in-time per
 * call and does not mutate `GameState`. See
 * `engine::game::derived_views::ClientGameStateRef`.
 */
export function get_game_state(): any;

/**
 * Viewer-scoped legal actions. Returns the same shape as `get_legal_actions_js`
 * but empty when the viewer is not the player currently expected to act. Used
 * by the P2P host to broadcast per-guest legal-action payloads without leaking
 * game logic into the transport adapter.
 */
export function get_legal_actions_for_viewer_js(player_id: number): any;

/**
 * Get the legal actions, auto-pass recommendation, and spell costs for the current game state.
 * Returns `{ actions: GameAction[], autoPassRecommended: boolean, spellCosts: Record<string, ManaCost> }`.
 */
export function get_legal_actions_js(): any;

/**
 * Current stack pressure bucket for animation pacing (Normal/Elevated/Rapid/Instant).
 * Not a rules concept — presentation policy owned by the engine for consistency
 * across browser/desktop/server consumers. Returned as a string to avoid
 * tsify enum-sharing overhead; frontend maps the string to a multiplier.
 */
export function get_stack_pressure(): any;

export function get_viewer_snapshot_js(player_id: number): any;

/**
 * Whether the current game has an in-progress replay recording. `false`
 * before any game has started, or after the recording was invalidated by
 * undo/restore (see `restore_game_state`).
 */
export function has_replay_recording(): boolean;

/**
 * Initialize panic hook for better error messages in WASM.
 * Called automatically on first use — safe to call multiple times.
 *
 * We install our own hook (composing with `console_error_panic_hook`'s
 * console output) so panics are *both* logged to devtools and captured
 * for later retrieval. With `panic = 'abort'`, the hook runs before the
 * WASM trap, so a thread-local written here is readable from the next JS
 * call into the module.
 */
export function init_panic_hook(): void;

/**
 * Initialize a new game for local (single-player / AI) play.
 * Accepts deck_data as a DeckList (name-only) or null/undefined for empty libraries.
 * format_config_js: optional FormatConfig JSON — defaults to Standard if null/undefined.
 * match_config_js: optional MatchConfig JSON — defaults to BO1 if null/undefined.
 * player_count: number of players — defaults to 2 if not provided.
 * first_player: 0 = human plays first (CR 103.1), 1 = opponent plays first, None = random.
 * Names are resolved against the card database loaded via load_card_database().
 * Returns the initial ActionResult (events + waiting_for).
 *
 * Refuses with an `engine_occupied` envelope when a multiplayer host session
 * holds this engine — on a memory-constrained device that host shares this
 * very worker, and overwriting its game would destroy the authoritative state
 * its guests are playing against.
 */
export function initialize_game(deck_data: any, seed: number | null | undefined, format_config_js: any, match_config_js: any, player_count?: number | null, first_player?: number | null): any;

/**
 * Initialize a new game *and* claim this engine for a multiplayer host
 * session, in one call.
 *
 * Same parameters and same return envelope as `initialize_game`. The P2P host
 * uses this instead, for two reasons that only a single call can satisfy:
 *
 * 1. **Refuses an occupied engine.** A hosted game must never start on top of
 *    a live local game. A client-side probe followed by an install is two
 *    round-trips with a window between them; this guard runs inside the same
 *    synchronous worker task as the install, so nothing can interleave.
 * 2. **Atomic multiplayer-flag claim.** The flag is set on the line after the
 *    state install (see `claim_engine_for`), so there is no window where a
 *    stray `restore_game_state` (undo) would be accepted, and no window where
 *    a failed init leaves the flag set on an engine it never took. Mirrors
 *    `resume_multiplayer_host_state`, the resume-side sibling of this call.
 */
export function initialize_multiplayer_host_game(deck_data: any, seed: number | null | undefined, format_config_js: any, match_config_js: any, player_count?: number | null, first_player?: number | null): any;

/**
 * Whether the named card can serve as this format's command-zone leader.
 * Reads the engine's MTGJSON-derived `CardFace` leadership fields and
 * format-specific deck-validation predicates.
 */
export function isCardCommanderEligibleForFormat(name: string, format: any): boolean;

/**
 * CR 903.3: Whether the named card can serve as a commander
 * (legendary creature, legendary background, or "can be your commander").
 * Returns false if the card database isn't loaded or the card isn't found.
 */
export function is_card_commander_eligible(name: string): boolean;

/**
 * Read the multiplayer enforcement flag. Exposed primarily for tests and
 * adapters that need to defend their own paths (e.g., skip history pushes).
 */
export function is_multiplayer_mode(): boolean;

/**
 * Read-only preview of cast-time target slots for a currently castable spell.
 * Returns `[]` for uncastable, untargeted, or target-ambiguous casts.
 */
export function legal_targets_for_castable_js(object_id: number): any;

/**
 * Batch variant for hover/drag clients that need previews for many castable
 * cards. The engine flushes layers once and reuses that snapshot for every id.
 */
export function legal_targets_for_castables_js(object_ids: any): any;

/**
 * Returns the engine-typed catalog of debug-spawnable token presets,
 * loaded from `crates/engine/data/known-tokens.toml`. Read by the debug UI
 * to populate the Create Token dropdown — frontend never derives this list.
 */
export function list_token_presets_js(): any;

/**
 * Load the card database from a JSON string (card-data.json contents).
 * Must be called before initialize_game to enable name-based deck resolution.
 */
export function load_card_database(json_str: string): number;

/**
 * Load a replay log (the JSON produced by `export_replay_log`) for
 * scrubbing/playback. Independent of the live `GAME_STATE` — does not
 * require, and does not affect, an active game. Uses the loaded `CARD_DB`
 * to resolve the recorded deck list when reconstructing the starting
 * state — and errors (rather than silently reconstructing empty
 * libraries) if the replay carries deck data but no card database is
 * loaded; see `ReplayError::MissingCardDatabase`. Returns the total number
 * of recorded actions; valid `replay_seek_js` targets are `0..=length`.
 */
export function load_replay_for_playback(json_str: string): number;

/**
 * CR 100.2a / CR 903.5b: How many copies of the named card a `format` deck may
 * legally contain across main deck, sideboard, and command zone combined
 * (CR 100.4a). Unlike `deckCopyLimit`, this is the *resolved* ceiling — it
 * already applies the basic-land exemption, the card's printed override, and
 * the format default, so the caller compares a count against it directly.
 *
 * Serialized as the `DeckCopyLimit` tagged union (`{"type":"Unlimited"}` or
 * `{"type":"UpTo","data":N}`); switch on `.type`. Returns `{"type":"Unlimited"}`
 * when the card database isn't loaded, so a not-yet-hydrated frontend never
 * blocks a legal add.
 */
export function maxDeckCopies(name: string, format: any): any;

/**
 * Verify WASM integration works.
 */
export function ping(): string;

/**
 * Issue #5468: non-mutating dry-run of `action` for `actor`. Runs the action on
 * a throwaway clone (the live `GAME_STATE` is never touched) and returns the
 * PUBLIC deltas — life-total changes, public-zone object transitions, created
 * tokens, and objects that ceased to exist — a viewer could observe, for
 * hover-preview UX ("this kills that", "you take 4").
 *
 * Hidden-zone movements never leak: the diff is taken over
 * `filter_state_for_viewer` snapshots (so any identity the viewer can't see is
 * already redacted), AND a transition is surfaced only when at least one
 * endpoint is a public zone (see `engine::game::preview`), so a fully-hidden
 * hand↔library draw is elided even for the acting player's opponents. Returns
 * an error string when `action` is malformed or illegal in the current state.
 */
export function preview_action_js(actor: number, action: any): any;

/**
 * Non-mutating automatic spell-payment preview. The engine simulates the
 * exact, currently legal `CastSpell` action and returns the permanent ids that
 * produced mana before that spell was committed to the stack. It returns an
 * empty array when the cast needs another choice before payment can be final.
 */
export function preview_mana_payment_js(actor: number, action: any): any;

/**
 * Project an authoritative seat view from Rust so frontend transports do not
 * need to understand format topology details.
 */
export function project_seat_view(state_json: string): any;

/**
 * The loaded replay's header (format/match config, player count, seed,
 * deck data), or `null` if none is loaded. Lets the viewer show "vs. <deck>"
 * chrome without re-deriving it from the action sequence.
 */
export function replay_header_js(): any;

/**
 * Total number of recorded actions in the loaded replay, or `0` if none is loaded.
 */
export function replay_length_js(): number;

/**
 * Seek the loaded replay to `target` (clamped to the recording's length) and
 * return the reconstructed state at that point, wrapped the same way
 * `get_game_state` wraps the live state. Returns `Ok(null)` only when no
 * replay is loaded — a reconstruction desync (`ReplayError::Desync`, an
 * engine-version mismatch between recording and playback, not a rules
 * outcome) is a real failure and must not be silently swallowed into the
 * same null the caller uses for "nothing loaded"; it throws instead, like
 * every other fallible engine entry point that returns `Result<_, JsValue>`.
 */
export function replay_seek_js(target: number): any;

export function resolve_all(requester: number, ai_seats_json: string, max_resolutions: number): any;

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
 * 1. **Fresh RNG seed.** `restore_game_state` re-seeds from the SAVED
 *    `rng_seed` and fast-forwards to the saved `rng_word_pos`, so the
 *    restored game continues the very stream the snapshot was taken on —
 *    correct for undo, wrong for resume, where continued play must not
 *    re-draw the values the pre-save timeline already committed to. This
 *    function stamps a FRESH seed and resets `rng_word_pos` to 0 so the
 *    resumed host diverges instead.
 *
 *    It does NOT rewind to position 0: that was true only before issue
 *    #5466 taught the restore path to carry the offset, and it survives
 *    today just for snapshots written back then, which carry
 *    `rng_word_pos == 0`. Both the shared decode chokepoint
 *    (`PersistedGameState::into_game_state`) and `restore_game_state`'s
 *    own repeat call `rehydrate_rng`.
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
 * Search the loaded card database. The engine is the single authority for the
 * rules data search filters on — format legality, set membership, card types,
 * mana value, and colors — so deck-builder search runs here, never as a
 * third-party API call. Returns `{ results, total }` (see `CardSearchResults`),
 * or an error if the database is not loaded or the query is malformed.
 */
export function search_cards_js(query: any): any;

/**
 * Set the multiplayer enforcement flag directly.
 *
 * Entering multiplayer is *not* done here: the engine claims the flag itself,
 * in the same call that installs the game (`initialize_multiplayer_host_game`,
 * `resume_multiplayer_host_state`), so no client can leave the flag and the
 * game it describes out of step. This entry point serves the release side —
 * `releaseHostSession` clears the flag when a host session ends, so the next
 * local game on a shared worker may undo again.
 */
export function set_multiplayer_mode(enabled: boolean): void;

/**
 * CR 100.4a: Returns the sideboard policy for a given game format as a
 * tagged union: `{"type": "Forbidden"}`, `{"type": "Limited", "data": 15}`,
 * or `{"type": "Unlimited"}`.
 *
 * The frontend must exhaustive-switch on `.type` — unit variants (`Forbidden`,
 * `Unlimited`) emit no `data` field under `#[serde(tag, content)]`.
 *
 * The engine is the single authority for format sideboard rules; the frontend
 * never hardcodes 15 or any other cap.
 */
export function sideboardPolicyForFormat(format: any): any;

/**
 * Returns the engine-authored Oathbreaker signature-spell selection policy.
 */
export function signatureSpellSelectionPolicy(request: any): any;

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

/**
 * Submit an action selected from an engine-issued AI proposal.
 *
 * A stale or foreign proposal is a normal race outcome and is returned as a
 * tagged value. Rejected actions leave the proposal live for diagnostics or a
 * retry; only a successful apply invalidates the authority generation.
 */
export function submit_ai_action_proposal(token: string, actor: number, action: any): any;

/**
 * Submit one opaque, engine-authored interaction response. The browser never
 * materializes a `GameAction`; only a successful engine reducer result exposes
 * the exact action to the replay recorder.
 */
export function submit_interaction_js(actor: number, submission: any): any;

/**
 * Drain the last captured panic message (consuming it). Returns `null` when
 * no panic has been observed since the last drain. JS calls this after a
 * thrown `RuntimeError` to decide whether to surface the modal as a real
 * engine crash (with the panic text + report link) or a transient
 * state-loss (the legacy reload prompt).
 */
export function take_last_panic_message(): string | undefined;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly apply_seat_mutation: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly build_ai_card_subset: () => [number, number, number, number];
    readonly classify_deck_js: (a: any) => [number, number, number];
    readonly clear_game_state: () => void;
    readonly commanderPartnerCandidates: (a: number, b: number, c: any) => [number, number, number];
    readonly companionCandidates: (a: any) => [number, number, number];
    readonly deckCopyLimit: (a: number, b: number) => any;
    readonly estimate_bracket_for_deck: (a: any) => [number, number, number];
    readonly evaluate_deck_compatibility_js: (a: any) => [number, number, number];
    readonly export_game_state_json: () => [number, number, number, number];
    readonly export_replay_log: () => [number, number, number, number];
    readonly get_ai_action_proposal: (a: number, b: number, c: number) => [number, number, number];
    readonly get_ai_action_proposal_from_scores: (a: number, b: number, c: number, d: number, e: number, f: bigint) => [number, number, number];
    readonly get_ai_action_proposal_from_scores_with_diagnostics: (a: number, b: number, c: number, d: number, e: number, f: bigint) => [number, number, number];
    readonly get_ai_action_proposal_with_diagnostics: (a: number, b: number, c: number) => [number, number, number];
    readonly get_ai_scored_candidates: (a: number, b: number, c: number, d: bigint) => [number, number, number];
    readonly get_ai_tactical_action_proposal: (a: number, b: number, c: number) => [number, number, number];
    readonly get_ai_tactical_action_proposal_with_diagnostics: (a: number, b: number, c: number) => [number, number, number];
    readonly get_card_face_data: (a: number, b: number) => any;
    readonly get_card_parse_details: (a: number, b: number) => any;
    readonly get_card_rulings: (a: number, b: number) => any;
    readonly get_filtered_game_state: (a: number) => any;
    readonly get_legal_actions_for_viewer_js: (a: number) => any;
    readonly get_viewer_snapshot_js: (a: number) => any;
    readonly has_replay_recording: () => number;
    readonly initialize_game: (a: any, b: number, c: number, d: any, e: any, f: number, g: number) => any;
    readonly initialize_multiplayer_host_game: (a: any, b: number, c: number, d: any, e: any, f: number, g: number) => any;
    readonly isCardCommanderEligibleForFormat: (a: number, b: number, c: any) => number;
    readonly is_card_commander_eligible: (a: number, b: number) => number;
    readonly is_multiplayer_mode: () => number;
    readonly legal_targets_for_castable_js: (a: number) => any;
    readonly legal_targets_for_castables_js: (a: any) => any;
    readonly load_card_database: (a: number, b: number) => [number, number, number];
    readonly load_replay_for_playback: (a: number, b: number) => [number, number, number];
    readonly maxDeckCopies: (a: number, b: number, c: any) => any;
    readonly ping: () => [number, number];
    readonly preview_action_js: (a: number, b: any) => any;
    readonly preview_mana_payment_js: (a: number, b: any) => any;
    readonly project_seat_view: (a: number, b: number) => [number, number, number];
    readonly replay_seek_js: (a: number) => [number, number, number];
    readonly resolve_all: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly restore_game_state: (a: number, b: number) => [number, number];
    readonly resume_multiplayer_host_state: (a: number, b: number) => [number, number];
    readonly search_cards_js: (a: any) => [number, number, number];
    readonly set_multiplayer_mode: (a: number) => void;
    readonly sideboardPolicyForFormat: (a: any) => [number, number, number];
    readonly signatureSpellSelectionPolicy: (a: any) => [number, number, number];
    readonly submit_action: (a: number, b: any) => any;
    readonly submit_ai_action_proposal: (a: number, b: number, c: number, d: any) => any;
    readonly submit_interaction_js: (a: number, b: any) => any;
    readonly take_last_panic_message: () => [number, number];
    readonly get_game_state: () => any;
    readonly get_legal_actions_js: () => any;
    readonly get_stack_pressure: () => any;
    readonly init_panic_hook: () => void;
    readonly replay_header_js: () => any;
    readonly list_token_presets_js: () => any;
    readonly create_initial_state: () => any;
    readonly getFormatRegistry: () => any;
    readonly clear_replay_playback: () => void;
    readonly replay_length_js: () => number;
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
