import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { LOG_CATEGORIES, type GameLogEntry, type LogCategory } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { filterLogByVerbosity, type LogVerbosity } from "../../viewmodel/logFormatting.ts";
import {
  exportLogEntriesJson,
  filterLogEntries,
  segmentsToPlainText,
  uniqueTurns,
} from "../../viewmodel/logSearch.ts";
import { LogEntry } from "./LogEntry.tsx";

const EMPTY_LOG: GameLogEntry[] = [];

const VERBOSITY_OPTIONS: LogVerbosity[] = ["full", "compact", "minimal"];
const LOG_PANEL_WIDTH_PX = 320;

const VERBOSITY_LABEL_KEYS: Record<LogVerbosity, string> = {
  full: "log.verbosityFull",
  compact: "log.verbosityCompact",
  minimal: "log.verbosityMinimal",
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

export function GameLogPanel() {
  const { t } = useTranslation("game");
  const logHistory = useGameStore((s) => s.logHistory ?? EMPTY_LOG);
  const logDefaultState = usePreferencesStore((s) => s.logDefaultState);
  const isGameOver = useGameStore((s) => s.gameState?.waiting_for?.type === "GameOver");
  const isOpen = useUiStore((s) => s.logPanelOpen);
  const setLogPanelOpen = useUiStore((s) => s.setLogPanelOpen);
  const inspectObject = useUiStore((s) => s.inspectObject);

  const [verbosity, setVerbosity] = useState<LogVerbosity>("compact");
  const [searchQuery, setSearchQuery] = useState("");
  const [turnFilter, setTurnFilter] = useState<number | null>(null);
  const [categoryFilter, setCategoryFilter] = useState<Set<LogCategory>>(new Set());
  const scrollRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const verbosityFiltered = useMemo(
    () => filterLogByVerbosity(logHistory, verbosity),
    [logHistory, verbosity],
  );

  const filteredEntries = useMemo(
    () =>
      filterLogEntries(verbosityFiltered, {
        query: searchQuery,
        categories: categoryFilter.size > 0 ? categoryFilter : null,
        turn: turnFilter,
      }),
    [verbosityFiltered, searchQuery, categoryFilter, turnFilter],
  );

  const availableTurns = useMemo(() => uniqueTurns(verbosityFiltered), [verbosityFiltered]);

  const seededRef = useRef(false);
  useEffect(() => {
    if (seededRef.current) return;
    seededRef.current = true;
    if (logDefaultState === "open") setLogPanelOpen(true);
  }, [logDefaultState, setLogPanelOpen]);

  useEffect(() => {
    if (isGameOver) {
      setLogPanelOpen(true);
      setVerbosity("full");
    }
  }, [isGameOver, setLogPanelOpen]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [filteredEntries.length]);

  const handleOutsideClick = useCallback(
    (e: MouseEvent) => {
      if (isOpen && !isGameOver && panelRef.current && !panelRef.current.contains(e.target as Node)) {
        setLogPanelOpen(false);
      }
    },
    [isOpen, isGameOver, setLogPanelOpen],
  );

  useEffect(() => {
    if (isOpen) {
      document.addEventListener("mousedown", handleOutsideClick);
      return () => document.removeEventListener("mousedown", handleOutsideClick);
    }
  }, [isOpen, handleOutsideClick]);

  useEffect(() => {
    const root = document.documentElement;
    root.style.setProperty("--game-right-rail-offset", isOpen ? `${LOG_PANEL_WIDTH_PX}px` : "0px");
    return () => root.style.setProperty("--game-right-rail-offset", "0px");
  }, [isOpen]);

  const toggleCategory = (category: LogCategory) => {
    setCategoryFilter((prev) => {
      const next = new Set(prev);
      if (next.has(category)) next.delete(category);
      else next.add(category);
      return next;
    });
  };

  const handleExport = () => {
    const blob = new Blob([exportLogEntriesJson(filteredEntries)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `phase-log-${Date.now()}.json`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  const handleCopy = async () => {
    const text = filteredEntries
      .map((entry) => segmentsToPlainText(entry.segments))
      .join("\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      /* clipboard may be unavailable */
    }
  };

  return (
    <>
      <AnimatePresence>
        {isOpen && (
          <motion.div
            ref={panelRef}
            className="fixed bottom-0 right-0 top-0 z-[60] flex w-80 flex-col border-l border-gray-700 bg-gray-900/95 shadow-2xl"
            initial={{ x: "100%" }}
            animate={{ x: 0 }}
            exit={{ x: "100%" }}
            transition={{ type: "spring", stiffness: 300, damping: 30 }}
          >
            <div className="flex items-center justify-between border-b border-gray-700 px-3 py-2">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-300">
                {t("log.title")}
              </h3>
              <div className="flex items-center gap-1">
                <button
                  type="button"
                  onClick={() => void handleCopy()}
                  className="rounded p-1 text-[10px] text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
                  title={t("log.copy")}
                >
                  {t("log.copy")}
                </button>
                <button
                  type="button"
                  onClick={handleExport}
                  className="rounded p-1 text-[10px] text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
                  title={t("log.export")}
                >
                  {t("log.export")}
                </button>
                <button
                  onClick={() => setLogPanelOpen(false)}
                  className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
                  aria-label={t("log.closeLog")}
                >
                  <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                    <path d="M6.28 5.22a.75.75 0 0 0-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 1 0 1.06 1.06L10 11.06l3.72 3.72a.75.75 0 1 0 1.06-1.06L11.06 10l3.72-3.72a.75.75 0 0 0-1.06-1.06L10 8.94 6.28 5.22Z" />
                  </svg>
                </button>
              </div>
            </div>

            <div className="space-y-2 border-b border-gray-800 px-3 py-2">
              <input
                type="search"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                placeholder={t("log.searchPlaceholder")}
                className="w-full rounded-md border border-gray-700 bg-gray-950 px-2 py-1 text-[11px] text-gray-200 placeholder:text-gray-600 focus:border-cyan-500/50 focus:outline-none"
              />
              <div className="flex flex-wrap gap-1">
                <button
                  type="button"
                  onClick={() => setTurnFilter(null)}
                  className={`rounded px-1.5 py-0.5 text-[9px] ${
                    turnFilter == null ? "bg-cyan-600 text-white" : "bg-gray-800 text-gray-400"
                  }`}
                >
                  {t("log.allTurns")}
                </button>
                {availableTurns.map((turn) => (
                  <button
                    key={turn}
                    type="button"
                    onClick={() => setTurnFilter(turn)}
                    className={`rounded px-1.5 py-0.5 text-[9px] tabular-nums ${
                      turnFilter === turn ? "bg-cyan-600 text-white" : "bg-gray-800 text-gray-400"
                    }`}
                  >
                    {t("log.turnChip", { turn })}
                  </button>
                ))}
              </div>
              <div className="flex flex-wrap gap-1 max-h-16 overflow-y-auto">
                {LOG_CATEGORIES.map((category) => (
                  <button
                    key={category}
                    type="button"
                    onClick={() => toggleCategory(category)}
                    className={`rounded px-1.5 py-0.5 text-[9px] ${
                      categoryFilter.has(category)
                        ? "bg-indigo-600 text-white"
                        : "bg-gray-800 text-gray-400"
                    }`}
                  >
                    {t(CATEGORY_LABEL_KEYS[category])}
                  </button>
                ))}
              </div>
            </div>

            <div className="flex gap-1 border-b border-gray-800 px-3 py-1.5">
              {VERBOSITY_OPTIONS.map((v) => (
                <button
                  key={v}
                  onClick={() => setVerbosity(v)}
                  className={`rounded px-2 py-0.5 text-[10px] font-medium transition-colors ${
                    verbosity === v
                      ? "bg-cyan-600 text-white"
                      : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-300"
                  }`}
                >
                  {t(VERBOSITY_LABEL_KEYS[v])}
                </button>
              ))}
              <span className="ml-auto self-center text-[9px] tabular-nums text-gray-500">
                {t("log.entryCount", { count: filteredEntries.length })}
              </span>
            </div>

            <div ref={scrollRef} className="select-text flex-1 overflow-y-auto px-3 py-1">
              {filteredEntries.length === 0 ? (
                <p className="py-4 text-center text-xs italic text-gray-600">{t("log.noEvents")}</p>
              ) : (
                filteredEntries.map((entry) => (
                  <LogEntry
                    key={entry.seq}
                    entry={entry}
                    onInspectObject={inspectObject}
                  />
                ))
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
}
