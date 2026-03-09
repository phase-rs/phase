import { useEffect, useState } from "react";
import { motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";

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

  return (
    <svg
      className="pointer-events-none fixed inset-0 z-30"
      width="100%"
      height="100%"
    >
      <defs>
        <marker
          id={`blocker-arrow-${blockerId}`}
          markerWidth="8"
          markerHeight="6"
          refX="8"
          refY="3"
          orient="auto"
        >
          <path d="M0,0 L8,3 L0,6 Z" fill="rgba(249,115,22,0.8)" />
        </marker>
      </defs>
      {isMinimal ? (
        <line
          x1={positions.from.x}
          y1={positions.from.y}
          x2={positions.to.x}
          y2={positions.to.y}
          stroke="rgba(249,115,22,0.5)"
          strokeWidth={1.5}
          markerEnd={`url(#blocker-arrow-${blockerId})`}
        />
      ) : (
        <motion.line
          x1={positions.from.x}
          y1={positions.from.y}
          x2={positions.to.x}
          y2={positions.to.y}
          stroke="rgba(249,115,22,0.6)"
          strokeWidth={2.5}
          markerEnd={`url(#blocker-arrow-${blockerId})`}
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
