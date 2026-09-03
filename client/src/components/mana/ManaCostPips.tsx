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

interface ManaCostPipsProps {
  cost: ManaCost;
  isReduced?: boolean;
  size?: PipSize;
  className?: string;
}

/** Mana cost pips with dark circular backgrounds, MTGA-style. */
export function ManaCostPips({ cost, isReduced, size = "md", className = "" }: ManaCostPipsProps) {
  const shards = manaCostToShards(cost);
  // Show {0} only when cost was reduced to zero (not for tokens/naturally free cards)
  const isFreeBadge = shards.length === 0 && isReduced;
  if (isFreeBadge) shards.push("0");
  if (shards.length === 0) return null;

  const s = PIP_SIZES[size];
  const anchor = size !== "fluid" ? "" : isFreeBadge ? FLUID_ANCHOR_FREE : FLUID_ANCHOR;

  return (
    <div className={`pointer-events-none ${anchor} ${className}`}>
      <div className={`relative flex ${s.gap}`}>
        <div
          data-mana-cost-backdrop
          className={`absolute ${s.backdrop} rounded-full bg-gray-900/70`}
        />
        {shards.map((shard, i) => (
          <div
            key={i}
            className={`relative flex items-center justify-center ${s.container} rounded-full bg-gray-900/80 shadow-[0_1px_3px_rgba(0,0,0,0.6)] ${
              isReduced ? "ring-[1.5px] ring-green-400" : ""
            }`}
          >
            <ManaSymbol shard={shard} size="xs" className="w-full h-full" />
          </div>
        ))}
      </div>
    </div>
  );
}
