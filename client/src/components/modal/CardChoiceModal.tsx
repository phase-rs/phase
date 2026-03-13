import { useCallback, useState } from "react";
import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ObjectId, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { NamedChoiceModal } from "./NamedChoiceModal.tsx";

type ScryChoice = Extract<WaitingFor, { type: "ScryChoice" }>;
type DigChoice = Extract<WaitingFor, { type: "DigChoice" }>;
type SurveilChoice = Extract<WaitingFor, { type: "SurveilChoice" }>;
type RevealChoice = Extract<WaitingFor, { type: "RevealChoice" }>;
type SearchChoice = Extract<WaitingFor, { type: "SearchChoice" }>;
type DiscardToHandSize = Extract<WaitingFor, { type: "DiscardToHandSize" }>;
const CHOICE_CARD_IMAGE_CLASS = "h-[clamp(136px,24vh,224px)] w-[clamp(97px,17vh,160px)]";
const CHOICE_CARD_ROW_CLASS =
  "mb-6 flex w-full max-w-5xl flex-wrap items-start justify-center gap-3 overflow-y-auto px-1 sm:mb-10";

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
    case "NamedChoice":
      if (waitingFor.data.player !== playerId) return null;
      return <NamedChoiceModal data={waitingFor.data} />;
    case "DiscardToHandSize":
      if (waitingFor.data.player !== playerId) return null;
      return <DiscardModal data={waitingFor.data} />;
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

  return (
    <ChoiceOverlay
      title="Scry"
      subtitle={`Look at the top ${data.cards.length} card${data.cards.length > 1 ? "s" : ""} of your library`}
    >
      <div className={CHOICE_CARD_ROW_CLASS}>
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
                  className={CHOICE_CARD_IMAGE_CLASS}
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
      </div>
      <ConfirmButton onClick={handleConfirm} />
    </ChoiceOverlay>
  );
}

// ── Dig Modal ───────────────────────────────────────────────────────────────

function DigModal({ data }: { data: DigChoice["data"] }) {
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

  const isReady = selected.size === data.keep_count;

  return (
    <ChoiceOverlay
      title="Choose Cards"
      subtitle={`Select ${data.keep_count} card${data.keep_count > 1 ? "s" : ""} to put into your hand`}
    >
      <div className={CHOICE_CARD_ROW_CLASS}>
        {data.cards.map((id, index) => {
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
      </div>
      <ConfirmButton
        onClick={handleConfirm}
        disabled={!isReady}
        label={`Confirm (${selected.size}/${data.keep_count})`}
      />
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
    >
      <div className={CHOICE_CARD_ROW_CLASS}>
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
      </div>
      <ConfirmButton onClick={handleConfirm} />
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
    >
      <div className={CHOICE_CARD_ROW_CLASS}>
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
      </div>
      <ConfirmButton
        onClick={handleConfirm}
        disabled={selected === null}
      />
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
    >
      <div className={CHOICE_CARD_ROW_CLASS}>
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
      </div>
      <ConfirmButton
        onClick={handleConfirm}
        disabled={selectedSet.size !== data.count}
      />
    </ChoiceOverlay>
  );
}

// ── Discard to Hand Size Modal ───────────────────────────────────────────────

function DiscardModal({ data }: { data: DiscardToHandSize["data"] }) {
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
      title="Discard"
      subtitle={`Choose ${data.count} card${data.count > 1 ? "s" : ""} to discard`}
    >
      <div className={CHOICE_CARD_ROW_CLASS}>
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
      </div>
      <ConfirmButton
        onClick={handleConfirm}
        disabled={!isReady}
        label={`Discard (${selected.size}/${data.count})`}
      />
    </ChoiceOverlay>
  );
}
