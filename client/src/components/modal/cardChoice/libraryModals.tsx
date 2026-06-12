import { useCallback, useState } from "react";
import { motion, Reorder } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { ObjectId, WaitingFor } from "../../../adapter/types";
import { CardImage } from "../../card/CardImage";
import { objectImageProps } from "../../../services/cardImageLookup";
import { useGameStore } from "../../../stores/gameStore";
import { useGameDispatch } from "../../../hooks/useGameDispatch";
import { useInspectHoverProps } from "../../../hooks/useInspectHoverProps";
import { ChoiceOverlay, ConfirmButton, ScrollableCardStrip } from "../ChoiceOverlay";
import { CHOICE_CARD_IMAGE_CLASS } from "./shared";

type ScryChoice = Extract<WaitingFor, { type: "ScryChoice" }>;
type CoinFlipKeepChoice = Extract<WaitingFor, { type: "CoinFlipKeepChoice" }>;
type DigChoice = Extract<WaitingFor, { type: "DigChoice" }>;
type SurveilChoice = Extract<WaitingFor, { type: "SurveilChoice" }>;
type RevealChoice = Extract<WaitingFor, { type: "RevealChoice" }>;

export function ReorderableTopChoice({
  cards,
  title,
  subtitle,
  keepLabel,
  restLabel,
  reorderHint,
  keepTone,
}: {
  cards: ObjectId[];
  title: string;
  subtitle: string;
  keepLabel: string;
  restLabel: string;
  reorderHint: string;
  keepTone: "emerald" | "blue";
}) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const hoverProps = useInspectHoverProps();
  const [order, setOrder] = useState<ObjectId[]>(cards);
  const [restSet, setRestSet] = useState<Set<ObjectId>>(new Set());

  const toggleRest = useCallback((id: ObjectId) => {
    setRestSet((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleConfirm = useCallback(() => {
    const keep = order.filter((id) => !restSet.has(id));
    dispatch({ type: "SelectCards", data: { cards: keep } });
  }, [dispatch, order, restSet]);

  if (!objects) return null;

  const overlayWidthClassName =
    cards.length <= 1
      ? "max-w-[22rem] sm:max-w-[26rem] lg:max-w-[30rem]"
      : cards.length === 2
        ? "max-w-[30rem] sm:max-w-[38rem] lg:max-w-[46rem]"
        : "max-w-[38rem] sm:max-w-[48rem] lg:max-w-[58rem]";

  const keepRing =
    keepTone === "emerald"
      ? "ring-emerald-400/70 hover:shadow-[0_0_16px_rgba(100,220,150,0.3)]"
      : "ring-blue-400/70 hover:shadow-[0_0_16px_rgba(100,150,255,0.3)]";
  const keepBtn = keepTone === "emerald" ? "bg-emerald-500/80" : "bg-blue-500/80";
  const keepBadge = keepTone === "emerald" ? "bg-emerald-500/90" : "bg-blue-500/90";
  const keepOrder = order.filter((id) => !restSet.has(id));

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      maxWidthClassName={overlayWidthClassName}
      footer={<ConfirmButton onClick={handleConfirm} />}
    >
      <Reorder.Group
        as="div"
        axis="x"
        values={order}
        onReorder={setOrder}
        className="mx-auto flex min-h-0 flex-1 items-center justify-center gap-2 overflow-x-auto px-1 py-2 lg:gap-3"
      >
        {order.map((id) => {
          const obj = objects[id];
          if (!obj) return null;
          const isRest = restSet.has(id);
          const position = keepOrder.indexOf(id) + 1;
          return (
            <Reorder.Item
              key={id}
              as="div"
              value={id}
              className="relative flex shrink-0 cursor-grab flex-col items-center gap-2 active:cursor-grabbing"
              whileDrag={{ scale: 1.05, zIndex: 20 }}
            >
              <div
                className={`relative rounded-lg ring-2 transition ${
                  isRest ? "opacity-50 ring-red-400/70" : keepRing
                }`}
                {...hoverProps(id)}
              >
                <CardImage
                  {...objectImageProps(obj)}
                  size="normal"
                  className={CHOICE_CARD_IMAGE_CLASS}
                />
                {!isRest && (
                  <div
                    className={`pointer-events-none absolute left-1 top-1 flex h-6 w-6 items-center justify-center rounded-full text-xs font-bold text-white ${keepBadge}`}
                  >
                    {position}
                  </div>
                )}
              </div>
              <button
                onClick={() => toggleRest(id)}
                className={`rounded-full px-3 py-1 text-xs font-bold text-white transition ${
                  isRest ? "bg-red-500/80" : keepBtn
                }`}
              >
                {isRest ? restLabel : keepLabel}
              </button>
            </Reorder.Item>
          );
        })}
      </Reorder.Group>
      <p className="mt-1 shrink-0 text-center text-xs text-slate-400">{reorderHint}</p>
    </ChoiceOverlay>
  );
}

export function ScryModal({ data }: { data: ScryChoice["data"] }) {
  const { t } = useTranslation("game");
  return (
    <ReorderableTopChoice
      key={data.cards.join("-")}
      cards={data.cards}
      title={t("cardChoice.scry.title")}
      subtitle={t("cardChoice.scry.subtitle", { count: data.cards.length })}
      keepLabel={t("cardChoice.badges.top")}
      restLabel={t("cardChoice.badges.bottom")}
      reorderHint={t("cardChoice.reorderHint")}
      keepTone="emerald"
    />
  );
}

export function SurveilModal({ data }: { data: SurveilChoice["data"] }) {
  const { t } = useTranslation("game");
  return (
    <ReorderableTopChoice
      key={data.cards.join("-")}
      cards={data.cards}
      title={t("cardChoice.surveil.title")}
      subtitle={t("cardChoice.surveil.subtitle", { count: data.cards.length })}
      keepLabel={t("cardChoice.badges.keep")}
      restLabel={t("cardChoice.badges.graveyard")}
      reorderHint={t("cardChoice.reorderHint")}
      keepTone="blue"
    />
  );
}

export function CoinFlipKeepModal({ data }: { data: CoinFlipKeepChoice["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();

  const keepFlip = useCallback(
    (index: number) => {
      dispatch({
        type: "SelectCoinFlips",
        data: { keep_indices: [index] },
      });
    },
    [dispatch],
  );

  return (
    <ChoiceOverlay
      title={t("coinFlip.keep.title")}
      subtitle={t("coinFlip.keep.subtitle")}
    >
      <div className="flex flex-wrap justify-center gap-4">
        {data.results.map((won, index) => (
          <motion.button
            key={index}
            type="button"
            onClick={() => keepFlip(index)}
            className="flex flex-col items-center gap-2 rounded-xl border border-white/10 bg-white/5 px-6 py-4"
            whileHover={{ scale: 1.05 }}
            whileTap={{ scale: 0.98 }}
          >
            <span
              className={`flex h-16 w-16 items-center justify-center rounded-full text-sm font-bold ${
                won ? "bg-amber-400/90 text-amber-950" : "bg-slate-500/80 text-slate-100"
              }`}
            >
              {won ? t("coinFlip.keep.heads") : t("coinFlip.keep.tails")}
            </span>
            <span className="rounded-full bg-emerald-500/90 px-3 py-1 text-xs font-bold text-white">
              {t("coinFlip.buttons.keep")}
            </span>
          </motion.button>
        ))}
      </div>
    </ChoiceOverlay>
  );
}

export function DigModal({ data }: { data: DigChoice["data"] }) {
  const { t } = useTranslation("game");
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
        if (next.has(id)) next.delete(id);
        else if (next.size < data.keep_count) next.add(id);
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
    data.kept_destination === "Library"
    && data.rest_destination === "Library"
    && data.keep_count === data.cards.length;

  const isReady = isUpTo
    ? selected.size <= data.keep_count
    : selected.size === data.keep_count;

  const destLabel =
    data.kept_destination === "Library"
      ? t("cardChoice.dig.destinationTop")
      : data.kept_destination === "Battlefield"
        ? t("cardChoice.dig.destinationBattlefield")
        : t("cardChoice.dig.destinationHand");

  const title = isReorderOnly ? t("cardChoice.dig.titleReorder") : t("cardChoice.dig.title");
  const subtitle = isReorderOnly
    ? t("cardChoice.dig.subtitleReorder", { count: data.cards.length })
    : isUpTo
      ? t("cardChoice.dig.subtitleUpTo", { count: data.keep_count, destination: destLabel })
      : t("cardChoice.dig.subtitleExact", { count: data.keep_count, destination: destLabel });
  const confirmLabel = isReorderOnly
    ? t("cardChoice.buttons.confirmOrder", { selected: selected.size, count: data.keep_count })
    : t("cardChoice.buttons.confirmCount", { selected: selected.size, count: data.keep_count });

  return (
    <ChoiceOverlay
      title={title}
      subtitle={subtitle}
      footer={
        <ConfirmButton onClick={handleConfirm} disabled={!isReady} label={confirmLabel} />
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
                    {isReorderOnly ? selectedOrder : t("cardChoice.badges.keep")}
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

export function RevealModal({ data }: { data: RevealChoice["data"] }) {
  const { t } = useTranslation("game");
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

  const handleDecline = useCallback(() => {
    dispatch({
      type: "SelectCards",
      data: { cards: [] },
    });
  }, [dispatch]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title={isOptional ? t("cardChoice.reveal.titleReveal") : t("cardChoice.reveal.titleOpponentHand")}
      subtitle={isOptional ? t("cardChoice.reveal.subtitleReveal") : t("cardChoice.reveal.subtitleChoose")}
      footer={
        <div className="flex gap-2">
          {isOptional && <ConfirmButton onClick={handleDecline} label={t("cardChoice.buttons.decline")} />}
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
