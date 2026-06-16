import { useState, useRef, useCallback, useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { scryfallLegalityKey, type ScryfallCard } from "../../services/scryfall";
import { searchCards } from "../../services/engineRuntime";
import type { GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { useSetList } from "../../hooks/useSetList";
import { hasSearchCriteria } from "./searchFilters";
import { MenuSelect } from "../ui/MenuSelect";

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
const FILTER_MENU_CLASS =
  "min-h-[44px] rounded-[16px] text-base sm:min-h-0 sm:text-sm";

export type BrowserLegalityFilter = "all" | GameFormat;

export interface CardSearchFilters {
  text: string;
  colors: string[];
  type: string;
  cmcMax?: number;
  sets: string[];
  browseFormat: BrowserLegalityFilter;
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
  const { t } = useTranslation("deck-builder");
  const browserFormats = useMemo<{ value: BrowserLegalityFilter; label: string }[]>(
    () => [
      { value: "all", label: t("search.browseFormat.all") },
      ...FORMAT_REGISTRY.map(({ format, label }) => ({
        value: format as BrowserLegalityFilter,
        label,
      })),
    ],
    [t],
  );
  const typeOptions = useMemo(
    () => [
      { value: "", label: t("search.allTypes") },
      ...CARD_TYPES.map((cardType) => ({ value: cardType, label: cardType })),
    ],
    [t],
  );
  const selectedTypeLabel =
    typeOptions.find((opt) => opt.value === filters.type)?.label ?? t("search.allTypes");
  const selectedBrowseFormatLabel =
    browserFormats.find((opt) => opt.value === filters.browseFormat)?.label
    ?? t("search.browseFormat.all");
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

      // AbortController is the staleness guard: a newer search aborts this one
      // so its results never overwrite the latest. The search itself runs
      // locally through the engine (no network, no signal to pass).
      const controller = new AbortController();
      abortRef.current = controller;
      setLoading(true);
      setError(null);

      try {
        const { cards, total } = await searchCards({
          text: searchText || undefined,
          colors: colors.length > 0 ? colors : undefined,
          type: type || undefined,
          cmcMax: cmc,
          sets,
          legalFormat: browseFormat === "all" ? undefined : scryfallLegalityKey(browseFormat),
        });
        if (!controller.signal.aborted) {
          onResults(cards, total);
          setResultCount(total);
        }
      } catch (err) {
        if (!controller.signal.aborted) {
          setError(err instanceof Error ? err.message : t("search.searchFailed"));
          onResults([], 0);
          setResultCount(null);
        }
      } finally {
        if (!controller.signal.aborted) {
          setLoading(false);
        }
      }
    },
    [onResults, t],
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
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">{t("search.title")}</div>
          <div className="mt-1 text-sm text-slate-300">{t("search.subtitle")}</div>
        </div>
        <button
          type="button"
          onClick={onReset}
          className="rounded-full border border-white/10 bg-white/6 px-2.5 py-1 text-[0.68rem] uppercase tracking-[0.16em] text-slate-300 hover:bg-white/10 hover:text-white"
        >
          {t("search.reset")}
        </button>
      </div>

      <input
        type="text"
        value={filters.text}
        onChange={(e) => handleTextChange(e.target.value)}
        placeholder={t("search.textPlaceholder")}
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

      <MenuSelect
        ariaLabel={t("search.cardType")}
        label={selectedTypeLabel}
        selectedValue={filters.type}
        items={typeOptions}
        onSelect={handleTypeChange}
        menuLayout="dropdown"
        wrapperClassName="w-full"
        className={FILTER_MENU_CLASS}
      />

      <div className="flex items-center gap-2">
        <label className="text-xs text-gray-400">{t("search.cmcMax")}</label>
        <input
          type="number"
          min={0}
          max={16}
          value={filters.cmcMax ?? ""}
          onChange={(e) => handleCmcChange(e.target.value)}
          className="w-16 rounded-[12px] border border-white/10 bg-black/18 px-2 py-1 text-sm text-white focus:border-white/20 focus:outline-none"
        />
      </div>

      <MenuSelect
        ariaLabel={t("search.browseFormatFilter")}
        label={selectedBrowseFormatLabel}
        selectedValue={filters.browseFormat}
        items={browserFormats}
        onSelect={(value) => handleBrowseFormatChange(value as BrowserLegalityFilter)}
        menuLayout="dropdown"
        wrapperClassName="w-full"
        className={FILTER_MENU_CLASS}
      />

      <div className="space-y-2">
        <label className="text-xs text-gray-400">{t("search.sets")}</label>
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
            placeholder={t("search.addSetPlaceholder")}
            className="min-w-0 flex-1 rounded-[16px] border border-white/10 bg-black/18 px-3 py-2 text-sm text-white placeholder-gray-500 focus:border-white/20 focus:outline-none"
          />
          <button
            type="button"
            onClick={handleAddSet}
            disabled={!setInput.trim()}
            className="rounded-[16px] border border-white/10 bg-white/10 px-3 py-2 text-xs font-medium text-white hover:bg-white/14 disabled:opacity-40"
          >
            {t("search.addSet")}
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
                  title={t("search.removeSet", { name: setName })}
                >
                  {setCode} x
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="text-xs text-gray-400">
        {!loading && resultCount === null && !error && !hasSearchCriteria(filters) && t("search.emptyHint")}
        {loading && t("search.searching")}
        {!loading && resultCount !== null && t("search.results", { count: resultCount })}
        {error && <span className="text-red-400">{error}</span>}
      </div>
    </div>
  );
}
