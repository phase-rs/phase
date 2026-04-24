import { useMemo } from "react";
import { createPortal } from "react-dom";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import {
  arcPath,
  useAttackerArrowPositions,
  type AttackerArrow,
} from "../../hooks/useAttackerArrowPositions.ts";

/** Red solid-arc arrows from attackers to their declared targets.
 *
 *  Unified across all target kinds — Player, Planeswalker, Battle — so the
 *  visual weight of a gang attack on your planeswalker reads the same as a
 *  gang attack on your life total. `isAtMe` thickens the stroke and enables
 *  the glow filter so the local defender's view stays dominant over arrows
 *  between other opponents.
 *
 *  Player-target arrows only draw in multiplayer (>2 players); in 1v1 the
 *  player attack is implicit and drawing would be visual noise. */
export function AttackTargetLines() {
  const combat = useGameStore((s) => s.gameState?.combat ?? null);
  const objects = useGameStore((s) => s.gameState?.objects);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const localPlayerId = usePlayerId();
  const isMinimal = vfxQuality === "minimal";

  const isMultiplayer = (seatOrder?.length ?? 0) > 2;

  const arrows = useMemo<AttackerArrow[]>(() => {
    if (!combat) return [];
    const out: AttackerArrow[] = [];
    for (const attacker of combat.attackers) {
      const t = attacker.attack_target;
      switch (t.type) {
        case "Player": {
          if (!isMultiplayer) break;
          out.push({
            attackerId: attacker.object_id,
            target: { kind: "player", playerId: t.data },
            isAtMe: t.data === localPlayerId,
          });
          break;
        }
        case "Planeswalker":
        case "Battle": {
          // `isAtMe` for a permanent target is "do I control the thing being attacked?"
          const controller = objects?.[t.data]?.controller;
          out.push({
            attackerId: attacker.object_id,
            target: { kind: "object", objectId: t.data },
            isAtMe: controller === localPlayerId,
          });
          break;
        }
        default: {
          // Exhaustiveness check — a new AttackTarget variant must be
          // handled explicitly above. Fails typecheck if a variant is
          // added to the engine and this switch is not updated.
          const _exhaustive: never = t;
          return _exhaustive;
        }
      }
    }
    return out;
  }, [combat, isMultiplayer, localPlayerId, objects]);

  const positions = useAttackerArrowPositions(arrows);

  if (positions.length === 0) return null;

  return createPortal(
    <svg className="pointer-events-none fixed inset-0 z-30 h-full w-full">
      {!isMinimal && (
        <defs>
          <filter id="attack-target-glow">
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
          {/* Two markers so the arrowhead weight matches the stroke weight —
              otherwise an `isAtMe` (3.5px) arrow looks nose-heavy with an
              understroked head. */}
          <marker
            id="attack-target-arrow-spectator"
            markerWidth="8"
            markerHeight="6"
            refX="8"
            refY="3"
            orient="auto"
          >
            <path d="M0,0 L8,3 L0,6 Z" fill="rgba(220,38,38,0.55)" />
          </marker>
          <marker
            id="attack-target-arrow-atme"
            markerWidth="10"
            markerHeight="8"
            refX="10"
            refY="4"
            orient="auto"
          >
            <path d="M0,0 L10,4 L0,8 Z" fill="rgba(220,38,38,0.95)" />
          </marker>
        </defs>
      )}

      {positions.map((arrow) => (
        <path
          key={arrow.key}
          d={arcPath(arrow.from, arrow.to)}
          stroke={arrow.isAtMe ? "rgba(220,38,38,0.95)" : "rgba(220,38,38,0.45)"}
          strokeWidth={arrow.isAtMe ? 3.5 : 2}
          fill="none"
          filter={isMinimal || !arrow.isAtMe ? undefined : "url(#attack-target-glow)"}
          markerEnd={
            isMinimal
              ? undefined
              : arrow.isAtMe
                ? "url(#attack-target-arrow-atme)"
                : "url(#attack-target-arrow-spectator)"
          }
        />
      ))}
    </svg>,
    document.body,
  );
}
