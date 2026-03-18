import { useId } from "react";

import { motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { getArcPath } from "./arcPath.ts";
import type { Point } from "./arcPath.ts";

interface TargetArrowProps {
  from: Point;
  to: Point;
}

const GOLD = "#C9B037";

export function TargetArrow({ from, to }: TargetArrowProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const isMinimal = vfxQuality === "minimal";
  const d = getArcPath(from, to);
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.sqrt(dx * dx + dy * dy);

  const uid = useId();
  const glowId = `gold-glow-${uid}`;
  const arrowheadId = `arrowhead-gold-${uid}`;

  return (
    <svg
      className="pointer-events-none fixed inset-0 z-50"
      width="100%"
      height="100%"
    >
      <defs>
        <filter id={glowId}>
          <feGaussianBlur stdDeviation="3" result="blur" />
          <feMerge>
            <feMergeNode in="blur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <marker
          id={arrowheadId}
          markerWidth="8"
          markerHeight="6"
          refX="8"
          refY="3"
          orient="auto"
        >
          <path d="M0,0 L8,3 L0,6 Z" fill={GOLD} />
        </marker>
      </defs>
      {isMinimal ? (
        <path
          d={d}
          stroke={GOLD}
          strokeWidth={3}
          fill="none"
          filter={`url(#${glowId})`}
          markerEnd={`url(#${arrowheadId})`}
        />
      ) : (
        <motion.path
          d={d}
          stroke={GOLD}
          strokeWidth={3}
          fill="none"
          filter={`url(#${glowId})`}
          markerEnd={`url(#${arrowheadId})`}
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
