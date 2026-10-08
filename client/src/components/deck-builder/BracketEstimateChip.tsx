import { useTranslation } from "react-i18next";

import {
  BRACKET_LABEL,
  BRACKET_NUMERIC_TIER,
  BRACKET_TIER_CHIP_CLASS,
  BRACKET_TIER_NUMERIC,
  type CommanderBracket,
  type CommanderBracketTier,
} from "../../types/bracket";

interface Props {
  tier: CommanderBracketTier | null;
}

export function BracketEstimateChip({ tier }: Props) {
  const { t } = useTranslation("deck-builder");
  if (tier === null) return null;
  const num = BRACKET_TIER_NUMERIC[tier];
  const label = t("bracket.estimatedChipFull", { tier: num, label: BRACKET_LABEL[num] });
  return (
    <span
      className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${BRACKET_TIER_CHIP_CLASS[tier]}`}
      aria-label={label}
      title={label}
    >
      {t("bracket.estimatedChip", { tier: num })}
    </span>
  );
}

interface ManualProps {
  bracket: CommanderBracket;
}

/**
 * Declared-bracket chip: same tier colors as the estimate chip, but without
 * the "Estimated:" prefix — the absence of the prefix is what tells the
 * pilot this is a human declaration, which overrides any estimate.
 */
export function ManualBracketChip({ bracket }: ManualProps) {
  const { t } = useTranslation("deck-builder");
  const tier = BRACKET_NUMERIC_TIER[bracket];
  const label = t("bracket.manualChipFull", { tier: bracket, label: BRACKET_LABEL[bracket] });
  return (
    <span
      className={`rounded-full border px-2 py-0.5 text-[10px] font-medium ${BRACKET_TIER_CHIP_CLASS[tier]}`}
      aria-label={label}
      title={label}
    >
      {t("bracket.manualChip", { tier: bracket })}
    </span>
  );
}
