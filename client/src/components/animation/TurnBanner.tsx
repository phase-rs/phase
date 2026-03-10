import { motion, AnimatePresence, useReducedMotion } from "framer-motion";

import { useUiStore } from "../../stores/uiStore";

/**
 * Cinematic turn banner — dramatic horizontal sweep with glowing text,
 * decorative lines, and light rays. Mimics MTG Arena-style turn transitions.
 *
 * Self-contained: reads showTurnBanner / turnBannerText from the UI store.
 * Triggered via uiStore.flashTurnBanner().
 */
export function TurnBanner() {
  const showTurnBanner = useUiStore((s) => s.showTurnBanner);
  const turnBannerText = useUiStore((s) => s.turnBannerText);
  const shouldReduceMotion = useReducedMotion();

  const isYourTurn = turnBannerText.toUpperCase().includes("YOUR");

  const colors = isYourTurn
    ? {
        primary: "#fbbf24",
        glow: "rgba(251, 191, 36, 0.7)",
        glowStrong: "rgba(251, 191, 36, 0.9)",
        line: "rgba(251, 191, 36, 0.6)",
        lineFade: "rgba(251, 191, 36, 0)",
        bg: "rgba(120, 80, 0, 0.25)",
        ray: "rgba(251, 191, 36, 0.08)",
      }
    : {
        primary: "#94a3b8",
        glow: "rgba(148, 163, 184, 0.5)",
        glowStrong: "rgba(148, 163, 184, 0.7)",
        line: "rgba(148, 163, 184, 0.4)",
        lineFade: "rgba(148, 163, 184, 0)",
        bg: "rgba(51, 65, 85, 0.2)",
        ray: "rgba(148, 163, 184, 0.06)",
      };

  if (shouldReduceMotion) {
    return (
      <AnimatePresence>
        {showTurnBanner && (
          <motion.div
            className="fixed inset-0 z-50 flex items-center justify-center pointer-events-none"
            role="status"
            aria-live="polite"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.2 }}
          >
            <div className="absolute inset-0 bg-black/50" />
            <span
              className="relative text-5xl font-extrabold tracking-wider select-none"
              style={{ color: colors.primary }}
            >
              {turnBannerText}
            </span>
          </motion.div>
        )}
      </AnimatePresence>
    );
  }

  return (
    <AnimatePresence>
      {showTurnBanner && (
        <motion.div
          className="fixed inset-0 z-50 flex items-center justify-center pointer-events-none"
          role="status"
          aria-live="polite"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.25 }}
        >
          {/* Dark overlay */}
          <motion.div
            className="absolute inset-0 bg-black/60"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.2 }}
          />

          {/* Horizontal light ray burst from center */}
          <motion.div
            className="absolute inset-0"
            style={{
              background: `radial-gradient(ellipse 120% 40% at 50% 50%, ${colors.ray}, transparent)`,
            }}
            initial={{ opacity: 0, scaleX: 0.3 }}
            animate={{ opacity: [0, 1, 0.7], scaleX: [0.3, 1.2, 1] }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.6, ease: "easeOut" }}
          />

          {/* Banner strip background — horizontal bar */}
          <motion.div
            className="absolute left-0 right-0 flex items-center justify-center"
            style={{
              height: 80,
              top: "50%",
              marginTop: -40,
              background: `linear-gradient(180deg, transparent, ${colors.bg} 30%, ${colors.bg} 70%, transparent)`,
            }}
            initial={{ scaleX: 0, opacity: 0 }}
            animate={{ scaleX: 1, opacity: 1 }}
            exit={{ scaleX: 0, opacity: 0 }}
            transition={{ duration: 0.35, ease: [0.22, 1, 0.36, 1] }}
          />

          {/* Top decorative line */}
          <motion.div
            className="absolute left-0 right-0"
            style={{
              height: 2,
              top: "calc(50% - 36px)",
              background: `linear-gradient(90deg, ${colors.lineFade}, ${colors.line} 30%, ${colors.primary} 50%, ${colors.line} 70%, ${colors.lineFade})`,
            }}
            initial={{ scaleX: 0, opacity: 0 }}
            animate={{ scaleX: 1, opacity: 1 }}
            exit={{ scaleX: 0, opacity: 0 }}
            transition={{ duration: 0.4, ease: [0.22, 1, 0.36, 1], delay: 0.05 }}
          />

          {/* Bottom decorative line */}
          <motion.div
            className="absolute left-0 right-0"
            style={{
              height: 2,
              top: "calc(50% + 34px)",
              background: `linear-gradient(90deg, ${colors.lineFade}, ${colors.line} 30%, ${colors.primary} 50%, ${colors.line} 70%, ${colors.lineFade})`,
            }}
            initial={{ scaleX: 0, opacity: 0 }}
            animate={{ scaleX: 1, opacity: 1 }}
            exit={{ scaleX: 0, opacity: 0 }}
            transition={{ duration: 0.4, ease: [0.22, 1, 0.36, 1], delay: 0.05 }}
          />

          {/* Diamond accent — left */}
          <motion.div
            className="absolute"
            style={{
              width: 10,
              height: 10,
              top: "calc(50% - 5px)",
              left: "calc(50% - 160px)",
              background: colors.primary,
              transform: "rotate(45deg)",
              boxShadow: `0 0 12px ${colors.glow}`,
            }}
            initial={{ opacity: 0, scale: 0 }}
            animate={{ opacity: [0, 1, 0.8], scale: [0, 1.2, 1] }}
            exit={{ opacity: 0, scale: 0 }}
            transition={{ duration: 0.3, delay: 0.2 }}
          />

          {/* Diamond accent — right */}
          <motion.div
            className="absolute"
            style={{
              width: 10,
              height: 10,
              top: "calc(50% - 5px)",
              left: "calc(50% + 150px)",
              background: colors.primary,
              transform: "rotate(45deg)",
              boxShadow: `0 0 12px ${colors.glow}`,
            }}
            initial={{ opacity: 0, scale: 0 }}
            animate={{ opacity: [0, 1, 0.8], scale: [0, 1.2, 1] }}
            exit={{ opacity: 0, scale: 0 }}
            transition={{ duration: 0.3, delay: 0.2 }}
          />

          {/* Banner text — scales in with punch */}
          <motion.span
            className="relative text-5xl font-extrabold tracking-[0.2em] uppercase select-none"
            style={{
              color: colors.primary,
              textShadow: `0 0 20px ${colors.glow}, 0 0 40px ${colors.glow}, 0 0 60px ${colors.glowStrong}, 0 2px 4px rgba(0,0,0,0.5)`,
            }}
            initial={{ opacity: 0, scale: 1.6, y: 0 }}
            animate={{ opacity: [0, 1, 1], scale: [1.6, 0.95, 1], y: 0 }}
            exit={{ opacity: 0, scale: 0.9, y: 10 }}
            transition={{
              duration: 0.45,
              ease: [0.22, 1, 0.36, 1],
              delay: 0.08,
            }}
          >
            {turnBannerText}
          </motion.span>

          {/* Center glow pulse behind text */}
          <motion.div
            className="absolute"
            style={{
              width: 300,
              height: 60,
              top: "calc(50% - 30px)",
              left: "calc(50% - 150px)",
              borderRadius: "50%",
              background: `radial-gradient(ellipse, ${colors.glow}, transparent 70%)`,
              filter: "blur(8px)",
            }}
            initial={{ opacity: 0 }}
            animate={{ opacity: [0, 0.6, 0.3, 0.5] }}
            exit={{ opacity: 0 }}
            transition={{ duration: 1.2, ease: "easeOut" }}
          />
        </motion.div>
      )}
    </AnimatePresence>
  );
}
