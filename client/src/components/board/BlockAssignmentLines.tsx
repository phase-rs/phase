import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useRafPositions } from "../../hooks/useRafPositions.ts";
import { arcPath } from "../../hooks/useAttackerArrowPositions.ts";
import { objectAnchorSelector } from "../../utils/objectAnchorSelector.ts";
import { getVisibleBoardPlayerIds, isOneOnOne } from "../../viewmodel/gameStateView.ts";
import type { ObjectId, PlayerId } from "../../adapter/types.ts";
import { filterVisibleBlockerPairs } from "./blockAssignmentVisibility.ts";

const BLOCK_COLOR = "rgba(56,189,248,0.95)";
const BLOCK_COLOR_HEAD = "rgba(56,189,248,0.9)";

export function BlockAssignmentLines() {
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const combatMode = useUiStore((s) => s.combatMode);
  const focusedOpponent = useUiStore((s) => s.focusedOpponent) as PlayerId | null;
  const combat = useGameStore((s) => s.gameState?.combat ?? null);
  const objects = useGameStore((s) => s.gameState?.objects);
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const multiplayerBoardLayout = usePreferencesStore((s) => s.multiplayerBoardLayout);
  const localPlayerId = usePlayerId();

  const gameState = useGameStore((s) => s.gameState);
  const isMultiplayer = gameState != null && !isOneOnOne(gameState);
  const visiblePlayerIds = useMemo(
    () => new Set(getVisibleBoardPlayerIds(
      gameState,
      localPlayerId,
      focusedOpponent,
      multiplayerBoardLayout,
    )),
    [focusedOpponent, gameState, localPlayerId, multiplayerBoardLayout],
  );

  const pairs = useMergedPairs(blockerAssignments, combat?.blocker_to_attacker ?? null);
  const visibleBlockerPairs = useMemo(
    () => filterVisibleBlockerPairs(pairs, objects ?? null, visiblePlayerIds),
    [objects, pairs, visiblePlayerIds],
  );

  const positions = useRafPositions(visibleBlockerPairs);

  const isVisible =
    combatMode === "blockers" ||
    (combat !== null && Object.keys(combat.blocker_to_attacker).length > 0);

  // Compute HUD→attacker indicator arrows for off-screen blocking opponents
  const hudIndicators = useHudBlockIndicators(
    combat?.blocker_assignments ?? null,
    objects ?? null,
    localPlayerId,
    visiblePlayerIds,
    isMultiplayer,
  );

  const showCreatureArrows = isVisible && positions.size > 0;
  const showHudIndicators = hudIndicators.length > 0;

  if (!showCreatureArrows && !showHudIndicators) return null;

  const isMinimal = vfxQuality === "minimal";

  return createPortal(
    <svg className="pointer-events-none fixed inset-0 z-30 h-full w-full">
      <defs>
        {!isMinimal && (
          <filter id="block-line-glow">
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        )}
        <marker
          id="block-arrow-head"
          markerWidth="4"
          markerHeight="3.5"
          refX="4"
          refY="1.75"
          orient="auto"
        >
          <path d="M0,0 L4,1.75 L0,3.5 Z" fill={BLOCK_COLOR_HEAD} />
        </marker>
      </defs>
      {showCreatureArrows &&
        Array.from(positions.entries()).map(([blockerId, pos]) => {
          const d = arcPath(pos.from, pos.to);
          return (
            <g key={blockerId}>
              <path
                d={d}
                stroke="black"
                strokeWidth={isMinimal ? 3 : 5}
                fill="none"
                strokeLinecap="round"
                markerEnd="url(#block-arrow-head)"
              />
              <path
                d={d}
                stroke={BLOCK_COLOR}
                strokeWidth={isMinimal ? 1.5 : 2}
                fill="none"
                filter={isMinimal ? undefined : "url(#block-line-glow)"}
                markerEnd="url(#block-arrow-head)"
                strokeLinecap="round"
              />
            </g>
          );
        })}
      {showHudIndicators &&
        hudIndicators.map((ind) => {
          const d = arcPath(ind.from, ind.to);
          return (
            <g key={ind.key}>
              <path
                d={d}
                stroke="black"
                strokeWidth={isMinimal ? 3 : 5}
                fill="none"
                strokeLinecap="round"
                markerEnd="url(#block-arrow-head)"
              />
              <path
                d={d}
                stroke={BLOCK_COLOR}
                strokeWidth={isMinimal ? 1.5 : 2}
                fill="none"
                filter={isMinimal ? undefined : "url(#block-line-glow)"}
                markerEnd="url(#block-arrow-head)"
                strokeLinecap="round"
              />
            </g>
          );
        })}
    </svg>,
    document.body,
  );
}

interface HudIndicator {
  key: string;
  from: { x: number; y: number };
  to: { x: number; y: number };
}

function useHudBlockIndicators(
  blockerAssignments: Record<string, ObjectId[]> | null,
  objects: Record<string, { controller: PlayerId }> | null,
  localPlayerId: PlayerId,
  visiblePlayerIds: ReadonlySet<PlayerId>,
  isMultiplayer: boolean,
): HudIndicator[] {
  const [indicators, setIndicators] = useState<HudIndicator[]>([]);
  const stableCountRef = useRef(0);

  // Compute which attackers are blocked by off-screen opponents
  const offScreenBlockedAttackers = useMemo(() => {
    if (!isMultiplayer || !blockerAssignments || !objects) return [];
    const result: { attackerId: ObjectId; blockingPlayerId: PlayerId }[] = [];
    for (const [attackerId, blockerIds] of Object.entries(blockerAssignments)) {
      if (blockerIds.length === 0) continue;
      const blockerController = objects[String(blockerIds[0])]?.controller;
      if (blockerController == null) continue;
      if (blockerController === localPlayerId) continue;
      if (visiblePlayerIds.has(blockerController)) continue;
      result.push({ attackerId: Number(attackerId), blockingPlayerId: blockerController });
    }
    return result;
  }, [isMultiplayer, blockerAssignments, objects, localPlayerId, visiblePlayerIds]);

  const prevCountRef = useRef(0);

  useEffect(() => {
    if (offScreenBlockedAttackers.length === 0) {
      setIndicators([]);
      prevCountRef.current = 0;
      return;
    }
    stableCountRef.current = 0;
    let rafId: number;

    function poll() {
      const next: HudIndicator[] = [];

      for (const { attackerId, blockingPlayerId } of offScreenBlockedAttackers) {
        const hudEl = document.querySelector(`[data-player-hud="${blockingPlayerId}"]`);
        const attackerEl = document.querySelector(objectAnchorSelector(attackerId));
        if (!hudEl || !attackerEl) continue;
        const hudRect = hudEl.getBoundingClientRect();
        const attackerRect = attackerEl.getBoundingClientRect();
        next.push({
          key: `hud:${blockingPlayerId}->${attackerId}`,
          from: { x: hudRect.left + hudRect.width / 2, y: hudRect.top + hudRect.height / 2 },
          to: { x: attackerRect.left + attackerRect.width / 2, y: attackerRect.top + attackerRect.height / 2 },
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
  }, [offScreenBlockedAttackers]);

  return indicators;
}

function useMergedPairs(
  uiAssignments: Map<ObjectId, ObjectId>,
  engineAssignments: Record<string, ObjectId[]> | null,
): Map<ObjectId, ObjectId> {
  return useMemo(() => {
    const merged = new Map(uiAssignments);
    if (engineAssignments) {
      for (const [blockerId, attackerIds] of Object.entries(engineAssignments)) {
        if (attackerIds.length > 0) {
          merged.set(Number(blockerId), attackerIds[0]);
        }
      }
    }
    return merged;
  }, [uiAssignments, engineAssignments]);
}
