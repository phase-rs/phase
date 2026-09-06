/**
 * U15 — the Commander pod's launch affordance, end to end through the REAL
 * `multiplayerDraftStore` action.
 *
 * The store is deliberately NOT mocked. Every assertion here is about what
 * `launchCommanderGame` produces — the game it hosts, the decks it assembles
 * and the URL it lands on — so a mocked store would leave the whole subject
 * untested. Only the HOST ADAPTER and the TRANSPORT are mocked; `hostDraft` is
 * driven for real so the store's module-private `activeHostAdapter` is
 * installed the way production installs it.
 *
 * PORTED from the `?mode=ai` design. Step 3a-ii replaced the local-AI
 * navigation with a real N-player P2P host, so the URL no longer carries
 * `format`/`players`/`difficulty` and there is no staged
 * `phase:draft-deck:` sessionStorage blob at all. Each case below keeps its
 * claim and moves its assertion to where the value now lives — the
 * `P2PHostAdapter` constructor argument and the seat mutations.
 *
 * Four mocks, each load-bearing. `../../network/connection` and
 * `../../adapter/p2p-adapter`, because the rewritten launch calls the REAL
 * `hostRoom` (which opens a PeerJS `Peer`) and then parks on `await roomFull`.
 * A `getSnapshot` on the fake adapter and
 * `../../game/controllers/gameLoopController`, because `installMatchRuntime`
 * calls `adapter.getSnapshot()`, the REAL `useGameStore.commitEngineSnapshot`
 * and `createGameLoopController` before the launch can navigate. Without them
 * the cheapest way to green is to weaken the end-to-end assertion, which would
 * quietly delete the only test covering the whole host path.
 *
 * At base a completed Commander pod offers exactly one button, "Return to
 * Menu", and no `navigate` call exists to capture.
 */

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DraftPodPage } from "../DraftPodPage";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { P2PHostAdapter } from "../../adapter/p2p-adapter";
import { FORMAT_DEFAULTS } from "../../stores/multiplayerStore";
import type { DraftPlayerView, SeatPublicView } from "../../adapter/draft-adapter";
import type {
  CommanderSeatDecks,
  DraftCommanderLaunch,
  DraftDeckPayload,
} from "../../network/draftProtocol";

// ── Mocks ──────────────────────────────────────────────────────────────

const navigateSpy = vi.fn();

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => navigateSpy,
}));

const commanderSeatDecks = vi.fn<
  (view: DraftPlayerView, localSeat: number) => Promise<CommanderSeatDecks>
>();

/**
 * The store's own listener on the pod host adapter (step 4 addition).
 *
 * The pre-existing `onEvent: vi.fn(() => vi.fn())` DISCARDED it, which made the
 * host's launch-in-flight state unreachable in this suite: `commanderLaunch` is
 * written only by the `commanderLaunch` arm of `handleHostEvent`, and nothing
 * could reach that arm.
 */
const hostListeners: Array<(event: unknown) => void> = [];

/**
 * Completes production's causal chain at PRODUCTION'S OWN EMIT SITE, rather
 * than inventing one: `DraftPodHostAdapter.sendCommanderLaunches` delegates to
 * `P2PDraftHost.sendCommanderLaunches`, whose `sendToSeat` seat-0 arm turns the
 * host's own recipient entry into a `commanderLaunch` host event
 * (`p2p-draft-host.ts:155-158`), which `draftPodHostAdapter.ts:452-459`
 * re-emits to the store. The fake reproduces exactly that one hop; every seat
 * classification and the wire payload's shape stay the mocked adapter's
 * business, so nothing here can make a launch look like it reached seats it
 * did not.
 */
const sendCommanderLaunches = vi.fn(
  (view: DraftPlayerView, gameId: string, roomCode: string, decks: CommanderSeatDecks) => {
    for (const listener of [...hostListeners]) {
      listener({
        type: "commanderLaunch",
        launch: {
          gameId,
          roomCode,
          localDeck: decks.hostDeck,
          playerCount: view.seats.length,
          draftSetCodes: view.draft_set_codes ?? null,
        },
      });
    }
  },
);

const mockHostAdapter = {
  onEvent: vi.fn((listener: (event: unknown) => void) => {
    hostListeners.push(listener);
    return () => {
      const at = hostListeners.indexOf(listener);
      if (at >= 0) hostListeners.splice(at, 1);
    };
  }),
  initialize: vi.fn(async () => {}),
  dispose: vi.fn(async () => {}),
  commanderSeatDecks,
  sendCommanderLaunches,
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

const transport = vi.hoisted(() => {
  const seatMutations: Array<{ type: string; data?: { seatIndex: number; kind?: unknown } }> = [];
  const startPregameGame = vi.fn(async () => ({ log_entries: [] }));
  const instances: Array<{ finish: () => void }> = [];
  return {
    seatMutations,
    startPregameGame,
    instances,
    hostDestroy: vi.fn(),
    dispose: vi.fn(),
    // Step 4: `cancelCommanderLaunch` tears the room down through
    // `terminateGame()`, never `dispose()` — the real adapter has both, and
    // without this the cancel would reject on a missing method.
    terminateGame: vi.fn(async () => {}),
  };
});

vi.mock("../../network/connection", () => ({
  hostRoom: vi.fn(async (_signal: unknown, options?: Record<string, unknown>) => ({
    roomCode: String(options?.preferredRoomCode ?? "ABCDE"),
    peerId: "peer-id",
    peer: { id: "peer-id" },
    onGuestConnected: vi.fn(() => vi.fn()),
    destroy: transport.hostDestroy,
  })),
}));

/**
 * The fake emits `roomFull` from inside `applySeatMutation`, once no waiting
 * seat is left — mirroring the real adapter's ORDERING (its emit is the last
 * statement of that method's body, guarded by `firstWaitingSeat() === null`).
 * A fake that never emits hangs every case; a fake the test pokes by hand
 * passes whether or not the launch's call ordering is right.
 */
vi.mock("../../adapter/p2p-adapter", () => ({
  P2PHostAdapter: vi.fn().mockImplementation(function (...args: unknown[]) {
    const waiting = new Set(Array.from({ length: (args[3] as number) - 1 }, (_, i) => i + 1));
    const listeners: Array<(event: unknown) => void> = [];
    const emitRoomFull = () => {
      for (const listener of [...listeners]) listener({ type: "roomFull" });
    };
    transport.instances.push({ finish: emitRoomFull });
    return {
      onEvent(listener: (event: unknown) => void) {
        listeners.push(listener);
        return () => {};
      },
      initialize: vi.fn(async () => {}),
      applySeatMutation: vi.fn(async (mutation: { type: string; data?: { seatIndex: number } }) => {
        transport.seatMutations.push(mutation);
        if (mutation.type === "SetKind" && mutation.data) waiting.delete(mutation.data.seatIndex);
        if (waiting.size === 0) emitRoomFull();
      }),
      startPregameGame: transport.startPregameGame,
      getSnapshot: vi.fn(async () => ({
        seq: 1,
        state: { waiting_for: { type: "Priority", data: {} } },
        legalResult: {
          actions: [],
          autoPassRecommended: false,
          endContinuousEffectOffers: [],
          manaPaymentShortcutActions: [],
          spellCosts: {},
          legalActionsByObject: {},
        },
      })),
      dispose: transport.dispose,
      terminateGame: transport.terminateGame,
    };
  }),
}));

const matchLoopMock = vi.hoisted(() => ({
  controller: { start: vi.fn(), stop: vi.fn(), dispose: vi.fn() },
  create: vi.fn(),
}));

vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: (...args: unknown[]) => {
    matchLoopMock.create(...args);
    return matchLoopMock.controller;
  },
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
function deckFor(index: number): DraftDeckPayload {
  return {
    main_deck: [`Commander ${index}`, `Spell ${index}`],
    sideboard: [`Side ${index}`],
    commander: [`Commander ${index}`],
  };
}

/**
 * What `commanderSeatDecks` returns for a pod whose seat 0 is the human host
 * and whose remaining seats are bots: `localSeat` is the only live human, and
 * its entry in `liveSeatDecks` is the SAME OBJECT as `hostDeck`.
 */
function seatDecksFor(seatCount: number, localSeat = 0): CommanderSeatDecks {
  const hostDeck = deckFor(localSeat);
  return {
    hostDeck,
    liveSeatDecks: [{ seat: localSeat, deck: hostDeck }],
    engineSeatDecks: Array.from({ length: seatCount }, (_, i) => i)
      .filter((i) => i !== localSeat)
      .map((seatIndex) => ({ seat: seatIndex, deck: deckFor(seatIndex) })),
  };
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
  commanderSeatDecks.mockResolvedValue(seatDecksFor(seatCount, localSeat));
}

// ── Step 4 fixtures ────────────────────────────────────────────────────

/**
 * A pod whose seat 1 is a SECOND live human, so the launch PARKS.
 *
 * Not decoration: the P2P fake fills a waiting seat only on the `SetKind`
 * mutation an ENGINE seat receives, so an all-bot pod fills itself, resolves
 * `roomFull` and navigates away before any waiting state can be observed. Seat
 * 1 receives no mutation, `roomFull` never fires, and the launch sits exactly
 * where the host's waiting UI exists to describe it — which is also the only
 * window `cancelCommanderLaunch` can act in.
 */
function commanderViewAwaitingGuest(seatCount: number): DraftPlayerView {
  const view = commanderView(seatCount);
  return {
    ...view,
    seats: view.seats.map((s, i) => (i === 1 ? seat(1, false) : s)),
  } as DraftPlayerView;
}

/** `seatDecksFor`'s counterpart for that pod: seats 0 and 1 are both live. */
function seatDecksAwaitingGuest(seatCount: number): CommanderSeatDecks {
  const hostDeck = deckFor(0);
  return {
    hostDeck,
    liveSeatDecks: [
      { seat: 0, deck: hostDeck },
      { seat: 1, deck: deckFor(1) },
    ],
    engineSeatDecks: Array.from({ length: seatCount }, (_, i) => i)
      .filter((i) => i > 1)
      .map((seatIndex) => ({ seat: seatIndex, deck: deckFor(seatIndex) })),
  };
}

async function installPodAwaitingGuest(seatCount = 4) {
  await installCompletedPod(seatCount);
  useMultiplayerDraftStore.setState({ view: commanderViewAwaitingGuest(seatCount) });
  commanderSeatDecks.mockResolvedValue(seatDecksAwaitingGuest(seatCount));
}

/**
 * What a GUEST's `commanderLaunch` arm wrote — the invitation, as it exists on
 * a client that has not pressed Join. Hand-fed here on purpose: the page's
 * whole contract is that it READS this field, and what puts it there is
 * `handleGuestEvent`, which `multiplayerDraftStore.commanderLaunch.test.ts`
 * owns end to end.
 */
function guestLaunch(): DraftCommanderLaunch {
  return {
    gameId: "9d1f0d1e-commander",
    roomCode: "ABCDE-commander-9d1f0d1e",
    localDeck: deckFor(1),
    playerCount: 4,
    draftSetCodes: ["TST"],
  };
}

/**
 * `joinCommanderGame`, replaced in the store for the guest rows.
 *
 * The action's own behaviour (dial the room, seat from `playerIdentity`,
 * navigate) is `multiplayerDraftStore.commanderLaunch.test.ts`'s subject and is
 * covered there. What step 4 fixes is that NOTHING CALLED IT, so the page rows
 * assert the call. Restored in `afterEach` — `reset()` spreads `initialState`,
 * which carries no actions, so a replacement would otherwise outlive its test.
 */
const joinCommanderGameSpy = vi.fn(async () => {});
const realJoinCommanderGame = useMultiplayerDraftStore.getState().joinCommanderGame;

/** The message a join onto a room the host already cancelled surfaces (F5). */
const DEAD_ROOM_MESSAGE = "Could not connect to peer ABCDE-commander-9d1f0d1e";

function renderPage() {
  return render(
    <MemoryRouter>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

/**
 * The captured URL, asserted non-null so no negative below can be vacuous.
 *
 * AWAITED, unlike the pre-port version: the launch now spans `hostRoom`,
 * `commanderSeatDecks`, the adapter's `initialize`, one mutation per engine
 * seat, `roomFull` and `startPregameGame` before it navigates, so a
 * synchronous read after the click would race the chain. Awaiting it also
 * makes it the reach guard for every assertion that follows in its case.
 */
async function capturedUrl(): Promise<string> {
  await waitFor(() => expect(navigateSpy).toHaveBeenCalledTimes(1));
  const arg = navigateSpy.mock.calls[0][0];
  expect(typeof arg).toBe("string");
  return arg as string;
}

/** The `P2PHostAdapter` constructor's positional arguments for this launch. */
function ctorArgs(): unknown[] {
  expect(P2PHostAdapter).toHaveBeenCalledTimes(1);
  return vi.mocked(P2PHostAdapter).mock.calls[0] as unknown[];
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
    commanderSeatDecks.mockReset();
    sendCommanderLaunches.mockClear();
    vi.mocked(P2PHostAdapter).mockClear();
    transport.seatMutations.length = 0;
    transport.instances.length = 0;
    transport.startPregameGame.mockClear();
    transport.hostDestroy.mockClear();
    transport.dispose.mockClear();
    transport.terminateGame.mockClear();
    joinCommanderGameSpy.mockClear();
    // The host adapter is a MODULE-SCOPE object, so its recorded listeners
    // outlive a test. `hostDraft` registers one per install and `reset()` does
    // not unregister it, so without this a later launch would emit into every
    // earlier test's store listener.
    hostListeners.length = 0;
    useMultiplayerDraftStore.setState({ error: null });
  });

  afterEach(async () => {
    // Unpark any launch still waiting on `roomFull`. The store's in-flight
    // handle is module-local and cleared only by its own `finally`, so a case
    // that left one parked would make the NEXT case return at the in-flight
    // guard. Every pod here is all-bot and fills itself, so this is a belt for
    // the failure cases; one macrotask drains the continuation.
    for (const instance of transport.instances) instance.finish();
    await new Promise((resolve) => setTimeout(resolve, 0));
    cleanup();
    useMultiplayerDraftStore.getState().reset();
    // AFTER `reset()`: it spreads `initialState`, which carries state only, so
    // a replaced action survives it and would leak into every later test.
    useMultiplayerDraftStore.setState({ joinCommanderGame: realJoinCommanderGame });
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

    // PORTED: the format used to ride the URL as `&format=CommanderDraft`; it
    // is now the host adapter's 5th constructor argument. PROBE B's control
    // still matters — `Limited` and `CommanderDraft` differ on five
    // FormatConfig fields, so this cannot pass against the incumbent.
    expect(await capturedUrl()).toContain("/game/");
    expect(ctorArgs()[4]).toBe(FORMAT_DEFAULTS.CommanderDraft);
  });

  // VM-2 — the seat count is READ from `view.seats`, never the literal 4.
  // The 5-seat case is what makes this non-vacuous: a hardcoded `4` passes the
  // first case and fails the second.
  it.each([[4], [5]])("carries the pod's own seat count (%i seats)", async (seatCount) => {
    await installCompletedPod(seatCount);
    renderPage();
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    await clickLaunch();

    // PORTED from `players=N` on the URL to the constructor's 4th argument,
    // which is the value `P2PHostAdapter` actually seats the game at.
    await capturedUrl();
    expect(ctorArgs()[3]).toBe(seatCount);
  });

  // VM-3 — a negative, so its reach guard is the non-null capture in
  // `capturedUrl()` rather than a bare absence over a call that never happened.
  // DISCLOSED: at base no URL is produced, so this is a FORWARD guard against a
  // future edit adding the params, not a revert detector.
  it("does not bind the game to a local draft run", async () => {
    await installCompletedPod(4);
    renderPage();
    await clickLaunch();

    const url = await capturedUrl();
    // PORTED: `mode=ai` was the bug — it sent the host into a LOCAL AI game and
    // told the other three players nothing. The launch now hosts a real P2P
    // game, and this is the assertion that reds if it regresses.
    expect(url).toContain("mode=draft-match");
    expect(url).not.toContain("mode=ai");
    expect(url).not.toContain("source=draft");
    expect(url).not.toContain("draftId=");
  });

  // VM-5 — PORTED. There is no `phase:draft-deck:` blob any more: the host's
  // own deck is the constructor payload's `player`, and the other seats reach
  // the game as `SetKind` Ai mutations keyed by their OWN seat index rather
  // than by a position in an ordered array. The mapping claim survives; its
  // evidence moves.
  it("seats the local player's own deck and gives every other seat an engine pilot", async () => {
    await installCompletedPod(4);
    renderPage();
    await clickLaunch();
    await capturedUrl();

    const payload = ctorArgs()[0] as { player: DraftDeckPayload; draft_set_codes: unknown };
    // Reach guard before any mapping assertion.
    expect(payload.player.main_deck.length).toBeGreaterThan(0);
    expect(payload.player.commander).toEqual(["Commander 0"]);

    expect(transport.seatMutations.map((m) => m.data?.seatIndex)).toEqual([1, 2, 3]);
    expect(
      transport.seatMutations.map(
        (m) => (m.data?.kind as { data: { deck: { data: DraftDeckPayload } } }).data.deck.data.commander[0],
      ),
    ).toEqual(["Commander 1", "Commander 2", "Commander 3"]);
    // The store passes the LOCAL seat through to the deck assembler, which is
    // the binding this row pins.
    expect(commanderSeatDecks).toHaveBeenCalledWith(expect.anything(), 0);
  });

  // VM-6 — each seat's OWN commander survives the wire. Two seats with
  // DIFFERENT designations, so "they differ" cannot pass on two empties.
  it("carries each seat's own commander", async () => {
    await installCompletedPod(4);
    renderPage();
    await clickLaunch();
    await capturedUrl();

    const payload = ctorArgs()[0] as { player: DraftDeckPayload };
    const firstAiDeck = (transport.seatMutations[0].data?.kind as {
      data: { deck: { data: DraftDeckPayload } };
    }).data.deck.data;
    expect(payload.player.commander.length).toBeGreaterThan(0);
    expect(payload.player.commander).not.toEqual(firstAiDeck.commander);
  });

  // Hostile fixture — a non-zero local seat. SYNTHETIC before the port
  // (`hostDraft` sets `seatIndex: 0` when the host role is taken, so a host
  // with a non-zero seat is not production-reachable) and DOUBLY so after it:
  // this suite's view makes seat 0 the live human and seats 1..N-1 bots, so a
  // local seat of 2 describes a pod whose live human seat can never be filled.
  // Such a launch parks on `roomFull` forever and never navigates.
  //
  // Kept rather than dropped — the charter requires this fixture get an
  // explicit disposition — but NARROWED to the one claim that survives the new
  // design: `seatIndex` is passed through to the deck assembler rather than
  // hardcoded to 0. Its reach guard is the constructor call, which happens
  // strictly after `commanderSeatDecks` resolves, not a URL that cannot appear.
  it("passes a non-zero local seat through to the deck assembler", async () => {
    await installCompletedPod(4, 2);
    renderPage();
    await clickLaunch();

    await waitFor(() => expect(P2PHostAdapter).toHaveBeenCalledTimes(1));
    expect(commanderSeatDecks).toHaveBeenCalledWith(expect.anything(), 2);
    expect(navigateSpy).not.toHaveBeenCalled();
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

    expect(commanderSeatDecks).not.toHaveBeenCalled();
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
  // `commanderSeatDecks` propagate into this store's `try/catch`. The
  // store's job is that a rejection becomes visible `error` text and NO
  // navigation — one behaviour, one test. The Rust rows tell the causes apart.
  //
  // This is the same shape as an unsubmitted local seat, whose
  // `submittedDeckForSeat` throw reaches the identical catch.
  it("surfaces a payload rejection as visible text and does not navigate", async () => {
    await installCompletedPod(4);
    commanderSeatDecks.mockRejectedValue(new Error(REJECTION_MESSAGE));
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

  // ── Step 4 — the three store-read states of `CompleteView` ───────────
  //
  // Every row below asserts through `getByRole`, never a class name, and every
  // one of them re-asserts that "Return to Menu" is still reachable: it is the
  // one control the charter requires in EVERY state, and a state machine that
  // drops it is exactly the reported bug in a new costume.

  /**
   * THE REPORTED BUG. Four humans drafted a Commander cube; the host launched
   * and the other three saw a screen whose only control was "Return to Menu".
   * Steps 1-3b built the whole mechanism and left nothing calling it.
   *
   * REVERT-FAILING: restore `CompleteView`'s single `canLaunch &&` block and
   * this row reds on a missing button, not on a weaker assertion.
   *
   * The action is replaced rather than run: what it DOES is
   * `multiplayerDraftStore.commanderLaunch.test.ts`'s subject; what step 4 adds
   * is the call, so the call is what this asserts.
   */
  /**
   * The guest's mirror of the host's waiting row. The join parks until every
   * live seat has joined AND the host starts the game, so a Join button with
   * no `disabled` and no status text is indistinguishable from a dead one for
   * minutes — the same "stranded with nothing to look at" complaint this whole
   * change exists to answer, reintroduced one screen later.
   *
   * Two independent affordances, asserted separately so removing either reds
   * this row on its own: the live region a screen reader hears, and the
   * `disabled` the browser enforces against a second press.
   */
  it("disables the guest join and announces the wait while a join is in flight", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({
      role: "guest",
      commanderLaunch: guestLaunch(),
      commanderJoinPending: true,
      joinCommanderGame: joinCommanderGameSpy,
    });
    renderPage();
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    expect(screen.getByRole("status")).toHaveTextContent(
      "Joining — waiting for the other players…",
    );
    expect(screen.getByRole("button", { name: "Join Commander Game" })).toBeDisabled();
    // The escape hatch survives every state — a guest waiting on a host that
    // never starts must still be able to leave.
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
  });

  /**
   * The negative half: with no join running the button is live and silent.
   * Without this row the assertions above pass against a Join button that is
   * ALWAYS disabled and a status line that is always shown.
   */
  it("leaves the guest join pressable and silent when no join is in flight", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({
      role: "guest",
      commanderLaunch: guestLaunch(),
      joinCommanderGame: joinCommanderGameSpy,
    });
    renderPage();
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    expect(screen.getByRole("button", { name: "Join Commander Game" })).toBeEnabled();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("renders a join affordance for a guest holding a launch and dispatches the join", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({
      role: "guest",
      commanderLaunch: guestLaunch(),
      joinCommanderGame: joinCommanderGameSpy,
    });
    renderPage();
    // Reach guard: `CompleteView` mounted, so every absence below is a real
    // absence rather than a failed render.
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Join Commander Game" }));

    expect(joinCommanderGameSpy).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
    // A guest never acquires the host's controls: it holds no pod session to
    // assemble decks from.
    expect(
      screen.queryByRole("button", { name: "Start Commander Game" }),
    ).not.toBeInTheDocument();
  });

  /**
   * The control that makes the row above non-vacuous. `commanderLaunch` is
   * `null` at base, so a Join button rendered unconditionally for guests would
   * pass that row and fail this one.
   */
  it("renders no join affordance for a guest that has received no launch", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({ role: "guest" });
    renderPage();

    expect(screen.getByText("Draft Complete")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Join Commander Game" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
  });

  /**
   * The host's launch-in-flight state, reached by a REAL click on a REAL
   * launch that parks on `roomFull` — see `commanderViewAwaitingGuest` for why
   * the second live seat is what makes it park.
   */
  it("shows the waiting state, disables the launch and offers Cancel while a launch is in flight", async () => {
    await installPodAwaitingGuest(4);
    renderPage();
    await clickLaunch();

    // Reach guard AND the field driven non-null: the waiting text can only
    // appear once `sendCommanderLaunches` has invited the seats, which is the
    // sole writer of `commanderLaunch` on the host.
    expect(await screen.findByText("Waiting for players to join…")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start Commander Game" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Cancel Launch" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
    // The host holds a `commanderLaunch` of its own — its seat is invited like
    // every other live seat — so a join affordance keyed on that field ALONE
    // would offer the host a room it is already hosting. The role half of the
    // guest predicate is what this pins.
    expect(
      screen.queryByRole("button", { name: "Join Commander Game" }),
    ).not.toBeInTheDocument();
    // The pod is still waiting on a human seat, so nothing has navigated.
    expect(navigateSpy).not.toHaveBeenCalled();
  });

  /**
   * D7 — Cancel returns the pod to the launch-available state, through the REAL
   * `cancelCommanderLaunch`: the launch above left a live in-flight handle, so
   * this exercises the abort, the `terminateGame()` teardown and the state
   * clear rather than a fake standing in for them.
   */
  it("restores the launch-available state when the host cancels", async () => {
    await installPodAwaitingGuest(4);
    renderPage();
    await clickLaunch();
    // The waiting state must EXIST before it can be shown to go away.
    expect(await screen.findByText("Waiting for players to join…")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Cancel Launch" }));

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Start Commander Game" })).toBeEnabled(),
    );
    expect(screen.queryByText("Waiting for players to join…")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel Launch" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
    // The room is torn down with `terminateGame()`, never `dispose()`, so the
    // guests that DID connect are told rather than left reconnecting.
    expect(transport.terminateGame).toHaveBeenCalledTimes(1);
    expect(transport.dispose).not.toHaveBeenCalled();
    // A cancel is a user action, not a failure: the launch's catch returns on
    // `signal.aborted` before it can raise a banner.
    expect(screen.queryByTestId("pod-error-banner")).not.toBeInTheDocument();
    expect(navigateSpy).not.toHaveBeenCalled();
  });

  /**
   * A pod the P2P transport cannot carry still gets a game — a LOCAL one.
   *
   * The engine's Commander Draft format allows eight seats while
   * `P2PHostAdapter` throws above six, so the ceiling decides WHICH game the
   * button starts, not whether it starts one. This row previously asserted the
   * button was disabled; that was a regression, because a legally drafted
   * 8-pod then produced decks nobody could play. The label states the outcome
   * and the button stays pressable.
   *
   * Seven, not six: six is the boundary the ceiling ADMITS, so a fixture at six
   * would pass against an off-by-one and against no check at all.
   */
  it("offers a local game, not a dead button, for a pod over the P2P seat ceiling", async () => {
    await installCompletedPod(7);
    renderPage();
    expect(screen.getByText("Draft Complete")).toBeInTheDocument();

    const launch = screen.getByRole("button", { name: "Start Local Commander Game" });
    expect(launch).toBeEnabled();
    expect(
      screen.getByText(
        "This pod seats 7 players; a peer-to-peer Commander game supports at most 6."
          + " You will play this one locally against the engine, with every drafted deck.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
    // The P2P name must NOT be offered — a user over the ceiling is never told
    // they are getting the multiplayer game.
    expect(
      screen.queryByRole("button", { name: "Start Commander Game" }),
    ).not.toBeInTheDocument();
  });

  /**
   * A six-seat pod is the control for the row above: it sits ON the ceiling, so
   * it must offer the REAL multiplayer launch and none of the local-fallback
   * copy. Without it, "always show the local fallback" would pass.
   */
  it("still offers the peer-to-peer launch for a pod exactly at the P2P seat ceiling", async () => {
    await installCompletedPod(6);
    renderPage();

    expect(screen.getByRole("button", { name: "Start Commander Game" })).toBeEnabled();
    expect(screen.queryByText(/supports at most/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Start Local Commander Game" }),
    ).not.toBeInTheDocument();
  });

  /**
   * CARRY-FORWARD ITEM 1 (review F5) — a cancel never reaches a guest that has
   * not pressed Join.
   *
   * `cancelCommanderLaunch` broadcasts `host_left` over the GAME adapter's
   * sessions; a guest still on the POD wire has none, so it keeps a
   * `commanderLaunch` whose room is gone and its Join dials a dead Peer. The
   * store's catch writes exactly the pair driven here — the message, and
   * `commanderLaunch` DELIBERATELY left set, because "the invitation is still
   * open and the seat can still be taken".
   *
   * What this row pins is that the resulting screen is RECOVERABLE rather than
   * a dead end: the failure is stated, the banner can be dismissed, the Join is
   * live again (a relaunch overwrites `commanderLaunch` on the pod wire, which
   * is what makes retrying meaningful), and leaving is still one press away.
   */
  it("leaves a guest whose join failed able to dismiss it, retry, or leave", async () => {
    await installCompletedPod(4);
    useMultiplayerDraftStore.setState({
      role: "guest",
      commanderLaunch: guestLaunch(),
      joinCommanderGame: joinCommanderGameSpy,
      error: DEAD_ROOM_MESSAGE,
    });
    renderPage();

    // Reach guard: the failure is VISIBLE, so nothing below passes on a screen
    // that simply never rendered the banner.
    expect(screen.getByText(DEAD_ROOM_MESSAGE)).toBeInTheDocument();

    const join = screen.getByRole("button", { name: "Join Commander Game" });
    expect(join).toBeEnabled();
    await userEvent.click(join);
    expect(joinCommanderGameSpy).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByTestId("pod-error-banner")).not.toBeInTheDocument();
    // Still offered after the dismissal — the retry survives clearing the
    // banner, and the exit was never taken away.
    expect(screen.getByRole("button", { name: "Join Commander Game" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
  });
});
