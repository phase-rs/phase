import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";

const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  loadActiveDraftPod: vi.fn(() => null),
  // The host-recovery probe `draftPodStore.resumeHostedPod` actually calls.
  // `absent` terminates it cleanly so this suite measures the kind-intent
  // effect rather than a recovery it never seeded.
  inspectActiveDraftPod: vi.fn(() => ({ type: "absent" })),
  clearActiveDraftPodIfCurrent: vi.fn(),
  persistedDraftHostSessionState: vi.fn(() => "live"),
  loadDraftHostSession: vi.fn(),
  clearActiveDraftPod: vi.fn(),
  multiplayerState: {
    phase: "idle",
    leave: vi.fn(async () => {}),
    role: null as "host" | "guest" | null,
    roomCode: null as string | null,
    hostDraft: vi.fn(async () => {}),
    // The page's guest-recovery arm calls this when the host probe reports no
    // host locator. `absent` terminates it, leaving the kind-intent effect as
    // this suite's only subject.
    resumeDraft: vi.fn(async () => "absent" as const),
    view: null as {
      kind: string;
      seats: { seat_index: number }[];
      pack_count: number;
      cards_per_pack: number;
      pack_sizes: number[];
      min_deck_size: number;
    } | null,
  },
}));

// `DraftPodPage` selects off this store, and the REAL `draftPodStore` under test
// calls `.getState()` on it, so the mock has to serve both shapes.
vi.mock("../../stores/multiplayerDraftStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/multiplayerDraftStore")>()),
  useMultiplayerDraftStore: Object.assign(
    (selector: (state: typeof mocks.multiplayerState) => unknown) =>
      selector(mocks.multiplayerState),
    { getState: () => mocks.multiplayerState, subscribe: () => () => {} },
  ),
}));

// The engine's per-kind procedure. `pod_size` is deliberately NOT 4 so a client
// literal cannot coincide with the adopted value.
vi.mock("../../adapter/draft-adapter", () => ({
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
  },
}));

vi.mock("../../services/draftPersistence", () => ({
  loadActiveDraftPod: mocks.loadActiveDraftPod,
  inspectActiveDraftPod: mocks.inspectActiveDraftPod,
  clearActiveDraftPodIfCurrent: mocks.clearActiveDraftPodIfCurrent,
  persistedDraftHostSessionState: mocks.persistedDraftHostSessionState,
  loadDraftHostSession: mocks.loadDraftHostSession,
  clearActiveDraftPod: mocks.clearActiveDraftPod,
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));

import { DraftPodPage } from "../DraftPodPage";
import { useDraftPodStore } from "../../stores/draftPodStore";

/** An engine-published `DraftPlayerView` slice containing the procedure the intro reads.
 *  `seats` is the pod's real size — `draftPodStore.config.podSize` is this client's
 *  own intent and is deliberately left on its default in every fixture below. */
function engineView(
  kind: string,
  seatCount: number,
  procedure: Partial<{
    pack_count: number;
    cards_per_pack: number;
    pack_sizes: number[];
    min_deck_size: number;
    launch_capability: "None" | "CommanderMultiplayer";
    commanders_required: number;
  }> = {},
) {
  return {
    kind,
    launch_capability: "None",
    commanders_required: 0,
    seats: Array.from({ length: seatCount }, (_, seat_index) => ({ seat_index })),
    pack_count: 4,
    cards_per_pack: 12,
    pack_sizes: [12, 12, 12, 12],
    min_deck_size: 45,
    ...procedure,
  };
}

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

describe("DraftPodPage ?kind= mode entry", () => {
  afterEach(cleanup);

  beforeEach(() => {
    vi.clearAllMocks();
    mocks.multiplayerState.phase = "idle";
    mocks.multiplayerState.view = null;
    mocks.loadActiveDraftPod.mockReturnValue(null);
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture({
      pod_size: 6,
      human_seats: 1,
      min_pod_size: 3,
      max_pod_size: 8,
      allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: 2,
      distribution: "PickAndPass",
      min_deck_size: 60,
      cube_min_deck_size: 53,
      post_draft_play: "CompleteImmediately",
      match_config: { match_type: "Bo1" },
    }));
    useDraftPodStore.getState().reset();
  });

  it("applies the deep-linked kind and the engine's pod size", async () => {
    renderAt("/draft-pod?kind=commander");

    // REVERT-FAILING: BASE has no `?kind=` effect, so `kind` stays "Premier".
    // The `podSize: 6` half additionally fails any hardcoded client default.
    await waitFor(() =>
      expect(useDraftPodStore.getState().config).toMatchObject({
        kind: "CommanderDraft",
        podSize: 6,
      }),
    );
    expect(mocks.draftProcedure).toHaveBeenCalledWith("CommanderDraft", "Swiss");
  });

  it("offers Commander Draft in the pod kind selector", async () => {
    const user = userEvent.setup();
    renderAt("/draft-pod?kind=commander");

    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    // Reach guard: the pre-existing radios rendered, so an absent Commander
    // radio would be a real absence.
    expect(screen.getByRole("radio", { name: "Premier" })).toBeInTheDocument();
    // REVERT-FAILING: BASE's radio group has three members and its description
    // map is `Partial` with a `?? ""` fallback, so both of these are absent.
    await waitFor(() =>
      expect(screen.getByRole("radio", { name: "Commander" })).toBeChecked(),
    );
    expect(
      screen.getByText(
        "Each player drafts two cards at a time and builds a 60-card Commander deck, then the pod plays one multiplayer game.",
      ),
    ).toBeInTheDocument();
  });

  it("leaves a bare /draft-pod on the Premier default", async () => {
    const user = userEvent.setup();
    renderAt("/draft-pod");

    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    // First production branch reached: `searchParams.get("kind") !== COMMANDER_DRAFT_ENTRY`.
    expect(useDraftPodStore.getState().config.kind).toBe("Premier");
    expect(screen.getByRole("radio", { name: "Commander" })).not.toBeChecked();
    // The witness for "the kind-intent effect did not fire" is `podSize`, not
    // whether `draftProcedure` was called: setup re-reads the engine's per-kind
    // axes on every kind change, so the CALL is no longer specific to
    // `enterKind`. Adopting the kind's `pod_size` still is, and the fixture
    // publishes 6 against the store's 8 default precisely so a stray
    // `enterKind` cannot hide behind a coincidence.
    await waitFor(() => expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier", "Swiss"));
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
  });

  it.each([
    {
      description: "normal Swiss pod",
      kind: "Premier",
      tournamentFormat: "Swiss",
      expectedSizes: [2, 3, 4, 5, 6, 7, 8],
    },
    {
      description: "Commander Swiss pod",
      kind: "CommanderDraft",
      tournamentFormat: "Swiss",
      expectedSizes: [3, 4, 5, 6, 7, 8],
    },
    {
      description: "normal single-elimination pod",
      kind: "Premier",
      tournamentFormat: "SingleElimination",
      expectedSizes: [8],
    },
    {
      description: "Commander single-elimination pod",
      kind: "CommanderDraft",
      tournamentFormat: "SingleElimination",
      expectedSizes: [3, 4, 5, 6, 7, 8],
    },
  ])("offers exactly the legal seat counts for a $description", async ({
    kind,
    tournamentFormat,
    expectedSizes,
  }) => {
    const user = userEvent.setup();
    mocks.draftProcedure.mockImplementation(async (
      requestedKind: string,
      requestedTournamentFormat: string,
    ) => draftProcedureFixture({
      pod_size: 8,
      human_seats: 1,
      min_pod_size: requestedKind === "CommanderDraft" ? 3 : 2,
      max_pod_size: 8,
      allowed_pod_sizes: requestedTournamentFormat === "SingleElimination"
        && requestedKind !== "CommanderDraft"
        ? [8]
        : requestedKind === "CommanderDraft"
          ? [3, 4, 5, 6, 7, 8]
          : [2, 3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: requestedKind === "CommanderDraft" ? 2 : 1,
      distribution: "PickAndPass",
      min_deck_size: 60,
      cube_min_deck_size: 53,
      post_draft_play: requestedKind === "CommanderDraft"
        ? "CompleteImmediately"
        : "TournamentPairings",
      match_config: { match_type: "Bo1" },
    }));

    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));

    if (kind === "CommanderDraft") {
      await user.click(screen.getByRole("radio", { name: "Commander" }));
    }
    if (tournamentFormat === "SingleElimination") {
      await user.click(screen.getByRole("radio", { name: "Single Elimination" }));
    }

    await waitFor(() => expect(useDraftPodStore.getState().allowedPodSizes).toEqual(expectedSizes));
    const podSizeSelect = screen.getByRole("button", { name: "Pod Size" });
    expect(podSizeSelect).toBeEnabled();
    await user.click(podSizeSelect);

    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual(
      expectedSizes.map((size) => `${size} players`),
    );
  });

  it("waits for Commander procedure data instead of offering the previous kind's floor", async () => {
    let resolveCommanderProcedure!: () => void;
    mocks.draftProcedure.mockImplementation((kind: string) => {
      if (kind !== "CommanderDraft") {
        return Promise.resolve(draftProcedureFixture({
          pod_size: 8,
          human_seats: 1,
          min_pod_size: 2,
          max_pod_size: 8,
          allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
          packs_per_player: 3,
          cards_per_pick: 1,
          distribution: "PickAndPass",
          min_deck_size: 60,
          cube_min_deck_size: 53,
          post_draft_play: "TournamentPairings",
          match_config: { match_type: "Bo1" },
        }));
      }

      return new Promise((resolve) => {
        resolveCommanderProcedure = () => resolve(draftProcedureFixture({
          pod_size: 4,
          human_seats: 1,
          min_pod_size: 3,
          max_pod_size: 8,
          allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
          packs_per_player: 3,
          cards_per_pick: 2,
          distribution: "PickAndPass",
          min_deck_size: 60,
          cube_min_deck_size: 73,
          post_draft_play: "CompleteImmediately",
          match_config: { match_type: "Bo1" },
        }));
      });
    });

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(useDraftPodStore.getState().allowedPodSizes).toEqual([2, 3, 4, 5, 6, 7, 8]));

    await user.click(screen.getByRole("radio", { name: "Commander" }));

    const podSizeSelect = screen.getByRole("button", { name: "Pod Size" });
    expect(useDraftPodStore.getState().allowedPodSizes).toBeNull();
    expect(podSizeSelect).toBeDisabled();
    expect(screen.queryByRole("option", { name: "2 players" })).toBeNull();
    await waitFor(() => expect(resolveCommanderProcedure).toBeTypeOf("function"));

    resolveCommanderProcedure();

    await waitFor(() => expect(useDraftPodStore.getState().allowedPodSizes).toEqual([3, 4, 5, 6, 7, 8]));
    expect(podSizeSelect).toBeEnabled();
    await user.click(podSizeSelect);
    expect(screen.getAllByRole("option").map((option) => option.textContent)).toEqual([
      "3 players",
      "4 players",
      "5 players",
      "6 players",
      "7 players",
      "8 players",
    ]);
  });

  it("keeps the real cube panel mounted across pending floor changes", async () => {
    let resolveHigher!: () => void;
    let resolveLower!: () => void;
    mocks.draftProcedure.mockImplementation((kind: string) => {
      const base = draftProcedureFixture({
        pod_size: 8,
        human_seats: 1,
        min_pod_size: 2,
        max_pod_size: 8,
        allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
        packs_per_player: 3,
        cards_per_pick: 1,
        distribution: "PickAndPass",
        min_deck_size: 40,
        cube_min_deck_size: 53,
        post_draft_play: "TournamentPairings",
        match_config: { match_type: "Bo1" },
      });
      if (kind === "CommanderDraft") {
        return new Promise((resolve) => {
          resolveHigher = () => resolve({
            ...base,
            cube_min_deck_size: 73,
            min_pod_size: 3,
            cards_per_pick: 2,
            post_draft_play: "CompleteImmediately",
          });
        });
      }
      if (kind === "Traditional") {
        return new Promise((resolve) => {
          resolveLower = () => resolve({ ...base, cube_min_deck_size: 61 });
        });
      }
      return Promise.resolve(base);
    });

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(useDraftPodStore.getState().cubeMinDeckSize).toBe(53));
    await user.click(screen.getByRole("button", { name: "Cube" }));

    const minimum = screen.getByRole("spinbutton", { name: "Min Deck" });
    const cubeList = screen.getByPlaceholderText(/1 Lightning Bolt/);
    fireEvent.change(minimum, { target: { value: "67" } });
    await user.type(cubeList, "1 Opt");

    await user.click(screen.getByRole("radio", { name: "Commander" }));
    expect(screen.getByRole("spinbutton", { name: "Min Deck" })).toBe(minimum);
    expect(screen.getByRole("button", { name: "Start Cube Draft" })).toBeDisabled();
    expect(cubeList).toHaveValue("1 Opt");
    resolveHigher();
    await waitFor(() => expect(minimum).toHaveValue(73));

    await user.click(screen.getByRole("radio", { name: "Traditional" }));
    expect(screen.getByRole("spinbutton", { name: "Min Deck" })).toBe(minimum);
    expect(screen.getByRole("button", { name: "Start Cube Draft" })).toBeDisabled();
    expect(cubeList).toHaveValue("1 Opt");
    resolveLower();
    await waitFor(() => expect(minimum).toHaveValue(67));

    await user.click(screen.getByRole("button", { name: "Start Cube Draft" }));
    expect(useDraftPodStore.getState().cubeForm).toMatchObject({
      cubeListText: "1 Opt",
      settings: { min_deck_size: 67 },
    });
  });

  it("leaves cube setup only after an all-at-once procedure resolves", async () => {
    mocks.draftProcedure.mockImplementation(async (kind: string) => draftProcedureFixture({
      pod_size: 8,
      human_seats: 1,
      min_pod_size: 2,
      max_pod_size: 8,
      allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
      packs_per_player: kind === "Sealed" ? 6 : 3,
      cards_per_pick: 1,
      distribution: kind === "Sealed" ? "AllAtOnce" : "PickAndPass",
      min_deck_size: 40,
      cube_min_deck_size: 1,
      post_draft_play: "TournamentPairings",
      match_config: { match_type: "Bo1" },
    }));

    const user = userEvent.setup();
    renderAt("/draft-pod");
    await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
    await waitFor(() => expect(useDraftPodStore.getState().cubeMinDeckSize).toBe(1));
    await user.click(screen.getByRole("button", { name: "Cube" }));
    expect(screen.getByPlaceholderText(/1 Lightning Bolt/)).toBeInTheDocument();

    await user.click(screen.getByRole("radio", { name: "Sealed" }));

    await waitFor(() => expect(useDraftPodStore.getState().packDistribution).toBe("AllAtOnce"));
    expect(useDraftPodStore.getState().poolMode).toBe("set");
    expect(screen.queryByPlaceholderText(/1 Lightning Bolt/)).toBeNull();
  });

  it("lets the persisted session win over a URL kind intent", async () => {
    // HOSTILE multi-authority: `?resume=1` names a persisted pod whose kind is
    // the higher authority; the URL intent must not overwrite it. First
    // production branch reached: `searchParams.get("resume") === "1"`.
    renderAt("/draft-pod?resume=1&kind=commander");

    // `inspectActiveDraftPod` is the probe `resumeHostedPod` runs; witnessing
    // it is what proves the resume path — not the kind-intent effect — took
    // this route.
    await waitFor(() => expect(mocks.inspectActiveDraftPod).toHaveBeenCalled());
    expect(useDraftPodStore.getState().config.kind).toBe("Premier");
    // As above: `enterKind` is witnessed by the pod size it would have adopted
    // (6), not by `draftProcedure` going uncalled — the setup screen re-reads
    // the engine's axes for whatever kind is selected.
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
  });

  it("picks the intro variant from the ENGINE-published launch capability", () => {
    // A guest's local `draftPodStore.config` is never populated from the host's
    // kind, so the intro must read the engine capability. This fixture is exactly that
    // case: the store is left on its "Premier" default.
    mocks.multiplayerState.phase = "drafting";
    mocks.multiplayerState.view = engineView("Premier", 4, {
      min_deck_size: 63,
      launch_capability: "CommanderMultiplayer",
    });
    renderAt("/draft-pod");

    expect(useDraftPodStore.getState().config.kind).toBe("Premier");
    // REVERT-FAILING: BASE renders `<DraftIntro mode="pod" .../>` unconditionally.
    expect(
      screen.getByText(
        "Open 4 packs; each pack contains 12 cards — pick two cards, pass the rest",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "After drafting, build a Commander deck of at least 63 cards and play one multiplayer game",
      ),
    ).toBeInTheDocument();
  });

  it("counts the pod from the ENGINE-published seats, not the local config", () => {
    // GUEST FIXTURE, multi-authority: the engine publishes a 4-seat Commander pod
    // while `draftPodStore.config.podSize` sits on this client's own `8` default,
    // which a guest never overwrites. The two authorities disagree by construction.
    mocks.multiplayerState.phase = "drafting";
    mocks.multiplayerState.view = engineView("Premier", 4, {
      launch_capability: "CommanderMultiplayer",
    });
    renderAt("/draft-pod");

    // Reach guard on the WRONG authority: the config really does still say 8, so
    // the assertion below distinguishes the two sources rather than agreeing with
    // both. (Also proves the intro mounted at all, via the sentence itself.)
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
    // REVERT-FAILING: `podSize={podSize}` off `config.podSize` renders
    // "You're drafting with 8 players in a pod" for this exact fixture.
    expect(
      screen.getByText("You're drafting with 4 players in a pod"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("You're drafting with 8 players in a pod"),
    ).toBeNull();
  });

  it("renders no intro until the engine has published a view", () => {
    // `statusChanged("drafting")` can land before the first `viewUpdated`, so the
    // seat count is briefly unknown. Nothing is rendered rather than the component
    // default of 8 — the count is engine state, and a guess here is unfalsifiable.
    mocks.multiplayerState.phase = "drafting";
    mocks.multiplayerState.view = null;
    renderAt("/draft-pod");

    expect(screen.queryByText(/You're drafting with/)).toBeNull();
    // Non-vacuous: the same fixture with a view DOES render the sentence (test
    // above), so this null is the view gate, not a failed render.
    expect(screen.queryByRole("button", { name: "Start Drafting" })).toBeNull();
  });

  it("keeps the pod intro for the other pod kinds", () => {
    mocks.multiplayerState.phase = "drafting";
    mocks.multiplayerState.view = engineView("Premier", 8);
    renderAt("/draft-pod");

    expect(
      screen.getByText("Open 4 packs; each pack contains 12 cards — pick one, pass the rest"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "After drafting, build a deck of at least 45 cards and play tournament matches",
      ),
    ).toBeInTheDocument();
    // Non-vacuous: the positive above proves the intro mounted.
    expect(
      screen.queryByText(
        "Open 4 packs; each pack contains 12 cards — pick two cards, pass the rest",
      ),
    ).toBeNull();
  });

  it("renders mixed pack sizes from the engine-published view", () => {
    mocks.multiplayerState.phase = "drafting";
    mocks.multiplayerState.view = engineView("Premier", 8, {
      pack_count: 3,
      cards_per_pack: 15,
      pack_sizes: [15, 14, 15],
    });
    renderAt("/draft-pod");

    expect(
      screen.getByText(
        "Open 3 packs of mixed sizes, in this order: 15 cards, 14 cards, and 15 cards — pick one, pass the rest",
      ),
    ).toBeInTheDocument();
  });
});
