import type * as DraftWasm from "@wasm/draft";

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
}

// @sync-with: crates/draft-core/src/view.rs
export interface SeatPublicView {
  seat_index: number;
  display_name: string;
  is_bot: boolean;
  connected: boolean;
  has_submitted_deck: boolean;
  pick_status: "Pending" | "Picked" | "TimedOut" | "NotDrafting";
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

export type TournamentFormat = "Swiss" | "SingleElimination";

export type PodPolicy = "Competitive" | "Casual";

export type PairingStatus = "Pending" | "InProgress" | "Complete";

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
export interface DraftPlayerView {
  status: DraftStatus;
  kind: "Quick" | "Premier" | "Traditional";
  current_pack_number: number;
  pick_number: number;
  pass_direction: "Left" | "Right";
  current_pack: DraftCardInstance[] | null;
  pool: DraftCardInstance[];
  seats: SeatPublicView[];
  cards_per_pack: number;
  pack_count: number;
  timer_remaining_ms: number | null;
  standings: StandingEntry[];
  current_round: number;
  tournament_format: TournamentFormat;
  pod_policy: PodPolicy;
  pairings: PairingView[];
}

export interface SuggestedDeck {
  main_deck: string[];
  lands: Record<string, number>;
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

  async submitPick(cardInstanceId: string): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_pick(cardInstanceId) as DraftPlayerView;
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

  async startMultiplayerDraft(
    setPoolJson: string,
    kind: "Premier" | "Traditional",
    seatNames: string[],
    seed: number,
  ): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.start_multiplayer_draft(
      setPoolJson,
      kind,
      JSON.stringify(seatNames),
      seed,
    ) as DraftPlayerView;
  }

  async submitPickForSeat(seat: number, cardInstanceId: string): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_pick_for_seat(seat, cardInstanceId) as DraftPlayerView;
  }

  async submitDeckForSeat(seat: number, mainDeck: string[]): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.submit_deck_for_seat(seat, JSON.stringify(mainDeck)) as DraftPlayerView;
  }

  async getViewForSeat(seat: number): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.get_view_for_seat(seat) as DraftPlayerView;
  }

  async exportSession(): Promise<string> {
    const wasm = await ensureDraftWasm();
    return wasm.export_draft_session();
  }

  async importSession(json: string): Promise<DraftPlayerView> {
    const wasm = await ensureDraftWasm();
    return wasm.import_draft_session(json) as DraftPlayerView;
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

  async replaceSeatWithBot(seat: number): Promise<DraftPlayerView> {
    return this.applyActionAndGetHostView(
      JSON.stringify({ type: "ReplaceSeatWithBot", data: { seat } }),
    );
  }
}
