// @vitest-environment happy-dom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConnectivityStore } from "../../../stores/connectivityStore.ts";

const mocks = vi.hoisted(() => ({ prepare: vi.fn() }));

vi.mock("../../../services/offlinePreparation.ts", () => ({ prepareForOffline: mocks.prepare }));

import { OfflinePreparationSection } from "../OfflinePreparationSection.tsx";

const ready = {
  status: "ready" as const,
  capabilities: {
    appShell: { status: "ready" as const },
    browserEngine: { status: "ready" as const },
    scryfallSearch: { status: "ready" as const },
    preconCatalog: { status: "ready" as const },
    bundledAiCatalog: { status: "ready" as const },
    deckLibrary: { status: "not-installed" as const },
    coreVisuals: { status: "ready" as const },
    nativeEngine: { status: "not-applicable" as const },
  },
  visualPacks: { status: "not-installed" as const, installedPacks: [] },
  requiredGaps: [],
};

const incomplete = {
  ...ready,
  status: "failed" as const,
  capabilities: { ...ready.capabilities, browserEngine: { status: "not-ready" as const } },
  requiredGaps: ["browserEngine"],
};

describe("OfflinePreparationSection", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.prepare.mockResolvedValue(ready);
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it("prepares without changing Work Offline policy", async () => {
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    await screen.findByText("This device is ready for offline local play.");
    expect(useConnectivityStore.getState().forcedOffline).toBe(false);
    expect(screen.getByRole("list", { name: "Offline readiness checklist" })).toBeInTheDocument();
  });

  it("reruns fresh preparation before enabling Work Offline", async () => {
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);
    const checkbox = screen.getByRole("checkbox", { name: "Work Offline" });

    fireEvent.click(checkbox);

    await vi.waitFor(() => expect(mocks.prepare).toHaveBeenCalledOnce());
    expect(useConnectivityStore.getState().forcedOffline).toBe(true);
  });

  it("runs a new enable-offline preparation after an earlier manual result", async () => {
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));
    await screen.findByText("This device is ready for offline local play.");
    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));

    await vi.waitFor(() => expect(mocks.prepare).toHaveBeenCalledTimes(2));
    expect(useConnectivityStore.getState().forcedOffline).toBe(true);
  });

  it("keeps policy online until a held enable-offline preparation is ready", async () => {
    let resolve!: (value: typeof ready) => void;
    mocks.prepare.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));

    expect(useConnectivityStore.getState().forcedOffline).toBe(false);
    expect(screen.getByRole("status")).toHaveTextContent("Preparing local play capabilities…");
    await act(async () => { resolve(ready); });
    expect(useConnectivityStore.getState().forcedOffline).toBe(true);
  });

  it("requires confirmation before enabling Work Offline with required gaps", async () => {
    mocks.prepare.mockResolvedValueOnce(incomplete);
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));

    await screen.findByRole("alertdialog");
    expect(useConnectivityStore.getState().forcedOffline).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(useConnectivityStore.getState().forcedOffline).toBe(false);
  });

  it("applies Work Offline only after explicit confirmation", async () => {
    mocks.prepare.mockResolvedValueOnce(incomplete);
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));
    await screen.findByRole("alertdialog");
    fireEvent.click(screen.getByRole("button", { name: "Enable Work Offline" }));

    expect(useConnectivityStore.getState().forcedOffline).toBe(true);
  });

  it("turns Work Offline off immediately without preparation", () => {
    useConnectivityStore.setState({ forcedOffline: true });
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));

    expect(useConnectivityStore.getState().forcedOffline).toBe(false);
    expect(mocks.prepare).not.toHaveBeenCalled();
  });

  it("does not call a warmer when an already-offline toggle is requested", async () => {
    useConnectivityStore.setState({ browserOnline: false });
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));

    await screen.findByText("Reconnect before preparing offline local play.");
    expect(mocks.prepare).not.toHaveBeenCalled();
    expect(screen.getByText("Native engine").parentElement).toHaveTextContent("Not applicable");
  });

  it("supersedes a held request with reconnect-required without leaving stale preparation UI", async () => {
    let resolve!: (value: typeof incomplete) => void;
    mocks.prepare.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));
    expect(screen.getByRole("status")).toHaveTextContent("Preparing local play capabilities…");
    await act(async () => {
      useConnectivityStore.setState({ browserOnline: false });
    });
    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));

    await screen.findByText("Reconnect before preparing offline local play.");
    expect(screen.getByRole("button", { name: "Prepare for Offline" })).toBeEnabled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(useConnectivityStore.getState().forcedOffline).toBe(false);
    await act(async () => { resolve(incomplete); });
    expect(screen.getByRole("status")).toHaveTextContent("Reconnect before preparing offline local play.");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("keeps the live preparing state ahead of an earlier ready result", async () => {
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));
    await screen.findByText("This device is ready for offline local play.");
    let resolve!: (value: typeof ready) => void;
    mocks.prepare.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    expect(screen.getByRole("status")).toHaveTextContent("Preparing local play capabilities…");
    await act(async () => { resolve(ready); });
  });

  it("suppresses duplicate preparation clicks but allows a newer enable-offline intent", async () => {
    let resolveFirst!: (value: typeof incomplete) => void;
    let resolveSecond!: (value: typeof ready) => void;
    mocks.prepare
      .mockImplementationOnce(() => new Promise((done) => { resolveFirst = done; }))
      .mockImplementationOnce(() => new Promise((done) => { resolveSecond = done; }));
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    const prepare = screen.getByRole("button", { name: "Prepare for Offline" });
    fireEvent.click(prepare);
    fireEvent.click(prepare);
    expect(mocks.prepare).toHaveBeenCalledOnce();
    expect(prepare).toBeDisabled();

    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));
    await vi.waitFor(() => expect(mocks.prepare).toHaveBeenCalledTimes(2));
    await act(async () => { resolveSecond(ready); });
    expect(useConnectivityStore.getState().forcedOffline).toBe(true);

    await act(async () => { resolveFirst(incomplete); });
    expect(screen.getByRole("status")).toHaveTextContent("This device is ready for offline local play.");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("runs a fresh retry after an earlier preparation result", async () => {
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));
    await screen.findByText("This device is ready for offline local play.");
    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    await vi.waitFor(() => expect(mocks.prepare).toHaveBeenCalledTimes(2));
  });

  it("renders optional visual issues while preparation remains ready", async () => {
    mocks.prepare.mockResolvedValueOnce({
      ...ready,
      visualPacks: { status: "warning", issueKinds: ["missing_object"] },
    });
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    await screen.findByText("This device is ready for offline local play.");
    expect(screen.getByText(/Installed visual packs need attention/)).toBeInTheDocument();
  });

  it.each([
    ["required capability failure", incomplete, "Some required local-play capabilities are not ready."],
    ["app shell reload", { ...incomplete, status: "reload-or-relaunch-required" as const }, "Reload the app or relaunch the desktop shell, then prepare again."],
    ["browser engine reload", {
      ...ready,
      status: "reload-or-relaunch-required" as const,
      capabilities: { ...ready.capabilities, browserEngine: { status: "reload-required" as const } },
      requiredGaps: ["browserEngine"],
    }, "Reload the app or relaunch the desktop shell, then prepare again."],
  ] as const)("reports %s with the actionable terminal status", async (_label, settled, message) => {
    mocks.prepare.mockResolvedValueOnce(settled);
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    await screen.findByText(message);
  });

  it("retains the mounted fence through StrictMode setup and cleanup", async () => {
    render(
      <StrictMode>
        <OfflinePreparationSection nativeEngineEnabled={false} />
      </StrictMode>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    await screen.findByText("This device is ready for offline local play.");
  });

  /** The gap this panel used to have: with nothing cached it rendered no card
   *  image line at all, so a device with zero art still read as ready. */
  it("warns when no card images are cached", async () => {
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    await screen.findByText(/No card images are installed/);
  });

  it("names the card image packs that are cached", async () => {
    mocks.prepare.mockResolvedValue({
      ...ready,
      visualPacks: { status: "ready" as const, installedPacks: ["curated", "deck_library"] },
    });
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    const cached = await screen.findByText(/Card images cached/);
    expect(cached).toHaveTextContent("One image per card");
    expect(cached).toHaveTextContent("Deck library");
    expect(screen.queryByText(/No card images are installed/)).not.toBeInTheDocument();
  });

  it("lists the core visuals row in the readiness checklist", async () => {
    render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Prepare for Offline" }));

    const checklist = await screen.findByRole("list", { name: "Offline readiness checklist" });
    expect(checklist).toHaveTextContent("Card back and mana symbols");
  });

  it.each([
    ["ready", ready],
    ["incomplete", incomplete],
    ["reload", { ...incomplete, status: "reload-or-relaunch-required" as const }],
  ] as const)("ignores a late %s preparation result after unmount", async (_label, settled) => {
    let resolve!: (value: typeof settled) => void;
    mocks.prepare.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const section = render(<OfflinePreparationSection nativeEngineEnabled={false} />);

    fireEvent.click(screen.getByRole("checkbox", { name: "Work Offline" }));
    section.rerender(<p>Settings closed</p>);
    await act(async () => { resolve(settled); });

    expect(screen.getByText("Settings closed")).toBeInTheDocument();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(useConnectivityStore.getState().forcedOffline).toBe(false);
  });
});
