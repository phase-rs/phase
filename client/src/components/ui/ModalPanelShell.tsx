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
        className="fixed inset-0 z-50 flex items-stretch px-0 py-0 lg:items-center lg:justify-center lg:px-4 lg:py-6"
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
          className={`card-scale-reset relative z-10 flex h-full w-full flex-col overflow-hidden border-white/10 bg-[#0b1020]/96 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md lg:h-auto lg:max-h-[calc(100vh_-_3rem_-_env(safe-area-inset-top)_-_env(safe-area-inset-bottom))] lg:rounded-[24px] lg:border ${maxWidthClassName}`}
          initial={{ scale: 0.97, opacity: 0, y: 10 }}
          animate={{ scale: 1, opacity: 1, y: 0 }}
          exit={{ scale: 0.97, opacity: 0, y: 10 }}
          transition={{ duration: 0.2, ease: "easeOut" }}
        >
          <div className="flex items-start justify-between gap-2 border-b border-white/10 px-2 py-1.5 lg:gap-4 lg:px-6 lg:py-5">
            <div className="min-w-0">
              {eyebrow && (
                <div className="text-[0.55rem] uppercase tracking-[0.22em] text-slate-500 lg:text-[0.68rem]">
                  {eyebrow}
                </div>
              )}
              <h2 className="mt-0.5 text-sm font-semibold text-white lg:mt-1 lg:text-xl">{title}</h2>
              {subtitle && (
                <p className="mt-1 text-xs text-slate-400 lg:text-sm">{subtitle}</p>
              )}
            </div>
            <button
              onClick={onClose}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-[10px] border border-white/10 bg-black/18 text-slate-400 transition hover:bg-white/6 hover:text-white lg:h-11 lg:w-11 lg:rounded-[16px]"
              aria-label={`Close ${title}`}
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-3.5 w-3.5 lg:h-5 lg:w-5">
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
