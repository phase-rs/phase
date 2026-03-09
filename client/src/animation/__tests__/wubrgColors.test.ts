import { describe, expect, it } from "vitest";

import { getCardColors, WUBRG_COLORS } from "../wubrgColors";

describe("WUBRG_COLORS", () => {
  it("maps each mana color to its hex value", () => {
    expect(WUBRG_COLORS.White).toBe("#fbbf24");
    expect(WUBRG_COLORS.Blue).toBe("#06b6d4");
    expect(WUBRG_COLORS.Black).toBe("#a855f7");
    expect(WUBRG_COLORS.Red).toBe("#ef4444");
    expect(WUBRG_COLORS.Green).toBe("#22c55e");
    expect(WUBRG_COLORS.Colorless).toBe("#94a3b8");
  });
});

describe("getCardColors", () => {
  it("returns colorless for empty array", () => {
    expect(getCardColors([])).toEqual(["#94a3b8"]);
  });

  it("returns single color hex", () => {
    expect(getCardColors(["Red"])).toEqual(["#ef4444"]);
  });

  it("returns multiple color hexes in order", () => {
    expect(getCardColors(["White", "Blue"])).toEqual(["#fbbf24", "#06b6d4"]);
  });

  it("handles all five colors", () => {
    expect(getCardColors(["White", "Blue", "Black", "Red", "Green"])).toEqual([
      "#fbbf24",
      "#06b6d4",
      "#a855f7",
      "#ef4444",
      "#22c55e",
    ]);
  });
});
