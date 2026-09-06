/**
 * U15 — the Commander pod's launch affordance, end to end through the REAL
 * `multiplayerDraftStore` action.
 *
 * The store is deliberately NOT mocked. Every assertion here is about what
 * `launchCommanderGame` produces — the URL's format and seat count, the staged
 * `phase:draft-deck:` blob, and the seat -> game-player mapping — so a mocked
 * store would leave the whole subject untested. Only the HOST ADAPTER is
 * mocked, which is the same seam `multiplayerDraftStore.test.ts` mocks, and
 * `hostDraft` is driven for real so the store's module-private
 * `activeHostAdapter` is installed the way production installs it.
 *
 * At base a completed Commander pod offers exactly one button, "Return to
 * Menu", and no `navigate` call exists to capture.
 */

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import type { DraftPlayerView, SeatPublicView } from "../../adapter/draft-adapter";
import type { DraftMatchDeckPayload } from "../../network/draftProtocol";

// ── Mocks ──────────────────────────────────────────────────────────────

const navigateSpy = vi.fn();

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => navigateSpy,
}));

const podCommanderDeckPayload = vi.fn<
  (view: DraftPlayerView, localSeat: number) => Promise<DraftMatchDeckPayload>
>();

const mockHostAdapter = {
  onEvent: vi.fn(() => vi.fn()),
  initialize: vi.fn(async () => {}),
  dispose: vi.fn(async () => {}),
  podCommanderDeckPayload,
  status: "lobby" as const,
  roomCode: "ABCDE",
};

vi.mock("../../adapter/draftPodHostAdapter", () => ({
  // `function`, not an arrow: `hostDraft` calls `new DraftPodHostAdapter()`,
  // and an arrow function is not a constructor.
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    return mockHostAdapter;
  }),
}));

// Only the hook is stubbed; every other export stays real, so the `?kind=`
// slug the page's entry effect reads keeps its single authority.
vi.mock("../../stores/draftPodStore", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../stores/draftPodStore")>()),
  useDraftPodStore: (
    selector: (state: { reset: () => void; resumeHostedPod: () => void }) => unknown,
  ) => selector({ reset: vi.fn(), resumeHostedPod: vi.fn() }),
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

// ── Fixtures ───────────────────────────────────────────────────────────

function seat(index: number, isBot: boolean): SeatPublicView {
  return {
    seat_index: index,
    display_name: isBot ? `Bot ${index}` : `Player ${index}`,
    is_bot: isBot,
    connected: true,
    has_submitted_deck: true,
    pick_status: "NotDrafting",
    active_pack_count: 0,
    face_up_draft_cards: [],
  };
}

function commanderView(seatCount: number): DraftPlayerView {
  return {
    status: "Complete",
    kind: "CommanderDraft",
    launch_capability: "CommanderMultiplayer",
    commanders_required: 1,
    current_pack_number: 3,
    pick_number: 1,
    pass_direction: "Left",
    current_pack: null,
    required_pick_count: 0,
    pool: [],
    draft_effects: [],
    pool_groups: {
      color_groups: [],
      type_groups: [],
      cmc_groups: [],
      rarity_groups: [],
      type_filter_options: [],
      color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    },
    seats: Array.from({ length: seatCount }, (_, i) => seat(i, i !== 0)),
    cards_per_pack: 14,
    pack_count: 3,
    min_deck_size: 60,
    addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
  } as unknown as DraftPlayerView;
}

/** One `DraftDeckPayload` per seat, each carrying its OWN commander. */
function payloadForSeats(seatOrder: number[]): DraftMatchDeckPayload {
  const deck = (s: number) => ({
    main_deck: [`Commander ${s}`, `Spell ${s}`],
    sideboard: [`Side ${s}`],
    commander: [`Commander ${s}`],
  });
  const [player, opponent, ...ai] = seatOrder.map(deck);
  return { player, opponent, ai_decks: ai };
}

async function installCompletedPod(seatCount: number, localSeat = 0) {
  await useMultiplayerDraftStore.getState().hostDraft({
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    kind: "CommanderDraft",
    podSize: seatCount,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
  });
  useMultiplayerDraftStore.setState({
    phase: "complete",
    role: "host",
    seatIndex: localSeat,
    view: commanderView(seatCount),
    standings: [],
    error: null,
  });
}

function renderPage() {
  return render(
    <MemoryRouter>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

/** The captured URL, asserted non-null so no negative below can be vacuous. */
function capturedUrl(): string {
  expect(navigateSpy).toHaveBeenCalledTimes(1);
  const arg = navigateSpy.mock.calls[0][0];
  expect(typeof arg).toBe("string");
  return arg as string;
}

/** The exact `Error.message` the store surfaces verbatim through the banner. */
const REJECTION_MESSAGE = "Card database must be loaded before a Commander Draft bot deck";

async function clickLaunch() {
  const button = await screen.findByRole("button", { name: "Start Commander Game" });
  await userEvent.click(button);
}

// ── Tests ──────────────────────────────────────────────────────────────

describe("DraftPodPage Commander launch", () => {
  beforeEach(() => {
    navigateSpy.mockClear();
    podCommanderDeckPayload.mockReset();
    podCommanderDeckPayload.mockResolvedValue(payloadForSeats([0, 1, 2, 3]));
    sessionStorage.clear();
    useMultiplayerDraftStore.setState({ error: null });
  });

  afterEach(() => {
    cleanup();
    sessionStorage.clear();
  });

  // VM-1 — REVERT-FAILING: at base `CompleteView` renders exactly one button
  // and no `navigate` call exists to capture.
  it("launches a CommanderDraft game", async () => {
    await installCompletedPod(4);
    renderPage();
    // Reach guard: `CompleteView` mounted, so a missing button would be a real
    // absence rather than a failed render.
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    await clickLaunch();

    // PROBE B's control matters here: `Limited` and `CommanderDraft` differ on
    // five FormatConfig fields, so this cannot pass against the incumbent.
    expect(capturedUrl()).toContain("format=CommanderDraft");
  });

  // VM-2 — the seat count is READ from `view.seats`, never the literal 4.
  // The 5-seat case is what makes this non-vacuous: a hardcoded `4` passes the
  // first case and fails the second.
  it.each([
    [4, "players=4"],
    [5, "players=5"],
  ])("carries the pod's own seat count (%i seats)", async (seatCount, expected) => {
    podCommanderDeckPayload.mockResolvedValue(
      payloadForSeats(Array.from({ length: seatCount }, (_, i) => i)),
    );
    await installCompletedPod(seatCount);
    renderPage();
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    await clickLaunch();

    expect(capturedUrl()).toContain(expected);
  });

  // VM-3 — a negative, so its reach guard is the non-null capture in
  // `capturedUrl()` rather than a bare absence over a call that never happened.
  // DISCLOSED: at base no URL is produced, so this is a FORWARD guard against a
  // future edit adding the params, not a revert detector.
  it("does not bind the game to a local draft run", async () => {
    await installCompletedPod(4);
    renderPage();
    await clickLaunch();

    const url = capturedUrl();
    expect(url).toContain("mode=ai");
    expect(url).not.toContain("source=draft");
    expect(url).not.toContain("draftId=");
  });

  // VM-5 — the seat -> game-player mapping, read back out of sessionStorage.
  it("maps the local seat to game player 0", async () => {
    await installCompletedPod(4);
    renderPage();
    await clickLaunch();

    const url = capturedUrl();
    const gameId = url.slice("/game/".length, url.indexOf("?"));
    const raw = sessionStorage.getItem(`phase:draft-deck:${gameId}`);
    expect(raw).not.toBeNull();
    const blob = JSON.parse(raw as string) as DraftMatchDeckPayload;

    // Reach guard before any ordering assertion.
    expect(blob.player.main_deck.length).toBeGreaterThan(0);
    expect(blob.player.commander).toEqual(["Commander 0"]);
    expect(blob.opponent.commander).toEqual(["Commander 1"]);
    expect(blob.ai_decks.map((d) => d.commander[0])).toEqual([
      "Commander 2",
      "Commander 3",
    ]);
    expect(blob.ai_decks).toHaveLength(4 - 2);
    // The store passes the LOCAL seat through to the payload assembler, which
    // is the binding this row pins.
    expect(podCommanderDeckPayload).toHaveBeenCalledWith(expect.anything(), 0);
  });

  // VM-6 — each seat's OWN commander survives the wire. Two seats with
  // DIFFERENT designations, so "they differ" cannot pass on two empties.
  it("carries each seat's own commander", async () => {
    await installCompletedPod(4);
    renderPage();
    await clickLaunch();

    const url = capturedUrl();
    const gameId = url.slice("/game/".length, url.indexOf("?"));
    const blob = JSON.parse(
      sessionStorage.getItem(`phase:draft-deck:${gameId}`) as string,
    ) as DraftMatchDeckPayload;

    expect(blob.player.commander.length).toBeGreaterThan(0);
    expect(blob.player.commander).not.toEqual(blob.opponent.commander);
  });

  // Hostile fixture — a non-zero local seat. SYNTHETIC: `hostDraft` sets
  // `seatIndex: 0` when the host role is taken, so a host with a non-zero seat
  // is not production-reachable. Kept as a MAPPING-RULE pin, not as a real
  // multi-authority fixture.
  it("passes a non-zero local seat through to the payload assembler", async () => {
    await installCompletedPod(4, 2);
    renderPage();
    await clickLaunch();

    capturedUrl();
    expect(podCommanderDeckPayload).toHaveBeenCalledWith(expect.anything(), 2);
  });

  // Hostile fixture — the engine capability, rather than the kind label,
  // authorizes a completed pod game.
  // The reach guard and the negative live in this one case because the guard
  // ("Draft Complete") is a POSITIVE assertion about a different element, so it
  // cannot mask the negative below it.
  it("renders no launch button when the engine withdraws launch capability", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({
      view: { ...commanderView(4), launch_capability: "None" } as DraftPlayerView,
    });
    renderPage();

    expect(screen.getByText("Draft Complete")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Start Commander Game" }),
    ).not.toBeInTheDocument();
  });

  it("refuses a direct launch request when the engine withdraws launch capability", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({
      view: { ...commanderView(4), launch_capability: "None" } as DraftPlayerView,
    });

    await useMultiplayerDraftStore.getState().launchCommanderGame(navigateSpy);

    expect(podCommanderDeckPayload).not.toHaveBeenCalled();
    expect(navigateSpy).not.toHaveBeenCalled();
  });

  // Hostile fixture — role authority. A guest has no `activeHostAdapter` and
  // therefore no session to read the decks from.
  it("renders no launch button for a guest", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({ role: "guest" });
    renderPage();

    expect(screen.getByText("Draft Complete")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Start Commander Game" }),
    ).not.toBeInTheDocument();
  });

  // [B1] propagation — written against the REJECTION, not against its cause.
  // `get_bot_deck_inner` refuses on two conditions (a missing card database and
  // a deck under `min_deck_size`) and both surface identically here: an `Err`
  // becomes a rejected `getBotDeck` promise, which `botDeckForSeat` and
  // `podCommanderDeckPayload` propagate into this store's `try/catch`. The
  // store's job is that a rejection becomes visible `error` text and NO
  // navigation — one behaviour, one test. The Rust rows tell the causes apart.
  //
  // This is the same shape as an unsubmitted local seat, whose
  // `submittedDeckForSeat` throw reaches the identical catch.
  it("surfaces a payload rejection as visible text and does not navigate", async () => {
    podCommanderDeckPayload.mockRejectedValue(new Error(REJECTION_MESSAGE));
    await installCompletedPod(4);
    renderPage();
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    await clickLaunch();

    // The RENDERED surface, asserted FIRST and asserted instead of the store
    // write alone: `store.error` reaches this screen only through
    // `<PodErrorBanner />`, so a `CompleteView` without the banner writes the
    // field and displays nothing. A `getState().error` row passes on exactly
    // that screen — which is how the missing banner concealed itself. This row
    // reds if the banner is removed from `CompleteView`.
    expect(await screen.findByText(REJECTION_MESSAGE)).toBeInTheDocument();
    // Reach guard for the negative below: the banner proves the catch actually
    // ran, so "did not navigate" cannot pass on a click that never dispatched.
    expect(navigateSpy).not.toHaveBeenCalled();
    // Kept: the rendered text is `error` verbatim, but this pins WHICH field
    // the banner is reading, so a future banner sourced from elsewhere reds.
    expect(useMultiplayerDraftStore.getState().error).toBe(REJECTION_MESSAGE);
  });
});
