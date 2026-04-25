import { zipSync, strToU8 } from "fflate";
import { useCallback, useEffect, useRef, useState } from "react";

import type { GameState } from "../../adapter/types";
import { audioManager } from "../../audio/AudioManager";
import { restoreGameState } from "../../game/dispatch";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";

const SCROLL_THRESHOLD = 40; // px from bottom to count as "at bottom"

type ConsoleLevel = "log" | "warn" | "error" | "debug";
const CONSOLE_LEVELS: readonly ConsoleLevel[] = ["error", "warn", "log", "debug"] as const;

interface ConsoleEntry {
  level: ConsoleLevel;
  message: string;
  timestamp: number;
}

/** Ring buffer of captured console output, shared across mount/unmount cycles. */
const consoleLogs: ConsoleEntry[] = [];
const MAX_CONSOLE_ENTRIES = 200;
let consolePatched = false;

function patchConsole(): void {
  if (consolePatched) return;
  consolePatched = true;

  const levels = ["log", "warn", "error", "debug"] as const;
  for (const level of levels) {
    const original = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      original(...args);
      const message = args.map((a) =>
        typeof a === "string" ? a : JSON.stringify(a, null, 2),
      ).join(" ");
      consoleLogs.push({ level, message, timestamp: Date.now() });
      if (consoleLogs.length > MAX_CONSOLE_ENTRIES) {
        consoleLogs.splice(0, consoleLogs.length - MAX_CONSOLE_ENTRIES);
      }
    };
  }
}

// Patch immediately so we capture logs from app startup
patchConsole();

export function DebugPanel() {
  const open = useUiStore((s) => s.debugPanelOpen);
  const turnCheckpoints = useGameStore((s) => s.turnCheckpoints);
  const gameState = useGameStore((s) => s.gameState);
  const gameMode = useGameStore((s) => s.gameMode);
  const [importText, setImportText] = useState("");
  const [status, setStatus] = useState<{ type: "success" | "error"; message: string } | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const [consoleSnapshot, setConsoleSnapshot] = useState<ConsoleEntry[]>([]);
  const [enabledLevels, setEnabledLevels] = useState<Set<ConsoleLevel>>(
    // `debug` starts off — it's the noisiest level and most viewers want it
    // hidden until they explicitly reach for it.
    () => new Set<ConsoleLevel>(["log", "warn", "error"]),
  );
  const consoleContainerRef = useRef<HTMLDivElement>(null);
  const consoleEndRef = useRef<HTMLDivElement>(null);

  // Smart scroll tracking: only auto-scroll if user is at the bottom
  const isAtBottomRef = useRef(true);
  const [newMessageCount, setNewMessageCount] = useState(0);
  const [showJumpToBottom, setShowJumpToBottom] = useState(false);
  const prevSnapshotLenRef = useRef(0);

  const canRestoreCheckpoints = gameMode === "ai" || gameMode === "local";

  const handleRestore = useCallback(async (state: GameState) => {
    setStatus(null);
    const err = await restoreGameState(state);
    if (err) {
      setStatus({ type: "error", message: err });
    } else {
      setStatus({ type: "success", message: "State restored" });
    }
  }, []);

  const handleImport = useCallback(async () => {
    setStatus(null);
    let parsed: unknown;
    try {
      parsed = JSON.parse(importText);
    } catch {
      setStatus({ type: "error", message: "Invalid JSON" });
      return;
    }

    // Accept either a bare GameState or the full debug export format {gameState, ...}
    const state = (
      parsed && typeof parsed === "object" && "gameState" in parsed
        ? (parsed as { gameState: GameState }).gameState
        : parsed
    ) as GameState;

    if (!state || typeof state !== "object" || !("waiting_for" in state)) {
      setStatus({ type: "error", message: "JSON does not look like a GameState (missing waiting_for)" });
      return;
    }

    const err = await restoreGameState(state);
    if (err) {
      setStatus({ type: "error", message: err });
    } else {
      setStatus({ type: "success", message: "State restored from import" });
      setImportText("");
    }
  }, [importText]);

  const handleCopyState = useCallback(() => {
    if (!gameState) return;
    const debug = {
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: useGameStore.getState().legalActions,
      turnCheckpoints: useGameStore.getState().turnCheckpoints,
    };
    navigator.clipboard.writeText(JSON.stringify(debug, null, 2))
      .then(() => setStatus({ type: "success", message: "Copied to clipboard" }))
      .catch(() => setStatus({ type: "error", message: "Failed to copy" }));
  }, [gameState]);

  const scrollToBottom = useCallback(() => {
    consoleEndRef.current?.scrollIntoView({ behavior: "smooth" });
    setNewMessageCount(0);
    setShowJumpToBottom(false);
  }, []);

  const visibleEntries = consoleSnapshot.filter((e) => enabledLevels.has(e.level));

  const toggleLevel = useCallback((level: ConsoleLevel) => {
    setEnabledLevels((prev) => {
      const next = new Set(prev);
      if (next.has(level)) next.delete(level);
      else next.add(level);
      return next;
    });
  }, []);

  const formatEntries = useCallback((entries: ConsoleEntry[]) =>
    entries
      .map((e) => {
        const ts = new Date(e.timestamp).toISOString().slice(11, 23);
        return `${ts} [${e.level}] ${e.message}`;
      })
      .join("\n"),
  []);

  const handleCopyConsole = useCallback(() => {
    if (visibleEntries.length === 0) {
      setStatus({ type: "error", message: "No console entries to copy" });
      return;
    }
    navigator.clipboard.writeText(formatEntries(visibleEntries))
      .then(() => setStatus({ type: "success", message: `Copied ${visibleEntries.length} entries` }))
      .catch(() => setStatus({ type: "error", message: "Failed to copy console" }));
  }, [visibleEntries, formatEntries]);

  const handleExportZip = useCallback(() => {
    if (visibleEntries.length === 0) {
      setStatus({ type: "error", message: "No console entries to export" });
      return;
    }
    try {
      const stamp = new Date().toISOString().replace(/[:.]/g, "-");
      const filename = `debug-log-${stamp}.log`;
      const zipped = zipSync(
        { [filename]: strToU8(formatEntries(visibleEntries)) },
        { level: 9 },
      );
      const blob = new Blob([zipped as BlobPart], { type: "application/zip" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `debug-log-${stamp}.zip`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
      setStatus({ type: "success", message: `Exported ${visibleEntries.length} entries` });
    } catch {
      setStatus({ type: "error", message: "Failed to export zip" });
    }
  }, [visibleEntries, formatEntries]);

  const handleConsoleScroll = useCallback(() => {
    const el = consoleContainerRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < SCROLL_THRESHOLD;
    isAtBottomRef.current = atBottom;
    if (atBottom) {
      setNewMessageCount(0);
      setShowJumpToBottom(false);
    } else {
      setShowJumpToBottom(true);
    }
  }, []);

  // Refresh console snapshot periodically while the panel is open
  useEffect(() => {
    if (!open) return;
    setConsoleSnapshot([...consoleLogs]);
    const interval = setInterval(() => {
      setConsoleSnapshot([...consoleLogs]);
    }, 1000);
    return () => clearInterval(interval);
  }, [open]);

  // Smart scroll: auto-scroll only when at bottom, track new messages
  // otherwise. Track against the filtered view length so toggling a level
  // off doesn't spuriously count filtered-out arrivals as "new".
  useEffect(() => {
    const newLen = visibleEntries.length;
    const added = newLen - prevSnapshotLenRef.current;
    prevSnapshotLenRef.current = newLen;

    if (added <= 0) return;

    if (isAtBottomRef.current) {
      consoleEndRef.current?.scrollIntoView({ behavior: "smooth" });
    } else {
      setNewMessageCount((prev) => prev + added);
    }
  }, [visibleEntries]);

  if (!open) return null;

  return (
    <div className="fixed right-0 top-0 z-[9999] flex h-full w-80 flex-col border-l border-gray-700 bg-gray-900/95 text-sm text-gray-300 shadow-xl backdrop-blur-sm">
      <div className="flex items-center justify-between border-b border-gray-700 px-3 py-2">
        <span className="font-mono text-xs font-bold uppercase tracking-wider text-gray-400">
          Debug Panel
        </span>
        <button
          onClick={() => useUiStore.getState().toggleDebugPanel()}
          className="text-gray-500 hover:text-gray-300"
        >
          &times;
        </button>
      </div>

      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        {/* Checkpoints */}
        <section className="border-b border-gray-800 px-3 py-2">
          <h3 className="mb-1 font-mono text-xs font-bold uppercase tracking-wider text-gray-500">
            Turn Checkpoints
          </h3>
          {!canRestoreCheckpoints ? (
            <p className="text-xs text-gray-600">Restore disabled in multiplayer</p>
          ) : turnCheckpoints.length === 0 ? (
            <p className="text-xs text-gray-600">No checkpoints yet (saved at turn start)</p>
          ) : (
            <div className="flex flex-col gap-1">
              {turnCheckpoints.map((cp, i) => (
                <button
                  key={i}
                  onClick={() => handleRestore(cp)}
                  className="rounded bg-gray-800 px-2 py-1 text-left text-xs transition-colors hover:bg-gray-700"
                >
                  Turn {cp.turn_number} &middot; Player {cp.active_player}
                </button>
              ))}
            </div>
          )}
        </section>

        {/* Import — only available in AI/local modes */}
        {canRestoreCheckpoints && (
          <section className="border-b border-gray-800 px-3 py-2">
            <h3 className="mb-1 font-mono text-xs font-bold uppercase tracking-wider text-gray-500">
              Import State
            </h3>
            <textarea
              ref={textareaRef}
              value={importText}
              onChange={(e) => setImportText(e.target.value)}
              placeholder="Paste GameState JSON..."
              className="w-full rounded border border-gray-700 bg-gray-800 px-2 py-1 font-mono text-xs text-gray-300 placeholder-gray-600 focus:border-blue-500 focus:outline-none"
              rows={4}
            />
            <button
              onClick={handleImport}
              disabled={!importText.trim()}
              className="mt-1 w-full rounded bg-blue-700 px-2 py-1 text-xs font-medium text-white transition-colors hover:bg-blue-600 disabled:cursor-not-allowed disabled:opacity-40"
            >
              Restore
            </button>
          </section>
        )}

        {/* Copy current state */}
        <section className="px-3 py-2">
          <button
            onClick={handleCopyState}
            disabled={!gameState}
            className="w-full rounded bg-gray-800 px-2 py-1 text-xs transition-colors hover:bg-gray-700 disabled:cursor-not-allowed disabled:opacity-40"
          >
            Copy Current State to Clipboard
          </button>
        </section>

        {/* Audio diagnostics */}
        <section className="border-b border-gray-800 px-3 py-2">
          <h3 className="mb-1 font-mono text-xs font-bold uppercase tracking-wider text-gray-500">
            Audio
          </h3>
          <div className="flex flex-col gap-1 text-xs">
            <AudioState />
            <button
              onClick={() => audioManager.restart()}
              className="rounded bg-gray-800 px-2 py-1 text-left transition-colors hover:bg-gray-700"
            >
              Restart AudioContext
            </button>
          </div>
        </section>

        {/* Console */}
        <section className="flex min-h-0 flex-1 flex-col px-3 py-2">
          <div className="mb-1 flex items-center justify-between">
            <h3 className="font-mono text-xs font-bold uppercase tracking-wider text-gray-500">
              Console
            </h3>
            <div className="flex items-center gap-2">
              <button
                onClick={handleCopyConsole}
                className="text-[10px] text-gray-600 hover:text-gray-400"
              >
                Copy
              </button>
              <button
                onClick={handleExportZip}
                className="text-[10px] text-gray-600 hover:text-gray-400"
                title="Download visible console entries as a compressed .zip file"
              >
                Export ZIP
              </button>
              <button
                onClick={() => { consoleLogs.length = 0; setConsoleSnapshot([]); prevSnapshotLenRef.current = 0; setNewMessageCount(0); }}
                className="text-[10px] text-gray-600 hover:text-gray-400"
              >
                Clear
              </button>
            </div>
          </div>
          <div className="mb-1 flex flex-wrap gap-1">
            {CONSOLE_LEVELS.map((level) => {
              const active = enabledLevels.has(level);
              const count = consoleSnapshot.reduce((n, e) => (e.level === level ? n + 1 : n), 0);
              const activeColor =
                level === "error"
                  ? "border-red-500/60 bg-red-500/20 text-red-300"
                  : level === "warn"
                    ? "border-yellow-500/60 bg-yellow-500/20 text-yellow-300"
                    : level === "debug"
                      ? "border-gray-500/60 bg-gray-500/20 text-gray-400"
                      : "border-blue-500/60 bg-blue-500/20 text-blue-300";
              return (
                <button
                  key={level}
                  onClick={() => toggleLevel(level)}
                  className={
                    "rounded-full border px-2 py-0.5 font-mono text-[10px] uppercase tracking-wider transition-colors " +
                    (active
                      ? activeColor
                      : "border-gray-700 bg-transparent text-gray-600 hover:border-gray-600 hover:text-gray-500")
                  }
                >
                  {level} ({count})
                </button>
              );
            })}
          </div>
          <div className="relative min-h-0 flex-1">
            <div
              ref={consoleContainerRef}
              onScroll={handleConsoleScroll}
              className="h-full select-text overflow-y-auto rounded bg-black/40 p-1 font-mono text-[10px] leading-tight"
            >
              {visibleEntries.map((entry, i) => (
                <div
                  key={i}
                  className={
                    entry.level === "error"
                      ? "text-red-400"
                      : entry.level === "warn"
                        ? "text-yellow-400"
                        : entry.level === "debug"
                          ? "text-gray-600"
                          : "text-gray-400"
                  }
                >
                  {entry.message}
                </div>
              ))}
              <div ref={consoleEndRef} />
            </div>
            {showJumpToBottom && (
              <button
                onClick={scrollToBottom}
                className="absolute bottom-2 left-1/2 -translate-x-1/2 rounded-full bg-blue-700/90 px-3 py-1 text-[10px] font-medium text-white shadow-lg backdrop-blur-sm transition-colors hover:bg-blue-600"
              >
                {newMessageCount > 0
                  ? newMessageCount === 1
                    ? "1 new message"
                    : `${newMessageCount} new messages`
                  : "Jump to bottom"}
              </button>
            )}
          </div>
        </section>

        {/* Status message */}
        {status && (
          <div
            className={`mx-3 mb-2 rounded px-2 py-1 text-xs ${
              status.type === "error"
                ? "bg-red-900/50 text-red-300"
                : "bg-green-900/50 text-green-300"
            }`}
          >
            {status.message}
          </div>
        )}
      </div>
    </div>
  );
}

/** Live AudioContext state readout — refreshes every second while panel is open. */
function AudioState() {
  const [info, setInfo] = useState("");
  useEffect(() => {
    const update = () => setInfo(audioManager.diagnostics());
    update();
    const id = setInterval(update, 1000);
    return () => clearInterval(id);
  }, []);
  return <span className="text-gray-500">{info}</span>;
}
