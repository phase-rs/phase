import type { EngineAdapter, GameAction, GameEvent, GameState } from "./types";
import { AdapterError, AdapterErrorCode } from "./types";

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
    // Dynamic import avoids bundling @tauri-apps/api in web builds.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const tauriCore = await (Function('return import("@tauri-apps/api/core")')() as Promise<any>);
    this.invoke = tauriCore.invoke as InvokeFn;
  }

  async initializeGame(deckData?: unknown): Promise<GameEvent[]> {
    this.assertInitialized();
    const result = await this.invoke!("initialize_game", { deckData: deckData ?? null });
    return (result as GameEvent[]) ?? [];
  }

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    this.assertInitialized();
    try {
      const result = await this.invoke!("submit_action", { action });
      return result as GameEvent[];
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
      const state = await this.invoke!("get_game_state");
      return state as GameState;
    } catch (error) {
      throw new AdapterError(
        AdapterErrorCode.WASM_ERROR,
        error instanceof Error ? error.message : String(error),
        false,
      );
    }
  }

  async getLegalActions(): Promise<GameAction[]> {
    this.assertInitialized();
    try {
      const actions = await this.invoke!("get_legal_actions");
      return actions as GameAction[];
    } catch {
      return [];
    }
  }

  restoreState(_state: GameState): void {
    throw new AdapterError(
      AdapterErrorCode.WASM_ERROR,
      "restoreState not supported in TauriAdapter",
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
