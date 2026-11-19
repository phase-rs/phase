/**
 * HostSetup — the host picks which seat is the archenemy.
 *
 * Archenemy-family formats previously always seated the archenemy at seat 0
 * (the host). The setup form now offers every seat; the pick rides
 * `FormatConfig.archenemy_player` to the hosted game. Seat numbers are
 * 1-based to match the seat panel and the lobby's `Player N` names. A pick
 * stranded by shrinking the table clamps to the last (guest) seat rather
 * than silently returning the role to the host.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

// A real `localStorage` for the store's `persist` middleware, installed
// before the modules under test are imported (mirrors HostSetup.test.tsx:
// on some Node versions a built-in WebStorage global shadows the DOM
// environment's `localStorage` with a method-less object).
const localStorageItems = vi.hoisted(() => {
  const items = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => {
        items.set(key, value);
      },
      removeItem: (key: string) => {
        items.delete(key);
      },
      clear: () => {
        items.clear();
      },
      key: (index: number) => [...items.keys()][index] ?? null,
      get length() {
        return items.size;
      },
    },
  });
  return items;
});

// The saved-format select flow resolves through the WASM adapter. No real
// engine runs in this environment; stub the one method HostSetup calls.
vi.mock("../../../adapter/wasm-adapter", () => ({
  getHostAdapter: () => ({
    formatConfigForCustomRules: vi.fn(),
  }),
}));

const { ceilingMock } = vi.hoisted(() => ({ ceilingMock: vi.fn() }));
vi.mock("../../../services/engineRuntime", async () => ({
  ...(await vi.importActual<typeof import("../../../services/engineRuntime")>(
    "../../../services/engineRuntime",
  )),
  bestOfThreeCeilingForFormat: ceilingMock,
}));

import { HostSetup } from "../HostSetup";
import * as serverDirectory from "../../../services/serverDirectory";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import type { GameFormat } from "../../../adapter/types";
import {
  LOBBY_PROTOCOL_VERSION,
  PROTOCOL_VERSION,
} from "../../../adapter/ws-adapter";
import { DEFAULT_MULTIPLAYER_SERVER_URL } from "../../../config/multiplayerServer";
import { refuseRealWebSockets } from "../../../test/helpers/refusingWebSocket";

const testPreparationEpoch = { current: 0, invalidate() { this.current += 1; } };

describe("HostSetup — archenemy seat", () => {
  beforeEach(() => {
    ceilingMock.mockReset();
    ceilingMock.mockImplementation(async () => "Bo3");
    refuseRealWebSockets();
    vi.spyOn(serverDirectory, "refreshServerDirectory").mockResolvedValue(undefined);
    vi.spyOn(useMultiplayerStore.getState(), "ensureSubscriptionSocket").mockResolvedValue(null);
    localStorageItems.clear();
    useMultiplayerStore.setState({
      displayName: "",
      formatConfig: null,
      lastHostConfig: null,
      // Without these four a picker fixture leaks into every following case.
      userLobbySources: [],
      sourceStatus: new Map([[DEFAULT_MULTIPLAYER_SERVER_URL, {
        state: "open", playerCount: 0,
        serverInfo: {
          version: "test", buildCommit: "test", mode: "Full",
          protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
        },
      }]]),
      directorySources: [],
      disabledDirectorySources: [],
    });
  });

  afterEach(cleanup);

  /** Seed a remembered config so the form mounts on the given Archenemy format. */
  function seedArchenemyConfig(format: GameFormat, playerCount: number): void {
    useMultiplayerStore.setState({
      lastHostConfig: {
        format,
        formatConfig: FORMAT_DEFAULTS[format],
        savedCustomFormatId: null,
        playerCount,
        matchType: "Bo1",
        loopDetection: { type: "Off" },
        isPublic: true,
        startWhenFull: true,
        ranked: false,
        aiSeats: [],
      },
    });
  }

  function renderHostSetup(onHost = vi.fn()) {
    return render(
      <HostSetup
        preparationEpoch={testPreparationEpoch}
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
        onConnectionModeChange={vi.fn()}
      />,
    );
  }

  it("offers every seat for Archenemy, defaulting to the host", async () => {
    seedArchenemyConfig("Archenemy", 4);
    renderHostSetup();

    const you = await screen.findByRole("button", { name: "You (Host)" });
    expect(screen.getByRole("button", { name: "Seat 2" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Seat 3" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Seat 4" })).toBeInTheDocument();
    expect(you).toHaveClass("bg-white/10");
  });

  it("offers no archenemy selector outside the Archenemy family", async () => {
    renderHostSetup();

    expect(await screen.findByLabelText("Starting Life")).toBeInTheDocument();
    // The seat panel's "You (Host)" is a span; no BUTTON with that name
    // means the selector is absent.
    expect(screen.queryByRole("button", { name: "You (Host)" })).not.toBeInTheDocument();
  });

  it("hosts with the picked guest seat as archenemy", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();
    seedArchenemyConfig("Archenemy", 4);
    renderHostSetup(onHost);

    await user.click(await screen.findByRole("button", { name: "Seat 3" }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({ format: "Archenemy", archenemy_player: 2 }),
      }),
      expect.any(String),
    );
    expect(
      useMultiplayerStore.getState().lastHostConfig?.formatConfig.archenemy_player,
    ).toBe(2);
  });

  it("hosts the holder as archenemy when no seat is picked", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();
    seedArchenemyConfig("ArchenemyCommander", 3);
    renderHostSetup(onHost);

    expect(await screen.findByRole("button", { name: "You (Host)" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({ format: "ArchenemyCommander", archenemy_player: 0 }),
      }),
      expect.any(String),
    );
  });

  it("clamps a stranded pick to the last guest seat when the table shrinks", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();
    seedArchenemyConfig("Archenemy", 4);
    renderHostSetup(onHost);

    await user.click(await screen.findByRole("button", { name: "Seat 4" }));
    // Shrink below the pick: the role follows to the last remaining guest
    // seat rather than silently returning to the host.
    await user.click(screen.getByRole("button", { name: "2" }));
    expect(screen.getByRole("button", { name: "Seat 2" })).toHaveClass("bg-white/10");

    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({ archenemy_player: 1 }),
      }),
      expect.any(String),
    );
  });
});
