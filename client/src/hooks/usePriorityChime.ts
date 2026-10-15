import { useEffect, useRef } from "react";

import { audioManager } from "../audio/AudioManager";
import { usePreferencesStore } from "../stores/preferencesStore";
import { usePriorityAttention } from "./usePriorityAttention";

/** Mount once: consume each announcement, even when its sound is disabled. */
export function usePriorityChime(): void {
  const { cue, fireCount } = usePriorityAttention();
  const soundEnabled = usePreferencesStore((s) => s.priorityAlertSound);
  const previousCount = useRef(fireCount);
  const pending = useRef<AbortController | null>(null);

  useEffect(() => {
    const increased = fireCount > previousCount.current;
    previousCount.current = fireCount;
    if (increased && soundEnabled) {
      pending.current?.abort();
      pending.current = new AbortController();
      audioManager.playCue("priority", pending.current.signal);
    }
  }, [fireCount, soundEnabled]);

  // A cue still waiting on a suspended audio context must not sound once the
  // announcement is over, the sound is switched off, or the game screen is gone.
  useEffect(() => {
    if (cue === "none" || !soundEnabled) pending.current?.abort();
  }, [cue, soundEnabled]);
  useEffect(() => () => pending.current?.abort(), []);
}
