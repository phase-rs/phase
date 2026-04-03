import { useCallback, useState } from "react";
import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ManaCost, ObjectId, TargetFilter, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { ChoiceOverlay, ConfirmButton, ScrollableCardStrip } from "./ChoiceOverlay.tsx";
import { NamedChoiceModal } from "./NamedChoiceModal.tsx";
import { DungeonChoiceModal, RoomChoiceModal } from "./DungeonChoiceModal.tsx";
import { DamageAssignmentModal } from "../combat/DamageAssignmentModal.tsx";
import { DistributeAmongModal } from "./DistributeAmongModal.tsx";
import { RetargetChoiceModal } from "./RetargetChoiceModal.tsx";

type ScryChoice = Extract<WaitingFor, { type: "ScryChoice" }>;
type DigChoice = Extract<WaitingFor, { type: "DigChoice" }>;
type SurveilChoice = Extract<WaitingFor, { type: "SurveilChoice" }>;
type RevealChoice = Extract<WaitingFor, { type: "RevealChoice" }>;
type SearchChoice = Extract<WaitingFor, { type: "SearchChoice" }>;
type ChooseFromZoneChoice = Extract<WaitingFor, { type: "ChooseFromZoneChoice" }>;
type EffectZoneChoice = Extract<WaitingFor, { type: "EffectZoneChoice" }>;
type DiscardToHandSize = Extract<WaitingFor, { type: "DiscardToHandSize" }>;
type SacrificeForCost = Extract<WaitingFor, { type: "SacrificeForCost" }>;
type ExileFromGraveyardForCost = Extract<WaitingFor, { type: "ExileFromGraveyardForCost" }>;
type CollectEvidenceChoice = Extract<WaitingFor, { type: "CollectEvidenceChoice" }>;
type HarmonizeTapChoice = Extract<WaitingFor, { type: "HarmonizeTapChoice" }>;
type ChooseLegend = Extract<WaitingFor, { type: "ChooseLegend" }>;
type ManifestDreadChoice = Extract<WaitingFor, { type: "ManifestDreadChoice" }>;
type CrewVehicle = Extract<WaitingFor, { type: "CrewVehicle" }>;
const CHOICE_CARD_IMAGE_CLASS = "";
const SCRY_CARD_IMAGE_CLASS = "";

/**
 * Generic card choice modal for Scry, Dig, Surveil, Reveal, Search, and NamedChoice.
 * Renders based on the WaitingFor type.
 */
export function CardChoiceModal() {
  const playerId = usePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);

  if (!waitingFor) return null;

  switch (waitingFor.type) {
    case "ScryChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <ScryModal data={waitingFor.data} />;
    case "DigChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <DigModal data={waitingFor.data} />;
    case "SurveilChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <SurveilModal data={waitingFor.data} />;
    case "RevealChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <RevealModal data={waitingFor.data} />;
    case "SearchChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <SearchModal data={waitingFor.data} />;
    case "ChooseFromZoneChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <ChooseFromZoneModal data={waitingFor.data} />;
    case "EffectZoneChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <EffectZoneModal data={waitingFor.data} />;
    case "NamedChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <NamedChoiceModal data={waitingFor.data} />;
    case "DiscardToHandSize":
      if (waitingFor.data.player !== playerId) return null;
      return <DiscardModal data={waitingFor.data} />;
    case "DiscardForCost":
      if (waitingFor.data.player !== playerId) return null;
      return <DiscardModal data={waitingFor.data} title="Discard as additional cost" />;
    case "SacrificeForCost":
      if (waitingFor.data.player !== playerId) return null;
      return <SacrificeModal data={waitingFor.data} />;
    case "CrewVehicle":
      if (waitingFor.data.player !== playerId) return null;
      return <CrewModal data={waitingFor.data} />;
    case "ExileFromGraveyardForCost":
      if (waitingFor.data.player !== playerId) return null;
      return <ExileFromGraveyardModal data={waitingFor.data} />;
    case "CollectEvidenceChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <CollectEvidenceModal data={waitingFor.data} />;
    case "HarmonizeTapChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <HarmonizeTapModal data={waitingFor.data} />;
    case "ChooseLegend":
      if (waitingFor.data.player !== playerId) return null;
      return <LegendChoiceModal data={waitingFor.data} />;
    case "ConniveDiscard":
      if (waitingFor.data.player !== playerId) return null;
      return <DiscardModal data={waitingFor.data} title={`Connive \u2014 Discard ${waitingFor.data.count === 1 ? "a card" : `${waitingFor.data.count} cards`}`} />;
    case "DiscardChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <DiscardModal data={waitingFor.data} title={waitingFor.data.up_to ? `Discard up to ${waitingFor.data.count} cards` : `Discard ${waitingFor.data.count === 1 ? "a card" : `${waitingFor.data.count} cards`}`} />;
    case "WardDiscardChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <DiscardModal data={{ ...waitingFor.data, count: 1 }} title="Ward \u2014 Discard a card" />;
    case "WardSacrificeChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <WardSacrificeModal data={waitingFor.data} />;
    case "AssignCombatDamage":
      if (waitingFor.data.player !== playerId) return null;
      return <DamageAssignmentModal data={waitingFor.data} />;
    case "DistributeAmong":
      if (waitingFor.data.player !== playerId) return null;
      return <DistributeAmongModal data={waitingFor.data} />;
    case "RetargetChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <RetargetChoiceModal data={waitingFor.data} />;
    case "ManifestDreadChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <ManifestDreadModal data={waitingFor.data} />;
    case "ChooseDungeon":
      if (waitingFor.data.player !== playerId) return null;
      return <DungeonChoiceModal data={waitingFor.data} />;
    case "ChooseDungeonRoom":
      if (waitingFor.data.player !== playerId) return null;
      return <RoomChoiceModal data={waitingFor.data} />;
    default:
      return null;
  }
}

// ── Scry Modal ──────────────────────────────────────────────────────────────

function ScryModal({ data }: { data: ScryChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const inspectObject = useUiStore((s) => s.inspectObject);
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
                onMouseEnter={() => inspectObject(id)}
                onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
                onMouseEnter={() => inspectObject(id)}
                onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
  const [selected, setSelected] = useState<ObjectId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({
        type: "SelectCards",
        data: { cards: [selected] },
      });
    }
  }, [dispatch, selected]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Opponent's Hand"
      subtitle="Choose a card"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
      title="Choose Cards"
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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

// ── Crew Vehicle Modal ──────────────────────────────────────────────────────

function CrewModal({ data }: { data: CrewVehicle["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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

// ── Ward Sacrifice Modal ─────────────────────────────────────────────────────

type WardSacrificeChoice = Extract<WaitingFor, { type: "WardSacrificeChoice" }>;

function WardSacrificeModal({ data }: { data: WardSacrificeChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);

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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);

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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
  const inspectObject = useUiStore((s) => s.inspectObject);
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
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
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
