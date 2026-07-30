import { describe, expect, it } from "vitest";
import { hexToRgb } from "../particleEffects";

describe("hexToRgb", () => {
  it("parses a 6-digit hex color", () => {
    expect(hexToRgb("#ff8800")).toEqual({ r: 255, g: 136, b: 0 });
    expect(hexToRgb("00ff00")).toEqual({ r: 0, g: 255, b: 0 }); // tolerant of a missing '#'
  });

  it("expands 3-digit shorthand instead of producing a NaN channel", () => {
    // Without expansion, the blue channel is parseInt("", 16) === NaN, yielding a
    // corrupt rgb(...) color. `#fff` must expand to `ffffff`.
    const c = hexToRgb("#fff");
    expect(Number.isNaN(c.b)).toBe(false);
    expect(c).toEqual({ r: 255, g: 255, b: 255 });
    expect(hexToRgb("#0af")).toEqual({ r: 0, g: 170, b: 255 });
  });
});
