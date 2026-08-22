import { useTranslation } from "react-i18next";

import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

/**
 * Dismissible error banner for the pod tournament phases.
 *
 * Self-sources from the store, matching `StandingsTable`'s convention on this
 * surface, and renders `store.error` verbatim — the string is composed by the
 * host adapter and is not i18n'd. Reuses the red banner treatment already in
 * `DraftPodLobby` and the `role="status"` shape of the pause banner.
 */
export function PodErrorBanner() {
  const { t } = useTranslation("common");
  const error = useMultiplayerDraftStore((s) => s.error);
  const clearError = useMultiplayerDraftStore((s) => s.clearError);

  if (!error) return null;

  return (
    <div
      role="status"
      data-testid="pod-error-banner"
      className="flex items-start gap-3 rounded-lg border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-300"
    >
      <span className="min-w-0 flex-1">{error}</span>
      <button
        type="button"
        onClick={clearError}
        aria-label={t("actions.close")}
        className="shrink-0 text-lg leading-none text-red-300/70 transition-colors hover:text-red-200"
      >
        &times;
      </button>
    </div>
  );
}
