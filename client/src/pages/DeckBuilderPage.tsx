import { useCallback, useMemo, useState } from "react";
import { useSearchParams } from "react-router";

import type { GameFormat } from "../adapter/types";
import { useAudioContext } from "../audio/useAudioContext";
import { CardPreview } from "../components/card/CardPreview";
import { DeckBuilder } from "../components/deck-builder/DeckBuilder";
import type { BrowserLegalityFilter, CardSearchFilters } from "../components/deck-builder/CardSearch";
import { DECK_CONSTRUCTION_FORMATS } from "../data/formatRegistry";
import { useAltToggle } from "../hooks/useAltToggle";

const DEFAULT_DECK_FORMAT: GameFormat = "Standard";
const DEFAULT_SEARCH_FILTERS: CardSearchFilters = {
  text: "",
  colors: [],
  type: "",
  cmcMax: undefined,
  sets: [],
  browseFormat: "all",
};

function parseDeckFormat(value: string | null): GameFormat {
  if (!value) return DEFAULT_DECK_FORMAT;
  const match = DECK_CONSTRUCTION_FORMATS.find(
    (m) => m.format.toLowerCase() === value.toLowerCase(),
  );
  return match?.format ?? DEFAULT_DECK_FORMAT;
}

function parseBrowseFormat(value: string | null): BrowserLegalityFilter {
  if (value == null || value === "") return "all";
  if (value === "all") return "all";
  return parseDeckFormat(value);
}

function parseSearchFilters(searchParams: URLSearchParams): CardSearchFilters {
  const cmcRaw = searchParams.get("cmcMax");
  const cmcMax = cmcRaw ? Number.parseInt(cmcRaw, 10) : undefined;

  return {
    text: searchParams.get("q") ?? "",
    colors: (searchParams.get("colors") ?? "")
      .split(",")
      .map((color) => color.trim())
      .filter(Boolean),
    type: searchParams.get("type") ?? "",
    cmcMax: Number.isFinite(cmcMax) ? cmcMax : undefined,
    sets: (searchParams.get("sets") ?? "")
      .split(",")
      .map((setCode) => setCode.trim().toUpperCase())
      .filter(Boolean),
    browseFormat: parseBrowseFormat(searchParams.get("browseFormat")),
  };
}

export function DeckBuilderPage() {
  useAudioContext("deck_builder");
  useAltToggle();
  const [searchParams, setSearchParams] = useSearchParams();
  const [hoveredCard, setHoveredCard] = useState<{ name: string; scryfallId?: string } | null>(null);
  const format = parseDeckFormat(searchParams.get("format"));
  const initialDeckName = searchParams.get("create") === "1"
    ? null
    : searchParams.get("deck");
  const searchFilters = useMemo(
    () => parseSearchFilters(searchParams),
    [searchParams],
  );

  const backPath = useMemo(() => {
    const returnTo = searchParams.get("returnTo");
    if (!returnTo) return "/";
    if (!returnTo.startsWith("/") || returnTo.startsWith("//")) return "/";
    return returnTo;
  }, [searchParams]);

  const updateSearchParams = useCallback((next: {
    format?: GameFormat;
    searchFilters?: CardSearchFilters;
  }) => {
    const params = new URLSearchParams(searchParams);
    const nextFormat = next.format ?? format;
    const nextSearchFilters = next.searchFilters ?? searchFilters;

    if (nextFormat === DEFAULT_DECK_FORMAT) params.delete("format");
    else params.set("format", nextFormat.toLowerCase());

    if (nextSearchFilters.text) params.set("q", nextSearchFilters.text);
    else params.delete("q");

    if (nextSearchFilters.colors.length > 0) params.set("colors", nextSearchFilters.colors.join(","));
    else params.delete("colors");

    if (nextSearchFilters.type) params.set("type", nextSearchFilters.type);
    else params.delete("type");

    if (nextSearchFilters.cmcMax !== undefined) params.set("cmcMax", String(nextSearchFilters.cmcMax));
    else params.delete("cmcMax");

    if (nextSearchFilters.sets.length > 0) params.set("sets", nextSearchFilters.sets.join(","));
    else params.delete("sets");

    if (nextSearchFilters.browseFormat === DEFAULT_SEARCH_FILTERS.browseFormat) params.delete("browseFormat");
    else params.set("browseFormat", nextSearchFilters.browseFormat.toLowerCase());

    setSearchParams(params, { replace: true });
  }, [format, searchFilters, searchParams, setSearchParams]);

  const handleFormatChange = useCallback((nextFormat: GameFormat) => {
    updateSearchParams({ format: nextFormat });
  }, [updateSearchParams]);

  const handleSearchFiltersChange = useCallback((nextSearchFilters: CardSearchFilters) => {
    updateSearchParams({ searchFilters: nextSearchFilters });
  }, [updateSearchParams]);

  const handleResetSearch = useCallback(() => {
    updateSearchParams({ searchFilters: DEFAULT_SEARCH_FILTERS });
  }, [updateSearchParams]);

  return (
    <div className="menu-scene deck-builder-shell flex flex-col overflow-hidden">
      <DeckBuilder
        onCardHover={useCallback((name: string | null, scryfallId?: string) => {
          setHoveredCard(name ? { name, scryfallId } : null);
        }, [])}
        format={format}
        onFormatChange={handleFormatChange}
        initialDeckName={initialDeckName}
        backPath={backPath}
        searchFilters={searchFilters}
        onSearchFiltersChange={handleSearchFiltersChange}
        onResetSearch={handleResetSearch}
      />
      <CardPreview
        cardName={hoveredCard?.name ?? null}
        scryfallId={hoveredCard?.scryfallId}
        onDismiss={useCallback(() => setHoveredCard(null), [])}
        mobileLayout="compact"
      />
    </div>
  );
}
