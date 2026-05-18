import {
  BRACKET_LABEL,
  BRACKET_TIER_CHIP_CLASS,
  BRACKET_TIER_NUMERIC,
  type CommanderBracketTier,
} from "../../types/bracket";

interface Props {
  tier: CommanderBracketTier | null;
}

export function BracketEstimateChip({ tier }: Props) {
  if (tier === null) return null;
  const num = BRACKET_TIER_NUMERIC[tier];
  const label = `Estimated bracket: B${num} ${BRACKET_LABEL[num]}`;
  return (
    <span
      className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${BRACKET_TIER_CHIP_CLASS[tier]}`}
      aria-label={label}
      title={label}
    >
      Estimated: B{num}
    </span>
  );
}
