import { memo, useState, useCallback, useMemo, useRef } from "react";
import { AnimatePresence, motion, useMotionValue, useSpring, useTransform, useReducedMotion } from "framer-motion";
import type { MotionValue, PanInfo } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { ManaCostPips } from "../mana/ManaCostPips.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useLongPress } from "../../hooks/useLongPress.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useCanActForWaitingState, usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import type { ManaCost, ObjectId } from "../../adapter/types.ts";
import {
  collectObjectActions,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";
import { DRAG_PLAY_THRESHOLD } from "../../hooks/useDragToCast.ts";
import {
  computeHandInsertionSlot,
  computeHandInsertionMarker,
  computeFlankDisplacement,
  computeGapPx,
  flankingHandIndices,
} from "./handInsertionSlot.ts";
import { useCastableZoneObjects } from "../../hooks/useCastableZoneObjects.ts";
import { ZONE_THEME, type ZoneTheme } from "../../viewmodel/zoneAffordance.ts";

// Horizontal overlap between adjacent hand cards. Negative margin pulls each
// card leftward over the previous one. Tightens continuously as the hand grows
// so a Commander-sized hand (up to ~20 cards) still fits on screen.
//
// The margin is a fraction of `--hand-card-w` — the SAME basis the cards are
// rendered at (CardImage below). Using the base `--card-w` here while the cards
// render at `--hand-card-w` (1.14–1.4× larger) left the real overlap off by the
// scale factor, so the fan spread ~40% wider than the formula intends and the
// error compounded with hand size, pushing the fan off-center to the right.
function getHandOverlap(handSize: number): string {
  if (handSize <= 5) return "calc(var(--hand-card-w) * -0.25)";
  if (handSize <= 7) return "calc(var(--hand-card-w) * -0.45)";
  // For 8+ cards: target total width ≈ 4× card width.
  // First card occupies 1w; remaining (n-1) each contribute (1 + overlap)w.
  // (n-1)(1 + overlap) = 3  =>  overlap = 3/(n-1) - 1, clamped to [-0.85, -0.6].
  const overlap = Math.max(-0.85, Math.min(-0.6, 3 / (handSize - 1) - 1));
  return `calc(var(--hand-card-w) * ${overlap})`;
}

// Quadratic arc lift coefficient. Scales down as the hand grows so the parabola
// stays inside the hand band instead of pushing edge cards off-screen.
function getArcCoefficient(handSize: number): number {
  if (handSize <= 7) return 6;
  // Keep max arc lift (at the edges) roughly constant at ~54px.
  const maxDist = (handSize - 1) / 2;
  return 54 / (maxDist * maxDist);
}

// Geometry for the WHOLE displayed row — hand cards plus the castable exile
// (left) and graveyard (right) "wings" — as one fan. Overlap, per-card tilt and
// arc are all sized by the TOTAL number of displayed cards, so a 3-card hand
// shown alongside 13 graveyard delve-candidates tucks into the same tight,
// angle-clamped arc a 16-card hand would, instead of inheriting the loose
// 3-card spacing and spilling off-screen with near-sideways edge cards.
//
// `k` is a card's absolute position across the row: exile cards occupy [0, E),
// hand cards [E, E + H), graveyard cards [E + H, N). When there are no wings
// (N === H, E === 0) a hand card at index i sits at k === i, so the hand keeps
// its familiar standalone fan; wings only ever shift the shared center, never
// the hand's reorder bookkeeping (index/handSize stay hand-local).
function fanGeometry(totalCards: number) {
  const center = (totalCards - 1) / 2;
  // Size the SHAPE (tilt + arc) from at least two cards so a lone card / wing
  // still fans (a raw delta of 0 would render flat).
  const shape = Math.max(2, totalCards);
  const delta = Math.min(6, 36 / (shape - 1));
  const arcCoeff = getArcCoefficient(shape);
  // Downward parabola (edges drop, center rides highest), clamped at the row's
  // own edges so the outermost cards rest level with the band instead of sinking
  // below it and clipping.
  const edgeLift = center * center * arcCoeff;
  return {
    overlap: getHandOverlap(totalCards),
    rotation: (k: number) => (k - center) * delta,
    arc: (k: number) => {
      const d = k - center;
      return Math.min(d * d * arcCoeff, edgeLift);
    },
  };
}

// Rendered size (px) of the bouncing drop-arrow's square box. Fixed (not
// card-relative) so the imperative center / above-slot offsets stay exact in px.
const DROP_ARROW_PX = 28;
// Fraction of the box height at which the arrow's TIP (chevron point) sits —
// viewBox y=20/24. The arrow is anchored and pivots about this point so the tip
// stays on the gap center for any fan tilt.
const ARROW_TIP_FRAC = 20 / 24;

export function PlayerHand() {
  const playerId = usePerspectivePlayerId();
  const handContainerRef = useRef<HTMLDivElement | null>(null);
  const player = useGameStore((s) => s.gameState?.players[playerId]);
  const objects = useGameStore((s) => s.gameState?.objects);
  // Use dispatchAction (animation pipeline) instead of store dispatch
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const setMobileHandOpen = useUiStore((s) => s.setMobileHandOpen);
  const isMobile = useIsMobile();
  const isCompactHeight = useIsCompactHeight();

  const [expanded, setExpanded] = useState(false);
  const [selectedCardId, setSelectedCardId] = useState<number | null>(null);
  const [draggingCardId, setDraggingCardId] = useState<number | null>(null);

  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);

  // Hide the card being cast (shown on stack as preview during TargetSelection)
  const pendingObjectId = useGameStore((s) => {
    const wf = s.waitingFor;
    if (wf?.type === "TargetSelection") return wf.data.pending_cast.object_id;
    return null;
  });

  const canActForWaitingState = useCanActForWaitingState();
  const hasPriority = useGameStore((s) =>
    canActForWaitingState && s.waitingFor?.type === "Priority",
  );

  const playableObjectIds = useMemo(() => {
    return new Set(Object.keys(legalActionsByObject ?? {}).map(Number));
  }, [legalActionsByObject]);

  // Castable graveyard/exile cards, rendered as colored "wings" continuing the
  // hand fan (engine authority — see useCastableZoneObjects). These are NOT
  // hand cards: they carry no `data-card-hover`, so the reorder DOM sweep never
  // sees them and they can never be dragged into the middle of the hand. Their
  // only drag gesture is flick-up-to-cast.
  const exileCards = useCastableZoneObjects("exile", playerId);
  const graveyardCards = useCastableZoneObjects("graveyard", playerId);

  const playCard = useCallback(
    (objectId: number) => {
      if (!hasPriority || !objects) return;
      const obj = objects[objectId];
      if (!obj) return;

      const allActions = collectObjectActions(legalActionsByObject, objectId as ObjectId);

      if (allActions.length === 0) return;
      inspectObject(null);
      // #506: a lone card-consuming action (cycling / Channel — its cost
      // discards the card, CR 702.29a) must surface the choice modal so the
      // player explicitly opts in. resolveSingleActionDispatch is the single
      // decision authority.
      const auto = resolveSingleActionDispatch(allActions, obj);
      if (auto) {
        dispatchAction(auto);
      } else {
        setPendingAbilityChoice({ objectId: objectId as ObjectId, actions: allActions });
      }
    },
    [hasPriority, objects, legalActionsByObject, inspectObject, setPendingAbilityChoice],
  );

  const hoveredSlotRef = useRef<number | null>(null);
  const shouldReduceMotion = useReducedMotion();

  // Drop-position arrow (drag-to-rearrange). A single bouncing arrow marks the
  // gap the flanking cards open. Driven by MotionValues set imperatively in
  // handleDrag — NOT React state — so the memoized fan never re-renders on
  // pointer move. A short spring glides the arrow between slots; when
  // prefers-reduced-motion is set we bind the raw values so it snaps. The arrow
  // is tilted to the average fan rotation of the two flanking cards so it sits
  // square in the angled gap.
  const arrowXRaw = useMotionValue(0);
  const arrowYRaw = useMotionValue(0);
  const arrowRotateRaw = useMotionValue(0);
  const arrowXSpring = useSpring(arrowXRaw, { stiffness: 900, damping: 48, mass: 0.4 });
  const arrowYSpring = useSpring(arrowYRaw, { stiffness: 900, damping: 48, mass: 0.4 });
  const arrowRotateSpring = useSpring(arrowRotateRaw, { stiffness: 900, damping: 48, mass: 0.4 });
  const arrowX = shouldReduceMotion ? arrowXRaw : arrowXSpring;
  const arrowY = shouldReduceMotion ? arrowYRaw : arrowYSpring;
  const arrowRotate = shouldReduceMotion ? arrowRotateRaw : arrowRotateSpring;
  const arrowOpacity = useMotionValue(0);

  // Shared slide-apart signal: the active insertion slot (drag-excluded space)
  // and the dragged card's handObjects index, both -1 when no reorder drag is in
  // flight. Each HandCard derives its own edge highlight + displacement from
  // these via useTransform — set imperatively here so the fan never re-renders.
  const insertionSlotMV = useMotionValue(-1);
  const draggingIndexMV = useMotionValue(-1);
  // Measured-once-per-drag displacement that opens a visible slot of
  // VISIBLE_GAP_FRACTION of the card width between the flanking cards (set in
  // handleDragStart from the rendered card geometry). Each HandCard halves it.
  const gapPxMV = useMotionValue(0);
  // Rendered card height (transform-free), measured once per drag. Half of it
  // lifts the arrow from the gap center up to the slot's top edge along the fan.
  const cardHeightMV = useMotionValue(0);

  const handleDrag = useCallback(
    (objectId: number, info: PanInfo) => {
      const container = handContainerRef.current;
      if (!container) return;

      // One DOM sweep, reused for both the slot and the arrow position.
      const rects = Array.from(
        container.querySelectorAll<HTMLElement>("[data-card-hover]"),
      ).map((el) => {
        const r = el.getBoundingClientRect();
        return {
          objectId: Number(el.dataset.objectId),
          left: r.left,
          width: r.width,
          top: r.top,
          height: r.height,
        };
      });

      const slot = computeHandInsertionSlot(rects, info.point.x, objectId);
      hoveredSlotRef.current = slot;
      const fromIdx = rects.findIndex((r) => r.objectId === objectId);

      // Average fan tilt of the flanking card(s) (single neighbor at an edge) —
      // drives both the arrow's lean and the direction it lifts to reach the
      // (tilted) slot's top edge. Computed from the SAME whole-row fanGeometry
      // the cards actually render with (hand card `idx` sits at fan position
      // `E + idx`), so the arrow stays aligned — and on the correct side — even
      // when castable exile/graveyard wings shift the fan center off the hand.
      let angle = 0;
      if (slot != null) {
        const { left, right } = flankingHandIndices(slot, fromIdx, rects.length);
        const fan = fanGeometry(exileCards.length + rects.length + graveyardCards.length);
        const rotations = [left, right]
          .filter((idx): idx is number => idx != null)
          .map((idx) => fan.rotation(exileCards.length + idx));
        if (rotations.length) angle = rotations.reduce((a, b) => a + b, 0) / rotations.length;
      }

      // Position the arrow whenever a target slot exists (so the spring tracks it
      // even while hidden), then gate visibility separately. Anchor the tip at the
      // TOP-center of the slot: take the gap-center point (cards' vertical center)
      // and lift it UP ALONG the fan tilt by half a card height, so the tip rides
      // the tilted corridor to its top edge. The tilt pivots about the tip
      // (overlay originX/originY), keeping it centered at any fan angle.
      const bounds = container.getBoundingClientRect();
      const marker = slot == null ? null : computeHandInsertionMarker(rects, slot, objectId);
      if (marker) {
        const aRad = (angle * Math.PI) / 180;
        const lift = cardHeightMV.get() / 2;
        const tipX = marker.x + Math.sin(aRad) * lift;
        const tipY = marker.y - Math.cos(aRad) * lift;
        arrowXRaw.set(tipX - bounds.left - DROP_ARROW_PX / 2);
        arrowYRaw.set(tipY - bounds.top - DROP_ARROW_PX * ARROW_TIP_FRAC);
      }

      // CR n/a — pure UI gating. Reorder is a sideways/inside gesture; an upward
      // drag past the play threshold (or leaving the hand band) is a play, so hide
      // the arrow then. Suppress during a pending cast and on mobile, and on the
      // no-op slot (releasing in place — mirrors the fromIdx === targetSlot guard).
      const insideHand =
        info.point.x >= bounds.left &&
        info.point.x <= bounds.right &&
        info.point.y >= bounds.top &&
        info.point.y <= bounds.bottom;
      const show =
        !isMobile &&
        pendingObjectId == null &&
        marker != null &&
        insideHand &&
        info.offset.y >= DRAG_PLAY_THRESHOLD &&
        slot !== fromIdx;
      arrowOpacity.set(show ? 1 : 0);

      // Lean the arrow to the fan tilt and open the slide-apart gap by publishing
      // the active slot + dragged index. -1 == inactive (no gap).
      if (show && slot != null) {
        arrowRotateRaw.set(angle);
        draggingIndexMV.set(fromIdx);
        insertionSlotMV.set(slot);
      } else {
        arrowRotateRaw.set(0);
        insertionSlotMV.set(-1);
        draggingIndexMV.set(-1);
      }
    },
    [isMobile, pendingObjectId, arrowXRaw, arrowYRaw, arrowRotateRaw, arrowOpacity, insertionSlotMV, draggingIndexMV, cardHeightMV, exileCards.length, graveyardCards.length],
  );

  // Drag-to-play applies the same gesture rule as `useDragToCast` (the
  // Commander-zone single-cast path): release above DRAG_PLAY_THRESHOLD
  // while holding priority and outside the source zone. A React hook cannot
  // be called once per hand card, so we inline the rule here but share the
  // threshold constant with `useDragToCast` — there is exactly one
  // definition of "how far up counts as a play."
  const handleDragEnd = useCallback(
    (objectId: number, _event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => {
      arrowOpacity.set(0);
      arrowRotateRaw.set(0);
      insertionSlotMV.set(-1);
      draggingIndexMV.set(-1);
      const bounds = handContainerRef.current?.getBoundingClientRect();
      const releasedInsideHand =
        bounds != null
        && info.point.x >= bounds.left
        && info.point.x <= bounds.right
        && info.point.y >= bounds.top
        && info.point.y <= bounds.bottom;

      // Reorder branch: released inside the hand, a different slot is hovered.
      if (releasedInsideHand) {
        const targetSlot = hoveredSlotRef.current;
        hoveredSlotRef.current = null;
        // Reorder is disabled while a cast is in progress: handObjects filters
        // out `pendingObjectId`, so the DOM has N-1 slots but `player.hand`
        // has N entries. The slot index from `computeHandInsertionSlot` would
        // map to the wrong position in the unfiltered hand.
        if (pendingObjectId != null) return false;
        if (targetSlot == null || !player) return false;
        const currentOrder = player.hand.slice();
        const fromIdx = currentOrder.indexOf(objectId as ObjectId);
        if (fromIdx === -1 || fromIdx === targetSlot) return false;
        const [moved] = currentOrder.splice(fromIdx, 1);
        currentOrder.splice(targetSlot, 0, moved);
        dispatchAction({ type: "ReorderHand", data: { order: currentOrder } });
        return false;
      }

      // Play branch (unchanged from the existing implementation).
      if (!hasPriority) return false;
      if (info.offset.y >= DRAG_PLAY_THRESHOLD) return false;
      playCard(objectId);
      return true;
    },
    [hasPriority, playCard, player, pendingObjectId, arrowOpacity, arrowRotateRaw, insertionSlotMV, draggingIndexMV],
  );

  const handleCardClick = useCallback(
    (objectId: number, e?: React.MouseEvent) => {
      if (useUiStore.getState().debugInteractionMode && e) {
        e.stopPropagation();
        useUiStore.getState().openDebugContextMenu({ objectId, x: e.clientX, y: e.clientY });
        return;
      }
      if (isMobile) {
        setMobileHandOpen(true);
        return;
      }
      if (!hasPriority) return;

      setSelectedCardId(objectId);
      inspectObject(objectId);
    },
    [isMobile, hasPriority, inspectObject, setMobileHandOpen],
  );

  const handleCardDoubleClick = useCallback(
    (objectId: number) => {
      if (useUiStore.getState().debugInteractionMode) return;
      if (!hasPriority) return;
      playCard(objectId);
      setSelectedCardId(null);
    },
    [hasPriority, playCard],
  );

  const handleContainerClick = useCallback(
    (e: React.MouseEvent) => {
      // On mobile the fanned cards are `pointer-events-none` (the drawer is the
      // interaction surface), so every tap in the hand area falls through to this
      // container — or to the inner lift wrapper, which bubbles here. Any such tap
      // opens the full-hand drawer. This MUST run before the target===currentTarget
      // guard below: the lift wrapper makes `e.target` the wrapper rather than the
      // container, so the guard alone would swallow taps that land over a card.
      if (isMobile) {
        setMobileHandOpen(true);
        return;
      }
      // Desktop: only a click on the empty container area (card clicks stop
      // propagation) toggles the hand lift.
      if (e.target === e.currentTarget) {
        setSelectedCardId(null);
        setExpanded((prev) => !prev);
      }
    },
    [isMobile, setMobileHandOpen],
  );

  const handleDragStart = useCallback(
    (id: number) => {
      setDraggingCardId(id);
      // Measure the rendered card geometry once per drag (stable while dragging)
      // so the slide-apart gap opens to a visible 2/3 card width. getComputedStyle
      // returns transform-free layout values, so the fan's rotation/scale don't
      // pollute the width or the resting overlap (the negative margin-left).
      const container = handContainerRef.current;
      const cards = container?.querySelectorAll<HTMLElement>("[data-card-hover]");
      if (cards && cards.length >= 2) {
        const cs0 = getComputedStyle(cards[0]);
        const cardWidthPx = parseFloat(cs0.width);
        const cardHeightPx = parseFloat(cs0.height);
        // cards[0] has margin-left 0; any later card carries the overlap margin.
        const edgeOverlapPx = Math.abs(parseFloat(getComputedStyle(cards[1]).marginLeft));
        if (Number.isFinite(cardWidthPx) && Number.isFinite(edgeOverlapPx)) {
          gapPxMV.set(computeGapPx(cardWidthPx, edgeOverlapPx));
        }
        if (Number.isFinite(cardHeightPx)) cardHeightMV.set(cardHeightPx);
      }
    },
    [gapPxMV, cardHeightMV],
  );
  const handleDragStop = useCallback(() => {
    setDraggingCardId(null);
    arrowOpacity.set(0);
    arrowRotateRaw.set(0);
    insertionSlotMV.set(-1);
    draggingIndexMV.set(-1);
  }, [arrowOpacity, arrowRotateRaw, insertionSlotMV, draggingIndexMV]);
  const handleMouseEnter = useCallback((id: number) => { setExpanded(true); inspectObject(id); }, [inspectObject]);
  const handleMouseLeave = useCallback(() => inspectObject(null), [inspectObject]);

  if (!player || !objects) return null;

  const handObjects = player.hand
    .map((id) => objects[id])
    .filter((obj) => obj && obj.id !== pendingObjectId);

  // The hand and its exile (left) / graveyard (right) castable wings render as
  // ONE fan sized by the total card count, so many wings tuck in tightly instead
  // of inheriting the loose hand-only spacing. `k` is each card's absolute
  // position across the row (exile [0,E), hand [E,E+H), graveyard [E+H,N)). The
  // first card of each section keeps margin 0, leaving a hairline seam that
  // visually groups the colored wings apart from the white hand cards.
  const handSize = handObjects.length;
  const exileCount = exileCards.length;
  const fan = fanGeometry(exileCount + handSize + graveyardCards.length);

  return (
    <div
      ref={handContainerRef}
      className={`relative flex items-end justify-center overflow-visible px-4 py-1 ${
        isCompactHeight ? "min-h-[40px]" : "min-h-[calc(var(--card-h)*0.7)]"
      }`}
      style={{ perspective: "800px", zIndex: draggingCardId != null || expanded ? 40 : undefined }}
      onClick={handleContainerClick}
      onMouseLeave={() => {
        setExpanded(false);
        setSelectedCardId(null);
      }}
    >
      {/* The whole hand lifts as one unit on hover. Keeping this uniform -50px
          lift on a container — rather than baking `expanded` into each card's
          animate target — lets the memoized HandCards skip re-rendering when the
          hand expands/collapses. The lift lives on an inner wrapper so the outer
          container (which owns onMouseLeave) stays put and its collapse hit-area
          doesn't move under the cursor.
          The drag drop-arrow below is likewise driven by MotionValues (not state)
          so pointer-move updates never re-render these memoized cards — do not
          lift the hovered slot into React state. */}
      <motion.div
        className="flex items-end justify-center"
        animate={{ y: expanded ? -50 : 0 }}
        transition={{ duration: 0.25 }}
      >
        <AnimatePresence>
          {/* Exile wing (left): absolute fan positions 0 .. E-1. Cast-only —
              never reorder targets. zIndex stays negative so exile sits beneath
              the hand cards (whose zIndex is their 0-based hand index). */}
          {exileCards.map((obj, j) => {
            return (
              <ZoneFanCard
                key={obj.id}
                objectId={obj.id}
                cardName={obj.name}
                manaCost={obj.mana_cost}
                unimplementedMechanics={obj.unimplemented_mechanics}
                rotation={fan.rotation(j)}
                arcOffset={fan.arc(j)}
                marginLeft={j === 0 ? 0 : fan.overlap}
                zIndex={j - exileCount}
                theme={ZONE_THEME.exile}
                hasPriority={hasPriority}
                isSelected={selectedCardId === obj.id}
                onPlay={playCard}
                onClick={handleCardClick}
                onDoubleClick={handleCardDoubleClick}
                onMouseEnter={handleMouseEnter}
                onMouseLeave={handleMouseLeave}
              />
            );
          })}
          {handObjects.map((obj, i) => {
          // Hand cards occupy absolute fan positions E .. E+H-1.
          const k = exileCount + i;
          const isPlayable = hasPriority && playableObjectIds.has(Number(obj.id));

          return (
            <HandCard
              key={obj.id}
              objectId={obj.id}
              cardName={obj.name}
              manaCost={obj.mana_cost}
              unimplementedMechanics={obj.unimplemented_mechanics}
              index={i}
              handSize={handObjects.length}
              insertionSlotMV={insertionSlotMV}
              draggingIndexMV={draggingIndexMV}
              gapPxMV={gapPxMV}
              rotation={fan.rotation(k)}
              arcOffset={fan.arc(k)}
              marginLeft={i === 0 ? 0 : fan.overlap}
              isPlayable={isPlayable}
              isSelected={selectedCardId === obj.id}
              hasPriority={hasPriority}
              isMobile={isMobile}
              onDragEnd={handleDragEnd}
              onDrag={handleDrag}
              onClick={handleCardClick}
              onDoubleClick={handleCardDoubleClick}
              isDragging={draggingCardId === obj.id}
              onDragStart={handleDragStart}
              onDragStop={handleDragStop}
              onMouseEnter={handleMouseEnter}
              onMouseLeave={handleMouseLeave}
            />
          );
        })}
          {/* Graveyard wing (right): absolute fan positions E+H .. N-1. Cast-only
              — never reorder targets. zIndex stays above the hand cards. */}
          {graveyardCards.map((obj, j) => {
            const k = exileCount + handSize + j;
            return (
              <ZoneFanCard
                key={obj.id}
                objectId={obj.id}
                cardName={obj.name}
                manaCost={obj.mana_cost}
                unimplementedMechanics={obj.unimplemented_mechanics}
                rotation={fan.rotation(k)}
                arcOffset={fan.arc(k)}
                marginLeft={j === 0 ? 0 : fan.overlap}
                zIndex={handSize + j}
                theme={ZONE_THEME.graveyard}
                hasPriority={hasPriority}
                isSelected={selectedCardId === obj.id}
                onPlay={playCard}
                onClick={handleCardClick}
                onDoubleClick={handleCardDoubleClick}
                onMouseEnter={handleMouseEnter}
                onMouseLeave={handleMouseLeave}
              />
            );
          })}
        </AnimatePresence>
      </motion.div>
      {/* Drop-position arrow: a single glowing arrow that bounces over the slot
          the flanking cards open (their inner edges light up via per-card edge
          highlights). x/y/rotate/opacity are MotionValues set in handleDrag, so
          the memoized fan never re-renders. The inner element bounces toward the
          slot (suppressed under prefers-reduced-motion). Hidden on mobile (the
          drawer is the surface). */}
      {!isMobile && (
        <motion.div
          aria-hidden
          // Above the dragged card (whileDrag z-9999), which shares this
          // container's stacking context, so the drop arrow is never occluded.
          className="pointer-events-none absolute left-0 top-0 z-[10000]"
          // Pivot the tilt around the arrow's TIP (chevron point, ARROW_TIP_FRAC
          // down the box), not its center. framer-motion manages the transform,
          // so the pivot must be set via originX/originY (a `transformOrigin`
          // style string is ignored). Rotating about the center swings the tip
          // sideways off the gap; pinning the tip keeps it on the gap-center for
          // any fan angle while the body leans with the fan.
          style={{
            x: arrowX,
            y: arrowY,
            rotate: arrowRotate,
            opacity: arrowOpacity,
            originX: 0.5,
            originY: ARROW_TIP_FRAC,
          }}
        >
          <motion.div
            animate={shouldReduceMotion ? undefined : { y: [0, 9, 0] }}
            transition={
              shouldReduceMotion
                ? undefined
                : { duration: 0.85, repeat: Infinity, ease: "easeInOut" }
            }
          >
            <svg
              width={DROP_ARROW_PX}
              height={DROP_ARROW_PX}
              viewBox="0 0 24 24"
              fill="none"
              strokeWidth={3}
              strokeLinecap="round"
              strokeLinejoin="round"
              className="stroke-ember-bright drop-shadow-[0_0_8px_rgba(251,146,60,0.9)]"
            >
              {/* Downward arrow: stem + chevron head pointing into the slot. */}
              <path d="M12 3 V19 M5 12 l7 8 7-8" />
            </svg>
          </motion.div>
        </motion.div>
      )}
    </div>
  );
}

interface HandCardProps {
  objectId: number;
  cardName: string;
  manaCost: ManaCost;
  unimplementedMechanics?: string[];
  index: number;
  handSize: number;
  insertionSlotMV: MotionValue<number>;
  draggingIndexMV: MotionValue<number>;
  gapPxMV: MotionValue<number>;
  rotation: number;
  arcOffset: number;
  marginLeft: string | number;
  isPlayable: boolean;
  isSelected: boolean;
  isDragging: boolean;
  hasPriority: boolean;
  isMobile: boolean;
  onDragStart: (id: number) => void;
  onDragStop: () => void;
  onDragEnd: (objectId: number, event: MouseEvent | TouchEvent | PointerEvent, info: PanInfo) => boolean;
  onDrag: (objectId: number, info: PanInfo) => void;
  onClick: (objectId: number, e?: React.MouseEvent) => void;
  onDoubleClick: (objectId: number) => void;
  onMouseEnter: (id: number) => void;
  onMouseLeave: () => void;
}

const HandCard = memo(function HandCard({
  objectId,
  cardName,
  manaCost,
  unimplementedMechanics,
  index,
  handSize,
  insertionSlotMV,
  draggingIndexMV,
  gapPxMV,
  rotation,
  arcOffset,
  marginLeft,
  isPlayable,
  isSelected,
  isDragging,
  hasPriority,
  isMobile,
  onDragStart: onDragStartProp,
  onDragStop,
  onDragEnd,
  onDrag,
  onClick,
  onDoubleClick,
  onMouseEnter,
  onMouseLeave,
}: HandCardProps) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setDragging = useUiStore((s) => s.setDragging);

  // Slide-apart displacement: derive this card's signed x offset from the shared
  // insertion signal. useTransform updates imperatively when the MotionValues
  // change (pointer move) and never re-renders this memoized component; the
  // transformer closure is refreshed on every real re-render, so index stays
  // current after a reorder. A gentle spring keeps cards from oscillating;
  // prefers-reduced-motion binds the raw target so the gap snaps open/closed.
  const shouldReduceMotion = useReducedMotion();
  const displaceTarget = useTransform(
    [insertionSlotMV, draggingIndexMV, gapPxMV],
    ([slot, draggingIndex, gapPx]: number[]) =>
      computeFlankDisplacement(index, slot, draggingIndex, gapPx),
  );
  const displaceSpring = useSpring(displaceTarget, { stiffness: 550, damping: 70 });
  const displaceX = shouldReduceMotion ? displaceTarget : displaceSpring;

  // Inner-edge highlights: when this card flanks the active slot, light up the
  // edge facing the gap. The card to the LEFT of the gap lights its RIGHT edge;
  // the card to the RIGHT lights its LEFT edge. Driven by the same shared signal
  // via useTransform, so toggling the glow never re-renders this memoized card.
  const rightEdgeOpacity = useTransform(
    [insertionSlotMV, draggingIndexMV],
    ([slot, draggingIndex]: number[]) =>
      slot >= 0 && draggingIndex >= 0
        && flankingHandIndices(slot, draggingIndex, handSize).left === index
        ? 1
        : 0,
  );
  const leftEdgeOpacity = useTransform(
    [insertionSlotMV, draggingIndexMV],
    ([slot, draggingIndex]: number[]) =>
      slot >= 0 && draggingIndex >= 0
        && flankingHandIndices(slot, draggingIndex, handSize).right === index
        ? 1
        : 0,
  );

  // Use effective spell cost from engine if available (reflects reductions),
  // otherwise fall back to printed mana cost.
  const effectiveCost = useGameStore((s) => s.spellCosts[String(objectId)]);
  const displayCost = effectiveCost ?? manaCost;
  // Detect cost reduction by comparing effective vs printed generic mana
  const isReduced = effectiveCost?.type === "Cost" && manaCost.type === "Cost"
    && (effectiveCost.generic < manaCost.generic || effectiveCost.shards.length < manaCost.shards.length);
  const playedRef = useRef(false);

  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(() => {
    inspectObject(objectId);
    setPreviewSticky(true);
  });

  const glowClass = hasPriority
    ? isPlayable
      ? "shadow-[0_0_16px_4px_rgba(34,211,238,0.6)] ring-2 ring-cyan-400"
      : "opacity-90"
    : "";

  // `rotation`, `arcOffset` and `marginLeft` come from the parent's whole-row
  // `fanGeometry` (sized by hand + wing count) so the hand stays continuous with
  // any castable wings. `index`/`handSize` remain purely for the reorder system.

  return (
    <motion.div
      data-card-hover
      data-object-id={objectId}
      layout
      initial={{ opacity: 0, y: 40 }}
      animate={{
        opacity: 1,
        y: 30 + arcOffset,
        rotate: rotation,
      }}
      exit={{ opacity: 0, scale: 0.8 }}
      whileHover={{ y: 20 + arcOffset, scale: 1.08, zIndex: 30 }}
      whileDrag={{ scale: 1.05, zIndex: 9999 }}
      transition={{
        delay: index * 0.03,
        duration: 0.25,
        layout: { duration: 0.15, delay: 0 },
      }}
      drag
      dragConstraints={false}
      dragElastic={0}
      dragSnapToOrigin={!playedRef.current}
      onDragStart={() => {
        playedRef.current = false;
        setDragging(true);
        inspectObject(null);
        onDragStartProp(objectId);
      }}
      onDrag={(_event, info) => onDrag(objectId, info)}
      onDragEnd={(event, info) => {
        setDragging(false);
        onDragStop();
        const didPlay = onDragEnd(objectId, event, info);
        if (didPlay) {
          playedRef.current = true;
        }
      }}
      onClick={(e) => {
        e.stopPropagation();
        if (longPressFired.current) { longPressFired.current = false; return; }
        onClick(objectId, e);
      }}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onDoubleClick(objectId);
      }}
      onMouseEnter={() => onMouseEnter(objectId)}
      onMouseLeave={onMouseLeave}
      className={`relative cursor-pointer leading-[0] select-none ${
        isMobile ? "pointer-events-none" : ""
      }`}
      style={{
        marginLeft,
        // Selected card sits above every non-selected hand card. Offset by
        // handSize (not a fixed 20) so it still wins in a Commander-sized hand
        // whose plain indices can exceed 20.
        zIndex: isDragging ? 9999 : isSelected ? handSize + 20 : index,
      }}
      {...longPressHandlers}
    >
      <motion.div
        className={`relative rounded-lg ${glowClass} ${isSelected ? "ring-2 ring-cyan-400" : ""}`}
        style={{ x: displaceX }}
      >
        <CardImage
          cardName={cardName}
          size="normal"
          unimplementedMechanics={unimplementedMechanics}
          className="!w-[var(--hand-card-w)] !h-[var(--hand-card-h)]"
        />
        {/* Inner-edge drop highlights. Always rendered, normally invisible; their
            opacity is driven by MotionValues so the glow toggles without a
            re-render. They sit inside the displaced + rotated card, so they track
            the slid-apart edge and the fan tilt. */}
        <motion.div
          aria-hidden
          className="pointer-events-none absolute inset-y-0 left-0 w-[3px] rounded-full bg-ember-bright shadow-[0_0_10px_3px_rgba(251,146,60,0.85)]"
          style={{ opacity: leftEdgeOpacity }}
        />
        <motion.div
          aria-hidden
          className="pointer-events-none absolute inset-y-0 right-0 w-[3px] rounded-full bg-ember-bright shadow-[0_0_10px_3px_rgba(251,146,60,0.85)]"
          style={{ opacity: rightEdgeOpacity }}
        />
        <ManaCostPips cost={displayCost} isReduced={isReduced} className="absolute right-[4%] top-[2%]" />
      </motion.div>
    </motion.div>
  );
});

interface ZoneFanCardProps {
  objectId: number;
  cardName: string;
  manaCost: ManaCost;
  unimplementedMechanics?: string[];
  rotation: number;
  arcOffset: number;
  marginLeft: string | number;
  zIndex: number;
  theme: ZoneTheme;
  hasPriority: boolean;
  isSelected: boolean;
  onPlay: (objectId: number) => void;
  onClick: (objectId: number, e?: React.MouseEvent) => void;
  onDoubleClick: (objectId: number) => void;
  onMouseEnter: (id: number) => void;
  onMouseLeave: () => void;
}

// A castable graveyard/exile card sitting in the hand fan's wing. It mirrors
// HandCard's resting animation (arc + tilt + hover lift) for visual continuity
// but is deliberately NOT part of the reorder system: no `data-card-hover`, no
// insertion-slot wiring, no displacement spring. Its sole drag gesture is
// flick-up-to-cast (CR-agnostic UI gating, same DRAG_PLAY_THRESHOLD as the hand
// and the commander zone). Per-source drag policy lives here — a zone card can
// be flung up to cast but can never be dropped into the middle of the hand.
const ZoneFanCard = memo(function ZoneFanCard({
  objectId,
  cardName,
  manaCost,
  unimplementedMechanics,
  rotation,
  arcOffset,
  marginLeft,
  zIndex,
  theme,
  hasPriority,
  isSelected,
  onPlay,
  onClick,
  onDoubleClick,
  onMouseEnter,
  onMouseLeave,
}: ZoneFanCardProps) {
  const inspectObject = useUiStore((s) => s.inspectObject);
  const setDragging = useUiStore((s) => s.setDragging);
  const setPreviewSticky = useUiStore((s) => s.setPreviewSticky);
  const { handlers: longPressHandlers, firedRef: longPressFired } = useLongPress(() => {
    inspectObject(objectId);
    setPreviewSticky(true);
  });

  const effectiveCost = useGameStore((s) => s.spellCosts[String(objectId)]);
  const displayCost = effectiveCost ?? manaCost;
  const isReduced = effectiveCost?.type === "Cost" && manaCost.type === "Cost"
    && (effectiveCost.generic < manaCost.generic || effectiveCost.shards.length < manaCost.shards.length);
  // Suppress dragSnapToOrigin only when the flick actually cast the card, so a
  // short/sideways drag springs back into the wing instead of flying off.
  const playedRef = useRef(false);

  return (
    <motion.div
      layout
      initial={{ opacity: 0, y: 40 }}
      animate={{ opacity: 1, y: 30 + arcOffset, rotate: rotation }}
      exit={{ opacity: 0, scale: 0.8 }}
      whileHover={{ y: 20 + arcOffset, scale: 1.08, zIndex: 30 }}
      whileDrag={{ scale: 1.05, zIndex: 9999 }}
      transition={{ duration: 0.25, layout: { duration: 0.15, delay: 0 } }}
      drag
      dragConstraints={false}
      dragElastic={0}
      dragSnapToOrigin={!playedRef.current}
      onDragStart={() => {
        playedRef.current = false;
        setDragging(true);
        inspectObject(null);
      }}
      onDragEnd={(_event, info: PanInfo) => {
        setDragging(false);
        // Cast-only: flick up past the threshold while holding priority. There
        // is no reorder branch, so this card can never land in the hand.
        if (hasPriority && info.offset.y < DRAG_PLAY_THRESHOLD) {
          playedRef.current = true;
          onPlay(objectId);
        }
      }}
      onClick={(e) => {
        e.stopPropagation();
        if (longPressFired.current) { longPressFired.current = false; return; }
        onClick(objectId, e);
      }}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onDoubleClick(objectId);
      }}
      onMouseEnter={() => onMouseEnter(objectId)}
      onMouseLeave={onMouseLeave}
      className="relative cursor-pointer leading-[0] select-none"
      style={{ marginLeft, zIndex }}
      {...longPressHandlers}
    >
      <div
        className={`relative overflow-hidden rounded-lg border ${theme.cardBorder} ${
          isSelected ? "ring-2 ring-cyan-400" : ""
        }`}
      >
        <CardImage
          cardName={cardName}
          size="normal"
          unimplementedMechanics={unimplementedMechanics}
          className="!w-[var(--hand-card-w)] !h-[var(--hand-card-h)]"
        />
        {/* Per-zone translucent wash marking "castable from elsewhere". */}
        <div className={`pointer-events-none absolute inset-0 transition-colors ${theme.overlayCard}`} />
      </div>
      {/* Per-zone castable glow ring (sibling of the clipped image so it isn't cropped). */}
      <div className={`pointer-events-none absolute inset-0 rounded-lg ${theme.ring}`} />
      <ManaCostPips cost={displayCost} isReduced={isReduced} className="absolute right-[4%] top-[2%]" />
    </motion.div>
  );
});
