import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type {
  CastAuthorityChoice,
  CastingVariant,
  CastingVariantFace,
  GameAction,
  WaitingFor,
} from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { formatCounterType } from "../../viewmodel/cardProps.ts";
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
  const objects = useGameStore((s) => s.gameState?.objects);
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
            {option.authority && (
              <span className="mt-1 block text-xs text-slate-300">
                {describeAuthority(
                  option.authority,
                  objects?.[option.authority.announcement.permission.source]?.name,
                  t,
                )}
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

/**
 * CR 601.2a + CR 601.2b: the permission an option is announced under, from the
 * engine's authority. The source name is the viewer's filtered object, so a
 * face-down or hidden source shows only what this viewer may see.
 */
function describeAuthority(
  authority: CastAuthorityChoice,
  sourceName: string | undefined,
  t: TFunction<"game">,
): string {
  const parts: string[] = [];
  if (sourceName) parts.push(t("castingVariant.viaSource", { name: sourceName }));
  if (authority.extra_cost) {
    const cost = describeAdditionalCost(authority.extra_cost.cost, t);
    parts.push(
      authority.extra_cost.mode === "Alternative"
        ? t("castingVariant.extraCostAlternative", { cost })
        : t("castingVariant.extraCostAdditional", { cost }),
    );
  }
  if (authority.enters_with_counter) {
    parts.push(
      t("castingVariant.entersWithCounter", {
        counter: formatCounterType(authority.enters_with_counter),
      }),
    );
  }
  if (authority.frequency === "OncePerTurnPerPermanentType") {
    const slot = authority.announcement.slot_type;
    parts.push(
      slot
        ? t("castingVariant.slotPerType", { type: slot })
        : t("castingVariant.slotChooseType"),
    );
  } else if (authority.frequency === "OncePerTurn") {
    parts.push(t("castingVariant.slotOncePerTurn"));
  }
  return parts.join(" \u00b7 ");
}
