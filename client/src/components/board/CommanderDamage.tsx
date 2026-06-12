import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { getSeatColor } from "../../hooks/useSeatColor.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerDisplayName, useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import { commanderDamageEntriesFor } from "../../viewmodel/commanderColumn.ts";

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
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const playerNames = useMultiplayerStore((s) => s.playerNames);
  const localPlayerId = usePlayerId();
  const threshold =
    gameState?.format_config?.commander_damage_threshold ??
    DEFAULT_COMMANDER_DAMAGE_LETHAL;

  // Per-victim grouping lives in viewmodel/commanderColumn, shared with
  // PlayerArea's column-visibility gate so the wrapper renders from the exact
  // same set this component does (they previously drifted — see that module).
  const entriesForVictim = gameState ? commanderDamageEntriesFor(gameState, playerId) : [];

  if (entriesForVictim.length === 0) return null;

  return (
    <div
      className="flex flex-col gap-1"
      data-testid={`commander-damage-${playerId}`}
    >
      {entriesForVictim.map(({ attacker, views }) => {
        const attackerId = Number(attacker) as PlayerId;
        const attackerLabel = attackerId === localPlayerId
          ? t("player.you")
          : playerNames.get(attackerId) ?? getPlayerDisplayName(attackerId, localPlayerId);
        const attackerSeatColor = getSeatColor(attackerId, gameState?.seat_order);
        const total = views.reduce((n, e) => n + e.damage, 0);
        return (
          <div
            key={`from-${attacker}`}
            className="flex flex-wrap items-center gap-1"
            title={t("player.commanderDamageFrom", { source: attackerLabel, damage: total, threshold })}
          >
            <span
              className="flex items-center gap-1 text-[9px] font-semibold uppercase tracking-wide"
              style={{ color: attackerSeatColor }}
            >
              <span
                aria-hidden
                className="h-1.5 w-1.5 shrink-0 rounded-full"
                style={{ backgroundColor: attackerSeatColor }}
              />
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
                  title={t("player.commanderDamageFrom", { source: name, damage: view.damage, threshold })}
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
