import type {
  BracketShape,
  MatchArity,
  PairingId,
  PodOutcome,
  ScoringPolicy,
  TournamentCreatedReply,
  TournamentJoinedReply,
  TournamentSummary,
  TournamentUpdateReply,
  TournamentView,
} from "../adapter/types";
import type { PhaseSocket } from "./openPhaseSocket";

/**
 * Tournament-organizer RPCs and broadcast subscription over a **borrowed**
 * `PhaseSocket`. Five properties of this module are deliberate and load-bearing;
 * each is pinned by a test in `__tests__/tournamentClient.test.ts`.
 *
 * ## 1. This module owns no socket
 *
 * Every function here takes an already-open socket as its first parameter and
 * leaves its lifetime alone: nothing here opens one, and nothing here shuts one
 * down on any path — the borrowed socket's owner does that. `openPhaseSocket`
 * is imported for the `PhaseSocket` type alone, never invoked. Socket
 * acquisition, credential storage and reconnect handling all belong to
 * `stores/multiplayerStore.ts`.
 *
 * ## 2. This module owns no subscription
 *
 * {@link subscribeTournamentsOver} sends neither `SubscribeLobby` nor
 * `UnsubscribeLobby` — it sends nothing at all, ever. That is a deliberate
 * departure from `subscribeLobbyOver` in `brokerClient.ts`, which does send
 * both frames from its attach and its detach.
 *
 * The reason is that broker-side those two frames are not per-subscriber, they
 * are per-connection: `SubscribeLobby` inserts this connection's sender into one
 * delivery set (`AddSubscriber`) and `UnsubscribeLobby` removes it
 * (`RemoveSubscriber`, `crates/lobby-broker/src/broker.rs:317-343`), so a single
 * removal kills every tournament subscription riding the same socket no matter
 * how many subscribes preceded it. One reference count spanning both lobby and
 * tournament subscribers is therefore the only correct model, and that count
 * lives with the socket's owner in `multiplayerStore.ts`. If this module sent
 * the frames, they would sit outside the count that has to govern them.
 *
 * ## 3. Four of the seven RPCs get no point reply at all
 *
 * `StartTournamentRound`, `ReportMatchResult`, `DropFromTournament` and
 * `EndTournament` produce no `ToSelf` frame on success — only a `ToSubscribers`
 * broadcast (`crates/lobby-broker/src/broker.rs:1205-1336`). Their helpers
 * therefore settle on the *broadcast*, which means an unsubscribed caller never
 * observes success and will settle `"timeout"` instead. Only
 * `CreateTournament`, `JoinTournament` and `GetTournament` have a genuine point
 * reply.
 *
 * ## 4. `TournamentUpdate` is both a point reply and a broadcast
 *
 * It is emitted `ToSelf` from exactly one place — `handle_get_tournament`
 * (`broker.rs:1177`) — and `ToSubscribers` from `handle_join_tournament`
 * (`:1168`), `handle_start_tournament_round` (`:1206`),
 * `handle_report_match_result` (`:1268`), `handle_drop_from_tournament`
 * (`:1309`), `handle_end_tournament` (`:1334`) and `reap_expired`'s `Abandoned`
 * arm (`:550-555`). The frames are byte-identical in shape; the wire carries no
 * request-vs-broadcast discriminator beyond `code`.
 *
 * Consequence, stated rather than worked around: while one of the four gated
 * helpers is in flight, **any** other actor's action on the same tournament —
 * including this same caller's own concurrent {@link getTournamentOver}, whose
 * `ToSelf` reply is wire-identical to a foreign broadcast — produces a frame
 * that matches our filter and settles our promise `{ok: true}` with *that*
 * view, ahead of our own outcome. A later `Error` for our actually-rejected
 * request then arrives with no listener left and is dropped. Every candidate
 * client-side fix (a sequence number the broker does not echo, timing
 * heuristics, view diffing) would fabricate provenance the broker never sent and
 * be silently wrong instead of documented.
 *
 * Callers that need to know whether their own mutation landed must read the
 * ambient subscription's view, not this promise's payload, and must not treat
 * `{ok: false}` from a gated helper as a complete rejection detector.
 *
 * ## 5. The `Error` reply carries no correlator
 *
 * `LobbyServerMessage::Error { message, code? }`
 * (`crates/lobby-broker/src/protocol.rs:769-773`) has no tournament code — its
 * optional `code` is a `ServerErrorCode` *class*, not a tournament code, and is
 * usually absent. An `Error` frame therefore settles every RPC in flight on this
 * socket, exactly as `resolveGuestOver` already behaves for the same reason.
 * The server's message is passed through raw; classifying or translating it is
 * the caller's job.
 */

/** Wait cap before an unanswered request settles `"timeout"`. */
const DEFAULT_TIMEOUT_MS = 10_000;

/**
 * Why a tournament RPC did not produce a value. A closed union rather than a
 * boolean pair or a bare string: each member is a distinct thing a caller can
 * do something different about.
 *
 * - `rejected` — the broker answered `Error`; `message` is its text verbatim.
 * - `aborted` — the caller's `AbortSignal` fired (or was already aborted).
 * - `timeout` — no matching frame arrived within `timeoutMs`.
 * - `connection_lost` — the socket was not open, or closed while in flight.
 */
export type TournamentRpcFailureReason =
  | "rejected"
  | "aborted"
  | "timeout"
  | "connection_lost";

export type TournamentRpcResult<T> =
  | { ok: true; value: T }
  | { ok: false; reason: TournamentRpcFailureReason; message: string };

export interface TournamentRequestOptions {
  /**
   * Aborts the in-flight request. Registration of the controller (so a
   * reconnect or teardown can fire it) belongs to the socket's owner.
   */
  signal?: AbortSignal;
  /**
   * Wait cap in ms; defaults to {@link DEFAULT_TIMEOUT_MS}. `Infinity` is
   * supported for callers explicitly opting out of the cap.
   */
  timeoutMs?: number;
}

/** The minimum shape every inbound frame shares at the trust boundary. */
interface InboundFrame {
  type: string;
  data?: unknown;
}

/**
 * Decides whether an inbound frame is the reply this request is waiting for,
 * returning its payload when it is and `null` when it is not.
 */
type ReplyMatcher<T> = (msg: InboundFrame) => T | null;

/**
 * The single authority for putting a tournament frame on a borrowed socket and
 * awaiting its reply. Generalizes `resolveGuestOver`'s structure — readyState
 * guard, abort pre-guard, correlated message listener, close listener, abort
 * listener, timeout timer, one `cleanup()`, listeners attached before the frame
 * goes out — so all seven helpers share one lifetime implementation instead of
 * seven copies that can drift.
 *
 * Terminal paths are mutually exclusive and total: a matched reply, an `Error`
 * frame, an abort, a timeout, or the socket dropping. `cleanup()` runs exactly
 * once, on whichever of those fired, and removes the registrations that path
 * left live. The borrowed socket's own lifetime is untouched on every one of
 * them — shutting it down is its owner's call, never this module's.
 */
export function requestOver<T>(
  socket: PhaseSocket,
  frame: unknown,
  match: ReplyMatcher<T>,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<T>> {
  const { ws } = socket;
  const { signal, timeoutMs = DEFAULT_TIMEOUT_MS } = opts;

  return new Promise<TournamentRpcResult<T>>((resolve) => {
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
        reason: "aborted",
        message: "Tournament request aborted before start",
      });
      return;
    }

    const listener = (event: MessageEvent) => {
      // Trust-boundary parse-then-dispatch, the same shape every other
      // borrowed-socket listener in `brokerClient.ts` uses: a frame this
      // module cannot parse is ignored rather than thrown out of a listener
      // no caller can catch.
      let msg: InboundFrame;
      try {
        msg = JSON.parse(event.data as string) as InboundFrame;
      } catch {
        return;
      }

      const matched = match(msg);
      if (matched !== null) {
        cleanup();
        resolve({ ok: true, value: matched });
        return;
      }

      // Unfiltered by design — see the module header, part 5.
      if (msg.type === "Error") {
        const data = (msg.data ?? {}) as { message?: string };
        cleanup();
        resolve({
          ok: false,
          reason: "rejected",
          message: data.message ?? "The tournament server rejected the request",
        });
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
        reason: "aborted",
        message: "Tournament request aborted",
      });
    };

    const timer =
      Number.isFinite(timeoutMs) && timeoutMs > 0
        ? setTimeout(() => {
            cleanup();
            resolve({
              ok: false,
              reason: "timeout",
              message: `No response from the tournament server within ${timeoutMs}ms`,
            });
          }, timeoutMs)
        : null;

    const cleanup = () => {
      if (timer !== null) clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      ws.removeEventListener("message", listener);
      ws.removeEventListener("close", closeListener);
    };

    // Attach before sending: a synchronous transport could otherwise deliver
    // the reply before this request is listening for it.
    signal?.addEventListener("abort", onAbort, { once: true });
    ws.addEventListener("message", listener);
    ws.addEventListener("close", closeListener, { once: true });

    ws.send(JSON.stringify(frame));
  });
}

/**
 * Builds the reply filter for one request: the frame's tag must match, and —
 * for every reply that carries a tournament code — the code must be ours.
 *
 * Pass `code: null` only for `TournamentCreated`, whose code the broker mints
 * in the reply itself (`broker.rs:1096`), so there is nothing to correlate on
 * at send time. The cost is documented and narrow: two concurrent creates on
 * one socket cannot be told apart.
 *
 * The `code` conjunct discriminates **tournaments, not requests**. For
 * `"TournamentUpdate"` that distinction matters — see the module header,
 * part 4: a same-code frame produced by someone else's action passes this
 * filter, because on the wire it is the same frame.
 *
 * Both fields every tournament reply carries — `code` and `view` — are
 * *presence*-checked, not merely cast. This is the same trust-boundary rule
 * {@link subscribeTournamentsOver}'s listener applies to the very same
 * `TournamentUpdate` frames, and the two must agree: a payload missing its
 * `view` is not a reply, and settling a caller `{ok: true}` on one would hand
 * out a value the type says is there and the wire did not send. Note this is
 * strictly a well-formedness check and nothing more — it cannot and does not
 * address part 4's provenance limitation, which is about a perfectly
 * well-formed frame belonging to someone else's action.
 */
function matchReply<T extends { code: string; view: TournamentView }>(
  replyType: string,
  code: string | null,
): ReplyMatcher<T> {
  return (msg) => {
    if (msg.type !== replyType) return null;
    // Read as optional-everything: at this boundary `T` is what the frame is
    // claimed to be, not what has been established about it yet.
    const data = msg.data as Partial<T> | undefined | null;
    if (data == null) return null;
    if (data.code == null || data.view == null) return null;
    // `code === null` is `TournamentCreated`, whose code the broker mints in
    // the reply — nothing to correlate against, but the presence check above
    // still applies, since that minted code is the caller's only route to the
    // tournament it just created.
    if (code !== null && data.code !== code) return null;
    return data as T;
  };
}

/**
 * The shape a client chooses at creation. `totalRounds` omitted or `null` uses
 * the broker's bracket- and arity-selected default.
 *
 * No combination is pre-rejected here — notably `SingleElimination` with an
 * arity other than 2, which the broker refuses. Legality is the broker's
 * authority; duplicating it client-side would be a second, drifting copy of a
 * rule the server already owns.
 */
export interface CreateTournamentRequest {
  name: string;
  arity: MatchArity;
  scoring: ScoringPolicy;
  bracket: BracketShape;
  totalRounds?: number | null;
}

/** `CreateTournament` → `TournamentCreated` (point reply, carries the token). */
export function createTournamentOver(
  socket: PhaseSocket,
  req: CreateTournamentRequest,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentCreatedReply>> {
  return requestOver<TournamentCreatedReply>(
    socket,
    {
      type: "CreateTournament",
      data: {
        name: req.name,
        arity: req.arity,
        scoring: req.scoring,
        bracket: req.bracket,
        total_rounds: req.totalRounds ?? null,
      },
    },
    matchReply<TournamentCreatedReply>("TournamentCreated", null),
    opts,
  );
}

/**
 * `JoinTournament` → `TournamentJoined` (point reply, carries this entrant's
 * token). `playerKey` is client-supplied and opaque to the broker — it is the
 * stable per-entrant identity every later view keys on.
 */
export function joinTournamentOver(
  socket: PhaseSocket,
  code: string,
  playerKey: string,
  displayName: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentJoinedReply>> {
  return requestOver<TournamentJoinedReply>(
    socket,
    {
      type: "JoinTournament",
      data: { code, player_key: playerKey, display_name: displayName },
    },
    matchReply<TournamentJoinedReply>("TournamentJoined", code),
    opts,
  );
}

/**
 * `GetTournament` → `TournamentUpdate`. Ungated: a tournament is public once
 * its code is known. This is the one helper whose reply is genuinely `ToSelf`,
 * so a racing same-code broadcast still answers the question actually asked.
 */
export function getTournamentOver(
  socket: PhaseSocket,
  code: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return requestOver<TournamentUpdateReply>(
    socket,
    { type: "GetTournament", data: { code } },
    matchReply<TournamentUpdateReply>("TournamentUpdate", code),
    opts,
  );
}

/**
 * `StartTournamentRound`, organizer-gated. No point reply exists: this settles
 * on the `TournamentUpdate` broadcast, so it is subject to the same-code
 * provenance limitation in the module header, part 4.
 */
export function startTournamentRoundOver(
  socket: PhaseSocket,
  code: string,
  organizerToken: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return requestOver<TournamentUpdateReply>(
    socket,
    {
      type: "StartTournamentRound",
      data: { code, organizer_token: organizerToken },
    },
    matchReply<TournamentUpdateReply>("TournamentUpdate", code),
    opts,
  );
}

/**
 * `ReportMatchResult`, player-gated — the token must belong to a player seated
 * in this pairing. No point reply exists: settles on the `TournamentUpdate`
 * broadcast, subject to the module header's part 4 limitation.
 */
export function reportMatchResultOver(
  socket: PhaseSocket,
  code: string,
  pairingId: PairingId,
  playerToken: string,
  outcome: PodOutcome,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return requestOver<TournamentUpdateReply>(
    socket,
    {
      type: "ReportMatchResult",
      data: {
        code,
        pairing_id: pairingId,
        player_token: playerToken,
        outcome,
      },
    },
    matchReply<TournamentUpdateReply>("TournamentUpdate", code),
    opts,
  );
}

/**
 * `DropFromTournament`, player-gated. No point reply exists: settles on the
 * `TournamentUpdate` broadcast, subject to the module header's part 4
 * limitation.
 */
export function dropFromTournamentOver(
  socket: PhaseSocket,
  code: string,
  playerToken: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return requestOver<TournamentUpdateReply>(
    socket,
    { type: "DropFromTournament", data: { code, player_token: playerToken } },
    matchReply<TournamentUpdateReply>("TournamentUpdate", code),
    opts,
  );
}

/**
 * `EndTournament`, organizer-gated. No point reply exists: settles on the
 * `TournamentUpdate` broadcast, subject to the module header's part 4
 * limitation.
 */
export function endTournamentOver(
  socket: PhaseSocket,
  code: string,
  organizerToken: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return requestOver<TournamentUpdateReply>(
    socket,
    { type: "EndTournament", data: { code, organizer_token: organizerToken } },
    matchReply<TournamentUpdateReply>("TournamentUpdate", code),
    opts,
  );
}

/**
 * Inbound tournament broadcasts. Every handler is optional so a caller can take
 * only the stream it renders.
 */
export interface TournamentSubscriptionHandlers {
  /** The broker's complete, sorted tournament list. */
  onListUpdate?: (tournaments: TournamentSummary[]) => void;
  /** One tournament's detail view changed. */
  onTournamentUpdate?: (code: string, view: TournamentView) => void;
  /** A tournament record is gone (stale registration, or past retention). */
  onTournamentRemoved?: (code: string) => void;
}

/**
 * Attaches handlers for the three tournament broadcast frames on a borrowed
 * socket, returning a detach function.
 *
 * Two deliberate departures from `subscribeLobbyOver`:
 *
 * 1. **Nothing is sent, in either direction.** Not on attach, not on detach.
 *    See the module header, part 2 — the shared `SubscribeLobby` reference
 *    count is the socket owner's, and detach here only removes this listener.
 *    The borrowed socket's lifetime is likewise untouched.
 * 2. **No derived state is held.** `TournamentListUpdate` carries the whole
 *    sorted list every time (`tournament_summaries()`,
 *    `crates/lobby-broker/src/broker.rs:261-269`) and there are no add/update
 *    delta frames to fold in, so this is a pure pass-through. Reducing the list
 *    client-side would be inventing a delta protocol the broker does not speak.
 */
export function subscribeTournamentsOver(
  socket: PhaseSocket,
  handlers: TournamentSubscriptionHandlers,
): () => void {
  const { ws } = socket;

  const listener = (event: MessageEvent) => {
    let msg: InboundFrame;
    try {
      msg = JSON.parse(event.data as string) as InboundFrame;
    } catch {
      return;
    }

    switch (msg.type) {
      case "TournamentListUpdate": {
        const data = msg.data as { tournaments?: TournamentSummary[] } | undefined | null;
        if (data?.tournaments == null) return;
        handlers.onListUpdate?.(data.tournaments);
        break;
      }
      case "TournamentUpdate": {
        const data = msg.data as
          | { code?: string; view?: TournamentView }
          | undefined
          | null;
        if (data?.code == null || data.view == null) return;
        handlers.onTournamentUpdate?.(data.code, data.view);
        break;
      }
      case "TournamentRemoved": {
        const data = msg.data as { code?: string } | undefined | null;
        if (data?.code == null) return;
        handlers.onTournamentRemoved?.(data.code);
        break;
      }
    }
  };

  ws.addEventListener("message", listener);

  return () => {
    ws.removeEventListener("message", listener);
  };
}
