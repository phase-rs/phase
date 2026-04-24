import { useEffect, useRef, useState } from "react";

import { onNonFatalPanic } from "../../game/engineRecovery";

const SESSION_SUPPRESS_KEY = "phase-rs:suppress-nonfatal-panic-toast";
/**
 * How long to stay quiet after a toast fires, unless the panic *text*
 * differs. A cascade of identical panics (e.g., a wasm hot-loop calling
 * into a broken code path) would otherwise re-surface the toast every
 * ~50ms, which is worse UX than one scary modal. A new message text
 * bypasses the cooldown — distinct panics are worth surfacing.
 */
const TOAST_COOLDOWN_MS = 30_000;

interface PanicSnapshot {
  reason: string;
  panic: string | null;
}

/**
 * Non-blocking toast for recoverable engine panics.
 *
 * A Rust panic was captured, but `attemptStateRehydrate` succeeded — the
 * engine's state either survived the panic (side-path instrumentation, a
 * debug assertion off the mutation path) or was rebuilt from the store
 * snapshot. The user can keep playing; this toast is an informational
 * nudge that still exposes the "Report on GitHub" path so a real bug
 * doesn't hide silently.
 *
 * "Don't show again this session" latches via `sessionStorage`. A full
 * page reload resets it — intentional, since a rare panic after refresh
 * is worth re-surfacing.
 */
export function NonFatalPanicToast() {
  const [snapshot, setSnapshot] = useState<PanicSnapshot | null>(null);
  const lastShownRef = useRef<{ panic: string | null; at: number } | null>(null);

  useEffect(() => {
    if (sessionStorage.getItem(SESSION_SUPPRESS_KEY) === "1") return;
    return onNonFatalPanic((event) => {
      const now = Date.now();
      const last = lastShownRef.current;
      // Suppress cascades of the same panic within the cooldown window.
      // A new panic *message* bypasses the cooldown — distinct failures
      // are worth re-surfacing so the user / triager sees both.
      if (
        last
        && last.panic === (event.panic ?? null)
        && now - last.at < TOAST_COOLDOWN_MS
      ) {
        return;
      }
      lastShownRef.current = { panic: event.panic ?? null, at: now };
      setSnapshot({ reason: event.reason, panic: event.panic ?? null });
    });
  }, []);

  if (!snapshot) return null;

  const dismiss = () => setSnapshot(null);
  const suppressForSession = () => {
    sessionStorage.setItem(SESSION_SUPPRESS_KEY, "1");
    setSnapshot(null);
  };
  const reportUrl = buildReportUrl(snapshot);

  return (
    <div
      className="fixed bottom-4 right-4 z-[90] max-w-sm rounded-lg bg-gray-900/95 p-4 shadow-xl ring-1 ring-amber-700/50 backdrop-blur-sm"
      data-nonfatal-panic-reason={snapshot.reason}
    >
      <div className="mb-2 flex items-start justify-between gap-3">
        <h3 className="text-sm font-semibold text-amber-200">
          Engine warning (non-fatal)
        </h3>
        <button
          type="button"
          onClick={dismiss}
          aria-label="Dismiss"
          className="text-gray-500 hover:text-gray-300"
        >
          &times;
        </button>
      </div>
      <p className="mb-3 text-xs text-gray-400">
        phase.rs hit an internal warning but recovered. The game is still
        safe to continue. Please{" "}
        <a
          href={reportUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="text-amber-400 underline hover:text-amber-300"
        >
          report it
        </a>{" "}
        so we can investigate.
      </p>
      <div className="flex justify-end">
        <button
          type="button"
          onClick={suppressForSession}
          className="text-[11px] text-gray-500 underline hover:text-gray-300"
        >
          Don&rsquo;t show again this session
        </button>
      </div>
    </div>
  );
}

const MAX_DIAGNOSTIC_CHARS_IN_URL = 4000;

function buildReportUrl({ reason, panic }: PanicSnapshot): string {
  const titleSeed = panic ? extractPanicSummary(panic) : reason;
  const title = `Non-fatal engine panic: ${titleSeed}`;
  const diagnostic = [
    `Build: v${__APP_VERSION__} (${__BUILD_HASH__})`,
    `Diagnostic tag: ${reason}`,
    `User agent: ${navigator.userAgent}`,
    "",
    "Panic (engine recovered):",
    panic ?? "<no panic text>",
  ].join("\n");
  const truncated =
    diagnostic.length > MAX_DIAGNOSTIC_CHARS_IN_URL
      ? `${diagnostic.slice(0, MAX_DIAGNOSTIC_CHARS_IN_URL)}\n[…truncated]`
      : diagnostic;
  const body = `**What happened**\n_briefly describe what you were doing_\n\n**Diagnostic**\n\`\`\`\n${truncated}\n\`\`\``;
  const params = new URLSearchParams({
    title,
    body,
    labels: "bug,engine-panic-nonfatal",
  });
  return `${__GIT_REPO_URL__}/issues/new?${params.toString()}`;
}

function extractPanicSummary(panic: string): string {
  const colonIdx = panic.indexOf(": ");
  if (colonIdx < 0) return panic.slice(0, 80);
  const summary = panic.slice(colonIdx + 2).split("\n")[0];
  return summary.length > 80 ? `${summary.slice(0, 77)}…` : summary;
}
