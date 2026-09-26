import type {
  AttackerInfo,
  ObjectId,
  PlayerId,
} from "../../adapter/types.ts";
import {
  tabletopHeldHandLayout,
  type TabletopPlacement,
  type TabletopSeat,
  type TabletopSeatAssignment,
  type TabletopTableLayout,
} from "./tabletopLayout.ts";

export const TABLETOP_ATTACK_STRIKE_DURATION_SECONDS = 0.84;

const WINDUP_END = 0.22;
const STRIKE_END = 0.58;
const IMPACT_END = 0.68;
const WINDUP_DISTANCE = 0.16;
const MAX_STRIKE_DISTANCE = 7.2;
const TARGET_STANDOFF_DISTANCE = 0.55;
const PEAK_LIFT = 0.7;
const IMPACT_LIFT = 0.54;
const RECOIL_DISTANCE = 0.14;

export interface TabletopAttackStrikeTarget {
  key: string;
  x: number;
  z: number;
}

export interface TabletopAttackStrikeTransform {
  offsetX: number;
  offsetZ: number;
  lift: number;
  scale: number;
}

/**
 * Presentation-only attack arc: lift, wind up, surge, recoil, and return.
 * The destination comes from the engine-authored combat declaration.
 */
export function tabletopAttackStrikeTransform(
  progress: number,
  origin: readonly [number, number],
  target: readonly [number, number],
): TabletopAttackStrikeTransform {
  const clamped = Math.min(1, Math.max(0, progress));
  const deltaX = target[0] - origin[0];
  const deltaZ = target[1] - origin[1];
  const distance = Math.hypot(deltaX, deltaZ);
  const directionX = distance > 0 ? deltaX / distance : 0;
  const directionZ = distance > 0 ? deltaZ / distance : 0;
  const strikeDistance = Math.min(
    MAX_STRIKE_DISTANCE,
    Math.max(0, distance - TARGET_STANDOFF_DISTANCE),
  );

  let travel: number;
  let lift: number;
  let scale: number;
  if (clamped <= WINDUP_END) {
    const phase = smoothstep(clamped / WINDUP_END);
    travel = -WINDUP_DISTANCE * phase;
    lift = IMPACT_LIFT * easeOutCubic(phase);
    scale = 1 + 0.04 * phase;
  } else if (clamped <= STRIKE_END) {
    const phase = (clamped - WINDUP_END) / (STRIKE_END - WINDUP_END);
    const surge = phase * phase * phase;
    travel = lerp(-WINDUP_DISTANCE, strikeDistance, surge);
    lift = IMPACT_LIFT
      + (PEAK_LIFT - IMPACT_LIFT) * Math.sin(Math.PI * phase);
    scale = 1.04 + 0.05 * Math.sin(Math.PI * phase);
  } else if (clamped <= IMPACT_END) {
    const phase = easeOutCubic(
      (clamped - STRIKE_END) / (IMPACT_END - STRIKE_END),
    );
    travel = strikeDistance - RECOIL_DISTANCE * phase;
    lift = lerp(IMPACT_LIFT, IMPACT_LIFT - 0.12, phase);
    scale = 1.04;
  } else {
    const phase = smoothstep(
      (clamped - IMPACT_END) / (1 - IMPACT_END),
    );
    travel = (strikeDistance - RECOIL_DISTANCE) * (1 - phase);
    lift = (IMPACT_LIFT - 0.12) * (1 - phase);
    scale = 1 + 0.04 * (1 - phase);
  }

  return {
    offsetX: directionX * travel,
    offsetZ: directionZ * travel,
    lift,
    scale,
  };
}

/** Maps committed combat destinations to world-space strike anchors. */
export function tabletopAttackStrikeTargets(
  attackers: readonly AttackerInfo[],
  placements: readonly TabletopPlacement[],
  perspectivePlayerId: PlayerId,
  opponentSeats: readonly TabletopSeatAssignment[],
  tableLayout: TabletopTableLayout,
): ReadonlyMap<ObjectId, TabletopAttackStrikeTarget> {
  const placementsById = new Map(
    placements.map((placement) => [placement.objectId, placement] as const),
  );
  const seatsByPlayer = new Map<PlayerId, TabletopSeat>([
    [perspectivePlayerId, "local"],
    ...opponentSeats.map(
      ({ playerId, seat }) => [playerId, seat] as const,
    ),
  ]);

  return new Map(
    attackers.flatMap((attacker) => {
      const targetPlacement =
        attacker.attack_target.type === "Player"
          ? undefined
          : placementsById.get(attacker.attack_target.data);
      const defendingSeat = seatsByPlayer.get(attacker.defending_player);
      const seatAnchor = defendingSeat
        ? tabletopHeldHandLayout(defendingSeat, tableLayout).position
        : undefined;
      const targetX = targetPlacement?.position[0] ?? seatAnchor?.[0];
      const targetZ = targetPlacement?.position[2] ?? seatAnchor?.[2];
      if (targetX == null || targetZ == null) return [];

      return [[
        attacker.object_id,
        {
          key: `${attacker.attack_target.type}:${attacker.attack_target.data}`,
          x: targetX,
          z: targetZ,
        },
      ] as const];
    }),
  );
}

function smoothstep(value: number): number {
  const clamped = Math.min(1, Math.max(0, value));
  return clamped * clamped * (3 - 2 * clamped);
}

function easeOutCubic(value: number): number {
  const clamped = Math.min(1, Math.max(0, value));
  return 1 - (1 - clamped) ** 3;
}

function lerp(start: number, end: number, amount: number): number {
  return start + (end - start) * amount;
}
