import { useTranslation } from "react-i18next";

import type { DeckCompatibilityResult } from "../../services/deckCompatibility";
import { scryfallLegalityKey } from "../../services/scryfall";
import { DECK_CONSTRUCTION_FORMATS } from "../../data/formatRegistry";
import type { BracketEstimate, CommanderBracket } from "../../types/bracket";
import { ManaCurve } from "./ManaCurve";
import { BracketAuditPanel } from "./BracketAuditPanel";
import { BracketPicker } from "./BracketPicker";

const LEGALITY_STYLES: Record<string, string> = {
  legal: "bg-emerald-600/70 text-emerald-100",
  banned: "bg-red-600/70 text-red-100",
  restricted: "bg-yellow-600/70 text-yellow-100",
  not_legal: "bg-gray-600/40 text-gray-500",
};

const FORMAT_BADGE_METADATA = DECK_CONSTRUCTION_FORMATS
  .map((metadata) => {
    const legalityKey = scryfallLegalityKey(metadata.format);
    return legalityKey
      ? { key: legalityKey, label: metadata.short_label, title: metadata.label }
      : null;
  })
  .filter((entry): entry is { key: string; label: string; title: string } => entry !== null);

function formatLegalityBadges(formatLegality: Record<string, string>) {
  const knownKeys = new Set(FORMAT_BADGE_METADATA.map((entry) => entry.key));
  return [
    ...FORMAT_BADGE_METADATA.filter((entry) => entry.key in formatLegality),
    ...Object.keys(formatLegality)
      .filter((key) => !knownKeys.has(key))
      .sort()
      .map((key) => ({ key, label: key.slice(0, 3).toUpperCase(), title: key })),
  ];
}

interface StatsPanelProps {
  compatibility: DeckCompatibilityResult | null;
  cmcValues: number[];
  colorValues: string[];
  isCommander: boolean;
  estimate: BracketEstimate | null;
  manualBracket: CommanderBracket | null;
  onBracketChange: (bracket: CommanderBracket | null) => void;
  auditEmptyReason?: "not-commander" | "no-commander" | "unsupported";
  onCardClick: (cardName: string) => void;
}

export function StatsPanel({
  compatibility,
  cmcValues,
  colorValues,
  isCommander,
  estimate,
  manualBracket,
  onBracketChange,
  auditEmptyReason,
  onCardClick,
}: StatsPanelProps) {
  const { t } = useTranslation("deck-builder");
  const coverage = compatibility?.coverage;
  const showLegality = Boolean(
    compatibility?.format_legality
      || (coverage && coverage.unsupported_cards.length > 0),
  );

  return (
    <div className="flex flex-col gap-3">
      {isCommander && (
        <div className="space-y-2">
          {/* The bracket picker lives beside the audit it's compared against, so
              setting a bracket and seeing the deck's estimated tier (and any
              mismatch) read as one unit. Both are Commander-only. */}
          <div className="space-y-1.5">
            <span className="text-[10px] uppercase tracking-wider text-gray-500">
              {t("toolbar.bracket")}
            </span>
            <BracketPicker value={manualBracket} onChange={onBracketChange} />
          </div>
          <BracketAuditPanel
            estimate={estimate}
            manualBracket={manualBracket}
            emptyReason={auditEmptyReason}
            onCardClick={onCardClick}
          />
        </div>
      )}

      <div className="rounded-[18px] border border-white/8 bg-black/18 p-3">
        <ManaCurve cmcValues={cmcValues} colorValues={colorValues} />
      </div>

      {showLegality && (
        <div className="space-y-3 rounded-[18px] border border-white/8 bg-black/18 p-3">
          {compatibility?.format_legality && (
            <div>
              <div className="mb-1 text-[10px] uppercase tracking-wider text-gray-500">{t("stats.formatLegality")}</div>
              <div className="flex flex-wrap gap-1">
                {formatLegalityBadges(compatibility.format_legality).map((fmt) => {
                  const status = compatibility.format_legality?.[fmt.key] ?? "not_legal";
                  return (
                    <span
                      key={fmt.key}
                      className={`rounded px-1.5 py-0.5 text-[9px] font-semibold leading-tight ${LEGALITY_STYLES[status] ?? LEGALITY_STYLES.not_legal}`}
                      title={`${fmt.title}: ${status.replace("_", " ")}`}
                    >
                      {fmt.label}
                    </span>
                  );
                })}
              </div>
            </div>
          )}
          {coverage && coverage.unsupported_cards.length > 0 && (
            <div>
              <div className="mb-1 text-[10px] uppercase tracking-wider text-gray-500">{t("stats.engineCoverage")}</div>
              <div className="flex items-center gap-2">
                <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-gray-700">
                  <div
                    className="h-full rounded-full bg-orange-500"
                    style={{ width: `${coverage.total_unique > 0 ? (coverage.supported_unique / coverage.total_unique) * 100 : 0}%` }}
                  />
                </div>
                <span
                  className="shrink-0 text-[10px] text-gray-400"
                  title={t("stats.unsupportedTitle", {
                    list: coverage.unsupported_cards.map((c) => `${c.name}: ${c.gaps.join(", ")}`).join("\n"),
                  })}
                >
                  {coverage.supported_unique}/{coverage.total_unique}
                </span>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
