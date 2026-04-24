import { useEffect, useState } from "react";

import { onEngineLost } from "../../game/engineRecovery";
import { useGameStore } from "../../stores/gameStore";

/**
 * Layer 3 fallback for engine state loss — the user-facing prompt.
 *
 * Two presentations driven by whether the Rust panic hook captured a message:
 *
 * - **Engine crashed (panic captured):** the engine's thread-local state was
 *   lost because Rust code panicked. Re-running the same input would re-panic,
 *   so we don't retry — instead we surface the panic text and a one-click
 *   "Report on GitHub" link with diagnostic context pre-filled. Reload still
 *   works because `GameProvider` resumes from IDB on mount, but the panic
 *   itself needs an engine fix.
 *
 * - **Connection lost (no panic):** the legacy transient-loss path —
 *   worker restart, PWA update activation race. Reload restores from IDB.
 *
 * The listener is de-duped (`shown` latch) so repeated failures within
 * the same tab session don't stack multiple modals.
 */
/**
 * Frozen-at-trigger snapshot used for diagnostics. Captured inside the
 * onEngineLost listener — NOT read from the store at render time, because
 * the recovery path may have nulled gameId/gameMode before the user clicks
 * "Copy diagnostic" or "Report on GitHub".
 */
interface EngineLostSnapshot {
  reason: string;
  panic: string | null;
  gameId: string | null;
  gameMode: string | null;
}

export function EngineLostModal() {
  const [snapshot, setSnapshot] = useState<EngineLostSnapshot | null>(null);
  const [showDetails, setShowDetails] = useState(false);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    // External latch instead of a setState updater with a side effect.
    // Updater functions can run multiple times in React concurrent mode,
    // which would double-set state and could double-show the modal
    // after a state reset in dev (StrictMode).
    let fired = false;
    return onEngineLost((event) => {
      if (fired) return;
      fired = true;
      // Snapshot store fields right now — recovery / cleanup paths may
      // null these before the user interacts with the modal, which would
      // leave the diagnostic blank exactly when it matters most.
      const { gameId, gameMode } = useGameStore.getState();
      setSnapshot({
        reason: event.reason,
        panic: event.panic ?? null,
        gameId,
        gameMode,
      });
    });
  }, []);

  if (!snapshot) return null;

  const { reason, panic } = snapshot;
  const handleReload = () => {
    window.location.reload();
  };

  const isPanic = panic !== null;
  const diagnostic = buildDiagnostic(snapshot);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(diagnostic);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      // Clipboard API can fail in insecure contexts. Fall back to opening
      // a prompt so the user can copy manually rather than leaving them
      // stuck — the whole point of this modal is making the report easy.
      window.prompt("Copy this diagnostic:", diagnostic);
    }
  };

  const reportUrl = buildReportUrl({ panic, diagnostic });

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center"
      data-engine-lost-reason={reason}
    >
      <div className="absolute inset-0 bg-black/80" />
      <div className="relative z-10 max-w-lg rounded-xl bg-gray-900 p-8 shadow-2xl ring-1 ring-rose-700/60">
        {isPanic ? (
          <>
            <h2 className="mb-3 text-xl font-bold text-white">Engine crashed</h2>
            <p className="mb-4 text-sm text-gray-300">
              phase.rs hit an internal error and can&rsquo;t safely continue this
              action. Your last saved turn is preserved &mdash; reload to restore the
              game. <strong className="text-rose-200">Please report this</strong>{" "}
              so we can fix it.
            </p>
            <pre className="mb-4 max-h-40 overflow-auto rounded-lg bg-black/60 p-3 font-mono text-[11px] leading-relaxed text-rose-100 whitespace-pre-wrap">
              {panic}
            </pre>
          </>
        ) : (
          <>
            <h2 className="mb-3 text-xl font-bold text-white">Engine connection lost</h2>
            <p className="mb-4 text-sm text-gray-300">
              phase.rs lost its link to the game engine &mdash; most often caused by a
              background update activating mid-game. Your last saved turn is
              preserved; reload to restore the game.
            </p>
          </>
        )}
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
        <div className="flex flex-wrap justify-end gap-3">
          {isPanic && (
            <>
              <button
                type="button"
                onClick={handleCopy}
                className="rounded-lg bg-gray-700 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-gray-600"
              >
                {copied ? "Copied!" : "Copy diagnostic"}
              </button>
              <a
                href={reportUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="rounded-lg bg-amber-600 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-amber-500"
              >
                Report on GitHub
              </a>
            </>
          )}
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

/**
 * Build the diagnostic blob that ships with both the clipboard copy and the
 * GitHub issue body. Kept compact (no full state dump) so it round-trips
 * through `mailto:` / GitHub URL length limits without truncation, and so a
 * triager can read it without scrolling.
 *
 * All inputs come from the listener-time snapshot — never re-read from the
 * store, because recovery may have nulled gameId/gameMode by the time the
 * user opens this report.
 */
function buildDiagnostic({ reason, panic, gameId, gameMode }: EngineLostSnapshot): string {
  const lines = [
    `Build: v${__APP_VERSION__} (${__BUILD_HASH__})`,
    `Diagnostic tag: ${reason}`,
    `Game id: ${gameId ?? "<none>"}`,
    `Game mode: ${gameMode ?? "<none>"}`,
    `User agent: ${navigator.userAgent}`,
    "",
    "Panic:",
    panic ?? "<no panic captured — transient state-loss>",
  ];
  return lines.join("\n");
}

/**
 * Pre-fill a GitHub issue with the diagnostic. URL-encoded, so users can
 * one-click from the modal and just hit "Submit". Title is short + the
 * panic's source location so duplicates collapse.
 */
/** GitHub caps the issue URL around 8 KB. Truncate the diagnostic so a
 *  pathological panic (deep serde error, multi-line backtrace) doesn't bust
 *  the limit and silently drop fields. The clipboard copy keeps the full
 *  text — only the URL prefill is bounded. */
const MAX_DIAGNOSTIC_CHARS_IN_URL = 4000;

function buildReportUrl({
  panic,
  diagnostic,
}: {
  panic: string | null;
  diagnostic: string;
}): string {
  const titleSeed = panic ? extractPanicSummary(panic) : "Engine connection lost";
  const title = `Engine crash: ${titleSeed}`;
  const truncated =
    diagnostic.length > MAX_DIAGNOSTIC_CHARS_IN_URL
      ? `${diagnostic.slice(0, MAX_DIAGNOSTIC_CHARS_IN_URL)}\n[…truncated; click "Copy diagnostic" for full text]`
      : diagnostic;
  const body = `**What happened**\n_briefly describe what you were doing_\n\n**Diagnostic**\n\`\`\`\n${truncated}\n\`\`\``;
  const params = new URLSearchParams({
    title,
    body,
    labels: "bug,engine-crash",
  });
  return `${__GIT_REPO_URL__}/issues/new?${params.toString()}`;
}

/** Extract a short summary from a `panicked at file:line:col: message` line —
 *  used as the issue-title hint. */
function extractPanicSummary(panic: string): string {
  const colonIdx = panic.indexOf(": ");
  if (colonIdx < 0) return panic.slice(0, 80);
  const summary = panic.slice(colonIdx + 2).split("\n")[0];
  return summary.length > 80 ? `${summary.slice(0, 77)}…` : summary;
}
