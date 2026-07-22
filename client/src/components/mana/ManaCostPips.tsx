import type { ManaCost } from "../../adapter/types.ts";
import { manaCostToShards } from "../../viewmodel/costLabel.ts";
import { ManaSymbol } from "./ManaSymbol.tsx";

type PipSize = "2xs" | "xs" | "sm" | "md" | "lg" | "fluid";

const PIP_SIZES: Record<PipSize, { container: string; gap: string; backdrop: string }> = {
  "2xs": { container: "w-[10px] h-[10px] p-[0px]", gap: "gap-[0.5px]", backdrop: "-inset-x-[1px] top-[2px] -bottom-[3px]" },
  xs: { container: "w-[12px] h-[12px] p-[0px]", gap: "gap-[0.5px]", backdrop: "-inset-x-[1px] top-[2px] -bottom-[4px]" },
  sm: { container: "w-[18px] h-[18px] p-[0px]", gap: "gap-[1px]", backdrop: "-inset-x-[2px] top-[4px] -bottom-[8px]" },
  md: { container: "w-[22px] h-[22px] p-[2px]", gap: "gap-[1px]", backdrop: "-inset-x-[3px] -top-[2px] -bottom-[4px]" },
  lg: { container: "w-[28px] h-[28px] py-[2px] px-[2.5px]", gap: "gap-[0.5px]", backdrop: "-inset-x-[3px] -top-[2px] -bottom-[4px]" },
  // Card-relative sizing in container-query inline units (1cqi = 1% of the
  // nearest `@container` ancestor's width). Consumers that pass `size="fluid"`
  // MUST wrap the pips in an element with `container-type: inline-size` sized to
  // the card (e.g. an `absolute inset-0 @container` overlay). 15cqi matches the
  // fixed `md` (22px) pip on a ~145px desktop hand card, then scales down with
  // the card so pips never look oversized on smaller layouts. Secondary
  // dimensions use the same md ratios (p 9.1%, gap 4.5%, backdrop 13.6/9.1/18.2%).
  fluid: { container: "w-[15cqi] h-[15cqi] p-[1.4cqi]", gap: "gap-[0.7cqi]", backdrop: "-inset-x-[2cqi] -top-[1.4cqi] -bottom-[2.7cqi]" },
};

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
    <div className={`pointer-events-none ${className}`}>
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
