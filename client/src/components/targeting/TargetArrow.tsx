import { motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";

interface Point {
  x: number;
  y: number;
}

interface TargetArrowProps {
  from: Point;
  to: Point;
}

function getArcPath(from: Point, to: Point): string {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const dist = Math.sqrt(dx * dx + dy * dy);
  // Perpendicular offset for curve — proportional to distance
  const offset = Math.min(80, dist * 0.3);
  const nx = -dy / dist;
  const ny = dx / dist;
  const cx = mx + nx * offset;
  const cy = my + ny * offset;
  return `M ${from.x} ${from.y} Q ${cx} ${cy} ${to.x} ${to.y}`;
}

const GOLD = "#C9B037";

export function TargetArrow({ from, to }: TargetArrowProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const isMinimal = vfxQuality === "minimal";
  const d = getArcPath(from, to);
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const length = Math.sqrt(dx * dx + dy * dy);

  return (
    <svg
      className="pointer-events-none fixed inset-0 z-50"
      width="100%"
      height="100%"
    >
      <defs>
        <filter id="gold-glow">
          <feGaussianBlur stdDeviation="3" result="blur" />
          <feMerge>
            <feMergeNode in="blur" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <marker
          id="arrowhead-gold"
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
          filter="url(#gold-glow)"
          markerEnd="url(#arrowhead-gold)"
        />
      ) : (
        <motion.path
          d={d}
          stroke={GOLD}
          strokeWidth={3}
          fill="none"
          filter="url(#gold-glow)"
          markerEnd="url(#arrowhead-gold)"
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
