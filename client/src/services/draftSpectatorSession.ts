import type { SpectatorDraftView } from "../adapter/draft-adapter";
import { openPhaseSocket } from "./openPhaseSocket";
import { isValidWebSocketUrl } from "./serverDetection";

export type DraftSpectatorEvent =
  | { type: "connected" }
  | { type: "view"; view: SpectatorDraftView }
  | { type: "error"; message: string }
  | { type: "disconnected" };

export interface DraftSpectatorSession {
  close: () => void;
  onEvent: (listener: (event: DraftSpectatorEvent) => void) => () => void;
}

export async function connectDraftSpectator(
  serverUrl: string,
  draftCode: string,
): Promise<DraftSpectatorSession> {
  if (!isValidWebSocketUrl(serverUrl)) {
    throw new Error("Invalid WebSocket URL");
  }

  const socket = await openPhaseSocket(serverUrl);
  const listeners: Array<(event: DraftSpectatorEvent) => void> = [];

  const emit = (event: DraftSpectatorEvent) => {
    for (const listener of listeners) {
      listener(event);
    }
  };

  const onMessage = (event: MessageEvent) => {
    if (typeof event.data !== "string") return;
    let msg: { type: string; data?: unknown };
    try {
      msg = JSON.parse(event.data) as { type: string; data?: unknown };
    } catch {
      return;
    }
    switch (msg.type) {
      case "DraftSpectatorView":
        emit({
          type: "view",
          view: (msg.data as { view: SpectatorDraftView }).view,
        });
        emit({ type: "connected" });
        break;
      case "Error":
        emit({ type: "error", message: (msg.data as { message?: string })?.message ?? "Unknown error" });
        break;
      default:
        break;
    }
  };

  socket.ws.addEventListener("message", onMessage);
  socket.ws.addEventListener("close", () => emit({ type: "disconnected" }));
  socket.ws.send(
    JSON.stringify({ type: "SpectateDraft", data: { draft_code: draftCode } }),
  );

  return {
    close: () => {
      socket.ws.removeEventListener("message", onMessage);
      socket.close();
    },
    onEvent: (listener) => {
      listeners.push(listener);
      return () => {
        const index = listeners.indexOf(listener);
        if (index >= 0) listeners.splice(index, 1);
      };
    },
  };
}
