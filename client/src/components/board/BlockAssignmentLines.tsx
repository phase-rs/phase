import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import type { ObjectId, PlayerId } from "../../adapter/types.ts";
import { isAttackerTargetingPlayer } from "../../viewmodel/battlefieldProps.ts";

interface LinePosition {
  from: { x: number; y: number };
  to: { x: number; y: number };
  length: number;
}

export function BlockAssignmentLines() {
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const combatMode = useUiStore((s) => s.combatMode);
  const combat = useGameStore((s) => s.gameState?.combat ?? null);
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const myId = usePlayerId();

  // Merge UI blocker assignments with confirmed engine assignments
  const pairs = useMergedPairs(blockerAssignments, combat?.blocker_to_attacker ?? null);

  const positions = useRafPositions(pairs);

  const isVisible =
    combatMode === "blockers" ||
    (combat !== null && Object.keys(combat.blocker_to_attacker).length > 0);

  if (!isVisible || positions.size === 0) return null;

  const isMinimal = vfxQuality === "minimal";

  return createPortal(
    <svg className="pointer-events-none fixed inset-0 z-30 h-full w-full">
      {!isMinimal && (
        <defs>
          <filter id="block-line-glow">
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        </defs>
      )}
      {Array.from(positions.entries()).map(([blockerId, pos]) => {
        const attackerId = pairs.get(blockerId);
        const targetsMe = attackerId != null && isAttackerTargetingPlayer(combat, attackerId, myId);
        // In multi-defender combat, dim lines for attackers not targeting the current player
        const lineOpacity = targetsMe || combat === null ? 0.7 : 0.25;
        return (
          <g key={blockerId} opacity={targetsMe || combat === null ? 1 : 0.4}>
            <line
              x1={pos.from.x}
              y1={pos.from.y}
              x2={pos.to.x}
              y2={pos.to.y}
              stroke={`rgba(249,115,22,${lineOpacity})`}
              strokeWidth={isMinimal ? 1.5 : 2.5}
              strokeDasharray={isMinimal ? undefined : "8 4"}
              filter={isMinimal ? undefined : "url(#block-line-glow)"}
            />
            {!isMinimal && targetsMe && <PulseDot from={pos.from} to={pos.to} length={pos.length} />}
          </g>
        );
      })}
    </svg>,
    document.body,
  );
}

/** Merge UI-side blockerAssignments map with engine-confirmed blocker_to_attacker. */
function useMergedPairs(
  uiAssignments: Map<ObjectId, ObjectId>,
  engineAssignments: Record<string, ObjectId> | null,
): Map<ObjectId, ObjectId> {
  return useMemo(() => {
    const merged = new Map(uiAssignments);
    if (engineAssignments) {
      for (const [blockerId, attackerId] of Object.entries(engineAssignments)) {
        merged.set(Number(blockerId), attackerId);
      }
    }
    return merged;
  }, [uiAssignments, engineAssignments]);
}

/** RAF polling for element positions -- stabilizes after 10 unchanged frames. */
function useRafPositions(pairs: Map<ObjectId, ObjectId>): Map<ObjectId, LinePosition> {
  const [positions, setPositions] = useState<Map<ObjectId, LinePosition>>(new Map());
  const prevRectsRef = useRef<Map<string, DOMRect>>(new Map());
  const stableCountRef = useRef(0);

  useEffect(() => {
    if (pairs.size === 0) {
      setPositions(new Map());
      return;
    }

    stableCountRef.current = 0;
    prevRectsRef.current = new Map();
    let rafId: number;

    function poll() {
      const currentRects = new Map<string, DOMRect>();
      let changed = false;

      for (const [blockerId, attackerId] of pairs) {
        for (const id of [blockerId, attackerId]) {
          const key = String(id);
          if (currentRects.has(key)) continue;
          const el = document.querySelector(`[data-object-id="${id}"]`);
          if (!el) continue;
          const rect = el.getBoundingClientRect();
          currentRects.set(key, rect);
          const prev = prevRectsRef.current.get(key);
          if (
            !prev ||
            Math.abs(prev.left - rect.left) > 0.5 ||
            Math.abs(prev.top - rect.top) > 0.5 ||
            Math.abs(prev.width - rect.width) > 0.5
          ) {
            changed = true;
          }
        }
      }

      if (changed) {
        stableCountRef.current = 0;
      } else {
        stableCountRef.current++;
      }

      prevRectsRef.current = currentRects;

      // Update positions on each frame until stable
      const next = new Map<ObjectId, LinePosition>();
      for (const [blockerId, attackerId] of pairs) {
        const blockerRect = currentRects.get(String(blockerId));
        const attackerRect = currentRects.get(String(attackerId));
        if (!blockerRect || !attackerRect) continue;
        const from = {
          x: blockerRect.left + blockerRect.width / 2,
          y: blockerRect.top + blockerRect.height / 2,
        };
        const to = {
          x: attackerRect.left + attackerRect.width / 2,
          y: attackerRect.top + attackerRect.height / 2,
        };
        const dx = to.x - from.x;
        const dy = to.y - from.y;
        next.set(blockerId, { from, to, length: Math.sqrt(dx * dx + dy * dy) });
      }
      setPositions(next);

      // Stop polling after 10 stable frames
      if (stableCountRef.current < 10) {
        rafId = requestAnimationFrame(poll);
      }
    }

    rafId = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(rafId);
  }, [pairs]);

  return positions;
}

/** Animated dot that pulses from blocker to attacker. */
function PulseDot({
  from,
  to,
  length,
}: {
  from: { x: number; y: number };
  to: { x: number; y: number };
  length: number;
}) {
  const [t, setT] = useState(0);

  useEffect(() => {
    const duration = Math.max(800, Math.min(length * 3, 2000));
    let start: number | null = null;
    let rafId: number;

    function tick(now: number) {
      if (start === null) start = now;
      const elapsed = now - start;
      setT((elapsed % duration) / duration);
      rafId = requestAnimationFrame(tick);
    }

    rafId = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafId);
  }, [length]);

  const cx = from.x + (to.x - from.x) * t;
  const cy = from.y + (to.y - from.y) * t;

  return <circle cx={cx} cy={cy} r={3} fill="rgba(249,115,22,0.9)" />;
}

