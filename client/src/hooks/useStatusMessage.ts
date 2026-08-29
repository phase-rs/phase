import { useEffect, useState } from "react";

import { fetchStatus, isStatusLive, type StatusMessage } from "../services/status";
import { usePreferencesStore } from "../stores/preferencesStore";

/** Poll cadence. The object is served with `max-age=60`, so a 60s poll bounds
 * how long a newly published (or cleared) message stays invisible at roughly two
 * minutes — poll and TTL drift independently — at negligible cost for a ~300
 * byte object. Polls are suppressed entirely while the tab is hidden. */
const POLL_INTERVAL_MS = 60_000;

/**
 * Drives the operator status banner: the currently renderable message, or null.
 *
 * Null covers every "show nothing" case uniformly — nothing published, the
 * fetch failed, the message expired, or this player dismissed this exact id.
 *
 * Expiry is re-evaluated against a FRESH clock on every tick rather than only
 * when the payload changes: inside the `max-age=60` freshness window a poll is
 * served straight from the HTTP cache and returns a byte-identical payload, so
 * payload-identity memoization would leave an expired message on screen forever
 * in a tab left open.
 */
export function useStatusMessage(): StatusMessage | null {
  // Selector, not getState(): the store write from the dismiss button must
  // re-render this hook's consumer, otherwise the banner would not hide.
  const dismissedId = usePreferencesStore((s) => s.dismissedStatusId);
  const [message, setMessage] = useState<StatusMessage | null>(null);
  // Re-stamped on every completed poll — this is what re-evaluates expiry.
  const [checkedAt, setCheckedAt] = useState(() => Date.now());

  useEffect(() => {
    let active = true;
    const poll = () => {
      // A hidden tab has no one to show the banner to; skip the round trip.
      if (document.visibilityState !== "visible") return;
      void fetchStatus().then((next) => {
        if (!active) return;
        setMessage(next);
        setCheckedAt(Date.now());
      });
    };

    poll();
    const timer = setInterval(poll, POLL_INTERVAL_MS);
    // Doubles as the catch-up fetch: a tab that was hidden across one or more
    // suppressed ticks refreshes the moment it comes back.
    document.addEventListener("visibilitychange", poll);
    return () => {
      active = false;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", poll);
    };
  }, []);

  if (!message) return null;
  // Equality, not >=: a NEW id after a dismissal re-shows the banner.
  if (message.id === dismissedId) return null;
  return isStatusLive(message, checkedAt) ? message : null;
}
