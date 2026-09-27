/**
 * Surface a refused saved-deck library write to the user. Precedent for a non-component toast:
 * `game/actionRejectionReporter.ts::reportStructuredActionRejection`. Precedent for a typed
 * per-case label map: `draftDeckAutosave.ts::autosaveSlotLabels`.
 */
import i18n from "i18next";
import {
  SavedDeckChangedError,
  SavedDeckLibraryBusyError,
  type SavedDeckTxnFailure,
  type SavedDeckTxnSkipReason,
} from "./savedDeckTransaction";
import { useAppNotificationStore } from "../stores/appToastStore";

export type SavedDeckWriteAction =
  | "save"
  | "clone"
  | "import"
  | "delete"
  | "organize"
  | "updateFeeds"
  | "restore"
  | "applyCloud";

const titleByAction: Record<SavedDeckWriteAction, () => string> = {
  save: () => i18n.t("savedDeckLibraryBusy.title.save"),
  clone: () => i18n.t("savedDeckLibraryBusy.title.clone"),
  import: () => i18n.t("savedDeckLibraryBusy.title.import"),
  delete: () => i18n.t("savedDeckLibraryBusy.title.delete"),
  organize: () => i18n.t("savedDeckLibraryBusy.title.organize"),
  updateFeeds: () => i18n.t("savedDeckLibraryBusy.title.updateFeeds"),
  restore: () => i18n.t("savedDeckLibraryBusy.title.restore"),
  applyCloud: () => i18n.t("savedDeckLibraryBusy.title.applyCloud"),
};

const STORAGE_FAILURE_REASONS: ReadonlySet<SavedDeckTxnFailure> = new Set([
  "generation-unreadable",
  "generation-unpublished",
]);

function savedDeckLibraryBusyDescription(): string {
  return i18n.t("savedDeckLibraryBusy.description");
}

function savedDeckLibraryStorageFailureDescription(): string {
  return i18n.t("savedDeckLibraryStorageFailure.description");
}

/** Single authority for choosing the busy vs. storage-failure description from a txn failure
 *  reason — shared by the toast helpers here and by `cloudSyncStore`'s inline error text. */
export function busyOrStorageDescription(reason: SavedDeckTxnFailure): string {
  return STORAGE_FAILURE_REASONS.has(reason)
    ? savedDeckLibraryStorageFailureDescription()
    : savedDeckLibraryBusyDescription();
}

export function notifySavedDeckLibraryBusy(action: SavedDeckWriteAction, reason?: SavedDeckTxnFailure): void {
  const description = reason !== undefined ? busyOrStorageDescription(reason) : savedDeckLibraryBusyDescription();
  useAppNotificationStore.getState().showNotification({ title: titleByAction[action](), description });
}

/** The deck a user-initiated write targeted changed before the write ran (`SavedDeckChangedError`). */
export function notifySavedDeckChanged(action: SavedDeckWriteAction): void {
  useAppNotificationStore.getState().showNotification({
    title: titleByAction[action](),
    description: i18n.t("savedDeckChanged.description"),
  });
}

/** A background draft autosave (never rejects; `reason` comes from its skipped result) was not
 *  written. `"lock-unavailable"` (no Web Locks API) is silent: every submission without the API
 *  would otherwise toast, since there is no lock to ever acquire. */
export function notifyDraftAutosaveSkipped(reason: SavedDeckTxnSkipReason): void {
  if (reason === "lock-unavailable") return;
  useAppNotificationStore.getState().showNotification({
    title: i18n.t("savedDeckLibraryBusy.title.autosaveDraft"),
    description: busyOrStorageDescription(reason),
  });
}

/** Run a user-initiated deck-library write; if it is refused, tell the user and resolve `{ ok: false }`. Other errors propagate. */
export async function attemptSavedDeckWrite<T>(
  action: SavedDeckWriteAction,
  write: () => Promise<T>,
): Promise<{ ok: true; value: T } | { ok: false }> {
  try {
    return { ok: true, value: await write() };
  } catch (error) {
    if (error instanceof SavedDeckLibraryBusyError) {
      notifySavedDeckLibraryBusy(action, error.reason);
      return { ok: false };
    }
    if (error instanceof SavedDeckChangedError) {
      notifySavedDeckChanged(action);
      return { ok: false };
    }
    throw error;
  }
}
