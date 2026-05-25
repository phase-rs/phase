import { useTranslation } from "react-i18next";

import type { GameFormat } from "../../adapter/types";
import { scryfallLegalityKey } from "../../services/scryfall";

interface LegalityBadgeProps {
  legalities: Record<string, string>;
  format: GameFormat;
}

const STATUS_STYLES: Record<string, string> = {
  legal: "bg-green-700/60 text-green-300",
  banned: "bg-red-700/60 text-red-300",
  restricted: "bg-yellow-700/60 text-yellow-300",
  not_legal: "bg-gray-700/60 text-gray-400",
};

const STATUS_LABEL_KEYS: Record<string, string> = {
  legal: "legality.legal",
  banned: "legality.banned",
  restricted: "legality.restricted",
  not_legal: "legality.notLegal",
};

export function LegalityBadge({ legalities, format }: LegalityBadgeProps) {
  const { t } = useTranslation("deck-builder");
  const legalityKey = scryfallLegalityKey(format);
  if (!legalityKey) return null;

  const status = (legalities[legalityKey] ?? "not_legal").toLowerCase();
  const style = STATUS_STYLES[status] ?? STATUS_STYLES.not_legal;
  const label = t(STATUS_LABEL_KEYS[status] ?? "legality.notLegal");

  return (
    <span
      className={`inline-block rounded px-1.5 py-0.5 text-[9px] font-semibold leading-tight ${style}`}
    >
      {label}
    </span>
  );
}
