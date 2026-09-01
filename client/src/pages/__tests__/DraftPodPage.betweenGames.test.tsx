import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";

const { captured, draftState, intergameWorkspace, playerView, sideboardPrompt } = vi.hoisted(() => {
  const pool = [
    { instance_id: "twin-a", name: "Twin", set_code: "TST", collector_number: "1", rarity: "common", colors: [], cmc: 1, type_line: "Card" },
    { instance_id: "twin-b", name: "Twin", set_code: "TST", collector_number: "2", rarity: "common", colors: [], cmc: 1, type_line: "Card" },
  ];
  const intergameWorkspace = {
    schemaVersion: 1,
    placements: {
      "twin-a": { zone: "deck", row: 0, column: 0, order: 0 },
      "twin-b": { zone: "sideboard", row: 0, column: 0, order: 0 },
      drafted: { zone: "deck", row: 0, column: 0, order: 1 },
      generated: { zone: "sideboard", row: 0, column: 0, order: 1 },
    },
    virtualBasics: [
      { instanceId: "drafted", name: "Island" },
      { instanceId: "generated", name: "Forest" },
    ],
  };
  const playerView = { pool };
  const sideboardPrompt = {
    matchId: "bo3-1",
    gameNumber: 2,
    score: { p0_wins: 1, p1_wins: 0, draws: 0 },
    loserSeat: 1,
    timerMs: 60_000,
  };
  return { captured: { shellMode: "", menuShell: null as null | { compactTopPadding?: boolean }, builderProps: null as null | {
    responsiveLayout?: string;
    responsiveHeightMode?: string;
  } }, intergameWorkspace, playerView, sideboardPrompt, draftState: {
    phase: "betweenGames",
    sideboardPrompt: sideboardPrompt as typeof sideboardPrompt | null,
    playDrawPrompt: null as null | {
      matchId: string;
      gameNumber: number;
      score: typeof sideboardPrompt.score;
    },
    sideboardSubmitted: false,
    seatIndex: 0,
    timerRemainingMs: 60_000,
    submittedDeck: ["Twin", "Island"],
    view: playerView as typeof playerView | null,
    intergameWorkspaceState: intergameWorkspace as typeof intergameWorkspace | null,
    setIntergameWorkspaceState: vi.fn(),
    submitSideboard: vi.fn(),
    choosePlayDraw: vi.fn(),
    leave: vi.fn(),
  } };
});

// The spread keeps the REAL `draftPodScreen` / `intergamePromptKey`, so these
// rows pin the shipped rule rather than a re-implementation in the mock.
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
vi.mock("../../components/chrome/ShellContext", () => ({
  useDraftShellChrome: (mode: string) => { captured.shellMode = mode; },
}));
vi.mock("../../components/menu/MenuShell", () => ({ MenuShell: (props: { children: ReactNode; compactTopPadding?: boolean }) => {
  captured.menuShell = props;
  return <>{props.children}</>;
} }));
vi.mock("../../components/draft/HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});
vi.mock("../../components/draft/LimitedDeckBuilder", () => ({
  LimitedDeckBuilder: (props: {
    local: { capabilities?: { kind: string }; onSubmitDeck: () => void };
    responsiveLayout?: string;
    responsiveHeightMode?: string;
  }) => {
    captured.builderProps = props;
    return (
      <button data-testid="limited-builder" data-capability={props.local.capabilities?.kind} onClick={props.local.onSubmitDeck}>
        Submit Sideboard
      </button>
    );
  },
}));
vi.mock("../../components/draft/ScoreBadge", () => ({ ScoreBadge: () => <div data-testid="score-badge" /> }));
// Mocking the COMPONENT — not padding the fixture with `standings`/`currentRound`
// — is what isolates the unit under test (the page's routing) while keeping the
// suppression destination renderable.
vi.mock("../../components/draft/StandingsTable", () => ({ StandingsTable: () => <div data-testid="standings-table" /> }));

function renderPage() {
  return render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
}

function setViewport(width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
}

describe("DraftPodPage betweenGames", () => {
  afterEach(cleanup);

  beforeEach(() => {
    setViewport(1440, 900);
    draftState.phase = "betweenGames";
    draftState.sideboardPrompt = sideboardPrompt;
    draftState.playDrawPrompt = null;
    draftState.sideboardSubmitted = false;
    draftState.submittedDeck = ["Twin", "Island"];
    draftState.view = playerView;
    draftState.intergameWorkspaceState = intergameWorkspace;
    draftState.submitSideboard.mockClear();
    captured.shellMode = "";
    captured.menuShell = null;
    captured.builderProps = null;
  });

  it("renders the live sideboard prompt and submits its current deck through the store authority", async () => {
    const user = userEvent.setup();
    renderPage();

    expect(screen.getByRole("heading", { name: "Sideboard — Game 2" })).toBeInTheDocument();
    const submit = screen.getByRole("button", { name: "Submit Sideboard" });
    expect(submit).toHaveAttribute("data-capability", "fixed-pool");
    await user.click(submit);

    expect(draftState.submitSideboard).toHaveBeenCalledWith(
      "bo3-1",
      ["Twin", "Island"],
      [{ name: "Twin", count: 1 }, { name: "Forest", count: 1 }],
    );
  });

  it("shows the submitted deck read-only while waiting for the opponent", () => {
    draftState.sideboardSubmitted = true;
    renderPage();

    expect(screen.getByText("Waiting for opponent to submit sideboard...")).toBeInTheDocument();
    expect(screen.getByText("Twin, Island")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Submit Sideboard" })).toBeNull();
  });

  it.each([
    ["tablet portrait", 768, 1024, "tablet-portrait"],
    ["tablet landscape", 1024, 768, "tablet-landscape"],
  ])("uses bounded container-height editor chrome on %s", (_label, width, height, layout) => {
    setViewport(width, height);
    renderPage();

    expect(captured.shellMode).toBe("tablet-deckbuilding");
    expect(captured.builderProps).toMatchObject({
      responsiveLayout: layout,
      responsiveHeightMode: "container",
    });
    expect(captured.menuShell).toMatchObject({ compactTopPadding: true });
    const builder = screen.getByTestId("limited-builder");
    expect(builder.parentElement).toHaveClass("min-h-0", "flex-1", "overflow-hidden");
    expect(builder.parentElement?.parentElement).toHaveClass(
      "h-[calc(100dvh_-_4rem)]",
      "min-h-0",
      "max-w-none",
      "overflow-hidden",
    );
  });

  it.each([
    ["play/draw prompt", () => { draftState.playDrawPrompt = { matchId: "bo3-1", gameNumber: 2, score: sideboardPrompt.score }; }],
    ["submitted sideboard", () => { draftState.sideboardSubmitted = true; }],
    ["missing sideboard prompt", () => { draftState.sideboardPrompt = null; }],
    ["missing player view", () => { draftState.view = null; }],
    ["missing intergame workspace", () => { draftState.intergameWorkspaceState = null; }],
    ["non-tablet editor", () => { setViewport(1440, 900); }],
    ["non-between-games phase", () => { draftState.phase = "error"; }],
  ])("resets tablet editor chrome for %s", (_label, arrange) => {
    setViewport(768, 1024);
    arrange();
    renderPage();

    expect(captured.shellMode).toBe("default");
    expect(captured.menuShell).toMatchObject({ compactTopPadding: false });
  });
});
