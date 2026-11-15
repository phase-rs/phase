import type { ContestClickInput, PlayerId } from "../adapter/types.ts";

/** The contest a host is currently running: its engine-authored identity and seats. */
export interface ActiveContest {
  id: number;
  players: readonly [PlayerId, PlayerId];
}

/** Most clicks one seat may have waiting for the next tick. A legitimate seat
 *  contributes a handful per 50ms tick; the cap bounds memory regardless of rate. */
export const MAX_PENDING_CLICKS_PER_SEAT = 64;
/** Sustained clicks per second a seat may submit — the server's contest policy
 *  (`ContestPolicy`) — and the instant burst allowed before it applies. */
export const MAX_CLICKS_PER_SECOND = 30;
export const CLICK_BURST = 40;

const MAX_SEQUENCE = 0xffff_ffff;

/** The engine decodes a sequence as `u32`, so anything else must never reach it. */
export function isContestSequence(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= MAX_SEQUENCE;
}

function isContestId(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

interface Bucket {
  tokens: number;
  lastMs: number;
}

/**
 * Where a P2P host holds guests' contest clicks until its next engine tick.
 *
 * A click is accepted only while a contest is running, only from one of its two
 * seats, only for that contest's identity, only with a well-formed sequence, and only
 * within a per-seat rate and queue bound — so an authenticated guest cannot grow host
 * memory during ordinary play or smuggle a malformed value into the engine batch.
 * Dropping a click is harmless: a click only ever signals activity.
 */
export class ContestClickBuffer {
  private contest: ActiveContest | null = null;
  private pending: ContestClickInput[] = [];
  private readonly buckets = new Map<PlayerId, Bucket>();

  /** Record which contest is running (null: none). Switching contests discards
   *  anything buffered for the previous one. */
  setContest(contest: ActiveContest | null): void {
    if (contest?.id === this.contest?.id) {
      this.contest = contest;
      return;
    }
    this.contest = contest;
    this.pending = [];
    this.buckets.clear();
  }

  /** Offer one frame from the authenticated `seat`; returns whether it was kept. */
  offer(seat: PlayerId, frame: { contest: unknown; seq: unknown }, nowMs: number): boolean {
    const contest = this.contest;
    if (!contest || !contest.players.includes(seat)) return false;
    if (!isContestId(frame.contest) || frame.contest !== contest.id) return false;
    if (!isContestSequence(frame.seq)) return false;
    if (!this.takeToken(seat, nowMs)) return false;
    if (this.pending.filter((click) => click.player === seat).length >= MAX_PENDING_CLICKS_PER_SEAT) {
      return false;
    }
    this.pending.push({ contest: frame.contest, player: seat, seq: frame.seq, at_ms: nowMs });
    return true;
  }

  /** Take every buffered click, oldest first. */
  drain(): ContestClickInput[] {
    const clicks = this.pending;
    this.pending = [];
    return clicks;
  }

  /**
   * Put back clicks a drive rejected before the engine accepted them, ahead of anything
   * that arrived since. They keep their original stamps, and only clicks of the running
   * contest return, within the same per-seat bound as any other, so a failing engine
   * cannot grow the buffer. A restored click the engine did already accept is rejected
   * there as a stale sequence, so a retry can never count it twice.
   */
  restore(clicks: readonly ContestClickInput[]): void {
    const contest = this.contest;
    if (!contest) return;
    const restored: ContestClickInput[] = [];
    const perSeat = new Map<PlayerId, number>();
    for (const click of [...clicks, ...this.pending]) {
      if (click.contest !== contest.id) continue;
      const kept = perSeat.get(click.player) ?? 0;
      if (kept >= MAX_PENDING_CLICKS_PER_SEAT) continue;
      perSeat.set(click.player, kept + 1);
      restored.push(click);
    }
    this.pending = restored;
  }

  private takeToken(seat: PlayerId, nowMs: number): boolean {
    const bucket = this.buckets.get(seat) ?? { tokens: CLICK_BURST, lastMs: nowMs };
    const elapsed = Math.max(0, nowMs - bucket.lastMs);
    bucket.tokens = Math.min(CLICK_BURST, bucket.tokens + (elapsed * MAX_CLICKS_PER_SECOND) / 1000);
    bucket.lastMs = Math.max(bucket.lastMs, nowMs);
    const allowed = bucket.tokens >= 1;
    if (allowed) bucket.tokens -= 1;
    this.buckets.set(seat, bucket);
    return allowed;
  }
}
