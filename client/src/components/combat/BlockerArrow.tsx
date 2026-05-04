import { useEffect, useState } from "react";
import { motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { arcPath } from "../../hooks/useAttackerArrowPositions.ts";

interface BlockerArrowProps {
  blockerId: number;
  attackerId: number;
}

export function BlockerArrow({ blockerId, attackerId }: BlockerArrowProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const [positions, setPositions] = useState<{
    from: { x: number; y: number };
    to: { x: number; y: number };
  } | null>(null);

  useEffect(() => {
    const blockerEl = document.querySelector(
      `[data-object-id="${blockerId}"]`,
    );
    const attackerEl = document.querySelector(
      `[data-object-id="${attackerId}"]`,
    );
    if (!blockerEl || !attackerEl) return;

    const blockerRect = blockerEl.getBoundingClientRect();
    const attackerRect = attackerEl.getBoundingClientRect();

    setPositions({
      from: {
        x: blockerRect.left + blockerRect.width / 2,
        y: blockerRect.top + blockerRect.height / 2,
      },
      to: {
        x: attackerRect.left + attackerRect.width / 2,
        y: attackerRect.top + attackerRect.height / 2,
      },
    });
  }, [blockerId, attackerId]);

  if (!positions) return null;

  const dx = positions.to.x - positions.from.x;
  const dy = positions.to.y - positions.from.y;
  const length = Math.sqrt(dx * dx + dy * dy);
  const isMinimal = vfxQuality === "minimal";
  const d = arcPath(positions.from, positions.to);

  return (
    <svg
      className="pointer-events-none fixed inset-0 z-30"
      width="100%"
      height="100%"
    >
      <defs>
        <marker
          id={`blocker-arrow-${blockerId}`}
          markerWidth="10"
          markerHeight="8"
          refX="10"
          refY="4"
          orient="auto"
        >
          <path d="M0,0 L10,4 L0,8 Z" fill="rgba(249,115,22,0.9)" />
        </marker>
        {!isMinimal && (
          <filter id={`blocker-glow-${blockerId}`}>
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        )}
      </defs>
      {isMinimal ? (
        <path
          d={d}
          stroke="rgba(249,115,22,0.6)"
          strokeWidth={1.5}
          fill="none"
          markerEnd={`url(#blocker-arrow-${blockerId})`}
        />
      ) : (
        <motion.path
          d={d}
          stroke="rgba(249,115,22,0.85)"
          strokeWidth={3}
          fill="none"
          markerEnd={`url(#blocker-arrow-${blockerId})`}
          filter={`url(#blocker-glow-${blockerId})`}
          initial={{ pathLength: 0, opacity: 0 }}
          animate={{ pathLength: 1, opacity: 1 }}
          transition={{
            duration: Math.min(length / 800, 0.4),
            ease: "easeOut",
          }}
        />
      )}
    </svg>
  );
}
