import {
  LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL,
  MIN_SUPPORTED_SERVER_PROTOCOL,
  PROTOCOL_VERSION,
  type ServerInfo,
} from "../adapter/ws-adapter";

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
  removeEventListener(type: "close", listener: (event: CloseEvent) => void): void;
  send(data: string): void;
  close(): void;
}

export type PhaseSocketFactory<T extends PhaseSocketTransport = PhaseSocketTransport> =
  (url: string) => T;

export interface PhaseSocket<T extends PhaseSocketTransport = WebSocket> {
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
 * inspect `serverInfo.mode` themselves.
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
): Promise<PhaseSocket<WebSocket>>;
export function openPhaseSocket<T extends PhaseSocketTransport>(
  wsUrl: string,
  opts: OpenOptions<T>,
): Promise<PhaseSocket<T>>;
export function openPhaseSocket(
  wsUrl: string,
  opts: OpenOptions<PhaseSocketTransport> = {},
): Promise<PhaseSocket<PhaseSocketTransport>> {
  const { signal, timeoutMs = 5000 } = opts;

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
        public_url?: string;
      };
      const info: ServerInfo = {
        version: data.server_version,
        buildCommit: data.build_commit,
        protocolVersion: data.protocol_version,
        mode: data.mode,
        publicUrl: data.public_url,
      };

      const minAcceptedProtocol =
        info.mode === "LobbyOnly"
          ? LOBBY_MIN_SUPPORTED_SERVER_PROTOCOL
          : MIN_SUPPORTED_SERVER_PROTOCOL;

      // Accept any server in [minAcceptedProtocol, PROTOCOL_VERSION]. Full
      // servers are current-only for breaking game protocol releases; LobbyOnly
      // brokers keep a one-version rollout window because they do not carry
      // game-state/action payloads.
      if (
        info.protocolVersion < minAcceptedProtocol ||
        info.protocolVersion > PROTOCOL_VERSION
      ) {
        const reason =
          info.protocolVersion < minAcceptedProtocol
            ? `Server protocol version ${info.protocolVersion} is older than supported (client speaks ${PROTOCOL_VERSION}, min ${minAcceptedProtocol}). Please wait for the lobby to finish rolling out.`
            : `Server protocol version ${info.protocolVersion} is newer than this client (${PROTOCOL_VERSION}). Please refresh to update.`;
        settle(() => {
          ws.close();
          reject(new HandshakeError("protocol_mismatch", reason, info));
        });
        return;
      }

      const clientProtocolVersion =
        info.mode === "LobbyOnly" ? info.protocolVersion : PROTOCOL_VERSION;

      // Send our ClientHello back. For LobbyOnly brokers in the rollout
      // window, echo the accepted broker protocol so an older deployed worker
      // does not reject a newer local-dev client as a future protocol.
      ws.send(
        JSON.stringify({
          type: "ClientHello",
          data: {
            client_version: __APP_VERSION__,
            build_commit: __BUILD_HASH__,
            protocol_version: clientProtocolVersion,
          },
        }),
      );

      settle(() => {
        resolve({
          ws,
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
