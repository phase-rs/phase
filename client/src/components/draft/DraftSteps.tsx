// Lightweight progress map for the Quick Draft flow. Display-only: it reads the
// store-owned draft phase and renders where the player is in the journey.

import { useTranslation } from "react-i18next";

import type { DraftPhase } from "../../stores/draftStore";

const STEP_KEYS = ["chooseSet", "draft", "buildDeck", "play"] as const;

const PHASE_STEP: Record<DraftPhase, number> = {
  setup: 0,
  drafting: 1,
  deckbuilding: 2,
  launching: 3,
  playing: 3,
  complete: 4,
};

function CheckIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 16 16" className="h-3 w-3 fill-current">
      <path d="M6.2 11.3 2.9 8l1.1-1.1 2.2 2.2 5-5L12.3 5l-6.1 6.3Z" />
    </svg>
  );
}

export function DraftSteps({ phase }: { phase: DraftPhase }) {
  const { t } = useTranslation("draft");
  const current = PHASE_STEP[phase];

  return (
    <nav aria-label={t("steps.navLabel")} className="mx-auto flex w-full max-w-md items-center">
      {STEP_KEYS.map((key, i) => {
        const label = t(`steps.${key}`);
        const done = i < current;
        const active = i === current;
        return (
          <div key={label} className="flex flex-1 items-center last:flex-none">
            <div className="flex flex-col items-center gap-1.5">
              <span
                className={`flex h-6 w-6 items-center justify-center rounded-full border text-[11px] font-semibold transition-colors ${
                  done
                    ? "border-emerald-400/40 bg-emerald-400/15 text-emerald-200"
                    : active
                      ? "border-emerald-400 bg-emerald-400 text-gray-950"
                      : "border-white/15 bg-white/[0.03] text-white/35"
                }`}
              >
                {done ? <CheckIcon /> : i + 1}
              </span>
              <span
                className={`text-[0.62rem] font-medium uppercase tracking-[0.14em] transition-colors ${
                  active ? "text-emerald-200" : done ? "text-white/45" : "text-white/30"
                }`}
              >
                {label}
              </span>
            </div>
            {i < STEP_KEYS.length - 1 && (
              <span
                aria-hidden="true"
                className={`mx-2 -mt-3 h-px flex-1 transition-colors ${done ? "bg-emerald-400/35" : "bg-white/10"}`}
              />
            )}
          </div>
        );
      })}
    </nav>
  );
}
