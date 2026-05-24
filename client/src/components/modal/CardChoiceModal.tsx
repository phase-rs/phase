import { useCallback, useEffect, useMemo, useState } from "react";
import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import type { GameObject, ManaCost, ManaType, ObjectId, TargetFilter, WaitingFor } from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { CancelButton, ChoiceOverlay, ConfirmButton, ScrollableCardStrip } from "./ChoiceOverlay.tsx";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";
import { NamedChoiceModal } from "./NamedChoiceModal.tsx";
import { VoteChoiceModal } from "./VoteChoiceModal.tsx";
import {
  SeparatePilesChoiceModal,
  SeparatePilesPartitionModal,
} from "./SeparatePilesModal.tsx";
import { DungeonChoiceModal, RoomChoiceModal } from "./DungeonChoiceModal.tsx";
import { DamageAssignmentModal } from "../combat/DamageAssignmentModal.tsx";
import { DistributeAmongModal } from "./DistributeAmongModal.tsx";
import { RetargetChoiceModal } from "./RetargetChoiceModal.tsx";
import { ProliferateModal } from "./ProliferateModal.tsx";
import { CategoryChoiceModal } from "./CategoryChoiceModal.tsx";

type ScryChoice = Extract<WaitingFor, { type: "ScryChoice" }>;
type DigChoice = Extract<WaitingFor, { type: "DigChoice" }>;
type SurveilChoice = Extract<WaitingFor, { type: "SurveilChoice" }>;
type RevealChoice = Extract<WaitingFor, { type: "RevealChoice" }>;
type SearchChoice = Extract<WaitingFor, { type: "SearchChoice" }>;
type SearchPartitionChoice = Extract<WaitingFor, { type: "SearchPartitionChoice" }>;
type OutsideGameChoice = Extract<WaitingFor, { type: "OutsideGameChoice" }>;
type ChooseFromZoneChoice = Extract<WaitingFor, { type: "ChooseFromZoneChoice" }>;
type EffectZoneChoice = Extract<WaitingFor, { type: "EffectZoneChoice" }>;
type DrawnThisTurnTopdeckChoice = Extract<WaitingFor, { type: "DrawnThisTurnTopdeckChoice" }>;
type DiscardToHandSize = Extract<WaitingFor, { type: "DiscardToHandSize" }>;
type SacrificeForCost = Extract<WaitingFor, { type: "SacrificeForCost" }>;
type SacrificeForManaAbility = Extract<WaitingFor, { type: "SacrificeForManaAbility" }>;
type DiscardForManaAbility = Extract<WaitingFor, { type: "DiscardForManaAbility" }>;
type ExileFromBattlefieldForManaAbility = Extract<WaitingFor, { type: "ExileFromBattlefieldForManaAbility" }>;
type MultiTargetSelection = Extract<WaitingFor, { type: "MultiTargetSelection" }>;
type ParadigmCastOffer = Extract<WaitingFor, { type: "ParadigmCastOffer" }>;
type PayManaAbilityMana = Extract<WaitingFor, { type: "PayManaAbilityMana" }>;
type ReturnToHandForCost = Extract<WaitingFor, { type: "ReturnToHandForCost" }>;
type RemoveCounterForCost = Extract<WaitingFor, { type: "RemoveCounterForCost" }>;
type BlightChoice = Extract<WaitingFor, { type: "BlightChoice" }>;
type BeholdForCost = Extract<WaitingFor, { type: "BeholdForCost" }>;
type ExileForCost = Extract<WaitingFor, { type: "ExileForCost" }>;
type CollectEvidenceChoice = Extract<WaitingFor, { type: "CollectEvidenceChoice" }>;
type HarmonizeTapChoice = Extract<WaitingFor, { type: "HarmonizeTapChoice" }>;
type PairChoice = Extract<WaitingFor, { type: "PairChoice" }>;
type ChooseLegend = Extract<WaitingFor, { type: "ChooseLegend" }>;
type CommanderZoneChoice = Extract<WaitingFor, { type: "CommanderZoneChoice" }>;
type RevealUntilKeptChoice = Extract<WaitingFor, { type: "RevealUntilKeptChoice" }>;
type RepeatDecision = Extract<WaitingFor, { type: "RepeatDecision" }>;
type ManifestDreadChoice = Extract<WaitingFor, { type: "ManifestDreadChoice" }>;
type CrewVehicle = Extract<WaitingFor, { type: "CrewVehicle" }>;
type StationTarget = Extract<WaitingFor, { type: "StationTarget" }>;
type SaddleMount = Extract<WaitingFor, { type: "SaddleMount" }>;
type DamageSourceChoice = Extract<WaitingFor, { type: "DamageSourceChoice" }>;
type ChooseRingBearer = Extract<WaitingFor, { type: "ChooseRingBearer" }>;
const CHOICE_CARD_IMAGE_CLASS = "";
const SCRY_CARD_IMAGE_CLASS = "";

function objectImageProps(obj: GameObject) {
  const { name, faceIndex, oracleId, faceName } = cardImageLookup(obj);
  const isToken = obj.display_source === "Token";
  return {
    cardName: name,
    faceIndex,
    oracleId,
    faceName,
    isToken,
    tokenFilters: isToken ? tokenFiltersForObject(obj) : undefined,
    tokenImageRef: isToken ? obj.token_image_ref : undefined,
  };
}

function CostActionFooter({
  onCancel,
  children,
}: {
  onCancel: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="mx-auto flex w-full max-w-xl flex-col gap-2 sm:flex-row">
      <div className="flex-1">
        <CancelButton onClick={onCancel} />
      </div>
      <div className="flex-1">
        {children}
      </div>
    </div>
  );
}

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

function searchChoiceSubtitle(data: SearchChoice["data"]): string {
  const countText = data.up_to ? `up to ${data.count}` : `${data.count}`;
  const cardText = `card${data.count > 1 ? "s" : ""}`;
  const constraint = data.constraint;

  if (constraint?.type === "MatchEachFilter") {
    return `Choose ${countText} ${cardText} matching the listed search requirements`;
  }
  if (constraint?.type === "DistinctQualities") {
    return `Choose ${countText} ${cardText} with distinct qualities`;
  }
  if (constraint?.type === "TotalManaValue") {
    return `Choose ${countText} ${cardText} within the mana value limit`;
  }

  return `Choose ${countText} ${cardText}`;
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
    case "SearchPartitionChoice":
      if (!canActForWaitingState) return null;
      return <SearchPartitionModal data={waitingFor.data} />;
    case "OutsideGameChoice":
      if (!canActForWaitingState) return null;
      return <OutsideGameModal key={outsideGameChoiceKey(waitingFor.data)} data={waitingFor.data} />;
    case "ChooseFromZoneChoice":
      if (!canActForWaitingState) return null;
      return <ChooseFromZoneModal data={waitingFor.data} />;
    case "EffectZoneChoice":
      if (!canActForWaitingState) return null;
      return <EffectZoneModal data={waitingFor.data} />;
    case "DrawnThisTurnTopdeckChoice":
      if (!canActForWaitingState) return null;
      return <DrawnThisTurnTopdeckModal data={waitingFor.data} />;
    case "NamedChoice":
      if (!canActForWaitingState) return null;
      return <NamedChoiceModal data={waitingFor.data} />;
    case "DamageSourceChoice":
      if (!canActForWaitingState) return null;
      return <DamageSourceModal data={waitingFor.data} />;
    case "VoteChoice":
      if (!canActForWaitingState) return null;
      return <VoteChoiceModal data={waitingFor.data} />;
    case "SeparatePilesPartition":
      if (!canActForWaitingState) return null;
      return <SeparatePilesPartitionModal data={waitingFor.data} />;
    case "SeparatePilesChoice":
      if (!canActForWaitingState) return null;
      return <SeparatePilesChoiceModal data={waitingFor.data} />;
    case "DiscardToHandSize":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={waitingFor.data} />;
    case "DiscardForCost":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={waitingFor.data} title="Discard as additional cost" canCancel />;
    case "SacrificeForCost":
      if (!canActForWaitingState) return null;
      return <SacrificeModal key={waitingFor.data.permanents.join(",")} data={waitingFor.data} />;
    case "SacrificeForManaAbility":
      if (!canActForWaitingState) return null;
      return <SacrificeForManaAbilityModal data={waitingFor.data} />;
    case "DiscardForManaAbility":
      if (!canActForWaitingState) return null;
      return <DiscardModal data={waitingFor.data} title="Discard for mana ability" />;
    case "ExileFromBattlefieldForManaAbility":
      if (!canActForWaitingState) return null;
      return <ExileFromBattlefieldForManaAbilityModal data={waitingFor.data} />;
    case "MultiTargetSelection":
      if (!canActForWaitingState) return null;
      return <MultiTargetSelectionModal data={waitingFor.data} />;
    case "ParadigmCastOffer":
      if (!canActForWaitingState) return null;
      return <ParadigmCastOfferModal data={waitingFor.data} />;
    case "PayManaAbilityMana":
      if (!canActForWaitingState) return null;
      return <PayManaAbilityManaModal data={waitingFor.data} />;
    case "CopyRetarget":
      // Handled by TargetingOverlay + battlefield clicks (ChooseTarget slot-by-slot).
      return null;
    case "ReturnToHandForCost":
      if (!canActForWaitingState) return null;
      return <ReturnToHandModal key={waitingFor.data.permanents.join(",")} data={waitingFor.data} />;
    case "RemoveCounterForCost":
      if (!canActForWaitingState) return null;
      return <RemoveCounterModal key={waitingFor.data.permanents.join(",")} data={waitingFor.data} />;
    case "BlightChoice":
      if (!canActForWaitingState) return null;
      return <BlightModal data={waitingFor.data} />;
    case "BeholdForCost":
      if (!canActForWaitingState) return null;
      return <BeholdModal data={waitingFor.data} />;
    case "CrewVehicle":
      if (!canActForWaitingState) return null;
      return <CrewModal data={waitingFor.data} />;
    case "StationTarget":
      if (!canActForWaitingState) return null;
      return <StationTargetModal data={waitingFor.data} />;
    case "SaddleMount":
      if (!canActForWaitingState) return null;
      return <SaddleModal data={waitingFor.data} />;
    case "ExileForCost":
      if (!canActForWaitingState) return null;
      return <ExileForCostDispatch data={waitingFor.data} />;
    case "CollectEvidenceChoice":
      if (!canActForWaitingState) return null;
      return <CollectEvidenceModal data={waitingFor.data} />;
    case "HarmonizeTapChoice":
      if (!canActForWaitingState) return null;
      return <HarmonizeTapModal data={waitingFor.data} />;
    case "PairChoice":
      if (!canActForWaitingState) return null;
      return <PairChoiceModal data={waitingFor.data} />;
    case "ChooseLegend":
      if (!canActForWaitingState) return null;
      return <LegendChoiceModal data={waitingFor.data} />;
    case "CommanderZoneChoice":
      if (!canActForWaitingState) return null;
      return <CommanderZoneChoiceModal data={waitingFor.data} />;
    case "RevealUntilKeptChoice":
      if (!canActForWaitingState) return null;
      return <RevealUntilKeptChoiceModal data={waitingFor.data} />;
    case "RepeatDecision":
      if (!canActForWaitingState) return null;
      return <RepeatDecisionModal data={waitingFor.data} />;
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
    case "UnlessBounceChoice":
      if (!canActForWaitingState) return null;
      return <UnlessBounceModal data={waitingFor.data} />;
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
    case "ChooseObjectsSelection":
      if (!canActForWaitingState) return null;
      return (
        <ProliferateModal data={waitingFor.data} variant="chooseObjects" />
      );
    case "CategoryChoice":
      if (!canActForWaitingState) return null;
      return <CategoryChoiceModal data={waitingFor.data} />;
    case "ManifestDreadChoice":
      if (!canActForWaitingState) return null;
      return <ManifestDreadModal data={waitingFor.data} />;
    case "ChooseDungeon":
      if (!canActForWaitingState) return null;
      return <DungeonChoiceModal data={waitingFor.data} />;
    case "ChooseDungeonRoom":
      if (!canActForWaitingState) return null;
      return <RoomChoiceModal data={waitingFor.data} />;
    case "ChooseRingBearer":
      if (!canActForWaitingState) return null;
      return <RingBearerModal data={waitingFor.data} />;
    case "ChooseManaColor":
      if (!canActForWaitingState) return null;
      return <ManaColorChoiceModal data={waitingFor.data} />;
    default:
      return null;
  }
}

// ── Ring-bearer Modal ──────────────────────────────────────────────────────

function RingBearerModal({ data }: { data: ChooseRingBearer["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<ObjectId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseRingBearer", data: { target: selected } });
    }
  }, [dispatch, selected]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Choose Ring-bearer"
      subtitle="Choose a creature you control"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
    >
      <ScrollableCardStrip>
        {data.candidates.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected === id;
          return (
            <motion.button
              key={id}
              type="button"
              aria-label={obj.name}
              className={`relative flex flex-col items-center gap-2 rounded-lg transition ${
                isSelected
                  ? "ring-2 ring-emerald-400/80"
                  : "ring-1 ring-white/10 hover:ring-white/35"
              }`}
              initial={{ opacity: 0, y: 40, scale: 0.9 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => setSelected(id)}
              {...hoverProps(id)}
            >
              <CardImage {...objectImageProps(obj)} size="normal" />
              <span
                className={`rounded-full px-3 py-1 text-xs font-bold transition ${
                  isSelected
                    ? "bg-emerald-500/80 text-white"
                    : "bg-slate-800/90 text-slate-300"
                }`}
              >
                {isSelected ? "Selected" : "Choose"}
              </span>
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
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
                  {...objectImageProps(obj)}
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

  const isReorderOnly =
    data.kept_destination === "Library" &&
    data.rest_destination === "Library" &&
    data.keep_count === data.cards.length;

  const isReady = isUpTo
    ? selected.size <= data.keep_count
    : selected.size === data.keep_count;

  const destLabel =
    isReorderOnly
      ? "on top of your library"
      : data.kept_destination === "Battlefield"
      ? "onto the battlefield"
      : "into your hand";

  const countLabel = isUpTo
    ? `up to ${data.keep_count}`
    : `${data.keep_count}`;
  const title = isReorderOnly ? "Reorder Cards" : "Choose Cards";
  const subtitle = isReorderOnly
    ? `Select all ${data.cards.length} cards in top-to-bottom order`
    : `Select ${countLabel} card${data.keep_count > 1 ? "s" : ""} to put ${destLabel}`;
  const confirmLabel = isReorderOnly
    ? `Confirm Order (${selected.size}/${data.keep_count})`
    : `Confirm (${selected.size}/${data.keep_count})`;

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!isReady}
          label={confirmLabel}
        />
      }
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          const isSelectable = selectableSet.has(id);
          const selectedOrder = Array.from(selected).indexOf(id) + 1;
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
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    {isReorderOnly ? selectedOrder : "Keep"}
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
                  {...objectImageProps(obj)}
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
                {...objectImageProps(obj)}
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
  const countValid = data.up_to
    ? selectedSet.size <= data.count
    : selectedSet.size === data.count;
  const subtitle = searchChoiceSubtitle(data);

  useEffect(() => {
    setSelectedSet(new Set());
  }, [data]);

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
    if (countValid) {
      dispatch({
        type: "SelectCards",
        data: { cards: Array.from(selectedSet) },
      });
    }
  }, [countValid, dispatch, selectedSet]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Search Library"
      subtitle={subtitle}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!countValid} />}
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
                {...objectImageProps(obj)}
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

function SearchPartitionModal({ data }: { data: SearchPartitionChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selectedSet, setSelectedSet] = useState<Set<ObjectId>>(new Set());
  const countValid = selectedSet.size === data.primary_count;
  const cardText = `card${data.primary_count > 1 ? "s" : ""}`;
  const tappedText = data.primary_enter_tapped ? " tapped" : "";

  useEffect(() => {
    setSelectedSet(new Set());
  }, [data]);

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelectedSet((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < data.primary_count) {
          next.add(id);
        }
        return next;
      });
    },
    [data.primary_count],
  );

  const handleConfirm = useCallback(() => {
    if (countValid) {
      dispatch({
        type: "SelectCards",
        data: { cards: Array.from(selectedSet) },
      });
    }
  }, [countValid, dispatch, selectedSet]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Choose Cards for the Battlefield"
      subtitle={`Choose ${data.primary_count} ${cardText} to put onto the battlefield${tappedText}; the rest go to your hand`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!countValid} />}
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
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-emerald-500/20">
                  <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
                    Battlefield
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

function OutsideGameModal({ data }: { data: OutsideGameChoice["data"] }) {
  const dispatch = useGameDispatch();
  const [selectedCounts, setSelectedCounts] = useState<Map<number, number>>(new Map());
  const availableCounts = useMemo(
    () => new Map(data.choices.map((choice) => [choice.sideboard_index, choice.entry.count])),
    [data.choices],
  );
  const selectedIndices = useMemo(
    () =>
      Array.from(selectedCounts.entries()).flatMap(([sideboardIndex, count]) => {
        const availableCount = availableCounts.get(sideboardIndex) ?? 0;
        return Array.from({ length: Math.min(count, availableCount) }, () => sideboardIndex);
      }),
    [availableCounts, selectedCounts],
  );
  const minCount = data.up_to ? 0 : data.count;
  const countValid = selectedIndices.length >= minCount && selectedIndices.length <= data.count;

  const toggleSelect = useCallback(
    (sideboardIndex: number, maxCopies: number) => {
      setSelectedCounts((prev) => {
        const next = new Map(prev);
        const current = next.get(sideboardIndex) ?? 0;
        const selectedTotal = Array.from(prev.values()).reduce((sum, count) => sum + count, 0);
        if (current > 0 && (current >= maxCopies || selectedTotal >= data.count)) {
          next.delete(sideboardIndex);
        } else if (selectedTotal < data.count) {
          next.set(sideboardIndex, current + 1);
        }
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    if (countValid) {
      dispatch({
        type: "ChooseOutsideGameCards",
        data: { sideboard_indices: selectedIndices },
      });
    }
  }, [countValid, dispatch, selectedIndices]);

  return (
    <ChoiceOverlay
      title="Choose From Sideboard"
      subtitle={`Choose ${data.up_to ? "up to " : ""}${data.count}`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!countValid} />}
    >
      <div className="flex max-h-[60vh] min-w-[280px] flex-col gap-2 overflow-y-auto p-1">
        {data.choices.map((choice) => {
          const selectedCount = Math.min(
            selectedCounts.get(choice.sideboard_index) ?? 0,
            choice.entry.count,
          );
          const isSelected = selectedCount > 0;
          return (
            <button
              key={choice.sideboard_index}
              type="button"
              className={`flex items-center justify-between rounded-md border px-3 py-2 text-left text-sm transition ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/20 text-white"
                  : "border-white/15 bg-black/30 text-zinc-100 hover:bg-white/10"
              }`}
              onClick={() => toggleSelect(choice.sideboard_index, choice.entry.count)}
            >
              <span>{choice.entry.card.name}</span>
              <span className="text-xs text-zinc-400">
                {isSelected ? `${selectedCount}/` : ""}x{choice.entry.count}
              </span>
            </button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}

function outsideGameChoiceKey(data: OutsideGameChoice["data"]) {
  const choicesKey = data.choices
    .map((choice) => `${choice.sideboard_index}:${choice.entry.count}`)
    .join(",");
  return `${data.player}:${data.count}:${data.up_to ?? false}:${data.destination}:${choicesKey}`;
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
                {...objectImageProps(obj)}
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

function PairChoiceModal({ data }: { data: PairChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const handleChoose = useCallback(
    (id: ObjectId | null) => {
      dispatch({
        type: "ChoosePair",
        data: { partner: id },
      });
    },
    [dispatch],
  );

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Choose Soulbond Partner"
      subtitle="Pair with an unpaired creature you control"
      footer={(
        <div className="mx-auto w-full max-w-xl">
          <CancelButton onClick={() => handleChoose(null)} label="Decline" />
        </div>
      )}
    >
      <ScrollableCardStrip>
        {data.choices.map((id) => {
          const obj = objects[id];
          if (!obj) return null;
          return (
            <motion.button
              key={id}
              type="button"
              className="relative flex-shrink-0 rounded-lg border-2 border-transparent transition hover:border-emerald-400"
              onClick={() => handleChoose(id)}
              {...hoverProps(id)}
            >
              <CardImage
                {...objectImageProps(obj)}
                className={CHOICE_CARD_IMAGE_CLASS}
              />
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
  const isSacrifice = data.zone === "Battlefield" && data.destination == null;
  const isUpTo = data.up_to === true;
  const minCount = data.min_count ?? 0;

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

  const isTopdeck = data.effect_kind === "PutAtLibraryPosition";
  const selectedOrder = isTopdeck ? Array.from(selected) : [];
  const selectedOrderLabels = selectedOrder.map((_, index) => formatTopdeckOrderLabel(index));
  const isReady = isUpTo
    ? selected.size >= minCount && selected.size <= data.count
    : selected.size === data.count;
  const title = isSacrifice ? "Sacrifice" : isTopdeck ? "Put on Library" : "Put onto Battlefield";
  const subtitle = isSacrifice
    ? isUpTo
      ? minCount > 0
        ? `Choose ${minCount}-${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`
        : `Choose up to ${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`
      : `Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`
    : isTopdeck
      ? isUpTo
        ? minCount > 0
          ? `Choose ${minCount}-${data.count} card${data.count > 1 ? "s" : ""} to put on top of your library`
          : `Choose up to ${data.count} card${data.count > 1 ? "s" : ""} to put on top of your library`
        : `Choose ${data.count} card${data.count > 1 ? "s" : ""} to put on top of your library`
    : isUpTo
      ? minCount > 0
        ? `Choose ${minCount}-${data.count} card${data.count > 1 ? "s" : ""} to put onto the battlefield`
        : `Choose up to ${data.count} card${data.count > 1 ? "s" : ""} to put onto the battlefield`
      : `Choose ${data.count} card${data.count > 1 ? "s" : ""} to put onto the battlefield`;
  const actionLabel = selected.size === 0 && isUpTo && minCount === 0
    ? (isSacrifice ? "Skip" : "Decline")
    : isTopdeck && selectedOrderLabels.length > 0
      ? `Put on top (${selectedOrderLabels.join(" -> ")})`
      : `${isSacrifice ? "Confirm" : isTopdeck ? "Top" : "Put"} (${selected.size}/${data.count})`;
  const ringClass = isSacrifice ? "ring-red-400/80" : isTopdeck ? "ring-sky-300/80" : "ring-emerald-400/80";
  const overlayClass = isSacrifice ? "bg-red-500/20" : isTopdeck ? "bg-sky-500/20" : "bg-emerald-500/20";
  const badgeClass = isSacrifice ? "bg-red-500/90" : isTopdeck ? "bg-sky-500/90" : "bg-emerald-500/90";

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
          const selectedIndex = selectedOrder.indexOf(id);
          const badgeLabel = isSacrifice
            ? "Sacrifice"
            : isTopdeck && selectedIndex >= 0
              ? formatTopdeckOrderLabel(selectedIndex)
              : "Put";
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
                {...objectImageProps(obj)}
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

function formatTopdeckOrderLabel(index: number): string {
  if (index === 0) return "Top";
  const position = index + 1;
  const suffix = position === 2 ? "nd" : position === 3 ? "rd" : "th";
  return `${position}${suffix}`;
}

function DrawnThisTurnTopdeckModal({ data }: { data: DrawnThisTurnTopdeckChoice["data"] }) {
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

  const payments = data.count - selected.size;
  const actionLabel =
    selected.size === 0 ? `Pay ${payments * data.life_payment} life` : `Confirm (${selected.size}/${data.count})`;
  const disabled = selected.size < data.min_count || selected.size > data.count;

  return (
    <ChoiceOverlay
      title="Drawn This Turn"
      subtitle={`Put up to ${data.count} on top; pay ${data.life_payment} life for each kept`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={disabled} label={actionLabel} />}
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
                  ? "z-10 ring-2 ring-sky-300/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage {...objectImageProps(obj)} size="normal" className={CHOICE_CARD_IMAGE_CLASS} />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-sky-500/20">
                  <span className="rounded-full bg-sky-500/90 px-3 py-1 text-xs font-bold text-white">
                    Top
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
      choices={data.permanents}
      title="Sacrifice"
      subtitle={`Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`}
      label="Sacrifice"
      selectedClassName="z-10 ring-2 ring-red-400/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-red-500/20"
      badgeClassName="rounded-full bg-red-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

function SacrificeForManaAbilityModal({ data }: { data: SacrificeForManaAbility["data"] }) {
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
    dispatch({ type: "SelectCards", data: { cards: Array.from(selected) } });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = selected.size === data.count;

  return (
    <ChoiceOverlay
      title="Sacrifice"
      subtitle={`Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to sacrifice`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Sacrifice (${selected.size}/${data.count})`} />}
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
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-red-500/20">
                  <span className="rounded-full bg-red-500/90 px-3 py-1 text-xs font-bold text-white">Sacrifice</span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Exile From Battlefield For Mana Ability Modal ─────────────────────────────

function ExileFromBattlefieldForManaAbilityModal({ data }: { data: ExileFromBattlefieldForManaAbility["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id);
        else if (next.size < data.count) next.add(id);
        return next;
      });
    },
    [data.count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectCards", data: { cards: Array.from(selected) } });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = selected.size === data.count;

  return (
    <ChoiceOverlay
      title="Exile"
      subtitle={`Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to exile`}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Exile (${selected.size}/${data.count})`} />}
    >
      <ScrollableCardStrip>
        {data.permanents.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${isSelected ? "z-10 ring-2 ring-amber-400/80" : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"}`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage {...objectImageProps(obj)} size="normal" className={CHOICE_CARD_IMAGE_CLASS} />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-amber-500/20">
                  <span className="rounded-full bg-amber-500/90 px-3 py-1 text-xs font-bold text-white">Exile</span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Multi-Target Selection Modal ──────────────────────────────────────────────

function MultiTargetSelectionModal({ data }: { data: MultiTargetSelection["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) next.delete(id);
        else if (next.size < data.max_targets) next.add(id);
        return next;
      });
    },
    [data.max_targets],
  );

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectCards", data: { cards: Array.from(selected) } });
  }, [dispatch, selected]);

  if (!objects) return null;

  const isReady = selected.size >= data.min_targets && selected.size <= data.max_targets;
  const subtitle = data.min_targets === data.max_targets
    ? `Choose ${data.max_targets} target${data.max_targets > 1 ? "s" : ""}`
    : `Choose ${data.min_targets}–${data.max_targets} targets`;

  return (
    <ChoiceOverlay
      title="Choose Targets"
      subtitle={subtitle}
      footer={<ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Confirm (${selected.size}/${data.max_targets})`} />}
    >
      <ScrollableCardStrip>
        {data.legal_targets.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${isSelected ? "z-10 ring-2 ring-cyan-400/80" : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"}`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => toggleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage {...objectImageProps(obj)} size="normal" className={CHOICE_CARD_IMAGE_CLASS} />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-cyan-500/20">
                  <span className="rounded-full bg-cyan-500/90 px-3 py-1 text-xs font-bold text-white">Target</span>
                </div>
              )}
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Paradigm Cast Offer Modal ─────────────────────────────────────────────────

function ParadigmCastOfferModal({ data }: { data: ParadigmCastOffer["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const handleSelect = useCallback(
    (id: ObjectId) => dispatch({ type: "CastParadigmCopy", data: { source: id } }),
    [dispatch],
  );
  const handlePass = useCallback(
    () => dispatch({ type: "PassParadigmOffer" }),
    [dispatch],
  );

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Paradigm"
      subtitle="Cast a copy of one of these spells without paying its mana cost, or pass."
      footer={
        <div className="mx-auto flex w-full max-w-xl gap-2">
          <div className="flex-1">
            <CancelButton onClick={handlePass} label="Pass" />
          </div>
        </div>
      }
    >
      <ScrollableCardStrip>
        {data.offers.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          return (
            <motion.button
              key={id}
              className="relative rounded-lg transition hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => handleSelect(id)}
              {...hoverProps(id)}
            >
              <CardImage {...objectImageProps(obj)} size="normal" className={CHOICE_CARD_IMAGE_CLASS} />
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Pay Mana Ability Mana Modal ───────────────────────────────────────────────

function ReturnToHandModal({ data }: { data: ReturnToHandForCost["data"] }) {
  return (
    <PermanentCostModal
      data={data}
      choices={data.permanents}
      title="Return"
      subtitle={`Choose ${data.count} permanent${data.count > 1 ? "s" : ""} to return`}
      label="Return"
      selectedClassName="z-10 ring-2 ring-sky-300/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-sky-500/20"
      badgeClassName="rounded-full bg-sky-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

function RemoveCounterModal({ data }: { data: RemoveCounterForCost["data"] }) {
  return (
    <PermanentCostModal
      data={data}
      choices={data.permanents}
      title="Remove Counter"
      subtitle="Choose a permanent to remove a counter from"
      label="Remove"
      selectedClassName="z-10 ring-2 ring-violet-300/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-violet-500/20"
      badgeClassName="rounded-full bg-violet-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

function PermanentCostModal({
  data,
  choices,
  title,
  subtitle,
  label,
  selectedClassName,
  overlayClassName,
  badgeClassName,
}: {
  data:
    | SacrificeForCost["data"]
    | ReturnToHandForCost["data"]
    | RemoveCounterForCost["data"]
    | ExileFromBattlefieldForManaAbility["data"]
    | SacrificeForManaAbility["data"];
  choices: ObjectId[];
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

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  if (!objects) return null;

  const isReady = selected.size === data.count;

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`${label} (${selected.size}/${data.count})`} />
        </CostActionFooter>
      }
    >
      <ScrollableCardStrip>
        {choices.map((id, index) => {
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
                {...objectImageProps(obj)}
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

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  if (!objects) return null;

  const isReady = selected.size === data.count;

  return (
    <ChoiceOverlay
      title="Blight"
      subtitle={`Put a -1/-1 counter on ${data.count} creature${data.count > 1 ? "s" : ""} you control`}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Confirm (${selected.size}/${data.count})`} />
        </CostActionFooter>
      }
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
                {...objectImageProps(obj)}
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
                {...objectImageProps(obj)}
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
                {...objectImageProps(obj)}
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
                {...objectImageProps(obj)}
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
                {...objectImageProps(obj)}
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

// ── Unless Bounce Modal ─────────────────────────────────────────────────────

type UnlessBounceChoice = Extract<WaitingFor, { type: "UnlessBounceChoice" }>;

function UnlessBounceModal({ data }: { data: UnlessBounceChoice["data"] }) {
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
      title={data.remaining > 1 ? `Return ${data.remaining} permanents to hand` : "Return a permanent to hand"}
      subtitle="Choose a permanent to return to its owner's hand"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected == null} label="Return" />}
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
                  ? "z-10 ring-2 ring-blue-400/80"
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
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-blue-500/20">
                  <span className="rounded-full bg-blue-500/90 px-3 py-1 text-xs font-bold text-white">
                    Return
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

// ── Shared exile-for-cost modal (graveyard and hand variants share this) ─────

function ExileForCostModal({
  cards,
  count,
  title,
  subtitle,
  confirmLabel = "Exile",
}: {
  cards: ObjectId[];
  count: number;
  title: string;
  subtitle: string;
  confirmLabel?: string;
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
        } else if (next.size < count) {
          next.add(id);
        }
        return next;
      });
    },
    [count],
  );

  const handleConfirm = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: Array.from(selected) },
    });
  }, [dispatch, selected]);

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  if (!objects) return null;

  const isReady = selected.size === count;

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`${confirmLabel} (${selected.size}/${count})`} />
        </CostActionFooter>
      }
    >
      <ScrollableCardStrip>
        {cards.map((id, index) => {
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
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-purple-500/20">
                  <span className="rounded-full bg-purple-500/90 px-3 py-1 text-xs font-bold text-white">
                    {confirmLabel}
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

function ExileForCostDispatch({ data }: { data: ExileForCost["data"] }) {
  let title: string;
  let sourceLabel: string;
  switch (data.zone) {
    case "Hand":
      title = "Alternative cost";
      sourceLabel = "your hand";
      break;
    case "Graveyard":
      title = "Escape";
      sourceLabel = "your graveyard";
      break;
  }
  return (
    <ExileForCostModal
      cards={data.cards}
      count={data.count}
      title={title}
      subtitle={`Exile ${data.count} card${data.count > 1 ? "s" : ""} from ${sourceLabel}`}
    />
  );
}

function BeholdModal({ data }: { data: BeholdForCost["data"] }) {
  const exilesChosen = data.action === "ExileChosen";
  return (
    <ExileForCostModal
      cards={data.choices}
      count={data.count}
      title="Behold"
      subtitle={exilesChosen ? "Exile a matching permanent or card" : "Choose a matching permanent or reveal a matching card"}
      confirmLabel={exilesChosen ? "Exile" : "Behold"}
    />
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

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

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
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Collect (${total}/${data.minimum_mana_value})`} />
        </CostActionFooter>
      }
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
                {...objectImageProps(obj)}
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

function DiscardModal({
  data,
  title = "Discard",
  canCancel = false,
}: {
  data: (DiscardToHandSize["data"] | DiscardForManaAbility["data"]) & {
    up_to?: boolean;
    unless_filter?: TargetFilter;
  };
  title?: string;
  canCancel?: boolean;
}) {
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

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

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
      footer={
        canCancel ? (
          <CostActionFooter onCancel={handleCancel}>
            <ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Discard (${selected.size}/${data.count})`} />
          </CostActionFooter>
        ) : (
          <ConfirmButton onClick={handleConfirm} disabled={!isReady} label={`Discard (${selected.size}/${data.count})`} />
        )
      }
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
                {...objectImageProps(obj)}
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

  const handleCancel = useCallback(() => {
    dispatch({ type: "CancelCast" });
  }, [dispatch]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Harmonize"
      subtitle="Tap a creature to reduce casting cost by its power, or skip"
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton onClick={handleSkip} label="Skip (pay full cost)" />
        </CostActionFooter>
      }
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
                {...objectImageProps(obj)}
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
  const gameState = useGameStore((s) => s.gameState);
  const objects = gameState?.objects;
  const turnNumber = gameState?.turn_number;
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
          const isCurrentTurnEntry =
            turnNumber != null && obj.entered_battlefield_turn === turnNumber;
          const entryLabel = isCurrentTurnEntry ? "Just entered" : "Already on battlefield";
          return (
            <motion.button
              key={id}
              aria-label={`Keep ${obj.name} (${entryLabel})`}
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
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              <div className="absolute top-2 left-1/2 -translate-x-1/2">
                <span
                  className={`whitespace-nowrap rounded-full px-2 py-0.5 text-[11px] font-bold text-white shadow ${
                    isCurrentTurnEntry
                      ? "bg-amber-500/95"
                      : "bg-sky-700/95"
                  }`}
                >
                  {entryLabel}
                </span>
              </div>
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Commander Zone Choice Modal (CR 903.9a) ───────────────────────────────

function CommanderZoneChoiceModal({ data }: { data: CommanderZoneChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  const obj = objects[data.commander_id];
  const zoneName = data.current_zone.charAt(0).toUpperCase() + data.current_zone.slice(1);

  return (
    <ChoiceOverlay
      title="Commander Zone"
      subtitle={`${obj?.name ?? "Commander"} was put into the ${zoneName}. Return to the Command Zone?`}
    >
      <div className="flex items-center gap-6">
        <motion.div
          className="relative rounded-lg"
          initial={{ opacity: 0, y: 60, scale: 0.85 }}
          animate={{ opacity: 0.85, y: 0, scale: 1 }}
          transition={{ delay: 0.1, duration: 0.35 }}
          {...hoverProps(data.commander_id)}
        >
          <CardImage
            cardName={obj?.name ?? "Unknown"}
            size="normal"
            className={CHOICE_CARD_IMAGE_CLASS}
          />
        </motion.div>
        <div className="flex flex-col gap-3">
          <ConfirmButton
            label="Command Zone"
            onClick={() => dispatch({ type: "DecideOptionalEffect", data: { accept: true } })}
          />
          <ConfirmButton
            label={`Leave in ${zoneName}`}
            onClick={() => dispatch({ type: "DecideOptionalEffect", data: { accept: false } })}
          />
        </div>
      </div>
    </ChoiceOverlay>
  );
}

// ── Reveal Until Kept Choice Modal (CR 701.20a) ───────────────────────────

function RevealUntilKeptChoiceModal({ data }: { data: RevealUntilKeptChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  const obj = objects[data.hit_card];
  const declineZone = data.decline_zone.charAt(0).toUpperCase() + data.decline_zone.slice(1);

  return (
    <ChoiceOverlay
      title="Reveal Until"
      subtitle={`Put ${obj?.name ?? "the revealed card"} onto the battlefield?`}
    >
      <div className="flex items-center gap-6">
        <motion.div
          className="relative rounded-lg"
          initial={{ opacity: 0, y: 60, scale: 0.85 }}
          animate={{ opacity: 0.85, y: 0, scale: 1 }}
          transition={{ delay: 0.1, duration: 0.35 }}
          {...hoverProps(data.hit_card)}
        >
          <CardImage
            cardName={obj?.name ?? "Unknown"}
            size="normal"
            className={CHOICE_CARD_IMAGE_CLASS}
          />
        </motion.div>
        <div className="flex flex-col gap-3">
          <ConfirmButton
            label="Onto the battlefield"
            onClick={() => dispatch({ type: "DecideOptionalEffect", data: { accept: true } })}
          />
          <ConfirmButton
            label={`Into ${declineZone}`}
            onClick={() => dispatch({ type: "DecideOptionalEffect", data: { accept: false } })}
          />
        </div>
      </div>
    </ChoiceOverlay>
  );
}

// ── Repeat Decision Modal ──────────────────────────────────────────────────

function RepeatDecisionModal({ data: _data }: { data: RepeatDecision["data"] }) {
  const dispatch = useGameDispatch();

  return (
    <ChoiceOverlay
      title="Repeat This Process"
      subtitle="Repeat the process again?"
    >
      <div className="flex flex-col gap-3">
        <ConfirmButton
          label="Repeat"
          onClick={() => dispatch({ type: "DecideOptionalEffect", data: { accept: true } })}
        />
        <ConfirmButton
          label="Stop"
          onClick={() => dispatch({ type: "DecideOptionalEffect", data: { accept: false } })}
        />
      </div>
    </ChoiceOverlay>
  );
}

// ── Damage Source Choice Modal ─────────────────────────────────────────────

function DamageSourceModal({ data }: { data: DamageSourceChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Damage Source"
      subtitle="Choose a source"
    >
      <ScrollableCardStrip>
        {data.options.map((id, index) => {
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
                dispatch({ type: "ChooseDamageSource", data: { source: id } })
              }
              {...hoverProps(id)}
            >
              <CardImage
                {...objectImageProps(obj)}
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
                {...objectImageProps(obj)}
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
  // sequence). `AnyCombination` is a per-mana-slot spell/effect choice
  // (Manamorphose). All share this single modal — the engine dispatches a
  // `ManaChoice` whose shape mirrors the prompt.
  if (data.choice.type === "Combination") {
    return <ManaCombinationChoiceModal options={data.choice.data.options} />;
  }
  if (data.choice.type === "AnyCombination") {
    return (
      <ManaAnyCombinationChoiceModal
        count={data.choice.data.count}
        options={data.choice.data.options}
      />
    );
  }
  return <ManaSingleColorChoiceModal options={data.choice.data.options} />;
}

function PayManaAbilityManaModal({ data }: { data: PayManaAbilityMana["data"] }) {
  return (
    <ManaCombinationChoiceModal
      options={data.options}
      title="Pay Mana Ability Cost"
      subtitle="Select which mana to spend"
      actionType="PayManaAbilityMana"
    />
  );
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

function ManaAnyCombinationChoiceModal({
  count,
  options,
}: {
  count: number;
  options: ManaType[];
}) {
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<(ManaType | null)[]>(
    Array.from({ length: count }, () => null),
  );

  const handleSelect = useCallback((slot: number, color: ManaType) => {
    setSelected((current) => {
      const next = [...current];
      next[slot] = color;
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    if (selected.every((color): color is ManaType => color !== null)) {
      dispatch({
        type: "ChooseManaColor",
        data: {
          choice: { type: "Combination", data: selected },
        },
      });
    }
  }, [dispatch, selected]);

  return (
    <ChoiceOverlay
      title="Choose Mana Combination"
      subtitle="Select each mana color to produce"
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-lg"
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected.some((color) => color === null)}
        />
      }
    >
      <div className="mx-auto flex w-fit flex-col gap-4 px-4 py-4 sm:px-6 sm:py-6">
        {selected.map((slotColor, slot) => (
          <div key={slot} className="flex items-center justify-center gap-3">
            {options.map((color) => {
              const isSelected = slotColor === color;
              return (
                <motion.button
                  key={`${slot}-${color}`}
                  className={`flex h-12 w-12 items-center justify-center rounded-full border-2 transition sm:h-14 sm:w-14 ${
                    isSelected ? MANA_COLOR_SELECTED[color] : MANA_COLOR_STYLES[color]
                  }`}
                  initial={{ opacity: 0, y: 10, scale: 0.95 }}
                  animate={{ opacity: 1, y: 0, scale: 1 }}
                  transition={{ delay: 0.04 + slot * 0.04, duration: 0.2 }}
                  whileHover={{ scale: 1.08 }}
                  onClick={() => handleSelect(slot, color)}
                >
                  <ManaSymbol shard={MANA_COLOR_SHARDS[color]} size="md" />
                </motion.button>
              );
            })}
          </div>
        ))}
      </div>
    </ChoiceOverlay>
  );
}

// CR 605.3b + CR 106.1a: Filter-land combination picker (Shadowmoor/Eventide).
// Renders one button per combination option, each showing the full mana
// sequence with the source pips side-by-side.
function ManaCombinationChoiceModal({
  options,
  title = "Choose Mana Combination",
  subtitle = "Select which combination of mana to produce",
  actionType = "ChooseManaColor",
}: {
  options: ManaType[][];
  title?: string;
  subtitle?: string;
  actionType?: "ChooseManaColor" | "PayManaAbilityMana";
}) {
  const dispatch = useGameDispatch();
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);

  const handleConfirm = useCallback(() => {
    if (selectedIndex !== null) {
      if (actionType === "PayManaAbilityMana") {
        dispatch({
          type: "PayManaAbilityMana",
          data: { payment: options[selectedIndex] },
        });
      } else {
        dispatch({
          type: "ChooseManaColor",
          data: {
            choice: { type: "Combination", data: options[selectedIndex] },
          },
        });
      }
    }
  }, [actionType, dispatch, options, selectedIndex]);

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
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
