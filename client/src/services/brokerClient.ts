import type { FormatConfig, LobbyGame, MatchConfig, PeerInfo } from "../adapter/types";
import type { ServerInfo } from "../adapter/ws-adapter";
import {
  HandshakeError,
  openPhaseSocket,
  type PhaseSocket,
} from "./openPhaseSocket";

export interface RegisterHostRequest {
  /** PeerJS peer ID guests dial to reach the host's engine. */
  hostPeerId: string;
  deck: {
    main_deck: string[];
    sideboard: string[];
    commander: string[];
  };
  displayName: string;
  public: boolean;
  password: string | null;
  timerSeconds: number | null;
  playerCount: number;
  matchConfig: MatchConfig;
  formatConfig: FormatConfig | null;
  aiSeats: unknown[];
  roomName: string | null;
}

export interface RegisteredGame {
  gameCode: string;
  playerToken: string;
}

/**
 * Discriminated result of a guest-side `resolveGuest` RPC. Returning a
 * discriminated union (rather than throwing on every error) lets the
 * caller retry without a fresh handshake on the common `password_required`
 * path — the existing socket is reused.
 */
export type ResolveResult =
  | { ok: true; peerInfo: PeerInfo }
  | {
      ok: false;
      reason:
        | "password_required"
        | "build_mismatch"
        | "not_found"
        | "room_full"
        | "connection_lost"
        | "error";
      message: string;
    };

export interface BrokerClient {
  readonly serverInfo: ServerInfo;
  registerHost(req: RegisterHostRequest): Promise<RegisteredGame>;
  updateMetadata(gameCode: string, currentPlayers: number, maxPlayers: number): void;
  unregister(gameCode: string): Promise<void>;
  close(): void;
}

export interface OpenBrokerOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

/**
 * Opens a `LobbyOnly`-only broker session. Rejects with `HandshakeError`
 * if the reachable server isn't in `LobbyOnly` mode — callers that want
 * the generic handshake (Full or LobbyOnly) should use `openPhaseSocket`
 * directly and inspect `serverInfo.mode` themselves.
 */
export async function openBrokerClient(
  wsUrl: string,
  opts: OpenBrokerOptions = {},
): Promise<BrokerClient> {
  const socket = await openPhaseSocket(wsUrl, opts);
  if (socket.serverInfo.mode !== "LobbyOnly") {
    socket.close();
    throw new HandshakeError(
      "protocol_mismatch",
      `Expected LobbyOnly server, got ${socket.serverInfo.mode}`,
    );
  }
  return makeBrokerClient(socket);
}

function makeBrokerClient(socket: PhaseSocket): BrokerClient {
  const { ws, serverInfo } = socket;
  let closed = false;

  const registerHost = (req: RegisterHostRequest): Promise<RegisteredGame> => {
    return new Promise<RegisteredGame>((resolve, reject) => {
      if (closed || ws.readyState !== WebSocket.OPEN) {
        reject(new Error("Broker socket not open"));
        return;
      }

      const listener = (event: MessageEvent) => {
        // Trust-boundary parse: ignore malformed frames rather than
        // letting the exception escape to the MessageEvent handler.
        let msg: { type: string; data?: unknown };
        try {
          msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
        } catch {
          return;
        }
        if (msg.type === "GameCreated") {
          const data = msg.data as { game_code: string; player_token: string };
          cleanup();
          resolve({ gameCode: data.game_code, playerToken: data.player_token });
        } else if (msg.type === "Error") {
          const data = msg.data as { message: string };
          cleanup();
          reject(new Error(data.message));
        }
      };

      const closeListener = () => {
        cleanup();
        reject(new Error("Broker socket closed before registration"));
      };

      const cleanup = () => {
        ws.removeEventListener("message", listener);
        ws.removeEventListener("close", closeListener);
      };

      ws.addEventListener("message", listener);
      ws.addEventListener("close", closeListener, { once: true });

      ws.send(
        JSON.stringify({
          type: "CreateGameWithSettings",
          data: {
            deck: req.deck,
            display_name: req.displayName,
            public: req.public,
            password: req.password,
            timer_seconds: req.timerSeconds,
            player_count: req.playerCount,
            match_config: req.matchConfig,
            format_config: req.formatConfig,
            ai_seats: req.aiSeats,
            room_name: req.roomName,
            host_peer_id: req.hostPeerId,
          },
        }),
      );
    });
  };

  const updateMetadata = (gameCode: string, currentPlayers: number, maxPlayers: number): void => {
    if (closed || ws.readyState !== WebSocket.OPEN) return;
    ws.send(
      JSON.stringify({
        type: "UpdateLobbyMetadata",
        data: { game_code: gameCode, current_players: currentPlayers, max_players: maxPlayers },
      }),
    );
  };

  const unregister = async (gameCode: string): Promise<void> => {
    if (closed || ws.readyState !== WebSocket.OPEN) {
      // Best-effort: if the socket is already gone the server's
      // 5-minute expiry will reap the lobby entry. Callers already
      // treat unregister as fire-and-forget.
      return;
    }
    ws.send(
      JSON.stringify({
        type: "UnregisterLobby",
        data: { game_code: gameCode },
      }),
    );
  };

  return {
    serverInfo,
    registerHost,
    updateMetadata,
    unregister,
    close: () => {
      if (closed) return;
      closed = true;
      socket.close();
    },
  };
}

// ── Helpers that operate on externally-owned PhaseSockets ─────────────

export interface ResolveGuestOptions {
  /**
   * Abort a pending resolve. When the signal fires the promise resolves
   * (not rejects) with `{ ok: false, reason: "connection_lost" }` and
   * listeners are detached — keeping the API uniform with the other
   * `ResolveResult` paths so callers have one branch shape to handle.
   */
  signal?: AbortSignal;
  /**
   * Cap the wait before we give up and resolve with `connection_lost`.
   * Defaults to 10_000 — servers that hang without replying should not
   * leak listeners forever. A caller that wants unbounded waits can
   * pass `Infinity`.
   */
  timeoutMs?: number;
}

/**
 * Sends `JoinGameWithPassword` over an already-open `PhaseSocket` (typically
 * the multiplayer-home subscription socket) and awaits the `PeerInfo`
 * response, mapping server-side errors into a discriminated `ResolveResult`.
 *
 * Does NOT close the socket — ownership stays with the caller. A password
 * retry loop can call this repeatedly without paying a fresh handshake.
 */
export function resolveGuestOver(
  socket: PhaseSocket,
  code: string,
  password?: string,
  opts: ResolveGuestOptions = {},
): Promise<ResolveResult> {
  const { ws } = socket;
  const { signal, timeoutMs = 10_000 } = opts;

  return new Promise<ResolveResult>((resolve) => {
    if (ws.readyState !== WebSocket.OPEN) {
      resolve({
        ok: false,
        reason: "connection_lost",
        message: "Lobby connection dropped, please try again",
      });
      return;
    }
    if (signal?.aborted) {
      resolve({
        ok: false,
        reason: "connection_lost",
        message: "Resolve aborted before start",
      });
      return;
    }

    const listener = (event: MessageEvent) => {
      // See the `registerHost` listener above — same trust-boundary
      // parse-then-dispatch pattern.
      let msg: { type: string; data?: unknown };
      try {
        msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
      } catch {
        return;
      }
      if (msg.type === "PeerInfo") {
        const data = msg.data as PeerInfo;
        if (data.game_code !== code) return;
        cleanup();
        resolve({ ok: true, peerInfo: data });
      } else if (msg.type === "PasswordRequired") {
        const data = msg.data as { game_code: string };
        if (data.game_code !== code) return;
        cleanup();
        resolve({
          ok: false,
          reason: "password_required",
          message: "This room requires a password",
        });
      } else if (msg.type === "Error") {
        const data = msg.data as { message: string };
        cleanup();
        resolve({ ok: false, reason: classifyError(data.message), message: data.message });
      }
    };

    const closeListener = () => {
      cleanup();
      resolve({
        ok: false,
        reason: "connection_lost",
        message: "Lobby connection dropped, please try again",
      });
    };

    const onAbort = () => {
      cleanup();
      resolve({
        ok: false,
        reason: "connection_lost",
        message: "Resolve aborted",
      });
    };

    // Cap the wait so an unresponsive server doesn't leak listeners.
    // `Infinity` is supported for callers that explicitly opt out.
    const timer =
      Number.isFinite(timeoutMs) && timeoutMs > 0
        ? setTimeout(() => {
            cleanup();
            resolve({
              ok: false,
              reason: "connection_lost",
              message: `No response from lobby within ${timeoutMs}ms`,
            });
          }, timeoutMs)
        : null;

    const cleanup = () => {
      if (timer !== null) clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      ws.removeEventListener("message", listener);
      ws.removeEventListener("close", closeListener);
    };

    signal?.addEventListener("abort", onAbort, { once: true });
    ws.addEventListener("message", listener);
    ws.addEventListener("close", closeListener, { once: true });

    ws.send(
      JSON.stringify({
        type: "JoinGameWithPassword",
        data: {
          game_code: code,
          // Guest-path resolve is deck-less: the broker does not need a
          // deck to hand back PeerInfo. Deck submission happens over the
          // P2P channel once the guest has dialed the host.
          deck: { main_deck: [], sideboard: [], commander: [] },
          display_name: "",
          password: password ?? null,
        },
      }),
    );
  });
}

type FailureReason = Extract<ResolveResult, { ok: false }>["reason"];

function classifyError(message: string): FailureReason {
  const lower = message.toLowerCase();
  if (lower.includes("build mismatch")) return "build_mismatch";
  if (lower.includes("not found")) return "not_found";
  if (lower.includes("full")) return "room_full";
  // Server sends `Error { message: "Wrong password" }` on a retry with the
  // incorrect password (not a second `PasswordRequired` — see
  // `phase-server/src/main.rs:1984-2003`). Map it back so the password
  // retry loop in `MultiplayerPage.joinP2PRoom` reprompts rather than
  // bailing out with a generic toast.
  if (lower.includes("wrong password") || lower.includes("password")) {
    return "password_required";
  }
  return "error";
}

/**
 * Subscribes to lobby-broadcast updates on an externally-owned
 * `PhaseSocket`. The initial `LobbyUpdate` snapshot is authoritative — if
 * it arrives after a reconnect, UIs should replace the cached list
 * rather than merge. `LobbyGameAdded/Updated/Removed` are delta frames.
 *
 * Returns a cleanup function that detaches listeners and sends
 * `UnsubscribeLobby`. Does NOT close the socket.
 */
export function subscribeLobbyOver(
  socket: PhaseSocket,
  onUpdate: (games: LobbyGame[]) => void,
): () => void {
  const { ws } = socket;
  let current: LobbyGame[] = [];

  const listener = (event: MessageEvent) => {
    let msg: { type: string; data?: unknown };
    try {
      msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
    } catch {
      return;
    }
    switch (msg.type) {
      case "LobbyUpdate": {
        const data = msg.data as { games: LobbyGame[] };
        current = data.games;
        onUpdate(current);
        break;
      }
      // Added and Updated are both handled as an upsert keyed by
      // `game_code`. Applying a delta twice must equal applying it once
      // (idempotence), otherwise any overlap between the initial
      // `LobbyUpdate` snapshot and a subsequent delta — or a reconnect
      // replay — duplicates rows in the UI. Treating Update as an
      // insert-if-missing also guards against missed Adds on a dropped
      // frame; the server is authoritative either way.
      case "LobbyGameAdded":
      case "LobbyGameUpdated": {
        const data = msg.data as { game: LobbyGame };
        const idx = current.findIndex(
          (g) => g.game_code === data.game.game_code,
        );
        current =
          idx >= 0
            ? current.map((g, i) => (i === idx ? data.game : g))
            : [...current, data.game];
        onUpdate(current);
        break;
      }
      case "LobbyGameRemoved": {
        const data = msg.data as { game_code: string };
        current = current.filter((g) => g.game_code !== data.game_code);
        onUpdate(current);
        break;
      }
    }
  };

  ws.addEventListener("message", listener);

  if (ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ type: "SubscribeLobby" }));
  }

  return () => {
    ws.removeEventListener("message", listener);
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "UnsubscribeLobby" }));
    }
  };
}
