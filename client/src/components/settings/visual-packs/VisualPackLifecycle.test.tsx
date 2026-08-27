import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { VisualPackBackendError, type VisualPackBackend } from "../../../services/visualPacks/backend.ts";
import {
  catalogRoot,
  installedRevision,
  operationId,
  packId,
  type CatalogSummary,
  type ProgressEvent,
} from "../../../services/visualPacks/types.ts";
import { VisualPackManager } from "./VisualPackManager.tsx";

const platform = vi.hoisted(() => ({ load: vi.fn() }));
vi.mock("../../../services/platform.ts", () => ({ loadVisualPackBackend: platform.load }));
vi.mock("../../../hooks/useSetSymbols.ts", () => ({
  useSetCatalog: () => ({ catalog: { zzz: { name: "Suggestion only", released_at: "2026-01-01" } }, isLoading: false }),
}));

const ROOT = catalogRoot("a".repeat(64));
const ROOT_B = catalogRoot("d".repeat(64));
const OPERATION = operationId("c".repeat(32));
const REVISION = installedRevision("90071992547409930");
const ADVANCED_REVISION = installedRevision("90071992547409931");
let progressListener: ((event: ProgressEvent) => void) | null = null;

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function emitProgress(event: ProgressEvent) {
  progressListener?.(event);
}

function selectorName(selector: Parameters<VisualPackBackend["estimateInstall"]>[0]): string {
  switch (selector.kind) {
    case "core": return "core";
    case "printing": return `printing:${selector.set}`;
    case "locale": return `locale:${selector.language}:${selector.set}`;
    case "complete": return "complete";
  }
}

function summary(): CatalogSummary {
  return {
    catalogRoot: ROOT,
    epoch: 7,
    selectorCount: 9,
    shardCount: 4,
    installedRevision: REVISION,
    installedPacks: [
      { packId: packId("core"), catalogRoot: ROOT },
      { packId: packId("printing:abc"), catalogRoot: catalogRoot("b".repeat(64)) },
    ],
  };
}

function fakeBackend(): VisualPackBackend {
  return {
    catalogStatus: vi.fn(async () => ({ status: "ready" as const, summary: summary() })),
    refreshCatalog: vi.fn(async () => summary()),
    catalogSummary: vi.fn(async () => summary()),
    estimateInstall: vi.fn(async (selector) => ({
      catalogRoot: ROOT,
      installedRevision: REVISION,
      selector: selectorName(selector),
      packIds: [packId("printing:zzz"), packId("core"), packId("printing:abc")],
      assetRecords: "900719925474099312345",
      uniqueObjects: "000-opaque",
      logicalImageBytes: "999999999999999999999",
      uniqueImageBytes: "123456789012345678901",
      shardCount: "17",
      shardBytes: "888888888888888888888",
    })),
    start: vi.fn(async () => ({ status: "started" as const, operationId: OPERATION, catalogRoot: ROOT })),
    cancel: vi.fn(async () => ({
      operationId: OPERATION, catalogRoot: ROOT, kind: "install" as const, state: "cancelled" as const,
      packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
    })),
    operationStatus: vi.fn(async () => ({
      operationId: OPERATION, catalogRoot: ROOT, kind: "install" as const, state: "downloading" as const,
      packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
    })),
    remove: vi.fn(async () => ({
      removed: [],
      revision: REVISION,
      cleanupIssues: [
        { kind: "malformed_entry" as const },
        { kind: "unsafe_entry" as const },
        { kind: "remove_failed" as const },
        { kind: "catalog_state" as const },
      ],
    })),
    verify: vi.fn(async () => ({
      revision: REVISION,
      issues: [
        { kind: "missing_root_witness" as const },
        { kind: "invalid_root_witness" as const },
        { kind: "receipt_metadata" as const },
        { kind: "missing_shard" as const },
        { kind: "invalid_shard" as const },
        { kind: "missing_object" as const },
        { kind: "invalid_object_metadata" as const },
        { kind: "corrupt_object" as const },
        { kind: "dependency_drift" as const },
        { kind: "projection_drift" as const },
      ],
    })),
    resolve: vi.fn(async () => ({ revision: REVISION, entries: [] })),
    subscribeProgress: vi.fn(async (listener) => { progressListener = listener; return () => {}; }),
    subscribeRevision: vi.fn(async () => () => {}),
  };
}

async function ready(backend: VisualPackBackend) {
  platform.load.mockResolvedValue(backend);
  render(<VisualPackManager />);
  await screen.findByText(/Installed catalog status/i);
}

describe("VisualPackManager lifecycle", () => {
  beforeEach(() => {
    platform.load.mockReset();
    progressListener = null;
  });
  afterEach(cleanup);

  it("normalizes every selector family and preserves backend estimate order and decimal strings", async () => {
    const backend = fakeBackend();
    await ready(backend);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await waitFor(() => expect(backend.estimateInstall).toHaveBeenLastCalledWith({ kind: "core" }));

    fireEvent.click(screen.getByRole("radio", { name: /English set printings/i }));
    fireEvent.change(screen.getByLabelText(/set code/i), { target: { value: "  ABC  " } });
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await waitFor(() => expect(backend.estimateInstall).toHaveBeenLastCalledWith({ kind: "printing", set: "abc" }));
    expect(screen.getAllByRole("listitem").map((node) => node.textContent)).toEqual(["printing:zzz", "core", "printing:abc"]);
    expect(screen.getByText("900719925474099312345")).toBeInTheDocument();
    expect(screen.getByText("000-opaque")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("radio", { name: /Localized set printings/i }));
    for (const language of ["de", "es", "fr", "it", "pt"]) {
      fireEvent.change(screen.getByRole("combobox", { name: /image language/i }), { target: { value: language } });
      fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
      await waitFor(() => expect(backend.estimateInstall).toHaveBeenLastCalledWith({ kind: "locale", language, set: "abc" }));
    }

    fireEvent.click(screen.getByRole("radio", { name: /Complete current catalog/i }));
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await waitFor(() => expect(backend.estimateInstall).toHaveBeenLastCalledWith({ kind: "complete", rootSha256: ROOT }));
  });

  it("rejects invalid and Polish selectors and performs the started status-gap query", async () => {
    const backend = fakeBackend();
    await ready(backend);
    fireEvent.click(screen.getByRole("checkbox", { name: /^core\b/i }));
    fireEvent.click(screen.getByRole("radio", { name: /Localized set printings/i }));
    fireEvent.change(screen.getByLabelText(/set code/i), { target: { value: "../x" } });
    expect(screen.getByRole("button", { name: /estimate download/i })).toBeDisabled();
    expect(screen.getByRole("combobox", { name: /image language/i }).querySelector('option[value="pl"]')).toBeNull();
    expect(backend.estimateInstall).not.toHaveBeenCalled();

    fireEvent.change(screen.getByLabelText(/set code/i), { target: { value: "abc" } });
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await waitFor(() => expect(screen.getByRole("button", { name: /install selection/i })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith({ kind: "install", selector: { kind: "locale", language: "de", set: "abc" } }));
    expect(backend.operationStatus).toHaveBeenCalledWith(OPERATION);
    expect(await screen.findByText(/Packs: 1 of 3/i)).toBeInTheDocument();
    expect(screen.getAllByText(ROOT, { selector: "dd" })).toHaveLength(2);

    act(() => emitProgress({
      phase: "running",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
        packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
      },
    }));
    expect(await screen.findByRole("button", { name: /cancel operation/i })).toBeInTheDocument();
    expect(screen.getAllByRole("progressbar")[0]).toHaveAttribute("value", "1");
    expect(screen.getAllByRole("progressbar")[0]).toHaveAttribute("max", "3");
    expect(screen.getByRole("button", { name: /install selection/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /repair selected/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /remove all offline visuals/i })).toBeDisabled();

    act(() => emitProgress({
      phase: "failed",
      error: "network",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
        packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
      },
    }));
    expect(await screen.findByText(/ready to resume/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /cancel operation/i })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /resume operation/i }));
    await waitFor(() => expect(backend.start).toHaveBeenLastCalledWith({ kind: "resume", operationId: OPERATION }));

    act(() => emitProgress({
      phase: "running",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "cancel_requested",
        packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
      },
    }));
    expect(await screen.findByText(/Cancellation requested/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /install selection/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /repair selected/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /remove selected/i })).toBeDisabled();

    act(() => emitProgress({
      phase: "running",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "finalizing",
        packTotal: 3, packsPromoted: 3, objectTotal: 9, objectsPromoted: 9, completedRevision: null,
      },
    }));
    expect(await screen.findByText(/^Finalizing$/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /remove all offline visuals/i })).toBeDisabled();
  });

  it("does not offer resume after cancellation and reaches the healthy install branch", async () => {
    const backend = fakeBackend();
    vi.mocked(backend.start).mockResolvedValue({ status: "healthy" });
    await ready(backend);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await screen.findByText(/Backend estimate/i);
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
    await waitFor(() => expect(backend.catalogSummary).toHaveBeenCalled());
    expect(screen.queryByRole("button", { name: /resume operation/i })).not.toBeInTheDocument();
  });

  it("latches a failed progress event against stale running progress after adoption", async () => {
    const backend = fakeBackend();
    const pendingStatus = deferred<Awaited<ReturnType<VisualPackBackend["operationStatus"]>>>();
    vi.mocked(backend.operationStatus).mockReturnValue(pendingStatus.promise);
    await ready(backend);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await screen.findByText(/Backend estimate/i);
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
    await waitFor(() => expect(backend.operationStatus).toHaveBeenCalledWith(OPERATION));
    act(() => emitProgress({
      phase: "failed",
      error: "network",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
        packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
      },
    }));
    act(() => emitProgress({
      phase: "running",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
        packTotal: 3, packsPromoted: 2, objectTotal: 9, objectsPromoted: 4, completedRevision: null,
      },
    }));
    expect(await screen.findByRole("button", { name: /resume operation/i })).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(/network request failed/i);
    pendingStatus.resolve({
      operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
      packTotal: 3, packsPromoted: 0, objectTotal: 9, objectsPromoted: 0, completedRevision: null,
    });
    await waitFor(() => expect(screen.getByRole("button", { name: /resume operation/i })).toBeInTheDocument());
  });

  it.each(["install", "repair"] as const)(
    "latches a failed %s event against stale running progress before adoption",
    async (kind) => {
      const backend = fakeBackend();
      vi.mocked(backend.start).mockImplementation(async () => {
        emitProgress({
          phase: "failed",
          error: "network",
          operation: {
            operationId: OPERATION, catalogRoot: ROOT, kind, state: "downloading",
            packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
          },
        });
        emitProgress({
          phase: "running",
          error: null,
          operation: {
            operationId: OPERATION, catalogRoot: ROOT, kind, state: "downloading",
            packTotal: 3, packsPromoted: 2, objectTotal: 9, objectsPromoted: 4, completedRevision: null,
          },
        });
        return { status: "started", operationId: OPERATION, catalogRoot: ROOT };
      });
      await ready(backend);
      if (kind === "install") {
        fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
        await screen.findByText(/Backend estimate/i);
        fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
        await waitFor(() => expect(backend.start).toHaveBeenCalledWith({ kind: "install", selector: { kind: "core" } }));
      } else {
        fireEvent.click(screen.getByRole("checkbox", { name: /^core\b/i }));
        fireEvent.click(screen.getByRole("button", { name: /repair selected/i }));
        await waitFor(() => expect(backend.start).toHaveBeenCalledWith({ kind: "repair", packIds: [packId("core")] }));
      }
      expect(await screen.findByText(/ready to resume/i)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /resume operation/i })).toBeEnabled();
      expect(screen.getByRole("alert")).toHaveTextContent(/network request failed/i);
      expect(backend.operationStatus).toHaveBeenCalledWith(OPERATION);
    },
  );

  it("keeps buffered finalizing progress when stale downloading progress arrives before adoption", async () => {
    const backend = fakeBackend();
    vi.mocked(backend.start).mockImplementation(async () => {
      emitProgress({
        phase: "running",
        error: null,
        operation: {
          operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "finalizing",
          packTotal: 3, packsPromoted: 3, objectTotal: 9, objectsPromoted: 9, completedRevision: null,
        },
      });
      emitProgress({
        phase: "running",
        error: null,
        operation: {
          operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
          packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
        },
      });
      return { status: "started", operationId: OPERATION, catalogRoot: ROOT };
    });
    await ready(backend);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await screen.findByText(/Backend estimate/i);
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));

    expect(await screen.findByText(/^Finalizing$/i)).toBeInTheDocument();
    expect(screen.getByText(/Packs: 3 of 3/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /cancel operation/i })).not.toBeInTheDocument();
  });

  it.each(["before", "after"] as const)(
    "latches a completed outcome against a stale cancelled event emitted %s adoption",
    async (timing) => {
      const backend = fakeBackend();
      const completed: ProgressEvent = {
        phase: "completed",
        error: null,
        operation: {
          operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "completed",
          packTotal: 3, packsPromoted: 3, objectTotal: 9, objectsPromoted: 9,
          completedRevision: ADVANCED_REVISION,
        },
      };
      const staleCancelled: ProgressEvent = {
        phase: "cancelled",
        error: "cancelled",
        operation: {
          operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "cancelled",
          packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2,
          completedRevision: null,
        },
      };
      if (timing === "before") {
        vi.mocked(backend.start).mockImplementation(async () => {
          emitProgress(completed);
          emitProgress(staleCancelled);
          return { status: "started", operationId: OPERATION, catalogRoot: ROOT };
        });
      }
      await ready(backend);
      fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
      await screen.findByText(/Backend estimate/i);
      fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
      await waitFor(() => expect(backend.start).toHaveBeenCalled());
      if (timing === "after") {
        await screen.findByRole("button", { name: /cancel operation/i });
        act(() => {
          emitProgress(completed);
          emitProgress(staleCancelled);
        });
      }
      expect(await screen.findByText(/^Completed$/i)).toBeInTheDocument();
      expect(screen.queryByText(/^Cancelled$/i)).not.toBeInTheDocument();
    },
  );

  it("clears a failed alert on resumed progress and disables cancel until the resume start settles", async () => {
    const backend = fakeBackend();
    const resumed = deferred<Awaited<ReturnType<VisualPackBackend["start"]>>>();
    vi.mocked(backend.start)
      .mockResolvedValueOnce({ status: "started", operationId: OPERATION, catalogRoot: ROOT })
      .mockReturnValueOnce(resumed.promise);
    await ready(backend);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await screen.findByText(/Backend estimate/i);
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
    expect(await screen.findByRole("button", { name: /cancel operation/i })).toBeEnabled();
    act(() => emitProgress({
      phase: "failed",
      error: "network",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
        packTotal: 3, packsPromoted: 1, objectTotal: 9, objectsPromoted: 2, completedRevision: null,
      },
    }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/network request failed/i);
    fireEvent.click(screen.getByRole("button", { name: /resume operation/i }));
    await waitFor(() => expect(backend.start).toHaveBeenLastCalledWith({ kind: "resume", operationId: OPERATION }));
    act(() => emitProgress({
      phase: "running",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT, kind: "install", state: "downloading",
        packTotal: 3, packsPromoted: 2, objectTotal: 9, objectsPromoted: 4, completedRevision: null,
      },
    }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /cancel operation/i })).toBeDisabled();
    resumed.resolve({ status: "started", operationId: OPERATION, catalogRoot: ROOT });
    await waitFor(() => expect(screen.getByRole("button", { name: /cancel operation/i })).toBeEnabled());
  });

  it("cancels only the exact running operation and never resumes the cancelled state", async () => {
    const backend = fakeBackend();
    await ready(backend);
    fireEvent.click(screen.getByRole("button", { name: /estimate download/i }));
    await screen.findByText(/Backend estimate/i);
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
    expect(await screen.findByRole("button", { name: /cancel operation/i })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /cancel operation/i }));
    await waitFor(() => expect(backend.cancel).toHaveBeenCalledWith(OPERATION));
    expect(screen.queryByRole("button", { name: /resume operation/i })).not.toBeInTheDocument();
  });

  it("visibly disables every PackStatus mutation while a repair request is pending", async () => {
    const backend = fakeBackend();
    const repair = deferred<Awaited<ReturnType<VisualPackBackend["start"]>>>();
    vi.mocked(backend.start).mockReturnValue(repair.promise);
    await ready(backend);
    fireEvent.click(screen.getByRole("checkbox", { name: /^core\b/i }));
    fireEvent.click(screen.getByRole("button", { name: /repair selected/i }));
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith({ kind: "repair", packIds: [packId("core")] }));

    for (const name of [
      /repair selected/i,
      /remove selected/i,
      /remove complete catalog/i,
      /remove all offline visuals/i,
    ]) expect(screen.getByRole("button", { name })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /remove all offline visuals/i }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(backend.remove).not.toHaveBeenCalled();

    repair.resolve({ status: "healthy" });
    await waitFor(() => expect(screen.getByRole("button", { name: /repair selected/i })).toBeEnabled());
  });

  it("clears a failed verification alert as soon as a retry acquires its pending action", async () => {
    const backend = fakeBackend();
    const retry = deferred<Awaited<ReturnType<VisualPackBackend["verify"]>>>();
    vi.mocked(backend.verify)
      .mockRejectedValueOnce(new VisualPackBackendError("network"))
      .mockReturnValueOnce(retry.promise);
    await ready(backend);
    fireEvent.click(screen.getByRole("button", { name: /verify metadata/i }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/network request failed/i);

    fireEvent.click(screen.getByRole("button", { name: /verify metadata/i }));
    await waitFor(() => expect(backend.verify).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    retry.resolve({ revision: REVISION, issues: [] });
    expect(await screen.findByText(/No verification issues found/i)).toBeInTheDocument();
  });

  it("reconciles the authoritative summary when repair start rejects", async () => {
    const backend = fakeBackend();
    vi.mocked(backend.start).mockRejectedValue(new VisualPackBackendError("network"));
    vi.mocked(backend.catalogSummary).mockResolvedValue({
      ...summary(),
      catalogRoot: ROOT_B,
      installedRevision: ADVANCED_REVISION,
      installedPacks: [{ packId: packId("core"), catalogRoot: ROOT_B }],
    });
    await ready(backend);
    fireEvent.click(screen.getByRole("checkbox", { name: /^core\b/i }));
    fireEvent.click(screen.getByRole("button", { name: /repair selected/i }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/network request failed/i);
    await waitFor(() => expect(backend.catalogSummary).toHaveBeenCalled());
    expect(await screen.findAllByText(ROOT_B)).not.toHaveLength(0);
    expect(screen.getByText(ADVANCED_REVISION)).toBeInTheDocument();
  });

  it("verifies exact modes, repairs checked ids, and freezes conflict escalation", async () => {
    const backend = fakeBackend();
    vi.mocked(backend.start).mockResolvedValue({ status: "healthy" });
    vi.mocked(backend.remove)
      .mockRejectedValueOnce(new VisualPackBackendError("conflict"))
      .mockResolvedValueOnce({
        removed: [],
        revision: REVISION,
        cleanupIssues: [
          { kind: "malformed_entry" },
          { kind: "unsafe_entry" },
          { kind: "remove_failed" },
          { kind: "catalog_state" },
        ],
      });
    await ready(backend);

    fireEvent.click(screen.getByRole("button", { name: /verify metadata/i }));
    await waitFor(() => expect(backend.verify).toHaveBeenCalledWith("metadata"));
    for (const issue of [
      /Missing catalog-root witness/i,
      /Invalid catalog-root witness/i,
      /Receipt metadata differs/i,
      /Missing catalog shard/i,
      /Invalid catalog shard/i,
      /Missing image object/i,
      /Invalid image-object metadata/i,
      /Corrupt image object/i,
      /dependency records differ/i,
      /lookup records differ/i,
    ]) expect(await screen.findByText(issue)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /verify all files/i }));
    await waitFor(() => expect(backend.verify).toHaveBeenCalledWith("full"));

    fireEvent.click(screen.getByRole("checkbox", { name: /^core\b/i }));
    fireEvent.click(screen.getByRole("button", { name: /repair selected/i }));
    await waitFor(() => expect(backend.start).toHaveBeenCalledWith({ kind: "repair", packIds: [packId("core")] }));

    fireEvent.click(screen.getByRole("button", { name: /remove selected/i }));
    expect(await screen.findByRole("alertdialog", { name: /Remove dependent packs/i })).toHaveTextContent("core");
    fireEvent.click(screen.getByRole("button", { name: /Remove dependents/i }));
    await waitFor(() => expect(backend.remove).toHaveBeenLastCalledWith({ kind: "packs", packIds: [packId("core")] }, "cascade_dependents"));
    for (const issue of [
      /malformed cache entry/i,
      /unsafe cache entry/i,
      /cache file could not be removed/i,
      /Historical catalog state/i,
    ]) expect(await screen.findByText(issue)).toBeInTheDocument();
  });

  it("freezes complete and all-installed removals and preserves dialog focus and Escape", async () => {
    const backend = fakeBackend();
    await ready(backend);
    const trigger = screen.getByRole("button", { name: /remove complete catalog/i });
    trigger.focus();
    fireEvent.click(trigger);
    const dialog = await screen.findByRole("alertdialog", { name: /Remove this complete catalog/i });
    expect(dialog).toHaveTextContent(ROOT);
    await waitFor(() => expect(screen.getByRole("button", { name: /^Cancel$/i })).toHaveFocus());
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());

    fireEvent.click(screen.getByRole("button", { name: /remove all offline visuals/i }));
    fireEvent.click(await screen.findByRole("button", { name: /^Remove$/i }));
    await waitFor(() => expect(backend.remove).toHaveBeenLastCalledWith({ kind: "all_installed" }, "reject_dependents"));
  });

});
