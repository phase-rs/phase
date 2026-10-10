import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

// Same WebStorage shadowing workaround as HostSetup.test.tsx.
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

vi.mock("../../../adapter/wasm-adapter", () => ({ getHostAdapter: () => ({}) }));

const engine = vi.hoisted(() => ({
  bestOfThreeCeilingForFormat: vi.fn(async () => "Bo1"),
  deckSupplyForFormat: vi.fn(async (format: string) =>
    format === "Dandan" ? "HostPile" : format === "Momir" ? "EngineFixed" : "PlayerBuilt"),
}));
vi.mock("../../../services/engineRuntime", async () => ({
  ...(await vi.importActual<typeof import("../../../services/engineRuntime")>("../../../services/engineRuntime")),
  ...engine,
}));

vi.mock("../../../services/aiDeckCatalog", async () => ({
  ...(await vi.importActual<typeof import("../../../services/aiDeckCatalog")>("../../../services/aiDeckCatalog")),
  useAiDeckCatalog: () => ({ candidates: [], loading: false, error: null }),
}));

const compat = vi.hoisted(() => ({ evaluateDeckCompatibility: vi.fn() }));
vi.mock("../../../services/deckCompatibility", () => compat);

import { HostSetup } from "../HostSetup";
import * as serverDirectory from "../../../services/serverDirectory";
import type { GameFormat } from "../../../adapter/types";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import { LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION } from "../../../adapter/ws-adapter";
import { DEFAULT_MULTIPLAYER_SERVER_URL } from "../../../config/multiplayerServer";
import { STORAGE_KEY_PREFIX } from "../../../constants/storage";
import { emptySeatDeck } from "../../../services/pileSource";
import { refuseRealWebSockets } from "../../../test/helpers/refusingWebSocket";

function rememberWithAiSeat(format: "Dandan" | "Standard") {
  useMultiplayerStore.setState({
    lastHostConfig: {
      format,
      formatConfig: FORMAT_DEFAULTS[format],
      savedCustomFormatId: null,
      playerCount: 2,
      matchType: "Bo1",
      loopDetection: { type: "Off" },
      isPublic: true,
      startWhenFull: true,
      ranked: false,
      aiSeats: [{ seatIndex: 1, difficulty: "Medium", deckName: null }],
    },
  });
}

function renderHostSetup(onHost = vi.fn().mockResolvedValue(false)) {
  render(<HostSetup onHost={onHost} onBack={vi.fn()} connectionMode="server" onConnectionModeChange={vi.fn()} />);
  return onHost;
}

async function settled(format: GameFormat) {
  await waitFor(() => expect(engine.deckSupplyForFormat).toHaveBeenCalledWith(format));
  await act(async () => {});
}

describe("HostSetup — host-supplied pile", () => {
  beforeEach(() => {
    refuseRealWebSockets();
    vi.spyOn(serverDirectory, "refreshServerDirectory").mockResolvedValue(undefined);
    vi.spyOn(useMultiplayerStore.getState(), "ensureSubscriptionSocket").mockResolvedValue(null);
    localStorageItems.clear();
    compat.evaluateDeckCompatibility.mockReset();
    engine.deckSupplyForFormat.mockClear();
    useMultiplayerStore.setState({
      displayName: "",
      formatConfig: null,
      lastHostConfig: null,
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
    localStorage.setItem(`${STORAGE_KEY_PREFIX}Pile A`, JSON.stringify({ main: [{ name: "Island", count: 80 }], sideboard: [] }));
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("hosts Dandan AI seats with an empty deck and the default pile though the AI catalog is empty", async () => {
    rememberWithAiSeat("Dandan");
    const onHost = renderHostSetup();
    await settled("Dandan");
    const host = screen.getByRole("button", { name: "Host Game" });
    expect(host).toBeEnabled();
    await userEvent.setup().click(host);
    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        aiSeats: [expect.objectContaining({ seatIndex: 1, deck: { type: "DeckList", data: emptySeatDeck() } })],
        pile: { type: "Default" },
      }),
      expect.any(String),
    );
  });

  it("a named pile the engine refuses blocks hosting", async () => {
    rememberWithAiSeat("Dandan");
    compat.evaluateDeckCompatibility.mockResolvedValue({ selected_format_compatible: false, selected_format_reasons: ["Too many Islands"] });
    renderHostSetup();
    await settled("Dandan");
    await userEvent.setup().selectOptions(screen.getByRole("combobox", { name: "Pile" }), "Pile A");
    expect(await screen.findByText("Too many Islands")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Host Game" })).toBeDisabled();
  });

  it("a named pile the engine accepts rides the settings", async () => {
    rememberWithAiSeat("Dandan");
    compat.evaluateDeckCompatibility.mockResolvedValue({ selected_format_compatible: true, selected_format_reasons: [] });
    const onHost = renderHostSetup();
    await settled("Dandan");
    const user = userEvent.setup();
    await user.selectOptions(screen.getByRole("combobox", { name: "Pile" }), "Pile A");
    await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "Host Game" }));
    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({ pile: { type: "SavedDeck", name: "Pile A" } }),
      expect.any(String),
    );
  });

  it("changing format drops a named pile", async () => {
    rememberWithAiSeat("Dandan");
    compat.evaluateDeckCompatibility.mockResolvedValue({ selected_format_compatible: true, selected_format_reasons: [] });
    const onHost = renderHostSetup();
    await settled("Dandan");
    const user = userEvent.setup();
    await user.selectOptions(screen.getByRole("combobox", { name: "Pile" }), "Pile A");
    expect(screen.getByRole("combobox", { name: "Pile" })).toHaveValue("Pile A");
    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Standard" }));
    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Dandân" }));
    expect(await screen.findByRole("combobox", { name: "Pile" })).toHaveValue("");
    expect(onHost).not.toHaveBeenCalled();
  });

  it("a player-built format with AI seats and an empty catalog stays blocked", async () => {
    rememberWithAiSeat("Standard");
    renderHostSetup();
    await settled("Standard");
    expect(screen.queryByRole("combobox", { name: "Pile" })).toBeNull();
    expect(screen.getByRole("button", { name: "Host Game" })).toBeDisabled();
  });
});
