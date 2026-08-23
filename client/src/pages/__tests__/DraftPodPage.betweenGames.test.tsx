import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { MatchScore } from "../../adapter/types";
import { DraftPodPage } from "../DraftPodPage";

// The store declares the two prompt objects inline and anonymously
// (`multiplayerDraftStore.ts`), and `MultiplayerDraftState` is not exported, so
// indexed access is unavailable and the harness declares its own. Their `score`
// field is the exported `MatchScore` — imported rather than re-declared, so the
// fixture cannot drift from the type it stands in for. Without the annotations
// TypeScript infers `playDrawPrompt: null` and a non-nullable `sideboardPrompt`,
// and branch selection needs the opposite of both. Type aliases are erased, so
// declaring them above `vi.hoisted` does not disturb the hoist.
type SideboardPrompt = {
  matchId: string;
  gameNumber: number;
  score: MatchScore;
  loserSeat: number | null;
  timerMs: number;
} | null;
type PlayDrawPrompt = {
  matchId: string;
  gameNumber: number;
  score: MatchScore;
  timerMs: number;
} | null;

const { draftState } = vi.hoisted(() => ({
  draftState: {
    // `matchInProgress` is the production phase for the whole Bo3 match, games
    // included — the intergame screen is DERIVED by `draftPodScreen` from this
    // plus a live `sideboardPrompt`, not stored in `phase`.
    phase: "matchInProgress",
    error: null as string | null,
    clearError: vi.fn(),
    // Non-null on purpose: `matchPairing` is written on `matchStart` and cleared
    // only by `disposeMatchAdapter`/`leave`, neither of which fires inside the
    // intergame window — so `matchPairing === null` is a state production cannot
    // be in, and it is what the suppression destination reads.
    matchPairing: {
      type: "HumanHost",
      matchId: "bo3-1",
      opponentName: "Opp",
    },
    startMatch: vi.fn(),
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

// The spread keeps the REAL `draftPodScreen` / `intergamePromptKey`, so these
// rows pin the shipped rule rather than a re-implementation in the mock.
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
// Mocking the COMPONENT — not padding the fixture with `standings`/`currentRound`
// — is what isolates the unit under test (the page's routing) while keeping the
// suppression destination renderable.
vi.mock("../../components/draft/StandingsTable", () => ({ StandingsTable: () => <div data-testid="standings-table" /> }));

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
    draftState.choosePlayDraw.mockClear();
    draftState.leave.mockClear();
    draftState.startMatch.mockClear();
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

  // ── The local exit ───────────────────────────────────────────────────
  //
  // Removing the clobber also removes an accidental escape some users relied on
  // when the host's intergame orchestration deadlocks. These rows pin the
  // deliberate replacement: a component-scoped render suppression keyed to the
  // prompt's identity, plus a persistent banner offering the way back. It nulls
  // no store field, so the sideboard can still be submitted the moment the
  // viewer returns.
  const PLAY_DRAW_PROMPT: PlayDrawPrompt = {
    matchId: "bo3-1",
    gameNumber: 2,
    score: { p0_wins: 1, p1_wins: 0, draws: 0 },
    timerMs: 10_000,
  };

  it.each<[string, () => void, string]>([
    [
      "play/draw",
      () => {
        draftState.playDrawPrompt = PLAY_DRAW_PROMPT;
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
    ["sideboard-editing", () => {}, "Sideboard — Game 2"],
  ])("lets the viewer set the overlay aside from the %s branch", async (_branch, setup, branchText) => {
    setup();
    const user = userEvent.setup();
    renderPage();

    // Reach-guard: the intended branch rendered, so a missing button below would
    // be a real absence rather than a fixture that routed elsewhere.
    expect(screen.getByText(branchText)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Hide this screen" }));

    // REVERT-FAILING: delete this branch's dismiss `<button>`, or no-op the
    // page's `onDismissOverlay`, and the overlay stays on screen.
    expect(screen.queryByText(branchText)).toBeNull();
    // Paired positive: the suppression routed somewhere real, not to a blank tree.
    expect(screen.getByTestId("standings-table")).toBeInTheDocument();
  });

  it("dismisses without calling any store action", async () => {
    const user = userEvent.setup();
    renderPage();

    // Reach-guard for three negatives: the branch really rendered and the control
    // really was clicked, so the three absences below are real absences and not a
    // page that rendered nothing. Deliberately NOT asserting the suppression here
    // — this row's job is to say whether the click was destructive, and a row
    // asserting two independent propositions cannot say which one moved.
    expect(screen.getByText("Sideboard — Game 2")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Hide this screen" }));

    // REVERT-FAILING: wire `onDismiss` to a handler that also calls `onLeave`.
    // `leave(true)` is the destructive path — on the host it closes every guest
    // session while sending no `draft_host_left`.
    expect(draftState.leave).not.toHaveBeenCalled();
    expect(draftState.submitSideboard).not.toHaveBeenCalled();
    expect(draftState.choosePlayDraw).not.toHaveBeenCalled();
  });

  it("offers the way back while a live overlay is suppressed", async () => {
    const user = userEvent.setup();
    renderPage();

    expect(screen.getByText("Sideboard — Game 2")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Hide this screen" }));

    // REVERT-FAILING: delete the banner block, or only its `<button>`.
    expect(screen.getByText("Sideboarding is still open for this game.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Show this screen" }));

    expect(screen.getByText("Sideboard — Game 2")).toBeInTheDocument();
    expect(screen.queryByText("Sideboarding is still open for this game.")).toBeNull();
  });

  it("expires the dismissal when the next game's prompt arrives", async () => {
    const user = userEvent.setup();
    const { rerender } = renderPage();

    expect(screen.getByText("Sideboard — Game 2")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Hide this screen" }));

    // Reach-guard: the overlay really is hidden before the prompt swap.
    expect(screen.getByTestId("standings-table")).toBeInTheDocument();
    expect(screen.queryByText("Sideboard — Game 2")).toBeNull();

    draftState.sideboardPrompt = {
      matchId: "bo3-1",
      gameNumber: 3,
      score: { p0_wins: 1, p1_wins: 1, draws: 0 },
      loserSeat: 0,
      timerMs: 60_000,
    };
    rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);

    // REVERT-FAILING: weaken `overlayDismissed` to `dismissedPromptKey !== null`
    // — the dismissal becomes a latch that outlives the window it was hiding.
    expect(screen.getByText("Sideboard — Game 3")).toBeInTheDocument();
  });

  it("does not carry the dismissal over to the same game's play/draw decision", async () => {
    const user = userEvent.setup();
    const { rerender } = renderPage();

    expect(screen.getByText("Sideboard — Game 2")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Hide this screen" }));

    expect(screen.getByTestId("standings-table")).toBeInTheDocument();
    expect(screen.queryByText("Sideboard — Game 2")).toBeNull();

    // Exactly what `bo3ChoosePlayDraw` does: set `playDrawPrompt` and leave
    // `sideboardPrompt` alone. The pod host sends both prompts of one window with
    // the SAME `matchId` and `gameNumber`, so only the prompt-type component of
    // the key can tell the two decisions apart.
    draftState.playDrawPrompt = PLAY_DRAW_PROMPT;
    rerender(<MemoryRouter><DraftPodPage /></MemoryRouter>);

    // REVERT-FAILING: drop the `#${… ? "pd" : "sb"}` component from
    // `intergamePromptKey` and a sideboard dismissal silently suppresses a live
    // 10-second play/draw decision that auto-chooses on expiry.
    expect(screen.getByText("Game 2")).toBeInTheDocument();
  });

  it("names the decision the banner is hiding", async () => {
    draftState.playDrawPrompt = PLAY_DRAW_PROMPT;
    const user = userEvent.setup();
    renderPage();

    expect(screen.getByText("Game 2")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Hide this screen" }));
    // Reach-guard: the overlay really is suppressed.
    expect(screen.getByTestId("standings-table")).toBeInTheDocument();

    // REVERT-FAILING: make the banner's copy unconditional. The paired negative
    // pins the SELECTION, not merely the presence of a string.
    expect(screen.getByText("A play or draw choice is still open for this game.")).toBeInTheDocument();
    expect(screen.queryByText("Sideboarding is still open for this game.")).toBeNull();
  });
});
