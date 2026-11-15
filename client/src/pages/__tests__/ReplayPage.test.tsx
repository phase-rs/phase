import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router";

import { ReplayPage } from "../ReplayPage";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { gameStateFactory } from "../../test/factories/gameStateFactory";

const { mockIsMobile, replayState, gameState } = vi.hoisted(() => ({
  mockIsMobile: vi.fn(() => false),
  replayState: {
    adapter: {} as object | null,
    isLoading: false,
    error: null as string | null,
    loadReplay: vi.fn(),
    unload: vi.fn(),
  },
  gameState: { current: null as unknown },
}));

vi.mock("../../audio/useAudioContext", () => ({
  useAudioContext: () => undefined,
}));

vi.mock("../../hooks/useIsMobile", () => ({
  useIsMobile: () => mockIsMobile(),
}));

vi.mock("../../stores/replayStore", () => ({
  useReplayStore: (selector: (state: typeof replayState) => unknown) => selector(replayState),
}));

vi.mock("../../stores/gameStore", () => ({
  useGameStore: (selector: (state: { gameState: unknown }) => unknown) =>
    selector({ gameState: gameState.current }),
}));

vi.mock("../../components/board/GameBoard", () => ({
  GameBoard: ({ effectiveMultiplayerBoardLayout }: { effectiveMultiplayerBoardLayout: string }) => (
    <div data-layout={effectiveMultiplayerBoardLayout} data-testid="replay-board-layout" />
  ),
}));

vi.mock("../../components/replay/ReplayControls", () => ({
  ReplayControls: () => null,
}));

describe("ReplayPage multiplayer layout", () => {
  beforeEach(() => {
    mockIsMobile.mockReturnValue(false);
    gameState.current = gameStateFactory.withPlayers(0, 1, 2).priority(0).build();
    usePreferencesStore.setState({ multiplayerBoardLayout: "auto" });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it.each([
    ["mobile", true, "focused"],
    ["desktop", false, "split"],
  ] as const)("resolves auto for a %s replay viewport", (_viewport, isMobile, layout) => {
    mockIsMobile.mockReturnValue(isMobile);

    render(
      <MemoryRouter>
        <ReplayPage />
      </MemoryRouter>,
    );

    expect(screen.getByTestId("replay-board-layout")).toHaveAttribute("data-layout", layout);
  });
});
