/**
 * Shared interface for opponent controllers (AI, WebSocket, etc.).
 * The game loop uses this abstraction to manage opponent lifecycle.
 */
export interface OpponentController {
  start(): void;
  stop(): void;
  dispose(): void;
}
