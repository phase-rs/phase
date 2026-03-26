import { useCallback, useState } from "react";
import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ObjectId, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { ChoiceOverlay, ConfirmButton, ScrollableCardStrip } from "./ChoiceOverlay.tsx";
import { NamedChoiceModal } from "./NamedChoiceModal.tsx";
import { DamageAssignmentModal } from "../combat/DamageAssignmentModal.tsx";
import { DistributeAmongModal } from "./DistributeAmongModal.tsx";
import { RetargetChoiceModal } from "./RetargetChoiceModal.tsx";

type ScryChoice = Extract<WaitingFor, { type: "ScryChoice" }>;
type DigChoice = Extract<WaitingFor, { type: "DigChoice" }>;
type SurveilChoice = Extract<WaitingFor, { type: "SurveilChoice" }>;
type RevealChoice = Extract<WaitingFor, { type: "RevealChoice" }>;
type SearchChoice = Extract<WaitingFor, { type: "SearchChoice" }>;
type ChooseFromZoneChoice = Extract<WaitingFor, { type: "ChooseFromZoneChoice" }>;
type DiscardToHandSize = Extract<WaitingFor, { type: "DiscardToHandSize" }>;
type SacrificeForCost = Extract<WaitingFor, { type: "SacrificeForCost" }>;
type ExileFromGraveyardForCost = Extract<WaitingFor, { type: "ExileFromGraveyardForCost" }>;
type HarmonizeTapChoice = Extract<WaitingFor, { type: "HarmonizeTapChoice" }>;
type ChooseLegend = Extract<WaitingFor, { type: "ChooseLegend" }>;
type ManifestDreadChoice = Extract<WaitingFor, { type: "ManifestDreadChoice" }>;
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
    case "ExileFromGraveyardForCost":
      if (waitingFor.data.player !== playerId) return null;
      return <ExileFromGraveyardModal data={waitingFor.data} />;
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
      return <DiscardModal data={waitingFor.data} title={`Discard ${waitingFor.data.count === 1 ? "a card" : `${waitingFor.data.count} cards`}`} />;
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
      title="Ward \u2014 Sacrifice a permanent"
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

// ── Discard to Hand Size Modal ───────────────────────────────────────────────

function DiscardModal({ data, title = "Discard" }: { data: DiscardToHandSize["data"]; title?: string }) {
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
      title={title}
      subtitle={`Choose ${data.count} card${data.count > 1 ? "s" : ""} to discard`}
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
