import { STATE_CODEC_DECODE_TIMEOUT_MS } from "../adapter/state-codec-runtime";
import { frameType } from "./stateCodecSocket";

/**
 * The uncertain interval of a reconnect: from the moment a candidate's setup
 * frame is sent until its positive bootstrap arrives.
 *
 * The server replaces a seat's sender when it handles `Reconnect` /
 * `ReconnectDraft` and never closes the predecessor, so once the frame is sent
 * neither the incumbent nor the candidate is known to own the seat. The
 * candidate is adopted only on its positive bootstrap; a close, timeout or
 * abort first retires both sockets.
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
/**
 * The bound on the interval. It runs from the setup write until the decoded
 * bootstrap reaches the adapter, so it spans the server's own work before the
 * bootstrap (session lock, any AI actions it runs first, building the frame),
 * the transfer and the decode. It is the decoder's own deadline: a bootstrap
 * whose decode alone takes that long has already failed its socket, so the
 * bound never retires a candidate the codec would still accept on decode time
 * alone. Server work and transfer share it; it is not measured against them, so
 * an attempt whose server work runs past it ends as a timeout and the game
 * adapter retries within its episode budget.
 */
export const SETUP_CONFIRMATION_TIMEOUT_MS = STATE_CODEC_DECODE_TIMEOUT_MS;

export type SetupConfirmationOutcome = "confirmed" | "refused" | "closed" | "timeout" | "aborted";

/** The server's refusal of a setup frame, as the adapters surface it. */
export interface SetupRefusal {
  readonly message: string;
  readonly code?: string;
}

export type SetupFrameClass =
  /** The positive bootstrap: the server installed the candidate's sender. */
  | { readonly kind: "positive" }
  /** `Error`: the server refused the setup and installed nothing. Settles the interval. */
  | { readonly kind: "refusal"; readonly refusal: SetupRefusal }
  /**
   * Draft `DraftActionRejected`: the server's answer on the candidate, which may
   * already hold the draft seat (see {@link classifySetupFrame}). It adopts the
   * candidate exactly as the merge base did, and the rejection then replays to
   * reject the reattach.
   */
  | { readonly kind: "answered" }
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
  // the frame: the positive bootstrap carries the whole board and is parsed
  // once, by the adapter, after adoption. Only the small frames whose data the
  // decision needs are parsed here.
  const type = frameType(text);
  if (POSITIVE_FRAMES[surface].has(type)) return { kind: "positive" };
  if (surface === "draft" && type === "DraftActionRejected") return { kind: "answered" };
  const needsData = type === "Error" || (surface === "game" && type === "StateWithheld");
  if (!needsData) return { kind: "other" };
  let data: { message?: unknown; code?: unknown; withheld?: unknown };
  try {
    data = ((JSON.parse(text) as { data?: unknown }).data ?? {}) as typeof data;
  } catch {
    return { kind: "other" };
  }
  if (type === "StateWithheld") {
    const withheld = data.withheld as { type?: unknown } | null | undefined;
    return withheld?.type === "GameStarted" ? { kind: "positive" } : { kind: "other" };
  }
  return {
    kind: "refusal",
    refusal: {
      message: typeof data.message === "string" ? data.message : "The server refused the setup.",
      ...(typeof data.code === "string" ? { code: data.code } : {}),
    },
  };
}

export class SetupConfirmation {
  /** Resolves once, with the first outcome. Never rejects. */
  readonly settled: Promise<SetupConfirmationOutcome>;
  private resolve!: (outcome: SetupConfirmationOutcome) => void;
  private outcome: SetupConfirmationOutcome | null = null;
  private timer: ReturnType<typeof setTimeout> | null;
  private abortSignal: AbortSignal | null = null;
  private refusalValue: SetupRefusal | null = null;
  private readonly onAbort = () => this.finish("aborted");

  constructor(timeoutMs: number = SETUP_CONFIRMATION_TIMEOUT_MS) {
    this.settled = new Promise((resolve) => { this.resolve = resolve; });
    this.timer = setTimeout(() => this.finish("timeout"), timeoutMs);
  }

  /** Feeds one frame the candidate received during the interval. */
  observe(surface: "game" | "draft", data: unknown): void {
    // A settled interval classifies nothing more: later frames only wait, in
    // order, for the adopted socket's own handler.
    if (this.outcome !== null) return;
    const frame = classifySetupFrame(surface, data);
    switch (frame.kind) {
      case "positive":
      case "answered":
        this.confirm();
        return;
      case "refusal":
        this.refuse(frame.refusal);
        return;
      case "other":
        return;
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
