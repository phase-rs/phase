import { motion, AnimatePresence } from "framer-motion";

interface ConcedeDialogProps {
  isOpen: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConcedeDialog({ isOpen, onConfirm, onCancel }: ConcedeDialogProps) {
  return (
    <AnimatePresence>
      {isOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <motion.div
            className="absolute inset-0 bg-black/70"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={onCancel}
          />
          <motion.div
            className="relative z-10 w-80 rounded-xl bg-gray-900 p-6 text-center shadow-2xl ring-1 ring-gray-700"
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.9 }}
            transition={{ type: "spring", stiffness: 300, damping: 25 }}
          >
            <h2 className="mb-2 text-xl font-bold text-white">Concede Game?</h2>
            <p className="mb-6 text-sm text-gray-400">
              Your opponent will be declared the winner.
            </p>
            <div className="flex justify-center gap-3">
              <button
                onClick={onCancel}
                className="rounded-lg bg-gray-700 px-5 py-2 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
              >
                Cancel
              </button>
              <button
                onClick={onConfirm}
                className="rounded-lg bg-red-600 px-5 py-2 text-sm font-semibold text-white transition hover:bg-red-500"
              >
                Concede
              </button>
            </div>
          </motion.div>
        </div>
      )}
    </AnimatePresence>
  );
}
