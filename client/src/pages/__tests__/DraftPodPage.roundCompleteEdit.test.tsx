import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  LocalDeckBuilderController,
  WorkspaceDeckBuilderController,
} from "../../components/draft/LimitedDeckBuilder";
import { DraftPodPage } from "../DraftPodPage";

/**
 * V8. MTR 7.3: a Casual pod may revise its deck between rounds. The
 * round-complete screen offers an editor only when the engine publishes
 * `deck_submission_open`; it never derives the window from `status` +
 * `pod_policy`.
 */

const captured = vi.hoisted(() => ({
  deckbuilder: null as WorkspaceDeckBuilderController | null,
}));

const { draftState } = vi.hoisted(() => ({
  draftState: {
    phase: "roundComplete",
    sideboardPrompt: null,
    playDrawPrompt: null,
    currentRound: 1,
    nextPairingRound: 2,
    standings: [],
    pairings: [],
    seatIndex: 0,
    error: null,
    pickInteractionLocked: false,
    view: {
      tournament_format: "Swiss",
      pod_policy: "Casual",
      post_draft_play: "TournamentPairings",
      deck_submission_open: true,
    },
    workspaceState: { schemaVersion: 1, placements: {}, virtualBasics: [] },
    matchPairing: null,
    startMatch: vi.fn(),
    leave: vi.fn(),
    autoSuggestLands: vi.fn(),
    submitDeck: vi.fn(async (_commanders?: string[]) => {}),
  },
}));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: (selector: (state: typeof draftState) => unknown) => selector(draftState),
}));

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
vi.mock("../../components/draft/LimitedDeckBuilder", () => ({
  LimitedDeckBuilder: (props: { local?: WorkspaceDeckBuilderController }) => {
    captured.deckbuilder = props.local ?? null;
    return <div data-testid="deckbuilder" />;
  },
}));

function isEditableDeckBuilderController(
  controller: WorkspaceDeckBuilderController | null,
): controller is LocalDeckBuilderController {
  return controller !== null && controller.capabilities?.kind !== "fixed-pool";
}

function renderPage() {
  return render(<MemoryRouter><DraftPodPage /></MemoryRouter>);
}

function openEditor(): LocalDeckBuilderController {
  fireEvent.click(screen.getByRole("button", { name: "Edit Deck" }));
  expect(screen.getByTestId("deckbuilder")).toBeInTheDocument();
  const deckbuilder = captured.deckbuilder;
  if (!isEditableDeckBuilderController(deckbuilder)) {
    throw new Error("editable deck builder controller not installed");
  }
  return deckbuilder;
}

describe("DraftPodPage round-complete deck revision", () => {
  beforeEach(() => {
    captured.deckbuilder = null;
    draftState.view.deck_submission_open = true;
    draftState.submitDeck.mockClear();
  });

  afterEach(cleanup);

  it("revises the deck between rounds through the confirmation and closes the editor", async () => {
    renderPage();
    const deckbuilder = openEditor();
    expect(screen.getByText("Casual pod: you can change your deck before the next round starts.")).toBeInTheDocument();
    // Land suggestion is deckbuilding-only on both seams, so the between-rounds
    // builder does not offer it.
    expect(deckbuilder.capabilities).toEqual({ kind: "editable-pool", suggestions: false });

    let submission: Promise<void> | void = undefined;
    act(() => {
      submission = deckbuilder.onSubmitDeck([]);
    });
    const dialog = await screen.findByRole("alertdialog", { name: "Submit this deck?" });
    expect(dialog).toHaveAccessibleDescription("You can still change your deck between rounds.");
    fireEvent.click(screen.getByRole("button", { name: "Confirm Submission" }));
    await act(async () => {
      await submission;
    });

    expect(draftState.submitDeck).toHaveBeenCalledOnce();
    expect(screen.queryByTestId("deckbuilder")).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Round Complete" })).toBeInTheDocument();
  });

  it("keeps the editor open when the between-rounds submission is cancelled", async () => {
    renderPage();
    const deckbuilder = openEditor();

    let submission: Promise<void> | void = undefined;
    act(() => {
      submission = deckbuilder.onSubmitDeck([]);
    });
    await screen.findByRole("alertdialog", { name: "Submit this deck?" });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await submission;
    await vi.waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());

    expect(draftState.submitDeck).not.toHaveBeenCalled();
    expect(screen.getByTestId("deckbuilder")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Back to Standings" })).toBeInTheDocument();
  });

  it("offers no editor when the engine keeps the deck submission closed", () => {
    draftState.view.deck_submission_open = false;
    renderPage();

    // Reach guard: the round-complete screen rendered.
    expect(screen.getByRole("heading", { name: "Round Complete" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit Deck" })).not.toBeInTheDocument();
  });
});
