import { Fragment, useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";

import { CardImage } from "../card/CardImage.tsx";
import { cardImageLookup, tokenFiltersForObject } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useChoiceCardLayout } from "../../hooks/useChoiceCardLayout.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import {
  ChoiceCardGrid,
  ChoiceHeaderConfirm,
  ChoiceOverlay,
  ConfirmButton,
  type ChoiceCardEntry,
} from "./ChoiceOverlay.tsx";
import type { GameObject, ObjectId, WaitingFor } from "../../adapter/types.ts";

type SeparatePilesPartition = Extract<WaitingFor, { type: "SeparatePilesPartition" }>;
type SeparatePilesChoice = Extract<WaitingFor, { type: "SeparatePilesChoice" }>;

// Below lg the overlay fills a phone's screen; from lg up it stays the panel
// sized to its cards.
const PILES_OVERLAY_WIDTH = "w-full lg:w-fit max-w-full";

/** Each pile's colors, shared by the desktop pile and the phone's. */
const PILE_TONE_CLASSES = {
  emerald: { ring: "ring-emerald-400/80", background: "bg-emerald-900/30", header: "text-emerald-300" },
  sky: { ring: "ring-sky-400/80", background: "bg-sky-900/30", header: "text-sky-300" },
} as const satisfies Record<string, { ring: string; background: string; header: string }>;

type PileTone = keyof typeof PILE_TONE_CLASSES;

function objectImageProps(obj: GameObject) {
  const { name, faceIndex, oracleId, faceName, ownerPrinting } = cardImageLookup(obj);
  const isToken = obj.display_source === "Token";
  return {
    cardName: name,
    faceIndex,
    oracleId,
    faceName,
    ownerPrinting,
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
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const isCompactHeight = useIsCompactHeight();
  const { isBelowLg, hasSwitched } = useChoiceCardLayout();
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
  const chooserName = t("separatePiles.chooserName", { number: data.chooser + 1 });
  const remaining = data.remaining_subjects.length;
  const subtitle = remaining > 0
    ? t("separatePiles.partitionSubtitleRemaining", { chooser: chooserName, count: remaining })
    : t("separatePiles.partitionSubtitle", { chooser: chooserName });

  if (!objects) return null;

  const renderCard = (id: ObjectId, entry: ChoiceCardEntry) => {
    const obj = objects[id];
    if (!obj) return null;
    const inA = pileA.has(id);
    return (
      <motion.button
        type="button"
        aria-label={t("separatePiles.pileAria", { name: obj.name, pile: inA ? "A" : "B" })}
        className={`relative flex flex-col items-center gap-2 rounded-lg transition ${
          inA
            ? "ring-2 ring-emerald-400/80"
            : "ring-2 ring-sky-400/60"
        }`}
        {...entry}
        animate={{ opacity: 1, y: 0, scale: 1 }}
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
          {t("separatePiles.pileBadge", { pile: inA ? "A" : "B" })}
        </span>
      </motion.button>
    );
  };

  // The pile counts stay above the cards: on a phone the grid below them scrolls.
  const body = (
    <>
      <div className="mb-4 flex justify-center gap-6 text-sm text-slate-300">
        <span className="rounded bg-emerald-900/40 px-3 py-1">
          {t("separatePiles.pileALabel")} <span className="font-bold text-emerald-300">{pileA.size}</span>
        </span>
        <span className="rounded bg-sky-900/40 px-3 py-1">
          {t("separatePiles.pileBLabel")} <span className="font-bold text-sky-300">{pileBCount}</span>
        </span>
      </div>
      {isBelowLg ? (
        <ChoiceCardGrid cards={data.eligible} renderCard={renderCard} />
      ) : (
        <div className="mx-auto flex max-w-5xl flex-wrap justify-center gap-3">
          {data.eligible.map((id, index) => (
            <Fragment key={id}>
              {renderCard(
                id,
                // A device turned across lg remounts the cards; they skip their entrance then.
                hasSwitched
                  ? { initial: false, transition: { duration: 0.25 } }
                  : {
                      initial: { opacity: 0, y: 30, scale: 0.92 },
                      transition: { delay: 0.05 + index * 0.04, duration: 0.25 },
                    },
              )}
            </Fragment>
          ))}
        </div>
      )}
    </>
  );

  // On a short screen (a landscape phone) one header line carries the title
  // and who chooses a pile, with the confirm at its end and no footer, as the
  // other card choices do there. The title stays: it names the action, which
  // the chooser half of the line does not.
  if (isCompactHeight) {
    const instruction = remaining > 0
      ? t("separatePiles.partitionInstructionRemaining", { chooser: chooserName, count: remaining })
      : t("separatePiles.partitionInstruction", { chooser: chooserName });
    return (
      <ChoiceOverlay
        title={t("choiceOverlay.titleWithInstruction", {
          title: t("separatePiles.partitionTitle"),
          instruction,
        })}
        widthClassName={PILES_OVERLAY_WIDTH}
        maxWidthClassName="max-w-5xl"
        headerActions={<ChoiceHeaderConfirm onClick={handleConfirm} />}
      >
        {body}
      </ChoiceOverlay>
    );
  }

  return (
    <ChoiceOverlay
      title={t("separatePiles.partitionTitle")}
      subtitle={subtitle}
      widthClassName={PILES_OVERLAY_WIDTH}
      maxWidthClassName="max-w-5xl"
      footer={<ConfirmButton onClick={handleConfirm} />}
    >
      {body}
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
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const isCompactHeight = useIsCompactHeight();
  const { isBelowLg } = useChoiceCardLayout();

  const chooseA = useCallback(() => {
    dispatch({ type: "ChoosePile", data: { pile: { type: "A" } } });
  }, [dispatch]);
  const chooseB = useCallback(() => {
    dispatch({ type: "ChoosePile", data: { pile: { type: "B" } } });
  }, [dispatch]);

  const subjectName = t("separatePiles.chooserName", { number: data.current.subject + 1 });
  const pendingCount = data.pending.length;
  const subtitle = pendingCount > 0
    ? t("separatePiles.chooseSubtitleRemaining", { subject: subjectName, count: pendingCount })
    : t("separatePiles.chooseSubtitle", { subject: subjectName });

  // One card of a pile, for both layouts: the desktop pile and the phone's grid.
  const renderPileCard = useCallback(
    (id: ObjectId) => {
      const obj = objects?.[id];
      if (!obj) return null;
      return (
        <div className="flex flex-col items-center" {...hoverProps(id)}>
          <CardImage {...objectImageProps(obj)} size="normal" />
        </div>
      );
    },
    [objects, hoverProps],
  );

  const renderPile = useCallback(
    (ids: ObjectId[], label: "A" | "B", tone: PileTone) => {
      const toneClasses = PILE_TONE_CLASSES[tone];
      return (
        <div
          className={`flex flex-1 flex-col gap-3 rounded-lg ${toneClasses.background} p-4 ring-2 ${toneClasses.ring}`}
        >
          <div className={`text-center text-lg font-bold ${toneClasses.header}`}>
            {t("separatePiles.pileHeader", { pile: label, count: ids.length })}
          </div>
          {ids.length === 0 ? (
            <div className="my-6 text-center text-sm italic text-slate-400">
              {t("separatePiles.empty")}
            </div>
          ) : (
            <div className="flex flex-wrap justify-center gap-3">
              {ids.map((id) => (
                <Fragment key={id}>{renderPileCard(id)}</Fragment>
              ))}
            </div>
          )}
        </div>
      );
    },
    [renderPileCard, t],
  );

  if (!objects) return null;

  // On a phone each pile fills its share of the screen with its own grid of
  // larger cards that scrolls down. The piles sit side by side on a phone
  // turned sideways and stacked on one held upright, so each grid keeps the
  // width for two columns. A pile is marked by its ring and header alone: the
  // grid's scroll fades are the overlay's color, which a tinted pile would show.
  const renderPhonePile = (ids: ObjectId[], label: "A" | "B", tone: PileTone) => (
    <div className={`flex min-h-0 flex-1 flex-col gap-1 rounded-lg p-2 ring-2 ${PILE_TONE_CLASSES[tone].ring}`}>
      <div className={`text-center font-bold ${PILE_TONE_CLASSES[tone].header}`}>
        {t("separatePiles.pileHeader", { pile: label, count: ids.length })}
      </div>
      {ids.length === 0 ? (
        <div className="my-6 text-center text-sm italic text-slate-400">
          {t("separatePiles.empty")}
        </div>
      ) : (
        <ChoiceCardGrid cards={ids} renderCard={renderPileCard} />
      )}
    </div>
  );

  const body = isBelowLg ? (
    <div className="flex min-h-0 flex-1 flex-col gap-3 landscape:flex-row">
      {renderPhonePile(data.current.pile_a, "A", "emerald")}
      {renderPhonePile(data.current.pile_b, "B", "sky")}
    </div>
  ) : (
    <div className="flex flex-col gap-4 sm:flex-row">
      {renderPile(data.current.pile_a, "A", "emerald")}
      {renderPile(data.current.pile_b, "B", "sky")}
    </div>
  );

  // On a short screen (a landscape phone) the choice is one header line: the
  // instruction alone, since "Choose a Pile" only repeats it, with both choose
  // buttons at its end and no footer. The buttons stay apart from the piles
  // rather than making a pile tappable: a tap on a card inspects it, and must
  // never choose a pile by accident.
  if (isCompactHeight) {
    return (
      <ChoiceOverlay
        title={subtitle}
        widthClassName={PILES_OVERLAY_WIDTH}
        maxWidthClassName="max-w-5xl"
        headerActions={
          <>
            <ChoiceHeaderConfirm onClick={chooseA} label={t("separatePiles.choosePileA")} />
            <ChoiceHeaderConfirm onClick={chooseB} label={t("separatePiles.choosePileB")} />
          </>
        }
      >
        {body}
      </ChoiceOverlay>
    );
  }

  return (
    <ChoiceOverlay
      title={t("separatePiles.chooseTitle")}
      subtitle={subtitle}
      widthClassName={PILES_OVERLAY_WIDTH}
      maxWidthClassName="max-w-5xl"
      footer={
        <div className="mx-auto flex w-full max-w-xl gap-3">
          <div className="flex-1">
            <ConfirmButton onClick={chooseA} label={t("separatePiles.choosePileA")} />
          </div>
          <div className="flex-1">
            <ConfirmButton onClick={chooseB} label={t("separatePiles.choosePileB")} />
          </div>
        </div>
      }
    >
      {body}
    </ChoiceOverlay>
  );
}
