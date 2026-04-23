import { useEffect, useState } from "react";

import { onEngineLost } from "../../game/engineRecovery";

/**
 * Layer 3 fallback for engine state loss — the user-facing prompt.
 *
 * The recovery layers sit above this:
 *   Layer 1: adapter classifies `NOT_INITIALIZED:` as STATE_LOST.
 *   Layer 2: `attemptStateRehydrate` silently restores from the store.
 * When Layer 2 fails (or Layer 2 can't run because the mode isn't
 * locally recoverable — P2P host, WS, or the AI controller hits its hard
 * stop), this modal fires.
 *
 * Reloading is the correct escalation: `GameProvider` runs its resume
 * path on mount, rehydrating from IDB for AI/local games or from the
 * persisted P2P host session for hosts. The user's last-saved turn is
 * preserved because `dispatch.ts` saves to IDB *before* animations play.
 *
 * The listener is de-duped (`shown` latch) so repeated failures within
 * the same tab session don't stack multiple modals.
 */
export function EngineLostModal() {
  const [shown, setShown] = useState(false);
  // `reason` is a developer tag ("submitAction-retry", "ai-controller-stuck")
  // used for support-log collection via the debug toggle. Never surfaced as
  // primary UI copy — users see the user-facing explanation instead.
  const [reason, setReason] = useState<string>("");
  const [showDetails, setShowDetails] = useState(false);

  useEffect(() => {
    // External latch instead of a setState updater with a side effect.
    // Updater functions can run multiple times in React concurrent mode,
    // which would double-set `reason` and could double-show the modal
    // after a state reset in dev (StrictMode).
    let fired = false;
    return onEngineLost((r) => {
      if (fired) return;
      fired = true;
      setReason(r);
      setShown(true);
    });
  }, []);

  if (!shown) return null;

  const handleReload = () => {
    window.location.reload();
  };

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center"
      data-engine-lost-reason={reason}
    >
      <div className="absolute inset-0 bg-black/80" />
      <div className="relative z-10 max-w-md rounded-xl bg-gray-900 p-8 shadow-2xl ring-1 ring-rose-700/60">
        <h2 className="mb-3 text-xl font-bold text-white">Engine connection lost</h2>
        <p className="mb-4 text-sm text-gray-300">
          phase.rs lost its link to the game engine — most often caused by a
          background update activating mid-game. Your last saved turn is
          preserved; reload to restore the game.
        </p>
        {showDetails ? (
          <p className="mb-6 font-mono text-[11px] text-gray-500">
            diagnostic: {reason}
          </p>
        ) : (
          <button
            type="button"
            onClick={() => setShowDetails(true)}
            className="mb-6 text-[11px] text-gray-600 underline hover:text-gray-400"
          >
            Show details
          </button>
        )}
        <div className="flex justify-end gap-3">
          <button
            onClick={handleReload}
            className="rounded-lg bg-rose-600 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-rose-500"
            autoFocus
          >
            Reload
          </button>
        </div>
      </div>
    </div>
  );
}
