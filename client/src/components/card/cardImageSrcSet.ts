import { IMAGE_SIZE_WIDTHS, deriveImageUrl, imageUrlSize } from "../../services/scryfall.ts";
import type { ImageRungs } from "../../services/visualPacks/types.ts";

/**
 * The image-element attributes that offer a card image at both of Scryfall's usable
 * widths. Returned as one object — never as loose attributes — because the four
 * only hold together as a set: `sizes="auto"` is valid *only* alongside
 * `loading="lazy"` (a call site that copied `srcSet` and `sizes` but dropped
 * `loading` would silently revert to always picking the 488px asset, with no
 * error anywhere to catch it), and `sizes="auto"` is only *safe* alongside
 * `width`/`height` (see {@link RUNG_INTRINSIC_SIZE}).
 */
export interface CardImageSrcSetProps {
  srcSet: string;
  sizes: string;
  loading: "lazy";
  width: number;
  height: number;
}

/**
 * The intrinsic size the `<img>` advertises through `width`/`height` while
 * `sizes="auto"` is in play. Without the pair the element has no intrinsic
 * aspect ratio of its own: the spec's 300x150 default object size stands in,
 * and every call site that lets the image supply its own height renders a
 * 150px-tall letterbox slice of the card instead (measured in Chromium 151 on
 * the hover preview: 472x150, where the same markup without these attributes
 * lays out 472x658). Both rungs are the same card scan at two widths, so
 * pinning the `normal` rung's 488x680 — the ratio the client's
 * `aspect-[488/680]` placeholder tiles already encode — fixes the ratio for
 * the whole ladder. CSS still wins wherever a call site sizes the element
 * itself, so the pair only ever supplies the ratio nothing else provides.
 */
const RUNG_INTRINSIC_SIZE = { width: IMAGE_SIZE_WIDTHS.normal, height: 680 } as const;

/**
 * Build the two-rung `srcset` ladder for a card image URL, or `undefined` when
 * the URL has no size variants (face-down backs, test mocks, `art_crop`, the
 * `soon.jpg` placeholder). Consumers spread the result directly, so `undefined`
 * makes React omit all three attributes.
 *
 * Rotated hand cards render ~135 CSS px wide but were served the 488px asset —
 * a 3.6x downscale that Skia resolves with bilinear-without-mipmaps for
 * non-axis-aligned draws, which aliases badly. Offering the 146px asset lets
 * the browser pick it wherever the used width x DPR fits.
 *
 * `sizes="auto"` resolves the element's *used layout width*, so the fan
 * rotation and hover scale are correctly ignored. The `200px` fallback is what
 * browsers without `auto` support read (Safari, Safari iOS; `auto` is
 * Chrome/Edge 126+ and Firefox 132+) — at both DPR 1 and DPR 2 it selects the
 * 488w rung, keeping those browsers byte-identical to today. A bare
 * `sizes="auto"` would be actively wrong: with no `width`/`height` attributes
 * on these elements the spec falls back to a 300x150 default intrinsic size,
 * which reselects 488w and silently reinstates the aliasing. Those attributes
 * ship with the set — see {@link RUNG_INTRINSIC_SIZE}: that same default also
 * leaves the element with no aspect ratio of its own.
 */
export function getCardImageSrcSetProps(
  src: string | null | undefined,
  explicitRungs?: ImageRungs,
): CardImageSrcSetProps | undefined {
  if (!src) return undefined;
  if (explicitRungs?.small && explicitRungs.normal) {
    return {
      srcSet:
        `${explicitRungs.small} ${IMAGE_SIZE_WIDTHS.small}w, `
        + `${explicitRungs.normal} ${IMAGE_SIZE_WIDTHS.normal}w`,
      sizes: "auto, 200px",
      loading: "lazy",
      ...RUNG_INTRINSIC_SIZE,
    };
  }
  const size = imageUrlSize(src);
  // `art_crop` is a crop, not a scaled variant — its rungs are different images.
  if (size === null || size === "art_crop") return undefined;
  return {
    srcSet:
      `${deriveImageUrl(src, "small")} ${IMAGE_SIZE_WIDTHS.small}w, `
      + `${deriveImageUrl(src, "normal")} ${IMAGE_SIZE_WIDTHS.normal}w`,
    sizes: "auto, 200px",
    loading: "lazy",
    ...RUNG_INTRINSIC_SIZE,
  };
}
