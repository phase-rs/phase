import type { CardImageSource } from "../services/visualPacks/types.ts";

/** Advance one source ladder without retrying a URL that has already failed. */
export function nextImageSourceIndex(
  sources: readonly CardImageSource[],
  sourceIndex: number,
  failedSources: Set<string>,
  failedSrc: string,
): number | null {
  const current = sources[sourceIndex];
  if (!current?.src || current.src !== failedSrc || failedSources.has(failedSrc)) return null;

  failedSources.add(failedSrc);
  let nextIndex = sourceIndex + 1;
  while (true) {
    const nextSrc = sources[nextIndex]?.src;
    if (!nextSrc || !failedSources.has(nextSrc)) break;
    nextIndex += 1;
  }
  return nextIndex;
}
