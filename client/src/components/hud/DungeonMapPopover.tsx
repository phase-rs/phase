import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import { useDungeonCardImage } from "../../hooks/useDungeonCardImage.ts";
import type { DungeonRoomView } from "../../adapter/types.ts";

interface Props {
  anchorEl: HTMLElement;
  /** Engine projection of the dungeon and where this player's marker sits.
   *  Every value rendered here is engine-authored (CR 309.4); this component
   *  positions and styles them and computes nothing about the game. */
  view: DungeonRoomView;
}

const ANCHOR_GAP_PX = 10;

/** Rendered width of the dungeon card inside the popover. The card scans are
 *  488x680 (`normal`) and 672x936 (`large`); 340 CSS px sits between the two,
 *  so the `large` rung the hook requests is downscaled slightly rather than
 *  upscaled — the room labels printed on these cards stay legible, which is the
 *  whole point of showing the card rather than a redrawn map. */
const CARD_WIDTH_PX = 340;
const CARD_ASPECT = 680 / 488;

/**
 * Hover/click panel for the dungeon HUD badge: the printed dungeon card with
 * the player's venture marker drawn on the room they currently occupy.
 *
 * Why the card image rather than a redrawn map — the five dungeon cards are the
 * unusual case where the art IS the game state. Each card is a labeled floor
 * plan whose rooms are drawn as rectangles with their names and effects printed
 * inside, so overlaying a marker on the real card shows a player exactly what
 * they would see across the table. `marker` positions come from the engine
 * (`DungeonRoomNodeView.marker`, permille of the card face).
 *
 * Mirrors `RingBenefitsPopover`: portaled to `document.body` (HudPlate's
 * `transform` would otherwise clip it) and auto-flipped above/below based on
 * the anchor's viewport half. Unlike that one this panel is NOT
 * `pointer-events-none` — it is hoverable so the pointer can travel from the
 * badge into the card without dismissing it.
 */
export function DungeonMapPopover({ anchorEl, view }: Props) {
  const { t } = useTranslation("game");
  const [pos, setPos] = useState<{
    left: number;
    top: number;
    placement: "above" | "below";
  } | null>(null);

  const { src, isLoading } = useDungeonCardImage(view.card);

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

  // CR 309.5a: the rooms reachable from where the marker stands. Engine-
  // provided edges; this only reads them into a set for styling.
  const reachable = useMemo(() => {
    const current = view.rooms.find((room) => room.index === view.room.index);
    return new Set(current?.next_rooms ?? []);
  }, [view.rooms, view.room.index]);

  if (!pos) return null;

  const transform =
    pos.placement === "above" ? "translate(-50%, -100%)" : "translate(-50%, 0)";
  // CR 309.4a: the marker starts on room index 0; players count from 1.
  const position = view.room.index + 1;

  return createPortal(
    <div
      className="fixed z-[130]"
      style={{ left: pos.left, top: pos.top, transform }}
      role="dialog"
      aria-label={t("badges.dungeonAriaLabel", {
        name: view.dungeon_name,
        roomName: view.room.name,
        room: position,
        total: view.room_count,
      })}
    >
      <div className="rounded-xl border border-violet-300/25 bg-slate-950/95 p-3 shadow-[0_18px_50px_rgba(0,0,0,0.6)] backdrop-blur-sm">
        <div className="mb-2 flex items-baseline justify-between gap-3">
          <span className="text-[11px] font-semibold uppercase tracking-[0.14em] text-violet-200">
            {view.dungeon_name}
          </span>
          <span className="text-[11px] tabular-nums text-slate-400">
            {t("badges.dungeonRoomTooltip", {
              roomName: view.room.name,
              room: position,
              total: view.room_count,
            })}
          </span>
        </div>

        <div
          className="relative overflow-hidden rounded-lg bg-slate-900"
          style={{ width: CARD_WIDTH_PX, height: Math.round(CARD_WIDTH_PX * CARD_ASPECT) }}
        >
          {src ? (
            <img
              src={src}
              alt={view.dungeon_name}
              className="absolute inset-0 h-full w-full object-contain"
              draggable={false}
            />
          ) : (
            <div className="absolute inset-0 grid place-items-center px-4 text-center text-[11px] text-slate-500">
              {isLoading ? t("badges.dungeonMapLoading") : t("badges.dungeonMapUnavailable")}
            </div>
          )}

          {/* The marker layer. Drawn even when the card image is missing so the
              room list below still reads as a map; without the art it simply
              floats on the placeholder. */}
          {view.rooms.map((room) => {
            const isCurrent = room.index === view.room.index;
            const isReachable = reachable.has(room.index);
            if (!isCurrent && !isReachable) return null;
            return (
              <span
                key={room.index}
                aria-hidden
                title={room.name}
                className={
                  isCurrent
                    ? "absolute h-4 w-4 -translate-x-1/2 -translate-y-1/2 rounded-full border-2 border-white bg-violet-500 shadow-[0_0_0_3px_rgba(139,92,246,0.45),0_0_14px_rgba(139,92,246,0.9)]"
                    : "absolute h-2.5 w-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full border border-white/70 bg-white/25"
                }
                style={{
                  left: `${room.marker.x_permille / 10}%`,
                  top: `${room.marker.y_permille / 10}%`,
                }}
              />
            );
          })}
        </div>

        {view.room.text ? (
          <p className="mt-2 max-w-[340px] text-[11px] leading-snug text-slate-300">
            {view.room.text}
          </p>
        ) : null}
      </div>
    </div>,
    document.body,
  );
}
