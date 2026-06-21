import { AdapterError } from "./types";

export { WasmAdapter } from "./wasm-adapter";
export { P2PHostAdapter, P2PGuestAdapter } from "./p2p-adapter";
export { DraftPodHostAdapter } from "./draftPodHostAdapter";
export { DraftPodGuestAdapter } from "./draftPodGuestAdapter";
export { ServerDraftAdapter } from "./server-draft-adapter";
export { AdapterError, AdapterErrorCode } from "./types";
export type { EngineAdapter, GameAction, GameEvent, GameState, GameObject, WaitingFor, ActionResult } from "./types";
export type { ObjectId, CardId, PlayerId, Phase, Zone, Player, StackEntry } from "./types";
export type { P2PAdapterEvent } from "./p2p-adapter";
export type { DraftPodHostEvent, DraftPodHostConfig, DraftPodHostStatus } from "./draftPodHostAdapter";
export type { DraftPodGuestEvent, DraftPodGuestConfig, DraftPodGuestStatus } from "./draftPodGuestAdapter";
export type { ServerDraftAdapterEvent, CreateDraftSettings, DraftPhase } from "./server-draft-adapter";

/**
 * Validates that the adapter type is allowed for the given player count.
 * P2P is only available for 2-player games; 3+ player games require WebSocket.
 */
export function validateAdapterForPlayerCount(
  adapterType: "p2p" | "websocket" | "wasm",
  playerCount: number,
): void {
  if (adapterType === "p2p" && playerCount > 2) {
    throw new AdapterError(
      "P2P_PLAYER_LIMIT",
      "P2P is only available for 2-player games. Use server mode for multiplayer.",
      false,
    );
  }
}
