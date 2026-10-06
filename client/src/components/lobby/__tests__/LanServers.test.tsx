import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LanServers } from "../LanServers";
import { MAX_USER_LOBBY_SOURCES, useMultiplayerStore } from "../../../stores/multiplayerStore";
import { discoverLanServers, getLanServerStatus, initializeLanCapabilities, startLanServer, stopLanServer } from "../../../services/lan";

vi.mock("../../../services/lan", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../../services/lan")>(),
  initializeLanCapabilities: vi.fn(), getLanServerStatus: vi.fn(),
  startLanServer: vi.fn(), stopLanServer: vi.fn(), discoverLanServers: vi.fn(),
  canUseLanBridge: () => false, isLanEndpoint: () => false,
}));
vi.mock("../../../services/nativeEngine", () => ({ nativeEngineKeyForCurrentOrigin: () => ({ release: { version: "test" } }) }));

const url = "ws://192.168.1.2:9374/ws";
const stopped = { running: false, addresses: [], key: null };
const running = { running: true, addresses: [url], key: { release: { version: "test" } } };
const subscribe = vi.fn().mockResolvedValue(null);
const originalSubscribe = useMultiplayerStore.getState().ensureSubscriptionSocket;

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(initializeLanCapabilities).mockResolvedValue(true);
  vi.mocked(getLanServerStatus).mockResolvedValue(stopped);
  vi.mocked(startLanServer).mockResolvedValue(running);
  vi.mocked(stopLanServer).mockResolvedValue();
  vi.mocked(discoverLanServers).mockResolvedValue([]);
  useMultiplayerStore.setState({ userLobbySources: [], sourceStatus: new Map(), ensureSubscriptionSocket: subscribe, connectionMode: "p2p" });
});
afterEach(() => { cleanup(); useMultiplayerStore.setState({ ensureSubscriptionSocket: originalSubscribe }); vi.useRealTimers(); });

describe("LAN controls", () => {
  it("shows honest manual guidance without invoking lifecycle commands when unsupported", async () => {
    vi.mocked(initializeLanCapabilities).mockResolvedValue(false);
    render(<LanServers />);
    expect(await screen.findByText(/Enter a server address/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Start LAN hosting" })).not.toBeInTheDocument();
    expect(getLanServerStatus).not.toHaveBeenCalled();
    expect(startLanServer).not.toHaveBeenCalled();
  });

  it("starts only on request, copies a ready address and stops without submitting its enclosing form", async () => {
    const submit = vi.fn();
    const copy = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText: copy } });
    render(<form onSubmit={submit}><LanServers /></form>);
    const start = await screen.findByRole("button", { name: "Start LAN hosting" });
    await waitFor(() => expect(start).toBeEnabled());
    expect(startLanServer).not.toHaveBeenCalled();
    fireEvent.click(start);
    expect(await screen.findByText(url)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Copy address" }));
    expect(await screen.findByText("Address copied")).toBeInTheDocument();
    expect(copy).toHaveBeenCalledWith(url);
    fireEvent.click(screen.getByRole("button", { name: "Stop LAN hosting" }));
    await waitFor(() => expect(screen.queryByText(url)).not.toBeInTheDocument());
    expect(stopLanServer).toHaveBeenCalledOnce();
    expect(submit).not.toHaveBeenCalled();
  });

  it("scans explicitly and retries errors and empty results without changing mode", async () => {
    vi.mocked(discoverLanServers).mockRejectedValueOnce({ detail: "multicast unavailable" });
    render(<LanServers />);
    fireEvent.click(await screen.findByRole("button", { name: "Find LAN servers" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("multicast unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Find LAN servers" }));
    expect(await screen.findByText(/No LAN servers found/)).toBeInTheDocument();
    vi.mocked(discoverLanServers).mockResolvedValue([{ name: "Neighbor", url, channel: null }]);
    fireEvent.click(screen.getByRole("button", { name: "Find LAN servers" }));
    expect(await screen.findByText("Neighbor")).toBeInTheDocument();
    expect(useMultiplayerStore.getState().userLobbySources).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Add server" }));
    await waitFor(() => expect(subscribe).toHaveBeenCalledWith(url));
    expect(useMultiplayerStore.getState().connectionMode).toBe("p2p");
  });

  it("recovers status on remount and removes only its owned source on stop", async () => {
    vi.mocked(getLanServerStatus).mockResolvedValue(running);
    const view = render(<LanServers />);
    fireEvent.click(await screen.findByRole("button", { name: "Add server" }));
    await waitFor(() => expect(subscribe).toHaveBeenCalledWith(url));
    const remote = "ws://192.168.1.3:9374/ws";
    useMultiplayerStore.getState().addUserLobbySource(remote);
    view.unmount();
    render(<LanServers />);
    await screen.findByRole("button", { name: "Stop LAN hosting" });
    vi.mocked(getLanServerStatus).mockResolvedValue(stopped);
    fireEvent.click(screen.getByRole("button", { name: "Stop LAN hosting" }));
    await waitFor(() => expect(useMultiplayerStore.getState().userLobbySources.map((source) => source.url)).toEqual([remote]));
  });

  it.each([1, 2])("ignores stopped polls overtaken by Start and Add across %s mounted panels", async (panelCount) => {
    vi.useFakeTimers();
    render(<><LanServers />{panelCount === 2 && <LanServers />}</>);
    await act(async () => { await Promise.resolve(); });
    const panel = within(screen.getAllByRole("region", { name: "Local network" })[0]);
    expect(panel.getByRole("button", { name: "Start LAN hosting" })).toBeEnabled();

    let resolvePoll!: (status: typeof stopped) => void;
    const oldStatus = new Promise<typeof stopped>((resolve) => { resolvePoll = resolve; });
    vi.mocked(getLanServerStatus).mockReturnValue(oldStatus);
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(getLanServerStatus).toHaveBeenCalledTimes(panelCount * 2);

    await act(async () => { fireEvent.click(panel.getByRole("button", { name: "Start LAN hosting" })); });
    await act(async () => { fireEvent.click(panel.getByRole("button", { name: "Add server" })); });
    expect(useMultiplayerStore.getState().userLobbySources.map((source) => source.url)).toContain(url);

    await act(async () => { resolvePoll(stopped); await oldStatus; });
    expect(panel.getByRole("button", { name: "Stop LAN hosting" })).toBeInTheDocument();
    expect(useMultiplayerStore.getState().userLobbySources.map((source) => source.url)).toContain(url);
  });

  it("surfaces duplicates and the source cap while preserving preexisting local sources", async () => {
    vi.mocked(getLanServerStatus).mockResolvedValue(running);
    useMultiplayerStore.getState().addUserLobbySource(url);
    render(<LanServers />);
    fireEvent.click(await screen.findByRole("button", { name: "Add server" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/already/i);
    vi.mocked(getLanServerStatus).mockResolvedValue(stopped);
    fireEvent.click(screen.getByRole("button", { name: "Stop LAN hosting" }));
    await screen.findByRole("button", { name: "Start LAN hosting" });
    expect(useMultiplayerStore.getState().userLobbySources.map((source) => source.url)).toContain(url);
    for (let i = 1; i < MAX_USER_LOBBY_SOURCES; i++) useMultiplayerStore.getState().addUserLobbySource(`wss://server${i}.test/ws`);
    vi.mocked(discoverLanServers).mockResolvedValue([{ name: "Extra", url: "ws://10.0.0.3:9374/ws", channel: null }]);
    fireEvent.click(screen.getByRole("button", { name: "Find LAN servers" }));
    await screen.findByText("Extra");
    fireEvent.click(screen.getByRole("button", { name: "Add server" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(String(MAX_USER_LOBBY_SOURCES));
  });
});
