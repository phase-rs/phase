import { WasmAdapter } from "./wasm-adapter";

export { WasmAdapter } from "./wasm-adapter";
export { P2PHostAdapter, P2PGuestAdapter } from "./p2p-adapter";
export { AdapterError, AdapterErrorCode } from "./types";
export type { EngineAdapter, GameAction, GameEvent, GameState, GameObject, WaitingFor, ActionResult } from "./types";
export type { ObjectId, CardId, PlayerId, Phase, Zone, Player, StackEntry } from "./types";
export type { P2PAdapterEvent } from "./p2p-adapter";

/** Tauri v2 detection: present when running inside a Tauri webview. */
declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
  }
}

/**
 * Creates the appropriate EngineAdapter based on platform detection.
 * - Tauri v2 detected: returns TauriAdapter (dynamic import to avoid bundling)
 * - Browser: returns WasmAdapter
 */
export async function createAdapter(): Promise<EngineAdapter> {
  if (typeof window !== "undefined" && window.__TAURI_INTERNALS__) {
    const { TauriAdapter } = await import("./tauri-adapter");
    return new TauriAdapter();
  }
  return new WasmAdapter();
}
