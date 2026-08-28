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

  it("keeps srcSet, sizes, loading and the intrinsic size together", () => {
    // `sizes="auto"` is valid only alongside `loading="lazy"`; a site that lost
    // `loading` would silently revert to always selecting the 488px asset.
    expect(getCardImageSrcSetProps(sized("normal"))).toEqual({
      srcSet: `${sized("small")} 146w, ${sized("normal")} 488w`,
      sizes: "auto, 200px",
      loading: "lazy",
      width: 488,
      height: 680,
    });
  });

  it("never offers sizes=auto without the intrinsic size that resolves it", () => {
    // Dropping `width`/`height` leaves the element with no intrinsic aspect
    // ratio: the spec's 300x150 default object size stands in, so every call
    // site that lets the image supply its own height (the hover and mobile
    // previews, the textbox slice, the coverage dashboard) lays out a 2:1
    // letterbox and `object-cover` crops the card to a middle slice.
    for (const props of [
      getCardImageSrcSetProps(sized("normal")),
      getCardImageSrcSetProps("http://visual-pack.localhost/current", {
        small: "http://visual-pack.localhost/small-object",
        normal: "http://visual-pack.localhost/normal-object",
      }),
    ]) {
      expect(props?.sizes).toContain("auto");
      expect(props?.width).toBe(488);
      expect(props?.height).toBe(680);
      // The pair must describe the card scan itself, not an arbitrary box:
      // both rungs are the same image at two widths.
      expect(props!.width / props!.height).toBeCloseTo(488 / 680, 5);
    }
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

  it("uses explicit installed rungs without rewriting an opaque protocol URL", () => {
    expect(getCardImageSrcSetProps("http://visual-pack.localhost/current", {
      small: "http://visual-pack.localhost/small-object",
      normal: "http://visual-pack.localhost/normal-object",
    })).toEqual({
      srcSet: "http://visual-pack.localhost/small-object 146w, http://visual-pack.localhost/normal-object 488w",
      sizes: "auto, 200px",
      loading: "lazy",
      width: 488,
      height: 680,
    });
    expect(getCardImageSrcSetProps("http://visual-pack.localhost/current")).toBeUndefined();
  });
});
