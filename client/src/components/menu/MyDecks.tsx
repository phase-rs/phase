import type { ReactNode } from "react";
import { useEffect, useMemo, useState } from "react";

import type { GameFormat, MatchType } from "../../adapter/types";
import type { FeedDeck } from "../../types/feed";
import { ACTIVE_DECK_KEY, listSavedDeckNames, getDeckMeta, deleteDeck } from "../../constants/storage";
import {
  getDeckFeedOrigin,
  listSubscriptions,
  refreshAllFeeds,
  adoptFeedDeck,
  feedDeckToParsedDeck,
} from "../../services/feedService";
import {
  useCachedFeed,
  useFeedCacheSnapshot,
} from "../../services/feedPersistence";
import { FeedManagerModal } from "./FeedManagerModal";
import { useCardImage } from "../../hooks/useCardImage";
import type { ParsedDeck } from "../../services/deckParser";
import {
  evaluateDeckCompatibilityBatch,
  type DeckCompatibilityResult,
} from "../../services/deckCompatibility";
import { ImportDeckModal } from "./ImportDeckModal";
import { MenuPanel } from "./MenuShell";
import { menuButtonClass } from "./buttonStyles";
import {
  COLOR_DOT_CLASS,
  getDeckCardCount,
  getDeckColorIdentity,
  getRepresentativeCard,
  isBundledDeck,
  loadDeck,
} from "./deckHelpers";

const BASIC_LANDS = new Set(["Plains", "Island", "Swamp", "Mountain", "Forest"]);
const PRECON_PREFIX = "[Pre-built] ";

/** Tags that represent a format/archetype — shown with active (green) styling. */
const FORMAT_TAGS = new Set(["standard", "modern", "pioneer", "commander", "legacy", "vintage", "pauper", "historic", "brawl", "metagame"]);

type DeckFilter = "all" | "standard" | "pioneer" | "modern" | "legacy" | "vintage" | "pauper" | "commander" | "historic" | "brawl" | "bo3";
type DeckSort = "alpha" | "recent" | "format";

/** Ordered list of format filters shown in the filter bar. */
const FORMAT_FILTERS: Array<{ key: DeckFilter; label: string; aetherhubUrl?: string }> = [
  { key: "all", label: "All" },
  { key: "standard", label: "Standard" },
  { key: "pioneer", label: "Pioneer" },
  { key: "modern", label: "Modern" },
  { key: "legacy", label: "Legacy" },
  { key: "vintage", label: "Vintage" },
  { key: "pauper", label: "Pauper" },
  { key: "commander", label: "Commander" },
  { key: "historic", label: "Historic", aetherhubUrl: "https://aetherhub.com/Metagame/Historic" },
  { key: "brawl", label: "Brawl", aetherhubUrl: "https://aetherhub.com/Metagame/Brawl" },
  { key: "bo3", label: "BO3" },
];

/** Formats that use `format_legality` for filtering (all except standard/commander/bo3 which have dedicated checks). */
const LEGALITY_BASED_FORMATS = new Set<DeckFilter>(["pioneer", "modern", "legacy", "vintage", "pauper", "historic", "brawl"]);

function DeckArtTile({ cardName }: { cardName: string | null }) {
  const { src, isLoading } = useCardImage(cardName ?? "", { size: "art_crop" });

  if (!cardName || isLoading || !src) {
    return <div className="absolute inset-0 animate-pulse bg-gray-800" />;
  }

  return <img src={src} alt="" className="absolute inset-0 h-full w-full object-cover" />;
}

export function StatusBadge({ label, active }: { label: string; active: boolean }) {
  return (
    <span
      className={`rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider ${
        active ? "bg-emerald-500/80 text-black" : "bg-gray-700/80 text-gray-200"
      }`}
    >
      {label}
    </span>
  );
}

interface DeckTileProps {
  deckName: string;
  isActive: boolean;
  compatibility: DeckCompatibilityResult | undefined;
  onClick: () => void;
  onDelete?: () => void;
  onAdopt?: () => void;
  /** When true, suppress the feed badge (used in subscription view where the header already identifies the feed). */
  hideFeedBadge?: boolean;
  /** Provide feed deck data directly so the tile doesn't depend on localStorage. */
  feedDeckOverride?: FeedDeck;
}

function DeckTile({ deckName, isActive, compatibility, onClick, onDelete, onAdopt, hideFeedBadge, feedDeckOverride }: DeckTileProps) {
  const [coverageHovered, setCoverageHovered] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  useEffect(() => {
    if (!confirmingDelete) return;
    const timer = setTimeout(() => setConfirmingDelete(false), 3000);
    return () => clearTimeout(timer);
  }, [confirmingDelete]);
  const colors = compatibility?.color_identity
    ?? feedDeckOverride?.colors
    ?? getDeckColorIdentity(deckName);
  const count = feedDeckOverride
    ? feedDeckOverride.main.reduce((sum, e) => sum + e.count, 0)
    : getDeckCardCount(deckName);
  const representativeCard = feedDeckOverride
    ? (feedDeckOverride.commander?.[0] ?? feedDeckOverride.main.find((e) => !BASIC_LANDS.has(e.name))?.name ?? null)
    : getRepresentativeCard(deckName);
  const feedOrigin = getDeckFeedOrigin(deckName);
  const feedForBadge = useCachedFeed(feedOrigin ?? "");
  const feedBadge = !hideFeedBadge && feedOrigin ? (feedForBadge?.name ?? "Feed") : null;
  const isPrecon = deckName.startsWith(PRECON_PREFIX);
  const displayName = isPrecon ? deckName.slice(PRECON_PREFIX.length) : deckName;

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onClick}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onClick(); } }}
      className={`group relative flex aspect-[4/3] cursor-pointer flex-col justify-end overflow-hidden rounded-xl text-left transition ${
        isActive
          ? "ring-2 ring-white/30 ring-offset-2 ring-offset-[#060a16]"
          : "ring-1 ring-white/10 hover:ring-white/20"
      }`}
    >
      <DeckArtTile cardName={representativeCard} />

      {feedBadge && (
        <span className="absolute right-2 top-2 z-10 rounded-full bg-amber-500/80 px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-black">
          {feedBadge}
        </span>
      )}

      {onDelete && (
        confirmingDelete ? (
          <div className="absolute left-2 top-2 z-20 flex gap-1">
            <button
              onClick={(e) => { e.stopPropagation(); onDelete(); setConfirmingDelete(false); }}
              className="rounded-full bg-red-600 px-2 py-0.5 text-[10px] font-semibold text-white transition-colors hover:bg-red-500"
            >
              Delete
            </button>
            <button
              onClick={(e) => { e.stopPropagation(); setConfirmingDelete(false); }}
              className="rounded-full bg-black/70 px-2 py-0.5 text-[10px] font-medium text-gray-300 transition-colors hover:bg-black/90"
            >
              Cancel
            </button>
          </div>
        ) : (
          <button
            onClick={(e) => { e.stopPropagation(); setConfirmingDelete(true); }}
            className="absolute left-2 top-2 z-20 flex h-6 w-6 items-center justify-center rounded-full bg-black/70 text-gray-400 opacity-0 transition-opacity hover:bg-red-600 hover:text-white group-hover:opacity-100"
            title="Delete deck"
          >
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
              <path fillRule="evenodd" d="M5 3.25V4H2.75a.75.75 0 0 0 0 1.5h.3l.815 8.15A1.5 1.5 0 0 0 5.357 15h5.285a1.5 1.5 0 0 0 1.493-1.35l.815-8.15h.3a.75.75 0 0 0 0-1.5H11v-.75A2.25 2.25 0 0 0 8.75 1h-1.5A2.25 2.25 0 0 0 5 3.25Zm2.25-.75a.75.75 0 0 0-.75.75V4h3v-.75a.75.75 0 0 0-.75-.75h-1.5ZM6.05 6a.75.75 0 0 1 .787.713l.275 5.5a.75.75 0 0 1-1.498.075l-.275-5.5A.75.75 0 0 1 6.05 6Zm3.9 0a.75.75 0 0 1 .712.787l-.275 5.5a.75.75 0 0 1-1.498-.075l.275-5.5A.75.75 0 0 1 9.95 6Z" clipRule="evenodd" />
            </svg>
          </button>
        )
      )}

      {onAdopt && (
        <button
          onClick={(e) => { e.stopPropagation(); onAdopt(); }}
          className="absolute left-2 top-2 z-20 rounded bg-black/70 px-2 py-1 text-[10px] font-medium text-white opacity-0 transition-opacity hover:bg-black/90 group-hover:opacity-100"
          title="Copy to My Decks (removes feed tracking)"
        >
          Copy to My Decks
        </button>
      )}

      <div className="relative z-10 bg-gradient-to-t from-black/95 via-black/70 to-transparent px-3 pb-3 pt-8">
        <p className="truncate text-sm font-semibold text-white">{displayName}</p>
        <div className="mt-1 flex items-center gap-2">
          <div className="flex gap-1">
            {colors.map((color) => (
              <span
                key={color}
                className={`inline-block h-2.5 w-2.5 rounded-full ${COLOR_DOT_CLASS[color] ?? "bg-gray-400"}`}
              />
            ))}
            {colors.length === 0 && (
              <span className="inline-block h-2.5 w-2.5 rounded-full bg-gray-500" />
            )}
          </div>
          <span className="text-xs text-gray-300">{count} cards</span>
        </div>
        <div className="mt-2 flex flex-wrap gap-1">
          {/* Feed format/archetype tags */}
          {feedDeckOverride?.tags?.map((tag) => (
            <StatusBadge key={tag} label={tag} active={FORMAT_TAGS.has(tag)} />
          ))}
          {isPrecon && !feedDeckOverride?.tags?.length && (
            <StatusBadge label="precon" active />
          )}
          {/* Engine compatibility badges */}
          {compatibility?.standard.compatible && <StatusBadge label="STD" active />}
          {compatibility?.commander.compatible && <StatusBadge label="CMD" active />}
          {compatibility?.bo3_ready && <StatusBadge label="BO3" active />}
          {compatibility && compatibility.unknown_cards.length > 0 && (
            <span
              className="rounded bg-amber-500/80 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-black"
              title={`Unknown cards:\n${compatibility.unknown_cards.join("\n")}`}
            >
              Unknown {compatibility.unknown_cards.length}
            </span>
          )}
          {compatibility?.coverage && (() => {
            const { supported_unique, total_unique, unsupported_cards } = compatibility.coverage;
            if (supported_unique === total_unique) return null;
            const pct = total_unique > 0 ? (supported_unique / total_unique) * 100 : 0;
            const barColor =
              pct >= 75 ? "bg-lime-500"
              : pct >= 50 ? "bg-amber-500"
              : "bg-red-500";
            const totalCopiesAffected = unsupported_cards.reduce((sum, c) => sum + (c.copies ?? 1), 0);
            return (
              <div
                className="flex w-full items-center gap-1.5"
                title={`Unsupported (${unsupported_cards.length} unique, ${totalCopiesAffected} copies):\n${unsupported_cards.map((c) => `${(c.copies ?? 1) > 1 ? `${c.copies}x ` : ""}${c.name}: ${c.gaps.join(", ")}`).join("\n")}`}
                onMouseEnter={() => setCoverageHovered(true)}
                onMouseLeave={() => setCoverageHovered(false)}
              >
                <div className="h-1 flex-1 overflow-hidden rounded-full bg-white/10">
                  <div
                    className={`h-full rounded-full ${barColor}`}
                    style={{ width: `${pct}%` }}
                  />
                </div>
                <span className="shrink-0 text-right text-[10px] tabular-nums text-gray-400" style={{ minWidth: `${String(total_unique).length * 2 + 1}ch` }}>
                  {coverageHovered ? `${Math.round(pct)}%` : `${supported_unique}/${total_unique}`}
                </span>
              </div>
            );
          })()}
        </div>
      </div>
    </div>
  );
}

interface MyDecksProps {
  mode: "manage" | "select";
  selectedFormat?: GameFormat;
  selectedMatchType?: MatchType;
  activeDeckName?: string | null;
  onSelectDeck?: (deckName: string) => void;
  onConfirmSelection?: () => void;
  confirmLabel?: string;
  confirmAction?: ReactNode;
  onCreateDeck?: () => void;
  onEditDeck?: (deckName: string) => void;
  /** When true, render without the MenuPanel wrapper and header (for embedding). */
  bare?: boolean;
  /** Called whenever compatibility data is updated, so the parent can use it. */
  onCompatibilityUpdate?: (data: Record<string, DeckCompatibilityResult>) => void;
}

type MyDecksTab = "decks" | "subscriptions";

export function MyDecks({
  mode,
  selectedFormat,
  selectedMatchType,
  activeDeckName = null,
  onSelectDeck,
  onConfirmSelection,
  confirmLabel = "Continue",
  confirmAction,
  onCreateDeck,
  onEditDeck,
  bare = false,
  onCompatibilityUpdate,
}: MyDecksProps) {
  const [activeTab, setActiveTab] = useState<MyDecksTab>("decks");
  const [deckNames, setDeckNames] = useState<string[]>([]);
  const [showImport, setShowImport] = useState(false);
  const [showFeedManager, setShowFeedManager] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [compatibilities, setCompatibilities] = useState<Record<string, DeckCompatibilityResult>>({});
  const [isEvaluating, setIsEvaluating] = useState(false);
  const [compatibilityError, setCompatibilityError] = useState<string | null>(null);
  const feedCache = useFeedCacheSnapshot();

  const contextualFilter = useMemo<DeckFilter | null>(() => {
    if (!selectedFormat) return null;
    const map: Partial<Record<GameFormat, DeckFilter>> = {
      Standard: "standard",
      Commander: "commander",
      Pioneer: "pioneer",
      Historic: "historic",
      Pauper: "pauper",
      Brawl: "brawl",
      HistoricBrawl: "brawl",
    };
    return map[selectedFormat] ?? null;
  }, [selectedFormat]);
  const [activeFilter, setActiveFilter] = useState<DeckFilter>(contextualFilter ?? "all");
  const [activeSort, setActiveSort] = useState<DeckSort>(
    mode === "select" ? (selectedFormat ? "format" : "recent") : "alpha",
  );
  const [sortAsc, setSortAsc] = useState(mode !== "select");
  const [searchQuery, setSearchQuery] = useState("");

  useEffect(() => {
    setActiveFilter(contextualFilter ?? "all");
  }, [contextualFilter]);

  useEffect(() => {
    setDeckNames(listSavedDeckNames());
  }, [selectedFormat]);

  useEffect(() => {
    if (mode !== "select") return;
    if (!onSelectDeck) return;
    if (activeDeckName != null) return;
    const stored = localStorage.getItem(ACTIVE_DECK_KEY);
    if (!stored || !deckNames.includes(stored)) return;
    onSelectDeck(stored);
  }, [mode, activeDeckName, deckNames, onSelectDeck]);

  useEffect(() => {
    let cancelled = false;
    async function evaluateCompat(): Promise<void> {
      // Collect localStorage decks
      const loadedDecks: Array<{ name: string; deck: ParsedDeck }> = [];
      const seen = new Set<string>();
      for (const name of deckNames) {
        const deck = loadDeck(name);
        if (deck) {
          loadedDecks.push({ name, deck });
          seen.add(name);
        }
      }

      // Collect feed decks not already in localStorage
      for (const sub of listSubscriptions()) {
        const feed = feedCache[sub.sourceId];
        if (!feed) continue;
        for (const feedDeck of feed.decks) {
          if (!seen.has(feedDeck.name)) {
            loadedDecks.push({ name: feedDeck.name, deck: feedDeckToParsedDeck(feedDeck) });
            seen.add(feedDeck.name);
          }
        }
      }

      if (loadedDecks.length === 0) {
        if (!cancelled) {
          setCompatibilities({});
          setCompatibilityError(null);
          setIsEvaluating(false);
        }
        return;
      }

      try {
        setIsEvaluating(true);
        const results = await evaluateDeckCompatibilityBatch(loadedDecks, {
          selectedFormat,
          selectedMatchType,
        });
        if (!cancelled) {
          setCompatibilities(results);
          setCompatibilityError(null);
          onCompatibilityUpdate?.(results);
        }
      } catch (error) {
        if (!cancelled) {
          setCompatibilityError(error instanceof Error ? error.message : String(error));
          setCompatibilities({});
        }
      } finally {
        if (!cancelled) {
          setIsEvaluating(false);
        }
      }
    }

    evaluateCompat();
    return () => {
      cancelled = true;
    };
  }, [deckNames, selectedFormat, selectedMatchType, onCompatibilityUpdate, feedCache]);

  const filteredDeckNames = useMemo(() => {
    return deckNames.filter((deckName) => {
      const compatibility = compatibilities[deckName];
      if (!compatibility) return true;

      const selectedFormatCompatible = compatibility.selected_format_compatible;
      if (contextualFilter && activeFilter === contextualFilter && selectedFormatCompatible != null) {
        return selectedFormatCompatible;
      }

      // Formats without a contextual filter mapping (e.g. FreeForAll, TwoHeadedGiant):
      // use the engine's selected_format_compatible when activeFilter is "all".
      if (selectedFormat && !contextualFilter && activeFilter === "all" && selectedFormatCompatible != null) {
        return selectedFormatCompatible;
      }

      if (activeFilter === "standard") return compatibility.standard.compatible;
      if (activeFilter === "commander") return compatibility.commander.compatible;
      if (activeFilter === "bo3") return compatibility.bo3_ready;
      if (LEGALITY_BASED_FORMATS.has(activeFilter)) {
        return compatibility.format_legality?.[activeFilter] === "legal";
      }
      return true;
    });
  }, [deckNames, compatibilities, activeFilter, contextualFilter, selectedFormat]);

  const searchFiltered = useMemo(() => {
    if (!searchQuery) return filteredDeckNames;
    const q = searchQuery.toLowerCase();
    return filteredDeckNames.filter((name) => name.toLowerCase().includes(q));
  }, [filteredDeckNames, searchQuery]);

  const { userDecks, bundledDecks } = useMemo(() => {
    const dir = sortAsc ? 1 : -1;
    const sortNames = (names: string[]): string[] => {
      if (activeSort === "alpha") return [...names].sort((a, b) => a.localeCompare(b) * dir);
      if (activeSort === "format") {
        return [...names].sort((a, b) => {
          const compatA = compatibilities[a]?.selected_format_compatible;
          const compatB = compatibilities[b]?.selected_format_compatible;
          const scoreA = compatA === true ? 0 : compatA === false ? 2 : 1;
          const scoreB = compatB === true ? 0 : compatB === false ? 2 : 1;
          if (scoreA !== scoreB) return (scoreA - scoreB) * dir;
          return a.localeCompare(b);
        });
      }
      return [...names].sort((a, b) => {
        const metaA = getDeckMeta(a);
        const metaB = getDeckMeta(b);
        const scoreA = Math.max(metaA?.lastPlayedAt ?? 0, metaA?.addedAt ?? 0);
        const scoreB = Math.max(metaB?.lastPlayedAt ?? 0, metaB?.addedAt ?? 0);
        return (scoreA - scoreB) * dir;
      });
    };

    const user: string[] = [];
    const bundled: string[] = [];
    for (const name of searchFiltered) {
      if (isBundledDeck(name)) {
        bundled.push(name);
      } else {
        user.push(name);
      }
    }
    return { userDecks: sortNames(user), bundledDecks: sortNames(bundled) };
  }, [searchFiltered, activeSort, sortAsc, compatibilities]);

  const noDeckSelected = mode === "select"
    ? !activeDeckName || !searchFiltered.includes(activeDeckName)
    : false;
  const selectedDeckLabel = mode === "select" && activeDeckName && searchFiltered.includes(activeDeckName)
    ? activeDeckName
    : null;

  const handleTileClick = (deckName: string) => {
    if (mode === "manage") {
      onEditDeck?.(deckName);
      return;
    }
    onSelectDeck?.(deckName);
  };

  const handleImported = (name: string, names: string[]) => {
    setDeckNames(names);
    if (mode === "select") {
      onSelectDeck?.(name);
    }
  };

  const handleRefreshAll = async () => {
    setIsRefreshing(true);
    try {
      await refreshAllFeeds();
      setDeckNames(listSavedDeckNames());
    } finally {
      setIsRefreshing(false);
    }
  };

  const handleAdoptDeck = (deckName: string) => {
    const newName = prompt("Save as:", deckName);
    if (!newName) return;
    adoptFeedDeck(deckName, newName);
    setDeckNames(listSavedDeckNames());
  };

  const handleDeleteDeck = (deckName: string) => {
    deleteDeck(deckName);
    setDeckNames(listSavedDeckNames());
  };

  const handleFeedManagerClose = () => {
    setShowFeedManager(false);
    setDeckNames(listSavedDeckNames());
  };

  const Wrapper = bare ? "div" : MenuPanel;
  const wrapperClass = bare
    ? "flex w-full flex-col items-center gap-4"
    : "flex w-full max-w-5xl flex-col items-center gap-6 px-4 py-5";

  return (
    <Wrapper className={wrapperClass}>
      {!bare && (
      <div className="flex w-full items-center justify-between gap-3">
        <div className="flex items-center gap-4">
          <h2 className="menu-display text-[1.9rem] leading-tight text-white">
            {mode === "manage" ? "My Decks" : "Select Deck"}
          </h2>
          {mode === "manage" && (
            <div className="flex rounded-lg border border-white/10">
              <button
                onClick={() => setActiveTab("decks")}
                className={`rounded-l-lg px-3 py-1.5 text-xs font-medium transition-colors ${
                  activeTab === "decks"
                    ? "bg-white/10 text-white"
                    : "text-slate-400 hover:text-white"
                }`}
              >
                My Decks
              </button>
              <button
                onClick={() => setActiveTab("subscriptions")}
                className={`rounded-r-lg px-3 py-1.5 text-xs font-medium transition-colors ${
                  activeTab === "subscriptions"
                    ? "bg-white/10 text-white"
                    : "text-slate-400 hover:text-white"
                }`}
              >
                Subscriptions
              </button>
            </div>
          )}
        </div>
        {mode === "manage" && activeTab === "decks" && (
          <button
            onClick={onCreateDeck}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            Create New
          </button>
        )}
        {mode === "manage" && activeTab === "subscriptions" && (
          <div className="flex gap-2">
            <button
              onClick={handleRefreshAll}
              disabled={isRefreshing}
              className={menuButtonClass({ tone: "neutral", size: "sm", disabled: isRefreshing })}
            >
              {isRefreshing ? "Refreshing…" : "Refresh All"}
            </button>
            <button
              onClick={() => setShowFeedManager(true)}
              className={menuButtonClass({ tone: "neutral", size: "sm" })}
            >
              Manage Feeds
            </button>
          </div>
        )}
      </div>
      )}

      {(activeTab === "decks" || mode === "select") && (<>
      {/* Search + filter/sort controls */}
      <div className="flex w-full flex-wrap items-center gap-2">
        <div className="relative">
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-slate-500">
            <path fillRule="evenodd" d="M9.965 11.026a5 5 0 1 1 1.06-1.06l2.755 2.754a.75.75 0 1 1-1.06 1.06l-2.755-2.754ZM10.5 7a3.5 3.5 0 1 1-7 0 3.5 3.5 0 0 1 7 0Z" clipRule="evenodd" />
          </svg>
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search decks…"
            className="rounded-lg bg-black/30 py-1.5 pl-8 pr-3 text-xs text-slate-200 outline-none ring-1 ring-white/10 transition-colors placeholder:text-slate-500 focus:ring-white/20"
          />
        </div>

        {mode === "manage" && (<>
        {FORMAT_FILTERS.map(({ key, label, aetherhubUrl }) => (
          <span key={key} className="inline-flex items-center gap-0.5">
            <button
              onClick={() => setActiveFilter(key)}
              className={`rounded px-2 py-1 text-xs font-medium ${
                activeFilter === key
                  ? "bg-white/10 text-white"
                  : "bg-black/18 text-slate-400 hover:bg-white/8 hover:text-white"
              }`}
            >
              {label}
            </button>
            {aetherhubUrl && (
              <a
                href={aetherhubUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="rounded p-0.5 text-slate-500 transition-colors hover:bg-white/5 hover:text-slate-300"
                title={`Browse ${label} decks on Aetherhub`}
              >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3 w-3">
                  <path fillRule="evenodd" d="M4.5 2A2.5 2.5 0 0 0 2 4.5v7A2.5 2.5 0 0 0 4.5 14h7a2.5 2.5 0 0 0 2.5-2.5V9a.75.75 0 0 0-1.5 0v2.5a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1H7a.75.75 0 0 0 0-1.5H4.5ZM9 2a.75.75 0 0 0 0 1.5h2.69L8.22 7.03a.75.75 0 1 0 1.06 1.06l3.47-3.47V7a.75.75 0 0 0 1.5 0V2H9Z" clipRule="evenodd" />
                </svg>
              </a>
            )}
          </span>
        ))}
        {contextualFilter && activeFilter === contextualFilter && (
          <button
            onClick={() => setActiveFilter("all")}
            className="rounded border border-indigo-500/50 bg-indigo-500/10 px-2 py-1 text-xs font-medium text-indigo-200 hover:bg-indigo-500/20"
          >
            Show all decks
          </button>
        )}
        <div className="ml-auto flex items-center gap-1">
          <select
            value={activeSort}
            onChange={(e) => {
              const next = e.target.value as DeckSort;
              setActiveSort(next);
              setSortAsc(next === "alpha");
            }}
            className="rounded bg-black/30 px-2 py-1 text-xs text-slate-300 outline-none ring-1 ring-white/10 focus:ring-white/20"
          >
            <option value="alpha">Name</option>
            <option value="recent">Date Added</option>
            {selectedFormat && <option value="format">Format</option>}
          </select>
          <button
            onClick={() => setSortAsc((prev) => !prev)}
            className="rounded p-1 text-slate-400 ring-1 ring-white/10 transition-colors hover:bg-white/5 hover:text-white"
            title={sortAsc ? "Ascending" : "Descending"}
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              viewBox="0 0 16 16"
              fill="currentColor"
              className={`h-3.5 w-3.5 transition-transform duration-150 ${sortAsc ? "" : "rotate-180"}`}
            >
              <path fillRule="evenodd" d="M8 3.5a.5.5 0 0 1 .354.146l4 4a.5.5 0 0 1-.708.708L8 4.707 4.354 8.354a.5.5 0 1 1-.708-.708l4-4A.5.5 0 0 1 8 3.5ZM3.5 10a.5.5 0 0 1 .5-.5h8a.5.5 0 0 1 0 1H4a.5.5 0 0 1-.5-.5Z" clipRule="evenodd" />
            </svg>
          </button>
        </div>
        </>)}
      </div>

      {/* Format-filter banner: in select mode, when the caller pins a format
          (host pre-chosen format on host-setup, or the host's format on join),
          the deck list is filtered to only legal decks. Without this banner the
          filtering is silent — users see a shorter list with no explanation. */}
      {mode === "select" && selectedFormat && (
        <div className="flex w-full items-center justify-between gap-3 rounded-xl border border-indigo-400/25 bg-indigo-500/[0.08] px-4 py-2.5">
          <div className="flex items-center gap-2 text-sm">
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5 text-indigo-300">
              <path fillRule="evenodd" d="M2 4a1 1 0 0 1 1-1h10a1 1 0 0 1 .8 1.6L10 9.333V13a1 1 0 0 1-.553.894l-2 1A1 1 0 0 1 6 14V9.333L2.2 4.6A1 1 0 0 1 2 4Z" clipRule="evenodd" />
            </svg>
            <span className="text-indigo-100">
              {activeFilter === "all"
                ? <>Showing all saved decks for <span className="font-semibold">{selectedFormat}</span></>
                : <>Showing decks legal in <span className="font-semibold">{selectedFormat}</span></>}
            </span>
            <span className="text-xs text-indigo-300/70">
              · {searchFiltered.length} of {deckNames.length}
            </span>
          </div>
          <button
            onClick={() => setActiveFilter((current) => (current === "all" ? (contextualFilter ?? "all") : "all"))}
            className="rounded border border-indigo-300/25 bg-indigo-400/10 px-2.5 py-1 text-xs font-medium text-indigo-100 transition-colors hover:bg-indigo-400/18"
          >
            {activeFilter === "all" ? "Show legal only" : "Show all decks"}
          </button>
        </div>
      )}

      {isEvaluating && (
        <div className="flex w-full items-center justify-center gap-2.5 rounded-xl border border-indigo-400/20 bg-indigo-500/10 px-4 py-3">
          <span className="inline-block h-2.5 w-2.5 animate-pulse rounded-full bg-indigo-400" />
          <span className="text-sm font-medium text-indigo-200">Evaluating deck compatibility…</span>
        </div>
      )}

      {compatibilityError && (
        <div className="w-full rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          Compatibility check unavailable: {compatibilityError}
        </div>
      )}

      {searchFiltered.length === 0 ? (
        <div className="flex w-full flex-col items-center justify-center gap-4 rounded-[20px] border border-dashed border-white/10 bg-black/12 px-6 py-12 text-center">
          <div className="text-lg font-medium text-white">No decks match this filter.</div>
          <div className="max-w-md text-sm leading-6 text-slate-400">
            {mode === "select"
              ? "Import a compatible deck or change your format to see available decks."
              : "Pick a different filter or show all decks to choose from your full collection."}
          </div>
          {mode === "manage" && (
            <button
              onClick={() => setActiveFilter("all")}
              className={menuButtonClass({ tone: "neutral", size: "sm" })}
            >
              Show All Decks
            </button>
          )}
        </div>
      ) : (
        <div className="flex w-full flex-col gap-6">
          {/* User decks section */}
          <div>
            <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
              My Decks
              {userDecks.length > 0 && (
                <span className="ml-2 text-slate-600">{userDecks.length}</span>
              )}
            </h3>
            <div className="grid w-full grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
              <button
                onClick={() => setShowImport(true)}
                className="group relative flex aspect-[4/3] flex-col items-center justify-center gap-2 overflow-hidden rounded-xl ring-1 ring-white/10 transition hover:bg-white/5 hover:ring-white/20"
              >
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  viewBox="0 0 20 20"
                  fill="currentColor"
                  className="h-8 w-8 text-gray-500 transition-colors group-hover:text-gray-300"
                >
                  <path d="M10.75 4.75a.75.75 0 0 0-1.5 0v4.5h-4.5a.75.75 0 0 0 0 1.5h4.5v4.5a.75.75 0 0 0 1.5 0v-4.5h4.5a.75.75 0 0 0 0-1.5h-4.5v-4.5Z" />
                </svg>
                <span className="text-xs font-medium text-gray-500 transition-colors group-hover:text-gray-300">
                  Import Deck
                </span>
              </button>

              {userDecks.map((deckName) => (
                <DeckTile
                  key={deckName}
                  deckName={deckName}
                  isActive={deckName === activeDeckName}
                  compatibility={compatibilities[deckName]}
                  onClick={() => handleTileClick(deckName)}
                  onDelete={mode === "manage" ? () => handleDeleteDeck(deckName) : undefined}
                />
              ))}
            </div>
          </div>

          {/* Bundled decks section */}
          {bundledDecks.length > 0 && (
            <div>
              <div className="mb-3 flex items-center justify-between">
                <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-400">
                  Starter Decks
                  <span className="ml-2 text-slate-600">{bundledDecks.length}</span>
                </h3>
                <button
                  onClick={() => setShowFeedManager(true)}
                  className="text-[11px] text-slate-500 transition-colors hover:text-slate-300"
                >
                  Manage Feeds
                </button>
              </div>
              <div className="grid w-full grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
                {bundledDecks.map((deckName) => (
                  <DeckTile
                    key={deckName}
                    deckName={deckName}
                    isActive={deckName === activeDeckName}
                    compatibility={compatibilities[deckName]}
                    onClick={() => handleTileClick(deckName)}
                  />
                ))}
              </div>
            </div>
          )}
        </div>
      )}
      </>)}

      {activeTab === "subscriptions" && mode === "manage" && (
        <SubscriptionsView
          activeDeckName={activeDeckName}
          compatibilities={compatibilities}
          onTileClick={handleTileClick}
          onAdopt={handleAdoptDeck}
        />
      )}

      {mode === "select" && onConfirmSelection && (
        <div className="sticky bottom-3 z-10 w-full">
          <div className="flex items-center justify-between gap-4 rounded-[20px] border border-white/10 bg-[#0a0f1b]/90 px-4 py-3 shadow-[0_18px_40px_rgba(0,0,0,0.28)] backdrop-blur-md">
            <div className="min-w-0">
              <div className="text-xs text-slate-500">Selected deck</div>
              <div className="truncate text-sm font-medium text-white">
                {selectedDeckLabel ?? "Choose a deck to continue"}
              </div>
            </div>
          {confirmAction ?? (
            <button
              onClick={onConfirmSelection}
              disabled={noDeckSelected}
              className={menuButtonClass({ tone: "indigo", size: "sm", disabled: noDeckSelected })}
            >
              {confirmLabel}
            </button>
          )}
        </div>
      </div>
      )}

      <ImportDeckModal
        open={showImport}
        onClose={() => setShowImport(false)}
        onImported={handleImported}
      />
      <FeedManagerModal
        open={showFeedManager}
        onClose={handleFeedManagerClose}
      />
    </Wrapper>
  );
}

interface SubscriptionsViewProps {
  activeDeckName: string | null;
  compatibilities: Record<string, DeckCompatibilityResult>;
  onTileClick: (deckName: string) => void;
  onAdopt: (deckName: string) => void;
}

function SubscriptionsView({
  activeDeckName,
  compatibilities,
  onTileClick,
  onAdopt,
}: SubscriptionsViewProps) {
  const subs = listSubscriptions();
  const feedCache = useFeedCacheSnapshot();

  if (subs.length === 0) {
    return (
      <div className="flex w-full flex-col items-center justify-center gap-4 rounded-[20px] border border-dashed border-white/10 bg-black/12 px-6 py-12 text-center">
        <div className="text-lg font-medium text-white">No feed subscriptions</div>
        <div className="max-w-md text-sm leading-6 text-slate-400">
          Subscribe to deck feeds to get curated deck collections that auto-update.
        </div>
      </div>
    );
  }

  return (
    <div className="flex w-full flex-col gap-6">
      {subs.map((sub) => {
        const feed = feedCache[sub.sourceId] ?? null;
        const feedDecks = feed?.decks ?? [];
        const deckCount = feedDecks.length;
        const lastRefreshed = new Date(sub.lastRefreshedAt).toLocaleDateString();

        return (
          <div key={sub.sourceId}>
            <div className="mb-3 flex items-center justify-between">
              <div>
                <h3 className="text-sm font-semibold text-white">
                  {feed?.icon && (
                    <span className="mr-2 inline-flex h-5 w-5 items-center justify-center rounded bg-white/10 text-[10px] font-bold">
                      {feed.icon}
                    </span>
                  )}
                  {feed?.name ?? sub.sourceId}
                </h3>
                <p className="mt-0.5 text-xs text-slate-500">
                  {feed?.description} · {deckCount} {deckCount === 1 ? "deck" : "decks"} · Updated {lastRefreshed}
                  {sub.error && <span className="ml-2 text-red-400">Error: {sub.error}</span>}
                </p>
              </div>
            </div>
            <div className="grid w-full grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
              {[...feedDecks].sort((a, b) => a.name.localeCompare(b.name)).map((deck) => (
                <DeckTile
                  key={deck.name}
                  deckName={deck.name}
                  isActive={deck.name === activeDeckName}
                  compatibility={compatibilities[deck.name]}
                  onClick={() => onTileClick(deck.name)}
                  onAdopt={() => onAdopt(deck.name)}
                  hideFeedBadge
                  feedDeckOverride={deck}
                />
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}
