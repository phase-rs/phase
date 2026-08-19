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
};

// @sync-with: crates/draft-core/src/view.rs
export interface SeatPublicView {
  seat_index: number;
  display_name: string;
  is_bot: boolean;
  connected: boolean;
  has_submitted_deck: boolean;
  pick_status: "Pending" | "Picked" | "TimedOut" | "NotDrafting";
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

export type DraftKind = "Quick" | "Premier" | "Traditional" | "Sealed";

export type TournamentFormat = "Swiss" | "SingleElimination";

export type PodPolicy = "Competitive" | "Casual";

export type PairingStatus = "Pending" | "InProgress" | "Complete";

/** Fields consumed by `DraftProgress` (shared by player and spectator views). */
export interface DraftProgressFields {
  current_pack_number: number;
  pick_number: number;
  cards_per_pack: number;
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
  current_pack_number: number;
  pick_number: number;
  pass_direction: "Left" | "Right";
  seats: SeatPublicView[];
  cards_per_pack: number;
  pack_count: number;
  min_deck_size: number;
  addable_cards: string[];
  standings: StandingEntry[];
  current_round: number;
  tournament_format: TournamentFormat;
  pod_policy: PodPolicy;
  pairings: PairingView[];
  match_config: MatchConfig;
  /** Present only when the host enabled omniscient spectator visibility. */
  pools?: DraftCardInstance[][];
  current_packs?: (DraftCardInstance[] | null)[];
}

// @sync-with: crates/draft-core/src/view.rs
export interface DraftPlayerView {
  status: DraftStatus;
  kind: DraftKind;
  current_pack_number: number;
  pick_number: number;
  pass_direction: "Left" | "Right";
  current_pack: DraftCardInstance[] | null;
  pool: DraftCardInstance[];
  draft_effects: DraftCardInstance[];
  /** Engine-owned grouping, ordering, and duplicate counts for the pool. */
  pool_groups: DraftPoolGroups;
  /** Engine-provided sealed packs in opening order. Absent for draft events. */
  sealed_packs?: DraftCardInstance[][] | null;
  seats: SeatPublicView[];
  cards_per_pack: number;
  pack_count: number;
  min_deck_size: number;
  addable_cards: string[];
  timer_remaining_ms: number | null;
  standings: StandingEntry[];
  current_round: number;
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
 */
export type PoolInput =
  | { type: "Set"; data: { set_pool_json: string } }
  | {
      type: "Cube";
      data: {
        cube_list_text: string;
        cube_name: string;
        cube_draft_settings: CubeDraftSettings;
      };
    };

export interface SuggestedDeck {
  main_deck: string[];
  lands: Record<string, number>;
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
    setPoolJson: string,
    difficulty: number,
    seed: number,
  ): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.start_quick_draft(setPoolJson, difficulty, seed) as DraftPlayerView;
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
    const wasm = await ensureDraftWasm();
    return wasm.filter_pool_listing(
      JSON.stringify(listing),
      JSON.stringify(filter),
    ) as string[];
  }

  /**
   * The engine-owned filter option lists, computed from the pool instances
   * alone — for views whose delivered groups predate the option fields
   * (review round 5). Never reconstructed in the display layer.
   */
  async poolFilterOptions(pool: DraftCardInstance[]): Promise<PoolFilterOptions> {
    const wasm = await ensureDraftWasm();
    return wasm.pool_filter_options(JSON.stringify(pool)) as PoolFilterOptions;
  }

  async initializeSealed(
    setPoolJson: string,
    difficulty: number,
    seed: number,
  ): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.start_sealed_draft(setPoolJson, difficulty, seed) as DraftPlayerView;
  }

  async initializeCube(
    cubeListText: string,
    cubeName: string,
    settings: CubeDraftSettings,
    difficulty: number,
    seed: number,
  ): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.start_quick_cube_draft(
      cubeListText,
      cubeName,
      JSON.stringify(settings),
      difficulty,
      seed,
    ) as DraftPlayerView;
  }

  async submitPick(cardInstanceId: string): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_pick(cardInstanceId) as DraftPlayerView;
  }

  async submitPickWithDraftEffect(
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_pick_with_draft_effect(
      effectCardInstanceId,
      JSON.stringify(cardInstanceIds),
    ) as DraftPlayerView;
  }

  /** Let the bot AI pick the best card from the current pack for the player. */
  async autoPick(): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.auto_pick() as DraftPlayerView;
  }

  async getView(): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.get_view() as DraftPlayerView;
  }

  async submitDeck(mainDeck: string[]): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_deck(JSON.stringify(mainDeck)) as DraftPlayerView;
  }

  async suggestDeck(): Promise<SuggestedDeck> {
    const wasm = await ensureDraftWasm();
    return wasm.suggest_deck() as SuggestedDeck;
  }

  async suggestLands(spells: string[]): Promise<Record<string, number>> {
    const wasm = await ensureDraftWasm();
    return wasm.suggest_lands(JSON.stringify(spells)) as Record<string, number>;
  }

  async getBotDeck(botSeat: number): Promise<SuggestedDeck> {
    const wasm = await ensureDraftWasm();
    return wasm.get_bot_deck(botSeat) as SuggestedDeck;
  }

  async loadCardDatabase(json: string): Promise<number> {
    const wasm = await ensureDraftWasm();
    return wasm.load_card_database(json);
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
    const wasm = await ensureDraftWasm();
    const kindId: Record<Exclude<DraftKind, "Quick">, number> = {
      Premier: 1,
      Traditional: 2,
      Sealed: 3,
    };
    return wasm.create_multiplayer_draft(
      JSON.stringify(poolInput),
      JSON.stringify(seats),
      kindId[kind],
      seed,
      draftCode,
      tournamentFormat,
      podPolicy,
    ) as DraftPlayerView;
  }

  async submitPickForSeat(seat: number, cardInstanceId: string): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_pick_for_seat(seat, cardInstanceId) as DraftPlayerView;
  }

  async submitPickWithDraftEffectForSeat(
    seat: number,
    effectCardInstanceId: string,
    cardInstanceIds: string[],
  ): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_pick_with_draft_effect_for_seat(
      seat,
      effectCardInstanceId,
      JSON.stringify(cardInstanceIds),
    ) as DraftPlayerView;
  }

  async submitDeckForSeat(seat: number, mainDeck: string[]): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_deck_for_seat(seat, JSON.stringify(mainDeck)) as DraftPlayerView;
  }

  async getViewForSeat(seat: number): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.get_view_for_seat(seat) as DraftPlayerView;
  }

  /**
   * Mark a human seat as connected or disconnected. Drives the
   * `seats[*].connected` field on subsequent views.
   */
  async setSeatConnected(seat: number, connected: boolean): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.set_seat_connected(seat, connected) as DraftPlayerView;
  }

  async exportSession(): Promise<string> {
    const wasm = await ensureDraftWasm();
    return wasm.export_draft_session();
  }

  async importSession(json: string, difficulty: number): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.import_draft_session(json, difficulty) as DraftPlayerView;
  }

  async allPicksSubmitted(): Promise<boolean> {
    const wasm = await ensureDraftWasm();
    return wasm.all_picks_submitted();
  }

  // ── Tournament actions (route through apply_draft_action → get host view) ──

  private async applyActionAndGetHostView(actionJson: string): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    wasm.apply_draft_action(actionJson);
    return wasm.get_view_for_seat(0) as DraftPlayerView;
  }

  async generatePairings(round: number): Promise<DraftPlayerView> {
    return this.applyActionAndGetHostView(
      JSON.stringify({ type: "GeneratePairings", data: { round } }),
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
