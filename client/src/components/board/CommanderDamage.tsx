import type { PlayerId } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface CommanderDamageProps {
  playerId: PlayerId;
}

/**
 * Fallback threshold used only when FormatConfig.commander_damage_threshold
 * is unset (non-Commander formats that somehow produced commander-damage
 * entries). Real threshold comes from the engine's FormatConfig — see
 * crates/engine/src/types/format.rs.
 */
const DEFAULT_COMMANDER_DAMAGE_LETHAL = 21;

/**
 * Pure renderer for engine-authored commander-damage grouping. The
 * grouping logic lives in `crates/engine/src/game/derived_views.rs`
 * (`derive_views`); this component never groups, filters, or aggregates
 * game state — CLAUDE.md: "The frontend is a display layer, not a logic
 * layer." Reads `gameState.derived.commander_damage_by_attacker`, which
 * the adapter attaches from the wire-format `ClientGameState.derived`
 * envelope on every state snapshot.
 */
export function CommanderDamage({ playerId }: CommanderDamageProps) {
  const gameState = useGameStore((s) => s.gameState);
  const localPlayerId = usePlayerId();
  const threshold =
    gameState?.format_config?.commander_damage_threshold ??
    DEFAULT_COMMANDER_DAMAGE_LETHAL;

  const byAttacker = gameState?.derived?.commander_damage_by_attacker ?? {};
  // Filter to entries inflicted on *this* player (the victim axis), then
  // group by attacker for HUD rendering. The inner arrays are already
  // per-commander thanks to engine-side partner handling.
  const entriesForVictim: Array<{
    attacker: string;
    views: { commander: number; damage: number }[];
  }> = [];
  for (const [attackerKey, views] of Object.entries(byAttacker)) {
    const forMe = views.filter((v) => v.victim === playerId && v.damage > 0);
    if (forMe.length === 0) continue;
    entriesForVictim.push({ attacker: attackerKey, views: forMe });
  }

  if (entriesForVictim.length === 0) return null;

  return (
    <div
      className="flex flex-col gap-1"
      data-testid={`commander-damage-${playerId}`}
    >
      {entriesForVictim.map(({ attacker, views }) => {
        const attackerLabel =
          Number(attacker) === localPlayerId ? "You" : `Opp ${attacker}`;
        const total = views.reduce((n, e) => n + e.damage, 0);
        return (
          <div
            key={`from-${attacker}`}
            className="flex flex-wrap items-center gap-1"
            title={`Commander damage from ${attackerLabel}: ${total}/${threshold}`}
          >
            <span className="text-[9px] uppercase tracking-wide text-slate-400">
              {attackerLabel}
            </span>
            {views.map((view) => {
              const obj = gameState?.objects[view.commander];
              const name = obj?.name ?? `#${view.commander}`;
              const isLethal = view.damage >= threshold;
              const isWarning = view.damage >= threshold - 5;
              return (
                <div
                  key={`${view.commander}`}
                  className={`flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium ${
                    isLethal
                      ? "bg-red-900/80 text-red-200"
                      : isWarning
                        ? "bg-yellow-900/60 text-yellow-200"
                        : "bg-gray-800/80 text-gray-300"
                  }`}
                  title={`Commander damage from ${name}: ${view.damage}/${threshold}`}
                >
                  <span className="max-w-[60px] truncate">{name}</span>
                  <span className="tabular-nums font-bold">{view.damage}</span>
                </div>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}
