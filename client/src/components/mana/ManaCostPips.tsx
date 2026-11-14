import type { ManaCost } from "../../adapter/types.ts";
import { manaCostToShards } from "../../viewmodel/costLabel.ts";
import { ManaSymbol } from "./ManaSymbol.tsx";

type PipSize = "2xs" | "xs" | "sm" | "md" | "fluid";

const PIP_SIZES: Record<PipSize, { container: string; gap: string; backdrop: string }> = {
  "2xs": { container: "w-[10px] h-[10px] p-[0px]", gap: "gap-[0.5px]", backdrop: "-inset-x-[1px] top-[2px] -bottom-[3px]" },
  xs: { container: "w-[12px] h-[12px] p-[0px]", gap: "gap-[0.5px]", backdrop: "-inset-x-[1px] top-[2px] -bottom-[4px]" },
  sm: { container: "w-[18px] h-[18px] p-[0px]", gap: "gap-[1px]", backdrop: "-inset-x-[2px] top-[4px] -bottom-[8px]" },
  md: { container: "w-[22px] h-[22px] p-[2px]", gap: "gap-[1px]", backdrop: "-inset-x-[3px] -top-[2px] -bottom-[4px]" },
  // Card-relative sizing in container-query inline units (1cqi = 1% of the
  // nearest `@container` ancestor's width). Consumers that pass `size="fluid"`
  // MUST wrap the pips in an element with `container-type: inline-size` sized to
  // the card (e.g. an `absolute inset-0 @container` overlay); the badge then
  // anchors itself over the printed cost via FLUID_ANCHOR.
  //
  // The badge stands in for the card's PRINTED mana cost (it shows the engine's
  // effective cost), so its geometry is calibrated against that cost rather than
  // against any fixed px size. Measured on M15-frame art, a printed symbol is
  // ~5.2% of the card's width, so the 0.4cqi padding sizes the symbol to exactly
  // that and the 6cqi disk reads as a thin ring around it. The printed symbols
  // on the same card are legible at this size, which is what makes it enough.
  //
  // Keep the three values solving 5*container + 4*gap = 32cqi: that is the
  // widest cost the frame carries (five symbols), and 32cqi is what clears the
  // card name instead of running through it.
  fluid: { container: "w-[6cqi] h-[6cqi] p-[0.4cqi]", gap: "gap-[0.5cqi]", backdrop: "-inset-x-[0.5cqi] -top-[0.5cqi] -bottom-[1cqi]" },
};

// CR 709.3 + CR 712.11b: a card whose player chooses a spell face at cast time
// carries TWO payable costs, rendered `front // back`. The pair must not grow
// past the 32cqi a single five-symbol cost spans, or it runs through the card
// name (or, on a rotated split card, the second half's own name). The class —
// every non-fuse spell//spell split card and every spell//spell MDFC — is 127
// pairs in card-data.json (125 cards plus the Arena-rebalanced A-Rowan //
// A-Will and A-Alrund // A-Hakka, counted beside their originals) whose two
// costs together carry 2–8 symbols (2:2, 3:14, 4:43, 5:45, 6:20, 7:2, 8:1;
// the 8 is Esika, God of the Tree // The Prismatic Bridge, {1}{G}{G} //
// {W}{U}{B}{R}{G}). The row holds n pips AND the separator, so it has n gaps;
// the separator is a declared 2.4cqi wide (two slashes at a 3cqi font,
// ~0.3em each, centred). Budget: n * pip + n * 0.5cqi + 2.4cqi. Up to four
// symbols the single badge's own 6cqi pip fits (4 * 6.5 + 2.4 = 28.4); from
// five on the pip shrinks by count: 5 * 5.3 + 2.5 + 2.4 = 31.4, 6 * 4.3 + 3 +
// 2.4 = 31.2, 7 * 3.6 + 3.5 + 2.4 = 31.1, 8 * 3.1 + 4 + 2.4 = 31.2, all under
// the 32cqi. The backdrop insets scale with the pip (x and top ≈ pip / 12,
// bottom ≈ pip / 6, the single badge's own ratios, rounded to 0.05cqi). The
// anchor is unchanged: it fixes the badge's top-right EDGE to the printed
// cost's, which a smaller pip does not move. Only the fluid size renders a pair — the fixed-px sizes
// have no width budget, so `backFace` is ignored there.
const FLUID_PAIR_SIZES: Array<{ maxSymbols: number; geometry: (typeof PIP_SIZES)["fluid"] }> = [
  { maxSymbols: 4, geometry: PIP_SIZES.fluid },
  { maxSymbols: 5, geometry: { container: "w-[5.3cqi] h-[5.3cqi] p-[0.35cqi]", gap: "gap-[0.5cqi]", backdrop: "-inset-x-[0.45cqi] -top-[0.45cqi] -bottom-[0.9cqi]" } },
  { maxSymbols: 6, geometry: { container: "w-[4.3cqi] h-[4.3cqi] p-[0.3cqi]", gap: "gap-[0.5cqi]", backdrop: "-inset-x-[0.35cqi] -top-[0.35cqi] -bottom-[0.7cqi]" } },
  { maxSymbols: 7, geometry: { container: "w-[3.6cqi] h-[3.6cqi] p-[0.25cqi]", gap: "gap-[0.5cqi]", backdrop: "-inset-x-[0.3cqi] -top-[0.3cqi] -bottom-[0.6cqi]" } },
  { maxSymbols: 8, geometry: { container: "w-[3.1cqi] h-[3.1cqi] p-[0.2cqi]", gap: "gap-[0.5cqi]", backdrop: "-inset-x-[0.25cqi] -top-[0.25cqi] -bottom-[0.5cqi]" } },
];
const FLUID_PAIR_SEPARATOR = "w-[2.4cqi] text-center text-[3cqi]";
/** The fluid geometry for a pair of `symbols` symbols in total. Clamps to the
 *  smallest tier: a pair past the corpus maximum of 8 (a live front that grew
 *  a symbol) overflows the 32cqi budget rather than shrinking further. */
function fluidPairGeometry(symbols: number): (typeof PIP_SIZES)["fluid"] {
  return (FLUID_PAIR_SIZES.find((tier) => symbols <= tier.maxSymbols) ?? FLUID_PAIR_SIZES[FLUID_PAIR_SIZES.length - 1]).geometry;
}

// Where the printed cost sits on an M15 frame: right edge ~7% in from the card's
// right edge, top edge ~5.4% down from its top. Owning this here keeps the
// anchor in lockstep with the pip diameter above — the two only look right
// together, and every card overlay wants the same placement.
const FLUID_ANCHOR = "absolute right-[6.5%] top-[5%]";

// A cost the engine reduced all the way to {0} is a badge of exactly ONE
// symbol whose width never varies with the card. It also carries nothing the
// player doesn't already know from the offer itself, while the mana VALUE it
// would cover is what alternative costs are measured in — Amped Raptor pays an
// amount of {E} equal to it, Nashi pays that much life. So this one badge parks
// in the frame's right margin instead: beside the printed cost, not on it.
const FLUID_ANCHOR_FREE = "absolute right-[0.5%] top-[5%]";

interface FaceCost {
  cost: ManaCost;
  isReduced?: boolean;
}

interface ManaCostPipsProps extends FaceCost {
  /** CR 709.3 + CR 712.11b: the OTHER castable spell face's live cost, as the
   *  engine published it (`DerivedViews.back_face_spell_costs`) — a Room or
   *  other split card, a spell//spell MDFC. Rendered after a `//`, each face
   *  ringed by its own reduction. Absent for a single-faced card. */
  backFace?: FaceCost;
  size?: PipSize;
  className?: string;
}

/** The symbols one face's badge shows: its shards, or a lone {0} when the
 *  engine reduced a real printed cost all the way (never for a naturally
 *  free card — that face has no badge). */
function faceShards({ cost, isReduced }: FaceCost): string[] {
  const shards = manaCostToShards(cost);
  if (shards.length === 0 && isReduced) shards.push("0");
  return shards;
}

/** Mana cost pips with dark circular backgrounds, MTGA-style. */
export function ManaCostPips({ cost, isReduced, backFace, size = "md", className = "" }: ManaCostPipsProps) {
  const front = faceShards({ cost, isReduced });
  // The back face never stands alone: without a front badge there is nothing
  // to say which face a lone number belongs to.
  if (front.length === 0) return null;
  const back = size === "fluid" && backFace ? faceShards(backFace) : [];
  const isPair = back.length > 0;
  // A lone {0} carries nothing the offer doesn't say; only that single badge
  // leaves the printed cost (FLUID_ANCHOR_FREE). A pair always has more to say.
  const isFreeBadge = !isPair && manaCostToShards(cost).length === 0 && isReduced;

  const s = isPair ? fluidPairGeometry(front.length + back.length) : PIP_SIZES[size];
  const anchor = size !== "fluid" ? "" : isFreeBadge ? FLUID_ANCHOR_FREE : FLUID_ANCHOR;
  const pips = (shards: string[], reduced: boolean | undefined, keyPrefix: string) =>
    shards.map((shard, i) => (
      <div
        key={`${keyPrefix}${i}`}
        className={`relative flex items-center justify-center ${s.container} rounded-full bg-gray-900/80 shadow-[0_1px_3px_rgba(0,0,0,0.6)] ${
          reduced ? "ring-[1.5px] ring-green-400" : ""
        }`}
      >
        <ManaSymbol shard={shard} size="xs" className="w-full h-full" />
      </div>
    ));

  return (
    <div className={`pointer-events-none ${anchor} ${className}`}>
      <div className={`relative flex ${s.gap}`}>
        <div
          data-mana-cost-backdrop
          className={`absolute ${s.backdrop} rounded-full bg-gray-900/70`}
        />
        {pips(front, isReduced, "f")}
        {isPair && (
          <span
            data-mana-cost-face-separator
            className={`relative self-center font-semibold leading-none text-white/90 ${FLUID_PAIR_SEPARATOR}`}
          >
            //
          </span>
        )}
        {pips(back, backFace?.isReduced, "b")}
      </div>
    </div>
  );
}
