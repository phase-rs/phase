import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  MAX_GAME_CODE_LEN,
  MAX_METRICS_BYTES,
  MAX_REPORTS_PER_BATCH,
  MAX_RTT_MS,
  METRICS_SCHEMA,
  PROBE_OUTCOMES,
  SERVER_PROBE_REPORT_KEYS,
  __resetServerMetricsForTests,
  flushMetricsNow,
  installServerMetricsLifecycle,
  metricsUrl,
  reportConnectOutcome,
  type ServerProbeReport,
} from "../serverMetrics";
import { usePreferencesStore } from "../../stores/preferencesStore";
import {
  ddlColumns,
  interfaceFields,
  numericConstant,
  stringArrayLiteral,
  workerDirectory,
  workerLobbyDo,
} from "./workerSourceExtractors";

/**
 * This is the ONLY suite that loads the real `serverMetrics` module, so it is
 * the only place a queue, a timer or a request body is ever constructed. Both
 * transports are stubbed before every case: `vitest.config.ts` defines
 * `__OFFICIAL_MULTIPLAYER_SERVER_URL__` as the REAL official URL (unlike
 * `__TELEMETRY_URL__`, which is `""`), so an unstubbed `sendBeacon`/`fetch`
 * here would POST to production. Every other suite that can dial a directory
 * source `vi.mock`s this module instead.
 */
const sendBeacon = vi.fn(() => true);
const fetchMock = vi.fn(() => Promise.resolve({ ok: true } as Response));

/** The reports of the last body handed to `sendBeacon`, parsed. */
function lastBeaconBatch(): { schema?: unknown; reports?: ServerProbeReport[] } {
  const [, body] = sendBeacon.mock.calls[sendBeacon.mock.calls.length - 1] as unknown as [
    string,
    string,
  ];
  return JSON.parse(body) as { schema?: unknown; reports?: ServerProbeReport[] };
}

const URL_A = "wss://a.example";

describe("serverMetrics", () => {
  beforeEach(() => {
    // Queue, timer, session counter and lifecycle latch. Without this the
    // session cap is consumed cumulatively across cases and V-U12k's listeners
    // outlive it, so the file's result would depend on its own ordering.
    __resetServerMetricsForTests();
    vi.stubGlobal("navigator", { sendBeacon });
    vi.stubGlobal("fetch", fetchMock);
    sendBeacon.mockReset();
    sendBeacon.mockReturnValue(true);
    fetchMock.mockReset();
    fetchMock.mockResolvedValue({ ok: true } as Response);
    usePreferencesStore.setState({ telemetryEnabled: true });
  });

  afterEach(() => {
    // Drain whatever a case left queued, so no report and no armed timer
    // crosses into the next one. The transports are still stubbed here.
    flushMetricsNow();
    vi.unstubAllGlobals();
  });

  // V-U12e
  it("sends the Worker's envelope, as a bare string body", () => {
    reportConnectOutcome(URL_A, "connect_ok", 120);
    reportConnectOutcome("wss://b.example", "connect_fail");
    flushMetricsNow();

    expect(sendBeacon).toHaveBeenCalledTimes(1);
    const [url, body] = sendBeacon.mock.calls[0] as unknown as [string, unknown];
    expect(url).toBe(metricsUrl());
    // Hostile: a `Blob` body would carry a content type and cost a preflight.
    // A bare string is `text/plain`, which is CORS-safelisted.
    expect(typeof body).toBe("string");
    expect(JSON.parse(body as string)).toEqual({
      schema: 1,
      reports: [
        { url: URL_A, outcome: "connect_ok", rtt_ms: 120 },
        { url: "wss://b.example", outcome: "connect_fail" },
      ],
    });

    // The queue emptied: a second flush with nothing new sends nothing.
    flushMetricsNow();
    expect(sendBeacon).toHaveBeenCalledTimes(1);
  });

  // V-U12f
  it("falls back to a keepalive fetch when sendBeacon cannot take it", () => {
    sendBeacon.mockReturnValue(false);
    reportConnectOutcome(URL_A, "connect_ok", 10);
    flushMetricsNow();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe(metricsUrl());
    expect(init).toMatchObject({
      method: "POST",
      keepalive: true,
      headers: { "Content-Type": "text/plain" },
    });

    // Second leg: no `sendBeacon` at all (an older webview, or a policy that
    // removed it) takes the same fallback.
    vi.stubGlobal("navigator", {});
    reportConnectOutcome(URL_A, "connect_ok", 10);
    flushMetricsNow();
    expect(fetchMock).toHaveBeenCalledTimes(2);

    // Paired: with `sendBeacon` accepting the body, `fetch` is NOT called.
    vi.stubGlobal("navigator", { sendBeacon });
    sendBeacon.mockReturnValue(true);
    reportConnectOutcome(URL_A, "connect_ok", 10);
    flushMetricsNow();
    expect(sendBeacon).toHaveBeenCalled();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  // V-U12g
  it("honours the telemetry opt-out at enqueue", () => {
    // The default is asserted off the store's INITIAL state, not off a
    // `beforeEach` snapshot — a snapshot would assert what this file wrote.
    expect(usePreferencesStore.getInitialState().telemetryEnabled).toBe(true);

    usePreferencesStore.setState({ telemetryEnabled: false });
    reportConnectOutcome(URL_A, "connect_ok", 10);
    flushMetricsNow();
    expect(sendBeacon).not.toHaveBeenCalled();
    expect(fetchMock).not.toHaveBeenCalled();

    // Paired positive: the identical sequence with the preference on does
    // enqueue and does send, so the silence above is the gate.
    usePreferencesStore.setState({ telemetryEnabled: true });
    reportConnectOutcome(URL_A, "connect_ok", 10);
    flushMetricsNow();
    expect(sendBeacon).toHaveBeenCalledTimes(1);
    expect(lastBeaconBatch().reports).toHaveLength(1);
  });

  // V-U12h
  it("never presents the Worker a batch it would truncate", () => {
    for (let i = 0; i < MAX_REPORTS_PER_BATCH + 10; i += 1) {
      reportConnectOutcome(`wss://s${i}.example`, "connect_ok", 5);
    }
    flushMetricsNow();

    // The count trigger fires along the way, so several sends happen; what
    // matters is that not one of them exceeds the Worker's per-batch cap.
    expect(sendBeacon).toHaveBeenCalled();
    for (const [, body] of sendBeacon.mock.calls as unknown as [string, string][]) {
      const batch = JSON.parse(body) as { reports: ServerProbeReport[] };
      expect(batch.reports.length).toBeLessThanOrEqual(MAX_REPORTS_PER_BATCH);
      expect(batch.reports.length).toBeGreaterThan(0);
      // The same body is also comfortably inside the Worker's byte cap.
      expect(body.length).toBeLessThan(MAX_METRICS_BYTES);
    }
    // Paired: one report under every cap still sends.
    sendBeacon.mockClear();
    reportConnectOutcome(URL_A, "connect_ok", 5);
    flushMetricsNow();
    expect(sendBeacon).toHaveBeenCalledTimes(1);
  });

  // V-U12i
  it("rounds and clamps rtt the way the Worker's sanitiser does", () => {
    reportConnectOutcome("wss://neg.example", "connect_ok", -5);
    reportConnectOutcome("wss://big.example", "connect_ok", 99_999);
    reportConnectOutcome("wss://frac.example", "connect_ok", 12.6);
    flushMetricsNow();

    expect(lastBeaconBatch().reports).toEqual([
      { url: "wss://neg.example", outcome: "connect_ok", rtt_ms: 0 },
      { url: "wss://big.example", outcome: "connect_ok", rtt_ms: MAX_RTT_MS },
      { url: "wss://frac.example", outcome: "connect_ok", rtt_ms: 13 },
    ]);

    // Hostile: a non-finite latency produces a report with NO `rtt_ms` — not
    // `rtt_ms: null`, which would claim an unmeasurable connect.
    sendBeacon.mockClear();
    reportConnectOutcome("wss://nan.example", "connect_ok", Number.NaN);
    reportConnectOutcome("wss://inf.example", "connect_ok", Number.POSITIVE_INFINITY);
    flushMetricsNow();
    const reports = lastBeaconBatch().reports!;
    expect(reports).toEqual([
      { url: "wss://nan.example", outcome: "connect_ok" },
      { url: "wss://inf.example", outcome: "connect_ok" },
    ]);
    expect(reports.every((report) => !("rtt_ms" in report))).toBe(true);

    // The OUTCOME half of the same guard: a latency handed in with a FAILED
    // connect is dropped, because there was no completed handshake to time and
    // folding one into the latency histogram would make a server that refuses
    // connections quickly look fast.
    sendBeacon.mockClear();
    reportConnectOutcome("wss://fail.example", "connect_fail", 120);
    flushMetricsNow();
    expect(lastBeaconBatch().reports).toEqual([
      { url: "wss://fail.example", outcome: "connect_fail" },
    ]);
  });

  // V-U12j
  it("never surfaces a transport failure", () => {
    sendBeacon.mockImplementation(() => {
      throw new Error("beacon exploded");
    });
    reportConnectOutcome(URL_A, "connect_ok", 10);
    expect(() => flushMetricsNow()).not.toThrow();
    // Reach-guard: the throwing stub really ran, so the assertion above is not
    // passing on a flush that sent nothing.
    expect(sendBeacon).toHaveBeenCalledTimes(1);

    sendBeacon.mockReset();
    sendBeacon.mockReturnValue(false);
    fetchMock.mockRejectedValue(new Error("network down"));
    reportConnectOutcome(URL_A, "connect_ok", 10);
    expect(() => flushMetricsNow()).not.toThrow();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  // V-U12k
  it("installs the page-lifecycle drain once and flushes on hide", () => {
    const visibility = vi.spyOn(document, "visibilityState", "get");
    // Hoisted ABOVE the first install so the handler OBJECT the installer
    // passes is captured; the removal assertion below compares against that
    // very object, not merely against "some function".
    const addDoc = vi.spyOn(document, "addEventListener");
    const removeDoc = vi.spyOn(document, "removeEventListener");
    const removeWin = vi.spyOn(window, "removeEventListener");
    try {
      installServerMetricsLifecycle();
      installServerMetricsLifecycle();

      // The exact function registered for `visibilitychange`. An inline arrow
      // at the add site would make this a fresh object on every install, and
      // the reset's by-reference removal would then silently take nothing off
      // — which is the regression the module-scope hoist exists to prevent and
      // which an `expect.any(Function)` assertion cannot see.
      const visibilityCalls = addDoc.mock.calls.filter(
        ([type]) => type === "visibilitychange",
      );
      expect(visibilityCalls).toHaveLength(1);
      const installedHandler = visibilityCalls[0][1];

      reportConnectOutcome(URL_A, "connect_ok", 10);
      visibility.mockReturnValue("hidden");
      document.dispatchEvent(new Event("visibilitychange"));

      // Once, not twice — a second registration would double every drain.
      expect(sendBeacon).toHaveBeenCalledTimes(1);
      expect(lastBeaconBatch().reports).toHaveLength(1);

      // The queue is already drained, so the second lifecycle event sends
      // nothing.
      window.dispatchEvent(new Event("pagehide"));
      expect(sendBeacon).toHaveBeenCalledTimes(1);

      // Paired: becoming VISIBLE is not a drain trigger.
      sendBeacon.mockClear();
      reportConnectOutcome(URL_A, "connect_ok", 10);
      visibility.mockReturnValue("visible");
      document.dispatchEvent(new Event("visibilitychange"));
      expect(sendBeacon).not.toHaveBeenCalled();

      // The double-registration negative: a reset must UNREGISTER, not merely
      // clear the latch, or each reset+install leaks a listener pair and one
      // hide drains once per pair.
      //
      // Counted on the DOM registrations, deliberately: a second drain is
      // INVISIBLE to `sendBeacon`, because `flushMetricsNow` returns early on
      // an empty queue and the first drain already emptied it. Asserting a
      // send count here would be vacuous — it passes with or without the
      // removal — so what is measured is the live-listener balance instead.
      __resetServerMetricsForTests();
      // The reset takes both listeners off BY REFERENCE: the `visibilitychange`
      // removal is asserted against the very object the install registered, so
      // a removal passing any other function fails here.
      expect(removeDoc).toHaveBeenCalledWith("visibilitychange", installedHandler);
      expect(removeWin).toHaveBeenCalledWith("pagehide", expect.any(Function));

      installServerMetricsLifecycle();
      // ...so across the whole case two installs added two listeners and the
      // reset took one off: one live pair, not two.
      expect(
        addDoc.mock.calls.filter(([type]) => type === "visibilitychange"),
      ).toHaveLength(2);
      expect(
        removeDoc.mock.calls.filter(([type]) => type === "visibilitychange"),
      ).toHaveLength(1);

      // And the drain still works through the freshly registered handler.
      sendBeacon.mockClear();
      reportConnectOutcome(URL_A, "connect_ok", 10);
      visibility.mockReturnValue("hidden");
      document.dispatchEvent(new Event("visibilitychange"));
      expect(sendBeacon).toHaveBeenCalledTimes(1);
    } finally {
      visibility.mockRestore();
      addDoc.mockRestore();
      removeDoc.mockRestore();
      removeWin.mockRestore();
    }
  });

  // ── The mirror gate: this client's duplicate declaration vs the Worker's ──

  // V-M0 — guard the guard. An extractor that silently returned [] (or NaN)
  // would make every comparison below pass over nothing.
  it("extracts non-empty metrics declarations from the Worker's source", () => {
    expect(interfaceFields(workerDirectory, "ServerProbeReport")).toContain("url");
    expect(stringArrayLiteral(workerDirectory, "PROBE_OUTCOMES").length).toBeGreaterThan(0);
    expect(numericConstant(workerDirectory, "MAX_REPORTS_PER_BATCH")).toBeGreaterThan(0);
    expect(numericConstant(workerDirectory, "MAX_RTT_MS")).toBeGreaterThan(0);
    expect(numericConstant(workerDirectory, "MAX_GAME_CODE_LEN")).toBeGreaterThan(0);
    expect(numericConstant(workerDirectory, "MAX_METRICS_BYTES")).toBeGreaterThan(0);
    // The route the client POSTs to is declared by the DO, and the reports it
    // accepts are keyed on the `servers` PRIMARY KEY.
    expect(workerLobbyDo).toContain('"/servers/metrics"');
    expect(ddlColumns(workerLobbyDo, "servers")).toContain("url");
  });

  // V-M6 — compared against the CLIENT's own runtime key array, never a
  // literal: a literal is a third declaration that agrees with the Worker
  // while the interface it claims to mirror drifts away from both.
  it("mirrors ServerProbeReport field-for-field", () => {
    expect(new Set(SERVER_PROBE_REPORT_KEYS)).toEqual(
      new Set(interfaceFields(workerDirectory, "ServerProbeReport")),
    );
  });

  // V-M7 — including the two outcomes this build deliberately does not emit:
  // a mirror that omitted them would go green while drifting.
  it("mirrors the outcome union value-for-value", () => {
    expect(new Set(PROBE_OUTCOMES)).toEqual(
      new Set(stringArrayLiteral(workerDirectory, "PROBE_OUTCOMES")),
    );
    expect(PROBE_OUTCOMES).toHaveLength(4);
  });

  // V-M8
  it("mirrors the four ingest caps", () => {
    expect(numericConstant(workerDirectory, "MAX_REPORTS_PER_BATCH")).toBe(
      MAX_REPORTS_PER_BATCH,
    );
    expect(numericConstant(workerDirectory, "MAX_RTT_MS")).toBe(MAX_RTT_MS);
    expect(numericConstant(workerDirectory, "MAX_GAME_CODE_LEN")).toBe(MAX_GAME_CODE_LEN);
    expect(numericConstant(workerDirectory, "MAX_METRICS_BYTES")).toBe(MAX_METRICS_BYTES);
  });

  // V-M9 — the envelope discriminant. A drift here is the one contract break
  // that is invisible from the client side: the Worker answers 204 and ingests
  // nothing.
  it("mirrors the envelope schema the sanitiser requires", () => {
    const guard = /batch\.schema !== (\d+)/.exec(workerDirectory);
    expect(guard).not.toBeNull();
    expect(Number(guard![1])).toBe(METRICS_SCHEMA);
  });
});
