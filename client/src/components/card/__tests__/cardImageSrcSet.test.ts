import { describe, expect, it } from "vitest";

import { CARD_BACK_URL } from "../../../services/scryfall.ts";
import { getCardImageSrcSetProps } from "../cardImageSrcSet.ts";

const SLUG = "front/w/r/war-room.jpg?1783905318";
const sized = (size: string) => `https://cards.scryfall.io/${size}/${SLUG}`;

describe("getCardImageSrcSetProps", () => {
  it("offers exactly two rungs, whatever size the source URL is", () => {
    // The regression guard for the 672px `large` asset: adding it would ship
    // ~+51 KB and ~+90% decoded bitmap per card to Safari/iOS.
    for (const size of ["small", "normal", "large"]) {
      const props = getCardImageSrcSetProps(sized(size));

      expect(props?.srcSet.split(",")).toHaveLength(2);
      expect(props?.srcSet).toBe(
        `${sized("small")} 146w, ${sized("normal")} 488w`,
      );
      expect(props?.srcSet).not.toContain("672w");
    }
  });

  it("keeps srcSet, sizes and loading together", () => {
    // `sizes="auto"` is valid only alongside `loading="lazy"`; a site that lost
    // `loading` would silently revert to always selecting the 488px asset.
    expect(getCardImageSrcSetProps(sized("normal"))).toEqual({
      srcSet: `${sized("small")} 146w, ${sized("normal")} 488w`,
      sizes: "auto, 200px",
      loading: "lazy",
    });
  });

  it("returns undefined for sources with no size variants", () => {
    // `art_crop` is a crop rather than a scaled variant — its rungs would be
    // different images, not the same image at two widths.
    expect(getCardImageSrcSetProps(sized("art_crop"))).toBeUndefined();
    // Face-down cards render `CARD_BACK_URL`, and `useCardImage("")` yields "".
    expect(getCardImageSrcSetProps(CARD_BACK_URL)).toBeUndefined();
    expect(getCardImageSrcSetProps("")).toBeUndefined();
    expect(getCardImageSrcSetProps(null)).toBeUndefined();
    expect(getCardImageSrcSetProps(undefined)).toBeUndefined();
    expect(getCardImageSrcSetProps("Focused Opponent Card.png")).toBeUndefined();
  });
});
