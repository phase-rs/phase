import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";

const { draftState } = vi.hoisted(() => ({
  draftState: {
    phase: "pairing",
    sideboardPrompt: null,
    error: null as string | null,
    clearError: vi.fn(),
    currentRound: 2,
    nextPairingRound: 3,
    standings: [],
    pairings: [],
    seatIndex: 0,
    view: { tournament_format: "Swiss" },
    matchPairing: null,
    startMatch: vi.fn(),
    leave: vi.fn(),
    mainDeck: [],
    landCounts: {},
    addToDeck: vi.fn(),
    removeFromDeck: vi.fn(),
    setLandCount: vi.fn(),
    submitDeck: vi.fn(),
  },
}));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
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

const ERROR_TEXT = "Failed to advance round: pairing generation failed";

describe("DraftPodPage pod error banner", () => {
  afterEach(cleanup);

  beforeEach(() => {
    draftState.error = null;
    draftState.clearError.mockClear();
  });

  it("surfaces the store error in the pairing phase", () => {
    draftState.phase = "pairing";
    draftState.error = ERROR_TEXT;
    renderPage();

    // Reach-guard: the phase view itself mounted, so an absent banner is a
    // real absence rather than a failed render.
    expect(screen.getByText("Tournament Pairings")).toBeInTheDocument();
    // REVERT-FAILING ASSERTION: at BASE nothing in this view reads `s.error`.
    expect(screen.getByText(/Failed to advance round/)).toBeInTheDocument();
  });

  it("surfaces the store error while a match is in progress", () => {
    draftState.phase = "matchInProgress";
    draftState.error = ERROR_TEXT;
    renderPage();

    expect(screen.getByText("Waiting for match results...")).toBeInTheDocument();
    expect(screen.getByText(/Failed to advance round/)).toBeInTheDocument();
  });

  it("surfaces the store error on the round-complete screen", () => {
    draftState.phase = "roundComplete";
    draftState.error = ERROR_TEXT;
    renderPage();

    expect(screen.getByText("Round Complete")).toBeInTheDocument();
    expect(screen.getByText(/Failed to advance round/)).toBeInTheDocument();
  });

  it("renders no banner when there is no error", () => {
    draftState.phase = "pairing";
    draftState.error = null;
    renderPage();

    expect(screen.getByText("Tournament Pairings")).toBeInTheDocument();
    expect(screen.queryByText(/Failed to advance round/)).toBeNull();
    expect(screen.queryByTestId("pod-error-banner")).toBeNull();
  });

  it("does not double-surface the error during deckbuilding", () => {
    // Deckbuilding already surfaces `store.error` through `LimitedDeckBuilder`'s
    // `submissionError`. Asserted on the banner's own testid, NOT on a text
    // count: the harness mocks the deck builder out, so the count would be zero.
    draftState.phase = "deckbuilding";
    draftState.error = "boom";
    renderPage();

    expect(screen.getByTestId("limited-deck-builder")).toBeInTheDocument();
    expect(screen.queryByTestId("pod-error-banner")).toBeNull();
  });

  it("clears the error through the store when dismissed", async () => {
    const user = userEvent.setup();
    draftState.phase = "pairing";
    draftState.error = ERROR_TEXT;
    renderPage();

    expect(screen.getByTestId("pod-error-banner")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Close" }));

    expect(draftState.clearError).toHaveBeenCalled();
  });
});
