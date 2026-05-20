import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const mocks = vi.hoisted(() => ({
  startDraft: vi.fn(async () => {}),
  toggleBotFill: vi.fn(),
  kickPlayer: vi.fn(),
  leave: vi.fn(async () => {}),
  multiplayerState: {
    role: "host",
    seats: [
      {
        seat_index: 0,
        display_name: "Host",
        is_bot: false,
        connected: true,
        has_submitted_deck: false,
        pick_status: "NotDrafting",
      },
    ],
    joined: 1,
    total: 4,
    roomCode: "ABCDE",
    seatIndex: 0,
    error: null,
  },
  podState: {
    botFillEnabled: true,
    config: {
      setCode: "dft",
      setName: "Draft Set",
      kind: "Premier",
      podSize: 4,
    },
  },
}));

type MultiplayerMockState = typeof mocks.multiplayerState & {
  kickPlayer: typeof mocks.kickPlayer;
  leave: typeof mocks.leave;
};

type PodMockState = typeof mocks.podState & {
  toggleBotFill: typeof mocks.toggleBotFill;
  startDraft: typeof mocks.startDraft;
};

vi.mock("../../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: (selector: (state: MultiplayerMockState) => unknown) =>
    selector({
      ...mocks.multiplayerState,
      kickPlayer: mocks.kickPlayer,
      leave: mocks.leave,
    }),
}));

vi.mock("../../../stores/draftPodStore", () => ({
  useDraftPodStore: (selector: (state: PodMockState) => unknown) =>
    selector({
      ...mocks.podState,
      toggleBotFill: mocks.toggleBotFill,
      startDraft: mocks.startDraft,
    }),
}));

import { DraftPodLobby } from "../DraftPodLobby";

describe("DraftPodLobby", () => {
  beforeEach(() => {
    mocks.startDraft.mockClear();
    mocks.toggleBotFill.mockClear();
    mocks.kickPlayer.mockClear();
    mocks.leave.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("shows the host in the first seat and allows starting with bot fill", () => {
    render(<DraftPodLobby onLeave={vi.fn()} />);

    expect(screen.getByText("Host")).toBeInTheDocument();
    expect(screen.getByText("HOST")).toBeInTheDocument();
    expect(screen.getByText("1 / 4 seats filled")).toBeInTheDocument();

    const startButton = screen.getByRole("button", { name: "Start Draft" });
    expect(startButton).toBeEnabled();

    fireEvent.click(startButton);

    expect(mocks.startDraft).toHaveBeenCalledTimes(1);
  });
});
