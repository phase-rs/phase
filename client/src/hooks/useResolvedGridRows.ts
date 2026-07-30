import { useIsCompactHeight } from "./useIsCompactHeight.ts";
import {
  usePreferencesStore,
  type CappedTrack,
  type GridBands,
} from "../stores/preferencesStore.ts";

/** Render one grid band as a CSS track. Desktop caps the band at
 *  `min(pct%, pxCap px)` (mirroring the prior hardcoded `minmax(0,min(12%,100px))`);
 *  compact-height mode drops the px cap, matching the prior compact layout. */
function band(track: CappedTrack, isCompactHeight: boolean): string {
  return isCompactHeight
    ? `minmax(0,${track.pct}%)`
    : `minmax(0,min(${track.pct}%,${track.pxCap}px))`;
}

/** Pure resolver: {@link GridBands} → a CSS `grid-template-rows` value. The
 *  middle (battlefield) row is always `1fr` and absorbs the remainder, so only
 *  the top/bottom bands are configurable. Exported so the byte-identical-to-today
 *  regression can be asserted directly in tests. */
export function resolveGridRows(bands: GridBands, isCompactHeight: boolean): string {
  return `${band(bands.top, isCompactHeight)} 1fr ${band(bands.bottom, isCompactHeight)}`;
}

export function resolveSplitGridRows(bands: GridBands, isCompactHeight: boolean): string {
  return `0px 1fr ${band(bands.bottom, isCompactHeight)}`;
}

/** Hook form: reads the persisted bands and the live compact-height media query,
 *  returning the `grid-template-rows` string for the board grid. Mirrors the
 *  `useResolvedCommandZoneDisplay` resolve-at-use-site precedent. The default
 *  layout resolves byte-identical to the prior hardcoded value. */
export function useResolvedGridRows(): string {
  const bands = usePreferencesStore((s) => s.flexLayout.gridBands);
  const isCompactHeight = useIsCompactHeight();
  return resolveGridRows(bands, isCompactHeight);
}

export function useResolvedSplitGridRows(): string {
  const bands = usePreferencesStore((s) => s.flexLayout.gridBands);
  const isCompactHeight = useIsCompactHeight();
  return resolveSplitGridRows(bands, isCompactHeight);
}
