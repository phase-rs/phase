import { useEffect, useState } from "react";

import { loadPrintingsData } from "../services/scryfall.ts";

let resolved = false;

export function usePrintingsLoaded(): boolean {
  const [loaded, setLoaded] = useState(resolved);

  useEffect(() => {
    // Module-level `resolved` is the load-once guard; depend on [] so the effect
    // runs a single time on mount instead of re-subscribing after setLoaded(true)
    // only to immediately bail. Guard the async setState against unmount.
    if (resolved) return;
    let cancelled = false;
    loadPrintingsData().then((data) => {
      if (data && !cancelled) {
        resolved = true;
        setLoaded(true);
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return loaded;
}
