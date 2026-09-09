import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router";

import { HostControlTile } from "../HostControlTile";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import type { DeckChoice, PlayerSlot } from "../../../stores/multiplayerStore";

const catalogMocks = vi.hoisted(() => ({
  candidates: [] as unknown[],
}));

vi.mock("../../../services/aiDeckCatalog", () => ({
  useAiDeckCatalog: () => ({
    candidates: catalogMocks.candidates,
    loading: false,
    error: null,
  }),
}));

const twoHeadedGiantSlots: PlayerSlot[] = [
  {
    playerId: 0,
    name: "Host",
    kind: { type: "HostHuman" },
    teamInfo: { teamIndex: 0, positionInTeam: 0 },
  },
  {
    playerId: 1,
    name: "Partner",
    kind: { type: "JoinedHuman" },
    teamInfo: { teamIndex: 0, positionInTeam: 1 },
  },
  {
    playerId: 2,
    name: "",
    kind: { type: "WaitingHuman" },
    teamInfo: { teamIndex: 1, positionInTeam: 0 },
  },
  {
    playerId: 3,
    name: "AI",
    kind: { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } },
    teamInfo: { teamIndex: 1, positionInTeam: 1 },
  },
];

function renderHostControlTile(playerSlots: PlayerSlot[]) {
  useMultiplayerStore.setState({
    hostGameCode: "ABCD1",
    hostingStatus: "waiting",
    hostSession: {
      formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
      timerSeconds: null,
      matchType: "Bo1",
    },
    playerSlots,
    serverInfo: null,
  });

  render(
    <MemoryRouter initialEntries={["/multiplayer"]}>
      <HostControlTile />
    </MemoryRouter>,
  );
}

describe("HostControlTile", () => {
  afterEach(() => {
    cleanup();
    catalogMocks.candidates = [];
    useMultiplayerStore.setState({
      hostGameCode: null,
      hostingStatus: "idle",
      hostSession: null,
      playerSlots: [],
      serverInfo: null,
    });
    vi.clearAllMocks();
  });

  describe("AI seat deck promotion", () => {
    const CATALOG_DECK = {
      id: "cat-1",
      name: "Catalog Deck",
      source: "bundled",
      deck: { main: [{ count: 1, name: "Forest" }], sideboard: [] },
      coveragePct: null,
      archetype: null,
      bracket: null,
    };

    function renderWithAiSeat(deck: DeckChoice) {
      const seatMutate = vi.fn();
      catalogMocks.candidates = [CATALOG_DECK];
      useMultiplayerStore.setState({
        hostGameCode: "ABCD1",
        hostingStatus: "waiting",
        hostSession: {
          formatConfig: FORMAT_DEFAULTS.Standard,
          timerSeconds: null,
          matchType: "Bo1",
        },
        playerSlots: [
          { playerId: 0, name: "Host", kind: { type: "HostHuman" } },
          {
            playerId: 1,
            name: "AI",
            kind: { type: "Ai", data: { difficulty: "Medium", deck } },
          },
        ] as PlayerSlot[],
        serverInfo: null,
        seatMutate,
      });
      render(
        <MemoryRouter initialEntries={["/multiplayer"]}>
          <HostControlTile />
        </MemoryRouter>,
      );
      return seatMutate;
    }

    it("stops re-sending once the server echoes the installed deck", () => {
      // Guards the re-resolution loop.
      const onRandom = renderWithAiSeat({ type: "Random" });
      expect(onRandom).toHaveBeenCalledTimes(1);

      cleanup();

      const onDeckList = renderWithAiSeat({
        type: "DeckList",
        data: { main_deck: ["Forest"], sideboard: [], commander: [] },
      });
      expect(onDeckList).not.toHaveBeenCalled();
    });
  });

  it("renders team badges only for slots with team metadata", () => {
    renderHostControlTile(twoHeadedGiantSlots);

    expect(screen.getAllByText("Team 1")).toHaveLength(2);
    expect(screen.getAllByText("Team 2")).toHaveLength(2);

    cleanup();
    renderHostControlTile(twoHeadedGiantSlots.map(({ teamInfo: _teamInfo, ...slot }) => slot));

    expect(screen.queryByText("Team 1")).not.toBeInTheDocument();
    expect(screen.queryByText("Team 2")).not.toBeInTheDocument();
  });

  describe("join-link copy", () => {
    const realClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");

    function renderWithJoinLink(showToast: (message: string) => void) {
      useMultiplayerStore.setState({
        hostGameCode: "ABCD1",
        hostingStatus: "waiting",
        hostSession: {
          formatConfig: FORMAT_DEFAULTS.Standard,
          timerSeconds: null,
          matchType: "Bo1",
        },
        playerSlots: [],
        serverInfo: {
          version: "0.60.0",
          buildCommit: "abc1234",
          protocolVersion: 33,
          mode: "Full",
          publicUrl: "https://play.example.com",
        },
        showToast,
      });
      render(
        <MemoryRouter initialEntries={["/multiplayer"]}>
          <HostControlTile />
        </MemoryRouter>,
      );
      return screen.getByTitle(/ABCD1@play\.example\.com/);
    }

    afterEach(() => {
      if (realClipboard) Object.defineProperty(navigator, "clipboard", realClipboard);
      Reflect.deleteProperty(document, "execCommand");
    });

    it("confirms only when the clipboard actually took the link", async () => {
      Object.defineProperty(navigator, "clipboard", {
        value: { writeText: () => Promise.resolve() },
        configurable: true,
      });
      const showToast = vi.fn();

      fireEvent.click(renderWithJoinLink(showToast));
      await vi.waitFor(() => expect(showToast).toHaveBeenCalledWith("Join link copied"));

      cleanup();
      // Same click, a webview that cannot write: no confirmation may appear.
      Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
      Object.defineProperty(document, "execCommand", { value: () => false, configurable: true });
      const silentToast = vi.fn();

      fireEvent.click(renderWithJoinLink(silentToast));
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(silentToast).not.toHaveBeenCalled();
    });
  });
});
