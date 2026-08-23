import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";

// The store declares these shapes inline and anonymously
// (`multiplayerDraftStore.ts`), so there is no named type to import and the
// harness declares its own. Without the annotations TypeScript infers
// `playDrawPrompt: null` and a non-nullable `sideboardPrompt`, and branch
// selection needs the opposite of both. Type aliases are erased, so declaring
// them above `vi.hoisted` does not disturb the hoist.
type Score = { p0_wins: number; p1_wins: number; draws: number };
type SideboardPrompt = {
  matchId: string;
  gameNumber: number;
  score: Score;
  loserSeat: number | null;
  timerMs: number;
} | null;
type PlayDrawPrompt = {
  matchId: string;
  gameNumber: number;
  score: Score;
  timerMs: number;
} | null;

const { draftState } = vi.hoisted(() => ({
  draftState: {
    phase: "betweenGames",
    error: null as string | null,
    clearError: vi.fn(),
    sideboardPrompt: {
      matchId: "bo3-1",
      gameNumber: 2,
      score: { p0_wins: 1, p1_wins: 0, draws: 0 },
      loserSeat: 1,
      timerMs: 60_000,
    } as SideboardPrompt,
    playDrawPrompt: null as PlayDrawPrompt,
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

const ERROR_TEXT = "Sideboard timer expired without a registered deck";

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
    draftState.error = null;
    draftState.submitSideboard.mockClear();
    draftState.clearError.mockClear();
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

  // Both `betweenGames` error emitters (`autoSubmitSideboards`,
  // `submitDefaultIntergameCommand` in `p2p-draft-host.ts`) ABORT the intergame
  // progression, so the screen the user is left on is whichever branch the
  // prompt state happened to be in. Every branch therefore has to surface it.
  it.each<[string, () => void, string]>([
    [
      "play/draw",
      () => {
        draftState.playDrawPrompt = {
          matchId: "bo3-1",
          gameNumber: 2,
          score: { p0_wins: 1, p1_wins: 0, draws: 0 },
          timerMs: 60_000,
        };
      },
      "Game 2",
    ],
    [
      "sideboard-submitted",
      () => {
        draftState.sideboardSubmitted = true;
      },
      "Sideboarding",
    ],
    [
      "sideboard-editing",
      () => {},
      "Sideboard — Game 2",
    ],
    [
      "fallback",
      () => {
        draftState.sideboardPrompt = null;
      },
      "Preparing next game...",
    ],
  ])("surfaces a live pod error on the %s branch", (_branch, setup, branchText) => {
    setup();
    draftState.error = ERROR_TEXT;
    renderPage();

    // Reach-guard: the intended branch rendered, so a missing banner below is a
    // real absence and not a fixture that routed somewhere else.
    expect(screen.getByText(branchText)).toBeInTheDocument();
    // REVERT-FAILING: drop this branch's `<PodErrorBanner />` and this row goes
    // red — `getByTestId` throws on absence.
    expect(screen.getByTestId("pod-error-banner")).toBeInTheDocument();
    expect(screen.getByText(ERROR_TEXT)).toBeInTheDocument();
  });

  it("renders no banner when there is no live error", () => {
    renderPage();

    // Same reach-guard, so the absence below is a real absence.
    expect(screen.getByText("Sideboard — Game 2")).toBeInTheDocument();
    expect(screen.queryByTestId("pod-error-banner")).toBeNull();
  });
});
