import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";

import type {
  GameAction,
  ManaCost,
  SerializedAbilityCost,
  WaitingFor,
} from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { ManaCostSymbols } from "../mana/ManaCostSymbols.tsx";
import { CardTextboxPreview } from "./CardTextboxPreview.tsx";
import { DialogShell } from "./DialogShell.tsx";

type AlternativeCastChoice = Extract<
  WaitingFor,
  { type: "AlternativeCastChoice" }
>;
type Keyword = AlternativeCastChoice["data"]["keyword"]["type"];

interface KeywordCopy {
  eyebrow: string;
  normalLabel: string;
  altLabel: string;
  altSuffix?: string;
  /** True when the card's printed Oracle text is helpful context for the chosen keyword. */
  showOracleText: boolean;
  subtitle: string;
}

function assertNeverKeyword(keyword: never): never {
  throw new Error(`Unhandled alternative cast keyword: ${String(keyword)}`);
}

// Per-keyword display copy. Driven by the engine-provided `keyword` axis;
// the modal itself is a pure display layer per CLAUDE.md frontend rules.
function keywordCopy(
  keyword: Keyword,
  cardName: string,
  t: TFunction<"game">,
): KeywordCopy {
  switch (keyword) {
    case "Warp":
      return {
        eyebrow: t("alternativeCost.warpEyebrow"),
        normalLabel: t("alternativeCost.warpNormalLabel"),
        altLabel: t("alternativeCost.warpAltLabel"),
        altSuffix: t("alternativeCost.warpAltSuffix"),
        showOracleText: false,
        subtitle: t("alternativeCost.warpSubtitle", { name: cardName }),
      };
    case "Evoke":
      return {
        eyebrow: t("alternativeCost.evokeEyebrow"),
        normalLabel: t("alternativeCost.evokeNormalLabel"),
        altLabel: t("alternativeCost.evokeAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.evokeSubtitle", { name: cardName }),
      };
    // CR 702.119a-c: Emerge — sacrifice a creature while casting; the emerge
    // cost is reduced by that creature's mana value (handled engine-side).
    case "Emerge":
      return {
        eyebrow: t("alternativeCost.emergeEyebrow"),
        normalLabel: t("alternativeCost.emergeNormalLabel"),
        altLabel: t("alternativeCost.emergeAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.emergeSubtitle", { name: cardName }),
      };
    // CR 702.109a: Dash — like Warp, the rider (haste + end-step return to hand)
    // lives on the keyword itself and doesn't change the spell's printed text.
    case "Dash":
      return {
        eyebrow: t("alternativeCost.dashEyebrow"),
        normalLabel: t("alternativeCost.dashNormalLabel"),
        altLabel: t("alternativeCost.dashAltLabel"),
        altSuffix: t("alternativeCost.dashAltSuffix"),
        showOracleText: false,
        subtitle: t("alternativeCost.dashSubtitle", { name: cardName }),
      };
    case "Overload":
      return {
        eyebrow: t("alternativeCost.overloadEyebrow"),
        normalLabel: t("alternativeCost.overloadNormalLabel"),
        altLabel: t("alternativeCost.overloadAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.overloadSubtitle", { name: cardName }),
      };
    case "Bestow":
      return {
        eyebrow: t("alternativeCost.bestowEyebrow"),
        normalLabel: t("alternativeCost.bestowNormalLabel"),
        altLabel: t("alternativeCost.bestowAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.bestowSubtitle", { name: cardName }),
      };
    case "Awaken":
      return {
        eyebrow: t("alternativeCost.awakenEyebrow"),
        normalLabel: t("alternativeCost.awakenNormalLabel"),
        altLabel: t("alternativeCost.awakenAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.awakenSubtitle", { name: cardName }),
      };
    case "Cleave":
      return {
        eyebrow: t("alternativeCost.cleaveEyebrow"),
        normalLabel: t("alternativeCost.cleaveNormalLabel"),
        altLabel: t("alternativeCost.cleaveAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.cleaveSubtitle", { name: cardName }),
      };
    case "MoreThanMeetsTheEye":
      return {
        eyebrow: t("alternativeCost.mtmteEyebrow"),
        normalLabel: t("alternativeCost.mtmteNormalLabel"),
        altLabel: t("alternativeCost.mtmteAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.mtmteSubtitle", { name: cardName }),
      };
    // CR 702.140a: Mutate — pay the mutate cost to cast as a mutating creature
    // spell targeting a non-Human creature you own.
    case "Mutate":
      return {
        eyebrow: t("alternativeCost.mutateEyebrow"),
        normalLabel: t("alternativeCost.mutateNormalLabel"),
        altLabel: t("alternativeCost.mutateAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.mutateSubtitle", { name: cardName }),
      };
    // CR 702.152a: Blitz — pay the blitz cost; the creature gains haste and
    // "when it dies, draw a card", but is sacrificed at the next end step.
    case "Blitz":
      return {
        eyebrow: t("alternativeCost.blitzEyebrow"),
        normalLabel: t("alternativeCost.blitzNormalLabel"),
        altLabel: t("alternativeCost.blitzAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.blitzSubtitle", { name: cardName }),
      };
    // CR 702.176a: Impending — pay the impending cost; the permanent enters with
    // time counters and isn't a creature until the last one is removed.
    case "Impending":
      return {
        eyebrow: t("alternativeCost.impendingEyebrow"),
        normalLabel: t("alternativeCost.impendingNormalLabel"),
        altLabel: t("alternativeCost.impendingAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.impendingSubtitle", { name: cardName }),
      };
    // CR 702.160a: Prototype — pay the prototype cost; the spell/permanent uses
    // its secondary power, toughness, and mana cost while it is a creature.
    case "Prototype":
      return {
        eyebrow: t("alternativeCost.prototypeEyebrow"),
        normalLabel: t("alternativeCost.prototypeNormalLabel"),
        altLabel: t("alternativeCost.prototypeAltLabel"),
        showOracleText: true,
        subtitle: t("alternativeCost.prototypeSubtitle", { name: cardName }),
      };
    // CR 702.137a: Spectacle — pay the spectacle cost (legal only if an opponent
    // lost life this turn). A pure cost substitution; the spell resolves normally
    // with no rider, so the printed Oracle text needs no extra context.
    case "Spectacle":
      return {
        eyebrow: t("alternativeCost.spectacleEyebrow"),
        normalLabel: t("alternativeCost.spectacleNormalLabel"),
        altLabel: t("alternativeCost.spectacleAltLabel"),
        showOracleText: false,
        subtitle: t("alternativeCost.spectacleSubtitle", { name: cardName }),
      };
  }
  return assertNeverKeyword(keyword);
}

/**
 * CR 702.74a + CR 601.2h: Compact display copy for the non-mana portion of
 * an alternative cost (e.g., Solitude's Evoke "Exile a white card from your
 * hand."). Mirrors the engine's typed `AbilityCost` taxonomy 1:1 by the
 * discriminant `type` field — the FE does not interpret game state, it just
 * renders the engine-provided variant.
 */
function describeAdditionalCost(
  cost: SerializedAbilityCost,
  t: TFunction<"game">,
): string {
  switch (cost.type) {
    case "Exile":
      return t("alternativeCost.additionalExile");
    case "Sacrifice":
      return t("alternativeCost.additionalSacrifice");
    case "PayLife":
      return t("alternativeCost.additionalPayLife");
    case "Discard":
      return t("alternativeCost.additionalDiscard");
    case "TapCreatures":
      return t("alternativeCost.additionalTapCreatures");
    default:
      return t("alternativeCost.additionalGeneric", { type: cost.type });
  }
}

/**
 * CR 118.9: Unified prompt for keyword-granted alternative casting costs
 * (Warp custom, Evoke per CR 702.74a, Overload per CR 702.96a, Bestow per
 * CR 702.103a, Cleave per CR 702.148a). All share the same player-decision
 * shape; the engine's `keyword` axis selects display copy only.
 */
export function AlternativeCostModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  if (waitingFor?.type !== "AlternativeCastChoice") return null;
  if (!canActForWaitingState) return null;

  const data = waitingFor.data;

  return (
    <AlternativeCostContent
      objectId={data.object_id}
      keyword={data.keyword.type}
      normalCost={data.normal_cost}
      alternativeCost={data.alternative_cost}
      alternativeAdditionalCost={data.alternative_additional_cost}
      dispatch={dispatch}
    />
  );
}

function AlternativeCostContent({
  objectId,
  keyword,
  normalCost,
  alternativeCost,
  alternativeAdditionalCost,
  dispatch,
}: {
  objectId: number;
  keyword: Keyword;
  normalCost: ManaCost;
  alternativeCost: ManaCost | null;
  alternativeAdditionalCost: SerializedAbilityCost | null;
  dispatch: (action: GameAction) => Promise<unknown>;
}) {
  const { t } = useTranslation("game");
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;
  const copy = keywordCopy(keyword, cardName, t);

  return (
    <DialogShell
      eyebrow={copy.eyebrow}
      title={t("alternativeCost.title")}
      subtitle={copy.subtitle}
      previewObjectId={objectId}
    >
      {copy.showOracleText && (
        <div className="px-3 pt-3 lg:px-5 lg:pt-4">
          <CardTextboxPreview cardName={cardName} />
        </div>
      )}
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({
              type: "ChooseAlternativeCast",
              data: { choice: { type: "Normal" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">{copy.normalLabel}</span>
          <span className="ml-2">
            <ManaCostSymbols cost={normalCost} />
          </span>
        </button>
        <button
          onClick={() =>
            dispatch({
              type: "ChooseAlternativeCast",
              data: { choice: { type: "Alternative" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-emerald-400/30"
        >
          <span className="font-semibold text-white">{copy.altLabel}</span>
          {alternativeCost && (
            <span className="ml-2">
              <ManaCostSymbols cost={alternativeCost} />
            </span>
          )}
          {alternativeAdditionalCost && (
            <span className="ml-2 text-xs text-slate-300">
              {describeAdditionalCost(alternativeAdditionalCost, t)}
            </span>
          )}
          {copy.altSuffix && (
            <span className="ml-1 text-xs text-slate-400">
              {copy.altSuffix}
            </span>
          )}
        </button>
      </div>
    </DialogShell>
  );
}
