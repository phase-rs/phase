import type { DataConnection } from "peerjs";

import type { P2PMessage } from "./protocol";
import { validateMessage } from "./protocol";

export interface PeerSession {
  /** Send a message. Returns false if the message was dropped (connection closed). */
  send(msg: P2PMessage): boolean;
  onMessage(handler: (msg: P2PMessage) => void): () => void;
  onDisconnect(handler: (reason: string) => void): () => void;
  close(reason?: string): void;
}

export function createPeerSession(conn: DataConnection, destroyPeer: () => void): PeerSession {
  const messageHandlers = new Set<(msg: P2PMessage) => void>();
  const disconnectHandlers = new Set<(reason: string) => void>();
  let closed = false;
  let disconnectReason: string | null = null;

  const pendingMessages: P2PMessage[] = [];

  // Ping/pong keep-alive
  let pingInterval: ReturnType<typeof setInterval> | null = null;
  let pongTimeout: ReturnType<typeof setTimeout> | null = null;

  const clearKeepAlive = () => {
    if (pingInterval !== null) { clearInterval(pingInterval); pingInterval = null; }
    if (pongTimeout !== null) { clearTimeout(pongTimeout); pongTimeout = null; }
  };

  const trySend = (msg: P2PMessage): boolean => {
    if (closed || !conn.open) return false;
    try {
      if (msg.type !== "ping" && msg.type !== "pong") {
        const size = JSON.stringify(msg).length;
        console.log(`[PeerSession] sending "${msg.type}" (${(size / 1024).toFixed(1)} KB)`);
      }
      conn.send(msg);
      return true;
    } catch (err) {
      console.warn("[PeerSession] send failed:", err);
      return false;
    }
  };

  const startKeepAlive = () => {
    pingInterval = setInterval(() => {
      if (!conn.open) return;

      if (pongTimeout !== null) { clearTimeout(pongTimeout); pongTimeout = null; }

      if (!trySend({ type: "ping", timestamp: Date.now() })) {
        handleDisconnect("Channel send failed");
        return;
      }

      pongTimeout = setTimeout(() => {
        if (!closed) handleDisconnect("Ping timeout");
      }, 10_000);
    }, 5_000);
  };

  const beforeUnloadHandler = () => {
    if (!closed && conn.open) {
      try {
        conn.send({ type: "disconnect", reason: "Page closed" });
      } catch { /* best-effort */ }
    }
  };
  window.addEventListener("beforeunload", beforeUnloadHandler);

  const handleDisconnect = (reason: string) => {
    if (closed) return;
    closed = true;
    disconnectReason = reason;
    console.warn("[PeerSession] disconnected:", reason);
    clearKeepAlive();
    window.removeEventListener("beforeunload", beforeUnloadHandler);
    for (const handler of disconnectHandlers) {
      handler(reason);
    }
    try { destroyPeer(); } catch (e) {
      console.warn("Error destroying peer:", e);
    }
  };

  const onData = (data: unknown) => {
    let msg: P2PMessage;
    try {
      msg = validateMessage(data);
    } catch (e) {
      console.warn("Failed to decode message from peer:", e);
      return;
    }

    if (msg.type === "pong") {
      if (pongTimeout !== null) { clearTimeout(pongTimeout); pongTimeout = null; }
      return;
    }

    if (msg.type === "ping") {
      trySend({ type: "pong", timestamp: msg.timestamp });
      return;
    }

    if (msg.type === "disconnect") {
      handleDisconnect(msg.reason);
      return;
    }

    if (messageHandlers.size === 0) {
      pendingMessages.push(msg);
      return;
    }

    for (const handler of messageHandlers) {
      handler(msg);
    }
  };

  conn.on("data", onData);
  conn.on("close", () => handleDisconnect("Connection closed"));
  conn.on("error", (err) => handleDisconnect(`Connection error: ${err.message}`));

  startKeepAlive();

  return {
    send(msg) {
      return trySend(msg);
    },
    onMessage(handler) {
      messageHandlers.add(handler);

      if (pendingMessages.length > 0) {
        const queued = pendingMessages.splice(0);
        for (const msg of queued) {
          handler(msg);
        }
      }

      return () => {
        messageHandlers.delete(handler);
      };
    },
    onDisconnect(handler) {
      disconnectHandlers.add(handler);

      if (disconnectReason !== null) {
        handler(disconnectReason);
      }

      return () => {
        disconnectHandlers.delete(handler);
      };
    },
    close(reason = "Left game") {
      if (!closed && conn.open) {
        try {
          conn.send({ type: "disconnect", reason });
        } catch { /* closing anyway */ }
      }
      handleDisconnect(reason);
    },
  };
}
