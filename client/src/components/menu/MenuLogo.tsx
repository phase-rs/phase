import { motion } from "framer-motion";

export function MenuLogo() {
  return (
    <div className="relative flex items-center justify-center -mb-10">
      {/* Outer ambient glow — slow pulse like a flame */}
      <motion.div
        className="pointer-events-none absolute"
        style={{
          width: 600,
          height: 400,
          borderRadius: "50%",
          background:
            "radial-gradient(ellipse, rgba(251, 146, 60, 0.2), rgba(251, 146, 60, 0.08) 40%, transparent 70%)",
          filter: "blur(30px)",
        }}
        animate={{
          opacity: [0.6, 1, 0.7, 0.9, 0.6],
          scale: [1, 1.08, 0.97, 1.05, 1],
        }}
        transition={{
          duration: 4,
          repeat: Infinity,
          ease: "easeInOut",
        }}
      />
      {/* Inner bright glow — slightly faster pulse */}
      <motion.div
        className="pointer-events-none absolute"
        style={{
          width: 300,
          height: 200,
          borderRadius: "50%",
          background:
            "radial-gradient(ellipse, rgba(251, 191, 36, 0.25), rgba(251, 146, 60, 0.1) 50%, transparent 70%)",
          filter: "blur(15px)",
        }}
        animate={{
          opacity: [0.5, 0.9, 0.6, 0.85, 0.5],
          scale: [1, 1.12, 0.95, 1.06, 1],
        }}
        transition={{
          duration: 3,
          repeat: Infinity,
          ease: "easeInOut",
        }}
      />
      <motion.img
        src="/logo.webp"
        alt="phase.rs"
        className="relative w-80 max-w-[70vw]"
        style={{
          filter: "drop-shadow(0 0 25px rgba(251, 146, 60, 0.5)) drop-shadow(0 0 50px rgba(251, 146, 60, 0.2))",
        }}
        initial={{ opacity: 0, scale: 0.7, y: 20 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        transition={{ duration: 0.8, type: "spring", stiffness: 200, damping: 15 }}
      />
    </div>
  );
}
