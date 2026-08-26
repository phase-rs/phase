import { describe, expect, it } from "vitest";

import {
  claimUpdateStatus,
  getDownloadProgress,
  releaseUpdateStatus,
  setDownloadProgress,
} from "../updateStatus";

describe("setDownloadProgress", () => {
  it("clamps values into the 0–100 range and rounds", () => {
    setDownloadProgress(42.4);
    expect(getDownloadProgress()).toBe(42);
    setDownloadProgress(150);
    expect(getDownloadProgress()).toBe(100);
    setDownloadProgress(-10);
    expect(getDownloadProgress()).toBe(0);
  });

  it("clamps ±Infinity to the range bounds", () => {
    setDownloadProgress(Number.POSITIVE_INFINITY);
    expect(getDownloadProgress()).toBe(100);
    setDownloadProgress(Number.NEGATIVE_INFINITY);
    expect(getDownloadProgress()).toBe(0);
  });

  it("ignores NaN and preserves the last valid progress", () => {
    // NaN is what `receivedBytes / totalBytes * 100` yields when a download
    // reports no content-length (totalBytes === 0). It must never be stored:
    // it escapes the Math.max/min clamp and, because NaN !== NaN, defeats the
    // change guard too, rendering "NaN%" in the UI.
    setDownloadProgress(42);
    expect(getDownloadProgress()).toBe(42);

    setDownloadProgress(Number.NaN);

    expect(Number.isNaN(getDownloadProgress())).toBe(false);
    expect(getDownloadProgress()).toBe(42);
  });
});

describe("update status ownership", () => {
  it("keeps a concurrent updater from claiming the shared badge", () => {
    expect(claimUpdateStatus("serviceWorker")).toBe(true);
    expect(claimUpdateStatus("tauri")).toBe(false);

    releaseUpdateStatus("serviceWorker");

    expect(claimUpdateStatus("tauri")).toBe(true);
    releaseUpdateStatus("tauri");
  });
});
