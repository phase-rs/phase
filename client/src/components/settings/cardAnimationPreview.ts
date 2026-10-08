import type { CardAnimationStyle } from "../../animation/types.ts";

const ROOT = `${import.meta.env.BASE_URL}card-animation-previews/`;

/**
 * Recorded footage of the same game moments played under each card-animation
 * style. Both clips share one timeline (see `CARD_ANIMATION_PREVIEW_MOMENTS`),
 * so the picker can seek and play them in lockstep.
 */
export const CARD_ANIMATION_PREVIEW_CLIPS: Record<CardAnimationStyle, { video: string; poster: string }> = {
  webgl: { video: `${ROOT}webgl.mp4`, poster: `${ROOT}webgl-poster.webp` },
  classic: { video: `${ROOT}classic.mp4`, poster: `${ROOT}classic-poster.webp` },
};

export interface CardAnimationPreviewMoment {
  /** Card whose resolution the segment shows. Card names stay in English. */
  spell: string;
  /** Segment start, in seconds on the shared clip timeline. */
  start: number;
  /** The frame where the two styles differ most; shown while the preview is idle. */
  peak: number;
}

/** The big spells, where the two styles are easiest to tell apart. */
export const CARD_ANIMATION_PREVIEW_MOMENTS: readonly CardAnimationPreviewMoment[] = [
  { spell: "Wrath of God", start: 0, peak: 1.5 },
  { spell: "Evacuation", start: 4.03, peak: 5.7 },
  { spell: "Lightning Bolt", start: 8.06, peak: 9.0 },
];

/** Index of the moment playing at `time` (the last one that has started). */
export function momentIndexAt(time: number): number {
  let index = 0;
  CARD_ANIMATION_PREVIEW_MOMENTS.forEach((moment, i) => {
    if (time >= moment.start) index = i;
  });
  return index;
}
