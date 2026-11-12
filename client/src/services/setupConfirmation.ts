import { STATE_CODEC_DECODE_TIMEOUT_MS } from "../adapter/state-codec-runtime";
import { frameType } from "./stateCodecSocket";

/**
 * The bound on the interval, counted from the setup write. It spans the
 * server's own work before the bootstrap (session lock, any AI actions it runs
 * first, building the frame) and the transfer; it is not measured against
 * them, so an attempt whose server work runs past it ends as a timeout and the
 * game adapter retries within its episode budget. It does not pre-empt a decode:
 * when it expires while the candidate's codec transport is decoding a frame, it
 * defers once to that decode, whose own deadline (the same value, counted from
 * the decode call) bounds the wait, so the decoder's verdict settles the
 * interval: a delivered bootstrap confirms, and a terminal `decodeFailure`
 * closes the candidate with its failure recorded for the episode. A game
 * reconnect can decode its bootstrap `Full` inside the interval; a draft
 * reattach is answered by a plain frame first (`DraftMatchStart`,
 * `DraftStateUpdate`), so with this server no decode is in flight there.
 */
export const SETUP_CONFIRMATION_TIMEOUT_MS = STATE_CODEC_DECODE_TIMEOUT_MS;

export type SetupConfirmationOutcome = "confirmed" | "refused" | "closed" | "timeout" | "aborted";

/** The server's refusal of a setup frame, as the adapters surface it. */
export interface SetupRefusal {
  readonly message: string;
  readonly code?: string;
}

/** A setup frame parsed once by the interval, handed on to the adapter's dispatch. */
export interface SetupFrameMessage {
  readonly type: string;
  readonly data?: unknown;
}

export type SetupFrameClass =
  /** The positive bootstrap: the server installed the candidate's sender. */
  | { readonly kind: "positive"; readonly message: SetupFrameMessage }
  /** `Error`: the server refused the setup and installed nothing. Settles the interval. */
  | { readonly kind: "refusal"; readonly refusal: SetupRefusal; readonly message: SetupFrameMessage }
  /**
   * Draft `DraftActionRejected`: the server's answer on the candidate, which may
   * already hold the draft seat (see {@link classifySetupFrame}). It adopts the
   * candidate exactly as the merge base did, and the rejection then replays to
   * reject the reattach.
   */
  | { readonly kind: "answered"; readonly message: SetupFrameMessage }
  /** Anything else: neither adopts nor refuses; kept in order for the adopted socket. */
  | { readonly kind: "other" };

/**
 * What the server sends after `Reconnect` (game) and `ReconnectDraft` (draft),
 * in `crates/phase-server/src/main.rs` unless noted.
 *
 * Positive bootstrap, game: `GameStarted` (`build_game_started_message` :770,
 * queued at :10730 before the sender is installed at :10734) for a started
 * game; `GameCreated` then `SessionAttached` (`return_full_seat_while_session_locked`
 * :2389-2400) for a pregame seat; `StateWithheld` whose `withheld` frame is
 * `GameStarted` (`socket_mailbox.rs:150`), which replaces that `GameStarted`
 * when it is over the frame bound. A `StateWithheld` of a `StateUpdate` cannot
 * precede the bootstrap, so it is not positive.
 * Refusal, game: `Error` only (:10563-10583, the `ReconnectOutcome::Err` reply
 * at :10840-10843, and the mode, wire and authority gates at :10062-10090).
 *
 * Positive bootstrap, draft: `DraftStateUpdate` (`attach_current_draft_match`
 * :2644, :2678, :2753, :2767, :2785, :2815) and `DraftMatchStart` (:2795).
 * Refusal, draft: `Error` from the mode and wire gates (:10062-10076), which
 * run before dispatch, so nothing was installed.
 * Answered, draft: `DraftActionRejected`. It cannot tell the client who owns
 * the seat: the handler sends it for refusals before anything is installed
 * (:13037, :13049, and `reconnect_draft_seat` errors before :2584, answered at
 * :13100), for failures of `attach_current_draft_match` AFTER
 * `reconnect_draft_seat` installed the candidate as the seat's sender (:2584)
 * and bound its identity (:2597-2599), answered alone at :13089-13093, and right
 * before the `DraftStateUpdate` of a reattach that succeeded (:2750-2753,
 * :2764-2767, :2782-2785). Retiring the candidate would drop a seat the server
 * gave it, and nothing retries a reattach, so the candidate is adopted as the
 * merge base adopted it, and the replayed rejection rejects the reattach.
 *
 * Any other frame (pings, slot updates, lobby fan-out) is neither: it is kept
 * in order for the adopted socket and does not settle the interval.
 */
const POSITIVE_FRAMES: Record<"game" | "draft", ReadonlySet<string>> = {
  game: new Set(["GameStarted", "GameCreated", "SessionAttached"]),
  draft: new Set(["DraftStateUpdate", "DraftMatchStart"]),
};

export function classifySetupFrame(surface: "game" | "draft", text: unknown): SetupFrameClass {
  if (typeof text !== "string") return { kind: "other" };
  // The tag comes from the canonical type-first envelope without materializing
  // the frame, so an unrelated frame is never parsed here. A frame whose tag can
  // settle the interval is parsed exactly once, here: a body that does not parse
  // as that envelope is `other` (it neither adopts nor refuses, and keeps its
  // malformed-frame handling), and the parsed envelope is what the adapter
  // dispatches after adoption, so the bootstrap is still materialized once.
  const type = frameType(text);
  const settling = POSITIVE_FRAMES[surface].has(type)
    || (surface === "draft" && type === "DraftActionRejected")
    || type === "Error"
    || (surface === "game" && type === "StateWithheld");
  if (!settling) return { kind: "other" };
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return { kind: "other" };
  }
  if (typeof parsed !== "object" || parsed === null || (parsed as { type?: unknown }).type !== type) {
    return { kind: "other" };
  }
  const message = parsed as SetupFrameMessage;
  if (POSITIVE_FRAMES[surface].has(type)) return { kind: "positive", message };
  if (type === "DraftActionRejected") return { kind: "answered", message };
  const data = (message.data ?? {}) as { message?: unknown; code?: unknown; withheld?: unknown };
  if (type === "StateWithheld") {
    const withheld = data.withheld as { type?: unknown } | null | undefined;
    return withheld?.type === "GameStarted" ? { kind: "positive", message } : { kind: "other" };
  }
  return {
    kind: "refusal",
    message,
    refusal: {
      message: typeof data.message === "string" ? data.message : "The server refused the setup.",
      ...(typeof data.code === "string" ? { code: data.code } : {}),
    },
  };
}

/**
 * The uncertain interval of a reconnect: from the moment a candidate's setup
 * frame is sent until its positive bootstrap arrives.
 *
 * The server replaces a seat's sender when it handles `Reconnect` /
 * `ReconnectDraft` and never closes the predecessor, so once the frame is sent
 * neither the incumbent nor the candidate is known to own the seat. The
 * candidate is adopted on its positive bootstrap (or, for a draft, on the
 * server's `DraftActionRejected` answer); an `Error` refusal, a close, the
 * bound or an abort first retires both sockets.
 *
 * `SetupConfirmation` is the single owner of that interval for both server
 * adapters. It settles exactly once, with whichever of these happens first:
 *
 *  - `confirmed`: the candidate delivered a frame of the positive bootstrap set,
 *    or a draft candidate the server's answer (see {@link classifySetupFrame});
 *  - `refused`: the candidate delivered the server's `Error` answer to the
 *    setup (the server installed nothing, but the incumbent's standing is no
 *    longer known either, so it is treated like a close that carries a reason);
 *  - `closed`: the candidate closed;
 *  - `timeout`: the bounded wait elapsed (this also bounds a setup write whose
 *    receipt never settles);
 *  - `aborted`: the attachment was cancelled (only once {@link watchAbort} is
 *    armed, because before the frame is written an abort is delivered to the
 *    transport through the write's own signal).
 *
 * A draft `DraftActionRejected` also confirms: it is the server's answer on a
 * candidate that may already hold the seat (see {@link classifySetupFrame}).
 */
export class SetupConfirmation {
  /** Resolves once, with the first outcome. Never rejects. */
  readonly settled: Promise<SetupConfirmationOutcome>;
  private resolve!: (outcome: SetupConfirmationOutcome) => void;
  private outcome: SetupConfirmationOutcome | null = null;
  private timer: ReturnType<typeof setTimeout> | null;
  private abortSignal: AbortSignal | null = null;
  private disposed = false;
  private refusalValue: SetupRefusal | null = null;
  private readonly onAbort = () => this.finish("aborted");

  /**
   * `decodeInFlight` reports the candidate transport's decode in flight, if
   * any; the bound defers once to it (see {@link SETUP_CONFIRMATION_TIMEOUT_MS}).
   */
  constructor(
    timeoutMs: number = SETUP_CONFIRMATION_TIMEOUT_MS,
    private readonly decodeInFlight: () => Promise<void> | null = () => null,
  ) {
    this.settled = new Promise((resolve) => { this.resolve = resolve; });
    this.timer = setTimeout(() => this.expire(), timeoutMs);
  }

  private expire(): void {
    this.timer = null;
    if (this.outcome !== null) return;
    const decoding = this.decodeInFlight();
    if (decoding === null) {
      this.finish("timeout");
      return;
    }
    // One deferral: once the decode has answered, its delivery or its terminal
    // close (both queued behind it) settles first; otherwise this times out.
    void decoding.then(() => {
      if (this.outcome !== null || this.disposed) return;
      this.timer = setTimeout(() => this.finish("timeout"), 0);
    });
  }

  /**
   * Feeds one frame the candidate received during the interval. Returns the
   * envelope it parsed to settle the interval, which the adapter must dispatch
   * instead of parsing the frame again; `undefined` when it parsed nothing.
   */
  observe(surface: "game" | "draft", data: unknown): SetupFrameMessage | undefined {
    // A settled interval classifies nothing more: later frames only wait, in
    // order, for the adopted socket's own handler.
    if (this.outcome !== null) return undefined;
    const frame = classifySetupFrame(surface, data);
    switch (frame.kind) {
      case "positive":
      case "answered":
        this.confirm();
        return frame.message;
      case "refusal":
        this.refuse(frame.refusal);
        return frame.message;
      case "other":
        return undefined;
    }
  }

  /** The candidate delivered its positive bootstrap. */
  confirm(): void {
    this.finish("confirmed");
  }

  /** The candidate delivered the server's refusal of the setup. */
  refuse(refusal: SetupRefusal): void {
    if (this.outcome !== null) return;
    this.refusalValue = refusal;
    this.finish("refused");
  }

  /** The refusal that settled this confirmation, when it was refused. */
  get refusal(): SetupRefusal | null {
    return this.refusalValue;
  }

  /** The candidate closed. */
  closed(): void {
    this.finish("closed");
  }

  /** From now on an abort of `signal` settles this confirmation. */
  watchAbort(signal: AbortSignal): void {
    if (this.outcome !== null) return;
    if (signal.aborted) {
      this.finish("aborted");
      return;
    }
    this.abortSignal = signal;
    signal.addEventListener("abort", this.onAbort, { once: true });
  }

  /** The outcome so far, or null while the interval is still open. */
  get current(): SetupConfirmationOutcome | null {
    return this.outcome;
  }

  /** Releases the timer and the abort listener; idempotent. */
  dispose(): void {
    this.disposed = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
    this.abortSignal?.removeEventListener("abort", this.onAbort);
    this.abortSignal = null;
  }

  private finish(outcome: SetupConfirmationOutcome): void {
    if (this.outcome !== null) return;
    this.outcome = outcome;
    this.dispose();
    this.resolve(outcome);
  }
}

/**
 * Reports a listener failure that must not unwind its caller as an uncaught
 * error: a window `error` event, which a raw socket's handler would raise and
 * telemetry records as `js_error`.
 */
export function reportUncaughtError(error: unknown): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new ErrorEvent("error", {
    error,
    message: error instanceof Error ? error.message : String(error),
  }));
}
