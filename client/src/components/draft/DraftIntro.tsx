import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles";

// ── Types ───────────────────────────────────────────────────────────────

type DraftMode = "quick" | "pod";

interface DraftIntroProps {
  mode: DraftMode;
  podSize?: number;
  onContinue: () => void;
}

// ── Steps ───────────────────────────────────────────────────────────────

interface Step {
  icon: string;
  text: string;
}

// ── Component ───────────────────────────────────────────────────────────

export function DraftIntro({ mode, podSize = 8, onContinue }: DraftIntroProps) {
  const { t } = useTranslation("draft");

  const quickSteps: Step[] = [
    { icon: "1", text: t("intro.quick.step1") },
    { icon: "2", text: t("intro.quick.step2") },
    { icon: "3", text: t("intro.quick.step3") },
    { icon: "4", text: t("intro.quick.step4") },
  ];
  const podStepList: Step[] = [
    { icon: "1", text: t("intro.pod.step1", { count: podSize }) },
    { icon: "2", text: t("intro.pod.step2") },
    { icon: "3", text: t("intro.pod.step3") },
    { icon: "4", text: t("intro.pod.step4") },
  ];

  const steps = mode === "quick" ? quickSteps : podStepList;
  const title = mode === "quick" ? t("intro.quickTitle") : t("intro.podTitle");

  return (
    <div className="mx-auto flex w-full max-w-lg flex-col items-center gap-8 py-12">
      <div className="flex flex-col items-center gap-2">
        <h1 className="menu-display text-3xl text-white">{title}</h1>
        <p className="text-sm text-white/50">{t("intro.subtitle")}</p>
      </div>

      <div className="flex w-full flex-col gap-3">
        {steps.map((step) => (
          <div
            key={step.icon}
            className="flex items-start gap-4 rounded-[16px] border border-white/10 bg-black/18 px-5 py-4 backdrop-blur-md"
          >
            <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full border border-white/15 bg-white/8 text-sm font-semibold text-white/70">
              {step.icon}
            </span>
            <span className="pt-0.5 text-sm leading-relaxed text-white/80">
              {step.text}
            </span>
          </div>
        ))}
      </div>

      <button
        onClick={onContinue}
        className={menuButtonClass({ tone: "emerald", size: "lg" })}
      >
        {t("intro.startDrafting")}
      </button>
    </div>
  );
}
