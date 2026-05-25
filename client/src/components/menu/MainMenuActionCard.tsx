import { motion } from "framer-motion";
import { forwardRef } from "react";
import type { ReactNode } from "react";

type MainMenuAccent = "ember" | "arcane" | "jade" | "stone";

const ACCENTS: Record<MainMenuAccent, string> = {
  ember: "text-amber-200",
  arcane: "text-sky-200",
  jade: "text-emerald-200",
  stone: "text-slate-200",
};

interface MainMenuActionCardProps {
  title: string;
  description: string;
  accent: MainMenuAccent;
  icon: ReactNode;
  onClick: () => void;
  aside?: ReactNode;
  /** When set, the card is non-interactive and shows `statusLabel` + a spinner
   *  in place of the arrow. Used to gate deck-requiring actions until the card
   *  database has warmed. */
  disabled?: boolean;
  /** Short status shown while `disabled` (e.g. "Preparing cards…"). */
  statusLabel?: string;
}

export const MainMenuActionCard = forwardRef<HTMLButtonElement, MainMenuActionCardProps>(function MainMenuActionCard({
  title,
  description,
  accent,
  icon,
  onClick,
  aside,
  disabled = false,
  statusLabel,
}, ref) {
  return (
    <motion.div
      whileHover={disabled ? undefined : { y: -2, scale: 1.003 }}
      className={[
        "group relative flex overflow-hidden rounded-[22px] border border-white/10 bg-[linear-gradient(180deg,rgba(13,18,29,0.88),rgba(9,13,24,0.92))] p-0 text-left text-white transition-transform",
        "before:pointer-events-none before:absolute before:inset-[1px] before:rounded-[20px] before:border before:border-white/6",
        "after:pointer-events-none after:absolute after:inset-0 after:bg-[linear-gradient(90deg,rgba(255,255,255,0.03),transparent_24%,transparent_76%,rgba(255,255,255,0.03))] after:opacity-0 after:transition-opacity after:duration-300",
        disabled ? "opacity-55" : "hover:border-white/18 hover:after:opacity-100",
      ].join(" ")}
    >
      <div className={`pointer-events-none absolute inset-y-5 left-0 w-[3px] rounded-r ${accent === "ember" ? "bg-amber-300/70" : accent === "arcane" ? "bg-sky-300/70" : accent === "jade" ? "bg-emerald-300/70" : "bg-slate-300/60"}`} />

      <div className="relative z-10 flex w-full items-stretch">
        <motion.button
          ref={ref}
          type="button"
          onClick={disabled ? undefined : onClick}
          disabled={disabled}
          aria-disabled={disabled}
          whileTap={disabled ? undefined : { scale: 0.995 }}
          className={[
            "flex min-w-0 flex-1 items-center gap-4 px-5 py-4 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/30",
            disabled ? "cursor-not-allowed" : "",
          ].join(" ")}
        >
          <div className={`rounded-[16px] border border-white/8 bg-black/18 p-3 shadow-inner shadow-white/5 ${ACCENTS[accent]}`}>
            {icon}
          </div>

          <div className="min-w-0 flex-1">
            <h2 className="menu-display text-[1.42rem] leading-[1.04] text-white">
              {title}
            </h2>
            <p className="mt-1.5 max-w-[38rem] text-sm leading-6 text-white/50">
              {description}
            </p>
          </div>

          {disabled ? (
            <div className="flex items-center gap-2 self-center pl-2 text-[11px] font-medium uppercase tracking-[0.14em] text-white/45">
              <Spinner />
              {statusLabel && <span className="hidden sm:inline">{statusLabel}</span>}
            </div>
          ) : (
            !aside && (
              <div className="flex items-center self-stretch pl-2">
                <div className="rounded-full border border-white/10 bg-black/18 px-3 py-2 text-white/42 transition-colors group-hover:text-white/72">
                  <ArrowIcon />
                </div>
              </div>
            )
          )}
        </motion.button>

        {aside && !disabled && (
          <div className="flex shrink-0 items-stretch border-l border-white/10">
            {aside}
          </div>
        )}
      </div>
    </motion.div>
  );
});

function Spinner() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-4 w-4 animate-spin fill-none stroke-current">
      <circle cx="12" cy="12" r="9" strokeWidth="2.5" className="opacity-25" />
      <path d="M21 12a9 9 0 0 0-9-9" strokeWidth="2.5" strokeLinecap="round" />
    </svg>
  );
}

function ArrowIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-5 w-5 fill-current">
      <path d="m13.2 5.4 1.4-1.4 8 8-8 8-1.4-1.4 5.6-5.6H2v-2h16.8l-5.6-5.6Z" />
    </svg>
  );
}
