import { useCallback, useMemo, useState } from "react";
import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import type { GameObject, ManaCost, ManaType, ObjectId, TargetFilter, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { ChoiceOverlay, ConfirmButton, ScrollableCardStrip } from "./ChoiceOverlay.tsx";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";
import { NamedChoiceModal } from "./NamedChoiceModal.tsx";
import { VoteChoiceModal } from "./VoteChoiceModal.tsx";
import { DungeonChoiceModal, RoomChoiceModal } from "./DungeonChoiceModal.tsx";
import { DamageAssignmentModal } from "../combat/DamageAssignmentModal.tsx";
import { DistributeAmongModal } from "./DistributeAmongModal.tsx";
import { RetargetChoiceModal } from "./RetargetChoiceModal.tsx";
import { ProliferateModal } from "./ProliferateModal.tsx";

type ScryChoice = Extract<WaitingFor, { type: "ScryChoice" }>;
type DigChoice = Extract<WaitingFor, { type: "DigChoice" }>;
type SurveilChoice = Extract<WaitingFor, { type: "SurveilChoice" }>;
type RevealChoice = Extract<WaitingFor, { type: "RevealChoice" }>;
type SearchChoice = Extract<WaitingFor, { type: "SearchChoice" }>;
type ChooseFromZoneChoice = Extract<WaitingFor, { type: "ChooseFromZoneChoice" }>;
type EffectZoneChoice = Extract<WaitingFor, { type: "EffectZoneChoice" }>;
type DiscardToHandSize = Extract<WaitingFor, { type: "DiscardToHandSize" }>;
type SacrificeForCost = Extract<WaitingFor, { type: "SacrificeForCost" }>;
type ReturnToHandForCost = Extract<WaitingFor, { type: "ReturnToHandForCost" }>;
type BlightChoice = Extract<WaitingFor, { type: "BlightChoice" }>;
type ExileFromGraveyardForCost = Extract<WaitingFor, { type: "ExileFromGraveyardForCost" }>;
type CollectEvidenceChoice = Extract<WaitingFor, { type: "CollectEvidenceChoice" }>;
type HarmonizeTapChoice = Extract<WaitingFor, { type: "HarmonizeTapChoice" }>;
type ChooseLegend = Extract<WaitingFor, { type: "ChooseLegend" }>;
type ManifestDreadChoice = Extract<WaitingFor, { type: "ManifestDreadChoice" }>;
type CrewVehicle = Extract<WaitingFor, { type: "CrewVehicle" }>;
type StationTarget = Extract<WaitingFor, { type: "StationTarget" }>;
type SaddleMount = Extract<WaitingFor, { type: "SaddleMount" }>;
const CHOICE_CARD_IMAGE_CLASS = "";
const SCRY_CARD_IMAGE_CLASS = "";

function canAssignDistinctCardTypes(
  objects: Record<ObjectId, GameObject | undefined>,
  selectedIds: ObjectId[],
  categories: string[],
): boolean {
  if (selectedIds.length === 0) return true;
  if (selectedIds.length > categories.length) return false;

  const cardOptions = selectedIds
    .map((id) => {
      const obj = objects[id];
      if (!obj) return null;
      return categories
        .map((category, index) =>
          obj.card_types.core_types.includes(category) ? index : -1,
        )
        .filter((index) => index >= 0);
    });

  if (cardOptions.some((options) => !options || options.length === 0)) {
    return false;
  }

  const sortedOptions = [...cardOptions]
    .filter((options): options is number[] => Array.isArray(options))
    .sort((a, b) => a.length - b.length);
  const used = new Array(categories.length).fill(false);

  const assign = (idx: number): boolean => {
    if (idx === sortedOptions.length) return true;
    for (const categoryIndex of sortedOptions[idx]) {
      if (used[categoryIndex]) continue;
      used[categoryIndex] = true;
      if (assign(idx + 1)) return true;
      used[categoryIndex] = false;
    }
    return false;
  };

  return assign(0);
}

/**
 * Generic card choice modal for Scry, Dig, Surveil, Reveal, Search, and NamedChoice.
 * Renders based on the WaitingFor type.
 */
export function CardChoiceModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (!waitingFor) return null;

  switch (waitingFor.type) {
    case "ScryChoice":
      if (!canActForWaitingState) return null;
      return <ScryModal data={waitingFor.data} />;
    case "DigChoice":
      if (!canActForWaitingState) return null;
      return <DigModal data={waitingFor.data} />;
    case "SurveilChoice":
      if (!canActForWaitingState) return null;
      return <SurveilModal data={waitingFor.data} />;
    case "RevealChoice":
      if (!canActForWaitingState) return null;
      return <RevealModal data={waitingFor.data} />;
    case "SearchChoice":
      if (!canActForWaitingState) return null;
      return <SearchModal data={waitingFor.data} />;
    case "ChooseFromZoneChoice":
      if (!canActForWaitingState) return null;
      return <ChooseFromZoneModal data={waitingFor.data} />;
    case "EffectZoneChoice":
      if (!canActForWaitingState) return null;
      return <EffectZoneModal data={waitingFor.data} />;
    case "NamedChoice":
      if (!canActForWaitingState) return null;
      return <NamedChoiceModal data={waitingFor.data} />;
    case "VoteChoice":
      if (!canActForWaitingState) return null;
      return <VoteChoiceModal data={waitingFor.data} />;
    case "DiscardToHandSize":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={waitingFor.data} />;
    case "DiscardForCost":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={waitingFor.data} title="Discard as additional cost" />;
    case "SacrificeForCost":
      if (!canActForWaitingState) return null;
      return <SacrificeModal data={waitingFor.data} />;
    case "ReturnToHandForCost":
      if (!canActForWaitingState) return null;
      return <ReturnToHandModal data={waitingFor.data} />;
    case "BlightChoice":
      if (!canActForWaitingState) return null;
      return <BlightModal data={waitingFor.data} />;
    case "CrewVehicle":
      if (!canActForWaitingState) return null;
      return <CrewModal data={waitingFor.data} />;
    case "StationTarget":
      if (!canActForWaitingState) return null;
      return <StationTargetModal data={waitingFor.data} />;
    case "SaddleMount":
      if (!canActForWaitingState) return null;
      return <SaddleModal data={waitingFor.data} />;
    case "ExileFromGraveyardForCost":
      if (!canActForWaitingState) return null;
      return <ExileFromGraveyardModal data={waitingFor.data} />;
    case "CollectEvidenceChoice":
      if (!canActForWaitingState) return null;
      return <CollectEvidenceModal data={waitingFor.data} />;
    case "HarmonizeTapChoice":
      if (!canActForWaitingState) return null;
      return <HarmonizeTapModal data={waitingFor.data} />;
    case "ChooseLegend":
      if (!canActForWaitingState) return null;
      return <LegendChoiceModal data={waitingFor.data} />;
    case "ConniveDiscard":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={waitingFor.data} title={`Connive \u2014 Discard ${waitingFor.data.count === 1 ? "a card" : `${waitingFor.data.count} cards`}`} />;
    case "DiscardChoice":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={waitingFor.data} title={waitingFor.data.up_to ? `Discard up to ${waitingFor.data.count} cards` : `Discard ${waitingFor.data.count === 1 ? "a card" : `${waitingFor.data.count} cards`}`} />;
    case "WardDiscardChoice":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={{ ...waitingFor.data, count: 1 }} title="Ward \u2014 Discard a card" />;
    case "WardSacrificeChoice":
      if (!canActForWaitingState) return null;
      return <WardSacrificeModal data={waitingFor.data} />;
    case "AssignCombatDamage":
      if (!canActForWaitingState) return null;
      return <DamageAssignmentModal data={waitingFor.data} />;
    case "DistributeAmong":
      if (!canActForWaitingState) return null;
      return <DistributeAmongModal data={waitingFor.data} />;
    case "RetargetChoice":
      if (!canActForWaitingState) return null;
      return <RetargetChoiceModal data={waitingFor.data} />;
    case "ProliferateChoice":
      if (!canActForWaitingState) return null;
      return <ProliferateModal data={waitingFor.data} />;
    case "ManifestDreadChoice":
      if (!canActForWaitingState) return null;
      return <ManifestDreadModal data={waitingFor.data} />;
    case "ChooseDungeon":
      if (!canActForWaitingState) return null;
      return <DungeonChoiceModal data={waitingFor.data} />;
    case "ChooseDungeonRoom":
      if (!canActForWaitingState) return null;
      return <RoomChoiceModal data={waitingFor.data} />;
    case "ChooseManaColor":
      if (!canActForWaitingState) return null;
      return <ManaColorChoiceModal data={waitingFor.data} />;
    default:
      return null;
  }
}

// ── Scry Modal ──────────────────────────────────────────────────────────────

function ScryModal({ data }: { data: ScryChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  // Track which cards go to bottom (default: all on top)
  const [bottomSet, setBottomSet] = useState<Set<ObjectId>>(new Set());

  const toggleBottom = useCallback((id: ObjectId) => {
    setBottomSet((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    // Send cards that stay on top (not in bottomSet)
    const topCards = data.cards.filter((id) => !bottomSet.has(id));
    dispatch({ type: "SelectCards", data: { cards: topCards } });
  }, [dispatch, data.cards, bottomSet]);

  if (!objects) return null;

  const overlayWidthClassName =
    data.cards.length <= 1
      ? "max-w-[22rem] sm:max-w-[26rem] lg:max-w-[30rem]"
      : data.cards.length === 2
        ? "max-w-[30rem] sm:max-w-[38rem] lg:max-w-[46rem]"
        : "max-w-[38rem] sm:max-w-[48rem] lg:max-w-[58rem]";

  return (
    <ChoiceOverlay
      title="Scry"
      subtitle={`Look at the top ${data.cards.length} card${data.cards.length > 1 ? "s" : ""} of your library`}
      maxWidthClassName={overlayWidthClassName}
      footer={<ConfirmButton onClick={handleConfirm} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isBottom = bottomSet.has(id);
          return (
            <motion.div
              key={id}
              className="relative flex flex-col items-center gap-2"
              initial={{ opacity: 0, y: 40, scale: 0.9 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
            >
              <motion.div
                className={`cursor-pointer rounded-lg transition ${
                  isBottom
                    ? "opacity-50 ring-2 ring-red-400/70"
                    : "ring-2 ring-emerald-400/70 hover:shadow-[0_0_16px_rgba(100,220,150,0.3)]"
                }`}
                whileHover={{ scale: 1.05, y: -6 }}
                onClick={() => toggleBottom(id)}
                {...hoverProps(id)}
              >
                <CardImage
                  cardName={obj.name}
                  size="normal"
                  className={SCRY_CARD_IMAGE_CLASS}
                />
              </motion.div>
              <button
                onClick={() => toggleBottom(id)}
                className={`rounded-full px-3 py-1 text-xs font-bold transition ${
                  isBottom
                    ? "bg-red-500/80 text-white"
                    : "bg-emerald-500/80 text-white"
                }`}
              >
                {isBottom ? "Bottom" : "Top"}
              </button>
            </motion.div>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Dig Modal ───────────────────────────────────────────────────────────────

function DigModal({ data }: { data: DigChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const isUpTo = data.up_to ?? false;
  const selectableSet = new Set(data.selectable_cards ?? data.cards);

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.keep_count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.keep_count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = isUpTo
    ? selected.size <= data.keep_count
    : selected.size === data.keep_count;

  const destLabel =
    data.kept_destination === "Battlefield"
      ? "onto the battlefield"
      : "into your hand";

  const countLabel = isUpTo
    ? `up to ${data.keep_count}`
    : `${data.keep_count}`;

  return (
    <ChoiceOverlay
      title="Choose Cards"
      subtitle={`Select ${countLabel} card${data.keep_count > 1 ? "s" : ""} to put ${destLabel}`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Confirm (${selected.size}/${data.keep_count})`} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          const isSelectable = selectableSet.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-emerald-400/80"
                  : isSelectable
                    ? "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
                    : "opacity-40 cursor-not-allowed"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{
                opacity: isSelected ? 1 : isSelectable ? 0.7 : 0.3,
                y: 0,
                scale: 1,
              }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={isSelectable ? { scale: 1.05, y: -6 } : undefined}
              onClick={() => isSelectable && toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    Keep
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Surveil Modal ───────────────────────────────────────────────────────────

function SurveilModal({ data }: { data: SurveilChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  // Track which cards go to graveyard (default: all stay on top)
  const [graveyardSet, setGraveyardSet] = useState<Set<ObjectId>>(new Set());

  const toggleGraveyard = useCallback((id: ObjectId) => {
    setGraveyardSet((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(graveyardSet) },
    });
  }, [dispatch, graveyardSet]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Surveil"
      subtitle={`Look at the top ${data.cards.length} card${data.cards.length > 1 ? "s" : ""} of your library`}
      footer={<ConfirmButton onClick={handleConfirm} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const toGraveyard = graveyardSet.has(id);
          return (
            <motion.div
              key={id}
              className="relative flex flex-col items-center gap-2"
              initial={{ opacity: 0, y: 40, scale: 0.9 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
            >
              <motion.div
                className={`cursor-pointer rounded-lg transition ${
                  toGraveyard
                    ? "opacity-50 ring-2 ring-red-400/70"
                    : "ring-2 ring-blue-400/70 hover:shadow-[0_0_16px_rgba(100,150,255,0.3)]"
                }`}
                whileHover={{ scale: 1.05, y: -6 }}
                onClick={() => toggleGraveyard(id)}
                {...hoverProps(id)}
              >
                <CardImage
                  cardName={obj.name}
                  size="normal"
                  className={CHOICE_CARD_IMAGE_CLASS}
                />
              </motion.div>
              <button
                onClick={() => toggleGraveyard(id)}
                className={`rounded-full px-3 py-1 text-xs font-bold transition ${
                  toGraveyard
                    ? "bg-red-500/80 text-white"
                    : "bg-blue-500/80 text-white"
                }`}
              >
                {toGraveyard ? "Graveyard" : "Keep"}
              </button>
            </motion.div>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Reveal Modal ─────────────────────────────────────────────────────────────

function RevealModal({ data }: { data: RevealChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<ObjectId | null>(null);
  const isOptional = data.optional === true;

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({
        type: "SelectCards",
        data: { cards: [selected] },
      });
    }
  }, [dispatch, selected]);

  // CR 701.20a: Optional reveals (reveal-lands like Port Town) offer a
  // "decline" path — dispatch an empty selection so the engine's RevealChoice
  // handler runs the source's decline branch (e.g., Tap SelfRef).
  const handleDecline = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: [] },
    });
  }, [dispatch]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={isOptional ? "Reveal from Hand" : "Opponent's Hand"}
      subtitle={isOptional ? "Choose a card to reveal, or decline" : "Choose a card"}
      footer={
        <div className="flex gap-2">
          {isOptional && <ConfirmButton onClick={handleDecline} label="Decline" />}
          <ConfirmButton onClick={handleConfirm} disabled={selected === null} />
        </div>
      }
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected === id;
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-emerald-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => setSelected(isSelected ? null : id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    Choose
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Search Modal ─────────────────────────────────────────────────────────────

function SearchModal({ data }: { data: SearchChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selectedSet, setSelectedSet] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelectedSet((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    if (selectedSet.size === data.count) {
      dispatch({
        type: "SelectCards",
        data: { cards: Array.from(selectedSet) },
      });
    }
  }, [dispatch, selectedSet, data.count]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Search Library"
      subtitle={`Choose ${data.count} card${data.count > 1 ? "s" : ""}`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={selectedSet.size !== data.count} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selectedSet.has(id);
          return (
            <motion.button
              key={id}
              className={`relative shrink-0 rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-emerald-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    Choose
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Choose From Zone Modal ───────────────────────────────────────────────────

function ChooseFromZoneModal({
  data,
}: {
  data: ChooseFromZoneChoice["data"];
}) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selectedSet, setSelectedSet] = useState<Set<ObjectId>>(new Set());
  const selectedIds = useMemo(() => Array.from(selectedSet), [selectedSet]);
  const selectionRule = data.constraint;
  const selectionValid =
    !!objects &&
    (!selectionRule ||
      (selectionRule.type === "DistinctCardTypes" &&
        canAssignDistinctCardTypes(objects, selectedIds, selectionRule.categories)));
  const countValid = data.up_to
    ? selectedSet.size <= data.count
    : selectedSet.size === data.count;
  const canConfirm = countValid && selectionValid;

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelectedSet((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    if (canConfirm) {
      dispatch({
        type: "SelectCards",
        data: { cards: selectedIds },
      });
    }
  }, [canConfirm, dispatch, selectedIds]);

  if (!objects) return null;

  const subtitle = selectionRule?.type === "DistinctCardTypes"
    ? `Choose up to ${data.count} cards with distinct card types`
    : data.up_to
      ? `Choose up to ${data.count} card${data.count > 1 ? "s" : ""}`
      : `Choose ${data.count} card${data.count > 1 ? "s" : ""}`;

  return (
    <ChoiceOverlay
      title="Choose Cards"
      subtitle={subtitle}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!canConfirm} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selectedSet.has(id);
          return (
            <motion.button
              key={id}
              className={`relative shrink-0 rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-emerald-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    Choose
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

function EffectZoneModal({ data }: { data: EffectZoneChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());
  const isSacrifice = data.zone === "Battlefield";
  const isUpTo = data.up_to === true;

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = isUpTo ? selected.size <= data.count : selected.size === data.count;
  const title = isSacrifice ? "Sacrifice" : "Put onto Battlefield";
  const subtitle = isSacrifice
    ? isUpTo
      ? `Choose up to ${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`
      : `Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`
    : isUpTo
      ? `Choose up to ${data.count} card${data.count > 1 ? "s" : ""} to put onto the battlefield`
      : `Choose ${data.count} card${data.count > 1 ? "s" : ""} to put onto the battlefield`;
  const actionLabel = selected.size === 0 && isUpTo
    ? (isSacrifice ? "Skip" : "Decline")
    : `${isSacrifice ? "Confirm" : "Put"} (${selected.size}/${data.count})`;
  const ringClass = isSacrifice ? "ring-red-400/80" : "ring-emerald-400/80";
  const overlayClass = isSacrifice ? "bg-red-500/20" : "bg-emerald-500/20";
  const badgeClass = isSacrifice ? "bg-red-500/90" : "bg-emerald-500/90";
  const badgeLabel = isSacrifice ? "Sacrifice" : "Put";

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={actionLabel} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? `z-10 ring-2 ${ringClass}`
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className={`absolute inset-0 flex items-center justify-center rounded-lg ${overlayClass}`}>
                  <span className={`rounded-full px-3 py-1 text-xs font-bold text-white ${badgeClass}`}>
                    {badgeLabel}
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Sacrifice Modal ──────────────────────────────────────────────────────────

function SacrificeModal({ data }: { data: SacrificeForCost["data"] }) {
  return (
    <PermanentCostModal
      data={data}
      title="Sacrifice"
      subtitle={`Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`}
      label="Sacrifice"
      selectedClassName="z-10 ring-2 ring-red-400/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-red-500/20"
      badgeClassName="rounded-full bg-red-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

function ReturnToHandModal({ data }: { data: ReturnToHandForCost["data"] }) {
  return (
    <PermanentCostModal
      data={data}
      title="Return"
      subtitle={`Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to return`}
      label="Return"
      selectedClassName="z-10 ring-2 ring-sky-300/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-sky-500/20"
      badgeClassName="rounded-full bg-sky-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

function PermanentCostModal({
  data,
  title,
  subtitle,
  label,
  selectedClassName,
  overlayClassName,
  badgeClassName,
}: {
  data: SacrificeForCost["data"] | ReturnToHandForCost["data"];
  title: string;
  subtitle: string;
  label: string;
  selectedClassName: string;
  overlayClassName: string;
  badgeClassName: string;
}) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = selected.size === data.count;

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`${label} (${selected.size}/${data.count})`} />}
    >
      <ScrollableCardStrip>
        {data.permanents.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? selectedClassName
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className={overlayClassName}>
                  <span className={badgeClassName}>{label}</span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Blight Modal ─────────────────────────────────────────────────────────────

function BlightModal({ data }: { data: BlightChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = selected.size === data.count;

  return (
    <ChoiceOverlay
      title="Blight"
      subtitle={`Put a -1/-1 counter on ${data.count} creature${data.count > 1 ? "s" : ""} you control`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Confirm (${selected.size}/${data.count})`} />}
    >
      <ScrollableCardStrip>
        {data.creatures.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-purple-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-purple-500/20">
                  <span className="rounded-full bg-purple-500/90 px-3 py-1 text-xs font-bold text-white">
                    -1/-1
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Crew Vehicle Modal ──────────────────────────────────────────────────────

function CrewModal({ data }: { data: CrewVehicle["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback((id: ObjectId) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const totalPower = Array.from(selected).reduce((sum, id) => {
    const obj = objects?.[id];
    return sum + Math.max(obj?.power ?? 0, 0);
  }, 0);

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "CrewVehicle",
      data: { vehicle_id: data.vehicle_id, creature_ids: Array.from(selected) },
    });
  }, [dispatch, data.vehicle_id, selected]);

  if (!objects) return null;

  const isReady = totalPower >= data.crew_power;

  return (
    <ChoiceOverlay
      title="Crew Vehicle"
      subtitle={`Tap creatures with total power ${data.crew_power} or greater`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Crew (${totalPower}/${data.crew_power})`} />}
    >
      <ScrollableCardStrip>
        {data.eligible_creatures.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-blue-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-blue-500/20">
                  <span className="rounded-full bg-blue-500/90 px-3 py-1 text-xs font-bold text-white">
                    Crew ({obj.power ?? 0})
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Station Target Modal ────────────────────────────────────────────────────
// CR 702.184a: Pick exactly one untapped creature you control to tap as the
// station ability's cost. Charge counters added = that creature's power.

function StationTargetModal({ data }: { data: StationTarget["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<ObjectId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected == null) return;
    dispatch({
      type: "ActivateStation",
      data: { spacecraft_id: data.spacecraft_id, creature_id: selected },
    });
  }, [dispatch, data.spacecraft_id, selected]);

  if (!objects) return null;

  const selectedPower = selected != null
    ? Math.max(objects[selected]?.power ?? 0, 0)
    : 0;

  return (
    <ChoiceOverlay
      title="Station"
      subtitle="Tap another untapped creature you control. Charge counters added equals its power."
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected == null}
          label={selected != null ? `Station (+${selectedPower} charge)` : "Station"}
        />
      }
    >
      <ScrollableCardStrip>
        {data.eligible_creatures.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected === id;
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-blue-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => setSelected(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-blue-500/20">
                  <span className="rounded-full bg-blue-500/90 px-3 py-1 text-xs font-bold text-white">
                    Station (+{Math.max(obj.power ?? 0, 0)})
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Saddle Mount Modal ──────────────────────────────────────────────────────
// CR 702.171a: Tap any number of other untapped creatures you control with
// total power ≥ N. Mirrors CrewModal's selection + total-power gate.

function SaddleModal({ data }: { data: SaddleMount["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback((id: ObjectId) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const totalPower = Array.from(selected).reduce((sum, id) => {
    const obj = objects?.[id];
    return sum + Math.max(obj?.power ?? 0, 0);
  }, 0);

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SaddleMount",
      data: { mount_id: data.mount_id, creature_ids: Array.from(selected) },
    });
  }, [dispatch, data.mount_id, selected]);

  if (!objects) return null;

  const isReady = totalPower >= data.saddle_power;

  return (
    <ChoiceOverlay
      title="Saddle Mount"
      subtitle={`Tap creatures with total power ${data.saddle_power} or greater`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Saddle (${totalPower}/${data.saddle_power})`} />}
    >
      <ScrollableCardStrip>
        {data.eligible_creatures.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-blue-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-blue-500/20">
                  <span className="rounded-full bg-blue-500/90 px-3 py-1 text-xs font-bold text-white">
                    Saddle ({obj.power ?? 0})
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Ward Sacrifice Modal ─────────────────────────────────────────────────────

type WardSacrificeChoice = Extract<WaitingFor, { type: "WardSacrificeChoice" }>;

function WardSacrificeModal({ data }: { data: WardSacrificeChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<ObjectId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected == null) return;
    dispatch({
      type: "SelectCards",
      data: { cards: [selected] },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={data.remaining > 1 ? `Ward \u2014 Sacrifice ${data.remaining} permanents` : "Ward \u2014 Sacrifice a permanent"}
      subtitle="Choose a permanent to sacrifice"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected == null} label="Sacrifice" />}
    >
      <ScrollableCardStrip>
        {data.permanents.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected === id;
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-red-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => setSelected(isSelected ? null : id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-red-500/20">
                  <span className="rounded-full bg-red-500/90 px-3 py-1 text-xs font-bold text-white">
                    Sacrifice
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Exile from Graveyard Modal (Escape cost) ────────────────────────────────

function ExileFromGraveyardModal({ data }: { data: ExileFromGraveyardForCost["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = selected.size === data.count;

  return (
    <ChoiceOverlay
      title="Escape"
      subtitle={`Exile ${data.count} card${data.count > 1 ? "s" : ""} from your graveyard`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Exile (${selected.size}/${data.count})`} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-purple-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-purple-500/20">
                  <span className="rounded-full bg-purple-500/90 px-3 py-1 text-xs font-bold text-white">
                    Exile
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

function manaValueOfShard(shard: string): number {
  switch (shard) {
    case "TwoWhite":
    case "TwoBlue":
    case "TwoBlack":
    case "TwoRed":
    case "TwoGreen":
      return 2;
    case "X":
      return 0;
    default:
      return 1;
  }
}

function manaValueOfCost(cost: ManaCost): number {
  switch (cost.type) {
    case "NoCost":
    case "SelfManaCost":
      return 0;
    case "Cost":
      return cost.generic + cost.shards.reduce((sum, shard) => sum + manaValueOfShard(shard), 0);
  }
}

function manaValueOfObject(obj: { mana_cost: ManaCost }): number {
  return manaValueOfCost(obj.mana_cost);
}

function CollectEvidenceModal({ data }: { data: CollectEvidenceChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback((id: ObjectId) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  const total = Array.from(selected).reduce((sum, id) => {
    const obj = objects[id];
    return obj ? sum + manaValueOfObject(obj) : sum;
  }, 0);
  const isReady = total >= data.minimum_mana_value;

  return (
    <ChoiceOverlay
      title="Collect Evidence"
      subtitle={`Exile cards from your graveyard with total mana value ${data.minimum_mana_value} or greater`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Collect (${total}/${data.minimum_mana_value})`} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          const manaValue = manaValueOfObject(obj);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-amber-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              <div className="absolute left-2 top-2 rounded-full bg-black/75 px-2 py-1 text-xs font-semibold text-white">
                MV {manaValue}
              </div>
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-amber-500/20">
                  <span className="rounded-full bg-amber-500/90 px-3 py-1 text-xs font-bold text-white">
                    Evidence
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Discard to Hand Size Modal ───────────────────────────────────────────────

function DiscardModal({ data, title = "Discard" }: { data: DiscardToHandSize["data"] & { up_to?: boolean; unless_filter?: TargetFilter }; title?: string }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());
  const hasUnlessOption = data.unless_filter != null;
  const isUpTo = data.up_to === true;

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  // CR 701.9b: "up to N" allows 0..=count; exact requires precisely count.
  // CR 608.2c: "discard N unless you discard a [type]" — accept 1 card OR count cards.
  const isReady = isUpTo
    ? selected.size <= data.count
    : selected.size === data.count || (hasUnlessOption && selected.size === 1);

  const subtitle = isUpTo
    ? `Choose up to ${data.count} card${data.count > 1 ? "s" : ""} to discard`
    : hasUnlessOption
      ? `Choose ${data.count} cards or 1 matching card to discard`
      : `Choose ${data.count} card${data.count > 1 ? "s" : ""} to discard`;

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Discard (${selected.size}/${data.count})`} />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-red-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-red-500/20">
                  <span className="rounded-full bg-red-500/90 px-3 py-1 text-xs font-bold text-white">
                    Discard
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Harmonize Tap Choice Modal ──────────────────────────────────────────────

function HarmonizeTapModal({ data }: { data: HarmonizeTapChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const handleTap = useCallback(
    (id: ObjectId) => {
      dispatch({ type: "HarmonizeTap", data: { creature_id: id } });
    },
    [dispatch],
  );

  const handleSkip = useCallback(() => {
    dispatch({ type: "HarmonizeTap", data: { creature_id: null } });
  }, [dispatch]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Harmonize"
      subtitle="Tap a creature to reduce casting cost by its power, or skip"
      footer={<ConfirmButton onClick={handleSkip} label="Skip (pay full cost)" />}
    >
      <ScrollableCardStrip>
        {data.eligible_creatures.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const power = obj.power ?? 0;
          return (
            <motion.button
              key={id}
              className="relative rounded-lg transition hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: 0.85, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => handleTap(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              <div className="absolute bottom-1 left-1/2 -translate-x-1/2">
                <span className="rounded-full bg-emerald-600/90 px-2 py-0.5 text-xs font-bold text-white shadow">
                  -{power} generic
                </span>
              </div>
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Legend Choice Modal ─────────────────────────────────────────────────────

function LegendChoiceModal({ data }: { data: ChooseLegend["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Legend Rule"
      subtitle={`Choose which "${data.legend_name}" to keep`}
    >
      <ScrollableCardStrip>
        {data.candidates.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          return (
            <motion.button
              key={id}
              className="relative rounded-lg transition hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: 0.85, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() =>
                dispatch({ type: "ChooseLegend", data: { keep: id } })
              }
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Manifest Dread Modal ─────────────────────────────────────────────────

function ManifestDreadModal({ data }: { data: ManifestDreadChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<ObjectId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected === null) return;
    dispatch({
      type: "SelectCards",
      data: { cards: [selected] },
    });
  }, [dispatch, selected]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Manifest Dread"
      subtitle="Choose a card to manifest face-down. The other goes to your graveyard."
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} label="Confirm Manifest" />}
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected === id;
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-emerald-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => setSelected(id)}
              {...hoverProps(id)}
            >
              <CardImage
                cardName={obj.name}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    Manifest
                  </span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Mana Color Choice Modal ────────────────────────────────────────────────

type ChooseManaColor = Extract<WaitingFor, { type: "ChooseManaColor" }>;

const MANA_COLOR_STYLES: Record<ManaType, string> = {
  White: "border-yellow-400 bg-yellow-400/20 text-yellow-200 hover:bg-yellow-400/40",
  Blue: "border-blue-400 bg-blue-500/20 text-blue-200 hover:bg-blue-500/40",
  Black: "border-gray-400 bg-gray-700/40 text-gray-200 hover:bg-gray-700/60",
  Red: "border-red-400 bg-red-500/20 text-red-200 hover:bg-red-500/40",
  Green: "border-green-400 bg-green-600/20 text-green-200 hover:bg-green-600/40",
  Colorless: "border-gray-400 bg-gray-500/20 text-gray-200 hover:bg-gray-500/40",
};

const MANA_COLOR_SELECTED: Record<ManaType, string> = {
  White: "border-yellow-300 bg-yellow-400/50 text-white",
  Blue: "border-blue-300 bg-blue-500/50 text-white",
  Black: "border-gray-300 bg-gray-600/60 text-white",
  Red: "border-red-300 bg-red-500/50 text-white",
  Green: "border-green-300 bg-green-500/50 text-white",
  Colorless: "border-gray-300 bg-gray-500/50 text-white",
};

const MANA_COLOR_SHARDS: Record<ManaType, string> = {
  White: "W",
  Blue: "U",
  Black: "B",
  Red: "R",
  Green: "G",
  Colorless: "C",
};

function ManaColorChoiceModal({ data }: { data: ChooseManaColor["data"] }) {
  // CR 605.3b: Prompt shape is a typed union. `SingleColor` is the legacy
  // one-of-N colors shape (Treasures, City of Brass, Pit of Offerings).
  // `Combination` is the filter-land prompt (pick one complete multi-mana
  // sequence). Both share this single modal — the engine dispatches a
  // `ManaChoice` whose shape mirrors the prompt.
  if (data.choice.type === "Combination") {
    return <ManaCombinationChoiceModal options={data.choice.data.options} />;
  }
  return <ManaSingleColorChoiceModal options={data.choice.data.options} />;
}

function ManaSingleColorChoiceModal({ options }: { options: ManaType[] }) {
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<ManaType | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected) {
      dispatch({
        type: "ChooseManaColor",
        data: { choice: { type: "SingleColor", data: selected } },
      });
    }
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title="Choose Mana Color"
      subtitle="Select which color of mana to produce"
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-md"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
    >
      <div className="mx-auto flex w-fit items-center justify-center gap-3 px-4 py-4 sm:gap-5 sm:px-6 sm:py-6">
        {options.map((color, index) => {
          const isSelected = selected === color;
          return (
            <motion.button
              key={color}
              className={`flex h-14 w-14 items-center justify-center rounded-full border-2 transition sm:h-[4.5rem] sm:w-[4.5rem] ${
                isSelected ? MANA_COLOR_SELECTED[color] : MANA_COLOR_STYLES[color]
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.9 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.05, duration: 0.25 }}
              whileHover={{ scale: 1.1 }}
              onClick={() => setSelected(isSelected ? null : color)}
            >
              <ManaSymbol shard={MANA_COLOR_SHARDS[color]} size="lg" />
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}

// CR 605.3b + CR 106.1a: Filter-land combination picker (Shadowmoor/Eventide).
// Renders one button per combination option, each showing the full mana
// sequence with the source pips side-by-side.
function ManaCombinationChoiceModal({ options }: { options: ManaType[][] }) {
  const dispatch = useGameDispatch();
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);

  const handleConfirm = useCallback(() => {
    if (selectedIndex !== null) {
      dispatch({
        type: "ChooseManaColor",
        data: {
          choice: { type: "Combination", data: options[selectedIndex] },
        },
      });
    }
  }, [dispatch, options, selectedIndex]);

  return (
    <ChoiceOverlay
      title="Choose Mana Combination"
      subtitle="Select which combination of mana to produce"
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-lg"
      footer={
        <ConfirmButton onClick={handleConfirm} disabled={selectedIndex === null} />
      }
    >
      <div className="mx-auto flex w-fit flex-col items-center justify-center gap-3 px-4 py-4 sm:gap-4 sm:px-6 sm:py-6">
        {options.map((combo, index) => {
          const isSelected = selectedIndex === index;
          // Visual tier: when the combination is two of the same color, use
          // that color's styling; otherwise fall back to a neutral panel.
          const uniqueColors = Array.from(new Set(combo));
          const tint: ManaType | null =
            uniqueColors.length === 1 ? uniqueColors[0] : null;
          const tintClass = tint
            ? isSelected
              ? MANA_COLOR_SELECTED[tint]
              : MANA_COLOR_STYLES[tint]
            : isSelected
              ? "border-gray-300 bg-gray-600/50 text-white"
              : "border-gray-500 bg-gray-700/40 text-gray-200 hover:bg-gray-700/60";
          return (
            <motion.button
              key={index}
              className={`flex items-center justify-center gap-2 rounded-xl border-2 px-5 py-3 transition ${tintClass}`}
              initial={{ opacity: 0, y: 20, scale: 0.9 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.05, duration: 0.25 }}
              whileHover={{ scale: 1.03 }}
              onClick={() => setSelectedIndex(isSelected ? null : index)}
            >
              {combo.map((color, pipIndex) => (
                <ManaSymbol
                  key={pipIndex}
                  shard={MANA_COLOR_SHARDS[color]}
                  size="md"
                />
              ))}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
