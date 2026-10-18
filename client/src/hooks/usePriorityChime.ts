import { useEffect, useRef } from "react";

import { audioManager } from "../audio/AudioManager";
import { usePreferencesStore } from "../stores/preferencesStore";
import { usePriorityAttention } from "./usePriorityAttention";

/** Mount once: consume each chime the controller grants, even when sound is disabled. */
export function usePriorityChime(): void {
  const { cue, chimeCount } = usePriorityAttention();
  const soundEnabled = usePreferencesStore((s) => s.priorityAlertSound);
  const previousCount = useRef(chimeCount);
  const pending = useRef<AbortController | null>(null);

  useEffect(() => {
    const increased = chimeCount > previousCount.current;
    previousCount.current = chimeCount;
    if (increased && soundEnabled) {
      pending.current?.abort();
      pending.current = new AbortController();
      audioManager.playCue("priority", pending.current.signal);
    }
  }, [chimeCount, soundEnabled]);

  // A cue still waiting on a suspended audio context must not sound once the
  // window is answered, the sound is switched off, or the game screen is gone.
  useEffect(() => {
    if (cue === "none" || !soundEnabled) pending.current?.abort();
  }, [cue, soundEnabled]);
  useEffect(() => () => pending.current?.abort(), []);
}
