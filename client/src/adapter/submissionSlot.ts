import type { SubmitResult } from "./types";

/** The one game command an adapter has in flight, with its settlement callbacks. */
export interface PendingSubmission {
  readonly resolve: (result: SubmitResult) => void;
  readonly reject: (error: Error) => void;
  /**
   * The `command_id` stamped on the outgoing frame, or `null` when the server
   * did not negotiate `CommandIdV1` and the legacy first-completion-frame
   * rules apply to this submission.
   */
  readonly commandId: number | null;
}

/** How to treat a completion frame that did not settle the pending submission. */
export interface UnmatchedCompletion {
  /** Emit the legacy `actionPendingChanged: false` (never for a stamped slot). */
  readonly releasesIndicator: boolean;
  /** Surface the frame's own failure event (`requestRejected`, `operationFailed`). */
  readonly surfaces: boolean;
}

/**
 * Single in-flight game-command slot shared by the WebSocket-backed adapters.
 *
 * The slot owns the resolve/reject pair and the stamped command id as one
 * value, so they cannot drift, and it is the only place that decides whether a
 * completion frame (`StateUpdate`, `ActionRejected`, `ActionFailed`,
 * `ActionNoOp`, `StateWithheld`) answers the pending command:
 *
 * - a stamped submission settles only on a frame echoing its `command_id`;
 * - an unstamped submission (server without `CommandIdV1`) settles on the
 *   first completion frame, as before.
 *
 * Correlation is decided from the pending submission, never from the current
 * server info, so a command always follows the rules it was sent under.
 * Command ids are monotonic per slot and never reused, so a late echo from a
 * previous socket cannot match a later command.
 */
export class SubmissionSlot {
  private pending: PendingSubmission | null = null;
  private nextCommandId = 1;

  get occupied(): boolean {
    return this.pending !== null;
  }

  /** True while a submission stamped with a `command_id` is in flight. */
  get stamped(): boolean {
    return this.pending !== null && this.pending.commandId !== null;
  }

  /** Claim the slot; stamp a fresh id only when `correlate` is true. */
  claim(
    resolve: (result: SubmitResult) => void,
    reject: (error: Error) => void,
    correlate: boolean,
  ): PendingSubmission {
    const submission: PendingSubmission = {
      resolve,
      reject,
      commandId: correlate ? this.nextCommandId++ : null,
    };
    this.pending = submission;
    return submission;
  }

  owns(submission: PendingSubmission): boolean {
    return this.pending === submission;
  }

  /** Add the submission's `command_id` beside `type`/`data` when it has one. */
  stamp<T extends object>(frame: T, submission: PendingSubmission): T | (T & { command_id: number }) {
    return submission.commandId === null ? frame : { ...frame, command_id: submission.commandId };
  }

  /** Release the slot if `submission` still owns it. Returns whether it did. */
  release(submission: PendingSubmission): boolean {
    if (this.pending !== submission) return false;
    this.pending = null;
    return true;
  }

  /** Unconditionally empty the slot (close, dispose, AI fault) and return what was in it. */
  drop(): PendingSubmission | null {
    const dropped = this.pending;
    this.pending = null;
    return dropped;
  }

  /**
   * Settle on a completion frame. Returns the submission it answers, or `null`
   * (leaving the slot untouched) for a peer/ambient frame, a late echo or a
   * frame with no id while the pending command is stamped.
   */
  take(commandId: unknown): PendingSubmission | null {
    const pending = this.pending;
    if (pending === null) return null;
    if (pending.commandId !== null && commandId !== pending.commandId) return null;
    this.pending = null;
    return pending;
  }

  /**
   * Policy for a completion frame `take` declined. An id-less failure is not an
   * answer to this connection's command (for example the rejection of a
   * malformed frame the server could not attribute to a command), so it
   * surfaces without touching the slot; a frame naming another command's id
   * belongs to that command and is swallowed. The legacy indicator release only
   * applies while no stamped command is in flight.
   */
  unmatched(commandId: unknown): UnmatchedCompletion {
    return { releasesIndicator: !this.stamped, surfaces: commandId === undefined };
  }
}
