import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type RefCallback,
} from "react";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance, DraftPoolGroups } from "../../../adapter/draft-adapter";
import { HoverCardPreview } from "../../card/HoverCardPreview";
import { CardPoolColumn } from "./CardPoolColumn";
import { DraftWorkspaceToolbar } from "./DraftWorkspaceToolbar";
import {
  buildCardPoolBoardModel,
  activateWorkspaceInstance,
  moveWorkspaceInstance,
  rebuildWorkspaceZone,
  rebuildWorkspaceZoneRows,
  resolveAvailableBoardSort,
  type WorkspaceBoardModel,
  type WorkspaceCardEntryModel,
  type WorkspaceDropState,
  type WorkspaceMoveTarget,
} from "./workspacePlacement";
import type { DraftWorkspaceState, DraftZone } from "./types";
import type { DraftBoardPreferences, DraftCardPreviewMode } from "./workspacePreferences";
import type { DraftWorkspaceDragController, WorkspaceDragSource } from "./useDraftWorkspaceDrag";

export interface CardPoolBoardProps {
  heading?: string;
  deckTypeCounts?: { creatures: number; lands: number };
  deckControls?: ReactNode;
  trailingControls?: ReactNode;
  zone: DraftZone;
  pool: readonly DraftCardInstance[];
  poolGroups: DraftPoolGroups;
  workspace: DraftWorkspaceState;
  preferences: DraftBoardPreferences;
  cardPreviewMode: DraftCardPreviewMode;
  cardPreviewHoverDelayMs: number;
  oppositeZonePreferences?: DraftBoardPreferences;
  dropState?: WorkspaceDropState;
  interactionLocked?: boolean;
  dragController?: DraftWorkspaceDragController;
  registerBoard?: RefCallback<HTMLElement>;
  registerColumn?(zone: DraftZone, column: number): RefCallback<HTMLElement>;
  visualColumnCap?: number;
  forceShowHeaders?: boolean;
  phoneToolbar?: boolean;
  phoneLayoutDialog?: boolean;
  phonePortraitDeckToolbar?: boolean;
  tabletMode?: boolean;
  compactDeckTypeCounts?: boolean;
  visualColumnCapValue?: number;
  visualColumnCapMax?: number;
  onVisualColumnCapChange?(next: number): void;
  touchDragEnabled?: boolean;
  touchScrollEnabled?: boolean;
  onWorkspaceChange(next: DraftWorkspaceState): void;
  onPreferencesChange(next: DraftBoardPreferences): void;
}

interface CardLocation {
  column: number;
  row: number;
  index: number;
}

function cardLocation(model: WorkspaceBoardModel, instanceId: string): CardLocation | null {
  for (const column of model.columns) {
    for (const row of column.rows) {
      const index = row.cards.findIndex((card) => card.instanceId === instanceId);
      if (index >= 0) return { column: column.column, row: row.row, index };
    }
  }
  return null;
}

function horizontalCard(
  model: WorkspaceBoardModel,
  source: CardLocation,
  direction: -1 | 1,
): WorkspaceCardEntryModel | null {
  for (
    let column = source.column + direction;
    column >= 0 && column < model.columnCount;
    column += direction
  ) {
    const preferred = model.columns[column].rows[source.row]?.cards ?? [];
    const alternateRow = source.row === 0 ? 1 : 0;
    const alternate = model.columns[column].rows[alternateRow]?.cards ?? [];
    const stack = preferred.length > 0 ? preferred : alternate;
    if (stack.length > 0) return stack[Math.min(source.index, stack.length - 1)];
  }
  return null;
}

function crossRowCard(
  model: WorkspaceBoardModel,
  source: CardLocation,
  targetRow: number,
): WorkspaceCardEntryModel | null {
  const columns = model.columns
    .map((column) => column.column)
    .sort((left, right) => (
      Math.abs(left - source.column) - Math.abs(right - source.column) || left - right
    ));
  for (const column of columns) {
    const stack = model.columns[column].rows[targetRow]?.cards ?? [];
    if (stack.length > 0) return stack[Math.min(source.index, stack.length - 1)];
  }
  return null;
}

export function CardPoolBoard({
  heading,
  deckTypeCounts,
  deckControls,
  trailingControls,
  zone,
  pool,
  poolGroups,
  workspace,
  preferences,
  cardPreviewMode,
  cardPreviewHoverDelayMs,
  oppositeZonePreferences = preferences,
  dropState,
  interactionLocked = false,
  dragController,
  registerBoard,
  registerColumn,
  visualColumnCap,
  forceShowHeaders = false,
  phoneToolbar = false,
  phoneLayoutDialog = false,
  phonePortraitDeckToolbar = false,
  tabletMode = false,
  compactDeckTypeCounts = false,
  visualColumnCapValue,
  visualColumnCapMax,
  onVisualColumnCapChange,
  touchDragEnabled = false,
  touchScrollEnabled = false,
  onWorkspaceChange,
  onPreferencesChange,
}: CardPoolBoardProps) {
  const { t } = useTranslation("draft");
  const [previewCard, setPreviewCard] = useState<WorkspaceCardEntryModel | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const cardRefs = useRef(new Map<string, HTMLButtonElement>());
  const headerRefs = useRef(new Map<number, HTMLElement>());
  const minusRef = useRef<HTMLButtonElement>(null);
  const pendingCardFocus = useRef<string | null>(null);
  const pendingHeaderFocus = useRef<number | null>(null);
  const resolvedPreferences = {
    ...preferences,
    sort: resolveAvailableBoardSort(preferences.sort, poolGroups.workspace_capabilities),
  };
  const resolvedOppositeZonePreferences = {
    ...oppositeZonePreferences,
    sort: resolveAvailableBoardSort(
      oppositeZonePreferences.sort,
      poolGroups.workspace_capabilities,
    ),
  };
  const model = buildCardPoolBoardModel(
    zone,
    pool,
    poolGroups,
    workspace,
    resolvedPreferences,
    dropState,
  );
  const oppositeZone = zone === "deck" ? "sideboard" : "deck";
  const allPreferences = {
    [zone]: resolvedPreferences,
    [oppositeZone]: resolvedOppositeZonePreferences,
  } as Readonly<Record<DraftZone, DraftBoardPreferences>>;
  const oppositeModel = buildCardPoolBoardModel(
    oppositeZone,
    pool,
    poolGroups,
    workspace,
    resolvedOppositeZonePreferences,
  );
  const visualColumnGroups = visualColumnCap === undefined
    ? [model.columns]
    : Array.from(
      { length: Math.ceil(model.columns.length / visualColumnCap) },
      (_, groupIndex) => model.columns.slice(
        groupIndex * visualColumnCap,
        (groupIndex + 1) * visualColumnCap,
      ),
    );
  const showHeaders = forceShowHeaders || model.showHeaders;

  useEffect(() => {
    if (interactionLocked) return;
    const instanceId = pendingCardFocus.current;
    if (instanceId !== null) {
      const element = cardRefs.current.get(instanceId);
      if (element !== undefined) {
        element.focus();
        pendingCardFocus.current = null;
      }
    }
    const column = pendingHeaderFocus.current;
    if (column !== null) {
      const target = headerRefs.current.get(Math.min(column, model.columnCount - 1))
        ?? headerRefs.current.get(model.columnCount - 1)
        ?? minusRef.current;
      target?.focus();
      pendingHeaderFocus.current = null;
    }
  }, [interactionLocked, model]);

  const focusCard = (card: WorkspaceCardEntryModel | null) => {
    if (interactionLocked) return;
    if (card !== null) cardRefs.current.get(card.key)?.focus();
  };

  const dispatchMove = (card: WorkspaceCardEntryModel, target: WorkspaceMoveTarget): boolean => {
    if (interactionLocked) return false;
    const next = moveWorkspaceInstance(
      workspace,
      pool,
      poolGroups,
      allPreferences,
      card.instanceId,
      target,
    );
    if (next === workspace) return false;
    pendingCardFocus.current = card.key;
    onWorkspaceChange(next);
    const destinationModel = buildCardPoolBoardModel(
      target.zone,
      pool,
      poolGroups,
      next,
      allPreferences[target.zone],
    );
    const destination = cardLocation(destinationModel, card.instanceId);
    setAnnouncement(t("workspace.announcement.moved", {
      card: card.name,
      zone: t(`workspace.zone.${target.zone}`),
      column: target.column + 1,
      position: (destination?.index ?? 0) + 1,
    }));
    return true;
  };

  const makeDragSource = (
    card: WorkspaceCardEntryModel,
    previewWidth: number,
    previewHeight: number,
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
    return {
      kind: "workspace",
      instanceIds: [card.instanceId],
      cards: [draftCard],
      previewWidth,
      previewHeight,
      onDrop: (target) => dispatchMove(card, { ...target, beforeInstanceId: null }),
    };
  };

  const activateCard = (card: WorkspaceCardEntryModel) => {
    if (interactionLocked) return;
    const next = activateWorkspaceInstance(
      workspace,
      pool,
      poolGroups,
      allPreferences,
      card.instanceId,
    );
    if (next === workspace) return;
    onWorkspaceChange(next);
    if (card.isVirtualBasic) {
      setAnnouncement(t("limitedDeck.removeCard", { name: card.name }));
      return;
    }
    const targetZone = card.placement.zone === "deck" ? "sideboard" : "deck";
    const destinationModel = buildCardPoolBoardModel(
      targetZone,
      pool,
      poolGroups,
      next,
      allPreferences[targetZone],
    );
    const destination = cardLocation(destinationModel, card.instanceId);
    setAnnouncement(t("workspace.announcement.moved", {
      card: card.name,
      zone: t(`workspace.zone.${targetZone}`),
      column: (destination?.column ?? 0) + 1,
      position: (destination?.index ?? 0) + 1,
    }));
  };

  const handlePreferencesChange = (next: DraftBoardPreferences) => {
    if (interactionLocked) return;
    const requiresRebuild = next.sort !== resolvedPreferences.sort
      || next.rows !== resolvedPreferences.rows
      || next.columnCount !== resolvedPreferences.columnCount;
    if (next.columnCount < resolvedPreferences.columnCount) {
      setAnnouncement(t("workspace.announcement.columnRemoved", {
        column: resolvedPreferences.columnCount,
      }));
    }
    onPreferencesChange(next);
    if (requiresRebuild) {
      const onlyRowsChanged = next.rows !== resolvedPreferences.rows
        && next.sort === resolvedPreferences.sort
        && next.columnCount === resolvedPreferences.columnCount;
      onWorkspaceChange(onlyRowsChanged
        ? rebuildWorkspaceZoneRows(workspace, zone, poolGroups, next)
        : rebuildWorkspaceZone(workspace, zone, pool, poolGroups, next));
    }
  };

  const removeColumn = (column: number) => {
    if (interactionLocked) return;
    if (resolvedPreferences.columnCount <= 2) return;
    pendingHeaderFocus.current = column;
    const next = {
      ...resolvedPreferences,
      columnCount: resolvedPreferences.columnCount - 1,
    };
    handlePreferencesChange(next);
  };

  const handleCardKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    card: WorkspaceCardEntryModel,
  ) => {
    if (interactionLocked) return;
    const location = cardLocation(model, card.instanceId);
    if (location === null) return;
    const stack = model.columns[location.column].rows[location.row].cards;
    let focusTarget: WorkspaceCardEntryModel | null = null;
    let moveTarget: WorkspaceMoveTarget | null = null;

    if (event.ctrlKey && event.shiftKey) {
      if (event.key === "ArrowDown" && zone === "deck") {
        moveTarget = {
          zone: "sideboard",
          column: Math.min(location.column, oppositeModel.columnCount - 1),
          beforeInstanceId: null,
        };
      } else if (event.key === "ArrowUp" && zone === "sideboard") {
        moveTarget = {
          zone: "deck",
          column: Math.min(location.column, oppositeModel.columnCount - 1),
          beforeInstanceId: null,
        };
      }
    } else if (event.ctrlKey) {
      if (event.key === "ArrowUp" && location.index > 0) {
        moveTarget = { zone, column: location.column, beforeInstanceId: stack[location.index - 1].instanceId };
      } else if (event.key === "ArrowDown" && location.index < stack.length - 1) {
        moveTarget = {
          zone,
          column: location.column,
          beforeInstanceId: stack[location.index + 2]?.instanceId ?? null,
        };
      } else if (event.key === "ArrowLeft" && location.column > 0) {
        moveTarget = { zone, column: location.column - 1, beforeInstanceId: null };
      } else if (event.key === "ArrowRight" && location.column < model.columnCount - 1) {
        moveTarget = { zone, column: location.column + 1, beforeInstanceId: null };
      }
    } else {
      switch (event.key) {
        case "Enter":
        case " ":
          setPreviewCard(card);
          break;
        case "ArrowUp":
          focusTarget = stack[location.index - 1]
            ?? (location.row === 1 ? crossRowCard(model, location, 0) : null);
          break;
        case "ArrowDown":
          focusTarget = stack[location.index + 1]
            ?? (location.row === 0 && model.rowCount === 2
              ? crossRowCard(model, location, 1)
              : null);
          break;
        case "ArrowLeft":
          focusTarget = horizontalCard(model, location, -1);
          break;
        case "ArrowRight":
          focusTarget = horizontalCard(model, location, 1);
          break;
        case "Home":
          focusTarget = stack[0] ?? null;
          break;
        case "End":
          focusTarget = stack[stack.length - 1] ?? null;
          break;
        case "Escape":
          setPreviewCard(null);
          break;
      }
    }

    if (moveTarget !== null) {
      event.preventDefault();
      dispatchMove(card, moveTarget);
    } else if (focusTarget !== null) {
      event.preventDefault();
      focusCard(focusTarget);
    } else if (["Enter", " ", "Escape"].includes(event.key)) {
      event.preventDefault();
    }
  };

  const renderColumn = (column: WorkspaceBoardModel["columns"][number]) => (
    <CardPoolColumn
      key={column.key}
      column={column}
      sort={model.effectiveSort}
      showHeader={showHeaders}
      canRemove={model.columnCount > 2}
      interactionLocked={interactionLocked}
      dragController={dragController}
      touchDragEnabled={touchDragEnabled}
      touchScrollEnabled={touchScrollEnabled}
      makeDragSource={makeDragSource}
      registerRoot={registerColumn?.(zone, column.column)}
      registerCard={(instanceId) => (element) => {
        if (element === null) cardRefs.current.delete(instanceId);
        else cardRefs.current.set(instanceId, element);
      }}
      registerHeader={(columnNumber) => (element) => {
        if (element === null) headerRefs.current.delete(columnNumber);
        else headerRefs.current.set(columnNumber, element);
      }}
      onRemoveColumn={removeColumn}
      onCardHover={(card) => setPreviewCard(card)}
      onCardActivate={activateCard}
      onCardKeyDown={handleCardKeyDown}
    />
  );

  return (
    <div className="overflow-clip rounded-card border border-hairline bg-black/18 text-fg shadow-[0_10px_26px_rgba(0,0,0,0.2)]">
      <DraftWorkspaceToolbar
        heading={heading}
        deckTypeCounts={deckTypeCounts}
        deckControls={deckControls}
        trailingControls={trailingControls}
        preferences={resolvedPreferences}
        capabilities={poolGroups.workspace_capabilities}
        minusRef={minusRef}
        onChange={handlePreferencesChange}
        interactionLocked={interactionLocked}
        phoneMode={phoneToolbar}
        phoneLayoutDialog={phoneLayoutDialog}
        phonePortraitDeckToolbar={phonePortraitDeckToolbar}
        tabletMode={tabletMode}
        compactDeckTypeCounts={compactDeckTypeCounts}
        visualColumnCapValue={visualColumnCapValue}
        visualColumnCapMax={visualColumnCapMax}
        onVisualColumnCapChange={onVisualColumnCapChange}
      />
      <div
        ref={registerBoard}
        className="overflow-x-hidden bg-black/12"
        data-drop-state={model.drop.state}
        aria-describedby={model.drop.active ? `${model.key}:drop-description` : undefined}
      >
        {model.drop.active && (
          <span
            id={`${model.key}:drop-description`}
            className="sr-only"
          >
            {t(model.drop.descriptionKey!)}
          </span>
        )}
        {visualColumnCap === undefined ? (
          <div className={model.rowCount === 2
          ? "grid w-full grid-cols-[2rem_minmax(0,1fr)] grid-rows-[auto_auto_auto] gap-x-2 p-2"
          : "flex w-full gap-2 p-2"
          }>
          {model.rowCount === 2 && (
            <div
              data-row-headers
              className="col-start-1 row-start-2 row-span-2 grid w-8 shrink-0"
              style={{ gridTemplateRows: "subgrid" }}
            >
              {(["creatures", "nonCreatures"] as const).map((row, index) => (
                <h3
                  key={row}
                  data-row-header={row}
                  className={`flex min-h-0 items-center justify-center overflow-hidden rounded-[6px] border border-hairline bg-white/[0.035] px-1 py-2 text-[10px] font-semibold uppercase text-fg-muted ${index === 1 ? "mt-2" : ""}`}
                >
                  <span className="whitespace-nowrap [writing-mode:vertical-rl] rotate-180">
                    {t(`workspace.rowHeaders.${row}`)}
                  </span>
                </h3>
              ))}
            </div>
          )}
          <div
            data-board-columns
            className={`grid min-w-0 flex-1 gap-x-2 ${model.rowCount === 2
              ? "col-start-2 row-start-1 row-span-3 grid-rows-subgrid"
              : "gap-y-2"
            }`}
            style={{
              gridTemplateColumns: `repeat(${model.columnCount}, minmax(0, 1fr))`,
              ...(model.rowCount === 2 ? { gridTemplateRows: "subgrid" } : {}),
            }}
          >
          {model.columns.map(renderColumn)}
          </div>
          </div>
        ) : (
          <div data-board-columns className="grid min-w-0 gap-y-2 p-2">
            {visualColumnGroups.map((columns, groupIndex) => (
              model.rowCount === 2 ? (
                <div
                  key={columns[0]?.key ?? groupIndex}
                  data-board-column-group={groupIndex}
                  className="grid w-full grid-cols-[2rem_minmax(0,1fr)] grid-rows-[auto_auto_auto] gap-x-2"
                >
                  <div
                    data-row-headers
                    className="col-start-1 row-start-2 row-span-2 grid w-8 shrink-0"
                    style={{ gridTemplateRows: "subgrid" }}
                  >
                    {(["creatures", "nonCreatures"] as const).map((row, index) => (
                      <h3
                        key={row}
                        data-row-header={row}
                        className={`flex min-h-0 items-center justify-center overflow-hidden rounded-[6px] border border-hairline bg-white/[0.035] px-1 py-2 text-[10px] font-semibold uppercase text-fg-muted ${index === 1 ? "mt-2" : ""}`}
                      >
                        <span className="whitespace-nowrap [writing-mode:vertical-rl] rotate-180">
                          {t(`workspace.rowHeaders.${row}`)}
                        </span>
                      </h3>
                    ))}
                  </div>
                  <div
                    className="col-start-2 row-start-1 row-span-3 grid min-w-0 grid-rows-subgrid gap-x-2"
                    style={{
                      gridTemplateColumns: `repeat(${columns.length}, minmax(0, 1fr))`,
                      gridTemplateRows: "subgrid",
                    }}
                  >
                    {columns.map(renderColumn)}
                  </div>
                </div>
              ) : (
                <div
                  key={columns[0]?.key ?? groupIndex}
                  data-board-column-group={groupIndex}
                  className="grid min-w-0 gap-2"
                  style={{ gridTemplateColumns: `repeat(${columns.length}, minmax(0, 1fr))` }}
                >
                  {columns.map(renderColumn)}
                </div>
              )
            ))}
          </div>
        )}
      </div>
      <HoverCardPreview
        card={previewCard?.preview ?? null}
        mode={cardPreviewMode}
        hoverDelayMs={cardPreviewHoverDelayMs}
        onDismiss={() => setPreviewCard(null)}
        mobileLayout="compact"
      />
      <div role="status" className="sr-only" aria-live="polite" aria-atomic="true">
        {announcement}
      </div>
    </div>
  );
}
