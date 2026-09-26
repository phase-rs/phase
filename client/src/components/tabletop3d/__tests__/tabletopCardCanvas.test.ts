import { describe, expect, it } from "vitest";

import {
  TABLETOP_STAT_INK,
  tabletopCardStatText,
  tabletopFrameLetter,
  drawTabletopStatText,
  fitTabletopStatFontSize,
  fitTabletopCardTitle,
  tokenizeTabletopRulesParagraph,
} from "../tabletopCardCanvas.ts";

describe("tabletopCardStatText", () => {
  it("uses narrow typographic spacing around the P/T separator", () => {
    expect(
      tabletopCardStatText({
        damageMarked: 1,
        loyalty: null,
        power: 12,
        toughness: 13,
      }),
    ).toBe("12\u200a/\u200a12");
  });
});

describe("tabletopFrameLetter", () => {
  it("selects the matching monocolor frame", () => {
    expect(
      tabletopFrameLetter({
        colors: ["Green"],
        typeLine: "Creature — Squirrel",
      }),
    ).toBe("G");
  });

  it("uses dedicated land, artifact, colorless, and multicolor frames", () => {
    expect(tabletopFrameLetter({ colors: [], typeLine: "Basic Land — Forest" }))
      .toBe("L");
    expect(tabletopFrameLetter({ colors: [], typeLine: "Artifact" })).toBe("A");
    expect(tabletopFrameLetter({ colors: [], typeLine: "Creature — Eldrazi" }))
      .toBe("V");
    expect(
      tabletopFrameLetter({
        colors: ["Blue", "Red"],
        typeLine: "Legendary Creature",
      }),
    ).toBe("M");
  });
});

describe("fitTabletopCardTitle", () => {
  const measure = (text: string, fontSize: number) =>
    Array.from(text).length * fontSize;

  it("uses the largest title size that fits without horizontal compression", () => {
    expect(fitTabletopCardTitle("Sol Ring", 480, measure)).toEqual({
      fontSize: 58,
      text: "Sol Ring",
    });
  });

  it("ellipsizes names that do not fit at the minimum title size", () => {
    const layout = fitTabletopCardTitle(
      "A Very Long Legendary Permanent Name",
      460,
      measure,
    );

    expect(layout.fontSize).toBe(46);
    expect(layout.text.endsWith("…")).toBe(true);
    expect(measure(layout.text, layout.fontSize)).toBeLessThanOrEqual(460);
  });
});

describe("drawTabletopStatText", () => {
  it("uses a rounded high-contrast field and light serif numerals", () => {
    const calls: string[] = [];
    const textPositions: Array<[number, number]> = [];
    const context = {
      beginPath: () => calls.push("beginPath"),
      fill: () => calls.push("fill"),
      fillText: (text: string, x: number, y: number) => {
        calls.push(`fillText:${text}`);
        textPositions.push([x, y]);
      },
      measureText: () => ({
        actualBoundingBoxAscent: 48,
        actualBoundingBoxDescent: 12,
        actualBoundingBoxLeft: 4,
        actualBoundingBoxRight: 96,
        width: 100,
      }),
      restore: () => calls.push("restore"),
      roundRect: () => calls.push("roundRect"),
      save: () => calls.push("save"),
      stroke: () => calls.push("stroke"),
      fillStyle: "",
      font: "",
      lineJoin: "miter",
      lineWidth: 1,
      strokeStyle: "",
      textAlign: "left",
      textBaseline: "alphabetic",
    } as unknown as CanvasRenderingContext2D;

    drawTabletopStatText(context, "12/12");

    expect(calls[0]).toBe("save");
    expect(calls.filter((call) => call === "beginPath")).toHaveLength(2);
    expect(calls.filter((call) => call === "roundRect")).toHaveLength(2);
    expect(calls.filter((call) => call === "fill")).toHaveLength(2);
    expect(calls.filter((call) => call === "stroke")).toHaveLength(2);
    expect(calls).toContain("fillText:12/12");
    expect(calls[calls.length - 1]).toBe("restore");
    const fontSize = Number(context.font.match(/600 ([\d.]+)px/)?.[1]);
    expect(fontSize).toBeCloseTo(52.68, 1);
    expect(textPositions[0]?.[0]).toBeCloseTo(
      (0.762 + 0.198 * 0.51) * 1005 - 46,
    );
    expect(textPositions[0]?.[1]).toBeCloseTo(
      (0.888 + 0.072 * 0.44) * 1407 + 18,
    );
  });

  it("colors increased and decreased numerals independently", () => {
    const painted: Array<{ ink: string; text: string }> = [];
    const context = {
      beginPath: () => undefined,
      fill: () => undefined,
      fillText: (text: string) => {
        painted.push({ ink: String(context.fillStyle), text });
      },
      measureText: (text: string) => ({
        actualBoundingBoxAscent: 48,
        actualBoundingBoxDescent: 12,
        actualBoundingBoxLeft: 0,
        actualBoundingBoxRight: text.length * 20,
        width: text.length * 20,
      }),
      restore: () => undefined,
      roundRect: () => undefined,
      save: () => undefined,
      stroke: () => undefined,
      fillStyle: "",
      font: "",
      lineJoin: "miter",
      lineWidth: 1,
      strokeStyle: "",
      textAlign: "left",
      textBaseline: "alphabetic",
    } as unknown as CanvasRenderingContext2D;

    drawTabletopStatText(
      context,
      "5\u200a/\u200a2",
      undefined,
      { power: "blue", toughness: "red" },
    );

    expect(painted).toEqual([
      { ink: TABLETOP_STAT_INK.blue, text: "5\u200a" },
      { ink: TABLETOP_STAT_INK.white, text: "/" },
      { ink: TABLETOP_STAT_INK.red, text: "\u200a2" },
    ]);
  });

  it("uses the preview badge asset as the background when available", () => {
    const calls: string[] = [];
    const context = {
      beginPath: () => calls.push("beginPath"),
      clip: () => calls.push("clip"),
      drawImage: () => calls.push("drawImage"),
      fillText: () => calls.push("fillText"),
      measureText: () => ({
        actualBoundingBoxAscent: 48,
        actualBoundingBoxDescent: 12,
        width: 80,
      }),
      restore: () => calls.push("restore"),
      roundRect: () => calls.push("roundRect"),
      save: () => calls.push("save"),
      fillStyle: "",
      font: "",
      textAlign: "left",
      textBaseline: "alphabetic",
    } as unknown as CanvasRenderingContext2D;

    drawTabletopStatText(context, "2/2", {} as CanvasImageSource);

    expect(calls).toEqual([
      "save",
      "save",
      "beginPath",
      "roundRect",
      "clip",
      "drawImage",
      "restore",
      "fillText",
      "restore",
    ]);
  });
});

describe("fitTabletopStatFontSize", () => {
  it("keeps short values comfortably inside the inner field", () => {
    expect(
      fitTabletopStatFontSize("2/2", 322, 176, () => 130),
    ).toBeCloseTo(91.52);
  });

  it("scales wide values down to the inner-field width", () => {
    const fontSize = fitTabletopStatFontSize(
      "123/123",
      322,
      176,
      () => 300,
    );

    expect(fontSize).toBeCloseTo(62.87, 1);
    expect((300 * fontSize) / (176 * 0.52)).toBeCloseTo(322 * 0.64);
  });
});

describe("tokenizeTabletopRulesParagraph", () => {
  it("keeps mana symbols inline when punctuation or reminder text surrounds them", () => {
    const context = {
      measureText: (text: string) => ({ width: text.length * 10 }),
    } as Pick<CanvasRenderingContext2D, "measureText">;

    expect(
      tokenizeTabletopRulesParagraph(
        context,
        "({T}: Add one {R}.)",
        24,
      ),
    ).toMatchObject([
      { kind: "text", text: "(", attachPrevious: false },
      { kind: "pip", symbol: "T", attachPrevious: true },
      { kind: "text", text: ":", attachPrevious: true },
      { kind: "text", text: "Add", attachPrevious: false },
      { kind: "text", text: "one", attachPrevious: false },
      { kind: "pip", symbol: "R", attachPrevious: false },
      { kind: "text", text: ".)", attachPrevious: true },
    ]);
  });
});
