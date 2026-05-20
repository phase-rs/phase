import { useMemo } from "react";
import { AnimatePresence, motion } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage";
import { useDraftStore } from "../../stores/draftStore";
import { menuButtonClass } from "../menu/buttonStyles";
import type { DraftCardInstance, DraftPlayerView } from "../../adapter/draft-adapter";
import { ManaCurve } from "./ManaCurve";

// Shared enter/exit for cards moving between the pool and the deck.
const CARD_MOTION = {
  layout: true,
  initial: { opacity: 0, scale: 0.85 },
  animate: { opacity: 1, scale: 1 },
  exit: { opacity: 0, scale: 0.85 },
  transition: { duration: 0.18, ease: "easeOut" as const },
};

// ── Constants ───────────────────────────────────────────────────────────

const BASIC_LANDS = [
  { name: "Plains", color: "W", colorClass: "bg-yellow-200" },
  { name: "Island", color: "U", colorClass: "bg-blue-400" },
  { name: "Swamp", color: "B", colorClass: "bg-slate-400" },
  { name: "Mountain", color: "R", colorClass: "bg-red-500" },
  { name: "Forest", color: "G", colorClass: "bg-green-500" },
] as const;

const LAND_COLOR_CLASSES: Record<string, string> = {
  Plains: "bg-yellow-200",
  Island: "bg-blue-400",
  Swamp: "bg-slate-400",
  Mountain: "bg-red-500",
  Forest: "bg-green-500",
  Wastes: "bg-neutral-300",
};

// ── Card image tile ─────────────────────────────────────────────────────

interface CardTileProps {
  card: DraftCardInstance;
  count?: number;
  dimmed?: boolean;
  onClick: () => void;
}

function CardTile({ card, count, dimmed, onClick }: CardTileProps) {
  const { src, isLoading } = useCardImage(card.name, {
    size: "normal",
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  });

  return (
    <button
      onClick={onClick}
      className={`relative cursor-pointer overflow-hidden rounded-[14px] ring-1 ring-white/10 transition-all duration-150 hover:scale-[1.02] hover:ring-white/20
        ${dimmed ? "opacity-70 hover:opacity-90" : ""}`}
    >
      {isLoading || !src ? (
        <div className="flex aspect-[488/680] animate-pulse items-center justify-center bg-white/5">
          <span className="px-2 text-center text-xs text-white/40">{card.name}</span>
        </div>
      ) : (
        <img
          src={src}
          alt={card.name}
          draggable={false}
          className="aspect-[488/680] w-full object-cover"
        />
      )}
      <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/80 to-transparent px-1.5 py-1">
        <span className="line-clamp-1 text-[10px] leading-tight text-white/80">
          {card.name}
        </span>
      </div>
      {count !== undefined && count > 1 && (
        <div className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-full bg-black/70 text-[10px] font-bold text-white">
          {count}
        </div>
      )}
    </button>
  );
}

// ── Land row ────────────────────────────────────────────────────────────

interface LandRowProps {
  name: string;
  colorClass: string;
  count: number;
  onDecrement: () => void;
  onIncrement: () => void;
}

function LandRow({ name, colorClass, count, onDecrement, onIncrement }: LandRowProps) {
  return (
    <div className="flex items-center gap-2">
      <div className={`h-3 w-3 shrink-0 rounded-full ${colorClass}`} />
      <span className="flex-1 text-sm text-white/60">{name}</span>
      <button
        type="button"
        onClick={onDecrement}
        disabled={count <= 0}
        aria-label={`Remove ${name}`}
        className={menuButtonClass({ tone: "neutral", size: "icon", disabled: count <= 0, className: "font-bold" })}
      >
        -
      </button>
      <span className="w-6 text-center text-sm tabular-nums text-white">{count}</span>
      <button
        type="button"
        onClick={onIncrement}
        aria-label={`Add ${name}`}
        className={menuButtonClass({ tone: "neutral", size: "icon", className: "font-bold" })}
      >
        +
      </button>
    </div>
  );
}

// ── Helpers ─────────────────────────────────────────────────────────────

function groupByName(
  cards: DraftCardInstance[],
  nameList: string[],
): { card: DraftCardInstance; count: number }[] {
  const countMap = new Map<string, number>();
  for (const name of nameList) {
    countMap.set(name, (countMap.get(name) ?? 0) + 1);
  }

  const seen = new Set<string>();
  const groups: { card: DraftCardInstance; count: number }[] = [];
  for (const card of cards) {
    if (!seen.has(card.name) && countMap.has(card.name)) {
      seen.add(card.name);
      groups.push({ card, count: countMap.get(card.name)! });
    }
  }

  return groups;
}

function computeRemainingPool(
  pool: DraftCardInstance[],
  mainDeck: string[],
): DraftCardInstance[] {
  const deckCounts = new Map<string, number>();
  for (const name of mainDeck) {
    deckCounts.set(name, (deckCounts.get(name) ?? 0) + 1);
  }

  const remaining: DraftCardInstance[] = [];
  const used = new Map<string, number>();
  for (const card of pool) {
    const usedCount = used.get(card.name) ?? 0;
    const deckCount = deckCounts.get(card.name) ?? 0;
    if (usedCount < deckCount) {
      used.set(card.name, usedCount + 1);
    } else {
      remaining.push(card);
    }
  }
  return remaining;
}

// ── Main component ──────────────────────────────────────────────────────

interface LimitedDeckBuilderProps {
  view?: DraftPlayerView | null;
  mainDeck?: string[];
  landCounts?: Record<string, number>;
  onAddToDeck?: (cardName: string) => void;
  onRemoveFromDeck?: (cardName: string) => void;
  onSetLandCount?: (landName: string, count: number) => void;
  onSubmitDeck?: () => Promise<void> | void;
  showSuggestions?: boolean;
}

export function LimitedDeckBuilder({
  view: viewOverride,
  mainDeck: mainDeckOverride,
  landCounts: landCountsOverride,
  onAddToDeck,
  onRemoveFromDeck,
  onSetLandCount,
  onSubmitDeck,
  showSuggestions = true,
}: LimitedDeckBuilderProps = {}) {
  const quickView = useDraftStore((s) => s.view);
  const quickMainDeck = useDraftStore((s) => s.mainDeck);
  const quickLandCounts = useDraftStore((s) => s.landCounts);
  const quickAddToDeck = useDraftStore((s) => s.addToDeck);
  const quickRemoveFromDeck = useDraftStore((s) => s.removeFromDeck);
  const quickSetLandCount = useDraftStore((s) => s.setLandCount);
  const autoSuggestDeck = useDraftStore((s) => s.autoSuggestDeck);
  const autoSuggestLands = useDraftStore((s) => s.autoSuggestLands);
  const quickSubmitDeck = useDraftStore((s) => s.submitDeck);

  const view = viewOverride !== undefined ? viewOverride : quickView;
  const mainDeck = mainDeckOverride ?? quickMainDeck;
  const landCounts = landCountsOverride ?? quickLandCounts;
  const addToDeck = onAddToDeck ?? quickAddToDeck;
  const removeFromDeck = onRemoveFromDeck ?? quickRemoveFromDeck;
  const setLandCount = onSetLandCount ?? quickSetLandCount;
  const submitDeck = onSubmitDeck ?? quickSubmitDeck;

  const pool = useMemo(() => view?.pool ?? [], [view?.pool]);

  const remainingPool = useMemo(
    () => computeRemainingPool(pool, mainDeck),
    [pool, mainDeck],
  );

  const deckGroups = useMemo(
    () => groupByName(pool, mainDeck),
    [pool, mainDeck],
  );

  const totalLands = useMemo(
    () => Object.values(landCounts).reduce((sum, n) => sum + n, 0),
    [landCounts],
  );

  const totalCards = mainDeck.length + totalLands;
  const minDeckSize = view?.min_deck_size ?? 40;
  const addableCards = view?.addable_cards?.length
    ? view.addable_cards
    : BASIC_LANDS.map((land) => land.name);
  const deckValid = totalCards >= minDeckSize;

  if (!view) return null;

  return (
    <div className="flex h-full flex-col gap-4">
      <DeckStatus spells={mainDeck.length} lands={totalLands} min={minDeckSize} />

      <div className="flex min-h-0 flex-1 gap-6">
        {/* Left column: Pool + Main Deck */}
        <div className="flex min-w-0 flex-[7] flex-col gap-6 overflow-y-auto">
          {/* Pool section */}
          <section>
            <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              Pool ({remainingPool.length})
            </h3>
            <div className="grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-7">
              <AnimatePresence mode="popLayout" initial={false}>
                {remainingPool.map((card) => (
                  <motion.div key={card.instance_id} {...CARD_MOTION}>
                    <CardTile card={card} dimmed onClick={() => addToDeck(card.name)} />
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
            {remainingPool.length === 0 && (
              <p className="py-4 text-sm text-white/30">All cards added to deck.</p>
            )}
          </section>

          {/* Main deck section */}
          <section>
            <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              Main Deck
            </h3>
            <div className="grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-7">
              <AnimatePresence mode="popLayout" initial={false}>
                {deckGroups.map(({ card, count }) => (
                  <motion.div key={card.instance_id} {...CARD_MOTION}>
                    <CardTile card={card} count={count} onClick={() => removeFromDeck(card.name)} />
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
            {mainDeck.length === 0 && (
              <p className="py-4 text-sm text-white/30">
                Click cards from the pool to add them to your deck.
              </p>
            )}
          </section>
        </div>

        {/* Right column: Lands, Mana Curve, Actions */}
        <div className="flex min-w-[220px] flex-[3] flex-col gap-6 overflow-y-auto">
          {/* Land counts */}
          <section>
            <div className="mb-3 flex items-center justify-between">
              <h3 className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
                Addable Cards
              </h3>
              {showSuggestions && (
                <button
                  type="button"
                  onClick={autoSuggestLands}
                  className={menuButtonClass({ tone: "neutral", size: "xs", ghost: true })}
                >
                  Auto Lands
                </button>
              )}
            </div>
            <div className="flex flex-col gap-2">
              {addableCards.map((name) => (
                <LandRow
                  key={name}
                  name={name}
                  colorClass={LAND_COLOR_CLASSES[name] ?? "bg-cyan-300"}
                  count={landCounts[name] ?? 0}
                  onDecrement={() => setLandCount(name, (landCounts[name] ?? 0) - 1)}
                  onIncrement={() => setLandCount(name, (landCounts[name] ?? 0) + 1)}
                />
              ))}
            </div>
          </section>

          {/* Mana curve */}
          <section>
            <ManaCurve cards={mainDeck} />
          </section>

          {/* Actions */}
          <section className="flex flex-col gap-3">
            {showSuggestions && (
              <button
                type="button"
                onClick={autoSuggestDeck}
                className={menuButtonClass({ tone: "neutral", size: "sm", className: "w-full" })}
              >
                Suggest Deck
              </button>
            )}

            <button
              type="button"
              onClick={submitDeck}
              disabled={!deckValid}
              className={menuButtonClass({
                tone: "emerald",
                size: "md",
                disabled: !deckValid,
                className: "w-full",
              })}
            >
              Submit Deck
            </button>
          </section>
        </div>
      </div>
    </div>
  );
}

// ── Deck status bar ─────────────────────────────────────────────────────

function DeckStatus({ spells, lands, min }: { spells: number; lands: number; min: number }) {
  const total = spells + lands;
  const valid = total >= min;
  const remaining = Math.max(0, min - total);
  const pct = Math.min(100, (total / min) * 100);

  return (
    <div className="rounded-[16px] border border-white/10 bg-black/18 px-4 py-3 backdrop-blur-md">
      <div className="flex items-baseline justify-between">
        <span className="text-sm font-medium text-white">
          {total} <span className="text-white/40">/ {min} cards</span>
        </span>
        <span className="text-xs text-white/45">
          {spells} spell{spells === 1 ? "" : "s"} · {lands} land{lands === 1 ? "" : "s"}
          {valid ? (
            <span className="ml-2 font-medium text-emerald-300">ready to submit</span>
          ) : (
            <span className="ml-2 text-white/55">{remaining} more needed</span>
          )}
        </span>
      </div>
      <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-white/8">
        <div
          className={`h-full rounded-full transition-all duration-300 ${valid ? "bg-emerald-400/80" : "bg-white/30"}`}
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}
