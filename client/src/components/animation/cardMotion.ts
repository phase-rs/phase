import type { StackDockSide } from "../../stores/preferencesStore.ts";
import type { CardMotionTarget } from "../../stores/animationStore.ts";

export interface ScreenPoint {
  x: number;
  y: number;
}

export interface ProjectedCardPoints {
  center: ScreenPoint;
  left: ScreenPoint;
  right: ScreenPoint;
  top: ScreenPoint;
  bottom: ScreenPoint;
}

export interface CardFlightControl {
  x: number;
  y: number;
  rotation: number;
}

function distance(a: ScreenPoint, b: ScreenPoint): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

/** Convert projected card axes into the DOM pose used by the in-flight piece. */
export function projectedCardMotionTarget(
  points: ProjectedCardPoints,
): CardMotionTarget {
  const width = distance(points.left, points.right);
  const height = distance(points.top, points.bottom);
  const topVector = {
    x: points.top.x - points.center.x,
    y: points.top.y - points.center.y,
  };

  return {
    rect: new DOMRect(
      points.center.x - width / 2,
      points.center.y - height / 2,
      width,
      height,
    ),
    rotation: Math.atan2(topVector.x, -topVector.y) * 180 / Math.PI,
  };
}

/**
 * Predict the first floating stack-card pose before StackDisplay mounts with
 * the engine's post-action state. This mirrors the live pile's edge inset and
 * responsive size so the released piece never hands off to a separate panel.
 */
export function stackCardMotionTarget(
  viewport: { width: number; height: number },
  dockSide: StackDockSide,
): CardMotionTarget {
  const widthScale =
    viewport.width < 640 ? 0.58
    : viewport.width < 1024 ? 0.72
    : viewport.width < 1440 ? 0.86
    : 1;
  const heightScale = viewport.height < 820 ? 0.9 : 1;
  const width = Math.max(118, Math.round(168 * widthScale * heightScale));
  const height = Math.max(165, Math.round(width * 1.4));
  const edgeInset =
    viewport.width < 640 ? 14
    : viewport.width < 1024 ? 28
    : viewport.width < 1440 ? 44
    : 64;
  const centerX =
    dockSide === "left"
      ? edgeInset + width / 2
      : viewport.width - edgeInset - width / 2;
  const centerY = Math.max(
    height / 2 + 12,
    Math.min(
      viewport.height * (viewport.width < 768 ? 0.39 : 0.44),
      viewport.height - height / 2 - 12,
    ),
  );

  return {
    rect: new DOMRect(
      centerX - width / 2,
      centerY - height / 2,
      width,
      height,
    ),
    rotation: 0,
  };
}

/** Momentum-aware lift point for a card flying between two screen poses. */
export function cardFlightControl(
  from: CardMotionTarget,
  to: CardMotionTarget,
  velocity: { x: number; y: number },
): CardFlightControl {
  const fromCenter = {
    x: from.rect.x + from.rect.width / 2,
    y: from.rect.y + from.rect.height / 2,
  };
  const toCenter = {
    x: to.rect.x + to.rect.width / 2,
    y: to.rect.y + to.rect.height / 2,
  };
  const travel = distance(fromCenter, toCenter);
  const lift = Math.max(54, Math.min(170, travel * 0.2));

  return {
    x:
      (from.rect.x + to.rect.x) / 2
      + Math.max(-110, Math.min(110, velocity.x * 0.075)),
    y:
      Math.min(from.rect.y, to.rect.y)
      - lift
      + Math.max(-70, Math.min(40, velocity.y * 0.035)),
    rotation:
      (from.rotation + to.rotation) / 2
      + Math.max(-12, Math.min(12, velocity.x * 0.012)),
  };
}
