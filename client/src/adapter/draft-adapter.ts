import type * as DraftWasm from "@wasm/draft";
import type { MatchConfig } from "./types";
import type {
  LlmDraftOutcome,
  LlmDraftPickRequest,
} from "../services/llm/types";

/** One seat's LLM reply, handed back to the engine to resolve into a pick. */
export interface LlmDraftResponsePayload {
  seat: number;
  fingerprint: string;
  provider: string;
  /** HTTP status, so the engine can refuse a non-2xx reply whatever its body
   *  looks like. */
  status: number;
  body: string;
}

// ── Types (mirror Rust serde output from draft-core) ────────────────────

export interface DraftCardInstance {
  instance_id: string;
  name: string;
  set_code: string;
  collector_number: string;
  rarity: string;
  colors: string[];
  cmc: number;
  type_line: string;
  draft_effect?: "additional_pick";
}

export type DraftPoolGroupKind =
  | "white"
  | "blue"
  | "black"
  | "red"
  | "green"
  | "multicolor"
  | "colorless"
  | "creature"
  | "instant"
  | "sorcery"
  | "enchantment"
  | "artifact"
  | "planeswalker"
  | "land"
  | "other"
  | "mythic"
  | "rare"
  | "uncommon"
  | "common"
  | "rarity_other"
  | "mana_value0"
  | "mana_value1"
  | "mana_value2"
  | "mana_value3"
  | "mana_value4"
  | "mana_value5"
  | "mana_value6_plus";

export type DraftRarityGroupKind =
  | "mythic"
  | "rare"
  | "uncommon"
  | "common"
  | "rarity_other";

export interface DraftPoolEntry {
  card: DraftCardInstance;
  count: number;
  /** Every collapsed copy's instance id — the collapse keys on the name, so
   * same-name instances (a reprint at a different rarity) are only
   * addressable through these. */
  instance_ids: string[];
}

export interface DraftPoolGroup {
  kind: DraftPoolGroupKind;
  total: number;
  cards: DraftPoolEntry[];
}

export interface DraftPoolColorCounts {
  white: number;
  blue: number;
  black: number;
  red: number;
  green: number;
}

export interface DraftWorkspaceCapabilities {
  rarity_group_order: DraftRarityGroupKind[] | null;
}

export interface DraftWorkspaceRowClassification {
  creature_instance_ids: string[];
  noncreature_instance_ids: string[];
}

/** Typed filter contract mirroring `draft_core::view::PoolFilter` (#7546):
 * the display sends WHAT it asks for; the engine decides WHICH instances
 * match. Empty axis = unconstrained. */
export interface PoolFilter {
  query: string;
  types: DraftPoolGroupKind[];
  colors: DraftPoolGroupKind[];
  rarities: DraftPoolGroupKind[];
}

/** Engine-computed filter option lists (`draft_core::view::PoolFilterOptions`):
 * the stateless path for views that predate the option fields. */
export interface PoolFilterOptions {
  types: DraftPoolGroupKind[];
  colors: DraftPoolGroupKind[];
  rarities: DraftPoolGroupKind[];
}

export interface DraftPoolGroups {
  color_groups: DraftPoolGroup[];
  type_groups: DraftPoolGroup[];
  cmc_groups: DraftPoolGroup[];
  rarity_groups: DraftPoolGroup[];
  /** Engine-owned option list for a type-filter control: every type bucket
   * any pool member belongs to (multi-valued), in engine order. The exclusive
   * `type_groups` axis stays a presentation/sorting shape. */
  type_filter_options: DraftPoolGroupKind[];
  /** Engine-owned option list for a color-filter control (CR 105.2: a card
   * can be one or more colors). The exclusive `color_groups` axis stays a
   * presentation shape. */
  color_filter_options: DraftPoolGroupKind[];
  color_counts: DraftPoolColorCounts;
  workspace_capabilities: DraftWorkspaceCapabilities;
  workspace_row_classification: DraftWorkspaceRowClassification;
}

/** Empty engine-shaped pool data for a lobby before a draft session exists. */
export const EMPTY_DRAFT_POOL_GROUPS: DraftPoolGroups = {
  color_groups: [],
  type_groups: [],
  cmc_groups: [],
  rarity_groups: [],
  type_filter_options: [],
  color_filter_options: [],
  color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
  workspace_capabilities: { rarity_group_order: null },
  workspace_row_classification: {
    creature_instance_ids: [],
    noncreature_instance_ids: [],
  },
};

// @sync-with: crates/draft-core/src/view.rs
export interface SeatPublicView {
  seat_index: number;
  display_name: string;
  is_bot: boolean;
  connected: boolean;
  has_submitted_deck: boolean;
  /**
   * `"Waiting"` is the shared-stack seat that is not the active seat: no seat
   * ever holds a `current_pack` under `SharedStackPiles`, so the engine cannot
   * describe that seat with the pick-and-pass pair. Engine-owned; never derive
   * it from `active_pack_count`.
   */
  pick_status: "Pending" | "Picked" | "Waiting" | "TimedOut" | "NotDrafting";
  /**
   * Engine-owned active-pack presence: exactly 0 or 1, never a card count.
   * Required by P2P draft v24 and the full WebSocket protocol v49.
   */
  active_pack_count: number;
  /**
   * How many cards this seat has drafted so far — a count, never an identity.
   * Public in every draft kind: a pick-and-pass seat's total is already implied
   * by the pick number, and at a shared stack the players watch each other's
   * drafted pile grow across the table.
   *
   * Distinct from `active_pack_count`, which answers "is this seat holding a
   * pack right now" and is 0 under `SharedStackPiles` by construction.
   */
  drafted_card_count: number;
  face_up_draft_cards: DraftCardInstance[];
}

export type DraftStatus =
  | "Lobby"
  | "Drafting"
  | "Paused"
  | "Deckbuilding"
  | "Pairing"
  | "MatchInProgress"
  | "RoundComplete"
  | "Complete"
  | "Abandoned";

/**
 * Every draft kind, as one runtime tuple the type is DERIVED from.
 *
 * The tuple exists because a type-guard body is not checked against its target
 * union: `function isDraftKind(v): v is DraftKind` compiles whether the body
 * enumerates six kinds or two, so a duplicated enumeration beside the union
 * goes silently narrow the moment a kind is added — and a persisted session of
 * the new kind is then discarded on resume with no error anywhere. Folding the
 * guard over this tuple makes the enumeration the type, so the class cannot
 * recur at the next widening. Never restate these members anywhere else;
 * derive from `DRAFT_KINDS`.
 */
// @sync-with: crates/draft-core/src/types.rs `DraftKind::ALL`
export const DRAFT_KINDS = [
  "Quick",
  "Premier",
  "Traditional",
  "Sealed",
  "CommanderDraft",
  "Winston",
] as const;

export type DraftKind = (typeof DRAFT_KINDS)[number];

/**
 * View-safe source metadata from `draft_core::view::DraftSourceView`.
 *
 * This is intentionally not the persisted DraftSource. In particular, a
 * Chaos view can name its candidate sets and the viewer's scoped information,
 * but cannot represent the host-only assignment matrix.
 */
export type DraftSourceView =
  | {
      type: "Set";
      data: { layout: DraftSetLayoutView };
    }
  | {
      type: "Cube";
      data: { id: string; name: string };
    };

/** Matches the externally tagged Rust `SetLayoutView` enum. */
export type DraftSetLayoutView =
  | { UniformByRound: { codes: string[] } }
  | {
      Chaos: {
        candidate_codes: string[];
        current_pack_code: string | null;
        completed_own_pack_codes: string[] | null;
        actual_set_codes: string[] | null;
      };
    };

/** What the engine does after every seat has submitted a deck. */
export type PostDraftPlay = "CompleteImmediately" | "TournamentPairings";
/** How the engine procedure distributes packs to seats. */
/**
 * How the engine procedure distributes packs to seats.
 *
 * `SharedStackPiles` is the externally-tagged member, mirroring the Rust
 * `PackDistribution::SharedStackPiles { pile_count }` — the same spelling
 * `DraftSetLayoutView` above uses for its data-carrying variants. The two
 * string members stay bare because their Rust counterparts are unit variants.
 *
 * Every existing `distribution === "AllAtOnce"` comparison stays both
 * type-valid and meaning-correct against this union: each asks "is this the
 * one-shot sealed shape?", whose answer for a shared stack is `false`.
 */
export type PackDistribution =
  | "PickAndPass"
  | "AllAtOnce"
  | { SharedStackPiles: { pile_count: number } };

/**
 * Is this the shared-stack distribution, and if so what is its pile count?
 *
 * THE single client-side spelling of that question. The engine no longer
 * refuses a bot seat under `PackDistribution::SharedStackPiles` — it seats and
 * drives one (`resolve_shared_stack_bot_turns`) — so this predicate is NOT a
 * refusal proxy and must never be reintroduced as one. What it still answers is
 * the only question a shared-stack pod's turn structure poses: is this the
 * one-active-seat, take-or-decline procedure, whose seats have no
 * `current_pack` at all? Both surviving consumers ask exactly that —
 * `autoPickAllPending`'s timeout sweep and `resolveBotPicks`'s bot dispatch,
 * each of which would otherwise fall through to a `current_pack` loop that is
 * null for every seat here.
 *
 * Ask the DISTRIBUTION, never the `human_seats` scalar. That scalar is a
 * per-kind constant that merely correlates, and it correlates WRONGLY: the
 * procedure table seats humans in every seat for Premier, Traditional and
 * Sealed too (`human_seats == pod_size == 8`), so a guard written on it
 * silently caught three kinds it was never about. That lesson outlived the
 * refusal it was learned on, and `p2pDraftHostBotFill.test.ts` is its
 * revert-probe.
 *
 * A narrowing predicate rather than a `boolean`, so a caller that needs
 * `pile_count` gets it from the same test instead of re-destructuring the
 * union and re-deciding what counts as a shared stack.
 *
 * `null` — the setup surfaces' "the engine has not published a procedure yet"
 * state — answers `false`, so a caller need not re-spell the question with its
 * own null test. The explicit `!== null` is load-bearing rather than defensive:
 * `typeof null === "object"` in JavaScript, so without it the `in` below throws
 * on exactly that input.
 */
export function isSharedStackDistribution(
  distribution: PackDistribution | null,
): distribution is { SharedStackPiles: { pile_count: number } } {
  return distribution !== null
    && typeof distribution === "object"
    && "SharedStackPiles" in distribution;
}

/** Engine-authorized game launch for a completed draft procedure. */
export type DraftLaunchCapability = "None" | "CommanderMultiplayer";

/**
 * The numeric kind the wasm bridge expects. Mirrors `draft_kind_wire_number`
 * in `crates/draft-wasm/src/lib.rs`, which is the single authority.
 *
 * The TOTALITY of this Record is the compile-time half of the kind boundary's
 * loudness guarantee: `Record<K, V>` requires every member of
 * `Exclude<DraftKind, "Quick">`, so widening the union without adding a wire
 * number here is a TS2741 before any bytes move. Never relax this to
 * `Partial<Record<…>>`, never `?? 0`, and never assert on the index — that
 * would trade a compile error for a draft created as the WRONG kind.
 */
// @sync-with: crates/draft-wasm/src/lib.rs
const DRAFT_KIND_WIRE_NUMBER: Record<Exclude<DraftKind, "Quick">, number> = {
  Premier: 1,
  Traditional: 2,
  Sealed: 3,
  CommanderDraft: 4,
  Winston: 5,
};

/**
 * The engine-owned per-kind procedure axes, mirroring `DraftProcedureDto` in
 * `crates/draft-wasm/src/lib.rs`. Read these; never re-derive them.
 */
// @sync-with: crates/draft-wasm/src/lib.rs
/** The discriminant of a set layout, without its payload. Mirrors the Rust
 *  `SetLayoutKind`. */
export type SetLayoutKind = "UniformByRound" | "Chaos";

export interface DraftProcedure {
  pod_size: number;
  human_seats: number;
  min_pod_size: number;
  max_pod_size: number;
  /** Exact engine-allowed seat counts for the requested tournament format. */
  allowed_pod_sizes: number[];
  packs_per_player: number;
  cards_per_pick: number;
  /** Engine-owned interaction policy for selecting cards in one pick step. */
  pick_selection_mode: "Direct" | "Ordered";
  distribution: PackDistribution;
  /**
   * Which set-layout shapes this kind admits, published by the engine.
   *
   * Render exactly this list. Do NOT re-derive layout legality from
   * `distribution` -- that was a second authority over a rule the engine owns
   * (`DraftProcedure::allowed_set_layouts`, which `validate_source` and the
   * server's admission guard both read), correct only by coincidence and free
   * to drift the moment a distribution is added.
   */
  allowed_set_layouts: SetLayoutKind[];
  min_deck_size: number;
  /** Engine-owned minimum accepted for cube settings under this procedure. */
  cube_min_deck_size: number;
  /**
   * CR 903.3: how many commanders a deck built from this kind's pool must
   * designate. `0` for the four CR 905.1a kinds, `1` for CommanderDraft.
   * Required, not optional: a literal that forgets it must be a `tsc` error
   * rather than a silent `undefined`, which is the whole point of a mirror.
   */
  commanders_required: number;
  /** Engine-owned tournament-pairing capability for this draft kind. */
  post_draft_play: PostDraftPlay;
  /** Engine-authorized game launch for a completed draft procedure. */
  launch_capability: DraftLaunchCapability;
  match_config: MatchConfig;
}

export type TournamentFormat = "Swiss" | "SingleElimination";

export type PodPolicy = "Competitive" | "Casual";

export type PairingStatus = "Pending" | "InProgress" | "Complete";

/** Fields consumed by `DraftProgress` (shared by player and spectator views). */
export interface DraftProgressFields {
  current_pack_number: number;
  pick_number: number;
  /** Cards in the booster being drafted right now. */
  cards_per_pack: number;
  /** Cards in each booster, in pack order. Multi-set drafts mix sizes. */
  pack_sizes?: number[];
  /** The set filling each booster, in pack order. */
  pack_set_codes?: string[];
  /**
   * Safe source metadata. Optional while peers transition to the redacted
   * source contract; it never falls back to a persisted source snapshot.
   */
  source?: DraftSourceView;
  /**
   * CR 903.13b: pick STEPS in each booster, in pack order — the per-pack
   * counterpart of `pick_steps_per_pack`. A progress display measures each
   * booster against this, never against `pack_sizes`: the two differ whenever
   * a kind takes more than one card per step.
   */
  pack_pick_steps?: number[];
  /**
   * CR 903.13b: how many pick STEPS this session's pack contains —
   * `cards_per_pack.div_ceil(cards_per_pick)`, computed by the engine's
   * `DraftProcedure::pick_steps_per_pack`. `pick_number` counts steps, not
   * cards, so this is the denominator a progress bar can actually reach: a
   * 14-card Commander pack is 7 steps, not 14. Read it; never re-derive it
   * from `cards_per_pack`.
   */
  pick_steps_per_pack: number;
  pack_count: number;
  pass_direction: "Left" | "Right";
}

// @sync-with: crates/draft-core/src/view.rs
export interface StandingEntry {
  seat_index: number;
  display_name: string;
  match_wins: number;
  match_losses: number;
  game_wins: number;
  game_losses: number;
}

// @sync-with: crates/draft-core/src/view.rs
export interface PairingView {
  round: number;
  table: number;
  seat_a: number;
  name_a: string;
  seat_b: number;
  name_b: string;
  match_id: string;
  status: PairingStatus;
  winner_seat: number | null;
  /** Game wins for seat A in the current match (Bo3 tracking). */
  score_a: number | null;
  /** Game wins for seat B in the current match (Bo3 tracking). */
  score_b: number | null;
}

/**
 * Take the pile, or put it back. Mirrors the Rust `SharedStackPileDecision`,
 * which is deliberately a named axis rather than a boolean so a refusal, a
 * delta and an i18n key can all key on it.
 */
// @sync-with: crates/draft-core/src/types.rs
export type SharedStackPileDecision = "Take" | "Decline";

/**
 * Every reason the engine can refuse a shared-stack decision. ONE vocabulary
 * for the reducer's refusal and the view's publication, so the display layer
 * renders the engine's reason instead of reinventing it.
 */
// @sync-with: crates/draft-core/src/types.rs
export type SharedStackRefusal = "PileNotActive" | "PileEmpty" | "NoGuaranteedCard";

/**
 * One decision and the engine's verdict on it. `refusal: null` means legal.
 *
 * The reducer converts this same value into `SharedStackDecisionRefused`, so
 * the published verdict and the enforced one cannot disagree — which is why
 * nothing in the client may compute legality from `total` or
 * `main_stack_remaining`.
 */
// @sync-with: crates/draft-core/src/view.rs
export interface SharedStackDecisionView {
  decision: SharedStackPileDecision;
  refusal: SharedStackRefusal | null;
}

/**
 * One seat's decision on one pile, and how tall that pile was when they made
 * it.
 *
 * PUBLIC INFORMATION: at a physical table everyone watches a player pick a pile
 * up, weigh it and put it back, and every pile's HEIGHT is visible across the
 * table (the same reason `SharedStackPileView.total` is published to every
 * viewer).
 *
 * WHAT IS NOT HERE, and must never be added: the pile's CONTENTS. This record
 * is the one place a future author might reach for them, and they are the
 * format's only secret. A consumer that wants to know WHICH cards a seat passed
 * reconstructs them from ITS OWN published `revealed` prefix plus `pile_size` —
 * exactly the information a player at the table has.
 */
// @sync-with: crates/draft-core/src/types.rs
export interface SharedStackDecisionRecord {
  /** The seat that decided. */
  seat: number;
  /** The pile it decided on, addressed by the engine's own pile index (the same
   * index `SharedStackPileView.index` publishes), never by a position in a
   * vector. */
  pile: number;
  decision: SharedStackPileDecision;
  /** The pile's height at the moment of the decision, captured before the
   * decision moved the pile. */
  pile_size: number;
}

/** One shared-stack pile, projected for one viewer. */
// @sync-with: crates/draft-core/src/view.rs
export interface SharedStackPileView {
  /** Position from the left, 0-based. Address a pile by this, not by its index
   * in `piles`. */
  index: number;
  /** How many cards the pile holds. A face-down pile's HEIGHT is public. */
  total: number;
  /** The prefix this viewer has looked at this turn; empty for every viewer
   * that is not the active seat. Never re-derive it from `total`. */
  revealed: DraftCardInstance[];
  /** The engine's verdict per decision. Read it; never compute legality. */
  legality: SharedStackDecisionView[];
}

/**
 * The live state of a `SharedStackPiles` turn, projected for ONE viewer.
 *
 * Counts are public (a player can count every pile across a physical table),
 * and so is `active_pile`: at a physical table an opponent watches which pile
 * you are handling, and the engine's `legality` vector reveals the cursor
 * anyway (every non-cursor pile answers `PileNotActive`). `revealed` is the ONE
 * viewer-scoped field — the pile's CONTENTS are the secret. The order of the
 * main stack is published to nobody, which is why this type carries a remaining
 * COUNT and has no representation for a main-stack card.
 */
// @sync-with: crates/draft-core/src/view.rs
export interface SharedStackView {
  main_stack_remaining: number;
  total_cards: number;
  active_seat: number;
  /**
   * The pile the active seat is deciding on. Public, and non-nullable: a live
   * pile turn always has a cursor, and a session with no live turn publishes no
   * `shared_stack` at all. NEVER read this as "is it my turn" — compare
   * `active_seat` against the viewer's own seat for that.
   */
  active_pile: number;
  piles: SharedStackPileView[];
  /**
   * Applied decisions since `StartDraft` — a monotone change detector and
   * nothing else. This is the field an acknowledging client watches, because a
   * non-final decline adds no card to any pool and therefore cannot be
   * acknowledged by pool growth.
   */
  decisions: number;
  /**
   * The applied decisions the session still retains, oldest first and bounded
   * by the engine's `SHARED_STACK_HISTORY_CAPACITY`.
   *
   * Published to EVERY viewer — every seat and both spectator visibilities —
   * for the same reason `decisions` is: it is a record of PUBLIC events. It
   * carries NO CARD; see `SharedStackDecisionRecord`.
   */
  history: SharedStackDecisionRecord[];
  /**
   * The card THIS VIEWER's most recent forced draw gave them, held until they
   * decide again; `null` for every other viewer and for a spectator.
   *
   * The only private field on this type. A final-pile decline takes the top of
   * the main stack sight unseen and drops it into the declining seat's pool, so
   * this is the engine telling that seat what it just got — the one card in the
   * format a player receives without having looked at it. Do not render it for
   * anyone but its owner; the engine already refuses to send it to anyone else.
   */
  forced_draw: DraftCardInstance | null;
}

// @sync-with: crates/draft-core/src/view.rs
export interface SpectatorDraftView {
  status: DraftStatus;
  kind: DraftKind;
  /** Candidate intent only for Chaos; no seat assignment is present. */
  source?: DraftSourceView;
  current_pack_number: number;
  pick_number: number;
  pass_direction: "Left" | "Right";
  seats: SeatPublicView[];
  /** Cards in the booster being drafted right now, not a session-wide size. */
  cards_per_pack: number;
  /** Cards in each booster, in pack order. Multi-set drafts mix sizes. */
  pack_sizes?: number[];
  /** The set filling each booster, in pack order. */
  pack_set_codes?: string[];
  /**
   * CR 903.13b: pick STEPS in each booster, in pack order — the per-pack
   * counterpart of `pick_steps_per_pack`. A progress display measures each
   * booster against this, never against `pack_sizes`: the two differ whenever
   * a kind takes more than one card per step.
   */
  pack_pick_steps?: number[];
  /** CR 903.13b: mirrors `DraftPlayerView.pick_steps_per_pack`; see that one. */
  pick_steps_per_pack: number;
  pack_count: number;
  min_deck_size: number;
  addable_cards: string[];
  standings: StandingEntry[];
  current_round: number;
  tournament_format: TournamentFormat;
  pod_policy: PodPolicy;
  pairings: PairingView[];
  match_config: MatchConfig;
  /** Present only for non-Chaos drafts when the host enabled omniscient visibility. */
  pools?: DraftCardInstance[][];
  current_packs?: (DraftCardInstance[] | null)[];
  /**
   * The live shared-stack turn, with no pile CONTENTS for a spectator. The
   * counts and `active_pile` are published to spectators in both
   * visibilities, exactly as to players; only `revealed` is withheld.
   *
   * OPTIONAL because the Rust field is
   * `#[serde(default, skip_serializing_if = "Option::is_none")]`: it is
   * genuinely absent from every non-Winston frame and from every frame outside
   * `Drafting`, so `shared_stack != null` means exactly "a pile turn is live".
   */
  shared_stack?: SharedStackView | null;
}

// @sync-with: crates/engine/src/game/deck_validation.rs
/**
 * CR 903.13e: the commander filler this draft's booster set lets a player add
 * to their card pool, and the cap on the ADDED copies. Engine-derived; the
 * client never learns which sets grant what.
 */
export interface GrantableCommanderFiller {
  card_name: string;
  max_copies: number;
}

// @sync-with: crates/draft-core/src/view.rs
export interface DraftPlayerView {
  status: DraftStatus;
  kind: DraftKind;
  /** Candidate intent plus the viewer-scoped Chaos metadata from the engine. */
  source?: DraftSourceView;
  /** Engine-owned completed-pod launch capability; never infer this from kind. */
  launch_capability: DraftLaunchCapability;
  /**
   * How this procedure delivers boosters to seats. Published for the same
   * reason `launch_capability` is: a procedure fact a display layer needs and
   * must never infer from the kind label.
   *
   * NOT status-gated, unlike `shared_stack` — which is why a surface that
   * outlives the drafting phase (a pod-status dialog, the standings) asks THIS
   * rather than `shared_stack !== null`. Pair it with
   * `isSharedStackDistribution`.
   */
  distribution: PackDistribution;
  /**
   * CR 903.3 / CR 903.13f: exact number of commanders this procedure requires.
   * This remains a count because valid Commander construction can designate
   * multiple cards; never infer designation capability from `kind`.
   */
  commanders_required: number;
  current_pack_number: number;
  pick_number: number;
  pass_direction: "Left" | "Right";
  current_pack: DraftCardInstance[] | null;
  /**
   * CR 903.13b: how many cards this seat's next pick step takes —
   * `min(cards_per_pick, remaining pack size)`, computed by the engine's
   * `pick_pass::required_pick_count` and enforced by `apply_pick_inner`.
   * 0 when there is no pending pack. Read it; never re-derive it from `kind`.
   */
  required_pick_count: number;
  /** Engine-owned selection interaction, independent of the current count. */
  pick_selection_mode: "Direct" | "Ordered";
  pool: DraftCardInstance[];
  draft_effects: DraftCardInstance[];
  /** Engine-owned grouping, ordering, and duplicate counts for the pool. */
  pool_groups: DraftPoolGroups;
  /** Engine-provided sealed packs in opening order. Absent for draft events. */
  sealed_packs?: DraftCardInstance[][] | null;
  seats: SeatPublicView[];
  /** Cards in the booster being drafted right now, not a session-wide size. */
  cards_per_pack: number;
  /** Cards in each booster, in pack order. Multi-set drafts mix sizes. */
  pack_sizes?: number[];
  /** The set filling each booster, in pack order. */
  pack_set_codes?: string[];
  /**
   * CR 903.13b: pick STEPS in each booster, in pack order — the per-pack
   * counterpart of `pick_steps_per_pack`. A progress display measures each
   * booster against this, never against `pack_sizes`: the two differ whenever
   * a kind takes more than one card per step.
   */
  pack_pick_steps?: number[];
  /**
   * CR 903.13b: how many pick STEPS this session's pack contains —
   * `cards_per_pack.div_ceil(cards_per_pick)`, computed by the engine's
   * `DraftProcedure::pick_steps_per_pack`. `pick_number` counts steps, not
   * cards, so this is the denominator a progress bar can actually reach: a
   * 14-card Commander pack is 7 steps, not 14. Read it; never re-derive it
   * from `cards_per_pack`.
   */
  pick_steps_per_pack: number;
  pack_count: number;
  min_deck_size: number;
  addable_cards: string[];
  /**
   * CR 903.13e: every granted commander filler, or absent/empty when no set the
   * draft contained grants one. Plural because CR 903.13e states its grants per
   * contained set — a draft that opened Commander Masters and Battle for
   * Baldur's Gate boosters concedes both cards. Deliberately NOT folded into
   * `addable_cards`, whose contract is *unlimited quantity* — the exact
   * property CR 903.13e denies. The caps and the commander-only condition are
   * enforced by the engine at submission, never here.
   */
  grantable_commander_fillers?: GrantableCommanderFiller[] | null;
  /**
   * CR 903.13f(3): OPAQUE courier tokens for `commanderPartnerCandidates`.
   * Pass them through; never interpret them, and never reconstruct them from a
   * pool card's `set_code`. Plural for the same reason as
   * `grantable_commander_fillers`: the rule asks what the draft CONTAINED, and
   * a mixed-set draft contained all of them.
   */
  draft_set_codes?: string[] | null;
  timer_remaining_ms: number | null;
  standings: StandingEntry[];
  current_round: number;
  /**
   * Engine-derived round that pairings may next be generated for. Always >= 1.
   * Published unconditionally, so on a `Complete` pod it names a round that can
   * never be generated — read `current_round` there instead.
   */
  next_pairing_round: number;
  tournament_format: TournamentFormat;
  pod_policy: PodPolicy;
  pairings: PairingView[];
  match_config: MatchConfig;
  /**
   * The live shared-stack turn, projected for this viewer.
   *
   * OPTIONAL because the Rust field is
   * `#[serde(default, skip_serializing_if = "Option::is_none")]`: it is
   * genuinely absent from every non-Winston frame and from every frame outside
   * `Drafting`, so `shared_stack != null` is the engine-published
   * discriminator for "render the pile table", and no kind check is needed.
   */
  shared_stack?: SharedStackView | null;
  /**
   * The seat that chooses who plays first in the games after the draft, from
   * the engine's latched starting seat. `null`/absent for pods larger than two
   * seats and for every kind with no shared stack. Deliberately NOT status
   * gated — the choice is exercised after the draft.
   *
   * ADVISORY: the engine does not enforce it, so this is rendered as an
   * instruction to the players and never as a control.
   */
  play_first_chooser?: number | null;
}

export type MultiplayerSeatDescriptor =
  | { type: "Human"; player_id: number; display_name: string }
  | { type: "Bot"; name: string };

/**
 * Pool source for multiplayer draft creation. Mirrors the Rust `PoolInput`
 * enum in draft-wasm. Snake_case fields match the existing `CubeDraftSettings`
 * TS↔Rust mirror convention (no `rename_all` machinery on the Rust side).
 *
 * A Set pod carries the same `SetPackSequence` a local draft does, so both
 * boundaries describe a pack sequence identically and a pod can mix sets.
 * Hosts that predate multi-set pods persisted `{ set_pool_json }` instead;
 * draft-wasm still accepts that spelling, so an in-flight pod survives the
 * upgrade — nothing new should ever write it.
 */
export type PoolInput =
  | { type: "Set"; data: SetPackSequence | { set_pool_json: string } }
  | {
      /**
       * Host-local Chaos Draft input. The host provides candidate pools only;
       * draft-wasm derives the persisted seat-by-round assignments from its
       * private seed, so this shape can never carry assignments to a guest.
       */
      type: "Chaos";
      data: { pools: unknown[]; candidate_codes: string[] };
    }
  | {
      type: "Cube";
      data: {
        cube_list_text: string;
        cube_name: string;
        cube_draft_settings: CubeDraftSettings;
      };
    };

/**
 * The sets backing a local draft and the order their boosters open in. Mirrors
 * the Rust `SetPackSequence` in draft-wasm.
 *
 * `pools` carries each distinct set's `draft-pools.json` entry once; `sequence`
 * names which set fills each booster, in pack order, so a set may be drafted
 * more than once without shipping its pool data twice. The sequence length is
 * the draft's pack count.
 */
export interface SetPackSequence {
  pools: unknown[];
  sequence: string[];
}

/**
 * Join the distinct entries of a pack sequence for display, in first-appearance
 * order. Mirrors the engine's own source label (`DraftSource::set_code`), which
 * dedupes the same way, so a mixed draft reads as "ISD+DKA+AVR" on both sides
 * of the boundary. Codes join with `+`; names read better with `" · "`.
 */
export function distinctJoined(values: string[], separator: string): string {
  return [...new Set(values)].join(separator);
}

/**
 * Pair an ordered pack list with the `draft-pools.json` entry for each distinct
 * set it names — the payload every set-backed entry point takes, local and pod
 * alike.
 *
 * One pool per DISTINCT set: a set drafted in several packs still crosses the
 * boundary once, and `sequence` is what repeats. Throws on the first set with
 * no pool data rather than shipping a sequence draft-wasm will refuse by name.
 */
export function setPackSequence(
  packs: readonly { code: string }[],
  allPools: Record<string, unknown>,
): SetPackSequence {
  const sequence = packs.map((pack) => pack.code);
  const pools = [...new Set(sequence)].map((code) => {
    const pool = allPools[code.toLowerCase()] ?? allPools[code.toUpperCase()];
    if (!pool) throw new Error(`No pool data for set: ${code}`);
    return pool;
  });
  return { pools, sequence };
}

export interface SuggestedDeck {
  main_deck: string[];
  lands: Record<string, number>;
  /**
   * CR 903.3 + CR 903.5a: the designated commander(s). Every name here is also
   * a member of `main_deck` — a designation is a label on a deck card, never an
   * extra card beside the deck. Empty for the four CR 905.1a kinds.
   */
  commander: string[];
}

export type DeckAddableCardPolicy =
  | "StandardBasics"
  | "CustomOnly"
  | "StandardBasicsPlusCustom";

export interface CubeDraftSettings {
  pod_size: number;
  pack_count: number;
  cards_per_pack: number;
  min_deck_size: number;
  addable_cards: {
    policy: DeckAddableCardPolicy;
    custom: string[];
  };
}

// ── Lazy WASM singleton ─────────────────────────────────────────────────

let wasmModule: typeof DraftWasm | null = null;

async function ensureDraftWasm(): Promise<typeof DraftWasm> {
  if (!wasmModule) {
    const mod = await import("@wasm/draft");
    await mod.default();
    wasmModule = mod;
  }
  return wasmModule;
}

export class DraftEngineOperationLease {
  constructor(private readonly wasm: typeof DraftWasm) {}

  initialize(setPoolJson: string, difficulty: number, seed: number): DraftPlayerView {
    return this.wasm.start_quick_draft(setPoolJson, difficulty, seed) as DraftPlayerView;
  }

  filterPoolListing(listing: DraftCardInstance[], filter: PoolFilter): string[] {
    return this.wasm.filter_pool_listing(
      JSON.stringify(listing),
      JSON.stringify(filter),
    ) as string[];
  }

  poolFilterOptions(pool: DraftCardInstance[]): PoolFilterOptions {
    return this.wasm.pool_filter_options(JSON.stringify(pool)) as PoolFilterOptions;
  }

  initializeSealed(setPoolJson: string, difficulty: number, seed: number): DraftPlayerView {
    return this.wasm.start_sealed_draft(setPoolJson, difficulty, seed) as DraftPlayerView;
  }

  initializeCube(
    cubeListText: string,
    cubeName: string,
    settings: CubeDraftSettings,
    difficulty: number,
    seed: number,
  ): DraftPlayerView {
    return this.wasm.start_quick_cube_draft(
      cubeListText,
      cubeName,
      JSON.stringify(settings),
      difficulty,
      seed,
    ) as DraftPlayerView;
  }

  submitPick(cardInstanceId: string): DraftPlayerView {
    return this.wasm.submit_pick(cardInstanceId) as DraftPlayerView;
  }

  /**
   * Engine-authored LLM pick requests for this pod's eligible bot seats.
   *
   * Takes no seat list: which seats an LLM may draft for is decided by the
   * draft engine from its own roster, so the display layer never names one.
   * Read-only — no pick is applied and no session state changes.
   */
  buildLlmDraftPickRequests(
    endpointJson: string,
    setNames: Record<string, string>,
  ): LlmDraftPickRequest[] {
    return this.wasm.buildLlmDraftPickRequests(
      endpointJson,
      JSON.stringify(setNames),
    ) as LlmDraftPickRequest[];
  }

  /**
   * Apply the human's pick, resolving each LLM seat's pick from its response.
   *
   * Per-seat fallback is the engine's: a response it cannot decode, or one
   * whose pack has moved on, leaves that seat to the heuristic bot in the same
   * pass. The pick always completes.
   */
  submitPickWithLlmBotPicks(
    cardInstanceId: string,
    responses: LlmDraftResponsePayload[],
  ): { view: DraftPlayerView; llmOutcomes: LlmDraftOutcome[] } {
    return this.wasm.submitPickWithLlmBotPicks(
      cardInstanceId,
      JSON.stringify(responses),
    ) as { view: DraftPlayerView; llmOutcomes: LlmDraftOutcome[] };
  }

  submitPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): DraftPlayerView {
    return this.wasm.submit_pick_with_draft_effect(
      effectCardInstanceId,
      JSON.stringify(cardInstanceIds),
    ) as DraftPlayerView;
  }

  autoPick(): DraftPlayerView {
    return this.wasm.auto_pick() as DraftPlayerView;
  }

  getView(): DraftPlayerView {
    return this.wasm.get_view() as DraftPlayerView;
  }

  submitDeck(mainDeck: string[], commanders: string[]): DraftPlayerView {
    return this.wasm.submit_deck(
      JSON.stringify(mainDeck),
      JSON.stringify(commanders),
    ) as DraftPlayerView;
  }

  suggestDeck(): SuggestedDeck {
    return this.wasm.suggest_deck() as SuggestedDeck;
  }

  suggestLands(spells: string[]): Record<string, number> {
    return this.wasm.suggest_lands(JSON.stringify(spells)) as Record<string, number>;
  }

  suggestLandsForSeat(seat: number, spells: string[]): Record<string, number> {
    return this.wasm.suggest_lands_for_seat(
      seat,
      JSON.stringify(spells),
    ) as Record<string, number>;
  }

  getBotDeck(botSeat: number): SuggestedDeck {
    return this.wasm.get_bot_deck(botSeat) as SuggestedDeck;
  }

  loadCardDatabase(json: string): number {
    return this.wasm.load_card_database(json);
  }

  /**
   * `difficulty` is LAST, mirroring the wasm export, and must stay last: the
   * engine reads this boundary positionally and the host's tests read it back
   * by index. It is the strength this pod's bot seats play at
   * (`map_difficulty`, 0..=4), and passing it is what stops a pod inheriting
   * the difficulty of whatever draft this tab ran before it — `DIFFICULTY` is
   * a per-thread cell in the engine with no reset.
   */
  createMultiplayerDraft(
    poolInput: PoolInput,
    seats: MultiplayerSeatDescriptor[],
    kind: Exclude<DraftKind, "Quick">,
    seed: number,
    draftCode: string,
    tournamentFormat: TournamentFormat,
    podPolicy: PodPolicy,
    difficulty: number,
  ): DraftPlayerView {
    return this.wasm.create_multiplayer_draft(
      JSON.stringify(poolInput),
      JSON.stringify(seats),
      DRAFT_KIND_WIRE_NUMBER[kind],
      seed,
      draftCode,
      tournamentFormat,
      podPolicy,
      difficulty,
    ) as DraftPlayerView;
  }

  submitPickForSeat(seat: number, cardInstanceIds: string[]): DraftPlayerView {
    return this.wasm.submit_pick_for_seat(
      seat,
      JSON.stringify(cardInstanceIds),
    ) as DraftPlayerView;
  }

  submitPickWithDraftEffectForSeat(
    seat: number,
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): DraftPlayerView {
    return this.wasm.submit_pick_with_draft_effect_for_seat(
      seat,
      effectCardInstanceId,
      JSON.stringify(cardInstanceIds),
    ) as DraftPlayerView;
  }

  /**
   * One whole shared-stack turn decision for `seat`.
   *
   * Routed through `apply_draft_action` rather than a dedicated wasm export,
   * because `DraftAction::SharedStackDecision` is what the reducer accepts and
   * there is no pick-shaped export for it. `pile` is an optimistic-concurrency
   * check, not a selector: the cursor is the ENGINE's, and a second frame that
   * names a stale pile is refused `PileNotActive` instead of silently applying
   * to the next pile. Nothing here decides legality.
   *
   * Returns the filtered view for THAT seat, the same contract
   * `submit_pick_for_seat` has — not the host view. `revealed` prefixes are
   * viewer-scoped, so acknowledging a guest with seat 0's projection would hand
   * it somebody else's turn.
   */
  submitSharedStackDecisionForSeat(
    seat: number,
    pile: number,
    decision: SharedStackPileDecision,
  ): DraftPlayerView {
    this.wasm.apply_draft_action(
      JSON.stringify({ type: "SharedStackDecision", data: { seat, pile, decision } }),
    );
    return this.wasm.get_view_for_seat(seat) as DraftPlayerView;
  }

  /**
   * Resolve every consecutive shared-stack turn a BOT seat owns, and report
   * how many decisions the engine made.
   *
   * The loop, its bound and its termination proof are the engine's
   * (`resolve_shared_stack_bot_turns_inner`); this boundary only forwards the
   * call. It is deliberately NOT a decision-shaped method: the host cannot
   * name a pile or a decision here, so there is no second authority over what
   * a bot seat does.
   *
   * Returns the engine's `DraftDelta` list VERBATIM and untyped. There is no
   * TypeScript spelling of `DraftDelta` in this client and this method does not
   * introduce one: nothing here reads a delta's contents, and the host uses
   * only the LENGTH — empty means the engine moved nothing (the active seat is
   * human, the draft is over, or the session has no shared stack), so no
   * persistence fence is owed.
   */
  resolveSharedStackBotTurns(): unknown[] {
    return this.wasm.resolve_shared_stack_bot_turns() as unknown[];
  }

  /**
   * The decision the ENGINE would apply for a seat whose turn must be resolved
   * without that seat choosing. `null` when there is no shared stack or the
   * seat has no legal move.
   *
   * The host asks this instead of scanning the published `legality` vector for
   * the first entry with no refusal. Same algorithm, but the engine folds
   * `SharedStackPileDecision::ALL` in declaration order while the client folded
   * whatever order the view happened to serialize -- agreement by coincidence
   * rather than by construction. Choosing a rules outcome is the reducer's job.
   */
  sharedStackForcedDecision(seat: number): SharedStackPileDecision | null {
    return this.wasm.shared_stack_forced_decision(seat) as SharedStackPileDecision | null;
  }

  submitDeckForSeat(
    seat: number,
    mainDeck: string[],
    commanders: string[],
  ): DraftPlayerView {
    return this.wasm.submit_deck_for_seat(
      seat,
      JSON.stringify(mainDeck),
      JSON.stringify(commanders),
    ) as DraftPlayerView;
  }

  getViewForSeat(seat: number): DraftPlayerView {
    return this.wasm.get_view_for_seat(seat) as DraftPlayerView;
  }

  setSeatConnected(seat: number, connected: boolean): DraftPlayerView {
    return this.wasm.set_seat_connected(seat, connected) as DraftPlayerView;
  }

  exportSession(): string {
    return this.wasm.export_draft_session();
  }

  /**
   * Host-only original cube multiset for the next game launch. This is never
   * projected onto a participant or spectator draft view.
   */
  boosterPackPoolForGame(): string[] | null {
    return this.wasm.booster_pack_pool_for_game() as string[] | null;
  }

  importSession(json: string, difficulty: number): DraftPlayerView {
    return this.wasm.import_draft_session(json, difficulty) as DraftPlayerView;
  }

  allPicksSubmitted(): boolean {
    return this.wasm.all_picks_submitted();
  }

  draftProcedure(
    kind: Exclude<DraftKind, "Quick">,
    tournamentFormat: TournamentFormat,
  ): DraftProcedure {
    return this.wasm.draft_procedure(
      DRAFT_KIND_WIRE_NUMBER[kind],
      tournamentFormat,
    ) as DraftProcedure;
  }

  applyActionAndGetHostView(actionJson: string): DraftPlayerView {
    this.wasm.apply_draft_action(actionJson);
    return this.wasm.get_view_for_seat(0) as DraftPlayerView;
  }
}

let draftEngineOperationTail: Promise<void> = Promise.resolve();

export function withDraftEngineOperation<T>(
  work: (lease: DraftEngineOperationLease) => Promise<T> | T,
): Promise<T> {
  const operation = draftEngineOperationTail.then(async () => {
    const wasm = await ensureDraftWasm();
    return work(new DraftEngineOperationLease(wasm));
  });
  draftEngineOperationTail = operation.then(
    () => undefined,
    () => undefined,
  );
  return operation;
}

export async function drainDraftEngineOperations(): Promise<void> {
  await draftEngineOperationTail;
}

// ── DraftAdapter ────────────────────────────────────────────────────────

/**
 * Wraps draft-wasm exports with lazy loading and typed return values.
 *
 * Follows the WasmAdapter singleton pattern: WASM is loaded on first use,
 * then all subsequent calls are synchronous behind the async interface.
 * Per D-08: separate from engine-wasm, lazy-loaded only when entering draft.
 */
export class DraftAdapter {
  async initialize(
    selection: SetPackSequence,
    difficulty: number,
    seed: number,
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) =>
      lease.initialize(JSON.stringify(selection), difficulty, seed),
    );
  }

  /**
   * Narrow a limited-pool listing through the ENGINE's filtering authority
   * (#7546 review). Each instance is classified inside draft-core — the
   * wire-delivered groups are not an input, so a legacy (pre-v11) view
   * filters every collapsed copy correctly. Stateless — works for P2P
   * guests; no draft session is required.
   */
  async filterPoolListing(
    listing: DraftCardInstance[],
    filter: PoolFilter,
  ): Promise<string[]> {
    return withDraftEngineOperation((lease) => lease.filterPoolListing(listing, filter));
  }

  /**
   * The engine-owned filter option lists, computed from the pool instances
   * alone — for views whose delivered groups predate the option fields
   * (review round 5). Never reconstructed in the display layer.
   */
  async poolFilterOptions(pool: DraftCardInstance[]): Promise<PoolFilterOptions> {
    return withDraftEngineOperation((lease) => lease.poolFilterOptions(pool));
  }

  async initializeSealed(
    selection: SetPackSequence,
    difficulty: number,
    seed: number,
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) =>
      lease.initializeSealed(JSON.stringify(selection), difficulty, seed),
    );
  }

  async initializeCube(
    cubeListText: string,
    cubeName: string,
    settings: CubeDraftSettings,
    difficulty: number,
    seed: number,
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) =>
      lease.initializeCube(cubeListText, cubeName, settings, difficulty, seed),
    );
  }

  async submitPick(cardInstanceId: string): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.submitPick(cardInstanceId));
  }

  async submitPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) =>
      lease.submitPickWithDraftEffect(effectCardInstanceId, cardInstanceIds),
    );
  }

  /** Let the bot AI pick the best card from the current pack for the player. */
  async autoPick(): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.autoPick());
  }

  async getView(): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.getView());
  }

  async submitDeck(mainDeck: string[], commanders: string[]): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.submitDeck(mainDeck, commanders));
  }

  async suggestDeck(): Promise<SuggestedDeck> {
    return withDraftEngineOperation((lease) => lease.suggestDeck());
  }

  async suggestLands(spells: string[]): Promise<Record<string, number>> {
    return withDraftEngineOperation((lease) => lease.suggestLands(spells));
  }

  async suggestLandsForSeat(seat: number, spells: string[]): Promise<Record<string, number>> {
    return withDraftEngineOperation((lease) => lease.suggestLandsForSeat(seat, spells));
  }

  async getBotDeck(botSeat: number): Promise<SuggestedDeck> {
    return withDraftEngineOperation((lease) => lease.getBotDeck(botSeat));
  }

  async boosterPackPoolForGame(): Promise<string[] | null> {
    return withDraftEngineOperation((lease) => lease.boosterPackPoolForGame());
  }

  async loadCardDatabase(json: string): Promise<number> {
    return withDraftEngineOperation((lease) => lease.loadCardDatabase(json));
  }

  // ── Multi-seat API (P2P Tournament Host) ─────────────────────────────

  /** See the lease method for why `difficulty` is last and what it fixes. */
  async createMultiplayerDraft(
    poolInput: PoolInput,
    seats: MultiplayerSeatDescriptor[],
    kind: Exclude<DraftKind, "Quick">,
    seed: number,
    draftCode: string,
    tournamentFormat: TournamentFormat,
    podPolicy: PodPolicy,
    difficulty: number,
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) =>
      lease.createMultiplayerDraft(
        poolInput,
        seats,
        kind,
        seed,
        draftCode,
        tournamentFormat,
        podPolicy,
        difficulty,
      ),
    );
  }

  /**
   * Submit one whole CR 903.13b pick step for a seat. The engine owns the
   * session-specific cardinality; this boundary serializes the full step.
   */
  async submitPickForSeat(seat: number, cardInstanceIds: string[]): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.submitPickForSeat(seat, cardInstanceIds));
  }

  /**
   * One whole shared-stack turn decision for `seat`. See the lease method for
   * why `pile` travels with the decision and why legality is not asked here.
   */
  async submitSharedStackDecisionForSeat(
    seat: number,
    pile: number,
    decision: SharedStackPileDecision,
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) =>
      lease.submitSharedStackDecisionForSeat(seat, pile, decision));
  }

  /**
   * Drive every consecutive bot-owned shared-stack turn. See the lease method
   * for why the engine owns the loop and why the deltas are returned untyped.
   */
  async resolveSharedStackBotTurns(): Promise<unknown[]> {
    return withDraftEngineOperation((lease) => lease.resolveSharedStackBotTurns());
  }

  async sharedStackForcedDecision(seat: number): Promise<SharedStackPileDecision | null> {
    return withDraftEngineOperation((lease) => lease.sharedStackForcedDecision(seat));
  }

  /** The engine-owned per-kind procedure axes; never re-derived by the UI. */
  async draftProcedure(
    kind: Exclude<DraftKind, "Quick">,
    tournamentFormat: TournamentFormat,
  ): Promise<DraftProcedure> {
    return withDraftEngineOperation((lease) => lease.draftProcedure(kind, tournamentFormat));
  }

  async submitPickWithDraftEffectForSeat(
    seat: number,
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) =>
      lease.submitPickWithDraftEffectForSeat(seat, effectCardInstanceId, cardInstanceIds),
    );
  }

  async submitDeckForSeat(
    seat: number,
    mainDeck: string[],
    commanders: string[],
  ): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.submitDeckForSeat(seat, mainDeck, commanders));
  }

  async getViewForSeat(seat: number): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.getViewForSeat(seat));
  }

  /**
   * Mark a human seat as connected or disconnected. Drives the
   * `seats[*].connected` field on subsequent views.
   */
  async setSeatConnected(seat: number, connected: boolean): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.setSeatConnected(seat, connected));
  }

  async exportSession(): Promise<string> {
    return withDraftEngineOperation((lease) => lease.exportSession());
  }

  async importSession(json: string, difficulty: number): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.importSession(json, difficulty));
  }

  async allPicksSubmitted(): Promise<boolean> {
    return withDraftEngineOperation((lease) => lease.allPicksSubmitted());
  }

  // ── Tournament actions (route through apply_draft_action → get host view) ──

  private async applyActionAndGetHostView(actionJson: string): Promise<DraftPlayerView> {
    return withDraftEngineOperation((lease) => lease.applyActionAndGetHostView(actionJson));
  }

  async generatePairings(): Promise<DraftPlayerView> {
    return this.applyActionAndGetHostView(
      JSON.stringify({ type: "GeneratePairings" }),
    );
  }

  async reportMatchResult(matchId: string, winnerSeat: number | null): Promise<DraftPlayerView> {
    return this.applyActionAndGetHostView(
      JSON.stringify({ type: "ReportMatchResult", data: { match_id: matchId, winner_seat: winnerSeat } }),
    );
  }

  async advanceRound(): Promise<DraftPlayerView> {
    return this.applyActionAndGetHostView(
      JSON.stringify({ type: "AdvanceRound" }),
    );
  }

  async replaceSeatWithBot(seat: number, name?: string): Promise<DraftPlayerView> {
    return this.applyActionAndGetHostView(
      JSON.stringify({ type: "ReplaceSeatWithBot", data: { seat, name } }),
    );
  }
}
