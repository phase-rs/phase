import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import type { GameLogEntry } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { filterLogByVerbosity } from "../../viewmodel/logFormatting.ts";
import type { LogVerbosity } from "../../viewmodel/logFormatting.ts";
import { LogEntry } from "./LogEntry.tsx";

const EMPTY_LOG: GameLogEntry[] = [];

const VERBOSITY_OPTIONS: LogVerbosity[] = ["full", "compact", "minimal"];
const LOG_PANEL_WIDTH_PX = 320;

export function GameLogPanel() {
  const logHistory = useGameStore((s) => s.logHistory ?? EMPTY_LOG);
  const logDefaultState = usePreferencesStore((s) => s.logDefaultState);
  const isOpen = useUiStore((s) => s.logPanelOpen);
  const setLogPanelOpen = useUiStore((s) => s.setLogPanelOpen);

  const [verbosity, setVerbosity] = useState<LogVerbosity>("compact");
  const scrollRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  const filteredEntries = filterLogByVerbosity(logHistory, verbosity);

  // One-time seed of the open state from user preference
  const seededRef = useRef(false);
  useEffect(() => {
    if (seededRef.current) return;
    seededRef.current = true;
    if (logDefaultState === "open") setLogPanelOpen(true);
  }, [logDefaultState, setLogPanelOpen]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [filteredEntries.length]);

  // Close panel when clicking outside
  const handleOutsideClick = useCallback(
    (e: MouseEvent) => {
      if (isOpen && panelRef.current && !panelRef.current.contains(e.target as Node)) {
        setLogPanelOpen(false);
      }
    },
    [isOpen, setLogPanelOpen],
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

  return (
    <>
      {/* Slide-out panel (open via board right-click context menu) */}
      <AnimatePresence>
        {isOpen && (
          <motion.div
            ref={panelRef}
            className="fixed bottom-0 right-0 top-0 z-40 flex w-80 flex-col border-l border-gray-700 bg-gray-900/95 shadow-2xl"
            initial={{ x: "100%" }}
            animate={{ x: 0 }}
            exit={{ x: "100%" }}
            transition={{ type: "spring", stiffness: 300, damping: 30 }}
          >
            {/* Header */}
            <div className="flex items-center justify-between border-b border-gray-700 px-3 py-2">
              <h3 className="text-xs font-semibold uppercase tracking-wider text-gray-300">
                Game Log
              </h3>
              <button
                onClick={() => setLogPanelOpen(false)}
                className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
                aria-label="Close game log"
              >
                <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor" className="h-4 w-4">
                  <path d="M6.28 5.22a.75.75 0 0 0-1.06 1.06L8.94 10l-3.72 3.72a.75.75 0 1 0 1.06 1.06L10 11.06l3.72 3.72a.75.75 0 1 0 1.06-1.06L11.06 10l3.72-3.72a.75.75 0 0 0-1.06-1.06L10 8.94 6.28 5.22Z" />
                </svg>
              </button>
            </div>

            {/* Verbosity filter */}
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
                  {v}
                </button>
              ))}
            </div>

            {/* Log entry list */}
            <div ref={scrollRef} className="select-text flex-1 overflow-y-auto px-3 py-1">
              {filteredEntries.length === 0 ? (
                <p className="py-4 text-center text-xs italic text-gray-600">No events yet</p>
              ) : (
                filteredEntries.map((entry) => (
                  <LogEntry key={entry.seq} entry={entry} />
                ))
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
}
