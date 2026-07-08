import { useMemo, useState, type CSSProperties, type ReactNode } from "react";
import { Reorder, useDragControls } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { PlayerId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import {
  usePreferencesStore,
  DEFAULT_LAND_SUPPORT_RATIO,
  DEFAULT_CELL_ALIGN,
  DEFAULT_MIDDLE_ROW_ORDER,
  type CellAlign,
  type MiddleCell,
} from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import type { PlayerBattlefieldView } from "../../viewmodel/gameStateView.ts";
import { BattlefieldRow } from "./BattlefieldRow.tsx";
import { BattlefieldZoneOverflow } from "./BattlefieldZoneOverflow.tsx";
import { CompactStrip } from "./CompactStrip.tsx";
import { CommandDock } from "../zone/CommandDock.tsx";
import { DraggableWidget } from "../flexlayout/DraggableWidget.tsx";
import { ColumnEdgeHandle } from "../flexlayout/ColumnEdgeHandle.tsx";
import { CellAlignControl } from "../flexlayout/CellAlignControl.tsx";
import type { DraggableTarget } from "../../hooks/useDraggableWidget.ts";

/** Base scales — used when few cards; shrinks as more are added.
 *  On compact-height (landscape phones), lands shrink hard so creatures
 *  (which players actually interact with — attack, block, P/T, abilities)
 *  get vertical breathing room. */
const LAND_BASE_SCALE = 0.82;
const LAND_BASE_SCALE_COMPACT = 0.42;
const OTHER_BASE_SCALE = 0.92;
const OTHER_BASE_SCALE_COMPACT = 0.42;
/** Minimum scale floor */
const MIN_ZONE_SCALE = 0.35;

/** Compute dynamic scale that shrinks as group count increases */
function zoneScale(baseScale: number, groupCount: number): number {
  if (groupCount <= 3) return baseScale;
  // Inverse-sqrt decay past threshold, floored at MIN_ZONE_SCALE
  const excess = groupCount - 3;
  return Math.max(MIN_ZONE_SCALE, baseScale / Math.sqrt(1 + excess * 0.2));
}

function zoneStyle(scale: number): React.CSSProperties {
  return {
    "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${scale})`,
    "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${scale} * 0.85)`,
    "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${scale})`,
    "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${scale} * 1.4)`,
  } as React.CSSProperties;
}

/** CellAlign → flexbox justify class. Literal strings so Tailwind's JIT keeps
 *  them (a `justify-${align}` template would not be detected). */
const JUSTIFY_CLASS: Record<CellAlign, string> = {
  start: "justify-start",
  center: "justify-center",
  end: "justify-end",
};

/** One reorderable middle-row cell, described once and rendered either plain or
 *  wrapped in a draggable {@link MiddleRowCell}. */
interface MiddleCellDescriptor {
  className: string;
  style?: CSSProperties;
  debugLabel: string;
  flexZone?: string;
  /** i18n key (game namespace) for the edit-mode cell label. */
  labelKey: string;
  /** Edit-mode ring + fill that bounds the cell in a distinct hue. */
  editClass: string;
  /** Edit-mode label-badge background (matches `editClass`'s hue). */
  badgeClass: string;
  content: ReactNode;
}

/** A draggable middle-row cell (lands / support / command) in edit mode.
 *
 *  Each cell owns its own {@link useDragControls} (a hook, so it can't live in
 *  the parent's `.map()`), and Framer's drag is started manually from the cell's
 *  own `onPointerDown` with `dragListener={false}`. That keeps the whole-cell
 *  drag while letting a child — the {@link ColumnEdgeHandle} — opt out cleanly
 *  with a plain synthetic `stopPropagation`: both are React events, so the
 *  child's stop prevents the parent's drag-start. */
function MiddleRowCell({
  cellKey,
  cell,
  label,
  isDivider,
  alignable,
  columnResizing,
  onResizeStart,
  onResizeEnd,
}: {
  cellKey: MiddleCell;
  cell: MiddleCellDescriptor;
  label: string;
  isDivider: boolean;
  /** Whether the cell has free space to justify within (lands/support, not the
   *  content-width command cell) — gates the alignment control. */
  alignable: boolean;
  columnResizing: boolean;
  onResizeStart: () => void;
  onResizeEnd: () => void;
}) {
  const controls = useDragControls();
  return (
    <Reorder.Item
      as="div"
      value={cellKey}
      dragListener={false}
      dragControls={controls}
      onPointerDown={(e) => controls.start(e)}
      // Animate the reorder shuffle by position. Reorder.Item requires a truthy
      // `layout`, so we can't disable it mid divider-drag; instead we zero ONLY
      // the layout transition while resizing, so the cell edge snaps to the
      // pointer each frame instead of spring-chasing it (the laggy "stretch").
      layout="position"
      transition={columnResizing ? { layout: { duration: 0 } } : undefined}
      // Edit mode bounds each cell in a distinct hue with a labelled, grippable
      // badge so it's obvious what's being rearranged.
      className={`${cell.className} relative cursor-grab rounded-lg ${cell.editClass} active:cursor-grabbing`}
      style={cell.style}
      data-debug-label={cell.debugLabel}
      data-flex-zone={cell.flexZone}
    >
      <span
        className={`pointer-events-none absolute -top-2.5 left-1 z-20 flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-bold uppercase tracking-wide text-slate-950 shadow ${cell.badgeClass}`}
      >
        <span aria-hidden>⠿</span>
        {label}
      </span>
      {alignable && <CellAlignControl cell={cellKey} />}
      {cell.content}
      {isDivider && <ColumnEdgeHandle onResizeStart={onResizeStart} onResizeEnd={onResizeEnd} />}
    </Reorder.Item>
  );
}

export type PlayerAreaMode = "full" | "focused" | "compact";

interface PlayerAreaProps {
  playerId: PlayerId;
  mode: PlayerAreaMode;
  onFocus?: () => void;
  /** Whether this compact strip is the currently focused opponent */
  isActive?: boolean;
  /** Override creature groups with pre-sorted list (for blocker alignment) */
  creatureOverride?: GroupedPermanent[];
  battlefieldView?: PlayerBattlefieldView;
  /** HUD element rendered inline between lands and support in the middle row */
  hud?: React.ReactNode;
  /** Split multiplayer overview uses the focused layout, but top-anchors it. */
  splitOverview?: boolean;
}

export function PlayerArea({
  playerId,
  mode,
  onFocus,
  isActive,
  creatureOverride,
  battlefieldView,
  hud,
  splitOverview = false,
}: PlayerAreaProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const isCompactHeight = useIsCompactHeight();
  // Lands↔support split (lands' share of the middle row). One global ratio,
  // applied symmetrically to every player area; absent ⇒ even halves.
  const landSupportRatio =
    usePreferencesStore((s) => s.flexLayout.landSupportRatio) ?? DEFAULT_LAND_SUPPORT_RATIO;
  // Middle-row cell order (drag-to-reorder); a global preference applied to
  // every area. Reordering is enabled only in the viewer's own area in edit mode.
  const storedMiddleOrder = usePreferencesStore((s) => s.flexLayout.middleRowOrder);
  const setFlexMiddleRowOrder = usePreferencesStore((s) => s.setFlexMiddleRowOrder);
  const flexEditMode = useUiStore((s) => s.flexEditMode);
  // While the lands↔support seam is being dragged, the middle-row cells must NOT
  // run Framer's layout animation: the divider changes `flexGrow`, sliding the
  // right-of-seam cells' left edge every frame, and `layout="position"` would
  // spring-chase that moving target (the laggy "stretch"). Suppressing `layout`
  // during the drag makes the resize track the pointer instantly; restoring it
  // afterward keeps the reorder shuffle animated.
  const [columnResizing, setColumnResizing] = useState(false);
  // User-chosen per-cell content alignment (read here, above any early return,
  // to satisfy rules-of-hooks; the derived justify classes live further down).
  const cellAlign = usePreferencesStore((s) => s.flexLayout.cellAlign);
  // Combined support cluster: artifacts/enchantments then planeswalkers, in ONE
  // wrapping row (like the lands column) so it stays a single line until crowded.
  // Keeping it one row keeps the middle-row band ~one card tall so the flex-1
  // creature row isn't pinched. Memoized for a stable ref (BattlefieldRow perf);
  // declared above the early return to keep hook order stable.
  const supportGroups = useMemo(
    () => [...(battlefieldView?.support ?? []), ...(battlefieldView?.planeswalkers ?? [])],
    [battlefieldView?.support, battlefieldView?.planeswalkers],
  );

  if (!gameState) return null;

  // Compact mode renders a condensed strip
  if (mode === "compact") {
    return (
      <CompactStrip
        playerId={playerId}
        onClick={onFocus}
        isActive={isActive}
      />
    );
  }

  const player = gameState.players[playerId];
  const isEliminated = player?.is_eliminated ?? false;
  // CR 702.26-style player phasing: while phased out, dim the player area
  // to mirror the engine-side exclusion (targeting/damage/attack/SBA). Use
  // the same visual treatment as elimination for consistency.
  const isPhasedOut = player?.status?.type === "PhasedOut";
  const isMirrored = mode === "focused";
  // The viewer's own area (mode "full"). Only this area exposes the lands↔support
  // boundary markers so the editor grabs a single, predictable divider — the
  // ratio it sets is global, so every area reflows in step.
  const isOwnArea = mode === "full";
  const partitioned = battlefieldView;

  const creatures = creatureOverride ?? partitioned?.creatures ?? [];
  // User-chosen per-cell content alignment (left/center/right), defaulting to the
  // prior hardcoded lands-left / support-right. The justify class is the only
  // part that varies; the wrap/cross-axis classes are fixed. (`cellAlign` is read
  // above with the other hooks.)
  const landsJustify = JUSTIFY_CLASS[cellAlign?.lands ?? DEFAULT_CELL_ALIGN.lands];
  const supportJustify = JUSTIFY_CLASS[cellAlign?.support ?? DEFAULT_CELL_ALIGN.support];
  const landAlignClass = isCompactHeight
    ? `flex-nowrap items-center ${landsJustify}`
    : `flex-wrap items-center content-center ${landsJustify}`;
  // Support cluster mirrors the lands column: one wrapping row that wraps only
  // when crowded (cards shrink with count via supportStyle).
  const supportAlignClass = isCompactHeight
    ? `flex-nowrap items-center ${supportJustify}`
    : `flex-wrap items-center content-center ${supportJustify}`;

  const landCount = partitioned?.lands.length ?? 0;
  // Count the full support cluster (enchantments/artifacts + planeswalkers) so
  // zoneScale shrinks the cards as it fills — mirroring lands. Counting only
  // `support` left the planeswalkers unscaled and overflowing the column.
  const supportLen = partitioned?.support.length ?? 0;
  const planeswalkerLen = partitioned?.planeswalkers.length ?? 0;
  const supportCount = supportLen + planeswalkerLen;
  // Divider sits at the enchantment/artifact → planeswalker boundary within the
  // single combined support row, but only when both sub-clusters are present.
  const supportDividerIndex = supportLen > 0 && planeswalkerLen > 0 ? supportLen : undefined;
  const landBase = isCompactHeight ? LAND_BASE_SCALE_COMPACT : LAND_BASE_SCALE;
  const supportBase = isCompactHeight ? OTHER_BASE_SCALE_COMPACT : OTHER_BASE_SCALE;
  const landStyle = zoneStyle(zoneScale(landBase, landCount));
  const supportStyle = zoneStyle(zoneScale(supportBase, supportCount));

  // Middle row of three reorderable cells — lands, support, command. The lands
  // and support tracks split the row by `landSupportRatio` (flexGrow); command
  // is `shrink-0`. The HUD gets its own band (`hudBand`) adjacent to this row.
  // Each cell is described once and rendered either plain or wrapped in a
  // Reorder.Item, so reordering never duplicates the cell markup.
  const middleCells: Record<MiddleCell, MiddleCellDescriptor> = {
    lands: {
      // `flexGrow` overrides `flex-1`'s grow so the lands/support boundary sits
      // at the stored ratio; shrink/basis from `flex-1` are unchanged.
      className: `z-10 flex min-w-0 basis-0 flex-1 gap-2 pl-2 ${landAlignClass}`,
      style: { ...landStyle, flexGrow: landSupportRatio },
      debugLabel: "Lands",
      flexZone: isOwnArea ? "lands-col" : undefined,
      labelKey: "battlefieldOverflow.lands.label",
      editClass: "ring-2 ring-emerald-400/70 bg-emerald-400/10",
      badgeClass: "bg-emerald-400",
      content: (
        <>
          <BattlefieldZoneOverflow
            groups={partitioned?.lands ?? []}
            zone="lands"
            side="left"
            className="justify-start px-0"
            showCollapseControl={isOwnArea}
            splitOverview={splitOverview}
          />
        </>
      ),
    },
    // Support cluster: artifacts/enchantments + planeswalkers in ONE wrapping row
    // (mirrors the lands column). A thin divider (`supportDividerIndex`) separates
    // the two sub-clusters without stacking them onto a second row.
    support: {
      className: `z-10 flex min-w-0 basis-0 flex-1 gap-2 ${supportAlignClass}`,
      style: { ...supportStyle, flexGrow: 1 - landSupportRatio },
      debugLabel: "Support",
      flexZone: isOwnArea ? "support-col" : undefined,
      labelKey: "battlefieldOverflow.support.label",
      editClass: "ring-2 ring-violet-400/70 bg-violet-400/10",
      badgeClass: "bg-violet-400",
      content: (
        <BattlefieldZoneOverflow
          groups={supportGroups}
          zone="support"
          side="right"
          dividerBeforeIndex={supportDividerIndex}
          className="justify-end px-0"
          showCollapseControl={isOwnArea}
          splitOverview={splitOverview}
        />
      ),
    },
    // Command zone (CR 408) — `shrink-0` so it claims only its content width.
    // CommandDock renders null when empty, collapsing the cell to nothing.
    command: {
      className: "z-10 flex shrink-0 items-center self-center pr-2",
      debugLabel: "Command",
      labelKey: "zone.commandZone",
      editClass: "ring-2 ring-amber-400/70 bg-amber-400/10",
      badgeClass: "bg-amber-400",
      content: (
        <CommandDock
          playerId={playerId}
          isMirrored={isMirrored}
          splitOverview={splitOverview}
        />
      ),
    },
  };

  // Resolve the stored order, guarding against a corrupt/partial value.
  const middleOrder: MiddleCell[] =
    storedMiddleOrder && storedMiddleOrder.length === 3
      ? storedMiddleOrder
      : [...DEFAULT_MIDDLE_ROW_ORDER];
  // The lands↔support width grip lives on the seam between them — only when they
  // are adjacent (no cell reordered between). It rides the left cell of the pair.
  const landsIdx = middleOrder.indexOf("lands");
  const supportIdx = middleOrder.indexOf("support");
  const dividerCell: MiddleCell | null =
    Math.abs(landsIdx - supportIdx) === 1 ? (landsIdx < supportIdx ? "lands" : "support") : null;
  // Split panes get MORE row gap, not less — at a third-width the bands crowd
  // each other visually, so breathing room does the de-cluttering.
  const middleRowGap = "gap-2";
  const middleRowClass = `flex min-h-0 min-w-0 items-stretch justify-between ${middleRowGap}`;
  // Drag-to-reorder is enabled only in the viewer's own area while editing; the
  // resulting order persists globally and applies to every area (incl. plain
  // render below). Framer's Reorder distinguishes a drag from a tap, so cards
  // stay tappable.
  const middleRow =
    isOwnArea && flexEditMode ? (
      <Reorder.Group
        as="div"
        axis="x"
        values={middleOrder}
        onReorder={setFlexMiddleRowOrder}
        className={middleRowClass}
        data-debug-label="Middle Row"
      >
        {middleOrder.map((key) => (
          <MiddleRowCell
            key={key}
            cellKey={key}
            cell={middleCells[key]}
            label={t(middleCells[key].labelKey)}
            isDivider={key === dividerCell}
            // Command is content-width (shrink-0) — no free space to justify
            // within — so only lands/support get the alignment control.
            alignable={key !== "command"}
            columnResizing={columnResizing}
            onResizeStart={() => setColumnResizing(true)}
            onResizeEnd={() => setColumnResizing(false)}
          />
        ))}
      </Reorder.Group>
    ) : (
      <div className={middleRowClass} data-debug-label="Middle Row">
        {middleOrder.map((key) => {
          const cell = middleCells[key];
          return (
            <div
              key={key}
              className={cell.className}
              style={cell.style}
              data-debug-label={cell.debugLabel}
              data-flex-zone={cell.flexZone}
            >
              {cell.content}
            </div>
          );
        })}
      </div>
    );

  // Player HUD (life, mana, phase arrows) overlaid below the middle row rather
  // than wedged into its own column or lane. As a content-width absolute overlay
  // it consumes zero vertical space, so the flex-1 creature row keeps its full
  // height, and the box hugs the HUD instead of spanning the row. Centered
  // horizontally (`left-1/2 -translate-x-1/2`) and dropped below the middle
  // row for the player (`top-[165%] -translate-y-full`); the focused opponent
  // uses the vertical mirror (`bottom-[130%] translate-y-full`) so the HUD sits
  // just above its middle row. z-20 keeps it
  // above resting cards (lands/support are z-10) but below a hovered card
  // (PermanentCard lifts to z-60), so a card slides over the HUD on hover.
  // The player's own HUD is a shared-global widget; a HUD in `focused` mode is
  // the 1v1 opponent (multiplayer routes its opponent HUD through GameBoard
  // instead), keyed by the "oneVsOne" table size.
  const hudTarget: DraggableTarget =
    mode === "full"
      ? { kind: "widget", key: "playerHud" }
      : { kind: "opponentHud", tableSize: "oneVsOne" };
  const hudBand = hud ? (
    <div
      className={`absolute left-1/2 z-20 -translate-x-1/2 ${isMirrored ? "bottom-[130%] translate-y-full" : "top-[165%] -translate-y-full"}`}
      data-debug-label="HUD"
      {...(mode === "full" ? { "data-player-hud-anchor": "" } : {})}
    >
      {/* Inner node owns the drag offset so the outer `-translate-x-1/2`
          centering transform is never clobbered. */}
      <DraggableWidget
        target={hudTarget}
        flexZone={mode === "full" ? "playerHud" : "opponentHud"}
      >
        {hud}
      </DraggableWidget>
    </div>
  ) : null;

  const areaGap = splitOverview ? "gap-2.5" : isCompactHeight ? "gap-0.5" : "gap-2";
  const verticalPlacement = mode === "full"
    ? isCompactHeight ? "pt-0 pb-0.5" : "pt-1 pb-8"
    : splitOverview
      ? "justify-start py-1"
      : isCompactHeight ? "justify-end py-0" : "justify-end py-1";
  const mirroredCreatureAlign = "items-end";

  return (
    <div
      className={`relative flex min-h-0 min-w-0 flex-1 overflow-visible ${
        isEliminated ? "opacity-40 grayscale" : isPhasedOut ? "opacity-70" : ""
      }`}
      data-testid={`player-area-${playerId}`}
      data-phased-out={isPhasedOut ? "true" : undefined}
    >
      <div
        className={`flex min-w-0 flex-1 flex-col px-1 ${areaGap} ${verticalPlacement}`}
      >
        {isMirrored ? (
          <>
            <BattlefieldRow groups={partitioned?.other ?? []} rowType="other" />
            <div className={`relative ${isCompactHeight ? "min-h-0 max-h-[40%]" : "shrink-0"}`}>
              {middleRow}
              {hudBand}
            </div>
            <div
              className={`flex min-h-0 flex-1 ${mirroredCreatureAlign} px-2`}
              data-debug-label="Opp Creatures"
            >
              <BattlefieldZoneOverflow
                groups={creatures}
                zone="creatures"
                side="left"
                className="w-full"
                splitOverview={splitOverview}
              />
            </div>
          </>
        ) : (
          <>
            <div className="min-h-0 flex-1 px-2" data-debug-label="Creatures">
              <BattlefieldZoneOverflow
                groups={creatures}
                zone="creatures"
                side="left"
              />
            </div>
            <div className={`relative ${isCompactHeight ? "min-h-0 max-h-[40%]" : "shrink-0"}`}>
              {middleRow}
              {hudBand}
            </div>
            <BattlefieldRow groups={partitioned?.other ?? []} rowType="other" />
          </>
        )}
      </div>
      {/* Eliminated badge */}
      {isEliminated && (
        <div className="absolute inset-0 z-30 flex items-center justify-center pointer-events-none">
          <span className="rounded-lg bg-red-900/80 px-4 py-2 text-lg font-bold text-red-200">
            {t("player.eliminated")}
          </span>
        </div>
      )}
      {/* Phased-out tint overlay + badge (CR 702.26-style player phasing).
          Translucent blue evokes the "ethereal plane" reading of phasing and
          is a stronger signal than dim-alone, which overlaps with tap/grayed
          states. `pointer-events-none` preserves card hover/click semantics —
          interactivity gating is an engine concern, not a visual one. */}
      {isPhasedOut && !isEliminated && (
        <>
          <div className="absolute inset-0 z-20 bg-sky-500/25 mix-blend-screen pointer-events-none" />
          <div className="absolute inset-0 z-30 flex items-center justify-center pointer-events-none">
            <span className="rounded-lg bg-indigo-900/80 px-4 py-2 text-lg font-bold text-indigo-200">
              {t("player.phasedOut")}
            </span>
          </div>
        </>
      )}
    </div>
  );
}
