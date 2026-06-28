import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router";

import { HostControlTile } from "../HostControlTile";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import type { PlayerSlot } from "../../../stores/multiplayerStore";

vi.mock("../../../services/aiDeckCatalog", () => ({
  useAiDeckCatalog: () => ({ candidates: [], loading: false, error: null }),
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
    useMultiplayerStore.setState({
      hostGameCode: null,
      hostingStatus: "idle",
      hostSession: null,
      playerSlots: [],
      serverInfo: null,
    });
    vi.clearAllMocks();
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
});
