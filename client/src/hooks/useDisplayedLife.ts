import { useAnimationStore } from "../stores/animationStore.ts";
import { useGameStore } from "../stores/gameStore.ts";

/**
 * The life total to render for a player right now.
 *
 * `snapshotLife` is the committed engine snapshot's value and remains the
 * authority: this only substitutes an engine-reported total from a hit that has
 * already landed on screen but whose snapshot has not been committed yet, which
 * is the whole animation window of a multi-hit combat. Every life readout goes
 * through here so they all tick on the same beat.
 */
export function useDisplayedLife(playerId: number, snapshotLife: number): number {
  const displayedLife = useAnimationStore((state) => state.displayedLife);
  const engineCommitEpoch = useGameStore((state) => state.engineCommitEpoch);

  // Totals recorded before the current snapshot describe an older one; the
  // snapshot itself is then both newer and authoritative.
  if (!displayedLife || displayedLife.epoch !== engineCommitEpoch) {
    return snapshotLife;
  }

  return displayedLife.totals.get(playerId) ?? snapshotLife;
}
