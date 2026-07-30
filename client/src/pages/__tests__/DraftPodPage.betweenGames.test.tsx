import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";

const { draftState } = vi.hoisted(() => ({
  draftState: {
    phase: "betweenGames",
    sideboardPrompt: {
      matchId: "bo3-1",
      gameNumber: 2,
      score: { p0_wins: 1, p1_wins: 0, draws: 0 },
      loserSeat: 1,
      timerMs: 60_000,
    },
    playDrawPrompt: null,
    sideboardSubmitted: false,
    seatIndex: 0,
    timerRemainingMs: 60_000,
    mainDeck: ["Plains", "Island"],
    submittedDeck: ["Plains", "Island"],
    submitSideboard: vi.fn(),
    choosePlayDraw: vi.fn(),
    leave: vi.fn(),
  },
}));

vi.mock("../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: (selector: (state: typeof draftState) => unknown) => selector(draftState),
}));

vi.mock("../../stores/draftPodStore", () => ({
  useDraftPodStore: (selector: (state: { reset: () => void; resumeHostedPod: () => void }) => unknown) => selector({
    reset: vi.fn(),
    resumeHostedPod: vi.fn(),
  }),
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: ({ children }: { children: ReactNode }) => <>{children}</> }));
vi.mock("../../components/draft/HostControls", () => ({ HostControls: () => null }));
vi.mock("../../components/draft/LimitedDeckBuilder", () => ({ LimitedDeckBuilder: () => <div data-testid="limited-deck-builder" /> }));
vi.mock("../../components/draft/ScoreBadge", () => ({ ScoreBadge: () => <div data-testid="score-badge" /> }));

function renderPage() {
  return render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
}

describe("DraftPodPage betweenGames", () => {
  afterEach(cleanup);

  beforeEach(() => {
    draftState.sideboardPrompt = {
      matchId: "bo3-1",
      gameNumber: 2,
      score: { p0_wins: 1, p1_wins: 0, draws: 0 },
      loserSeat: 1,
      timerMs: 60_000,
    };
    draftState.playDrawPrompt = null;
    draftState.sideboardSubmitted = false;
    draftState.submittedDeck = ["Plains", "Island"];
    draftState.submitSideboard.mockClear();
  });

  it("renders the live sideboard prompt and submits its current deck through the store authority", async () => {
    const user = userEvent.setup();
    renderPage();

    expect(screen.getByRole("heading", { name: "Sideboard — Game 2" })).toBeInTheDocument();
    expect(screen.getByTestId("limited-deck-builder")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Submit Sideboard" }));

    expect(draftState.submitSideboard).toHaveBeenCalledWith("bo3-1", ["Plains", "Island"], []);
  });

  it("shows the submitted deck read-only while waiting for the opponent", () => {
    draftState.sideboardSubmitted = true;
    renderPage();

    expect(screen.getByText("Waiting for opponent to submit sideboard...")).toBeInTheDocument();
    expect(screen.getByText("Plains, Island")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Submit Sideboard" })).toBeNull();
  });
});
