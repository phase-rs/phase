import { motion } from "framer-motion";

interface TargetArrowProps {
  from: { x: number; y: number };
  to: { x: number; y: number };
}

export function TargetArrow({ from, to }: TargetArrowProps) {
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
        <marker
          id="arrowhead"
          markerWidth="8"
          markerHeight="6"
          refX="8"
          refY="3"
          orient="auto"
        >
          <path d="M0,0 L8,3 L0,6 Z" fill="rgba(0,229,255,0.8)" />
        </marker>
      </defs>
      <motion.line
        x1={from.x}
        y1={from.y}
        x2={to.x}
        y2={to.y}
        stroke="rgba(0,229,255,0.6)"
        strokeWidth={2.5}
        markerEnd="url(#arrowhead)"
        initial={{ pathLength: 0, opacity: 0 }}
        animate={{ pathLength: 1, opacity: 1 }}
        transition={{ duration: Math.min(length / 800, 0.4), ease: "easeOut" }}
      />
    </svg>
  );
}
