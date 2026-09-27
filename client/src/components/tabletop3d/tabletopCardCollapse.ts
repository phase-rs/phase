import {
  TABLETOP_CARD_ART_BOTTOM_RATIO,
  TABLETOP_CARD_STAT_RECT,
} from "./tabletopCardCanvas.ts";

/**
 * The compact face ends exactly where the rendered artwork ends. The preserved
 * lower rail is composited directly against this seam, leaving no type-box
 * frame between them.
 */
export const TABLETOP_ART_ONLY_DEPTH_RATIO = TABLETOP_CARD_ART_BOTTOM_RATIO;

/** Bottom-most portion of the full frame retained beneath the cropped art. */
export const TABLETOP_BOTTOM_FRAME_DEPTH_RATIO = 0.045;

/** Full-card texture span retained: crown/name/art plus the lower frame rail. */
export const TABLETOP_COLLAPSED_TEXTURE_RATIO =
  TABLETOP_ART_ONLY_DEPTH_RATIO + TABLETOP_BOTTOM_FRAME_DEPTH_RATIO;

/**
 * Final physical depth of a battlefield permanent.
 *
 * This is intentionally taller than the retained texture span. The compact
 * face stretches only the art presentation—not the hidden type/rules panels—
 * so sparse boards keep substantial, readable game pieces without revealing a
 * sliver of the type box at the art/frame seam.
 */
export const TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO = 0.68;

/** Normal-speed time from battlefield arrival to the compact frame's settled state. */
export const TABLETOP_CARD_COLLAPSE_DURATION_SECONDS = 0.46;

/** Brief arrival beat before the lower frame begins moving. */
export const TABLETOP_CARD_COLLAPSE_HOLD_FRACTION = 0.1;

export interface TabletopCardCollapseTransform {
  /** Smoothstep-normalized animation progress. */
  easedProgress: number;
  /** Scale applied only along the card's local depth axis. */
  depthScale: number;
  /** Normalized center shift that keeps the top edge stationary. */
  centerOffsetInCardDepths: number;
  /** Portion of the original full-card texture that remains visible. */
  visibleTextureRatio: number;
}

/** Converts arrival age into the frame-only collapse phase. */
export function tabletopCardCollapseProgress(
  ageSeconds: number,
  durationSeconds = TABLETOP_CARD_COLLAPSE_DURATION_SECONDS,
): number {
  if (durationSeconds <= 0) return 1;
  const arrivalProgress = Math.min(
    1,
    Math.max(0, ageSeconds / durationSeconds),
  );
  return Math.min(
    1,
    Math.max(
      0,
      (arrivalProgress - TABLETOP_CARD_COLLAPSE_HOLD_FRACTION)
        / (1 - TABLETOP_CARD_COLLAPSE_HOLD_FRACTION),
    ),
  );
}

/** Applies the persisted global animation-speed preference to the 3D arrival. */
export function tabletopCardCollapseDuration(
  animationSpeedMultiplier: number,
): number {
  return TABLETOP_CARD_COLLAPSE_DURATION_SECONDS
    * Math.max(0, animationSpeedMultiplier);
}

/** Frame-rate-independent response for the card's position, rotation, and scale. */
export function tabletopCardSettleResponse(
  deltaSeconds: number,
  animationSpeedMultiplier: number,
): number {
  if (animationSpeedMultiplier <= 0) return 1;
  return 1 - Math.exp(
    -Math.max(0, deltaSeconds) * 14 / animationSpeedMultiplier,
  );
}

export interface TabletopCardRestPose {
  x: number;
  y: number;
  z: number;
  rotationY: number;
}

/** Keeps attack selection in-place and represents it with the normal tap pose. */
export function tabletopCardRestPose(
  position: readonly [number, number, number],
  faceAngle: number,
  tapped: boolean,
  attacking: boolean,
  hovered: boolean,
): TabletopCardRestPose {
  return {
    x: position[0],
    y: position[1] + (hovered ? 0.045 : 0),
    z: position[2],
    rotationY: faceAngle + (tapped || attacking ? Math.PI / 4 : 0),
  };
}

/**
 * Smoothly raises the bottom edge while pinning the card's top edge. The same
 * eased ratio drives geometry depth and texture cropping, so frame contents do
 * not stretch while the text and type panels disappear.
 */
export function tabletopCardCollapseTransform(
  progress: number,
  targetDepthRatio = TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO,
  targetTextureRatio = TABLETOP_COLLAPSED_TEXTURE_RATIO,
): TabletopCardCollapseTransform {
  const clampedProgress = Math.min(1, Math.max(0, progress));
  const clampedTarget = Math.min(1, Math.max(0.05, targetDepthRatio));
  const clampedTextureTarget = Math.min(
    1,
    Math.max(0.05, targetTextureRatio),
  );
  const eased = clampedProgress * clampedProgress * (3 - 2 * clampedProgress);
  const depthScale = 1 + (clampedTarget - 1) * eased;

  return {
    easedProgress: eased,
    depthScale,
    centerOffsetInCardDepths: (depthScale - 1) / 2,
    visibleTextureRatio:
      1 + (clampedTextureTarget - 1) * eased,
  };
}

/** Maps a full-card UV into the retained top portion of the texture. */
export function collapsedTabletopCardV(
  baseV: number,
  visibleTextureRatio: number,
): number {
  const visible = Math.min(1, Math.max(0.05, visibleTextureRatio));
  return 1 - visible + baseV * visible;
}

/** Maps a plane UV into the original live card's P/T or loyalty box. */
export function tabletopCardStatUv(
  baseU: number,
  baseV: number,
): { u: number; v: number } {
  return {
    u: TABLETOP_CARD_STAT_RECT.x + baseU * TABLETOP_CARD_STAT_RECT.width,
    v:
      1
      - TABLETOP_CARD_STAT_RECT.y
      - TABLETOP_CARD_STAT_RECT.height
      + baseV * TABLETOP_CARD_STAT_RECT.height,
  };
}
