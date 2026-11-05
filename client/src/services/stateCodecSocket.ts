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
 *    until a Full decodes, with a bounded recovery window;
 *  - never interprets, diffs or patches state. Rust owns reconstruction.
 *
 * Ownership: the wrapper owns the prepared decoder session lease and releases
 * it exactly once, on any terminal path.
 */
import type { StateCodecDecodeResult, StateCodecSession } from "../adapter/state-codec-runtime";
import { AdapterError, AdapterErrorCode } from "../adapter/types";
import type { PhaseSocketTransport } from "./openPhaseSocket";

/** The only codec this client advertises. */
export const STATE_CODEC_V1 = "StateCodecV1";
export type StateCodecName = typeof STATE_CODEC_V1;

/** What the owning adapter says about the current attachment's bootstrap. */
export type StateCodecBootstrap = "pending" | "accepted";

/** Ordered notification from the wrapper to the owning adapter. */
export type StateCodecRecoveryState = "recovering" | "ready";

/**
 * One attachment's bootstrap authority, shared by the owning adapters. `socket`
 * is `null` until the attachment's socket is adopted; `status` becomes
 * `accepted` only after a genuine GameStarted passes the adapter's guards.
 */
export interface StateCodecBootstrapBinding {
  socket: PhaseSocketTransport | null;
  status: StateCodecBootstrap;
}

/** Rejection message for a command dropped while codec recovery begins. */
export const STATE_CODEC_RECOVERY_INTERRUPTED_MESSAGE = "State delivery interrupted during action";

/** Rejection message for a submission refused while codec recovery is in progress. */
export const STATE_CODEC_RECOVERY_SUBMISSION_BLOCKED_MESSAGE =
  "State delivery is being recovered; this submission was not sent.";

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
   * Fresh, still-pending authority for one attach. It is not current until
   * {@link adopt}: until then a still-live older socket keeps its own binding
   * and recovery gate.
   */
  begin(): StateCodecBootstrapBinding {
    return { socket: null, status: "pending" };
  }

  /** The attachment's socket is adopted: retire the previous binding and its recovery gate. */
  adopt(binding: StateCodecBootstrapBinding, socket: PhaseSocketTransport): void {
    binding.socket = socket;
    this.current = binding;
    this.recovery = null;
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
   * Codec recovery notification for `binding`. On `"recovering"` the gate is
   * set and the pending command is interrupted BEFORE any listener can run:
   * the interruption emits synchronously and a listener may submit, which must
   * already be refused. `interruptPending` drops the adapter's submission slot
   * and, if it held a command, rejects it with the supplied retryable error.
   */
  handleRecovery(
    binding: StateCodecBootstrapBinding,
    state: StateCodecRecoveryState,
    currentSocket: PhaseSocketTransport | null,
    disposed: boolean,
    interruptPending: (error: AdapterError) => void,
  ): void {
    if (disposed) return;
    if (this.current !== binding) return;
    if (binding.socket === null || currentSocket !== binding.socket) return;
    if (state === "recovering") {
      this.recovery = state;
      interruptPending(new AdapterError("WS_CLOSED", STATE_CODEC_RECOVERY_INTERRUPTED_MESSAGE, true));
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
        STATE_CODEC_RECOVERY_SUBMISSION_BLOCKED_MESSAGE,
        true,
      );
    }
  }

  /** The current attachment's socket is gone: a retired transport can no longer gate submissions. */
  clearRecovery(): void {
    this.recovery = null;
  }

  /** The owning adapter is disposed: no binding or recovery state survives. */
  release(): void {
    this.recovery = null;
    this.current = null;
  }
}

/** Bounded optional preparation budget (event-loop time, not network). */
export const STATE_CODEC_PREPARATION_TIMEOUT_MS = 5_000;
/** Confirmation and recovery window; the same value bounds both. */
export const STATE_CODEC_SOCKET_TIMEOUT_MS = 5_000;

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
  | { kind: "recovery"; state: StateCodecRecoveryState };

export interface StateCodecSocketOptions {
  inner: PhaseSocketTransport;
  session: StateCodecSession;
  /**
   * Consulted on rejection and after delivering a recovery Full. Application
   * bootstrap, not a valid wire baseline, decides whether recovery is safe.
   */
  bootstrap: () => StateCodecBootstrap;
  onRecovery?: (state: StateCodecRecoveryState) => void;
  /** Actual send accounting for an uncompressed inner transport. */
  onFrameSent?: () => void;
  confirmationTimeoutMs?: number;
  recoveryTimeoutMs?: number;
}

export class StateCodecSocket implements PhaseSocketTransport {
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  private closeHandler: CloseListener | null = null;

  private handler: MessageListener | null = null;
  private readonly messageListeners = new Map<MessageListener, boolean>();
  private readonly closeListeners = new Map<CloseListener, boolean>();
  private queue: Promise<void> = Promise.resolve();
  private state: ConnectionState = "awaitingConfirmation";
  private confirmationTimer: ReturnType<typeof setTimeout> | null = null;
  private recoveryTimer: ReturnType<typeof setTimeout> | null = null;
  private sessionReleased = false;
  private readonly consumerWaiters: (() => void)[] = [];
  private pendingClose: Extract<Outbound, { kind: "close" }> | null = null;
  private readonly encoder = new TextEncoder();

  constructor(private readonly options: StateCodecSocketOptions) {
    const { inner } = options;
    this.confirmationTimer = setTimeout(
      () => this.terminate(),
      options.confirmationTimeoutMs ?? STATE_CODEC_SOCKET_TIMEOUT_MS,
    );
    inner.onopen = (event) => {
      try {
        this.onopen?.(event);
      } catch {
        // A throwing open handler must not break the event routing.
      }
    };
    inner.onerror = (event) => {
      this.onerror?.(event);
    };
    inner.onmessage = (event) => this.enqueue(() => this.receive(event.data as string));
    inner.onclose = (event) => this.enqueue(() => this.handleNaturalClose(event));
  }

  get readyState(): number {
    return this.options.inner.readyState;
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

  get onmessage(): MessageListener | null {
    return this.handler;
  }

  set onmessage(listener: MessageListener | null) {
    this.handler = listener;
    if (listener) this.wakeConsumers();
  }

  send(data: string): void {
    if (this.state === "closed") return;
    this.options.inner.send(data);
    this.options.onFrameSent?.();
  }

  close(): void {
    // Local close is terminal even during an outstanding decode: the state is
    // invalidated and the lease released here. The notification is posted on a
    // microtask, as native and gzip sockets deliver close asynchronously, so a
    // caller that closes and then settles its own state (dispose, a failed
    // setup send) still observes its own reason before the close handler runs.
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

  private enqueue(task: () => Promise<void> | void): void {
    this.queue = this.queue.then(task).catch(() => undefined);
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
        this.terminate();
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
      this.beginRecovery();
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
      this.terminate();
      return null;
    }
  }

  private handleConfirmation(text: string): void {
    let selected: unknown;
    try {
      const parsed = JSON.parse(text) as { data?: { selected_state_codec?: unknown } };
      selected = parsed.data?.selected_state_codec;
    } catch {
      this.terminate();
      return;
    }
    if (selected !== STATE_CODEC_V1) {
      // Contradictory or missing selection: the peer will not speak the codec.
      this.terminate();
      return;
    }
    this.clearConfirmationTimer();
    this.state = "ready";
  }

  private beginRecovery(): void {
    if (this.state !== "ready") {
      // A second rejection, or a rejection inside an open recovery window.
      this.terminate();
      return;
    }
    this.state = "awaitingFull";
    this.recoveryTimer = setTimeout(
      () => this.terminate(),
      this.options.recoveryTimeoutMs ?? STATE_CODEC_SOCKET_TIMEOUT_MS,
    );
    this.notifyRecovery("recovering");
    if (this.closed) return;
    if (!this.sendResync()) {
      this.terminate();
      return;
    }
    // Before application bootstrap there is no state to restore: request the
    // Full best-effort, then take the fatal close path.
    if (this.bootstrap() === "pending") this.terminate();
  }

  private finishRecovery(json: string): void {
    if (this.state !== "awaitingFull") return;
    const event = new MessageEvent<string>("message", { data: json });
    // Receive waits for application attachment before decoding.
    try {
      this.dispatch({ kind: "message", event });
    } finally {
      // The application handler decides bootstrap acceptance while this frame
      // is delivered, so the decision is made after delivery and in a
      // throwing-listener-safe path.
      this.finishRecoveryDecision();
    }
  }

  private finishRecoveryDecision(): void {
    if (this.state !== "awaitingFull") return;
    if (this.bootstrap() !== "accepted") {
      this.terminate();
      return;
    }
    this.clearRecoveryTimer();
    this.state = "ready";
    this.notifyRecovery("ready");
  }

  private sendResync(): boolean {
    if (this.state === "closed") return false;
    try {
      this.send('{"type":"ResyncState"}');
      return true;
    } catch {
      return false;
    }
  }

  private handleNaturalClose(event: CloseEvent): void {
    if (this.state === "closed") return;
    this.state = "closed";
    this.clearTimers();
    this.wakeConsumers();
    try {
      this.releaseSession();
    } finally {
      // Natural close follows all previously received messages.
      this.deliver({ kind: "close", event });
    }
  }

  /**
   * Fatal or local close: invalidation, timer cancellation and lease release
   * are immediate, idempotent and independent of the receive queue. Only the
   * close notification may be deferred ("deferred" = local `close()`).
   */
  private terminate(notification: CloseNotification = "immediate"): void {
    if (this.state === "closed") return;
    this.state = "closed";
    this.clearTimers();
    this.wakeConsumers();
    try {
      this.releaseSession();
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

  private dispatch(item: Outbound): void {
    switch (item.kind) {
      case "message":
        try {
          this.handler?.(item.event);
        } catch {
          // Subscribers do not own queue progress or transport cleanup.
        }
        for (const [listener, once] of [...this.messageListeners]) {
          if (once) this.messageListeners.delete(listener);
          try {
            listener(item.event);
          } catch {
            // Continue delivering to independent subscribers.
          }
        }
        return;
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
        return;
      case "recovery":
        // A throwing application callback must not strand the transport in
        // recovery without sending the resync that exits it.
        try {
          this.options.onRecovery?.(item.state);
        } catch {
          // The owning adapter already dropped its slot before emitting.
        }
        return;
    }
  }

  private notifyRecovery(state: StateCodecRecoveryState): void {
    if (this.state === "closed") return;
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

  private bootstrap(): StateCodecBootstrap {
    try {
      return this.options.bootstrap();
    } catch {
      return "pending";
    }
  }

  private clearConfirmationTimer(): void {
    if (this.confirmationTimer !== null) {
      clearTimeout(this.confirmationTimer);
      this.confirmationTimer = null;
    }
  }

  private clearRecoveryTimer(): void {
    if (this.recoveryTimer !== null) {
      clearTimeout(this.recoveryTimer);
      this.recoveryTimer = null;
    }
  }

  private clearTimers(): void {
    this.clearConfirmationTimer();
    this.clearRecoveryTimer();
  }

  private releaseSession(): void {
    if (this.sessionReleased) return;
    this.sessionReleased = true;
    this.options.session.close();
  }
}

/**
 * The reserved top-level tag of a frame, without re-serializing it. Anything
 * unparseable is treated as a plain frame and passed through untouched.
 */
function frameType(text: string): string {
  try {
    const parsed = JSON.parse(text) as { type?: unknown };
    return typeof parsed?.type === "string" ? parsed.type : "";
  } catch {
    return "";
  }
}
