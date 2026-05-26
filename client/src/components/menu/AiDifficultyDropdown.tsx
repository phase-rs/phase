import { useTranslation } from "react-i18next";

import { AI_DIFFICULTIES, type AIDifficulty } from "../../constants/ai";

interface AiDifficultyDropdownProps {
  difficulty: AIDifficulty;
  onChange: (difficulty: AIDifficulty) => void;
  align?: "left" | "right";
  className?: string;
  panelClassName?: string;
  compact?: boolean;
}

export function AiDifficultyDropdown({
  difficulty,
  onChange,
  className,
  compact = false,
}: AiDifficultyDropdownProps) {
  const { t } = useTranslation("menu");
  return (
    <div className={`relative ${className ?? ""}`}>
      <label className="sr-only" htmlFor={`ai-difficulty-${compact ? "compact" : "full"}`}>
        {t("aiDifficulty.label")}
      </label>
      <select
        id={`ai-difficulty-${compact ? "compact" : "full"}`}
        aria-label={t("aiDifficulty.ariaLabel", {
          difficulty: t(`aiDifficulty.levels.${difficulty}`),
        })}
        value={difficulty}
        onClick={(event) => event.stopPropagation()}
        onChange={(event) => onChange(event.target.value as AIDifficulty)}
        className={[
          "h-full min-h-11 appearance-none bg-white/[0.03] px-3 pr-9 text-sm font-medium text-white/88 transition-colors",
          "hover:bg-white/[0.08] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/30",
          compact ? "min-w-[6.25rem]" : "min-w-[7.75rem]",
        ].join(" ")}
      >
        {AI_DIFFICULTIES.map((item) => (
          <option key={item.id} value={item.id} className="bg-[#0a0f1b] text-slate-100">
            {t(`aiDifficulty.levels.${item.id}`)}
          </option>
        ))}
      </select>

      <div className="pointer-events-none absolute inset-y-0 right-0 flex items-center pr-3 text-white/70">
        <ChevronDownIcon />
      </div>
    </div>
  );
}

function ChevronDownIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 20 20" className="h-4 w-4 fill-current">
      <path d="M5.47 7.97a.75.75 0 0 1 1.06 0L10 11.44l3.47-3.47a.75.75 0 1 1 1.06 1.06l-4 4a.75.75 0 0 1-1.06 0l-4-4a.75.75 0 0 1 0-1.06Z" />
    </svg>
  );
}
