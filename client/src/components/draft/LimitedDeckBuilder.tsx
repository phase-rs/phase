import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";

import { useCardImage } from "../../hooks/useCardImage";
import { useLongPress } from "../../hooks/useLongPress";
import { useDraftStore } from "../../stores/draftStore";
import { menuButtonClass } from "../menu/buttonStyles";
import type {
  DraftCardInstance,
  DraftPlayerView,
  DraftPoolGroupKind,
} from "../../adapter/draft-adapter";
import { EMPTY_DRAFT_POOL_GROUPS } from "../../adapter/draft-adapter";
import {
  axisKinds,
  EMPTY_POOL_FILTER,
  fetchPoolFilterOptions,
  filterPoolListing,
  poolFilterActive,
  toggleKind,
} from "../../viewmodel/limitedPoolFilter";
import type { PoolFilter, PoolFilterOptions } from "../../adapter/draft-adapter";
import { POOL_GROUP_LABEL_KEYS } from "./poolGroupLabels";
import type { CardHoverInfo } from "../card/CardPreview";
import { HoverCardPreview } from "../card/HoverCardPreview";
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
  onHover: (info: CardHoverInfo | null) => void;
}

function CardTile({ card, count, dimmed, onClick, onHover }: CardTileProps) {
  const { src, isLoading } = useCardImage(card.name, {
    size: "normal",
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  });
  const hoverInfo = {
    name: card.name,
    sourcePrinting: { setCode: card.set_code, collectorNumber: card.collector_number },
  };
  const { handlers, firedRef } = useLongPress(() => onHover(hoverInfo));

  const handleClick = () => {
    if (firedRef.current) {
      firedRef.current = false;
      return;
    }
    onClick();
  };

  return (
    <button
      onClick={handleClick}
      onMouseEnter={() => onHover(hoverInfo)}
      onMouseLeave={() => onHover(null)}
      {...handlers}
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
  const { t } = useTranslation("draft");
  return (
    <div className="flex items-center gap-2">
      <div className={`h-3 w-3 shrink-0 rounded-full ${colorClass}`} />
      <span className="flex-1 text-sm text-white/60">{name}</span>
      <button
        type="button"
        onClick={onDecrement}
        disabled={count <= 0}
        aria-label={t("limitedDeck.removeCard", { name })}
        className={menuButtonClass({ tone: "neutral", size: "icon", disabled: count <= 0, className: "font-bold" })}
      >
        -
      </button>
      <span className="w-6 text-center text-sm tabular-nums text-white">{count}</span>
      <button
        type="button"
        onClick={onIncrement}
        aria-label={t("limitedDeck.addCard", { name })}
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
  submissionError?: string | null;
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
  submissionError = null,
  showSuggestions = true,
}: LimitedDeckBuilderProps = {}) {
  const { t } = useTranslation("draft");
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

  const [hoveredCard, setHoveredCard] = useState<CardHoverInfo | null>(null);
  const [addableQuery, setAddableQuery] = useState("");
  const [poolFilter, setPoolFilter] = useState<PoolFilter>(EMPTY_POOL_FILTER);
  const [keptInstanceIds, setKeptInstanceIds] = useState<string[] | null>(null);
  const [poolFilterFailed, setPoolFilterFailed] = useState(false);
  const [legacyFilterOptions, setLegacyFilterOptions] =
    useState<PoolFilterOptions | null>(null);
  const [localSubmissionError, setLocalSubmissionError] = useState<string | null>(null);

  const pool = useMemo(() => view?.pool ?? [], [view?.pool]);

  const remainingPool = useMemo(
    () => computeRemainingPool(pool, mainDeck),
    [pool, mainDeck],
  );

  // #7507 + #7546 review: the ENGINE is the single filtering authority. The
  // display sends the listing, the engine-delivered groups and the typed
  // filter; it renders exactly the returned instance ids. React holds only
  // the presentation state (which chips are pressed, the query text).
  const poolGroups = view?.pool_groups ?? EMPTY_DRAFT_POOL_GROUPS;
  useEffect(() => {
    if (!poolFilterActive(poolFilter)) {
      setKeptInstanceIds(null);
      setPoolFilterFailed(false);
      return;
    }
    let stale = false;
    filterPoolListing(remainingPool, poolFilter)
      .then((ids) => {
        if (stale) return;
        setKeptInstanceIds(ids);
        setPoolFilterFailed(false);
      })
      .catch(() => {
        if (stale) return;
        // Engine unavailable: show the unfiltered listing rather than an
        // empty grid — the display must not interpret the data itself — and
        // SAY so, so the grid cannot silently contradict the active controls
        // (review round 3).
        setKeptInstanceIds(null);
        setPoolFilterFailed(true);
      });
    return () => {
      stale = true;
    };
  }, [remainingPool, poolFilter]);
  // Review round 5: a view that predates the option fields (legacy) gets its
  // control options from the ENGINE's stateless path — never from the lossy
  // exclusive presentation buckets, and never reconstructed here. A v11 view
  // with a non-empty pool always carries non-empty option lists
  // (classification is total), so empty lists + a non-empty pool identify
  // the legacy shape. While the call is pending or failed, the axes stay
  // hidden rather than mis-offered.
  const isLegacyView =
    pool.length > 0 &&
    poolGroups.type_filter_options.length === 0 &&
    poolGroups.color_filter_options.length === 0;
  useEffect(() => {
    if (!isLegacyView) {
      setLegacyFilterOptions(null);
      return;
    }
    // Do not display the prior legacy pool's engine-owned options while this
    // pool's stateless request is pending.
    setLegacyFilterOptions(null);
    let stale = false;
    fetchPoolFilterOptions(pool)
      .then((options) => {
        if (!stale) setLegacyFilterOptions(options);
      })
      .catch(() => {
        if (!stale) setLegacyFilterOptions(null);
      });
    return () => {
      stale = true;
    };
  }, [isLegacyView, pool]);
  const typeChipKinds = isLegacyView
    ? (legacyFilterOptions?.types ?? [])
    : poolGroups.type_filter_options;
  const colorChipKinds = isLegacyView
    ? (legacyFilterOptions?.colors ?? [])
    : poolGroups.color_filter_options;
  const rarityChipKinds = isLegacyView
    ? (legacyFilterOptions?.rarities ?? [])
    : axisKinds(poolGroups.rarity_groups);

  const displayedPool = useMemo(() => {
    if (keptInstanceIds === null) return remainingPool;
    const kept = new Set(keptInstanceIds);
    return remainingPool.filter((card) => kept.has(card.instance_id));
  }, [remainingPool, keptInstanceIds]);

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
  const addableCards = view?.addable_cards ?? BASIC_LANDS.map((land) => land.name);
  const filteredAddableCards = useMemo(() => {
    const query = addableQuery.trim().toLowerCase();
    return query
      ? addableCards.filter((name) => name.toLowerCase().includes(query))
      : addableCards;
  }, [addableCards, addableQuery]);
  const deckValid = totalCards >= minDeckSize;
  const displayedSubmissionError = submissionError ?? localSubmissionError;

  useEffect(() => {
    setLocalSubmissionError(null);
  }, [mainDeck, landCounts]);

  const handleSubmit = async () => {
    setLocalSubmissionError(null);
    try {
      await submitDeck();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setLocalSubmissionError(message || t("limitedDeck.submitFailed"));
    }
  };

  if (!view) return null;

  return (
    <div className="flex h-full flex-col gap-4">
      <HoverCardPreview
        card={hoveredCard}
        mobileLayout="compact"
        onDismiss={() => setHoveredCard(null)}
      />
      <DeckStatus spells={mainDeck.length} lands={totalLands} min={minDeckSize} />

      <div className="flex min-h-0 flex-1 gap-6">
        {/* Left column: Pool + Main Deck */}
        <div className="flex min-w-0 flex-[7] flex-col gap-6 overflow-y-auto">
          {/* Pool section */}
          <section>
            <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              {t("limitedDeck.poolHeading", { count: remainingPool.length })}
            </h3>
            <div className="mb-3 flex flex-col gap-2">
              <input
                type="search"
                value={poolFilter.query}
                onChange={(event) =>
                  setPoolFilter((prev) => ({ ...prev, query: event.target.value }))
                }
                placeholder={t("limitedDeck.searchPool")}
                aria-label={t("limitedDeck.searchPool")}
                className="w-full rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-emerald-400/50"
              />
              <PoolFilterChips
                kinds={typeChipKinds}
                selected={poolFilter.types}
                onToggle={(kind) =>
                  setPoolFilter((prev) => ({ ...prev, types: toggleKind(prev.types, kind) }))
                }
              />
              <PoolFilterChips
                kinds={colorChipKinds}
                selected={poolFilter.colors}
                onToggle={(kind) =>
                  setPoolFilter((prev) => ({ ...prev, colors: toggleKind(prev.colors, kind) }))
                }
              />
              <PoolFilterChips
                kinds={rarityChipKinds}
                selected={poolFilter.rarities}
                onToggle={(kind) =>
                  setPoolFilter((prev) => ({
                    ...prev,
                    rarities: toggleKind(prev.rarities, kind),
                  }))
                }
              />
              {poolFilterFailed && (
                <p role="alert" className="text-xs text-amber-300/80">
                  {t("limitedDeck.filterUnavailable")}
                </p>
              )}
            </div>
            <div className="grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-7">
              <AnimatePresence mode="popLayout" initial={false}>
                {displayedPool.map((card) => (
                  <motion.div key={card.instance_id} {...CARD_MOTION}>
                    <CardTile
                      card={card}
                      dimmed
                      onClick={() => addToDeck(card.name)}
                      onHover={setHoveredCard}
                    />
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
            {remainingPool.length === 0 ? (
              <p className="py-4 text-sm text-white/30">{t("limitedDeck.allAdded")}</p>
            ) : (
              displayedPool.length === 0 &&
              poolFilterActive(poolFilter) && (
                <p className="py-4 text-sm text-white/30">
                  {t("limitedDeck.noFilterMatches")}
                </p>
              )
            )}
          </section>

          {/* Main deck section */}
          <section>
            <h3 className="mb-3 text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
              {t("limitedDeck.mainDeck")}
            </h3>
            <div className="grid grid-cols-3 gap-2 sm:grid-cols-4 md:grid-cols-5 lg:grid-cols-6 xl:grid-cols-7">
              <AnimatePresence mode="popLayout" initial={false}>
                {deckGroups.map(({ card, count }) => (
                  <motion.div key={card.instance_id} {...CARD_MOTION}>
                    <CardTile
                      card={card}
                      count={count}
                      onClick={() => removeFromDeck(card.name)}
                      onHover={setHoveredCard}
                    />
                  </motion.div>
                ))}
              </AnimatePresence>
            </div>
            {mainDeck.length === 0 && (
              <p className="py-4 text-sm text-white/30">
                {t("limitedDeck.emptyDeckHint")}
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
                {t("limitedDeck.addableCards")}
              </h3>
              {showSuggestions && (
                <button
                  type="button"
                  onClick={autoSuggestLands}
                  className={menuButtonClass({ tone: "neutral", size: "xs", ghost: true })}
                >
                  {t("limitedDeck.autoLands")}
                </button>
              )}
            </div>
            <input
              type="search"
              value={addableQuery}
              onChange={(event) => setAddableQuery(event.target.value)}
              placeholder={t("limitedDeck.searchAddableCards")}
              aria-label={t("limitedDeck.searchAddableCards")}
              className="mb-3 w-full rounded-lg border border-white/10 bg-black/30 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-emerald-400/50"
            />
            <div className="flex flex-col gap-2">
              {filteredAddableCards.map((name) => (
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
            <ManaCurve pool={pool} cards={mainDeck} />
          </section>

          {/* Actions */}
          <section className="flex flex-col gap-3">
            {showSuggestions && (
              <button
                type="button"
                onClick={autoSuggestDeck}
                className={menuButtonClass({ tone: "neutral", size: "sm", className: "w-full" })}
              >
                {t("limitedDeck.suggestDeck")}
              </button>
            )}

            <button
              type="button"
              onClick={() => void handleSubmit()}
              disabled={!deckValid}
              className={menuButtonClass({
                tone: "emerald",
                size: "md",
                disabled: !deckValid,
                className: "w-full",
              })}
            >
              {t("limitedDeck.submitDeck")}
            </button>
            {displayedSubmissionError && (
              <p
                role="alert"
                className="rounded-lg border border-red-400/40 bg-red-500/10 px-3 py-2 text-sm text-red-100"
              >
                <span className="font-medium">{t("limitedDeck.validationTitle")}: </span>
                {displayedSubmissionError}
              </p>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}

// ── Deck status bar ─────────────────────────────────────────────────────

function DeckStatus({ spells, lands, min }: { spells: number; lands: number; min: number }) {
  const { t } = useTranslation("draft");
  const total = spells + lands;
  const valid = total >= min;
  const remaining = Math.max(0, min - total);
  const pct = Math.min(100, (total / min) * 100);

  return (
    <div className="rounded-[16px] border border-white/10 bg-black/18 px-4 py-3 backdrop-blur-md">
      <div className="flex items-baseline justify-between">
        <span className="text-sm font-medium text-white">
          {total} <span className="text-white/40">{t("limitedDeck.cardCount", { min })}</span>
        </span>
        <span className="text-xs text-white/45">
          {t("limitedDeck.spellCount", { count: spells })} · {t("limitedDeck.landCount", { count: lands })}
          {valid ? (
            <span className="ml-2 font-medium text-emerald-300">{t("limitedDeck.readyToSubmit")}</span>
          ) : (
            <span className="ml-2 text-white/55">{t("limitedDeck.moreNeeded", { count: remaining })}</span>
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

// ── Pool filter chips (#7507) ───────────────────────────────────────────

/**
 * One filter axis as toggle chips. The offered kinds are exactly the groups
 * the engine delivered for this pool (`axisKinds`), in engine order; labels
 * come from the shared engine-kind label map. An axis with fewer than two
 * groups offers no narrowing and renders nothing.
 */
function PoolFilterChips({
  kinds,
  selected,
  onToggle,
}: {
  kinds: DraftPoolGroupKind[];
  selected: DraftPoolGroupKind[];
  onToggle: (kind: DraftPoolGroupKind) => void;
}) {
  const { t } = useTranslation("draft");
  if (kinds.length < 2) return null;
  return (
    <div className="flex flex-wrap gap-1">
      {kinds.map((kind) => (
        <button
          key={kind}
          type="button"
          aria-pressed={selected.includes(kind)}
          onClick={() => onToggle(kind)}
          // 44pt coarse-pointer floor in BOTH dimensions (index.css),
          // relaxed only for fine pointers — a wide touch device keeps the
          // full target (review round 4).
          className={`min-h-[44px] min-w-[44px] rounded-full px-3 py-1 text-xs transition-colors pointer-fine:min-h-0 pointer-fine:min-w-0 pointer-fine:px-2.5 ${
            selected.includes(kind)
              ? "bg-white/10 text-white"
              : "text-white/40 hover:bg-white/5 hover:text-white/70"
          }`}
        >
          {t(POOL_GROUP_LABEL_KEYS[kind])}
        </button>
      ))}
    </div>
  );
}
