import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { VisualPackBackendError, type VisualPackBackend } from "../../../services/visualPacks/backend.ts";
import {
  catalogRoot,
  installedRevision,
  operationId,
  packId,
  type CatalogSummary,
  type ProgressEvent,
  type RevisionEvent,
} from "../../../services/visualPacks/types.ts";
import { VisualPackManager } from "./VisualPackManager.tsx";
import i18n from "../../../i18n/index.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";

const platform = vi.hoisted(() => ({ load: vi.fn() }));
vi.mock("../../../services/platform.ts", () => ({ loadVisualPackBackend: platform.load }));
vi.mock("../../../hooks/useSetSymbols.ts", () => ({ useSetCatalog: () => ({ catalog: null, isLoading: false }) }));

const ROOT_A = catalogRoot("a".repeat(64));
const ROOT_B = catalogRoot("b".repeat(64));
const OPERATION = operationId("c".repeat(32));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function summary(root = ROOT_A, revision = "90071992547409930"): CatalogSummary {
  return {
    catalogRoot: root,
    epoch: 1,
    selectorCount: 8,
    shardCount: 3,
    installedRevision: installedRevision(revision),
    installedPacks: [{ packId: packId("core"), catalogRoot: root }],
  };
}

function backend(status: VisualPackBackend["catalogStatus"] = vi.fn(async () => ({ status: "ready" as const, summary: summary() }))) {
  let progress: ((event: ProgressEvent) => void) | null = null;
  let revision: ((event: RevisionEvent) => void) | null = null;
  const value: VisualPackBackend = {
    catalogStatus: status,
    refreshCatalog: vi.fn(async () => summary()),
    catalogSummary: vi.fn(async () => summary()),
    estimateInstall: vi.fn(async (selector) => ({
      catalogRoot: ROOT_A,
      installedRevision: installedRevision("90071992547409930"),
      selector: selector.kind,
      packIds: [packId("core")],
      assetRecords: "1",
      uniqueObjects: "1",
      logicalImageBytes: "2",
      uniqueImageBytes: "2",
      shardCount: "1",
      shardBytes: "3",
    })),
    start: vi.fn(async () => ({ status: "started" as const, operationId: OPERATION, catalogRoot: ROOT_A })),
    cancel: vi.fn(async () => ({
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install" as const, state: "cancelled" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 1, objectsPromoted: 0, completedRevision: null,
    })),
    operationStatus: vi.fn(async () => ({
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install" as const, state: "downloading" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 2, objectsPromoted: 0, completedRevision: null,
    })),
    remove: vi.fn(async () => ({ removed: [], revision: installedRevision("90071992547409931"), cleanupIssues: [] })),
    verify: vi.fn(async () => ({ revision: installedRevision("90071992547409930"), issues: [] })),
    resolve: vi.fn(async () => ({ revision: installedRevision("1"), entries: [] })),
    subscribeProgress: vi.fn(async (listener) => { progress = listener; return vi.fn(); }),
    subscribeRevision: vi.fn(async (listener) => { revision = listener; return vi.fn(); }),
  };
  return { value, emitProgress: (event: ProgressEvent) => progress?.(event), emitRevision: (event: RevisionEvent) => revision?.(event) };
}

describe("VisualPackManager initialization", () => {
  beforeEach(() => platform.load.mockReset());
  afterEach(async () => {
    cleanup();
    usePreferencesStore.getState().setLanguage("en");
    await i18n.changeLanguage("en");
  });

  it("treats plain web as unavailable without making lifecycle calls", async () => {
    platform.load.mockResolvedValue(null);
    render(<VisualPackManager />);
    expect(await screen.findByText(/not configured in this build/i)).toBeInTheDocument();
    expect(screen.getByText(/trusted catalog-signing policy/i)).toBeInTheDocument();
    expect(platform.load).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: /install/i })).not.toBeInTheDocument();
  });

  it("calls status before both subscriptions and disposes exactly one pair", async () => {
    const calls: string[] = [];
    const progressUnlisten = vi.fn();
    const revisionUnlisten = vi.fn();
    const fixture = backend(vi.fn(async () => { calls.push("status"); return { status: "ready" as const, summary: summary() }; }));
    vi.mocked(fixture.value.subscribeProgress).mockImplementation(async () => { calls.push("progress"); return progressUnlisten; });
    vi.mocked(fixture.value.subscribeRevision).mockImplementation(async () => { calls.push("revision"); return revisionUnlisten; });
    platform.load.mockResolvedValue(fixture.value);
    const view = render(<VisualPackManager />);
    expect(await screen.findByText(/Installed catalog status/i)).toBeInTheDocument();
    expect(calls).toEqual(["status", "progress", "revision"]);
    view.unmount();
    expect(progressUnlisten).toHaveBeenCalledTimes(1);
    expect(revisionUnlisten).toHaveBeenCalledTimes(1);
  });

  it("disposes a progress listener whose promise resolves after unmount", async () => {
    const fixture = backend();
    const pending = deferred<() => void>();
    const unlisten = vi.fn();
    vi.mocked(fixture.value.subscribeProgress).mockReturnValue(pending.promise);
    platform.load.mockResolvedValue(fixture.value);
    const view = render(<VisualPackManager />);
    await waitFor(() => expect(fixture.value.subscribeProgress).toHaveBeenCalled());
    view.unmount();
    pending.resolve(unlisten);
    await waitFor(() => expect(unlisten).toHaveBeenCalledTimes(1));
    expect(fixture.value.subscribeRevision).not.toHaveBeenCalled();
  });

  it("latches unsupported shell without subscribing and never displays diagnostics", async () => {
    const fixture = backend(vi.fn(async () => { throw new VisualPackBackendError("unsupported_shell"); }));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    expect(await screen.findByText(/too old to manage/i)).toBeInTheDocument();
    expect(fixture.value.catalogStatus).toHaveBeenCalledTimes(1);
    expect(fixture.value.subscribeProgress).not.toHaveBeenCalled();
    expect(fixture.value.subscribeRevision).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain("visual-pack backend unsupported_shell");
  });

  it("retries a transient failure on the same loaded backend and installs one listener pair", async () => {
    const fixture = backend();
    const retriedStatus = deferred<Awaited<ReturnType<VisualPackBackend["catalogStatus"]>>>();
    vi.mocked(fixture.value.catalogStatus)
      .mockRejectedValueOnce(new VisualPackBackendError("network"))
      .mockReturnValueOnce(retriedStatus.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    fireEvent.click(await screen.findByRole("button", { name: /try again/i }));
    await waitFor(() => expect(fixture.value.catalogStatus).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    retriedStatus.resolve({ status: "ready", summary: summary() });
    expect(await screen.findByText(/Installed catalog status/i)).toBeInTheDocument();
    expect(platform.load).toHaveBeenCalledTimes(1);
    expect(fixture.value.catalogStatus).toHaveBeenCalledTimes(2);
    expect(fixture.value.subscribeProgress).toHaveBeenCalledTimes(1);
    expect(fixture.value.subscribeRevision).toHaveBeenCalledTimes(1);
  });

  it("accepts only a newer revision summary and ignores an unrelated operation event", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.catalogSummary).mockResolvedValue(summary(ROOT_B, "90071992547409931"));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Installed catalog status/i);
    fixture.emitRevision({ cause: "remove", operationId: null, catalogRoot: ROOT_B, revision: installedRevision("90071992547409931") });
    expect(await screen.findByText(ROOT_B)).toBeInTheDocument();
    fixture.emitProgress({
      phase: "failed",
      error: "storage",
      operation: {
        operationId: operationId("d".repeat(32)), catalogRoot: ROOT_A, kind: "install", state: "cancelled",
        packTotal: 1, packsPromoted: 0, objectTotal: 1, objectsPromoted: 0, completedRevision: null,
      },
    });
    await waitFor(() => expect(screen.queryByText(/could not be written/i)).not.toBeInTheDocument());
  });

  it("invalidates an in-flight verification when a same-revision root refresh wins", async () => {
    const fixture = backend();
    const pending = deferred<Awaited<ReturnType<VisualPackBackend["verify"]>>>();
    vi.mocked(fixture.value.verify).mockReturnValue(pending.promise);
    vi.mocked(fixture.value.refreshCatalog).mockResolvedValue(summary(ROOT_B));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Installed catalog status/i);
    fireEvent.click(screen.getByRole("button", { name: /verify metadata/i }));
    fireEvent.click(screen.getByRole("button", { name: /refresh catalog/i }));
    expect(await screen.findByText(ROOT_B)).toBeInTheDocument();
    pending.resolve({ revision: installedRevision("90071992547409930"), issues: [{ kind: "projection_drift" }] });
    await waitFor(() => expect(screen.queryByText(/lookup records differ/i)).not.toBeInTheDocument());
  });

  it("invalidates a displayed estimate when the ready catalog refresh changes root", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.refreshCatalog).mockResolvedValue(summary(ROOT_B));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Installed catalog status/i);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    expect(await screen.findByText(/Backend estimate and dependency closure/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /refresh catalog/i }));
    expect(await screen.findByText(ROOT_B)).toBeInTheDocument();
    expect(screen.queryByText(/Backend estimate and dependency closure/i)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /install selection/i })).toBeDisabled();
  });

  it("allows unrelated estimate and verification reads to overlap", async () => {
    const fixture = backend();
    const estimate = deferred<Awaited<ReturnType<VisualPackBackend["estimateInstall"]>>>();
    const verification = deferred<Awaited<ReturnType<VisualPackBackend["verify"]>>>();
    vi.mocked(fixture.value.estimateInstall).mockReturnValue(estimate.promise);
    vi.mocked(fixture.value.verify).mockReturnValue(verification.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Installed catalog status/i);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    fireEvent.click(screen.getByRole("button", { name: /verify metadata/i }));
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1);
    expect(fixture.value.verify).toHaveBeenCalledWith("metadata");
    estimate.resolve({
      catalogRoot: ROOT_A,
      installedRevision: installedRevision("90071992547409930"),
      selector: "core",
      packIds: [packId("core")],
      assetRecords: "1", uniqueObjects: "1", logicalImageBytes: "2",
      uniqueImageBytes: "2", shardCount: "1", shardBytes: "3",
    });
    verification.resolve({ revision: installedRevision("90071992547409930"), issues: [] });
    expect(await screen.findByText(/No verification issues found/i)).toBeInTheDocument();
  });

  it("renders a representative translated locale through the real manager", async () => {
    usePreferencesStore.getState().setLanguage("es");
    await waitFor(() => expect(i18n.resolvedLanguage).toBe("es"));
    platform.load.mockResolvedValue(null);
    render(<VisualPackManager />);
    expect(await screen.findByText("Catálogo visual sin conexión")).toBeInTheDocument();
    expect(screen.getByText(/no están configuradas en esta compilación/i)).toBeInTheDocument();
  });
});
