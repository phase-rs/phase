import { motion, useReducedMotion } from "framer-motion";

import type {
  CardMotionTarget,
  ReleasedCardMotion,
} from "../../stores/animationStore.ts";
import { TabletopStackCardSurface } from "../stack/TabletopStackCardSurface.tsx";
import { cardFlightControl } from "./cardMotion.ts";
import {
  ResolvedAnimationImage,
  type AnimationImageSnapshot,
} from "./ResolvedAnimationImage.tsx";

interface CastArcAnimationProps {
  objectId?: number;
  from: CardMotionTarget | { x: number; y: number };
  to: CardMotionTarget | { x: number; y: number };
  release?: ReleasedCardMotion;
  snapshot?: AnimationImageSnapshot | null;
  mode:
    | "cast"
    | "play-permanent"
    | "resolve-permanent"
    | "resolve-spell";
  duration?: number;
  onComplete: () => void;
}

const SNAPSHOT_CARD_WIDTH = 80;
const SNAPSHOT_CARD_HEIGHT = 112;
const SNAPSHOT_ARC_HEIGHT = 100;

function isCardMotionTarget(
  target: CardMotionTarget | { x: number; y: number },
): target is CardMotionTarget {
  return "rect" in target;
}

function SnapshotCard({ snapshot }: { snapshot: AnimationImageSnapshot | null }) {
  if (!snapshot) {
    return <div className="h-full w-full bg-black/70" />;
  }
  return (
    <ResolvedAnimationImage
      snapshot={snapshot}
      size="normal"
      alt={snapshot.cardName}
      fallback={<div className="h-full w-full bg-black/70" />}
      style={{ width: "100%", height: "100%", objectFit: "cover" }}
    />
  );
}

function SnapshotCastArc({
  from,
  to,
  snapshot,
  mode,
  onComplete,
}: {
  from: { x: number; y: number };
  to: { x: number; y: number };
  snapshot: AnimationImageSnapshot | null;
  mode: CastArcAnimationProps["mode"];
  onComplete: () => void;
}) {
  if (mode === "resolve-spell") {
    return (
      <motion.div
        initial={{ opacity: 1, scale: 1 }}
        animate={{ opacity: 0, scale: 0.3 }}
        transition={{ duration: 0.3, ease: "easeIn" }}
        onAnimationComplete={onComplete}
        style={{
          position: "fixed",
          left: from.x - SNAPSHOT_CARD_WIDTH / 2,
          top: from.y - SNAPSHOT_CARD_HEIGHT / 2,
          width: SNAPSHOT_CARD_WIDTH,
          height: SNAPSHOT_CARD_HEIGHT,
          pointerEvents: "none",
          zIndex: 45,
          borderRadius: 6,
          overflow: "hidden",
        }}
      >
        <SnapshotCard snapshot={snapshot} />
      </motion.div>
    );
  }

  const midX = (from.x + to.x) / 2;
  const midY = Math.min(from.y, to.y) - SNAPSHOT_ARC_HEIGHT;
  const transitDuration = mode === "cast" ? 0.4 : 0.3;
  return (
    <motion.div
      initial={{ x: from.x, y: from.y, opacity: 1 }}
      animate={{
        x: [from.x, midX, to.x],
        y: [from.y, midY, to.y],
        opacity: 1,
      }}
      transition={{ duration: transitDuration, ease: "easeOut", times: [0, 0.5, 1] }}
      onAnimationComplete={onComplete}
      style={{
        position: "fixed",
        left: -SNAPSHOT_CARD_WIDTH / 2,
        top: -SNAPSHOT_CARD_HEIGHT / 2,
        width: SNAPSHOT_CARD_WIDTH,
        height: SNAPSHOT_CARD_HEIGHT,
        pointerEvents: "none",
        zIndex: 45,
        borderRadius: 6,
        overflow: "hidden",
      }}
    >
      <SnapshotCard snapshot={snapshot} />
    </motion.div>
  );
}

/**
 * Carries the live, composed card face between zones. It intentionally does
 * not substitute a printing image: crown, title, counters, and frame treatment
 * remain the same visual game piece the player picked up in hand.
 */
export function CastArcAnimation({
  objectId,
  from,
  to,
  release,
  snapshot = null,
  mode,
  duration = mode === "cast" ? 0.42 : 0.3,
  onComplete,
}: CastArcAnimationProps) {
  const shouldReduceMotion = useReducedMotion();
  if (!isCardMotionTarget(from) && !isCardMotionTarget(to)) {
    return (
      <SnapshotCastArc
        from={from}
        to={to}
        snapshot={snapshot}
        mode={mode}
        onComplete={onComplete}
      />
    );
  }
  if (!isCardMotionTarget(from) || !isCardMotionTarget(to)) return null;
  if (objectId == null) return null;
  const velocity = release?.velocity ?? { x: 0, y: 0 };
  const control = cardFlightControl(from, to, velocity);
  const transitDuration = shouldReduceMotion
    ? Math.min(duration, 0.12)
    : duration;
  const midWidth = (from.rect.width + to.rect.width) / 2 * 1.04;
  const midHeight = (from.rect.height + to.rect.height) / 2 * 1.04;

  return (
    <motion.div
      initial={{
        x: from.rect.x,
        y: from.rect.y,
        width: from.rect.width,
        height: from.rect.height,
        rotate: from.rotation,
        scale: 1,
        opacity: 1,
      }}
      animate={{
        x: shouldReduceMotion
          ? to.rect.x
          : [from.rect.x, control.x, to.rect.x],
        y: shouldReduceMotion
          ? to.rect.y
          : [from.rect.y, control.y, to.rect.y],
        width: shouldReduceMotion
          ? to.rect.width
          : [from.rect.width, midWidth, to.rect.width],
        height: shouldReduceMotion
          ? to.rect.height
          : [from.rect.height, midHeight, to.rect.height],
        rotate: shouldReduceMotion
          ? to.rotation
          : [from.rotation, control.rotation, to.rotation],
        scale: shouldReduceMotion ? 1 : [1, 1.035, 1],
        opacity: 1,
      }}
      transition={{
        duration: transitDuration,
        ease: [0.2, 0.72, 0.18, 1],
        times: shouldReduceMotion ? undefined : [0, 0.48, 1],
      }}
      onAnimationComplete={onComplete}
      style={{
        position: "fixed",
        left: 0,
        top: 0,
        pointerEvents: "none",
        zIndex: 45,
        transformOrigin: "50% 50%",
        transformPerspective: 900,
        "--hand-card-w": "100%",
        "--hand-card-h": "100%",
      } as React.CSSProperties}
      data-card-in-flight={objectId}
      data-card-flight-mode={mode}
    >
      {snapshot ? (
        <SnapshotCard snapshot={snapshot} />
      ) : (
        <TabletopStackCardSurface
          objectId={objectId}
          transitDuration={transitDuration}
        />
      )}
    </motion.div>
  );
}
