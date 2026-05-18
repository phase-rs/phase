import type {
  ActionResult,
  EngineAdapter,
  GameAction,
  GameState,
  LegalActionsResult,
  MatchConfig,
  PlayerId,
  SubmitResult,
} from "./types";
import { AdapterError, AdapterErrorCode } from "./types";
import type { BracketDeckRequest, BracketEstimate } from "../types/bracketEstimate";

/**
 * Tauri IPC-backed implementation of EngineAdapter.
 * Uses dynamic import of @tauri-apps/api/core to avoid bundling
 * Tauri API in web builds. Requires a Tauri v2 backend with
 * matching Rust commands (initialize_game, submit_action,
 * get_game_state, dispose_game).
 */
type InvokeFn = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

export class TauriAdapter implements EngineAdapter {
  private invoke: InvokeFn | null = null;

  async initialize(): Promise<void> {
    // Dynamic import so Vite code-splits @tauri-apps/api/core into a lazy
    // chunk that only loads inside Tauri. Avoids the `new Function(...)` hack
    // that would require 'unsafe-eval' in the Tauri CSP.
    const tauriCore = await import("@tauri-apps/api/core");
    this.invoke = tauriCore.invoke as InvokeFn;

    // Ship card-data.json to the Tauri backend once per adapter session so
    // the Rust CardDatabase is available to `initialize_game`. Without
    // this, `load_and_hydrate_decks` runs with `db=None` and dual-faced
    // cards (Adventure, Omen, MDFC, Transform, Meld, Prepare) silently
    // lose their face-specific behavior on desktop. Same card-data.json
    // the WASM path fetches — single source of truth.
    try {
      const resp = await fetch(__CARD_DATA_URL__);
      if (resp.ok) {
        const text = await resp.text();
        await this.invoke("load_card_database", { jsonStr: text });
      } else {
        console.warn(
          `TauriAdapter: card-data.json fetch failed (${resp.status}); dual-faced cards will be disabled.`,
        );
      }
    } catch (err) {
      console.warn(
        "TauriAdapter: card-data.json load failed; dual-faced cards will be disabled.",
        err,
      );
    }
  }

  async initializeGame(
    deckData?: unknown,
    _formatConfig?: unknown,
    _playerCount?: number,
    matchConfig?: MatchConfig,
    _firstPlayer?: number,
  ): Promise<SubmitResult> {
    this.assertInitialized();
    const seed = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
    const result = await this.invoke!("initialize_game", {
      deckData: deckData ?? null,
      seed,
      matchConfig: matchConfig ?? null,
    });
    const ar = result as ActionResult;
    return { events: ar.events ?? [], log_entries: ar.log_entries ?? [] };
  }

  async submitAction(action: GameAction, actor: PlayerId): Promise<SubmitResult> {
    this.assertInitialized();
    try {
      const result = await this.invoke!("submit_action", { actor, action });
      const ar = result as ActionResult;
      return { events: ar.events ?? [], log_entries: ar.log_entries ?? [] };
    } catch (error) {
      throw new AdapterError(
        AdapterErrorCode.INVALID_ACTION,
        error instanceof Error ? error.message : String(error),
        true,
      );
    }
  }

  async getState(): Promise<GameState> {
    this.assertInitialized();
    try {
      // Tauri `get_game_state` returns ClientGameState { state, derived };
      // flatten to the store-side shape (derived as optional on GameState).
      const wrapped = (await this.invoke!("get_game_state")) as
        | { state: GameState; derived?: GameState["derived"] }
        | GameState;
      if (wrapped != null && typeof wrapped === "object" && "state" in wrapped) {
        const w = wrapped as { state: GameState; derived?: GameState["derived"] };
        return { ...w.state, derived: w.derived ?? w.state.derived };
      }
      return wrapped as GameState;
    } catch (error) {
      throw new AdapterError(
        AdapterErrorCode.WASM_ERROR,
        error instanceof Error ? error.message : String(error),
        false,
      );
    }
  }

  async getLegalActions(): Promise<LegalActionsResult> {
    this.assertInitialized();
    try {
      const result = await this.invoke!("get_legal_actions");
      return result as LegalActionsResult;
    } catch {
      return { actions: [], autoPassRecommended: false };
    }
  }

  async getAiAction(
    difficulty: string,
    playerId: number,
  ): Promise<GameAction | null> {
    this.assertInitialized();
    const result = await this.invoke!("get_ai_action", {
      difficulty,
      playerId,
    });
    return (result as GameAction) ?? null;
  }

  restoreState(_state: GameState): void {
    throw new AdapterError(
      AdapterErrorCode.WASM_ERROR,
      "restoreState not supported in TauriAdapter",
      false,
    );
  }

  estimateBracket(_deck: BracketDeckRequest): Promise<BracketEstimate | null> {
    // Bracket estimation runs locally against the WASM card database.
    // The Tauri sidecar does not yet expose an `estimate_bracket_for_deck`
    // command. When Tauri parity is needed, add a Tauri command in
    // client/src-tauri/src/main.rs and invoke it here.
    throw new AdapterError(
      AdapterErrorCode.BRACKET_ESTIMATION_UNSUPPORTED,
      "Bracket estimation is not yet available in the Tauri desktop build.",
      false,
    );
  }

  dispose(): void {
    if (this.invoke) {
      this.invoke("dispose_game").catch(() => {
        // Ignore errors during disposal
      });
      this.invoke = null;
    }
  }

  private assertInitialized(): void {
    if (!this.invoke) {
      throw new AdapterError(
        AdapterErrorCode.NOT_INITIALIZED,
        "TauriAdapter not initialized. Call initialize() first.",
        true,
      );
    }
  }
}
