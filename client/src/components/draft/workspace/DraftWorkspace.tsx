import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance, DraftPoolGroups } from "../../../adapter/draft-adapter";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import type { CardHoverInfo } from "../../card/CardPreview";
import { PoolPanel } from "../PoolPanel";
import { CardPoolBoard } from "./CardPoolBoard";
import { CompactSideboard } from "./CompactSideboard";
import { DeckTypeCounts } from "./DeckTypeCounts";
import type { DraftWorkspaceDragController, WorkspaceDragSource } from "./useDraftWorkspaceDrag";
import { normalizeWorkspaceForBoardGeometry, resolveWorkspaceRow } from "./workspacePlacement";
import type { DraftWorkspaceFilter, DraftWorkspaceState, DraftZone } from "./types";
import type {
  DraftBoardPreferences,
  DraftBoardSort,
  DraftWorkspacePreferences,
  DraftWorkspaceView,
  ResponsiveDraftLayout,
} from "./workspacePreferences";
import {
  DRAFT_WORKSPACE_COLLAPSED_SIDEBOARD_CARD_WIDTH_PX,
  resolveDraftWorkspaceSideboardCollapsed,
  resolveDraftWorkspaceVisualColumnCap,
  resolveDraftWorkspaceView,
} from "./workspacePreferences";

const DRAG_PREVIEW_SCALE = 0.55;
const DESKTOP_DRAFT_PACK_DRAG_PREVIEW_SCALE = DRAG_PREVIEW_SCALE * 1.5;
const COMPACT_WORKSPACE_DRAG_PREVIEW_SCALE = 0.4;
const DRAG_PREVIEW_GAP = 4;
const DRAG_PREVIEW_OFFSET = 12;
const DESKTOP_DRAFT_COLLAPSED_SIDEBOARD_SCALE = 0.8;

type VisualColumnCapOrientation = "portrait" | "landscape";
type VisualColumnCapPreferenceTarget = "phoneDeckVisualColumnCaps" | "tabletDeckVisualColumnCaps";

interface VisualColumnCapDescriptor {
  value: number;
  maximum: number;
  orientation: VisualColumnCapOrientation;
  target: VisualColumnCapPreferenceTarget;
}

interface DragProjectionRect {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

interface DragProjectionMeasurement extends DragProjectionRect {
  readonly token: number;
  readonly source: WorkspaceDragSource;
  readonly sourceInstanceId: string;
  readonly zone: DraftZone;
  readonly column: number;
  readonly row: number;
  readonly geometryRevision: number;
}

export function shouldShowDraftWorkspaceDeck(
  deckCollapsed: boolean,
  builderCompact: boolean,
): boolean {
  return !deckCollapsed || builderCompact;
}

function dragPreviewPosition(
  clientX: number,
  clientY: number,
  cardCount: number,
  cardWidth: number,
  cardHeight: number,
  maximumScale = DRAG_PREVIEW_SCALE,
) {
  const viewport = window.visualViewport;
  const finiteOrZero = (value: number) => Number.isFinite(value) ? value : 0;
  const nonNegative = (value: number) => Math.max(0, finiteOrZero(value));
  const viewportLeft = finiteOrZero(viewport?.offsetLeft ?? 0);
  const viewportTop = finiteOrZero(viewport?.offsetTop ?? 0);
  const viewportWidth = nonNegative(viewport?.width ?? window.innerWidth);
  const viewportHeight = nonNegative(viewport?.height ?? window.innerHeight);
  const safeCardCount = nonNegative(cardCount);
  const safeCardWidth = nonNegative(cardWidth);
  const safeCardHeight = nonNegative(cardHeight);
  const gapWidth = Math.min(
    Math.max(0, safeCardCount - 1) * DRAG_PREVIEW_GAP,
    viewportWidth,
  );
  const widthScale = safeCardCount * safeCardWidth > 0
    ? (viewportWidth - gapWidth) / (safeCardCount * safeCardWidth)
    : maximumScale;
  const heightScale = safeCardHeight > 0
    ? viewportHeight / safeCardHeight
    : maximumScale;
  const scale = Math.max(0, Math.min(maximumScale, widthScale, heightScale));
  const overlayWidth = safeCardCount * safeCardWidth * scale + gapWidth;
  const overlayHeight = safeCardHeight * scale;
  const maximumLeft = viewportLeft + viewportWidth - overlayWidth;
  const maximumTop = viewportTop + viewportHeight - overlayHeight;

  return {
    left: Math.min(Math.max(finiteOrZero(clientX) + DRAG_PREVIEW_OFFSET, viewportLeft), maximumLeft),
    top: Math.min(Math.max(finiteOrZero(clientY) + DRAG_PREVIEW_OFFSET, viewportTop), maximumTop),
    cardWidth: safeCardWidth * scale,
    cardHeight: safeCardHeight * scale,
    gap: safeCardCount > 1 ? gapWidth / (safeCardCount - 1) : 0,
  };
}

function DragPreviewCard({ image, instanceId, width, height }: {
  image: { readonly src: string | null; readonly alt: string };
  instanceId: string;
  width: number;
  height: number;
}) {
  return image.src === null
    ? <span aria-hidden="true" data-drag-instance-id={instanceId} className="block rounded bg-neutral-900" style={{ width, height }} />
    : <img data-drag-instance-id={instanceId} src={image.src} alt="" draggable={false} className="rounded object-contain" style={{ width, height }} />;
}

function WorkspaceDragProjectionOverlay({
  projection,
  rect,
  onSettled,
}: {
  projection: import("./WorkspaceCard").WorkspaceDragProjection & {
    readonly zone: DraftZone;
    readonly geometryRevision: number;
  };
  rect: DragProjectionRect;
  onSettled(): void;
}) {
  const shouldReduceMotion = useReducedMotion();
  const previewImage = projection.previewImage;
  return (
    <motion.div
      aria-hidden="true"
      data-drag-projection={projection.sourceInstanceId}
      data-drag-projection-source-instance-id={projection.sourceInstanceId}
      data-drag-projection-zone={projection.zone}
      data-drag-projection-column={projection.column}
      data-drag-projection-row={projection.row}
      data-drag-projection-geometry-revision={projection.geometryRevision}
      data-workspace-drag-motion={shouldReduceMotion ? "reduced" : "enabled"}
      className="pointer-events-none fixed z-50 overflow-hidden rounded-md"
      initial={{
        left: projection.origin.left,
        top: projection.origin.top,
        width: projection.origin.width,
        height: projection.origin.height,
      }}
      animate={{ left: rect.left, top: rect.top, width: rect.width, height: rect.height }}
      transition={shouldReduceMotion ? { duration: 0 } : { duration: 0.16, ease: "easeOut" }}
      onAnimationComplete={onSettled}
    >
      {previewImage?.src === null || previewImage === undefined ? (
        <span aria-hidden="true" className="block h-full w-full bg-neutral-900" />
      ) : (
        <img src={previewImage.src} alt="" draggable={false} className="h-full w-full object-cover shadow-lg" />
      )}
    </motion.div>
  );
}

export interface DraftWorkspaceProps {
  pool: readonly DraftCardInstance[];
  poolGroups: DraftPoolGroups;
  workspace: DraftWorkspaceState;
  preferences: DraftWorkspacePreferences;
  interactionLocked?: boolean;
  dragController?: DraftWorkspaceDragController;
  deckControls?: ReactNode;
  compactDeckControls?: ReactNode;
  onWorkspaceChange(next: DraftWorkspaceState): void;
  onPreferencesChange(next: DraftWorkspacePreferences): void;
  onCardHover?(info: CardHoverInfo | null): void;
  responsiveLayout?: ResponsiveDraftLayout;
  mobileOverlay?: boolean;
  mobileWorkspaceOpen?: boolean;
  onMobileWorkspaceOpenChange?(open: boolean): void;
  responsiveContext?: "draft" | "builder";
}

export function DraftWorkspace({
  pool,
  poolGroups,
  workspace,
  preferences,
  interactionLocked = false,
  dragController,
  deckControls,
  compactDeckControls,
  onWorkspaceChange,
  onPreferencesChange,
  onCardHover,
  responsiveLayout = "desktop",
  mobileOverlay = false,
  mobileWorkspaceOpen = false,
  onMobileWorkspaceOpenChange,
  responsiveContext = "draft",
}: DraftWorkspaceProps) {
  const { t } = useTranslation(["draft", "common"]);
  const [filter, setFilter] = useState<DraftWorkspaceFilter>("deck");
  const [compactSort, setCompactSort] = useState<DraftBoardSort>(preferences.deck.sort);
  const [deckCollapsed, setDeckCollapsed] = useState(false);
  const [phoneDraftView, setPhoneDraftView] = useState<DraftWorkspaceView>("compact");
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);
  const cardPreviewMode = usePreferencesStore((state) => state.draftCardPreviewMode);
  const normalizedWorkspaceSourceRef = useRef<DraftWorkspaceState | null>(null);
  const boardPreferences = { deck: preferences.deck, sideboard: preferences.sideboard };
  const normalized = normalizeWorkspaceForBoardGeometry(
    workspace,
    pool,
    poolGroups,
    boardPreferences,
  );
  const phoneWorkspaceOverlayOpen = mobileOverlay
    && mobileWorkspaceOpen
    && (responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape");
  const tabletDraftLayout = responsiveContext === "draft"
    && (responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape");
  const phoneDraftOverlay = responsiveContext === "draft" && phoneWorkspaceOverlayOpen;
  const view: DraftWorkspaceView = resolveDraftWorkspaceView(
    preferences.explicitView,
    viewportWidth,
    responsiveLayout,
  );
  const renderedView: DraftWorkspaceView = tabletDraftLayout
    ? "board"
    : phoneDraftOverlay
      ? phoneDraftView
      : view;
  const sideboardCollapsed = resolveDraftWorkspaceSideboardCollapsed(
    preferences.sideboardCollapsed,
    viewportWidth,
    responsiveLayout,
    responsiveContext,
    preferences.builderPhoneSideboardCollapsed,
  );
  const sideboardDropActive = dragController?.activeTarget?.zone === "sideboard";
  const dragPreview = dragController?.dragPreview;
  const desktopWorkspace = responsiveLayout === "desktop";
  const workspaceProjection = desktopWorkspace ? dragController?.workspaceProjection : undefined;
  const activeDragTarget = workspaceProjection?.target;
  const geometryRevision = dragController?.geometryRevision;
  const workspaceShellRef = useRef<HTMLElement>(null);
  const [workspaceProjectionRect, setWorkspaceProjectionRect] = useState<DragProjectionMeasurement | undefined>();
  const [deckVisualColumnWidth, setDeckVisualColumnWidth] = useState<number | undefined>();
  const workspaceDragSource = workspaceProjection === undefined || workspaceProjection === null
    ? undefined
    : workspaceProjection.source;
  const canonicalSource = workspaceDragSource === undefined
    ? undefined
    : normalized.placements[workspaceDragSource.instanceIds[0]];
  const projectionRow = activeDragTarget === null || activeDragTarget === undefined || workspaceDragSource === undefined
    ? undefined
    : activeDragTarget.row ?? resolveWorkspaceRow(
      workspaceDragSource.instanceIds[0],
      boardPreferences[activeDragTarget.zone],
      poolGroups,
    );
  const workspaceDragProjectionTarget = workspaceDragSource !== undefined
    && activeDragTarget !== null
    && activeDragTarget !== undefined
    && canonicalSource !== undefined
    && projectionRow !== undefined
    && geometryRevision !== undefined
    ? {
      source: workspaceDragSource,
      card: workspaceDragSource.cards[0],
      sourceInstanceId: workspaceDragSource.instanceIds[0],
      zone: activeDragTarget.zone,
      column: activeDragTarget.column,
      row: projectionRow,
      geometryRevision,
      origin: workspaceDragSource.origin,
      previewImage: workspaceDragSource.previewImage,
    }
    : undefined;
  const hasTokenLocalWorkspaceProjection = workspaceProjection?.token !== undefined
    && workspaceProjection.token !== 0;
  const targetIsDifferent = canonicalSource?.zone !== workspaceDragProjectionTarget?.zone
    || canonicalSource?.column !== workspaceDragProjectionTarget?.column
    || canonicalSource?.row !== workspaceDragProjectionTarget?.row;
  const showWorkspaceProjection = hasTokenLocalWorkspaceProjection
    && workspaceDragProjectionTarget !== undefined
    && (workspaceProjection?.hasLeftCanonicalTarget === true || targetIsDifferent || workspaceProjection?.settling === true);
  const workspaceProjectionSource = workspaceDragProjectionTarget?.source;
  const workspaceProjectionSourceId = workspaceDragProjectionTarget?.sourceInstanceId;
  const workspaceProjectionZone = workspaceDragProjectionTarget?.zone;
  const workspaceProjectionColumn = workspaceDragProjectionTarget?.column;
  const workspaceProjectionRow = workspaceDragProjectionTarget?.row;
  const workspaceProjectionGeometryRevision = workspaceDragProjectionTarget?.geometryRevision;

  useLayoutEffect(() => {
    if (
      workspaceProjectionSource === undefined
      || workspaceProjectionSourceId === undefined
      || workspaceProjectionZone === undefined
      || workspaceProjectionColumn === undefined
      || workspaceProjectionRow === undefined
      || workspaceProjectionGeometryRevision === undefined
    ) {
      setWorkspaceProjectionRect(undefined);
      return;
    }
    const slot = workspaceShellRef.current?.querySelector<HTMLElement>(
      `[data-drag-projection-slot="${workspaceProjectionSourceId}"]`,
    );
    if (slot === null || slot === undefined) return;
    const { left, top, width, height } = slot.getBoundingClientRect();
    const next = {
      token: workspaceProjection!.token,
      source: workspaceProjectionSource,
      sourceInstanceId: workspaceProjectionSourceId,
      zone: workspaceProjectionZone,
      column: workspaceProjectionColumn,
      row: workspaceProjectionRow,
      geometryRevision: workspaceProjectionGeometryRevision,
      left,
      top,
      width,
      height,
    };
    setWorkspaceProjectionRect((current) => (
      current?.token === next.token
      && current.source === next.source
      && current.sourceInstanceId === next.sourceInstanceId
      && current.zone === next.zone
      && current.column === next.column
      && current.row === next.row
      && current.geometryRevision === next.geometryRevision
      && current.left === next.left
      && current.top === next.top
      && current.width === next.width
      && current.height === next.height
        ? current
        : next
    ));
  }, [
    workspaceProjectionColumn,
    workspaceProjectionGeometryRevision,
    workspaceProjectionRow,
    workspaceProjectionSource,
    workspaceProjectionSourceId,
    workspaceProjectionZone,
    workspaceProjection?.token,
  ]);
  const projectionMeasurementMatches = workspaceDragProjectionTarget !== undefined
    && workspaceProjectionRect !== undefined
    && workspaceProjectionRect.token === workspaceProjection?.token
    && workspaceProjectionRect.source === workspaceDragSource
    && workspaceProjectionRect.sourceInstanceId === workspaceDragProjectionTarget.sourceInstanceId
    && workspaceProjectionRect.zone === workspaceDragProjectionTarget.zone
    && workspaceProjectionRect.column === workspaceDragProjectionTarget.column
    && workspaceProjectionRect.row === workspaceDragProjectionTarget.row
    && workspaceProjectionRect.geometryRevision === workspaceDragProjectionTarget.geometryRevision;
  const workspaceDragProjection = !showWorkspaceProjection
    || workspaceProjectionRect === undefined
    || workspaceProjectionRect.source !== workspaceDragSource
    ? undefined
    : projectionMeasurementMatches
      ? workspaceDragProjectionTarget
      : {
        card: workspaceDragSource.cards[0],
        sourceInstanceId: workspaceProjectionRect.sourceInstanceId,
        zone: workspaceProjectionRect.zone,
        column: workspaceProjectionRect.column,
        row: workspaceProjectionRect.row,
        geometryRevision: workspaceProjectionRect.geometryRevision,
        origin: workspaceDragSource.origin,
        previewImage: workspaceDragSource.previewImage,
      };
  const pointerProjectionRect = workspaceProjection === undefined || workspaceProjection === null
    ? undefined
    : dragPreviewPosition(
      workspaceProjection.clientX,
      workspaceProjection.clientY,
      1,
      workspaceDragSource?.previewWidth ?? 0,
      workspaceDragSource?.previewHeight ?? 0,
    );
  const renderedWorkspaceProjectionRect = projectionMeasurementMatches
    ? workspaceProjectionRect
    : pointerProjectionRect === undefined
      ? undefined
      : {
        left: pointerProjectionRect.left,
        top: pointerProjectionRect.top,
        width: pointerProjectionRect.cardWidth,
        height: pointerProjectionRect.cardHeight,
      };
  const workspaceProjectionDestinationReady = workspaceProjection?.settling === true
    && projectionMeasurementMatches
    && canonicalSource?.zone === workspaceDragProjectionTarget?.zone
    && canonicalSource?.column === workspaceDragProjectionTarget?.column
    && canonicalSource?.row === workspaceDragProjectionTarget?.row;
  useLayoutEffect(() => {
    if (!workspaceProjectionDestinationReady || workspaceProjection?.token === undefined || workspaceProjection.token === 0) return;
    dragController?.markWorkspaceProjectionDestinationReady?.(workspaceProjection.token, workspaceProjectionGeometryRevision!);
  }, [dragController, workspaceProjection?.token, workspaceProjectionDestinationReady, workspaceProjectionGeometryRevision]);
  const draggedSourceInstanceId = showWorkspaceProjection
    ? workspaceDragSource?.instanceIds[0]
    : undefined;
  const fixedDragPreview = dragPreview?.source.kind === "workspace" && desktopWorkspace
    ? undefined
    : dragPreview;
  const dragPreviewStyle = fixedDragPreview === null || fixedDragPreview === undefined
    ? undefined
    : dragPreviewPosition(
      fixedDragPreview.clientX,
      fixedDragPreview.clientY,
      fixedDragPreview.source.cards.length,
      fixedDragPreview.source.previewWidth,
      fixedDragPreview.source.previewHeight,
      fixedDragPreview.source.kind === "workspace"
        ? COMPACT_WORKSPACE_DRAG_PREVIEW_SCALE
        : desktopWorkspace
          ? DESKTOP_DRAFT_PACK_DRAG_PREVIEW_SCALE
          : DRAG_PREVIEW_SCALE,
    );
  const counts = Object.values(normalized.placements).reduce(
    (totals, placement) => ({ ...totals, [placement.zone]: totals[placement.zone] + 1 }),
    { deck: 0, sideboard: 0 },
  );
  const deckTypeCounts = pool.reduce(
    (totals, card) => {
      if (normalized.placements[card.instance_id]?.zone !== "deck") return totals;
      return {
        creatures: totals.creatures + (/\bcreature\b/i.test(card.type_line) ? 1 : 0),
        lands: totals.lands + (/\bland\b/i.test(card.type_line) ? 1 : 0),
      };
    },
    {
      creatures: 0,
      lands: normalized.virtualBasics.filter((basic) => (
        normalized.placements[basic.instanceId]?.zone === "deck"
      )).length,
    },
  );
  const phoneLayout = responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape";
  const tabletLayout = responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape";
  const builderPhoneLayout = responsiveContext === "builder" && phoneLayout;
  const builderPhonePortraitLayout = builderPhoneLayout && responsiveLayout === "phone-portrait";
  const builderPhoneLandscapeLayout = builderPhoneLayout && responsiveLayout === "phone-landscape";
  const draftPhoneLayout = responsiveContext === "draft" && phoneLayout;
  const phoneDraftTextLayout = draftPhoneLayout && renderedView === "compact";
  const tabletPortraitLayout = responsiveLayout === "tablet-portrait";
  const visualColumnCap = resolveDraftWorkspaceVisualColumnCap(
    responsiveLayout,
    responsiveContext,
    preferences.phoneDeckVisualColumnCaps,
    preferences.tabletDeckVisualColumnCaps,
  );
  const visualColumnCapDescriptor: VisualColumnCapDescriptor | undefined = visualColumnCap === undefined
    ? undefined
    : {
      value: visualColumnCap,
      maximum: 10,
      orientation: responsiveLayout === "phone-portrait" || responsiveLayout === "tablet-portrait"
        ? "portrait"
        : "landscape",
      target: responsiveContext === "builder" && tabletLayout
        ? "tabletDeckVisualColumnCaps"
        : "phoneDeckVisualColumnCaps",
    };
  const touchDragEnabled = responsiveLayout !== "desktop";
  const mobileOverlayActive = mobileOverlay && phoneLayout;
  const tabletLandscapeLayout = responsiveLayout === "tablet-landscape";
  const expandedDraftLandscapeLayout = responsiveContext === "draft"
    && (responsiveLayout === "phone-landscape" || tabletLandscapeLayout)
    && !sideboardCollapsed;
  const builderPhoneOrTabletLayout = responsiveContext === "builder" && (phoneLayout || tabletLayout);
  const builderCompact = builderPhoneOrTabletLayout && renderedView === "compact";
  const builderTouchVisualToolbar = responsiveContext === "builder"
    && responsiveLayout !== "desktop"
    && renderedView === "board";
  const showDeckContents = shouldShowDraftWorkspaceDeck(deckCollapsed, builderCompact);
  const collapsedSideboardCardWidth = responsiveContext === "draft" && responsiveLayout === "desktop"
    ? `clamp(${208 * DESKTOP_DRAFT_COLLAPSED_SIDEBOARD_SCALE}px, 16vw, ${DRAFT_WORKSPACE_COLLAPSED_SIDEBOARD_CARD_WIDTH_PX * DESKTOP_DRAFT_COLLAPSED_SIDEBOARD_SCALE}px)`
    : `${DRAFT_WORKSPACE_COLLAPSED_SIDEBOARD_CARD_WIDTH_PX}px`;
  const collapsedCompositionClass = phoneDraftTextLayout
    ? "flex h-full min-h-0 min-w-0 flex-col overflow-hidden"
    : builderCompact
    ? tabletLayout
      ? "flex h-full min-h-0 min-w-0 flex-col overflow-hidden"
      : "h-full min-h-0 min-w-0"
    : tabletPortraitLayout
    ? sideboardCollapsed
      ? "grid h-full min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_48px] gap-2"
      : "relative h-full min-h-0 min-w-0"
    : draftPhoneLayout && responsiveLayout === "phone-portrait"
      ? sideboardCollapsed
        ? "grid h-full min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_48px] gap-2"
        : "grid h-full min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_minmax(168px,42%)] gap-2"
    : builderPhonePortraitLayout
    ? "flex min-h-full min-w-0 flex-col gap-2"
    : expandedDraftLandscapeLayout
    ? "grid h-full min-h-0 min-w-0 grid-cols-1 grid-rows-[minmax(0,1fr)_minmax(152px,32vh)] gap-2"
    : (builderPhoneLayout || tabletLandscapeLayout) && !sideboardCollapsed
    ? responsiveLayout === "phone-landscape"
      ? "grid h-full min-h-0 min-w-0 grid-cols-[minmax(0,1fr)_172px] gap-2"
      : responsiveLayout === "tablet-landscape"
        ? "grid h-full min-h-0 min-w-0 grid-cols-[minmax(0,1fr)_196px] gap-2"
      : "grid h-full min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_minmax(152px,32vh)] gap-2"
    : responsiveLayout === "desktop"
    ? "grid min-w-0 grid-cols-1 gap-[clamp(4px,1vw,16px)] lg:grid-cols-[minmax(0,1fr)_minmax(0,calc(var(--collapsed-sideboard-card-width)_+_2px))]"
    : responsiveLayout === "phone-landscape"
      ? responsiveContext === "builder"
        ? "grid h-full min-h-0 min-w-0 grid-cols-[minmax(0,1fr)_40px] gap-2"
        : sideboardCollapsed
          ? "grid h-full min-h-0 min-w-0 grid-cols-[minmax(0,1fr)_48px] gap-2"
          : "grid h-full min-h-0 min-w-0 grid-cols-[minmax(0,1fr)_112px] gap-2"
      : responsiveLayout === "phone-portrait"
        ? responsiveContext === "builder"
          ? "grid h-full min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_auto] gap-2"
          : "grid h-full min-h-0 min-w-0 grid-rows-[minmax(0,1fr)_150px] gap-2"
        : "grid h-full min-h-0 min-w-0 grid-cols-[minmax(0,1fr)_48px] gap-2";

  const compactBuilderPrimaryControls = responsiveContext === "builder" ? compactDeckControls : undefined;
  const compactBuilderCount = builderPhoneOrTabletLayout ? (
    <DeckTypeCounts counts={deckTypeCounts} compact={builderPhoneLayout} />
  ) : undefined;
  const compactBuilderTrailingControls = responsiveContext === "builder" ? (
    <button
      type="button"
      onClick={() => onPreferencesChange({ ...preferences, explicitView: "board" })}
      className={`${builderPhoneOrTabletLayout ? "min-h-11" : "min-h-8"} shrink-0 whitespace-nowrap px-2 text-xs font-medium text-jade`}
    >
      {t("limitedDeck.visualBuilder")}
    </button>
  ) : undefined;
  const phoneDraftCloseControl = phoneDraftOverlay ? (
    <button
      type="button"
      onClick={() => onMobileWorkspaceOpenChange?.(false)}
      aria-label={t("common:actions.close")}
      className="min-h-11 min-w-11 shrink-0 rounded-[6px] border border-hairline bg-slate-950 text-lg text-fg"
    >
      <span aria-hidden="true">×</span>
    </button>
  ) : undefined;
  const phoneDraftTextControls = phoneDraftOverlay ? (
    <>
      <button
        type="button"
        data-phone-draft-view="visual"
        onClick={() => setPhoneDraftView("board")}
        className="min-h-11 shrink-0 whitespace-nowrap px-2 text-xs font-medium text-jade"
      >
        {t("limitedDeck.visualBuilder")}
      </button>
      {phoneDraftCloseControl}
    </>
  ) : undefined;
  const phoneDraftVisualControls = phoneDraftOverlay ? (
    <>
      <button
        type="button"
        data-phone-draft-view="text"
        onClick={() => setPhoneDraftView("compact")}
        className="min-h-11 shrink-0 whitespace-nowrap px-2 text-xs font-medium text-jade"
      >
        {t("limitedDeck.textBuilder")}
      </button>
      {phoneDraftCloseControl}
    </>
  ) : undefined;
  const visualBuilderControls = builderPhoneOrTabletLayout ? (
    <button
      type="button"
      onClick={() => onPreferencesChange({ ...preferences, explicitView: "compact" })}
      className="ml-auto min-h-11 shrink-0 whitespace-nowrap px-2 text-xs font-medium text-jade"
    >
      {t("limitedDeck.textBuilder")}
    </button>
  ) : undefined;

  useLayoutEffect(() => {
    if (responsiveContext !== "draft" || responsiveLayout === "desktop" || renderedView !== "board") {
      setDeckVisualColumnWidth(undefined);
      return;
    }
    const deckCardArea = workspaceShellRef.current?.querySelector<HTMLElement>(
      '[data-zone="deck"] [data-board-column] [data-card-area]',
    );
    if (deckCardArea === null || deckCardArea === undefined) return;
    const measure = () => {
      const style = getComputedStyle(deckCardArea);
      const next = deckCardArea.getBoundingClientRect().width
        - (Number.parseFloat(style.borderLeftWidth) || 0)
        - (Number.parseFloat(style.borderRightWidth) || 0);
      if (next <= 0) return;
      setDeckVisualColumnWidth((current) => current === next ? current : next);
    };
    measure();
    if (typeof ResizeObserver !== "function") return;
    const observer = new ResizeObserver(measure);
    observer.observe(deckCardArea);
    return () => observer.disconnect();
  }, [renderedView, responsiveContext, responsiveLayout, viewportWidth, visualColumnCap]);

  useEffect(() => {
    if (normalized === workspace) {
      normalizedWorkspaceSourceRef.current = null;
    } else if (!interactionLocked && normalizedWorkspaceSourceRef.current !== workspace) {
      normalizedWorkspaceSourceRef.current = workspace;
      onWorkspaceChange(normalized);
    }
  }, [interactionLocked, normalized, onWorkspaceChange, workspace]);

  useLayoutEffect(() => {
    const mediaQueries = typeof window.matchMedia === "function"
      ? [window.matchMedia("(min-width: 1024px)")]
      : [];
    const refreshWidth = () => setViewportWidth(window.innerWidth);
    const mediaRegistrations = mediaQueries.map((query) => {
      if (typeof query.addEventListener === "function") {
        query.addEventListener("change", refreshWidth);
        return { query, modern: true };
      }
      query.addListener(refreshWidth);
      return { query, modern: false };
    });

    refreshWidth();
    window.addEventListener("resize", refreshWidth);

    return () => {
      window.removeEventListener("resize", refreshWidth);
      for (const { query, modern } of mediaRegistrations) {
        if (modern) query.removeEventListener("change", refreshWidth);
        else query.removeListener(refreshWidth);
      }
    };
  }, []);

  const setBoardPreferences = (zone: DraftZone, next: DraftBoardPreferences) => {
    if (interactionLocked) return;
    onPreferencesChange({ ...preferences, [zone]: next });
  };
  const toggleSideboard = () => {
    if (interactionLocked) return;
    if (builderPhoneLayout) {
      onPreferencesChange({
        ...preferences,
        builderPhoneSideboardCollapsed: !sideboardCollapsed,
      });
      return;
    }
    onPreferencesChange({ ...preferences, sideboardCollapsed: !sideboardCollapsed });
  };
  const deck = (
    <section
      aria-label={t("workspace.zone.deck")}
      data-zone="deck"
      className={builderCompact
        ? "flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
        : builderPhonePortraitLayout
        ? "min-w-0 shrink-0"
        : `min-h-0 min-w-0 flex-1 ${(
          tabletLayout || phoneWorkspaceOverlayOpen || builderPhoneLandscapeLayout
        ) ? "overflow-y-auto overscroll-contain" : ""}`}
    >
      {renderedView === "board" ? (
        <CardPoolBoard
          heading={phoneLayout || (responsiveContext === "builder" && tabletLayout)
            ? undefined
            : t("workspace.count.deck", { count: counts.deck })}
          deckTypeCounts={deckTypeCounts}
          deckControls={deckControls}
          trailingControls={phoneDraftVisualControls ?? visualBuilderControls}
          zone="deck"
          pool={pool}
          poolGroups={poolGroups}
          workspace={normalized}
          preferences={preferences.deck}
          cardPreviewMode={cardPreviewMode}
          cardPreviewHoverDelayMs={0}
          oppositeZonePreferences={preferences.sideboard}
          onWorkspaceChange={onWorkspaceChange}
          onPreferencesChange={(next) => setBoardPreferences("deck", next)}
          interactionLocked={interactionLocked}
          dragController={dragController}
          registerBoard={dragController?.registerBoard("deck")}
          registerColumn={dragController?.registerColumn}
          dropState={dragController?.dropState("deck")}
          visualColumnCap={visualColumnCap}
          forceShowHeaders={phoneLayout}
          phoneToolbar={phoneLayout || tabletLayout}
          phoneLayoutDialog={phoneLayout || tabletLayout}
          phonePortraitDeckToolbar={responsiveLayout === "phone-portrait"}
          tabletMode={tabletLayout}
          responsiveDraftGrouping={responsiveContext === "draft" && (phoneLayout || tabletLayout)}
          compactPhoneDraft={draftPhoneLayout}
          compactDeckTypeCounts={phoneLayout}
          builderTouchVisualToolbar={builderTouchVisualToolbar}
          visualColumnCapValue={visualColumnCapDescriptor?.value}
          visualColumnCapMax={visualColumnCapDescriptor?.maximum}
          onVisualColumnCapChange={(next) => {
            if (interactionLocked) return;
            if (visualColumnCapDescriptor === undefined) return;
            onPreferencesChange({
              ...preferences,
              [visualColumnCapDescriptor.target]: {
                ...preferences[visualColumnCapDescriptor.target],
                [visualColumnCapDescriptor.orientation]: next,
              },
            });
          }}
          touchDragEnabled={touchDragEnabled}
          touchScrollEnabled={builderPhoneLayout || tabletLayout}
          desktopCardAreaMinHeight={responsiveLayout === "desktop"}
          draggedSourceInstanceId={draggedSourceInstanceId}
          dragProjection={draggedSourceInstanceId !== undefined && workspaceDragProjectionTarget?.zone === "deck" ? workspaceDragProjectionTarget : undefined}
        />
      ) : (
        responsiveLayout === "desktop" ? (
          <>
            <h2 className="border-b border-hairline bg-surface-panel px-4 py-3 font-display text-base font-semibold text-fg">
              {t("workspace.count.deck", { count: counts.deck })}
            </h2>
            <PoolPanel
              onCardHover={onCardHover}
              controlledWorkspace={{
                pool,
                poolGroups,
                workspace: normalized,
                preferences: boardPreferences,
                filter,
                sort: compactSort,
                onFilterChange: setFilter,
                onSortChange: setCompactSort,
                onWorkspaceChange: (next) => { if (!interactionLocked) onWorkspaceChange(next); },
                compactPrimaryControls: compactBuilderPrimaryControls,
                compactCount: phoneDraftTextLayout
                  ? <DeckTypeCounts counts={deckTypeCounts} compact />
                  : compactBuilderCount,
                compactTrailingControls: phoneDraftTextControls ?? compactBuilderTrailingControls,
                builderCompact: builderCompact || phoneDraftTextLayout,
              }}
            />
          </>
        ) : <>
          {!phoneLayout && !builderCompact && <h2 className="flex items-center gap-2 border-b border-hairline bg-surface-panel px-4 py-3 font-display text-base font-semibold text-fg">
            <span className="min-w-0 flex-1 truncate">{t("workspace.count.deck", { count: counts.deck })}</span>
            {tabletLayout && !builderCompact && (
              <button
                type="button"
                data-deck-collapse-toggle
                aria-expanded={!deckCollapsed}
                aria-label={deckCollapsed ? t("workspace.sideboard.expand", { count: counts.deck }) : t("workspace.sideboard.collapse")}
                onClick={() => setDeckCollapsed((collapsed) => !collapsed)}
                className="h-8 w-8 shrink-0 rounded-[6px] border border-hairline bg-slate-950/72 text-fg-muted"
              >
                <span aria-hidden="true">{deckCollapsed ? "▼" : "▲"}</span>
              </button>
            )}
          </h2>}
          {showDeckContents && (
            <PoolPanel
              onCardHover={onCardHover}
              controlledWorkspace={{
                pool,
                poolGroups,
                workspace: normalized,
                preferences: boardPreferences,
                filter,
                sort: compactSort,
                onFilterChange: setFilter,
                onSortChange: setCompactSort,
                onWorkspaceChange: (next) => { if (!interactionLocked) onWorkspaceChange(next); },
                compactPrimaryControls: compactBuilderPrimaryControls,
                compactCount: phoneDraftTextLayout
                  ? <DeckTypeCounts counts={deckTypeCounts} compact />
                  : compactBuilderCount,
                compactTrailingControls: phoneDraftTextControls ?? compactBuilderTrailingControls,
                builderCompact: builderCompact || phoneDraftTextLayout,
              }}
            />
          )}
        </>
      )}
    </section>
  );
  return (
    <>
    {phoneWorkspaceOverlayOpen && (
      <div
        aria-hidden="true"
        data-mobile-workspace-scrim
        className="fixed inset-0 z-30 bg-slate-950 touch-none overscroll-contain"
        onWheel={(event) => event.preventDefault()}
      />
    )}
    <section
      ref={workspaceShellRef}
      aria-label={t("workspace.shell.label")}
      data-responsive-workspace-layout={responsiveLayout}
      className={mobileOverlayActive
        ? mobileWorkspaceOpen
          ? "fixed z-[35] flex min-h-0 flex-col overflow-hidden overscroll-contain rounded-card bg-slate-950 text-fg shadow-panel"
          : "hidden"
        : responsiveLayout === "desktop"
          ? "relative flex min-h-0 w-full flex-col overflow-hidden rounded-card surface-card text-fg shadow-panel"
          : builderPhonePortraitLayout
            ? "relative flex h-full min-h-0 w-full flex-col overflow-y-auto overscroll-contain rounded-card surface-card text-fg shadow-panel"
          : "relative flex h-full min-h-0 w-full flex-col overflow-hidden rounded-card surface-card text-fg shadow-panel"}
      style={mobileOverlayActive && mobileWorkspaceOpen
        ? responsiveLayout === "phone-landscape"
          ? { inset: "58px 9px 58px", padding: responsiveContext === "draft" ? 4 : 6 }
          : { inset: "52px 9px 112px", padding: responsiveContext === "draft" ? 4 : 7 }
        : undefined}
    >
      <div role="status" aria-live="polite" aria-atomic="true" className="sr-only">
        {interactionLocked ? t("workspace.locked") : dragController?.announcement ?? ""}
      </div>
      <fieldset disabled={interactionLocked} className="contents">
        {sideboardCollapsed || builderPhoneLayout || draftPhoneLayout || tabletLayout ? (
          <div
            data-workspace-composition="collapsed"
            style={{
              "--collapsed-sideboard-card-width": collapsedSideboardCardWidth,
            } as CSSProperties}
            className={collapsedCompositionClass}
          >
            {deck}
            {!phoneDraftTextLayout && !deckCollapsed && !builderCompact && <section
              aria-label={t("workspace.zone.sideboard")}
              data-zone="sideboard"
              className={tabletPortraitLayout
                ? sideboardCollapsed
                  ? "h-full min-h-0 min-w-0"
                  : "absolute inset-0 z-20 min-h-0 min-w-0"
                : builderPhoneLandscapeLayout || tabletLandscapeLayout || draftPhoneLayout
                ? "h-full min-h-0 min-w-0"
                : builderPhonePortraitLayout
                  ? "min-w-0 shrink-0"
                  : "min-w-0"}
            >
              <CompactSideboard
                pool={pool}
                poolGroups={poolGroups}
                workspace={normalized}
                preferences={boardPreferences}
                interactionLocked={interactionLocked}
                dropActive={sideboardDropActive}
                registerCardArea={dragController?.registerCollapsedSideboard}
                collapsed={sideboardCollapsed}
                {...(dragController === undefined ? {} : { dragController })}
                onToggle={toggleSideboard}
                onWorkspaceChange={(next) => { if (!interactionLocked) onWorkspaceChange(next); }}
                onCardHover={interactionLocked ? undefined : onCardHover}
                responsiveLayout={responsiveLayout}
                responsiveContext={responsiveContext}
                visualColumnCap={responsiveContext === "draft" && responsiveLayout !== "desktop"
                  ? visualColumnCap
                  : undefined}
                visualColumnWidth={responsiveContext === "draft" && responsiveLayout !== "desktop"
                  ? deckVisualColumnWidth
                  : undefined}
                touchDragEnabled={touchDragEnabled}
                draggedSourceInstanceId={draggedSourceInstanceId}
                dragProjection={draggedSourceInstanceId !== undefined && workspaceDragProjectionTarget?.zone === "sideboard" ? workspaceDragProjectionTarget : undefined}
              />
            </section>}
          </div>
        ) : (
          <div
            data-workspace-composition="expanded"
            className="flex min-w-0 flex-col gap-4"
          >
            {deck}
            <section
              aria-label={t("workspace.zone.sideboard")}
              data-zone="sideboard"
              className="min-w-0"
            >
          <CardPoolBoard
            heading={t("workspace.count.sideboard", { count: counts.sideboard })}
            trailingControls={(
              <button
                type="button"
                aria-label={t("workspace.sideboard.collapse")}
                title={t("workspace.sideboard.collapse")}
                aria-expanded={true}
                disabled={interactionLocked}
                onClick={toggleSideboard}
                className="h-9 w-9 shrink-0 rounded-[6px] border border-hairline bg-slate-950/72 text-fg-muted transition-colors hover:border-hairline-hover hover:text-fg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-jade/30 disabled:cursor-not-allowed disabled:opacity-40"
              >
                <span
                  aria-hidden="true"
                  className={tabletLandscapeLayout ? "block rotate-90" : "block"}
                >
                  ▲
                </span>
              </button>
            )}
            zone="sideboard"
            pool={pool}
            poolGroups={poolGroups}
            workspace={normalized}
            preferences={preferences.sideboard}
            cardPreviewMode={cardPreviewMode}
            cardPreviewHoverDelayMs={0}
            oppositeZonePreferences={preferences.deck}
            onWorkspaceChange={onWorkspaceChange}
            onPreferencesChange={(next) => setBoardPreferences("sideboard", next)}
            interactionLocked={interactionLocked}
            dragController={dragController}
            registerBoard={dragController?.registerBoard("sideboard")}
            registerColumn={dragController?.registerColumn}
            dropState={dragController?.dropState("sideboard")}
            forceShowHeaders={phoneLayout}
            phoneToolbar={phoneLayout}
            touchDragEnabled={touchDragEnabled}
            touchScrollEnabled={builderPhoneLayout}
            desktopCardAreaMinHeight={responsiveLayout === "desktop"}
            draggedSourceInstanceId={draggedSourceInstanceId}
            dragProjection={draggedSourceInstanceId !== undefined && workspaceDragProjectionTarget?.zone === "sideboard" ? workspaceDragProjectionTarget : undefined}
          />
            </section>
          </div>
        )}
      </fieldset>
      {workspaceDragProjection !== undefined && renderedWorkspaceProjectionRect !== undefined && (
        <WorkspaceDragProjectionOverlay
          projection={workspaceDragProjection}
          rect={renderedWorkspaceProjectionRect}
          onSettled={() => {
            if (workspaceProjection?.token !== undefined && workspaceProjection.token !== 0) {
              dragController?.completeWorkspaceProjection?.(
                workspaceProjection.token,
                workspaceDragProjection.geometryRevision,
              );
            }
          }}
        />
      )}
      {fixedDragPreview !== null && fixedDragPreview !== undefined && dragPreviewStyle !== undefined && (
        <div
          data-testid="draft-drag-preview"
          aria-hidden="true"
          className={`fixed z-50 flex gap-1 ${fixedDragPreview.source.kind === "workspace" ? "opacity-60" : "opacity-70"}`}
          style={{
            left: dragPreviewStyle.left,
            top: dragPreviewStyle.top,
            columnGap: dragPreviewStyle.gap,
            pointerEvents: "none",
          }}
        >
          {fixedDragPreview.source.cards.map((card, index) => (
            <DragPreviewCard
              key={card.instance_id}
              image={fixedDragPreview.source.kind === "workspace"
                ? fixedDragPreview.source.previewImage ?? { src: null, alt: card.name }
                : fixedDragPreview.source.previewImages?.[index] ?? { src: null, alt: card.name }}
              instanceId={card.instance_id}
              width={dragPreviewStyle.cardWidth}
              height={dragPreviewStyle.cardHeight}
            />
          ))}
        </div>
      )}
    </section>
    {mobileOverlayActive && (
      <>
      <div
        data-mobile-workspace-summary
        className={responsiveLayout === "phone-landscape"
          ? "fixed bottom-0 left-[9px] z-[41] flex min-h-[58px] w-[calc(32.5%_-_12px)] items-center gap-1 overflow-x-auto whitespace-nowrap border border-hairline bg-slate-900 px-2.5 text-[10px] text-fg-muted"
          : "fixed inset-x-[9px] bottom-[calc(73px_+_env(safe-area-inset-bottom))] z-[41] flex min-h-[39px] items-center gap-1 overflow-x-auto whitespace-nowrap rounded-t-[8px] border border-hairline bg-slate-900 px-2.5 text-[10px] text-fg-muted"}
      >
        <span data-mobile-deck-count className="shrink-0">
          <strong className="text-fg">{t("workspace.zone.deck")}</strong> {counts.deck}
        </span>
        <span data-mobile-sideboard-count className="shrink-0">
          <strong className="text-fg">{t("workspace.zone.sideboard")}</strong> {counts.sideboard}
        </span>
        <span className="flex-1" />
        <button
          type="button"
          aria-expanded={mobileWorkspaceOpen}
          disabled={interactionLocked}
          onClick={() => onMobileWorkspaceOpenChange?.(!mobileWorkspaceOpen)}
          className="min-h-8 shrink-0 whitespace-nowrap text-jade disabled:cursor-not-allowed disabled:opacity-40"
        >
          {mobileWorkspaceOpen ? t("workspace.shell.hide") : t("workspace.shell.show")}
        </button>
      </div>
      </>
    )}
    </>
  );
}
