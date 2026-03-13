import { AnimatePresence, motion } from "framer-motion";

interface ModalPanelShellProps {
  title: string;
  subtitle?: string;
  onClose: () => void;
  children: React.ReactNode;
  eyebrow?: string;
  maxWidthClassName?: string;
  bodyClassName?: string;
}

export function ModalPanelShell({
  title,
  subtitle,
  onClose,
  children,
  eyebrow = "Workspace Tool",
  maxWidthClassName = "max-w-4xl",
  bodyClassName = "",
}: ModalPanelShellProps) {
  return (
    <AnimatePresence>
      <motion.div
        className="fixed inset-0 z-50 flex items-center justify-center px-3 py-3 sm:px-4 sm:py-6"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.18 }}
      >
        <button
          type="button"
          className="absolute inset-0 bg-black/68 backdrop-blur-[2px]"
          onClick={onClose}
          aria-label={`Close ${title}`}
        />

        <motion.div
          className={`relative z-10 flex max-h-[calc(100vh_-_1.5rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))] w-full flex-col overflow-hidden rounded-[20px] border border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md sm:rounded-[24px] ${maxWidthClassName}`}
          initial={{ scale: 0.97, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.97, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className="flex items-start justify-between gap-3 border-b border-white/10 px-4 py-4 sm:gap-4 sm:px-6 sm:py-5">
            <div className="min-w-0">
              {eyebrow && (
                <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
                  {eyebrow}
                </div>
              )}
              <h2 className="mt-1 text-lg font-semibold text-white sm:text-xl">{title}</h2>
              {subtitle && (
                <p className="mt-1 text-xs text-slate-400 sm:text-sm">{subtitle}</p>
              )}
            </div>
            <button
              onClick={onClose}
              className="flex h-11 w-11 shrink-0 items-center justify-center rounded-[16px] border border-white/10 bg-black/18 text-slate-400 transition hover:bg-white/6 hover:text-white"
              aria-label={`Close ${title}`}
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5">
                <path d="M6.28 5.22a.75.75 0 0 0-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 1 0 1.06 1.06L10 11.06l3.72 3.72a.75.75 0 1 0 1.06-1.06L11.06 10l3.72-3.72a.75.75 0 0 0-1.06-1.06L10 8.94 6.28 5.22Z" />
              </svg>
            </button>
          </div>

          <div className={`min-h-0 flex-1 pb-[env(safe-area-inset-bottom)] ${bodyClassName}`}>
            {children}
          </div>
        </motion.div>
      </motion.div>
    </AnimatePresence>
  );
}
