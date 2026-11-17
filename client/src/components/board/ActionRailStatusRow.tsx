import { useLayoutEffect, useState } from "react";

import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { FullControlToggle } from "../controls/FullControlToggle.tsx";
import { MobilePhaseChip } from "../controls/MobilePhaseChip.tsx";
import { HandBadge } from "../hand/HandBadge.tsx";
import { DayNightBadge } from "../hud/HudBadges.tsx";
import { TurnStatusLine } from "../hud/TurnStatusLine.tsx";
import { MayTriggerAutoChoiceList } from "./MayTriggerAutoChoiceList.tsx";
import { PriorityYieldList } from "./PriorityYieldList.tsx";

/** Space kept between the HUD cluster and the row: the row's own chip gap
 *  (`gap-1.5`). */
const HUD_CLEARANCE_GAP_PX = 6;

interface ActionRailStatusRowProps {
  /** The local player's HUD root (`PlayerHud`'s `ref`), or null until it mounts. */
  playerHud: HTMLElement | null;
}

/**
 * The action rail's status row in landscape and on desktop: phase, priority,
 * hand, day/night, and the standing yield and auto-choice summaries beside Full
 * Control. Portrait docks the same chips on the rail's left instead.
 *
 * On short landscape screens the rail and the player HUD share the bottom edge,
 * and a row with no bound grew left over Undo, Manual and the life total. There
 * the chips go compact, the row is bounded to the room right of the HUD cluster,
 * and it wraps upward (`flex-wrap-reverse`) instead of sliding left. The rail and
 * the HUD are both draggable Flex Layout widgets, so the bound is measured from
 * where they are on screen, not from their docked homes.
 */
export function ActionRailStatusRow({ playerHud }: ActionRailStatusRowProps) {
  const isMobile = useIsMobile();
  const isCompactHeight = useIsCompactHeight();
  // The row only displays in landscape below `lg` (portrait docks these chips
  // on the left), so here a compact height below `lg` is a short landscape
  // screen. Desktop keeps the full row.
  const compact = isMobile && isCompactHeight;
  const [row, setRow] = useState<HTMLDivElement | null>(null);
  const maxWidth = useHudClearanceWidth(row, playerHud, compact);

  return (
    <div
      ref={setRow}
      data-testid="action-rail-status-row"
      className={`hidden flex-row items-center gap-1.5 max-lg:landscape:flex lg:flex ${compact ? "flex-wrap-reverse justify-end" : ""}`}
      style={maxWidth === undefined ? undefined : { maxWidth }}
    >
      {/* <lg only: desktop conveys phase via the PhaseDot strips in
          PlayerHud, which are hidden on mobile. */}
      <MobilePhaseChip className="lg:hidden" compact={compact} />
      <TurnStatusLine compact={compact} />
      <HandBadge compact={compact} />
      <DayNightBadge />
      {/* CR 117.3d: standing priority-yield summary chip, beside the
          Full Control toggle (self-hides when no yields stand). */}
      <PriorityYieldList />
      {/* CR 603.5: standing "don't ask again" auto-choice summary chip,
          beside the priority-yield chip (self-hides when none stand). */}
      <MayTriggerAutoChoiceList />
      <FullControlToggle />
    </div>
  );
}

interface HudClusterBounds {
  top: number;
  bottom: number;
  right: number;
}

/**
 * The widest the row may lay out, in its own unscaled pixels, and still end
 * right of the HUD cluster. Undefined while the row is free to take its natural
 * width: disabled, not mounted, or not level with the cluster.
 *
 * Remeasures on every size change and on every drag of either widget, so the
 * bound follows a moved rail or plate.
 */
function useHudClearanceWidth(
  row: HTMLElement | null,
  playerHud: HTMLElement | null,
  enabled: boolean,
): number | undefined {
  const railScale = usePreferencesStore((s) => s.flexLayout.scales?.actionRail ?? 1);
  const [maxWidth, setMaxWidth] = useState<number>();

  useLayoutEffect(() => {
    if (!enabled || !row || !playerHud) return;

    const sideControls = playerHud.querySelector<HTMLElement>("[data-player-hud-side-controls]");
    const measure = () => {
      const cluster = measureHudCluster(playerHud, sideControls);
      setMaxWidth(hudClearanceWidth(row.getBoundingClientRect(), cluster, railScale));
    };
    measure();

    // Size changes: chips coming and going, the plate's badges, Manual and
    // Undo appearing, the rail's action buttons (they lift the row), and the
    // board resizing around the centered HUD.
    const rail = row.closest<HTMLElement>("[data-flex-zone]");
    const hudWidget = playerHud.closest<HTMLElement>("[data-flex-zone]");
    const hudContainer = playerHud.closest("[data-player-hud-anchor]")?.parentElement;
    const resizeObserver = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    for (const element of [row, rail, playerHud, sideControls, hudContainer]) {
      if (element) resizeObserver?.observe(element);
    }
    // Moves: DraggableWidget applies drag offsets and box scale as inline
    // transforms, which move a widget without any resize or scroll event.
    const styleObserver = new MutationObserver(measure);
    for (const widget of [rail, hudWidget]) {
      if (widget) styleObserver.observe(widget, { attributes: true, attributeFilter: ["style"] });
    }
    window.addEventListener("resize", measure);

    return () => {
      resizeObserver?.disconnect();
      styleObserver.disconnect();
      window.removeEventListener("resize", measure);
      setMaxWidth(undefined);
    };
  }, [enabled, row, playerHud, railScale]);

  return maxWidth;
}

/** The plate plus the Manual / Undo column, which hangs off the plate's right
 *  edge outside the HUD root's own box. */
function measureHudCluster(
  playerHud: HTMLElement,
  sideControls: HTMLElement | null,
): HudClusterBounds {
  const plate = playerHud.getBoundingClientRect();
  if (!sideControls) return plate;
  const controls = sideControls.getBoundingClientRect();
  return {
    top: Math.min(plate.top, controls.top),
    bottom: Math.max(plate.bottom, controls.bottom),
    right: Math.max(plate.right, controls.right),
  };
}

function hudClearanceWidth(
  row: DOMRect,
  cluster: HudClusterBounds,
  railScale: number,
): number | undefined {
  // Only a cluster level with the row can be overlapped: a plate or rail
  // dragged apart vertically leaves the row free to grow.
  if (cluster.bottom <= row.top || cluster.top >= row.bottom) return undefined;
  // The row is right-aligned in the rail, so its right edge stays put however
  // wide it gets. A cluster reaching past that edge leaves no room to its
  // right, and the row keeps its natural width.
  const clearance = row.right - cluster.right - HUD_CLEARANCE_GAP_PX;
  if (clearance <= 0) return undefined;
  // The rail's Flex Layout box scale scales the row too, so convert screen
  // pixels back to the row's own. Whole pixels keep remeasures from churning.
  return Math.floor(clearance / railScale);
}
