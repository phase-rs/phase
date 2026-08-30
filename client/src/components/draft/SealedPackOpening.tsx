import { useEffect, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance, DraftPoolGroup, DraftPlayerView } from "../../adapter/draft-adapter";
import { useCardImage } from "../../hooks/useCardImage";
import { menuButtonClass } from "../menu/buttonStyles";
import { getCardImageSrcSetProps } from "../card/cardImageSrcSet.ts";
import { POOL_GROUP_LABEL_KEYS } from "./poolGroupLabels";

interface SealedPackOpeningProps {
  view: DraftPlayerView;
  onComplete: () => void;
}

function PullCard({ card, index }: { card: DraftCardInstance; index: number }) {
  const { src, isLoading, rungs, advanceFailedSource } = useCardImage(card.name, {
    size: "normal",
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  });
  const reduceMotion = useReducedMotion();

  return (
    <motion.div
      initial={reduceMotion ? false : { opacity: 0, y: 26, rotate: index % 2 ? 2 : -2 }}
      animate={{ opacity: 1, y: 0, rotate: 0 }}
      transition={{ delay: reduceMotion ? 0 : index * 0.045, type: "spring", stiffness: 230, damping: 22 }}
      className="relative overflow-hidden rounded-[14px] ring-1 ring-white/10"
    >
      {isLoading || !src ? (
        <div className="flex aspect-[488/680] animate-pulse items-center justify-center bg-white/5 px-2 text-center text-xs text-white/40">
          {card.name}
        </div>
      ) : (
        <img
          src={src}
          {...getCardImageSrcSetProps(src, rungs)}
          alt={card.name}
          draggable={false}
          onError={() => advanceFailedSource?.(src)}
          className="aspect-[488/680] w-full object-cover"
        />
      )}
      <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/80 to-transparent px-2 pb-1.5 pt-5">
        <span className="line-clamp-1 text-[10px] leading-tight text-white/85">{card.name}</span>
      </div>
    </motion.div>
  );
}

function PackBack({ onOpen, packNumber, packCount }: {
  onOpen: () => void;
  packNumber: number;
  packCount: number;
}) {
  const { t } = useTranslation("draft");
  const reduceMotion = useReducedMotion();

  return (
    <motion.div
      initial={reduceMotion ? false : { opacity: 0, scale: 0.9, y: 20 }}
      animate={{ opacity: 1, scale: 1, y: 0 }}
      exit={{ opacity: 0, scale: 1.08, rotate: 4 }}
      transition={{ type: "spring", stiffness: 220, damping: 20 }}
      className="flex flex-col items-center gap-7 py-8"
    >
      <p className="text-sm font-medium uppercase tracking-[0.18em] text-white/50">
        {t("sealedOpening.packProgress", { current: packNumber, total: packCount })}
      </p>
      <motion.button
        type="button"
        aria-label={t("sealedOpening.openPackArt")}
        onClick={onOpen}
        whileHover={reduceMotion ? undefined : { y: -5, rotate: -1 }}
        whileTap={{ scale: 0.97 }}
        className="relative flex aspect-[5/7] w-52 items-center justify-center overflow-hidden rounded-[18px] border border-amber-200/35 bg-[radial-gradient(circle_at_35%_22%,rgba(254,243,199,0.45),transparent_25%),linear-gradient(145deg,#713f12,#b45309_45%,#451a03)] shadow-[0_24px_50px_rgba(0,0,0,0.45),inset_0_1px_0_rgba(255,255,255,0.4)] focus-visible:ring-[3px] focus-visible:ring-amber-200/50"
      >
        <span className="absolute inset-3 rounded-[12px] border border-amber-100/25" />
        <span className="absolute inset-x-0 top-1/2 h-px bg-amber-100/30" />
        <span className="relative flex h-20 w-20 items-center justify-center rounded-full border border-amber-100/45 bg-amber-950/35 text-3xl text-amber-100 shadow-[inset_0_1px_10px_rgba(0,0,0,0.45)]">
          ✦
        </span>
      </motion.button>
      <button
        type="button"
        onClick={onOpen}
        className={menuButtonClass({ tone: "emerald", size: "lg" })}
      >
        {t("sealedOpening.openPack")}
      </button>
    </motion.div>
  );
}

function OpenedPack({ cards, packNumber, packCount, onNext }: {
  cards: DraftCardInstance[];
  packNumber: number;
  packCount: number;
  onNext: () => void;
}) {
  const { t } = useTranslation("draft");

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      className="flex flex-col gap-5"
    >
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <p className="text-sm font-medium uppercase tracking-[0.18em] text-white/50">
            {t("sealedOpening.packProgress", { current: packNumber, total: packCount })}
          </p>
          <h2 className="mt-1 menu-display text-2xl text-white">{t("sealedOpening.pulls")}</h2>
        </div>
        <span className="text-sm text-white/45">{t("pack.cardsInPack", { count: cards.length })}</span>
      </div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-7">
        {cards.map((card, index) => <PullCard key={card.instance_id} card={card} index={index} />)}
      </div>
      <div className="flex justify-center pt-2">
        <button type="button" onClick={onNext} className={menuButtonClass({ tone: "emerald", size: "lg" })}>
          {packNumber === packCount ? t("sealedOpening.viewPool") : t("sealedOpening.nextPack")}
        </button>
      </div>
    </motion.div>
  );
}

function SealedPoolReview({ groups, poolSize, onComplete }: {
  groups: DraftPoolGroup[];
  poolSize: number;
  onComplete: () => void;
}) {
  const { t } = useTranslation("draft");

  return (
    <motion.div initial={{ opacity: 0, y: 12 }} animate={{ opacity: 1, y: 0 }} className="flex flex-col gap-7">
      <div className="text-center">
        <h2 className="menu-display text-3xl text-white">{t("sealedOpening.poolTitle")}</h2>
        <p className="mt-2 text-sm text-white/50">{t("sealedOpening.poolSubtitle", { count: poolSize })}</p>
      </div>
      {groups.map((group) => (
        <section key={group.kind}>
          <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-400">
            {t(POOL_GROUP_LABEL_KEYS[group.kind])} ({group.total})
          </h3>
          <div className="grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-8">
            {group.cards.map(({ card, count }, index) => (
              <div key={card.instance_id} className="relative">
                <PullCard card={card} index={index} />
                {count > 1 && (
                  <span className="absolute right-1 top-1 flex h-5 min-w-5 items-center justify-center rounded-full bg-black/75 px-1 text-[10px] font-bold text-white ring-1 ring-white/15">
                    {count}
                  </span>
                )}
              </div>
            ))}
          </div>
        </section>
      ))}
      <div className="flex justify-center pt-2">
        <button type="button" onClick={onComplete} className={menuButtonClass({ tone: "emerald", size: "lg" })}>
          {t("sealedOpening.buildDeck")}
        </button>
      </div>
    </motion.div>
  );
}

/** Displays the engine-provided sealed packs one at a time before deckbuilding. */
export function SealedPackOpening({ view, onComplete }: SealedPackOpeningProps) {
  const { t } = useTranslation("draft");
  const packs = view.sealed_packs ?? [];
  const [packIndex, setPackIndex] = useState(0);
  const [opened, setOpened] = useState(false);

  useEffect(() => {
    setOpened(false);
  }, [packIndex]);

  if (packs.length === 0) return null;

  const packNumber = packIndex + 1;
  const showReview = packIndex >= packs.length;
  const advance = () => setPackIndex((current) => current + 1);

  return (
    <div className="mx-auto w-full max-w-6xl py-4" aria-live="polite">
      {!showReview && (
        <div className="mb-5 text-center">
          <h1 className="menu-display text-3xl text-white">{t("sealedOpening.title")}</h1>
          <p className="mt-2 text-sm text-white/50">
            {t("sealedOpening.subtitle", { count: view.pack_count })}
          </p>
        </div>
      )}
      <AnimatePresence mode="wait">
        {showReview ? (
          <SealedPoolReview
            key="pool-review"
            groups={view.pool_groups.type_groups}
            poolSize={view.pool.length}
            onComplete={onComplete}
          />
        ) : opened ? (
          <OpenedPack
            key={`opened-${packIndex}`}
            cards={packs[packIndex]}
            packNumber={packNumber}
            packCount={packs.length}
            onNext={advance}
          />
        ) : (
          <PackBack
            key={`closed-${packIndex}`}
            packNumber={packNumber}
            packCount={packs.length}
            onOpen={() => setOpened(true)}
          />
        )}
      </AnimatePresence>
    </div>
  );
}
