import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type { ObjectId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useBlockerConstraints, type BlockerConstraint } from "./useBlockerConstraints.ts";

interface Anchor {
  x: number;
  top: number;
}

/**
 * RAF-polls the top-center of each creature card (`data-object-id`) so a badge
 * can float above it. Mirrors `BlockRequirementBadges` — settles after 10 stable
 * frames so it tracks layout shifts without polling forever.
 */
function useObjectAnchors(objectIds: ObjectId[]): Map<ObjectId, Anchor> {
  const [anchors, setAnchors] = useState<Map<ObjectId, Anchor>>(new Map());
  const stableCountRef = useRef(0);
  const key = objectIds.slice().sort((a, b) => a - b).join(",");

  useEffect(() => {
    if (objectIds.length === 0) {
      setAnchors(new Map());
      return;
    }
    stableCountRef.current = 0;
    let rafId: number;
    let prev = "";

    function poll() {
      const next = new Map<ObjectId, Anchor>();
      for (const id of objectIds) {
        const el = document.querySelector(`[data-object-id="${id}"]`);
        if (!el) continue;
        const rect = el.getBoundingClientRect();
        next.set(id, { x: rect.left + rect.width / 2, top: rect.top });
      }
      const signature = Array.from(next.entries())
        .map(([id, a]) => `${id}:${Math.round(a.x)}:${Math.round(a.top)}`)
        .join("|");
      stableCountRef.current = signature === prev ? stableCountRef.current + 1 : 0;
      prev = signature;
      setAnchors(next);
      if (stableCountRef.current < 10) {
        rafId = requestAnimationFrame(poll);
      }
    }

    rafId = requestAnimationFrame(poll);
    return () => cancelAnimationFrame(rafId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return anchors;
}

function badgeTone(status: BlockerConstraint["status"]): string {
  switch (status) {
    case "satisfied":
      return "border-emerald-300/60 bg-emerald-950/85 text-emerald-100";
    case "pending":
      return "border-rose-300/50 bg-rose-950/85 text-rose-100 animate-pulse";
    case "info":
      return "border-slate-300/40 bg-slate-900/85 text-slate-200";
  }
}

/**
 * Floating must-block / can't-block badge over each of the defending player's
 * creatures carrying an engine-provided blocker constraint (CR 509.1b/c).
 * Renders only while the local player is declaring blockers. Pure display of
 * engine-provided constraints vs the player's in-progress assignments
 * (see `useBlockerConstraints`).
 */
export function BlockerConstraintBadges() {
  const { t } = useTranslation("game");
  const { byObject } = useBlockerConstraints();
  const objects = useGameStore((s) => s.gameState?.objects);
  const objectIds = Array.from(byObject.keys());
  const anchors = useObjectAnchors(objectIds);

  if (byObject.size === 0) return null;

  return createPortal(
    <div className="pointer-events-none fixed inset-0 z-40">
      {Array.from(byObject.values()).map((req) => {
        const anchor = anchors.get(req.objectId);
        if (!anchor) return null;
        const label =
          req.kind === "CantBlock"
            ? t("combat.cantBlockBadge")
            : req.status === "satisfied"
              ? t("combat.mustBlockSatisfiedBadge")
              : t("combat.mustBlockBadge");
        // Display-only source attribution: suppress a self-source (intrinsic
        // requirement shows a bare badge) and skip ids that no longer resolve
        // (departed-source guard), mirroring PermanentCard.
        const names = req.sources
          .filter((id) => id !== req.objectId)
          .map((id) => objects?.[String(id)]?.name)
          .filter((n): n is string => !!n);
        const title = names.length
          ? `${label} ${t("preview.fromSource", { source: names.join(", ") })}`
          : label;
        return (
          <div
            key={req.objectId}
            className="absolute -translate-x-1/2 -translate-y-[120%]"
            style={{ left: anchor.x, top: anchor.top }}
          >
            <span
              title={title}
              className={`flex items-center gap-1 whitespace-nowrap rounded-full border px-2 py-0.5 text-[11px] font-bold shadow-[0_4px_12px_rgba(0,0,0,0.5)] backdrop-blur-sm ${badgeTone(req.status)}`}
            >
              {/* Shield glyph — a creature that must / can't block. */}
              <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} className="h-3 w-3">
                <path strokeLinecap="round" strokeLinejoin="round" d="M8 2 3 4v4c0 3 2 5 5 6 3-1 5-3 5-6V4L8 2Z" />
              </svg>
              {label}
            </span>
          </div>
        );
      })}
    </div>,
    document.body,
  );
}
