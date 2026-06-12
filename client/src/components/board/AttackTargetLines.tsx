import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { objectAnchorSelector } from "../../utils/objectAnchorSelector.ts";
import { isOneOnOne } from "../../viewmodel/gameStateView.ts";
import {
  arcPath,
  useAttackerArrowPositions,
  type AttackerArrow,
  type AttackerArrowTarget,
} from "../../hooks/useAttackerArrowPositions.ts";
import type { ObjectId, PlayerId } from "../../adapter/types.ts";

interface AttackArrowData {
  key: string;
  from: { x: number; y: number };
  to: { x: number; y: number };
  isAtMe: boolean;
  isBlocked: boolean;
}

function AttackArrowPath({ arrow, isMinimal }: { arrow: AttackArrowData; isMinimal: boolean }) {
  const d = arcPath(arrow.from, arrow.to);
  const dash = arrow.isBlocked ? "8 6" : undefined;
  return (
    <g>
      <path
        d={d}
        stroke="black"
        strokeWidth={isMinimal ? 3 : 5}
        fill="none"
        strokeLinecap="round"
        strokeDasharray={dash}
        markerEnd="url(#attack-arrow-head)"
      />
      <path
        d={d}
        stroke={arrow.isAtMe ? "rgba(220,38,38,0.95)" : "rgba(220,38,38,0.45)"}
        strokeWidth={arrow.isAtMe ? 2.5 : 2}
        fill="none"
        filter={isMinimal || !arrow.isAtMe ? undefined : "url(#attack-target-glow)"}
        strokeDasharray={dash}
        markerEnd="url(#attack-arrow-head)"
        strokeLinecap="round"
      />
    </g>
  );
}

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
  const gameState = useGameStore((s) => s.gameState);
  const combat = gameState?.combat ?? null;
  const objects = gameState?.objects;
  const seatOrder = gameState?.seat_order;
  const eliminatedPlayers = gameState?.eliminated_players;
  const focusedOpponent = useUiStore((s) => s.focusedOpponent) as PlayerId | null;
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const localPlayerId = usePlayerId();
  const isMinimal = vfxQuality === "minimal";

  const isMultiplayer = gameState != null && !isOneOnOne(gameState);
  const effectiveFocusedOpponent = useMemo(() => {
    if (focusedOpponent != null) return focusedOpponent;
    const eliminated = new Set(eliminatedPlayers ?? []);
    return seatOrder?.find((id) => id !== localPlayerId && !eliminated.has(id)) ?? null;
  }, [focusedOpponent, seatOrder, eliminatedPlayers, localPlayerId]);

  const isControllerOnScreen = useMemo(() => {
    return (controllerId: PlayerId) =>
      controllerId === localPlayerId || controllerId === effectiveFocusedOpponent;
  }, [localPlayerId, effectiveFocusedOpponent]);

  const blockedAttackerIds = useMemo<Set<number>>(() => {
    if (!combat) return new Set();
    const ids = new Set<number>();
    for (const [attackerId, blockers] of Object.entries(combat.blocker_assignments)) {
      if (blockers.length > 0) ids.add(Number(attackerId));
    }
    return ids;
  }, [combat]);

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
          const controller = objects?.[t.data]?.controller;
          out.push({
            attackerId: attacker.object_id,
            target: { kind: "object", objectId: t.data },
            isAtMe: controller === localPlayerId,
          });
          break;
        }
        default: {
          const _exhaustive: never = t;
          return _exhaustive;
        }
      }
    }
    return out;
  }, [combat, isMultiplayer, localPlayerId, objects]);

  const onScreenArrows = useMemo(() => {
    if (!isMultiplayer || !objects) return arrows;
    return arrows.filter((a) => {
      const controller = objects[a.attackerId]?.controller;
      return controller == null || isControllerOnScreen(controller);
    });
  }, [arrows, isMultiplayer, objects, isControllerOnScreen]);

  const positions = useAttackerArrowPositions(onScreenArrows);

  const hudIndicators = useHudAttackIndicators(
    arrows,
    objects ?? null,
    isControllerOnScreen,
    isMultiplayer,
    blockedAttackerIds,
  );

  const creatureArrowData = useMemo<AttackArrowData[]>(() =>
    positions.map((p) => ({
      key: p.key,
      from: p.from,
      to: p.to,
      isAtMe: p.isAtMe,
      isBlocked: blockedAttackerIds.has(Number(p.key.split("->")[0])),
    })),
  [positions, blockedAttackerIds]);

  if (creatureArrowData.length === 0 && hudIndicators.length === 0) return null;

  return createPortal(
    <svg className="pointer-events-none fixed inset-0 z-30 h-full w-full">
      <defs>
        {!isMinimal && (
          <filter id="attack-target-glow">
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        )}
        <marker
          id="attack-arrow-head"
          markerWidth="4"
          markerHeight="3.5"
          refX="4"
          refY="1.75"
          orient="auto"
        >
          <path d="M0,0 L4,1.75 L0,3.5 Z" fill="rgba(220,38,38,0.95)" />
        </marker>
      </defs>

      {creatureArrowData.map((arrow) => (
        <AttackArrowPath key={arrow.key} arrow={arrow} isMinimal={isMinimal} />
      ))}
      {hudIndicators.map((arrow) => (
        <AttackArrowPath key={arrow.key} arrow={arrow} isMinimal={isMinimal} />
      ))}
    </svg>,
    document.body,
  );
}

function useHudAttackIndicators(
  arrows: AttackerArrow[],
  objects: Record<string, { controller: PlayerId }> | null,
  isControllerOnScreen: (id: PlayerId) => boolean,
  isMultiplayer: boolean,
  blockedAttackerIds: Set<number>,
): AttackArrowData[] {
  const [indicators, setIndicators] = useState<AttackArrowData[]>([]);
  const stableCountRef = useRef(0);

  const offScreenAttacks = useMemo(() => {
    if (!isMultiplayer || !objects) return [];
    const result: { attackingPlayerId: PlayerId; target: AttackerArrowTarget; isAtMe: boolean; attackerId: ObjectId }[] = [];
    for (const a of arrows) {
      const controller = objects[a.attackerId]?.controller;
      if (controller == null) continue;
      if (isControllerOnScreen(controller)) continue;
      result.push({
        attackingPlayerId: controller,
        target: a.target,
        isAtMe: a.isAtMe,
        attackerId: a.attackerId,
      });
    }
    return result;
  }, [arrows, isMultiplayer, objects, isControllerOnScreen]);

  const prevCountRef = useRef(0);

  useEffect(() => {
    if (offScreenAttacks.length === 0) {
      setIndicators([]);
      prevCountRef.current = 0;
      return;
    }
    stableCountRef.current = 0;
    let rafId: number;

    function targetSelector(target: AttackerArrowTarget): string {
      return target.kind === "player"
        ? `[data-player-hud="${target.playerId}"]`
        : objectAnchorSelector(target.objectId);
    }

    function poll() {
      const next: AttackArrowData[] = [];

      for (const { attackingPlayerId, target, isAtMe, attackerId } of offScreenAttacks) {
        const hudEl = document.querySelector(`[data-player-hud="${attackingPlayerId}"]`);
        const targetEl = document.querySelector(targetSelector(target));
        if (!hudEl || !targetEl) continue;
        const hudRect = hudEl.getBoundingClientRect();
        const targetRect = targetEl.getBoundingClientRect();
        const targetKey = target.kind === "player" ? `p${target.playerId}` : `o${target.objectId}`;
        next.push({
          key: `hud:${attackingPlayerId}:${attackerId}->${targetKey}`,
          from: { x: hudRect.left + hudRect.width / 2, y: hudRect.top + hudRect.height / 2 },
          to: { x: targetRect.left + targetRect.width / 2, y: targetRect.top + targetRect.height / 2 },
          isAtMe,
          isBlocked: blockedAttackerIds.has(attackerId),
        });
      }

      const changed = next.length !== prevCountRef.current;
      prevCountRef.current = next.length;
      stableCountRef.current = changed ? 0 : stableCountRef.current + 1;
      setIndicators(next);

      if (stableCountRef.current < 10) {
        rafId = requestAnimationFrame(poll);
      }
    }

    rafId = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(rafId);
  }, [offScreenAttacks, blockedAttackerIds]);

  return indicators;
}
