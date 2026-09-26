import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type { CastingVariant, CastingVariantFace, GameAction, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";
import { describeAdditionalCost } from "./describeAdditionalCost.ts";
import { DialogShell } from "./DialogShell.tsx";

type CastingVariantChoice = Extract<
  WaitingFor,
  { type: "CastingVariantChoice" }
>;

// Maps each engine `CastingVariant` discriminant to its i18n key leaf. Variants
// not listed fall back to the parameterized `variantFallback`.
const VARIANT_KEYS: Partial<Record<CastingVariant["type"], string>> = {
  Normal: "variantNormal",
  Adventure: "variantAdventure",
  Omen: "variantOmen",
  Warp: "variantWarp",
  Escape: "variantEscape",
  Retrace: "variantRetrace",
  Harmonize: "variantHarmonize",
  Mayhem: "variantMayhem",
  Flashback: "variantFlashback",
  Aftermath: "variantAftermath",
  GraveyardPermission: "variantGraveyardPermission",
  HandPermission: "variantHandPermission",
  Miracle: "variantMiracle",
  Madness: "variantMadness",
  Evoke: "variantEvoke",
  Suspend: "variantSuspend",
  Plot: "variantPlot",
  Foretell: "variantForetell",
  Overload: "variantOverload",
  Bestow: "variantBestow",
  Blitz: "variantBlitz",
  Mutate: "variantMutate",
  Awaken: "variantAwaken",
  Cleave: "variantCleave",
  Impending: "variantImpending",
  MoreThanMeetsTheEye: "variantMoreThanMeetsTheEye",
  Prototype: "variantPrototype",
  FaceDown: "variantFaceDown",
  Freerunning: "variantFreerunning",
  Fuse: "variantFuse",
};

export function CastingVariantModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "CastingVariantChoice") return null;
  if (!canActForWaitingState) return null;

  return (
    <CastingVariantContent
      data={waitingFor.data}
      dispatch={dispatch}
    />
  );
}

function CastingVariantContent({
  data,
  dispatch,
}: {
  data: CastingVariantChoice["data"];
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[data.object_id]);
  const legalActions = useGameStore((s) => s.legalActions);
  if (!obj) return null;

  return (
    <DialogShell
      eyebrow={t("castingVariant.eyebrow")}
      title={t("castingVariant.title")}
      subtitle={obj.name}
      previewObjectId={data.object_id}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        {data.options.map((option, index) => {
          const action = legalActions.find(
            (candidate) =>
              candidate.type === "ChooseCastingVariant" && candidate.data.index === index,
          );
          if (!action) return null;
          return <button
            key={`${option.variant.type}-${index}`}
            onClick={() => dispatch(action)}
            className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
          >
            <span className="font-semibold text-white">
              {labelForVariant(option.variant, t)} {labelForFace(option.face, obj)}
            </span>
            <span className="ml-2">
              <ManaCostSymbols cost={option.mana_cost} />
            </span>
            {option.additional_cost && (
              <span className="ml-2 text-xs text-slate-300">
                {describeAdditionalCost(option.additional_cost, t)}
              </span>
            )}
          </button>;
        })}
      </div>
    </DialogShell>
  );
}

function labelForFace(face: CastingVariantFace, obj: { name: string; back_face?: { name: string } | null }): string {
  if (face === "Right") return obj.back_face?.name ?? "";
  return obj.name;
}

function labelForVariant(variant: CastingVariant, t: TFunction<"game">): string {
  const key = VARIANT_KEYS[variant.type];
  return key
    ? t(`castingVariant.${key}`)
    : t("castingVariant.variantFallback", { type: variant.type });
}
