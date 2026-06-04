import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { useIsMobile } from "../../hooks/useIsMobile";
import { hasAlternatePrintingsSync, resolveOracleIdSync } from "../../services/scryfall";
import { DeckCardContextMenu } from "./DeckCardContextMenu";
import { PrintingPickerModal } from "./PrintingPickerModal";
import { CardSearch } from "./CardSearch";
import type { CardSearchFilters } from "./CardSearch";
import { hasSearchCriteria } from "./searchFilters";
import { CardGrid } from "./CardGrid";
import { DeckStack } from "./DeckStack";
import { DeckList } from "./DeckList";
import { StatsPanel } from "./StatsPanel";
import type { GameFormat } from "../../adapter/types";
import { CommanderPanel } from "./CommanderPanel";
import { DeckBuilderToolbar } from "./DeckBuilderToolbar";
import { DeckBuilderTabBar } from "./DeckBuilderTabBar";
import { PlaytestPanel } from "./PlaytestPanel";
import { panelId, tabId } from "./deckBuilderTabs";
import { useDeckBuilder } from "./useDeckBuilder";

interface DeckBuilderProps {
  onCardHover?: (cardName: string | null, scryfallId?: string) => void;
  format: GameFormat;
  onFormatChange: (format: GameFormat) => void;
  initialDeckName?: string | null;
  backPath?: string;
  searchFilters: CardSearchFilters;
  onSearchFiltersChange: (filters: CardSearchFilters) => void;
  onResetSearch: () => void;
}

export function DeckBuilder({
  onCardHover,
  format,
  onFormatChange,
  initialDeckName = null,
  backPath = "/",
  searchFilters,
  onSearchFiltersChange,
  onResetSearch,
}: DeckBuilderProps) {
  const {
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
    currentDeck,
    isCommander,
    expectedDeckSize,
    estimate,
    auditEmptyReason,
    cmcValues,
    colorValues,
    cardCounts,
    warnings,
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
  } = useDeckBuilder({ format, onFormatChange, initialDeckName, searchFilters });
  const { t } = useTranslation("deck-builder");

  // Deck-first: the main canvas shows the deck unless a search is active, in
  // which case it shows the results grid (cleared via "Back to deck").
  const searchActive = hasSearchCriteria(searchFilters);
  const deckCount = deck.main.reduce((sum, e) => sum + e.count, 0) + commanders.length;

  // Filters are a collapsible rail (lg+) / overlay sheet (below lg), shown on
  // demand so the deck canvas owns the space by default. useIsMobile flips at the
  // same 1024px boundary as the rail's `lg:static`, so it cleanly distinguishes
  // "modal sheet" (mobile/tablet) from "inline rail" (desktop) — only the former
  // gets dialog semantics + a focus trap.
  const isMobile = useIsMobile();
  const filterPanelRef = useRef<HTMLDivElement>(null);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const filtersAsDialog = filtersOpen && isMobile;
  useEffect(() => {
    if (!filtersOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFiltersOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [filtersOpen]);

  // Focus management for the filter sheet (mobile/tablet overlay only — the lg+
  // inline rail is part of the page flow and needs no trap). On open: move focus
  // into the sheet and trap Tab within it; on close: restore focus to whatever
  // opened it (the Search trigger).
  useEffect(() => {
    if (!filtersAsDialog) return;
    const panel = filterPanelRef.current;
    if (!panel) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    const focusables = () =>
      panel.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      );
    focusables()[0]?.focus();

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const items = focusables();
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    panel.addEventListener("keydown", onKeyDown);
    return () => {
      panel.removeEventListener("keydown", onKeyDown);
      // Restore focus to the opener — but only if it's still visible. If the
      // sheet was closed programmatically (e.g. a surface switch that hid the
      // opener's section), focusing a display:none node silently drops focus to
      // <body>; getClientRects() is empty for hidden/disconnected nodes.
      if (previouslyFocused && previouslyFocused.getClientRects().length > 0) {
        previouslyFocused.focus();
      }
    };
  }, [filtersAsDialog]);
  // Dismiss the filter rail/sheet when switching surfaces (good UX: changing
  // tabs closes the overlay). The rail is a sibling of the main section — not a
  // descendant — so this is purely UX, not a visibility correctness guard.
  useEffect(() => {
    setFiltersOpen(false);
  }, [activeSurface]);

  // Unsaved-changes guard. beforeunload covers tab close / refresh / browser
  // back; an in-app confirm covers the back button and loading another deck.
  const navigate = useNavigate();
  const [pendingAction, setPendingAction] = useState<
    { type: "back" } | { type: "load"; name: string } | null
  >(null);

  useEffect(() => {
    if (!dirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [dirty]);

  const performAction = useCallback(
    (action: { type: "back" } | { type: "load"; name: string }) => {
      if (action.type === "back") navigate(backPath);
      else handleLoad(action.name);
    },
    [navigate, backPath, handleLoad],
  );

  const requestBack = useCallback(() => {
    if (dirty) setPendingAction({ type: "back" });
    else navigate(backPath);
  }, [dirty, navigate, backPath]);

  const requestLoad = useCallback(
    (name: string) => {
      if (dirty) setPendingAction({ type: "load", name });
      else handleLoad(name);
    },
    [dirty, handleLoad],
  );

  const confirmSaveThen = useCallback(async () => {
    const action = pendingAction;
    await handleSave();
    setPendingAction(null);
    if (action) performAction(action);
  }, [pendingAction, handleSave, performAction]);

  const confirmDiscardThen = useCallback(() => {
    const action = pendingAction;
    setPendingAction(null);
    if (action) performAction(action);
  }, [pendingAction, performAction]);

  // Phone: tab bar picks one surface. md+: both columns show.
  const mainVisible = activeSurface === "deck" ? "flex" : "hidden md:flex";
  const infoVisible = activeSurface === "info" ? "flex" : "hidden md:flex";
  const playtestVisible = activeSurface === "playtest" ? "flex" : "hidden md:flex";

  return (
    <div className="flex h-screen flex-col bg-transparent">
      <DeckBuilderToolbar
        onBack={requestBack}
        deckName={deckName}
        onDeckNameChange={setDeckName}
        justSaved={justSaved && !dirty}
        onClearJustSaved={() => setJustSaved(false)}
        onSave={handleSave}
        onClone={handleClone}
        canClone={deckCount > 0}
        savedDecks={savedDecks}
        onLoad={requestLoad}
        format={format}
        onFormatChange={onFormatChange}
      />

      <DeckBuilderTabBar
        activeSurface={activeSurface}
        onSurfaceChange={setActiveSurface}
        deckCount={deckCount}
      />

      <div className="flex min-h-0 flex-1">
        {/* Filter rail backdrop (mobile overlay). */}
        {filtersOpen && (
          <button
            type="button"
            aria-label={t("filters.close")}
            onClick={() => setFiltersOpen(false)}
            className="fixed inset-0 z-30 bg-black/60 lg:hidden"
          />
        )}

        {/* Collapsible filter rail: inline sidebar at lg+ when open, overlay
            sheet below lg, hidden when closed. A single CardSearch instance
            (kept mounted so filter-driven searches keep running). */}
        <div
          ref={filterPanelRef}
          role={filtersAsDialog ? "dialog" : undefined}
          aria-modal={filtersAsDialog ? true : undefined}
          aria-label={filtersAsDialog ? t("filters.title") : undefined}
          className={
            filtersOpen
              ? "fixed inset-y-0 left-0 z-40 flex w-[min(20rem,85vw)] flex-col border-r border-white/10 bg-[#0b1020]/96 backdrop-blur-md lg:static lg:z-auto lg:w-64 lg:bg-black/12"
              : "hidden"
          }
        >
          <div className="flex shrink-0 items-center justify-between border-b border-white/8 px-4 py-3">
            <span className="text-sm font-semibold text-white">{t("filters.title")}</span>
            <button
              type="button"
              onClick={() => setFiltersOpen(false)}
              className="rounded-lg px-2 py-1 text-xs text-slate-300 hover:bg-white/10"
            >
              {t("filters.done")}
            </button>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto pb-16">
            <CardSearch
              onResults={handleSearchResults}
              onSearchTrigger={handleSearchTrigger}
              filters={searchFilters}
              onFiltersChange={onSearchFiltersChange}
              onReset={onResetSearch}
            />
          </div>
        </div>

        {/* Main canvas: the deck (idle) or search results (searching). Tab panel
            for the phone tab bar; on md+ it's simply a visible column (the
            controlling tab is display:none, but aria-labelledby still resolves
            its name from the hidden node, so the region stays labelled). */}
        <section
          id={panelId("deck")}
          role="tabpanel"
          aria-labelledby={tabId("deck")}
          className={`${mainVisible} min-h-0 min-w-0 flex-1 flex-col`}
        >
          <div className="flex shrink-0 items-center justify-between gap-2 border-b border-white/8 px-3 py-2">
            <button
              type="button"
              onClick={() => setFiltersOpen((v) => !v)}
              aria-pressed={filtersOpen}
              className={`flex items-center gap-1.5 rounded-xl border px-3 py-1.5 text-sm transition-colors ${
                filtersOpen || searchActive
                  ? "border-white/18 bg-white/10 text-white"
                  : "border-white/10 bg-black/18 text-slate-200 hover:bg-white/6"
              }`}
            >
              <FunnelIcon />
              {t("filters.search")}
            </button>

            {searchActive ? (
              <div className="flex items-center gap-2">
                <span className="text-xs text-slate-400">{t("search.results", { count: searchResults.length })}</span>
                <button
                  type="button"
                  onClick={onResetSearch}
                  className="rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-xs text-slate-200 hover:bg-white/6"
                >
                  &larr; {t("deck.backToDeck")}
                </button>
              </div>
            ) : (
              <div className="flex gap-1 rounded-lg border border-white/8 bg-black/18 p-0.5">
                {(["list", "stack"] as const).map((view) => (
                  <button
                    key={view}
                    type="button"
                    onClick={() => setDeckView(view)}
                    aria-label={view === "list" ? t("deck.listView") : t("deck.stackView")}
                    aria-pressed={deckView === view}
                    title={view === "list" ? t("deck.listView") : t("deck.stackView")}
                    className={`flex h-7 w-7 items-center justify-center rounded-md transition-colors ${
                      deckView === view
                        ? "bg-white/14 text-white"
                        : "text-slate-400 hover:text-slate-200"
                    }`}
                  >
                    {view === "list" ? <ListViewIcon /> : <StackViewIcon />}
                  </button>
                ))}
              </div>
            )}
          </div>

          <div className="min-h-0 flex-1 overflow-hidden">
            {searchActive ? (
              <div className="h-full overflow-y-auto pb-16">
                <CardGrid
                  cards={searchResults}
                  onAddCard={handleAddCard}
                  onCardHover={onCardHover}
                  cardCounts={cardCounts}
                  legalityFormat={searchFilters.browseFormat}
                />
              </div>
            ) : (
              // Deck view: validation warnings pin as a banner above the deck so
              // they stay visible in both list and stack views (and while
              // scrolling a long list), rather than being buried in the list-only
              // DeckList. Capped height so a deck with many violations can't
              // swallow the canvas.
              <div className="flex h-full flex-col">
                {warnings.length > 0 && (
                  <div className="max-h-32 shrink-0 space-y-0.5 overflow-y-auto border-b border-white/8 px-3 py-2">
                    {warnings.map((w) => (
                      <div
                        key={w}
                        className="rounded-xl border border-amber-300/18 bg-amber-400/8 px-2 py-1 text-xs text-amber-200"
                      >
                        {w}
                      </div>
                    ))}
                  </div>
                )}
                <div className="min-h-0 flex-1 overflow-hidden">
                  {deckView === "list" ? (
                    <div className="h-full overflow-y-auto px-3 pt-3 pb-16">
                      <DeckList
                        deck={currentDeck}
                        onRemoveCard={handleRemoveCard}
                        onMoveCard={handleMoveCard}
                        onImport={handleImport}
                        onCardHover={onCardHover}
                        format={format}
                        compatibility={compatibility}
                        onChooseArt={handleListContextMenu}
                        onSetAsCommander={isCommander ? handleSetCommander : undefined}
                        isCommanderEligible={isCommander ? isCommanderEligible : undefined}
                        onOpenArtPicker={handleOpenArtPicker}
                        commanders={commanders}
                        onRemoveCommander={handleRemoveCommander}
                      />
                    </div>
                  ) : (
                    <DeckStack
                      deck={deck}
                      commanders={commanders}
                      cardDataCache={cardDataCache}
                      onAddCard={handleAddCardByName}
                      onRemoveCard={handleRemoveCard}
                      onMoveCard={handleMoveCard}
                      onRemoveCommander={handleRemoveCommander}
                      onCardHover={onCardHover}
                      format={format}
                      getEffectiveCap={effectiveCap}
                    />
                  )}
                </div>
              </div>
            )}
          </div>
        </section>

        {/* Info rail: commander + stats (curve, colors, legality, coverage, bracket). */}
        <section
          id={panelId("info")}
          role="tabpanel"
          aria-labelledby={tabId("info")}
          className={`${infoVisible} min-h-0 w-full flex-col overflow-hidden border-white/8 bg-black/12 backdrop-blur-sm md:w-80 md:shrink-0 md:border-l lg:w-96`}
        >
          <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 pt-3 pb-16">
            {isCommander && (
              <CommanderPanel
                commanders={commanders}
                deck={deck.main}
                cardDataCache={cardDataCache}
                expectedDeckSize={expectedDeckSize}
                isCommanderEligible={isCommanderEligible}
                onSetCommander={handleSetCommander}
                onRemoveCommander={handleRemoveCommander}
                onCardHover={onCardHover}
                getEffectiveCap={effectiveCap}
              />
            )}
            <StatsPanel
              compatibility={compatibility}
              cmcValues={cmcValues}
              colorValues={colorValues}
              isCommander={isCommander}
              estimate={estimate}
              manualBracket={bracket}
              onBracketChange={setBracket}
              auditEmptyReason={auditEmptyReason}
              onCardClick={handleScrollToCard}
            />
          </div>
        </section>

        <section
          id={panelId("playtest")}
          role="tabpanel"
          aria-labelledby={tabId("playtest")}
          className={`${playtestVisible} min-h-0 min-w-0 flex-1 flex-col overflow-hidden`}
        >
          <PlaytestPanel deck={deck} format={format} mainCount={deckCount} />
        </section>
      </div>

      {listContextMenu && (
        <DeckCardContextMenu
          x={listContextMenu.x}
          y={listContextMenu.y}
          cardName={listContextMenu.cardName}
          hasOverride={!!artOverrides[resolveOracleIdSync(listContextMenu.cardName) ?? ""]}
          hasAlternates={hasAlternatePrintingsSync(resolveOracleIdSync(listContextMenu.cardName) ?? "")}
          onChooseArt={handleListChooseArt}
          onClearOverride={handleListClearOverride}
          onClose={() => setListContextMenu(null)}
        />
      )}

      {listPickerCard && (
        <PrintingPickerModal
          cardName={listPickerCard.cardName}
          oracleId={listPickerCard.oracleId}
          onCardHover={onCardHover}
          onClose={() => setListPickerCard(null)}
        />
      )}

      {pendingAction && (
        <div
          className="fixed inset-0 z-[120] flex items-center justify-center p-4"
          role="dialog"
          aria-modal="true"
          aria-label={t("unsaved.title")}
        >
          <button
            type="button"
            aria-label={t("unsaved.dismiss")}
            className="absolute inset-0 bg-black/60 backdrop-blur-[2px]"
            onClick={() => setPendingAction(null)}
          />
          <div className="relative z-10 w-full max-w-sm rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-5 shadow-[0_28px_80px_rgba(0,0,0,0.42)] backdrop-blur-md">
            <h2 className="text-base font-semibold text-white">{t("unsaved.title")}</h2>
            <p className="mt-1.5 text-sm text-slate-400">
              {pendingAction.type === "back"
                ? t("unsaved.bodyLeaving")
                : t("unsaved.bodyLoading")}
            </p>
            <div className="mt-4 flex flex-wrap justify-end gap-2">
              <button
                type="button"
                onClick={() => setPendingAction(null)}
                className="rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-slate-200 hover:bg-white/6"
              >
                {t("common:actions.cancel")}
              </button>
              <button
                type="button"
                onClick={confirmDiscardThen}
                className="rounded-xl border border-red-400/30 bg-red-500/10 px-3 py-1.5 text-sm text-red-200 hover:bg-red-500/20"
              >
                {t("unsaved.discard")}
              </button>
              <button
                type="button"
                onClick={confirmSaveThen}
                disabled={!deckName.trim()}
                title={deckName.trim() ? undefined : t("toolbar.nameToSave")}
                className="rounded-xl border border-emerald-400/40 bg-emerald-500/20 px-3 py-1.5 text-sm text-emerald-100 hover:bg-emerald-500/30 disabled:opacity-40"
              >
                {t("unsaved.saveAndContinue")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function FunnelIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" className="h-3.5 w-3.5" aria-hidden="true">
      <path d="M2 3.5A.5.5 0 0 1 2.5 3h11a.5.5 0 0 1 .39.812L9.5 9.3v3.2a.5.5 0 0 1-.276.447l-2 1A.5.5 0 0 1 6.5 13.5V9.3L2.11 3.812A.5.5 0 0 1 2 3.5Z" />
    </svg>
  );
}

function ListViewIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" className="h-4 w-4" aria-hidden="true">
      <line x1="3" y1="4" x2="13" y2="4" />
      <line x1="3" y1="8" x2="13" y2="8" />
      <line x1="3" y1="12" x2="13" y2="12" />
    </svg>
  );
}

function StackViewIcon() {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" className="h-4 w-4" aria-hidden="true">
      <rect x="2" y="2" width="5" height="5" rx="1" />
      <rect x="9" y="2" width="5" height="5" rx="1" />
      <rect x="2" y="9" width="5" height="5" rx="1" />
      <rect x="9" y="9" width="5" height="5" rx="1" />
    </svg>
  );
}
