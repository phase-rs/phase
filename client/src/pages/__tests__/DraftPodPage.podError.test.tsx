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
    playDrawPrompt: null,
    error: null as string | null,
    guestRecoveryFailure: null as { kind: "retryable" | "incompatible" | "invalid"; message: string } | null,
    clearError: vi.fn(),
    resumeDraft: vi.fn<(options?: { signal?: AbortSignal }) => Promise<string>>(async () => "resumed"),
    currentRound: 2,
    nextPairingRound: 3,
    standings: [],
    pairings: [],
    seatIndex: 0,
    view: { tournament_format: "Swiss" },
    matchPairing: null,
    workspaceState: {
      schemaVersion: 1,
      placements: {},
      virtualBasics: [],
    },
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

// Only the hook is stubbed; every other export of the module stays real. The
// `?kind=` slug the page's entry effect reads is `COMMANDER_DRAFT_ENTRY` in the
// leaf module `components/draft/draftKind`, which is not mocked anywhere — a
// literal here would be a second copy of the same fact.
vi.mock("../../stores/draftPodStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/draftPodStore")>()),
  useDraftPodStore: (selector: (state: { reset: () => void; resumeHostedPod: () => void; enterKind: () => void }) => unknown) => selector({
    reset: vi.fn(),
    resumeHostedPod: vi.fn(),
    enterKind: vi.fn(),
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
  return render(<MemoryRouter initialEntries={["/draft-pod?entry=host"]}><DraftPodPage /></MemoryRouter>);
}

const ERROR_TEXT = "Failed to advance round: pairing generation failed";

describe("DraftPodPage pod error banner", () => {
  afterEach(cleanup);

  beforeEach(() => {
    draftState.error = null;
    draftState.guestRecoveryFailure = null;
    draftState.clearError.mockClear();
    draftState.resumeDraft.mockClear();
    draftState.leave.mockClear();
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

  it("offers retry only for a typed retryable guest recovery failure", async () => {
    const user = userEvent.setup();
    draftState.phase = "error";
    draftState.guestRecoveryFailure = {
      kind: "retryable",
      message: "Host is still coming back online",
    };
    renderPage();

    expect(screen.getByText("Host is still coming back online")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Try Reconnecting" }));
    expect(draftState.resumeDraft).toHaveBeenCalledOnce();
  });

  it("does not offer retry for an incompatible recovery failure", () => {
    draftState.phase = "error";
    draftState.guestRecoveryFailure = {
      kind: "incompatible",
      message: "Refresh both windows",
    };
    renderPage();

    expect(screen.getByText("Refresh both windows")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
  });

  it("revokes recovery when the participant explicitly returns to the menu", async () => {
    const user = userEvent.setup();
    draftState.phase = "error";
    renderPage();

    await user.click(screen.getByRole("button", { name: "Return to Menu" }));

    expect(draftState.leave).toHaveBeenCalledOnce();
    expect(draftState.leave).toHaveBeenCalledWith(false);
  });

  it("aborts the retry attempt when its page unmounts", async () => {
    const user = userEvent.setup();
    let settle!: (outcome: string) => void;
    draftState.phase = "error";
    draftState.guestRecoveryFailure = { kind: "retryable", message: "Host is restarting" };
    draftState.resumeDraft.mockImplementationOnce(({ signal }: { signal?: AbortSignal } = {}) => new Promise<string>((resolve) => {
      settle = resolve;
      expect(signal?.aborted).toBe(false);
    }));
    const { unmount } = renderPage();

    await user.click(screen.getByRole("button", { name: "Try Reconnecting" }));
    const [{ signal } = {}] = draftState.resumeDraft.mock.calls[0]!;
    unmount();

    expect(signal?.aborted).toBe(true);
    settle("superseded");
  });
});
