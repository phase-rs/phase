import { lazy, Suspense, useCallback, useEffect, useState, type ReactNode } from "react";
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

// Accent RGB triples (sans `rgb(...)`) so they compose into rgba()/gradients.
const GOLD = "251,191,36"; // amber-400 — the winner / emphasis accent
const NEUTRAL = "148,163,184"; // slate-400 — a non-decisive die

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
 * Mirrors the TurnBanner pattern: `fixed inset-0`, AnimatePresence,
 * pointer-events-none. Falls back to a static result under reduced-motion or
 * when WebGL is unavailable — the roll is cosmetic, so degrading is safe.
 *
 * Sits at `z-[55]` — above the `z-50` board overlays (turn banner, mulligan) so
 * the roll is never occluded mid-animation. The starting-player contest is also
 * sequenced ahead of the mulligan UI in `GamePage` (CR 103.1 before CR 103.5),
 * so the two never compete; the raised z-index is defense-in-depth for in-game
 * rolls that can fire while other overlays are up.
 */
export function DiceRollOverlay() {
  const diceRoll = useUiStore((s) => s.diceRoll);
  const skipDiceRoll = useUiStore((s) => s.skipDiceRoll);
  const shouldReduceMotion = useReducedMotion();
  const { t } = useTranslation();

  // Clear any active/queued roll and its advance timer when leaving the game.
  // The store is a module singleton that outlives this mount, so without this an
  // in-flight roll could pop into the next game.
  useEffect(() => () => useUiStore.getState().resetDiceRoll(), []);

  // Tap-to-skip via keyboard: Escape dismisses the current roll (advancing to
  // the next queued one, or clearing the overlay). Bound only while a roll is
  // showing so it never shadows Escape elsewhere.
  useEffect(() => {
    if (!diceRoll) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        skipDiceRoll();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [diceRoll, skipDiceRoll]);

  // Prefetch the code-split 3D chunk on mount. Every game opens with the
  // starting-player contest (CR 103.1), so `three` is needed within seconds —
  // warming it here means the first roll animates instead of flashing the static
  // fallback while the chunk downloads. Skipped where we'd never animate.
  useEffect(() => {
    if (shouldReduceMotion || !hasWebGL()) return;
    void import("./dice3d/Dice3D.tsx");
    void import("./dice3d/Coin3D.tsx");
  }, [shouldReduceMotion]);

  return (
    <AnimatePresence>
      {diceRoll && (
        <motion.div
          className="fixed inset-0 z-[55] flex items-center justify-center pointer-events-none"
          role="status"
          aria-live="polite"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.25 }}
        >
          {/* Vignette backdrop: darker at the edges, focusing the eye centrally.
              Clickable (pointer-events re-enabled) to skip the roll — the overlay
              itself stays pointer-events-none so it never traps board input. */}
          <button
            type="button"
            aria-label={t("diceRoll.skip")}
            onClick={skipDiceRoll}
            className="absolute inset-0 cursor-pointer bg-[radial-gradient(circle_at_center,rgba(2,6,23,0.55),rgba(2,6,23,0.86)_70%)] backdrop-blur-[2px] pointer-events-auto"
          />
          {/* Keyed by payload identity so each roll the FIFO advances to gets a
              fresh component instance (resets settle state, re-runs the 3D mount). */}
          <DiceRollContent
            key={diceRollKey(diceRoll)}
            payload={diceRoll}
            animate={!shouldReduceMotion && hasWebGL()}
          />
          <motion.span
            className="pointer-events-none absolute bottom-8 text-xs font-medium uppercase tracking-[0.2em] text-slate-400/70"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            transition={{ delay: 1, duration: 0.5 }}
          >
            {t("diceRoll.skip")}
          </motion.span>
        </motion.div>
      )}
    </AnimatePresence>
  );
}

/** Stable identity for a payload so the FIFO advancing from one roll to the next
 *  remounts `DiceRollContent` instead of reconciling stale settle state. */
function diceRollKey(payload: DiceRollPayload): string {
  return payload.kind === "coin"
    ? `coin-${payload.context}-${payload.playerId}-${payload.won}`
    : `die-${payload.context}-${payload.rolls.map((r) => `${r.playerId}:${r.value}`).join(",")}`;
}

function DiceRollContent({ payload, animate }: { payload: DiceRollPayload; animate: boolean }) {
  const { t } = useTranslation();
  const speedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);

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
        <DieFace animate={animate} accent={NEUTRAL}>
          {(handleSettle) =>
            animate ? (
              <Suspense fallback={<DiePlaceholder label="" />}>
                <Coin3D
                  face={face}
                  speedMultiplier={speedMultiplier}
                  onSettle={handleSettle}
                  size={DIE_SIZE}
                  labels={{ heads: t("diceRoll.heads"), tails: t("diceRoll.tails") }}
                />
              </Suspense>
            ) : (
              <DiePlaceholder label={t(`diceRoll.${face}`)} />
            )
          }
        </DieFace>
      </div>
    );
  }

  return payload.context === "startingPlayer" ? (
    <ContestDice
      payload={payload}
      animate={animate}
      speedMultiplier={speedMultiplier}
      playerLabel={playerLabel}
    />
  ) : (
    <InGameDice payload={payload} animate={animate} speedMultiplier={speedMultiplier} />
  );
}

/**
 * Starting-player contest (CR 103.1): one die per seat, a VS framing for the
 * two-player case, then a winner reveal once every die has settled — the
 * winner's panel lifts and glows gold, the others dim, and the caption resolves.
 * `winner` is the engine's authoritative pick, never recomputed here.
 */
function ContestDice({
  payload,
  animate,
  speedMultiplier,
  playerLabel,
}: {
  payload: Extract<DiceRollPayload, { kind: "die" }>;
  animate: boolean;
  speedMultiplier: number;
  playerLabel: (playerId: number) => string;
}) {
  const { t } = useTranslation();
  const [settledCount, setSettledCount] = useState(0);
  const onDieSettle = useCallback(() => setSettledCount((c) => c + 1), []);
  // Without animation the result is shown immediately, so treat it as settled.
  const allSettled = !animate || settledCount >= payload.rolls.length;

  const winner = payload.winner;
  const winnerIsYou = winner === getPlayerId();
  const caption =
    winner != null
      ? winnerIsYou
        ? t("diceRoll.youPlayFirst")
        : t("diceRoll.playerPlaysFirst", { name: getOpponentDisplayName(winner) })
      : null;

  return (
    <div className="relative flex flex-col items-center gap-8 select-none">
      <motion.span
        className="text-sm font-semibold uppercase tracking-[0.25em] text-slate-400"
        initial={{ opacity: 0, y: -8 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.4 }}
      >
        {t("diceRoll.rollingForFirst")}
      </motion.span>

      <div className="flex items-center justify-center gap-8">
        {payload.rolls.map((roll, i) => {
          const isWinner = roll.playerId === winner;
          const revealed = allSettled && winner != null;
          return (
            <div key={roll.playerId} className="contents">
              {/* `VS` separator between two contestants. */}
              {i > 0 && (
                <motion.span
                  className="text-xl font-black italic tracking-tight text-slate-500"
                  initial={{ opacity: 0, scale: 0.6 }}
                  animate={{ opacity: 1, scale: 1 }}
                  transition={{ delay: 0.2, duration: 0.3 }}
                >
                  {t("diceRoll.versus")}
                </motion.span>
              )}
              <motion.div
                className="flex flex-col items-center gap-3"
                animate={{
                  scale: revealed ? (isWinner ? 1.06 : 0.94) : 1,
                  opacity: revealed && !isWinner ? 0.45 : 1,
                }}
                transition={{ type: "spring", stiffness: 260, damping: 22 }}
              >
                <span
                  className="text-base font-bold uppercase tracking-wide transition-colors"
                  style={{ color: revealed && isWinner ? `rgb(${GOLD})` : "#cbd5e1" }}
                >
                  {playerLabel(roll.playerId)}
                </span>
                <DieFace
                  animate={animate}
                  accent={isWinner ? GOLD : NEUTRAL}
                  emphasize={revealed && isWinner}
                  value={roll.value}
                  onSettle={onDieSettle}
                >
                  {(handleSettle) =>
                    animate ? (
                      <Suspense fallback={<DiePlaceholder label={String(roll.value)} />}>
                        <Dice3D
                          sides={payload.sides}
                          result={roll.value}
                          speedMultiplier={speedMultiplier}
                          size={DIE_SIZE}
                          onSettle={handleSettle}
                        />
                      </Suspense>
                    ) : (
                      <DiePlaceholder label={String(roll.value)} />
                    )
                  }
                </DieFace>
              </motion.div>
            </div>
          );
        })}
      </div>

      <div className="flex h-10 items-center">
        <AnimatePresence>
          {allSettled && caption && (
            <motion.span
              className="text-4xl font-extrabold uppercase tracking-wider"
              style={{
                color: `rgb(${GOLD})`,
                textShadow: `0 0 22px rgba(${GOLD},0.6), 0 2px 4px rgba(0,0,0,0.5)`,
              }}
              initial={{ opacity: 0, scale: 0.85, y: 6 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              transition={{ duration: 0.4, ease: [0.22, 1, 0.36, 1] }}
            >
              {caption}
            </motion.span>
          )}
        </AnimatePresence>
      </div>
    </div>
  );
}

/**
 * In-game rolls (CR 705 / dN rolls): one die per result, grouped (e.g. a
 * Krark's-Thumb double), with the same landing flourish but no winner framing.
 */
function InGameDice({
  payload,
  animate,
  speedMultiplier,
}: {
  payload: Extract<DiceRollPayload, { kind: "die" }>;
  animate: boolean;
  speedMultiplier: number;
}) {
  return (
    <div className="relative flex items-end justify-center gap-10 select-none">
      {payload.rolls.map((roll, i) => (
        <DieFace key={i} animate={animate} accent={NEUTRAL} value={roll.value}>
          {(handleSettle) =>
            animate ? (
              <Suspense fallback={<DiePlaceholder label={String(roll.value)} />}>
                <Dice3D
                  sides={payload.sides}
                  result={roll.value}
                  speedMultiplier={speedMultiplier}
                  size={DIE_SIZE}
                  onSettle={handleSettle}
                />
              </Suspense>
            ) : (
              <DiePlaceholder label={String(roll.value)} />
            )
          }
        </DieFace>
      ))}
    </div>
  );
}

/**
 * Wraps a single die/coin and plays a landing flourish the moment it settles: a
 * scale "pop", an expanding accent ring, and a soft radial flash. `emphasize`
 * adds a lingering glow for the contest winner. The die is supplied as a render
 * prop so the wrapper can hand its settle callback to whichever 3D child (Dice3D
 * / Coin3D) it renders; the static fallback settles on mount instead.
 */
function DieFace({
  children,
  animate,
  accent,
  emphasize = false,
  value,
  onSettle,
}: {
  children: (handleSettle: () => void) => ReactNode;
  animate: boolean;
  accent: string;
  emphasize?: boolean;
  /** When set, a numeric result badge pops in below the die as it lands. */
  value?: number;
  onSettle?: () => void;
}) {
  // Static fallbacks never fire a 3D `onSettle`, so they start settled.
  const [settled, setSettled] = useState(!animate);
  const handleSettle = useCallback(() => {
    setSettled(true);
    onSettle?.();
  }, [onSettle]);

  // The static fallback has no settle event of its own — register it once so a
  // parent counting settles (the contest winner reveal) still reaches its total.
  useEffect(() => {
    if (!animate) onSettle?.();
  }, [animate, onSettle]);

  return (
    <motion.div
      className="relative flex flex-col items-center gap-2"
      animate={settled ? { scale: [1, 1.14, 1] } : { scale: 1 }}
      transition={{ duration: 0.36, ease: [0.34, 1.56, 0.64, 1] }}
    >
      <div className="relative rounded-2xl" style={{ width: DIE_SIZE, height: DIE_SIZE }}>
        {/* Lingering glow for the emphasized (winner) die. */}
        {emphasize && (
          <motion.div
            className="absolute -inset-1 rounded-2xl pointer-events-none"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            transition={{ duration: 0.4 }}
            style={{
              boxShadow: `0 0 30px 6px rgba(${accent},0.55)`,
              outline: `2px solid rgba(${accent},0.7)`,
            }}
          />
        )}

        {/* Settle flourish: expanding ring + radial flash, one-shot. */}
        {settled && animate && (
          <>
            <motion.div
              className="absolute inset-0 rounded-2xl pointer-events-none"
              initial={{ opacity: 0.85, scale: 0.65 }}
              animate={{ opacity: 0, scale: 1.7 }}
              transition={{ duration: 0.6, ease: "easeOut" }}
              style={{ border: `2px solid rgba(${accent},0.8)` }}
            />
            <motion.div
              className="absolute inset-0 rounded-2xl pointer-events-none"
              initial={{ opacity: 0.55, scale: 0.8 }}
              animate={{ opacity: 0, scale: 1.35 }}
              transition={{ duration: 0.45, ease: "easeOut" }}
              style={{ background: `radial-gradient(circle, rgba(${accent},0.5), transparent 70%)` }}
            />
          </>
        )}

        <div className="relative h-full w-full">{children(handleSettle)}</div>
      </div>

      {/* Result badge: the engine's rolled value, revealed as the die lands so
          the outcome is legible without reading the cluttered polyhedron face. */}
      {value != null && (
        <AnimatePresence>
          {settled && (
            <motion.span
              className="min-w-9 rounded-md px-2 py-0.5 text-center text-xl font-extrabold tabular-nums"
              initial={{ opacity: 0, scale: 0.6, y: -4 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={{ opacity: 0 }}
              transition={{ type: "spring", stiffness: 320, damping: 18 }}
              style={{
                color: `rgb(${accent})`,
                backgroundColor: `rgba(${accent},0.12)`,
                border: `1px solid rgba(${accent},0.4)`,
              }}
            >
              {value}
            </motion.span>
          )}
        </AnimatePresence>
      )}
    </motion.div>
  );
}

/** Static stand-in for the 3D die: the result face as plain text. Used as the
 *  Suspense fallback while `three` loads, and as the reduced-motion / no-WebGL
 *  presentation. */
function DiePlaceholder({ label }: { label: string }) {
  return (
    <div
      className="flex h-full w-full items-center justify-center rounded-2xl bg-slate-800/90 font-extrabold text-slate-100"
      style={{ fontSize: DIE_SIZE * 0.42 }}
    >
      {label}
    </div>
  );
}
