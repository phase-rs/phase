import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

import { TroubleshootingDialog } from "../TroubleshootingDialog";
import type { DiagnosticReport } from "../../../services/troubleshooting";

const { runDiagnostics, downloadBlob } = vi.hoisted(() => ({ runDiagnostics: vi.fn(), downloadBlob: vi.fn() }));
vi.mock("../../../services/troubleshooting", () => ({ runDiagnostics }));
vi.mock("../../../network/connectivityDiagnostics", () => ({ runConnectivityDiagnostics: vi.fn() }));
vi.mock("../../../services/fileDownload", () => ({ downloadBlob }));
vi.mock("../../../services/serverDirectory", () => ({ directoryUrl: () => "https://directory.test/servers" }));
const report: DiagnosticReport = {
  schemaVersion: 1, startedAt: 1, completedAt: 2,
  environment: { version: "test", build: "test", mode: null, online: true, visibility: "visible", webAssembly: true, webRtc: true },
  results: [{ check: "engine", status: "unavailable", reason: "noEngine", observedAt: 2 }], engines: [], peers: [], history: [],
};
beforeEach(() => { runDiagnostics.mockReset().mockResolvedValue(report); downloadBlob.mockReset().mockResolvedValue({ kind: "requested", filename: "phase-diagnostics.json" }); });
afterEach(cleanup);

it("runs only on request, renders unavailable honestly, and can run again", async () => {
  render(<TroubleshootingDialog onClose={vi.fn()} />);
  expect(runDiagnostics).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Download report" })).toBeDisabled();
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  expect(await screen.findByText("No local engine instance is registered. Engine health was not checked.")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Run again" }));
  await screen.findByRole("button", { name: "Run again" });
  expect(runDiagnostics).toHaveBeenCalledTimes(2);
});

it("prevents overlapping checks, aborts on close and ignores a late result", async () => {
  let resolve!: (value: DiagnosticReport) => void;
  runDiagnostics.mockReturnValue(new Promise<DiagnosticReport>((done) => { resolve = done; }));
  const onClose = vi.fn();
  render(<TroubleshootingDialog onClose={onClose} />);
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  fireEvent.click(screen.getByRole("button", { name: "Checking…" }));
  expect(runDiagnostics).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getAllByRole("button", { name: "Close Troubleshooting" })[1]);
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(runDiagnostics.mock.calls[0][2].aborted).toBe(true);
  await act(async () => { resolve(report); });
  expect(screen.queryByText("Not checked")).not.toBeInTheDocument();
});

it.each([
  ["saved", "Report saved."], ["requested", "Download requested. Check your downloads."], ["failed", "The report could not be saved. Try again."],
])("reports a %s export honestly", async (kind, message) => {
  downloadBlob.mockResolvedValue({ kind, filename: "phase-diagnostics.json" });
  render(<TroubleshootingDialog onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  await screen.findByRole("button", { name: "Run again" });
  fireEvent.click(screen.getByRole("button", { name: "Download report" }));
  expect(await screen.findByText(message)).toBeInTheDocument();
  expect(downloadBlob).toHaveBeenCalledWith("phase-diagnostics.json", expect.any(Blob));
  expect(downloadBlob.mock.calls[0][1].type).toBe("application/json");
});

it.each([
  [new DOMException("Cancelled", "AbortError"), "Save cancelled."],
  [new Error("SECRET"), "The report could not be saved. Try again."],
])("handles rejected saves without exposing raw errors", async (error, message) => {
  downloadBlob.mockRejectedValue(error);
  render(<TroubleshootingDialog onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  await screen.findByRole("button", { name: "Run again" });
  fireEvent.click(screen.getByRole("button", { name: "Download report" }));
  expect(await screen.findByText(message)).toBeInTheDocument();
  expect(screen.queryByText("SECRET")).not.toBeInTheDocument();
});

it("shows a recoverable failure if checks cannot finish", async () => {
  runDiagnostics.mockRejectedValue(new Error("SECRET"));
  render(<TroubleshootingDialog onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  expect(await screen.findByText("Checks could not finish. Try again.")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Run checks" })).toBeEnabled();
});

it("shows historical engine and unexpected disconnect observations without blaming a deliberate close", async () => {
  const engine = { observedAt: 10, initialized: false, initializing: false, disposed: false, execution: "unavailable" as const };
  const transport = { observedAt: 20, connectionState: null, iceState: null, channelState: null, bufferedBytes: null, pendingSends: 0, pendingDecodes: 0, receiveAgeMs: null, pongAgeMs: null, channelError: null };
  runDiagnostics.mockResolvedValue({ ...report, history: [
    { kind: "engine-not-initialized", observedAt: 10, operation: "getState", engine },
    { kind: "disconnect", observedAt: 20, cause: "send-error", transport, preClose: null },
    { kind: "disconnect", observedAt: 30, cause: "local-close", transport, preClose: null },
  ] });
  render(<TroubleshootingDialog onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  expect(await screen.findByText(/A local engine was not ready earlier/)).toBeInTheDocument();
  expect(screen.getByText(/An unexpected player disconnect was recorded earlier/)).toHaveTextContent(new Date(20).toLocaleString());
  expect(screen.getByText(/Past events do not prove a current outage/)).toBeInTheDocument();
});

it("prioritizes network checks and explains retained setup and route evidence", async () => {
  runDiagnostics.mockResolvedValue({ ...report, results: [
    { check: "credentials", status: "pass", reason: "credentialsReady", observedAt: 2 },
    { check: "signaling", status: "pass", reason: "signalingReady", observedAt: 2 },
    { check: "relay", status: "error", reason: "relayNoEcho", observedAt: 2, evidence: { durationMs: 15, iceErrorCodes: [701] } },
  ], history: [
    { kind: "signaling", side: "Guest", event: "error", error: "network", observedAt: 10 },
    { kind: "candidate-route", observedAt: 20, candidates: { localType: "relay", remoteType: "host", roundTripMs: 10 } },
  ] });
  render(<TroubleshootingDialog onClose={vi.fn()} />);
  expect(screen.getByText(/two temporary connections/)).toBeInTheDocument();
  expect(screen.getByText(/cannot verify another player/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  expect(await screen.findByText("Fresh TURN credentials")).toBeInTheDocument();
  expect(screen.getByText(/ICE error codes: 701/)).toBeInTheDocument();
  expect(screen.getByText(/player connection setup problem was recorded earlier/)).toHaveTextContent(new Date(10).toLocaleString());
  expect(screen.getByText(/Earlier selected route: local relay, remote host/)).toHaveTextContent(new Date(20).toLocaleString());
  expect(runDiagnostics.mock.calls[0][3]).toEqual(expect.any(Function));
});

it("shows negotiation errors, connection labels, and actual TURN transport", async () => {
  runDiagnostics.mockResolvedValue({ ...report, results: [
    { check: "relay", status: "error", reason: "relayConnectionFailed", observedAt: 2, evidence: { connectionError: "negotiation-failed" } },
    { check: "peer", status: "warning", reason: "peerDisconnectedDuringCheck", observedAt: 2, diagnosticId: "anonymous-test" },
    { check: "route", status: "pass", reason: "relayed", observedAt: 2, evidence: { candidates: { localType: "relay", remoteType: "relay", relayProtocol: "tls" } } },
  ] });
  render(<TroubleshootingDialog onClose={vi.fn()} />);
  fireEvent.click(screen.getByRole("button", { name: "Run checks" }));
  expect(await screen.findByText("PeerJS error category: negotiation-failed")).toBeInTheDocument();
  expect(screen.getByText("Diagnostic connection: anonymous-test")).toBeInTheDocument();
  expect(screen.getByText("Client-to-TURN transport: tls")).toBeInTheDocument();
  expect(screen.getByText(/disconnected during these checks/)).toBeInTheDocument();
});
