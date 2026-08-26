import { useTranslation } from "react-i18next";

import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

/**
 * Dismissible error banner for the pod tournament phases.
 *
 * Self-sources from the store, matching `StandingsTable`'s convention on this
 * surface, and renders `store.error` verbatim — the string is composed by the
 * host adapter and is not i18n'd. Reuses the red banner treatment already in
 * `DraftPodLobby`, and `LimitedDeckBuilder`'s `role="alert"` treatment of this
 * same store field — the pause banner's `role="status"` is the wrong precedent,
 * because a paused draft is a status and a failed action is not.
 *
 * `store.error` is scoped to the phase it was raised in:
 * `clearErrorOnPhaseChange` in `multiplayerDraftStore` retires it when the pod
 * changes phase, for both roles. So this banner never outlives its own phase,
 * and the `role="alert"` remount that each phase view performs cannot
 * re-announce a stale error — after a transition there is nothing to announce.
 * Within a phase the error persists across unrelated `viewUpdated` broadcasts;
 * dismissal is the clearing path there.
 */
export function PodErrorBanner() {
  const { t } = useTranslation("common");
  const error = useMultiplayerDraftStore((s) => s.error);
  const clearError = useMultiplayerDraftStore((s) => s.clearError);

  if (!error) return null;

  return (
    <div
      role="alert"
      data-testid="pod-error-banner"
      className="flex w-full items-start gap-3 rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300"
    >
      <span className="min-w-0 flex-1">{error}</span>
      <button
        type="button"
        onClick={clearError}
        aria-label={t("actions.close")}
        className="min-h-11 min-w-11 shrink-0 text-lg leading-none text-red-300/70 transition-colors hover:text-red-200"
      >
        &times;
      </button>
    </div>
  );
}
