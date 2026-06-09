import { useTranslation } from "react-i18next";

import { AI_DIFFICULTIES, type AIDifficulty } from "../../constants/ai";
import { SelectField } from "../ui/SelectField";

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
  const id = `ai-difficulty-${compact ? "compact" : "full"}`;

  return (
    <div className={className}>
      <label className="sr-only" htmlFor={id}>
        {t("aiDifficulty.label")}
      </label>
      <SelectField
        id={id}
        aria-label={t("aiDifficulty.ariaLabel", {
          difficulty: t(`aiDifficulty.levels.${difficulty}`),
        })}
        value={difficulty}
        onClick={(event) => event.stopPropagation()}
        onChange={(event) => onChange(event.target.value as AIDifficulty)}
        className={[
          "h-full min-h-11 bg-white/[0.03] px-3 text-sm font-medium text-white/88 transition-colors",
          "hover:bg-white/[0.08] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/30",
          compact ? "min-w-[6.25rem]" : "min-w-[7.75rem]",
        ].join(" ")}
      >
        {AI_DIFFICULTIES.map((item) => (
          <option key={item.id} value={item.id} className="bg-[#0a0f1b] text-slate-100">
            {t(`aiDifficulty.levels.${item.id}`)}
          </option>
        ))}
      </SelectField>
    </div>
  );
}
