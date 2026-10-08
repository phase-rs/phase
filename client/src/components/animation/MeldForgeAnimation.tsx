import { motion, useReducedMotion } from "framer-motion";

import { MELD_FORGE_PHASES } from "../../animation/types.ts";
import type { RGB } from "./particleSystem.ts";
import { forgeHeatColor } from "./particleEffects.ts";
import { MELDED_CARD_SCALE } from "../board/boardSizing.ts";
import {
  ResolvedAnimationImage,
  type AnimationImageSnapshot,
} from "./ResolvedAnimationImage.tsx";

/** One card of the meld pair, as it looked on the board before melding. */
export interface MeldForgePiece {
  snapshot: AnimationImageSnapshot | null;
  /** The card's board center, relative to the forge point. */
  offset: { x: number; y: number };
}

interface MeldForgeAnimationProps {
  /** Screen point the pair is forged over. */
  center: { x: number; y: number };
  /** Portrait size of one card of the pair. */
  cardSize: { width: number; height: number };
  source: MeldForgePiece;
  partner: MeldForgePiece;
  /** The melded permanent, showing its combined back face. */
  result: AnimationImageSnapshot | null;
  durationMs: number;
  onComplete: () => void;
}

const { gathered, strikes, fused, flipped } = MELD_FORGE_PHASES;
/** How far each card overlaps the forge center while the pair is pressed together. */
const PRESS_OFFSET = 0.32;
const RADIUS = 6;

function css({ r, g, b }: RGB, alpha = 1): string {
  return `rgba(${Math.round(r)}, ${Math.round(g)}, ${Math.round(b)}, ${alpha})`;
}

/** Radial gradient of steel at `heat` — white-hot core fading to its rim color. */
function hotSteel(heat: number): string {
  return `radial-gradient(ellipse at 50% 50%, ${css(forgeHeatColor(1))} 0%, ${css(forgeHeatColor(heat))} 55%, ${css(forgeHeatColor(heat * 0.4), 0.95)} 100%)`;
}

/** Heat rising blow by blow, 0 before the first strike to 1 at fusion. */
const HEAT_TIMES = [0, gathered, ...strikes, fused, 1];
const HEAT_LEVELS = [0, 0, 0.35, 0.6, 0.85, 1, 1];

/** Keyframes that squash the pressed pair under each hammer blow. */
function strikeSquash(): { times: number[]; scaleX: number[]; scaleY: number[] } {
  const times = [0];
  const scaleX = [1];
  const scaleY = [1];
  for (const at of strikes) {
    times.push(at - 0.012, at, at + 0.035);
    scaleX.push(1, 1.05, 1);
    scaleY.push(1, 0.92, 1);
  }
  times.push(1);
  scaleX.push(1);
  scaleY.push(1);
  return { times, scaleX, scaleY };
}

function CardFace({ snapshot }: { snapshot: AnimationImageSnapshot | null }) {
  const fallback = (
    <div className="flex h-full w-full items-center justify-center bg-black/70 p-1 text-center text-[0.6rem] text-white">
      {snapshot?.cardName}
    </div>
  );
  if (!snapshot) return fallback;
  return (
    <ResolvedAnimationImage
      snapshot={snapshot}
      size="normal"
      alt={snapshot.cardName}
      fallback={fallback}
      style={{ width: "100%", height: "100%", objectFit: "cover" }}
    />
  );
}

/** One card of the pair: lifted off the board, pressed against its partner, heated, then fused away. */
function ForgePiece({
  piece,
  side,
  cardSize,
  duration,
}: {
  piece: MeldForgePiece;
  side: -1 | 1;
  cardSize: { width: number; height: number };
  duration: number;
}) {
  const pressX = side * cardSize.width * PRESS_OFFSET;
  const times = [0, gathered, fused - 0.06, fused, 1];
  return (
    <motion.div
      initial={{ x: piece.offset.x, y: piece.offset.y, rotate: 0, opacity: 1 }}
      animate={{
        x: [piece.offset.x, pressX, pressX, 0, 0],
        y: [piece.offset.y, 0, 0, 0, 0],
        rotate: [0, side * 5, side * 5, 0, 0],
        opacity: [1, 1, 1, 0, 0],
      }}
      transition={{ duration, times, ease: "easeInOut" }}
      style={{
        position: "absolute",
        left: -cardSize.width / 2,
        top: -cardSize.height / 2,
        width: cardSize.width,
        height: cardSize.height,
        borderRadius: RADIUS,
        overflow: "hidden",
      }}
    >
      <motion.div
        className="h-full w-full"
        animate={{
          filter: HEAT_LEVELS.map(
            (heat) => `brightness(${1 + heat * 0.7}) saturate(${1 + heat * 0.8}) sepia(${heat * 0.7})`,
          ),
        }}
        transition={{ duration, times: HEAT_TIMES }}
      >
        <CardFace snapshot={piece.snapshot} />
      </motion.div>
      {/* Hot-metal glow washing over the card as it heats. */}
      <motion.div
        aria-hidden="true"
        animate={{ opacity: HEAT_LEVELS.map((heat) => heat * 0.85) }}
        transition={{ duration, times: HEAT_TIMES }}
        style={{
          position: "absolute",
          inset: 0,
          background: hotSteel(0.6),
          mixBlendMode: "screen",
        }}
      />
    </motion.div>
  );
}

/**
 * CR 701.42a: the two cards of a meld pair become one permanent. They are
 * forged together — gathered over an anvil, hammered white-hot, fused into one
 * blank — and the blank turns over to reveal the combined oversized card face
 * (CR 712.4a).
 */
export function MeldForgeAnimation({
  center,
  cardSize,
  source,
  partner,
  result,
  durationMs,
  onComplete,
}: MeldForgeAnimationProps) {
  const reduceMotion = useReducedMotion();
  const duration = durationMs / 1000;
  const resultSize = {
    width: cardSize.width * MELDED_CARD_SCALE,
    height: cardSize.height * MELDED_CARD_SCALE,
  };

  if (reduceMotion) {
    return (
      <motion.div
        data-testid="meld-forge-animation"
        initial={{ opacity: 0 }}
        animate={{ opacity: [0, 1, 1, 0] }}
        transition={{ duration, times: [0, 0.2, 0.85, 1] }}
        onAnimationComplete={onComplete}
        style={{
          position: "fixed",
          left: center.x - resultSize.width / 2,
          top: center.y - resultSize.height / 2,
          width: resultSize.width,
          height: resultSize.height,
          borderRadius: RADIUS,
          overflow: "hidden",
          pointerEvents: "none",
          zIndex: 50,
        }}
      >
        <CardFace snapshot={result} />
      </motion.div>
    );
  }

  const squash = strikeSquash();
  const flipTimes = [0, fused - 0.03, fused, flipped, 0.93, 1];

  return (
    <div
      data-testid="meld-forge-animation"
      aria-hidden="true"
      style={{
        position: "fixed",
        left: center.x,
        top: center.y,
        width: 0,
        height: 0,
        perspective: 1100,
        pointerEvents: "none",
        zIndex: 50,
      }}
    >
      {/* The pressed pair, squashed under each blow. */}
      <motion.div
        animate={{ scaleX: squash.scaleX, scaleY: squash.scaleY }}
        transition={{ duration, times: squash.times }}
        style={{ position: "absolute", left: 0, top: 0 }}
      >
        <ForgePiece piece={source} side={-1} cardSize={cardSize} duration={duration} />
        <ForgePiece piece={partner} side={1} cardSize={cardSize} duration={duration} />
      </motion.div>

      {/* The fused blank, turning over to the oversized combined face. */}
      <motion.div
        initial={{ opacity: 0, rotateY: 0, scale: 1 }}
        animate={{
          opacity: [0, 0, 1, 1, 1, 0],
          rotateY: [0, 0, 0, 180, 180, 180],
          scale: [1, 1, 1, MELDED_CARD_SCALE, MELDED_CARD_SCALE, MELDED_CARD_SCALE],
        }}
        transition={{ duration, times: flipTimes, ease: "easeInOut" }}
        onAnimationComplete={onComplete}
        style={{
          position: "absolute",
          left: -cardSize.width / 2,
          top: -cardSize.height / 2,
          width: cardSize.width,
          height: cardSize.height,
          transformStyle: "preserve-3d",
        }}
      >
        <div
          style={{
            position: "absolute",
            inset: 0,
            borderRadius: RADIUS,
            backfaceVisibility: "hidden",
            background: hotSteel(1),
            boxShadow: `0 0 40px 14px ${css(forgeHeatColor(0.8), 0.8)}`,
          }}
        />
        <div
          style={{
            position: "absolute",
            inset: 0,
            borderRadius: RADIUS,
            overflow: "hidden",
            backfaceVisibility: "hidden",
            transform: "rotateY(180deg)",
            boxShadow: `0 0 32px 8px ${css(forgeHeatColor(0.75), 0.7)}`,
          }}
        >
          <CardFace snapshot={result} />
          {/* The revealed face cools from forge heat to its printed colors. */}
          <motion.div
            aria-hidden="true"
            animate={{ opacity: [0.9, 0.9, 0.9, 0.9, 0, 0] }}
            transition={{ duration, times: flipTimes }}
            style={{
              position: "absolute",
              inset: 0,
              background: hotSteel(0.9),
              mixBlendMode: "screen",
            }}
          />
        </div>
      </motion.div>
    </div>
  );
}
