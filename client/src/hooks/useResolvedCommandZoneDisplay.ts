import { useEffect, useState } from "react";

import { usePreferencesStore } from "../stores/preferencesStore.ts";

/** Below this viewport width (px), an always-visible inline command dock crowds
 *  the board, so "auto" collapses it to the compact pile. */
const COMMAND_ZONE_COMPACT_WIDTH = 900;
/** Short viewports (landscape phones) also collapse — mirrors the height
 *  threshold `useIsCompactHeight` uses for the rest of the compact layout. */
const COMMAND_ZONE_COMPACT_HEIGHT = 500;

export type ResolvedCommandZoneDisplay = "compact" | "inline";

function autoResolve(): ResolvedCommandZoneDisplay {
  if (typeof window === "undefined") return "inline";
  return window.innerWidth < COMMAND_ZONE_COMPACT_WIDTH
    || window.innerHeight < COMMAND_ZONE_COMPACT_HEIGHT
    ? "compact"
    : "inline";
}

/**
 * Resolves the user's command-zone display preference to a concrete mode.
 * Explicit "compact"/"inline" pass through unchanged; "auto" is resolved by the
 * viewport (compact on narrow or short screens, inline otherwise). Mirrors the
 * `boardBackground` "auto-wubrg" precedent: the store holds the user's choice,
 * the use-site resolves it to a concrete value. The resize listener is only
 * attached while the preference is "auto", so explicit modes never re-render on
 * resize.
 */
export function useResolvedCommandZoneDisplay(): ResolvedCommandZoneDisplay {
  const preference = usePreferencesStore((s) => s.commandZoneDisplay);
  const [autoMode, setAutoMode] = useState<ResolvedCommandZoneDisplay>(autoResolve);

  useEffect(() => {
    if (preference !== "auto") return;
    function handleResize() {
      setAutoMode(autoResolve());
    }
    handleResize();
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [preference]);

  return preference === "auto" ? autoMode : preference;
}
