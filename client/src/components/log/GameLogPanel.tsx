import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { LOG_CATEGORIES, type GameLogEntry, type LogCategory } from "../../adapter/types.ts";
import { useDraggableWidget } from "../../hooks/useDraggableWidget.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  filterLogByView,
  timelineRowSeq,
  timelineRows,
  type LogView,
} from "../../viewmodel/logFormatting.ts";
import {
  exportLogEntriesJson,
  filterLogEntries,
  segmentsToPlainText,
  uniqueTurns,
} from "../../viewmodel/logSearch.ts";
import { LogEntry } from "./LogEntry.tsx";
import { copyText } from "../../services/copyText";

const EMPTY_LOG: GameLogEntry[] = [];
const LOG_PANEL_WIDTH_PX = 320;
const VIEWS: LogView[] = ["timeline", "details", "diagnostics"];

const VIEW_LABEL_KEYS: Record<LogView, string> = {
  timeline: "log.viewTimeline",
  details: "log.viewDetails",
  diagnostics: "log.viewDiagnostics",
};

const CATEGORY_LABEL_KEYS: Record<LogCategory, string> = {
  Game: "log.categoryGame",
  Turn: "log.categoryTurn",
  Stack: "log.categoryStack",
  Combat: "log.categoryCombat",
  Zone: "log.categoryZone",
  Life: "log.categoryLife",
  Mana: "log.categoryMana",
  State: "log.categoryState",
  Token: "log.categoryToken",
  Trigger: "log.categoryTrigger",
  Special: "log.categorySpecial",
  Destroy: "log.categoryDestroy",
  Debug: "log.categoryDebug",
};

function isNearBottom(element: HTMLDivElement): boolean {
  return element.scrollHeight - element.scrollTop - element.clientHeight < 32;
}

export function GameLogPanel() {
  const { t } = useTranslation("game");
  const logHistory = useGameStore((s) => s.logHistory ?? EMPTY_LOG);
  const logPanelLastChoice = usePreferencesStore((s) => s.logPanelLastChoice);
  const logDockSide = usePreferencesStore((s) => s.logDockSide);
  const setLogDockSide = usePreferencesStore((s) => s.setLogDockSide);
  const isGameOver = useGameStore((s) => s.gameState?.waiting_for?.type === "GameOver");
  const isOpen = useUiStore((s) => s.logPanelOpen);
  const setLogPanelOpen = useUiStore((s) => s.setLogPanelOpen);
  const setLogPanelOpenByUser = useUiStore((s) => s.setLogPanelOpenByUser);
  const inspectObjectSticky = useUiStore((s) => s.inspectObjectSticky);
  const gameSessionGeneration = useGameStore((s) => s.gameSessionGeneration);
  const reduceMotion = useReducedMotion();
  const isMobile = useIsMobile();
  const dockedLeft = !isMobile && logDockSide === "left";

  const [view, setView] = useState<LogView>("timeline");
  const [searchQuery, setSearchQuery] = useState("");
  const [turnFilter, setTurnFilter] = useState<number | null>(null);
  const [categoryFilter, setCategoryFilter] = useState<Set<LogCategory>>(new Set());
  const [showHiddenInformation, setShowHiddenInformation] = useState(false);
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [unreadCount, setUnreadCount] = useState(0);
  const [copyStatus, setCopyStatus] = useState<"success" | "failure" | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const lastGameSessionRef = useRef(gameSessionGeneration);
  const copyResetRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const nearBottomRef = useRef(true);
  const logDrag = useDraggableWidget({ kind: "widget", key: "logPanel" });

  const presentationFiltered = useMemo(
    () => filterLogByView(logHistory, view, categoryFilter.size > 0 ? categoryFilter : null, showHiddenInformation),
    [logHistory, view, categoryFilter, showHiddenInformation],
  );
  const filteredEntries = useMemo(
    () =>
      filterLogEntries(presentationFiltered, {
        query: searchQuery,
        categories: null,
        turn: turnFilter,
      }),
    [presentationFiltered, searchQuery, turnFilter],
  );
  const rows = useMemo(
    () => timelineRows(filteredEntries, categoryFilter.has("Turn")),
    [categoryFilter, filteredEntries],
  );
  const availableTurns = useMemo(() => uniqueTurns(logHistory), [logHistory]);
  const filterSignature = useMemo(
    () => JSON.stringify({
      categories: Array.from(categoryFilter).sort(),
      searchQuery,
      showHiddenInformation,
      turnFilter,
      view,
    }),
    [categoryFilter, searchQuery, showHiddenInformation, turnFilter, view],
  );
  const lastVisibleLogSeqRef = useRef(rows[rows.length - 1] && timelineRowSeq(rows[rows.length - 1]));
  const lastFilterSignatureRef = useRef(filterSignature);

  // Restore the user's remembered choice at the start of EVERY game session,
  // not merely on mount. Rematch navigates /game/:id -> /game/:newId and the
  // route carries no `key` (App.tsx:136), so this component RE-RENDERS in
  // place rather than remounting — a mount-scoped ref would leave a game-over
  // reveal's open panel standing through every subsequent rematch.
  // Authoritative (it can close, not only open) because `logPanelOpen` lives
  // in the non-persisted uiStore, which is a module singleton that outlives
  // the game and is not reset by sessionCleanup.
  // The mobile gate stays HERE rather than moving into the store's default
  // because `logPanelLastChoice` is persisted and travels between a user's
  // devices: services/backup.ts snapshots the raw blob and restore rehydrates
  // it, so a platform-derived persisted default would let a desktop's "open"
  // ride a backup onto a phone. Only a check at the point of consumption is
  // device-correct — and that matters MORE now the value is written
  // automatically on every user toggle.
  const seededSessionRef = useRef<number | null>(null);
  useEffect(() => {
    if (seededSessionRef.current === gameSessionGeneration) return;
    seededSessionRef.current = gameSessionGeneration;
    setLogPanelOpen(!isMobile && logPanelLastChoice === "open");
  }, [gameSessionGeneration, isMobile, logPanelLastChoice, setLogPanelOpen]);

  useEffect(() => {
    if (isGameOver) setLogPanelOpen(true);
  }, [isGameOver, setLogPanelOpen]);

  useEffect(() => {
    const nextLogSeq = rows[rows.length - 1] && timelineRowSeq(rows[rows.length - 1]);
    const previousLogSeq = lastVisibleLogSeqRef.current;
    lastVisibleLogSeqRef.current = nextLogSeq;
    const filtersChanged = lastFilterSignatureRef.current !== filterSignature;
    lastFilterSignatureRef.current = filterSignature;
    const sessionChanged = lastGameSessionRef.current !== gameSessionGeneration;
    lastGameSessionRef.current = gameSessionGeneration;
    if (filtersChanged || sessionChanged) {
      setUnreadCount(0);
      return;
    }
    if (nextLogSeq == null || previousLogSeq == null || nextLogSeq < previousLogSeq) {
      setUnreadCount(0);
      return;
    }
    if (nextLogSeq === previousLogSeq) return;
    const newEntries = rows.filter((row) => timelineRowSeq(row) > previousLogSeq).length;
    if (newEntries === 0) return;
    const element = scrollRef.current;
    if (element && nearBottomRef.current) {
      requestAnimationFrame(() => {
        element.scrollTop = element.scrollHeight;
      });
      return;
    }
    setUnreadCount((count) => count + newEntries);
  }, [filterSignature, gameSessionGeneration, rows]);

  useEffect(() => () => {
    if (copyResetRef.current) clearTimeout(copyResetRef.current);
  }, []);

  const jumpToLatest = useCallback(() => {
    const element = scrollRef.current;
    if (element) element.scrollTop = element.scrollHeight;
    nearBottomRef.current = true;
    setUnreadCount(0);
  }, []);

  const handleScroll = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    nearBottomRef.current = isNearBottom(element);
    if (nearBottomRef.current) setUnreadCount(0);
  }, []);

  useEffect(() => {
    const root = document.documentElement;
    root.style.setProperty(
      "--game-right-rail-offset",
      isOpen && !isMobile && !dockedLeft ? `${LOG_PANEL_WIDTH_PX}px` : "0px",
    );
    root.style.setProperty(
      "--game-left-rail-offset",
      isOpen && !isMobile && dockedLeft ? `${LOG_PANEL_WIDTH_PX}px` : "0px",
    );
    return () => {
      root.style.setProperty("--game-right-rail-offset", "0px");
      root.style.setProperty("--game-left-rail-offset", "0px");
    };
  }, [dockedLeft, isMobile, isOpen]);

  const toggleCategory = (category: LogCategory) => {
    setCategoryFilter((previous) => {
      const next = new Set(previous);
      if (next.has(category)) next.delete(category);
      else next.add(category);
      return next;
    });
  };

  const clearFilters = () => {
    setView("diagnostics");
    setSearchQuery("");
    setTurnFilter(null);
    setCategoryFilter(new Set());
    setShowHiddenInformation(false);
  };

  const reportCopyStatus = useCallback((status: "success" | "failure") => {
    setCopyStatus(status);
    if (copyResetRef.current) clearTimeout(copyResetRef.current);
    copyResetRef.current = setTimeout(() => setCopyStatus(null), 3000);
  }, []);

  const handleExport = () => {
    const blob = new Blob([exportLogEntriesJson(filteredEntries)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `phase-log-${Date.now()}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  const handleCopy = async () => {
    const text = filteredEntries
      .map((entry) => {
        const category = t(CATEGORY_LABEL_KEYS[entry.category]);
        const phase = t(`phaseName.${entry.phase}`);
        const context = entry.turn > 0 ? t("log.copyContext", { turn: entry.turn, phase, category }) : category;
        return `${context}: ${segmentsToPlainText(entry.segments)}`;
      })
      .join("\n");
    reportCopyStatus((await copyText(text)) ? "success" : "failure");
  };

  const filterSummary = t("log.filterSummary", { count: filteredEntries.length });

  return (
    <AnimatePresence>
      {isOpen && (
        <motion.div
          ref={logDrag.ref}
          data-flex-zone="logPanel"
          role="region"
          aria-label={t("log.panelLabel")}
          drag={logDrag.drag}
          dragMomentum={logDrag.dragMomentum}
          dragElastic={logDrag.dragElastic}
          onDragStart={logDrag.onDragStart}
          onDragEnd={logDrag.onDragEnd}
          onClickCapture={logDrag.onClickCapture}
          className={`fixed bottom-0 top-0 z-[60] flex w-[min(20rem,100vw)] flex-col bg-gray-900/95 pb-[env(safe-area-inset-bottom)] shadow-2xl ${dockedLeft ? "left-0 border-r border-gray-700" : "right-0 border-l border-gray-700"} ${logDrag.drag ? "cursor-grab active:cursor-grabbing" : ""}`}
          style={logDrag.style}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={reduceMotion ? { duration: 0 } : { type: "spring", stiffness: 300, damping: 30 }}
        >
          <div className="flex items-center justify-between border-b border-gray-700 px-3 py-2">
            <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-300">{t("log.title")}</h3>
            <div className="flex items-center gap-1">
              {!isMobile && (
                <button type="button" onClick={() => setLogDockSide(dockedLeft ? "right" : "left")} className="min-h-11 min-w-11 rounded text-gray-400 transition-colors hover:bg-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400" aria-label={t(dockedLeft ? "log.dockRight" : "log.dockLeft")} title={t(dockedLeft ? "log.dockRight" : "log.dockLeft")}>
                  <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" strokeWidth={2} stroke="currentColor" className="h-4 w-4">
                    <path strokeLinecap="round" strokeLinejoin="round" d="M7.5 21 3 16.5m0 0L7.5 12M3 16.5h13.5m0-9L21 3m0 0-4.5 4.5M21 3H7.5" />
                  </svg>
                </button>
              )}
              <button type="button" onClick={() => void handleCopy()} className="min-h-11 rounded px-2 text-[10px] text-gray-400 transition-colors hover:bg-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400" aria-label={t("log.copyFiltered", { count: filteredEntries.length })}>{t("log.copy")}</button>
              <button type="button" onClick={handleExport} className="min-h-11 rounded px-2 text-[10px] text-gray-400 transition-colors hover:bg-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400" aria-label={t("log.exportFiltered", { count: filteredEntries.length })}>{t("log.export")}</button>
              <button type="button" onClick={() => setLogPanelOpenByUser(false)} disabled={logDrag.drag} className="min-h-11 min-w-11 rounded text-gray-400 transition-colors hover:bg-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400 disabled:opacity-40" aria-label={t("log.closeLog")}>×</button>
            </div>
          </div>

          <div role="group" className="flex gap-1 border-b border-gray-800 px-3 py-1.5" aria-label={t("log.viewLabel")}>
            {VIEWS.map((candidate) => (
              <button key={candidate} type="button" onClick={() => setView(candidate)} aria-pressed={view === candidate} className={`min-h-11 rounded px-2 text-[10px] font-medium focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400 ${view === candidate ? "bg-cyan-600 text-white" : "bg-gray-800 text-white hover:bg-gray-700"}`}>{t(VIEW_LABEL_KEYS[candidate])}</button>
            ))}
            <span className="ml-auto self-center text-[9px] tabular-nums text-gray-500">{filterSummary}</span>
          </div>
          {view === "diagnostics" && <label className="flex min-h-11 items-center gap-2 border-b border-gray-800 px-3 text-[10px] text-gray-300"><input type="checkbox" checked={showHiddenInformation} onChange={(event) => setShowHiddenInformation(event.target.checked)} className="h-4 w-4 accent-cyan-500" />{t("log.showHiddenInformation")}</label>}

          <div className="border-b border-gray-800 px-3 py-2">
            <button type="button" onClick={() => setFiltersOpen((open) => !open)} aria-expanded={filtersOpen} className="min-h-11 w-full rounded px-1 text-left text-[10px] text-gray-300 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400">{t("log.filters", { count: categoryFilter.size + (turnFilter == null ? 0 : 1) + (searchQuery ? 1 : 0) })}</button>
            {filtersOpen && (
              <div className="space-y-2 pt-2">
                <label className="sr-only" htmlFor="game-log-search">{t("log.searchLabel")}</label>
                <input id="game-log-search" type="search" value={searchQuery} onChange={(event) => setSearchQuery(event.target.value)} placeholder={t("log.searchPlaceholder")} className="min-h-11 w-full rounded-md border border-gray-700 bg-gray-950 px-2 py-1 text-[11px] text-gray-200 placeholder:text-gray-600 focus:border-cyan-500 focus:outline-none" />
                <div className="flex flex-wrap gap-1">
                  <button type="button" onClick={() => setTurnFilter(null)} aria-pressed={turnFilter == null} className={`min-h-11 rounded px-2 text-[9px] focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400 ${turnFilter == null ? "bg-cyan-600 text-white" : "bg-gray-800 text-white"}`}>{t("log.allTurns")}</button>
                  {availableTurns.map((turn) => <button key={turn} type="button" onClick={() => setTurnFilter(turn)} aria-pressed={turnFilter === turn} className={`min-h-11 rounded px-2 text-[9px] tabular-nums focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400 ${turnFilter === turn ? "bg-cyan-600 text-white" : "bg-gray-800 text-white"}`}>{t("log.turnChip", { turn })}</button>)}
                </div>
                <div className="flex max-h-20 flex-wrap gap-1 overflow-y-auto">
                  {LOG_CATEGORIES.map((category) => <button key={category} type="button" onClick={() => toggleCategory(category)} aria-pressed={categoryFilter.has(category)} className={`min-h-11 rounded px-2 text-[9px] focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400 ${categoryFilter.has(category) ? "bg-indigo-600 text-white" : "bg-gray-800 text-white"}`}>{t(CATEGORY_LABEL_KEYS[category])}</button>)}
                </div>
                <button type="button" onClick={clearFilters} className="min-h-11 rounded px-2 text-[10px] text-cyan-300 hover:bg-gray-800 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400">{t("log.clearFilters")}</button>
              </div>
            )}
          </div>

          <div ref={scrollRef} role="region" tabIndex={0} aria-label={t("log.title")} onScroll={handleScroll} className="select-text flex-1 overflow-y-auto px-3 py-1 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400">
            {rows.length === 0 ? <div className="py-4 text-center text-xs text-gray-500"><p>{t("log.noMatchingEvents")}</p><button type="button" onClick={clearFilters} className="mt-2 min-h-11 rounded px-2 text-cyan-300 focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-400">{t("log.clearFilters")}</button></div> : rows.map((row) => row.type === "entry" ? <LogEntry key={row.entry.seq} entry={row.entry} onInspectObjectSticky={inspectObjectSticky} /> : <div key={`divider-${row.divider.seq}`} className="my-2 border-y border-gray-700 py-1 text-center text-[9px] font-semibold uppercase tracking-wide text-gray-500">{row.divider.turn > 0 && `${t("log.turnChip", { turn: row.divider.turn })} · `}{t(`phaseName.${row.divider.phase}`)}</div>)}
          </div>
          {unreadCount > 0 && <button type="button" onClick={jumpToLatest} className="m-2 min-h-11 rounded bg-cyan-700 px-3 text-xs font-medium text-white shadow focus-visible:outline focus-visible:outline-2 focus-visible:outline-cyan-200">{t("log.jumpToLatest", { count: unreadCount })}</button>}
          <p className="sr-only" aria-live="polite">{copyStatus === "success" ? t("log.copySuccess") : copyStatus === "failure" ? t("log.copyFailure") : filterSummary}</p>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
