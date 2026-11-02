import { useTranslation } from "react-i18next";

import { DIFFICULTY_NAMES, useDraftStore } from "../../stores/draftStore";
import { usePreferencesStore } from "../../stores/preferencesStore";

// ── Component ───────────────────────────────────────────────────────────

/**
 * Segmented control for the solo-draft bot difficulty. Reads/writes the shared
 * draft store's `difficulty` index (0–4), which `startDraft`/`startCubeDraft`
 * forward at setup and `launchMatch` forwards to the game URL so the AI loop
 * controller runs the chosen profile. Shared by the set-draft (`SetSelector`)
 * and cube-draft (`DraftPage`) setup flows so both drive the same value.
 *
 * Difficulty is a single-axis scale, so a segmented control reads more clearly
 * than per-level colors.
 */
interface Props {
  /** Show the draft-guide pick enhancements checkbox. Off for surfaces whose
   *  bots do not read the preference (the pod host). */
  showPickEnhancements?: boolean;
}

export function BotDifficultySelector({ showPickEnhancements = false }: Props) {
  const { t } = useTranslation("draft");
  const difficulty = useDraftStore((s) => s.difficulty);
  const setDifficulty = useDraftStore((s) => s.setDifficulty);
  const pickEnhancements = usePreferencesStore((s) => s.draftPickEnhancements);
  const setPickEnhancements = usePreferencesStore((s) => s.setDraftPickEnhancements);

  return (
    <div className="flex flex-col gap-2">
      <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
        {t("setSelector.botDifficulty")}
      </h3>
      <div className="flex w-full max-w-md rounded-xl border border-white/10 bg-black/18 p-1 backdrop-blur-md">
        {DIFFICULTY_NAMES.map((id, idx) => {
          const selected = difficulty === idx;
          return (
            <button
              key={id}
              type="button"
              onClick={() => setDifficulty(idx)}
              aria-pressed={selected}
              className={`flex-1 cursor-pointer rounded-lg px-2 py-2 text-xs font-medium transition-colors ${
                selected
                  ? "bg-emerald-400/15 text-emerald-100 shadow-[inset_0_0_0_1px] shadow-emerald-300/25"
                  : "text-white/45 hover:bg-white/[0.05] hover:text-white/70"
              }`}
            >
              {t(`setSelector.difficultyLevels.${id}`)}
            </button>
          );
        })}
      </div>
      {showPickEnhancements && (
        <label className="flex max-w-md cursor-pointer items-start gap-2 text-xs text-white/70">
          <input
            type="checkbox"
            checked={pickEnhancements}
            onChange={(event) => setPickEnhancements(event.target.checked)}
            className="mt-0.5 h-4 w-4 shrink-0 cursor-pointer accent-emerald-400"
          />
          <span className="flex flex-col">
            <span className="font-medium text-white/85">{t("setSelector.pickEnhancements.label")}</span>
            <span className="text-[11px] text-white/45">{t("setSelector.pickEnhancements.hint")}</span>
          </span>
        </label>
      )}
    </div>
  );
}
