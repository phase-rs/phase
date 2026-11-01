import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { type UnFx, type UnFxKind, useUnActionFxStore } from "../../stores/unActionFxStore.ts";

/** How long each gag stays on screen before it removes itself. */
const FX_DURATION_MS = 2200;

const GLYPHS: Record<UnFxKind, string> = {
  touchFace: "\u{1F926}", // 🤦 facepalm
  touchTable: "\u{1F590}\u{FE0F}", // 🖐️ open hand
  laugh: "\u{1F923}", // 🤣
  flickCards: "\u{1F0CF}", // 🃏
  saysWord: "\u{1F4AC}", // 💬
  phrase: "\u{1F64F}", // 🙏
  gotcha: "\u{1F449}", // 👉
};

/** Where the confetti-ish sparkle trail sits around the main glyph. */
const SPARKLES = ["✨", "\u{1F4A5}", "⭐", "✨", "\u{1F4AB}"];

function GagBody({ fx, reduced }: { fx: UnFx; reduced: boolean }) {
  const { t } = useTranslation("game");
  const glyph = GLYPHS[fx.kind];

  switch (fx.kind) {
    case "touchFace":
      // A hand smacks the face; the face wobbles and goes pink.
      return (
        <div className="relative text-center">
          <motion.div
            className="text-8xl"
            animate={reduced ? undefined : { rotate: [0, -14, 10, -8, 4, 0], scale: [1, 1.25, 0.9, 1.1, 1] }}
            transition={{ duration: 0.9 }}
          >
            {glyph}
          </motion.div>
          <motion.div
            className="absolute -right-10 -top-4 text-6xl"
            initial={{ x: 90, opacity: 0 }}
            animate={reduced ? { opacity: 1 } : { x: [90, -6, 12, 0], opacity: [0, 1, 1, 1], rotate: [20, -20, 0] }}
            transition={{ duration: 0.5 }}
          >
            {"\u{1F44B}"}
          </motion.div>
          <p className="mt-2 text-2xl font-black uppercase tracking-widest text-pink-200">
            {t("unActions.fx.touchFace")}
          </p>
        </div>
      );
    case "touchTable":
      // The hand slams down and the whole table (the overlay) shakes.
      return (
        <motion.div
          className="text-center"
          animate={reduced ? undefined : { x: [0, -14, 14, -10, 10, -4, 0], y: [0, 6, -4, 3, 0] }}
          transition={{ duration: 0.7, delay: 0.25 }}
        >
          <motion.div
            className="text-9xl"
            initial={{ y: -220, scale: 1.6 }}
            animate={{ y: 0, scale: 1 }}
            transition={{ type: "spring", stiffness: 520, damping: 12 }}
          >
            {glyph}
          </motion.div>
          <p className="mt-2 text-4xl font-black uppercase tracking-widest text-amber-200">
            {t("unActions.fx.touchTable")}
          </p>
        </motion.div>
      );
    case "laugh":
      // A row of ever-bouncing laughing faces with a HA HA HA ticker.
      return (
        <div className="text-center">
          <div className="flex items-end justify-center gap-2">
            {[0, 1, 2].map((i) => (
              <motion.span
                key={i}
                className="text-6xl"
                animate={reduced ? undefined : { y: [0, -34, 0], rotate: [0, i % 2 ? 18 : -18, 0] }}
                transition={{ duration: 0.45, repeat: 3, delay: i * 0.12 }}
              >
                {glyph}
              </motion.span>
            ))}
          </div>
          <p className="mt-2 text-3xl font-black uppercase tracking-widest text-yellow-200">
            {t("unActions.fx.laugh")}
          </p>
        </div>
      );
    case "flickCards":
      // Cards fan out and zip away with each flick.
      return (
        <div className="relative h-32 w-72 text-center">
          {[0, 1, 2, 3, 4].map((i) => (
            <motion.span
              key={i}
              className="absolute left-1/2 top-8 text-6xl"
              initial={{ x: -20, y: 0, opacity: 0, rotate: 0 }}
              animate={
                reduced
                  ? { opacity: 1 }
                  : { x: [-20, 130 - i * 8], y: [0, -50 + i * 18], opacity: [0, 1, 0], rotate: [0, 260 + i * 40] }
              }
              transition={{ duration: 0.9, delay: i * 0.14 }}
            >
              {glyph}
            </motion.span>
          ))}
          <p className="absolute inset-x-0 bottom-0 text-2xl font-black uppercase tracking-widest text-sky-200">
            {t("unActions.fx.flickCards")}
          </p>
        </div>
      );
    case "saysWord":
    case "phrase":
      // A speech bubble pops in and the words shout themselves out letter by letter.
      return (
        <div className="max-w-md text-center">
          <motion.div
            className="relative inline-block rounded-3xl border-4 border-white/80 bg-white px-6 py-4 shadow-2xl"
            initial={{ scale: 0.2, rotate: -12 }}
            animate={reduced ? { scale: 1 } : { scale: [0.2, 1.25, 0.95, 1.05, 1], rotate: [-12, 5, -3, 0] }}
            transition={{ duration: 0.6 }}
          >
            <span className="text-3xl font-black text-slate-900 [overflow-wrap:anywhere]">
              {fx.text}
            </span>
            <span className="absolute -bottom-5 left-8 h-0 w-0 border-x-[14px] border-t-[22px] border-x-transparent border-t-white" />
          </motion.div>
          <motion.div
            className="mt-6 text-6xl"
            animate={reduced ? undefined : { y: [0, -10, 0], scale: [1, 1.15, 1] }}
            transition={{ duration: 0.6, repeat: 2 }}
          >
            {glyph}
          </motion.div>
        </div>
      );
    case "gotcha":
      // A giant pointing finger jabs in, "GOTCHA!" slams down, confetti bursts.
      return (
        <div className="relative text-center">
          <motion.div
            className="text-9xl"
            initial={{ x: 240, rotate: 40, opacity: 0 }}
            animate={reduced ? { x: 0, opacity: 1 } : { x: [240, -20, 0], rotate: [40, -8, -10], opacity: [0, 1, 1] }}
            transition={{ duration: 0.45 }}
          >
            {glyph}
          </motion.div>
          <motion.p
            className="mt-2 text-6xl font-black uppercase tracking-wider text-rose-300 drop-shadow-[0_4px_0_rgba(0,0,0,0.6)]"
            initial={{ scale: 3, opacity: 0 }}
            animate={{ scale: 1, opacity: 1 }}
            transition={{ type: "spring", stiffness: 420, damping: 11, delay: 0.3 }}
          >
            {t("unActions.gotcha.button")}
          </motion.p>
          {!reduced &&
            SPARKLES.map((sparkle, i) => (
              <motion.span
                key={i}
                className="absolute left-1/2 top-1/2 text-4xl"
                initial={{ x: 0, y: 0, opacity: 0, scale: 0.4 }}
                animate={{
                  x: Math.cos((i / SPARKLES.length) * Math.PI * 2) * 170,
                  y: Math.sin((i / SPARKLES.length) * Math.PI * 2) * 120,
                  opacity: [0, 1, 0],
                  scale: [0.4, 1.4, 0.8],
                }}
                transition={{ duration: 1.1, delay: 0.35 }}
              >
                {sparkle}
              </motion.span>
            ))}
        </div>
      );
  }
}

function Gag({ fx }: { fx: UnFx }) {
  const remove = useUnActionFxStore((s) => s.remove);
  const reduced = useReducedMotion() ?? false;

  useEffect(() => {
    const timer = window.setTimeout(() => remove(fx.id), FX_DURATION_MS);
    return () => window.clearTimeout(timer);
  }, [fx.id, remove]);

  return (
    <motion.div
      className="pointer-events-none absolute inset-0 flex items-center justify-center"
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0, scale: 0.9 }}
      transition={{ duration: 0.15 }}
    >
      <GagBody fx={fx} reduced={reduced} />
    </motion.div>
  );
}

/**
 * The silly one-shot animations for Un-card gestures, spoken phrases and
 * "Gotcha!" calls. Purely presentational: the engine events that feed it are
 * reported by `flashUnActionEvents`.
 */
export function UnActionOverlay() {
  const effects = useUnActionFxStore((s) => s.effects);

  return (
    <div
      className="pointer-events-none fixed inset-0 z-[53]"
      aria-live="polite"
      data-testid="un-action-overlay"
    >
      <AnimatePresence>
        {effects.map((fx) => (
          <Gag key={fx.id} fx={fx} />
        ))}
      </AnimatePresence>
    </div>
  );
}
