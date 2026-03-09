import { AnimatePresence, motion } from "framer-motion";

import { usePreferencesStore } from "../../stores/preferencesStore";

interface TurnBannerProps {
  turnNumber: number;
  isPlayerTurn: boolean;
  speedMultiplier: number;
  onComplete?: () => void;
}

const AMBER = {
  banner: "rgba(245, 158, 11, 0.8)",
  glow: "#F59E0B",
  diamond: "rgba(245, 158, 11, 0.7)",
  textShadow: (blur: number) => `0 0 ${blur}px #F59E0B`,
};

const SLATE = {
  banner: "rgba(100, 116, 139, 0.8)",
  glow: "#64748B",
  diamond: "rgba(100, 116, 139, 0.7)",
  textShadow: (blur: number) => `0 0 ${blur}px #64748B`,
};

export function TurnBanner({
  turnNumber,
  isPlayerTurn,
  speedMultiplier,
  onComplete,
}: TurnBannerProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const theme = isPlayerTurn ? AMBER : SLATE;
  const label = isPlayerTurn ? "YOUR TURN" : "THEIR TURN";
  const totalDuration = 1.5 * speedMultiplier;

  const tripleGlow = `${theme.textShadow(8)}, ${theme.textShadow(20)}, ${theme.textShadow(40)}`;

  // Minimal: just text with fade
  if (vfxQuality === "minimal") {
    return (
      <AnimatePresence onExitComplete={onComplete}>
        <motion.div
          key={`turn-${turnNumber}`}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.3 * speedMultiplier }}
          style={{
            position: "fixed",
            top: "50%",
            left: 0,
            right: 0,
            transform: "translateY(-50%)",
            zIndex: 50,
            pointerEvents: "none",
            display: "flex",
            justifyContent: "center",
            alignItems: "center",
            padding: "1rem 0",
          }}
        >
          <span
            style={{
              color: "white",
              fontSize: "2rem",
              fontWeight: 800,
              letterSpacing: "0.15em",
              textTransform: "uppercase",
              textShadow: tripleGlow,
            }}
          >
            {label}
          </span>
        </motion.div>
      </AnimatePresence>
    );
  }

  // Reduced: banner strip + text, no light burst or diamonds
  if (vfxQuality === "reduced") {
    return (
      <AnimatePresence onExitComplete={onComplete}>
        <motion.div
          key={`turn-${turnNumber}`}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0, x: "100%" }}
          transition={{ duration: totalDuration, times: [0, 0.1, 0.8, 1] }}
          style={{
            position: "fixed",
            top: "50%",
            left: 0,
            right: 0,
            transform: "translateY(-50%)",
            zIndex: 50,
            pointerEvents: "none",
          }}
        >
          {/* Banner strip */}
          <motion.div
            initial={{ x: "-100%" }}
            animate={{ x: "0%" }}
            exit={{ x: "100%", opacity: 0 }}
            transition={{
              duration: 0.3 * speedMultiplier,
              ease: "easeOut",
            }}
            style={{
              height: 60,
              backgroundColor: theme.banner,
              display: "flex",
              justifyContent: "center",
              alignItems: "center",
            }}
          >
            {/* Text */}
            <motion.span
              initial={{ scale: 0.5, opacity: 0 }}
              animate={{ scale: 1, opacity: 1 }}
              exit={{ opacity: 0 }}
              transition={{
                delay: 0.2 * speedMultiplier,
                duration: 0.3 * speedMultiplier,
                ease: "easeOut",
              }}
              style={{
                color: "white",
                fontSize: "2rem",
                fontWeight: 800,
                letterSpacing: "0.15em",
                textTransform: "uppercase",
                textShadow: tripleGlow,
              }}
            >
              {label}
            </motion.span>
          </motion.div>
        </motion.div>
      </AnimatePresence>
    );
  }

  // Full quality: layered cinematic effect
  return (
    <AnimatePresence onExitComplete={onComplete}>
      <motion.div
        key={`turn-${turnNumber}`}
        initial={{ opacity: 1 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0, x: "10%" }}
        transition={{
          exit: { duration: 0.3 * speedMultiplier, ease: "easeIn" },
        }}
        style={{
          position: "fixed",
          top: "50%",
          left: 0,
          right: 0,
          transform: "translateY(-50%)",
          zIndex: 50,
          pointerEvents: "none",
          display: "flex",
          justifyContent: "center",
          alignItems: "center",
          height: 120,
        }}
      >
        {/* Phase 1: Light burst (0-0.3s) */}
        <motion.div
          initial={{ scale: 0, opacity: 0.4 }}
          animate={{ scale: 3, opacity: 0 }}
          transition={{
            duration: 0.3 * speedMultiplier,
            ease: "easeOut",
          }}
          style={{
            position: "absolute",
            width: 200,
            height: 200,
            borderRadius: "50%",
            background: `radial-gradient(circle, rgba(255,255,255,0.3), transparent)`,
          }}
        />

        {/* Phase 2: Banner strip (0.1-0.4s) */}
        <motion.div
          initial={{ x: "-100%" }}
          animate={{ x: "0%" }}
          transition={{
            delay: 0.1 * speedMultiplier,
            duration: 0.3 * speedMultiplier,
            ease: "easeOut",
          }}
          style={{
            position: "absolute",
            left: 0,
            right: 0,
            height: 60,
            backgroundColor: theme.banner,
          }}
        />

        {/* Phase 3: Diamond accents (0.3-0.5s) */}
        <motion.div
          initial={{ scale: 0, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          transition={{
            delay: 0.3 * speedMultiplier,
            duration: 0.2 * speedMultiplier,
            ease: "easeOut",
          }}
          style={{
            position: "absolute",
            left: 40,
            width: 16,
            height: 16,
            border: `2px solid ${theme.diamond}`,
            transform: "rotate(45deg)",
          }}
        />
        <motion.div
          initial={{ scale: 0, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          transition={{
            delay: 0.3 * speedMultiplier,
            duration: 0.2 * speedMultiplier,
            ease: "easeOut",
          }}
          style={{
            position: "absolute",
            right: 40,
            width: 16,
            height: 16,
            border: `2px solid ${theme.diamond}`,
            transform: "rotate(45deg)",
          }}
        />

        {/* Phase 4: Text punch (0.3-0.6s) with hold pulse (0.6-1.2s) */}
        <motion.span
          initial={{ scale: 0.5, opacity: 0 }}
          animate={{
            scale: [0.5, 1, 1, 1.02, 1, 1.02, 1],
            opacity: [0, 1, 1, 1, 1, 1, 1],
          }}
          transition={{
            delay: 0.3 * speedMultiplier,
            duration: 0.9 * speedMultiplier,
            times: [0, 0.33, 0.4, 0.55, 0.7, 0.85, 1],
            ease: "easeOut",
          }}
          style={{
            position: "relative",
            zIndex: 1,
            color: "white",
            fontSize: "2.25rem",
            fontWeight: 800,
            letterSpacing: "0.15em",
            textTransform: "uppercase",
            textShadow: tripleGlow,
          }}
        >
          {label}
        </motion.span>
      </motion.div>
    </AnimatePresence>
  );
}
