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
  // Below `lg` the row only displays in landscape (portrait docks these chips
  // on the left), so there a compact height is a short landscape screen.
  // Desktop keeps the full row, however short the window.
  const compact = isMobile && isCompactHeight;
  const [row, setRow] = useState<HTMLDivElement | null>(null);
  const maxWidth = useHudClearanceWidth(row, playerHud, compact);

  return (
    <div
      ref={setRow}
      data-testid="action-rail-status-row"
      // Compact chips keep their natural width (`shrink-0`): a line that cannot
      // hold a chip wraps it upward rather than squeezing it.
      className={`hidden flex-row items-center gap-1.5 max-lg:landscape:flex lg:flex ${compact ? "flex-wrap-reverse justify-end [&>*]:shrink-0" : ""}`}
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
 * right of the HUD cluster. Undefined while the row keeps its natural width:
 * disabled, not mounted, not level with the cluster, or with less room right
 * of the cluster than its widest chip needs.
 *
 * Remeasures on every size change, on every chip mounting or unmounting, and on
 * every drag of either widget, so the bound follows a moved rail or plate.
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
    const rail = row.closest<HTMLElement>("[data-flex-zone]");
    const hudWidget = playerHud.closest<HTMLElement>("[data-flex-zone]");
    // PlayerArea centers the HUD's anchor in its middle-row wrapper, in a
    // column between the creature row and the other-permanent row. The wrapper
    // resizing, or a row beside it growing, moves the HUD without resizing it.
    const boardColumn = playerHud.closest("[data-player-hud-anchor]")?.parentElement?.parentElement;

    const measure = () => {
      let widestChip = 0;
      for (const chip of row.children) {
        // A chip's own sheet (the phase-stop sheet) mounts inside the row as a
        // fixed overlay; it is not a chip, so its width does not count.
        if (window.getComputedStyle(chip).position === "fixed") continue;
        widestChip = Math.max(widestChip, chip.getBoundingClientRect().width);
      }
      setMaxWidth(
        hudClearanceWidth(
          row.getBoundingClientRect(),
          measureHudCluster(playerHud, sideControls),
          widestChip,
          railScale,
        ),
      );
    };

    // Size changes: the chips (they change width with what they say), the
    // plate's badges, Manual and Undo appearing, the rail's action buttons
    // (they lift the row), and the board rows around the HUD.
    const resizeObserver = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    const boardRows = boardColumn ? Array.from(boardColumn.children) : [];
    const chips = Array.from(row.children);
    for (const element of [row, rail, playerHud, sideControls, ...boardRows, ...chips]) {
      if (element) resizeObserver?.observe(element);
    }
    // Chips mount and unmount as the game moves on, sometimes without resizing
    // the row. Follow them here rather than from inside the resize callback,
    // which would hand ResizeObserver new targets mid-delivery.
    const chipObserver = new MutationObserver((records) => {
      for (const record of records) {
        for (const added of record.addedNodes) {
          if (added instanceof Element) resizeObserver?.observe(added);
        }
        for (const removed of record.removedNodes) {
          if (removed instanceof Element) resizeObserver?.unobserve(removed);
        }
      }
      measure();
    });
    chipObserver.observe(row, { childList: true });
    // Moves: DraggableWidget applies drag offsets and box scale as inline
    // transforms, which move a widget without any resize or scroll event.
    const styleObserver = new MutationObserver(measure);
    for (const widget of [rail, hudWidget]) {
      if (widget) styleObserver.observe(widget, { attributes: true, attributeFilter: ["style"] });
    }
    window.addEventListener("resize", measure);
    measure();

    return () => {
      resizeObserver?.disconnect();
      chipObserver.disconnect();
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
  widestChip: number,
  railScale: number,
): number | undefined {
  // Only a cluster level with the row can be overlapped: a plate or rail
  // dragged apart vertically leaves the row free to grow. This reads the row
  // as laid out now, so a cluster level with only a wrapped upper line keeps
  // the bound, while one level with no line leaves the row on one line. Both
  // keep the row clear of the cluster, and each holds once reached, so the
  // bound never oscillates.
  if (cluster.bottom <= row.top || cluster.top >= row.bottom) return undefined;
  // The row is right-aligned in the rail, so its right edge stays put however
  // wide it gets. With no room right of the cluster, or less than the widest
  // chip needs, a bound would only stack chips that still overflow back over
  // the HUD, so the row keeps its natural width.
  const clearance = row.right - cluster.right - HUD_CLEARANCE_GAP_PX;
  if (clearance <= 0 || clearance < widestChip) return undefined;
  // The rail's Flex Layout box scale scales the row too, so convert screen
  // pixels back to the row's own. Whole pixels keep remeasures from churning.
  return Math.floor(clearance / railScale);
}
