import { cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";

const { draftState } = vi.hoisted(() => ({
  draftState: {
    phase: "matchInProgress",
    sideboardPrompt: null,
    playDrawPrompt: null,
    currentRound: 2,
    nextPairingRound: 3,
    // `game_wins`/`game_losses` are not decoration: `formatGwp` sums them, and
    // if they are absent the row renders `NaN%` instead of `-`.
    standings: [
      {
        seat_index: 0,
        display_name: "Alice",
        match_wins: 2,
        match_losses: 0,
        game_wins: 4,
        game_losses: 1,
      },
    ],
    pairings: [],
    seatIndex: 0,
    view: { tournament_format: "Swiss" },
    matchPairing: null,
    startMatch: vi.fn(),
    leave: vi.fn(),
  },
}));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: (selector: (state: typeof draftState) => unknown) => selector(draftState),
}));

// Only the hook is stubbed; every other export of the module stays real. The
// `?kind=` slug the page's entry effect reads is `COMMANDER_DRAFT_ENTRY` in the
// leaf module `components/draft/draftKind`, which is not mocked anywhere — a
// literal here would be a second copy of the same fact.
vi.mock("../../stores/draftPodStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/draftPodStore")>()),
  useDraftPodStore: (selector: (state: { reset: () => void; resumeHostedPod: () => void }) => unknown) => selector({
    reset: vi.fn(),
    resumeHostedPod: vi.fn(),
  }),
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: ({ children }: { children: ReactNode }) => <>{children}</> }));
vi.mock("../../components/draft/HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});
vi.mock("../../components/draft/LimitedDeckBuilder", () => ({ LimitedDeckBuilder: () => <div data-testid="limited-deck-builder" /> }));
vi.mock("../../components/draft/ScoreBadge", () => ({ ScoreBadge: () => <div data-testid="score-badge" /> }));

function renderPage() {
  return render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
}

/**
 * Paired positive reach-guard for every case below: `StandingsTable`
 * early-returns `null` when `standings` is empty, so without proof that the
 * table body rendered, a missing heading is indistinguishable from a component
 * that never rendered at all.
 */
function expectStandingsRowRendered() {
  expect(screen.getByText("Alice")).toBeInTheDocument();
  expect(screen.getByText("2-0")).toBeInTheDocument();
}

describe("DraftPodPage standings round heading", () => {
  afterEach(cleanup);

  it("names the in-progress round during a match", () => {
    draftState.phase = "matchInProgress";
    draftState.currentRound = 2;
    draftState.nextPairingRound = 3;
    renderPage();

    // REVERT-FAILING ASSERTION: pre-fix the component rendered
    // `currentRound + 1` = 3 at this site.
    expect(
      screen.getByRole("heading", { name: "Standings — Round 2" }),
    ).toBeInTheDocument();
    expectStandingsRowRendered();
  });

  it("names the upcoming round in the pairing window", () => {
    // Preservation guard: green at BASE and green after. Its job is to prove the
    // fix preserved the two already-correct sites rather than flipping all four.
    draftState.phase = "pairing";
    draftState.currentRound = 2;
    draftState.nextPairingRound = 3;
    renderPage();

    expect(
      screen.getByRole("heading", { name: "Standings — Round 3" }),
    ).toBeInTheDocument();
    expectStandingsRowRendered();
  });

  it("names the upcoming round when the round is complete", () => {
    // The second preservation guard — the fourth render site.
    draftState.phase = "roundComplete";
    draftState.currentRound = 2;
    draftState.nextPairingRound = 3;
    renderPage();

    expect(
      screen.getByRole("heading", { name: "Standings — Round 3" }),
    ).toBeInTheDocument();
    expectStandingsRowRendered();
  });

  it("names the in-progress round from the stale window pairingsGenerated leaves", () => {
    // The store state `pairingsGenerated` deliberately produces: `viewUpdated`
    // for round 2 left `nextPairingRound: 3`, then `pairingsGenerated` advanced
    // `currentRound` to 3 without touching it. A stale-window rendering guard.
    draftState.phase = "matchInProgress";
    draftState.currentRound = 3;
    draftState.nextPairingRound = 3;
    renderPage();

    expect(
      screen.getByRole("heading", { name: "Standings — Round 3" }),
    ).toBeInTheDocument();
    expectStandingsRowRendered();
  });

  it("names the final round on the completed pod screen", () => {
    draftState.phase = "complete";
    draftState.currentRound = 3;
    draftState.nextPairingRound = 4;
    renderPage();

    expect(
      screen.getByRole("heading", { name: "Standings — Round 3" }),
    ).toBeInTheDocument();
    expectStandingsRowRendered();
  });
});
