import { useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { getArcPath } from "../targeting/arcPath.ts";
import { getStackEntryTargets } from "./stackTargets.ts";
import type { ObjectId, StackEntry } from "../../adapter/types.ts";

interface StackTargetArcsProps {
  stack: StackEntry[];
  activeEntryId: ObjectId | null;
  isCollapsed: boolean;
}

// ── Types ────────────────────────────────────────────────────────────────

interface ArcDatum {
  key: string;
  stackEntryId: ObjectId;
  sourceSelector: string;
  targetSelector: string;
}

interface ArcPosition {
  from: { x: number; y: number };
  to: { x: number; y: number };
}

// ── Colors ───────────────────────────────────────────────────────────────

const GOLD = "#C9B037";
const MUTED = "#9CA3AF";

// ── Component ────────────────────────────────────────────────────────────

export function StackTargetArcs({ stack, activeEntryId, isCollapsed }: StackTargetArcsProps) {
  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const isMinimal = vfxQuality === "minimal";

  const uid = useId();
  const glowId = `stack-arc-glow-${uid}`;
  const arrowActiveId = `stack-arrow-active-${uid}`;
  const arrowInactiveId = `stack-arrow-inactive-${uid}`;

  // Build arc data for all stack entries that have targets
  const arcData = useMemo(() => {
    const result: ArcDatum[] = [];
    for (const entry of stack) {
      const targets = getStackEntryTargets(entry);
      for (let i = 0; i < targets.length; i++) {
        const target = targets[i];
        let targetSelector: string;
        if ("Object" in target) {
          targetSelector = `[data-object-id="${target.Object}"]`;
        } else {
          targetSelector = `[data-player-hud="${target.Player}"]`;
        }
        result.push({
          key: `${entry.id}-${i}`,
          stackEntryId: entry.id,
          sourceSelector: `[data-stack-entry="${entry.id}"]`,
          targetSelector,
        });
      }
    }
    return result;
  }, [stack]);

  const positions = useStackArcPositions(arcData);

  if (isCollapsed || arcData.length === 0) return null;

  return createPortal(
    <svg
      className="pointer-events-none fixed inset-0"
      style={{ zIndex: 35 }}
      width="100%"
      height="100%"
    >
      <defs>
        {!isMinimal && (
          <filter id={glowId}>
            <feGaussianBlur stdDeviation="3" result="blur" />
            <feMerge>
              <feMergeNode in="blur" />
              <feMergeNode in="SourceGraphic" />
            </feMerge>
          </filter>
        )}
        <marker
          id={arrowActiveId}
          markerWidth="8"
          markerHeight="6"
          refX="8"
          refY="3"
          orient="auto"
        >
          <path d="M0,0 L8,3 L0,6 Z" fill={GOLD} />
        </marker>
        <marker
          id={arrowInactiveId}
          markerWidth="8"
          markerHeight="6"
          refX="8"
          refY="3"
          orient="auto"
        >
          <path d="M0,0 L8,3 L0,6 Z" fill={MUTED} opacity={0.5} />
        </marker>
      </defs>
      {arcData.map((arc) => {
        const pos = positions.get(arc.key);
        if (!pos) return null;
        const isActive = arc.stackEntryId === activeEntryId;
        const d = getArcPath(pos.from, pos.to);
        return (
          <path
            key={arc.key}
            d={d}
            stroke={isActive ? GOLD : MUTED}
            strokeWidth={isActive ? 2.5 : 1.5}
            fill="none"
            opacity={isActive ? 1 : 0.25}
            filter={isActive && !isMinimal ? `url(#${glowId})` : undefined}
            markerEnd={`url(#${isActive ? arrowActiveId : arrowInactiveId})`}
          />
        );
      })}
    </svg>,
    document.body,
  );
}

// ── RAF position polling (specialized for heterogeneous selectors) ───────

function useStackArcPositions(arcs: ArcDatum[]): Map<string, ArcPosition> {
  const [positions, setPositions] = useState<Map<string, ArcPosition>>(new Map());
  const prevRectsRef = useRef<Map<string, DOMRect>>(new Map());
  const stableCountRef = useRef(0);

  useEffect(() => {
    if (arcs.length === 0) {
      setPositions(new Map());
      return;
    }

    stableCountRef.current = 0;
    prevRectsRef.current = new Map();
    let rafId: number;

    function poll() {
      const currentRects = new Map<string, DOMRect>();
      let changed = false;

      // Collect all unique selectors and their rects
      for (const arc of arcs) {
        for (const selector of [arc.sourceSelector, arc.targetSelector]) {
          if (currentRects.has(selector)) continue;
          const el = document.querySelector(selector);
          if (!el) continue;
          const rect = el.getBoundingClientRect();
          currentRects.set(selector, rect);
          const prev = prevRectsRef.current.get(selector);
          if (
            !prev ||
            Math.abs(prev.left - rect.left) > 0.5 ||
            Math.abs(prev.top - rect.top) > 0.5 ||
            Math.abs(prev.width - rect.width) > 0.5
          ) {
            changed = true;
          }
        }
      }

      if (changed) {
        stableCountRef.current = 0;
      } else {
        stableCountRef.current++;
      }

      prevRectsRef.current = currentRects;

      const next = new Map<string, ArcPosition>();
      for (const arc of arcs) {
        const sourceRect = currentRects.get(arc.sourceSelector);
        const targetRect = currentRects.get(arc.targetSelector);
        if (!sourceRect || !targetRect) continue;
        next.set(arc.key, {
          from: {
            x: sourceRect.left + sourceRect.width / 2,
            y: sourceRect.top + sourceRect.height / 2,
          },
          to: {
            x: targetRect.left + targetRect.width / 2,
            y: targetRect.top + targetRect.height / 2,
          },
        });
      }
      setPositions(next);

      // Stop polling after 10 stable frames
      if (stableCountRef.current < 10) {
        rafId = requestAnimationFrame(poll);
      }
    }

    rafId = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(rafId);
  }, [arcs]);

  return positions;
}
