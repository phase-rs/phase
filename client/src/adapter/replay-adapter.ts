import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";
import type {
  EngineAdapter,
  EngineSnapshot,
  GameAction,
  GameState,
  LegalActionsResult,
  PlayerId,
  ReplayHeader,
  SubmitResult,
} from "./types";
import { EngineWorkerClient } from "./engine-worker-client";
import { unwrapClientGameState } from "./wasm-adapter";

/**
 * Read-only adapter for the Replay Viewer. Backed by its own dedicated
 * `EngineWorkerClient` — never the live game's worker — so loading or
 * scrubbing a replay can never touch, or be touched by, an in-progress game.
 *
 * Only `loadReplay` / `seek` / `length` / `header` / `dispose` do real work.
 * The rest of the `EngineAdapter` surface exists so a `ReplayAdapter` can sit
 * in `useGameStore.adapter` the same way a live-game adapter does; the
 * Replay Viewer always runs with `gameMode: "spectate"`, which already
 * short-circuits `dispatchAction` before any of these would be reached (see
 * `game/dispatch.ts`) — they no-op/throw here as a defense-in-depth
 * backstop, not as the primary safety mechanism.
 */
export class ReplayAdapter implements EngineAdapter {
  private client: EngineWorkerClient;

  constructor() {
    this.client = new EngineWorkerClient();
  }

  async initialize(): Promise<void> {
    await this.client.initialize();
    try {
      await this.client.loadCardDbFromUrl();
    } catch {
      // Not fatal here: a replay whose header carries no deck data (e.g. a
      // debug/sandbox game that started with empty libraries) reconstructs
      // fine without a card database. Replays that *do* carry deck data
      // require one — `reconstruct_initial_state` now fails loudly with
      // `MissingCardDatabase` rather than silently skipping deck hydration,
      // so `loadReplay` below will throw for those if this fetch failed.
    }
  }

  /** Load a replay (the JSON `exportReplayLog`/`replayExport.ts` produced). Returns the recorded action count. */
  async loadReplay(replayJson: string): Promise<number> {
    return this.client.loadReplayForPlayback(replayJson);
  }

  async length(): Promise<number> {
    return this.client.replayLength();
  }

  async header(): Promise<ReplayHeader | null> {
    return this.client.replayHeader();
  }

  /** Seek to `target` (clamped to the replay's length) and return the reconstructed state, or `null` if none is loaded. */
  async seek(target: number): Promise<GameState | null> {
    const wrapped = await this.client.replaySeek(target);
    if (wrapped == null) return null;
    return unwrapClientGameState(wrapped);
  }

  initializeGame(): Promise<SubmitResult> {
    return Promise.reject(
      new Error("ReplayAdapter does not support initializeGame — call loadReplay instead"),
    );
  }

  async submitAction(_action: GameAction, _actor: PlayerId): Promise<SubmitResult> {
    return { events: [], log_entries: [] };
  }

  getState(): Promise<GameState> {
    return Promise.reject(
      new Error("ReplayAdapter has no single current state — call seek(index) instead"),
    );
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    return {
      actions: [],
      autoPassRecommended: false,
      manaPaymentShortcutActions: [],
      spellCosts: {},
      legalActionsByObject: {},
    };
  }

  /** Same "no single current state" contract as `getState` — the Replay Viewer
   *  drives the store via `seek()`, never through the snapshot commit path. */
  getSnapshot(): Promise<EngineSnapshot> {
    return Promise.reject(
      new Error("ReplayAdapter has no single current state — call seek(index) instead"),
    );
  }

  getAiAction(): Promise<GameAction | null> {
    return Promise.resolve(null);
  }

  restoreState(): void {
    // No-op — replay state is driven exclusively by seek(), never restored.
  }

  dispose(): void {
    this.client.dispose();
  }

  async estimateBracket(_deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    return null;
  }
}
