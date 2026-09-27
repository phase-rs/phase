import { describe, expect, it } from "vitest";

import { tabletopComposableArtSource } from "../tabletopArtSource.ts";

describe("tabletopComposableArtSource", () => {
  it("routes Scryfall card art through the fixed same-origin transport", () => {
    expect(
      tabletopComposableArtSource(
        "https://cards.scryfall.io/art_crop/front/a/b/example.jpg?123",
      ),
    ).toBe("/card-image-art/art_crop/front/a/b/example.jpg?123");
  });

  it("routes the canonical card back through its own fixed transport", () => {
    expect(
      tabletopComposableArtSource(
        "https://backs.scryfall.io/normal/0/a/example.jpg",
      ),
    ).toBe("/card-image-back/normal/0/a/example.jpg");
  });

  it("leaves same-origin and unrelated image sources unchanged", () => {
    expect(tabletopComposableArtSource("/images/token.png")).toBe(
      "/images/token.png",
    );
    expect(tabletopComposableArtSource("https://example.com/card.jpg")).toBe(
      "https://example.com/card.jpg",
    );
  });
});
