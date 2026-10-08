import { beforeEach, describe, expect, it } from "vitest";

import { SavedDeckLibraryBusyError } from "../savedDeckTransaction";
import { attemptSavedDeckWrite, notifyDraftAutosaveSkipped } from "../savedDeckWriteFailure";
import { useAppNotificationStore } from "../../stores/appToastStore";

const BUSY_DESCRIPTION = "Another Phase tab is busy. Close other Phase tabs and try again.";
const STORAGE_FAILURE_DESCRIPTION = "Phase couldn't reach browser storage. Try again in a moment.";

beforeEach(() => {
  useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
});

describe("attemptSavedDeckWrite", () => {
  it("a lock refusal or timeout resolves ok:false and shows the busy toast", async () => {
    const result = await attemptSavedDeckWrite("save", () =>
      Promise.reject(new SavedDeckLibraryBusyError("lock-timeout")),
    );
    expect(result).toEqual({ ok: false });
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Couldn't save deck",
      description: BUSY_DESCRIPTION,
    });
  });

  it("a stale local view resolves ok:false and shows the busy toast", async () => {
    const result = await attemptSavedDeckWrite("save", () =>
      Promise.reject(new SavedDeckLibraryBusyError("library-view-stale")),
    );
    expect(result).toEqual({ ok: false });
    expect(useAppNotificationStore.getState().notification?.description).toBe(BUSY_DESCRIPTION);
  });

  it("an IDB read failure resolves ok:false and shows the storage-failure toast", async () => {
    const result = await attemptSavedDeckWrite("clone", () =>
      Promise.reject(new SavedDeckLibraryBusyError("generation-unreadable")),
    );
    expect(result).toEqual({ ok: false });
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Couldn't clone deck",
      description: STORAGE_FAILURE_DESCRIPTION,
    });
  });

  it("an IDB write failure resolves ok:false and shows the storage-failure toast", async () => {
    const result = await attemptSavedDeckWrite("import", () =>
      Promise.reject(new SavedDeckLibraryBusyError("generation-unpublished")),
    );
    expect(result).toEqual({ ok: false });
    expect(useAppNotificationStore.getState().notification?.description).toBe(STORAGE_FAILURE_DESCRIPTION);
  });

  it("other errors propagate and do not notify", async () => {
    await expect(attemptSavedDeckWrite("save", () => Promise.reject(new Error("boom")))).rejects.toThrow("boom");
    expect(useAppNotificationStore.getState().notification).toBeNull();
  });

  it("success resolves ok:true with the value and does not notify", async () => {
    const result = await attemptSavedDeckWrite("save", () => Promise.resolve("value"));
    expect(result).toEqual({ ok: true, value: "value" });
    expect(useAppNotificationStore.getState().notification).toBeNull();
  });
});

describe("notifyDraftAutosaveSkipped", () => {
  it("lock-unavailable stays silent", () => {
    notifyDraftAutosaveSkipped("lock-unavailable");
    expect(useAppNotificationStore.getState().notification).toBeNull();
  });

  it("lock-refused shows the busy toast", () => {
    notifyDraftAutosaveSkipped("lock-refused");
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Couldn't autosave your draft deck",
      description: BUSY_DESCRIPTION,
    });
  });
});
