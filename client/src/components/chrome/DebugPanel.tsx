import { useCallback, useRef, useState } from "react";

import type { GameState } from "../../adapter/types";
import { restoreGameState } from "../../game/dispatch";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";

export function DebugPanel() {
  const open = useUiStore((s) => s.debugPanelOpen);
  const turnCheckpoints = useGameStore((s) => s.turnCheckpoints);
  const gameState = useGameStore((s) => s.gameState);
  const [importText, setImportText] = useState("");
  const [status, setStatus] = useState<{ type: "success" | "error"; message: string } | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

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

      <div className="flex-1 overflow-y-auto">
        {/* Checkpoints */}
        <section className="border-b border-gray-800 px-3 py-2">
          <h3 className="mb-1 font-mono text-xs font-bold uppercase tracking-wider text-gray-500">
            Turn Checkpoints
          </h3>
          {turnCheckpoints.length === 0 ? (
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

        {/* Import */}
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
