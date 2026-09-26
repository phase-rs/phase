import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { ScryfallCard } from "../../services/scryfall";
import { resolveOracleIdSync } from "../../services/scryfall";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useAppNotificationStore } from "../../stores/appToastStore";
import type { ParsedDeck, DeckEntry } from "../../services/deckParser";
import { deduplicateEntries, expandParsedDeck, resolveCommander } from "../../services/deckParser";
import { evaluateDeckCompatibility, type DeckCompatibilityResult } from "../../services/deckCompatibility";
import {
  STORAGE_KEY_PREFIX,
  captureSavedDeck,
  freeDeckName,
  getDeckMeta,
  listFolders,
  loadSavedDeck,
  loadSavedDeckBracket,
  saveBuilderDeck,
  setDeckFolder,
  stampDeckMeta,
  writeSavedDeckData,
  type SavedDeckSnapshot,
} from "../../constants/storage";
import { withSavedDeckLibrary } from "../../services/savedDeckTransaction";
import { attemptSavedDeckWrite } from "../../services/savedDeckWriteFailure";
import { loadPreconDeckMap } from "../../hooks/useDecks";
import { preconDeckEntryToParsedDeck } from "../../services/preconDecks";
import { useDeckCardData } from "../../hooks/useDeckCardData";
import type { CardSearchFilters } from "./CardSearch";
import { hasSearchCriteria } from "./searchFilters";
import type { GroupMode } from "./deckGrouping";
import type { DeckSizeRule, GameFormat } from "../../adapter/types";
import { DECK_CONSTRUCTION_FORMATS, formatMetadata } from "../../data/formatRegistry";
import type { CommanderBracket } from "../../types/bracket";
import { getPreconBracket } from "../../data/preconBrackets";
import { getSharedAdapter } from "../../adapter/wasm-adapter";
import { useBracketEstimate } from "../../hooks/useBracketEstimate";
import { projectSignatureSpellForFormat, serializeSavedDeck } from "../../services/savedDeckProjection";
import {
  commanderPartnerCandidates,
  companionCandidates,
  isCardCommanderEligibleForFormat,
  maxDeckCopies,
  signatureSpellSelectionPolicy,
  type DeckCopyLimit,
} from "../../services/engineRuntime";

const PRECON_PREFIX = "[Pre-built] ";

/** "saved-then-changed": the write committed, but the editor changed while it was pending, so it no longer holds what was written. */
export type SaveOutcome = "refused" | "saved" | "saved-then-changed";

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
  const showNotification = useAppNotificationStore((s) => s.showNotification);
  const [deck, setDeck] = useState<ParsedDeck>({ main: [], sideboard: [] });
  const [searchResults, setSearchResults] = useState<ScryfallCard[]>([]);
  const [deckName, setDeckName] = useState("");
  const [bracket, setBracket] = useState<CommanderBracket | null>(null);
  const [savedDecks, setSavedDecks] = useState(listSavedDecks);
  // The deck currently loaded/saved in the editor, by name and stored bytes — not React state:
  // `saveBuilderDeck` and Clone update it inside their own transaction body, with the bytes they
  // just wrote, so a save queued behind an earlier one to the same name sees that write and not
  // a value captured before it (see saveBuilderDeck's doc).
  const savedDeckRef = useRef<SavedDeckSnapshot | null>(null);
  const [justSaved, setJustSaved] = useState(false);
  const [commanders, setCommanders] = useState<string[]>([]);
  // Which surface is foregrounded on phone (tablet/desktop show columns and
  // ignore this). Deck-first: the main canvas (deck or, while searching, the
  // results grid) is the default; "info" is the commander + stats rail.
  const [activeSurface, setActiveSurface] = useState<"deck" | "info">("deck");
  // Visual representation of the deck within the main canvas.
  const [deckView, setDeckView] = useState<"list" | "stack">("list");
  // How the main deck is sub-grouped within the canvas (by card type or color).
  const [groupMode, setGroupMode] = useState<GroupMode>("type");
  // Unsaved-changes flag: set on any deck mutation.
  // Drives the leave/load confirmation and the beforeunload guard.
  const [dirty, setDirty] = useState(false);
  // Bumped by every edit (markDirty).
  const editRevision = useRef(0);
  const markDirty = useCallback(() => {
    editRevision.current += 1;
    setDirty(true);
  }, []);
  // Bumped whenever the editor switches to another deck: a Load, or a Clone that opens its copy.
  const deckIdentityRevision = useRef(0);
  interface EditorCapture {
    identity: number;
    edit: number;
  }
  // Captured before an await, so the work after it can tell whether a Load, Clone or edit happened meanwhile.
  const captureEditor = useCallback(
    (): EditorCapture => ({ identity: deckIdentityRevision.current, edit: editRevision.current }),
    [],
  );
  const editorChangedSince = useCallback(
    (captured: EditorCapture) => ({
      reloaded: captured.identity !== deckIdentityRevision.current,
      edited: captured.edit !== editRevision.current,
    }),
    [],
  );
  const { cardDataCache, cacheCards } = useDeckCardData([
    ...deck.main.map((entry) => entry.name),
    ...deck.sideboard.map((entry) => entry.name),
    ...(deck.planar_deck ?? []),
    ...(deck.scheme_deck ?? []),
    ...(deck.signature_spell ?? []),
    ...(deck.companion ? [deck.companion] : []),
    ...commanders,
  ]);

  const [compatibility, setCompatibility] = useState<DeckCompatibilityResult | null>(null);
  const [commanderEligibleNames, setCommanderEligibleNames] = useState<Set<string>>(new Set());
  const [signatureSpellCandidates, setSignatureSpellCandidates] = useState<string[] | null>(null);
  const [companionCandidateNames, setCompanionCandidateNames] = useState<string[] | null>(null);

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
  const formatConfig = formatMetadata(format)?.default_config;

  // Stable key for deck contents to debounce compatibility evaluation
  const deckKey = useMemo(
    () => [
      ...deck.main.map((e) => `${e.count}x${e.name}`),
      "//",
      ...deck.sideboard.map((e) => `${e.count}x${e.name}`),
      "//",
      ...(deck.planar_deck ?? []),
      "//",
      ...(deck.scheme_deck ?? []),
      "//",
      ...(deck.signature_spell ?? []),
      "//",
      deck.companion ?? "",
      "//",
      ...commanders,
    ].join("|"),
    [deck, commanders],
  );

  useEffect(() => {
    if (
      currentDeck.main.length === 0
      && currentDeck.sideboard.length === 0
      && (currentDeck.planar_deck?.length ?? 0) === 0
      && (currentDeck.scheme_deck?.length ?? 0) === 0
    ) {
      setCompatibility(null);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      evaluateDeckCompatibility(currentDeck, { selectedFormat: format }).then((result) => {
        if (!cancelled) setCompatibility(result);
      }).catch(() => {
        // WASM may not be loaded yet; silently ignore
      });
    }, 300);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [currentDeck, deckKey, format]);

  useEffect(() => {
    let cancelled = false;
    const request = { ...expandParsedDeck(currentDeck), selected_format: format };
    Promise.all([signatureSpellSelectionPolicy(request), companionCandidates(request)])
      .then(([signaturePolicy, companionCandidates]) => {
        if (cancelled) return;
        setSignatureSpellCandidates(
          signaturePolicy.type === "Required" ? signaturePolicy.data.candidates : null,
        );
        setCompanionCandidateNames(
          formatConfig?.uses_commander ? companionCandidates : null,
        );
      })
      .catch(() => {
        if (!cancelled) {
          setSignatureSpellCandidates(null);
          setCompanionCandidateNames(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [currentDeck, deckKey, format, formatConfig?.uses_commander]);

  const isCommander = formatConfig?.command_zone ?? false;
  const deckSizeRule: DeckSizeRule = formatConfig?.deck_size ?? { type: "Minimum", data: 60 };

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
    markDirty();

    setDeck((prev) => {
      const existing = prev.main.find((e) => e.name === card.name);
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
  }, [cacheCards, markDirty]);

  const handleAddCardByName = useCallback((name: string) => {
    const card = cardDataCache.get(name);
    if (!card) return;
    handleAddCard(card);
  }, [cardDataCache, handleAddCard]);

  const handleRemoveCard = useCallback(
    (name: string, section: "main" | "sideboard") => {
      markDirty();
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
    [markDirty],
  );

  // CR 100.4a: the copy limit applies to the main deck, sideboard, and command
  // zone combined, so the increment gate counts every slot a card can occupy.
  const combinedCopyCounts = useMemo(() => {
    const counts = new Map<string, number>();
    const add = (name: string, n: number) =>
      counts.set(name, (counts.get(name) ?? 0) + n);
    for (const entry of deck.main) add(entry.name, entry.count);
    for (const entry of deck.sideboard) add(entry.name, entry.count);
    for (const name of commanders) add(name, 1);
    for (const name of deck.signature_spell ?? []) add(name, 1);
    if (deck.companion) add(deck.companion, 1);
    return counts;
  }, [deck, commanders]);

  // Distinct names currently in the partition, as a stable key — the ceiling
  // for a (name, format) pair never changes, so this only refetches when the
  // set of names or the format actually changes, not on every count edit.
  const copyLimitKey = useMemo(
    () =>
      [
        ...new Set([...deck.main, ...deck.sideboard].map((entry) => entry.name)),
      ]
        .sort()
        .join("|"),
    [deck.main, deck.sideboard],
  );

  // CR 100.2a / CR 903.5b: the ceiling is engine-resolved per card and format
  // (basic-land exemption, printed overrides like Relentless Rats or Seven
  // Dwarves, four-of vs singleton default). The builder never re-derives it.
  const [copyLimits, setCopyLimits] = useState<Map<string, DeckCopyLimit>>(
    () => new Map(),
  );
  useEffect(() => {
    const names = copyLimitKey ? copyLimitKey.split("|") : [];
    // The engine resolves the ceiling from the whole `FormatConfig`, so an
    // unresolved config (unknown format) leaves the map empty rather than
    // guessing a default client-side.
    if (names.length === 0 || !formatConfig) {
      setCopyLimits(new Map());
      return;
    }
    let cancelled = false;
    Promise.all(
      names.map(
        async (name) => [name, await maxDeckCopies(name, formatConfig)] as const,
      ),
    )
      .then((results) => {
        if (!cancelled) setCopyLimits(new Map(results));
      })
      .catch(() => {
        // WASM may not be loaded yet; an empty map leaves increments open and
        // the engine's compatibility warnings still flag any real violation.
        if (!cancelled) setCopyLimits(new Map());
      });
    return () => {
      cancelled = true;
    };
  }, [copyLimitKey, formatConfig]);

  const canIncrement = useCallback(
    (name: string) => {
      const limit = copyLimits.get(name);
      if (!limit || limit.type === "Unlimited") return true;
      return (combinedCopyCounts.get(name) ?? 0) < limit.data;
    },
    [copyLimits, combinedCopyCounts],
  );

  const handleIncrementCard = useCallback(
    (name: string, section: "main" | "sideboard") => {
      if (!canIncrement(name)) return;
      markDirty();
      setDeck((prev) => {
        const entries = prev[section];
        if (!entries.some((e) => e.name === name)) return prev;
        return {
          ...prev,
          [section]: entries.map((e) =>
            e.name === name ? { ...e, count: e.count + 1 } : e,
          ),
        };
      });
    },
    [canIncrement, markDirty],
  );

  const handleMoveCard = useCallback(
    (name: string, from: "main" | "sideboard") => {
      const to: "main" | "sideboard" = from === "main" ? "sideboard" : "main";
      markDirty();
      setDeck((prev) => {
        const source = prev[from];
        const target = prev[to];
        const sourceEntry = source.find((e) => e.name === name);
        if (!sourceEntry) return prev;

        const targetEntry = target.find((e) => e.name === name);

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
    [markDirty],
  );

  // User edits to the name, format and bracket advance editRevision like card edits, so a save that completes
  // after one reports "saved-then-changed". Re-selecting the current format or bracket is not an edit.
  // handleLoad calls the raw setters instead, so loading a deck is not an edit.
  const handleDeckNameChange = useCallback((name: string) => {
    markDirty();
    setDeckName(name);
  }, [markDirty]);

  const handleFormatChange = useCallback((next: GameFormat) => {
    if (next === format) return;
    markDirty();
    onFormatChange(next);
  }, [format, markDirty, onFormatChange]);

  const handleBracketChange = useCallback((next: CommanderBracket | null) => {
    if (next === bracket) return;
    markDirty();
    setBracket(next);
  }, [bracket, markDirty]);

  const applyDeckToEditor = useCallback((next: ParsedDeck, targetFormat: GameFormat = format) => {
    const projected = projectSignatureSpellForFormat(next, targetFormat);
    const targetUsesCommander = formatMetadata(targetFormat)?.default_config.uses_commander ?? false;
    const companionInDedicatedSlot = targetUsesCommander ? projected.companion : undefined;
    const sideboard = !targetUsesCommander && projected.companion
      && !projected.sideboard.some((entry) => entry.name === projected.companion)
      ? [...projected.sideboard, { count: 1, name: projected.companion }]
      : projected.sideboard;
    setDeck({
      main: deduplicateEntries(projected.main ?? []),
      sideboard: deduplicateEntries(sideboard ?? []),
      planar_deck: projected.planar_deck ? [...projected.planar_deck] : undefined,
      scheme_deck: projected.scheme_deck ? [...projected.scheme_deck] : undefined,
      signature_spell: projected.signature_spell ? [...projected.signature_spell] : undefined,
      companion: companionInDedicatedSlot,
    });
    setCommanders(projected.commander ?? []);
    if (projected.commander?.length && !isCommander) onFormatChange("Commander");
  }, [format, isCommander, onFormatChange]);

  const handleImport = useCallback((imported: ParsedDeck) => {
    applyDeckToEditor(imported);
    markDirty();
  }, [applyDeckToEditor, markDirty]);

  const handleSave = useCallback(async (): Promise<SaveOutcome> => {
    if (!deckName.trim()) return "refused";
    const captured = captureEditor();
    // Fixed now, before any await: the name this save should vacate if renamed, and the bytes
    // it should still hold, so a Load of a different deck queued behind this save cannot retarget it.
    const previous = savedDeckRef.current;
    // Save-time commander inference: when a Commander-format deck is shaped
    // like a 100-singleton list with no explicit commander, ask the engine
    // (via resolveCommander → WASM isCardCommanderEligible) to pick one. This
    // is the architectural successor to the deleted reactive auto-resolve
    // effect — running here means the user is never surprised mid-edit, and
    // every persisted record has a commander when one is derivable.
    const resolved = isCommander ? await resolveCommander(currentDeck) : currentDeck;
    const inferred =
      (resolved.commander?.length ?? 0) > (currentDeck.commander?.length ?? 0);
    const changed = editorChangedSince(captured);
    if (inferred && !changed.reloaded && !changed.edited) {
      // Reflect the engine's choice in the editor so the displayed state
      // matches what we're about to persist.
      applyDeckToEditor(resolved);
    }
    const data = serializeSavedDeck(resolved, format, bracket);
    const nextName = deckName.trim();
    const claimsEditor = () => !editorChangedSince(captured).reloaded;
    const saved = await attemptSavedDeckWrite("save", () =>
      saveBuilderDeck(previous, savedDeckRef, claimsEditor, nextName, data),
    );
    if (!saved.ok) return "refused";
    const after = editorChangedSince(captured);
    if (!after.reloaded) {
      setJustSaved(true);
      if (!after.edited) setDirty(false);
      showNotification({
        title: t("toolbar.savedToastTitle"),
        description: t("toolbar.savedToastDescription", { name: nextName }),
      });
    }
    setSavedDecks(listSavedDecks());
    return after.reloaded || after.edited ? "saved-then-changed" : "saved";
  }, [
    deckName,
    captureEditor,
    editorChangedSince,
    isCommander,
    currentDeck,
    applyDeckToEditor,
    format,
    bracket,
    showNotification,
    t,
  ]);

  // Clone = explicit duplicate. Unlike Save (which renames the current deck in
  // place), this always writes a NEW key and leaves the original untouched.
  const handleClone = useCallback(async () => {
    const captured = captureEditor();
    const sourceAtClick = savedDeckRef.current;
    // The clone's folder is decided at the click:
    // capture it now, before any await lets a Load or rename-Save race this transaction.
    const folderAtClick = sourceAtClick ? getDeckMeta(sourceAtClick.name)?.folderId ?? null : null;
    const base = deckName.trim() || "Untitled Deck";
    const data = serializeSavedDeck(currentDeck, format, bracket);
    const cloned = await attemptSavedDeckWrite("clone", () =>
      withSavedDeckLibrary((txn) => {
        const name = freeDeckName(txn, `${base} copy`, (i) => `${base} copy ${i}`);
        const copy = writeSavedDeckData(txn, name, data);
        stampDeckMeta(txn, name);
        // A clone lands beside its source: inherit the click-time folder, but start
        // unstarred (the star is a deliberate per-deck pin, not a copyable property).
        // The folder may have been deleted while this transaction waited, so only
        // assign it if it still exists — otherwise the copy would carry a dangling id.
        if (folderAtClick && listFolders().some((f) => f.id === folderAtClick)) {
          setDeckFolder(txn, name, folderAtClick);
        }
        const c = editorChangedSince(captured);
        // An edit alone must also block the claim, or the next Save moves this copy onto
        // the still-open source.
        if (!c.reloaded && !c.edited) savedDeckRef.current = copy;
        return copy;
      }),
    );
    if (!cloned.ok) return;
    const cloneName = cloned.value.name;
    setSavedDecks(listSavedDecks());
    showNotification({
      title: t("toolbar.clonedToastTitle"),
      description: t("toolbar.clonedToastDescription", { name: cloneName }),
    });
    const changed = editorChangedSince(captured);
    if (changed.reloaded || changed.edited) return;
    deckIdentityRevision.current += 1;
    setDeckName(cloneName);
    setJustSaved(true);
    setDirty(false);
  }, [deckName, captureEditor, editorChangedSince, currentDeck, format, bracket, showNotification, t]);

  useEffect(() => {
    if (!justSaved) return;
    const timer = setTimeout(() => setJustSaved(false), 1500);
    return () => clearTimeout(timer);
  }, [justSaved]);

  const handleLoad = useCallback(async (name: string) => {
    // Captured before the resolveCommander await below: a newer Load/Clone (reloaded) or any edit
    // that marks the deck dirty, including an Import (edited), must win over this Load.
    const captured = captureEditor();
    const parsed = loadSavedDeck(name);
    const stored = captureSavedDeck(name);
    if (!parsed || stored.raw === null) {
      if (!name.startsWith(PRECON_PREFIX)) return;
      const decks = await loadPreconDeckMap();
      const found = Object.entries(decks ?? {}).find(([, entry]) => PRECON_PREFIX + `${entry.name} (${entry.code})` === name);
      if (!found) return;
      const [deckId, deckEntry] = found;
      const resolved = await resolveCommander(preconDeckEntryToParsedDeck(deckEntry));
      const changedAfterPrecon = editorChangedSince(captured);
      if (changedAfterPrecon.reloaded || changedAfterPrecon.edited) return;
      applyDeckToEditor(resolved);
      setActiveSurface("deck");
      deckIdentityRevision.current += 1;
      setDirty(false);
      setDeckName(`${deckEntry.name} (${deckEntry.code})`);
      savedDeckRef.current = null;
      setBracket(getPreconBracket(deckId) ?? null);
      return;
    }
    const persisted = JSON.parse(stored.raw) as ParsedDeck & { format?: string };
    const resolved = await resolveCommander(parsed);
    const changedAfterLoad = editorChangedSince(captured);
    if (changedAfterLoad.reloaded || changedAfterLoad.edited) return;
    const savedFormat = persisted.format
      ? DECK_CONSTRUCTION_FORMATS.find(
          (metadata) => metadata.format.toLowerCase() === persisted.format!.toLowerCase(),
        )?.format
      : undefined;
    applyDeckToEditor(resolved, savedFormat);
    setActiveSurface("deck");
    deckIdentityRevision.current += 1;
    setDirty(false);
    if (savedFormat) {
      onFormatChange(savedFormat);
    } else if (resolved.commander?.length) {
      onFormatChange("Commander");
    }
    setDeckName(name);
    savedDeckRef.current = stored;
    setBracket(loadSavedDeckBracket(name));
  }, [applyDeckToEditor, onFormatChange, captureEditor, editorChangedSince]);

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
            // CR 903.13f(3): an EMPTY list = constructed play, which grants no
            // extra partner ability — so every existing caller keeps today's
            // behaviour and a constructed Commander deck is unaffected.
            // Discharged in phase 8 where it belongs: the DRAFT deckbuilder
            // (`LimitedDeckBuilder`) passes the engine-latched
            // `DraftPlayerView.draft_set_codes`. This call is constructed play,
            // which has no draft behind it, so `[]` is the rules-correct
            // argument here and must stay.
            isPartnerAdd = (
              await commanderPartnerCandidates(commanders[0], [cardName], [])
            ).includes(cardName);
          } catch {
            return;
          }
        }
        markDirty();
        const displaced =
          isPartnerAdd || commanders.length === 0 ? [] : commanders;
        const nextCommanders = isPartnerAdd
          ? [...commanders, cardName]
          : [cardName];

        setCommanders(nextCommanders);
        setDeck((prev) => {
          // CR 903.3: the designation is "an attribute of the card itself" —
          // a label on ONE card, not a removal of every copy. Take one copy
          // and leave the rest, using the count-aware shape this same file
          // already uses in `moveOneMainCardToSpecialSlot` below. Then
          // re-introduce any displaced commanders so they remain in the deck
          // for the user to re-pick.
          const selected = prev.main.find((entry) => entry.name === cardName);
          const filtered =
            selected?.count === 1
              ? prev.main.filter((entry) => entry.name !== cardName)
              : prev.main.map((entry) =>
                  entry.name === cardName
                    ? { ...entry, count: entry.count - 1 }
                    : entry,
                );
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
    [commanderEligibleNames, commanders, markDirty],
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
    markDirty();
    setCommanders((prev) => prev.filter((n) => n !== cardName));
    // CR 903.3: the exact inverse of `handleSetCommander`'s decrement. Routed
    // through the merge this file already uses so a card still present in main
    // gains a copy rather than gaining a DUPLICATE ROW; designate-then-remove
    // therefore returns the deck to its exact prior multiset, for any count.
    setDeck((prev) => ({
      ...prev,
      main: deduplicateEntries([...prev.main, { count: 1, name: cardName }]),
    }));
  }, [markDirty]);

  const moveOneMainCardToSpecialSlot = useCallback(
    (cardName: string, slot: "signature_spell" | "companion") => {
      markDirty();
      setDeck((prev) => {
        const selected = prev.main.find((entry) => entry.name === cardName);
        if (!selected) return prev;
        const prior = slot === "signature_spell" ? prev.signature_spell?.[0] : prev.companion;
        const mainWithoutSelected = selected.count === 1
          ? prev.main.filter((entry) => entry.name !== cardName)
          : prev.main.map((entry) => entry.name === cardName
            ? { ...entry, count: entry.count - 1 }
            : entry);
        const main = prior && prior !== cardName
          ? deduplicateEntries([...mainWithoutSelected, { count: 1, name: prior }])
          : mainWithoutSelected;
        return slot === "signature_spell"
          ? { ...prev, main, signature_spell: [cardName] }
          : { ...prev, main, companion: cardName };
      });
    },
    [markDirty],
  );

  const handleSetSignatureSpell = useCallback((cardName: string) => {
    if (!signatureSpellCandidates?.includes(cardName)) return;
    moveOneMainCardToSpecialSlot(cardName, "signature_spell");
  }, [moveOneMainCardToSpecialSlot, signatureSpellCandidates]);

  const handleRemoveSignatureSpell = useCallback(() => {
    markDirty();
    setDeck((prev) => {
      const signature = prev.signature_spell?.[0];
      if (!signature) return prev;
      return {
        ...prev,
        signature_spell: [],
        main: deduplicateEntries([...prev.main, { count: 1, name: signature }]),
      };
    });
  }, [markDirty]);

  const handleSetCompanion = useCallback((cardName: string) => {
    if (!companionCandidateNames?.includes(cardName)) return;
    moveOneMainCardToSpecialSlot(cardName, "companion");
  }, [companionCandidateNames, moveOneMainCardToSpecialSlot]);

  const handleRemoveCompanion = useCallback(() => {
    markDirty();
    setDeck((prev) => {
      if (!prev.companion) return prev;
      return {
        ...prev,
        companion: undefined,
        main: deduplicateEntries([...prev.main, { count: 1, name: prev.companion }]),
      };
    });
  }, [markDirty]);

  // Card metadata supplies CMCs; the engine supplies color identities.
  const cmcValues: number[] = [];
  for (const entry of deck.main) {
    const card = cardDataCache.get(entry.name);
    if (card) {
      for (let i = 0; i < entry.count; i++) {
        cmcValues.push(card.cmc);
      }
    }
  }
  const colorDistribution = compatibility?.color_distribution ?? [];

  const cardCounts = new Map(deck.main.map((entry) => [entry.name, entry.count]));
  for (const commander of commanders) {
    cardCounts.set(commander, (cardCounts.get(commander) ?? 0) + 1);
  }
  for (const signatureSpell of deck.signature_spell ?? []) {
    cardCounts.set(signatureSpell, (cardCounts.get(signatureSpell) ?? 0) + 1);
  }

  // Engine-driven validation — duplicate legality, color identity, and deck size
  // all come from evaluateDeckCompatibility (selected_format_reasons).
  const warnings: string[] = [
    ...(compatibility?.selected_format_reasons ?? []),
  ];
  // CR 702.139a: Warn if a companion card is also in the main deck (likely import error)
  if (deck.companion && deck.main.some((e) => e.name === deck.companion)) {
    warnings.push(t("warnings.companionInMain", { name: deck.companion }));
  }

  return {
    // State
    deck,
    searchResults,
    deckName,
    bracket,
    savedDecks,
    justSaved,
    setJustSaved,
    commanders,
    activeSurface,
    setActiveSurface,
    deckView,
    setDeckView,
    groupMode,
    setGroupMode,
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
    deckSizeRule,
    estimate,
    auditEmptyReason,
    cmcValues,
    colorDistribution,
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
    handleIncrementCard,
    canIncrement,
    handleMoveCard,
    handleImport,
    handleDeckNameChange,
    handleFormatChange,
    handleBracketChange,
    handleSave,
    handleClone,
    handleLoad,
    handleSetCommander,
    isCommanderEligible,
    handleRemoveCommander,
    signatureSpellCandidates,
    companionCandidateNames,
    handleSetSignatureSpell,
    handleRemoveSignatureSpell,
    handleSetCompanion,
    handleRemoveCompanion,
  };
}
