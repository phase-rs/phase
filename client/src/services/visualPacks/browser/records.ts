import type { CatalogRoot } from "../types.ts";

export type CacheMedia = "image/jpeg" | "image/svg+xml";
const PATH_PREFIX = "/__visual-" + "packs/sha256/";

/** The sole owner of local browser image URL construction. */
export function syntheticCachePath(object: CatalogRoot, media: CacheMedia): string {
  return `${PATH_PREFIX}${object}.${media === "image/jpeg" ? "jpg" : "svg"}`;
}
