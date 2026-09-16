// @vitest-environment happy-dom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SharedStackView } from "../../adapter/draft-adapter";
import { ShellProvider } from "../../components/chrome/ShellContext";
import { DraftPodPage } from "../DraftPodPage";

vi.mock("../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
  // The pile table's face-down stacks resolve the shared public card back
  // through the same hook.
  useCardBackImage: () => ({ src: null, advanceFailedSource: undefined }),
}));

/** Captures what the workspace is handed, so the page's own handler can be
 *  invoked directly -- outside `act`, which is what makes the timing visible. */
const workspaceProps = vi.hoisted(() => ({
  onPreferencesChange: null as null | ((next: unknown) => void),
}));
const arrivingPreferences = vi.hoisted(() => vi.fn());

const store = vi.hoisted(() => {
  const sharedStack: SharedStackView = {
    main_stack_remaining: 11,
    total_cards: 18,
    active_seat: 0,
    active_pile: 1,
    piles: [
      { index: 0, total: 2, revealed: [], legality: [{ decision: "Take", refusal: "PileNotActive" }, { decision: "Decline", refusal: "PileNotActive" }] },
      {
        index: 1,
        total: 3,
        revealed: [{
          instance_id: "pile-card-1",
          name: "Lightning Bolt",
          set_code: "tst",
          collector_number: "1",
          rarity: "common",
          colors: ["R"],
          cmc: 1,
          type_line: "Instant",
        }],
        legality: [{ decision: "Take", refusal: null }, { decision: "Decline", refusal: "NoGuaranteedCard" }],
      },
      { index: 2, total: 0, revealed: [], legality: [{ decision: "Take", refusal: "PileNotActive" }, { decision: "Decline", refusal: "PileNotActive" }] },
    ],
    decisions: 6,
    // Empty: this fixture exercises a display/transport path, and no client
    // consumer reads the history yet. Its fidelity to the reducer is pinned
    // in `draft-core` (`history_records_sizes_and_decisions_and_never_cards`).
    history: [],
    forced_draw: null,
  };

  const packCards = [
    { instance_id: "pack-1", name: "Opt", set_code: "TST", collector_number: "2", rarity: "common", colors: ["U"], cmc: 1, type_line: "Instant" },
  ];
  const view = {
    status: "Drafting",
    kind: "Winston",
    launch_capability: "None",
    distribution: { SharedStackPiles: { pile_count: 3 } },
    commanders_required: 0,
    pool: [],
    // Null for EVERY seat under a shared stack: the engine hands out no packs,
    // which is why `PackDisplay` cannot be the Winston surface.
    current_pack: null as typeof packCards | null,
    draft_effects: [],
    pool_groups: {
      color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
      type_filter_options: [], color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
    },
    seats: [
      { seat_index: 0, display_name: "Alice", is_bot: false, connected: true, has_submitted_deck: false, pick_status: "Pending", active_pack_count: 0, drafted_card_count: 0, face_up_draft_cards: [] },
      { seat_index: 1, display_name: "Bob", is_bot: false, connected: true, has_submitted_deck: false, pick_status: "Waiting", active_pack_count: 0, drafted_card_count: 0, face_up_draft_cards: [] },
    ],
    current_pack_number: 0, pick_number: 0, pass_direction: "Left",
    cards_per_pack: 15, pack_count: 3, min_deck_size: 40, addable_cards: [],
    timer_remaining_ms: null, standings: [], current_round: 0,
    tournament_format: "Swiss", pod_policy: "Casual", pairings: [], match_config: { match_type: "Bo1" },
    shared_stack: sharedStack as SharedStackView | null,
    play_first_chooser: 1 as number | null,
  };
  const state = {
    role: "host",
    phase: "drafting",
    // THIS viewer's seat, and it matches `sharedStack.active_seat` (0), so the
    // fixture is the active seat's own screen. Required explicitly rather than
    // implied by `role: "host"`: `active_pile` is published to EVERY viewer, so
    // the page identifies whose turn it is by comparing `active_seat` against
    // the seat the transport assigned — not by the cursor being non-null. Set
    // this to 1 and every control below correctly disappears.
    seatIndex: 0,
    view,
    packCards,
    workspaceState: { schemaVersion: 1, placements: {}, virtualBasics: [] },
    selectedCard: null,
    pendingPickIntent: null,
    interactionGeneration: 3,
    pickInteractionLocked: false,
    paused: false,
    pauseReason: null,
    sideboardPrompt: null,
    playDrawPrompt: null,
    sideboardSubmitted: false,
    intergameWorkspaceState: null,
    standings: [],
    pairings: [],
    error: null,
    selectCard: vi.fn(),
    submitPick: vi.fn(async () => ({ status: "acknowledged" as const })),
    submitPickStep: vi.fn(async () => ({ status: "acknowledged" as const })),
    confirmPick: vi.fn(async () => ({ status: "acknowledged" as const })),
    submitPickWithDraftEffect: vi.fn(async () => ({ status: "acknowledged" as const })),
    submitSharedStackDecision: vi.fn(async () => ({ status: "acknowledged" as const })),
    autoPickCard: vi.fn(async () => ({ status: "acknowledged" as const })),
    setWorkspaceState: vi.fn(),
    addBasicLand: vi.fn(),
    removeBasicLand: vi.fn(),
    autoSuggestLands: vi.fn(),
    submitDeck: vi.fn(),
    leave: vi.fn(async () => undefined),
    resumeDraft: vi.fn(async () => "absent" as const),
  };
  return { state, sharedStack };
});

const podStore = vi.hoisted(() => ({
  reset: vi.fn(),
  resumeHostedPod: vi.fn(),
  enterKind: vi.fn(),
  enterKindForEntry: vi.fn(),
}));

vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../stores/multiplayerDraftStore")>();
  const hook = Object.assign(
    (selector: (state: typeof store.state) => unknown) => selector(store.state),
    { getState: () => store.state, subscribe: () => vi.fn() },
  );
  return {
    ...actual,
    useMultiplayerDraftStore: hook,
    setArrivingCardBoardPreferences: arrivingPreferences,
    draftPodScreen: (state: typeof store.state) => state.phase,
    intergamePromptKey: () => null,
  };
});

vi.mock("../../stores/draftPodStore", () => ({
  useDraftPodStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({ config: { podSize: 2 }, ...podStore }),
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/DraftIntro", () => ({
  DraftIntro: ({ onContinue }: { onContinue(): void }) => (
    <button type="button" onClick={onContinue}>Continue</button>
  ),
}));
vi.mock("../../components/draft/HostControls", () => {
  const none: readonly [] = [];
  return { HostControls: () => null, useHostDraftTopActions: () => none };
});
vi.mock("../../components/draft/SeatStatusRing", () => ({ SeatStatusRing: () => null }));
vi.mock("../../components/draft/DraftProgress", () => ({
  DraftProgress: () => <div data-testid="draft-progress" />,
}));
vi.mock("../../components/draft/PickTimer", () => ({
  PickTimer: () => <div data-testid="pick-timer" />,
}));
vi.mock("../../components/draft/PackDisplay", () => ({
  PackDisplay: () => <div data-testid="pack-display" />,
}));
vi.mock("../../components/draft/workspace/DraftWorkspace", () => ({
  DraftWorkspace: (props: { onPreferencesChange: (next: unknown) => void }) => {
    workspaceProps.onPreferencesChange = props.onPreferencesChange;
    return <div data-testid="workspace" />;
  },
}));
vi.mock("../../components/card/HoverCardPreview", () => ({ HoverCardPreview: () => null }));

function renderDrafting() {
  const rendered = render(
    <MemoryRouter initialEntries={["/draft-pod"]}>
      <ShellProvider value={false}>
        <DraftPodPage />
      </ShellProvider>
    </MemoryRouter>,
  );
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  return rendered;
}

describe("DraftPodPage drafting-phase surface dispatch", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    store.state.phase = "drafting";
    store.state.view.shared_stack = store.sharedStack;
    store.state.view.current_pack = null;
    store.state.view.kind = "Winston";
    store.state.pickInteractionLocked = false;
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1440 });
    Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 900 });
    localStorage.clear();
  });

  afterEach(() => {
    cleanup();
    localStorage.clear();
  });

  it("renders the pile table, not the pack display, on a published shared stack", () => {
    renderDrafting();

    // REVERT-FAILING: BASE renders `PackDisplay` unconditionally, so the pile
    // table is absent and the pack surface is present — both halves flip.
    expect(document.querySelector("[data-winston-pile-table]")).not.toBeNull();
    expect(screen.queryByTestId("pack-display")).toBeNull();
    // The pile turn's own state reached the screen.
    expect(screen.getByText("Your turn — pile 2")).toBeInTheDocument();
    expect(screen.getByText("11 cards face down in the main stack")).toBeInTheDocument();
    expect(screen.getByText("Bob chooses who plays first in the games after the draft.")).toBeInTheDocument();
    // The pool workspace is NOT swapped out: a taken pile still fills a pool.
    expect(screen.getByTestId("workspace")).toBeInTheDocument();
    // The pack bar is gated on the same discriminator; the pick CLOCK is not —
    // see the dedicated row below for why.
    expect(screen.queryByTestId("draft-progress")).toBeNull();
  });

  it("renders the pack display when the engine published no shared stack", () => {
    // The SAME store, the same phase — only the engine's discriminator differs,
    // so neither arm can be passing on a kind check or on a fixture accident.
    store.state.view.shared_stack = null;
    store.state.view.current_pack = store.state.packCards;
    store.state.view.kind = "Premier";

    renderDrafting();

    expect(screen.getByTestId("pack-display")).toBeInTheDocument();
    expect(document.querySelector("[data-winston-pile-table]")).toBeNull();
    expect(screen.getByTestId("draft-progress")).toBeInTheDocument();
    expect(screen.getByTestId("pick-timer")).toBeInTheDocument();
  });

  it("shows the pick clock, because the host still auto-decides when it expires", () => {
    // THE HOST RE-ARMS this clock on every applied shared-stack decision, and
    // expiry runs `autoDecideSharedStackTurn` — it takes the pile for the
    // active seat. A player who cannot see the clock loses a turn with no
    // warning. `PickTimer` self-gates on Competitive + a live remaining time,
    // so rendering it unconditionally shows it exactly when the sweep can fire.
    renderDrafting();

    expect(screen.getByTestId("pick-timer")).toBeInTheDocument();
    // The pack bar stays gated: there is no pack number or pick step here, so
    // it would render frozen. The two are NOT the same question.
    expect(screen.queryByTestId("draft-progress")).toBeNull();
  });

  it("submits the engine's pile index through the store's decision action", () => {
    renderDrafting();

    const take = document.querySelector<HTMLButtonElement>(
      "[data-winston-pile='1'] [data-winston-decision='Take']",
    );
    expect(take).not.toBeNull();
    fireEvent.click(take!);

    expect(store.state.submitSharedStackDecision).toHaveBeenCalledWith(1, "Take");
    expect(store.state.submitPick).not.toHaveBeenCalled();
  });

  it("disables the decision the engine refused, on the page's own wiring", () => {
    renderDrafting();

    const decline = document.querySelector<HTMLButtonElement>(
      "[data-winston-pile='1'] [data-winston-decision='Decline']",
    );
    expect(decline).toBeDisabled();
    fireEvent.click(decline!);
    expect(store.state.submitSharedStackDecision).not.toHaveBeenCalled();
  });

  /**
   * THE BOARD COLUMNS ARE PUBLISHED BY THE HANDLER, NOT BY AN EFFECT.
   *
   * Cards reach the pool on paths that resolve no placement of their own -- a
   * shared-stack take collects a whole pile, a timed-out seat's decision is
   * applied by the host and broadcast -- so the store has to know which columns
   * this board currently means. An effect runs after React commits, and a
   * `viewUpdated` landing in that window placed the arrivals against the
   * PREVIOUS columns.
   *
   * The timing is observable precisely because the handler is invoked OUTSIDE
   * `act`: a state update schedules an effect but does not flush one, so a
   * publish that lives in an effect has provably not happened yet at the
   * assertion below, while a publish that lives in the handler has. Restore
   * `useEffect(..., [workspacePreferences.deck])` and this reds with zero calls.
   */
  it("publishes board columns during the preference change, not after a commit", () => {
    renderDrafting();
    expect(workspaceProps.onPreferencesChange).not.toBeNull();
    // The mount publish already happened; this test is about the CHANGE.
    arrivingPreferences.mockClear();

    const next = {
      deck: { sort: "color", columnCount: 7, rows: "one", showHeaders: true },
      pool: { sort: "color", columnCount: 7, rows: "one", showHeaders: true },
      packScale: 1,
      pileScale: 1.35,
    };
    // Deliberately NOT wrapped in `act`: no effect flush between this call and
    // the assertion.
    workspaceProps.onPreferencesChange!(next);

    expect(arrivingPreferences).toHaveBeenCalledTimes(1);
    expect(arrivingPreferences).toHaveBeenCalledWith(next.deck);
  });
});
