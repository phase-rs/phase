const AUTO_UPDATE_MARKER_KEY = "phase:auto-updated-at";
const AUTO_UPDATE_MARKER_MAX_AGE_MS = 2 * 60 * 1000;

export function markPendingAutoUpdate(): void {
  sessionStorage.setItem(AUTO_UPDATE_MARKER_KEY, String(Date.now()));
}

export function consumeRecentAutoUpdateMarker(): boolean {
  const marker = sessionStorage.getItem(AUTO_UPDATE_MARKER_KEY);
  if (!marker) return false;

  sessionStorage.removeItem(AUTO_UPDATE_MARKER_KEY);

  const updatedAt = Number(marker);
  if (!Number.isFinite(updatedAt)) return false;

  return Date.now() - updatedAt < AUTO_UPDATE_MARKER_MAX_AGE_MS;
}

const SW_RELOAD_COUNT_KEY = "phase:sw-reload-count";

/**
 * Claim the single service-worker-driven reload allowed per PWA session.
 *
 * A legitimate SW update needs exactly one `controllerchange` → reload. But
 * iOS standalone PWAs fire repeated spurious `controllerchange` events while
 * the SW lifecycle settles in the first minute of a session — each would
 * reload the page again, looping and making the early game unplayable.
 *
 * The count is persisted in `sessionStorage`, so it survives the reload it
 * triggers: the first call returns `true` (proceed with the reload), every
 * later call this session returns `false` (suppress — a second reload would
 * be a loop). The count resets when the PWA window is closed and reopened.
 */
export function claimServiceWorkerReload(): boolean {
  const prior = Number(sessionStorage.getItem(SW_RELOAD_COUNT_KEY)) || 0;
  sessionStorage.setItem(SW_RELOAD_COUNT_KEY, String(prior + 1));
  return prior === 0;
}
