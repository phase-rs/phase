import { useTranslation } from "react-i18next";
import type { CSSProperties, RefCallback } from "react";

import type { DraftCardInstance, DraftPoolGroups } from "../../../adapter/draft-adapter";
import type { CardHoverInfo } from "../../card/CardPreview";
import {
  activateWorkspaceInstance,
  buildCardPoolBoardModel,
  moveWorkspaceInstance,
  normalizeWorkspaceMoveTarget,
  resolveWorkspacePickPlacement,
  type WorkspaceCardEntryModel,
} from "./workspacePlacement";
import {
  WorkspaceCard,
  WorkspaceDragProjectionSlot,
  type WorkspaceDragProjection,
} from "./WorkspaceCard";
import type { DraftWorkspaceState, DraftZone } from "./types";
import type { DraftBoardPreferences, ResponsiveDraftLayout } from "./workspacePreferences";
import type {
  DraftWorkspaceDragController,
  WorkspaceDragOrigin,
  WorkspaceDragSource,
  WorkspaceDragPreviewImage,
} from "./useDraftWorkspaceDrag";

interface CompactSideboardProps {
  pool: readonly DraftCardInstance[];
  poolGroups: DraftPoolGroups;
  workspace: DraftWorkspaceState;
  preferences: Readonly<Record<DraftZone, DraftBoardPreferences>>;
  interactionLocked?: boolean;
  dropActive?: boolean;
  registerCardArea?: RefCallback<HTMLElement>;
  dragController?: DraftWorkspaceDragController;
  touchDragEnabled?: boolean;
  draggedSourceInstanceId?: string;
  dragProjection?: WorkspaceDragProjection;
  collapsed?: boolean;
  onToggle(): void;
  onWorkspaceChange(next: DraftWorkspaceState): void;
  onCardHover?(info: CardHoverInfo | null): void;
  responsiveLayout?: ResponsiveDraftLayout;
  responsiveContext?: "draft" | "builder";
  visualColumnCap?: number;
  visualColumnWidth?: number;
}

interface ResponsiveStackLayout {
  columnCount: number;
  columnGapPx: number;
  exposedStepPx: number;
}

// Sideboards deliberately reveal more of each card than the denser deck stacks.
// Both draft and builder use this same compact geometry on every non-desktop layout.
const RESPONSIVE_STACK_LAYOUTS: Readonly<Record<Exclude<ResponsiveDraftLayout, "desktop">, ResponsiveStackLayout>> = {
  "phone-portrait": { columnCount: 2, columnGapPx: 8, exposedStepPx: 56 },
  "phone-landscape": { columnCount: 1, columnGapPx: 0, exposedStepPx: 32 },
  "tablet-portrait": { columnCount: 3, columnGapPx: 8, exposedStepPx: 72 },
  "tablet-landscape": { columnCount: 1, columnGapPx: 0, exposedStepPx: 40 },
};

export function CompactSideboard({
  pool,
  poolGroups,
  workspace,
  preferences,
  interactionLocked = false,
  dropActive = false,
  registerCardArea,
  dragController,
  touchDragEnabled = false,
  draggedSourceInstanceId,
  dragProjection,
  collapsed = true,
  onToggle,
  onWorkspaceChange,
  onCardHover,
  responsiveLayout = "desktop",
  responsiveContext = "draft",
  visualColumnCap,
  visualColumnWidth,
}: CompactSideboardProps) {
  const { t } = useTranslation("draft");
  const sideboard = buildCardPoolBoardModel(
    "sideboard",
    pool,
    poolGroups,
    workspace,
    preferences.sideboard,
  );
  const cards = sideboard.columns.flatMap((column) => (
    column.rows.flatMap((row) => row.cards)
  ));
  const visibleCards = draggedSourceInstanceId === undefined
    ? cards
    : cards.filter((card) => card.instanceId !== draggedSourceInstanceId);
  const builderPhoneLayout = responsiveContext === "builder"
    && (responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape");
  const draftPhoneLayout = responsiveContext === "draft"
    && (responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape");
  const tabletLayout = responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape";
  const tabletPortraitLayout = responsiveLayout === "tablet-portrait";
  const isLandscapeBuilderPhone = builderPhoneLayout && responsiveLayout === "phone-landscape";
  const draftPhoneLandscapeSideRail = responsiveContext === "draft"
    && responsiveLayout === "phone-landscape"
    && collapsed;
  const draftPhoneLandscapeExpanded = responsiveContext === "draft"
    && responsiveLayout === "phone-landscape"
    && !collapsed;
  const draftPhonePortraitExpanded = responsiveContext === "draft"
    && responsiveLayout === "phone-portrait"
    && !collapsed;
  const responsiveStack = responsiveLayout !== "desktop";
  const draftResponsiveColumnParity = responsiveContext === "draft"
    && responsiveStack
    && visualColumnCap !== undefined;
  const responsiveStackLayout = responsiveStack
    ? draftResponsiveColumnParity
      ? {
        ...RESPONSIVE_STACK_LAYOUTS[responsiveLayout],
        columnCount: visualColumnCap,
        columnGapPx: 8,
      }
      : RESPONSIVE_STACK_LAYOUTS[responsiveLayout]
    : undefined;
  const projectionStackIndex = (() => {
    if (dragProjection === undefined) return visibleCards.length;
    let index = 0;
    for (const column of sideboard.columns) {
      for (const row of column.rows) {
        const visibleRowCount = row.cards.filter((card) => card.instanceId !== draggedSourceInstanceId).length;
        if (column.column === dragProjection.column && row.row === dragProjection.row) {
          return index + visibleRowCount;
        }
        index += visibleRowCount;
      }
    }
    return visibleCards.length;
  })();
  const responsiveProjectionIndex = projectionStackIndex;
  const responsiveProjectionColumn = responsiveStackLayout === undefined
    ? 0
    : responsiveProjectionIndex % responsiveStackLayout.columnCount;
  const responsiveProjectionRow = responsiveStackLayout === undefined
    ? 0
    : Math.floor(responsiveProjectionIndex / responsiveStackLayout.columnCount);
  const responsiveItemCount = visibleCards.length + (dragProjection === undefined ? 0 : 1);
  const maximumResponsiveStackRow = responsiveStackLayout === undefined || responsiveItemCount === 0
    ? 0
    : Math.floor((responsiveItemCount - 1) / responsiveStackLayout.columnCount);
  const responsiveCardWidth = responsiveStackLayout === undefined
    ? undefined
    : visualColumnWidth === undefined
      ? `calc(${100 / responsiveStackLayout.columnCount}% - ${((responsiveStackLayout.columnCount - 1) * responsiveStackLayout.columnGapPx) / responsiveStackLayout.columnCount}px)`
      : `${visualColumnWidth}px`;
  const responsiveColumnLeft = (column: number): string => visualColumnWidth === undefined
    ? `calc(${column * 100 / responsiveStackLayout!.columnCount}% + ${column * responsiveStackLayout!.columnGapPx / responsiveStackLayout!.columnCount}px)`
    : `${column * (visualColumnWidth + responsiveStackLayout!.columnGapPx)}px`;
  const responsiveProjectionStyle: CSSProperties | undefined = responsiveStackLayout === undefined || responsiveCardWidth === undefined
    ? undefined
    : {
      position: "absolute",
      top: responsiveProjectionRow * responsiveStackLayout.exposedStepPx,
      left: responsiveProjectionColumn === 0
        ? 0
        : responsiveColumnLeft(responsiveProjectionColumn),
      width: responsiveCardWidth,
    };
  const naturalCardLayout = responsiveStack;
  const scrollableCardLayout = responsiveStack;
  const collapsibleCompactSideboard = builderPhoneLayout || draftPhoneLayout || tabletLayout;
  const sideRailCollapsed = (
    isLandscapeBuilderPhone
    || responsiveLayout === "tablet-landscape"
    || draftPhoneLandscapeSideRail
  ) && collapsed;
  const tabletLandscapeCollapsed = responsiveLayout === "tablet-landscape" && collapsed;
  const combinedSideRailLabel = sideRailCollapsed && (
    responsiveLayout === "tablet-landscape"
    || draftPhoneLandscapeSideRail
  );
  const tabletPortraitCollapsed = tabletPortraitLayout && collapsed;
  const toggleLabel = collapsed
    ? t("workspace.sideboard.expand", { count: sideboard.count })
    : t("workspace.sideboard.collapse");
  const toggleSymbol = responsiveLayout === "tablet-landscape" || collapsed ? "▲" : "▼";
  const sideboardToggle = (responsiveLayout === "desktop" || builderPhoneLayout || draftPhoneLayout || tabletLayout) && (
    <button
      type="button"
      aria-label={toggleLabel}
      title={toggleLabel}
      aria-expanded={!collapsed}
      disabled={interactionLocked}
      onClick={onToggle}
      className="h-8 w-8 shrink-0 rounded-[6px] border border-hairline bg-slate-950/72 text-fg-muted transition-colors hover:border-hairline-hover hover:text-fg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-jade/30 disabled:cursor-not-allowed disabled:opacity-40"
    >
      <span
        aria-hidden="true"
        className={responsiveLayout === "tablet-landscape"
          ? collapsed ? "block -rotate-90" : "block rotate-90"
          : isLandscapeBuilderPhone || draftPhoneLandscapeSideRail ? "block -rotate-90" : "block"}
      >
        {responsiveLayout === "desktop" ? "▼" : toggleSymbol}
      </span>
    </button>
  );

  // Returning to the deck sorts as if freshly picked, unlike the column-preserving zone toggle.
  const activate = (instanceId: string) => {
    if (interactionLocked) return;
    const draftCard = pool.find((entry) => entry.instance_id === instanceId);
    const next = draftCard === undefined
      ? activateWorkspaceInstance(workspace, pool, poolGroups, preferences, instanceId)
      : moveWorkspaceInstance(workspace, pool, poolGroups, preferences, instanceId, {
        zone: "deck",
        ...resolveWorkspacePickPlacement(
          draftCard,
          "deck",
          pool,
          poolGroups,
          workspace,
          preferences.deck,
        ),
        beforeInstanceId: null,
      });
    if (next !== workspace) onWorkspaceChange(next);
  };

  const makeDragSource = (
    card: WorkspaceCardEntryModel,
    previewWidth: number,
    previewHeight: number,
    previewImage: WorkspaceDragPreviewImage,
    origin: WorkspaceDragOrigin,
  ): WorkspaceDragSource => {
    const draftCard = pool.find((entry) => entry.instance_id === card.instanceId) ?? {
      instance_id: card.instanceId,
      name: card.name,
      set_code: card.sourcePrinting?.setCode ?? "",
      collector_number: card.sourcePrinting?.collectorNumber ?? "",
      rarity: "",
      colors: [],
      cmc: 0,
      type_line: "",
    };
    const canonicalTarget = normalizeWorkspaceMoveTarget(
      card.instanceId,
      poolGroups,
      preferences,
      card.placement,
    );
    if (canonicalTarget === null) throw new Error("Workspace card has no canonical target");
    return {
      kind: "workspace",
      instanceIds: [card.instanceId],
      cards: [draftCard],
      canonicalTarget,
      previewWidth,
      previewHeight,
      origin,
      previewImage,
      onDrop: (target) => {
        if (interactionLocked) return false;
        const normalizedTarget = normalizeWorkspaceMoveTarget(
          card.instanceId,
          poolGroups,
          preferences,
          target,
        );
        if (
          normalizedTarget === null
          || (
            normalizedTarget.zone === canonicalTarget.zone
            && normalizedTarget.column === canonicalTarget.column
            && normalizedTarget.row === canonicalTarget.row
          )
        ) return false;
        const next = moveWorkspaceInstance(
          workspace,
          pool,
          poolGroups,
          preferences,
          card.instanceId,
          { ...normalizedTarget, beforeInstanceId: null },
        );
        if (next === workspace) return false;
        onWorkspaceChange(next);
        return true;
      },
    };
  };

  return (
    <section
      aria-label={t("workspace.compact.sideboardRegion")}
      data-zone="sideboard"
      data-responsive-sideboard-layout={responsiveLayout}
      data-sideboard-collapsed={collapsed ? "true" : "false"}
      className={`min-w-0 ${dropActive ? "overflow-visible" : "overflow-hidden"} rounded-card border border-hairline text-fg shadow-[0_10px_26px_rgba(0,0,0,0.2)] ${tabletPortraitLayout && !collapsed ? "bg-slate-950" : "bg-black/18"} ${tabletLandscapeCollapsed || draftPhoneLandscapeSideRail ? "relative" : ""} ${isLandscapeBuilderPhone || draftPhoneLayout || tabletLayout ? "flex h-full min-h-0 flex-col" : builderPhoneLayout ? "flex shrink-0 flex-col" : ""}`}
    >
      <header className={`relative z-10 ${sideRailCollapsed
        ? `relative flex min-h-11 flex-col items-center justify-start gap-1 bg-white/[0.035] px-1 py-2 ${tabletLandscapeCollapsed || draftPhoneLandscapeSideRail ? "h-full flex-1" : "shrink-0"}`
        : draftPhoneLandscapeExpanded
          ? "flex shrink-0 flex-col items-start gap-1 border-b border-hairline bg-white/[0.035] px-2 py-1.5"
        : tabletPortraitCollapsed
          ? "relative flex min-h-11 shrink-0 items-center justify-center border-b border-hairline bg-slate-950 px-12 py-1"
        : "flex min-h-11 shrink-0 items-center gap-2 border-b border-hairline bg-white/[0.035] px-2 py-1.5"}`}
      >
        {sideRailCollapsed && (draftPhoneLandscapeSideRail
          ? <span className="relative z-10">{sideboardToggle}</span>
          : responsiveLayout === "tablet-landscape"
            ? <span className="relative z-10">{sideboardToggle}</span>
            : sideboardToggle)}
        {sideRailCollapsed && !combinedSideRailLabel && (
          <span data-sideboard-count className="shrink-0 font-mono text-xs tabular-nums text-fg">
            {sideboard.count}
          </span>
        )}
        {tabletPortraitCollapsed && (
          <span className="absolute right-2 top-1/2 -translate-y-1/2">{sideboardToggle}</span>
        )}
        {draftPhoneLandscapeExpanded && sideboardToggle}
        <h2 className={draftPhoneLandscapeSideRail
          ? "pointer-events-none absolute inset-0 flex items-center justify-center whitespace-nowrap font-display text-sm font-semibold text-fg [writing-mode:vertical-rl] rotate-180"
          : combinedSideRailLabel
            ? "pointer-events-none absolute inset-0 flex items-center justify-center whitespace-nowrap font-display text-sm font-semibold text-fg [writing-mode:vertical-rl] rotate-180"
          : sideRailCollapsed
            ? "min-h-0 flex-1 whitespace-nowrap font-display text-sm font-semibold text-fg [writing-mode:vertical-rl] rotate-180"
          : tabletPortraitCollapsed
            ? "flex flex-col items-center font-display text-sm font-semibold leading-tight text-fg"
          : draftPhoneLandscapeExpanded
            ? "max-w-full text-left font-display text-xs font-semibold leading-tight text-fg"
          : "min-w-0 flex-1 truncate font-display text-sm font-semibold text-fg"}
        >
          {combinedSideRailLabel
            ? <>{t("workspace.zone.sideboard")} <span data-sideboard-count className="font-mono text-xs tabular-nums">({sideboard.count})</span></>
            : sideRailCollapsed
              ? t("workspace.zone.sideboard")
            : draftPhonePortraitExpanded
              ? t("workspace.zone.sideboard")
            : tabletPortraitCollapsed
              ? <><span>{t("workspace.zone.sideboard")}</span><span data-sideboard-count className="font-mono text-xs tabular-nums">({sideboard.count})</span></>
            : t("workspace.count.sideboard", { count: sideboard.count })}
        </h2>
        {!sideRailCollapsed && !tabletPortraitCollapsed && !draftPhoneLandscapeExpanded && sideboardToggle}
      </header>
      {(!collapsibleCompactSideboard || !collapsed) && <div
        ref={registerCardArea}
        data-sideboard-card-area
        data-drop-target="collapsed-sideboard"
        data-drop-state={dropActive ? "active" : "idle"}
        data-sideboard-slot={builderPhoneLayout || scrollableCardLayout ? undefined : ""}
        data-sideboard-body={builderPhoneLayout || scrollableCardLayout ? "" : undefined}
        data-deck-row-layout={draftResponsiveColumnParity ? preferences.deck.rows : undefined}
        className={`${scrollableCardLayout
          ? `relative min-h-0 flex-1 touch-pan-y overflow-y-auto overscroll-contain bg-black/12 thin-scrollbar ${draftResponsiveColumnParity
            ? preferences.deck.rows === "two" ? "p-6 pl-16" : "p-6"
            : "p-2"}`
          : builderPhoneLayout
            ? "relative shrink-0 overflow-visible bg-black/12 p-2"
            : `relative grid min-w-0 bg-black/12 ${responsiveLayout === "desktop" || responsiveLayout === "phone-landscape" ? "aspect-[488/680]" : ""}`} ${dropActive ? "draft-card-area-drop-active" : ""}`}
      >
        <span
          aria-hidden="true"
          data-card-height-baseline
          className={`${responsiveLayout === "desktop" ? "block" : "hidden"} aspect-[488/680] w-full self-start [grid-area:1/1]`}
        />
        {cards.length === 0 && dragProjection === undefined ? (
          <div
            data-card-stack={responsiveContext === "draft" && responsiveStack ? "" : undefined}
            data-sideboard-column-count={responsiveContext === "draft" ? responsiveStackLayout?.columnCount : undefined}
            data-sideboard-column-width={visualColumnWidth}
            className="flex items-center justify-center border border-dashed border-white/15 px-2 text-center text-xs text-white/35 [grid-area:1/1]"
          >
            {t("workspace.empty.sideboard")}
          </div>
        ) : (
          <div
            data-card-stack
            data-sideboard-column-count={responsiveStackLayout?.columnCount}
            data-sideboard-column-width={visualColumnWidth}
            className={responsiveStack
              ? "relative min-h-full min-w-0"
              : builderPhoneLayout
              ? isLandscapeBuilderPhone
                ? "flex min-w-0 flex-col"
                : "grid min-w-0 grid-cols-2 gap-2"
              : "relative h-full min-w-0 [grid-area:1/1]"}
          >
            {responsiveStackLayout !== undefined && responsiveCardWidth !== undefined && (
              <span
                aria-hidden="true"
                data-sideboard-stack-spacer
                className="block aspect-[488/680]"
                style={{
                  width: responsiveCardWidth,
                  marginBottom: maximumResponsiveStackRow * responsiveStackLayout.exposedStepPx,
                }}
              />
            )}
            {cards.map((card) => {
              const hidden = draggedSourceInstanceId === card.instanceId;
              const stackIndex = hidden ? 0 : visibleCards.findIndex((visibleCard) => visibleCard.instanceId === card.instanceId);
              const column = responsiveStackLayout === undefined ? 0 : Math.max(0, stackIndex) % responsiveStackLayout.columnCount;
              const row = responsiveStackLayout === undefined ? Math.max(0, stackIndex) : Math.floor(Math.max(0, stackIndex) / responsiveStackLayout.columnCount);
              const responsiveStackStyle: CSSProperties | undefined = responsiveStackLayout === undefined || responsiveCardWidth === undefined
                ? undefined
                : {
                  position: "absolute",
                  top: row * responsiveStackLayout.exposedStepPx,
                  left: column === 0
                    ? 0
                    : responsiveColumnLeft(column),
                  width: responsiveCardWidth,
                };
              const workspaceCard = (
              <WorkspaceCard
                key={card.key}
                card={card}
                stackIndex={stackIndex}
                dragHidden={hidden}
                interactionLocked={interactionLocked}
                onHover={(hoveredCard) => onCardHover?.(hoveredCard?.preview ?? null)}
                onBlur={() => onCardHover?.(null)}
                onActivate={(activatedCard) => activate(activatedCard.instanceId)}
                stackStyle={responsiveStack
                  ? responsiveStackStyle
                  : undefined}
                {...(dragController === undefined
                  ? {}
                  : {
                    drag: {
                      controller: dragController,
                      makeSource: makeDragSource,
                      touchDragEnabled,
                      touchScrollEnabled: builderPhoneLayout || naturalCardLayout,
                    },
                  })}
              />
              );
              return responsiveStackLayout === undefined ? workspaceCard : (
                <div
                  key={card.key}
                  className="contents"
                  data-sideboard-column={column}
                  data-sideboard-row={row}
                >
                  {workspaceCard}
                </div>
              );
            })}
            {dragProjection !== undefined && (
              <WorkspaceDragProjectionSlot
                sourceInstanceId={dragProjection.sourceInstanceId}
                stackIndex={projectionStackIndex}
                placementStyle={responsiveProjectionStyle}
              />
            )}
          </div>
        )}
        {cards.length > 0 && cards.map((card) => (
          <button
            key={card.key}
            type="button"
            disabled={interactionLocked}
            className="sr-only"
            aria-label={card.isVirtualBasic
              ? t("limitedDeck.removeCard", { name: card.name })
              : t("workspace.card.moveToZone", {
                card: card.name,
                zone: t("workspace.zone.deck"),
              })}
            onClick={() => activate(card.instanceId)}
          >
            {t("workspace.zone.deck")}
          </button>
        ))}
      </div>}
      {collapsibleCompactSideboard && collapsed && (
        <div
          ref={registerCardArea}
          data-sideboard-card-area
          data-drop-target="collapsed-sideboard"
          data-drop-state={dropActive ? "active" : "idle"}
          className={`${tabletLandscapeCollapsed || draftPhoneLandscapeSideRail ? "absolute inset-0" : "relative min-h-11 flex-1"} overflow-hidden ${dropActive ? "draft-card-area-drop-active" : ""}`}
        >
          {dragProjection !== undefined && (
            <WorkspaceDragProjectionSlot
              sourceInstanceId={dragProjection.sourceInstanceId}
              stackIndex={0}
              placementStyle={responsiveLayout === "phone-portrait" || responsiveLayout === "tablet-portrait"
                ? {
                  position: "absolute",
                  left: "50%",
                  top: 4,
                  height: "calc(100% - 8px)",
                  aspectRatio: "488 / 680",
                  transform: "translateX(-50%)",
                }
                : {
                  position: "absolute",
                  left: 4,
                  top: "50%",
                  width: "calc(100% - 8px)",
                  aspectRatio: "488 / 680",
                  transform: "translateY(-50%)",
                }}
            />
          )}
        </div>
      )}
    </section>
  );
}
