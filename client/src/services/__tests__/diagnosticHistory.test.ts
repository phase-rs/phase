import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { loadDiagnosticHistory, saveDiagnosticHistory } from "../diagnosticHistory";
import type { DiagnosticHistoryEntry } from "../troubleshooting";

const key = "phase-diagnostic-history-v1";
beforeEach(() => sessionStorage.clear());
afterEach(() => { vi.restoreAllMocks(); vi.useRealTimers(); sessionStorage.clear(); });
const event = (): DiagnosticHistoryEntry => ({ kind: "connection-attempt", observedAt: Date.now(), diagnosticId: crypto.randomUUID(), direction: "outgoing", event: "error", error: "negotiation-failed", state: { connectionState: "failed", iceState: "failed", channelState: "connecting" } });
it("restores typed failures and anonymous correlation from session storage", () => {
  const entry = event();
  saveDiagnosticHistory([entry]);
  expect(loadDiagnosticHistory()).toEqual([entry]);
});
it("bounds and expires history including across reloads", () => {
  vi.useFakeTimers();
  const entries = Array.from({ length: 40 }, event);
  saveDiagnosticHistory(entries);
  expect(loadDiagnosticHistory()).toEqual(entries.slice(-30));
  vi.advanceTimersByTime(59 * 60 * 1000);
  expect(loadDiagnosticHistory()).toEqual(entries.slice(-30));
  vi.advanceTimersByTime(60 * 1000 + 1);
  expect(loadDiagnosticHistory()).toEqual([]);
  expect(sessionStorage.getItem(key)).toBeNull();
});
it("rejects malformed, oversized, future, and secret-bearing storage", () => {
  for (const raw of ["{broken", "x".repeat(65 * 1024), JSON.stringify([
    { ...event(), observedAt: Date.now() + 1000 }, { ...event(), error: "SECRET" },
    { ...event(), peerId: "SECRET" }, { ...event(), state: { connectionState: "SECRET" } },
  ])]) {
    sessionStorage.setItem(key, raw);
    expect(loadDiagnosticHistory()).toEqual([]);
  }
});
it("tolerates unavailable browser storage", () => {
  vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("blocked"); });
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
  expect(loadDiagnosticHistory()).toEqual([]);
  expect(() => saveDiagnosticHistory([event()])).not.toThrow();
});
