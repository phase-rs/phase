import {
  LOBBY_PROTOCOL_VERSION,
  PROTOCOL_VERSION,
  serverProtocolRejection,
  type ProtocolSurface,
  type ServerInfo,
} from "../adapter/ws-adapter";
import { supportsGzipEnvelope, type WireFormat } from "../network/wireEnvelope";
import { GzipEnvelopeSocket } from "./gzipEnvelopeSocket";

/**
 * Result of a successful handshake with `phase-server`. Wraps the live
 * socket plus the `ServerInfo` parsed from `ServerHello`. Callers
 * own the socket — whoever received a `PhaseSocket` from `openPhaseSocket`
 * is responsible for calling `close()` when done.
 */
export interface PhaseSocketTransport {
  readonly readyState: number;
  onopen: ((event: Event) => void) | null;
  onmessage: ((event: MessageEvent<string>) => void) | null;
  onerror: ((event: Event) => void) | null;
  onclose: ((event: CloseEvent) => void) | null;
  addEventListener(
    type: "close",
    listener: (event: CloseEvent) => void,
    options?: AddEventListenerOptions | boolean,
  ): void;
  addEventListener(
    type: "message",
    listener: (event: MessageEvent<string>) => void,
    options?: AddEventListenerOptions | boolean,
  ): void;
  removeEventListener(type: "close", listener: (event: CloseEvent) => void): void;
  removeEventListener(type: "message", listener: (event: MessageEvent<string>) => void): void;
  send(data: string): void;
  close(): void;
}

export type PhaseSocketFactory<T extends PhaseSocketTransport = PhaseSocketTransport> =
  (url: string) => T;

export interface PhaseSocket<T extends PhaseSocketTransport = PhaseSocketTransport> {
  readonly ws: T;
  readonly serverInfo: ServerInfo;
  close(): void;
}

export interface OpenOptions<T extends PhaseSocketTransport = WebSocket> {
  /**
   * Abort the pending handshake. If the signal fires before resolution the
   * returned promise rejects with an `AbortError` AND the in-flight
   * `WebSocket` is closed synchronously, so no half-open socket leaks.
   */
  signal?: AbortSignal;
  /** WS-open + ServerHello wait cap, in ms. Defaults to 5000. */
  timeoutMs?: number;
  /**
   * Creates the transport used for the handshake. Omitted callers retain the
   * browser's direct `new WebSocket(url)` behavior.
   */
  socketFactory?: PhaseSocketFactory<T>;
  /**
   * Which protocol surface this socket will carry. Defaults to `"full"`, the
   * conservative choice: a caller that has not thought about it gets the
   * exact-match full-game window.
   *
   * Pass `"lobby"` ONLY for a socket that can never elicit a full-game reply.
   * Sending `LobbyClientMessage` variants is necessary but NOT sufficient: a
   * `Full` server answers `JoinGameWithPassword` and `CreateGameWithSettings`
   * from its server-run game path with `SessionAttached`/`StateUpdate`, which
   * are not `LobbyServerMessage` variants at all. Those two frames are gated in
   * `brokerClient.ts` — `resolveGuestOver` refuses every `Full` server, and
   * `openBrokerClient` accepts only `LobbyOnly` ones. Server-run hosting,
   * joining, drafts and spectating each open their own socket and must keep the
   * default.
   */
  surface?: ProtocolSurface;
}

export class HandshakeError extends Error {
  constructor(
    public readonly kind:
      | "invalid_url"
      | "timeout"
      | "closed_before_hello"
      | "protocol_mismatch"
      | "aborted"
      | "ws_error",
    message: string,
    /**
     * The `ServerInfo` parsed from `ServerHello`, when available. Only the
     * `protocol_mismatch` path currently populates this — the other
     * failure modes occur before identity is known. Surfaced so the UI
     * can render accurate "server is on X, you are on Y" diagnostics
     * instead of placeholder zeroes.
     */
    public readonly serverInfo?: ServerInfo,
  ) {
    super(message);
    this.name = "HandshakeError";
  }
}

/**
 * Opens a WebSocket to `wsUrl`, waits for `ServerHello`, sends `ClientHello`,
 * and resolves with a ready-to-use `PhaseSocket`. Mode-agnostic: works for
 * both `Full` and `LobbyOnly` servers — callers that need to gate on mode
 * inspect `serverInfo.mode` themselves. `opts.surface` selects which protocol
 * window the handshake is held to; see {@link OpenOptions.surface}.
 *
 * Failure modes (all result in the returned promise rejecting with a
 * `HandshakeError` and the underlying socket being closed):
 * - Invalid URL
 * - WS never opens within `timeoutMs`
 * - Server closes the socket before sending `ServerHello`
 * - Protocol-version mismatch (local `PROTOCOL_VERSION` vs server's)
 * - `opts.signal` aborts during the pending handshake
 */
export function openPhaseSocket(
  wsUrl: string,
  opts?: OpenOptions<WebSocket>,
): Promise<PhaseSocket<PhaseSocketTransport>>;
export function openPhaseSocket<T extends PhaseSocketTransport>(
  wsUrl: string,
  opts: OpenOptions<T>,
): Promise<PhaseSocket<PhaseSocketTransport>>;
export function openPhaseSocket(
  wsUrl: string,
  opts: OpenOptions<PhaseSocketTransport> = {},
): Promise<PhaseSocket<PhaseSocketTransport>> {
  const { signal, timeoutMs = 5000, surface = "full" } = opts;

  return new Promise<PhaseSocket<PhaseSocketTransport>>((resolve, reject) => {
    if (signal?.aborted) {
      reject(new HandshakeError("aborted", "Handshake aborted before start"));
      return;
    }

    let ws: PhaseSocketTransport;
    try {
      ws = opts.socketFactory?.(wsUrl) ?? new WebSocket(wsUrl);
    } catch (err) {
      reject(
        new HandshakeError(
          "invalid_url",
          err instanceof Error ? err.message : String(err),
        ),
      );
      return;
    }

    let settled = false;
    const settle = (fn: () => void) => {
      if (settled) return;
      settled = true;
      cleanup();
      fn();
    };

    const timer = setTimeout(() => {
      settle(() => {
        ws.close();
        reject(
          new HandshakeError(
            "timeout",
            `Handshake did not complete within ${timeoutMs}ms`,
          ),
        );
      });
    }, timeoutMs);

    const onAbort = () => {
      settle(() => {
        // Close synchronously so the caller cannot observe a half-open
        // socket after the promise rejects. Covered by the
        // `aborted signal closes the in-flight socket` unit test.
        ws.close();
        reject(new HandshakeError("aborted", "Handshake aborted"));
      });
    };

    const cleanup = () => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      ws.onopen = null;
      ws.onmessage = null;
      ws.onerror = null;
      ws.onclose = null;
    };

    signal?.addEventListener("abort", onAbort, { once: true });

    ws.onopen = () => {
      // Nothing to do on open — we wait for ServerHello before sending
      // ClientHello. The server sends it unprompted on accept.
    };

    ws.onerror = () => {
      settle(() => {
        ws.close();
        reject(new HandshakeError("ws_error", "WebSocket error during handshake"));
      });
    };

    ws.onclose = () => {
      settle(() => {
        reject(
          new HandshakeError(
            "closed_before_hello",
            "Socket closed before ServerHello arrived",
          ),
        );
      });
    };

    ws.onmessage = (event) => {
      // The socket is the client's trust boundary — a malformed or
      // hostile frame must not crash the handshake with an unhandled
      // exception. Parse errors drop the frame silently; a real
      // `ServerHello` is what we're waiting for, and the timeout covers
      // the case where one never arrives.
      let msg: { type: string; data?: unknown };
      try {
        msg = JSON.parse(event.data as string) as { type: string; data?: unknown };
      } catch {
        return;
      }
      if (msg.type !== "ServerHello") {
        // Ignore any stray frames pre-hello. A well-behaved server sends
        // ServerHello first and nothing else; if a malicious/broken server
        // sends other frames, we drop them on the floor rather than try
        // to reason about them before identity is known.
        return;
      }
      const data = msg.data as {
        server_version: string;
        build_commit: string;
        protocol_version: number;
        mode: "Full" | "LobbyOnly";
        lobby_protocol_version?: number;
        public_url?: string;
        wire_formats?: WireFormat[];
      };
      const info: ServerInfo = {
        version: data.server_version,
        buildCommit: data.build_commit,
        protocolVersion: data.protocol_version,
        mode: data.mode,
        lobbyProtocolVersion: data.lobby_protocol_version,
        publicUrl: data.public_url,
        wireFormats: data.wire_formats ?? [],
      };

      const rejection = serverProtocolRejection(info, surface);
      if (rejection) {
        settle(() => {
          ws.close();
          reject(new HandshakeError("protocol_mismatch", rejection, info));
        });
        return;
      }

      const onLobbySurface = surface === "lobby" || info.mode === "LobbyOnly";
      const clientProtocolVersion = onLobbySurface
        ? info.protocolVersion
        : PROTOCOL_VERSION;

      // Send our ClientHello back.
      //
      // `protocol_version` echoes the server's own number on the lobby surface.
      // `ClientHello` carries no surface field, so `HelloAcceptance::FullGame`
      // in `crates/phase-server/src/main.rs` holds every socket a `Full` server
      // accepts to `MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION` — the echo is
      // therefore this client's only way to declare the socket lobby-only, and
      // the one that reaches servers already deployed. Sound only because the
      // caller keeps its side of {@link OpenOptions.surface} — a `Full` server
      // will still answer a game frame sent over this socket.
      //
      // `lobby_protocol_version` is always our own: a server that understands
      // it gates on that instead, which is what decouples the lobby handshake
      // from full-game churn. Servers that predate the field ignore it (nothing
      // sets `deny_unknown_fields`).
      const localWireFormats: WireFormat[] = supportsGzipEnvelope() && "binaryType" in ws
        ? ["GzipEnvelopeV1"]
        : [];
      ws.send(
        JSON.stringify({
          type: "ClientHello",
          data: {
            client_version: __APP_VERSION__,
            build_commit: __BUILD_HASH__,
            protocol_version: clientProtocolVersion,
            lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
            wire_formats: localWireFormats,
          },
        }),
      );

      settle(() => {
        const useGzipEnvelope = localWireFormats.includes("GzipEnvelopeV1")
          && info.wireFormats?.includes("GzipEnvelopeV1");
        resolve({
          ws: useGzipEnvelope ? new GzipEnvelopeSocket(ws) : ws,
          serverInfo: info,
          close: () => ws.close(),
        });
      });
    };
  });
}

// ── withReconnect ───────────────────────────────────────────────────────

export type ReconnectState =
  | "connecting"
  | "open"
  | "reconnecting"
  | "offline";

export interface ReconnectOptions {
  signal?: AbortSignal;
  /** Number of reconnect attempts after an unexpected drop. Default 3. */
  attempts?: number;
  /**
   * Milliseconds to wait before attempt `n` (0-indexed). Default yields
   * 500, 1500, 4500 for the first three attempts.
   */
  backoffMs?: (attempt: number) => number;
  onStateChange?: (state: ReconnectState) => void;
}

export interface ReconnectHandle {
  /**
   * The current live `PhaseSocket`, or `null` while we're mid-reconnect,
   * before the first connect resolves, or after `close()`.
   */
  current(): PhaseSocket | null;
  /**
   * Abort any pending retry and close the current socket. Idempotent; safe
   * to call more than once.
   */
  close(): void;
}

const DEFAULT_BACKOFF = (attempt: number) => 500 * Math.pow(3, attempt);

/**
 * Re-runs `factory` on unexpected close up to `attempts` times. Surfaces
 * state transitions via `onStateChange` so callers can reject pending
 * in-flight work at the moment `reconnecting` fires, rather than waiting
 * for the drop to propagate up through their own timeouts.
 *
 * Deliberately does NOT track caller-level work — if the caller has a
 * pending RPC over the socket when it drops, they're responsible for
 * rejecting it. `onStateChange === "reconnecting"` is the hook they use.
 */
export function withReconnect(
  factory: (attempt: number) => Promise<PhaseSocket>,
  opts: ReconnectOptions = {},
): ReconnectHandle {
  const {
    signal,
    attempts = 3,
    backoffMs = DEFAULT_BACKOFF,
    onStateChange,
  } = opts;

  let socket: PhaseSocket | null = null;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let closed = false;
  let attempt = 0;

  const notify = (state: ReconnectState) => {
    try {
      onStateChange?.(state);
    } catch {
      // Swallow listener errors — one bad subscriber should not break
      // the reconnect loop.
    }
  };

  const clearRetry = () => {
    if (retryTimer !== null) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
  };

  const connect = async () => {
    if (closed || signal?.aborted) return;
    notify(attempt === 0 ? "connecting" : "reconnecting");
    try {
      const next = await factory(attempt);
      if (closed) {
        // A `close()` landed while the handshake was in flight; undo it.
        next.close();
        return;
      }
      socket = next;
      attempt = 0;
      notify("open");
      next.ws.addEventListener("close", onDrop, { once: true });
    } catch {
      scheduleRetry();
    }
  };

  const onDrop = () => {
    if (closed) return;
    socket = null;
    scheduleRetry();
  };

  const scheduleRetry = () => {
    if (closed) return;
    if (attempt >= attempts) {
      notify("offline");
      return;
    }
    notify("reconnecting");
    const delay = backoffMs(attempt);
    attempt++;
    retryTimer = setTimeout(() => {
      retryTimer = null;
      void connect();
    }, delay);
  };

  signal?.addEventListener(
    "abort",
    () => {
      close();
    },
    { once: true },
  );

  const close = () => {
    if (closed) return;
    closed = true;
    clearRetry();
    if (socket) {
      socket.close();
      socket = null;
    }
  };

  // Kick off the first connect. Swallow the synchronous path — any failure
  // transitions to "offline" via scheduleRetry.
  void connect();

  return {
    current: () => socket,
    close,
  };
}
