import { findPrintingById, pickOldestPrinting } from "./scryfall.ts";
import type { PrintingEntry } from "./scryfall.ts";
// Type-only, so no runtime dependency is created from `services/` back into
// `hooks/`. `services/deckParser.ts` already sources `SourcePrinting` the same
// way — the interface stays declared next to the hook that consumes it.
import type { SourcePrinting } from "../hooks/useCardImage.ts";
import type { ArtChainEntry, CardArtOverride } from "../stores/preferencesStore.ts";

/**
 * The printing a deck list's `(SET) NUM` annotation names. Set codes are stored
 * lowercase in `scryfall-printings.json` but arrive uppercase from deck lists.
 */
function matchSourcePrinting(
  printings: PrintingEntry[],
  source: SourcePrinting,
): PrintingEntry | null {
  const setLower = source.setCode.toLowerCase();
  return printings.find(
    (p) => p.set === setLower && p.collector_number === source.collectorNumber,
  ) ?? null;
}

function applyChainEntry(
  entry: ArtChainEntry,
  printings: PrintingEntry[],
  source?: SourcePrinting,
): PrintingEntry | null {
  switch (entry.type) {
    case "set":
      return printings.find((p) => p.set === entry.setCode) ?? null;
    case "newest":
      return printings[0];
    case "oldest":
      return pickOldestPrinting(printings);
    case "prefer_borderless":
      return printings.find((p) => p.border_color === "borderless") ?? null;
    case "prefer_extended":
      return printings.find((p) => p.frame_effects.includes("extendedart")) ?? null;
    case "source_printing":
      return source ? matchSourcePrinting(printings, source) : null;
  }
}

/**
 * Walk the user's ordered art chain and return the first printing an entry
 * matches, or `null` when no entry matches (including an empty printings list).
 */
export function applyChain(
  chain: ArtChainEntry[],
  printings: PrintingEntry[],
  source?: SourcePrinting,
): PrintingEntry | null {
  if (printings.length === 0) return null;
  for (const entry of chain) {
    const match = applyChainEntry(entry, printings, source);
    if (match) return match;
  }
  return null;
}

/**
 * The printing the app will display for this card under stored preferences.
 *
 * Single authority for "which printing": `useCardImage` renders what this
 * decides, so any consumer that needs to know the same answer ahead of render
 * (a downloader planning which images to cache, for instance) must ask here
 * rather than re-deriving the rule.
 *
 * Models branches 2-4 of `useCardImage`'s precedence. It does NOT model the
 * `scryfallId` prop (branch 1) — that is a per-render caller argument naming a
 * printing the UI is already showing, not a stored preference.
 *
 * `null` means no stored preference resolves to a printing; the caller falls
 * back to the canonical `scryfall-data` entry.
 */
export function selectedPrinting(
  oracleId: string,
  printings: PrintingEntry[],
  artChain: ArtChainEntry[],
  artOverrides: Record<string, CardArtOverride>,
  source?: SourcePrinting,
): PrintingEntry | null {
  const override = artOverrides[oracleId];
  if (override) {
    const pinned = findPrintingById(printings, override.scryfallId);
    if (pinned) return pinned;
    // An override that resolves to nothing does NOT fall through to the chain:
    // the hook's `else if (artOverrides[oracleId])` gate fires on key presence,
    // so a stale pin (the printings data was regenerated and that id vanished)
    // consumes the branch and leaves `overrideUrl` null. What renders next is
    // decided by the async path, which is handed the source printing only when
    // the chain is empty — so an empty chain still shows the deck's printing,
    // while any configured chain falls through to canonical art.
    return artChain.length === 0 && source ? matchSourcePrinting(printings, source) : null;
  }
  // `source` is passed unconditionally: only a `source_printing` entry reads
  // it, so a chain without one is unaffected. The hook guards this call with
  // `artChain.some(...)` because there the guard selects between two different
  // caches, not between two different results.
  if (artChain.length > 0) return applyChain(artChain, printings, source);
  if (source) return matchSourcePrinting(printings, source);
  return null;
}
