import { AnimatePresence, motion } from "framer-motion";

export interface ChoiceOption {
  id: string;
  label: string;
  description?: string;
}

interface ChoiceModalProps {
  title: string;
  options: ChoiceOption[];
  onChoose: (id: string) => void;
}

export function ChoiceModal({ title, options, onChoose }: ChoiceModalProps) {
  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-3 py-4 sm:px-4 sm:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        {/* Backdrop */}
        <div className="absolute inset-0 bg-black/60" />

        {/* Modal card */}
        <motion.div
          className="relative z-10 w-full max-w-sm rounded-[20px] bg-gray-900 p-4 shadow-2xl ring-1 ring-gray-700 sm:p-6"
          initial={{ scale: 0.9, opacity: 0 }}
          animate={{ scale: 1, opacity: 1 }}
          exit={{ scale: 0.9, opacity: 0 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <h2 className="mb-4 text-center text-base font-bold text-white sm:text-lg">
            {title}
          </h2>

          <div className="flex flex-col gap-2">
            {options.map((opt) => (
              <button
                key={opt.id}
                onClick={() => onChoose(opt.id)}
                className="min-h-11 rounded-lg bg-gray-800 px-4 py-3 text-left transition hover:bg-gray-700 hover:ring-1 hover:ring-cyan-400/50"
              >
                <span className="font-semibold text-white">{opt.label}</span>
                {opt.description && (
                  <p className="mt-1 text-xs text-gray-400">
                    {opt.description}
                  </p>
                )}
              </button>
            ))}
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
