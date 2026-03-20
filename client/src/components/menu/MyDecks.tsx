import type { ReactNode } from "react";
import { useEffect, useMemo, useState } from "react";

import type { GameFormat, MatchType } from "../../adapter/types";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX, listSavedDeckNames, stampDeckMeta, getDeckMeta } from "../../constants/storage";
import { COMMANDER_PRECONS } from "../../data/commanderPrecons";
import { STARTER_DECKS } from "../../data/starterDecks";
import { useCardImage } from "../../hooks/useCardImage";
import type { ParsedDeck } from "../../services/deckParser";
import {
  evaluateDeckCompatibilityBatch,
  type DeckCompatibilityResult,
} from "../../services/deckCompatibility";
import { ImportDeckModal } from "./ImportDeckModal";
import { MenuPanel } from "./MenuShell";
import { menuButtonClass } from "./buttonStyles";

const BASIC_LANDS = new Set(["Plains", "Island", "Swamp", "Mountain", "Forest"]);

const COLOR_DOT_CLASS: Record<string, string> = {
  W: "bg-amber-200",
  U: "bg-blue-400",
  B: "bg-gray-600",
  R: "bg-red-500",
  G: "bg-green-500",
};

const PRECON_PREFIX = "[Pre-built] ";
const PRECON_NAMES = new Set(COMMANDER_PRECONS.map((p) => PRECON_PREFIX + p.name));
const STARTER_NAMES = new Set(STARTER_DECKS.map((s) => s.name));

function isBundledDeck(deckName: string): boolean {
  return PRECON_NAMES.has(deckName) || STARTER_NAMES.has(deckName);
}

type DeckFilter = "all" | "standard" | "commander" | "bo3";
type DeckSort = "alpha" | "recent";

function seedCommanderPrecons(): void {
  for (const precon of COMMANDER_PRECONS) {
    const deckName = PRECON_PREFIX + precon.name;
    const key = STORAGE_KEY_PREFIX + deckName;
    if (localStorage.getItem(key)) continue;
    const deck: ParsedDeck = {
      main: precon.cards,
      sideboard: [],
      commander: [precon.commander],
    };
    localStorage.setItem(key, JSON.stringify(deck));
    stampDeckMeta(deckName, 0);
  }
}

function loadDeck(deckName: string): ParsedDeck | null {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return null;
  try {
    return JSON.parse(raw) as ParsedDeck;
  } catch {
    return null;
  }
}

function getDeckColorIdentity(deckName: string): string[] {
  const starter = STARTER_DECKS.find((s) => s.name === deckName);
  if (starter) return starter.colorIdentity;

  const preconName = deckName.startsWith(PRECON_PREFIX)
    ? deckName.slice(PRECON_PREFIX.length)
    : deckName;
  const precon = COMMANDER_PRECONS.find((p) => p.name === preconName);
  return precon?.colorIdentity ?? [];
}

function getDeckCardCount(deckName: string): number {
  const deck = loadDeck(deckName);
  if (!deck) return 0;

  const mainCount = deck.main.reduce((sum, entry) => sum + entry.count, 0);
  const commanders = deck.commander ?? [];
  const representedInMain = commanders.filter((name) =>
    deck.main.some((entry) => entry.name.toLowerCase() === name.toLowerCase()),
  ).length;
  return mainCount + (commanders.length - representedInMain);
}

function getRepresentativeCard(deckName: string): string | null {
  const deck = loadDeck(deckName);
  if (!deck) return null;
  if (deck.commander && deck.commander.length > 0) {
    return deck.commander[0];
  }
  const entry = deck.main.find((item) => !BASIC_LANDS.has(item.name));
  return entry?.name ?? null;
}

function DeckArtTile({ cardName }: { cardName: string | null }) {
  const { src, isLoading } = useCardImage(cardName ?? "", { size: "art_crop" });

  if (!cardName || isLoading || !src) {
    return <div className="absolute inset-0 animate-pulse bg-gray-800" />;
  }

  return <img src={src} alt="" className="absolute inset-0 h-full w-full object-cover" />;
}

function StatusBadge({ label, active }: { label: string; active: boolean }) {
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
}

function DeckTile({ deckName, isActive, compatibility, onClick }: DeckTileProps) {
  const colors = compatibility?.color_identity ?? getDeckColorIdentity(deckName);
  const count = getDeckCardCount(deckName);
  const representativeCard = getRepresentativeCard(deckName);
  const isPrecon = PRECON_NAMES.has(deckName);
  const displayName = isPrecon ? deckName.slice(PRECON_PREFIX.length) : deckName;

  return (
    <button
      onClick={onClick}
      className={`group relative flex aspect-[4/3] flex-col justify-end overflow-hidden rounded-xl text-left transition ${
        isActive
          ? "ring-2 ring-white/30 ring-offset-2 ring-offset-[#060a16]"
          : "ring-1 ring-white/10 hover:ring-white/20"
      }`}
    >
      <DeckArtTile cardName={representativeCard} />

      {isPrecon && (
        <span className="absolute right-2 top-2 z-10 rounded-full bg-amber-500/80 px-2 py-0.5 text-[10px] font-bold uppercase tracking-wider text-black">
          Pre-built
        </span>
      )}

      <div className="relative z-10 bg-gradient-to-t from-black/95 via-black/70 to-transparent px-3 pb-3 pt-8">
        <p className="text-sm font-semibold text-white">{displayName}</p>
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
        {compatibility && (
          <div className="mt-2 flex flex-wrap gap-1">
            {compatibility.standard.compatible && <StatusBadge label="STD" active />}
            {compatibility.commander.compatible && <StatusBadge label="CMD" active />}
            {compatibility.bo3_ready && <StatusBadge label="BO3" active />}
            {compatibility.unknown_cards.length > 0 && (
              <span
                className="rounded bg-amber-500/80 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wider text-black"
                title={`Unknown cards:\n${compatibility.unknown_cards.join("\n")}`}
              >
                Unknown {compatibility.unknown_cards.length}
              </span>
            )}
            {compatibility.coverage && (() => {
              const { supported_unique, total_unique, unsupported_cards } = compatibility.coverage;
              const pct = total_unique > 0 ? (supported_unique / total_unique) * 100 : 0;
              const barColor =
                pct === 100 ? "bg-emerald-500"
                : pct >= 75 ? "bg-lime-500"
                : pct >= 50 ? "bg-amber-500"
                : "bg-red-500";
              return (
                <div
                  className="flex w-full items-center gap-1.5"
                  title={
                    unsupported_cards.length === 0
                      ? "All cards fully supported by the engine"
                      : `Unsupported (${unsupported_cards.length}):\n${unsupported_cards.map((c) => `${c.name}: ${c.gaps.join(", ")}`).join("\n")}`
                  }
                >
                  <div className="h-1 flex-1 overflow-hidden rounded-full bg-white/10">
                    <div
                      className={`h-full rounded-full ${barColor}`}
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  <span className="shrink-0 text-[10px] tabular-nums text-gray-400">
                    {supported_unique}/{total_unique}
                  </span>
                </div>
              );
            })()}
          </div>
        )}
      </div>
    </button>
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
}

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
}: MyDecksProps) {
  const [deckNames, setDeckNames] = useState<string[]>([]);
  const [showImport, setShowImport] = useState(false);
  const [compatibilities, setCompatibilities] = useState<Record<string, DeckCompatibilityResult>>({});
  const [isEvaluating, setIsEvaluating] = useState(false);
  const [compatibilityError, setCompatibilityError] = useState<string | null>(null);

  const contextualFilter = useMemo<DeckFilter | null>(() => {
    if (selectedFormat === "Standard") return "standard";
    if (selectedFormat === "Commander") return "commander";
    return null;
  }, [selectedFormat]);
  const [activeFilter, setActiveFilter] = useState<DeckFilter>(contextualFilter ?? "all");
  const [activeSort, setActiveSort] = useState<DeckSort>("alpha");
  const [sortAsc, setSortAsc] = useState(true);

  useEffect(() => {
    setActiveFilter(contextualFilter ?? "all");
  }, [contextualFilter]);

  useEffect(() => {
    if (selectedFormat === "Commander") {
      seedCommanderPrecons();
    }
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
      const loadedDecks = deckNames
        .map((name) => {
          const deck = loadDeck(name);
          return deck ? { name, deck } : null;
        })
        .filter((entry): entry is { name: string; deck: ParsedDeck } => entry !== null);

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
  }, [deckNames, selectedFormat, selectedMatchType]);

  const filteredDeckNames = useMemo(() => {
    return deckNames.filter((deckName) => {
      const compatibility = compatibilities[deckName];
      if (!compatibility) return true;

      const selectedFormatCompatible = compatibility.selected_format_compatible;
      if (contextualFilter && activeFilter === contextualFilter && selectedFormatCompatible != null) {
        return selectedFormatCompatible;
      }

      switch (activeFilter) {
        case "standard":
          return compatibility.standard.compatible;
        case "commander":
          return compatibility.commander.compatible;
        case "bo3":
          return compatibility.bo3_ready;
        default:
          return true;
      }
    });
  }, [deckNames, compatibilities, activeFilter, contextualFilter]);

  const { userDecks, bundledDecks } = useMemo(() => {
    const dir = sortAsc ? 1 : -1;
    const sortNames = (names: string[]): string[] => {
      if (activeSort === "alpha") return [...names].sort((a, b) => a.localeCompare(b) * dir);
      return [...names].sort((a, b) => {
        const metaA = getDeckMeta(a);
        const metaB = getDeckMeta(b);
        return ((metaA?.addedAt ?? 0) - (metaB?.addedAt ?? 0)) * dir;
      });
    };

    const user: string[] = [];
    const bundled: string[] = [];
    for (const name of filteredDeckNames) {
      if (isBundledDeck(name)) {
        bundled.push(name);
      } else {
        user.push(name);
      }
    }
    return { userDecks: sortNames(user), bundledDecks: sortNames(bundled) };
  }, [filteredDeckNames, activeSort, sortAsc]);

  const noDeckSelected = mode === "select"
    ? !activeDeckName || !filteredDeckNames.includes(activeDeckName)
    : false;
  const selectedDeckLabel = mode === "select" && activeDeckName && filteredDeckNames.includes(activeDeckName)
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

  return (
    <MenuPanel className="flex w-full max-w-5xl flex-col items-center gap-6 px-4 py-5">
      <div className="flex w-full items-center justify-between gap-3">
        <h2 className="menu-display text-[1.9rem] leading-tight text-white">
          {mode === "manage" ? "My Decks" : "Select Deck"}
        </h2>
        {mode === "manage" && (
          <button
            onClick={onCreateDeck}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            Create New
          </button>
        )}
      </div>

      <div className="flex w-full flex-wrap items-center gap-2">
        <button
          onClick={() => setActiveFilter("all")}
          className={`rounded px-2 py-1 text-xs font-medium ${
            activeFilter === "all"
              ? "bg-white/10 text-white"
              : "bg-black/18 text-slate-400 hover:bg-white/8 hover:text-white"
          }`}
        >
          All
        </button>
        <button
          onClick={() => setActiveFilter("standard")}
          className={`rounded px-2 py-1 text-xs font-medium ${
            activeFilter === "standard"
              ? "bg-white/10 text-white"
              : "bg-black/18 text-slate-400 hover:bg-white/8 hover:text-white"
          }`}
        >
          Standard
        </button>
        <button
          onClick={() => setActiveFilter("commander")}
          className={`rounded px-2 py-1 text-xs font-medium ${
            activeFilter === "commander"
              ? "bg-white/10 text-white"
              : "bg-black/18 text-slate-400 hover:bg-white/8 hover:text-white"
          }`}
        >
          Commander
        </button>
        <button
          onClick={() => setActiveFilter("bo3")}
          className={`rounded px-2 py-1 text-xs font-medium ${
            activeFilter === "bo3"
              ? "bg-white/10 text-white"
              : "bg-black/18 text-slate-400 hover:bg-white/8 hover:text-white"
          }`}
        >
          BO3
        </button>
        {contextualFilter && activeFilter === contextualFilter && (
          <button
            onClick={() => setActiveFilter("all")}
            className="rounded border border-indigo-500/50 bg-indigo-500/10 px-2 py-1 text-xs font-medium text-indigo-200 hover:bg-indigo-500/20"
          >
            Show all decks
          </button>
        )}
        {isEvaluating && (
          <span className="text-xs text-gray-500">Evaluating compatibility…</span>
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
      </div>

      {compatibilityError && (
        <div className="w-full rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          Compatibility check unavailable: {compatibilityError}
        </div>
      )}

      {filteredDeckNames.length === 0 ? (
        <div className="flex w-full flex-col items-center justify-center gap-4 rounded-[20px] border border-dashed border-white/10 bg-black/12 px-6 py-12 text-center">
          <div className="text-lg font-medium text-white">No decks match this filter.</div>
          <div className="max-w-md text-sm leading-6 text-slate-400">
            Pick a different filter or show all decks to choose from your full collection.
          </div>
          <button
            onClick={() => setActiveFilter("all")}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            Show All Decks
          </button>
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
              {userDecks.map((deckName) => (
                <DeckTile
                  key={deckName}
                  deckName={deckName}
                  isActive={deckName === activeDeckName}
                  compatibility={compatibilities[deckName]}
                  onClick={() => handleTileClick(deckName)}
                />
              ))}

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
            </div>
          </div>

          {/* Bundled decks section */}
          {bundledDecks.length > 0 && (
            <div>
              <h3 className="mb-3 text-xs font-semibold uppercase tracking-wider text-slate-400">
                Starter Decks
                <span className="ml-2 text-slate-600">{bundledDecks.length}</span>
              </h3>
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

      {mode === "select" && (
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
    </MenuPanel>
  );
}
