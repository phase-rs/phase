import type { ReactNode } from "react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { GameFormat, MatchType } from "../../adapter/types";
import type { FeedDeck } from "../../types/feed";
import { ACTIVE_DECK_KEY, listSavedDeckNames, getDeckMeta, deleteDeck } from "../../constants/storage";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import {
  getDeckFeedOrigin,
  listSubscriptions,
  refreshAllFeeds,
  adoptFeedDeck,
} from "../../services/feedService";
import {
  useCachedFeed,
  useFeedCacheSnapshot,
} from "../../services/feedPersistence";
import { FeedManagerModal } from "./FeedManagerModal";
import { useCardImage } from "../../hooks/useCardImage";
import {
  evaluateDeckCompatibilityBatch,
  type DeckCompatibilityResult,
} from "../../services/deckCompatibility";
import {
  buildDeckCatalog,
  type DeckCatalogCandidate,
} from "../../services/deckCatalog";
import { ImportDeckModal } from "./ImportDeckModal";
import { PreconDeckModal } from "./PreconDeckModal";
import { savePreconDeck } from "../../services/preconDecks";
import type { DeckEntry as PreconDeckEntry } from "../../hooks/useDecks";
import { MenuPanel } from "./MenuShell";
import { menuButtonClass } from "./buttonStyles";
import { useSetSymbol } from "../../hooks/useSetSymbols";
import {
  COLOR_DOT_CLASS,
  getDeckCardCount,
  getDeckColorIdentity,
  getRepresentativeCard,
  isBundledDeck,
} from "./deckHelpers";
import { BASIC_LAND_NAMES } from "../../constants/game";
import { BracketEstimateChip } from "../deck-builder/BracketEstimateChip";
import { useBracketEstimate } from "../../hooks/useBracketEstimate";
import { getSharedAdapter } from "../../adapter/wasm-adapter";
const PRECON_PREFIX = "[Pre-built] ";
const PRECON_PAGE_SIZE = 12;
const DECK_SCAN_BATCH_SIZE = 1;
const COVERAGE_SCAN_BATCH_SIZE = 6;

/** Tags that represent a format/archetype — shown with active (green) styling. */
const FORMAT_TAGS = new Set([
  ...FORMAT_REGISTRY.flatMap((m) => [
    m.format.toLowerCase(),
    m.label.toLowerCase(),
    m.short_label.toLowerCase(),
  ]),
  "metagame",
]);
const DECK_FORMATS = FORMAT_REGISTRY.filter((m) => m.group !== "Multiplayer");
const BASIC_LAND_COLORS: Record<string, string> = {
  Plains: "W",
  Island: "U",
  Swamp: "B",
  Mountain: "R",
  Forest: "G",
  "Snow-Covered Plains": "W",
  "Snow-Covered Island": "U",
  "Snow-Covered Swamp": "B",
  "Snow-Covered Mountain": "R",
  "Snow-Covered Forest": "G",
};
const COLOR_ORDER = ["W", "U", "B", "R", "G"];

type DeckFilter = "all" | GameFormat;
type DeckSort = "alpha" | "recent" | "format";

function coverageFromPct(coveragePct: number | null | undefined): DeckCompatibilityResult["coverage"] {
  if (coveragePct == null) return null;
  return {
    total_unique: 100,
    supported_unique: Math.max(0, Math.min(100, Math.round(coveragePct))),
    unsupported_cards: [],
  };
}

function getPreconColorIdentity(deck: PreconDeckEntry | undefined): string[] {
  if (!deck) return [];
  const colors = new Set<string>();
  for (const entry of deck.mainBoard) {
    const color = BASIC_LAND_COLORS[entry.name];
    if (color) colors.add(color);
  }
  return COLOR_ORDER.filter((color) => colors.has(color));
}

function preconCandidateToDeckEntry(candidate: DeckCatalogCandidate): PreconDeckEntry {
  if (candidate.source.type !== "precon") {
    throw new Error("Expected precon deck candidate");
  }
  return {
    code: candidate.source.code,
    name: candidate.name.replace(/\s+\([^()]+\)$/, ""),
    type: candidate.preconDeck?.type ?? "Commander Deck",
    coveragePct: candidate.coveragePct ?? 100,
    mainBoard: candidate.deck.main.map((entry) => ({ name: entry.name, count: entry.count })),
    sideBoard: candidate.deck.sideboard.map((entry) => ({ name: entry.name, count: entry.count })),
    commander: candidate.deck.commander?.map((name) => ({ name, count: 1 })),
  };
}

/** Ordered list of format filters shown in the filter bar. */
const FORMAT_FILTERS: Array<{ key: DeckFilter; label: string; aetherhubUrl?: string }> = [
  { key: "all", label: "All" },
  ...DECK_FORMATS.map((m) => ({
    key: m.format,
    label: m.label,
    aetherhubUrl:
      m.format === "Historic"
        ? "https://aetherhub.com/Metagame/Historic"
        : m.format === "Brawl"
          ? "https://aetherhub.com/Metagame/Brawl"
          : undefined,
  })),
];

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

/** Inner component so the hook is always called unconditionally (Rules of Hooks).
 * Returns null for non-Commander decks — the hook handles that check. */
function BracketChipForDeck({ candidate }: { candidate: DeckCatalogCandidate }) {
  const { estimate } = useBracketEstimate({
    deck: candidate.deck,
    commanders: candidate.deck.commander ?? [],
    format: candidate.knownFormat,
    adapter: getSharedAdapter(),
  });
  return <BracketEstimateChip tier={estimate?.tier ?? null} />;
}

interface DeckTileProps {
  deckName: string;
  isActive: boolean;
  compatibility: DeckCompatibilityResult | undefined;
  onClick: () => void;
  onEdit?: () => void;
  onDelete?: () => void;
  onAdopt?: () => void;
  /** When true, suppress the feed badge (used in subscription view where the header already identifies the feed). */
  hideFeedBadge?: boolean;
  /** Provide feed deck data directly so the tile doesn't depend on localStorage. */
  feedDeckOverride?: FeedDeck;
  /** Provide precon data directly for virtual precon tiles not yet saved locally. */
  preconDeckOverride?: PreconDeckEntry;
  /** Catalog candidate — when provided and the deck is Commander format, renders
   *  a BracketEstimateChip in the tile's footer. */
  catalogCandidate?: DeckCatalogCandidate;
}

const DeckTile = memo(function DeckTile({ deckName, isActive, compatibility, onClick, onEdit, onDelete, onAdopt, hideFeedBadge, feedDeckOverride, preconDeckOverride, catalogCandidate }: DeckTileProps) {
  const [coverageHovered, setCoverageHovered] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  useEffect(() => {
    if (!confirmingDelete) return;
    const timer = setTimeout(() => setConfirmingDelete(false), 3000);
    return () => clearTimeout(timer);
  }, [confirmingDelete]);
  const colors = compatibility?.color_identity?.length
    ? compatibility.color_identity
    : feedDeckOverride?.colors?.length
      ? feedDeckOverride.colors
      : preconDeckOverride
        ? getPreconColorIdentity(preconDeckOverride)
        : getDeckColorIdentity(deckName);
  const count = feedDeckOverride
    ? feedDeckOverride.main.reduce((sum, e) => sum + e.count, 0)
    : preconDeckOverride
      ? preconDeckOverride.mainBoard.reduce((sum, e) => sum + e.count, 0)
    : getDeckCardCount(deckName);
  const representativeCard = feedDeckOverride
    ? (feedDeckOverride.commander?.[0] ?? feedDeckOverride.main.find((e) => !BASIC_LAND_NAMES.has(e.name))?.name ?? null)
    : preconDeckOverride
      ? (preconDeckOverride.commander?.[0]?.name ?? preconDeckOverride.mainBoard.find((e) => !BASIC_LAND_NAMES.has(e.name))?.name ?? null)
    : getRepresentativeCard(deckName);
  const feedOrigin = getDeckFeedOrigin(deckName);
  const feedForBadge = useCachedFeed(feedOrigin ?? "");
  const feedBadge = !hideFeedBadge && feedOrigin ? (feedForBadge?.name ?? "Feed") : null;
  const isPrecon = deckName.startsWith(PRECON_PREFIX);
  const displayName = isPrecon ? deckName.slice(PRECON_PREFIX.length) : deckName;
  const coverage = compatibility?.coverage ?? coverageFromPct(preconDeckOverride?.coveragePct);

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

      {onEdit && (
        <button
          onClick={(e) => { e.stopPropagation(); onEdit(); }}
          className={`absolute right-2 z-20 flex h-6 w-6 items-center justify-center rounded-full bg-black/70 text-gray-300 opacity-0 transition-opacity hover:bg-indigo-600 hover:text-white group-hover:opacity-100 ${feedBadge ? "top-10" : "top-2"}`}
          title={`Edit ${displayName}`}
          aria-label={`Edit ${displayName}`}
        >
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
            <path d="M11.013 1.427a1.75 1.75 0 0 1 2.474 2.474L6.226 11.16a2.25 2.25 0 0 1-.892.547l-2.115.705a.5.5 0 0 1-.632-.632l.705-2.115a2.25 2.25 0 0 1 .547-.892l7.174-7.346Z" />
            <path d="M3.75 13.5a.75.75 0 0 0 0 1.5h8.5a.75.75 0 0 0 0-1.5h-8.5Z" />
          </svg>
        </button>
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
        {preconDeckOverride?.code && (
          <div className="mb-1 flex justify-center">
            <PreconSetBadge deck={preconDeckOverride} />
          </div>
        )}
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
          {/* Bracket estimate chip (Commander decks only) */}
          {catalogCandidate && <BracketChipForDeck candidate={catalogCandidate} />}
          {/* Feed format/archetype tags */}
          {feedDeckOverride?.tags?.map((tag) => (
            <StatusBadge key={tag} label={tag} active={FORMAT_TAGS.has(tag)} />
          ))}
          {isPrecon && !preconDeckOverride && !feedDeckOverride?.tags?.length && (
            <StatusBadge label="precon" active />
          )}
          {/* Engine compatibility badges */}
          {compatibility?.standard.compatible && <StatusBadge label="STD" active />}
          {!preconDeckOverride && compatibility?.commander.compatible && <StatusBadge label="CMD" active />}
          {compatibility?.bo3_ready && <StatusBadge label="BO3" active />}
          {compatibility && compatibility.unknown_cards.length > 0 && (
            <span
              className="rounded bg-amber-500/80 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-black"
              title={`Unknown cards:\n${compatibility.unknown_cards.join("\n")}`}
            >
              Unknown {compatibility.unknown_cards.length}
            </span>
          )}
          {coverage && coverage.supported_unique < coverage.total_unique && (() => {
            const { supported_unique, total_unique, unsupported_cards } = coverage;
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
});

interface SavedDeckTileProps {
  deckName: string;
  isActive: boolean;
  compatibility: DeckCompatibilityResult | undefined;
  onTileClick: (deckName: string) => void;
  onEditDeck?: (deckName: string) => void;
  onDeleteDeck: (deckName: string) => void;
  preconCandidate?: DeckCatalogCandidate;
  /** Non-precon catalog candidate — provides deck + format data for bracket chip. */
  catalogCandidate?: DeckCatalogCandidate;
}

const SavedDeckTile = memo(function SavedDeckTile({
  deckName,
  isActive,
  compatibility,
  onTileClick,
  onEditDeck,
  onDeleteDeck,
  preconCandidate,
  catalogCandidate,
}: SavedDeckTileProps) {
  const handleClick = useCallback(() => onTileClick(deckName), [deckName, onTileClick]);
  const handleEdit = useMemo(
    () => onEditDeck ? () => onEditDeck(deckName) : undefined,
    [deckName, onEditDeck],
  );
  const handleDelete = useCallback(() => onDeleteDeck(deckName), [deckName, onDeleteDeck]);
  const preconDeckOverride = useMemo(
    () => preconCandidate ? preconCandidateToDeckEntry(preconCandidate) : undefined,
    [preconCandidate],
  );

  return (
    <DeckTile
      deckName={deckName}
      isActive={isActive}
      compatibility={compatibility}
      preconDeckOverride={preconDeckOverride}
      catalogCandidate={catalogCandidate ?? preconCandidate}
      onClick={handleClick}
      onEdit={handleEdit}
      onDelete={handleDelete}
    />
  );
});

interface FeedDeckTileProps {
  deck: FeedDeck;
  isActive: boolean;
  compatibility: DeckCompatibilityResult | undefined;
  onTileClick: (deckName: string) => void;
  onAdopt: (deckName: string) => void;
}

const FeedDeckTile = memo(function FeedDeckTile({
  deck,
  isActive,
  compatibility,
  onTileClick,
  onAdopt,
}: FeedDeckTileProps) {
  const handleClick = useCallback(() => onTileClick(deck.name), [deck.name, onTileClick]);
  const handleAdopt = useCallback(() => onAdopt(deck.name), [deck.name, onAdopt]);

  return (
    <DeckTile
      deckName={deck.name}
      isActive={isActive}
      compatibility={compatibility}
      onClick={handleClick}
      onAdopt={handleAdopt}
      hideFeedBadge
      feedDeckOverride={deck}
    />
  );
});

function PreconSetBadge({ deck }: { deck: PreconDeckEntry | undefined }) {
  const setIcon = useSetSymbol(deck?.code);
  if (!deck?.code) return null;

  return (
    <span
      className="flex h-7 min-w-7 items-center justify-center rounded-full bg-black/65 px-1.5 text-[10px] font-bold uppercase tracking-wide text-white/80 ring-1 ring-white/15 backdrop-blur-sm"
      title={deck.code}
    >
      {setIcon ? (
        <img
          src={setIcon}
          alt={`${deck.code} set icon`}
          className="h-[18px] w-[18px] invert"
        />
      ) : (
        deck.code
      )}
    </span>
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
  const [showPrecon, setShowPrecon] = useState(false);
  const [showFeedManager, setShowFeedManager] = useState(false);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [compatibilities, setCompatibilities] = useState<Record<string, DeckCompatibilityResult>>({});
  const [isEvaluating, setIsEvaluating] = useState(false);
  const [compatibilityStatus, setCompatibilityStatus] = useState<string | null>(null);
  const [compatibilityError, setCompatibilityError] = useState<string | null>(null);
  const pendingCompatibility = useRef(new Set<string>());
  const pendingCoverage = useRef(new Set<string>());
  const completedCoverage = useRef(new Set<string>());
  const coverageInFlight = useRef(false);
  const compatibilityGeneration = useRef(0);
  const [deckScanIndex, setDeckScanIndex] = useState(0);
  const [coverageStatus, setCoverageStatus] = useState<{ deckName: string; remaining: number } | null>(null);
  const [coverageQueueVersion, setCoverageQueueVersion] = useState(0);
  const [catalogCandidates, setCatalogCandidates] = useState<DeckCatalogCandidate[]>([]);
  const [preconDisplayCount, setPreconDisplayCount] = useState(PRECON_PAGE_SIZE);
  const feedCache = useFeedCacheSnapshot();

  const contextualFilter = useMemo<DeckFilter | null>(() => {
    return selectedFormat && DECK_FORMATS.some((m) => m.format === selectedFormat)
      ? selectedFormat
      : null;
  }, [selectedFormat]);
  const [activeFilter, setActiveFilter] = useState<DeckFilter>(contextualFilter ?? "all");
  const selectedFormatForCompatibility = selectedFormat ?? (activeFilter === "all" ? null : activeFilter);
  const activeFilterOption = FORMAT_FILTERS.find((option) => option.key === activeFilter);
  const requiresCompatibilityFilter = activeFilter !== "all";
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
    setPreconDisplayCount(PRECON_PAGE_SIZE);
  }, [selectedFormatForCompatibility, searchQuery]);

  useEffect(() => {
    let cancelled = false;
    buildDeckCatalog({ savedDeckNames: deckNames, feedCache }).then((candidates) => {
      if (cancelled) return;
      setCatalogCandidates(candidates);
    });

    return () => {
      cancelled = true;
    };
  }, [deckNames, feedCache]);

  useEffect(() => {
    if (mode !== "select") return;
    if (!onSelectDeck) return;
    if (activeDeckName != null) return;
    const stored = localStorage.getItem(ACTIVE_DECK_KEY);
    if (!stored || !deckNames.includes(stored)) return;
    onSelectDeck(stored);
  }, [mode, activeDeckName, deckNames, onSelectDeck]);

  const deckCandidatesByName = useMemo(() => {
    const decks = new Map<string, DeckCatalogCandidate>();
    for (const candidate of catalogCandidates) decks.set(candidate.name, candidate);
    return decks;
  }, [catalogCandidates]);
  const deckNamesKey = deckNames.join("\0");

  useEffect(() => {
    compatibilityGeneration.current += 1;
    pendingCompatibility.current.clear();
    pendingCoverage.current.clear();
    completedCoverage.current.clear();
    coverageInFlight.current = false;
    setCompatibilities({});
    setCompatibilityError(null);
    setIsEvaluating(false);
    setCompatibilityStatus(null);
    setDeckScanIndex(0);
    setCoverageStatus(null);
  }, [deckNamesKey, selectedFormatForCompatibility, selectedMatchType]);

  useEffect(() => {
    onCompatibilityUpdate?.(compatibilities);
  }, [compatibilities, onCompatibilityUpdate]);

  const searchedDeckNames = useMemo(() => {
    if (!searchQuery) return deckNames;
    const q = searchQuery.toLowerCase();
    return deckNames.filter((name) => name.toLowerCase().includes(q));
  }, [deckNames, searchQuery]);

  const unknownFormatDeckNames = useMemo(() => {
    if (!requiresCompatibilityFilter || !selectedFormatForCompatibility) return [];
    if (!activeDeckName || !searchedDeckNames.includes(activeDeckName)) return [];
    const candidate = deckCandidatesByName.get(activeDeckName);
    return candidate && candidate.knownFormat == null ? [activeDeckName] : [];
  }, [
    activeDeckName,
    deckCandidatesByName,
    requiresCompatibilityFilter,
    searchedDeckNames,
    selectedFormatForCompatibility,
  ]);
  const unknownFormatDeckNamesKey = unknownFormatDeckNames.join("\0");

  useEffect(() => {
    setDeckScanIndex(0);
  }, [unknownFormatDeckNamesKey, requiresCompatibilityFilter, selectedFormatForCompatibility, selectedMatchType]);

  useEffect(() => {
    if (!requiresCompatibilityFilter || !selectedFormatForCompatibility) return;
    if (deckScanIndex >= unknownFormatDeckNames.length) return;

    let cancelled = false;
    const generation = compatibilityGeneration.current;
    const batchNames = unknownFormatDeckNames.slice(deckScanIndex, deckScanIndex + DECK_SCAN_BATCH_SIZE);
    const batch = batchNames.flatMap((name) => {
      const candidate = deckCandidatesByName.get(name);
      return candidate
        && compatibilities[name]?.selected_format_compatible == null
        && !pendingCompatibility.current.has(name)
        ? [{ name, deck: candidate.deck }]
        : [];
    });
    if (batch.length === 0) {
      setDeckScanIndex((index) => index + batchNames.length);
      return;
    }

    setIsEvaluating(true);
    const deckName = batch[0]?.name ?? "user deck";
    for (const { name } of batch) {
      pendingCompatibility.current.add(name);
    }
    setCompatibilityStatus(`Checking ${deckName}…`);

    evaluateDeckCompatibilityBatch(batch, {
      selectedFormat: selectedFormatForCompatibility,
      selectedMatchType,
      summaryOnly: true,
      onStatus: (status) => {
        if (cancelled || generation !== compatibilityGeneration.current) return;
        if (status === "starting-worker") setCompatibilityStatus("Starting compatibility worker…");
        if (status === "loading-card-database") setCompatibilityStatus("Loading compatibility database…");
        if (status === "checking-deck") setCompatibilityStatus(`Checking ${deckName}…`);
      },
      onResult: (name, result) => {
        if (cancelled || generation !== compatibilityGeneration.current) return;
        setCompatibilityStatus(`Checked ${name}`);
        setCompatibilities((current) => {
          const next = { ...current, [name]: result };
          return next;
        });
      },
    }).then((results) => {
      if (cancelled || generation !== compatibilityGeneration.current) return;
      setCompatibilities((current) => {
        const next = { ...current, ...results };
        return next;
      });
      setCompatibilityError(null);
      setDeckScanIndex((index) => index + batchNames.length);
    }).catch((error) => {
      if (cancelled || generation !== compatibilityGeneration.current) return;
      setCompatibilityError(error instanceof Error ? error.message : String(error));
      setDeckScanIndex((index) => index + batchNames.length);
    }).finally(() => {
      if (cancelled || generation !== compatibilityGeneration.current) return;
      for (const { name } of batch) {
        pendingCompatibility.current.delete(name);
      }
      setCompatibilityStatus(null);
      setIsEvaluating(pendingCompatibility.current.size > 0);
    });

    return () => {
      cancelled = true;
    };
  }, [
    compatibilities,
    deckCandidatesByName,
    deckScanIndex,
    requiresCompatibilityFilter,
    selectedFormatForCompatibility,
    selectedMatchType,
    unknownFormatDeckNames,
  ]);

  const filteredDeckNames = useMemo(() => {
    return searchedDeckNames.filter((deckName) => {
      const compatibility = compatibilities[deckName];
      const knownFormat = deckCandidatesByName.get(deckName)?.knownFormat;
      if (requiresCompatibilityFilter && knownFormat === selectedFormatForCompatibility) return true;
      if (requiresCompatibilityFilter && !compatibility && getDeckFeedOrigin(deckName) == null) return true;
      if (!compatibility) return !requiresCompatibilityFilter;

      const selectedFormatCompatible = compatibility.selected_format_compatible;
      if (contextualFilter && activeFilter === contextualFilter && selectedFormatCompatible != null) {
        return selectedFormatCompatible;
      }

      if (activeFilter !== "all" && selectedFormatCompatible != null) {
        return selectedFormatCompatible;
      }
      return true;
    });
  }, [
    searchedDeckNames,
    compatibilities,
    activeFilter,
    contextualFilter,
    deckCandidatesByName,
    requiresCompatibilityFilter,
    selectedFormatForCompatibility,
  ]);

  const searchFiltered = useMemo(() => {
    return filteredDeckNames;
  }, [filteredDeckNames]);

  const filteredPreconCandidates = useMemo(() => {
    const saved = new Set(deckNames);
    const q = searchQuery.toLowerCase();
    return catalogCandidates
      .filter((candidate) => candidate.source.type === "precon")
      .filter((candidate) => {
        const prefixed = PRECON_PREFIX + candidate.name;
        if (saved.has(prefixed) || saved.has(candidate.name)) return false;
        return !q || prefixed.toLowerCase().includes(q);
      })
      .sort((a, b) => {
        const dateCompare = (b.source.type === "precon" ? b.source.releaseDate ?? "" : "")
          .localeCompare(a.source.type === "precon" ? a.source.releaseDate ?? "" : "");
        return dateCompare || a.name.localeCompare(b.name);
      });
  }, [catalogCandidates, deckNames, searchQuery]);

  const legalPreconCandidates = useMemo(() => {
    return selectedFormatForCompatibility === "Commander" ? filteredPreconCandidates : [];
  }, [filteredPreconCandidates, selectedFormatForCompatibility]);

  const legalPreconByName = useMemo(() => {
    const entries = legalPreconCandidates.map((candidate) => [
      PRECON_PREFIX + candidate.name,
      candidate,
    ] as const);
    return new Map(entries);
  }, [legalPreconCandidates]);

  const preconDeckNames = useMemo(() => {
    return Array.from(legalPreconByName.keys());
  }, [legalPreconByName]);

  const displayedPreconDeckNames = useMemo(
    () => preconDeckNames.slice(0, preconDisplayCount),
    [preconDeckNames, preconDisplayCount],
  );

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
    return {
      userDecks: sortNames(user),
      bundledDecks: sortNames(bundled),
    };
  }, [searchFiltered, activeSort, sortAsc, compatibilities]);

  const noDeckSelected = mode === "select"
    ? !activeDeckName || (!searchFiltered.includes(activeDeckName) && !preconDeckNames.includes(activeDeckName))
    : false;
  const selectedDeckLabel = mode === "select"
    && activeDeckName
    && (searchFiltered.includes(activeDeckName) || preconDeckNames.includes(activeDeckName))
    ? activeDeckName
    : null;
  const visibleDeckCount = searchFiltered.length + preconDeckNames.length;
  const userDeckScanTotal = requiresCompatibilityFilter ? unknownFormatDeckNames.length : 0;
  const userDeckScanCompleted = Math.min(deckScanIndex, userDeckScanTotal);
  const isScanningUserDecks = userDeckScanCompleted < userDeckScanTotal;
  const subscriptionVisibleDeckNames = useMemo(() => {
    if (activeTab !== "subscriptions" || mode !== "manage") return [];
    return listSubscriptions().flatMap((sub) =>
      [...(feedCache[sub.sourceId]?.decks ?? [])]
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((deck) => deck.name),
    );
  }, [activeTab, feedCache, mode]);
  const visibleCoverageDeckNames = useMemo(() => {
    const mainGridDecks = activeTab === "subscriptions" && mode === "manage"
      ? subscriptionVisibleDeckNames
      : [...userDecks, ...bundledDecks, ...displayedPreconDeckNames];
    return mainGridDecks.filter((deckName) => {
      const existing = compatibilities[deckName];
      if (existing?.coverage) return false;
      if (pendingCoverage.current.has(deckName)) return false;
      if (completedCoverage.current.has(deckName)) return false;
      return deckCandidatesByName.has(deckName)
        || legalPreconByName.has(deckName);
    });
  }, [
    activeTab,
    bundledDecks,
    compatibilities,
    deckCandidatesByName,
    displayedPreconDeckNames,
    legalPreconByName,
    mode,
    subscriptionVisibleDeckNames,
    userDecks,
  ]);

  useEffect(() => {
    if (isScanningUserDecks) return;
    if (coverageInFlight.current) return;
    if (visibleCoverageDeckNames.length === 0) return;

    const batchNames = visibleCoverageDeckNames.slice(0, COVERAGE_SCAN_BATCH_SIZE);
    const syntheticResults: Record<string, DeckCompatibilityResult> = {};
    const batch = batchNames.flatMap((name) => {
      const candidate = deckCandidatesByName.get(name) ?? legalPreconByName.get(name);
      if (!candidate || pendingCoverage.current.has(name)) return [];

      if (candidate.coveragePct != null) {
        completedCoverage.current.add(name);
        syntheticResults[name] = {
          standard: { compatible: candidate.knownFormat === "Standard", reasons: [] },
          commander: { compatible: candidate.knownFormat === "Commander", reasons: [] },
          bo3_ready: candidate.deck.sideboard.length > 0,
          unknown_cards: [],
          selected_format_compatible: selectedFormatForCompatibility
            ? candidate.knownFormat === selectedFormatForCompatibility
            : null,
          selected_format_reasons: [],
          color_identity: getPreconColorIdentity(candidate.preconDeck),
          coverage: coverageFromPct(candidate.coveragePct),
        };
        return [];
      }

      pendingCoverage.current.add(name);
      return [{ name, deck: candidate.deck }];
    });

    if (Object.keys(syntheticResults).length > 0) {
      setCompatibilities((current) => ({ ...current, ...syntheticResults }));
    }
    if (batch.length === 0) {
      setCoverageQueueVersion((version) => version + 1);
      return;
    }

    const generation = compatibilityGeneration.current;
    coverageInFlight.current = true;
    setIsEvaluating(true);
    const firstName = batch[0]?.name ?? "visible deck";
    setCoverageStatus({ deckName: firstName, remaining: visibleCoverageDeckNames.length });
    setCompatibilityStatus(`Loading coverage for ${firstName}…`);

    evaluateDeckCompatibilityBatch(batch, {
      selectedFormat: selectedFormatForCompatibility,
      selectedMatchType,
      onStatus: (status, statusDeckName) => {
        if (generation !== compatibilityGeneration.current) return;
        const currentDeckName = statusDeckName ?? firstName;
        if (status === "starting-worker") setCompatibilityStatus("Starting compatibility worker…");
        if (status === "loading-card-database") setCompatibilityStatus("Loading compatibility database…");
        if (status === "checking-deck") setCompatibilityStatus(`Loading coverage for ${currentDeckName}…`);
      },
      onResult: (name, result) => {
        if (generation !== compatibilityGeneration.current) return;
        completedCoverage.current.add(name);
        setCompatibilities((current) => ({ ...current, [name]: result }));
      },
    }).then((results) => {
      if (generation !== compatibilityGeneration.current) return;
      for (const name of Object.keys(results)) {
        completedCoverage.current.add(name);
      }
      setCompatibilityError(null);
    }).catch((error) => {
      if (generation !== compatibilityGeneration.current) return;
      setCompatibilityError(error instanceof Error ? error.message : String(error));
    }).finally(() => {
      for (const { name } of batch) pendingCoverage.current.delete(name);
      coverageInFlight.current = false;
      if (generation !== compatibilityGeneration.current) return;
      setCompatibilityStatus(null);
      setCoverageStatus(null);
      setIsEvaluating(pendingCompatibility.current.size > 0 || pendingCoverage.current.size > 0);
      setCoverageQueueVersion((version) => version + 1);
    });
  }, [
    coverageQueueVersion,
    deckCandidatesByName,
    isScanningUserDecks,
    legalPreconByName,
    selectedFormatForCompatibility,
    selectedMatchType,
    visibleCoverageDeckNames,
  ]);

  const coverageScanTotal = coverageStatus?.remaining ?? visibleCoverageDeckNames.length;
  const isScanningCoverage = coverageScanTotal > 0 || pendingCoverage.current.size > 0;
  const showEvaluationStatus = mode === "manage"
    && (isScanningUserDecks || isScanningCoverage || (isEvaluating && !requiresCompatibilityFilter));

  const materializePreconDeck = useCallback((deckName: string): boolean => {
    const candidate = legalPreconByName.get(deckName);
    if (!candidate || candidate.source.type !== "precon") return false;
    savePreconDeck(deckName, preconCandidateToDeckEntry(candidate));
    setDeckNames(listSavedDeckNames());
    return true;
  }, [legalPreconByName]);

  const handleTileClick = useCallback((deckName: string) => {
    if (mode === "manage") {
      onEditDeck?.(deckName);
      return;
    }
    materializePreconDeck(deckName);
    onSelectDeck?.(deckName);
  }, [materializePreconDeck, mode, onEditDeck, onSelectDeck]);

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

  const handleAdoptDeck = useCallback((deckName: string) => {
    const newName = prompt("Save as:", deckName);
    if (!newName) return;
    adoptFeedDeck(deckName, newName);
    setDeckNames(listSavedDeckNames());
  }, []);

  const handleDeleteDeck = useCallback((deckName: string) => {
    deleteDeck(deckName);
    setDeckNames(listSavedDeckNames());
  }, []);

  const handleFeedManagerClose = () => {
    setShowFeedManager(false);
    setDeckNames(listSavedDeckNames());
  };

  const Wrapper = bare ? "div" : MenuPanel;
  const wrapperClass = bare
    ? "flex w-full min-w-0 flex-col items-center gap-4"
    : "flex w-full min-w-0 max-w-5xl flex-col items-center gap-6 px-4 py-5";

  return (
    <Wrapper className={wrapperClass}>
      {!bare && (
      <div className="flex w-full flex-col items-stretch gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-center sm:gap-4">
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
            className={`${menuButtonClass({ tone: "neutral", size: "sm" })} self-start sm:self-auto`}
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
      <div className="flex w-full min-w-0 flex-col items-stretch gap-2 sm:flex-row sm:flex-wrap sm:items-center">
        <div className="relative min-w-0 sm:w-[182px]">
          <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-slate-500">
            <path fillRule="evenodd" d="M9.965 11.026a5 5 0 1 1 1.06-1.06l2.755 2.754a.75.75 0 1 1-1.06 1.06l-2.755-2.754ZM10.5 7a3.5 3.5 0 1 1-7 0 3.5 3.5 0 0 1 7 0Z" clipRule="evenodd" />
          </svg>
          <input
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Search decks…"
            className="w-full rounded-lg bg-black/30 py-1.5 pl-8 pr-3 text-xs text-slate-200 outline-none ring-1 ring-white/10 transition-colors placeholder:text-slate-500 focus:ring-white/20"
          />
        </div>

        {mode === "manage" && (<>
        <div className="flex min-w-0 items-center gap-1 sm:w-[228px]">
          <label htmlFor="my-decks-format-filter" className="text-[10px] font-semibold uppercase tracking-wider text-slate-500">
            Format
          </label>
          <select
            id="my-decks-format-filter"
            value={activeFilter}
            onChange={(e) => setActiveFilter(e.target.value as DeckFilter)}
            className="min-h-[30px] min-w-0 flex-1 rounded bg-black/30 px-2 py-1 text-xs text-slate-300 outline-none ring-1 ring-white/10 focus:ring-white/20 sm:w-44"
          >
            {FORMAT_FILTERS.map(({ key, label }) => (
              <option key={key} value={key}>
                {label}
              </option>
            ))}
          </select>
          {activeFilterOption?.aetherhubUrl && (
            <a
              href={activeFilterOption.aetherhubUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="rounded p-1 text-slate-500 transition-colors hover:bg-white/5 hover:text-slate-300"
              title={`Browse ${activeFilterOption.label} decks on Aetherhub`}
            >
              <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5">
                <path fillRule="evenodd" d="M4.5 2A2.5 2.5 0 0 0 2 4.5v7A2.5 2.5 0 0 0 4.5 14h7a2.5 2.5 0 0 0 2.5-2.5V9a.75.75 0 0 0-1.5 0v2.5a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1H7a.75.75 0 0 0 0-1.5H4.5ZM9 2a.75.75 0 0 0 0 1.5h2.69L8.22 7.03a.75.75 0 1 0 1.06 1.06l3.47-3.47V7a.75.75 0 0 0 1.5 0V2H9Z" clipRule="evenodd" />
              </svg>
            </a>
          )}
        </div>
        {contextualFilter && activeFilter === contextualFilter && (
          <button
            onClick={() => setActiveFilter("all")}
            className="rounded border border-indigo-500/50 bg-indigo-500/10 px-2 py-1 text-xs font-medium text-indigo-200 hover:bg-indigo-500/20"
          >
            Show all decks
          </button>
        )}
        <div className="flex items-center justify-end gap-1 sm:ml-auto">
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
              · {visibleDeckCount} of {deckNames.length + preconDeckNames.length}
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

      {showEvaluationStatus && (
        <div className="flex w-full items-center justify-between gap-3 rounded-xl border border-indigo-400/20 bg-indigo-500/10 px-4 py-3">
          <div className="flex items-center gap-2.5">
          <span className="inline-block h-2.5 w-2.5 animate-pulse rounded-full bg-indigo-400" />
            <span className="text-sm font-medium text-indigo-200">
              {compatibilityStatus
                ?? (isScanningUserDecks
                ? "Checking selected deck compatibility…"
                : coverageStatus
                  ? `Loading coverage for ${coverageStatus.deckName}…`
                  : "Evaluating visible decks…")}
            </span>
          </div>
          {isScanningUserDecks && (
            <span className="text-xs tabular-nums text-indigo-300/75">
              {userDeckScanCompleted}/{userDeckScanTotal}
            </span>
          )}
          {!isScanningUserDecks && isScanningCoverage && (
            <span className="text-xs tabular-nums text-indigo-300/75">
              {coverageScanTotal} remaining
            </span>
          )}
        </div>
      )}

      {compatibilityError && (
        <div className="w-full rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          Compatibility check unavailable: {compatibilityError}
        </div>
      )}

      {visibleDeckCount === 0 ? (
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
              <AddDeckTile
                label="Import Deck"
                onClick={() => setShowImport(true)}
                icon={
                  <path d="M10.75 4.75a.75.75 0 0 0-1.5 0v4.5h-4.5a.75.75 0 0 0 0 1.5h4.5v4.5a.75.75 0 0 0 1.5 0v-4.5h4.5a.75.75 0 0 0 0-1.5h-4.5v-4.5Z" />
                }
              />
              <AddDeckTile
                label="Preconstructed"
                onClick={() => setShowPrecon(true)}
                icon={
                  <path d="M3 3.5A1.5 1.5 0 0 1 4.5 2h7A1.5 1.5 0 0 1 13 3.5v13a1.5 1.5 0 0 1-1.5 1.5h-7A1.5 1.5 0 0 1 3 16.5v-13Zm11.25.5a.75.75 0 0 1 .75.75v11.5a.75.75 0 0 1-1.5 0V4.75a.75.75 0 0 1 .75-.75Zm2.5 1.5a.75.75 0 0 1 .75.75v8.5a.75.75 0 0 1-1.5 0v-8.5a.75.75 0 0 1 .75-.75Z" />
                }
              />

              {userDecks.map((deckName) => (
                <SavedDeckTile
                  key={deckName}
                  deckName={deckName}
                  isActive={deckName === activeDeckName}
                  compatibility={compatibilities[deckName]}
                  onTileClick={handleTileClick}
                  onEditDeck={onEditDeck}
                  onDeleteDeck={handleDeleteDeck}
                  preconCandidate={legalPreconByName.get(deckName)}
                  catalogCandidate={deckCandidatesByName.get(deckName)}
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
                  <SavedDeckTile
                    key={deckName}
                    deckName={deckName}
                    isActive={deckName === activeDeckName}
                    compatibility={compatibilities[deckName]}
                    onTileClick={handleTileClick}
                    onEditDeck={onEditDeck}
                    onDeleteDeck={handleDeleteDeck}
                    preconCandidate={legalPreconByName.get(deckName)}
                    catalogCandidate={deckCandidatesByName.get(deckName)}
                  />
                ))}
              </div>
            </div>
          )}

          {preconDeckNames.length > 0 && (
            <div className="rounded-[18px] border border-white/8 bg-black/10 p-3">
              <div className="mb-3 flex items-center justify-between gap-3">
                <h3 className="text-xs font-semibold uppercase tracking-wider text-slate-400">
                  Legal Precons
                  <span className="ml-2 text-slate-600">{preconDeckNames.length}</span>
                </h3>
                <button
                  onClick={() => setShowPrecon(true)}
                  className="text-[11px] text-slate-500 transition-colors hover:text-slate-300"
                >
                  Browse All
                </button>
              </div>
              <div className="grid w-full grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
                {displayedPreconDeckNames.map((deckName) => {
                  const candidate = legalPreconByName.get(deckName);
                  return (
                    <SavedDeckTile
                      key={deckName}
                      deckName={deckName}
                      isActive={deckName === activeDeckName}
                      compatibility={compatibilities[deckName]}
                      onTileClick={handleTileClick}
                      onEditDeck={onEditDeck}
                      onDeleteDeck={handleDeleteDeck}
                      preconCandidate={candidate}
                    />
                  );
                })}
              </div>
              {displayedPreconDeckNames.length < preconDeckNames.length && (
                <div className="mt-4 flex justify-center">
                  <button
                    type="button"
                    onClick={() => setPreconDisplayCount((count) => count + PRECON_PAGE_SIZE)}
                    className={menuButtonClass({ tone: "neutral", size: "sm" })}
                  >
                    Load More
                  </button>
                </div>
              )}
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
      <PreconDeckModal
        open={showPrecon}
        onClose={() => setShowPrecon(false)}
        onImported={(name) => handleImported(name, listSavedDeckNames())}
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
                <FeedDeckTile
                  key={deck.name}
                  deck={deck}
                  isActive={deck.name === activeDeckName}
                  compatibility={compatibilities[deck.name]}
                  onTileClick={onTileClick}
                  onAdopt={onAdopt}
                />
              ))}
            </div>
          </div>
        );
      })}
    </div>
  );
}

interface AddDeckTileProps {
  label: string;
  icon: ReactNode;
  onClick: () => void;
}

/** Shared call-to-action tile used for "Import Deck" and "Preconstructed" in
 * the deck grid. Keeps the two entry points visually identical so the only
 * thing that differs is the icon + label. */
function AddDeckTile({ label, icon, onClick }: AddDeckTileProps) {
  return (
    <button
      onClick={onClick}
      className="group relative flex aspect-[4/3] flex-col items-center justify-center gap-2 overflow-hidden rounded-xl ring-1 ring-white/10 transition hover:bg-white/5 hover:ring-white/20"
    >
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 20 20"
        fill="currentColor"
        className="h-8 w-8 text-gray-500 transition-colors group-hover:text-gray-300"
      >
        {icon}
      </svg>
      <span className="text-xs font-medium text-gray-500 transition-colors group-hover:text-gray-300">
        {label}
      </span>
    </button>
  );
}
