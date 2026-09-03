import type * as DraftWasm from "@wasm/draft";
import type { MatchConfig } from "./types";

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
  pick_status: "Pending" | "Picked" | "TimedOut" | "NotDrafting";
  /**
   * Engine-owned active-pack presence: exactly 0 or 1, never a card count.
   * Required by P2P draft v24 and the full WebSocket protocol v49.
   */
  active_pack_count: number;
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

export type DraftKind = "Quick" | "Premier" | "Traditional" | "Sealed" | "CommanderDraft";

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
export type PackDistribution = "PickAndPass" | "AllAtOnce";
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
};

/**
 * The engine-owned per-kind procedure axes, mirroring `DraftProcedureDto` in
 * `crates/draft-wasm/src/lib.rs`. Read these; never re-derive them.
 */
// @sync-with: crates/draft-wasm/src/lib.rs
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
  min_deck_size: number;
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

  getBotDeck(botSeat: number): SuggestedDeck {
    return this.wasm.get_bot_deck(botSeat) as SuggestedDeck;
  }

  loadCardDatabase(json: string): number {
    return this.wasm.load_card_database(json);
  }

  createMultiplayerDraft(
    poolInput: PoolInput,
    seats: MultiplayerSeatDescriptor[],
    kind: Exclude<DraftKind, "Quick">,
    seed: number,
    draftCode: string,
    tournamentFormat: TournamentFormat,
    podPolicy: PodPolicy,
  ): DraftPlayerView {
    return this.wasm.create_multiplayer_draft(
      JSON.stringify(poolInput),
      JSON.stringify(seats),
      DRAFT_KIND_WIRE_NUMBER[kind],
      seed,
      draftCode,
      tournamentFormat,
      podPolicy,
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

  async getBotDeck(botSeat: number): Promise<SuggestedDeck> {
    return withDraftEngineOperation((lease) => lease.getBotDeck(botSeat));
  }

  async loadCardDatabase(json: string): Promise<number> {
    return withDraftEngineOperation((lease) => lease.loadCardDatabase(json));
  }

  // ── Multi-seat API (P2P Tournament Host) ─────────────────────────────

  async createMultiplayerDraft(
    poolInput: PoolInput,
    seats: MultiplayerSeatDescriptor[],
    kind: Exclude<DraftKind, "Quick">,
    seed: number,
    draftCode: string,
    tournamentFormat: TournamentFormat,
    podPolicy: PodPolicy,
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
