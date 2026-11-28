import { useAnimationStore } from "../stores/animationStore";
import { usePreferencesStore } from "../stores/preferencesStore";

/**
 * The one pace presentation runs at: the Director's applied pace (speed
 * preference × stack pressure) while a paced run is playing, and the speed
 * preference otherwise (a direct enqueue, or before any dispatch).
 *
 * Both subscriptions are unconditional so hook order never changes.
 */
export function usePresentationPace(): number {
  const paced = useAnimationStore((state) => state.pace);
  const speedPreference = usePreferencesStore((state) => state.animationSpeedMultiplier);
  return paced ?? speedPreference;
}
