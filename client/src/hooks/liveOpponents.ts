/** A player as the resumable-match summary sees it (a structural subset of the
 *  engine `Player`, whose `is_eliminated` is optional on the wire — an absent
 *  flag means "not eliminated"). */
export interface ResumableSummaryPlayer {
  id: number;
  is_eliminated?: boolean;
}

/**
 * Number of live opponents for the local human (seat 0) in a resumable match.
 *
 * Per CR 800.4 an eliminated player is out, so opponents are the live seats
 * other than you. Only subtract the local seat when it is itself still alive:
 * if seat 0 has been eliminated but the match is still resumable (a multi-seat
 * local / FFA game), it is not among the live seats, so every live seat is an
 * opponent and there is nothing to subtract.
 */
export function countLiveOpponents(players: ResumableSummaryPlayer[]): number {
  const you = players.find((p) => p.id === 0);
  const liveCount = players.filter((p) => !p.is_eliminated).length;
  return liveCount - (you && !you.is_eliminated ? 1 : 0);
}
