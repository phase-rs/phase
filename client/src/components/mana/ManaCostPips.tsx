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
  // ~5.2% of the card's width. 7cqi keeps a legibility margin over that while
  // still fitting inside the title bar, so even a five-symbol cost clears the
  // card name instead of running through it.
  fluid: { container: "w-[7cqi] h-[7cqi] p-[0.6cqi]", gap: "gap-[0.6cqi]", backdrop: "-inset-x-[0.6cqi] -top-[0.6cqi] -bottom-[1.2cqi]" },
};

// Where the printed cost sits on an M15 frame: right edge ~7% in from the card's
// right edge, top edge ~5.4% down from its top. Owning this here keeps the
// anchor in lockstep with the pip diameter above — the two only look right
// together, and every card overlay wants the same placement.
const FLUID_ANCHOR = "absolute right-[6.5%] top-[5%]";

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
  if (shards.length === 0 && isReduced) shards.push("0");
  if (shards.length === 0) return null;

  const s = PIP_SIZES[size];

  return (
    <div className={`pointer-events-none ${size === "fluid" ? FLUID_ANCHOR : ""} ${className}`}>
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
