import { AnimatePresence, motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore";

interface DamageVignetteProps {
  active: boolean;
  damageAmount: number;
  speedMultiplier: number;
}

export function DamageVignette({
  active,
  damageAmount,
  speedMultiplier,
}: DamageVignetteProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);

  if (vfxQuality === "reduced" || vfxQuality === "minimal") {
    return null;
  }

  const opacity = Math.min(Math.max(damageAmount * 0.15, 0.2), 0.8);

  return (
    <AnimatePresence>
      {active && (
        <motion.div
          key="damage-vignette"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.2 * speedMultiplier }}
          style={{
            position: "fixed",
            inset: 0,
            pointerEvents: "none",
            zIndex: 45,
            background: `radial-gradient(ellipse at center, transparent 40%, rgba(239,68,68,${opacity}) 100%)`,
          }}
        />
      )}
    </AnimatePresence>
  );
}
