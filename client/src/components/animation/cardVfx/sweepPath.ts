// ─── Sweep path ───
// Where a board sweep's front travels, and when it reaches each permanent it
// sets off. Kept free of three.js, so the card VFX layer can time the
// permanents' own effects without loading the scene.

import type { ObjectId } from "../../../adapter/types.ts";
import type { CardPose } from "./cardAnchors.ts";
import type { Vec2 } from "./vfxParticles.ts";

/** The front crosses from before its first card to past its last in this long,
 *  in seconds before pace, however far apart the cards lie. */
export const SWEEP_TRAVEL_S = 0.7;
/** The front starts this far before its first card, and ends as far past its
 *  last, in px. */
export const LEAD_PX = 60;
/** The front reaches this far beyond the outermost cards on either side, in px. */
const SIDE_MARGIN_PX = 70;

/** A card the front reaches, where it lies. */
export interface SweepHit {
  objectId: ObjectId;
  /** The card's centre, canvas px with y down. */
  at: Vec2;
  /** When the front reaches it, in seconds after the front sets out, before pace. */
  atS: number;
}

/** The front's straight path across the board. */
export interface SweepPath {
  /** Where the front's middle sets out and stops, canvas px with y down. */
  from: Vec2;
  to: Vec2;
  /** Half the front's width, across its travel. */
  halfWidth: number;
  hits: SweepHit[];
}

const dot = (a: Vec2, b: Vec2) => a[0] * b[0] + a[1] * b[1];

/** The front's path over `members`, heading away from the caster's `hud`, or
 *  up the screen when there is no HUD to come from. Each member is reached
 *  as the front passes its centre. */
export function sweepPath(hud: CardPose | null, members: ReadonlyMap<ObjectId, CardPose>): SweepPath {
  const poses = [...members];
  const centroid: Vec2 = [
    poses.reduce((sum, [, pose]) => sum + pose.x, 0) / poses.length,
    poses.reduce((sum, [, pose]) => sum + pose.y, 0) / poses.length,
  ];
  const away: Vec2 = hud ? [centroid[0] - hud.x, centroid[1] - hud.y] : [0, -1];
  const length = Math.hypot(away[0], away[1]);
  const dir: Vec2 = length > 1 ? [away[0] / length, away[1] / length] : [0, -1];
  const across: Vec2 = [-dir[1], dir[0]];
  const along = poses.map(([objectId, pose]) => {
    const offset: Vec2 = [pose.x - centroid[0], pose.y - centroid[1]];
    // How far the card reaches along the front's travel from its centre.
    const reach = (Math.abs(dir[0]) * pose.w + Math.abs(dir[1]) * pose.h) / 2;
    return { objectId, pose, s: dot(offset, dir), reach, side: Math.abs(dot(offset, across)) + Math.hypot(pose.w, pose.h) / 2 };
  });
  const start = Math.min(...along.map(({ s, reach }) => s - reach)) - LEAD_PX;
  const end = Math.max(...along.map(({ s, reach }) => s + reach)) + LEAD_PX;
  const point = (s: number): Vec2 => [centroid[0] + dir[0] * s, centroid[1] + dir[1] * s];
  return {
    from: point(start),
    to: point(end),
    halfWidth: Math.max(...along.map(({ side }) => side)) + SIDE_MARGIN_PX,
    hits: along.map(({ objectId, pose, s }) => ({
      objectId,
      at: [pose.x, pose.y],
      atS: ((s - start) / (end - start)) * SWEEP_TRAVEL_S,
    })),
  };
}
