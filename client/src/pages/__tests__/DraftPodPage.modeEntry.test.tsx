import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
vi.mock("../../components/draft/HostControls", () => ({ HostControls: () => null }));
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));
vi.mock("../../components/draft/CubeSetupPanel", () => ({ CubeSetupPanel: () => null }));

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
  }> = {},
) {
  return {
    kind,
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
    mocks.draftProcedure.mockResolvedValue({
      pod_size: 6,
      human_seats: 1,
      min_pod_size: 3,
      packs_per_player: 3,
      cards_per_pick: 2,
      min_deck_size: 60,
      match_config: { best_of: 1 },
    });
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
    expect(mocks.draftProcedure).toHaveBeenCalledWith("CommanderDraft");
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
    await waitFor(() => expect(mocks.draftProcedure).toHaveBeenCalledWith("Premier"));
    expect(useDraftPodStore.getState().config.podSize).toBe(8);
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

  it("picks the intro variant from the ENGINE-published kind", () => {
    // A guest's local `draftPodStore.config` is never populated from the host's
    // kind, so the intro must read `view.kind`. This fixture is exactly that
    // case: the store is left on its "Premier" default.
    mocks.multiplayerState.phase = "drafting";
    mocks.multiplayerState.view = engineView("CommanderDraft", 4, {
      min_deck_size: 63,
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
    mocks.multiplayerState.view = engineView("CommanderDraft", 4);
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
