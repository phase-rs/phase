import { useEffect, useRef, useState } from "react";

import { initializeFeeds } from "../services/feedService";
import { PROFILE_REPLACED_EVENT } from "../stores/cloudSyncStore";
import { PROFILE_REPLACEMENT_KEY } from "../constants/storage";

/** Whether the feed generation for the mode currently rendered has settled. */
export function useFeedInitialization(effectiveOffline: boolean): boolean {
  const generationRef = useRef(0);
  const [settledMode, setSettledMode] = useState<boolean | null>(null);
  const [profileRevision, setProfileRevision] = useState(0);

  useEffect(() => {
    const onProfileReplaced = () => setProfileRevision((revision) => revision + 1);
    // A replacement committed by ANOTHER tab bumps PROFILE_REPLACEMENT_KEY via
    // a plain localStorage write, which only fires `storage` in other windows
    // (never this one) — PROFILE_REPLACED_EVENT is same-window only, so without
    // this listener this tab's aborted feed init would never re-run.
    const onStorage = (event: StorageEvent) => {
      if (event.key === PROFILE_REPLACEMENT_KEY) setProfileRevision((revision) => revision + 1);
    };
    window.addEventListener(PROFILE_REPLACED_EVENT, onProfileReplaced);
    window.addEventListener("storage", onStorage);
    return () => {
      window.removeEventListener(PROFILE_REPLACED_EVENT, onProfileReplaced);
      window.removeEventListener("storage", onStorage);
    };
  }, []);

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
  }, [effectiveOffline, profileRevision]);

  return settledMode === effectiveOffline;
}
