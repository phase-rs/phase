import { useTranslation } from "react-i18next";

import type { ComboDeclaration, ComboWindow } from "../../types/bracket";

interface Props {
  value: ComboDeclaration;
  onChange: (next: ComboDeclaration) => void;
}

const PRIMARY_BUTTON_CLASS =
  "min-h-[44px] rounded-full border px-2.5 py-1 text-xs font-medium sm:min-h-0";

function buttonClass(active: boolean): string {
  return active
    ? `${PRIMARY_BUTTON_CLASS} border-indigo-300/60 bg-indigo-500/30 text-indigo-100`
    : `${PRIMARY_BUTTON_CLASS} border-white/10 bg-black/20 text-slate-400 hover:bg-white/6`;
}

export function ComboDeclarationPicker({ value, onChange }: Props) {
  const { t } = useTranslation("deck-builder");
  const currentWindow = value.kind === "intended" ? value.window ?? null : null;
  const selectWindow = (window: ComboWindow | null) =>
    onChange({ kind: "intended", window });

  return (
    <div className="space-y-1.5">
      <div className="text-xs text-slate-300">{t("comboBarometer.question")}</div>
      <div
        className="flex flex-wrap items-center gap-1.5"
        role="group"
        aria-label={t("comboBarometer.ariaLabel")}
      >
        <button
          type="button"
          aria-pressed={value.kind === "undeclared"}
          onClick={() => onChange({ kind: "undeclared" })}
          className={buttonClass(value.kind === "undeclared")}
        >
          {t("comboBarometer.undeclared")}
        </button>
        <button
          type="button"
          aria-pressed={value.kind === "none_intended"}
          onClick={() => onChange({ kind: "none_intended" })}
          className={buttonClass(value.kind === "none_intended")}
        >
          {t("comboBarometer.noneIntended")}
        </button>
        <button
          type="button"
          aria-pressed={value.kind === "intended"}
          onClick={() => onChange({ kind: "intended", window: currentWindow })}
          className={buttonClass(value.kind === "intended")}
        >
          {t("comboBarometer.intended")}
        </button>
      </div>

      {value.kind === "intended" && (
        <div
          className="flex flex-wrap items-center gap-1.5"
          role="group"
          aria-label={t("comboBarometer.windowAriaLabel")}
        >
          <button
            type="button"
            aria-pressed={value.window == null}
            onClick={() => selectWindow(null)}
            className={buttonClass(value.window == null)}
          >
            {t("comboBarometer.windowUnstated")}
          </button>
          <button
            type="button"
            aria-pressed={value.window === "early_game"}
            onClick={() => selectWindow("early_game")}
            className={buttonClass(value.window === "early_game")}
          >
            {t("comboBarometer.windowEarly")}
          </button>
          <button
            type="button"
            aria-pressed={value.window === "late_game"}
            onClick={() => selectWindow("late_game")}
            className={buttonClass(value.window === "late_game")}
          >
            {t("comboBarometer.windowLate")}
          </button>
        </div>
      )}
    </div>
  );
}
