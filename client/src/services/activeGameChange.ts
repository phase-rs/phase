/**
 * Same-tab change notification for the records that back the resume list: the
 * active-game pointer and the online session that routes it. A same-tab
 * `localStorage` write fires no `storage` event, so every writer of those
 * records calls this to let a mounted consumer (Home's resume list) revalidate.
 */
export const ACTIVE_GAME_CHANGED_EVENT = "phase:active-game-changed";

export function notifyActiveGameChange(): void {
  if (typeof window !== "undefined") {
    try {
      window.dispatchEvent(new Event("storage"));
      window.dispatchEvent(new CustomEvent(ACTIVE_GAME_CHANGED_EVENT));
    } catch {
      /* ignore */
    }
  }
}
