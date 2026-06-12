import { useEffect, useRef, useState } from "react";

import type { ObjectId } from "../adapter/types.ts";
import { objectAnchorSelector } from "../utils/objectAnchorSelector.ts";

export interface LinePosition {
  from: { x: number; y: number };
  to: { x: number; y: number };
  length: number;
}

/** RAF polling for element positions — stabilizes after 10 unchanged frames.
 *  Pairs map: key → value, where both are ObjectIds with `data-object-id` attributes. */
export function useRafPositions(pairs: Map<ObjectId, ObjectId>): Map<ObjectId, LinePosition> {
  const [positions, setPositions] = useState<Map<ObjectId, LinePosition>>(new Map());
  const prevRectsRef = useRef<Map<string, DOMRect>>(new Map());
  const stableCountRef = useRef(0);

  useEffect(() => {
    if (pairs.size === 0) {
      setPositions(new Map());
      return;
    }

    stableCountRef.current = 0;
    prevRectsRef.current = new Map();
    let rafId: number;

    function poll() {
      const currentRects = new Map<string, DOMRect>();
      let changed = false;

      for (const [fromId, toId] of pairs) {
        for (const id of [fromId, toId]) {
          const key = String(id);
          if (currentRects.has(key)) continue;
          const el = document.querySelector(objectAnchorSelector(id));
          if (!el) continue;
          const rect = el.getBoundingClientRect();
          currentRects.set(key, rect);
          const prev = prevRectsRef.current.get(key);
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

      const next = new Map<ObjectId, LinePosition>();
      for (const [fromId, toId] of pairs) {
        const fromRect = currentRects.get(String(fromId));
        const toRect = currentRects.get(String(toId));
        if (!fromRect || !toRect) continue;
        const from = {
          x: fromRect.left + fromRect.width / 2,
          y: fromRect.top + fromRect.height / 2,
        };
        const to = {
          x: toRect.left + toRect.width / 2,
          y: toRect.top + toRect.height / 2,
        };
        const dx = to.x - from.x;
        const dy = to.y - from.y;
        next.set(fromId, { from, to, length: Math.sqrt(dx * dx + dy * dy) });
      }
      setPositions(next);

      // Stop polling after 10 stable frames
      if (stableCountRef.current < 10) {
        rafId = requestAnimationFrame(poll);
      }
    }

    rafId = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(rafId);
  }, [pairs]);

  return positions;
}
