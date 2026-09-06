import type {
  BracketShape,
  MatchArity,
  PairingId,
  PodOutcome,
  ScoringPolicy,
  TournamentActionAckReply,
  TournamentActionRejectedReply,
  TournamentCreatedReply,
  TournamentJoinedReply,
  TournamentSummary,
  TournamentUpdateReply,
  TournamentView,
} from "../adapter/types";
import { MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK } from "../adapter/ws-adapter";
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
 * ## 3. Four of the seven RPCs get no `TournamentUpdate` point reply
 *
 * `StartTournamentRound`, `ReportMatchResult`, `DropFromTournament` and
 * `EndTournament` still produce no `ToSelf` `TournamentUpdate` on success — for
 * these four that frame is only ever a `ToSubscribers` broadcast
 * (`crates/lobby-broker/src/broker.rs`). What they do produce, as of lobby
 * protocol 5, is a `TournamentActionAck` addressed to the requester and
 * carrying that request's own correlator, so **a correlated caller observes its
 * own success without holding a subscription**. An uncorrelated caller — a
 * pre-correlation client, or a request this module declined to correlate
 * because the peer is too old (part 4) — still sees the old behavior: no point
 * reply, and success observable only on the ambient broadcast. Only
 * `CreateTournament`, `JoinTournament` and `GetTournament` have a point reply
 * that predates the correlator.
 *
 * ## 4. `TournamentUpdate` is a broadcast; the ack is what answers a request
 *
 * `TournamentUpdate` is emitted `ToSelf` from exactly one place —
 * `handle_get_tournament` — and `ToSubscribers` from `handle_join_tournament`,
 * from all four gated handlers' settlement, and from `reap_expired`'s
 * `Abandoned` arm. The frames are byte-identical in shape; the wire carries no
 * request-vs-broadcast discriminator beyond `code`.
 *
 * That is precisely why the four gated helpers no longer settle on it. While
 * one of them was in flight, **any** other actor's action on the same
 * tournament — including this same caller's own concurrent
 * {@link getTournamentOver}, whose `ToSelf` reply is wire-identical to a
 * foreign broadcast — produced a frame that matched a tag + code filter and
 * settled the promise `{ok: true}` with *that* view, ahead of the caller's own
 * outcome; a later refusal for the request that actually failed then arrived
 * with no listener left.
 *
 * The fix is on the wire rather than here, and that is the point: every
 * candidate client-only fix (a sequence number the broker does not echo, timing
 * heuristics, view diffing) would have fabricated provenance the broker never
 * sent. The broker now sends it. Each gated helper mints a `request_id`, puts
 * it on the request, and settles on the `TournamentActionAck` /
 * `TournamentActionRejected` carrying that same id — see {@link matchAck} — so
 * a correlated `{ok: true}` is this caller's own success and a correlated
 * `{ok: false, reason: "rejected"}` is this caller's own refusal.
 *
 * Two consequences a caller must know:
 *
 * - A correlated request ignores a bare `Error` frame (part 5), and that gives
 *   up a deliberately-designed property. `reject_reply` in
 *   `lobby-worker/broker-wasm/src/lib.rs` answers a frame refused at the
 *   parse/validation boundary with an uncorrelated `Error` *specifically* so a
 *   pending RPC fails fast instead of waiting out its timeout. For these four
 *   actions that fast-fail no longer reaches the caller, which settles
 *   `"timeout"` instead. The trade is narrow rather than free: a frame that
 *   never parsed never carried our id, so no correlated answer to it can
 *   exist, and reaching that boundary at all takes a field-bounds violation on
 *   a payload the store builds from broker-minted values.
 * - Against a peer below `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK` there is no
 *   ack to wait for. The frame is still sent — the broker performs the action —
 *   and the call settles `{ok: false, reason: "unsupported"}`, which says *not
 *   confirmed*, never *failed*.
 *
 * ## 5. The `Error` reply carries no correlator
 *
 * `LobbyServerMessage::Error { message, code? }`
 * (`crates/lobby-broker/src/protocol.rs`) has no tournament code — its optional
 * `code` is a `ServerErrorCode` *class*, not a tournament code, and is usually
 * absent. An `Error` frame therefore settles every **uncorrelated** RPC in
 * flight on this socket — the three helpers that keep {@link matchReply} —
 * exactly as `resolveGuestOver` already behaves for the same reason. The
 * server's message is passed through raw; classifying or translating it is the
 * caller's job.
 *
 * The four correlated helpers deliberately do not settle on it. An uncorrelated
 * `Error` provably belongs to *some* request on this socket but not provably to
 * ours, so settling on it would be the mirror image of the bug part 4 describes
 * — a false negative in place of a false positive — and timeout is the honest
 * settlement. This is one choice with part 4's disclosed fast-fail trade, seen
 * from the other side.
 *
 * Of the three uncorrelated helpers, {@link getTournamentOver} is the one still
 * exposed to a racing same-code broadcast, and that exposure stays **benign**:
 * the question it asks is `ToSelf`-shaped, so a foreign frame answers it
 * correctly.
 */

/** Wait cap before an unanswered request settles `"timeout"`. */
const DEFAULT_TIMEOUT_MS = 10_000;

/**
 * Why a tournament RPC did not produce a value. A closed union rather than a
 * boolean pair or a bare string: each member is a distinct thing a caller can
 * do something different about.
 *
 * - `rejected` — the broker refused this request: an `Error` frame for an
 *   uncorrelated helper, or a `TournamentActionRejected` carrying **this
 *   caller's own correlator** for a gated one. `message` is its text verbatim.
 *   For the gated four this is now a reliable "the server refused *me*" signal,
 *   which before the correlator it was not.
 * - `aborted` — the caller's `AbortSignal` fired (or was already aborted).
 * - `timeout` — no matching frame arrived within `timeoutMs`.
 * - `connection_lost` — the socket was not open, or closed while in flight.
 * - `unsupported` — this peer's lobby protocol predates correlated tournament
 *   settlement, so it cannot answer a gated action at all. **The frame was
 *   sent and the broker very likely performed the action**; this client simply
 *   cannot confirm it. Never read as "the action failed".
 *
 * `unsupported` belongs to this wire union rather than to a store-level refusal
 * on the *was-anything-sent* axis: something went out, and the predicate reads
 * a **broker-advertised** fact (`ServerHello`'s `lobby_protocol_version`,
 * surfaced as `PhaseSocket.serverInfo.lobbyProtocolVersion`). That is the
 * distinction this union actually draws — whose fact the predicate reads, not
 * who evaluated it, since three of the four incumbent members are evaluated
 * client-side too. A refusal reading a fact the client itself authored belongs
 * outside; see `TournamentNotAuthorized` in `stores/multiplayerStore.ts`.
 */
export type TournamentRpcFailureReason =
  | "rejected"
  | "aborted"
  | "timeout"
  | "connection_lost"
  | "unsupported";

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
 * The correlated half of a gated request's settlement. Supplied by
 * {@link gatedRequestOver} and by nothing else — an uncorrelated request passes
 * `null` and keeps the pre-correlation behavior exactly.
 *
 * Exported only so {@link requestOver} can name it in its own signature; it is
 * an internal wiring detail, not a caller-facing option.
 */
export interface CorrelatedSettlement {
  /**
   * Matches **this** caller's own `TournamentActionRejected`, returning the
   * broker's text. A refusal carrying anyone else's correlator is not an answer
   * to this request and must leave it pending.
   */
  matchRejection: ReplyMatcher<string>;
  /**
   * This peer cannot mint an ack, so no correlated answer will ever arrive.
   * The frame still goes out and the call settles `"unsupported"` immediately
   * after the send — see {@link gatedRequestOver}.
   */
  unanswerable: boolean;
}

/**
 * The single authority for putting a tournament frame on a borrowed socket and
 * awaiting its reply. Generalizes `resolveGuestOver`'s structure — readyState
 * guard, abort pre-guard, correlated message listener, close listener, abort
 * listener, timeout timer, one `cleanup()`, listeners attached before the frame
 * goes out — so all seven helpers share one lifetime implementation instead of
 * seven copies that can drift.
 *
 * Terminal paths are mutually exclusive and total: a matched reply, a refusal
 * (this caller's own `TournamentActionRejected` when correlated, any `Error`
 * frame when not), an abort, a timeout, the socket dropping, or — for a
 * correlated request against a peer that cannot answer one — the immediate
 * `"unsupported"` settlement taken right after the send. `cleanup()` runs
 * exactly once, on whichever of those fired, and removes the registrations that
 * path left live. The borrowed socket's own lifetime is untouched on every one
 * of them — shutting it down is its owner's call, never this module's.
 */
export function requestOver<T>(
  socket: PhaseSocket,
  frame: unknown,
  match: ReplyMatcher<T>,
  opts: TournamentRequestOptions = {},
  correlation: CorrelatedSettlement | null = null,
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

      if (correlation !== null) {
        // Correlated: only this caller's OWN refusal settles it, and a bare
        // `Error` is deliberately ignored — see the module header, part 5. An
        // uncorrelated `Error` belongs to *some* request on this socket but not
        // provably to ours, so settling on it would be a false negative in
        // place of the false positive this whole change removes.
        const refusal = correlation.matchRejection(msg);
        if (refusal !== null) {
          cleanup();
          resolve({ ok: false, reason: "rejected", message: refusal });
        }
        return;
      }

      // Unfiltered by design, and only for an UNCORRELATED request — see the
      // module header, part 5.
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

    // The frame goes out first, deliberately: this peer performs the action, it
    // just cannot confirm it. Refusing to send would break a working feature
    // during version skew; falling back to the tag + code matcher would
    // reintroduce the exact bug this module was changed to remove. Settling now
    // rather than waiting out a timeout is the honest report of what is known.
    if (correlation?.unanswerable === true) {
      cleanup();
      resolve({
        ok: false,
        reason: "unsupported",
        message:
          "This lobby server is too old to confirm tournament actions; the request was sent but its outcome is unknown",
      });
    }
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
 * filter, because on the wire it is the same frame. That is why the four gated
 * helpers use {@link matchAck} instead; this matcher belongs to the three whose
 * replies are genuinely point replies.
 *
 * Both fields every tournament reply carries — `code` and `view` — are
 * *presence*-checked, not merely cast. This is the same trust-boundary rule
 * {@link subscribeTournamentsOver}'s listener applies to the very same
 * `TournamentUpdate` frames, and the two must agree: a payload missing its
 * `view` is not a reply, and settling a caller `{ok: true}` on one would hand
 * out a value the type says is there and the wire did not send. Note this is
 * strictly a well-formedness check and nothing more — it cannot and does not
 * address part 4's provenance limitation, which is about a perfectly
 * well-formed frame belonging to someone else's action. {@link matchAck} is
 * what addresses that, for the four helpers exposed to it.
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
 * The next correlator this module will mint.
 *
 * Module-scoped rather than per-socket: the wire only needs uniqueness among
 * the requests in flight on one socket, and a monotonic module counter is
 * strictly stronger — it also survives a reconnect, so a late ack for a request
 * the previous socket never answered cannot collide with a fresh one.
 *
 * Client-minted, following `PreviewManaPayment`: a server-assigned id would
 * need its own round trip to deliver, which is the problem being solved. The
 * wire type is a `u64` and JS integers are exact to 2^53, so at one increment
 * per organizer action the ceiling is unreachable.
 */
let nextRequestId = 1;

/**
 * The reply filter for a correlated gated action. Unlike {@link matchReply},
 * whose conjuncts are tag + tournament code, this one binds on the correlator
 * the caller minted, so an ambient `TournamentUpdate` for the same tournament —
 * from another participant, from this caller's own concurrent
 * {@link getTournamentOver}, or from the reaper — cannot match it at all.
 *
 * The `code` / `view` presence checks {@link matchReply} applies still apply
 * here, and for the same trust-boundary reason: a correlator establishes *whose
 * answer this is*, never that the answer is well-formed.
 */
function matchAck(requestId: number): ReplyMatcher<TournamentUpdateReply> {
  return (msg) => {
    if (msg.type !== "TournamentActionAck") return null;
    // Read as optional-everything, exactly as `matchReply` does: at this
    // boundary the frame is what it claims to be, not what has been
    // established about it.
    const data = msg.data as
      | Partial<TournamentActionAckReply>
      | undefined
      | null;
    if (data == null) return null;
    if (data.request_id !== requestId) return null;
    if (data.code == null || data.view == null) return null;
    return { code: data.code, view: data.view };
  };
}

/**
 * The refusal filter for a correlated gated action: **this** caller's own
 * `TournamentActionRejected` and no other's, returning the broker's text
 * verbatim. The fallback matches the one the uncorrelated `Error` branch of
 * {@link requestOver} uses for a text-less frame, so the two refusal paths
 * cannot disagree about what a message-less refusal reads as.
 */
function matchRejection(requestId: number): ReplyMatcher<string> {
  return (msg) => {
    if (msg.type !== "TournamentActionRejected") return null;
    const data = msg.data as
      | Partial<TournamentActionRejectedReply>
      | undefined
      | null;
    if (data == null) return null;
    if (data.request_id !== requestId) return null;
    return data.message ?? "The tournament server rejected the request";
  };
}

/**
 * The single authority for the four token-gated tournament actions: mint a
 * correlator, put it on the wire with the request, and settle on the broker's
 * answer to **that** request. The client half of `Broker::settle_gated`.
 *
 * Every gated helper routes through here, so no one of them can quietly
 * reintroduce tag + code settlement for itself.
 *
 * The capability gate reads a **floor**, never the current version.
 * `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK` is frozen at the version that
 * introduced correlated settlement, so a v6 or v7 broker — which still mints
 * the ack — is admitted. Writing the threshold as `LOBBY_PROTOCOL_VERSION`
 * instead would look identical today and, at the next lobby bump, silently
 * refuse every fully-compatible server and disable all four organizer actions.
 *
 * An absent `lobbyProtocolVersion` counts as unsupported. That deliberately
 * diverges from `adapter/ws-adapter.ts`'s compatibility check, which tolerates
 * an absent version — correctly, because it asks whether a lobby session may be
 * held at all, and refusing one would evict hosting, browsing and joining. This
 * gate asks whether *this specific peer* can mint an ack; a peer that never
 * advertised a lobby version predates the version that introduced one, so the
 * answer is no. Different question, different default: do not harmonize them.
 */
function gatedRequestOver(
  socket: PhaseSocket,
  type: string,
  data: Record<string, unknown>,
  opts: TournamentRequestOptions,
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  const requestId = nextRequestId++;
  const lobbyProtocolVersion = socket.serverInfo.lobbyProtocolVersion;
  const unanswerable =
    lobbyProtocolVersion === undefined ||
    lobbyProtocolVersion < MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK;

  return requestOver<TournamentUpdateReply>(
    socket,
    { type, data: { ...data, request_id: requestId } },
    matchAck(requestId),
    opts,
    { matchRejection: matchRejection(requestId), unanswerable },
  );
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
 * `StartTournamentRound`, organizer-gated. Correlated: this settles on the
 * broker's `TournamentActionAck` / `TournamentActionRejected` for **this exact
 * request**, so it is no longer subject to the same-code provenance limitation
 * in the module header, part 4. Against a peer below
 * `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK` the frame is still sent and the call
 * settles `"unsupported"` — *not confirmed*, never *failed*.
 */
export function startTournamentRoundOver(
  socket: PhaseSocket,
  code: string,
  organizerToken: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return gatedRequestOver(
    socket,
    "StartTournamentRound",
    { code, organizer_token: organizerToken },
    opts,
  );
}

/**
 * `ReportMatchResult`, player-gated — the token must belong to a player seated
 * in this pairing. Correlated: settles on the broker's ack or refusal for this
 * exact request, so it is no longer subject to the module header's part 4
 * limitation. Against a peer below `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK` the
 * frame is still sent and the call settles `"unsupported"` — *not confirmed*,
 * never *failed*.
 */
export function reportMatchResultOver(
  socket: PhaseSocket,
  code: string,
  pairingId: PairingId,
  playerToken: string,
  outcome: PodOutcome,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return gatedRequestOver(
    socket,
    "ReportMatchResult",
    {
      code,
      pairing_id: pairingId,
      player_token: playerToken,
      outcome,
    },
    opts,
  );
}

/**
 * `DropFromTournament`, player-gated. Correlated: settles on the broker's ack or
 * refusal for this exact request, so it is no longer subject to the module
 * header's part 4 limitation. Against a peer below
 * `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK` the frame is still sent and the call
 * settles `"unsupported"` — *not confirmed*, never *failed*.
 */
export function dropFromTournamentOver(
  socket: PhaseSocket,
  code: string,
  playerToken: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return gatedRequestOver(
    socket,
    "DropFromTournament",
    { code, player_token: playerToken },
    opts,
  );
}

/**
 * `EndTournament`, organizer-gated. Correlated: settles on the broker's ack or
 * refusal for this exact request, so it is no longer subject to the module
 * header's part 4 limitation. Against a peer below
 * `MIN_LOBBY_PROTOCOL_FOR_TOURNAMENT_ACK` the frame is still sent and the call
 * settles `"unsupported"` — *not confirmed*, never *failed*.
 */
export function endTournamentOver(
  socket: PhaseSocket,
  code: string,
  organizerToken: string,
  opts: TournamentRequestOptions = {},
): Promise<TournamentRpcResult<TournamentUpdateReply>> {
  return gatedRequestOver(
    socket,
    "EndTournament",
    { code, organizer_token: organizerToken },
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
