import { useEffect, useMemo, useRef } from "react";
import { createPortal } from "react-dom";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useRafPositions } from "../../hooks/useRafPositions.ts";
import { arcPath } from "../../hooks/useAttackerArrowPositions.ts";
import type { ObjectId } from "../../adapter/types.ts";
import { isAttackerTargetingPlayer } from "../../viewmodel/battlefieldProps.ts";

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
      <defs>
        {!isMinimal && (
          <filter id="block-line-glow">
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        )}
        <marker
          id="block-arrow-head"
          markerWidth="10"
          markerHeight="8"
          refX="10"
          refY="4"
          orient="auto"
        >
          <path d="M0,0 L10,4 L0,8 Z" fill="rgba(249,115,22,0.9)" />
        </marker>
        <marker
          id="block-arrow-head-dim"
          markerWidth="8"
          markerHeight="6"
          refX="8"
          refY="3"
          orient="auto"
        >
          <path d="M0,0 L8,3 L0,6 Z" fill="rgba(249,115,22,0.45)" />
        </marker>
      </defs>
      {Array.from(positions.entries()).map(([blockerId, pos]) => {
        const attackerId = pairs.get(blockerId);
        const targetsMe = attackerId != null && isAttackerTargetingPlayer(combat, attackerId, myId);
        const primary = targetsMe || combat === null;
        return (
          <g key={blockerId} opacity={primary ? 1 : 0.4}>
            <path
              d={arcPath(pos.from, pos.to)}
              stroke={`rgba(249,115,22,${primary ? 0.85 : 0.35})`}
              strokeWidth={isMinimal ? 1.5 : primary ? 3 : 2}
              fill="none"
              filter={isMinimal ? undefined : "url(#block-line-glow)"}
              markerEnd={primary ? "url(#block-arrow-head)" : "url(#block-arrow-head-dim)"}
            />
            {!isMinimal && primary && <PulseDot from={pos.from} to={pos.to} length={pos.length} />}
          </g>
        );
      })}
    </svg>,
    document.body,
  );
}

/** Merge UI-side blockerAssignments map with engine-confirmed blocker_to_attacker.
 *  Engine sends blocker_id → attacker_id[] (Vec supports multi-blocking via ExtraBlockers).
 *  We flatten to (blocker, attacker) pairs — for multi-block we use the first attacker
 *  since the line only needs one endpoint per blocker. */
function useMergedPairs(
  uiAssignments: Map<ObjectId, ObjectId>,
  engineAssignments: Record<string, ObjectId[]> | null,
): Map<ObjectId, ObjectId> {
  return useMemo(() => {
    const merged = new Map(uiAssignments);
    if (engineAssignments) {
      for (const [blockerId, attackerIds] of Object.entries(engineAssignments)) {
        if (attackerIds.length > 0) {
          merged.set(Number(blockerId), attackerIds[0]);
        }
      }
    }
    return merged;
  }, [uiAssignments, engineAssignments]);
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
  const circleRef = useRef<SVGCircleElement>(null);

  useEffect(() => {
    const duration = Math.max(800, Math.min(length * 3, 2000));
    let start: number | null = null;
    let rafId: number;

    function tick(now: number) {
      if (start === null) start = now;
      const elapsed = now - start;
      const t = (elapsed % duration) / duration;
      const cx = from.x + (to.x - from.x) * t;
      const cy = from.y + (to.y - from.y) * t;
      circleRef.current?.setAttribute("cx", String(cx));
      circleRef.current?.setAttribute("cy", String(cy));
      rafId = requestAnimationFrame(tick);
    }

    rafId = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafId);
  }, [length, from.x, from.y, to.x, to.y]);

  return <circle ref={circleRef} cx={from.x} cy={from.y} r={3} fill="rgba(249,115,22,0.9)" />;
}

