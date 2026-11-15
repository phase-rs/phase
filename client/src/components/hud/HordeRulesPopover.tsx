import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

import type { RuleSummaryLine } from "../../adapter/types.ts";

interface Props {
  anchorEl: HTMLElement;
  /** Engine-provided deck label, e.g. "Cyberman Horde". */
  deckLabel: string;
  /**
   * Engine-authored rules summary lines (label + detail), straight from
   * `ChallengeDeckMetadata.rules`. Rendered verbatim.
   */
  rules: readonly RuleSummaryLine[];
}

const ANCHOR_GAP_PX = 10;

/**
 * Passive hover popover listing a Horde challenge deck's engine-authored rules
 * summary — how the deck plays and how it differs (waves, survivor life, setup,
 * Horde combat). The label/detail lines come straight from the engine registry
 * (`ChallengeDeckMetadata.rules`, the same text the setup deck picker shows), so
 * the frontend renders them verbatim and never derives rules prose from the
 * structured `HordeRuleset`.
 *
 * Mirrors `RingBenefitsPopover`: `pointer-events-none`, portaled to
 * `document.body` (a seat plate's `transform` would otherwise clip it), and
 * auto-flipped above/below based on the anchor's viewport half.
 */
export function HordeRulesPopover({ anchorEl, deckLabel, rules }: Props) {
  const [pos, setPos] = useState<{
    left: number;
    top: number;
    placement: "above" | "below";
  } | null>(null);

  useEffect(() => {
    function recompute() {
      const rect = anchorEl.getBoundingClientRect();
      const placement: "above" | "below" =
        rect.top < window.innerHeight / 2 ? "below" : "above";
      const left = rect.left + rect.width / 2;
      const top = placement === "above" ? rect.top - ANCHOR_GAP_PX : rect.bottom + ANCHOR_GAP_PX;
      setPos({ left, top, placement });
    }
    recompute();
    window.addEventListener("resize", recompute);
    window.addEventListener("scroll", recompute, true);
    return () => {
      window.removeEventListener("resize", recompute);
      window.removeEventListener("scroll", recompute, true);
    };
  }, [anchorEl]);

  if (!pos) return null;

  const transform =
    pos.placement === "above" ? "translate(-50%, -100%)" : "translate(-50%, 0)";

  return createPortal(
    <div
      className="pointer-events-none fixed z-[130]"
      style={{ left: pos.left, top: pos.top, transform }}
      aria-hidden
    >
      <div className="w-72 rounded-2xl border border-amber-300/40 bg-slate-950/95 p-3 text-left shadow-[0_18px_36px_rgba(0,0,0,0.55)] backdrop-blur-md">
        <div className="mb-2 text-[12px] font-bold tracking-wide text-amber-200">
          {deckLabel}
        </div>
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
          {rules.map((rule) => (
            <div key={rule.label} className="contents">
              <dt className="text-[10px] font-semibold uppercase tracking-wide text-amber-200/70">
                {rule.label}
              </dt>
              <dd className="text-[11px] leading-snug text-slate-200">{rule.detail}</dd>
            </div>
          ))}
        </dl>
      </div>
    </div>,
    document.body,
  );
}
