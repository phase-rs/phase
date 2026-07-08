import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

interface Props {
  anchorEl: HTMLElement;
  /** Current ring level (0–4); levels ≤ this are active/unlocked. */
  level: number;
  bearerName: string | null;
}

const ANCHOR_GAP_PX = 10;
const RING_LEVELS = [1, 2, 3, 4] as const;

/**
 * Passive hover popover for the player's OWN Ring: lists the four CR 701.54
 * level abilities and highlights those active at the current `level`. The
 * ability TEXT comes from the existing `badges.ringLevelN` i18n strings (the
 * project's convention for fixed rules text); the only state-driven decision
 * is the active/inactive split — `lv <= level` — which is formatting, not game
 * logic (`ring_level` is engine-provided).
 *
 * Mirrors `AurasHoverPreview`: `pointer-events-none`, portaled to
 * `document.body` (HudPlate's `transform` would otherwise clip it), and
 * auto-flipped above/below based on the anchor's viewport half.
 */
export function RingBenefitsPopover({ anchorEl, level, bearerName }: Props) {
  const { t } = useTranslation("game");
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
        <div className="mb-1 text-[12px] font-bold tracking-wide text-amber-200">
          {t("badges.ringTooltip", { level })}
        </div>
        <div className="mb-2 text-[11px] text-slate-300">
          {bearerName
            ? t("badges.ringBearerTooltip", { name: bearerName })
            : t("badges.noRingBearerTooltip")}
        </div>
        <ul className="flex flex-col gap-1">
          {RING_LEVELS.map((lv) => {
            const active = lv <= level;
            return (
              <li
                key={lv}
                className={`flex gap-1.5 text-[11px] leading-snug ${
                  active ? "text-amber-100" : "text-slate-500"
                }`}
              >
                <span aria-hidden className={active ? "text-amber-300" : "text-slate-600"}>
                  {active ? "◆" : "◇"}
                </span>
                <span>{t(`badges.ringLevel${lv}`)}</span>
              </li>
            );
          })}
        </ul>
      </div>
    </div>,
    document.body,
  );
}
