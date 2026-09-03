import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SERVER_PRESETS } from "../../../services/serverDetection";
import {
  MAX_USER_LOBBY_SOURCES,
  useMultiplayerStore,
  type LobbySource,
} from "../../../stores/multiplayerStore";
import { ServerPicker } from "../ServerPicker";

const PRESET_URL = SERVER_PRESETS[0].url;

function userSource(host: string): LobbySource {
  return { url: `wss://${host}/ws`, name: host, origin: "user" };
}

const originalLocation = window.location;

function setPageProtocol(protocol: "http:" | "https:") {
  Object.defineProperty(window, "location", {
    value: { ...originalLocation, protocol },
    writable: true,
    configurable: true,
  });
}

describe("ServerPicker", () => {
  beforeEach(() => {
    // The "Test" button opens a real socket; stub the constructor so a probe
    // can never escape the test environment.
    vi.stubGlobal(
      "WebSocket",
      class {
        onopen: (() => void) | null = null;
        onerror: (() => void) | null = null;
        close = vi.fn();
      },
    );
    useMultiplayerStore.setState({
      hostingServer: PRESET_URL,
      userLobbySources: [],
      sourceStatus: new Map(),
    });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    Object.defineProperty(window, "location", {
      value: originalLocation,
      writable: true,
      configurable: true,
    });
  });

  async function addUrl(url: string) {
    const user = userEvent.setup();
    await user.type(screen.getByPlaceholderText(/wss:\/\//), url);
    await user.click(screen.getByRole("button", { name: "Use" }));
    return user;
  }

  it("adds and removes a user lobby source", async () => {
    useMultiplayerStore.setState({ userLobbySources: [userSource("keep.example")] });
    render(<ServerPicker onClose={vi.fn()} />);

    await addUrl("wss://play.example.com/ws");

    // Reach-guard: the add really landed before the removal is exercised.
    expect(useMultiplayerStore.getState().userLobbySources.map((s) => s.url)).toEqual([
      "wss://keep.example/ws",
      "wss://play.example.com/ws",
    ]);

    const row = screen.getByText("play.example.com").closest("li");
    await userEvent.setup().click(
      within(row as HTMLElement).getByRole("button", { name: "Remove" }),
    );

    // Only the named source is gone; the other survives.
    expect(useMultiplayerStore.getState().userLobbySources.map((s) => s.url)).toEqual([
      "wss://keep.example/ws",
    ]);
  });

  it("refuses a duplicate of a built-in source", async () => {
    render(<ServerPicker onClose={vi.fn()} />);

    await addUrl(PRESET_URL);

    expect(
      screen.getByText("That server is already a lobby source."),
    ).toBeInTheDocument();
    expect(useMultiplayerStore.getState().userLobbySources).toEqual([]);
  });

  it("refuses a source past the cap", async () => {
    useMultiplayerStore.setState({
      userLobbySources: Array.from({ length: MAX_USER_LOBBY_SOURCES }, (_, i) =>
        userSource(`s${i}.example`),
      ),
    });
    render(<ServerPicker onClose={vi.fn()} />);

    await addUrl("wss://one-too-many.example/ws");

    expect(
      screen.getByText(`Up to ${MAX_USER_LOBBY_SOURCES} sources can be added.`),
    ).toBeInTheDocument();
    expect(useMultiplayerStore.getState().userLobbySources).toHaveLength(
      MAX_USER_LOBBY_SOURCES,
    );
  });

  it("refuses a ws:// source from an https page", async () => {
    setPageProtocol("https:");
    render(<ServerPicker onClose={vi.fn()} />);

    await addUrl("ws://70.249.47.161:9374/ws");

    expect(screen.getByText(/HTTPS/)).toBeInTheDocument();
    expect(useMultiplayerStore.getState().userLobbySources).toEqual([]);

    // Paired positive: loopback is exempt from the mixed-content rule, so the
    // refusal above is the policy firing, not the form being inert.
    cleanup();
    render(<ServerPicker onClose={vi.fn()} />);
    await addUrl("ws://localhost:9374/ws");

    expect(useMultiplayerStore.getState().userLobbySources.map((s) => s.url)).toEqual([
      "ws://localhost:9374/ws",
    ]);
  });

  it("switches the hosting server and back to direct codes", async () => {
    useMultiplayerStore.setState({ userLobbySources: [userSource("play.example.com")] });
    const user = userEvent.setup();
    render(<ServerPicker onClose={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: /None \(P2P only\)/ }));

    expect(useMultiplayerStore.getState().hostingServer).toBeNull();
    // Sources are a separate axis: switching to direct codes keeps them.
    expect(useMultiplayerStore.getState().userLobbySources).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: /Official/ }));

    expect(useMultiplayerStore.getState().hostingServer).toBe(PRESET_URL);
  });

  it("uses a user source as the hosting server", async () => {
    const source = userSource("play.example.com");
    useMultiplayerStore.setState({ userLobbySources: [source] });
    render(<ServerPicker onClose={vi.fn()} />);

    const row = screen.getByText("play.example.com").closest("li");
    await userEvent.setup().click(
      within(row as HTMLElement).getByRole("button", { name: "Use for hosting" }),
    );

    expect(useMultiplayerStore.getState().hostingServer).toBe(source.url);
    expect(useMultiplayerStore.getState().userLobbySources).toEqual([source]);
  });

  it("reports each source's live status", () => {
    const source = userSource("play.example.com");
    useMultiplayerStore.setState({
      userLobbySources: [source],
      sourceStatus: new Map([
        [source.url, { state: "offline" as const, serverInfo: null, playerCount: null }],
      ]),
    });
    render(<ServerPicker onClose={vi.fn()} />);

    const row = screen.getByText("play.example.com").closest("li");
    expect(within(row as HTMLElement).getByText("Unreachable")).toBeInTheDocument();
  });
});
