import { afterEach, describe, expect, it, vi } from "vitest";

import { resolveBackground } from "../BattlefieldBackground";
import type { BoardBackground } from "../../../stores/preferencesStore";

describe("resolveBackground", () => {
  afterEach(() => vi.restoreAllMocks());

  it("selects a random playmat for colorless decks in auto-wubrg mode", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const lock = { current: null };

    const background = resolveBackground("auto-wubrg" as BoardBackground, "", null, lock);

    expect(background).toEqual({ kind: "image", src: "/battlefield/air_angelic_sky.webp" });
  });

  it("waits for deck data before locking a colored playmat", () => {
    vi.spyOn(Math, "random").mockReturnValue(0);
    const lock = { current: null };

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", undefined, lock)).toBeNull();

    expect(resolveBackground("auto-wubrg" as BoardBackground, "", "Blue", lock)).toEqual({
      kind: "image",
      src: "/battlefield/water_moonlit_ocean_temple.webp",
    });
  });
});
