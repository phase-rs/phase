import { act, cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  VisualPackBackendError,
  VisualPackStorageRefusalError,
  type VisualPackBackend,
} from "../../../services/visualPacks/backend.ts";
import {
  catalogRoot,
  estimatedImageBytes,
  installedRevision,
  operationId,
  packId,
  type CatalogSummary,
  type CuratedDrift,
  type CuratedInstallSelector,
  type InstallEstimate,
  type ProgressEvent,
  type RevisionEvent,
} from "../../../services/visualPacks/types.ts";
import { shortDigest } from "./packLabels.ts";
import { VisualPackManager } from "./VisualPackManager.tsx";
import { useVisualPackManager } from "./useVisualPackManager.ts";
import i18n from "../../../i18n/index.ts";
import { useConnectivityStore } from "../../../stores/connectivityStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";

const platform = vi.hoisted(() => ({ load: vi.fn() }));
vi.mock("../../../services/platform.ts", () => ({ loadVisualPackBackend: platform.load }));
vi.mock("../../../hooks/useSetSymbols.ts", () => ({ useSetCatalog: () => ({ catalog: null, isLoading: false }) }));

const ROOT_A = catalogRoot("a".repeat(64));
const ROOT_B = catalogRoot("b".repeat(64));
const OPERATION = operationId("c".repeat(32));
const OTHER_OPERATION = operationId("f".repeat(32));
/** A curated pack's root IS its membership digest, so it is deliberately
 *  neither of the catalog roots above — an estimate that matched one of those
 *  would hide a selector-identity bug rather than expose it. */
const CURATED_DIGEST = catalogRoot("d".repeat(64));
const DECK_LIBRARY_DIGEST = catalogRoot("e".repeat(64));

/** The storage half of an `InstallEstimate`, as a browser that answers reports
 *  it. These tests are about the panel, not about storage, so every fixture
 *  uses the same roomy, granted snapshot. */
const STORAGE = {
  usageBytes: 0,
  quotaBytes: 8 * 1024 * 1024 * 1024,
  availableBytes: 8 * 1024 * 1024 * 1024,
  persistence: "persisted" as const,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

/**
 * The default drift answer: nothing installed, so nothing has drifted.
 *
 * `installedDigest: null` is what the backend returns when no curated pack is
 * on disk, and it is the right default here because the default `summary()`
 * installs only `core`. A fixture that reported an installed digest would put
 * a Sync affordance on a panel with no curated pack to sync.
 */
function noDrift(): CuratedDrift {
  return { membershipDigest: CURATED_DIGEST, installedDigest: null, add: 105_165, remove: 0, refresh: 0 };
}

/** A summary whose installed list carries a curated pack at `digest`. */
function curatedSummary(digest = CURATED_DIGEST, revision = "90071992547409930"): CatalogSummary {
  return {
    ...summary(ROOT_A, revision),
    installedPacks: [{ packId: packId("core"), catalogRoot: ROOT_A }, { packId: packId("curated"), catalogRoot: digest }],
  };
}

/** A deck-library-only fixture keeps its membership digest distinct from the
 * catalog root, so a bulk-root comparison cannot accidentally satisfy a local
 * membership assertion. */
function deckLibrarySummary(digest = DECK_LIBRARY_DIGEST, revision = "90071992547409930"): CatalogSummary {
  return {
    ...summary(ROOT_A, revision),
    installedPacks: [{ packId: packId("core"), catalogRoot: ROOT_A }, { packId: packId("deck_library"), catalogRoot: digest }],
  };
}

function localPackSummary(revision = "90071992547409930"): CatalogSummary {
  return {
    ...summary(ROOT_A, revision),
    installedPacks: [
      { packId: packId("core"), catalogRoot: ROOT_A },
      { packId: packId("curated"), catalogRoot: CURATED_DIGEST },
      { packId: packId("deck_library"), catalogRoot: DECK_LIBRARY_DIGEST },
    ],
  };
}

function summary(root = ROOT_A, revision = "90071992547409930"): CatalogSummary {
  return {
    catalogRoot: root,
    epoch: 1,
    selectorCount: 8,
    shardCount: 3,
    installedRevision: installedRevision(revision),
    installedPacks: [{ packId: packId("core"), catalogRoot: root }],
    storage: STORAGE,
  };
}

function backend(status: VisualPackBackend["catalogStatus"] = vi.fn(async () => ({ status: "ready" as const, summary: summary() }))) {
  let progress: ((event: ProgressEvent) => void) | null = null;
  let revision: ((event: RevisionEvent) => void) | null = null;
  const value: VisualPackBackend = {
    catalogStatus: status,
    curatedSelector: vi.fn(async () => ({ kind: "curated" as const, membershipDigest: CURATED_DIGEST })),
    curatedDrift: vi.fn(async () => noDrift()),
    deckLibrarySelector: vi.fn(async () => ({ kind: "deck_library" as const, membershipDigest: DECK_LIBRARY_DIGEST })),
    deckLibraryDrift: vi.fn(async () => null),
    reconcileDeckLibrary: vi.fn(async () => {}),
    refreshCatalog: vi.fn(async () => summary()),
    catalogSummary: vi.fn(async () => summary()),
    estimateInstall: vi.fn(async (selector) => ({
      catalogRoot: ROOT_A,
      installedRevision: installedRevision("90071992547409930"),
      selector: selector.kind,
      packIds: [packId(selector.kind === "curated" || selector.kind === "deck_library" ? selector.kind : "core")],
      assetRecords: "1",
      uniqueObjects: "1",
      logicalImageBytes: "2",
      uniqueImageBytes: "2",
      shardCount: "1",
      shardBytes: "3",
      estimatedImageBytes: estimatedImageBytes(1),
      storage: STORAGE,
      headroom: "sufficient" as const,
    })),
    start: vi.fn(async () => ({
      status: "started" as const, operationId: OPERATION, catalogRoot: ROOT_A, persistence: "persisted" as const,
    })),
    cancel: vi.fn(async () => ({
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install" as const, state: "cancelled" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 1, objectEstimate: null, objectsPromoted: 0, completedRevision: null,
    })),
    operationStatus: vi.fn(async () => ({
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install" as const, state: "downloading" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: null, objectsPromoted: 0, completedRevision: null,
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
  beforeEach(() => {
    platform.load.mockReset();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });
  afterEach(async () => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    usePreferencesStore.getState().setLanguage("en");
    await waitFor(() => expect(i18n.resolvedLanguage).toBe("en"));
  });

  it("treats plain web as unavailable without making lifecycle calls", async () => {
    platform.load.mockResolvedValue(null);
    render(<VisualPackManager />);
    expect(await screen.findByText(/local storage features required/i)).toBeInTheDocument();
    expect(screen.getByText(/Cache Storage, IndexedDB, and gzip decompression/i)).toBeInTheDocument();
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
    expect(await screen.findByText(/Offline card images/i)).toBeInTheDocument();
    expect(calls).toEqual(["status", "progress", "revision"]);
    view.unmount();
    expect(progressUnlisten).toHaveBeenCalledTimes(1);
    expect(revisionUnlisten).toHaveBeenCalledTimes(1);
  });

  it("adopts a background operation and accepts a live lower-progress update", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await waitFor(() => expect(fixture.value.subscribeProgress).toHaveBeenCalled());
    const running = {
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install" as const, state: "downloading" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: 2, objectsPromoted: 1, completedRevision: null,
    };
    fixture.emitProgress({ phase: "running", error: null, operation: running });
    expect(await screen.findByText("1/2")).toBeInTheDocument();

    fixture.emitProgress({ phase: "running", error: null, operation: { ...running, objectsPromoted: 0 } });
    expect(await screen.findByText("0/2")).toBeInTheDocument();

    fixture.emitProgress({ phase: "completed", error: null, operation: { ...running, state: "completed", objectsPromoted: 2, completedRevision: installedRevision("2") } });
    expect(await screen.findByText("Completed")).toBeInTheDocument();
    fixture.emitProgress({
      phase: "started",
      error: null,
      operation: {
        ...running,
        operationId: operationId("d".repeat(32)),
        catalogRoot: ROOT_B,
        kind: "repair",
        objectsPromoted: 0,
        completedRevision: null,
      },
    });
    expect(await screen.findByText("0/2")).toBeInTheDocument();
  });

  it("keeps unknown image totals indeterminate until finalization makes the total authoritative", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await waitFor(() => expect(fixture.value.subscribeProgress).toHaveBeenCalled());
    const operation = {
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "repair" as const, state: "downloading" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 0, objectEstimate: null, objectsPromoted: 0, completedRevision: null,
    };
    fixture.emitProgress({
      phase: "running",
      error: null,
      operation,
    });
    expect(await screen.findByText("Images downloaded: 0")).toBeInTheDocument();
    const progressBars = screen.getAllByRole("progressbar");
    expect(progressBars[progressBars.length - 1]).not.toHaveAttribute("value");

    fixture.emitProgress({ phase: "running", error: null, operation: { ...operation, objectEstimate: 0 } });
    expect(await screen.findByText("0/0")).toBeInTheDocument();
    const knownProgressBars = screen.getAllByRole("progressbar");
    expect(knownProgressBars[knownProgressBars.length - 1]).toHaveAttribute("value", "0");

    fixture.emitProgress({ phase: "running", error: null, operation: { ...operation, objectTotal: 2, objectsPromoted: 1 } });
    expect(await screen.findByText("Images downloaded: 1")).toBeInTheDocument();
    fixture.emitProgress({ phase: "running", error: null, operation: { ...operation, state: "finalizing", objectTotal: 2, objectsPromoted: 1 } });
    expect(await screen.findByText("1/2")).toBeInTheDocument();
  });

  it("restores a removal confirmation to its pointer launcher", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    const prior = await screen.findByRole("button", { name: /verify metadata/i });
    const trigger = screen.getByRole("button", {
      name: /remove all offline visuals/i,
    });

    prior.focus();
    fireEvent.click(trigger);
    const dialog = await screen.findByRole("alertdialog");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus(),
    );
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(trigger).toHaveFocus();
  });

  it("keeps the causal removal launcher across an asynchronous conflict", async () => {
    const fixture = backend();
    const pending = deferred<Awaited<ReturnType<VisualPackBackend["remove"]>>>();
    vi.mocked(fixture.value.remove).mockReturnValue(pending.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    fireEvent.click(screen.getByRole("checkbox"));
    const trigger = screen.getByRole("button", { name: /remove selected/i });

    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("button", { name: /verify metadata/i }));
    pending.reject(new VisualPackBackendError("conflict"));
    const dialog = await screen.findByRole("alertdialog");
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus(),
    );
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(trigger).toHaveFocus();
  });

  it("moves direct selected-removal focus before its launcher disables", async () => {
    const fixture = backend();
    const pending = deferred<Awaited<ReturnType<VisualPackBackend["remove"]>>>();
    vi.mocked(fixture.value.remove).mockReturnValue(pending.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    const heading = await screen.findByRole("heading", {
      name: /offline card images/i,
    });
    fireEvent.click(screen.getByRole("checkbox"));
    const trigger = screen.getByRole("button", { name: /remove selected/i });
    trigger.focus();

    fireEvent.click(trigger);

    await waitFor(() => expect(fixture.value.remove).toHaveBeenCalledOnce());
    expect(trigger).toBeDisabled();
    expect(heading).toHaveFocus();
    expect(document.body).not.toHaveFocus();

    pending.resolve({
      removed: [],
      revision: installedRevision("90071992547409931"),
      cleanupIssues: [],
    });
    await waitFor(() => expect(trigger).toBeEnabled());
  });

  it("hands confirmed removal focus to the durable section heading", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    const trigger = await screen.findByRole("button", {
      name: /remove all offline visuals/i,
    });
    const heading = screen.getByRole("heading", { name: /offline card images/i });

    fireEvent.click(trigger);
    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));

    await waitFor(() => expect(fixture.value.remove).toHaveBeenCalledOnce());
    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(heading).toHaveFocus();
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
    expect(await screen.findByText(/Offline card images/i)).toBeInTheDocument();
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
    await screen.findByText(/Offline card images/i);
    fixture.emitRevision({ cause: "remove", operationId: null, catalogRoot: ROOT_B, revision: installedRevision("90071992547409931") });
    expect(await screen.findByText(shortDigest(ROOT_B))).toBeInTheDocument();
    fixture.emitProgress({
      phase: "failed",
      error: "storage",
      operation: {
        operationId: operationId("d".repeat(32)), catalogRoot: ROOT_A, kind: "install", state: "cancelled",
        packTotal: 1, packsPromoted: 0, objectTotal: 1, objectEstimate: null, objectsPromoted: 0, completedRevision: null,
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
    await screen.findByText(/Offline card images/i);
    fireEvent.click(screen.getByRole("button", { name: /verify metadata/i }));
    fireEvent.click(screen.getByRole("button", { name: /check Scryfall catalog/i }));
    expect(await screen.findByText(shortDigest(ROOT_B))).toBeInTheDocument();
    pending.resolve({ revision: installedRevision("90071992547409930"), issues: [{ kind: "projection_drift" }] });
    await waitFor(() => expect(screen.queryByText(/lookup records differ/i)).not.toBeInTheDocument());
  });

  it("invalidates a displayed estimate when the ready catalog refresh changes root", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.refreshCatalog).mockResolvedValue(summary(ROOT_B));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    expect(await screen.findByText(/Scryfall snapshot scan/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /check Scryfall catalog/i }));
    expect(await screen.findByText(shortDigest(ROOT_B))).toBeInTheDocument();
    expect(screen.queryByText(/Scryfall snapshot scan/i)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /install selection/i })).toBeDisabled();
  });

  it("shows the estimate failure detail beneath its user-facing error", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockRejectedValue(
      new Error("TypeError: DecompressionStream is not a constructor"),
    );
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    expect(await screen.findByText(/catalog operation could not be completed/i)).toBeInTheDocument();
    expect(screen.getByText("TypeError: DecompressionStream is not a constructor")).toBeInTheDocument();
  });

  it("shows catalog scan progress while an estimate is running", async () => {
    const fixture = backend();
    const estimate = deferred<Awaited<ReturnType<VisualPackBackend["estimateInstall"]>>>();
    vi.mocked(fixture.value.estimateInstall).mockImplementation((_selector, onProgress) => {
      onProgress?.({
        compressedBytesRead: 50 * 1024 * 1024,
        compressedBytesTotal: 100 * 1024 * 1024,
        recordsScanned: 12_345,
        assetRecords: 36_000,
      });
      return estimate.promise;
    });
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    expect(await screen.findByRole("progressbar")).toHaveAttribute("value", "52428800");
    expect(screen.getByText(/50 MB of 100 MB read.*12,345 cards scanned.*36,000 images found/i)).toBeInTheDocument();
    estimate.resolve({
      catalogRoot: ROOT_A,
      installedRevision: installedRevision("90071992547409930"),
      selector: "core",
      packIds: [packId("core")],
      assetRecords: "1", uniqueObjects: "1", logicalImageBytes: "2",
      uniqueImageBytes: "2", shardCount: "1", shardBytes: "3",
      estimatedImageBytes: estimatedImageBytes(1), storage: STORAGE, headroom: "sufficient",
    });
    await waitFor(() => expect(screen.queryByRole("progressbar")).not.toBeInTheDocument());
  });

  it("starts an install with the pinned image estimate", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue({
      catalogRoot: ROOT_A,
      installedRevision: installedRevision("90071992547409930"),
      selector: "core",
      packIds: [packId("core")],
      // DISTINCT from `assetRecords` on purpose: PackSelector renders both
      // metrics, so equal values make the `findByText` below match two
      // elements and fail as an ambiguous query. `objectEstimate` is derived
      // from `assetRecords` alone, so the assertion at the end is unaffected.
      assetRecords: "353331", uniqueObjects: "353330", logicalImageBytes: "unknown",
      uniqueImageBytes: "unknown", shardCount: "1", shardBytes: "392267935",
      estimatedImageBytes: estimatedImageBytes(353_331), storage: STORAGE, headroom: "sufficient",
    });
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    await screen.findByText("353331");
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
    expect(fixture.value.start).toHaveBeenCalledWith({
      kind: "install",
      selector: { kind: "core" },
      objectEstimate: 353331,
    });
  });

  it.each(["en", "de", "es", "fr", "it", "pt"])("installs %s through the combined set-printings selector", async (language) => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockImplementation(async (selector) => {
      const id = selector.kind === "printing" ? `printing:${selector.set}`
        : selector.kind === "locale" ? `locale:${selector.language}:${selector.set}` : "core";
      return {
        catalogRoot: ROOT_A, installedRevision: summary().installedRevision,
        selector: id, packIds: [packId(id)],
        assetRecords: "1", uniqueObjects: "1", logicalImageBytes: "2", uniqueImageBytes: "2",
        shardCount: "1", shardBytes: "3", estimatedImageBytes: estimatedImageBytes(1),
        storage: STORAGE, headroom: "sufficient",
      };
    });
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    fireEvent.click(await screen.findByRole("radio", { name: /^Set printings$/i }));
    expect(screen.queryByRole("radio", { name: /English set printings|Localized set printings/i })).not.toBeInTheDocument();
    const languages = screen.getByRole("combobox", { name: /image language/i });
    expect(languages).toHaveValue("en");
    expect(screen.getByRole("option", { name: "English" })).toHaveValue("en");
    expect(screen.getByRole("button", { name: /scan catalog and estimate/i })).toBeDisabled();
    fireEvent.change(screen.getByLabelText(/set code/i), { target: { value: " FIN " } });

    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    await waitFor(() => expect(screen.getByRole("button", { name: /install selection/i })).toBeEnabled());
    expect(fixture.value.estimateInstall).toHaveBeenLastCalledWith({ kind: "printing", set: "fin" }, expect.any(Function));
    if (language !== "en") {
      fireEvent.change(languages, { target: { value: language } });
      expect(screen.getByRole("button", { name: /install selection/i })).toBeDisabled();
      expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1);
      fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
      await waitFor(() => expect(screen.getByRole("button", { name: /install selection/i })).toBeEnabled());
    }
    const selector = language === "en" ? { kind: "printing", set: "fin" }
      : { kind: "locale", language, set: "fin" };
    expect(fixture.value.estimateInstall).toHaveBeenLastCalledWith(selector, expect.any(Function));
    fireEvent.click(screen.getByRole("button", { name: /install selection/i }));
    expect(fixture.value.start).toHaveBeenCalledWith({ kind: "install", selector, objectEstimate: 1 });
  });

  it("allows unrelated estimate and verification reads to overlap", async () => {
    const fixture = backend();
    const estimate = deferred<Awaited<ReturnType<VisualPackBackend["estimateInstall"]>>>();
    const verification = deferred<Awaited<ReturnType<VisualPackBackend["verify"]>>>();
    vi.mocked(fixture.value.estimateInstall).mockReturnValue(estimate.promise);
    vi.mocked(fixture.value.verify).mockReturnValue(verification.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
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
      estimatedImageBytes: estimatedImageBytes(1), storage: STORAGE, headroom: "sufficient",
    });
    verification.resolve({ revision: installedRevision("90071992547409930"), issues: [] });
    expect(await screen.findByText(/No verification issues found/i)).toBeInTheDocument();
  });

  /**
   * Drive the panel to the point where Install is live, then press it.
   *
   * Install is gated on a matching estimate, so the scan has to happen first;
   * waiting on the button's own enabled state rather than on a rendered metric
   * keeps this independent of what the estimate panel chooses to show.
   */
  async function pressInstall(estimateLabel: RegExp, installLabel: RegExp): Promise<void> {
    fireEvent.click(screen.getByRole("button", { name: estimateLabel }));
    await waitFor(() => expect(screen.getByRole("button", { name: installLabel })).toBeEnabled());
    fireEvent.click(screen.getByRole("button", { name: installLabel }));
  }

  /** 5.67 GiB needed against 1 GiB free — the shape `reserveStorage` refuses
   *  with, in the units it reports: raw bytes, for the panel to format. */
  const REFUSAL = { requiredBytes: 6_090_752_000, availableBytes: 1_073_741_824 };

  it("reports a storage refusal as a size, not as engine bytes", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.start).mockRejectedValue(new VisualPackStorageRefusalError(REFUSAL));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    await pressInstall(/scan catalog and estimate/i, /install selection/i);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("6.1 GB");
    expect(alert).toHaveTextContent("1.1 GB");
    // The defect this replaces, asserted as an ABSENCE and on the digits
    // themselves: a grouped "6,090,752,000" would slip past a match on the
    // literal, so compare what is left once every separator is stripped.
    expect(alert.textContent?.replace(/\D/g, "")).not.toContain("6090752000");
    expect(alert.textContent?.replace(/\D/g, "")).not.toContain("1073741824");
    // A refusal carries no verbatim detail line, so the Error's own
    // developer-facing message never reaches the panel either.
    expect(alert).not.toHaveTextContent(/visual-pack backend/i);
  });

  it("renders the refusal size in the active locale's unit and separators", async () => {
    // The half a hand-written `toFixed(1) + " GB"` gets wrong in six of the
    // seven locales: French writes Go, and the decimal separator is a comma.
    usePreferencesStore.getState().setLanguage("fr");
    await waitFor(() => expect(i18n.resolvedLanguage).toBe("fr"));
    const fixture = backend();
    vi.mocked(fixture.value.start).mockRejectedValue(new VisualPackStorageRefusalError(REFUSAL));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText("Catalogue visuel hors ligne");

    await pressInstall(/téléchargement de devis/i, /sélection d'installation/i);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("6,1 Go");
    expect(alert).toHaveTextContent("1,1 Go");
    expect(alert).toHaveTextContent(/espace libre insuffisant/i);
  });

  /** A summary whose storage half the browser answered differently. */
  function summaryWithStorage(storage: CatalogSummary["storage"]): CatalogSummary {
    return { ...summary(), storage };
  }

  it("reports disk in use and the eviction grant without running an estimate", async () => {
    const fixture = backend(vi.fn(async () => ({
      status: "ready" as const,
      summary: summaryWithStorage({ ...STORAGE, usageBytes: 6_500_000_000 }),
    })));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    expect(await screen.findByText(/Storage used by this site/i)).toBeInTheDocument();
    expect(screen.getByText("6.5 GB")).toBeInTheDocument();
    expect(screen.getByText(/will keep these images/i)).toBeInTheDocument();
    // The whole of NB7 + G1: a user who has ALREADY installed can see their
    // disk without first pricing an install they are not making. The figure
    // used to be reachable only through `InstallEstimate`.
    expect(fixture.value.estimateInstall).not.toHaveBeenCalled();
    // And the panel started no operation of its own to get them. That the
    // BACKEND asks for no storage grant while reading a summary is a different
    // claim, checked against the real backend in `installSizing.test.ts` —
    // this fixture is a mock and could not show it.
    expect(fixture.value.start).not.toHaveBeenCalled();
  });

  it("omits the usage row when the browser will not say, rather than showing zero", async () => {
    const fixture = backend(vi.fn(async () => ({
      status: "ready" as const,
      summary: summaryWithStorage({
        usageBytes: null, quotaBytes: null, availableBytes: null, persistence: "unsupported",
      }),
    })));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    expect(await screen.findByText(/will not say/i)).toBeInTheDocument();
    // `null` is unknown, and "0 B" would read as "nothing stored" — a figure
    // the panel would be inventing.
    expect(screen.queryByText(/Storage used by this site/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/^0 /)).not.toBeInTheDocument();
  });

  it("names an ungranted origin as evictable in the status panel", async () => {
    const fixture = backend(vi.fn(async () => ({
      status: "ready" as const,
      summary: summaryWithStorage({ ...STORAGE, usageBytes: 512_000_000, persistence: "best_effort" }),
    })));
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    expect(await screen.findByText(/may delete these images/i)).toBeInTheDocument();

    // REACH GUARD for the absence below. The headroom warning lives inside
    // PackSelector's estimate block, which is UNMOUNTED until an estimate is on
    // screen — asserting it absent without one asserts nothing whatever.
    chooseCurated();
    expect(await screen.findByText(/Estimated download size/i)).toBeInTheDocument();
    // Rendered, not judged: the panel states what the browser reported and
    // computes no headroom verdict from it. This estimate says `sufficient`,
    // so the warning must be absent from a block that is mounted and would
    // otherwise show it. `InstallEstimate.headroom` is where a verdict lives,
    // and the engine produces that one.
    expect(screen.queryByText(/larger than the free space/i)).not.toBeInTheDocument();
  });

  /**
   * A curated estimate as the backend signs one: named by its PACK id — which
   * is what `signedSelectorName` maps a curated selector to, and what the hook
   * checks before accepting an estimate — and taken against the catalog the
   * summary reports rather than against the membership digest.
   *
   * 105,165 image records is the measured curated default — 35,055 non-token
   * faces at three rungs each — so the size it projects is the one a real user
   * reads rather than a toy figure.
   */
  function curatedEstimate(overrides: Partial<InstallEstimate> = {}): InstallEstimate {
    return {
      catalogRoot: ROOT_A,
      installedRevision: installedRevision("90071992547409930"),
      selector: "curated",
      packIds: [packId("curated")],
      assetRecords: "105165", uniqueObjects: "105165",
      logicalImageBytes: "unknown", uniqueImageBytes: "unknown",
      shardCount: "0", shardBytes: "unknown",
      estimatedImageBytes: estimatedImageBytes(105_165),
      storage: STORAGE,
      headroom: "sufficient",
      ...overrides,
    };
  }

  function deckLibraryEstimate(overrides: Partial<InstallEstimate> = {}): InstallEstimate {
    return {
      ...curatedEstimate(),
      selector: "deck_library",
      packIds: [packId("deck_library")],
      ...overrides,
    };
  }

  /** Longer than PackSelector's curated debounce, so an estimate that was
   *  going to fire has fired by the time this resolves. */
  /** A `complete` estimate — what displaces a curated one from the panel. */
  function bulkEstimate(): InstallEstimate {
    return {
      ...curatedEstimate(),
      selector: "complete",
      packIds: [packId("complete")],
      shardCount: "1", shardBytes: "392267935",
    };
  }

  function pastCuratedDebounce(): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, 600));
  }

  function chooseCurated(): void {
    fireEvent.click(screen.getByRole("radio", { name: /one image per card/i }));
  }

  function chooseDeckLibrary(): void {
    fireEvent.click(screen.getByRole("radio", { name: /deck library/i }));
  }

  it("estimates the curated pack on selection and installs it without a second click", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate({
      assetRecords: "6", uniqueObjects: "6", estimatedImageBytes: estimatedImageBytes(6),
    }));
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    expect(fixture.value.curatedSelector).not.toHaveBeenCalled();

    chooseCurated();

    // The panel names a kind; the backend resolves the digest. A UI that built
    // the selector itself would be planning a membership in a display layer,
    // and would be a second assembly of the planner's input beside the one
    // `start()` compares against.
    await waitFor(() => expect(fixture.value.curatedSelector).toHaveBeenCalledTimes(1));
    // No button was pressed between the radio and this call.
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledWith(
      { kind: "curated", membershipDigest: CURATED_DIGEST },
      expect.any(Function),
    ));

    const install = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(install).toBeEnabled());
    fireEvent.click(install);

    expect(fixture.value.start).toHaveBeenCalledWith({
      kind: "install",
      selector: { kind: "curated", membershipDigest: CURATED_DIGEST },
      objectEstimate: 6,
    });
  });

  it("keeps the deck library opt-in idle until selected, then estimates before its explicit install", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate({ assetRecords: "7", uniqueObjects: "7" }));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    expect(fixture.value.deckLibrarySelector).not.toHaveBeenCalled();
    expect(fixture.value.estimateInstall).not.toHaveBeenCalled();
    expect(fixture.value.start).not.toHaveBeenCalled();

    const deckChoice = screen.getByRole("radio", { name: /deck library/i });
    deckChoice.focus();
    fireEvent.click(deckChoice);
    expect(deckChoice).toHaveFocus();
    expect(await screen.findByText(/one image per card used in the shared deck library, including AI Commander precons/i)).toBeInTheDocument();
    expect(screen.getByText(/removing this pack stops that synchronization/i)).toBeInTheDocument();
    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledWith(
      { kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST },
      expect.any(Function),
    ));
    expect(fixture.value.start).not.toHaveBeenCalled();

    const install = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(install).toBeEnabled());
    fireEvent.click(install);
    expect(fixture.value.start).toHaveBeenCalledWith({
      kind: "install",
      selector: { kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST },
      objectEstimate: 7,
    });
  });

  it("keeps a bulk estimate available while the deck-library selector resolves", async () => {
    const fixture = backend();
    const resolvingDeckLibrary = deferred<Awaited<ReturnType<VisualPackBackend["deckLibrarySelector"]>>>();
    vi.mocked(fixture.value.deckLibrarySelector).mockReturnValue(resolvingDeckLibrary.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledOnce());
    fireEvent.click(screen.getByRole("radio", { name: /card back/i }));

    const estimate = screen.getByRole("button", { name: /scan catalog and estimate/i });
    expect(estimate).toBeEnabled();
    fireEvent.click(estimate);
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledWith(
      { kind: "core" },
      expect.any(Function),
    ));
    await waitFor(() => expect(screen.getByRole("button", { name: /install selection/i })).toBeEnabled());

    resolvingDeckLibrary.resolve({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST });
  });

  it("binds automatic estimates to the local pack identity when digests match", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.deckLibrarySelector).mockResolvedValue({ kind: "deck_library", membershipDigest: CURATED_DIGEST });
    vi.mocked(fixture.value.estimateInstall).mockImplementation(async (selector) =>
      selector.kind === "deck_library" ? deckLibraryEstimate() : curatedEstimate(),
    );
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2));
    expect(vi.mocked(fixture.value.estimateInstall).mock.calls.map(([selector]) => selector.kind))
      .toEqual(["curated", "deck_library"]);
  });

  it("retries a rejected deck-library selector only after explicit reselection", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockRejectedValueOnce(new VisualPackBackendError("network"))
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST });
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    expect(await screen.findByRole("alert")).toHaveTextContent(/Scryfall catalog or image download failed/i);
    await pastCuratedDebounce();
    expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("radio", { name: /all current english card images/i }));
    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/Estimated download size/i)).toBeInTheDocument();
  });

  it("does not retry a failed deck-library resolver when an older curated resolver completes", async () => {
    const fixture = backend();
    const pendingCurated = deferred<Awaited<ReturnType<VisualPackBackend["curatedSelector"]>>>();
    vi.mocked(fixture.value.curatedSelector).mockReturnValueOnce(pendingCurated.promise);
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockRejectedValueOnce(new VisualPackBackendError("network"))
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST });
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();
    await waitFor(() => expect(fixture.value.curatedSelector).toHaveBeenCalledTimes(1));
    chooseDeckLibrary();
    expect(await screen.findByRole("alert")).toHaveTextContent(/Scryfall catalog or image download failed/i);
    expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(1);

    pendingCurated.resolve({ kind: "curated", membershipDigest: CURATED_DIGEST });
    await pastCuratedDebounce();

    expect(fixture.value.curatedSelector).toHaveBeenCalledTimes(1);
    expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("alert")).toHaveTextContent(/Scryfall catalog or image download failed/i);

    fireEvent.click(screen.getByRole("button", { name: /recalculate size/i }));
    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/Estimated download size/i)).toBeInTheDocument();
  });

  it("refreshes a stale deck-library request after art preferences invalidate it", async () => {
    const fixture = backend();
    const stale = deferred<Awaited<ReturnType<VisualPackBackend["deckLibrarySelector"]>>>();
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST });
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(1));
    usePreferencesStore.setState({ artChain: [{ type: "newest" }] });
    stale.resolve({ kind: "deck_library", membershipDigest: CURATED_DIGEST });

    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledWith(
      { kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST },
      expect.any(Function),
    ));
  });

  it("shows deck-library membership and removes only that pack", async () => {
    const fixture = backend(vi.fn(async () => ({ status: "ready" as const, summary: localPackSummary() })));
    vi.mocked(fixture.value.catalogSummary).mockResolvedValue(curatedSummary(CURATED_DIGEST, "90071992547409931"));
    vi.mocked(fixture.value.deckLibraryDrift).mockResolvedValue({
      membershipDigest: ROOT_B,
      installedDigest: DECK_LIBRARY_DIGEST,
      add: 3,
      remove: 2,
      refresh: 1,
    });
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    expect(await screen.findByText(/Membership fingerprint: e{12}…/i)).toBeInTheDocument();
    expect(await screen.findByText(/Upgrade available/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: /deck library/i }));
    fireEvent.click(screen.getByRole("button", { name: /remove selected/i }));
    await waitFor(() => expect(fixture.value.remove).toHaveBeenCalledWith(
      { kind: "packs", packIds: [packId("deck_library")] },
      "reject_dependents",
    ));
    await waitFor(() => expect(screen.queryByRole("checkbox", { name: /deck library/i })).not.toBeInTheDocument());
    expect(screen.getByRole("checkbox", { name: /one image per card/i })).toBeInTheDocument();
  });

  it("keeps an installed deck library unmeasured when its drift is unavailable", async () => {
    const fixture = backend(vi.fn(async () => ({ status: "ready" as const, summary: deckLibrarySummary() })));
    vi.mocked(fixture.value.deckLibraryDrift).mockResolvedValue(null);
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await waitFor(() => expect(fixture.value.deckLibraryDrift).toHaveBeenCalledTimes(1));

    chooseDeckLibrary();

    await waitFor(() => expect(screen.getByRole("button", { name: /sync images/i })).toBeEnabled());
    expect(screen.queryByText(/already match/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/to add/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/Upgrade available/i)).not.toBeInTheDocument();
  });

  it("uses the deck-library membership digest rather than a catalog root or curated drift", async () => {
    const fixture = backend(vi.fn(async () => ({ status: "ready" as const, summary: deckLibrarySummary() })));
    vi.mocked(fixture.value.deckLibraryDrift).mockResolvedValue({
      membershipDigest: DECK_LIBRARY_DIGEST,
      installedDigest: DECK_LIBRARY_DIGEST,
      add: 0,
      remove: 0,
      refresh: 0,
    });
    vi.mocked(fixture.value.curatedDrift).mockResolvedValue({
      membershipDigest: ROOT_B,
      installedDigest: CURATED_DIGEST,
      add: 7,
      remove: 5,
      refresh: 3,
    });
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();

    expect(await screen.findByText(/already match/i)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: /sync images/i })).toBeEnabled());
    expect(screen.queryByText(/Upgrade available/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/7 to add, 5 to remove, 3 to refresh/i)).not.toBeInTheDocument();
  });

  it("reports deck-library drift counts from the deck membership and keeps Sync available", async () => {
    const fixture = backend(vi.fn(async () => ({ status: "ready" as const, summary: deckLibrarySummary() })));
    vi.mocked(fixture.value.deckLibraryDrift).mockResolvedValue({
      membershipDigest: ROOT_B,
      installedDigest: DECK_LIBRARY_DIGEST,
      add: 7,
      remove: 5,
      refresh: 3,
    });
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();

    expect(await screen.findByText(/7 to add, 5 to remove, 3 to refresh/i)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: /sync images/i })).toBeEnabled());
    expect(await screen.findByText(/Upgrade available/i)).toBeInTheDocument();
  });

  it("clears stale scan progress when a newer summary invalidates a pending bulk estimate", async () => {
    const fixture = backend();
    const scan = deferred<InstallEstimate>();
    vi.mocked(fixture.value.estimateInstall).mockImplementationOnce((_selector, onProgress) => {
      onProgress?.({ compressedBytesRead: 10, compressedBytesTotal: 20, recordsScanned: 1, assetRecords: 1 });
      return scan.promise;
    });
    vi.mocked(fixture.value.refreshCatalog).mockResolvedValue(summary(ROOT_B, "90071992547409931"));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    fireEvent.click(screen.getByRole("radio", { name: /all current english card images/i }));
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    expect(await screen.findByRole("progressbar")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /check Scryfall catalog/i }));
    await screen.findByText(shortDigest(ROOT_B));
    scan.resolve(bulkEstimate());

    await waitFor(() => expect(screen.queryByRole("progressbar")).not.toBeInTheDocument());
    expect(screen.queryByText(/Scryfall snapshot scan/i)).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("re-estimates a selected deck library after art preferences change to the same digest", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    usePreferencesStore.setState({ artChain: [{ type: "newest" }] });

    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);
  });

  it("recovers a selected deck library estimate after its catalog membership changes", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST })
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: ROOT_B });
    vi.mocked(fixture.value.estimateInstall)
      .mockRejectedValueOnce(new VisualPackBackendError("conflict"))
      .mockResolvedValueOnce(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();

    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenNthCalledWith(
      2,
      { kind: "deck_library", membershipDigest: ROOT_B },
      expect.any(Function),
    ));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    expect(fixture.value.start).not.toHaveBeenCalled();
    expect(fixture.value.curatedSelector).not.toHaveBeenCalled();
  });

  it("requires a second explicit click after recovering a deck-library install conflict", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST })
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: ROOT_B });
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate({ assetRecords: "7" }));
    vi.mocked(fixture.value.start)
      .mockRejectedValueOnce(new VisualPackBackendError("conflict"))
      .mockResolvedValueOnce({ status: "healthy" });
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    const install = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(install).toBeEnabled());
    fireEvent.click(install);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalledTimes(1));

    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenNthCalledWith(
      2,
      { kind: "deck_library", membershipDigest: ROOT_B },
      expect.any(Function),
    ));
    const recoveredInstall = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(recoveredInstall).toBeEnabled());
    expect(fixture.value.start).toHaveBeenCalledTimes(1);

    fireEvent.click(recoveredInstall);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalledTimes(2));
    expect(fixture.value.start).toHaveBeenLastCalledWith({
      kind: "install",
      selector: { kind: "deck_library", membershipDigest: ROOT_B },
      objectEstimate: 7,
    });
  });

  it("ignores a stale deck-library estimate conflict after a newer art selection succeeds", async () => {
    const fixture = backend();
    const stale = deferred<InstallEstimate>();
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST })
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: ROOT_B });
    vi.mocked(fixture.value.estimateInstall)
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValueOnce(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    usePreferencesStore.setState({ artChain: [{ type: "newest" }] });
    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2));
    stale.reject(new VisualPackBackendError("conflict"));

    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenNthCalledWith(
      2,
      { kind: "deck_library", membershipDigest: ROOT_B },
      expect.any(Function),
    ));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    await pastCuratedDebounce();
    expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2);
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);
    expect(fixture.value.start).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("ignores a stale deck-library install conflict after a newer art selection succeeds", async () => {
    const fixture = backend();
    const stale = deferred<Awaited<ReturnType<VisualPackBackend["start"]>>>();
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST })
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: ROOT_B });
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(deckLibraryEstimate());
    vi.mocked(fixture.value.start).mockReturnValue(stale.promise);
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    const install = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(install).toBeEnabled());
    fireEvent.click(install);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalledTimes(1));
    usePreferencesStore.setState({ artChain: [{ type: "newest" }] });
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenNthCalledWith(
      2,
      { kind: "deck_library", membershipDigest: ROOT_B },
      expect.any(Function),
    ));
    stale.reject(new VisualPackBackendError("conflict"));

    const recoveredInstall = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(recoveredInstall).toBeEnabled());
    await pastCuratedDebounce();
    expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2);
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);
    expect(fixture.value.start).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("preserves a pending curated estimate when an older deck-library start conflicts", async () => {
    const fixture = backend();
    const staleStart = deferred<Awaited<ReturnType<VisualPackBackend["start"]>>>();
    const pendingCuratedEstimate = deferred<InstallEstimate>();
    vi.mocked(fixture.value.estimateInstall)
      .mockResolvedValueOnce(deckLibraryEstimate())
      .mockReturnValueOnce(pendingCuratedEstimate.promise);
    vi.mocked(fixture.value.start).mockReturnValue(staleStart.promise);
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    const install = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(install).toBeEnabled());
    fireEvent.click(install);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalledTimes(1));

    chooseCurated();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2));
    staleStart.reject(new VisualPackBackendError("conflict"));
    pendingCuratedEstimate.resolve(curatedEstimate());

    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(1);
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);
  });

  it("stops conflict recovery after a fresh deck-library selector network failure until the user retries", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.deckLibrarySelector)
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: DECK_LIBRARY_DIGEST })
      .mockRejectedValueOnce(new VisualPackBackendError("network"))
      .mockResolvedValueOnce({ kind: "deck_library", membershipDigest: ROOT_B });
    vi.mocked(fixture.value.estimateInstall)
      .mockRejectedValueOnce(new VisualPackBackendError("conflict"))
      .mockResolvedValueOnce(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();

    expect(await screen.findByText(/Scryfall catalog or image download failed/i)).toBeInTheDocument();
    await pastCuratedDebounce();
    expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(2);
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1);
    expect(fixture.value.start).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: /recalculate size/i }));
    await waitFor(() => expect(fixture.value.deckLibrarySelector).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledWith(
      { kind: "deck_library", membershipDigest: ROOT_B },
      expect.any(Function),
    ));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
  });

  it("retries a failed deck estimate after visiting a curated estimate that already matches", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall)
      .mockResolvedValueOnce(curatedEstimate())
      .mockRejectedValueOnce(new VisualPackBackendError("network"))
      .mockResolvedValueOnce(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();

    chooseDeckLibrary();
    expect(await screen.findByRole("alert")).toHaveTextContent(/Scryfall catalog or image download failed/i);
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);

    chooseCurated();
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(3));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(3);
  });

  it("ignores a stale pending deck estimate and performs one fresh estimate", async () => {
    const fixture = backend();
    const stale = deferred<InstallEstimate>();
    vi.mocked(fixture.value.estimateInstall)
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValueOnce(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    usePreferencesStore.setState({ artChain: [{ type: "newest" }] });
    stale.resolve(deckLibraryEstimate());

    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);
  });

  it("ignores a stale pending deck-estimate failure and does not retry it", async () => {
    const fixture = backend();
    const stale = deferred<InstallEstimate>();
    vi.mocked(fixture.value.estimateInstall)
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValueOnce(deckLibraryEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    usePreferencesStore.setState({ artChain: [{ type: "newest" }] });
    stale.reject(new VisualPackBackendError("network"));

    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);
  });

  it("recovers one fresh deck estimate after an installed-revision invalidation releases its pending slot", async () => {
    const fixture = backend();
    const stale = deferred<InstallEstimate>();
    vi.mocked(fixture.value.estimateInstall)
      .mockReturnValueOnce(stale.promise)
      .mockResolvedValueOnce({ ...deckLibraryEstimate(), installedRevision: installedRevision("90071992547409931") });
    vi.mocked(fixture.value.catalogSummary).mockResolvedValue(summary(ROOT_A, "90071992547409931"));
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseDeckLibrary();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    fixture.emitRevision({ cause: "install", operationId: null, catalogRoot: ROOT_A, revision: installedRevision("90071992547409931") });
    await screen.findByText("90071992547409931");
    stale.resolve(deckLibraryEstimate());

    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2);
  });

  it("leaves the bulk selectors' catalog scan behind a deliberate click", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    // Positive control in the same test: curated auto-estimates, so a zero
    // below is a property of the bulk selectors rather than of a debounce that
    // never fired at all.
    chooseCurated();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("radio", { name: /all current english card images/i }));
    await pastCuratedDebounce();

    // `complete` reads the whole multi-gigabyte Scryfall archive. Estimating
    // that because a radio moved would start a very expensive download nobody
    // asked for.
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1);
  });

  it("estimates curated once the scan it arrived during releases the slot", async () => {
    const fixture = backend();
    const scan = deferred<InstallEstimate>();
    vi.mocked(fixture.value.estimateInstall)
      .mockReturnValueOnce(scan.promise)
      .mockResolvedValue(curatedEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    // A bulk scan holds the single estimate slot. `estimateInstall` REFUSES a
    // second request outright, so a curated selection made while this runs is
    // dropped in silence — no estimate, no error, Install disabled for ever.
    fireEvent.click(screen.getByRole("radio", { name: /all current english card images/i }));
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));

    chooseCurated();
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1);

    scan.resolve(bulkEstimate());
    // Freeing the slot must be what re-asks; nothing else changes here.
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2), { timeout: 3000 });
    expect(await screen.findByText(/Estimated download size/i)).toBeInTheDocument();
  });

  it("re-estimates curated after a bulk estimate displaced it", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall)
      .mockResolvedValueOnce(curatedEstimate())
      .mockResolvedValueOnce(bulkEstimate())
      .mockResolvedValue(curatedEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();
    expect(await screen.findByText(/Estimated download size/i)).toBeInTheDocument();

    // A bulk estimate replaces the one on screen. Returning to curated finds
    // no matching estimate, and the auto-estimate must be free to ask again —
    // the plan behind it is memoized, so re-asking costs nothing.
    fireEvent.click(screen.getByRole("radio", { name: /all current english card images/i }));
    fireEvent.click(screen.getByRole("button", { name: /scan catalog and estimate/i }));
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2));

    chooseCurated();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(3), { timeout: 3000 });
    expect(await screen.findByRole("button", { name: /install selection/i })).toBeEnabled();
  });

  it("asks again for an estimate that failed once the option is reselected", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall)
      .mockRejectedValueOnce(new VisualPackBackendError("network"))
      .mockResolvedValue(curatedEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1));
    expect(await screen.findByRole("alert")).toBeInTheDocument();

    // A failed estimate must NOT be retried on the debounce timer — that would
    // be five requests a second at a backend that is already failing — so the
    // key stays recorded and the auto-estimate stays quiet.
    await pastCuratedDebounce();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(1);

    // Reselecting is the recovery the panel offers beside the button, and it
    // only works because leaving the option forgets that we asked.
    fireEvent.click(screen.getByRole("radio", { name: /all current english card images/i }));
    chooseCurated();
    await waitFor(() => expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(2), { timeout: 3000 });
    expect(await screen.findByText(/Estimated download size/i)).toBeInTheDocument();
  });

  it("shows the curated download size and free space, and none of the bulk catalog figures", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    const size = await screen.findByText(/Estimated download size/i);
    const estimatePanel = size.closest("section");
    expect(estimatePanel).toHaveTextContent("6.9 GB");
    expect(estimatePanel).toHaveTextContent(/Free space available/i);
    expect(estimatePanel).toHaveTextContent("8.6 GB");
    // False about a curated pack, not merely uninteresting: it opens no shard
    // of the Scryfall archive, so these describe an archive it never reads.
    expect(estimatePanel).not.toHaveTextContent(/Metadata files/i);
    expect(estimatePanel).not.toHaveTextContent(/Compressed Scryfall catalog/i);
    expect(estimatePanel).not.toHaveTextContent(/known after download/i);
    // No raw byte integer anywhere, compared on the digits alone so a grouped
    // "6,928,003,000" cannot slip past a match on the literal.
    expect(estimatePanel?.textContent?.replace(/\D/g, ""))
      .not.toContain(String(estimatedImageBytes(105_165)));
  });

  it("warns about insufficient headroom without blocking the install", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate({
      headroom: "insufficient",
      storage: { ...STORAGE, quotaBytes: 2_000_000_000, availableBytes: 2_000_000_000 },
    }));
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/larger than the free space/i)).toBeInTheDocument();
    // A warning, never a veto: the projection is an order-of-magnitude figure
    // from six samples per rung, and a quota failure mid-download is the
    // milder outcome because the operation stays resumable.
    await waitFor(() => expect(screen.getByRole("button", { name: /install selection/i })).toBeEnabled());
  });

  it("warns that a best-effort pack may be evicted", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate({
      storage: { ...STORAGE, persistence: "best_effort" },
    }));
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/has not granted persistent storage/i)).toBeInTheDocument();
  });

  it("says the pack follows each card's default art when no art rules are set", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    // The SHIPPED default (preferencesStore), and the state most users are in:
    // with no rules, no override and no deck source, every card falls to its
    // canonical art. Copy claiming "your configured set priority" would credit
    // the user with a choice they have not made.
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/no card art rules/i)).toBeInTheDocument();
    expect(screen.getByText(/default art/i)).toBeInTheDocument();
    // And it names where to change that, because the setting lives in a
    // different panel entirely.
    expect(screen.getByText(/Card Art Preferences/i)).toBeInTheDocument();
  });

  it("says the pack follows the configured art rules once there are any", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [{ type: "newest" }], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/chosen by your Card Art Preferences/i)).toBeInTheDocument();
    expect(screen.queryByText(/no card art rules/i)).not.toBeInTheDocument();
  });

  it("reports a failed curated resolution in the panel's own language", async () => {
    const fixture = backend();
    vi.mocked(fixture.value.curatedSelector).mockRejectedValue(new VisualPackBackendError("network"));
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/Scryfall catalog or image download failed/i);
    // The `Error`'s own developer-facing default must not appear under the
    // translated sentence. It is the message whenever no detail was supplied,
    // and it is English in all seven languages.
    expect(alert).not.toHaveTextContent(/visual-pack backend/i);
  });

  /**
   * A panel whose summary already carries an installed curated pack, plus the
   * drift the backend reports for it.
   *
   * `catalogSummary` is mocked alongside `catalogStatus` because the hook
   * re-reads it after any revision event, and a second answer without the
   * curated pack would silently take the drift indicator off screen.
   */
  function installedCuratedFixture(drift: CuratedDrift | null, installedAt = CURATED_DIGEST) {
    const fixture = backend(vi.fn(async () => ({ status: "ready" as const, summary: curatedSummary(installedAt) })));
    vi.mocked(fixture.value.catalogSummary).mockResolvedValue(curatedSummary(installedAt));
    vi.mocked(fixture.value.curatedDrift).mockResolvedValue(drift);
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate());
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    return fixture;
  }

  it("reports the three-way drift of an installed curated pack and offers Sync", async () => {
    const fixture = installedCuratedFixture(
      { membershipDigest: CURATED_DIGEST, installedDigest: ROOT_B, add: 1200, remove: 34, refresh: 7 },
      ROOT_B,
    );
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/1,200 to add, 34 to remove, 7 to refresh/i)).toBeInTheDocument();
    // Renaming the primary control is how the panel says this is an update to
    // a pack that is already there rather than a new install.
    expect(await screen.findByRole("button", { name: /sync images/i })).toBeInTheDocument();
    // The whole point of a user-initiated Sync: reading the drift downloads
    // nothing. Asserted on the START spy, because a mocked backend downloads
    // nothing whatever the panel does.
    expect(fixture.value.start).not.toHaveBeenCalled();
  });

  it("reports a refresh-only drift rather than an empty diff", async () => {
    // A Scryfall re-scan moves a `sourceUrl` under an unchanged asset key: the
    // membership digest differs, so Sync is live, while both key sets are
    // identical. An add/remove-only report reads as a bug in the panel.
    installedCuratedFixture(
      { membershipDigest: CURATED_DIGEST, installedDigest: ROOT_B, add: 0, remove: 0, refresh: 412 },
      ROOT_B,
    );
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/0 to add, 0 to remove, 412 to refresh/i)).toBeInTheDocument();
  });

  it("says an installed curated pack is up to date when its digest matches", async () => {
    installedCuratedFixture(
      { membershipDigest: CURATED_DIGEST, installedDigest: CURATED_DIGEST, add: 0, remove: 0, refresh: 0 },
    );
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/already match these settings/i)).toBeInTheDocument();
    // The badge is the discoverable half of the same fact, and a curated pack
    // whose digest matches has nothing to upgrade to. It was lit here BY
    // CONSTRUCTION before: a membership digest can never equal a catalog root.
    expect(screen.queryByText(/Upgrade available/i)).not.toBeInTheDocument();
  });

  it("lights the upgrade badge for a curated pack only once drift says so", async () => {
    installedCuratedFixture(
      { membershipDigest: CURATED_DIGEST, installedDigest: ROOT_B, add: 5, remove: 0, refresh: 0 },
      ROOT_B,
    );
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    expect(await screen.findByText(/Upgrade available/i)).toBeInTheDocument();
  });

  it("recomputes drift on an art-preference change without downloading anything", async () => {
    const fixture = installedCuratedFixture(
      { membershipDigest: CURATED_DIGEST, installedDigest: CURATED_DIGEST, add: 0, remove: 0, refresh: 0 },
    );
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await waitFor(() => expect(fixture.value.curatedDrift).toHaveBeenCalledTimes(1));

    vi.mocked(fixture.value.curatedDrift).mockResolvedValue(
      { membershipDigest: ROOT_B, installedDigest: CURATED_DIGEST, add: 9, remove: 2, refresh: 0 },
    );
    usePreferencesStore.setState({ artChain: [{ type: "newest" }] });

    await waitFor(() => expect(fixture.value.curatedDrift).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/Upgrade available/i)).toBeInTheDocument();
    // A preference toggle must never start a multi-gigabyte fetch on its own.
    expect(fixture.value.start).not.toHaveBeenCalled();
  });

  it("names installed packs and shortens their digests instead of quoting wire ids", async () => {
    const fixture = backend(vi.fn(async () => ({
      status: "ready" as const,
      summary: {
        ...summary(),
        installedPacks: [
          { packId: packId("printing:fin"), catalogRoot: ROOT_A },
          { packId: packId("curated"), catalogRoot: CURATED_DIGEST },
        ],
      },
    })));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    // Scoped to the installed list: "One image per card" also names the
    // curated RADIO, and an unscoped query would pass on that alone.
    const installed = (await screen.findByText(/FIN printings/)).closest("section");
    expect(installed).toHaveTextContent(/One image per card/);
    expect(screen.queryByText("printing:fin")).not.toBeInTheDocument();
    // A curated pack's root is its membership, not a snapshot it was built
    // from, so it must not be labelled as one.
    expect(screen.getByText(/Membership fingerprint: dddddddddddd…/)).toBeInTheDocument();
    expect(screen.getByText(/Installed from snapshot: aaaaaaaaaaaa…/)).toBeInTheDocument();
    expect(screen.queryByText(new RegExp(CURATED_DIGEST))).not.toBeInTheDocument();
  });

  it("reports a stale selection as out of date rather than as a dependency", async () => {
    // `conflict` is thrown when the catalog root or the curated membership a
    // selection names is no longer current. `remove()` never throws it — it
    // ignores its `RemovalMode` entirely — so the removal sentence this used to
    // carry described a behaviour no backend in this app has.
    const fixture = backend();
    vi.mocked(fixture.value.start).mockRejectedValue(new VisualPackBackendError("conflict"));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    await pressInstall(/scan catalog and estimate/i, /install selection/i);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/selection is out of date/i);
    expect(alert).not.toHaveTextContent(/depend on this selection/i);
  });

  /** Drive the panel to an operation that has failed and is offering Resume. */
  async function reachFailedOperation(
    fixture: ReturnType<typeof backend>,
    state: "downloading" | "finalizing" = "downloading",
  ): Promise<void> {
    await pressInstall(/scan catalog and estimate/i, /install selection/i);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalled());
    fixture.emitProgress({
      phase: "failed",
      error: "network",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state,
        packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: null, objectsPromoted: 1, completedRevision: null,
      },
    });
    await screen.findByRole("button", { name: /resume operation/i });
  }

  it.each(["downloading", "finalizing"] as const)("adopts a different durable operation after a failed %s operation", async (state) => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await reachFailedOperation(fixture, state);

    await act(async () => {
      fixture.emitProgress({
        phase: "running",
        error: null,
        operation: {
          operationId: OTHER_OPERATION, catalogRoot: ROOT_B, kind: "install", state: "downloading",
          packTotal: 1, packsPromoted: 0, objectTotal: 3, objectEstimate: 3, objectsPromoted: 2, completedRevision: null,
        },
      });
    });

    expect(await screen.findByText("2/3")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /resume operation/i })).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("does not let a different durable operation replace an active operation", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await pressInstall(/scan catalog and estimate/i, /install selection/i);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalled());

    fixture.emitProgress({
      phase: "running",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "downloading",
        packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: 2, objectsPromoted: 1, completedRevision: null,
      },
    });
    expect(await screen.findByText("1/2")).toBeInTheDocument();

    await act(async () => {
      fixture.emitProgress({
        phase: "running",
        error: null,
        operation: {
          operationId: OTHER_OPERATION, catalogRoot: ROOT_B, kind: "install", state: "downloading",
          packTotal: 1, packsPromoted: 0, objectTotal: 3, objectEstimate: 3, objectsPromoted: 2, completedRevision: null,
        },
      });
    });

    expect(screen.getByText("1/2")).toBeInTheDocument();
    expect(screen.queryByText("2/3")).not.toBeInTheDocument();
  });

  it("preserves a failed progress event emitted before a manual start reply", async () => {
    const fixture = backend();
    const started = deferred<Awaited<ReturnType<VisualPackBackend["start"]>>>();
    vi.mocked(fixture.value.start).mockReturnValue(started.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await pressInstall(/scan catalog and estimate/i, /install selection/i);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalled());
    fixture.emitProgress({
      phase: "failed",
      error: "network",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "downloading",
        packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: 2, objectsPromoted: 1, completedRevision: null,
      },
    });
    started.resolve({ status: "started", operationId: OPERATION, catalogRoot: ROOT_A, persistence: "persisted" });

    expect(await screen.findByRole("button", { name: /resume operation/i })).toBeInTheDocument();
  });

  it("reopens a failed operation only when the backend starts its retry", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await reachFailedOperation(fixture);

    fixture.emitProgress({
      phase: "started",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "downloading",
        packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: 2, objectsPromoted: 0, completedRevision: null,
      },
    });
    expect(await screen.findByText("0/2")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /resume operation/i })).not.toBeInTheDocument();

    fixture.emitProgress({
      phase: "completed",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "completed",
        packTotal: 1, packsPromoted: 1, objectTotal: 2, objectEstimate: 2, objectsPromoted: 2, completedRevision: installedRevision("2"),
      },
    });
    expect(await screen.findByText("Completed")).toBeInTheDocument();
  });

  it("accepts reconciliation cancellation after a retryable failure", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await reachFailedOperation(fixture);

    fixture.emitProgress({
      phase: "cancelled",
      error: null,
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "cancelled",
        packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: 2, objectsPromoted: 1, completedRevision: null,
      },
    });

    expect(await screen.findByText("Cancelled")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /resume operation/i })).not.toBeInTheDocument();
  });

  it("reports a terminated operation as stopped, not as a cancellation", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await reachFailedOperation(fixture);

    // The resume path: `run()` terminates a non-retryable operation by writing
    // `state: "cancelled"` — the same value a user's Cancel writes — so the
    // status `trackStarted` reads back cannot say which happened, and only the
    // event that follows it can.
    vi.mocked(fixture.value.operationStatus).mockResolvedValue({
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "cancelled",
      packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: null, objectsPromoted: 1, completedRevision: null,
    });
    fireEvent.click(screen.getByRole("button", { name: /resume operation/i }));
    await waitFor(() => expect(fixture.value.operationStatus).toHaveBeenCalled());

    fixture.emitProgress({
      phase: "failed",
      error: "conflict",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "cancelled",
        packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: null, objectsPromoted: 1, completedRevision: null,
      },
    });

    expect(await screen.findByText(/cannot be resumed/i)).toBeInTheDocument();
    expect(screen.queryByText(/^Cancelled$/)).not.toBeInTheDocument();
    // And the diagnostic that says WHY, which was dropped with the event.
    expect(await screen.findByText(/selection is out of date/i)).toBeInTheDocument();
  });

  it("keeps a deliberate cancellation from being re-reported as a failure", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await pressInstall(/scan catalog and estimate/i, /install selection/i);
    await screen.findByRole("button", { name: /cancel operation/i });

    fireEvent.click(screen.getByRole("button", { name: /cancel operation/i }));
    expect(await screen.findByText("Cancelled")).toBeInTheDocument();

    // A late event for the operation the user just cancelled. The panel
    // witnessed the cancellation, so nothing arriving afterwards may relabel it.
    fixture.emitProgress({
      phase: "failed",
      error: "network",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "cancelled",
        packTotal: 1, packsPromoted: 0, objectTotal: 2, objectEstimate: null, objectsPromoted: 0, completedRevision: null,
      },
    });

    await pastCuratedDebounce();
    expect(screen.getByText("Cancelled")).toBeInTheDocument();
    expect(screen.queryByText(/cannot be resumed/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/image download failed/i)).not.toBeInTheDocument();
  });

  it("says nothing about drift the backend declined to measure", async () => {
    // `null` is the backend refusing to load 76 MB of card data to answer a
    // question nobody asked. It is UNMEASURED, never "no drift" — so the badge
    // and the selector must both stay silent rather than report "up to date".
    const fixture = installedCuratedFixture(null, ROOT_B);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    // Reach guard: the effect really did run and really did ask. Without this
    // the absences below would pass just as well if drift were never read.
    await waitFor(() => expect(fixture.value.curatedDrift).toHaveBeenCalled());

    chooseCurated();
    expect(await screen.findByText(/Estimated download size/i)).toBeInTheDocument();
    expect(screen.queryByText(/Upgrade available/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/already match these settings/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/out of date/i)).not.toBeInTheDocument();
    // But the ACTION is still a sync: a curated pack is on disk, and the
    // summary says so for free. Keying the label on whether drift was measured
    // instead offered to "install" an installed pack — reachable and permanent,
    // because a `curatedSelector()` that rejects leaves both null for the life
    // of the tab.
    expect(screen.getByRole("button", { name: /sync images/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /install selection/i })).not.toBeInTheDocument();
  });

  it("reads a null installed digest as nothing installed, not as total drift", async () => {
    // The divergence F3 named: the badge tested `installedDigest !==
    // membershipDigest`, which is TRUE for a null, while the selector read the
    // same null as "nothing to report". They now ask one predicate, and it
    // compares against the digest the SUMMARY reports installed.
    installedCuratedFixture(
      { membershipDigest: ROOT_B, installedDigest: null, add: 105_165, remove: 0, refresh: 0 },
      ROOT_B,
    );
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();

    expect(await screen.findByText(/already match these settings/i)).toBeInTheDocument();
    expect(screen.queryByText(/Upgrade available/i)).not.toBeInTheDocument();
  });

  it("offers Resume for a finalize that failed, and does not call it unresumable", async () => {
    // `finish()` leaves the record `finalizing` when its transaction rejects;
    // that classifies as `storage`, which is retryable, and `create()`'s pending
    // loop re-runs every `downloading` OR `finalizing` record on next launch.
    // Calling it "stopped, cannot be resumed" was false, and it said so while
    // `durableMutationActive` held every other control disabled.
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await pressInstall(/scan catalog and estimate/i, /install selection/i);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalled());

    fixture.emitProgress({
      phase: "failed",
      error: "storage",
      operation: {
        operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "finalizing",
        packTotal: 1, packsPromoted: 1, objectTotal: 2, objectEstimate: null, objectsPromoted: 2, completedRevision: null,
      },
    });

    expect(await screen.findByText(/ready to resume/i)).toBeInTheDocument();
    expect(screen.queryByText(/cannot be resumed/i)).not.toBeInTheDocument();
    // The label promises a control, so the control has to be there — and it has
    // to reach the backend, which the hook's own guard used to refuse.
    const resume = await screen.findByRole("button", { name: /resume operation/i });
    fireEvent.click(resume);
    await waitFor(() => expect(fixture.value.start).toHaveBeenCalledWith({ kind: "resume", operationId: OPERATION }));
  });

  it("re-reads drift once the user's own action has loaded the card data", async () => {
    // The cold path: mount with a curated pack installed but no card data
    // resident, so the backend declines to measure. Choosing the curated option
    // resolves a selector through the same planner, which loads that data — so
    // the read that was unmeasurable a moment ago is now free, and the panel
    // has to ask again or the badge stays dark until a remount.
    const fixture = installedCuratedFixture(null, ROOT_B);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await waitFor(() => expect(fixture.value.curatedDrift).toHaveBeenCalledTimes(1));
    expect(screen.queryByText(/Upgrade available/i)).not.toBeInTheDocument();

    vi.mocked(fixture.value.curatedDrift).mockResolvedValue(
      { membershipDigest: CURATED_DIGEST, installedDigest: ROOT_B, add: 12, remove: 0, refresh: 3 },
    );
    chooseCurated();

    await waitFor(() => expect(fixture.value.curatedDrift).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/Upgrade available/i)).toBeInTheDocument();
    expect(await screen.findByText(/12 to add, 0 to remove, 3 to refresh/i)).toBeInTheDocument();
  });

  it("does not attribute a curated download's total to the Scryfall snapshot", async () => {
    // `OperationStatus` carries no selector, so this note is shown for every
    // install alike — which made it claim a bulk provenance over a total that
    // came from `planCuratedPack()` over this app's own data files.
    const fixture = backend();
    vi.mocked(fixture.value.estimateInstall).mockResolvedValue(curatedEstimate());
    vi.mocked(fixture.value.operationStatus).mockResolvedValue({
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install", state: "downloading",
      packTotal: 1, packsPromoted: 0, objectTotal: 105_165, objectEstimate: 105_165,
      objectsPromoted: 0, completedRevision: null,
    });
    platform.load.mockResolvedValue(fixture.value);
    usePreferencesStore.setState({ artChain: [], artOverrides: {} });
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);

    chooseCurated();
    const install = await screen.findByRole("button", { name: /install selection/i });
    await waitFor(() => expect(install).toBeEnabled());
    fireEvent.click(install);

    const note = await screen.findByText(/total was fixed when this download started/i);
    expect(note).toBeInTheDocument();
    expect(note).not.toHaveTextContent(/snapshot/i);
    // And the operation's own root is a label, not a 64-character identifier
    // dropped into the most prominent position on the screen.
    const operation = note.closest("section");
    expect(operation).toHaveTextContent("aaaaaaaaaaaa…");
    expect(operation?.textContent).not.toContain(ROOT_A);
  });

  it("renders a representative translated locale through the real manager", async () => {
    usePreferencesStore.getState().setLanguage("es");
    await waitFor(() => expect(i18n.resolvedLanguage).toBe("es"));
    platform.load.mockResolvedValue(null);
    render(<VisualPackManager />);
    expect(await screen.findByText("Catálogo visual sin conexión")).toBeInTheDocument();
    expect(screen.getByText(/no ofrece las funciones de almacenamiento local/i)).toBeInTheDocument();
  });
});

describe("VisualPackManager offline network boundary", () => {
  beforeEach(() => {
    platform.load.mockReset();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });
  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ])("disables network actions while %s but keeps local controls available", async (_name, offline) => {
    useConnectivityStore.setState(offline);
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);

    expect(await screen.findByText(/Network downloads are unavailable while offline/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /check scryfall catalog/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /scan catalog and estimate/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /install selection/i })).toBeDisabled();

    fireEvent.click(screen.getByRole("radio", { name: /one image per card/i }));
    fireEvent.click(screen.getByRole("radio", { name: /deck library/i }));
    expect(fixture.value.curatedSelector).not.toHaveBeenCalled();
    expect(fixture.value.deckLibrarySelector).not.toHaveBeenCalled();
    expect(fixture.value.estimateInstall).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("checkbox"));
    expect(screen.getByRole("button", { name: /repair selected/i })).toBeDisabled();
    const verify = screen.getByRole("button", { name: /verify metadata/i });
    const remove = screen.getByRole("button", { name: /remove selected/i });
    expect(verify).toBeEnabled();
    expect(remove).toBeEnabled();
    fireEvent.click(verify);
    await waitFor(() => expect(fixture.value.verify).toHaveBeenCalledWith("metadata"));
    fireEvent.click(screen.getByRole("button", { name: /verify all files/i }));
    await waitFor(() => expect(fixture.value.verify).toHaveBeenCalledWith("full"));
    fireEvent.click(remove);
    await waitFor(() => expect(fixture.value.remove).toHaveBeenCalled());
    expect(fixture.value.refreshCatalog).not.toHaveBeenCalled();
    expect(fixture.value.start).not.toHaveBeenCalled();
  });

  it.each([
    ["empty", "forced offline", { forcedOffline: true, browserOnline: true }],
    ["empty", "browser offline", { forcedOffline: false, browserOnline: false }],
    ["invalid", "forced offline", { forcedOffline: true, browserOnline: true }],
    ["invalid", "browser offline", { forcedOffline: false, browserOnline: false }],
  ] as const)("does not refresh a %s catalog while %s", async (status, _name, offline) => {
    useConnectivityStore.setState(offline);
    const fixture = backend(vi.fn(async () => status === "empty"
      ? { status: "empty" as const }
      : { status: "invalid" as const }));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);

    expect(await screen.findByRole("button", { name: /check scryfall catalog/i })).toBeDisabled();
    expect(fixture.value.refreshCatalog).not.toHaveBeenCalled();
  });

  it("rechecks a captured stale callback before pending mutation", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    const { result } = renderHook(() => useVisualPackManager());
    await waitFor(() => expect(result.current.availability.kind).toBe("ready"));

    const staleRefresh = result.current.refresh;
    act(() => useConnectivityStore.setState({ forcedOffline: true }));
    await act(async () => { await staleRefresh(); });

    expect(fixture.value.refreshCatalog).not.toHaveBeenCalled();
    expect(result.current.pendingActions.size).toBe(0);
    expect(result.current.availability.kind).toBe("ready");
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ])("cancels a pending curated estimate debounce while %s", async (_name, offline) => {
    const selector = deferred<CuratedInstallSelector>();
    const fixture = backend();
    vi.mocked(fixture.value.curatedSelector).mockReturnValue(selector.promise);
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    const curatedSelection = await screen.findByRole("radio", { name: /one image per card/i });
    fireEvent.click(curatedSelection);
    await waitFor(() => expect(fixture.value.curatedSelector).toHaveBeenCalledTimes(1));
    await act(async () => { selector.resolve({ kind: "curated", membershipDigest: CURATED_DIGEST }); });
    expect(screen.getByRole("button", { name: /recalculate size/i })).toBeEnabled();
    act(() => useConnectivityStore.setState(offline));
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 250)); });

    expect(fixture.value.estimateInstall).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /recalculate size/i })).toBeDisabled();
    expect(screen.queryByText(/working out the size/i)).not.toBeInTheDocument();
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ])("disables Recalculate and Sync while %s after an estimate, then reconnects without auto-starting", async (_name, offline) => {
    const fixture = backend(vi.fn(async () => ({ status: "ready" as const, summary: curatedSummary() })));
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    const curatedSelection = await screen.findByRole("radio", { name: /one image per card/i });
    fireEvent.click(curatedSelection);
    const sync = await screen.findByRole("button", { name: /sync images/i });
    await waitFor(() => expect(sync).toBeEnabled());
    expect(fixture.value.estimateInstall).toHaveBeenCalledWith(
      { kind: "curated", membershipDigest: CURATED_DIGEST },
      expect.any(Function),
    );
    const estimateCalls = vi.mocked(fixture.value.estimateInstall).mock.calls.length;
    expect(screen.getByRole("button", { name: /recalculate size/i })).toBeEnabled();
    expect(sync).toBeEnabled();

    act(() => useConnectivityStore.setState(offline));
    expect(screen.getByRole("button", { name: /recalculate size/i })).toBeDisabled();
    expect(sync).toBeDisabled();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(estimateCalls);

    act(() => useConnectivityStore.setState({ forcedOffline: false, browserOnline: true }));
    expect(screen.getByRole("button", { name: /recalculate size/i })).toBeEnabled();
    expect(sync).toBeEnabled();
    expect(fixture.value.estimateInstall).toHaveBeenCalledTimes(estimateCalls);
    expect(fixture.value.start).not.toHaveBeenCalled();
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ])("disables Resume while %s", async (_name, offline) => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await waitFor(() => expect(fixture.value.subscribeProgress).toHaveBeenCalled());
    const operation = {
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install" as const, state: "downloading" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 1, objectEstimate: 1, objectsPromoted: 0, completedRevision: null,
    };
    fixture.emitProgress({ phase: "failed", error: "network", operation });
    const resume = await screen.findByRole("button", { name: /resume operation/i });
    act(() => useConnectivityStore.setState(offline));
    expect(resume).toBeDisabled();
    expect(fixture.value.start).not.toHaveBeenCalled();
  });

  it("keeps cancel available for a fresh active operation offline", async () => {
    const fixture = backend();
    platform.load.mockResolvedValue(fixture.value);
    render(<VisualPackManager />);
    await screen.findByText(/Offline card images/i);
    await waitFor(() => expect(fixture.value.subscribeProgress).toHaveBeenCalled());
    const operation = {
      operationId: OPERATION, catalogRoot: ROOT_A, kind: "install" as const, state: "downloading" as const,
      packTotal: 1, packsPromoted: 0, objectTotal: 1, objectEstimate: 1, objectsPromoted: 0, completedRevision: null,
    };
    fixture.emitProgress({ phase: "running", error: null, operation });
    act(() => useConnectivityStore.setState({ forcedOffline: true }));
    const cancel = await screen.findByRole("button", { name: /cancel operation/i });
    expect(cancel).toBeEnabled();
    fireEvent.click(cancel);
    await waitFor(() => expect(fixture.value.cancel).toHaveBeenCalledWith(OPERATION));
  });
});
