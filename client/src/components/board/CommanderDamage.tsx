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
        const showAttackerLabel = views.length > 1;
        return (
          <div
            key={`from-${attacker}`}
            className="flex min-w-0 flex-col gap-1 border-l-2 pl-1"
            style={{ borderLeftColor: attackerSeatColor }}
            title={t("player.commanderDamageFrom", { source: attackerLabel, damage: total, threshold })}
          >
            {showAttackerLabel && (
              <span
                className="flex items-center gap-1 text-[9px] font-semibold uppercase tracking-[0.12em]"
                style={{ color: attackerSeatColor }}
              >
                <span
                  aria-hidden
                  className="h-1.5 w-1.5 shrink-0 rounded-full"
                  style={{ backgroundColor: attackerSeatColor }}
                />
                <span className="max-w-[7rem] truncate">{attackerLabel}</span>
              </span>
            )}
            {views.map((view) => {
              const obj = gameState?.objects[view.commander];
              const name = obj?.name ?? `#${view.commander}`;
              const isLethal = view.damage >= threshold;
              const isWarning = view.damage >= threshold * 0.75;
              const progress = Math.min(100, Math.max(0, (view.damage / threshold) * 100));
              const tone = isLethal
                ? {
                    bar: "bg-red-400",
                    card: "border-red-400/45 bg-red-950/55 text-red-50 shadow-red-950/25",
                    count: "bg-red-400 text-red-950",
                  }
                : isWarning
                  ? {
                      bar: "bg-amber-300",
                      card: "border-amber-300/35 bg-amber-950/38 text-amber-50 shadow-amber-950/20",
                      count: "bg-amber-300 text-amber-950",
                    }
                  : {
                      bar: "bg-slate-300",
                      card: "border-white/10 bg-slate-950/72 text-slate-100 shadow-black/20",
                      count: "bg-white/12 text-slate-50 ring-1 ring-white/15",
                    };
              return (
                <div
                  key={`${view.commander}`}
                  className={`w-[8.75rem] overflow-hidden rounded-md border px-2 py-1 text-[10px] shadow-md ${tone.card}`}
                  title={t("player.commanderDamageFrom", { source: name, damage: view.damage, threshold })}
                >
                  <div className="flex min-w-0 items-center justify-between gap-2">
                    <span className="min-w-0 truncate font-semibold leading-none">{name}</span>
                    <span className={`inline-flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full px-1 text-[10px] font-black leading-none tabular-nums ${tone.count}`}>
                      {view.damage}
                    </span>
                  </div>
                  <div className="mt-1 h-1 overflow-hidden rounded-full bg-black/35">
                    <div
                      aria-hidden
                      className={`h-full rounded-full ${tone.bar}`}
                      style={{ width: `${progress}%` }}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}
