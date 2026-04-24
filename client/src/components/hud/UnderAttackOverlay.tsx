import { motion, useReducedMotion } from "framer-motion";

/** Red-crimson ring + pulse overlay rendered on top of any HUD-plate-sized
 *  container to signal "one or more creatures are attacking this player."
 *
 *  Used by both `HudPlate` (1v1 + multiplayer single-opponent pill) and
 *  `OpponentHud`'s `OpponentTab` (multiplayer tab row) so the visual
 *  language is identical across every surface where a player can be
 *  targeted.
 *
 *  Composes with turn-pulse overlays — both pulses stack. If the calling
 *  surface also renders a rose/red turn pulse (opponent's turn), the
 *  stacking is intentional: "their turn" and "under attack" are distinct
 *  facts both worth surfacing. Dual pulses at different durations (1.2s vs
 *  0.9s) keep them distinguishable. */
export function UnderAttackOverlay() {
  const shouldReduceMotion = useReducedMotion();

  return (
    <>
      <div
        aria-hidden
        className="pointer-events-none absolute -inset-[3px] rounded-[20px] ring-2 ring-red-400/80"
      />
      {!shouldReduceMotion && (
        <motion.div
          aria-hidden
          className="pointer-events-none absolute -inset-0.5 rounded-[20px]"
          animate={{
            boxShadow: [
              "0 0 0 0 rgba(220,38,38,0.4), 0 0 16px 3px rgba(220,38,38,0.35)",
              "0 0 0 3px rgba(220,38,38,0.7), 0 0 28px 8px rgba(220,38,38,0.6)",
            ],
          }}
          transition={{
            duration: 0.9,
            repeat: Infinity,
            repeatType: "reverse",
            ease: "easeInOut",
          }}
        />
      )}
    </>
  );
}
