const SCRYFALL_ART_HOST = "cards.scryfall.io";
const SCRYFALL_BACK_HOST = "backs.scryfall.io";

/**
 * WebGL may only upload canvas pixels sourced from a CORS-clean image. Phase's
 * normal card faces intentionally use opaque cross-origin <img> requests, so
 * the renderer routes the two fixed Scryfall image hosts through same-origin
 * transports before compositing them.
 */
export function tabletopComposableArtSource(source: string): string {
  try {
    const url = new URL(source, window.location.origin);
    if (url.hostname === SCRYFALL_ART_HOST) {
      return `/card-image-art${url.pathname}${url.search}`;
    }
    if (url.hostname === SCRYFALL_BACK_HOST) {
      return `/card-image-back${url.pathname}${url.search}`;
    }
  } catch {
    return source;
  }
  return source;
}
