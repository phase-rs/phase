import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SERVER_PRESETS } from "../../../services/serverDetection";
import {
  MAX_USER_LOBBY_SOURCES,
  useMultiplayerStore,
  type LobbySource,
} from "../../../stores/multiplayerStore";
import {
  DIRECTORY_VERSION,
  projectDirectoryBody,
  type DirectoryRow,
  type DirectorySource,
} from "../../../services/serverDirectory";
import {
  LOBBY_PROTOCOL_VERSION,
  MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL,
  PROTOCOL_VERSION,
} from "../../../adapter/ws-adapter";
import { lobbySources } from "../../../stores/multiplayerStore";
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
      directorySources: [],
      directoryFetchedAtMs: null,
      disabledDirectorySources: [],
    });
  });

  /** Project fixtures through the PRODUCTION projection, so a rendered row
   * carries the same canonical URL and the same stored `rejection` a real
   * listing would. */
  function directoryEntries(
    ...rows: (Partial<DirectoryRow> & { url: string })[]
  ): DirectorySource[] {
    return projectDirectoryBody({
      directory_version: DIRECTORY_VERSION,
      servers: rows.map((overrides) => ({
        name: overrides.url.replace(/^wss:\/\//, "").replace(/\/.*$/, ""),
        mode: "LobbyOnly",
        server_version: "0.71.0",
        protocol_version: PROTOCOL_VERSION,
        lobby_protocol_version: LOBBY_PROTOCOL_VERSION,
        current_players: 0,
        first_seen_ms: 1_700_000_000_000,
        last_seen_ms: 1_700_000_060_000,
        score: null,
        ...overrides,
      })),
    })!;
  }

  const dialedUrls = () =>
    lobbySources(useMultiplayerStore.getState()).map((source) => source.url);

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

  // V-U11h
  it("switches a directory source out of the dialed set without deleting it", async () => {
    const URL_D = "wss://listed.example/ws";
    useMultiplayerStore.setState({ directorySources: directoryEntries({ url: URL_D }) });
    render(<ServerPicker onClose={vi.fn()} />);

    // Reach-guard: the entry starts out dialed and rendered.
    expect(dialedUrls()).toContain(URL_D);
    const row = screen.getByText("listed.example").closest("li") as HTMLElement;
    const user = userEvent.setup();
    await user.click(within(row).getByRole("button", { name: "Disable" }));

    expect(dialedUrls()).not.toContain(URL_D);
    // Switched off, not deleted: the listing survives and the row is still
    // rendered, now offering to switch it back on.
    expect(
      useMultiplayerStore.getState().directorySources.map((e) => e.source.url),
    ).toEqual([URL_D]);
    expect(useMultiplayerStore.getState().disabledDirectorySources).toEqual([URL_D]);
    const enableButton = await within(
      screen.getByText("listed.example").closest("li") as HTMLElement,
    ).findByRole("button", { name: "Enable" });

    // Paired: switching it back on restores it to the dialed set.
    await user.click(enableButton);
    expect(dialedUrls()).toContain(URL_D);
    expect(useMultiplayerStore.getState().disabledDirectorySources).toEqual([]);
  });

  // V-U18d
  it("greys an incompatible directory row, shows its version, and offers no toggle", () => {
    useMultiplayerStore.setState({
      directorySources: directoryEntries(
        { url: "wss://ok.example/ws" },
        {
          url: "wss://old.example/ws",
          server_version: "0.42.0",
          lobby_protocol_version: MIN_SUPPORTED_SERVER_LOBBY_PROTOCOL - 1,
        },
      ),
    });
    render(<ServerPicker onClose={vi.fn()} />);

    const badRow = screen.getByText("old.example").closest("li") as HTMLElement;
    expect(
      within(badRow).getByText(/Incompatible — server version 0\.42\.0/),
    ).toBeInTheDocument();
    expect(
      within(badRow).queryByRole("button", { name: /Disable|Enable/ }),
    ).toBeNull();

    // Paired: the compatible sibling DOES carry a toggle, so "no button" is
    // not "no rows rendered".
    const goodRow = screen.getByText("ok.example").closest("li") as HTMLElement;
    expect(
      within(goodRow).getByRole("button", { name: "Disable" }),
    ).toBeInTheDocument();
  });
});
