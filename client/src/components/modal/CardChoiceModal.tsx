import { useCallback, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { menuButtonClass } from "../menu/buttonStyles.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import type { ObjectId, WaitingFor } from "../../adapter/types.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";

type ScryChoice = Extract<WaitingFor, { type: "ScryChoice" }>;
type DigChoice = Extract<WaitingFor, { type: "DigChoice" }>;
type SurveilChoice = Extract<WaitingFor, { type: "SurveilChoice" }>;

/**
 * Generic card choice modal for Scry, Dig, and Surveil.
 * Renders based on the WaitingFor type:
 * - ScryChoice: per-card Top/Bottom toggles (MTGA-style)
 * - DigChoice: select keep_count cards to keep
 * - SurveilChoice: per-card Keep/Graveyard toggles
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
      <div className="mb-6 flex w-full max-w-4xl items-center justify-center gap-4 sm:mb-10">
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
                  className="h-[clamp(160px,28vh,224px)] w-[clamp(114px,20vh,160px)]"
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
      <div className="mb-6 flex w-full max-w-5xl items-center justify-center gap-3 sm:mb-10">
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
                className="h-[clamp(160px,28vh,224px)] w-[clamp(114px,20vh,160px)]"
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
      <div className="mb-6 flex w-full max-w-4xl items-center justify-center gap-4 sm:mb-10">
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
                  className="h-[clamp(160px,28vh,224px)] w-[clamp(114px,20vh,160px)]"
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

// ── Shared Components ───────────────────────────────────────────────────────

function ChoiceOverlay({
  title,
  subtitle,
  children,
}: {
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <div
      className="fixed inset-0 z-50 flex flex-col items-center justify-center px-4"
      style={{
        background:
          "radial-gradient(ellipse at center, rgba(30,30,50,0.95) 0%, rgba(0,0,0,0.98) 70%)",
      }}
    >
      <motion.div
        className="mb-4 text-center sm:mb-8"
        initial={{ opacity: 0, y: -20 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.5 }}
      >
        <h2
          className="text-2xl font-black tracking-wide text-white sm:text-3xl"
          style={{ textShadow: "0 0 20px rgba(200,200,255,0.3)" }}
        >
          {title}
        </h2>
        <p className="mt-2 text-sm text-gray-400">{subtitle}</p>
      </motion.div>
      {children}
    </div>
  );
}

function ConfirmButton({
  onClick,
  disabled = false,
  label = "Confirm",
}: {
  onClick: () => void;
  disabled?: boolean;
  label?: string;
}) {
  return (
    <AnimatePresence>
      <motion.div
        className="w-full max-w-xs px-4 sm:px-0"
        initial={{ opacity: 0, y: 20 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ delay: 0.5, duration: 0.3 }}
      >
        <button
          onClick={onClick}
          disabled={disabled}
          className={menuButtonClass({
            tone: "cyan",
            size: "lg",
            disabled,
            className: "w-full",
          })}
        >
          {label}
        </button>
      </motion.div>
    </AnimatePresence>
  );
}
