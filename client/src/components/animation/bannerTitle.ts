import { useLayoutEffect, useState } from "react";

/**
 * Banner titles that print a player's name: the turn banner, the first-player
 * roll winner, a coin flip's outcome. A name can be long, so each title fits the
 * line it is given instead of running off the screen: centred inside the side
 * gutters, at its size class wherever it fits on one line, shrunk to fit one line
 * where that keeps it above a floor, and otherwise wrapped into balanced lines.
 */

/**
 * Equal side gutters: the game screen's edge padding (`--game-edge-right`, as the
 * mobile action rail's in `index.css`) outside the wider of the two safe-area
 * insets. Both sides match, so a centred title keeps the screen's centre line,
 * and the decorations around it, even when a camera cutout insets one side only.
 */
export const BANNER_TITLE_GUTTERS =
  "px-[calc(max(env(safe-area-inset-left),env(safe-area-inset-right))+var(--game-edge-right,0.75rem))]";

/** Room left on a title's line for how browsers round the widths it is measured by. */
const FIT_SLACK_PX = 2;

/**
 * Fits a banner title to its line, the content box of its parent, which needs a
 * definite width. Returns the ref to put on the title, which must lay out as a
 * block (a block or a flex item), so that `width` applies to it.
 *
 * - A title that fits on one line keeps its size class, with no inline size.
 * - One that doesn't shrinks until it fits on one line, down to `minimumRem`.
 * - Below that it stays at `minimumRem` and wraps; the caller centres and
 *   balances its lines (`text-center text-balance`). The floor gives way only for
 *   a run the line can't break (a long one-word name) too wide for a line at it,
 *   so no word runs past the gutters.
 *
 * It measures the title as the browser lays it out, so it holds for any font and
 * script. It measures again when the line resizes, when web fonts finish loading,
 * and when the text changes.
 */
export function useBannerTitleFit(text: string, minimumRem: number) {
  // The title, by a callback ref: a banner's title mounts after the hook's
  // component does, when its banner or caption appears.
  const [title, setTitle] = useState<HTMLElement | null>(null);

  useLayoutEffect(() => {
    const line = title?.parentElement;
    if (!title || !line) {
      return;
    }

    const fit = () => {
      // Measure at the size class. With `width: 100%` the title spans its line;
      // absolutely positioned, it is as wide as its own text asks, on one line
      // (`max-content`) and at its longest unbreakable run (`min-content`). This
      // runs before paint, so the measuring styles never show.
      title.style.fontSize = "";
      title.style.width = "100%";
      const lineWidth = title.offsetWidth;
      title.style.position = "absolute";
      title.style.width = "max-content";
      const oneLineWidth = title.offsetWidth;
      title.style.width = "min-content";
      const longestRunWidth = title.offsetWidth;
      title.style.position = "";
      title.style.width = "";

      // A title not laid out (zero width) has nothing to fit. Chrome and Safari
      // snap each `offsetWidth` to whole pixels from its own left edge, so a
      // reading can be off by up to just under a pixel either way. Two pixels of
      // slack, in this check and in the room the text may fill, keep a title the
      // readings call a fit from wrapping after all.
      if (!lineWidth || oneLineWidth <= lineWidth - FIT_SLACK_PX) {
        return;
      }

      // Text widths scale with the font size; the title's padding and border
      // don't. A length the browser leaves unresolved counts as none.
      const titleStyle = getComputedStyle(title);
      const sizeClassPx = parseFloat(titleStyle.fontSize);
      const chromeWidth =
        (parseFloat(titleStyle.paddingLeft) || 0) +
        (parseFloat(titleStyle.paddingRight) || 0) +
        (parseFloat(titleStyle.borderLeftWidth) || 0) +
        (parseFloat(titleStyle.borderRightWidth) || 0);
      const textRoom = lineWidth - FIT_SLACK_PX - chromeWidth;
      const sizeToFit = (width: number) => (sizeClassPx * textRoom) / (width - chromeWidth);

      const minimumPx = minimumRem * parseFloat(getComputedStyle(document.documentElement).fontSize);
      const floorPx = Math.min(minimumPx, sizeToFit(longestRunWidth));
      title.style.fontSize = `${Math.max(floorPx, sizeToFit(oneLineWidth))}px`;
    };

    fit();

    // Refit in the frame after a resize, not inside the observer's callback:
    // a refit changes the line's height, and resizing an observed element while
    // its resize is being delivered makes the browser report a ResizeObserver
    // loop error.
    let frame = 0;
    const resizeObserver = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(fit);
    });
    resizeObserver.observe(line);

    // A web font that finishes loading after the first fit changes the text's width.
    let mounted = true;
    void document.fonts?.ready.then(() => {
      if (mounted) {
        fit();
      }
    });

    return () => {
      mounted = false;
      resizeObserver.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [title, text, minimumRem]);

  return setTitle;
}
