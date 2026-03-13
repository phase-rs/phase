import { motion, useReducedMotion } from "framer-motion";

export function MenuLogo() {
  const prefersReducedMotion = useReducedMotion();
  const sigilFrame = {
    left: "50%",
    top: "35%",
    transform: "translate(-50%, -50%)",
  };

  return (
    <div className="relative flex items-center justify-center">
      <div className="relative w-64 max-w-[60vw]">
        <motion.div
          className="pointer-events-none absolute"
          style={{
            ...sigilFrame,
            width: "220%",
            height: "150%",
            borderRadius: "50%",
            background:
              "radial-gradient(ellipse at center, rgba(251, 146, 60, 0.22) 0%, rgba(251, 146, 60, 0.1) 36%, rgba(251, 191, 36, 0.06) 56%, transparent 76%)",
            filter: "blur(36px)",
          }}
          animate={prefersReducedMotion
            ? { opacity: 0.58, scale: 1.02 }
            : {
              opacity: [0.42, 0.62, 0.5, 0.58, 0.42],
              scale: [0.98, 1.04, 1, 1.03, 0.98],
              x: [-10, 8, -4, 10, -10],
              y: [4, -6, 0, -4, 4],
            }}
          transition={{
            duration: 8,
            repeat: Infinity,
            ease: "easeInOut",
          }}
        />

        <motion.div
          className="pointer-events-none absolute"
          style={{
            ...sigilFrame,
            width: "82%",
            height: "82%",
            borderRadius: "50%",
            background:
              "conic-gradient(from 16deg, transparent 0deg, transparent 22deg, rgba(255, 224, 163, 0.08) 34deg, rgba(255, 224, 163, 0.34) 58deg, rgba(251, 146, 60, 0.18) 82deg, transparent 104deg, transparent 218deg, rgba(255, 224, 163, 0.06) 232deg, rgba(255, 224, 163, 0.24) 258deg, rgba(251, 191, 36, 0.12) 282deg, transparent 306deg, transparent 360deg)",
            WebkitMaskImage:
              "radial-gradient(circle, transparent 58%, black 63%, black 69%, transparent 76%)",
            maskImage:
              "radial-gradient(circle, transparent 58%, black 63%, black 69%, transparent 76%)",
            filter: "blur(7px)",
            mixBlendMode: "screen",
          }}
          animate={prefersReducedMotion
            ? { opacity: 0.34, scale: 1 }
            : {
              rotate: 360,
              opacity: [0.24, 0.42, 0.3, 0.38, 0.24],
              scale: [0.985, 1.01, 0.995, 1.008, 0.985],
            }}
          transition={prefersReducedMotion
            ? undefined
            : {
              rotate: { duration: 26, repeat: Infinity, ease: "linear" },
              opacity: { duration: 7, repeat: Infinity, ease: "easeInOut" },
              scale: { duration: 8, repeat: Infinity, ease: "easeInOut" },
            }}
        />

        <motion.div
          className="pointer-events-none absolute"
          style={{
            ...sigilFrame,
            width: "72%",
            height: "72%",
            borderRadius: "50%",
            background:
              "conic-gradient(from 218deg, transparent 0deg, transparent 36deg, rgba(255, 244, 214, 0.08) 54deg, rgba(255, 244, 214, 0.22) 82deg, transparent 106deg, transparent 212deg, rgba(251, 146, 60, 0.08) 236deg, rgba(255, 230, 174, 0.2) 268deg, transparent 294deg, transparent 360deg)",
            WebkitMaskImage:
              "radial-gradient(circle, transparent 63%, black 68%, black 72%, transparent 78%)",
            maskImage:
              "radial-gradient(circle, transparent 63%, black 68%, black 72%, transparent 78%)",
            filter: "blur(5px)",
            mixBlendMode: "screen",
          }}
          animate={prefersReducedMotion
            ? { opacity: 0.22 }
            : {
              rotate: -360,
              opacity: [0.16, 0.28, 0.18, 0.26, 0.16],
            }}
          transition={prefersReducedMotion
            ? undefined
            : {
              rotate: { duration: 18, repeat: Infinity, ease: "linear" },
              opacity: { duration: 5.5, repeat: Infinity, ease: "easeInOut" },
            }}
        />

        <motion.div
          className="pointer-events-none absolute overflow-hidden"
          style={{
            ...sigilFrame,
            width: "72%",
            height: "72%",
            borderRadius: "50%",
            filter: "blur(10px)",
          }}
          animate={prefersReducedMotion
            ? { opacity: 0.76, scale: 1.03 }
            : {
              opacity: [0.64, 0.88, 0.72, 0.84, 0.64],
              scale: [1, 1.04, 1.01, 1.03, 1],
            }}
          transition={{
            duration: 6,
            repeat: Infinity,
            ease: "easeInOut",
          }}
        >
          <div
            className="absolute inset-0"
            style={{
              background:
                "radial-gradient(ellipse at center, rgba(251, 191, 36, 0.34) 0%, rgba(251, 146, 60, 0.18) 42%, rgba(251, 146, 60, 0.06) 68%, transparent 84%)",
            }}
          />
          <motion.div
            className="absolute inset-y-[-8%] w-[82%] rounded-full"
            style={{
              left: "9%",
              background:
                "radial-gradient(ellipse at center, rgba(5, 8, 20, 0.86) 0%, rgba(5, 8, 20, 0.76) 56%, rgba(5, 8, 20, 0.34) 72%, transparent 88%)",
            }}
            animate={prefersReducedMotion
              ? { x: "18%" }
              : { x: ["34%", "10%", "-10%", "-34%", "-10%", "10%", "34%"] }}
            transition={{
              duration: 9,
              repeat: Infinity,
              ease: "easeInOut",
            }}
          />
        </motion.div>

        <motion.img
          src="/logo.webp"
          alt="phase.rs"
          className="relative block w-full"
          style={{
            filter: "drop-shadow(0 0 25px rgba(251, 146, 60, 0.5)) drop-shadow(0 0 50px rgba(251, 146, 60, 0.2))",
          }}
          initial={{ opacity: 0, scale: 0.7, y: 20 }}
          animate={{ opacity: 1, scale: 1, y: 0 }}
          transition={{ duration: 0.8, type: "spring", stiffness: 200, damping: 15 }}
        />
      </div>
    </div>
  );
}
