import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { AiDeckCandidate } from "../../services/aiDeckCatalog";
import { AI_DECK_RANDOM, type AiDeckSelection } from "../../stores/preferencesStore";
import { MenuSelect } from "../ui/MenuSelect";

interface AiDeckPickerProps {
  /** The seat's current selection: a catalog id or `AI_DECK_RANDOM`. */
  value: AiDeckSelection;
  /** Every legal catalog deck — always listed as explicit options, so a seat can
   *  pin any legal deck even when no preference filter leaves a Random pool. */
  candidates: AiDeckCandidate[];
  /** The filtered Random pool — drives only the "Random (N)" count label. */
  randomPool: AiDeckCandidate[];
  onChange: (value: AiDeckSelection) => void;
  /** Trigger button classes. */
  className: string;
  /** Wrapper classes around the trigger. */
  wrapperClassName?: string;
}

/**
 * One AI seat's deck dropdown: Random first, then each legal catalog deck
 * labeled `name — source · archetype · coverage%`. Shared by single-player vs-AI
 * setup and Host Game so both present the same list.
 */
export function AiDeckPicker({
  value,
  candidates,
  randomPool,
  onChange,
  className,
  wrapperClassName,
}: AiDeckPickerProps) {
  const { t } = useTranslation("menu");
  const isRandom = value === AI_DECK_RANDOM;
  // Explicit pins always list the full legal catalog: the Random pool is a
  // preference-filtered subset (possibly empty) and must not gate what can be pinned.
  const selectionValid = isRandom || candidates.some((d) => d.id === value);
  const effectiveSelection: AiDeckSelection = selectionValid ? value : AI_DECK_RANDOM;

  const randomDeckLabel = t("aiOpponent.deckRandomCount", { count: randomPool.length });
  const deckMenuItems = useMemo(() => {
    const sourceLabel = (candidate: AiDeckCandidate): string => {
      switch (candidate.source.type) {
        case "saved":
          return candidate.source.feedId ? t("aiOpponent.source.feed") : t("aiOpponent.source.user");
        case "feed":
          return candidate.source.feedId;
        case "precon":
          return t("aiOpponent.source.precon");
      }
    };
    const formatDeckLabel = (candidate: AiDeckCandidate): string => {
      const suffix = [
        sourceLabel(candidate),
        candidate.archetype,
        candidate.coveragePct != null ? `${candidate.coveragePct}%` : null,
      ]
        .filter(Boolean)
        .join(" · ");
      return suffix ? `${candidate.name} — ${suffix}` : candidate.name;
    };
    return [
      { value: AI_DECK_RANDOM, label: randomDeckLabel },
      ...candidates.map((d) => ({ value: d.id, label: formatDeckLabel(d) })),
    ];
  }, [candidates, randomDeckLabel, t]);
  const selectedDeckLabel =
    effectiveSelection === AI_DECK_RANDOM
      ? randomDeckLabel
      : (deckMenuItems.find((item) => item.value === effectiveSelection)?.label ?? randomDeckLabel);

  return (
    <MenuSelect
      ariaLabel={t("aiOpponent.deck")}
      label={selectedDeckLabel}
      selectedValue={effectiveSelection}
      items={deckMenuItems}
      onSelect={(next) => onChange(next as AiDeckSelection)}
      menuLayout="dropdown"
      fitContainer
      wrapperClassName={wrapperClassName}
      className={className}
    />
  );
}
