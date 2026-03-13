import { useState, useRef, useCallback, useEffect } from "react";
import {
  searchScryfall,
  buildScryfallQuery,
  type ScryfallCard,
} from "../../services/scryfall";

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

interface CardSearchProps {
  onResults: (cards: ScryfallCard[], total: number) => void;
  onSearchTrigger?: () => void;
  format?: string;
}

export function CardSearch({
  onResults,
  onSearchTrigger,
  format = "standard",
}: CardSearchProps) {
  const [text, setText] = useState("");
  const [selectedColors, setSelectedColors] = useState<string[]>([]);
  const [selectedType, setSelectedType] = useState("");
  const [cmcMax, setCmcMax] = useState<number | undefined>(undefined);
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
    ) => {
      abortRef.current?.abort();

      const query = buildScryfallQuery({
        text: searchText || undefined,
        colors: colors.length > 0 ? colors : undefined,
        type: type || undefined,
        cmcMax: cmc,
        format,
      });

      if (!query || query === `f:${format}`) {
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
    [onResults, format],
  );

  const scheduleSearch = useCallback(
    (
      searchText: string,
      colors: string[],
      type: string,
      cmc: number | undefined,
    ) => {
      onSearchTrigger?.();
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(
        () => doSearch(searchText, colors, type, cmc),
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

  const handleTextChange = (value: string) => {
    setText(value);
    scheduleSearch(value, selectedColors, selectedType, cmcMax);
  };

  const toggleColor = (color: string) => {
    const next = selectedColors.includes(color)
      ? selectedColors.filter((c) => c !== color)
      : [...selectedColors, color];
    setSelectedColors(next);
    scheduleSearch(text, next, selectedType, cmcMax);
  };

  const handleTypeChange = (type: string) => {
    setSelectedType(type);
    scheduleSearch(text, selectedColors, type, cmcMax);
  };

  const handleCmcChange = (value: string) => {
    const cmc = value === "" ? undefined : parseInt(value, 10);
    setCmcMax(cmc);
    scheduleSearch(text, selectedColors, selectedType, cmc);
  };

  return (
    <div className="flex flex-col gap-3 p-3">
      <div>
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Search</div>
        <div className="mt-1 text-sm text-slate-300">Add cards to the current list.</div>
      </div>

      <input
        type="text"
        value={text}
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
              selectedColors.includes(c) ? "opacity-100 ring-2 ring-white/50" : "opacity-45"
            }`}
          >
            {c}
          </button>
        ))}
      </div>

      <select
        value={selectedType}
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
          value={cmcMax ?? ""}
          onChange={(e) => handleCmcChange(e.target.value)}
          className="w-16 rounded-[12px] border border-white/10 bg-black/18 px-2 py-1 text-sm text-white focus:border-white/20 focus:outline-none"
        />
      </div>

      <div className="text-xs text-gray-400">
        {loading && "Searching..."}
        {!loading && resultCount !== null && `${resultCount} results`}
        {error && <span className="text-red-400">{error}</span>}
      </div>
    </div>
  );
}
