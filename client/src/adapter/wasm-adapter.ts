import init, {
  ping,
  create_initial_state,
  initialize_game,
  submit_action,
  get_game_state,
} from "../wasm/engine_wasm";
import type { EngineAdapter, GameAction, GameEvent, GameState } from "./types";
import { AdapterError, AdapterErrorCode } from "./types";

/**
 * WASM-backed implementation of EngineAdapter.
 * Communicates directly with the Rust engine compiled to WebAssembly.
 * Serializes all WASM access through an async queue (WASM is single-threaded).
 */
export class WasmAdapter implements EngineAdapter {
  private initialized = false;
  private queue: Promise<void> = Promise.resolve();

  async initialize(): Promise<void> {
    if (this.initialized) return;
    await init();
    this.initialized = true;
  }

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    this.assertInitialized();
    return this.enqueue(() => this.processAction(action));
  }

  async getState(): Promise<GameState> {
    this.assertInitialized();
    return this.enqueue(() => this.fetchState());
  }

  dispose(): void {
    this.initialized = false;
    this.queue = Promise.resolve();
  }

  /** Verify WASM module is responding. */
  ping(): string {
    this.assertInitialized();
    return ping();
  }

  /** Initialize a new game and return the initial events. */
  initializeGame(deckData?: unknown): GameEvent[] {
    this.assertInitialized();
    const result = initialize_game(deckData ?? null);
    return result.events ?? [];
  }

  private assertInitialized(): void {
    if (!this.initialized) {
      throw new AdapterError(
        AdapterErrorCode.NOT_INITIALIZED,
        "Adapter not initialized. Call initialize() first.",
        true,
      );
    }
  }

  /**
   * Enqueue a WASM operation to ensure serialized access.
   * Each operation waits for the previous one to complete.
   */
  private enqueue<T>(operation: () => T): Promise<T> {
    const result = this.queue.then(() => {
      try {
        return operation();
      } catch (error) {
        throw this.normalizeError(error);
      }
    });
    // Update queue to track completion (ignore rejections for queue chaining)
    this.queue = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  private processAction(action: GameAction): GameEvent[] {
    const result = submit_action(action);
    if (typeof result === "string") {
      throw new AdapterError(
        AdapterErrorCode.INVALID_ACTION,
        result,
        true,
      );
    }
    return result.events ?? [];
  }

  private fetchState(): GameState {
    const state = get_game_state();
    if (state === null) {
      return create_initial_state() as GameState;
    }
    return state as GameState;
  }

  private normalizeError(error: unknown): AdapterError {
    if (error instanceof AdapterError) return error;

    const message =
      error instanceof Error ? error.message : String(error);
    return new AdapterError(
      AdapterErrorCode.WASM_ERROR,
      message,
      false,
    );
  }
}
