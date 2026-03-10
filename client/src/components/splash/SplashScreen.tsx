import { useEffect } from "react";
import { motion, AnimatePresence } from "framer-motion";

interface SplashScreenProps {
  progress: number;
  onComplete: () => void;
}

export function SplashScreen({ progress, onComplete }: SplashScreenProps) {
  useEffect(() => {
    if (progress >= 100) {
      const timer = setTimeout(onComplete, 600);
      return () => clearTimeout(timer);
    }
  }, [progress, onComplete]);

  const isReady = progress >= 100;

  return (
    <AnimatePresence>
      {!isReady ? (
        <motion.div
          key="splash"
          initial={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.5 }}
          className="fixed inset-0 z-50 flex flex-col items-center justify-center bg-gray-950"
        >
          <motion.img
            src="/logo.webp"
            alt="phase.rs"
            className="mb-4 w-[200px]"
            initial={{ opacity: 0, scale: 0.8 }}
            animate={{ opacity: 1, scale: 1 }}
            transition={{ duration: 0.6, ease: "easeOut" }}
          />

          <p className="mb-6 text-sm tracking-wider text-gray-500">phase.rs</p>

          <div className="h-1 w-[200px] overflow-hidden rounded-full bg-gray-800">
            <motion.div
              className="h-full rounded-full bg-indigo-500"
              initial={{ width: 0 }}
              animate={{ width: `${progress}%` }}
              transition={{ duration: 0.2, ease: "linear" }}
            />
          </div>

          <p className="mt-3 text-xs text-gray-500">
            {isReady ? "Ready" : "Loading..."}
          </p>
        </motion.div>
      ) : null}
    </AnimatePresence>
  );
}
