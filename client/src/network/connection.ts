import Peer from "peerjs";
import type { DataConnection } from "peerjs";

/** Unambiguous characters -- no 0/O, 1/I/L confusion */
const CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LENGTH = 5;
const PEER_ID_PREFIX = "phase-";

// Override PeerJS defaults -- their bundled TURN servers are broken
const PEER_CONFIG: RTCConfiguration = {
  iceServers: [
    { urls: "stun:stun.relay.metered.ca:80" },
    {
      urls: "turn:global.relay.metered.ca:80",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
    {
      urls: "turn:global.relay.metered.ca:80?transport=tcp",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
    {
      urls: "turn:global.relay.metered.ca:443",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
    {
      urls: "turns:global.relay.metered.ca:443?transport=tcp",
      username: "a267722d3eb02873687da73c",
      credential: "ob5D/eUnCmkkf1Vp",
    },
  ],
};

export interface HostResult {
  roomCode: string;
  waitForGuest: () => Promise<{ conn: DataConnection; destroyPeer: () => void }>;
  destroy: () => void;
}

export function generateRoomCode(): string {
  const chars: string[] = [];
  for (let i = 0; i < CODE_LENGTH; i++) {
    chars.push(CODE_ALPHABET[Math.floor(Math.random() * CODE_ALPHABET.length)]);
  }
  return chars.join("");
}

/**
 * Validate and normalize a room code from user input.
 * Returns the uppercase code or null if invalid.
 */
export function parseRoomCode(input: string): string | null {
  const code = input.trim().toUpperCase();
  if (code.length !== CODE_LENGTH) return null;
  for (const ch of code) {
    if (!CODE_ALPHABET.includes(ch)) return null;
  }
  return code;
}

/** Host creates a room and waits for a guest to connect. */
export function hostRoom(): HostResult {
  const roomCode = generateRoomCode();
  const peerId = PEER_ID_PREFIX + roomCode;
  const peer = new Peer(peerId, { config: PEER_CONFIG });

  let destroyed = false;

  // Track when the host is registered on the signaling server
  const peerReady = new Promise<void>((resolve, reject) => {
    peer.on("open", () => {
      console.log("[P2P Host] registered on signaling server, code:", roomCode);
      resolve();
    });
    peer.on("error", (err) => reject(new Error(`Failed to create room: ${err.message}`)));
  });

  const waitForGuest = async (): Promise<{ conn: DataConnection; destroyPeer: () => void }> => {
    if (destroyed) throw new Error("Host was destroyed before a guest connected");

    // Ensure we're registered on the signaling server before listening
    await peerReady;

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        reject(new Error("No one joined. The room timed out."));
        peer.destroy();
      }, 120_000);

      peer.on("error", (err) => {
        clearTimeout(timeout);
        reject(new Error(`Connection error: ${err.message}`));
        peer.destroy();
      });

      peer.on("connection", (conn) => {
        clearTimeout(timeout);
        conn.on("open", () => {
          resolve({ conn, destroyPeer: () => peer.destroy() });
        });
        conn.on("error", (err) => {
          clearTimeout(timeout);
          reject(new Error(`Guest connection error: ${err.message}`));
          peer.destroy();
        });
      });
    });
  };

  const destroy = () => {
    destroyed = true;
    peer.destroy();
  };

  return { roomCode, waitForGuest, destroy };
}

/** Guest joins a room by code. */
export function joinRoom(code: string): Promise<{ conn: DataConnection; destroyPeer: () => void }> {
  return new Promise((resolve, reject) => {
    const peer = new Peer({ config: PEER_CONFIG });
    const peerId = PEER_ID_PREFIX + code;

    peer.on("open", () => {
      console.log("[P2P Guest] registered on signaling server, connecting to:", peerId);
      const conn = peer.connect(peerId);

      const timeout = setTimeout(() => {
        reject(new Error("Connection timed out. Check the room code and try again."));
        peer.destroy();
      }, 30_000);

      conn.on("open", () => {
        clearTimeout(timeout);
        resolve({ conn, destroyPeer: () => peer.destroy() });
      });

      conn.on("error", (err) => {
        clearTimeout(timeout);
        reject(new Error(`Connection error: ${err.message}`));
        peer.destroy();
      });
    });

    // PeerJS emits connection failures on the peer, not the conn (issue #1281)
    peer.on("error", (err) => {
      reject(new Error(`Failed to connect: ${err.message}`));
      peer.destroy();
    });
  });
}
