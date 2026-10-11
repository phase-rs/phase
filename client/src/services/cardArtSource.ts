import { usePreferencesStore, type CardArtSource } from "../stores/preferencesStore.ts";

/**
 * Chinese-locale card-art provider: 大学院废墟 (mtgch.com, also served as
 * sbwsz.com).
 *
 * WHY THIS IS A URL REWRITE AND NOT A LOOKUP TABLE
 * ------------------------------------------------
 * mtgch mirrors the official Simplified-Chinese printing of an *English Scryfall
 * printing* under the SAME Scryfall printing id — verified by comparing its API
 * record for LEA 161 against Scryfall's (`scryfall_id` identical on both sides).
 * That makes the whole mapping derivable from a URL the client already holds:
 * swap the host and the language path segment, keep the id. No sidecar map, no
 * per-card fetch, and the player's chosen printing and face carry over intact —
 * which is exactly what the Scryfall-art locale maps
 * (`scripts/gen-scryfall-locale-images.sh`) need a join for, and cannot do for
 * Chinese at all because Scryfall serves no Chinese printings under these ids.
 *
 * The CDN serves `webp` (the Scryfall art is `jpg`), which every browser this
 * app targets decodes. The path shape is identical to Scryfall's, so
 * `deriveImageUrl`'s size rewriting keeps working on a rewritten URL.
 */
export const MTGCH_IMAGE_HOST = "images.mtgch.com";

/**
 * Verified live against the mtgch API's own `zhs_image.image_uris` for multiple
 * printings. `normal` is 488x680 — the same nominal size Scryfall serves — so
 * the two providers are interchangeable at a given rung of the art ladder.
 */
const MTGCH_IMAGE_EXTENSION = "webp";

/** The three sizes mtgch publishes per face. */
type MtgchSize = "small" | "normal" | "large";

/**
 * mtgch publishes no `art_crop` rendition for Chinese printings — the endpoint
 * is a hard 404, verified live, while the English `/sf/art_crop/` path returns
 * 200. `art_crop` is the battlefield's DEFAULT display (`battlefieldCardDisplay`),
 * so this is not a corner case: a Chinese player would otherwise see a broken
 * image on most of the board.
 *
 * Falling back to Scryfall's English art crop is the deliberate choice. The
 * alternatives both mislead: serving `normal` in a crop slot would letterbox a
 * full card into an art-shaped frame, and serving nothing would replace readable
 * English art with a broken tile. Art is the book's cover, not its text — the
 * card's *wording* still comes from the Chinese sidecar — so an English crop
 * here loses far less than a hole in the board.
 */
const SIZES_WITHOUT_CHINESE_ART: readonly string[] = ["art_crop"];

/** Scryfall's URL shape, as consumed by `deriveImageUrl`/`splitSizedImageUrl`. */
const SCRYFALL_URL_PATTERN =
  /^https?:\/\/[^/]+\/(small|normal|large|art_crop)\/(front|back)\/([0-9a-f])\/([0-9a-f])\/([0-9a-f-]{36})(?:\.\w+)?(?:\?.*)?$/;

/**
 * Rewrite one Scryfall image URL to the same printing's Chinese art, or return
 * it unchanged when the URL is not a shape this provider can serve.
 *
 * Returning the INPUT unchanged (rather than null/undefined) is load-bearing:
 * callers thread the result straight into the existing art ladder, and a
 * fallback that produced `undefined` would read downstream as "this card has no
 * art", short-circuiting the printing-fallback chain that exists for cards whose
 * chosen printing genuinely lacks an image.
 */
export function applyCardArtSource(url: string, source: CardArtSource): string {
  if (source !== "mtgch") return url;

  const match = SCRYFALL_URL_PATTERN.exec(url);
  if (!match) return url;

  const [, size, face, idHead, idTail, printingId] = match;
  // No Chinese rendition exists for this size; keep the English original rather
  // than synthesizing a URL that 404s. See SIZES_WITHOUT_CHINESE_ART.
  if (SIZES_WITHOUT_CHINESE_ART.includes(size)) return url;

  // The lowercased id is load-bearing for the CDN's sharded path. Scryfall ids
  // are already lowercase, but a caller that uppercased one would otherwise
  // produce a URL that silently 404s, so the case is normalized here rather than
  // assumed.
  const head = idHead.toLowerCase();
  const tail = idTail.toLowerCase();
  return `https://${MTGCH_IMAGE_HOST}/zhs/${size as MtgchSize}/${face}/${head}/${tail}/${printingId.toLowerCase()}.${MTGCH_IMAGE_EXTENSION}`;
}

/**
 * Resolve the provider from the two preferences that decide it.
 *
 * Chinese art requires BOTH the Chinese UI language and the (default-on)
 * Chinese-art switch. The language is load-bearing rather than redundant:
 * `chineseCardArt` defaults to on, so keying off the flag alone would rewrite
 * every card URL to mtgch for a player using the app in English or German —
 * showing them Chinese cards because of a preference they never set. The
 * documented contract of the setting is "when the UI is Chinese, default to
 * Chinese art", and this is where that sentence is enforced.
 */
function resolveActiveSource(): CardArtSource {
  const { language, chineseCardArt } = usePreferencesStore.getState();
  return language === "zh" && chineseCardArt ? "mtgch" : "scryfall";
}

/**
 * The provider currently in force.
 *
 * Held as module state, with this module owning the store subscription, so that
 * `services/scryfall.ts` can consult the provider WITHOUT importing the
 * preferences store. That is not tidiness: `stores/preferencesStore.ts` imports
 * `services/engineRuntime.ts`, which imports `services/scryfall.ts`, so a store
 * import from `scryfall.ts` closes the loop
 * `engineRuntime -> scryfall -> preferencesStore -> engineRuntime` and makes
 * whichever module the bundler evaluates first observe the others mid-
 * initialization. Keeping the store edge confined to this leaf module breaks
 * that cycle. `services/scryfall.ts` imports only the pure
 * {@link applyCardArtSource} and {@link activeCardArtSource}.
 *
 * Seeded from the store rather than hardcoded, so the module agrees with the
 * store even if the subscription below has not been installed yet (a test that
 * sets the preference before importing this module, or a non-React entry point).
 */
let activeSource: CardArtSource = resolveActiveSource();

usePreferencesStore.subscribe((state, previous) => {
  if (
    state.language !== previous.language ||
    state.chineseCardArt !== previous.chineseCardArt
  ) {
    activeSource = resolveActiveSource();
  }
});

/** The provider every card-art resolution should currently use. */
export function activeCardArtSource(): CardArtSource {
  return activeSource;
}

/** Override the provider for a test. Restores the store-derived value when
 *  passed `null`, so a test cannot leak a source into its neighbours. */
export function setActiveCardArtSourceForTest(source: CardArtSource | null): void {
  activeSource = source ?? resolveActiveSource();
}
