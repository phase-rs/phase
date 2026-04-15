import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";

import type { Toast } from "../../stores/multiplayerStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";

interface ConnectionToastProps {
  /** Invoked by the generic-toast Retry button. Only attached to the generic
   * slot because the player-specific disconnect toasts have no equivalent
   * user-initiated retry (reconnection is driven by the remote client). */
  onRetry?: () => void;
  onSettings?: () => void;
}

const GENERIC_KEY = "generic";
const TICK_INTERVAL_MS = 500;

function secondsRemaining(expiresAt: number): number {
  return Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000));
}

export function ConnectionToast({ onRetry, onSettings }: ConnectionToastProps) {
  const toasts = useMultiplayerStore((s) => s.toasts);
  const clearToast = useMultiplayerStore((s) => s.clearToast);

  // Single interval drives both the countdown display and the auto-dismiss.
  // Because every toast stores an absolute `expiresAt` wall-clock timestamp,
  // dismissal is a pure function of `(toast.expiresAt, Date.now())` —
  // Map-mutation re-renders cannot reset the schedule. This replaces the
  // earlier pattern that layered separate `setTimeout` dismissal and
  // `setInterval` tick effects (which had two subtle bugs: a no-deps effect
  // calling `clearToast` during render, and 5s plain-toast timers that
  // restarted every time *any* toast changed).
  const [, forceTick] = useState(0);
  useEffect(() => {
    if (toasts.size === 0) return;
    const id = setInterval(() => {
      const now = Date.now();
      let anyExpired = false;
      for (const [key, toast] of toasts) {
        if (toast.expiresAt <= now) {
          clearToast(key);
          anyExpired = true;
        }
      }
      // Only force a re-render when no dismissal happened — dismissal
      // already triggers one via the store update. This keeps re-render
      // cadence at exactly the tick rate while countdowns are visible
      // without an extra render on the tick that dismissed.
      if (!anyExpired) forceTick((t) => t + 1);
    }, TICK_INTERVAL_MS);
    return () => clearInterval(id);
  }, [toasts, clearToast]);

  const entries = Array.from(toasts.entries());

  return (
    <AnimatePresence>
      {entries.map(([key, toast], index) => (
        <ToastBanner
          key={key}
          toast={toast}
          index={index}
          onRetry={key === GENERIC_KEY ? onRetry : undefined}
          onSettings={key === GENERIC_KEY ? onSettings : undefined}
          onDismiss={() => clearToast(key)}
        />
      ))}
    </AnimatePresence>
  );
}

interface ToastBannerProps {
  toast: Toast;
  index: number;
  onRetry?: () => void;
  onSettings?: () => void;
  onDismiss: () => void;
}

function ToastBanner({
  toast,
  index,
  onRetry,
  onSettings,
  onDismiss,
}: ToastBannerProps) {
  // Stack countdown toasts from the top and plain toasts from the bottom,
  // offsetting each by its index so multiple concurrent disconnects don't
  // overlap. 72px ≈ one toast row + breathing room.
  const stackOffset = `${index * 72}px`;

  return (
    <motion.div
      className={
        "fixed z-50 flex items-center gap-3 rounded-lg bg-gray-900 px-4 py-3 shadow-2xl ring-1 " +
        (toast.showCountdown
          ? "left-1/2 -translate-x-1/2 ring-amber-500/60"
          : "left-1/2 -translate-x-1/2 ring-red-700/50")
      }
      style={
        toast.showCountdown
          ? { top: `calc(1.5rem + ${stackOffset})` }
          : { bottom: `calc(1.5rem + ${stackOffset})` }
      }
      initial={{ opacity: 0, y: toast.showCountdown ? -20 : 20 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: toast.showCountdown ? -20 : 20 }}
      transition={{ duration: 0.25 }}
    >
      {toast.showCountdown && (
        <span
          aria-hidden="true"
          className="text-amber-400"
          title="Opponent disconnected"
        >
          !
        </span>
      )}
      <span className="text-sm text-gray-200">
        {toast.message}
        {toast.showCountdown && (
          <span className="ml-2 font-mono text-amber-200">
            — {secondsRemaining(toast.expiresAt)}s to forfeit
          </span>
        )}
      </span>
      {!toast.showCountdown && (
        <div className="flex gap-2">
          {onRetry && (
            <button
              onClick={() => {
                onDismiss();
                onRetry();
              }}
              className="rounded bg-red-600/80 px-2.5 py-1 text-xs font-semibold text-white transition hover:bg-red-500"
            >
              Retry
            </button>
          )}
          {onSettings && (
            <button
              onClick={() => {
                onDismiss();
                onSettings();
              }}
              className="rounded bg-gray-700 px-2.5 py-1 text-xs font-semibold text-gray-300 transition hover:bg-gray-600"
            >
              Settings
            </button>
          )}
        </div>
      )}
    </motion.div>
  );
}
