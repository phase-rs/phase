import type { CSSProperties } from "react";

import { fanGeometry, spreadFactor } from "../card/fanGeometry.ts";

const HAND_FAN_WIDTH_BUDGET_VW = 92;
const MOBILE_HAND_FAN_WIDTH_BUDGET_VW = 76;

export type HandFanPresentation = "desktop" | "mobile";

/** Resting cards sit mostly below the viewport edge, matching Tabletop's shallow
 *  bottom ribbon while leaving the battlefield vertically unobstructed. */
export const HAND_FAN_RESTING_Y = 42;
export const HAND_FAN_HOVER_Y = 38;
/** Phones expose a shallower resting ribbon; deliberate tap-to-lift still
 *  reveals the same full-size, centered cards for interaction. */
export const MOBILE_HAND_FAN_RESTING_Y = 64;
export const MOBILE_HAND_FAN_LIFT_Y = -28;
export const HAND_CARD_HEIGHT_SCALE = 1.4;
export const OPPONENT_CARD_SCALE = 0.78;
const COMPACT_HEIGHT_VERTICAL_SCALE = 0.9;
/** Opponent cards render at 0.78x the base card while the standard hand renders
 *  at 1.4x. Scale the mirrored vertical depth by the same 0.78 / 1.4 ratio. */
export const OPPONENT_HAND_VERTICAL_SCALE = OPPONENT_CARD_SCALE / HAND_CARD_HEIGHT_SCALE;

export interface HandFanVerticalMetrics {
  arcScale: number;
  hoverY: number;
  restingY: number;
}

/** Short landscape viewports retain an Tabletop-sized card face. Keep nearly the
 *  full desktop curve so the lower frame rests below the viewport rather than
 *  exposing the entire card and consuming battlefield space. */
export function handFanVerticalMetrics(
  isCompactHeight: boolean,
  cardScale = 1,
  presentation: HandFanPresentation = "desktop",
): HandFanVerticalMetrics {
  const viewportScale = isCompactHeight ? COMPACT_HEIGHT_VERTICAL_SCALE : 1;
  const scale = viewportScale * cardScale;
  const restingY = presentation === "mobile"
    ? MOBILE_HAND_FAN_RESTING_Y
    : HAND_FAN_RESTING_Y;
  return {
    arcScale: scale,
    hoverY: HAND_FAN_HOVER_Y * scale,
    restingY: restingY * scale,
  };
}

/** Desktop keeps the broad readable fan. Mobile preserves that same shallow
 *  curve and gentle rotation, changing only horizontal overlap so a normal
 *  hand tucks inward and overflow hands expose progressively narrower strips. */
export function handFanGeometry(
  totalCards: number,
  cardWidthVar = "--hand-card-w",
  verticalScale = 1,
  presentation: HandFanPresentation = "desktop",
) {
  const geometry = fanGeometry(
    totalCards,
    cardWidthVar,
    presentation === "mobile" ? "tight" : "wide",
  );
  return {
    ...geometry,
    arc: (index: number) => geometry.arc(index) * verticalScale,
  };
}

/** Preserve normal card size until the fan reaches its reserved screen lane.
 *  Mobile's narrower 76vw lane keeps both lower corners clear for world-space
 *  piles and the command zone; overflow is absorbed by horizontal overlap
 *  before card faces are reduced. */
export function playerHandFanSizingStyle(
  totalCards: number,
  presentation: HandFanPresentation = "desktop",
): CSSProperties {
  const profile = presentation === "mobile" ? "tight" : "wide";
  const widthBudget = presentation === "mobile"
    ? MOBILE_HAND_FAN_WIDTH_BUDGET_VW
    : HAND_FAN_WIDTH_BUDGET_VW;
  const widthCapVw = (widthBudget / spreadFactor(totalCards, profile)).toFixed(2);
  return {
    "--hand-card-w": `min(calc(var(--card-w) * var(--hand-card-scale)), ${widthCapVw}vw)`,
    "--hand-card-h": `calc(var(--hand-card-w) * ${HAND_CARD_HEIGHT_SCALE})`,
  } as CSSProperties;
}
