import type { CSSProperties } from "react";

import type { ObjectId, PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { partitionByType } from "../../viewmodel/battlefieldProps.ts";
import { tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { CardImage } from "../card/CardImage.tsx";

// Hard cap on how many mini-cards the peek will render. At 88×123 with an
// 8px gap, ~12 fits a viewport-friendly 3-row grid without dominating the
// screen on a 1080p display or smaller. Legal targets are pinned in front
// of the cap during targeting, so a token swarm never hides the cards the
// targeter actually needs to see.
const MAX_VISIBLE_CARDS = 12;

interface BattlefieldPeekPopoverProps {
  playerId: PlayerId;
  opponentName: string;
  /** Per-seat identity color from `getSeatColor`. Used as the popover's
   *  border + outer glow so the reader can match this floating panel to
   *  the tab it anchors to without parsing the text label. */
  seatColor: string;
  /** When true, render in targeting mode: legal-target cards get a bright
   *  cyan ring, non-legal cards are visible but muted. When false, all
   *  permanents are shown with a neutral hairline ring (pure scout view). */
  isTargeting: boolean;
  /** Object ids legal to target right now that are controlled by this
   *  opponent. Drives the per-card ring color in targeting mode. Ignored
   *  in idle mode. */
  legalTargetIds: readonly ObjectId[];
}

/** Hover popover surfaced from an `OpponentTab` whenever the local player
 *  hovers an unfocused opponent's tab. Shows mini card images for each of
 *  that opponent's creatures, planeswalkers, and battles so the reader
 *  can scout the board without committing to a focus switch.
 *
 *  Color hierarchy: the **border** carries identity (seat color, matches
 *  the tab's avatar tile), and **per-card rings** carry semantics — cyan
 *  during targeting for legal targets, muted otherwise. This separation
 *  keeps the popover visually anchored to its source tab regardless of
 *  game mode while still surfacing what's targetable.
 *
 *  `pointer-events-none` prevents events from fighting the tab's hover
 *  state — the wrapping `OpponentTab` owns the show/hide lifecycle.
 *  `aria-hidden` keeps screen readers from concatenating board detail
 *  into the tab's accessible name. */
export function BattlefieldPeekPopover({
  playerId,
  opponentName,
  seatColor,
  isTargeting,
  legalTargetIds,
}: BattlefieldPeekPopoverProps) {
  const battlefield = useGameStore((s) => s.gameState?.battlefield);
  const objects = useGameStore((s) => s.gameState?.objects);
  if (!battlefield || !objects) return null;

  const owned = battlefield
    .map((id) => objects[id])
    .filter((obj): obj is NonNullable<typeof obj> => obj != null && obj.controller === playerId);

  const partition = partitionByType(owned);
  // Show all non-land permanents that aren't already rendered through a
  // host (attached Auras / Equipment — `partitionByType` already drops
  // those). Lands are excluded: too many, low scouting value. Order is
  // creatures → planeswalkers → support (artifacts/enchantments) →
  // other (Battles, oddities), reflecting decreasing threat weight.
  const candidates: ObjectId[] = [
    ...partition.creatures,
    ...partition.planeswalkers,
    ...partition.support,
    ...partition.other,
  ];

  const legalSet = new Set(legalTargetIds);
  // Sort legal targets to the front during targeting so the cap can never
  // hide a card the player needs to see. In idle mode the order from
  // `partitionByType` (creatures → planeswalkers → support) is preserved
  // since there's no "more important" subset to surface.
  const sorted: ObjectId[] = isTargeting
    ? [...candidates].sort((a, b) => Number(legalSet.has(b)) - Number(legalSet.has(a)))
    : candidates;
  const visible = sorted.slice(0, MAX_VISIBLE_CARDS);
  const overflowCount = Math.max(0, sorted.length - visible.length);
  // Seat-color border + glow. Alpha-suffixed hex values mirror the tab's
  // avatar-tile style so popover and tile read as the same identity.
  const containerStyle: CSSProperties = {
    borderColor: `${seatColor}cc`,
    boxShadow: `0 0 0 1px ${seatColor}55, 0 20px 40px rgba(0,0,0,0.55), 0 0 22px ${seatColor}3a`,
  };

  if (visible.length === 0) {
    return (
      <div
        aria-hidden
        style={containerStyle}
        className="pointer-events-none rounded-lg border bg-slate-950/95 px-2.5 py-2 backdrop-blur-xl"
      >
        <div
          className="whitespace-nowrap text-center text-[10px] font-semibold uppercase tracking-[0.16em]"
          style={{ color: seatColor }}
        >
          {opponentName}'s board
        </div>
        <div className="mt-1 whitespace-nowrap text-center text-[10px] italic text-slate-400">
          No nonland permanents
        </div>
      </div>
    );
  }

  return (
    <div
      aria-hidden
      style={containerStyle}
      className="pointer-events-none max-w-[calc(100vw-1rem)] rounded-lg border bg-slate-950/95 px-3 py-2.5 backdrop-blur-xl"
    >
      <div
        className="mb-2 whitespace-nowrap text-center text-[10px] font-semibold uppercase tracking-[0.18em]"
        style={{ color: seatColor }}
      >
        {opponentName}'s board
      </div>
      <div className="flex flex-wrap justify-center gap-2">
        {visible.map((id) => {
          const obj = objects[id];
          if (!obj) return null;
          const pt = obj.power != null && obj.toughness != null
            ? `${obj.power}/${obj.toughness}`
            : null;
          const isLegal = isTargeting && legalSet.has(id);
          const ringClass = isTargeting
            ? isLegal
              ? "ring-2 ring-cyan-300/80 shadow-[0_0_12px_rgba(34,211,238,0.5)]"
              : "ring-1 ring-white/10 opacity-60"
            : "ring-1 ring-white/15";
          return (
            <div key={id} className="flex w-[88px] flex-col items-center gap-1">
              <div
                className={`overflow-hidden rounded ${ringClass}`}
                style={{ width: 88, height: 123 }}
              >
                <CardImage
                  cardName={obj.name}
                  size="small"
                  isToken={obj.display_source === "Token"}
                  tokenFilters={obj.display_source === "Token" ? tokenFiltersForObject(obj) : undefined}
                  tokenImageRef={obj.token_image_ref}
                />
              </div>
              {pt && (
                <div className="rounded bg-black/80 px-1.5 text-[10px] font-bold text-white">
                  {pt}
                </div>
              )}
            </div>
          );
        })}
      </div>
      {overflowCount > 0 && (
        <div
          className="mt-2 text-center text-[10px] font-medium italic text-slate-400"
          style={{ color: `${seatColor}aa` }}
        >
          +{overflowCount} more permanent{overflowCount === 1 ? "" : "s"}
          {isTargeting ? " (not targetable)" : ""}
        </div>
      )}
    </div>
  );
}
