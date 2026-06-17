import { useCallback, useEffect, useMemo, useState } from "react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import { CardImage } from "../card/CardImage.tsx";
import { objectImageProps } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import type {
  CounterMatch,
  CounterType,
  ExileCostSourceZone,
  GameObject,
  ManaCost,
  ManaType,
  ObjectId,
  OutsideGameChoiceEntry,
  OutsideGameSelection,
  PlayerId,
  TargetFilter,
  WaitingFor,
  Zone,
} from "../../adapter/types.ts";
import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import {
  CancelButton,
  ChoiceOverlay,
  ConfirmButton,
  ScrollableCardStrip,
} from "./ChoiceOverlay.tsx";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";
import { formatCounterType } from "../../viewmodel/cardProps.ts";
import { NamedChoiceModal } from "./NamedChoiceModal.tsx";
import { VoteChoiceModal } from "./VoteChoiceModal.tsx";
import { SpecializeColorModal } from "./SpecializeColorModal.tsx";
import {
  SeparatePilesChoiceModal,
  SeparatePilesPartitionModal,
} from "./SeparatePilesModal.tsx";
import { DungeonChoiceModal, RoomChoiceModal } from "./DungeonChoiceModal.tsx";
import {
  BlockerDamageAssignmentModal,
  DamageAssignmentModal,
} from "../combat/DamageAssignmentModal.tsx";
import { DistributeAmongModal } from "./DistributeAmongModal.tsx";
import { MoveCountersDistributionModal } from "./MoveCountersDistributionModal.tsx";
import { RetargetChoiceModal } from "./RetargetChoiceModal.tsx";
import { ProliferateModal } from "./ProliferateModal.tsx";
import { CategoryChoiceModal } from "./CategoryChoiceModal.tsx";
import {
  CoinFlipKeepModal,
  DigModal,
  RevealModal,
  ScryModal,
  SurveilModal,
} from "./cardChoice/libraryModals.tsx";
import {
  CHOICE_CARD_IMAGE_CLASS,
  CostActionFooter,
  EFFECT_ZONE_ACTION_LABEL_KEYS,
  EFFECT_ZONE_BADGE_KEYS,
  EFFECT_ZONE_VISUAL_CLASSES,
  canAssignDistinctCardTypes,
  searchChoiceSubtitle,
  type EffectZoneMode,
} from "./cardChoice/shared.tsx";
type SearchChoice = Extract<WaitingFor, { type: "SearchChoice" }>;
type SearchPartitionChoice = Extract<
  WaitingFor,
  { type: "SearchPartitionChoice" }
>;
type OutsideGameChoice = Extract<WaitingFor, { type: "OutsideGameChoice" }>;
type ChooseFromZoneChoice = Extract<
  WaitingFor,
  { type: "ChooseFromZoneChoice" }
>;
type EffectZoneChoice = Extract<WaitingFor, { type: "EffectZoneChoice" }>;
type DrawnThisTurnTopdeckChoice = Extract<
  WaitingFor,
  { type: "DrawnThisTurnTopdeckChoice" }
>;
type PayCost = Extract<WaitingFor, { type: "PayCost" }>;
type MultiTargetSelection = Extract<
  WaitingFor,
  { type: "MultiTargetSelection" }
>;
type PayManaAbilityMana = Extract<WaitingFor, { type: "PayManaAbilityMana" }>;
type BlightChoice = Extract<WaitingFor, { type: "BlightChoice" }>;
type CollectEvidenceChoice = Extract<
  WaitingFor,
  { type: "CollectEvidenceChoice" }
>;
type HarmonizeTapChoice = Extract<WaitingFor, { type: "HarmonizeTapChoice" }>;
type PairChoice = Extract<WaitingFor, { type: "PairChoice" }>;
type ChooseLegend = Extract<WaitingFor, { type: "ChooseLegend" }>;
type CommanderZoneChoice = Extract<WaitingFor, { type: "CommanderZoneChoice" }>;
type RevealUntilKeptChoice = Extract<
  WaitingFor,
  { type: "RevealUntilKeptChoice" }
>;
type RepeatDecision = Extract<WaitingFor, { type: "RepeatDecision" }>;
type ManifestDreadChoice = Extract<WaitingFor, { type: "ManifestDreadChoice" }>;
type CrewVehicle = Extract<WaitingFor, { type: "CrewVehicle" }>;
type StationTarget = Extract<WaitingFor, { type: "StationTarget" }>;
type SaddleMount = Extract<WaitingFor, { type: "SaddleMount" }>;
type DamageSourceChoice = Extract<WaitingFor, { type: "DamageSourceChoice" }>;
type ChooseRingBearer = Extract<WaitingFor, { type: "ChooseRingBearer" }>;
type LearnChoice = Extract<WaitingFor, { type: "LearnChoice" }>;

/**
 * Generic card choice modal for Scry, Dig, Surveil, Reveal, Search, and NamedChoice.
 * Renders based on the WaitingFor type.
 */
export function CardChoiceModal() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (!waitingFor) return null;

  switch (waitingFor.type) {
    case "ScryChoice":
      if (!canActForWaitingState) return null;
      return <ScryModal data={waitingFor.data} />;
    case "CoinFlipKeepChoice":
      if (!canActForWaitingState) return null;
      return <CoinFlipKeepModal data={waitingFor.data} />;
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
      return (
        <OutsideGameModal
          key={outsideGameChoiceKey(waitingFor.data)}
          data={waitingFor.data}
        />
      );
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
    case "ChooseUntapSubset":
      if (!canActForWaitingState) return null;
      return <ChooseUntapSubsetModal data={waitingFor.data} />;
    case "PayCost":
      if (!canActForWaitingState) return null;
      return <PayCostDispatch data={waitingFor.data} />;
    case "MultiTargetSelection":
      if (!canActForWaitingState) return null;
      return <MultiTargetSelectionModal data={waitingFor.data} />;
    case "CastOffer":
      if (!canActForWaitingState) return null;
      if (waitingFor.data.kind.type === "Paradigm") {
        return <ParadigmCastOfferModal offers={waitingFor.data.kind.offers} />;
      }
      return null;
    case "PayManaAbilityMana":
      if (!canActForWaitingState) return null;
      return <PayManaAbilityManaModal data={waitingFor.data} />;
    case "CopyRetarget":
      // Handled by TargetingOverlay + battlefield clicks (ChooseTarget slot-by-slot).
      return null;
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
      return (
        <DiscardModal
          data={waitingFor.data}
          title={t("cardChoice.discard.titleConnive", {
            count: waitingFor.data.count,
          })}
        />
      );
    case "DiscardChoice":
      if (!canActForWaitingState) return null;
      return (
        <DiscardModal
          data={waitingFor.data}
          title={
            waitingFor.data.up_to
              ? t("cardChoice.discard.titleUpTo", {
                  count: waitingFor.data.count,
                })
              : t("cardChoice.discard.titleExact", {
                  count: waitingFor.data.count,
                })
          }
        />
      );
    case "WardDiscardChoice":
      if (!canActForWaitingState) return null;
      return (
        <DiscardModal
          data={{ ...waitingFor.data, count: 1 }}
          title={t("cardChoice.discard.titleWard")}
        />
      );
    case "WardSacrificeChoice":
      if (!canActForWaitingState) return null;
      return <WardSacrificeModal data={waitingFor.data} />;
    case "UnlessBounceChoice":
      if (!canActForWaitingState) return null;
      return <UnlessBounceModal data={waitingFor.data} />;
    case "AssignCombatDamage":
      if (!canActForWaitingState) return null;
      return <DamageAssignmentModal data={waitingFor.data} />;
    case "AssignBlockerDamage":
      if (!canActForWaitingState) return null;
      return <BlockerDamageAssignmentModal data={waitingFor.data} />;
    case "DistributeAmong":
      if (!canActForWaitingState) return null;
      return <DistributeAmongModal data={waitingFor.data} />;
    case "MoveCountersDistribution":
      if (!canActForWaitingState) return null;
      return <MoveCountersDistributionModal data={waitingFor.data} />;
    case "RetargetChoice":
      if (!canActForWaitingState) return null;
      // CR 115.7: Single-target retargets are picked directly on the board via
      // TargetingOverlay; only multi-target (`All`-scope) retargets need the dialog.
      if (waitingFor.data.scope.type === "Single") return null;
      return <RetargetChoiceModal data={waitingFor.data} />;
    case "ProliferateChoice":
      if (!canActForWaitingState) return null;
      return <ProliferateModal data={waitingFor.data} />;
    case "TimeTravelChoice":
      if (!canActForWaitingState) return null;
      return (
        <ProliferateModal
          data={waitingFor.data}
          variant={
            waitingFor.data.phase === "Add"
              ? "timeTravelAdd"
              : "timeTravelRemove"
          }
        />
      );
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
    case "LearnChoice":
      if (!canActForWaitingState) return null;
      return <LearnModal data={waitingFor.data} />;
    case "ChooseManaColor":
      if (!canActForWaitingState) return null;
      return <ManaColorChoiceModal data={waitingFor.data} />;
    case "SpecializeColor":
      if (!canActForWaitingState) return null;
      return <SpecializeColorModal data={waitingFor.data} />;
    default:
      return null;
  }
}

// ── Ring-bearer Modal ──────────────────────────────────────────────────────

function RingBearerModal({ data }: { data: ChooseRingBearer["data"] }) {
  const { t } = useTranslation("game");
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
      title={t("cardChoice.ringBearer.title")}
      subtitle={t("cardChoice.ringBearer.subtitle")}
      footer={
        <ConfirmButton onClick={handleConfirm} disabled={selected === null} />
      }
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
                {isSelected
                  ? t("cardChoice.badges.selected")
                  : t("cardChoice.badges.choose")}
              </span>
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Learn Modal ────────────────────────────────────────────────────────────

// CR 701.48a: "Learn" means "You may discard a card. If you do, draw a card. If
// you didn't discard a card, you may reveal a Lesson card you own from outside
// the game and put it into your hand." The second mode (revealing a Lesson from
// outside the game) is engine-deferred, so this modal renders only the rummage
// (discard-then-draw) and skip branches. Selecting a card dispatches a Rummage
// LearnOption (engine discards then draws); skipping dispatches Skip (no-op).
function LearnModal({ data }: { data: LearnChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<ObjectId | null>(null);

  const handleRummage = useCallback(() => {
    if (selected !== null) {
      dispatch({
        type: "LearnDecision",
        data: { choice: { type: "Rummage", data: { card_id: selected } } },
      });
    }
  }, [dispatch, selected]);

  const handleSkip = useCallback(() => {
    dispatch({ type: "LearnDecision", data: { choice: { type: "Skip" } } });
  }, [dispatch]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={t("cardChoice.learn.title")}
      subtitle={t("cardChoice.learn.subtitle")}
      footer={
        <div className="mx-auto flex w-full max-w-xl flex-col gap-2">
          <ConfirmButton
            onClick={handleRummage}
            disabled={selected === null}
            label={t("cardChoice.learn.labelRummage")}
          />
          <CancelButton
            onClick={handleSkip}
            label={t("cardChoice.learn.labelSkip")}
          />
        </div>
      }
    >
      <ScrollableCardStrip>
        {data.hand_cards.map((id, index) => {
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
                {isSelected
                  ? t("cardChoice.badges.selected")
                  : t("cardChoice.badges.choose")}
              </span>
            </motion.button>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

// ── Search Modal ─────────────────────────────────────────────────────────────

function SearchModal({ data }: { data: SearchChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selectedSet, setSelectedSet] = useState<Set<ObjectId>>(new Set());
  const countValid = data.up_to
    ? selectedSet.size <= data.count
    : selectedSet.size === data.count;
  const subtitle = searchChoiceSubtitle(data, t);

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
      title={t("cardChoice.search.title")}
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
                    {t("cardChoice.badges.choose")}
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

function SearchPartitionModal({
  data,
}: {
  data: SearchPartitionChoice["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selectedSet, setSelectedSet] = useState<Set<ObjectId>>(new Set());
  const countValid = selectedSet.size === data.primary_count;
  const tappedText = data.primary_enter_tapped
    ? t("cardChoice.searchPartition.tapped")
    : "";

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
      title={t("cardChoice.searchPartition.title")}
      subtitle={t("cardChoice.searchPartition.subtitle", {
        count: data.primary_count,
        tapped: tappedText,
      })}
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
                    {t("cardChoice.badges.battlefield")}
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

/**
 * Stable string key for an `OutsideGameChoiceEntry`. Sideboard and face-up
 * exile entries share the modal's selection state, so their identities must
 * not collide as raw numbers — namespacing by source variant keeps the two
 * pools disjoint.
 */
function entryKey(entry: OutsideGameChoiceEntry): string {
  switch (entry.source.type) {
    case "Sideboard":
      return `sb:${entry.source.data.sideboard_index}`;
    case "FaceUpExile":
      return `fx:${entry.source.data.object_id}`;
  }
}

/**
 * Lower an `OutsideGameChoiceEntry` to the wire-format `OutsideGameSelection`
 * the engine consumes. Sideboard entries strip the embedded `CardFace`; exile
 * entries pass through their `object_id` unchanged.
 */
function entryToSelection(entry: OutsideGameChoiceEntry): OutsideGameSelection {
  switch (entry.source.type) {
    case "Sideboard":
      return {
        type: "Sideboard",
        data: { sideboard_index: entry.source.data.sideboard_index },
      };
    case "FaceUpExile":
      return {
        type: "FaceUpExile",
        data: { object_id: entry.source.data.object_id },
      };
  }
}

function OutsideGameModal({ data }: { data: OutsideGameChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  // Map keyed by `entryKey(entry)` → number of copies the user has selected.
  const [selectedCounts, setSelectedCounts] = useState<Map<string, number>>(
    new Map(),
  );

  const entriesByKey = useMemo(() => {
    const map = new Map<string, OutsideGameChoiceEntry>();
    for (const entry of data.choices) {
      map.set(entryKey(entry), entry);
    }
    return map;
  }, [data.choices]);

  const selections: OutsideGameSelection[] = useMemo(
    () =>
      Array.from(selectedCounts.entries()).flatMap(([key, count]) => {
        const entry = entriesByKey.get(key);
        if (!entry) return [];
        const clamped = Math.min(count, entry.count);
        return Array.from({ length: clamped }, () => entryToSelection(entry));
      }),
    [entriesByKey, selectedCounts],
  );

  const minCount = data.up_to ? 0 : data.count;
  const countValid =
    selections.length >= minCount && selections.length <= data.count;

  const toggleSelect = useCallback(
    (key: string, maxCopies: number) => {
      setSelectedCounts((prev) => {
        const next = new Map(prev);
        const current = next.get(key) ?? 0;
        const selectedTotal = Array.from(prev.values()).reduce(
          (sum, count) => sum + count,
          0,
        );
        if (
          current > 0 &&
          (current >= maxCopies || selectedTotal >= data.count)
        ) {
          next.delete(key);
        } else if (selectedTotal < data.count) {
          next.set(key, current + 1);
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
        data: { selections },
      });
    }
  }, [countValid, dispatch, selections]);

  return (
    <ChoiceOverlay
      title={t("cardChoice.outsideGame.title")}
      subtitle={
        data.up_to
          ? t("cardChoice.outsideGame.subtitleUpTo", { count: data.count })
          : t("cardChoice.outsideGame.subtitleExact", { count: data.count })
      }
      footer={<ConfirmButton onClick={handleConfirm} disabled={!countValid} />}
    >
      <div className="flex max-h-[60vh] min-w-[280px] flex-col gap-2 overflow-y-auto p-1">
        {data.choices.map((entry) => {
          const key = entryKey(entry);
          const selectedCount = Math.min(
            selectedCounts.get(key) ?? 0,
            entry.count,
          );
          const isSelected = selectedCount > 0;
          const sourceLabel =
            entry.source.type === "FaceUpExile"
              ? t("outsideGame.fromExile")
              : t("outsideGame.fromSideboard");
          return (
            <button
              key={key}
              type="button"
              className={`flex items-center justify-between rounded-md border px-3 py-2 text-left text-sm transition ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/20 text-white"
                  : "border-white/15 bg-black/30 text-zinc-100 hover:bg-white/10"
              }`}
              onClick={() => toggleSelect(key, entry.count)}
            >
              <span className="flex flex-col">
                <span>{entry.name}</span>
                <span className="text-[10px] uppercase tracking-wide text-zinc-400">
                  {sourceLabel}
                </span>
              </span>
              <span className="text-xs text-zinc-400">
                {isSelected ? `${selectedCount}/` : ""}x{entry.count}
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
    .map((entry) => `${entryKey(entry)}:${entry.count}`)
    .join(",");
  return `${data.player}:${data.source_id}:${data.count}:${data.up_to ?? false}:${data.destination}:${choicesKey}`;
}

// ── Choose From Zone Modal ───────────────────────────────────────────────────

function ChooseFromZoneModal({ data }: { data: ChooseFromZoneChoice["data"] }) {
  const { t } = useTranslation("game");
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
        canAssignDistinctCardTypes(
          objects,
          selectedIds,
          selectionRule.categories,
        )));
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

  const subtitle =
    selectionRule?.type === "DistinctCardTypes"
      ? t("cardChoice.chooseFromZone.subtitleDistinctCardTypes", {
          count: data.count,
        })
      : data.up_to
        ? t("cardChoice.chooseFromZone.subtitleUpTo", { count: data.count })
        : t("cardChoice.chooseFromZone.subtitleExact", { count: data.count });

  return (
    <ChoiceOverlay
      title={t("cardChoice.chooseFromZone.title")}
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
                    {t("cardChoice.badges.choose")}
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.pair.title")}
      subtitle={t("cardChoice.pair.subtitle")}
      footer={
        <div className="mx-auto w-full max-w-xl">
          <CancelButton
            onClick={() => handleChoose(null)}
            label={t("cardChoice.buttons.decline")}
          />
        </div>
      }
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
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());
  const isTapUntapChoice =
    data.effect_kind === "Untap" || data.effect_kind === "Tap";
  const isSacrifice =
    data.zone === "Battlefield" &&
    data.destination == null &&
    !isTapUntapChoice;
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
  const mode: EffectZoneMode = isTapUntapChoice
    ? data.effect_kind === "Untap"
      ? "Untap"
      : "Tap"
    : isSacrifice
      ? "Sacrifice"
      : isTopdeck
        ? "Topdeck"
        : data.destination === "Hand"
          ? "Hand"
          : "Battlefield";
  const visualClasses = EFFECT_ZONE_VISUAL_CLASSES[mode];
  const selectedOrder = isTopdeck ? Array.from(selected) : [];
  const selectedOrderLabels = selectedOrder.map((_, index) =>
    formatTopdeckOrderLabel(index, t),
  );
  const isReady = isUpTo
    ? selected.size >= minCount && selected.size <= data.count
    : selected.size === data.count;
  const subtitleKey = isUpTo
    ? minCount > 0
      ? `cardChoice.effectZone.subtitle${mode}Range`
      : `cardChoice.effectZone.subtitle${mode}UpTo`
    : `cardChoice.effectZone.subtitle${mode}Exact`;
  const title = t(`cardChoice.effectZone.title${mode}`);
  const subtitle = t(subtitleKey, { min: minCount, count: data.count });
  const actionLabel =
    selected.size === 0 && isUpTo && minCount === 0
      ? isSacrifice
        ? t("cardChoice.effectZone.labelSkip")
        : t("cardChoice.effectZone.labelDecline")
      : isTopdeck && selectedOrderLabels.length > 0
        ? t("cardChoice.effectZone.labelPutOnTop", {
            order: selectedOrderLabels.join(" -> "),
          })
        : t(EFFECT_ZONE_ACTION_LABEL_KEYS[mode], {
            selected: selected.size,
            count: data.count,
          });

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!isReady}
          label={actionLabel}
        />
      }
    >
      <ScrollableCardStrip>
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
          const selectedIndex = selectedOrder.indexOf(id);
          const badgeLabel =
            isTopdeck && selectedIndex >= 0
              ? formatTopdeckOrderLabel(selectedIndex, t)
              : t(EFFECT_ZONE_BADGE_KEYS[mode]);
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? `z-10 ring-2 ${visualClasses.ring}`
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
                <div
                  className={`absolute inset-0 flex items-center justify-center rounded-lg ${visualClasses.overlay}`}
                >
                  <span
                    className={`rounded-full px-3 py-1 text-xs font-bold text-white ${visualClasses.badge}`}
                  >
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

function formatTopdeckOrderLabel(index: number, t: TFunction<"game">): string {
  if (index === 0) return t("cardChoice.effectZone.orderTop");
  const position = index + 1;
  const suffix = position === 2 ? "nd" : position === 3 ? "rd" : "th";
  return `${position}${suffix}`;
}

function DrawnThisTurnTopdeckModal({
  data,
}: {
  data: DrawnThisTurnTopdeckChoice["data"];
}) {
  const { t } = useTranslation("game");
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
    selected.size === 0
      ? t("cardChoice.drawnThisTurn.labelPayLife", {
          life: payments * data.life_payment,
        })
      : t("cardChoice.drawnThisTurn.labelConfirm", {
          selected: selected.size,
          count: data.count,
        });
  const disabled = selected.size < data.min_count || selected.size > data.count;

  return (
    <ChoiceOverlay
      title={t("cardChoice.drawnThisTurn.title")}
      subtitle={t("cardChoice.drawnThisTurn.subtitle", {
        count: data.count,
        life: data.life_payment,
      })}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={disabled}
          label={actionLabel}
        />
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
              <CardImage
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-sky-500/20">
                  <span className="rounded-full bg-sky-500/90 px-3 py-1 text-xs font-bold text-white">
                    {t("cardChoice.badges.top")}
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

function SacrificeModal({ data }: { data: PayCost["data"] }) {
  const { t } = useTranslation("game");
  const isVariable = data.min_count !== data.count;
  const subtitle = isVariable
    ? t("cardChoice.effectZone.subtitleSacrificeUpTo", { count: data.count })
    : t("cardChoice.sacrifice.subtitle", { count: data.count });
  return (
    <PermanentCostModal
      data={data}
      choices={data.choices}
      title={t("cardChoice.sacrifice.title")}
      subtitle={subtitle}
      label={t("cardChoice.badges.sacrifice")}
      selectedClassName="z-10 ring-2 ring-red-400/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-red-500/20"
      badgeClassName="rounded-full bg-red-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

// CR 502.3: a MaxUntapPerType cap (Smoke / Stoic Angel / Damping Field / Winter
// Orb class) left more than `max` eligible tapped permanents, so the active
// player directly chooses the bounded subset (up to `max`) that untaps. The
// complement stays tapped. Answered with `SelectCards { cards }`.
function ChooseUntapSubsetModal({
  data,
}: {
  data: { player: PlayerId; group: ObjectId[]; max: number };
}) {
  const { t } = useTranslation("game");
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
        } else if (next.size < data.max) {
          next.add(id);
        }
        return next;
      });
    },
    [data.max],
  );

  const handleConfirm = useCallback(() => {
    dispatch({ type: "SelectCards", data: { cards: Array.from(selected) } });
  }, [dispatch, selected]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={t("cardChoice.untapSubset.title")}
      subtitle={t("cardChoice.untapSubset.subtitle", { count: data.max })}
      footer={
        // CR 502.3: a max-untap cap ("can't untap more than one <type>") bounds
        // the untap count from above only — choosing zero is legal (the whole
        // group simply stays tapped). Never force an at-least-one selection here.
        <ConfirmButton
          onClick={handleConfirm}
          label={t("cardChoice.buttons.labelCount", {
            label: t("gamePage.untap.untap"),
            selected: selected.size,
            count: data.max,
          })}
        />
      }
    >
      <ScrollableCardStrip>
        {data.group.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected.has(id);
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
                    {t("gamePage.untap.untap")}
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

function SacrificeForManaAbilityModal({ data }: { data: PayCost["data"] }) {
  const { t } = useTranslation("game");
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
      title={t("cardChoice.sacrifice.title")}
      subtitle={t("cardChoice.sacrifice.subtitle", { count: data.count })}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!isReady}
          label={t("cardChoice.buttons.sacrificeCount", {
            selected: selected.size,
            count: data.count,
          })}
        />
      }
    >
      <ScrollableCardStrip>
        {data.choices.map((id, index) => {
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
                    {t("cardChoice.badges.sacrifice")}
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

// ── Exile For Mana Ability Modal ──────────────────────────────────────────────

function ExileForManaAbilityModal({
  data,
  zone,
}: {
  data: PayCost["data"];
  zone: Zone;
}) {
  const { t } = useTranslation("game");
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
  const sourceLabel = t(`cardChoice.exileForManaAbility.sources.${zone}`);

  return (
    <ChoiceOverlay
      title={t("cardChoice.exileForManaAbility.title")}
      subtitle={t("cardChoice.exileForManaAbility.subtitle", {
        count: data.count,
        source: sourceLabel,
      })}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!isReady}
          label={t("cardChoice.buttons.exileCount", {
            selected: selected.size,
            count: data.count,
          })}
        />
      }
    >
      <ScrollableCardStrip>
        {data.choices.map((id, index) => {
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
              <CardImage
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-amber-500/20">
                  <span className="rounded-full bg-amber-500/90 px-3 py-1 text-xs font-bold text-white">
                    {t("cardChoice.badges.exile")}
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

// ── Multi-Target Selection Modal ──────────────────────────────────────────────

function MultiTargetSelectionModal({
  data,
}: {
  data: MultiTargetSelection["data"];
}) {
  const { t } = useTranslation("game");
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

  const isReady =
    selected.size >= data.min_targets && selected.size <= data.max_targets;
  const subtitle =
    data.min_targets === data.max_targets
      ? t("cardChoice.multiTarget.subtitleExact", { count: data.max_targets })
      : t("cardChoice.multiTarget.subtitleRange", {
          min: data.min_targets,
          max: data.max_targets,
        });

  return (
    <ChoiceOverlay
      title={t("cardChoice.multiTarget.title")}
      subtitle={subtitle}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!isReady}
          label={t("cardChoice.buttons.confirmCount", {
            selected: selected.size,
            count: data.max_targets,
          })}
        />
      }
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
              <CardImage
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              {isSelected && (
                <div className="absolute inset-0 flex items-center justify-center rounded-lg bg-cyan-500/20">
                  <span className="rounded-full bg-cyan-500/90 px-3 py-1 text-xs font-bold text-white">
                    {t("cardChoice.badges.target")}
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

// ── Paradigm Cast Offer Modal ─────────────────────────────────────────────────

function ParadigmCastOfferModal({ offers }: { offers: ObjectId[] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const handleSelect = useCallback(
    (id: ObjectId) =>
      dispatch({ type: "CastParadigmCopy", data: { source: id } }),
    [dispatch],
  );
  const handlePass = useCallback(
    () => dispatch({ type: "PassParadigmOffer" }),
    [dispatch],
  );

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={t("cardChoice.paradigm.title")}
      subtitle={t("cardChoice.paradigm.subtitle")}
      footer={
        <div className="mx-auto flex w-full max-w-xl gap-2">
          <div className="flex-1">
            <CancelButton
              onClick={handlePass}
              label={t("cardChoice.buttons.pass")}
            />
          </div>
        </div>
      }
    >
      <ScrollableCardStrip>
        {offers.map((id, index) => {
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

// ── Pay Mana Ability Mana Modal ───────────────────────────────────────────────

function ReturnToHandModal({ data }: { data: PayCost["data"] }) {
  const { t } = useTranslation("game");
  return (
    <PermanentCostModal
      data={data}
      choices={data.choices}
      title={t("cardChoice.returnToHand.title")}
      subtitle={t("cardChoice.returnToHand.subtitle", { count: data.count })}
      label={t("cardChoice.badges.return")}
      selectedClassName="z-10 ring-2 ring-sky-300/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-sky-500/20"
      badgeClassName="rounded-full bg-sky-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

function RemoveCounterModal({ data }: { data: PayCost["data"] }) {
  const { t } = useTranslation("game");
  const isAmongObjects =
    data.kind.type === "RemoveCounter" &&
    data.kind.selection === "AmongObjects";
  if (isAmongObjects) {
    return <RemoveCounterDistributionCostModal data={data} />;
  }
  return (
    <PermanentCostModal
      data={data}
      choices={data.choices}
      title={t("cardChoice.removeCounter.title")}
      subtitle={t("cardChoice.removeCounter.subtitle")}
      label={t("cardChoice.removeCounter.label")}
      maxSelections={isAmongObjects ? data.count : 1}
      minSelections={isAmongObjects ? data.min_count : 1}
      labelCount={isAmongObjects ? data.count : 1}
      selectedClassName="z-10 ring-2 ring-violet-300/80"
      overlayClassName="absolute inset-0 flex items-center justify-center rounded-lg bg-violet-500/20"
      badgeClassName="rounded-full bg-violet-500/90 px-3 py-1 text-xs font-bold text-white"
    />
  );
}

function removableCounterCostEntries(
  obj: GameObject,
  counterType: CounterMatch,
): [CounterType, number][] {
  if (counterType.type === "OfType") {
    const count = obj.counters[counterType.data] ?? 0;
    return count > 0 ? [[counterType.data, count]] : [];
  }
  return Object.entries(obj.counters).filter(
    (entry): entry is [CounterType, number] =>
      typeof entry[1] === "number" && entry[1] > 0,
  );
}

function RemoveCounterDistributionCostModal({
  data,
}: {
  data: PayCost["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [amounts, setAmounts] = useState<Record<string, number>>({});

  useEffect(() => {
    setAmounts({});
  }, [data]);

  if (data.kind.type !== "RemoveCounter") return null;
  if (!objects) return null;

  const counterKind = data.kind;
  const requiredCount = data.kind.count;
  const assigned = Object.values(amounts).reduce(
    (sum, amount) => sum + amount,
    0,
  );
  const remaining = requiredCount - assigned;
  const isReady = assigned === requiredCount;

  const amountKey = (id: ObjectId, counterType: CounterType) =>
    `${id}:${counterType}`;

  const setAmount = (
    id: ObjectId,
    counterType: CounterType,
    value: number,
    max: number,
  ) => {
    setAmounts((prev) => ({
      ...prev,
      [amountKey(id, counterType)]: Math.max(0, Math.min(max, value)),
    }));
  };

  const handleConfirm = () => {
    if (!isReady) return;
    const distribution = data.choices
      .flatMap((id) => {
        const obj = objects[id];
        if (!obj) return [];
        return removableCounterCostEntries(obj, counterKind.counter_type).map(
          ([counterType]) => ({
            object_id: id,
            counter_type: counterType,
            count: amounts[amountKey(id, counterType)] ?? 0,
          }),
        );
      })
      .filter((choice) => choice.count > 0);
    dispatch({
      type: "ChooseRemoveCounterCostDistribution",
      data: { distribution },
    });
  };

  return (
    <ChoiceOverlay
      title={t("cardChoice.removeCounter.title")}
      subtitle={t("cardChoice.removeCounter.subtitle")}
      footer={
        <CostActionFooter onCancel={() => dispatch({ type: "CancelCast" })}>
          <ConfirmButton
            onClick={handleConfirm}
            disabled={!isReady}
            label={t("cardChoice.buttons.labelCount", {
              label: t("cardChoice.removeCounter.label"),
              selected: assigned,
              count: requiredCount,
            })}
          />
        </CostActionFooter>
      }
    >
      <ScrollableCardStrip>
        {data.choices.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const entries = removableCounterCostEntries(
            obj,
            counterKind.counter_type,
          );
          const selectedAmount = entries.reduce(
            (sum, [counterType]) =>
              sum + (amounts[amountKey(id, counterType)] ?? 0),
            0,
          );
          return (
            <motion.div
              key={id}
              className="relative shrink-0 rounded-lg transition hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{
                opacity: selectedAmount > 0 ? 1 : 0.7,
                y: 0,
                scale: 1,
              }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              {...hoverProps(id)}
            >
              <CardImage
                {...objectImageProps(obj)}
                size="normal"
                className={CHOICE_CARD_IMAGE_CLASS}
              />
              <div className="absolute inset-x-2 bottom-2 flex flex-col gap-1 rounded-lg bg-gray-950/85 px-2 py-1">
                {entries.map(([counterType, maxAmount]) => {
                  const key = amountKey(id, counterType);
                  const amount = amounts[key] ?? 0;
                  return (
                    <div
                      key={key}
                      className="flex items-center justify-center gap-2"
                    >
                      {counterKind.counter_type.type === "Any" && (
                        <span className="w-14 truncate text-xs font-semibold text-gray-200">
                          {formatCounterType(counterType)}
                        </span>
                      )}
                      <button
                        type="button"
                        className="h-7 w-7 rounded-full bg-gray-700 text-sm font-bold text-white disabled:opacity-40"
                        onClick={() =>
                          setAmount(id, counterType, amount - 1, maxAmount)
                        }
                        disabled={amount <= 0}
                      >
                        -
                      </button>
                      <span className="w-8 text-center text-sm font-bold text-white">
                        {amount}
                      </span>
                      <button
                        type="button"
                        className="h-7 w-7 rounded-full bg-gray-700 text-sm font-bold text-white disabled:opacity-40"
                        onClick={() =>
                          setAmount(id, counterType, amount + 1, maxAmount)
                        }
                        disabled={remaining <= 0 || amount >= maxAmount}
                      >
                        +
                      </button>
                    </div>
                  );
                })}
              </div>
            </motion.div>
          );
        })}
      </ScrollableCardStrip>
    </ChoiceOverlay>
  );
}

function PermanentCostModal({
  data,
  choices,
  title,
  subtitle,
  label,
  minSelections,
  maxSelections,
  labelCount,
  selectedClassName,
  overlayClassName,
  badgeClassName,
}: {
  data: PayCost["data"];
  choices: ObjectId[];
  title: string;
  subtitle: string;
  label: string;
  minSelections?: number;
  maxSelections?: number;
  labelCount?: number;
  selectedClassName: string;
  overlayClassName: string;
  badgeClassName: string;
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [selected, setSelected] = useState<Set<ObjectId>>(new Set());
  const minCount = minSelections ?? data.count;
  const maxCount = maxSelections ?? data.count;
  const confirmCount = labelCount ?? data.count;

  const toggleSelect = useCallback(
    (id: ObjectId) => {
      setSelected((prev) => {
        const next = new Set(prev);
        if (next.has(id)) {
          next.delete(id);
        } else if (next.size < maxCount) {
          next.add(id);
        }
        return next;
      });
    },
    [maxCount],
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

  const isReady = selected.size >= minCount && selected.size <= maxCount;

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton
            onClick={handleConfirm}
            disabled={!isReady}
            label={t("cardChoice.buttons.labelCount", {
              label,
              selected: selected.size,
              count: confirmCount,
            })}
          />
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.blight.title")}
      subtitle={t("cardChoice.blight.subtitle", { count: data.count })}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton
            onClick={handleConfirm}
            disabled={!isReady}
            label={t("cardChoice.buttons.confirmCount", {
              selected: selected.size,
              count: data.count,
            })}
          />
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.crew.title")}
      subtitle={t("cardChoice.crew.subtitle", { power: data.crew_power })}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!isReady}
          label={t("cardChoice.crew.label", {
            total: totalPower,
            power: data.crew_power,
          })}
        />
      }
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
                    {t("cardChoice.badges.crew", { power: obj.power ?? 0 })}
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
  const { t } = useTranslation("game");
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

  const selectedPower =
    selected != null ? Math.max(objects[selected]?.power ?? 0, 0) : 0;

  return (
    <ChoiceOverlay
      title={t("cardChoice.station.title")}
      subtitle={t("cardChoice.station.subtitle")}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected == null}
          label={
            selected != null
              ? t("cardChoice.station.labelWithCharge", {
                  charge: selectedPower,
                })
              : t("cardChoice.station.label")
          }
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
                    {t("cardChoice.badges.station", {
                      power: Math.max(obj.power ?? 0, 0),
                    })}
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.saddle.title")}
      subtitle={t("cardChoice.saddle.subtitle", { power: data.saddle_power })}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={!isReady}
          label={t("cardChoice.saddle.label", {
            total: totalPower,
            power: data.saddle_power,
          })}
        />
      }
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
                    {t("cardChoice.badges.saddle", { power: obj.power ?? 0 })}
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.wardSacrifice.title", { count: data.remaining })}
      subtitle={t("cardChoice.wardSacrifice.subtitle")}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected == null}
          label={t("cardChoice.badges.sacrifice")}
        />
      }
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
                    {t("cardChoice.badges.sacrifice")}
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.unlessBounce.title", { count: data.remaining })}
      subtitle={t("cardChoice.unlessBounce.subtitle")}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected == null}
          label={t("cardChoice.badges.return")}
        />
      }
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
                    {t("cardChoice.badges.return")}
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
  minCount = count,
  title,
  subtitle,
  confirmLabel = "Exile",
}: {
  cards: ObjectId[];
  count: number;
  minCount?: number;
  title: string;
  subtitle: string;
  confirmLabel?: string;
}) {
  const { t } = useTranslation("game");
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

  const isReady = selected.size >= minCount && selected.size <= count;

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton
            onClick={handleConfirm}
            disabled={!isReady}
            label={t("cardChoice.buttons.labelCount", {
              label: confirmLabel,
              selected: selected.size,
              count,
            })}
          />
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

function ExileForCostDispatch({
  data,
  zone,
}: {
  data: PayCost["data"];
  zone: ExileCostSourceZone;
}) {
  const { t } = useTranslation("game");
  let title: string;
  let sourceLabel: string;
  switch (zone) {
    case "Hand":
      title = t("cardChoice.exileForCost.titleAlternative");
      sourceLabel = t("cardChoice.exileForCost.sourceHand");
      break;
    case "Graveyard":
      title = t("cardChoice.exileForCost.titleEscape");
      sourceLabel = t("cardChoice.exileForCost.sourceGraveyard");
      break;
  }
  return (
    <ExileForCostModal
      cards={data.choices}
      count={data.count}
      title={title}
      subtitle={t("cardChoice.exileForCost.subtitle", {
        count: data.count,
        source: sourceLabel,
      })}
      confirmLabel={t("cardChoice.badges.exile")}
    />
  );
}

function BeholdModal({
  data,
  action,
}: {
  data: PayCost["data"];
  action: "ChooseOrReveal" | "ExileChosen";
}) {
  const { t } = useTranslation("game");
  const exilesChosen = action === "ExileChosen";
  return (
    <ExileForCostModal
      cards={data.choices}
      count={data.count}
      title={t("cardChoice.behold.title")}
      subtitle={
        exilesChosen
          ? t("cardChoice.behold.subtitleExile")
          : t("cardChoice.behold.subtitleChoose")
      }
      confirmLabel={
        exilesChosen
          ? t("cardChoice.behold.labelExile")
          : t("cardChoice.behold.labelBehold")
      }
    />
  );
}

// CR 118.3 + CR 601.2b + CR 605.3b: single dispatch for the unified `PayCost`
// state — branch on `kind.type` to the matching cost-selection modal. The
// `key` forces a fresh selection set when the eligible-object list changes.
function PayCostDispatch({ data }: { data: PayCost["data"] }) {
  const { t } = useTranslation("game");
  const isManaAbility = data.resume.type === "ManaAbility";
  const choicesKey = data.choices.join(",");
  switch (data.kind.type) {
    case "Discard":
      return (
        <DiscardModal
          data={{ ...data, cards: data.choices }}
          title={
            isManaAbility
              ? t("cardChoice.discard.titleManaAbility")
              : t("cardChoice.discard.titleAdditionalCost")
          }
          canCancel={!isManaAbility}
        />
      );
    case "Sacrifice":
      return isManaAbility ? (
        <SacrificeForManaAbilityModal data={data} />
      ) : (
        <SacrificeModal key={choicesKey} data={data} />
      );
    case "ReturnToHand":
      return <ReturnToHandModal key={choicesKey} data={data} />;
    case "RemoveCounter":
      return <RemoveCounterModal key={choicesKey} data={data} />;
    case "TapCreatures":
      // Tap-creature costs are resolved by battlefield clicks + TargetingOverlay,
      // not a modal (mirrors the pre-collapse behavior).
      return null;
    case "Behold":
      return <BeholdModal data={data} action={data.kind.action} />;
    case "ExileFromZone":
      return <ExileForCostDispatch data={data} zone={data.kind.zone} />;
    case "ExileMaterials":
      return <CraftMaterialsModal data={data} />;
    case "ExilePermanent":
      return <ExilePermanentForCostModal data={data} />;
    case "ExileFromManaZone":
      return <ExileForManaAbilityModal data={data} zone={data.kind.zone} />;
  }
}

// CR 601.2h + CR 701.13: Exile a battlefield permanent you control as an
// additional/alternative casting cost (Food Chain class; Lunar Hatchling's
// escape "Exile a land you control"). Reuses the generic `ExileForCostModal`
// primitive (same as Craft / Behold / ExileFromZone). The cost is a mandatory
// fixed-count exile (`min_count == count`); the engine supplies the eligible
// battlefield permanents in `choices`.
function ExilePermanentForCostModal({ data }: { data: PayCost["data"] }) {
  const { t } = useTranslation("game");
  const exact = data.min_count === data.count;
  return (
    <ExileForCostModal
      cards={data.choices}
      count={data.count}
      minCount={data.min_count}
      title={t("cardChoice.exilePermanent.title")}
      subtitle={
        exact
          ? t("cardChoice.exilePermanent.subtitle", { count: data.count })
          : t("cardChoice.exilePermanent.subtitleRange", {
              min: data.min_count,
              count: data.count,
            })
      }
      confirmLabel={t("cardChoice.badges.exile")}
    />
  );
}

// CR 702.167a/b: Craft materials exile. Reuses the generic `ExileForCostModal`
// primitive (same as Behold / ExileFromZone). Exact costs surface
// `min_count == count`; "one or more" costs surface a lower `min_count` and use
// every engine-supplied eligible object as the maximum.
function CraftMaterialsModal({ data }: { data: PayCost["data"] }) {
  const { t } = useTranslation("game");
  const exact = data.min_count === data.count;
  return (
    <ExileForCostModal
      cards={data.choices}
      count={data.count}
      minCount={data.min_count}
      title={t("cardChoice.craft.title")}
      subtitle={
        exact
          ? t("cardChoice.craft.subtitle", { count: data.count })
          : t("cardChoice.craft.subtitleRange", {
              min: data.min_count,
              count: data.count,
            })
      }
      confirmLabel={t("cardChoice.badges.exile")}
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
      return (
        cost.generic +
        cost.shards.reduce((sum, shard) => sum + manaValueOfShard(shard), 0)
      );
  }
}

function manaValueOfObject(obj: { mana_cost: ManaCost }): number {
  return manaValueOfCost(obj.mana_cost);
}

function CollectEvidenceModal({
  data,
}: {
  data: CollectEvidenceChoice["data"];
}) {
  const { t } = useTranslation("game");
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
      title={t("cardChoice.collectEvidence.title")}
      subtitle={t("cardChoice.collectEvidence.subtitle", {
        minimum: data.minimum_mana_value,
      })}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton
            onClick={handleConfirm}
            disabled={!isReady}
            label={t("cardChoice.buttons.collectCount", {
              total,
              minimum: data.minimum_mana_value,
            })}
          />
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
                    {t("cardChoice.badges.evidence")}
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
  title,
  canCancel = false,
}: {
  data: { cards: ObjectId[]; count: number } & {
    up_to?: boolean;
    unless_filter?: TargetFilter;
  };
  title?: string;
  canCancel?: boolean;
}) {
  const { t } = useTranslation("game");
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
    ? t("cardChoice.discard.subtitleUpTo", { count: data.count })
    : hasUnlessOption
      ? t("cardChoice.discard.subtitleUnless", { count: data.count })
      : t("cardChoice.discard.subtitleExact", { count: data.count });

  return (
    <ChoiceOverlay
      title={title ?? t("cardChoice.discard.title")}
      subtitle={subtitle}
      footer={
        canCancel ? (
          <CostActionFooter onCancel={handleCancel}>
            <ConfirmButton
              onClick={handleConfirm}
              disabled={!isReady}
              label={t("cardChoice.buttons.discardCount", {
                selected: selected.size,
                count: data.count,
              })}
            />
          </CostActionFooter>
        ) : (
          <ConfirmButton
            onClick={handleConfirm}
            disabled={!isReady}
            label={t("cardChoice.buttons.discardCount", {
              selected: selected.size,
              count: data.count,
            })}
          />
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
                    {t("cardChoice.badges.discard")}
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.harmonize.title")}
      subtitle={t("cardChoice.harmonize.subtitle")}
      footer={
        <CostActionFooter onCancel={handleCancel}>
          <ConfirmButton
            onClick={handleSkip}
            label={t("cardChoice.harmonize.labelSkip")}
          />
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
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const gameState = useGameStore((s) => s.gameState);
  const objects = gameState?.objects;
  const turnNumber = gameState?.turn_number;
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={t("cardChoice.legend.title")}
      subtitle={t("cardChoice.legend.subtitle", { name: data.legend_name })}
    >
      <ScrollableCardStrip>
        {data.candidates.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isCurrentTurnEntry =
            turnNumber != null && obj.entered_battlefield_turn === turnNumber;
          const entryLabel = isCurrentTurnEntry
            ? t("cardChoice.legend.statusJustEntered")
            : t("cardChoice.legend.statusAlready");
          return (
            <motion.button
              key={id}
              aria-label={t("cardChoice.legend.keepAria", {
                name: obj.name,
                status: entryLabel,
              })}
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
                    isCurrentTurnEntry ? "bg-amber-500/95" : "bg-sky-700/95"
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

function CommanderZoneChoiceModal({
  data,
}: {
  data: CommanderZoneChoice["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  const obj = objects[data.commander_id];
  const zoneName =
    data.current_zone.charAt(0).toUpperCase() + data.current_zone.slice(1);

  return (
    <ChoiceOverlay
      title={t("cardChoice.commanderZone.title")}
      subtitle={t("cardChoice.commanderZone.subtitle", {
        name: obj?.name ?? t("cardChoice.commanderZone.commanderFallback"),
        zone: zoneName,
      })}
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
            label={t("cardChoice.commanderZone.labelCommandZone")}
            onClick={() =>
              dispatch({ type: "DecideOptionalEffect", data: { accept: true } })
            }
          />
          <ConfirmButton
            label={t("cardChoice.commanderZone.labelLeave", { zone: zoneName })}
            onClick={() =>
              dispatch({
                type: "DecideOptionalEffect",
                data: { accept: false },
              })
            }
          />
        </div>
      </div>
    </ChoiceOverlay>
  );
}

// ── Reveal Until Kept Choice Modal (CR 701.20a) ───────────────────────────

function RevealUntilKeptChoiceModal({
  data,
}: {
  data: RevealUntilKeptChoice["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  const obj = objects[data.hit_card];
  const declineZone =
    data.decline_zone.charAt(0).toUpperCase() + data.decline_zone.slice(1);

  return (
    <ChoiceOverlay
      title={t("cardChoice.revealUntil.title")}
      subtitle={t("cardChoice.revealUntil.subtitle", {
        name: obj?.name ?? t("cardChoice.revealUntil.cardFallback"),
      })}
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
            label={t("cardChoice.revealUntil.labelBattlefield")}
            onClick={() =>
              dispatch({ type: "DecideOptionalEffect", data: { accept: true } })
            }
          />
          <ConfirmButton
            label={t("cardChoice.revealUntil.labelInto", { zone: declineZone })}
            onClick={() =>
              dispatch({
                type: "DecideOptionalEffect",
                data: { accept: false },
              })
            }
          />
        </div>
      </div>
    </ChoiceOverlay>
  );
}

// ── Repeat Decision Modal ──────────────────────────────────────────────────

function RepeatDecisionModal({
  data: _data,
}: {
  data: RepeatDecision["data"];
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();

  return (
    <ChoiceOverlay
      title={t("cardChoice.repeatProcess.title")}
      subtitle={t("cardChoice.repeatProcess.subtitle")}
    >
      <div className="flex flex-col gap-3">
        <ConfirmButton
          label={t("cardChoice.buttons.repeat")}
          onClick={() =>
            dispatch({ type: "DecideOptionalEffect", data: { accept: true } })
          }
        />
        <ConfirmButton
          label={t("cardChoice.buttons.stop")}
          onClick={() =>
            dispatch({ type: "DecideOptionalEffect", data: { accept: false } })
          }
        />
      </div>
    </ChoiceOverlay>
  );
}

// ── Damage Source Choice Modal ─────────────────────────────────────────────

function DamageSourceModal({ data }: { data: DamageSourceChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={t("cardChoice.damageSource.title")}
      subtitle={t("cardChoice.damageSource.subtitle")}
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.manifestDread.title")}
      subtitle={t("cardChoice.manifestDread.subtitle")}
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected === null}
          label={t("cardChoice.manifestDread.label")}
        />
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
                    {t("cardChoice.badges.manifest")}
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
  White:
    "border-yellow-400 bg-yellow-400/20 text-yellow-200 hover:bg-yellow-400/40",
  Blue: "border-blue-400 bg-blue-500/20 text-blue-200 hover:bg-blue-500/40",
  Black: "border-gray-400 bg-gray-700/40 text-gray-200 hover:bg-gray-700/60",
  Red: "border-red-400 bg-red-500/20 text-red-200 hover:bg-red-500/40",
  Green:
    "border-green-400 bg-green-600/20 text-green-200 hover:bg-green-600/40",
  Colorless:
    "border-gray-400 bg-gray-500/20 text-gray-200 hover:bg-gray-500/40",
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
  // CR 605.3a: When the source is a mana ability with identical, choice-free
  // twins (the player's other Treasures, etc.), the engine reports them in
  // `context.batch_siblings`. Offer a quantity stepper so one color choice can
  // bulk-activate up to `siblings + 1` sources. `+ 1` counts the just-tapped
  // source already paid for before this prompt.
  const batchMax =
    data.context.type === "ManaAbility"
      ? (data.context.data.batch_siblings?.length ?? 0) + 1
      : 1;
  return (
    <ManaSingleColorChoiceModal
      options={data.choice.data.options}
      batchMax={batchMax}
    />
  );
}

function PayManaAbilityManaModal({
  data,
}: {
  data: PayManaAbilityMana["data"];
}) {
  const { t } = useTranslation("game");
  return (
    <ManaCombinationChoiceModal
      options={data.options}
      title={t("cardChoice.payManaAbility.title")}
      subtitle={t("cardChoice.payManaAbility.subtitle")}
      actionType="PayManaAbilityMana"
    />
  );
}

function ManaSingleColorChoiceModal({
  options,
  batchMax = 1,
}: {
  options: ManaType[];
  batchMax?: number;
}) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<ManaType | null>(null);
  // CR 605.3a: how many identical sources to activate with the chosen color.
  const [count, setCount] = useState(1);

  const handleConfirm = useCallback(() => {
    if (selected) {
      dispatch({
        type: "ChooseManaColor",
        data: { choice: { type: "SingleColor", data: selected }, count },
      });
    }
  }, [dispatch, selected, count]);

  const canBatch = batchMax > 1;
  const confirmLabel =
    selected && count > 1
      ? t("cardChoice.manaColor.labelAdd", { count })
      : t("cardChoice.manaColor.labelConfirm");

  return (
    <ChoiceOverlay
      title={t("cardChoice.manaColor.title")}
      subtitle={
        canBatch
          ? t("cardChoice.manaColor.subtitleBatch")
          : t("cardChoice.manaColor.subtitle")
      }
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-md"
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selected === null}
          label={confirmLabel}
        />
      }
    >
      <div className="mx-auto flex w-fit items-center justify-center gap-3 px-4 py-4 sm:gap-5 sm:px-6 sm:py-6">
        {options.map((color, index) => {
          const isSelected = selected === color;
          return (
            <motion.button
              key={color}
              className={`flex h-14 w-14 items-center justify-center rounded-full border-2 transition sm:h-[4.5rem] sm:w-[4.5rem] ${
                isSelected
                  ? MANA_COLOR_SELECTED[color]
                  : MANA_COLOR_STYLES[color]
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
      {canBatch && (
        <div className="mx-auto mb-4 flex w-fit items-center gap-4">
          <span className="text-sm text-white/70">
            {t("cardChoice.manaColor.howMany")}
          </span>
          <div className="flex items-center gap-3">
            <button
              type="button"
              aria-label={t("cardChoice.manaColor.tapFewer")}
              disabled={count <= 1}
              onClick={() => setCount((c) => Math.max(1, c - 1))}
              className="flex h-9 w-9 items-center justify-center rounded-full border border-white/20 text-xl leading-none text-white transition hover:border-white/40 disabled:opacity-30"
            >
              −
            </button>
            <span className="w-8 text-center text-lg font-semibold tabular-nums text-white">
              {count}
            </span>
            <button
              type="button"
              aria-label={t("cardChoice.manaColor.tapMore")}
              disabled={count >= batchMax}
              onClick={() => setCount((c) => Math.min(batchMax, c + 1))}
              className="flex h-9 w-9 items-center justify-center rounded-full border border-white/20 text-xl leading-none text-white transition hover:border-white/40 disabled:opacity-30"
            >
              +
            </button>
            <span className="text-sm text-white/50">/ {batchMax}</span>
          </div>
        </div>
      )}
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
  const { t } = useTranslation("game");
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
      title={t("cardChoice.manaCombination.title")}
      subtitle={t("cardChoice.manaCombination.subtitleAny")}
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
                    isSelected
                      ? MANA_COLOR_SELECTED[color]
                      : MANA_COLOR_STYLES[color]
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
  title,
  subtitle,
  actionType = "ChooseManaColor",
}: {
  options: ManaType[][];
  title?: string;
  subtitle?: string;
  actionType?: "ChooseManaColor" | "PayManaAbilityMana";
}) {
  const { t } = useTranslation("game");
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
      title={title ?? t("cardChoice.manaCombination.title")}
      subtitle={subtitle ?? t("cardChoice.manaCombination.subtitle")}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-lg"
      footer={
        <ConfirmButton
          onClick={handleConfirm}
          disabled={selectedIndex === null}
        />
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
