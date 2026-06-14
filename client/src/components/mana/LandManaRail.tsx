import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { GameObject } from "../../adapter/types.ts";
import { landManaAvailability, type LandManaSource } from "../../viewmodel/manaAvailability.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";
import { ManaSymbol } from "./ManaSymbol.tsx";

// Cap the rail width on crowded boards; the trailing "untapped/total" count
// stays exact even when individual pips overflow into a "+N".
const MAX_PIPS = 12;

interface LandManaRailProps {
  lands: GameObject[];
  className?: string;
}

/**
 * Per-source mana rail: one pip-group per land, untapped (available) sources
 * first and bright, tapped sources dimmed. Communicates "what mana is
 * available now" without summing per color — see `manaAvailability`.
 */
export function LandManaRail({ lands, className = "" }: LandManaRailProps) {
  const { t } = useTranslation("game");

  const { shown, overflow, total, untapped } = useMemo(() => {
    const availability = landManaAvailability(lands);
    // Untapped sources lead so the actionable (available) mana reads first.
    const ordered = [...availability.sources].sort(
      (a, b) => Number(a.tapped) - Number(b.tapped),
    );
    return {
      total: availability.total,
      untapped: availability.untapped,
      shown: ordered.slice(0, MAX_PIPS),
      overflow: Math.max(0, ordered.length - MAX_PIPS),
    };
  }, [lands]);

  if (total === 0) return null;

  return (
    <div className={`group relative flex items-center gap-1 ${className}`}>
      <div className="flex items-center gap-[2px]">
        {shown.map((source, i) => (
          <SourcePips key={i} source={source} />
        ))}
        {overflow > 0 && (
          <span className="text-[10px] font-medium tabular-nums text-gray-400">+{overflow}</span>
        )}
      </div>
      <span className="text-[10px] tabular-nums text-gray-500">
        {untapped}/{total}
      </span>
      <GameplayTooltip>{t("player.manaAvailable", { untapped, total })}</GameplayTooltip>
    </div>
  );
}

/** One land rendered as a single tight pip-group (lettered, rainbow, or neutral). */
function SourcePips({ source }: { source: LandManaSource }) {
  const dim = source.tapped ? "opacity-40 grayscale" : "";

  if (source.shards.length > 0) {
    return (
      <span className={`flex items-center ${dim}`}>
        {source.shards.map((shard, i) => (
          <ManaSymbol key={i} shard={shard} size="xs" />
        ))}
      </span>
    );
  }

  if (source.rainbow) {
    // Flexible output with no single Scryfall symbol → a WUBRG rainbow swatch.
    return (
      <span
        className={`inline-block h-3.5 w-3.5 rounded-full ring-1 ring-black/30 ${dim}`}
        style={{
          background:
            "conic-gradient(#f8e7b3 0deg 72deg, #a8d4f0 72deg 144deg, #c4bcb4 144deg 216deg, #f0a9a0 216deg 288deg, #acd6b0 288deg 360deg)",
        }}
      />
    );
  }

  // No mana ability (e.g. Maze of Ith): a neutral dot keeps the land count
  // honest without implying it taps for mana.
  return <span className={`inline-block h-3.5 w-3.5 rounded-full bg-gray-600 ${dim}`} />;
}
