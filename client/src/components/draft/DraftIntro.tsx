import { useTranslation } from "react-i18next";

import { menuButtonClass } from "../menu/buttonStyles";

// ── Types ───────────────────────────────────────────────────────────────

type DraftMode = "quick" | "pod" | "commander" | "winston";

interface DraftIntroProps {
  mode: DraftMode;
  podSize: number;
  packCount: number;
  cardsPerPack: number;
  minDeckSize: number;
  /**
   * Engine-provided size of each booster, in pack order. A multi-set draft
   * mixes sizes, so the "packs of N cards" line only holds when they agree.
   */
  packSizes?: number[];
  /**
   * Piles dealt off the shared stack, from the engine's own
   * `PackDistribution::SharedStackPiles { pile_count }`. Read only by the
   * `winston` mode, and never defaulted to a literal 3: the pile count is a
   * procedure axis, so a future shared-stack row with a different one gets
   * correct copy without touching this file.
   */
  pileCount?: number;
  onContinue: () => void;
}

// ── Steps ───────────────────────────────────────────────────────────────

interface Step {
  icon: string;
  text: string;
}

// ── Component ───────────────────────────────────────────────────────────

export function DraftIntro({
  mode,
  podSize,
  packCount,
  cardsPerPack,
  minDeckSize,
  packSizes,
  pileCount,
  onContinue,
}: DraftIntroProps) {
  const { t } = useTranslation("draft");

  const mixedPackSizes = (packSizes?.length ?? 0) > 1
    && new Set(packSizes).size > 1;
  const packs = t("intro.quantity.packsOpened", { count: packCount });
  const cardsPerPackLabel = t("intro.quantity.cardsContained", { count: cardsPerPack });
  const minimumDeckCards = t("intro.quantity.minimumDeckCards", { count: minDeckSize });
  const packSizeLabels = (packSizes ?? [])
    .map((size) => t("intro.quantity.packSizeEntry", { count: size }));
  const packPassing = t("intro.packPassing", { count: packCount });
  const pilesDealt = t("intro.quantity.pilesDealt", { count: pileCount ?? 0 });

  const quickSteps: Step[] = [
    {
      icon: "1",
      text: mixedPackSizes
        ? t("intro.quick.step1Mixed", {
            packs,
            packSizes: packSizeLabels,
          })
        : t("intro.quick.step1", { packs, cardsPerPack: cardsPerPackLabel }),
    },
    { icon: "2", text: t("intro.quick.step2") },
    { icon: "3", text: packPassing },
    { icon: "4", text: t("intro.quick.step4", { minimumDeckCards }) },
  ];
  const podStepList: Step[] = [
    { icon: "1", text: t("intro.pod.step1", { count: podSize }) },
    {
      icon: "2",
      text: mixedPackSizes
        ? t("intro.pod.step2Mixed", {
            packs,
            packSizes: packSizeLabels,
          })
        : t("intro.pod.step2", { packs, cardsPerPack: cardsPerPackLabel }),
    },
    { icon: "3", text: packPassing },
    { icon: "4", text: t("intro.pod.step4", { minimumDeckCards }) },
  ];
  // CR 903.13a/b: Commander Draft is a draft followed by a multiplayer game,
  // and players draft two cards from each booster before passing it. Commander
  // reuses the pod player-count key and the shared pack-passing key rather than
  // duplicating either sentence into seven locales.
  const commanderSteps: Step[] = [
    { icon: "1", text: t("intro.pod.step1", { count: podSize }) },
    {
      icon: "2",
      text: mixedPackSizes
        ? t("intro.commander.step2Mixed", {
            packs,
            packSizes: packSizeLabels,
          })
        : t("intro.commander.step2", { packs, cardsPerPack: cardsPerPackLabel }),
    },
    { icon: "3", text: packPassing },
    { icon: "4", text: t("intro.commander.step4", { minimumDeckCards }) },
  ];

  // A SHARED STACK PASSES NOTHING, so this mode shares only the player-count
  // line with the pod copy. Every other pod sentence — open a pack, pick one,
  // pass the rest, alternate direction each round — describes a procedure this
  // format does not have: the boosters are opened unseen and shuffled together
  // before the first decision, and a turn is a whole-pile take-or-decline.
  //
  // Five steps rather than four because the turn genuinely needs three of them
  // (the piles, the decision, and what declining the last one does), and this
  // screen exists to explain exactly that.
  const winstonSteps: Step[] = [
    { icon: "1", text: t("intro.pod.step1", { count: podSize }) },
    {
      icon: "2",
      text: mixedPackSizes
        ? t("intro.winston.step2Mixed", { packs, packSizes: packSizeLabels })
        : t("intro.winston.step2", { packs }),
    },
    { icon: "3", text: t("intro.winston.step3", { piles: pilesDealt }) },
    { icon: "4", text: t("intro.winston.step4") },
    { icon: "5", text: t("intro.winston.step5", { minimumDeckCards }) },
  ];

  // Total over `DraftMode`: a future mode is a compile error here rather than a
  // silent fall-through to the pod copy.
  const stepsByMode: Record<DraftMode, Step[]> = {
    quick: quickSteps,
    pod: podStepList,
    commander: commanderSteps,
    winston: winstonSteps,
  };
  const titleByMode: Record<DraftMode, string> = {
    quick: t("intro.quickTitle"),
    pod: t("intro.podTitle"),
    commander: t("intro.commanderTitle"),
    winston: t("intro.winstonTitle"),
  };
  const steps = stepsByMode[mode];
  const title = titleByMode[mode];

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
