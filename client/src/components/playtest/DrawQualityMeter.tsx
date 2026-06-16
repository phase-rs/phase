/**
 * Hand quality indicator. Shows how many lands / spells are in the current hand
 * and a simple color-coded quality score based on land/spell ratio.
 */
import { useTranslation } from "react-i18next";
import type { PlaytestCard } from "../../services/playtestSession";
import { isLandType } from "../../services/playtestSession";

interface Props {
  hand: PlaytestCard[];
  availableMana: number;
  turnNumber: number;
}

function qualityScore(lands: number, total: number, turn: number): "great" | "good" | "medium" | "poor" {
  if (total === 0) return "poor";
  const ratio = lands / total;
  // Ideal: 2–3 lands in a 7-card hand (ratio ~0.29–0.43). Scales with turn.
  const ideal = Math.min(0.5, 0.25 + turn * 0.02);
  const deviation = Math.abs(ratio - ideal);
  if (deviation < 0.08) return "great";
  if (deviation < 0.18) return "good";
  if (deviation < 0.30) return "medium";
  return "poor";
}

const QUALITY_LABEL: Record<string, string> = {
  great:  "quality.great",
  good:   "quality.good",
  medium: "quality.medium",
  poor:   "quality.poor",
};

const QUALITY_COLOR: Record<string, string> = {
  great:  "bg-emerald-500",
  good:   "bg-blue-500",
  medium: "bg-yellow-500",
  poor:   "bg-red-500",
};

export function DrawQualityMeter({ hand, availableMana, turnNumber }: Props) {
  const { t } = useTranslation("playtest");
  const lands = hand.filter((c) => isLandType(c.face.cardType?.coreTypes ?? [])).length;
  const nonLands = hand.length - lands;
  const quality = qualityScore(lands, hand.length, turnNumber);

  return (
    <div className="space-y-2 rounded-xl border border-white/8 bg-black/18 p-3">
      <div className="flex items-center justify-between">
        <span className="text-[0.7rem] font-medium uppercase tracking-wider text-slate-400">
          {t("handQuality")}
        </span>
        <span className={`rounded-md px-2 py-0.5 text-[0.65rem] font-semibold uppercase ${QUALITY_COLOR[quality]} text-white`}>
          {t(QUALITY_LABEL[quality])}
        </span>
      </div>

      {/* Land / spell breakdown */}
      <div className="flex gap-3 text-xs">
        <span className="text-amber-300">
          {t("landsInHand", { count: lands })}
        </span>
        <span className="text-blue-300">
          {t("spellsInHand", { count: nonLands })}
        </span>
        <span className="ml-auto text-slate-400">
          {t("manaAvailable", { count: availableMana })}
        </span>
      </div>

      {/* Ratio bar */}
      {hand.length > 0 && (
        <div className="flex h-1.5 overflow-hidden rounded-full bg-slate-700">
          <div
            className="bg-amber-400 transition-all"
            style={{ width: `${(lands / hand.length) * 100}%` }}
          />
          <div
            className="bg-blue-400 transition-all"
            style={{ width: `${(nonLands / hand.length) * 100}%` }}
          />
        </div>
      )}
    </div>
  );
}
