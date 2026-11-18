import { describe, expect, it } from "vitest";

import { bannerTitleStyle, type BannerTitleSize } from "../bannerTitle";

/** The turn banner's cinematic title: `text-5xl tracking-[0.2em]`. */
const TURN_TITLE: BannerTitleSize = { maxRem: 3, minRem: 1.75, letterSpacingEm: 0.2 };

/** The roll winner's caption: `text-4xl tracking-wider`. */
const WINNER_CAPTION: BannerTitleSize = { maxRem: 2.25, minRem: 1.5, letterSpacingEm: 0.05 };

/** A coin flip's outcome pill: `text-2xl tracking-wide`, `px-5` and a 1px border. */
const COIN_OUTCOME: BannerTitleSize = { maxRem: 1.5, minRem: 1.125, letterSpacingEm: 0.025, insetRem: 2.625 };

/**
 * Container widths inside the gutters (`--game-edge-right` and no safe-area
 * inset): a 390px and a 412px portrait phone (0.75rem each side), an 814px
 * landscape phone (0.375rem) and a 1440px desktop window (1rem).
 */
const PHONE_390 = 366;
const PHONE_412 = 388;
const LANDSCAPE_PHONE = 802;
const DESKTOP = 1408;

/**
 * The font size, in px, a browser computes from `bannerTitleStyle`'s
 * `font-size` in a container whose content box is `containerWidthPx` wide, at a
 * 16px root. happy-dom does no layout and drops `clamp()` from inline styles,
 * so the test resolves the expression itself: the custom properties, `cqi` and
 * `rem` become numbers, and `clamp`, `min` and `calc` their arithmetic.
 */
function resolvedFontPx(title: string, size: BannerTitleSize, containerWidthPx: number): number {
  const style = bannerTitleStyle(title, size);
  const expression = style.fontSize
    .replace(/var\(--title-width-em\)/g, String(style["--title-width-em"]))
    .replace(/var\(--title-run-width-em\)/g, String(style["--title-run-width-em"]))
    .replace(/100cqi/g, String(containerWidthPx))
    .replace(/(\d+(?:\.\d+)?)rem/g, "($1 * 16)")
    .replace(/calc\(/g, "(");
  const clamp = (minimum: number, preferred: number, maximum: number) =>
    Math.max(minimum, Math.min(preferred, maximum));
  return new Function("clamp", "min", `return ${expression};`)(clamp, Math.min);
}

describe("bannerTitleStyle", () => {
  it("estimates the title's width and its widest unbreakable run, letter-spacing included", () => {
    const style = bannerTitleStyle("ALPHINAUD LEVEILLEUR'S TURN", TURN_TITLE);

    // 27 characters and the 12 of "LEVEILLEUR'S", at (0.68 + 0.2)em each.
    expect(style["--title-width-em"]).toBe(23.76);
    expect(style["--title-run-width-em"]).toBe(10.56);
  });

  it("counts characters, not UTF-16 code units", () => {
    // "𝔸" is one narrow character in two code units.
    const style = bannerTitleStyle("𝔸LICE'S TURN", TURN_TITLE);

    expect(style["--title-width-em"]).toBe(10.56);
    expect(style["--title-run-width-em"]).toBe(6.16);
  });

  it("counts a CJK character at its full width, and breaks a run on either side of it", () => {
    // ja "{{name}}のターン": the name and the suffix share no space, but a line
    // can break around each kana, so the widest run is the name alone.
    const style = bannerTitleStyle("ALPHINAUDのターン", TURN_TITLE);

    // 9 narrow characters at (0.68 + 0.2)em and 4 full-width ones at (1.05 + 0.2)em.
    expect(style["--title-width-em"]).toBe(12.92);
    expect(style["--title-run-width-em"]).toBe(7.92);
  });

  it("counts full-width Latin letters, digits and punctuation at full width", () => {
    // "１２３" are full-width digits, common in Japanese names: all 11
    // characters are counted at (1.05 + 0.2)em.
    const style = bannerTitleStyle("ユーザー１２３のターン", TURN_TITLE);

    expect(style["--title-width-em"]).toBe(13.75);
    expect(style["--title-run-width-em"]).toBe(1.25);
  });

  it("ends a run after a hyphen, where a line can break", () => {
    // "ANNE-MARIE-LEFEBVRE'S" breaks into "ANNE-", "MARIE-" and the 10 of
    // "LEFEBVRE'S", so it is no single run too wide for the floor.
    const style = bannerTitleStyle("ANNE-MARIE-LEFEBVRE'S TURN", TURN_TITLE);

    expect(style["--title-width-em"]).toBe(22.88);
    expect(style["--title-run-width-em"]).toBe(8.8);
    expect(resolvedFontPx("ANNE-MARIE-LEFEBVRE'S TURN", TURN_TITLE, PHONE_412)).toBe(28);
  });

  it("fits the Japanese \"your turn\" on one line where it fits today", () => {
    // あなたのターン is 7 full-width characters, 7 × (1 + 0.2)em ≈ 403px at 48px:
    // wider than a 412px phone's 388px line, so it shrinks to fit rather than wrap.
    const fontPx = resolvedFontPx("あなたのターン", TURN_TITLE, PHONE_412);

    expect(fontPx).toBeLessThan(48);
    expect(7 * (1 + 0.2) * fontPx).toBeLessThanOrEqual(PHONE_412);
  });

  it("caps the size at the title's size class and floors it at its minimum", () => {
    const { fontSize } = bannerTitleStyle("YOUR TURN", TURN_TITLE);

    expect(fontSize).toMatch(/^clamp\(min\(1\.75rem, .+\), .+, 3rem\)$/);
  });

  it("keeps a short name at its size class where the estimate fits it on one line", () => {
    for (const containerWidthPx of [PHONE_412, LANDSCAPE_PHONE, DESKTOP]) {
      expect(resolvedFontPx("YOUR TURN", TURN_TITLE, containerWidthPx)).toBe(48);
    }
  });

  it("shrinks some short titles a little on a portrait phone, rather than risk a wrap", () => {
    // 9 characters at (0.68 + 0.2)em fill 366px at ~46.2px.
    const turnPx = resolvedFontPx("YOUR TURN", TURN_TITLE, PHONE_390);
    expect(turnPx).toBeGreaterThan(46);
    expect(turnPx).toBeLessThan(48);

    // 16 characters at (0.68 + 0.05)em fill 388px at ~33.2px, against 36px.
    const captionPx = resolvedFontPx("YOU WON THE ROLL", WINNER_CAPTION, PHONE_412);
    expect(captionPx).toBeGreaterThan(33);
    expect(captionPx).toBeLessThan(36);
    expect(resolvedFontPx("YOU WON THE ROLL", WINNER_CAPTION, LANDSCAPE_PHONE)).toBe(36);
  });

  it("shrinks a long name to fit one line where the width allows it", () => {
    // 27 characters at (0.68 + 0.2)em fill 802px at ~33.8px.
    const fontPx = resolvedFontPx("ALPHINAUD LEVEILLEUR'S TURN", TURN_TITLE, LANDSCAPE_PHONE);

    expect(fontPx).toBeLessThan(48);
    expect(fontPx).toBeGreaterThan(28);
    expect(23.76 * fontPx).toBeCloseTo(LANDSCAPE_PHONE);
  });

  it("stops shrinking at the floor and leaves the rest to wrapping", () => {
    // One line would need ~16.3px, so the title holds the 1.75rem floor and
    // wraps between words.
    expect(resolvedFontPx("ALPHINAUD LEVEILLEUR'S TURN", TURN_TITLE, PHONE_412)).toBe(28);
  });

  it("lowers the floor only as far as a run too wide for one line needs", () => {
    // "XXDRAGONSLAYER9000XX'S" is 22 characters, too wide for a 388px line at
    // the 28px floor, so the size drops until that run fits.
    const fontPx = resolvedFontPx("XXDRAGONSLAYER9000XX'S TURN", TURN_TITLE, PHONE_412);

    expect(fontPx).toBeLessThan(28);
    expect(22 * (0.68 + 0.2) * fontPx).toBeCloseTo(PHONE_412);
  });

  it("leaves the title's own padding and border out of the line it fits", () => {
    const { fontSize } = bannerTitleStyle("ALPHINAUD LEVEILLEUR WON THE FLIP", COIN_OUTCOME);

    expect(fontSize).toContain("(100cqi - 2.625rem)");
    // 802px of container leaves 760px for the text: 33 characters at
    // (0.68 + 0.025)em fill it at ~32.7px, above the 24px cap.
    expect(resolvedFontPx("ALPHINAUD LEVEILLEUR WON THE FLIP", COIN_OUTCOME, LANDSCAPE_PHONE)).toBe(24);
    // 388px leaves 346px: one line would need ~14.9px, under the 18px floor.
    expect(resolvedFontPx("ALPHINAUD LEVEILLEUR WON THE FLIP", COIN_OUTCOME, PHONE_412)).toBe(18);
  });
});
