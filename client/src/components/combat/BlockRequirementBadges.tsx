import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type { ObjectId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useBlockRequirements, type BlockRequirement } from "./useBlockRequirements.ts";

interface Anchor {
  x: number;
  top: number;
}

/**
 * RAF-polls the top-center of each attacker card (`data-object-id`) so a badge
 * can float above it. Mirrors the settle-after-10-stable-frames approach used by
 * the blocker arrows so it tracks layout shifts (tap rotations, reflow) without
 * polling forever.
 */
function useAttackerAnchors(attackerIds: ObjectId[]): Map<ObjectId, Anchor> {
  const [anchors, setAnchors] = useState<Map<ObjectId, Anchor>>(new Map());
  const stableCountRef = useRef(0);
  // Identity-stable key so the effect only restarts when the set changes.
  const key = attackerIds.slice().sort((a, b) => a - b).join(",");

  useEffect(() => {
    if (attackerIds.length === 0) {
      setAnchors(new Map());
      return;
    }
    stableCountRef.current = 0;
    let rafId: number;
    let prev = "";

    function poll() {
      const next = new Map<ObjectId, Anchor>();
      for (const id of attackerIds) {
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

function badgeTone(status: BlockRequirement["status"]): string {
  switch (status) {
    case "satisfied":
      return "border-emerald-300/60 bg-emerald-950/85 text-emerald-100";
    case "incomplete":
      return "border-amber-300/70 bg-amber-950/90 text-amber-100 animate-pulse";
    case "pending":
      return "border-rose-300/50 bg-rose-950/85 text-rose-100";
  }
}

/**
 * Floating "needs N blockers" badge over each attacker with a minimum-blocker
 * requirement (menace / "blocked by N or more"). Renders only while the local
 * player is assigning blockers. Pure display of engine-provided requirements vs
 * the player's in-progress assignments (see `useBlockRequirements`).
 */
export function BlockRequirementBadges() {
  const { t } = useTranslation("game");
  const { byAttacker } = useBlockRequirements();
  const objects = useGameStore((s) => s.gameState?.objects);
  const attackerIds = Array.from(byAttacker.keys());
  const anchors = useAttackerAnchors(attackerIds);

  if (byAttacker.size === 0) return null;

  return createPortal(
    <div className="pointer-events-none fixed inset-0 z-40">
      {Array.from(byAttacker.values()).map((req) => {
        const anchor = anchors.get(req.attackerId);
        if (!anchor) return null;
        const label =
          req.status === "satisfied"
            ? t("combat.blockSatisfiedBadge", { required: req.required })
            : req.status === "incomplete"
              ? t("combat.blockProgressBadge", { assigned: req.assigned, required: req.required })
              : t("combat.blockNeedsBadge", { required: req.required });
        const baseTitle =
          req.status === "incomplete"
            ? t("combat.blockIncompleteAttacker", { assigned: req.assigned, required: req.required })
            : t("combat.menaceRequirement", { count: req.required });
        // Display-only source attribution: suppress the self-source (Menace's
        // carrier is the attacker itself → bare badge) and skip ids that no
        // longer resolve (departed-source guard), mirroring AttackRequirementBadges.
        const names = req.sources
          .filter((id) => id !== req.attackerId)
          .map((id) => objects?.[String(id)]?.name)
          .filter((n): n is string => !!n);
        const title = names.length
          ? `${baseTitle} ${t("preview.fromSource", { source: names.join(", ") })}`
          : baseTitle;
        return (
          <div
            key={req.attackerId}
            className="absolute -translate-x-1/2 -translate-y-[120%]"
            style={{ left: anchor.x, top: anchor.top }}
          >
            <span
              title={title}
              className={`flex items-center gap-1 whitespace-nowrap rounded-full border px-2 py-0.5 text-[11px] font-bold tabular-nums shadow-[0_4px_12px_rgba(0,0,0,0.5)] backdrop-blur-sm ${badgeTone(req.status)}`}
            >
              {/* Two-pronged "menace" glyph — a single attacker needing multiple blockers. */}
              <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} className="h-3 w-3">
                <path strokeLinecap="round" strokeLinejoin="round" d="M3 2v4l2 2m6-6v4l-2 2M8 8v6m-3 0h6" />
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
