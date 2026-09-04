/**
 * Draft-specific PeerSession wrapper.
 *
 * Uses the same ordered DataConnection transport pattern as `peer.ts`,
 * with the draft-specific codec and session lifecycle.
 */

import type { DataConnection } from "peerjs";

import type { DraftP2PMessage } from "./draftProtocol";
import { decodeDraftWireMessage, encodeDraftWireMessage } from "./draftProtocol";

export interface DraftPeerSession {
  /** Resolves after submitting bytes to an open connection, not after peer acknowledgement. */
  send(msg: DraftP2PMessage): Promise<void>;
  onMessage(handler: (msg: DraftP2PMessage) => void | Promise<void>): () => void;
  onDisconnect(handler: (reason: string) => void): () => void;
  close(reason?: string): void;
}

export interface DraftPeerSessionOptions {
  onSessionEnd?: () => void;
}

export function createDraftPeerSession(
  conn: DataConnection,
  options: DraftPeerSessionOptions = {},
): DraftPeerSession {
  const { onSessionEnd } = options;
  const messageHandlers = new Set<(msg: DraftP2PMessage) => void | Promise<void>>();
  const disconnectHandlers = new Set<(reason: string) => void>();
  let lifecycle: "open" | "draining" | "closed" = "open";

  // FIFO send queue for async compression
  let sendChain = Promise.resolve();
  // Like peer.ts, serialize decoding AND async dispatch. A later state update
  // must not overtake an earlier compressed frame or durable acknowledgement.
  let receiveChain = Promise.resolve();

  function isClosed(): boolean {
    return lifecycle === "closed";
  }

  function assertCanSend(): void {
    if (lifecycle !== "open" || !conn.open) {
      throw new Error("Draft connection is not open");
    }
  }

  function fireDisconnect(reason: string): void {
    if (isClosed()) return;
    lifecycle = "closed";
    for (const handler of disconnectHandlers) {
      try { handler(reason); } catch { /* best-effort */ }
    }
    disconnectHandlers.clear();
    messageHandlers.clear();
    onSessionEnd?.();
  }

  const onData = (raw: unknown): Promise<void> => {
    // Remote EOF stops intake, but messages accepted before it still drain.
    if (lifecycle !== "open") return Promise.resolve();
    receiveChain = receiveChain.then(async () => {
      if (isClosed() || !(raw instanceof ArrayBuffer || raw instanceof Uint8Array)) return;
      const bytes = raw instanceof Uint8Array ? raw : new Uint8Array(raw);
      let msg: DraftP2PMessage;
      try {
        msg = await decodeDraftWireMessage(bytes);
      } catch (err) {
        console.warn("[DraftPeerSession] decode error:", err);
        return;
      }
      if (isClosed()) return;
      for (const handler of messageHandlers) {
        if (isClosed()) return;
        try {
          await handler(msg);
        } catch (err) {
          // One failed subscriber must not silence other subscribers or
          // poison the queue for every subsequent frame.
          console.warn("[DraftPeerSession] message handler threw:", err, msg.type);
        }
      }
    });
    // PeerJS ignores this promise; test connections can await the whole entry.
    return receiveChain;
  };

  conn.on("data", onData);

  conn.on("close", () => {
    if (lifecycle !== "open") return;
    lifecycle = "draining";
    // Hosts send terminal acknowledgements immediately before closing. Finish
    // handling all accepted frames before notifying subscribers of that EOF.
    void receiveChain.then(() => fireDisconnect("connection closed"));
  });
  conn.on("error", (err: Error) => fireDisconnect(err.message));

  const session: DraftPeerSession = {
    send(msg: DraftP2PMessage): Promise<void> {
      const p = sendChain.then(async () => {
        assertCanSend();
        const bytes = await encodeDraftWireMessage(msg);
        // Compression can outlive the connection, including its receive drain.
        assertCanSend();
        conn.send(bytes);
      });
      // Keep the queue live while preserving this entry's rejection for callers.
      sendChain = p.catch(() => { /* best-effort callers may ignore the result */ });
      return p;
    },
    onMessage(handler) {
      messageHandlers.add(handler);
      return () => { messageHandlers.delete(handler); };
    },
    onDisconnect(handler) {
      disconnectHandlers.add(handler);
      return () => { disconnectHandlers.delete(handler); };
    },
    close(reason?: string) {
      if (isClosed()) return;
      fireDisconnect(reason ?? "closed");
      try { conn.close(); } catch { /* best-effort */ }
    },
  };

  return session;
}
