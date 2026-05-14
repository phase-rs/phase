import type { CommanderBracketTier } from "../../adapter/types";
import { BRACKET_TIER_NUMERIC } from "../../adapter/types";
import { BRACKET_LABEL, type CommanderBracket } from "../../types/bracket";

const TIER_CHIP_CLASS: Record<CommanderBracketTier, string> = {
  exhibition: "border-slate-300/60 bg-slate-500/30 text-slate-100",
  core: "border-emerald-300/60 bg-emerald-500/30 text-emerald-100",
  upgraded: "border-indigo-300/60 bg-indigo-500/30 text-indigo-100",
  optimized: "border-amber-300/60 bg-amber-500/30 text-amber-100",
  cedh: "border-rose-300/60 bg-rose-500/30 text-rose-100",
};

interface Props {
  tier: CommanderBracketTier | null;
}

export function BracketEstimateChip({ tier }: Props) {
  if (tier === null) return null;
  const num = BRACKET_TIER_NUMERIC[tier];
  return (
    <span
      className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${TIER_CHIP_CLASS[tier]}`}
      title={`Estimated bracket: B${num} ${BRACKET_LABEL[num as CommanderBracket]}`}
    >
      Estimated: B{num}
    </span>
  );
}
