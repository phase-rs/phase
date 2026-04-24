import { useEffect, useRef, useState } from "react";

import type { ObjectId, PlayerId } from "../adapter/types.ts";

export interface Pos {
  x: number;
  y: number;
}

/** Where an attacker's arrow points. Two kinds so the same hook can resolve
 *  either DOM anchor convention — `data-player-hud` for player HUDs,
 *  `data-object-id` for planeswalkers and battles. */
export type AttackerArrowTarget =
  | { kind: "player"; playerId: PlayerId }
  | { kind: "object"; objectId: ObjectId };

export interface AttackerArrow {
  attackerId: ObjectId;
  target: AttackerArrowTarget;
  /** Tag payload — passes through to the rendered `AttackerArrowPos`.
   *  Callers use this to vary stroke weight / opacity. */
  isAtMe: boolean;
}

export interface AttackerArrowPos {
  key: string;
  from: Pos;
  to: Pos;
  isAtMe: boolean;
}

/** Quadratic Bézier arc between two points — perpendicular offset proportional
 *  to distance, capped at 80px. Shared between committed-attack arrows and
 *  pre-commit preview arrows so both fan out consistently. */
export function arcPath(from: Pos, to: Pos): string {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const dist = Math.sqrt(dx * dx + dy * dy) || 1;
  const offset = Math.min(80, dist * 0.3);
  const nx = -dy / dist;
  const ny = dx / dist;
  return `M ${from.x} ${from.y} Q ${mx + nx * offset} ${my + ny * offset} ${to.x} ${to.y}`;
}

function targetSelector(target: AttackerArrowTarget): string {
  return target.kind === "player"
    ? `[data-player-hud="${target.playerId}"]`
    : `[data-object-id="${target.objectId}"]`;
}

function targetKey(target: AttackerArrowTarget): string {
  return target.kind === "player"
    ? `p:${target.playerId}`
    : `o:${target.objectId}`;
}

function arrowKey(arrow: AttackerArrow): string {
  const suffix = arrow.target.kind === "player"
    ? `p${arrow.target.playerId}`
    : `o${arrow.target.objectId}`;
  return `${arrow.attackerId}->${suffix}`;
}

/** RAF-polled positions for attacker → target pairs. Target can be a player
 *  HUD (`data-player-hud`) or a permanent (`data-object-id`). Stabilizes
 *  after 10 unchanged frames to stop the loop. */
export function useAttackerArrowPositions(
  arrows: AttackerArrow[],
): AttackerArrowPos[] {
  const [positions, setPositions] = useState<AttackerArrowPos[]>([]);
  const prevRectsRef = useRef<Map<string, DOMRect>>(new Map());
  const stableCountRef = useRef(0);

  useEffect(() => {
    if (arrows.length === 0) {
      setPositions([]);
      return;
    }
    stableCountRef.current = 0;
    prevRectsRef.current = new Map();
    let rafId = 0;

    const poll = () => {
      const current = new Map<string, DOMRect>();
      let changed = false;

      for (const a of arrows) {
        const fromKey = `o:${a.attackerId}`;
        const toKey = targetKey(a.target);
        if (!current.has(fromKey)) {
          const el = document.querySelector(`[data-object-id="${a.attackerId}"]`);
          if (el) current.set(fromKey, el.getBoundingClientRect());
        }
        if (!current.has(toKey)) {
          const el = document.querySelector(targetSelector(a.target));
          if (el) current.set(toKey, el.getBoundingClientRect());
        }
        for (const key of [fromKey, toKey]) {
          const prev = prevRectsRef.current.get(key);
          const now = current.get(key);
          if (!now) continue;
          if (
            !prev
            || Math.abs(prev.left - now.left) > 0.5
            || Math.abs(prev.top - now.top) > 0.5
            || Math.abs(prev.width - now.width) > 0.5
          ) {
            changed = true;
          }
        }
      }

      stableCountRef.current = changed ? 0 : stableCountRef.current + 1;
      prevRectsRef.current = current;

      const next: AttackerArrowPos[] = [];
      for (const a of arrows) {
        const fromRect = current.get(`o:${a.attackerId}`);
        const toRect = current.get(targetKey(a.target));
        if (!fromRect || !toRect) continue;
        next.push({
          key: arrowKey(a),
          from: {
            x: fromRect.left + fromRect.width / 2,
            y: fromRect.top + fromRect.height / 2,
          },
          to: {
            x: toRect.left + toRect.width / 2,
            y: toRect.top + toRect.height / 2,
          },
          isAtMe: a.isAtMe,
        });
      }
      setPositions(next);

      if (stableCountRef.current < 10) {
        rafId = requestAnimationFrame(poll);
      }
    };

    rafId = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(rafId);
  }, [arrows]);

  return positions;
}
