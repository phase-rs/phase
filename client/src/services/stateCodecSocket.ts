/**
 * Negotiated StateCodecV1 delivery for server WebSocket transports.
 *
 * The wrapper sits outside the optional gzip envelope (raw → gzip → codec), so
 * it sees exactly the UTF-8 JSON text the snapshot transport would have seen.
 * It:
 *  - consumes the second, plain-text `ServerHello` that confirms the codec;
 *  - routes only the reserved top-level `Full` / `Delta` frame tags through the
 *    decoder session and emits the decoded `ServerMessage` text unchanged;
 *  - passes every other frame through untouched and in wire order;
 *  - requests `ResyncState` on the first rejected frame and suppresses deltas
 *    until the application commits a Full, with a bounded recovery window;
 *  - bounds queued expanded text behind one active frame and closes on overload
 *    without dropping deltas or waiting for the active decode to settle;
 *  - records natural transport death immediately, then gives already-received
 *    healthy frames at most five seconds to drain before closing consumers;
 *  - never interprets, diffs or patches state. Rust owns reconstruction.
 *
 * Ownership: the wrapper owns the prepared decoder session lease and releases
 * it exactly once, on any terminal path.
 */
import i18n from "i18next";
import type { StateCodecDecodeResult, StateCodecSession } from "../adapter/state-codec-runtime";
import { AdapterError, AdapterErrorCode } from "../adapter/types";
import type { PlayerId } from "../adapter/types";
import type { FullSessionKey } from "./multiplayerSession";
import type { PhaseSocketTransport } from "./openPhaseSocket";

/** The only codec this client advertises. */
export const STATE_CODEC_V1 = "StateCodecV1";
export type StateCodecName = typeof STATE_CODEC_V1;

/** What the owning adapter says about the current attachment's bootstrap. */
export type StateCodecBootstrap = "pending" | "accepted";

/** Recovery progress the wrapper reports to the owning adapter. */
export type StateCodecRecoveryState = "recovering" | "ready";

/**
 * Why a negotiated codec attachment ended in a codec-specific fatal close. The
 * closed list of transport-level triggers; ordinary network loss, local close,
 * identity-policy closes and absent capability are none of these.
 */
export type StateCodecFailureReason =
  /** The decoder rejected the RPC or threw (lifecycle error, trap, timeout). */
  | "decodeFailure"
  /** Expanded incoming text exceeded the outstanding receive budget. */
  | "receiveOverload"
  /** Missing or contradictory selection, or a codec frame before confirmation. */
  | "confirmationFailure"
  /** A frame was rejected before the application accepted a bootstrap. */
  | "bootstrapRejection"
  /** The recovery Full did not arrive within the recovery window. */
  | "recoveryTimeout"
  /** The recovery Full was itself rejected. */
  | "recoveryFullRejection"
  /** A second established-game recovery completed on the same wrapper. */
  | "repeatedRecovery";

/**
 * The three join credentials a GameStarted `Full` frame carries. Provisional
 * until the owning adapter has authenticated them with a plain acceptance; a
 * shape check is not authentication.
 */
export interface StateCodecJoinCredentials {
  readonly playerToken: string;
  readonly yourPlayer: PlayerId;
  readonly fullKey: FullSessionKey;
}

/**
 * Terminal notification: published once, before the close notification.
 *
 * `joinCredentials` is non-null only for a post-adoption failure whose exact
 * failing frame was a credential-bearing GameStarted `Full` received while the
 * bootstrap was pending: a decode rejection, a bootstrap rejection, a receive
 * overload, or a `Full` that preceded the codec confirmation. Every other failure carries
 * `null`, and a notice recorded before its socket was adopted is stripped to
 * `null` (the setup frame has not been sent, so a conforming join cannot have
 * received credentials yet).
 */
export interface StateCodecFailure {
  readonly kind: "failed";
  readonly reason: StateCodecFailureReason;
  readonly joinCredentials: StateCodecJoinCredentials | null;
}

/**
 * Ordered notification from the wrapper to the owning adapter: recovery
 * progress, or the terminal `failed` outcome of the attachment.
 */
export type StateCodecRecoveryNotice = StateCodecRecoveryState | StateCodecFailure;

/**
 * Per-adapter delivery policy. `snapshotOnly` is sticky for the lifetime of
 * the owning adapter: a codec failure never re-enables negotiation on the same
 * adapter, while a fresh adapter starts at `negotiate`.
 */
export type StateCodecPolicy =
  | { readonly kind: "negotiate" }
  | { readonly kind: "snapshotOnly"; readonly reason: StateCodecFailureReason };

/**
 * One attachment's bootstrap authority, shared by the owning adapters. `socket`
 * is `null` until the attachment's socket is adopted; `status` becomes
 * `accepted` only after a genuine GameStarted passes the adapter's guards.
 * `failure` is the complete first terminal notice this attachment reported
 * (first wins); it is captured even before adoption so an early failure is not
 * lost.
 */
export interface StateCodecBootstrapBinding {
  socket: PhaseSocketTransport | null;
  status: StateCodecBootstrap;
  failure: StateCodecFailure | null;
}

/**
 * Adapter-side authority for the codec attachments of one owning adapter.
 *
 * Both server adapters (normal play and draft) need the same three things from
 * the wrapper's callbacks: which attachment's bootstrap is current, whether a
 * recovery is in progress, and the refusal of submissions while it is. This
 * class owns that state; each adapter keeps only what differs, namely how its
 * pending submission slot is dropped and rejected (passed to
 * {@link handleRecovery}) and which socket it currently treats as live.
 *
 * Every method takes the socket and disposal state from the caller instead of
 * capturing them, so a retired attachment's callbacks are fenced by identity at
 * the moment they run.
 */
export class StateCodecAttachment {
  /** Bootstrap authority for the adopted socket; `null` before the first adoption. */
  private current: StateCodecBootstrapBinding | null = null;
  /** `null` while delivery is healthy; never holds `"ready"`. */
  private recovery: "recovering" | null = null;
  /**
   * Delivery policy of the owning adapter. The first qualifying failure of the
   * current attachment wins. Binding begin/adopt/reset/accept, recovery
   * completion, healthy delivery, a new match and reconnect never reset it;
   * only {@link release} (adapter disposal) does. No clock, global or user
   * toggle re-enables negotiation.
   */
  private policy: StateCodecPolicy = { kind: "negotiate" };

  /** Why this adapter stopped negotiating, or `negotiate`. */
  get deliveryPolicy(): StateCodecPolicy {
    return this.policy;
  }

  /**
   * The provider an attachment may use right now: the configured one while
   * negotiating, `null` (snapshot-only, no preparation, no await) once
   * degraded. `undefined`/default/`null` configuration is the caller's.
   */
  selectProvider<P>(configured: P | null): P | null {
    return this.policy.kind === "negotiate" ? configured : null;
  }

  /**
   * Post-preparation lease selection. A lease obtained while the policy was
   * still `negotiate` but that arrives after the adapter degraded is closed and
   * replaced by `null`, so the attachment opens as snapshot-only.
   */
  selectLease(session: StateCodecSession | null): StateCodecSession | null {
    if (session === null || this.policy.kind === "negotiate") return session;
    session.close();
    return null;
  }

  /**
   * Fresh, still-pending authority for one attach. It is not current until
   * {@link adopt}: until then a still-live older socket keeps its own binding
   * and recovery gate.
   */
  begin(): StateCodecBootstrapBinding {
    return { socket: null, status: "pending", failure: null };
  }

  /**
   * The attachment's socket is adopted: retire the previous binding and its
   * recovery gate. A failure the attachment reported before adoption is
   * consumed here, ahead of any server-hello emission or close consumer.
   */
  adopt(binding: StateCodecBootstrapBinding, socket: PhaseSocketTransport): void {
    binding.socket = socket;
    this.current = binding;
    this.recovery = null;
    if (binding.failure !== null) this.degrade(binding.failure.reason);
  }

  /** Bootstrap status reported to the codec transport for `binding`. */
  status(
    binding: StateCodecBootstrapBinding,
    currentSocket: PhaseSocketTransport | null,
    disposed: boolean,
  ): StateCodecBootstrap {
    // A retired socket's binding never authorizes a replacement.
    if (disposed || this.current !== binding) return "pending";
    if (binding.socket !== null && currentSocket !== binding.socket) return "pending";
    return binding.status;
  }

  /** A genuine GameStarted was accepted on `socket`, the current binding's socket. */
  accept(socket: PhaseSocketTransport | null): void {
    if (this.current === null || this.current.socket !== socket) return;
    this.current.status = "accepted";
  }

  /** A new bootstrap (e.g. a new draft match) began on `socket`: only a fresh GameStarted completes it. */
  reset(socket: PhaseSocketTransport | null): void {
    if (this.current === null || this.current.socket !== socket) return;
    this.current.status = "pending";
  }

  /**
   * Codec notification for `binding`. On `"recovering"` the gate is set and the
   * pending command is interrupted BEFORE any listener can run: the
   * interruption emits synchronously and a listener may submit, which must
   * already be refused. `interruptPending` drops the adapter's submission slot
   * and, if it held a command, rejects it with the supplied retryable error.
   *
   * A terminal `failed` notice is recorded on `binding` (the first notice wins)
   * even when `binding` is not yet adopted; a notice recorded before adoption
   * has its join credentials replaced by `null`. It degrades the adapter's policy
   * immediately only when `binding` is current and owns `currentSocket`: a
   * retired or still-pending attachment never poisons its replacement. It does
   * not touch the recovery gate, which the close that follows clears.
   */
  handleRecovery(
    binding: StateCodecBootstrapBinding,
    state: StateCodecRecoveryNotice,
    currentSocket: PhaseSocketTransport | null,
    disposed: boolean,
    interruptPending: (error: AdapterError) => void,
  ): void {
    if (disposed) return;
    if (typeof state === "object") {
      // Before adoption the setup frame has not been sent, so no conforming
      // server could have issued credentials to this attachment yet.
      binding.failure ??= binding.socket === null ? { ...state, joinCredentials: null } : state;
      if (this.current === binding && binding.socket !== null && currentSocket === binding.socket) {
        this.degrade(state.reason);
      }
      return;
    }
    if (this.current !== binding) return;
    if (binding.socket === null || currentSocket !== binding.socket) return;
    if (state === "recovering") {
      this.recovery = state;
      interruptPending(new AdapterError("WS_CLOSED", i18n.t("stateCodec.actionInterrupted"), true));
      return;
    }
    this.recovery = null;
  }

  /**
   * While codec state is being resynced, a new command would be sent into a
   * stream whose completions are suppressed. Refuse it before it is stamped.
   */
  assertNotRecovering(): void {
    if (this.recovery !== null) {
      throw new AdapterError(
        AdapterErrorCode.ACTION_NOT_SENT,
        i18n.t("stateCodec.submissionNotSent"),
        true,
      );
    }
  }

  /** The current attachment's socket is gone: a retired transport can no longer gate submissions. */
  clearRecovery(): void {
    this.recovery = null;
  }

  /** The owning adapter is disposed: no binding, recovery or policy state survives. */
  release(): void {
    this.recovery = null;
    this.current = null;
    this.policy = { kind: "negotiate" };
  }

  private degrade(reason: StateCodecFailureReason): void {
    if (this.policy.kind === "negotiate") this.policy = { kind: "snapshotOnly", reason };
  }
}

/** Bounded optional preparation budget (event-loop time, not network). */
export const STATE_CODEC_PREPARATION_TIMEOUT_MS = 5_000;
/**
 * Default recovery window: how long a requested recovery Full may take.
 * `openPhaseSocket` passes its handshake timeout instead (also 5 s by default).
 */
export const STATE_CODEC_SOCKET_TIMEOUT_MS = 5_000;
/**
 * Natural transport death permits one bounded, ordered drain of frames already
 * received. Independent of decoder RPC/recovery timeouts: a hung decode or a
 * missing consumer must release its lease and notify close within 5 s of
 * event-loop time, rather than waiting for the decoder's watchdog.
 */
const STATE_CODEC_NATURAL_CLOSE_DRAIN_TIMEOUT_MS = 5_000;

export type StateCodecSessionProvider = () => Promise<StateCodecSession | null>;

export class StateCodecPreparationAbortError extends Error {
  constructor() {
    super("State codec preparation aborted");
    this.name = "StateCodecPreparationAbortError";
  }
}

/**
 * Bounded, optional decoder preparation.
 *
 * The whole provider operation (engine bring-up, fallback, session open) races
 * the preparation deadline and the attachment's AbortSignal. Synchronous
 * throw, rejection and expiry all select snapshot-only delivery (`null`); an
 * abort rejects promptly so the caller can abandon the attachment before it
 * opens a socket. A session that arrives after expiry or abort is closed
 * exactly once, and a late rejection is consumed.
 */
export function prepareStateCodecSession(
  provider: StateCodecSessionProvider,
  signal?: AbortSignal,
  timeoutMs: number = STATE_CODEC_PREPARATION_TIMEOUT_MS,
): Promise<StateCodecSession | null> {
  if (signal?.aborted) return Promise.reject(new StateCodecPreparationAbortError());
  return new Promise<StateCodecSession | null>((resolve, reject) => {
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const onAbort = () => settle(() => reject(new StateCodecPreparationAbortError()));
    const settle = (finish: () => void) => {
      if (settled) return;
      settled = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      signal?.removeEventListener("abort", onAbort);
      finish();
    };

    timer = setTimeout(() => settle(() => resolve(null)), timeoutMs);
    signal?.addEventListener("abort", onAbort, { once: true });

    let pending: Promise<StateCodecSession | null>;
    try {
      pending = Promise.resolve(provider());
    } catch {
      settle(() => resolve(null));
      return;
    }
    void pending.then(
      (session) => {
        if (settled) {
          // Expired or aborted while the provider was still working: the late
          // lease must not leak, and it can never be advertised.
          session?.close();
          return;
        }
        settle(() => resolve(session));
      },
      () => {
        if (settled) return;
        settle(() => resolve(null));
      },
    ).catch(() => undefined); // Consume a late lease's cleanup failure too.
  });
}

type MessageListener = (event: MessageEvent<string>) => void;
type CloseListener = (event: CloseEvent) => void;

type ConnectionState = "awaitingConfirmation" | "ready" | "awaitingFull" | "closed";

/** When a terminating transport tells its consumers about the close. */
type CloseNotification = "immediate" | "deferred";

type Outbound =
  | { kind: "message"; event: MessageEvent<string> }
  | { kind: "close"; event: CloseEvent }
  | { kind: "recovery"; state: StateCodecRecoveryNotice };

/**
 * Established-game recovery cycles one wrapper tolerates. The first completed
 * recovery restores delivery; the second is delivered and then closes as
 * `repeatedRecovery`. Two is the smallest bound that tolerates one genuine
 * corruption while ending an always-rejected-Delta / valid-Full loop. Healthy
 * Deltas, ordinary Fulls, elapsed time and a new match never reset the count;
 * only a new wrapper starts at zero.
 */
const MAX_COMPLETED_RECOVERIES = 2;

/**
 * Bound expanded text queued behind the active receive. The 64-message budget
 * includes that active frame, whose supported size is independent of backlog.
 * Queued text has a separate 16 MiB UTF-16 budget; no individual-frame cap.
 */
const MAX_OUTSTANDING_MESSAGES = 64;
const MAX_QUEUED_TEXT_BYTES = 16 * 1024 * 1024;

export interface StateCodecSocketOptions {
  /** Undecorated transport: death must not wait for an inflater's delivery queue. */
  raw: PhaseSocketTransport;
  inner: PhaseSocketTransport;
  session: StateCodecSession;
  /**
   * Consulted on rejection and after delivering a recovery Full. The bootstrap
   * must still be accepted, and the primary onmessage handler must acknowledge
   * committing that specific Full before recovery can finish.
   */
  bootstrap: () => StateCodecBootstrap;
  /**
   * Recovery progress and the terminal outcome. `failed` is published at most
   * once, after the transport is invalidated and its lease released and before
   * the close notification, and never after a local, natural or identity close.
   */
  onRecovery?: (state: StateCodecRecoveryNotice) => void;
  /** Actual send accounting for an uncompressed inner transport. */
  onFrameSent?: () => void;
  recoveryTimeoutMs?: number;
}

export class StateCodecSocket implements PhaseSocketTransport {
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  private closeHandler: CloseListener | null = null;

  private handler: PhaseSocketTransport["onmessage"] = null;
  private readonly messageListeners = new Map<MessageListener, boolean>();
  private readonly closeListeners = new Map<CloseListener, boolean>();
  private readonly queue: (() => Promise<void> | void)[] = [];
  private draining = false;
  private outstandingMessages = 0;
  private queuedTextBytes = 0;
  private state: ConnectionState = "awaitingConfirmation";
  private recoveryTimer: ReturnType<typeof setTimeout> | null = null;
  /** First terminal cause, latched before the ordered close can run. */
  private naturalClose: CloseEvent | null = null;
  private naturalCloseTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly rawCloseListener: CloseListener = (event) => this.latchNaturalClose(event);
  private sessionReleased = false;
  private readonly consumerWaiters: (() => void)[] = [];
  private pendingClose: Extract<Outbound, { kind: "close" }> | null = null;
  private completedRecoveries = 0;
  private readonly encoder = new TextEncoder();

  constructor(private readonly options: StateCodecSocketOptions) {
    const { inner, raw } = options;
    // Capture first: an existing raw observer can synchronously dispose us.
    raw.addEventListener("close", this.rawCloseListener, { capture: true, once: true });
    inner.onopen = (event) => {
      try {
        this.onopen?.(event);
      } catch {
        // A throwing open handler must not break the event routing.
      }
    };
    // An error is ordered behind every frame this wrapper has already received
    // and ahead of the natural close that follows it. It is only a
    // notification: it never fails, degrades or closes the codec attachment.
    inner.onerror = (event) => {
      if (this.naturalClose !== null) return;
      this.enqueue(() => this.deliverError(event));
    };
    inner.onmessage = (event) => this.enqueueMessage(event.data as string);
    inner.onclose = (event) => this.recordNaturalClose(event);
  }

  get readyState(): number {
    return this.closed || this.naturalClose !== null ? WebSocket.CLOSED : this.options.inner.readyState;
  }

  /**
   * Read through a method so control-flow narrowing of `this.state` from an
   * earlier assignment cannot make a later terminal check unreachable to the
   * compiler (listeners and timers can terminate between the two points).
   */
  private get closed(): boolean {
    return this.state === "closed";
  }

  get onclose(): CloseListener | null {
    return this.closeHandler;
  }

  set onclose(listener: CloseListener | null) {
    this.closeHandler = listener;
    this.flushClose();
  }

  get onmessage(): PhaseSocketTransport["onmessage"] {
    return this.handler;
  }

  set onmessage(listener: PhaseSocketTransport["onmessage"]) {
    this.handler = listener;
    if (listener) this.wakeConsumers();
  }

  send(data: string): void {
    if (this.closed || this.naturalClose !== null) return;
    this.options.inner.send(data);
    this.options.onFrameSent?.();
  }

  close(): void {
    // A local-first close invalidates and releases even during an outstanding
    // decode. Its notification is posted on a microtask, as native and gzip
    // sockets close asynchronously, so dispose/setup failure can settle their
    // own reason first. If network death arrived earlier, finish that original
    // natural close instead of replacing its cause or closing the inner twice.
    // One close notification is retained for a consumer that has not attached
    // yet; the later native close cannot notify again.
    this.terminate("deferred");
  }

  addEventListener(
    type: "close",
    listener: CloseListener,
    options?: AddEventListenerOptions | boolean,
  ): void;
  addEventListener(
    type: "message",
    listener: MessageListener,
    options?: AddEventListenerOptions | boolean,
  ): void;
  addEventListener(
    type: "close" | "message",
    listener: CloseListener | MessageListener,
    options?: AddEventListenerOptions | boolean,
  ): void {
    const once = typeof options === "object" && options.once === true;
    if (type === "message") {
      this.messageListeners.set(listener as MessageListener, once);
      this.wakeConsumers();
    } else {
      this.closeListeners.set(listener as CloseListener, once);
      this.flushClose();
    }
  }

  removeEventListener(type: "close", listener: CloseListener): void;
  removeEventListener(type: "message", listener: MessageListener): void;
  removeEventListener(
    type: "close" | "message",
    listener: CloseListener | MessageListener,
  ): void {
    if (type === "message") {
      this.messageListeners.delete(listener as MessageListener);
    } else {
      this.closeListeners.delete(listener as CloseListener);
    }
  }

  // ── Receive pipeline ───────────────────────────────────────────────

  private enqueueMessage(text: string): void {
    if (this.closed || (this.naturalClose !== null && this.options.inner === this.options.raw)) return;
    // Gzip fences new raw arrivals at death, but text already in its inflater
    // can still join this bounded ordered drain before its close notification.
    // GzipEnvelopeSocket can expand the next frame while decode is awaiting the
    // shared WASM runtime. Reserve synchronously at this expanded-text boundary,
    // before a closure retains the frame. Never drop a Delta and keep playing.
    // The first reserved message is logically active even before the drain's
    // microtask starts. Its own size must not spend a stalled backlog budget.
    const queuedBytes = this.outstandingMessages === 0 ? 0 : text.length * 2;
    if (
      this.outstandingMessages >= MAX_OUTSTANDING_MESSAGES
      || queuedBytes > MAX_QUEUED_TEXT_BYTES - this.queuedTextBytes
    ) {
      this.terminate("immediate", "receiveOverload", this.credentialsOf(text));
      return;
    }
    this.outstandingMessages++;
    this.queuedTextBytes += queuedBytes;
    this.enqueue(async () => {
      // Promotion releases the queued charge before decoding begins, so the
      // next active frame again leaves the full backlog allowance available.
      this.queuedTextBytes -= queuedBytes;
      try {
        await this.receive(text);
      } finally {
        // Terminal cleanup already cleared the message budget, including the
        // active receive; its eventual completion must not subtract it twice.
        if (!this.closed) {
          this.outstandingMessages--;
        }
      }
    });
  }

  private enqueue(task: () => Promise<void> | void): void {
    if (this.closed) return;
    this.queue.push(task);
    if (this.draining) return;
    this.draining = true;
    queueMicrotask(() => { void this.drain(); });
  }

  private async drain(): Promise<void> {
    try {
      while (!this.closed) {
        const task = this.queue.shift();
        if (!task) return;
        try {
          await task();
        } catch {
          // A failed callback must not strand later transport notifications.
        }
      }
    } finally {
      this.draining = false;
    }
  }

  private clearReceiveQueue(): void {
    // Unlike a promise chain, this queue can release waiting frame closures
    // immediately even if the one active WASM decode never settles.
    this.queue.length = 0;
    this.outstandingMessages = 0;
    this.queuedTextBytes = 0;
  }

  /**
   * Queued delivery of a raw transport error. The handler is read at delivery
   * time (a replacement before delivery wins, `null` drops the error), nothing
   * is retained for a handler installed afterwards, and a throwing handler
   * cannot stall the queue. A closed wrapper suppresses it.
   */
  private deliverError(event: Event): void {
    if (this.closed) return;
    try {
      this.onerror?.(event);
    } catch {
      // Subscribers do not own queue progress or transport cleanup.
    }
  }

  private async receive(text: string): Promise<void> {
    if (this.closed) return;
    // Confirmation can precede application attachment. All subsequent frames
    // wait for the application handler, so a queued GameStarted is accepted
    // before a following rejection consults bootstrap authority. Wake only
    // schedules a continuation: adapters can install close handlers this turn.
    if (this.state !== "awaitingConfirmation") await this.waitForConsumer();
    if (this.state === "closed") return;
    const type = frameType(text);
    if (this.state === "awaitingConfirmation") {
      if (type === "ServerHello") {
        this.handleConfirmation(text);
        return;
      }
      if (type === "Full" || type === "Delta") {
        // A codec frame cannot precede the confirmation that authorizes it.
        this.terminate("immediate", "confirmationFailure", this.credentialsOf(text));
        return;
      }
      await this.waitForConsumer();
      if (this.closed) return;
      this.deliver({ kind: "message", event: new MessageEvent<string>("message", { data: text }) });
      return;
    }
    if (type !== "Full" && type !== "Delta") {
      this.deliver({ kind: "message", event: new MessageEvent<string>("message", { data: text }) });
      return;
    }
    if (type === "Delta" && this.state === "awaitingFull") return;
    const decoded = await this.decode(text);
    if (decoded === null || this.closed) return;
    if (decoded.kind === "resyncRequired") {
      if (this.naturalClose !== null) {
        this.handleNaturalClose(this.naturalClose);
      } else {
        this.beginRecovery(text);
      }
      return;
    }
    if (this.state === "awaitingFull") {
      this.finishRecovery(decoded.json);
      return;
    }
    this.deliver({
      kind: "message",
      event: new MessageEvent<string>("message", { data: decoded.json }),
    });
  }

  /** Decode, or terminate. `null` means the transport is already finished. */
  private async decode(text: string): Promise<StateCodecDecodeResult | null> {
    const frame = this.encoder.encode(text);
    try {
      return await this.options.session.decode(frame);
    } catch {
      // Decoder/RPC failure (unknown session, session closed, RPC timeout) is
      // a transport failure, not codec integrity: the socket cannot continue.
      this.terminate("immediate", "decodeFailure", this.credentialsOf(text));
      return null;
    }
  }

  private handleConfirmation(text: string): void {
    let selected: unknown;
    try {
      const parsed = JSON.parse(text) as { data?: { selected_state_codec?: unknown } };
      selected = parsed.data?.selected_state_codec;
    } catch {
      this.terminate("immediate", "confirmationFailure");
      return;
    }
    if (selected !== STATE_CODEC_V1) {
      // Contradictory or missing selection: the peer will not speak the codec.
      this.terminate("immediate", "confirmationFailure");
      return;
    }
    this.state = "ready";
  }

  /**
   * `text` is the exact frame the decoder rejected. It is read only for the
   * credentials of a bootstrap rejection and never retained.
   */
  private beginRecovery(text: string): void {
    if (this.state !== "ready") {
      // Deltas are suppressed while awaiting the Full, so a rejection inside an
      // open recovery window is the recovery Full itself.
      this.terminate("immediate", "recoveryFullRejection");
      return;
    }
    // Classified before any resync attempt, so a send failure cannot lose the
    // reason: before application bootstrap there is no state to restore.
    const bootstrapPending = this.bootstrap() === "pending";
    if (this.closed || this.naturalClose !== null) {
      this.terminate();
      return;
    }
    this.state = "awaitingFull";
    this.recoveryTimer = setTimeout(
      () => this.terminate("immediate", "recoveryTimeout"),
      this.options.recoveryTimeoutMs ?? STATE_CODEC_SOCKET_TIMEOUT_MS,
    );
    this.notifyRecovery("recovering");
    if (this.closed) return;
    const resyncSent = this.sendResync();
    // Request the Full best-effort, then take the fatal close path.
    if (bootstrapPending) {
      this.terminate("immediate", "bootstrapRejection", readGameStartedFullCredentials(text));
      return;
    }
    // A resync that could not be sent on an established game is a transport
    // problem, not codec evidence.
    if (!resyncSent) this.terminate();
  }

  private finishRecovery(json: string): void {
    if (this.state !== "awaitingFull") return;
    const event = new MessageEvent<string>("message", { data: json });
    // Only the primary application's receipt for this invocation can authorize
    // recovery. An ignored frame or a callback that throws before committing
    // cannot reuse an earlier GameStarted's bootstrap acceptance.
    let accepted = false;
    try {
      accepted = this.dispatch({ kind: "message", event });
    } finally {
      this.finishRecoveryDecision(accepted);
    }
  }

  private finishRecoveryDecision(accepted: boolean): void {
    if (this.state !== "awaitingFull" || this.naturalClose !== null) return;
    if (!accepted || this.bootstrap() !== "accepted") {
      // The application declined this Full or reset its bootstrap. There is no
      // committed recovery snapshot, and no codec evidence against the stream.
      this.terminate();
      return;
    }
    // Bootstrap authority is an application callback; it can close the socket.
    if (this.closed || this.naturalClose !== null) return;
    // The valid Full has been delivered; count the completed recovery. The
    // second one is still delivered, but it never publishes `ready`.
    this.completedRecoveries += 1;
    if (this.completedRecoveries >= MAX_COMPLETED_RECOVERIES) {
      this.terminate("immediate", "repeatedRecovery");
      return;
    }
    this.clearRecoveryTimer();
    this.state = "ready";
    this.notifyRecovery("ready");
  }

  private sendResync(): boolean {
    if (this.closed || this.naturalClose !== null) return false;
    try {
      this.send('{"type":"ResyncState"}');
      return true;
    } catch {
      return false;
    }
  }

  /**
   * Reserve raw death and its deadline before pending inflation, decoding or
   * consumer attachment can delay the ordered close notification.
   */
  private latchNaturalClose(event: CloseEvent): void {
    if (this.closed || this.naturalClose !== null) return;
    this.naturalClose = event;
    this.clearRecoveryTimer();
    this.naturalCloseTimer = setTimeout(
      () => this.handleNaturalClose(event),
      STATE_CODEC_NATURAL_CLOSE_DRAIN_TIMEOUT_MS,
    );
  }

  private recordNaturalClose(event: CloseEvent): void {
    if (this.closed) return;
    this.latchNaturalClose(event);
    const natural = this.naturalClose ?? event;
    this.enqueue(() => this.handleNaturalClose(natural));
  }

  private handleNaturalClose(event: CloseEvent): void {
    if (this.state === "closed") return;
    this.state = "closed";
    this.clearReceiveQueue();
    this.clearRecoveryTimer();
    this.clearNaturalCloseTimer();
    this.options.raw.removeEventListener("close", this.rawCloseListener, true);
    this.wakeConsumers();
    try {
      this.releaseSession();
    } finally {
      try {
        // Raw death alone permits the ordered drain. Once this owner actually
        // terminates, retire any wrapped inflater and its pending payloads too.
        if (this.options.inner !== this.options.raw) this.options.inner.close();
      } finally {
        // Healthy frames drained first, or a rejected/stalled drain was ended.
        this.deliver({ kind: "close", event });
      }
    }
  }

  /**
   * Fatal or local close: invalidation, timer cancellation and lease release
   * are immediate, idempotent and independent of the receive queue. Only the
   * close notification may be deferred ("deferred" = local `close()`).
   *
   * A codec failure (`failure` set, always an immediate close) additionally
   * publishes the typed `failed` notice once, after invalidation and before the
   * close notification. The first terminal path wins: a failure after a local,
   * natural or earlier fatal close reports nothing. A pending natural drain
   * ends with its original close event instead. A new failure notice is
   * dispatched directly because `notifyRecovery` drops everything once closed;
   * a throwing callback cannot skip the close notification or inner close.
   */
  private terminate(
    notification: CloseNotification = "immediate",
    failure: StateCodecFailureReason | null = null,
    joinCredentials: StateCodecJoinCredentials | null = null,
  ): void {
    if (this.state === "closed") return;
    if (this.naturalClose !== null) {
      this.handleNaturalClose(this.naturalClose);
      return;
    }
    this.state = "closed";
    this.clearReceiveQueue();
    this.clearRecoveryTimer();
    this.clearNaturalCloseTimer();
    this.options.raw.removeEventListener("close", this.rawCloseListener, true);
    this.wakeConsumers();
    try {
      this.releaseSession();
    } finally {
      try {
        if (failure !== null) {
          this.dispatch({
            kind: "recovery",
            state: { kind: "failed", reason: failure, joinCredentials },
          });
        }
      } finally {
        try {
          const item: Extract<Outbound, { kind: "close" }> = {
            kind: "close",
            event: new CloseEvent("close"),
          };
          if (notification === "deferred") {
            queueMicrotask(() => this.deliver(item));
          } else {
            this.deliver(item);
          }
        } finally {
          this.options.inner.close();
        }
      }
    }
  }

  // ── Delivery ───────────────────────────────────────────────────────

  private hasMessageConsumer(): boolean {
    return this.handler !== null || this.messageListeners.size > 0;
  }

  private deliver(item: Outbound): void {
    if (item.kind === "close") {
      this.pendingClose = item;
      this.flushClose();
      return;
    }
    if (this.closed) return;
    this.dispatch(item);
  }

  private dispatch(item: Outbound): boolean {
    switch (item.kind) {
      case "message": {
        let accepted = false;
        try {
          accepted = this.handler?.(item.event) === true;
        } catch {
          // A throwing application did not acknowledge this delivery.
        }
        for (const [listener, once] of [...this.messageListeners]) {
          if (once) this.messageListeners.delete(listener);
          try {
            listener(item.event);
          } catch {
            // Continue delivering to independent observers; they do not own
            // application snapshot acceptance.
          }
        }
        return accepted;
      }
      case "close":
        try {
          this.closeHandler?.(item.event);
        } catch {
          // Close notification must never skip raw transport cleanup.
        } finally {
          for (const [listener, once] of [...this.closeListeners]) {
            if (once) this.closeListeners.delete(listener);
            try {
              listener(item.event);
            } catch {
              // See terminate(): close consumers run independently.
            }
          }
        }
        return false;
      case "recovery":
        // A throwing application callback must not strand the transport in
        // recovery without sending the resync that exits it.
        try {
          this.options.onRecovery?.(item.state);
        } catch {
          // The owning adapter already dropped its slot before emitting.
        }
        return false;
    }
  }

  private notifyRecovery(state: StateCodecRecoveryNotice): void {
    if (this.closed || this.naturalClose !== null) return;
    this.deliver({ kind: "recovery", state });
  }

  private async waitForConsumer(): Promise<void> {
    if (this.hasMessageConsumer() || this.closed) return;
    // The client targets ES2020; Promise.withResolvers is not in its API surface.
    await new Promise<void>((resolve) => this.consumerWaiters.push(resolve));
  }

  private wakeConsumers(): void {
    for (const resolve of this.consumerWaiters.splice(0)) resolve();
  }

  private flushClose(): void {
    if (!this.pendingClose || (!this.closeHandler && this.closeListeners.size === 0)) return;
    const item = this.pendingClose;
    this.pendingClose = null;
    this.dispatch(item);
  }

  /**
   * Credentials of the exact failing frame, only while this wrapper is live and
   * the application bootstrap is still pending. Parsed once for this failure
   * and never retained.
   */
  private credentialsOf(text: string): StateCodecJoinCredentials | null {
    if (this.closed || this.naturalClose !== null || this.bootstrap() !== "pending") return null;
    return readGameStartedFullCredentials(text);
  }

  private bootstrap(): StateCodecBootstrap {
    try {
      return this.options.bootstrap();
    } catch {
      return "pending";
    }
  }

  private clearRecoveryTimer(): void {
    if (this.recoveryTimer !== null) {
      clearTimeout(this.recoveryTimer);
      this.recoveryTimer = null;
    }
  }

  private clearNaturalCloseTimer(): void {
    if (this.naturalCloseTimer !== null) {
      clearTimeout(this.naturalCloseTimer);
      this.naturalCloseTimer = null;
    }
  }

  private releaseSession(): void {
    if (this.sessionReleased) return;
    this.sessionReleased = true;
    this.options.session.close();
  }
}

/**
 * Read the server's compact, type-first codec header without materializing its
 * payload. Rust validates the body. The compatibility path keeps noncanonical
 * field order/escaped tags and plain or malformed non-codec routing unchanged.
 *
 * Canonical frames (any type, not only Full/Delta) are recognized by their
 * prefix so an ordinary pass-through frame is serialized to the adapter without
 * a throwaway parse here; only a noncanonical envelope falls back to parsing.
 */
const CANONICAL_ENVELOPE_PREFIX = '{"type":"';

function frameType(text: string): string {
  if (text.startsWith(CANONICAL_ENVELOPE_PREFIX)) {
    const end = text.indexOf('"', CANONICAL_ENVELOPE_PREFIX.length);
    // `,"data":` immediately after the tag is the canonical serde envelope; a
    // tag containing an escape or another field order falls back to parsing.
    if (end > CANONICAL_ENVELOPE_PREFIX.length && text.startsWith(',"data":', end + 1)) {
      const tag = text.slice(CANONICAL_ENVELOPE_PREFIX.length, end);
      // `{"type":"Fu\u006cl","data":…}` is a compact but escaped tag: the raw
      // slice is not the discriminator, so keep it on the JSON path.
      if (!tag.includes("\\")) return tag;
    }
  }
  try {
    const parsed = JSON.parse(text) as { type?: unknown };
    return typeof parsed?.type === "string" ? parsed.type : "";
  } catch {
    return "";
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isSafeInteger(value: unknown, minimum: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= minimum;
}

/**
 * The join credentials of a GameStarted `Full` frame, or `null` for anything
 * else. This isolates the envelope knowledge of the credential path:
 * `{type:"Full", data:{state:{type:"GameStarted", data:{player_token,
 * your_player, full_key, ...}}, revision}}`. The credentials sit at
 * `data.state.data`, not inside the nested GameState.
 *
 * Trust boundary, from the server's serialization (not from a captured
 * credential payload): the state codec embeds the raw ServerMessage value
 * verbatim (`crates/state-codec/src/lib.rs:416-432,493-501`); the socket layer
 * serializes the whole mailbox envelope before encoding it
 * (`crates/phase-server/src/socket_io.rs:970-1001`); the mailbox flattens the
 * ServerMessage into that envelope (`socket_mailbox.rs:116-125`); and the
 * adjacent `type`/`data` tags with the optional credentials are defined in
 * `crates/server-core/src/protocol.rs:720-722,824-831`. Historical run
 * evidence (`browser/e2e93/results-new.json`) shows Full/Delta state delivery
 * with no plain GameStarted and a published session/identity.
 *
 * Validation is strict and total: no coercion, trimming or default identity.
 * Unknown fields are tolerated and only the three credentials are copied. The
 * nested GameState and the codec revision are never inspected: a malformed
 * revision may itself be why the frame was rejected. A shape check is not
 * authentication; the server authenticates the token on Reconnect.
 */
export function readGameStartedFullCredentials(text: string): StateCodecJoinCredentials | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isRecord(parsed) || parsed.type !== "Full") return null;
  const data = parsed.data;
  if (!isRecord(data)) return null;
  const state = data.state;
  if (!isRecord(state) || state.type !== "GameStarted") return null;
  const started = state.data;
  if (!isRecord(started)) return null;
  const { player_token: playerToken, your_player: yourPlayer, full_key: fullKey } = started;
  if (typeof playerToken !== "string" || playerToken === "") return null;
  if (!isSafeInteger(yourPlayer, 0)) return null;
  if (!isRecord(fullKey)) return null;
  const { game_code: gameCode, generation } = fullKey;
  if (typeof gameCode !== "string" || gameCode === "") return null;
  if (!isSafeInteger(generation, 1)) return null;
  return {
    playerToken,
    yourPlayer,
    fullKey: { game_code: gameCode, generation },
  };
}
