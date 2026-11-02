import type { ContestKind, ContestResult, ContestState, PlayerId } from "../../adapter/types.ts";
import type { GameMode } from "../../stores/gameStore.ts";

/** How long the eye's closing beat lasts once the engine has settled the contest.
 *  Shorter than the result banner, so the banner has the last word. */
export const STARE_FINALE_MS = 2400;

/** Time constant of the strain curve. Cosmetic: long enough that a marathon keeps
 *  getting worse, short enough that a typical contest against the AI visibly strains. */
const STRAIN_TIME_CONSTANT_MS = 10_000;

/** Who blinks when the contest ends. The eye stands for the other side of the stare,
 *  so anyone who is not the one side facing it sees the eye give in. */
export type StareFinale = "EyeBlinks" | "ViewerBlinks" | "BothBlink";

/** What the eye is doing right now. */
export type StarePose = "Preparing" | "Staring" | StareFinale;

/**
 * The contestants this client plays: its own seat, or — in human hotseat, where one
 * client seats every non-AI player — each of them. The established seating
 * configuration (`aiSeatIds`, `gameMode`) decides; nothing is guessed from who is
 * "not me".
 */
export function localContestSeats(
  seats: readonly PlayerId[],
  gameMode: GameMode | null,
  me: PlayerId,
  aiSeatIds: readonly PlayerId[],
): PlayerId[] {
  if (gameMode === "local") return seats.filter((seat) => !aiSeatIds.includes(seat));
  return seats.filter((seat) => seat === me);
}

/** Milliseconds of unpaused Active play as of this snapshot plus the local time since
 *  it arrived. The host's clock is never compared against ours; a resume shifts
 *  `go_at_ms` by the outage, so the elapsed time carries straight across a pause. */
export function stareElapsedMs(
  contest: Pick<ContestState, "clock_ms" | "go_at_ms" | "paused_since_ms">,
  localMsSinceSnapshot: number,
): number {
  if (contest.go_at_ms == null) return 0;
  const until = contest.paused_since_ms ?? contest.clock_ms + Math.max(0, localMsSinceSnapshot);
  return Math.max(0, until - contest.go_at_ms);
}

/** Visual strain in `[0, 1)`: climbs fast, then ever more slowly, and never peaks, so
 *  the eye has somewhere left to go however long the contest runs. Display-only; the
 *  result is decided by the engine and never reads this. */
export function stareStrain(elapsedMs: number): number {
  return 1 - Math.exp(-Math.max(0, elapsedMs) / STRAIN_TIME_CONSTANT_MS);
}

/** The closing beat for a settled contest. Display-only reading of the engine's result:
 *  a cancelled contest had no loser, so nobody blinks. Only a screen with exactly one
 *  contestant at it has a viewer to blink: spectators have none, and hotseat's two
 *  players share the eye between them, so for both the eye gives in whoever lost. */
export function stareFinale(
  announcement: {
    kind: ContestKind;
    participants: readonly PlayerId[];
    result: ContestResult;
  },
  localSeats: readonly PlayerId[],
): StareFinale | null {
  const { kind, participants, result } = announcement;
  if (kind !== "Staring" || "Cancelled" in result) return null;
  const facing = participants.filter((seat) => localSeats.includes(seat));
  if (facing.length !== 1) return "EyeBlinks";
  if (result.Outcome === "Tie") return "BothBlink";
  return result.Outcome.Decided.loser === facing[0] ? "ViewerBlinks" : "EyeBlinks";
}

/** The pose of a running contest. */
export function stareContestPose(contest: ContestState): StarePose {
  return contest.phase === "Pending" || contest.phase === "Preparing" ? "Preparing" : "Staring";
}
