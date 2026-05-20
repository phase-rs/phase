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

// Per-keyword display copy. Driven by the engine-provided `keyword` axis;
// the modal itself is a pure display layer per CLAUDE.md frontend rules.
const KEYWORD_COPY: Record<
  Keyword,
  {
    eyebrow: string;
    normalLabel: string;
    altLabel: string;
    altSuffix?: string;
    /** True when the card's printed Oracle text is helpful context. Warp's
     * rider lives on the keyword itself; the other three meaningfully change
     * the spell's behavior. */
    showOracleText: boolean;
    subtitle: (cardName: string) => string;
  }
> = {
  Warp: {
    eyebrow: "Warp",
    normalLabel: "Cast Normally",
    altLabel: "Cast with Warp",
    altSuffix: "(exiles at end step)",
    showOracleText: false,
    subtitle: (cardName) => `Cast ${cardName} normally or use its Warp cost.`,
  },
  Evoke: {
    eyebrow: "Evoke",
    normalLabel: "Cast Normally",
    altLabel: "Cast with Evoke",
    showOracleText: true,
    subtitle: (cardName) =>
      `Cast ${cardName} normally or cast it for its Evoke cost.`,
  },
  Overload: {
    eyebrow: "Overload",
    normalLabel: "Cast Normally",
    altLabel: "Cast with Overload",
    showOracleText: true,
    subtitle: (cardName) =>
      `Cast ${cardName} normally targeting a single permanent, or pay its Overload cost to affect all valid targets.`,
  },
  Bestow: {
    eyebrow: "Bestow",
    normalLabel: "Cast as Creature",
    altLabel: "Cast with Bestow",
    showOracleText: true,
    subtitle: (cardName) =>
      `Cast ${cardName} normally as a creature, or pay its Bestow cost to cast it as an Aura.`,
  },
};

/**
 * CR 702.74a + CR 601.2h: Compact display copy for the non-mana portion of
 * an alternative cost (e.g., Solitude's Evoke "Exile a white card from your
 * hand."). Mirrors the engine's typed `AbilityCost` taxonomy 1:1 by the
 * discriminant `type` field — the FE does not interpret game state, it just
 * renders the engine-provided variant.
 */
function describeAdditionalCost(cost: SerializedAbilityCost): string {
  switch (cost.type) {
    case "Exile":
      return "+ Exile a card";
    case "Sacrifice":
      return "+ Sacrifice a permanent";
    case "PayLife":
      return "+ Pay life";
    case "Discard":
      return "+ Discard a card";
    case "TapCreatures":
      return "+ Tap creatures";
    default:
      return `+ ${cost.type}`;
  }
}

/**
 * CR 118.9: Unified prompt for keyword-granted alternative casting costs
 * (Warp custom, Evoke per CR 702.74a, Overload per CR 702.96a, Bestow per
 * CR 702.103a). All four share the same player-decision shape; the engine's
 * `keyword` axis selects display copy only.
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
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);

  if (!obj) return null;

  const cardName = obj.name;
  const copy = KEYWORD_COPY[keyword];

  return (
    <DialogShell
      eyebrow={copy.eyebrow}
      title="Choose Casting Cost"
      subtitle={copy.subtitle(cardName)}
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
              {describeAdditionalCost(alternativeAdditionalCost)}
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
