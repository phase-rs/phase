import { afterEach, describe, expect, it, vi } from "vitest";

/**
 * Regression tests for #11001: `trackEvent` reports whether it queued the
 * event, so the card report button can stop claiming "Reported" for a report
 * that a gate or cap dropped.
 *
 * `telemetry.ts` reads the `__TELEMETRY_URL__` build define once, at module
 * load, and Vitest builds it empty (telemetry off). Each case stubs the define
 * and imports a fresh copy of the module, with its own queue and counters, so a
 * dropped event can only come from the gate the case is about. The first case
 * is the reach guard: it fails if the stub never reached the module, which
 * would make every "dropped" case pass vacuously.
 */
const INGEST_URL = "https://telemetry.example.test/ingest";

const sendBeacon = vi.fn(() => true);
const fetchMock = vi.fn(() => Promise.resolve({ ok: true } as Response));

async function loadTelemetry(ingestUrl: string) {
  vi.stubGlobal("__TELEMETRY_URL__", ingestUrl);
  vi.resetModules();
  const { flushNow, trackEvent } = await import("../telemetry");
  const { usePreferencesStore } = await import("../../stores/preferencesStore");
  // The fresh store rehydrates from localStorage, where an earlier case may
  // have saved telemetry off. Every case starts with it on.
  usePreferencesStore.setState({ telemetryEnabled: true });

  // Stubbed after the imports, which may read the real navigator at load.
  // Fake timers hold the 10-second flush timer, so no case's batch can fire
  // after the stubs are gone.
  vi.stubGlobal("navigator", { sendBeacon });
  vi.stubGlobal("fetch", fetchMock);
  vi.useFakeTimers();

  return { flushNow, trackEvent, usePreferencesStore };
}

/** The events of the last batch handed to `sendBeacon`. */
function lastBeaconEvents(): Array<Record<string, unknown>> {
  const [, body] = sendBeacon.mock.calls[sendBeacon.mock.calls.length - 1] as unknown as [
    string,
    string,
  ];
  return (JSON.parse(body) as { events: Array<Record<string, unknown>> }).events;
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  sendBeacon.mockClear();
  fetchMock.mockClear();
});

describe("trackEvent", () => {
  it("returns true when it queues the event", async () => {
    const { flushNow, trackEvent } = await loadTelemetry(INGEST_URL);

    expect(trackEvent("card_report", { name: "Starship Enterprise" })).toBe(true);

    // The event is really in the queue: a flush sends it.
    flushNow();
    expect(sendBeacon).toHaveBeenCalledTimes(1);
    expect(sendBeacon).toHaveBeenCalledWith(INGEST_URL, expect.any(String));
    expect(lastBeaconEvents()).toEqual([
      expect.objectContaining({ event: "card_report", name: "Starship Enterprise" }),
    ]);
  });

  it("returns false when the build has no ingest URL", async () => {
    const { trackEvent } = await loadTelemetry("");

    expect(trackEvent("card_report")).toBe(false);
  });

  it("returns false and queues nothing when the player turned telemetry off", async () => {
    const { flushNow, trackEvent, usePreferencesStore } = await loadTelemetry(INGEST_URL);
    usePreferencesStore.setState({ telemetryEnabled: false });

    expect(trackEvent("card_report")).toBe(false);

    flushNow();
    expect(sendBeacon).not.toHaveBeenCalled();
  });

  it("returns false once an event's per-session cap is reached", async () => {
    const { trackEvent } = await loadTelemetry(INGEST_URL);

    // `card_report` is capped at 10 a session.
    for (let report = 1; report <= 10; report += 1) {
      expect(trackEvent("card_report")).toBe(true);
    }
    expect(trackEvent("card_report")).toBe(false);

    // The cap belongs to the event: an uncapped one still queues.
    expect(trackEvent("session_start")).toBe(true);
  });

  it("returns false once the session-wide cap is reached", async () => {
    const { trackEvent } = await loadTelemetry(INGEST_URL);

    // Every event shares a budget of 100 a session.
    for (let event = 1; event <= 100; event += 1) {
      expect(trackEvent("session_start")).toBe(true);
    }
    expect(trackEvent("session_start")).toBe(false);

    // A `card_report` under its own cap still finds the session budget spent.
    expect(trackEvent("card_report")).toBe(false);
  });
});
