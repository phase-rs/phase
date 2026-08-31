// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from "vitest";
import { PREFERENCES_KEY } from "../../../constants/storage";
import {
  watchUserStorage,
  withStorageWatchSuppressed,
} from "../storageWatcher";

const cleanups: Array<() => void> = [];

afterEach(() => {
  cleanups.splice(0).forEach((cleanup) => {
    cleanup();
  });
  localStorage.clear();
  sessionStorage.clear();
});

function watch(onDirty: (key: string) => void): () => void {
  const cleanup = watchUserStorage(onDirty);
  cleanups.push(cleanup);
  return cleanup;
}

describe("watchUserStorage", () => {
  it("notifies every registration for relevant localStorage writes and keeps duplicate callbacks independent", () => {
    const first = vi.fn();
    const second = vi.fn();
    const duplicate = vi.fn();
    const removeFirst = watch(first);
    watch(second);
    const removeFirstDuplicate = watch(duplicate);
    watch(duplicate);

    localStorage.setItem(PREFERENCES_KEY, "first");
    removeFirst();
    removeFirstDuplicate();
    localStorage.removeItem(PREFERENCES_KEY);

    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenNthCalledWith(1, PREFERENCES_KEY);
    expect(second).toHaveBeenNthCalledWith(2, PREFERENCES_KEY);
    expect(duplicate).toHaveBeenCalledTimes(3);
  });

  it("ignores sessionStorage and non-user-owned keys while writing before notification", () => {
    const observedValues: Array<string | null> = [];
    const dirty = vi.fn((key: string) => {
      observedValues.push(localStorage.getItem(key));
    });
    watch(dirty);

    localStorage.setItem("unrelated", "ignored");
    sessionStorage.setItem(PREFERENCES_KEY, "ignored");
    localStorage.setItem(PREFERENCES_KEY, "written-first");

    expect(dirty).toHaveBeenCalledExactlyOnceWith(PREFERENCES_KEY);
    expect(observedValues).toEqual(["written-first"]);
  });

  it("owns the shared prototype wrapper until the final cleanup, then can install again", () => {
    const originalSetItem = Storage.prototype.setItem;
    const originalRemoveItem = Storage.prototype.removeItem;
    const firstCleanup = watch(vi.fn());
    const wrappedSetItem = Storage.prototype.setItem;
    const wrappedRemoveItem = Storage.prototype.removeItem;
    const secondDirty = vi.fn();
    const secondCleanup = watch(secondDirty);

    expect(Storage.prototype.setItem).toBe(wrappedSetItem);
    expect(Storage.prototype.removeItem).toBe(wrappedRemoveItem);
    firstCleanup();
    firstCleanup();
    expect(Storage.prototype.setItem).toBe(wrappedSetItem);
    expect(Storage.prototype.removeItem).toBe(wrappedRemoveItem);
    secondCleanup();
    expect(Storage.prototype.setItem).toBe(originalSetItem);
    expect(Storage.prototype.removeItem).toBe(originalRemoveItem);
    wrappedSetItem.call(localStorage, PREFERENCES_KEY, "captured-wrapper");
    expect(localStorage.getItem(PREFERENCES_KEY)).toBe("captured-wrapper");

    const resubscribedDirty = vi.fn();
    watch(resubscribedDirty);
    localStorage.setItem(PREFERENCES_KEY, "resubscribed");
    expect(resubscribedDirty).toHaveBeenCalledExactlyOnceWith(PREFERENCES_KEY);
  });

  it("does not notify after a native write throws", () => {
    const originalSetItem = Storage.prototype.setItem;
    const throwingSetItem = vi.fn(() => {
      throw new Error("storage quota exceeded");
    });
    Storage.prototype.setItem = throwingSetItem;
    const dirty = vi.fn();
    const cleanup = watch(dirty);

    expect(() => localStorage.setItem(PREFERENCES_KEY, "nope")).toThrow(
      "storage quota exceeded",
    );
    expect(dirty).not.toHaveBeenCalled();

    cleanup();
    Storage.prototype.setItem = originalSetItem;
  });

  it("suppresses all subscribers across nested scopes and restores after a throwing scope", () => {
    const first = vi.fn();
    const second = vi.fn();
    watch(first);
    watch(second);

    withStorageWatchSuppressed(() => {
      localStorage.setItem(PREFERENCES_KEY, "outer");
      withStorageWatchSuppressed(() => {
        localStorage.removeItem(PREFERENCES_KEY);
      });
      localStorage.setItem(PREFERENCES_KEY, "still-outer");
    });

    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();
    expect(() =>
      withStorageWatchSuppressed(() => {
        throw new Error("apply failed");
      }),
    ).toThrow("apply failed");

    localStorage.setItem(PREFERENCES_KEY, "ordinary");
    expect(first).toHaveBeenCalledExactlyOnceWith(PREFERENCES_KEY);
    expect(second).toHaveBeenCalledExactlyOnceWith(PREFERENCES_KEY);
  });
});
