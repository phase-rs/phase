import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { getPlayerId } from "../../hooks/usePlayerId";
import { getOpponentDisplayName } from "../../stores/multiplayerStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import type { DiceRollPayload } from "../../stores/uiStore";

// Code-split the WebGL renderer: `three` only loads when a die is actually
// rolled, keeping it out of the main bundle.
const Dice3D = lazy(() => import("./dice3d/Dice3D.tsx").then((m) => ({ default: m.Dice3D })));
const Coin3D = lazy(() => import("./dice3d/Coin3D.tsx").then((m) => ({ default: m.Coin3D })));

const DIE_SIZE = 132;

/** Cached WebGL-availability probe. The dice overlay is the first component in
 *  the app that needs WebGL, so it must degrade gracefully where it's absent. */
let webglSupported: boolean | null = null;
function hasWebGL(): boolean {
  if (webglSupported !== null) return webglSupported;
  try {
    const canvas = document.createElement("canvas");
    webglSupported = Boolean(canvas.getContext("webgl2") ?? canvas.getContext("webgl"));
  } catch {
    webglSupported = false;
  }
  return webglSupported;
}

/**
 * Full-screen dice-roll / coin-flip moment. Gated on `uiStore.diceRoll` (set by
 * `flashDiceRoll`), it animates the engine's already-known result in real 3D.
 * Mirrors the TurnBanner pattern: `fixed inset-0 z-50`, AnimatePresence,
 * pointer-events-none. Falls back to a static result under reduced-motion or
 * when WebGL is unavailable — the roll is cosmetic, so degrading is safe.
 */
export function DiceRollOverlay() {
  const diceRoll = useUiStore((s) => s.diceRoll);
  const shouldReduceMotion = useReducedMotion();

  // Clear any active/queued roll and its advance timer when leaving the game.
  // The store is a module singleton that outlives this mount, so without this an
  // in-flight roll could pop into the next game.
  useEffect(() => () => useUiStore.getState().resetDiceRoll(), []);

  return (
    <AnimatePresence>
      {diceRoll && (
        <motion.div
          className="fixed inset-0 z-50 flex items-center justify-center pointer-events-none"
          role="status"
          aria-live="polite"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.25 }}
        >
          <div className="absolute inset-0 bg-black/55" />
          {/* Keyed by payload identity so each roll the FIFO advances to gets a
              fresh component instance (resets `settled`, re-runs the 3D mount). */}
          <DiceRollContent
            key={diceRollKey(diceRoll)}
            payload={diceRoll}
            animate={!shouldReduceMotion && hasWebGL()}
          />
        </motion.div>
      )}
    </AnimatePresence>
  );
}

/** Stable identity for a payload so the FIFO advancing from one roll to the next
 *  remounts `DiceRollContent` instead of reconciling stale `settled` state. */
function diceRollKey(payload: DiceRollPayload): string {
  return payload.kind === "coin"
    ? `coin-${payload.context}-${payload.playerId}-${payload.won}`
    : `die-${payload.context}-${payload.rolls.map((r) => `${r.playerId}:${r.value}`).join(",")}`;
}

function DiceRollContent({ payload, animate }: { payload: DiceRollPayload; animate: boolean }) {
  const { t } = useTranslation();
  const speedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);
  const [settled, setSettled] = useState(false);
  const onSettle = useCallback(() => setSettled(true), []);

  const playerLabel = (playerId: number): string =>
    playerId === getPlayerId() ? t("diceRoll.you") : getOpponentDisplayName(playerId);

  if (payload.kind === "coin") {
    // No engine-named face: `won` (relative to the flipping player) maps to a
    // heads/tails depiction. We show "heads" on a win — a pure display choice.
    const face = payload.won ? "heads" : "tails";
    return (
      <div className="relative flex flex-col items-center gap-6 select-none">
        <span className="text-2xl font-bold tracking-wider uppercase text-slate-200">
          {playerLabel(payload.playerId)}
        </span>
        {animate ? (
          <Suspense fallback={<DiePlaceholder label="" />}>
            <Coin3D
              face={face}
              speedMultiplier={speedMultiplier}
              onSettle={onSettle}
              size={DIE_SIZE}
              labels={{ heads: t("diceRoll.heads"), tails: t("diceRoll.tails") }}
            />
          </Suspense>
        ) : (
          <DiePlaceholder label={t(`diceRoll.${face}`)} />
        )}
      </div>
    );
  }

  // Dice: one per roll. The starting-player contest highlights the engine's
  // winner and captions who plays first; in-game rolls just show the value(s).
  const isContest = payload.context === "startingPlayer";
  const winnerIsYou = payload.winner === getPlayerId();
  const caption =
    isContest && payload.winner != null
      ? winnerIsYou
        ? t("diceRoll.youPlayFirst")
        : t("diceRoll.playerPlaysFirst", { name: getOpponentDisplayName(payload.winner) })
      : null;

  return (
    <div className="relative flex flex-col items-center gap-7 select-none">
      <div className="flex items-end justify-center gap-10">
        {payload.rolls.map((roll, i) => {
          const isWinner = isContest && roll.playerId === payload.winner;
          return (
            <div key={i} className="flex flex-col items-center gap-3">
              {isContest && (
                <span
                  className="text-lg font-bold tracking-wide uppercase"
                  style={{ color: isWinner ? "#fbbf24" : "#94a3b8" }}
                >
                  {playerLabel(roll.playerId)}
                </span>
              )}
              <div
                className="rounded-2xl"
                style={
                  isWinner
                    ? { boxShadow: "0 0 28px rgba(251,191,36,0.55)", outline: "2px solid rgba(251,191,36,0.7)" }
                    : undefined
                }
              >
                {animate ? (
                  <Suspense fallback={<DiePlaceholder label={String(roll.value)} />}>
                    <Dice3D
                      sides={payload.sides}
                      result={roll.value}
                      speedMultiplier={speedMultiplier}
                      onSettle={i === 0 ? onSettle : undefined}
                      size={DIE_SIZE}
                    />
                  </Suspense>
                ) : (
                  <DiePlaceholder label={String(roll.value)} />
                )}
              </div>
            </div>
          );
        })}
      </div>
      {caption && (
        <motion.span
          className="text-4xl font-extrabold tracking-wider uppercase"
          style={{
            color: "#fbbf24",
            textShadow: "0 0 20px rgba(251,191,36,0.6), 0 2px 4px rgba(0,0,0,0.5)",
          }}
          initial={{ opacity: 0, scale: 0.9 }}
          animate={animate && !settled ? { opacity: 0 } : { opacity: 1, scale: 1 }}
          transition={{ duration: 0.4, ease: [0.22, 1, 0.36, 1] }}
        >
          {caption}
        </motion.span>
      )}
    </div>
  );
}

/** Static stand-in for the 3D die: the result face as plain text. Used as the
 *  Suspense fallback while `three` loads, and as the reduced-motion / no-WebGL
 *  presentation. */
function DiePlaceholder({ label }: { label: string }) {
  return (
    <div
      className="flex items-center justify-center rounded-2xl bg-slate-800/90 font-extrabold text-slate-100"
      style={{ width: DIE_SIZE, height: DIE_SIZE, fontSize: DIE_SIZE * 0.42 }}
    >
      {label}
    </div>
  );
}
