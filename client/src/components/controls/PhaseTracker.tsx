import type { Phase } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

const PHASES: { key: Phase; label: string }[] = [
  { key: "Untap", label: "UNT" },
  { key: "Upkeep", label: "UPK" },
  { key: "Draw", label: "DRW" },
  { key: "PreCombatMain", label: "M1" },
  { key: "BeginCombat", label: "BC" },
  { key: "DeclareAttackers", label: "ATK" },
  { key: "DeclareBlockers", label: "BLK" },
  { key: "CombatDamage", label: "DMG" },
  { key: "EndCombat", label: "EC" },
  { key: "PostCombatMain", label: "M2" },
  { key: "End", label: "END" },
  { key: "Cleanup", label: "CLN" },
];

export function PhaseTracker() {
  const phase = useGameStore((s) => s.gameState?.phase ?? "Untap");
  const turnNumber = useGameStore((s) => s.gameState?.turn_number ?? 0);

  return (
    <div className="flex flex-col gap-1">
      <div className="text-center text-xs font-semibold text-gray-300">
        Turn {turnNumber}
      </div>
      <div className="flex flex-wrap gap-0.5">
        {PHASES.map(({ key, label }) => {
          const isCurrent = key === phase;
          return (
            <span
              key={key}
              className={`rounded px-1 py-0.5 text-[10px] font-bold transition-colors ${
                isCurrent
                  ? "bg-white/20 text-white shadow-[0_0_6px_1px_rgba(255,255,255,0.4)]"
                  : "text-gray-600"
              }`}
            >
              {label}
            </span>
          );
        })}
      </div>
    </div>
  );
}
