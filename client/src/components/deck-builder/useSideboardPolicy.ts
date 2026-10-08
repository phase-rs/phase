import { useEffect, useState } from "react";

import type { FormatConfig } from "../../adapter/types";
import { DECK_CONSTRUCTION_FORMATS } from "../../data/formatRegistry";
import {
  sideboardPolicyForFormat,
  type SideboardPolicy,
} from "../../services/engineRuntime";

/**
 * Map the lowercase deck-builder format string (e.g. "standard", "commander")
 * to the engine's resolved `FormatConfig` for that format. Derived from the
 * engine-authored deck-construction formats so adding a deck format is
 * automatic here. Returns the whole config, not a bare `GameFormat`: the
 * sideboard policy is a stored field on the config, and only the config can
 * carry a custom format's declared policy.
 */
function mapToEngineFormatConfig(format: string | undefined): FormatConfig | null {
  if (!format) return null;
  const lower = format.toLowerCase();
  const match = DECK_CONSTRUCTION_FORMATS.find((m) => m.format.toLowerCase() === lower);
  return match?.default_config ?? null;
}

/**
 * Used only when the deck's format string doesn't resolve to a known
 * GameFormat (e.g. user-imported "casual" labels). Constructed formats are
 * the common case for unfamiliar labels, so Limited(15) is the right default.
 */
const FALLBACK_CONSTRUCTED_POLICY: SideboardPolicy = { type: "Limited", data: 15 };

/**
 * Resolve a format's sideboard policy from the engine. CR 100.4a: the engine
 * is the single authority for format rules; the frontend only renders what it
 * reports. Shared by the list and visual deck views so both stay consistent.
 */
export function useSideboardPolicy(format: string | undefined): SideboardPolicy {
  const [policy, setPolicy] = useState<SideboardPolicy>(FALLBACK_CONSTRUCTED_POLICY);
  useEffect(() => {
    const engineFormatConfig = mapToEngineFormatConfig(format);
    if (!engineFormatConfig) {
      setPolicy(FALLBACK_CONSTRUCTED_POLICY);
      return;
    }
    let cancelled = false;
    sideboardPolicyForFormat(engineFormatConfig)
      .then((next) => {
        if (!cancelled) setPolicy(next);
      })
      .catch(() => {
        if (!cancelled) setPolicy(FALLBACK_CONSTRUCTED_POLICY);
      });
    return () => {
      cancelled = true;
    };
  }, [format]);
  return policy;
}

/**
 * Formats that forbid a competitive sideboard (Commander, Brawl) still get a
 * builder-only "Maybeboard" staging area in this app — a place to park cards
 * you're considering. The engine's Forbidden policy governs what's legal to
 * play; this only changes how the second section is labelled and offered.
 */
export function isMaybeboardPolicy(policy: SideboardPolicy): boolean {
  return policy.type === "Forbidden";
}
