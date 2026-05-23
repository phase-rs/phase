import { useCallback, useState } from "react";
import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import type { GameObject, ObjectId, WaitingFor } from "../../adapter/types.ts";

type SeparatePilesPartition = Extract<WaitingFor, { type: "SeparatePilesPartition" }>;
type SeparatePilesChoice = Extract<WaitingFor, { type: "SeparatePilesChoice" }>;

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

/**
 * CR 700.3 + CR 700.3a: Partition prompt for `Effect::SeparateIntoPiles`.
 * The subject (`data.player`) toggles each eligible object between pile A
 * and pile B; pile B is the derived complement (eligible ∖ pile_a). Empty
 * piles are legal (CR 700.3d). Submitting dispatches
 * `GameAction::SubmitPilePartition { pile_a }`.
 *
 * Display layer only — `eligible`, `completed`, and `remaining_subjects`
 * all come straight from the engine's `WaitingFor::SeparatePilesPartition`.
 */
export function SeparatePilesPartitionModal({
  data,
}: {
  data: SeparatePilesPartition["data"];
}) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  // pile_a membership — toggling moves an object between piles. Pile B is
  // the derived complement (eligible ∖ pile_a) and is not stored.
  const [pileA, setPileA] = useState<Set<ObjectId>>(new Set());

  const toggle = useCallback((id: ObjectId) => {
    setPileA((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    // Preserve engine-provided order so the partition is stable across
    // serializations.
    const pile_a = data.eligible.filter((id) => pileA.has(id));
    dispatch({ type: "SubmitPilePartition", data: { pile_a } });
  }, [dispatch, data.eligible, pileA]);

  const pileBCount = data.eligible.length - pileA.size;
  // The partitioner (CR 700.3) is the SUBJECT of `data.player`; the chooser
  // is announced so the partitioner understands who decides the outcome.
  const chooserName = `Player ${data.chooser + 1}`;
  const remaining = data.remaining_subjects.length;
  const subtitle = remaining > 0
    ? `Separate your creatures into two piles. ${chooserName} will choose one. (${remaining} more player${remaining === 1 ? "" : "s"} after you)`
    : `Separate your creatures into two piles. ${chooserName} will choose one.`;

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Separate into Two Piles"
      subtitle={subtitle}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-5xl"
      footer={<ConfirmButton onClick={handleConfirm} />}
    >
      <div className="mb-4 flex justify-center gap-6 text-sm text-slate-300">
        <span className="rounded bg-emerald-900/40 px-3 py-1">
          Pile A: <span className="font-bold text-emerald-300">{pileA.size}</span>
        </span>
        <span className="rounded bg-sky-900/40 px-3 py-1">
          Pile B: <span className="font-bold text-sky-300">{pileBCount}</span>
        </span>
      </div>
      <div className="mx-auto flex max-w-5xl flex-wrap justify-center gap-3">
        {data.eligible.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const inA = pileA.has(id);
          return (
            <motion.button
              key={id}
              type="button"
              aria-label={`${obj.name} — pile ${inA ? "A" : "B"}`}
              className={`relative flex flex-col items-center gap-2 rounded-lg transition ${
                inA
                  ? "ring-2 ring-emerald-400/80"
                  : "ring-2 ring-sky-400/60"
              }`}
              initial={{ opacity: 0, y: 30, scale: 0.92 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.04, duration: 0.25 }}
              whileHover={{ scale: 1.04, y: -4 }}
              onClick={() => toggle(id)}
              {...hoverProps(id)}
            >
              <CardImage {...objectImageProps(obj)} size="normal" />
              <span
                className={`rounded-full px-3 py-1 text-xs font-bold transition ${
                  inA
                    ? "bg-emerald-500/80 text-white"
                    : "bg-sky-500/70 text-white"
                }`}
              >
                Pile {inA ? "A" : "B"}
              </span>
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}

/**
 * CR 700.3 + CR 101.4c: Chooser prompt for `Effect::SeparateIntoPiles`. The
 * chooser (`data.player`) picks pile A or pile B of the current subject's
 * partition; the engine then applies `chosen_pile_effect` once per object in
 * the chosen pile (CR 608.2c). Empty piles are legal (CR 700.3d).
 *
 * Display layer only — `data.current` is the engine-owned `PileResult` for
 * the head of the chooser queue; `data.pending` is informational (later
 * subjects whose piles have not yet been chosen).
 */
export function SeparatePilesChoiceModal({
  data,
}: {
  data: SeparatePilesChoice["data"];
}) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();

  const chooseA = useCallback(() => {
    dispatch({ type: "ChoosePile", data: { pile: { type: "A" } } });
  }, [dispatch]);
  const chooseB = useCallback(() => {
    dispatch({ type: "ChoosePile", data: { pile: { type: "B" } } });
  }, [dispatch]);

  const subjectName = `Player ${data.current.subject + 1}`;
  const pendingCount = data.pending.length;
  const subtitle = pendingCount > 0
    ? `Choose one of ${subjectName}'s piles. (${pendingCount} more partition${pendingCount === 1 ? "" : "s"} after this)`
    : `Choose one of ${subjectName}'s piles.`;

  const renderPile = useCallback(
    (ids: ObjectId[], label: "A" | "B", tone: "emerald" | "sky") => {
      const toneRing = tone === "emerald" ? "ring-emerald-400/80" : "ring-sky-400/80";
      const toneBg = tone === "emerald" ? "bg-emerald-900/30" : "bg-sky-900/30";
      const toneHeader = tone === "emerald" ? "text-emerald-300" : "text-sky-300";
      return (
        <div className={`flex flex-1 flex-col gap-3 rounded-lg ${toneBg} p-4 ring-2 ${toneRing}`}>
          <div className={`text-center text-lg font-bold ${toneHeader}`}>
            Pile {label} ({ids.length})
          </div>
          {ids.length === 0 ? (
            <div className="my-6 text-center text-sm italic text-slate-400">
              (empty)
            </div>
          ) : (
            <div className="flex flex-wrap justify-center gap-3">
              {ids.map((id) => {
                const obj = objects?.[id];
                if (!obj) return null;
                return (
                  <div
                    key={id}
                    className="flex flex-col items-center"
                    {...hoverProps(id)}
                  >
                    <CardImage {...objectImageProps(obj)} size="normal" />
                  </div>
                );
              })}
            </div>
          )}
        </div>
      );
    },
    [objects, hoverProps],
  );

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Choose a Pile"
      subtitle={subtitle}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-5xl"
      footer={
        <div className="mx-auto flex w-full max-w-xl gap-3">
          <div className="flex-1">
            <ConfirmButton onClick={chooseA} label="Choose Pile A" />
          </div>
          <div className="flex-1">
            <ConfirmButton onClick={chooseB} label="Choose Pile B" />
          </div>
        </div>
      }
    >
      <div className="flex flex-col gap-4 sm:flex-row">
        {renderPile(data.current.pile_a, "A", "emerald")}
        {renderPile(data.current.pile_b, "B", "sky")}
      </div>
    </ChoiceOverlay>
  );
}
