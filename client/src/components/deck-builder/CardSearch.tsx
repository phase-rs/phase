import { useState, useRef, useCallback, useEffect, useMemo } from "react";
import {
  searchScryfall,
  buildScryfallQuery,
  scryfallLegalityKey,
  type ScryfallCard,
} from "../../services/scryfall";
import type { GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { useSetList } from "../../hooks/useSetList";

const DEBOUNCE_MS = 300;
const MANA_COLORS = ["W", "U", "B", "R", "G"] as const;
const COLOR_LABELS: Record<string, string> = {
  W: "White",
  U: "Blue",
  B: "Black",
  R: "Red",
  G: "Green",
};
const COLOR_STYLES: Record<string, string> = {
  W: "bg-amber-100 text-amber-900",
  U: "bg-blue-500 text-white",
  B: "bg-gray-800 text-gray-100",
  R: "bg-red-600 text-white",
  G: "bg-green-600 text-white",
};
const CARD_TYPES = [
  "Creature",
  "Instant",
  "Sorcery",
  "Enchantment",
  "Artifact",
  "Land",
  "Planeswalker",
];

const BROWSER_FORMATS: { value: BrowserLegalityFilter; label: string }[] = [
  { value: "all", label: "All cards" },
  ...FORMAT_REGISTRY.map(({ format, label }) => ({
    value: format as BrowserLegalityFilter,
    label,
  })),
];

export type BrowserLegalityFilter = "all" | GameFormat;

export interface CardSearchFilters {
  text: string;
  colors: string[];
  type: string;
  cmcMax?: number;
  sets: string[];
  browseFormat: BrowserLegalityFilter;
}

function hasSearchCriteria(filters: CardSearchFilters): boolean {
  return Boolean(
    filters.text
      || filters.colors.length > 0
      || filters.type
      || filters.cmcMax !== undefined
      || filters.sets.length > 0,
  );
}

interface CardSearchProps {
  onResults: (cards: ScryfallCard[], total: number) => void;
  onSearchTrigger?: () => void;
  filters: CardSearchFilters;
  onFiltersChange: (filters: CardSearchFilters) => void;
  onReset: () => void;
}

export function CardSearch({
  onResults,
  onSearchTrigger,
  filters,
  onFiltersChange,
  onReset,
}: CardSearchProps) {
  const setList = useSetList();
  const availableSets = useMemo(() => {
    if (!setList) return [];
    return Object.values(setList)
      .filter((set) => !set.isOnlineOnly)
      .sort((left, right) => {
        const leftDate = left.releaseDate ?? "";
        const rightDate = right.releaseDate ?? "";
        if (leftDate !== rightDate) return rightDate.localeCompare(leftDate);
        return left.code.localeCompare(right.code);
      });
  }, [setList]);
  const [setInput, setSetInput] = useState("");
  const [loading, setLoading] = useState(false);
  const [resultCount, setResultCount] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  const abortRef = useRef<AbortController | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const doSearch = useCallback(
    async (
      searchText: string,
      colors: string[],
      type: string,
      cmc: number | undefined,
      sets: string[],
      browseFormat: BrowserLegalityFilter,
    ) => {
      abortRef.current?.abort();

      const nextFilters: CardSearchFilters = {
        text: searchText,
        colors,
        type,
        cmcMax: cmc,
        sets,
        browseFormat,
      };

      if (!hasSearchCriteria(nextFilters)) {
        onResults([], 0);
        setResultCount(null);
        setLoading(false);
        setError(null);
        return;
      }

      const query = buildScryfallQuery({
        text: searchText || undefined,
        colors: colors.length > 0 ? colors : undefined,
        type: type || undefined,
        cmcMax: cmc,
        sets,
        format: browseFormat === "all" ? undefined : scryfallLegalityKey(browseFormat),
      });

      if (!query) {
        onResults([], 0);
        setResultCount(null);
        return;
      }

      const controller = new AbortController();
      abortRef.current = controller;
      setLoading(true);
      setError(null);

      try {
        const { cards, total } = await searchScryfall(query, controller.signal);
        if (!controller.signal.aborted) {
          onResults(cards, total);
          setResultCount(total);
        }
      } catch (err) {
        if (!controller.signal.aborted) {
          setError(err instanceof Error ? err.message : "Search failed");
          onResults([], 0);
          setResultCount(null);
        }
      } finally {
        if (!controller.signal.aborted) {
          setLoading(false);
        }
      }
    },
    [onResults],
  );

  const scheduleSearch = useCallback(
    (nextFilters: CardSearchFilters) => {
      if (hasSearchCriteria(nextFilters)) {
        onSearchTrigger?.();
      }
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(
        () => doSearch(
          nextFilters.text,
          nextFilters.colors,
          nextFilters.type,
          nextFilters.cmcMax,
          nextFilters.sets,
          nextFilters.browseFormat,
        ),
        DEBOUNCE_MS,
      );
    },
    [doSearch, onSearchTrigger],
  );

  useEffect(() => {
    return () => {
      abortRef.current?.abort();
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, []);

  useEffect(() => {
    scheduleSearch(filters);
  }, [filters, scheduleSearch]);

  const handleTextChange = (value: string) => {
    onFiltersChange({
      ...filters,
      text: value,
    });
  };

  const toggleColor = (color: string) => {
    const next = filters.colors.includes(color)
      ? filters.colors.filter((c) => c !== color)
      : [...filters.colors, color];
    onFiltersChange({
      ...filters,
      colors: next,
    });
  };

  const handleTypeChange = (type: string) => {
    onFiltersChange({
      ...filters,
      type,
    });
  };

  const handleCmcChange = (value: string) => {
    const cmc = value === "" ? undefined : parseInt(value, 10);
    onFiltersChange({
      ...filters,
      cmcMax: cmc,
    });
  };

  const handleBrowseFormatChange = (value: BrowserLegalityFilter) => {
    onFiltersChange({
      ...filters,
      browseFormat: value,
    });
  };

  const resolveSetCode = useCallback((value: string) => {
    const normalized = value.trim().toLowerCase();
    if (!normalized || !setList) return null;

    const byCode = Object.values(setList).find(
      (set) => set.code.toLowerCase() === normalized,
    );
    if (byCode) return byCode.code;

    const byName = Object.values(setList).find(
      (set) => set.name.toLowerCase() === normalized,
    );
    return byName?.code ?? null;
  }, [setList]);

  const handleAddSet = useCallback(() => {
    const setCode = resolveSetCode(setInput);
    if (!setCode) return;
    if (filters.sets.includes(setCode)) {
      setSetInput("");
      return;
    }

    setSetInput("");
    onFiltersChange({
      ...filters,
      sets: [...filters.sets, setCode],
    });
  }, [filters, onFiltersChange, resolveSetCode, setInput]);

  const handleRemoveSet = useCallback((setCode: string) => {
    onFiltersChange({
      ...filters,
      sets: filters.sets.filter((code) => code !== setCode),
    });
  }, [filters, onFiltersChange]);

  return (
    <div className="flex flex-col gap-3 p-3">
      <div className="flex items-start justify-between gap-2">
        <div>
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Search</div>
          <div className="mt-1 text-sm text-slate-300">Add cards to the current list.</div>
        </div>
        <button
          type="button"
          onClick={onReset}
          className="rounded-full border border-white/10 bg-white/6 px-2.5 py-1 text-[0.68rem] uppercase tracking-[0.16em] text-slate-300 hover:bg-white/10 hover:text-white"
        >
          Reset
        </button>
      </div>

      <input
        type="text"
        value={filters.text}
        onChange={(e) => handleTextChange(e.target.value)}
        placeholder="Search cards..."
        className="w-full rounded-[16px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-white/20 focus:outline-none"
      />

      <div className="flex gap-1">
        {MANA_COLORS.map((c) => (
          <button
            key={c}
            onClick={() => toggleColor(c)}
            title={COLOR_LABELS[c]}
            className={`h-8 w-8 rounded-full text-xs font-bold transition-opacity ${COLOR_STYLES[c]} ${
              filters.colors.includes(c) ? "opacity-100 ring-2 ring-white/50" : "opacity-45"
            }`}
          >
            {c}
          </button>
        ))}
      </div>

      <select
        value={filters.type}
        onChange={(e) => handleTypeChange(e.target.value)}
        className="rounded-[16px] border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-white focus:border-white/20 focus:outline-none"
      >
        <option value="">All types</option>
        {CARD_TYPES.map((t) => (
          <option key={t} value={t}>
            {t}
          </option>
        ))}
      </select>

      <div className="flex items-center gap-2">
        <label className="text-xs text-gray-400">CMC max:</label>
        <input
          type="number"
          min={0}
          max={16}
          value={filters.cmcMax ?? ""}
          onChange={(e) => handleCmcChange(e.target.value)}
          className="w-16 rounded-[12px] border border-white/10 bg-black/18 px-2 py-1 text-sm text-white focus:border-white/20 focus:outline-none"
        />
      </div>

      <select
        value={filters.browseFormat}
        onChange={(e) => handleBrowseFormatChange(e.target.value as BrowserLegalityFilter)}
        className="rounded-[16px] border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-white focus:border-white/20 focus:outline-none"
      >
        {BROWSER_FORMATS.map(({ value, label }) => (
          <option key={value} value={value}>
            {label}
          </option>
        ))}
      </select>

      <div className="space-y-2">
        <label className="text-xs text-gray-400">Sets</label>
        <div className="flex gap-2">
          <input
            type="text"
            value={setInput}
            onChange={(e) => setSetInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                handleAddSet();
              }
            }}
            list="deck-builder-set-list"
            placeholder="Add set code..."
            className="min-w-0 flex-1 rounded-[16px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-white/20 focus:outline-none"
          />
          <button
            type="button"
            onClick={handleAddSet}
            disabled={!setInput.trim()}
            className="rounded-[16px] border border-white/10 bg-white/10 px-3 py-2 text-xs font-medium text-white hover:bg-white/14 disabled:opacity-40"
          >
            Add
          </button>
        </div>
        <datalist id="deck-builder-set-list">
          {availableSets.map((set) => (
            <option key={set.code} value={set.code}>
              {`${set.code} - ${set.name}`}
            </option>
          ))}
        </datalist>
        {filters.sets.length > 0 && (
          <div className="flex flex-wrap gap-1.5">
            {filters.sets.map((setCode) => {
              const setName = setList?.[setCode]?.name ?? setCode;
              return (
                <button
                  key={setCode}
                  type="button"
                  onClick={() => handleRemoveSet(setCode)}
                  className="rounded-full border border-white/10 bg-white/10 px-2.5 py-1 text-xs text-slate-200 hover:bg-white/14"
                  title={`Remove ${setName}`}
                >
                  {setCode} x
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="text-xs text-gray-400">
        {!loading && resultCount === null && !error && !hasSearchCriteria(filters) && "Add a filter to start browsing"}
        {loading && "Searching..."}
        {!loading && resultCount !== null && `${resultCount} results`}
        {error && <span className="text-red-400">{error}</span>}
      </div>
    </div>
  );
}
