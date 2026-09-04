import { useEffect, useRef, useState } from "react";

import { initializeFeeds } from "../services/feedService";

/** Whether the feed generation for the mode currently rendered has settled. */
export function useFeedInitialization(effectiveOffline: boolean): boolean {
  const generationRef = useRef(0);
  const [settledMode, setSettledMode] = useState<boolean | null>(null);

  useEffect(() => {
    const generation = ++generationRef.current;
    const controller = new AbortController();
    setSettledMode(null);
    void initializeFeeds({ allowRefresh: !effectiveOffline, signal: controller.signal }).then(() => {
      if (generation === generationRef.current && !controller.signal.aborted) setSettledMode(effectiveOffline);
    }, (err: unknown) => {
      if (
        generation !== generationRef.current ||
        controller.signal.aborted ||
        (err instanceof DOMException && err.name === "AbortError")
      ) return;
      console.error("Feed initialization failed:", err);
      setSettledMode(effectiveOffline);
    });
    return () => controller.abort();
  }, [effectiveOffline]);

  return settledMode === effectiveOffline;
}
