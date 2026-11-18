/**
 * Sizing for the full-screen overlay titles that print a player's name (the turn
 * banner, the first-player roll winner, a coin flip's outcome). A name can be
 * long, so each title fits itself to the screen's width instead of running off
 * it: centred, inside the side gutters, shrunk where that keeps it on one line,
 * and otherwise wrapped into balanced lines.
 *
 * Nothing here measures text: a title's width is estimated from its characters.
 * The estimates err high, so a title the estimate fits on one line has room to
 * spare; where one runs low, the title wraps, still centred and balanced. The
 * exception is a run sized to exactly a line because it is too wide for one at
 * the floor (see `bannerTitleStyle`): if the estimate runs low for it, it runs
 * past the gutters.
 */

/**
 * Estimated width, in em before letter-spacing, of a character in the uppercase
 * extra-bold system sans these titles use. On an Android phone (Roboto Bold) a
 * name averages about 0.56em a character and a run of wide letters about 0.64em;
 * Helvetica-like system fonts reach about 0.66em.
 */
const NARROW_CHARACTER_EM = 0.68;

/** A full-width character is one em wide; the estimate keeps a little room, as the narrow one does. */
const WIDE_CHARACTER_EM = 1.05;

/**
 * Full-width characters: the CJK scripts, with the marks and punctuation they
 * share (such as ー); the ideographic space and the full-width forms of Latin
 * letters, digits and punctuation (such as １ and ！), common in Japanese names;
 * and emoji. A line can break on either side of one, which is how a ja or zh
 * template joins a name with no space ("{{name}}のターン").
 */
const WIDE_CHARACTER =
  /[\p{scx=Han}\p{scx=Hiragana}\p{scx=Katakana}\p{scx=Hangul}\u3000\uff01-\uff60\uffe0-\uffe6\p{Emoji_Presentation}]/u;

/**
 * Classes for the element a title fits itself to. Its side gutters are the game
 * screen's own edge padding (`--game-edge-right`, as the mobile action rail's in
 * `index.css`) outside the wider of the two safe-area insets. Both sides get the
 * same gutter, so a centred title shares the screen's centre line with the
 * banner's other decorations even when a camera cutout insets one side only.
 * Its width inside them is the `100cqi` that `bannerTitleStyle` divides. The
 * element needs a definite width (`w-full`, or `fixed inset-0`), because a size
 * container does not grow to fit its content.
 */
export const BANNER_TITLE_CONTAINER =
  "@container px-[calc(max(env(safe-area-inset-left),env(safe-area-inset-right))+var(--game-edge-right,0.75rem))]";

export type BannerTitleSize = {
  /** The title's size class, kept wherever the estimate fits it on one line. */
  maxRem: number;
  /** The smallest size it shrinks to before it wraps instead. */
  minRem: number;
  /** Its letter-spacing in em, from its `tracking-*` class. */
  letterSpacingEm: number;
  /** Its own horizontal padding and border, in rem, which its text can't use. */
  insetRem?: number;
};

/**
 * Inline style that sizes a title from its estimated width to the width of the
 * nearest `BANNER_TITLE_CONTAINER`:
 * - `maxRem` wherever the estimate fits the title on one line;
 * - smaller, down to `minRem`, where that fits it on one line;
 * - otherwise `minRem`, wrapping. The floor gives way only for a run too wide
 *   for a line at it (a long one-word name): a run ends at a space, after a
 *   hyphen, and on either side of a full-width character, where a line can break.
 *
 * The widths travel as custom properties, in em, so the size reads as the sum it
 * is. The caller centres and balances the wrapped lines (`text-center text-balance`).
 */
export function bannerTitleStyle(title: string, size: BannerTitleSize) {
  // One pass: the whole title's width, and the widest run a line can't break.
  let titleWidthEm = 0;
  let runWidthEm = 0;
  let widestRunEm = 0;
  for (const character of title) {
    const isWide = WIDE_CHARACTER.test(character);
    const widthEm = (isWide ? WIDE_CHARACTER_EM : NARROW_CHARACTER_EM) + size.letterSpacingEm;
    titleWidthEm += widthEm;

    if (isWide) {
      runWidthEm = 0;
      widestRunEm = Math.max(widestRunEm, widthEm);
      continue;
    }
    if (character === " ") {
      runWidthEm = 0;
      continue;
    }
    runWidthEm += widthEm;
    widestRunEm = Math.max(widestRunEm, runWidthEm);
    if (character === "-") {
      runWidthEm = 0;
    }
  }

  const lineWidth = size.insetRem
    ? `(100cqi - ${size.insetRem}rem)`
    : "100cqi";
  const fitsTitle = `calc(${lineWidth} / var(--title-width-em))`;
  const fitsWidestRun = `calc(${lineWidth} / var(--title-run-width-em))`;

  return {
    "--title-width-em": Number(titleWidthEm.toFixed(3)),
    "--title-run-width-em": Number(widestRunEm.toFixed(3)),
    fontSize: `clamp(min(${size.minRem}rem, ${fitsWidestRun}), ${fitsTitle}, ${size.maxRem}rem)`,
  };
}
