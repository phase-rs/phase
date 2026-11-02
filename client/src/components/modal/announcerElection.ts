import type { AnnouncerElection, WaitingFor } from "../../adapter/types.ts";

/**
 * The engine-provided announcing-opponent election facts, from an ordinary
 * cast (`ChooseAnnouncingOpponent`) or a copy announcement
 * (`CopyRetarget.announcer_election`). Both are answered with the same action.
 */
export function announcerElectionOf(
  waitingFor: WaitingFor | null | undefined,
): AnnouncerElection | null {
  if (waitingFor?.type === "ChooseAnnouncingOpponent") return waitingFor.data;
  if (waitingFor?.type === "CopyRetarget") return waitingFor.data.announcer_election ?? null;
  return null;
}
