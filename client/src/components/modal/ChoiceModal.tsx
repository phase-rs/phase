import { AnimatePresence, motion } from "framer-motion";
export interface ChoiceOption {
  id: string;
  label: string;
  description?: string;
}

interface ChoiceModalProps {
  title: string;
  subtitle?: string;
  options: ChoiceOption[];
  onChoose: (id: string) => void;
  onClose?: () => void;
}

export function ChoiceModal({
  title,
  subtitle,
  options,
  onChoose,
  onClose,
}: ChoiceModalProps) {
  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-3 py-4 sm:px-4 sm:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.2 }}
      >
        <div className="absolute inset-0 bg-black/60" onClick={onClose} />

        <motion.div
          className="relative z-10 max-h-[calc(100vh_-_2rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))] w-full max-w-sm overflow-y-auto rounded-[24px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md"
          initial={{ scale: 0.95, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.95, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className="border-b border-white/10 px-4 py-4 sm:px-5 sm:py-5">
            <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
              Game Choice
            </div>
            <h2 className="mt-1 text-lg font-semibold text-white sm:text-xl">{title}</h2>
            {subtitle && (
              <p className="mt-1 text-xs text-slate-400 sm:text-sm">{subtitle}</p>
            )}
          </div>
          <div className="px-4 py-4 sm:px-5 sm:py-5">
            <div className="flex flex-col gap-2">
              {options.map((opt) => (
                <button
                  key={opt.id}
                  onClick={() => onChoose(opt.id)}
                  className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/40"
                >
                  <span className="font-semibold text-white">{opt.label}</span>
                  {opt.description && (
                    <p className="mt-1 text-xs text-slate-400">
                      {opt.description}
                    </p>
                  )}
                </button>
              ))}
            </div>
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
