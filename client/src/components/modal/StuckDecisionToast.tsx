import { useEffect, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import type { TFunction } from "i18next";

import type { StuckDecisionDiagnostic } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { STUCK_DEBOUNCE_MS } from "../../constants/stuckDecision";

const SESSION_SUPPRESS_KEY = "phase-rs:suppress-stuck-decision-toast";

/**
 * Non-blocking toast for a wedged (stuck) decision point.
 *
 * The engine surfaces a `StuckDecisionDiagnostic` (an engine-level progress
 * wedge, not a rules outcome) when an owed decision has no legal action for any
 * authorized submitter — a misrouted or unsatisfiable `WaitingFor` that would
 * otherwise freeze the game with no UI feedback. This toast is display-only: it
 * informs the user that the game appears stuck and exposes the "Report on
 * GitHub" path so the underlying bug doesn't hide silently.
 *
 * Transient-blip protection: the diagnostic must persist for `STUCK_DEBOUNCE_MS`
 * without clearing before anything is shown, so normal resolution churn never
 * flashes the toast.
 *
 * "Don't show again this session" latches via `sessionStorage`. A full page
 * reload resets it.
 */
export function StuckDecisionToast() {
  const { t } = useTranslation("game");
  const diagnostic = useGameStore((s) => s.stuckDiagnostic);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    if (sessionStorage.getItem(SESSION_SUPPRESS_KEY) === "1") return;
    if (!diagnostic) {
      // Cleared — the decision advanced. Hide so a later genuine stuck state
      // can re-surface.
      setVisible(false);
      return;
    }
    // Wait out the debounce; if the diagnostic is still present, surface it.
    const timer = window.setTimeout(() => {
      setVisible(true);
    }, STUCK_DEBOUNCE_MS);
    return () => window.clearTimeout(timer);
  }, [diagnostic]);

  if (!visible || !diagnostic) return null;

  const dismiss = () => setVisible(false);
  const suppressForSession = () => {
    sessionStorage.setItem(SESSION_SUPPRESS_KEY, "1");
    setVisible(false);
  };
  const reportUrl = buildReportUrl(diagnostic, t);

  return (
    <div
      className="fixed bottom-4 right-4 z-[90] max-w-sm rounded-lg bg-gray-900/95 p-4 shadow-xl ring-1 ring-amber-700/50 backdrop-blur-sm"
      data-stuck-decision-kind={diagnostic.waitingForKind}
    >
      <div className="mb-2 flex items-start justify-between gap-3">
        <h3 className="text-sm font-semibold text-amber-200">
          {t("stuckDecision.heading")}
        </h3>
        <button
          type="button"
          onClick={dismiss}
          aria-label={t("stuckDecision.dismiss")}
          className="text-gray-500 hover:text-gray-300"
        >
          &times;
        </button>
      </div>
      <p className="mb-3 text-xs text-gray-400">
        <Trans
          i18nKey="stuckDecision.body"
          t={t}
          components={{
            report: (
              <a
                href={reportUrl}
                target="_blank"
                rel="noopener noreferrer"
                className="text-amber-400 underline hover:text-amber-300"
              />
            ),
          }}
        />
      </p>
      <div className="flex justify-end">
        <button
          type="button"
          onClick={suppressForSession}
          className="text-[11px] text-gray-500 underline hover:text-gray-300"
        >
          {t("stuckDecision.dontShowAgain")}
        </button>
      </div>
    </div>
  );
}

function buildReportUrl(
  diagnostic: StuckDecisionDiagnostic,
  t: TFunction<"game">,
): string {
  const title = t("stuckDecision.reportTitle", {
    kind: diagnostic.waitingForKind,
  });
  const players = diagnostic.stuckPlayers.join(", ");
  const diagnosticText = [
    `Build: v${__APP_VERSION__} (${__BUILD_HASH__})`,
    `Waiting for: ${diagnostic.waitingForKind}`,
    `Stuck players: ${players}`,
    `User agent: ${navigator.userAgent}`,
  ].join("\n");
  const body = t("stuckDecision.reportWhatHappened", {
    diagnostic: diagnosticText,
  });
  const params = new URLSearchParams({
    title,
    body,
    labels: "bug,stuck-decision",
  });
  return `${__GIT_REPO_URL__}/issues/new?${params.toString()}`;
}
