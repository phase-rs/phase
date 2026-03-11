import init, {
  ping,
  create_initial_state,
  initialize_game,
  submit_action,
  get_game_state,
  get_ai_action,
  get_legal_actions_js,
  restore_game_state,
  load_card_database,
} from "../wasm/engine_wasm";
import type { EngineAdapter, GameAction, GameEvent, GameState } from "./types";
import { AdapterError, AdapterErrorCode } from "./types";

/**
 * serde_wasm_bindgen serializes Rust HashMap<NonStringKey, V> as JS Map.
 * The frontend expects plain objects with bracket access, so we recursively
 * convert any Map instances to plain Record<string, V>.
 */
function convertMapsToRecords(value: unknown): unknown {
  if (value instanceof Map) {
    const obj: Record<string, unknown> = {};
    for (const [k, v] of value) {
      obj[String(k)] = convertMapsToRecords(v);
    }
    return obj;
  }
  if (Array.isArray(value)) {
    return value.map(convertMapsToRecords);
  }
  if (value !== null && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) {
      out[k] = convertMapsToRecords(v);
    }
    return out;
  }
  return value;
}

/**
 * WASM-backed implementation of EngineAdapter.
 * Communicates directly with the Rust engine compiled to WebAssembly.
 * Serializes all WASM access through an async queue (WASM is single-threaded).
 */
export class WasmAdapter implements EngineAdapter {
  private initialized = false;
  private queue: Promise<void> = Promise.resolve();
  cardDbLoaded = false;

  async initialize(): Promise<void> {
    if (this.initialized) return;
    await init();
    this.initialized = true;

    // Load the card database into WASM for name-based deck resolution
    try {
      const resp = await fetch("/card-data.json");
      if (resp.ok) {
        const text = await resp.text();
        const count = load_card_database(text);
        this.cardDbLoaded = true;
        console.log(`Card database loaded: ${count} cards`);
      } else {
        console.warn("card-data.json not available (HTTP", resp.status, ")");
      }
    } catch (err) {
      console.warn("Failed to load card database:", err);
    }
  }

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    this.assertInitialized();
    return this.enqueue(() => this.processAction(action));
  }

  async getState(): Promise<GameState> {
    this.assertInitialized();
    return this.enqueue(() => this.fetchState());
  }

  async getLegalActions(): Promise<GameAction[]> {
    this.assertInitialized();
    return this.enqueue(() => {
      const actions = get_legal_actions_js();
      if (actions === null) return [];
      return convertMapsToRecords(actions) as GameAction[];
    });
  }

  getAiAction(difficulty: string): GameAction | null {
    this.assertInitialized();
    const result = get_ai_action(difficulty);
    if (result == null) return null;
    return result as GameAction;
  }

  restoreState(state: GameState): void {
    this.assertInitialized();
    restore_game_state(JSON.stringify(state));
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
    const seed = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);
    const result = initialize_game(deckData ?? null, seed);
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
    return convertMapsToRecords(state) as GameState;
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
