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
 *  - `confirmed`: the candidate delivered a frame of the positive bootstrap set
 *    (see {@link classifySetupFrame});
 *  - `refused`: the candidate delivered the server's refusal of the setup (the
 *    server installed nothing, but the incumbent's standing is no longer known
 *    either, so it is treated like a close that carries a reason);
 *  - `closed`: the candidate closed;
 *  - `timeout`: the bounded wait elapsed (this also bounds a setup write whose
 *    receipt never settles);
 *  - `aborted`: the attachment was cancelled (only once {@link watchAbort} is
 *    armed, because before the frame is written an abort is delivered to the
 *    transport through the write's own signal).
 */
export const SETUP_CONFIRMATION_TIMEOUT_MS = 5_000;

export type SetupConfirmationOutcome = "confirmed" | "refused" | "closed" | "timeout" | "aborted";

/** The server's refusal of a setup frame, as the adapters surface it. */
export interface SetupRefusal {
  readonly message: string;
  readonly code?: string;
}

export type SetupFrameClass =
  | { readonly kind: "positive" }
  | { readonly kind: "refusal"; readonly refusal: SetupRefusal; readonly rejected: boolean }
  | { readonly kind: "other" };

/**
 * What the server sends after `Reconnect` (game) and `ReconnectDraft` (draft).
 *
 * Positive bootstrap, game (`crates/phase-server/src/main.rs`): `GameStarted`
 * (`build_game_started_message` :770, sent at :10730) for a started game;
 * `GameCreated` and `SessionAttached` (`return_full_seat_while_session_locked`
 * :2389-2400) for a pregame seat; `StateWithheld` (`socket_io.rs:1094`,
 * `socket_mailbox.rs:150`) when the bootstrap frame itself was withheld.
 * Refusal, game: `Error` (:10564-10586 and the `ReconnectOutcome::Err` reply
 * at :10841).
 *
 * Positive bootstrap, draft: `DraftStateUpdate` (`reconnect_draft_seat` :2644,
 * :2678) and `DraftMatchStart` (`attach_current_draft_match` :2795). Refusal,
 * draft: `DraftActionRejected` (:13037, :13049, :13091, :13100) and `Error`.
 *
 * Any other frame (pings, slot updates, lobby fan-out) is neither: it is kept
 * in order for the adopted socket and does not settle the interval.
 */
const POSITIVE_FRAMES: Record<"game" | "draft", ReadonlySet<string>> = {
  game: new Set(["GameStarted", "GameCreated", "SessionAttached", "StateWithheld"]),
  draft: new Set(["DraftStateUpdate", "DraftMatchStart"]),
};

export function classifySetupFrame(surface: "game" | "draft", text: unknown): SetupFrameClass {
  if (typeof text !== "string") return { kind: "other" };
  let message: { type?: unknown; data?: unknown };
  try {
    message = JSON.parse(text) as { type?: unknown; data?: unknown };
  } catch {
    return { kind: "other" };
  }
  const type = typeof message?.type === "string" ? message.type : null;
  if (type === null) return { kind: "other" };
  if (POSITIVE_FRAMES[surface].has(type)) return { kind: "positive" };
  const data = (message.data ?? {}) as { message?: unknown; reason?: unknown; code?: unknown };
  if (type === "Error") {
    return {
      kind: "refusal",
      rejected: false,
      refusal: {
        message: typeof data.message === "string" ? data.message : "The server refused the setup.",
        ...(typeof data.code === "string" ? { code: data.code } : {}),
      },
    };
  }
  if (surface === "draft" && type === "DraftActionRejected") {
    return {
      kind: "refusal",
      rejected: true,
      refusal: { message: typeof data.reason === "string" ? data.reason : "The server rejected the setup." },
    };
  }
  return { kind: "other" };
}

export class SetupConfirmation {
  /** Resolves once, with the first outcome. Never rejects. */
  readonly settled: Promise<SetupConfirmationOutcome>;
  private resolve!: (outcome: SetupConfirmationOutcome) => void;
  private outcome: SetupConfirmationOutcome | null = null;
  private timer: ReturnType<typeof setTimeout> | null;
  private abortSignal: AbortSignal | null = null;
  private refusalValue: { refusal: SetupRefusal; rejected: boolean } | null = null;
  private readonly onAbort = () => this.finish("aborted");

  constructor(timeoutMs: number = SETUP_CONFIRMATION_TIMEOUT_MS) {
    this.settled = new Promise((resolve) => { this.resolve = resolve; });
    this.timer = setTimeout(() => this.finish("timeout"), timeoutMs);
  }

  /** The candidate delivered its first post-setup frame. */
  confirm(): void {
    this.finish("confirmed");
  }

  /** The candidate delivered the server's refusal of the setup. */
  refuse(refusal: SetupRefusal, rejected: boolean): void {
    if (this.outcome !== null) return;
    this.refusalValue = { refusal, rejected };
    this.finish("refused");
  }

  /** The refusal that settled this confirmation, when it was refused. */
  get refusal(): { refusal: SetupRefusal; rejected: boolean } | null {
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
