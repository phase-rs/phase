import type { CSSProperties } from "react";

import { fanGeometry, spreadFactor } from "../card/fanGeometry.ts";

const HAND_FAN_WIDTH_BUDGET_VW = 92;

/** Resting cards sit mostly below the viewport edge, matching Arena's shallow
 *  bottom ribbon while leaving the battlefield vertically unobstructed. */
export const HAND_FAN_RESTING_Y = 48;
export const HAND_FAN_HOVER_Y = 38;
export const HAND_CARD_HEIGHT_SCALE = 1.4;
export const OPPONENT_CARD_SCALE = 0.78;
const COMPACT_HEIGHT_VERTICAL_SCALE = 0.5;
/** Opponent cards render at 0.78x the base card while the standard hand renders
 *  at 1.4x. Scale the mirrored vertical depth by the same 0.78 / 1.4 ratio. */
export const OPPONENT_HAND_VERTICAL_SCALE = OPPONENT_CARD_SCALE / HAND_CARD_HEIGHT_SCALE;

export interface HandFanVerticalMetrics {
  arcScale: number;
  hoverY: number;
  restingY: number;
}

/** Short landscape viewports shrink cards to 75% of their normal size. Scale
 *  the fan's vertical offsets with them so the same proportional amount of each
 *  card remains visible instead of fixed pixel offsets swallowing the card. */
export function handFanVerticalMetrics(
  isCompactHeight: boolean,
  cardScale = 1,
): HandFanVerticalMetrics {
  const viewportScale = isCompactHeight ? COMPACT_HEIGHT_VERTICAL_SCALE : 1;
  const scale = viewportScale * cardScale;
  return {
    arcScale: scale,
    hoverY: HAND_FAN_HOVER_Y * scale,
    restingY: HAND_FAN_RESTING_Y * scale,
  };
}

/** One authoritative wide, shallow geometry profile for both player and
 *  opponent hands on every viewport. Mobile still uses its drawer for the
 *  local player's interaction surface. */
export function handFanGeometry(
  totalCards: number,
  cardWidthVar = "--hand-card-w",
  verticalScale = 1,
) {
  const geometry = fanGeometry(totalCards, cardWidthVar, "wide");
  return {
    ...geometry,
    arc: (index: number) => geometry.arc(index) * verticalScale,
  };
}

/** Preserve the normal responsive hand-card size until the complete wide fan
 *  would exceed 92vw, then shrink cards just enough to fit smaller screens. */
export function playerHandFanSizingStyle(totalCards: number): CSSProperties {
  const widthCapVw = (HAND_FAN_WIDTH_BUDGET_VW / spreadFactor(totalCards, "wide")).toFixed(2);
  return {
    "--hand-card-w": `min(calc(var(--card-w) * var(--hand-card-scale)), ${widthCapVw}vw)`,
    "--hand-card-h": `calc(var(--hand-card-w) * ${HAND_CARD_HEIGHT_SCALE})`,
  } as CSSProperties;
}
