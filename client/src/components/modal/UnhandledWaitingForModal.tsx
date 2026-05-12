import { useState } from "react";

import { useGameStore } from "../../stores/gameStore";
import { useCanActForWaitingState } from "../../hooks/usePlayerId";
import { isWaitingForHandled } from "../../game/waitingForRegistry";

/**
 * Safety net for orphan `WaitingFor` states the frontend has no modal for.
 *
 * The engine and frontend must agree on every interactive WaitingFor variant.
 * Historically a parser bug (e.g. issue #311 — Undead Alchemist) could fire a
 * trigger that pushed the engine into a state the frontend had no UI for,
 * silently hanging the game with no escape — the user could not even reach
 * the system menu to concede.
 *
 * This modal short-circuits that failure mode:
 *   1. If `waitingFor.type` is not present in `HANDLED_WAITING_FOR_TYPES`
 *      and the local player is the actor, render a fail-loud diagnostic.
 *   2. The modal names the missing state so the user can file a bug.
 *   3. The modal provides a "Concede game" button so the user can always
 *      escape — the system menu remains reachable from the top-left
 *      hamburger.
 *
 * Adding a new WaitingFor variant requires:
 *   - Wire the UI handler in GamePage (or a self-mounting component).
 *   - Add the type discriminator to `HANDLED_WAITING_FOR_TYPES` in
 *     `client/src/game/waitingForRegistry.ts`.
 * The TS type guard in `isWaitingForHandled` ensures this modal is the
 * trapdoor of last resort, not a routine fallback.
 */
export function UnhandledWaitingForModal({
  onExit,
  exitLabel,
}: {
  /** Called when the user clicks the exit button — should concede online,
   *  navigate to main menu in AI/local. The caller decides; this modal does
   *  not touch routing or adapter state directly. */
  onExit: () => void;
  exitLabel: string;
}) {
  const waitingFor = useGameStore((s) => s.gameState?.waiting_for);
  const canAct = useCanActForWaitingState();
  const [copied, setCopied] = useState(false);

  // Only surface when the local player is the actor — otherwise the prompt
  // is in flight to another seat and the wait is legitimate.
  if (!waitingFor || !canAct) return null;
  if (isWaitingForHandled(waitingFor)) return null;

  const diagnostic = buildDiagnostic(waitingFor);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(diagnostic);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      window.prompt("Copy this diagnostic:", diagnostic);
    }
  };

  const reportUrl = buildReportUrl(waitingFor.type, diagnostic);

  return (
    <div
      className="fixed inset-0 z-[100] flex items-center justify-center"
      data-unhandled-waiting-for={waitingFor.type}
    >
      <div className="absolute inset-0 bg-black/80" />
      <div className="relative z-10 max-w-lg rounded-xl bg-gray-900 p-8 shadow-2xl ring-1 ring-amber-700/60">
        <h2 className="mb-3 text-xl font-bold text-white">Action required, but UI is missing</h2>
        <p className="mb-4 text-sm text-gray-300">
          The game is waiting on you for an action, but this version of
          phase.rs does not yet have a UI for it. This is a bug — please
          report it so we can fix it. Use the button below to leave the
          game.
        </p>
        <div className="mb-4 rounded-lg bg-black/60 p-3">
          <div className="text-[10px] uppercase tracking-wider text-gray-500">Missing state</div>
          <div className="font-mono text-sm text-amber-200">{waitingFor.type}</div>
        </div>
        <div className="flex flex-wrap justify-end gap-3">
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
          <button
            type="button"
            onClick={onExit}
            className="rounded-lg bg-rose-600 px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-rose-500"
            autoFocus
          >
            {exitLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

function buildDiagnostic(waitingFor: { type: string; data?: unknown }): string {
  const lines = [
    `Build: v${__APP_VERSION__} (${__BUILD_HASH__})`,
    `Missing WaitingFor type: ${waitingFor.type}`,
    `User agent: ${navigator.userAgent}`,
    "",
    "Payload (truncated):",
    truncate(JSON.stringify(waitingFor.data ?? null, null, 2), 2000),
  ];
  return lines.join("\n");
}

function truncate(s: string, max: number): string {
  return s.length > max ? `${s.slice(0, max)}\n[…truncated]` : s;
}

const MAX_DIAGNOSTIC_CHARS_IN_URL = 4000;

function buildReportUrl(typeName: string, diagnostic: string): string {
  const title = `Unhandled WaitingFor: ${typeName}`;
  const truncated =
    diagnostic.length > MAX_DIAGNOSTIC_CHARS_IN_URL
      ? `${diagnostic.slice(0, MAX_DIAGNOSTIC_CHARS_IN_URL)}\n[…truncated; click "Copy diagnostic" for full text]`
      : diagnostic;
  const body = `**What happened**\n_briefly describe what you were doing_\n\n**Diagnostic**\n\`\`\`\n${truncated}\n\`\`\``;
  const params = new URLSearchParams({
    title,
    body,
    labels: "bug,frontend,unhandled-waiting-for",
  });
  return `${__GIT_REPO_URL__}/issues/new?${params.toString()}`;
}
