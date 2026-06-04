import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { ScryfallCard } from "../../services/scryfall";
import { resolveOracleIdSync } from "../../services/scryfall";
import { usePreferencesStore } from "../../stores/preferencesStore";
import type { ParsedDeck, DeckEntry } from "../../services/deckParser";
import { deduplicateEntries, resolveCommander } from "../../services/deckParser";
import { evaluateDeckCompatibility, type DeckCompatibilityResult } from "../../services/deckCompatibility";
import {
  ACTIVE_DECK_KEY,
  STORAGE_KEY_PREFIX,
  loadSavedDeck,
  loadSavedDeckBracket,
  removeDeckMeta,
  stampDeckMeta,
} from "../../constants/storage";
import { BASIC_LAND_NAMES } from "../../constants/game";
import { loadPreconDeckMap } from "../../hooks/useDecks";
import { preconDeckEntryToParsedDeck } from "../../services/preconDecks";
import { useDeckCardData } from "../../hooks/useDeckCardData";
import type { CardSearchFilters } from "./CardSearch";
import { hasSearchCriteria } from "./searchFilters";
import type { GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY, formatMetadata } from "../../data/formatRegistry";
import type { CommanderBracket } from "../../types/bracket";
import { getPreconBracket } from "../../data/preconBrackets";
import { getSharedAdapter } from "../../adapter/wasm-adapter";
import { useBracketEstimate } from "../../hooks/useBracketEstimate";
import {
  getColorIdentityViolations,
  getSingletonViolations,
} from "./commanderUtils";
import {
  commanderPartnerCandidates,
  deckCopyLimit,
  isCardCommanderEligibleForFormat,
} from "../../services/engineRuntime";

const PRECON_PREFIX = "[Pre-built] ";

function listSavedDecks(): string[] {
  const keys: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (key?.startsWith(STORAGE_KEY_PREFIX)) {
      keys.push(key.slice(STORAGE_KEY_PREFIX.length));
    }
  }
  return keys.sort();
}

interface UseDeckBuilderParams {
  format: GameFormat;
  onFormatChange: (format: GameFormat) => void;
  initialDeckName?: string | null;
  searchFilters: CardSearchFilters;
}

export function useDeckBuilder({
  format,
  onFormatChange,
  initialDeckName = null,
  searchFilters,
}: UseDeckBuilderParams) {
  const { t } = useTranslation("deck-builder");
  const [deck, setDeck] = useState<ParsedDeck>({ main: [], sideboard: [] });
  const [searchResults, setSearchResults] = useState<ScryfallCard[]>([]);
  const [deckName, setDeckName] = useState("");
  const [bracket, setBracket] = useState<CommanderBracket | null>(null);
  const [savedDecks, setSavedDecks] = useState(listSavedDecks);
  const [savedDeckName, setSavedDeckName] = useState<string | null>(null);
  const [justSaved, setJustSaved] = useState(false);
  const [commanders, setCommanders] = useState<string[]>([]);
  // Which surface is foregrounded on phone (tablet/desktop show columns and
  // ignore this). Deck-first: the main canvas (deck or, while searching, the
  // results grid) is the default; "info" is the commander + stats rail.
  const [activeSurface, setActiveSurface] = useState<"deck" | "info" | "playtest">("deck");
  // Visual representation of the deck within the main canvas.
  const [deckView, setDeckView] = useState<"list" | "stack">("list");
  // Unsaved-changes flag: set on any deck mutation, cleared on save/clone/load.
  // Drives the leave/load confirmation and the beforeunload guard.
  const [dirty, setDirty] = useState(false);
  const { cardDataCache, cacheCards } = useDeckCardData([
    ...deck.main.map((entry) => entry.name),
    ...deck.sideboard.map((entry) => entry.name),
    ...commanders,
  ]);

  const [compatibility, setCompatibility] = useState<DeckCompatibilityResult | null>(null);
  const [commanderEligibleNames, setCommanderEligibleNames] = useState<Set<string>>(new Set());
  // CR 100.2a / CR 903.5b: per-card deck-construction copy-limit overrides, keyed
  // by card name. `Infinity` = "any number"; a finite cap = "up to N" / singleton.
  // Populated from the engine — never inferred from Oracle text client-side.
  const [deckCopyLimits, setDeckCopyLimits] = useState<Map<string, number>>(new Map());

  const artOverrides = usePreferencesStore((s) => s.artOverrides);
  const clearArtOverride = usePreferencesStore((s) => s.clearArtOverride);
  const [listContextMenu, setListContextMenu] = useState<{ cardName: string; x: number; y: number } | null>(null);
  const [listPickerCard, setListPickerCard] = useState<{ cardName: string; oracleId: string } | null>(null);

  const handleListContextMenu = useCallback((cardName: string, x: number, y: number) => {
    setListContextMenu({ cardName, x, y });
  }, []);

  const handleListChooseArt = useCallback(() => {
    if (!listContextMenu) return;
    const oracleId = resolveOracleIdSync(listContextMenu.cardName);
    if (oracleId) {
      setListPickerCard({ cardName: listContextMenu.cardName, oracleId });
    }
  }, [listContextMenu]);

  const handleListClearOverride = useCallback(() => {
    if (!listContextMenu) return;
    const oracleId = resolveOracleIdSync(listContextMenu.cardName);
    if (oracleId) clearArtOverride(oracleId);
  }, [listContextMenu, clearArtOverride]);

  // Touch-friendly art selection: opens the printing picker directly (the picker
  // has both choose-art and use-default), so the alternate-art badge can be a
  // tap target on mobile where right-click context menus don't exist.
  const handleOpenArtPicker = useCallback((cardName: string) => {
    const oracleId = resolveOracleIdSync(cardName);
    if (oracleId) setListPickerCard({ cardName, oracleId });
  }, []);
  const currentDeck = useMemo<ParsedDeck>(() => ({
    ...deck,
    commander: commanders.length > 0 ? commanders : undefined,
  }), [deck, commanders]);

  // Stable key for deck contents to debounce compatibility evaluation
  const deckKey = useMemo(
    () => [
      ...deck.main.map((e) => `${e.count}x${e.name}`),
      "//",
      ...deck.sideboard.map((e) => `${e.count}x${e.name}`),
      "//",
      ...commanders,
    ].join("|"),
    [deck, commanders],
  );

  useEffect(() => {
    if (currentDeck.main.length === 0 && currentDeck.sideboard.length === 0) {
      setCompatibility(null);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      evaluateDeckCompatibility(currentDeck).then((result) => {
        if (!cancelled) setCompatibility(result);
      }).catch(() => {
        // WASM may not be loaded yet; silently ignore
      });
    }, 300);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [currentDeck, deckKey]);

  const formatConfig = formatMetadata(format)?.default_config;
  const isCommander = formatConfig?.command_zone ?? false;
  const expectedDeckSize = formatConfig?.deck_size ?? 60;
  const maxCopies = formatConfig?.singleton ? 1 : 4;

  useEffect(() => {
    if (!isCommander) {
      setCommanderEligibleNames(new Set());
      return;
    }
    const names = deck.main.map((entry) => entry.name);
    if (names.length === 0) {
      setCommanderEligibleNames(new Set());
      return;
    }
    let cancelled = false;
    Promise.all(
      names.map(async (name) => [
        name,
        await isCardCommanderEligibleForFormat(name, format),
      ] as const),
    ).then((results) => {
      if (cancelled) return;
      setCommanderEligibleNames(
        new Set(results.filter(([, eligible]) => eligible).map(([name]) => name)),
      );
    }).catch(() => {
      if (!cancelled) setCommanderEligibleNames(new Set());
    });
    return () => {
      cancelled = true;
    };
  }, [deck.main, format, isCommander]);

  // CR 100.2a / CR 903.5b: resolve per-card copy-limit overrides for every card
  // referenced anywhere in the deck. The engine is the single authority — the
  // frontend never re-parses Oracle text. Mirrors the commander-eligibility
  // effect's union-of-names keying and cancellation guard.
  useEffect(() => {
    const names = Array.from(
      new Set([
        ...deck.main.map((e) => e.name),
        ...deck.sideboard.map((e) => e.name),
        ...commanders,
      ]),
    );
    if (names.length === 0) {
      setDeckCopyLimits(new Map());
      return;
    }
    let cancelled = false;
    Promise.all(
      names.map(async (name) => [name, await deckCopyLimit(name)] as const),
    ).then((results) => {
      if (cancelled) return;
      const next = new Map<string, number>();
      for (const [name, limit] of results) {
        if (!limit) continue;
        next.set(name, limit.type === "Unlimited" ? Infinity : limit.data);
      }
      setDeckCopyLimits(next);
    }).catch(() => {
      if (!cancelled) setDeckCopyLimits(new Map());
    });
    return () => {
      cancelled = true;
    };
  }, [deck.main, deck.sideboard, commanders]);

  // Synchronous per-card effective copy cap: the override when present, else the
  // format default (4 constructed / 1 singleton).
  const effectiveCap = useCallback(
    (name: string) => deckCopyLimits.get(name) ?? maxCopies,
    [deckCopyLimits, maxCopies],
  );

  const { estimate, unsupported: bracketUnsupported } = useBracketEstimate({
    deck,
    commanders,
    format,
    adapter: getSharedAdapter(),
  });

  const auditEmptyReason: "not-commander" | "no-commander" | "unsupported" | undefined =
    !isCommander
      ? "not-commander"
      : commanders.length === 0
        ? "no-commander"
        : bracketUnsupported
          ? "unsupported"
          : undefined;

  const handleScrollToCard = useCallback((cardName: string) => {
    // The target row only exists in the Deck surface's list view (CardEntryRow's
    // [data-card-name] node). The bracket-audit link that calls this lives in the
    // Stats surface, so bring the Deck list forward first; scrollIntoView on a
    // display:none ancestor is a no-op. Defer to the next frame so the surface is
    // laid out before we scroll.
    setActiveSurface("deck");
    setDeckView("list");
    requestAnimationFrame(() => {
      const node = document.querySelector<HTMLElement>(
        `[data-card-name="${cardName.toLowerCase()}"]`,
      );
      node?.scrollIntoView({ behavior: "smooth", block: "center" });
    });
  }, []);

  const handleSearchResults = useCallback(
    (cards: ScryfallCard[], total: number) => {
      // Results render in the main canvas (the "deck" surface). On phone, make
      // sure that surface is foregrounded so a search run from the Info tab or
      // the filter sheet is visible.
      if (!initialDeckName || total > 0 || hasSearchCriteria(searchFilters)) {
        setActiveSurface("deck");
      }
      setSearchResults(cards);
      cacheCards(cards);
    },
    [cacheCards, initialDeckName, searchFilters],
  );

  const handleSearchTrigger = useCallback(() => {
    setActiveSurface("deck");
  }, []);

  const handleAddCard = useCallback((card: ScryfallCard) => {
    cacheCards([card]);
    setDirty(true);

    setDeck((prev) => {
      const existing = prev.main.find((e) => e.name === card.name);
      if (existing && existing.count >= effectiveCap(card.name) && !BASIC_LAND_NAMES.has(card.name)) {
        return prev;
      }

      if (existing) {
        return {
          ...prev,
          main: prev.main.map((e) =>
            e.name === card.name ? { ...e, count: e.count + 1 } : e,
          ),
        };
      }
      return {
        ...prev,
        main: [...prev.main, { count: 1, name: card.name }],
      };
    });
  }, [cacheCards, effectiveCap]);

  const handleAddCardByName = useCallback((name: string) => {
    const card = cardDataCache.get(name);
    if (!card) return;
    handleAddCard(card);
  }, [cardDataCache, handleAddCard]);

  const handleRemoveCard = useCallback(
    (name: string, section: "main" | "sideboard") => {
      setDirty(true);
      setDeck((prev) => {
        const entries = prev[section];
        const existing = entries.find((e) => e.name === name);
        if (!existing) return prev;

        if (existing.count <= 1) {
          return {
            ...prev,
            [section]: entries.filter((e) => e.name !== name),
          };
        }
        return {
          ...prev,
          [section]: entries.map((e) =>
            e.name === name ? { ...e, count: e.count - 1 } : e,
          ),
        };
      });
    },
    [],
  );

  const handleMoveCard = useCallback(
    (name: string, from: "main" | "sideboard") => {
      const to: "main" | "sideboard" = from === "main" ? "sideboard" : "main";
      setDirty(true);
      setDeck((prev) => {
        const source = prev[from];
        const target = prev[to];
        const sourceEntry = source.find((e) => e.name === name);
        if (!sourceEntry) return prev;

        const targetEntry = target.find((e) => e.name === name);
        if (
          to === "main" &&
          targetEntry &&
          targetEntry.count >= effectiveCap(name) &&
          !BASIC_LAND_NAMES.has(name)
        ) {
          return prev;
        }

        const nextSource =
          sourceEntry.count <= 1
            ? source.filter((e) => e.name !== name)
            : source.map((e) =>
                e.name === name ? { ...e, count: e.count - 1 } : e,
              );

        const nextTarget = targetEntry
          ? target.map((e) =>
              e.name === name ? { ...e, count: e.count + 1 } : e,
            )
          : [...target, { count: 1, name }];

        return {
          ...prev,
          [from]: nextSource,
          [to]: nextTarget,
        };
      });
    },
    [effectiveCap],
  );

  const applyDeckToEditor = useCallback((next: ParsedDeck) => {
    setDeck({
      main: deduplicateEntries(next.main ?? []),
      sideboard: deduplicateEntries(next.sideboard ?? []),
      companion: next.companion,
    });
    setCommanders(next.commander ?? []);
    if (next.commander?.length && !isCommander) onFormatChange("Commander");
  }, [isCommander, onFormatChange]);

  const handleImport = useCallback((imported: ParsedDeck) => {
    applyDeckToEditor(imported);
    setDirty(true);
  }, [applyDeckToEditor]);

  const handleSave = async () => {
    if (!deckName.trim()) return;
    // Save-time commander inference: when a Commander-format deck is shaped
    // like a 100-singleton list with no explicit commander, ask the engine
    // (via resolveCommander → WASM isCardCommanderEligible) to pick one. This
    // is the architectural successor to the deleted reactive auto-resolve
    // effect — running here means the user is never surprised mid-edit, and
    // every persisted record has a commander when one is derivable.
    const resolved = isCommander ? await resolveCommander(currentDeck) : currentDeck;
    const inferred =
      (resolved.commander?.length ?? 0) > (currentDeck.commander?.length ?? 0);
    if (inferred) {
      // Reflect the engine's choice in the editor so the displayed state
      // matches what we're about to persist.
      applyDeckToEditor(resolved);
    }
    const payload: Record<string, unknown> = { ...resolved, format };
    if (bracket !== null) payload.bracket = bracket;
    const data = JSON.stringify(payload);
    const nextName = deckName.trim();
    if (
      savedDeckName
      && savedDeckName !== nextName
      && localStorage.getItem(STORAGE_KEY_PREFIX + savedDeckName) !== null
    ) {
      localStorage.removeItem(STORAGE_KEY_PREFIX + savedDeckName);
      removeDeckMeta(savedDeckName);
      if (localStorage.getItem(ACTIVE_DECK_KEY) === savedDeckName) {
        localStorage.setItem(ACTIVE_DECK_KEY, nextName);
      }
    }
    localStorage.setItem(STORAGE_KEY_PREFIX + nextName, data);
    stampDeckMeta(nextName);
    setSavedDeckName(nextName);
    setSavedDecks(listSavedDecks());
    setJustSaved(true);
    setDirty(false);
  };

  // Clone = explicit duplicate. Unlike Save (which renames the current deck in
  // place), this always writes a NEW key and leaves the original untouched, then
  // switches the editor to the copy so further edits/Saves target the clone.
  const handleClone = useCallback(() => {
    const base = deckName.trim() || "Untitled Deck";
    let cloneName = `${base} copy`;
    let suffix = 2;
    while (localStorage.getItem(STORAGE_KEY_PREFIX + cloneName) !== null) {
      cloneName = `${base} copy ${suffix++}`;
    }
    const payload: Record<string, unknown> = { ...currentDeck, format };
    if (bracket !== null) payload.bracket = bracket;
    localStorage.setItem(STORAGE_KEY_PREFIX + cloneName, JSON.stringify(payload));
    stampDeckMeta(cloneName);
    setDeckName(cloneName);
    setSavedDeckName(cloneName);
    setSavedDecks(listSavedDecks());
    setJustSaved(true);
    setDirty(false);
  }, [deckName, currentDeck, format, bracket]);

  useEffect(() => {
    if (!justSaved) return;
    const timer = setTimeout(() => setJustSaved(false), 1500);
    return () => clearTimeout(timer);
  }, [justSaved]);

  const handleLoad = useCallback(async (name: string) => {
    const parsed = loadSavedDeck(name);
    const data = localStorage.getItem(STORAGE_KEY_PREFIX + name);
    if (!parsed || !data) {
      if (!name.startsWith(PRECON_PREFIX)) return;
      const decks = await loadPreconDeckMap();
      const found = Object.entries(decks ?? {}).find(([, entry]) => PRECON_PREFIX + `${entry.name} (${entry.code})` === name);
      if (!found) return;
      const [deckId, deckEntry] = found;
      const resolved = await resolveCommander(preconDeckEntryToParsedDeck(deckEntry));
      applyDeckToEditor(resolved);
      setActiveSurface("deck");
      setDirty(false);
      setDeckName(`${deckEntry.name} (${deckEntry.code})`);
      setSavedDeckName(null);
      setBracket(getPreconBracket(deckId) ?? null);
      return;
    }
    const persisted = JSON.parse(data) as ParsedDeck & { format?: string };
    const resolved = await resolveCommander(parsed);
    applyDeckToEditor(resolved);
    setActiveSurface("deck");
    setDirty(false);
    if (persisted.format) {
      const match = FORMAT_REGISTRY.find(
        (m) => m.format.toLowerCase() === persisted.format!.toLowerCase(),
      );
      if (match) onFormatChange(match.format);
    } else if (resolved.commander?.length) {
      onFormatChange("Commander");
    }
    setDeckName(name);
    setSavedDeckName(name);
    setBracket(loadSavedDeckBracket(name));
  }, [applyDeckToEditor, onFormatChange]);

  const handleLoadRef = useRef(handleLoad);
  handleLoadRef.current = handleLoad;

  useEffect(() => {
    if (!initialDeckName) return;
    void handleLoadRef.current(initialDeckName);
  }, [initialDeckName]);

  // Set a card as commander with three-tier resolution:
  //   1. No commanders yet → add it.
  //   2. One commander and both have partner-family keywords → add as partner
  //      (CR 702.124 / 702.135 — pair stays together).
  //   3. Otherwise → swap: move existing commander(s) back to main and install
  //      the new card as sole commander. This is the swap UX users need when
  //      cycling through legendary creatures to pick the right commander.
  const handleSetCommander = useCallback(
    (cardName: string) => {
      if (!commanderEligibleNames.has(cardName)) return;
      // CR 702.124: a second pick joins as a co-commander only when the engine
      // confirms it legally pairs with the existing one; otherwise it swaps. The
      // pairing decision is queried on demand (authoritative at click time) so a
      // stale precomputed value can never misclassify an add as a swap.
      void (async () => {
        let isPartnerAdd = false;
        if (commanders.length === 1) {
          try {
            isPartnerAdd = (await commanderPartnerCandidates(commanders[0], [cardName])).includes(
              cardName,
            );
          } catch {
            return;
          }
        }
        setDirty(true);
        const displaced =
          isPartnerAdd || commanders.length === 0 ? [] : commanders;
        const nextCommanders = isPartnerAdd
          ? [...commanders, cardName]
          : [cardName];

        setCommanders(nextCommanders);
        setDeck((prev) => {
          // Remove the new commander from main, then re-introduce any displaced
          // commanders so they remain in the deck for the user to re-pick.
          const filtered = prev.main.filter((e) => e.name !== cardName);
          const restored = displaced.reduce<DeckEntry[]>((acc, name) => {
            const existing = acc.find((e) => e.name === name);
            if (existing) {
              return acc.map((e) =>
                e.name === name ? { ...e, count: e.count + 1 } : e,
              );
            }
            return [...acc, { count: 1, name }];
          }, filtered);
          return { ...prev, main: restored };
        });
      })();
    },
    [commanderEligibleNames, commanders],
  );

  // Eligibility predicate consulted by each main-deck row. The set is loaded
  // from the engine's format-aware command-zone predicate above.
  const isCommanderEligible = useCallback(
    (name: string) => {
      return commanderEligibleNames.has(name);
    },
    [commanderEligibleNames],
  );

  const handleRemoveCommander = useCallback((cardName: string) => {
    setDirty(true);
    setCommanders((prev) => prev.filter((n) => n !== cardName));
    // Add back to main deck
    setDeck((prev) => ({
      ...prev,
      main: [...prev.main, { count: 1, name: cardName }],
    }));
  }, []);

  // Compute CMC and color arrays for ManaCurve
  const cmcValues: number[] = [];
  const colorValues: string[] = [];
  for (const entry of deck.main) {
    const card = cardDataCache.get(entry.name);
    if (card) {
      for (let i = 0; i < entry.count; i++) {
        cmcValues.push(card.cmc);
        colorValues.push(card.color_identity?.join("") ?? "");
      }
    }
  }

  const cardCounts = new Map(deck.main.map((entry) => [entry.name, entry.count]));
  for (const commander of commanders) {
    cardCounts.set(commander, (cardCounts.get(commander) ?? 0) + 1);
  }

  // Compute validation warnings
  const warnings: string[] = [];
  if (isCommander) {
    const totalCards = deck.main.reduce((s, e) => s + e.count, 0) + commanders.length;
    if (totalCards > 0 && totalCards !== expectedDeckSize) {
      warnings.push(t("warnings.commanderCount", { count: totalCards, expected: expectedDeckSize }));
    }
    for (const name of getSingletonViolations(deck.main, cardDataCache, effectiveCap)) {
      warnings.push(t("warnings.singleton", { name }));
    }
    for (const name of getColorIdentityViolations(deck.main, commanders, cardDataCache)) {
      warnings.push(t("warnings.colorIdentity", { name }));
    }
  } else {
    const mainTotal = deck.main.reduce((s, e) => s + e.count, 0);
    if (mainTotal > 0 && mainTotal < 60) {
      warnings.push(t("warnings.minimumCount", { count: mainTotal }));
    }
    for (const entry of deck.main) {
      if (entry.count > effectiveCap(entry.name) && !BASIC_LAND_NAMES.has(entry.name)) {
        warnings.push(t("warnings.maxCopies", { name: entry.name, count: entry.count }));
      }
    }
  }
  // CR 702.139a: Warn if a companion card is also in the main deck (likely import error)
  if (deck.companion && deck.main.some((e) => e.name === deck.companion)) {
    warnings.push(t("warnings.companionInMain", { name: deck.companion }));
  }

  return {
    // State
    deck,
    searchResults,
    deckName,
    setDeckName,
    bracket,
    setBracket,
    savedDecks,
    justSaved,
    setJustSaved,
    commanders,
    activeSurface,
    setActiveSurface,
    deckView,
    setDeckView,
    dirty,
    cardDataCache,
    compatibility,
    artOverrides,
    listContextMenu,
    setListContextMenu,
    listPickerCard,
    setListPickerCard,
    // Derived
    currentDeck,
    isCommander,
    expectedDeckSize,
    estimate,
    auditEmptyReason,
    cmcValues,
    colorValues,
    cardCounts,
    warnings,
    // Handlers
    handleListContextMenu,
    handleListChooseArt,
    handleListClearOverride,
    handleOpenArtPicker,
    handleScrollToCard,
    handleSearchResults,
    handleSearchTrigger,
    handleAddCard,
    handleAddCardByName,
    handleRemoveCard,
    handleMoveCard,
    handleImport,
    handleSave,
    handleClone,
    handleLoad,
    handleSetCommander,
    isCommanderEligible,
    handleRemoveCommander,
    effectiveCap,
  };
}
