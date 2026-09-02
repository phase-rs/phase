import { useEffect, useLayoutEffect, useRef, useState, type MouseEvent as ReactMouseEvent, type PointerEvent as ReactPointerEvent } from "react";
import { flushSync } from "react-dom";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import { useLongPress } from "../../hooks/useLongPress";
import type {
  DraftPickDestination,
  DraftPickOutcome,
  DraftPickPlacementHint,
  PendingDraftPickIntent,
} from "../../stores/draftStore";
import type { CardHoverInfo } from "../card/CardPreview";
import { menuButtonClass } from "../menu/buttonStyles";
import { useDraftCardFace } from "./DraftCardFace.tsx";
import {
  DRAFT_PACK_CARD_BASE_WIDTH_PX,
  DRAFT_WORKSPACE_PACK_SCALE_DEFAULT,
  DRAFT_WORKSPACE_PACK_SCALE_MAX,
  DRAFT_WORKSPACE_PACK_SCALE_MIN,
  DRAFT_WORKSPACE_PACK_SCALE_STEP,
  repairDraftWorkspacePackScale,
  type ResponsiveDraftLayout,
} from "./workspace/workspacePreferences";

export type PackDropSettlement =
  | { readonly kind: "outcome"; readonly outcome: DraftPickOutcome }
  | { readonly kind: "conflict" }
  | { readonly kind: "error" };

export interface PackDropAdmission {
  readonly kind: "dispatch";
  readonly requestToken: string;
  readonly interactionGeneration: number;
}

interface PackDropSourceCommon {
  readonly authorityId: string;
  /** The rendered pack card which originated this drag, distinct from effect authority. */
  readonly sourceInstanceId: string;
  readonly cards: readonly DraftCardInstance[];
  readonly sourceIndices: readonly number[];
  readonly interactionGeneration: number;
  readonly previewWidth: number;
  readonly previewHeight: number;
  readonly onAdmission: (admission: PackDropAdmission) => void;
  readonly onSettled: (result: PackDropSettlement) => void;
}

export type PackDropSource = PackDropSourceCommon & (
  | { readonly kind: "pick"; readonly instanceIds: readonly [string] }
  | { readonly kind: "draft-effect"; readonly instanceIds: readonly [string, string] }
);

export interface PackCompatibilityActivation {
  readonly kind: "click" | "double-click";
  readonly detail: number;
  readonly pointerId: number | null;
  readonly pointerType?: string;
  readonly surface: "pack" | "workspace";
  readonly sourceInstanceId: string;
}

export interface PackDragController {
  handlePointerDown(
    event: ReactPointerEvent<HTMLElement>,
    source: PackDropSource,
    allowTouchPackDrag?: boolean,
  ): void;
  handlePointerMove(event: ReactPointerEvent<HTMLElement>): void;
  handlePointerUp(event: ReactPointerEvent<HTMLElement>): void;
  handlePointerCancel(event: ReactPointerEvent<HTMLElement>): void;
  handleLostPointerCapture(event: ReactPointerEvent<HTMLElement>): void;
  consumeCompatibilityActivation(activation: PackCompatibilityActivation): boolean;
}

function compatibilityActivation(
  event: ReactMouseEvent<HTMLElement>,
  kind: PackCompatibilityActivation["kind"],
  sourceInstanceId: string,
): PackCompatibilityActivation {
  const pointerEvent = event.nativeEvent as MouseEvent & { readonly pointerId?: number; readonly pointerType?: string };
  return {
    kind,
    detail: event.detail,
    pointerId: pointerEvent.pointerId ?? null,
    ...(pointerEvent.pointerType === undefined ? {} : { pointerType: pointerEvent.pointerType }),
    surface: "pack",
    sourceInstanceId,
  };
}

export interface WorkspacePackController {
  readonly kind: "local-workspace";
  readonly view: DraftPlayerView | null;
  readonly selectedCard: string | null;
  readonly pendingIntent: PendingDraftPickIntent | null;
  readonly interactionGeneration: number;
  readonly interactionLocked: boolean;
  readonly doubleClickPick: boolean;
  readonly dragController: PackDragController;
  selectCard(instanceId: string | null): void;
  pickCard(instanceId: string, destination: DraftPickDestination, placementHint?: DraftPickPlacementHint): Promise<DraftPickOutcome>;
  pickCardStep(instanceIds: readonly string[], destination: DraftPickDestination, placementHint?: DraftPickPlacementHint): Promise<DraftPickOutcome>;
  confirmPick(destination: DraftPickDestination, placementHint?: DraftPickPlacementHint): Promise<DraftPickOutcome>;
  pickCardWithDraftEffect(effectCardInstanceId: string, instanceIds: readonly [string, string], destination: DraftPickDestination, placementHint?: DraftPickPlacementHint): Promise<DraftPickOutcome>;
  autoPickCard(): Promise<DraftPickOutcome>;
}

export type LocalWorkspaceController = WorkspacePackController;

interface PodSingleConfirmController {
  readonly kind: "pod-single-confirm";
  readonly view: DraftPlayerView | null;
  readonly selectedCard: string | null;
  readonly interactionLocked: boolean;
  selectCard(instanceId: string | null): void;
  confirmPick(): Promise<void> | void;
  pickCardWithDraftEffect(effectCardInstanceId: string, instanceIds: [string, string]): Promise<void> | void;
  autoPickCard(): Promise<void> | void;
}

export type PackDisplayController = WorkspacePackController | PodSingleConfirmController;

export interface PackDisplayPresentation {
  readonly packScale: number;
  setPackScale(next: number): void;
}

interface PackDisplayProps {
  controller: PackDisplayController;
  presentation: PackDisplayPresentation;
  onCardHover: (info: CardHoverInfo | null) => void;
  enableDraftEffects?: boolean;
  responsiveLayout?: ResponsiveDraftLayout;
  phoneToolbarPinned?: boolean;
  mobileWorkspaceOpen?: boolean;
}

type CardVisualState = "leaving" | "submitting" | "waiting" | "failure-restored" | "selected" | "default";

interface RetainedCard {
  readonly card: DraftCardInstance;
  readonly sourceIndex: number;
  readonly requestOrder: number;
  readonly width: number;
  readonly height: number;
  readonly token: string;
  readonly generation: number;
}

interface CardVisualRecord {
  readonly state: CardVisualState;
  readonly token: string;
  readonly generation: number;
}

interface ScheduledVisual {
  readonly handle: ReturnType<typeof setTimeout>;
  readonly instanceId: string;
}

const DOUBLE_TAP_DELAY_MS = 350;
const TOUCH_TAP_MOVE_THRESHOLD_PX = 10;

const cardInfo = (card: DraftCardInstance): CardHoverInfo => ({
  name: card.name,
  sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
});

function PackCard({
  card, state, width, locked, local, doubleTapPickEnabled, doubleClickPickEnabled, allowTouchPackDrag, desktopLayout, onSelect, onDestination, onDoubleClickPick, onHover, makeDropSource,
}: {
  card: DraftCardInstance;
  state: CardVisualState;
  width: number;
  locked: boolean;
  local: LocalWorkspaceController | null;
  doubleTapPickEnabled: boolean;
  doubleClickPickEnabled: boolean;
  allowTouchPackDrag: boolean;
  desktopLayout: boolean;
  onSelect(): void;
  onDestination(destination: DraftPickDestination): void;
  onDoubleClickPick(): void;
  onHover(info: CardHoverInfo | null): void;
  makeDropSource(): PackDropSource | null;
}) {
  const { t } = useTranslation("draft");
  const { src, isLoading, displayName, hasAlternateFace, toggleFace, advanceFailedSource } = useDraftCardFace(
    card.name,
    { setCode: card.set_code, collectorNumber: card.collector_number },
  );
  const [loadedSrc, setLoadedSrc] = useState<string | null>(null);
  const touchStart = useRef<{ x: number; y: number } | null>(null);
  const touchMoved = useRef(false);
  const lastTouchTapAt = useRef<number | null>(null);
  const ignoreCompatibilityClickUntil = useRef(0);
  const imageLoaded = src !== null && loadedSrc === src;
  const longPress = useLongPress(() => onHover(cardInfo(card)), { delay: 500 });

  return (
    <motion.div
      data-instance-id={card.instance_id}
      data-visual-state={state}
      className={`relative shrink-0 select-none overflow-visible rounded-md caret-transparent ring-1 ${state === "selected" ? "transition-transform" : "transition-all"} duration-150 ${locked ? "" : `cursor-pointer ${desktopLayout ? "hover:scale-[1.05]" : ""}`} ${state === "selected" ? "z-10 ring-2 ring-arcane shadow-[0_0_7px_3px_#38bdf8] motion-safe:animate-[draft-pack-selected-glow_4.8s_ease-in-out_infinite]" : state === "failure-restored" ? "ring-red-300" : "ring-white/15 hover:ring-white/20"} ${state === "submitting" || state === "waiting" ? "opacity-55 grayscale" : ""}`}
      style={{ width, flexBasis: width, aspectRatio: "488 / 680" }}
      onMouseEnter={() => onHover(cardInfo(card))}
      onMouseLeave={() => onHover(null)}
      onPointerDown={(event) => {
        if (event.pointerType === "touch") {
          if (!locked) {
            touchStart.current = { x: event.clientX, y: event.clientY };
            touchMoved.current = false;
            longPress.handlers.onPointerDown(event);
            const source = allowTouchPackDrag ? makeDropSource() : null;
            if (source !== null) local?.dragController.handlePointerDown(event, source, true);
          }
        } else {
          const source = makeDropSource();
          if (source !== null) local?.dragController.handlePointerDown(event, source);
        }
      }}
      onPointerMove={(event) => {
        if (event.pointerType !== "touch") {
          local?.dragController.handlePointerMove(event);
          return;
        }
        const start = touchStart.current;
        if (start !== null) {
          const deltaX = event.clientX - start.x;
          const deltaY = event.clientY - start.y;
          if (deltaX * deltaX + deltaY * deltaY > TOUCH_TAP_MOVE_THRESHOLD_PX ** 2) {
            touchMoved.current = true;
          }
        }
        if (allowTouchPackDrag) local?.dragController.handlePointerMove(event);
        longPress.handlers.onPointerMove(event);
      }}
      onPointerUp={(event) => {
        if (event.pointerType !== "touch") {
          local?.dragController.handlePointerUp(event);
          return;
        }
        const longPressFired = longPress.firedRef.current;
        if (allowTouchPackDrag) local?.dragController.handlePointerUp(event);
        longPress.handlers.onPointerUp(event);
        touchStart.current = null;
        ignoreCompatibilityClickUntil.current = Date.now() + 500;
        if (locked || longPressFired || touchMoved.current) return;

        const now = Date.now();
        if (
          doubleTapPickEnabled
          && lastTouchTapAt.current !== null
          && now - lastTouchTapAt.current <= DOUBLE_TAP_DELAY_MS
        ) {
          lastTouchTapAt.current = null;
          onDoubleClickPick();
          return;
        }
        lastTouchTapAt.current = now;
        onSelect();
      }}
      onPointerCancel={(event) => {
        if (event.pointerType === "touch") {
          touchStart.current = null;
          touchMoved.current = false;
          if (allowTouchPackDrag) local?.dragController.handlePointerCancel(event);
          longPress.handlers.onPointerCancel(event);
        } else {
          local?.dragController.handlePointerCancel(event);
        }
      }}
      onLostPointerCapture={(event) => local?.dragController.handleLostPointerCapture(event)}
      onContextMenu={longPress.handlers.onContextMenu}
      onClick={(event) => {
        // The desktop drag controller captures the pointer on this shell. A
        // browser may consequently target its trailing click here instead of
        // the nested activation button; accept only that retargeted case.
        if (locked || !desktopLayout || event.target !== event.currentTarget) return;
        if (!longPress.firedRef.current && !local?.dragController.consumeCompatibilityActivation(compatibilityActivation(event, "click", card.instance_id))) onSelect();
      }}
      onDoubleClick={(event) => {
        const target = event.target as HTMLElement;
        if (target !== event.currentTarget && target.closest("[data-pack-card-activation]") === null) return;
        if (!local?.dragController.consumeCompatibilityActivation(compatibilityActivation(event, "double-click", card.instance_id)) && doubleClickPickEnabled) onDoubleClickPick();
      }}
    >
      <button
        type="button"
        data-pack-card-activation
        disabled={locked}
        onClick={(event) => {
          if (Date.now() < ignoreCompatibilityClickUntil.current) return;
          if (!longPress.firedRef.current && !local?.dragController.consumeCompatibilityActivation(compatibilityActivation(event, "click", card.instance_id))) onSelect();
        }}
        className="block h-full w-full overflow-hidden rounded-md disabled:cursor-not-allowed"
      >
        {isLoading || !src ? (
          <span className="flex h-full items-center justify-center bg-white/5 px-2 text-center text-xs text-white/50">{card.name}</span>
        ) : (
          <img
            src={src}
            alt={displayName}
            draggable={false}
            className="h-full w-full object-contain"
            onLoad={() => setLoadedSrc(src)}
            onError={() => {
              setLoadedSrc(null);
              advanceFailedSource?.(src);
            }}
          />
        )}
      </button>
      {hasAlternateFace && (
        <button
          type="button"
          aria-label={`Show other face of ${card.name}`}
          onPointerDown={(event) => event.stopPropagation()}
          onClick={(event) => {
            event.stopPropagation();
            toggleFace();
          }}
          className="absolute -right-2 -top-2 rounded bg-black/75 px-1.5 py-1 text-xs text-white hover:bg-black"
        >
          ↺
        </button>
      )}
      {!imageLoaded && (
        <div className="absolute inset-x-1 bottom-1 flex items-center gap-1 rounded bg-black/80 p-1">
          <span className="min-w-0 flex-1 truncate px-1 text-[10px] text-white/85">{card.name}</span>
        </div>
      )}
      {local !== null && (
        <>
          <button type="button" disabled={locked} onClick={() => onDestination("deck")} aria-label={t("pack.pickToDeck", { card: card.name })} className="sr-only">{t("workspace.zone.deck")}</button>
          <button type="button" disabled={locked} onClick={() => onDestination("sideboard")} aria-label={t("pack.pickToSideboard", { card: card.name })} className="sr-only">{t("workspace.zone.sideboard")}</button>
        </>
      )}
    </motion.div>
  );
}

export function PackDisplay({
  controller,
  presentation,
  onCardHover,
  enableDraftEffects = false,
  responsiveLayout = "desktop",
  phoneToolbarPinned = false,
  mobileWorkspaceOpen = false,
}: PackDisplayProps) {
  const { t } = useTranslation("draft");
  const reduceMotion = useReducedMotion();
  const [activeEffect, setActiveEffect] = useState<string | null>(null);
  const [additionalCards, setAdditionalCards] = useState<readonly string[]>([]);
  const [states, setStates] = useState<Readonly<Record<string, CardVisualRecord>>>({});
  const [retained, setRetained] = useState<readonly RetainedCard[]>([]);
  const statesRef = useRef<Readonly<Record<string, CardVisualRecord>>>({});
  const viewRef = useRef(controller.view);
  const requestOrder = useRef(0);
  const timers = useRef(new Map<string, ScheduledVisual>());
  const packSequenceRef = useRef<HTMLDivElement>(null);
  const responsiveScaleInitialized = useRef(false);
  const setPackScaleRef = useRef(presentation.setPackScale);
  const localGeneration = controller.kind === "local-workspace" ? controller.interactionGeneration : 0;
  const generationRef = useRef(localGeneration);
  viewRef.current = controller.view;
  setPackScaleRef.current = presentation.setPackScale;

  const cancelTimersFor = (instanceIds: readonly string[]) => {
    const ids = new Set(instanceIds);
    for (const [key, entry] of timers.current) {
      if (!ids.has(entry.instanceId)) continue;
      clearTimeout(entry.handle);
      timers.current.delete(key);
    }
  };
  const schedule = (purpose: "departure" | "failure", token: string, generation: number, instanceIds: readonly string[], delay: 0 | 180 | 1500, callback: (instanceId: string) => void) => {
    for (const instanceId of instanceIds) {
      const key = `${purpose}:${generation}:${token}:${instanceId}`;
      const previous = timers.current.get(key);
      if (previous !== undefined) clearTimeout(previous.handle);
      const handle = setTimeout(() => {
        timers.current.delete(key);
        callback(instanceId);
      }, delay);
      timers.current.set(key, { handle, instanceId });
    }
  };
  useEffect(() => () => {
    for (const entry of timers.current.values()) clearTimeout(entry.handle);
    timers.current.clear();
  }, []);

  const view = controller.view;
  const selectedCard = controller.selectedCard;
  const locked = controller.interactionLocked;
  const pack = view?.current_pack ?? [];
  const draftEffects = view?.draft_effects ?? [];
  const local = controller.kind === "local-workspace" ? controller : null;
  const isOrderedSelection = view?.pick_selection_mode === "Ordered";

  useEffect(() => {
    const live = new Set(controller.view?.current_pack?.map((card) => card.instance_id) ?? []);
    setRetained((current) => {
      const reappeared = current.filter((entry) => live.has(entry.card.instance_id));
      if (reappeared.length > 0) cancelTimersFor(reappeared.map((entry) => entry.card.instance_id));
      return current.filter((entry) => !live.has(entry.card.instance_id));
    });
  }, [controller.view?.current_pack]);
  useEffect(() => {
    if (generationRef.current === localGeneration) return;
    generationRef.current = localGeneration;
    for (const entry of timers.current.values()) clearTimeout(entry.handle);
    timers.current.clear();
    statesRef.current = {};
    setStates({});
    setRetained([]);
  }, [localGeneration]);
  useEffect(() => {
    if (pack.length === 1 && selectedCard === null && !locked) controller.selectCard(pack[0].instance_id);
  }, [controller, locked, pack, selectedCard]);
  useEffect(() => {
    if (activeEffect !== null && !draftEffects.some((card) => card.instance_id === activeEffect)) {
      setActiveEffect(null);
      setAdditionalCards([]);
    }
  }, [activeEffect, draftEffects]);

  useLayoutEffect(() => {
    if (responsiveScaleInitialized.current || responsiveLayout === "desktop" || view === null) return;
    const sequenceWidth = packSequenceRef.current?.getBoundingClientRect().width ?? 0;
    if (sequenceWidth <= 0) return;

    // Entry scale mirrors each mockup target; auto-fill/flex owns all later card counts.
    const columnCount = responsiveLayout === "phone-portrait"
      ? 2
      : responsiveLayout === "tablet-portrait"
        ? 5
        : 4;
    const gap = responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape"
      ? 7
      : 8;
    const phoneGutter = responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape"
      ? 8
      : 0;
    const initialCardWidth = (sequenceWidth - phoneGutter - gap * (columnCount - 1)) / columnCount;
    const initialScale = Math.floor(
      initialCardWidth / DRAFT_PACK_CARD_BASE_WIDTH_PX / DRAFT_WORKSPACE_PACK_SCALE_STEP,
    ) * DRAFT_WORKSPACE_PACK_SCALE_STEP;
    responsiveScaleInitialized.current = true;
    setPackScaleRef.current(repairDraftWorkspacePackScale(initialScale));
  }, [responsiveLayout, view]);

  // The step's identity is the engine's `(pack number, pick number)`, never
  // `current_pack`'s array identity: a guest receives a fresh array object on
  // every `draft_state_update` for the SAME step, and clearing on that would
  // wipe a half-made selection mid-pick.
  useEffect(() => {
    setAdditionalCards([]);
  }, [view?.current_pack_number, view?.pick_number]);

  if (!view) return null;
  if (pack.length === 0 && retained.length === 0) return <div className="flex justify-center py-12 text-white/40">{t("pack.waitingNext")}</div>;

  const updateStates = (update: (current: Readonly<Record<string, CardVisualRecord>>) => Readonly<Record<string, CardVisualRecord>>) => {
    const next = update(statesRef.current);
    statesRef.current = next;
    setStates(next);
  };
  const beginRequest = (ids: readonly string[], token: string, generation: number) => {
    cancelTimersFor(ids);
    const replaced = new Set(ids);
    setRetained((current) => current.filter((entry) => !replaced.has(entry.card.instance_id)));
    updateStates((current) => {
      const next = { ...current };
      for (const id of ids) next[id] = { state: "submitting", token, generation };
      return next;
    });
  };
  const requestIsCurrent = (ids: readonly string[], token: string, generation: number) => ids.every((id) => {
    const record = statesRef.current[id];
    return record?.token === token && record.generation === generation;
  });
  const setCardStates = (ids: readonly string[], token: string, generation: number, state: CardVisualState | null) => updateStates((current) => {
    if (!requestIsCurrent(ids, token, generation)) return current;
    const next = { ...current };
    for (const id of ids) {
      if (state === null) delete next[id];
      else next[id] = { state, token, generation };
    }
    return next;
  });
  const settle = (token: string, generation: number, cards: readonly DraftCardInstance[], indices: readonly number[], sizes: readonly { width: number; height: number }[], result: PackDropSettlement) => {
    const ids = cards.map((card) => card.instance_id);
    if (!requestIsCurrent(ids, token, generation)) return;
    if (result.kind !== "outcome") {
      setCardStates(ids, token, generation, null);
      return;
    }
    switch (result.outcome.status) {
      case "acknowledged": {
        const live = new Set(viewRef.current?.current_pack?.map((card) => card.instance_id) ?? []);
        const departed = cards.flatMap((card, index): RetainedCard[] => live.has(card.instance_id) ? [] : [{
          card, sourceIndex: indices[index], requestOrder: requestOrder.current,
          width: sizes[index]?.width ?? 0, height: sizes[index]?.height ?? 0, token, generation,
        }]);
        setCardStates(ids, token, generation, null);
        if (departed.length > 0) {
          setRetained((current) => [...current, ...departed]);
          schedule("departure", token, generation, departed.map((entry) => entry.card.instance_id), reduceMotion ? 0 : 180, (instanceId) => setRetained((current) => current.filter((entry) => entry.token !== token || entry.generation !== generation || entry.card.instance_id !== instanceId)));
        }
        break;
      }
      case "rejected":
        setCardStates(ids, token, generation, "failure-restored");
        schedule("failure", token, generation, ids, 1500, (instanceId) => setCardStates([instanceId], token, generation, null));
        break;
      case "ignored":
        setCardStates(ids, token, generation, null);
        break;
    }
  };
  const selectedIds = selectedCard === null ? [] : [selectedCard, ...additionalCards];
  const selectedCards = selectedIds.flatMap((id) => {
    const card = pack.find((candidate) => candidate.instance_id === id);
    return card === undefined ? [] : [card];
  });
  const requiredCount = activeEffect === null ? Math.max(1, view.required_pick_count) : 2;
  const chosenCards = (fallback: DraftCardInstance): readonly DraftCardInstance[] => {
    if (activeEffect === null && requiredCount === 1) return [fallback];
    return selectedCards.length === requiredCount && selectedCards.some((card) => card.instance_id === fallback.instance_id)
      ? selectedCards
      : [];
  };
  const request = async (cards: readonly DraftCardInstance[], destination: DraftPickDestination) => {
    if (local === null || locked || cards.length !== requiredCount) return;
    const token = crypto.randomUUID();
    requestOrder.current += 1;
    const generation = local.interactionGeneration;
    const ids = cards.map((card) => card.instance_id);
    const indices = cards.map((card) => pack.indexOf(card));
    beginRequest(ids, token, generation);
    const outcome = activeEffect !== null
      ? await local.pickCardWithDraftEffect(activeEffect, [ids[0], ids[1]], destination)
      : ids.length === 1
        ? await local.pickCard(ids[0], destination)
        : await local.pickCardStep(ids, destination);
    settle(token, generation, cards, indices, cards.map(() => ({ width: 0, height: 0 })), { kind: "outcome", outcome });
  };
  const select = (id: string) => {
    if (locked) return;
    cancelTimersFor([id]);
    updateStates((current) => {
      if (current[id] === undefined) return current;
      const next = { ...current };
      delete next[id];
      return next;
    });
    if (isOrderedSelection) {
      if (selectedCard === null) {
        controller.selectCard(id);
        setAdditionalCards([]);
      } else if (selectedCard === id) {
        if (additionalCards.length === 0) {
          controller.selectCard(null);
        } else {
          controller.selectCard(additionalCards[0]);
          setAdditionalCards((current) => current.slice(1));
        }
      } else if (additionalCards.includes(id)) {
        setAdditionalCards((current) => current.filter((cardId) => cardId !== id));
      } else if (additionalCards.length === 0) {
        setAdditionalCards([id]);
      } else {
        controller.selectCard(additionalCards[0]);
        setAdditionalCards([id]);
      }
    } else if (activeEffect === null && requiredCount <= 1) controller.selectCard(id);
    else if (selectedCard === id) {
      if (requiredCount <= 1) controller.selectCard(null);
    } else if (additionalCards.includes(id)) {
      setAdditionalCards((current) => current.filter((cardId) => cardId !== id));
    } else if (selectedCard === null) {
      controller.selectCard(id);
      setAdditionalCards([]);
    } else if (additionalCards.length + 1 < requiredCount) {
      setAdditionalCards((current) => [...current, id]);
    } else {
      setAdditionalCards((current) => [...current.slice(1), id]);
    }
  };

  const width = DRAFT_PACK_CARD_BASE_WIDTH_PX * presentation.packScale;
  const responsiveGrid = responsiveLayout === "phone-portrait"
    || responsiveLayout === "tablet-portrait"
    || responsiveLayout === "tablet-landscape";
  const canConfirmPick = local !== null
    && activeEffect === null
    && selectedCards.length === requiredCount;
  const slots = [
    ...pack.map((card, sourceIndex) => ({ kind: "live" as const, card, sourceIndex, requestOrder: -1 })),
    ...retained.map((entry) => ({ kind: "retained" as const, ...entry })),
  ].sort((left, right) => left.sourceIndex - right.sourceIndex || left.requestOrder - right.requestOrder);

  const mobileLayout = responsiveLayout === "phone-portrait" || responsiveLayout === "phone-landscape";
  const selectedPackCard = selectedCard === null
    ? null
    : pack.find((card) => card.instance_id === selectedCard) ?? null;

  return (
    <section
      data-responsive-pack-layout={responsiveLayout}
      className={responsiveLayout === "desktop"
        ? "flex flex-col gap-4 pt-3"
        : `flex h-full min-h-0 flex-col overflow-hidden ${phoneToolbarPinned ? "gap-0 pt-0" : "gap-2 pt-2"}`}
      aria-label={t("pack.label")}
    >
      {enableDraftEffects && draftEffects.length > 0 && (
        <div className="flex flex-wrap items-center gap-3 border border-amber-300/20 px-3 py-2">
          <span className="text-xs font-semibold text-amber-100">{t("pack.draftEffects")}</span>
          {draftEffects.map((card) => (
            <label key={card.instance_id} className="flex min-h-11 items-center gap-2 text-xs text-white/75">
              <input type="checkbox" disabled={locked} checked={activeEffect === card.instance_id} onChange={() => {
                setAdditionalCards([]);
                setActiveEffect((current) => current === card.instance_id ? null : card.instance_id);
              }} />
              {card.name}
            </label>
          ))}
        </div>
      )}
      <div
        data-pack-toolbar
        className={`flex min-w-0 shrink-0 flex-nowrap items-center gap-3 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden ${phoneToolbarPinned ? "sticky top-0 z-20 bg-slate-950 py-1" : ""}`}
      >
        <div data-pack-status-controls className="flex shrink-0 items-center gap-2">
          <span className="text-sm text-fg">
            {t("pack.currentPick", {
              pack: view.current_pack_number + 1,
              pick: view.pick_number + 1,
            })}
          </span>
          {canConfirmPick && !mobileLayout && (
            <button
              type="button"
              data-confirm-pick
              disabled={locked}
              onClick={() => void (requiredCount === 1
                ? local.confirmPick("deck")
                : local.pickCardStep(selectedCards.map((card) => card.instance_id), "deck"))}
              className={menuButtonClass({
                tone: "emerald",
                size: "sm",
                disabled: locked,
                className: "!min-h-9 select-none !py-0 caret-transparent",
              })}
            >
              {t("pack.confirmPick")}
            </button>
          )}
        </div>
        {mobileLayout || responsiveLayout === "tablet-landscape" ? (
          <div data-pack-scale-controls className="ml-auto flex shrink-0 items-center gap-1.5">
            <button type="button" disabled={locked} aria-label={t("pack.scaleDecrease")} onClick={() => presentation.setPackScale(repairDraftWorkspacePackScale(presentation.packScale - 0.1))} className={menuButtonClass({ tone: "neutral", size: "icon", disabled: locked })}>−</button>
            <label className="flex items-center">
              <span className="sr-only">{t("pack.scale")}</span>
              <input type="range" min={DRAFT_WORKSPACE_PACK_SCALE_MIN} max={DRAFT_WORKSPACE_PACK_SCALE_MAX} step={DRAFT_WORKSPACE_PACK_SCALE_STEP} value={presentation.packScale} disabled={locked} onChange={(event) => presentation.setPackScale(Number(event.target.value))} aria-label={t("pack.scale")} />
            </label>
            <button type="button" disabled={locked} aria-label={t("pack.scaleIncrease")} onClick={() => presentation.setPackScale(repairDraftWorkspacePackScale(presentation.packScale + 0.1))} className={menuButtonClass({ tone: "neutral", size: "icon", disabled: locked })}>+</button>
          </div>
        ) : (
          <div data-pack-scale-controls className="ml-auto flex shrink-0 items-center gap-2">
            <label className="flex items-center gap-2 text-xs text-white/70">
              {t("pack.scale")}
              <input type="range" min={DRAFT_WORKSPACE_PACK_SCALE_MIN} max={DRAFT_WORKSPACE_PACK_SCALE_MAX} step={DRAFT_WORKSPACE_PACK_SCALE_STEP} value={presentation.packScale} disabled={locked} onChange={(event) => presentation.setPackScale(Number(event.target.value))} aria-label={t("pack.scale")} />
            </label>
            <button type="button" disabled={locked} aria-label={t("pack.scaleDecrease")} onClick={() => presentation.setPackScale(repairDraftWorkspacePackScale(presentation.packScale - 0.1))} className={menuButtonClass({ tone: "neutral", size: "icon", disabled: locked })}>−</button>
            <button type="button" disabled={locked} aria-label={t("pack.scaleReset")} onClick={() => presentation.setPackScale(DRAFT_WORKSPACE_PACK_SCALE_DEFAULT)} className={menuButtonClass({ tone: "neutral", size: "icon", disabled: locked })}>
              <svg viewBox="0 0 20 20" fill="currentColor" className="h-5 w-5" aria-hidden="true">
                <path d="M5.75 3.5a3.25 3.25 0 0 0-3.25 3.25.75.75 0 0 0 1.5 0A1.75 1.75 0 0 1 5.75 5h6.69l-1.22 1.22a.75.75 0 1 0 1.06 1.06l2.5-2.5a.75.75 0 0 0 0-1.06l-2.5-2.5a.75.75 0 1 0-1.06 1.06L12.44 3.5H5.75Zm8.25 9.75A1.75 1.75 0 0 1 12.25 15H5.56l1.22-1.22a.75.75 0 1 0-1.06-1.06l-2.5 2.5a.75.75 0 0 0 0 1.06l2.5 2.5a.75.75 0 0 0 1.06-1.06L5.56 16.5h6.69a3.25 3.25 0 0 0 3.25-3.25.75.75 0 0 0-1.5 0Z" />
              </svg>
            </button>
            <button type="button" disabled={locked} aria-label={t("pack.scaleIncrease")} onClick={() => presentation.setPackScale(repairDraftWorkspacePackScale(presentation.packScale + 0.1))} className={menuButtonClass({ tone: "neutral", size: "icon", disabled: locked })}>+</button>
          </div>
        )}
      </div>
      <div
        ref={packSequenceRef}
        data-testid="pack-sequence"
        style={!responsiveGrid
          ? undefined
          : {
              gridTemplateColumns: `repeat(auto-fill, ${width}px)`,
              justifyContent: "safe center",
            }}
        className={responsiveLayout === "phone-portrait"
          ? "grid min-h-0 flex-1 content-start gap-[7px] overflow-auto p-1"
          : responsiveLayout === "phone-landscape"
            ? "flex min-h-0 flex-1 flex-nowrap justify-start gap-[7px] overflow-x-auto overflow-y-hidden p-1"
            : responsiveLayout === "tablet-portrait"
              ? "grid min-h-0 flex-1 content-start gap-2 overflow-auto pt-2"
              : responsiveLayout === "tablet-landscape"
                ? "grid min-h-0 flex-1 content-start gap-2 overflow-auto pt-2"
                : "flex flex-wrap justify-center gap-[23px] overflow-visible"}
      >
        <AnimatePresence initial={false}>
          {slots.map((slot) => {
            if (slot.kind === "retained") return (
              <motion.div key={`retained:${slot.token}:${slot.card.instance_id}`} data-instance-id={slot.card.instance_id} data-visual-state="leaving" initial={false} style={{ width: slot.width || width, height: slot.height || undefined, flexBasis: slot.width || width, aspectRatio: "488 / 680" }} className="shrink-0 rounded-md ring-1 ring-amber-300/50">
                <span className="flex h-full items-center justify-center text-xs text-white/60">{slot.card.name}</span>
              </motion.div>
            );
            const card = slot.card;
            const selected = selectedCard === card.instance_id || additionalCards.includes(card.instance_id);
            const waiting = local?.pendingIntent?.kind !== "auto-pick" && local?.pendingIntent?.instanceIds.includes(card.instance_id);
            const state = waiting
              ? "waiting"
              : states[card.instance_id]?.state ?? (selected ? "selected" : "default");
            return <PackCard
              key={card.instance_id}
              card={card}
              state={state}
              width={width}
              locked={locked}
              local={local}
              doubleTapPickEnabled={!isOrderedSelection && (local?.doubleClickPick ?? false)}
              doubleClickPickEnabled={!isOrderedSelection && (local?.doubleClickPick ?? false)}
              allowTouchPackDrag={responsiveLayout === "tablet-portrait" || responsiveLayout === "tablet-landscape"}
              desktopLayout={responsiveLayout === "desktop"}
              onSelect={() => select(card.instance_id)}
              onDestination={(destination) => void request(chosenCards(card), destination)}
              onDoubleClickPick={() => {
                if (activeEffect === null && requiredCount === 1) void local?.confirmPick("deck");
                else void request(chosenCards(card), "deck");
              }}
              onHover={onCardHover}
              makeDropSource={() => {
                if (local === null || locked) return null;
                const cards = chosenCards(card);
                if (cards.length !== 1 && cards.length !== 2) return null;
                if (activeEffect === null && requiredCount !== 1) return null;
                const ids = cards.map((candidate) => candidate.instance_id);
                const indices = cards.map((candidate) => pack.indexOf(candidate));
                const generation = local.interactionGeneration;
                let admission: PackDropAdmission | null = null;
                return {
                  kind: cards.length === 2 ? "draft-effect" : "pick",
                  authorityId: cards.length === 2 ? activeEffect! : ids[0],
                  sourceInstanceId: card.instance_id,
                  instanceIds: cards.length === 2 ? [ids[0], ids[1]] : [ids[0]],
                  cards,
                  sourceIndices: indices,
                  interactionGeneration: generation,
                  previewWidth: width,
                  previewHeight: width * 680 / 488,
                  onAdmission: (nextAdmission) => {
                    admission = nextAdmission;
                    requestOrder.current += 1;
                    flushSync(() => beginRequest(ids, nextAdmission.requestToken, nextAdmission.interactionGeneration));
                  },
                  onSettled: (result) => {
                    if (admission === null) return;
                    settle(admission.requestToken, admission.interactionGeneration, cards, indices, cards.map(() => ({ width, height: width * 680 / 488 })), result);
                  },
                } as PackDropSource;
              }}
            />;
          })}
        </AnimatePresence>
      </div>
      {mobileLayout && local !== null && (
        <div
          data-mobile-pick-dock
          className={responsiveLayout === "phone-portrait"
            ? "fixed inset-x-[9px] bottom-0 z-40 grid min-h-[calc(73px_+_env(safe-area-inset-bottom))] grid-cols-[minmax(0,1fr)_auto] items-center gap-2 border-t border-jade/30 bg-slate-950 px-2.5 py-2 shadow-[0_-12px_30px_rgba(0,0,0,0.42)]"
            : "fixed bottom-0 left-[32.5%] right-[9px] z-40 grid min-h-[58px] grid-cols-[minmax(0,1fr)_auto] items-center gap-2 border-t border-jade/30 bg-slate-950 px-2.5 py-[7px] shadow-[0_-12px_30px_rgba(0,0,0,0.42)]"}
        >
          <div data-mobile-selected-copy className="min-w-0">
            {selectedPackCard !== null && (
              <>
                <span className="block text-[9px] font-bold uppercase text-jade">{t("pack.selected")}</span>
                <strong className="block truncate text-xs text-fg">{selectedPackCard.name}</strong>
              </>
            )}
          </div>
          <div className="flex gap-1.5">
            <button
              type="button"
              data-mobile-deck-action
              disabled={!canConfirmPick || locked || mobileWorkspaceOpen}
              onClick={() => void (requiredCount === 1
                ? local.confirmPick("deck")
                : local.pickCardStep(selectedCards.map((card) => card.instance_id), "deck"))}
              className={menuButtonClass({ tone: "emerald", size: "sm", disabled: !canConfirmPick || locked || mobileWorkspaceOpen })}
            >
              {t("workspace.zone.deck")}
            </button>
            <button
              type="button"
              data-mobile-sideboard-action
              disabled={!canConfirmPick || locked || mobileWorkspaceOpen}
              onClick={() => void (requiredCount === 1
                ? local.confirmPick("sideboard")
                : local.pickCardStep(selectedCards.map((card) => card.instance_id), "sideboard"))}
              className={menuButtonClass({ tone: "emerald", size: "sm", disabled: !canConfirmPick || locked || mobileWorkspaceOpen })}
            >
              {t("workspace.zone.sideboard")}
            </button>
          </div>
        </div>
      )}
      <div className="sr-only" aria-live="polite" aria-atomic="true" />
    </section>
  );
}
