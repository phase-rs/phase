import { useCallback, useEffect, useMemo, useRef, useState, type RefCallback } from "react";
import { flushSync } from "react-dom";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import type { PackCompatibilityActivation, PackDragController, PackDropSource } from "../PackDisplay";
import type {
  DraftPickDestination,
  DraftPickOutcome,
  DraftPickPlacementHint,
  PendingDraftPickIntent,
} from "../../../stores/draftStore";
import type { DraftZone } from "./types";
import type { ResolvedWorkspaceDestination, WorkspaceDropState } from "./workspacePlacement";

const MOVE_THRESHOLD_SQUARED = 100;

export interface DraftPickInteractionSnapshot {
  readonly interactionGeneration: number;
  readonly pickInteractionLocked: boolean;
  readonly pendingPickIntent: PendingDraftPickIntent | null;
}

export interface DraftDropRequest {
  readonly requestToken: string;
  readonly interactionGeneration: number;
  readonly source: PackDropSource;
  readonly destination: DraftPickDestination;
  readonly placementHint?: DraftPickPlacementHint;
}

export interface DraftDropDispatch {
  readonly requestToken: string;
  readonly interactionGeneration: number;
  readonly outcome: Promise<DraftPickOutcome>;
}

interface UseDraftWorkspaceDragOptions {
  readonly enabled: boolean;
  readonly workspaceProjectionEnabled: boolean;
  readonly retainLastValidWorkspaceTarget?: boolean;
  readPickInteraction(): DraftPickInteractionSnapshot;
  subscribePickInteraction(listener: () => void): () => void;
  onDrop(request: DraftDropRequest): DraftDropDispatch;
  resolveCollapsedSideboardColumn(sourceInstanceId: string): number;
}

interface LedgerObservation extends DraftPickInteractionSnapshot {
  readonly order: number;
  readonly lockEpoch: number;
}

interface TargetResolution {
  readonly zone: DraftZone;
  readonly column: number;
  readonly row: number | null;
}

interface ResolvedTarget extends TargetResolution {
  readonly board: RegisteredTarget | null;
  readonly columnTarget: RegisteredTarget | null;
  readonly collapsedTarget: RegisteredTarget | null;
  readonly rowElement: HTMLElement | null;
}

type DropDestination = Pick<TargetResolution, "zone" | "column"> & { readonly row?: number };

export interface WorkspaceDragPreviewImage {
  readonly src: string | null;
  readonly alt: string;
}

export interface WorkspaceDragOrigin {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

export interface WorkspaceDragSource {
  readonly kind: "workspace";
  readonly instanceIds: readonly [string];
  readonly cards: readonly [DraftCardInstance];
  readonly canonicalTarget: ResolvedWorkspaceDestination;
  readonly previewWidth: number;
  readonly previewHeight: number;
  readonly origin: WorkspaceDragOrigin;
  readonly previewImage?: WorkspaceDragPreviewImage;
  onDrop(target: DropDestination): boolean;
}

type DraftDragSource = PackDropSource | WorkspaceDragSource;

type PointerPhase = "pending" | "dragging" | "settling" | "retired";
type CompatibilitySuppression =
  | { readonly kind: "none" }
  | {
    readonly kind: "pointer-sequence";
    readonly pointerId: number;
    readonly pointerType: "mouse" | "pen" | "touch";
    readonly surface: "pack" | "workspace";
    readonly sourceInstanceId: string;
    readonly phase: "awaiting-click" | "awaiting-double-click";
  };

interface PointerSession {
  phase: PointerPhase;
  readonly pointerId: number;
  readonly pointerType: "mouse" | "pen" | "touch";
  readonly element: HTMLElement;
  readonly source: DraftDragSource;
  readonly requestToken: string;
  readonly startX: number;
  readonly startY: number;
  clientX: number;
  clientY: number;
  captureOwned: boolean;
  released: boolean;
  expectedLostCapture: boolean;
  hasLeftCanonicalTarget: boolean;
}

interface GeometryRefresh {
  readonly token: object;
  readonly handle: number;
  readonly generation: number;
  readonly session: PointerSession;
  readonly pointerId: number;
  readonly source: DraftDragSource;
  readonly clientX: number;
  readonly clientY: number;
}

function pointerSequenceSuppression(session: PointerSession): Exclude<CompatibilitySuppression, { readonly kind: "none" }> {
  return {
    kind: "pointer-sequence",
    pointerId: session.pointerId,
    pointerType: session.pointerType,
    surface: session.source.kind === "workspace" ? "workspace" : "pack",
    sourceInstanceId: session.source.kind === "workspace"
      ? session.source.instanceIds[0]
      : session.source.sourceInstanceId,
    phase: "awaiting-click",
  };
}

type Admission =
  | { readonly kind: "owned"; readonly lockEpoch: number; readonly lockTrueOrder: number; readonly expectedIntent: PendingDraftPickIntent }
  | { readonly kind: "unowned"; readonly observedOrder: number }
  | { readonly kind: "conflict"; readonly observedOrder: number; readonly reason: string };

interface Settlement {
  readonly requestToken: string;
  readonly generation: number;
  readonly source: PackDropSource;
  readonly admission: Exclude<Admission, { kind: "conflict" }>;
  outcome: DraftPickOutcome | null;
  unlocked: boolean;
  terminal: boolean;
}

interface WorkspaceProjectionLatch {
  readonly token: number;
  readonly geometryRevision: number;
  destinationReady: boolean;
  motionComplete: boolean;
}

interface CompletedWorkspaceProjection {
  readonly token: number;
  readonly geometryRevision: number;
}

function targetsEqual(left: TargetResolution | null, right: TargetResolution | null): boolean {
  return left === right || (
    left !== null
    && right !== null
    && left.zone === right.zone
    && left.column === right.column
    && left.row === right.row
  );
}

interface RegisteredTarget {
  element: HTMLElement;
  zone: DraftZone;
  column: number | null;
  kind: "board" | "column" | "collapsed-sideboard";
}

function intentsEqual(left: PendingDraftPickIntent | null, right: PendingDraftPickIntent | null): boolean {
  if (left === right) return true;
  if (left === null || right === null || left.kind !== right.kind || left.destination !== right.destination) return false;
  if (left.kind === "auto-pick" || right.kind === "auto-pick") return left.kind === right.kind;
  return left.instanceIds.length === right.instanceIds.length
    && left.instanceIds.every((value, index) => value === right.instanceIds[index])
    && left.placementHint?.column === right.placementHint?.column
    && left.placementHint?.row === right.placementHint?.row;
}

function placementHint(target: TargetResolution): DraftPickPlacementHint {
  return {
    column: target.column,
    ...(target.row === null ? {} : { row: target.row }),
  };
}

function expectedIntent(source: PackDropSource, target: TargetResolution): PendingDraftPickIntent {
  const hint = placementHint(target);
  if (source.kind === "draft-effect") {
    return { kind: "draft-effect", instanceIds: source.instanceIds as readonly [string, string], destination: target.zone, placementHint: hint };
  }
  return { kind: "pick", instanceIds: source.instanceIds as readonly [string], destination: target.zone, placementHint: hint };
}

function clippedRect(element: HTMLElement): DOMRect | null {
  if (!element.isConnected) return null;
  const style = getComputedStyle(element);
  if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") return null;
  const rect = element.getBoundingClientRect();
  const viewport = window.visualViewport;
  const viewportLeft = viewport?.offsetLeft ?? 0;
  const viewportTop = viewport?.offsetTop ?? 0;
  const viewportRight = viewportLeft + (viewport?.width ?? window.innerWidth);
  const viewportBottom = viewportTop + (viewport?.height ?? window.innerHeight);
  const left = Math.max(rect.left, viewportLeft);
  const top = Math.max(rect.top, viewportTop);
  const right = Math.min(rect.right, viewportRight);
  const bottom = Math.min(rect.bottom, viewportBottom);
  if (right <= left || bottom <= top) return null;
  return { ...rect, left, top, right, bottom, width: right - left, height: bottom - top } as DOMRect;
}

const containsPoint = (rect: DOMRect, x: number, y: number) => x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;

function intersectRects(left: DOMRect, right: DOMRect): DOMRect | null {
  const intersectionLeft = Math.max(left.left, right.left);
  const intersectionTop = Math.max(left.top, right.top);
  const intersectionRight = Math.min(left.right, right.right);
  const intersectionBottom = Math.min(left.bottom, right.bottom);
  if (intersectionRight <= intersectionLeft || intersectionBottom <= intersectionTop) return null;
  return {
    left: intersectionLeft,
    top: intersectionTop,
    right: intersectionRight,
    bottom: intersectionBottom,
    width: intersectionRight - intersectionLeft,
    height: intersectionBottom - intersectionTop,
    x: intersectionLeft,
    y: intersectionTop,
    toJSON: () => ({}),
  } as DOMRect;
}

export interface DraftWorkspaceDragController extends PackDragController {
  readonly announcement: string;
  readonly activeTarget: TargetResolution | null;
  readonly dragPreview: DraftWorkspaceDragPreview | null;
  readonly workspaceProjection?: WorkspaceDragProjectionPreview | null;
  readonly geometryRevision: number;
  markWorkspaceProjectionDestinationReady?(token: number, geometryRevision: number): void;
  completeWorkspaceProjection?(token: number, geometryRevision: number): void;
  handleWorkspacePointerDown(
    event: React.PointerEvent<HTMLElement>,
    source: WorkspaceDragSource,
    touchDragEnabled?: boolean,
  ): void;
  registerBoard(zone: DraftZone): RefCallback<HTMLElement>;
  registerColumn(zone: DraftZone, column: number): RefCallback<HTMLElement>;
  registerCollapsedSideboard: RefCallback<HTMLElement>;
  dropState(zone: DraftZone): WorkspaceDropState;
  invalidateGeometry(): void;
  dispose(): void;
}

export interface DraftWorkspaceDragPreview {
  readonly source: DraftDragSource;
  readonly clientX: number;
  readonly clientY: number;
}

export interface WorkspaceDragProjectionPreview {
  readonly token: number;
  readonly source: WorkspaceDragSource;
  readonly clientX: number;
  readonly clientY: number;
  readonly target: TargetResolution | null;
  readonly hasLeftCanonicalTarget: boolean;
  readonly settling: boolean;
}

function targetDiffersFromCanonicalTarget(
  target: TargetResolution | null,
  canonicalTarget: ResolvedWorkspaceDestination,
): boolean {
  return target !== null
    && (
      target.zone !== canonicalTarget.zone
      || target.column !== canonicalTarget.column
      || (target.row !== null && target.row !== canonicalTarget.row)
    );
}

export function useDraftWorkspaceDrag(options: UseDraftWorkspaceDragOptions): DraftWorkspaceDragController {
  const { t } = useTranslation("draft");
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const [announcement, setAnnouncement] = useState("");
  const [activeTarget, setActiveTarget] = useState<TargetResolution | null>(null);
  const [dragPreview, setDragPreview] = useState<DraftWorkspaceDragPreview | null>(null);
  const [workspaceProjection, setWorkspaceProjection] = useState<WorkspaceDragProjectionPreview | null>(null);
  const [geometryRevision, setGeometryRevision] = useState(0);
  const activeTargetRef = useRef<TargetResolution | null>(null);
  const retainedTargetRef = useRef<ResolvedTarget | null>(null);
  const sessionRef = useRef<PointerSession | null>(null);
  const settlementRef = useRef<Settlement | null>(null);
  const suppressionRef = useRef<CompatibilitySuppression>({ kind: "none" });
  const targetsRef = useRef(new Map<string, RegisteredTarget>());
  const targetCallbacksRef = useRef(new Map<string, RefCallback<HTMLElement>>());
  const observerRef = useRef<ResizeObserver | null>(null);
  const geometryGenerationRef = useRef(0);
  const geometryRevisionRef = useRef(0);
  const pendingGeometryRefreshRef = useRef<GeometryRefresh | null>(null);
  const workspaceProjectionTokenRef = useRef(0);
  const workspaceProjectionLatchRef = useRef<WorkspaceProjectionLatch | null>(null);
  const completedWorkspaceProjectionRef = useRef<CompletedWorkspaceProjection | null>(null);
  const ledgerRef = useRef<LedgerObservation>({
    ...options.readPickInteraction(),
    order: 0,
    lockEpoch: 0,
  });

  const publishTarget = useCallback((target: TargetResolution | null) => {
    const previous = activeTargetRef.current;
    activeTargetRef.current = target;
    setActiveTarget(target);
    if (target !== null && (previous?.zone !== target.zone || previous.column !== target.column)) {
      setAnnouncement(t("workspace.drag.target", {
        zone: t(`workspace.zone.${target.zone}`),
        column: target.column + 1,
      }));
    }
  }, [t]);

  const clearRetainedTarget = useCallback(() => {
    retainedTargetRef.current = null;
  }, []);

  const cancelPendingGeometryRefresh = useCallback(() => {
    const pending = pendingGeometryRefreshRef.current;
    if (pending !== null) {
      window.cancelAnimationFrame(pending.handle);
      if (pendingGeometryRefreshRef.current === pending) pendingGeometryRefreshRef.current = null;
    }
    geometryGenerationRef.current += 1;
  }, []);

  const clearWorkspaceProjection = useCallback((token?: number) => {
    if (token !== undefined && workspaceProjectionTokenRef.current !== token) return;
    workspaceProjectionLatchRef.current = null;
    if (token === undefined || completedWorkspaceProjectionRef.current?.token === token) {
      completedWorkspaceProjectionRef.current = null;
    }
    setWorkspaceProjection(null);
  }, []);

  const settleWorkspaceProjection = useCallback((token: number) => {
    const latch = workspaceProjectionLatchRef.current;
    if (latch === null || latch.token !== token || !latch.destinationReady || !latch.motionComplete) return;
    clearWorkspaceProjection(token);
  }, [clearWorkspaceProjection]);

  const updateWorkspaceProjection = useCallback((
    source: WorkspaceDragSource,
    clientX: number,
    clientY: number,
    target: TargetResolution | null,
    hasLeftCanonicalTarget: boolean,
  ) => {
    if (!optionsRef.current.workspaceProjectionEnabled) {
      clearWorkspaceProjection();
      return;
    }
    setWorkspaceProjection((current) => {
      if (current?.source !== source) return current;
      if (!targetsEqual(current.target, target)) completedWorkspaceProjectionRef.current = null;
      return { ...current, clientX, clientY, target, hasLeftCanonicalTarget };
    });
  }, [clearWorkspaceProjection]);

  const retainWorkspaceProjection = useCallback((
    source: WorkspaceDragSource,
    clientX: number,
    clientY: number,
    target: TargetResolution,
  ) => {
    if (!optionsRef.current.workspaceProjectionEnabled) {
      clearWorkspaceProjection();
      return;
    }
    const token = workspaceProjectionTokenRef.current;
    const geometryRevision = geometryRevisionRef.current;
    const completedProjection = completedWorkspaceProjectionRef.current;
    workspaceProjectionLatchRef.current = {
      token,
      geometryRevision,
      destinationReady: false,
      motionComplete: completedProjection?.token === token
        && completedProjection.geometryRevision === geometryRevision,
    };
    setWorkspaceProjection((current) => current?.source === source
      ? { ...current, clientX, clientY, target, settling: true }
      : current);
  }, [clearWorkspaceProjection]);

  const releaseCapture = useCallback((session: PointerSession) => {
    if (session.released || !session.captureOwned) return;
    session.released = true;
    session.expectedLostCapture = true;
    if (!session.element.isConnected) return;
    try {
      session.element.releasePointerCapture(session.pointerId);
    } catch {
      session.captureOwned = false;
    }
  }, []);

  const retirePointer = useCallback((announceCancellation: boolean) => {
    const session = sessionRef.current;
    if (session === null || session.phase === "retired") return;
    cancelPendingGeometryRefresh();
    session.phase = "retired";
    setDragPreview(null);
    clearWorkspaceProjection();
    clearRetainedTarget();
    releaseCapture(session);
    publishTarget(null);
    if (announceCancellation) setAnnouncement(t("workspace.drag.cancelled"));
    if (settlementRef.current === null) sessionRef.current = null;
  }, [cancelPendingGeometryRefresh, clearRetainedTarget, clearWorkspaceProjection, publishTarget, releaseCapture, t]);

  const finishSettlement = useCallback(() => {
    const settlement = settlementRef.current;
    if (settlement === null || settlement.terminal || settlement.outcome === null) return;
    if (settlement.admission.kind === "owned" && !settlement.unlocked) return;
    settlement.terminal = true;
    clearRetainedTarget();
    settlement.source.onSettled({ kind: "outcome", outcome: settlement.outcome });
    if (settlement.outcome.status === "acknowledged") {
      setAnnouncement(t("workspace.drag.success", { card: settlement.source.cards.map((card) => card.name).join(", ") }));
    } else if (settlement.outcome.status === "rejected") {
      setAnnouncement(t("workspace.drag.rejected", { card: settlement.source.cards.map((card) => card.name).join(", ") }));
    }
    settlementRef.current = null;
    sessionRef.current = null;
  }, [clearRetainedTarget, t]);

  const conflictSettlement = useCallback(() => {
    const settlement = settlementRef.current;
    if (settlement === null || settlement.terminal) return;
    settlement.terminal = true;
    setDragPreview(null);
    clearRetainedTarget();
    settlement.source.onSettled({ kind: "conflict" });
    settlementRef.current = null;
    sessionRef.current = null;
    publishTarget(null);
  }, [clearRetainedTarget, publishTarget]);

  const observeInteraction = useCallback(() => {
    const next = optionsRef.current.readPickInteraction();
    const previous = ledgerRef.current;
    const changed = next.interactionGeneration !== previous.interactionGeneration
      || next.pickInteractionLocked !== previous.pickInteractionLocked
      || !intentsEqual(next.pendingPickIntent, previous.pendingPickIntent);
    if (!changed) return;
    const observation: LedgerObservation = {
      ...next,
      order: previous.order + 1,
      lockEpoch: !previous.pickInteractionLocked && next.pickInteractionLocked
        ? previous.lockEpoch + 1
        : previous.lockEpoch,
    };
    ledgerRef.current = observation;

    const session = sessionRef.current;
    if (
      session !== null
      && session.phase !== "retired"
      && session.source.kind !== "workspace"
      && observation.interactionGeneration !== session.source.interactionGeneration
    ) {
      retirePointer(false);
    }

    const settlement = settlementRef.current;
    if (settlement === null || settlement.terminal) return;
    if (observation.interactionGeneration !== settlement.generation) {
      conflictSettlement();
      return;
    }
    if (settlement.admission.kind === "unowned") {
      if (observation.pickInteractionLocked || observation.pendingPickIntent !== null) conflictSettlement();
      return;
    }
    const owned = settlement.admission;
    if (observation.pickInteractionLocked) {
      if (observation.lockEpoch !== owned.lockEpoch || !intentsEqual(observation.pendingPickIntent, owned.expectedIntent)) conflictSettlement();
      return;
    }
    if (observation.order > owned.lockTrueOrder && observation.pendingPickIntent === null) {
      settlement.unlocked = true;
      finishSettlement();
    } else {
      conflictSettlement();
    }
  }, [conflictSettlement, finishSettlement, retirePointer]);

  useEffect(() => optionsRef.current.subscribePickInteraction(observeInteraction), [observeInteraction]);

  const resolveTarget = useCallback((x: number, y: number, source: DraftDragSource): ResolvedTarget | null => {
    const collapsed = targetsRef.current.get("collapsed-sideboard");
    if (collapsed !== undefined) {
      const rect = clippedRect(collapsed.element);
      if (rect !== null && containsPoint(rect, x, y)) {
        return {
          zone: "sideboard",
          column: Math.max(0, optionsRef.current.resolveCollapsedSideboardColumn(source.instanceIds[0])),
          row: null,
          board: null,
          columnTarget: null,
          collapsedTarget: collapsed,
          rowElement: null,
        };
      }
    }
    const candidates: ResolvedTarget[] = [];
    for (const board of targetsRef.current.values()) {
      if (board.kind !== "board") continue;
      const boardRect = clippedRect(board.element);
      if (boardRect === null || !containsPoint(boardRect, x, y)) continue;
      for (const column of targetsRef.current.values()) {
        if (column.kind !== "column" || column.zone !== board.zone || column.column === null) continue;
        const columnRect = clippedRect(column.element);
        if (columnRect === null) continue;
        const rect = intersectRects(columnRect, boardRect);
        if (rect === null || !containsPoint(rect, x, y)) continue;
        const rowElement = [...column.element.querySelectorAll<HTMLElement>("[data-board-row]")]
          .find((element) => {
            const rowRect = clippedRect(element);
            return rowRect !== null && containsPoint(rowRect, x, y);
          });
        const row = rowElement?.dataset.boardRow;
        candidates.push({
          zone: column.zone,
          column: column.column,
          row: row === undefined ? null : Number(row),
          board,
          columnTarget: column,
          collapsedTarget: null,
          rowElement: rowElement ?? null,
        });
      }
    }
    candidates.sort((left, right) => left.column - right.column
      || (left.zone === right.zone ? 0 : left.zone === "deck" ? -1 : 1));
    const resolved = candidates[0];
    return resolved ?? null;
  }, []);

  const validRetainedTarget = useCallback((): TargetResolution | null => {
    const target = retainedTargetRef.current;
    if (target === null) return null;
    if (target.collapsedTarget !== null) {
      if (
        targetsRef.current.get("collapsed-sideboard") !== target.collapsedTarget
        || clippedRect(target.collapsedTarget.element) === null
      ) {
        clearRetainedTarget();
        return null;
      }
      return { zone: target.zone, column: target.column, row: null };
    }
    if (target.board === null || target.columnTarget === null) {
      clearRetainedTarget();
      return null;
    }
    if (
      targetsRef.current.get(`board:${target.zone}`) !== target.board
      || targetsRef.current.get(`column:${target.zone}:${target.column}`) !== target.columnTarget
    ) {
      clearRetainedTarget();
      return null;
    }
    const boardRect = clippedRect(target.board.element);
    const columnRect = clippedRect(target.columnTarget.element);
    if (boardRect === null || columnRect === null || intersectRects(boardRect, columnRect) === null) {
      clearRetainedTarget();
      return null;
    }
    if (
      target.rowElement !== null
      && (
        !target.columnTarget.element.contains(target.rowElement)
        || target.rowElement.dataset.boardRow !== String(target.row)
        || clippedRect(target.rowElement) === null
      )
    ) {
      clearRetainedTarget();
      return null;
    }
    return { zone: target.zone, column: target.column, row: target.row };
  }, [clearRetainedTarget]);

  const resolveEffectiveTarget = useCallback((x: number, y: number, source: DraftDragSource): TargetResolution | null => {
    const target = resolveTarget(x, y, source);
    if (target !== null) {
      if (source.kind === "workspace" && optionsRef.current.retainLastValidWorkspaceTarget) {
        retainedTargetRef.current = target;
      }
      return { zone: target.zone, column: target.column, row: target.row };
    }
    if (source.kind !== "workspace" || !optionsRef.current.retainLastValidWorkspaceTarget) return null;
    return validRetainedTarget();
  }, [resolveTarget, validRetainedTarget]);

  const invalidateGeometry = useCallback(() => {
    const session = sessionRef.current;
    if (session?.phase !== "dragging" || pendingGeometryRefreshRef.current !== null) return;
    const generation = geometryGenerationRef.current + 1;
    geometryGenerationRef.current = generation;
    const token = {};
    const handle = window.requestAnimationFrame(() => {
      const refresh = pendingGeometryRefreshRef.current;
      if (
        refresh === null
        || refresh.token !== token
        || geometryGenerationRef.current !== refresh.generation
        || sessionRef.current !== refresh.session
        || sessionRef.current.pointerId !== refresh.pointerId
        || sessionRef.current.source !== refresh.source
        || sessionRef.current.phase !== "dragging"
      ) return;
      pendingGeometryRefreshRef.current = null;
      const target = resolveEffectiveTarget(refresh.clientX, refresh.clientY, refresh.source);
      publishTarget(target);
      if (refresh.source.kind === "workspace" && optionsRef.current.workspaceProjectionEnabled) {
        refresh.session.hasLeftCanonicalTarget ||= targetDiffersFromCanonicalTarget(
          target,
          refresh.source.canonicalTarget,
        );
        updateWorkspaceProjection(
          refresh.source,
          refresh.clientX,
          refresh.clientY,
          target,
          refresh.session.hasLeftCanonicalTarget,
        );
      }
      completedWorkspaceProjectionRef.current = null;
      geometryRevisionRef.current += 1;
      setGeometryRevision(geometryRevisionRef.current);
    });
    const refresh: GeometryRefresh = {
      token,
      handle,
      generation,
      session,
      pointerId: session.pointerId,
      source: session.source,
      clientX: session.clientX,
      clientY: session.clientY,
    };
    pendingGeometryRefreshRef.current = refresh;
  }, [publishTarget, resolveEffectiveTarget, updateWorkspaceProjection]);

  const classifyAdmission = useCallback((
    source: PackDropSource,
    dispatch: DraftDropDispatch,
    intent: PendingDraftPickIntent,
    pre: DraftPickInteractionSnapshot,
    marker: LedgerObservation,
  ): Admission => {
    const post = optionsRef.current.readPickInteraction();
    const observed = ledgerRef.current;
    if (dispatch.requestToken !== sessionRef.current?.requestToken) return { kind: "conflict", observedOrder: observed.order, reason: "token" };
    if (dispatch.interactionGeneration !== source.interactionGeneration || post.interactionGeneration !== source.interactionGeneration) return { kind: "conflict", observedOrder: observed.order, reason: "generation" };
    if (pre.pickInteractionLocked || pre.pendingPickIntent !== null) return { kind: "conflict", observedOrder: observed.order, reason: "preexisting" };
    if (
      post.pickInteractionLocked
      && intentsEqual(post.pendingPickIntent, intent)
      && observed.order > marker.order
      && observed.lockEpoch > marker.lockEpoch
      && observed.interactionGeneration === source.interactionGeneration
      && intentsEqual(observed.pendingPickIntent, intent)
    ) {
      return { kind: "owned", lockEpoch: observed.lockEpoch, lockTrueOrder: observed.order, expectedIntent: intent };
    }
    if (!post.pickInteractionLocked && post.pendingPickIntent === null && observed.interactionGeneration === source.interactionGeneration) {
      return { kind: "unowned", observedOrder: observed.order };
    }
    return { kind: "conflict", observedOrder: observed.order, reason: "intervening-observation" };
  }, []);

  const beginPointer = useCallback((
    event: React.PointerEvent<HTMLElement>,
    source: DraftDragSource,
    touchWorkspaceDragEnabled = false,
    touchPackDragEnabled = false,
  ) => {
    if (
      !optionsRef.current.enabled
      || !event.isPrimary
      || event.button !== 0
      || sessionRef.current !== null
      || (event.pointerType === "touch" && (
        source.kind === "workspace" ? !touchWorkspaceDragEnabled : !touchPackDragEnabled
      ))
    ) return;
    clearWorkspaceProjection();
    const snapshot = optionsRef.current.readPickInteraction();
    if (
      snapshot.pickInteractionLocked
      || snapshot.pendingPickIntent !== null
      || (source.kind !== "workspace" && snapshot.interactionGeneration !== source.interactionGeneration)
    ) return;
    suppressionRef.current = { kind: "none" };
    clearRetainedTarget();
    const element = event.currentTarget;
    const session: PointerSession = {
      phase: "pending", pointerId: event.pointerId, pointerType: event.pointerType === "touch" ? "touch" : event.pointerType === "pen" ? "pen" : "mouse", element, source,
      requestToken: crypto.randomUUID(), startX: event.clientX, startY: event.clientY,
      clientX: event.clientX, clientY: event.clientY,
      captureOwned: false, released: false, expectedLostCapture: false, hasLeftCanonicalTarget: false,
    };
    sessionRef.current = session;
    try {
      element.setPointerCapture(session.pointerId);
      session.captureOwned = true;
    } catch {
      retirePointer(false);
    }
  }, [clearRetainedTarget, clearWorkspaceProjection, retirePointer]);
  const handlePointerDown = useCallback<PackDragController["handlePointerDown"]>(
    (event, source, allowTouchPackDrag = false) => beginPointer(event, source, false, allowTouchPackDrag),
    [beginPointer],
  );
  const handleWorkspacePointerDown = useCallback((
    event: React.PointerEvent<HTMLElement>,
    source: WorkspaceDragSource,
    touchDragEnabled = false,
  ) => beginPointer(event, source, touchDragEnabled), [beginPointer]);

  const handlePointerMove = useCallback<PackDragController["handlePointerMove"]>((event) => {
    const session = sessionRef.current;
    if (session === null || session.pointerId !== event.pointerId || session.phase === "settling" || session.phase === "retired") return;
    const startingDrag = session.phase === "pending";
    if (session.phase === "pending") {
      const dx = event.clientX - session.startX;
      const dy = event.clientY - session.startY;
      if (dx * dx + dy * dy <= MOVE_THRESHOLD_SQUARED) return;
      session.phase = "dragging";
      if (session.source.kind === "workspace" || session.pointerType === "touch") {
        suppressionRef.current = pointerSequenceSuppression(session);
      }
      setAnnouncement(t("workspace.drag.started", { card: session.source.cards.map((card) => card.name).join(", ") }));
    }
    session.clientX = event.clientX;
    session.clientY = event.clientY;
    cancelPendingGeometryRefresh();
    setDragPreview({ source: session.source, clientX: event.clientX, clientY: event.clientY });
    const target = resolveEffectiveTarget(event.clientX, event.clientY, session.source);
    if (session.source.kind === "workspace" && optionsRef.current.workspaceProjectionEnabled) {
      session.hasLeftCanonicalTarget ||= targetDiffersFromCanonicalTarget(
        target,
        session.source.canonicalTarget,
      );
      if (startingDrag) {
        workspaceProjectionTokenRef.current += 1;
        completedWorkspaceProjectionRef.current = null;
        setWorkspaceProjection({
          token: workspaceProjectionTokenRef.current,
          source: session.source,
          clientX: event.clientX,
          clientY: event.clientY,
          target,
          hasLeftCanonicalTarget: session.hasLeftCanonicalTarget,
          settling: false,
        });
      } else {
        updateWorkspaceProjection(
          session.source,
          event.clientX,
          event.clientY,
          target,
          session.hasLeftCanonicalTarget,
        );
      }
    }
    publishTarget(target);
  }, [cancelPendingGeometryRefresh, publishTarget, resolveEffectiveTarget, t, updateWorkspaceProjection]);

  const handlePointerUp = useCallback<PackDragController["handlePointerUp"]>((event) => {
    const session = sessionRef.current;
    if (session === null || session.pointerId !== event.pointerId || session.phase === "retired" || session.phase === "settling") return;
    cancelPendingGeometryRefresh();
    if (session.phase === "pending") {
      retirePointer(false);
      return;
    }
    if (session.source.kind === "workspace") {
      const interaction = optionsRef.current.readPickInteraction();
      if (
        !optionsRef.current.enabled
        || interaction.pickInteractionLocked
        || interaction.pendingPickIntent !== null
      ) {
        retirePointer(true);
        return;
      }
    }
    const target = resolveEffectiveTarget(event.clientX, event.clientY, session.source);
    publishTarget(target);
    if (target === null) {
      retirePointer(true);
      return;
    }
    if (session.source.kind !== "workspace" && session.pointerType !== "touch") {
      suppressionRef.current = pointerSequenceSuppression(session);
    }
    if (session.source.kind === "workspace") {
      flushSync(() => setDragPreview(null));
      session.phase = "settling";
      try {
        if (session.source.onDrop({
          zone: target.zone,
          column: target.column,
          ...(target.row === null ? {} : { row: target.row }),
        })) {
          retainWorkspaceProjection(session.source, event.clientX, event.clientY, target);
          setAnnouncement(t("workspace.drag.moved", {
            card: session.source.cards.map((card) => card.name).join(", "),
          }));
        } else {
          clearWorkspaceProjection();
        }
      } catch {
        clearWorkspaceProjection();
        setAnnouncement(t("workspace.drag.dispatchError", {
          card: session.source.cards.map((card) => card.name).join(", "),
        }));
      } finally {
        clearRetainedTarget();
        releaseCapture(session);
        session.phase = "retired";
        sessionRef.current = null;
        publishTarget(null);
      }
      return;
    }
    flushSync(() => setDragPreview(null));
    session.phase = "settling";
    const marker = ledgerRef.current;
    const pre = optionsRef.current.readPickInteraction();
    if (pre.pickInteractionLocked || pre.pendingPickIntent !== null || pre.interactionGeneration !== session.source.interactionGeneration) {
      session.phase = "retired";
      setDragPreview(null);
      session.source.onSettled({ kind: "conflict" });
      releaseCapture(session);
      sessionRef.current = null;
      publishTarget(null);
      return;
    }
    const intent = expectedIntent(session.source, target);
    let dispatch: DraftDropDispatch;
    try {
      session.source.onAdmission({
        kind: "dispatch",
        requestToken: session.requestToken,
        interactionGeneration: session.source.interactionGeneration,
      });
      dispatch = optionsRef.current.onDrop({
        requestToken: session.requestToken,
        interactionGeneration: session.source.interactionGeneration,
        source: session.source,
        destination: target.zone,
        placementHint: placementHint(target),
      });
    } catch {
      setDragPreview(null);
      releaseCapture(session);
      session.phase = "retired";
      session.source.onSettled({ kind: "error" });
      setAnnouncement(t("workspace.drag.dispatchError", { card: session.source.cards.map((card) => card.name).join(", ") }));
      sessionRef.current = null;
      publishTarget(null);
      return;
    } finally {
      releaseCapture(session);
    }
    const admission = classifyAdmission(session.source, dispatch, intent, pre, marker);
    if (admission.kind === "conflict") {
      session.phase = "retired";
      setDragPreview(null);
      session.source.onSettled({ kind: "conflict" });
      sessionRef.current = null;
      publishTarget(null);
      return;
    }
    settlementRef.current = {
      requestToken: session.requestToken,
      generation: session.source.interactionGeneration,
      source: session.source,
      admission,
      outcome: null,
      unlocked: admission.kind === "unowned",
      terminal: false,
    };
    void Promise.resolve(dispatch.outcome).then((outcome) => {
      const settlement = settlementRef.current;
      if (settlement === null || settlement.requestToken !== session.requestToken || settlement.terminal) return;
      const current = optionsRef.current.readPickInteraction();
      if (settlement.admission.kind === "unowned" && (current.interactionGeneration !== settlement.generation || current.pickInteractionLocked || current.pendingPickIntent !== null)) {
        conflictSettlement();
        return;
      }
      settlement.outcome = outcome;
      finishSettlement();
    }, () => {
      const settlement = settlementRef.current;
      if (settlement === null || settlement.requestToken !== session.requestToken || settlement.terminal) return;
      settlement.terminal = true;
      settlement.source.onSettled({ kind: "error" });
      setAnnouncement(t("workspace.drag.dispatchError", { card: settlement.source.cards.map((card) => card.name).join(", ") }));
      settlementRef.current = null;
      sessionRef.current = null;
      publishTarget(null);
    });
  }, [cancelPendingGeometryRefresh, classifyAdmission, clearRetainedTarget, clearWorkspaceProjection, conflictSettlement, finishSettlement, publishTarget, releaseCapture, resolveEffectiveTarget, retainWorkspaceProjection, retirePointer, t]);

  const handlePointerCancel = useCallback<PackDragController["handlePointerCancel"]>((event) => {
    const session = sessionRef.current;
    if (session === null || session.pointerId !== event.pointerId) return;
    if (session.phase === "pending") retirePointer(false);
    else if (session.phase === "dragging") retirePointer(true);
  }, [retirePointer]);

  const handleLostPointerCapture = useCallback<PackDragController["handleLostPointerCapture"]>((event) => {
    const session = sessionRef.current;
    if (session === null || session.pointerId !== event.pointerId) return;
    session.captureOwned = false;
    if (session.expectedLostCapture || session.phase === "settling") return;
    if (session.phase === "pending") retirePointer(false);
    else if (session.phase === "dragging") retirePointer(true);
  }, [retirePointer]);

  const consumeCompatibilityActivation = useCallback((activation: PackCompatibilityActivation) => {
    const suppression = suppressionRef.current;
    if (suppression.kind === "none") return false;
    if (
      suppression.phase === "awaiting-double-click"
      && activation.kind === "double-click"
      && activation.detail !== 0
      && activation.pointerId === null
      && activation.surface === suppression.surface
      && activation.sourceInstanceId === suppression.sourceInstanceId
    ) {
      suppressionRef.current = { kind: "none" };
      return true;
    }
    if (
      activation.detail === 0
      || activation.pointerId === null
      || activation.pointerId !== suppression.pointerId
      || activation.pointerType !== suppression.pointerType
      || activation.surface !== suppression.surface
      || activation.sourceInstanceId !== suppression.sourceInstanceId
    ) {
      suppressionRef.current = { kind: "none" };
      return false;
    }
    suppressionRef.current = activation.kind === "double-click"
      ? { kind: "none" }
      : { ...suppression, phase: "awaiting-double-click" };
    return true;
  }, []);

  const registerTarget = useCallback((key: string, target: Omit<RegisteredTarget, "element">): RefCallback<HTMLElement> => {
    const existing = targetCallbacksRef.current.get(key);
    if (existing !== undefined) return existing;
    const callback: RefCallback<HTMLElement> = (element) => {
      const previous = targetsRef.current.get(key);
      if (previous !== undefined) observerRef.current?.unobserve(previous.element);
      if (element === null) targetsRef.current.delete(key);
      else {
        targetsRef.current.set(key, { ...target, element });
        observerRef.current?.observe(element);
      }
      if (previous?.element !== element) {
        cancelPendingGeometryRefresh();
        clearRetainedTarget();
        invalidateGeometry();
      }
    };
    targetCallbacksRef.current.set(key, callback);
    return callback;
  }, [cancelPendingGeometryRefresh, clearRetainedTarget, invalidateGeometry]);
  const registerBoard = useCallback((zone: DraftZone) => registerTarget(`board:${zone}`, { kind: "board", zone, column: null }), [registerTarget]);
  const registerColumn = useCallback((zone: DraftZone, column: number) => registerTarget(`column:${zone}:${column}`, { kind: "column", zone, column }), [registerTarget]);
  const registerCollapsedSideboard = useMemo(() => registerTarget("collapsed-sideboard", { kind: "collapsed-sideboard", zone: "sideboard", column: null }), [registerTarget]);
  const markWorkspaceProjectionDestinationReady = useCallback((token: number, geometryRevision: number) => {
    if (!optionsRef.current.workspaceProjectionEnabled) {
      clearWorkspaceProjection();
      return;
    }
    const latch = workspaceProjectionLatchRef.current;
    if (latch === null || latch.token !== token || latch.geometryRevision !== geometryRevision) return;
    latch.destinationReady = true;
    settleWorkspaceProjection(token);
  }, [clearWorkspaceProjection, settleWorkspaceProjection]);
  const completeWorkspaceProjection = useCallback((token: number, geometryRevision: number) => {
    if (!optionsRef.current.workspaceProjectionEnabled) {
      clearWorkspaceProjection();
      return;
    }
    if (geometryRevisionRef.current !== geometryRevision) return;
    const latch = workspaceProjectionLatchRef.current;
    if (latch === null) {
      if (workspaceProjectionTokenRef.current === token) {
        completedWorkspaceProjectionRef.current = { token, geometryRevision };
      }
      return;
    }
    if (latch.token !== token || latch.geometryRevision !== geometryRevision) return;
    latch.motionComplete = true;
    settleWorkspaceProjection(token);
  }, [clearWorkspaceProjection, settleWorkspaceProjection]);
  const dispose = useCallback(() => {
    cancelPendingGeometryRefresh();
    retirePointer(false);
    conflictSettlement();
    clearRetainedTarget();
    clearWorkspaceProjection();
    for (const target of targetsRef.current.values()) observerRef.current?.unobserve(target.element);
    targetsRef.current.clear();
  }, [cancelPendingGeometryRefresh, clearRetainedTarget, clearWorkspaceProjection, conflictSettlement, retirePointer]);

  useEffect(() => {
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(invalidateGeometry);
    const registeredTargets = targetsRef.current;
    observerRef.current = observer;
    for (const target of registeredTargets.values()) observer?.observe(target.element);
    const onGeometryChange = () => invalidateGeometry();
    window.addEventListener("scroll", onGeometryChange, true);
    window.addEventListener("resize", onGeometryChange);
    window.visualViewport?.addEventListener("resize", onGeometryChange);
    window.visualViewport?.addEventListener("scroll", onGeometryChange);
    return () => {
      window.removeEventListener("scroll", onGeometryChange, true);
      window.removeEventListener("resize", onGeometryChange);
      window.visualViewport?.removeEventListener("resize", onGeometryChange);
      window.visualViewport?.removeEventListener("scroll", onGeometryChange);
      for (const target of registeredTargets.values()) observer?.unobserve(target.element);
      observer?.disconnect();
      if (observerRef.current === observer) observerRef.current = null;
      cancelPendingGeometryRefresh();
      retirePointer(false);
      conflictSettlement();
      clearWorkspaceProjection();
    };
  }, [cancelPendingGeometryRefresh, clearWorkspaceProjection, conflictSettlement, invalidateGeometry, retirePointer]);

  useEffect(() => {
    if (!options.enabled) {
      retirePointer(false);
      clearWorkspaceProjection();
    }
    if (!options.retainLastValidWorkspaceTarget) {
      clearRetainedTarget();
      if (sessionRef.current?.source.kind === "workspace") retirePointer(false);
    }
  }, [clearRetainedTarget, clearWorkspaceProjection, options.enabled, options.retainLastValidWorkspaceTarget, retirePointer]);

  useEffect(() => {
    if (!options.workspaceProjectionEnabled) clearWorkspaceProjection();
  }, [clearWorkspaceProjection, options.workspaceProjectionEnabled]);

  const dropState = useCallback((zone: DraftZone): WorkspaceDropState => ({
    zoneActive: activeTargetRef.current?.zone === zone,
    column: activeTargetRef.current?.zone === zone ? activeTargetRef.current.column : null,
    row: activeTargetRef.current?.zone === zone ? activeTargetRef.current.row : null,
  }), []);

  return useMemo(() => ({
    announcement,
    activeTarget,
    dragPreview,
    workspaceProjection,
    geometryRevision,
    markWorkspaceProjectionDestinationReady,
    completeWorkspaceProjection,
    handlePointerDown,
    handleWorkspacePointerDown,
    handlePointerMove,
    handlePointerUp,
    handlePointerCancel,
    handleLostPointerCapture,
    consumeCompatibilityActivation,
    registerBoard,
    registerColumn,
    registerCollapsedSideboard,
    dropState,
    invalidateGeometry,
    dispose,
  }), [
    activeTarget, announcement, completeWorkspaceProjection, consumeCompatibilityActivation, dispose, dragPreview, dropState, geometryRevision,
    handleLostPointerCapture, handlePointerCancel, handlePointerDown, handlePointerMove,
    handleWorkspacePointerDown, workspaceProjection,
    handlePointerUp, invalidateGeometry, registerBoard, registerCollapsedSideboard, registerColumn,
  ]);
}
