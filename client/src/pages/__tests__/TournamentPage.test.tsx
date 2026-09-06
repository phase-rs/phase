import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  MemoryRouter,
  Route,
  RouterProvider,
  Routes,
  createMemoryRouter,
} from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// See `TournamentLandingPage.test.tsx` for why this shim is copied rather than
// shared: the store's persist middleware touches `localStorage` at import time.
const localStorageMock = vi.hoisted(() => {
  const items = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => {
        items.set(key, value);
      },
      removeItem: (key: string) => {
        items.delete(key);
      },
      clear: () => {
        items.clear();
      },
      key: (index: number) => [...items.keys()][index] ?? null,
      get length() {
        return items.size;
      },
    },
  });
  return { items };
});

const mocks = vi.hoisted(() => ({ navigate: vi.fn() }));

vi.mock("react-router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react-router")>()),
  useNavigate: () => mocks.navigate,
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({
  ScreenChrome: () => null,
}));

// ONLY the transport seam is faked — the store, `tournamentClient` and
// `brokerClient` are real, or every frame assertion below would be vacuous.
vi.mock("../../services/openPhaseSocket", () => ({
  HandshakeError: class HandshakeError extends Error {
    kind: string;

    constructor(message: string, kind: string) {
      super(message);
      this.kind = kind;
    }
  },
  openPhaseSocket: vi.fn(),
  withReconnect: vi.fn(),
}));

import type {
  PlayerSummary,
  Tiebreaks,
  TournamentPairingView,
  TournamentStanding,
  TournamentSummary,
  TournamentView,
} from "../../adapter/types";
import { LOBBY_PROTOCOL_VERSION } from "../../adapter/ws-adapter";
import {
  expectCatalogValuePresent,
  expectNoRawKeyPaths,
} from "../../components/tournament/__tests__/tournamentTestUtils";
import { openPhaseSocket, withReconnect } from "../../services/openPhaseSocket";
import {
  useMultiplayerStore,
  type TournamentCredential,
} from "../../stores/multiplayerStore";
import { TournamentPage } from "../TournamentPage";

// ── Harness (copied from multiplayerStore.tournament.test.ts:66-165) ──────

type Listener = (event: unknown) => void;

function makeFakeSocket() {
  const listeners = new Map<string, Set<Listener>>();
  const send = vi.fn();
  const ws = {
    readyState: 1, // WebSocket.OPEN
    send,
    close: vi.fn(),
    addEventListener: vi.fn((type: string, fn: Listener) => {
      let bucket = listeners.get(type);
      if (!bucket) {
        bucket = new Set();
        listeners.set(type, bucket);
      }
      bucket.add(fn);
    }),
    removeEventListener: vi.fn((type: string, fn: Listener) => {
      listeners.get(type)?.delete(fn);
    }),
  };
  return {
    socket: {
      serverInfo: {
        version: "test",
        buildCommit: "test",
        mode: "LobbyOnly" as const,
        protocolVersion: 14,
        lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
      },
      ws,
      close: vi.fn(),
    },
    send,
    listenerCount: (type = "message") => listeners.get(type)?.size ?? 0,
    deliver: (type: string, data?: unknown) => {
      for (const fn of [...(listeners.get("message") ?? [])]) {
        fn({ data: JSON.stringify({ type, data }) });
      }
    },
    clearSent: () => send.mockClear(),
    /** Exact parsed-tag equality — never a regex. */
    tally: (tag: string) =>
      send.mock.calls.filter(
        ([raw]) => (JSON.parse(raw as string) as { type: string }).type === tag,
      ).length,
    frame: (tag: string, nth = 0) =>
      send.mock.calls
        .map(([raw]) => JSON.parse(raw as string) as { type: string; data?: unknown })
        .filter((f) => f.type === tag)[nth],
  };
}

type FakeSocket = ReturnType<typeof makeFakeSocket>;

function primeSocket(fake: FakeSocket, openWith?: Promise<unknown>): void {
  vi.mocked(openPhaseSocket).mockImplementation(
    () =>
      (openWith ?? Promise.resolve(fake.socket)) as ReturnType<typeof openPhaseSocket>,
  );
  vi.mocked(withReconnect).mockImplementation((factory, opts) => {
    let current: Awaited<ReturnType<typeof factory>> | null = null;
    void (async () => {
      current = await factory(0);
      opts?.onStateChange?.("open");
    })();
    return { current: () => current, close: vi.fn() };
  });
}

function primeOfflineSocket(): void {
  vi.mocked(openPhaseSocket).mockRejectedValue(new Error("unreachable"));
  vi.mocked(withReconnect).mockImplementation((_factory, opts) => {
    void (async () => {
      opts?.onStateChange?.("offline");
    })();
    return { current: () => null, close: vi.fn() };
  });
}

async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

const store = () => useMultiplayerStore.getState();

// ── Fixtures ─────────────────────────────────────────────────────────────
//
// Display names are single capitalised words on purpose: `RAW_KEY_PATH` is
// `/^[a-z][A-Za-z0-9]*(?:\.[A-Za-z0-9]+)+$/`, so a name like "alice.smith"
// would false-positive as an unresolved catalog key.

function seat(
  player_key: string,
  display_name: string,
  dropped = false,
): PlayerSummary {
  return { player_key, display_name, dropped };
}

const H2H: Tiebreaks = {
  HeadToHead: {
    opponents_match_win_pct: 0.5,
    game_win_pct: 0.66,
    opponents_game_win_pct: 0.4,
  },
};

const POD: Tiebreaks = {
  Multiplayer: {
    match_win_pct: 0.25,
    opponents_avg_match_points: 3.5,
    opponents_match_win_pct: 0.5,
  },
};

function standing(
  player_key: string,
  display_name: string,
  dropped = false,
  tiebreaks: Tiebreaks = H2H,
): TournamentStanding {
  return {
    player_key,
    display_name,
    dropped,
    match_points: 3,
    matches_played: 1,
    byes: 0,
    tiebreaks,
  };
}

function summaryFor(
  code: string,
  overrides: Partial<TournamentSummary> = {},
): TournamentSummary {
  return {
    code,
    name: `Event ${code}`,
    arity: 2,
    bracket: "Swiss",
    status: "InProgress",
    player_count: 2,
    current_round: 1,
    total_rounds: 3,
    created_at: 1_700_000_000,
    ...overrides,
  };
}

const ALICE = seat("alice", "Alice");
const BOB = seat("bob", "Bob");
const CARA = seat("cara", "Cara");
const DANA = seat("dana", "Dana");

/** Head-to-head: Alice vs Bob, round 1, pending. */
function h2hView(code = "TOUR01", overrides: Partial<TournamentView> = {}): TournamentView {
  return {
    summary: summaryFor(code),
    players: [ALICE, BOB],
    pairings: [{ id: 1, round: 1, players: [ALICE, BOB], outcome: null }],
    standings: [standing("alice", "Alice"), standing("bob", "Bob")],
    ...overrides,
  };
}

/**
 * The C2 hostile fixture: a 4-seat pod in the CURRENT round with a pending
 * outcome, in which Alice has dropped while three active seats remain.
 *
 * Every clause is load-bearing. `round === current_round` keeps `myPairing`
 * matching (C3 passes), `outcome: null` keeps `isReportable` true (the arm
 * gate passes), and `>= 2` remaining active seats is exactly the shape
 * `drop_player` leaves behind when its one-survivor forfeit guard does not
 * fire — so C2 is the ONLY conjunct that can refuse Alice.
 *
 * `dropped` is set in three places because three different consumers read
 * three different fields: `view.players` (what `isActiveEntrant` reads), the
 * per-seat entry in `pairing.players`, and `TournamentStanding.dropped` (what
 * the standings table's chip reads).
 */
function podView(): TournamentView {
  const droppedAlice = seat("alice", "Alice", true);
  return {
    summary: summaryFor("TOUR01", { arity: 4, player_count: 3, current_round: 1 }),
    players: [droppedAlice, BOB, CARA, DANA],
    pairings: [
      { id: 1, round: 1, players: [droppedAlice, BOB, CARA, DANA], outcome: null },
    ],
    standings: [
      standing("bob", "Bob", false, POD),
      standing("cara", "Cara", false, POD),
      standing("dana", "Dana", false, POD),
      standing("alice", "Alice", true, POD),
    ],
  };
}

const ORGANIZER: TournamentCredential = { organizerToken: "org-token", updatedAt: 1 };
function playerCredential(playerKey: string): TournamentCredential {
  return { playerToken: "player-token", playerKey, updatedAt: 1 };
}

function renderPage(code = "TOUR01") {
  return render(
    <MemoryRouter initialEntries={[`/tournament/${code}`]}>
      <Routes>
        <Route path="/tournament/:code" element={<TournamentPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

/**
 * Mounts the page under a DATA router, so a test can perform a real `:code`
 * navigation. `renderPage`'s `MemoryRouter` cannot: `initialEntries` is read
 * once at mount, and re-rendering it with a different entry changes nothing.
 *
 * The route pattern is `App.tsx`'s own, so `/tournament/TOUR01` →
 * `/tournament/TOUR02` re-matches the SAME route and React keeps the SAME
 * `TournamentPage` instance — which is the scenario under test. A remount
 * would make the stale-failure test below vacuous, because a `setState` from
 * an unmounted component's continuation is a silent no-op and would "pass"
 * with the guard removed.
 */
function renderRoutedPage(code = "TOUR01") {
  const router = createMemoryRouter(
    [{ path: "/tournament/:code", element: <TournamentPage /> }],
    { initialEntries: [`/tournament/${code}`] },
  );
  return { ...render(<RouterProvider router={router} />), router };
}

type TestRouter = ReturnType<typeof createMemoryRouter>;

async function navigateToCode(router: TestRouter, code: string): Promise<void> {
  await act(async () => {
    await router.navigate(`/tournament/${code}`);
  });
  await settle();
}

/** `renderRoutedPage` + the mount seed, settled and answered for `code`. */
async function mountRoutedWith(
  fake: FakeSocket,
  code: string,
): Promise<{ router: TestRouter }> {
  const { router } = renderRoutedPage(code);
  await settle();
  await act(async () => {
    fake.deliver("TournamentUpdate", { code, view: h2hView(code) });
  });
  await settle();
  return { router };
}

/** Mounts the page, settles the subscription, and seeds it with `view`. */
async function mountWith(
  fake: FakeSocket,
  view: TournamentView,
  code = "TOUR01",
): Promise<ReturnType<typeof renderPage>> {
  const result = renderPage(code);
  await settle();
  await act(async () => {
    fake.deliver("TournamentUpdate", { code: view.summary.code, view });
  });
  await settle();
  return result;
}

function yourMatchSection(): HTMLElement {
  return screen
    .getByRole("heading", { name: "Your Match" })
    .closest("section") as HTMLElement;
}

function fullPairingsSection(): HTMLElement {
  return screen
    .getByRole("heading", { name: "Pairings" })
    .closest("section") as HTMLElement;
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(openPhaseSocket).mockReset();
  vi.mocked(withReconnect).mockReset();
  store().closeSubscriptionSocket();
  localStorageMock.items.clear();
  useMultiplayerStore.setState({
    tournamentCredentials: {},
    hostingServer: "ws://localhost:8787",
    displayName: "",
  });
  // happy-dom ships no `window.confirm`, so this is a stub rather than a spy.
  // The destructive controls route through it exactly as `MultiplayerPage`'s
  // do; a test that skipped it would never reach the RPC at all.
  vi.stubGlobal("confirm", vi.fn(() => true));
});

afterEach(() => {
  cleanup();
  store().closeSubscriptionSocket();
  vi.unstubAllGlobals();
});

// ── V10 / V20 — state comes from the broadcast, never from the RPC return ──

describe("TournamentPage gated actions render from the broadcast", () => {
  type ActionCase = {
    label: string;
    frame: string;
    credentials: Record<string, TournamentCredential>;
    view: TournamentView;
    perform: (user: ReturnType<typeof userEvent.setup>) => Promise<void>;
  };

  const cases: ActionCase[] = [
    {
      label: "Start Round",
      frame: "StartTournamentRound",
      credentials: { TOUR01: ORGANIZER },
      view: h2hView(),
      perform: async (user) => {
        await user.click(screen.getByRole("button", { name: "Start Round" }));
      },
    },
    {
      label: "End Tournament",
      frame: "EndTournament",
      credentials: { TOUR01: ORGANIZER },
      view: h2hView(),
      perform: async (user) => {
        await user.click(screen.getByRole("button", { name: "End Tournament" }));
      },
    },
    {
      label: "Drop",
      frame: "DropFromTournament",
      credentials: { TOUR01: playerCredential("alice") },
      view: h2hView(),
      perform: async (user) => {
        await user.click(screen.getByRole("button", { name: "Drop" }));
      },
    },
    {
      label: "Report Result",
      frame: "ReportMatchResult",
      credentials: { TOUR01: playerCredential("alice") },
      view: h2hView(),
      perform: async (user) => {
        await user.click(screen.getByRole("button", { name: "Report Result" }));
        await user.click(within(screen.getByRole("dialog")).getByLabelText("Alice"));
        await user.click(screen.getByRole("button", { name: "Submit Result" }));
      },
    },
  ];

  it.each(cases)(
    "$label renders the broadcast's state and the rejection alert together",
    async ({ frame, credentials, view, perform }) => {
      const user = userEvent.setup();
      const fake = makeFakeSocket();
      primeSocket(fake);
      useMultiplayerStore.setState({ tournamentCredentials: credentials });
      await mountWith(fake, view);

      expect(screen.getByText("Event TOUR01")).toBeTruthy();

      await perform(user);
      await settle();
      // Reach-guard: the request frame really went out, so a silently no-op
      // page cannot satisfy the assertions below vacuously.
      expect(fake.tally(frame)).toBe(1);

      // The correlated `TournamentActionRejected` (Phase B) settles the gated
      // RPC `{ok:false}`; a bare `Error` no longer settles a correlated action.
      const requestId = (fake.frame(frame)?.data as { request_id: number })
        .request_id;
      await act(async () => {
        fake.deliver("TournamentActionRejected", {
          request_id: requestId,
          message: "broker said no",
        });
      });
      await settle();
      expect(screen.getByRole("alert").textContent).toBe(
        "The server rejected that: broker said no",
      );
      // Negative (a): with no broadcast, the rendered state is unchanged.
      expect(screen.getByText("Event TOUR01")).toBeTruthy();

      // ...and the broadcast still arrives afterwards, carrying the new state.
      // A page that rendered from the RPC return could never show this.
      await act(async () => {
        fake.deliver("TournamentUpdate", {
          code: "TOUR01",
          view: { ...view, summary: { ...view.summary, name: "Renamed Event" } },
        });
      });
      await settle();
      expect(screen.getByText("Renamed Event")).toBeTruthy();
      expect(screen.getByRole("alert").textContent).toBe(
        "The server rejected that: broker said no",
      );
    },
  );

  // Negative (b): the ambient broadcast is the render source on its own, with
  // no RPC in flight at all.
  it("updates from a broadcast with no request outstanding", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, h2hView());
    expect(screen.getByText("Event TOUR01")).toBeTruthy();

    await act(async () => {
      fake.deliver("TournamentUpdate", {
        code: "TOUR01",
        view: {
          ...h2hView(),
          summary: { ...summaryFor("TOUR01"), name: "Quiet Update" },
        },
      });
    });
    expect(screen.getByText("Quiet Update")).toBeTruthy();
  });
});

// ── V11 — no page ever writes a view from an RPC result ──────────────────

describe("TournamentPage RPC-result provenance", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const SET_FROM_RESULT = /set(?:View|Tournaments)\s*\(\s*[a-z]\w*\.value/;

  it("has a detector that would catch the forbidden write", () => {
    // Positive control: a regex matching nothing could not fail below.
    expect(SET_FROM_RESULT.test("setView(result.value.view)")).toBe(true);
    expect(SET_FROM_RESULT.test("setTournaments(r.value.list)")).toBe(true);
  });

  it.each(["TournamentPage.tsx", "TournamentLandingPage.tsx"])(
    "%s never writes page state from an RPC return value",
    (file) => {
      const source = readFileSync(resolve(here, "..", file), "utf8");
      expect(SET_FROM_RESULT.test(source)).toBe(false);
    },
  );
});

// ── V12 — the detail view re-seeds on a list push (reconnect recovery) ────

describe("TournamentPage re-seeding", () => {
  it("re-issues GetTournament on every list update", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, h2hView());

    // Reach-guard: the mount's own seed really happened.
    expect(fake.tally("GetTournament")).toBe(1);
    fake.clearSent();
    expect(fake.tally("GetTournament")).toBe(0);

    await act(async () => {
      fake.deliver("TournamentListUpdate", { tournaments: [] });
    });
    await settle();
    expect(fake.tally("GetTournament")).toBe(1);
  });

  it("does not re-seed on a plain TournamentUpdate", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, h2hView());
    fake.clearSent();

    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR01", view: h2hView() });
    });
    await settle();
    // Otherwise this would be a refetch-on-everything loop.
    expect(fake.tally("GetTournament")).toBe(0);
  });
});

// ── V6 — a foreign-code broadcast cannot touch this page ─────────────────

describe("TournamentPage broadcast scoping", () => {
  it("ignores a TournamentUpdate for another code", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, h2hView());

    await act(async () => {
      fake.deliver("TournamentUpdate", {
        code: "OTHER",
        view: {
          ...h2hView("OTHER"),
          summary: { ...summaryFor("OTHER"), name: "Someone Else's Event" },
        },
      });
    });
    expect(screen.queryByText("Someone Else's Event")).toBeNull();
    expect(screen.getByText("Event TOUR01")).toBeTruthy();
  });

  it("accepts a TournamentUpdate for its own code", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, h2hView());

    await act(async () => {
      fake.deliver("TournamentUpdate", {
        code: "TOUR01",
        view: {
          ...h2hView(),
          summary: { ...summaryFor("TOUR01"), name: "My Renamed Event" },
        },
      });
    });
    expect(screen.getByText("My Renamed Event")).toBeTruthy();
  });

  it("renders errors.notFound when this tournament is removed", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, h2hView());

    await act(async () => {
      fake.deliver("TournamentRemoved", { code: "OTHER" });
    });
    expect(screen.queryByText("No tournament with that code.")).toBeNull();

    await act(async () => {
      fake.deliver("TournamentRemoved", { code: "TOUR01" });
    });
    expect(screen.getByText("No tournament with that code.")).toBeTruthy();
  });
});

// ── V4 / V4b — organizer gating, and the §0.2 player-authority correction ──

describe("TournamentPage organizer gating", () => {
  it("hides organizer controls when the credential is for another tournament", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({ tournamentCredentials: { TOURA: ORGANIZER } });
    await mountWith(fake, h2hView("TOURB"), "TOURB");

    expect(screen.queryByText("Start Round")).toBeNull();
    expect(screen.queryByText("End Tournament")).toBeNull();
    // Reach-guard: the page really rendered this tournament.
    expect(screen.getByText("Event TOURB")).toBeTruthy();
  });

  it("shows organizer controls when the credential is for this tournament", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({ tournamentCredentials: { TOURB: ORGANIZER } });
    await mountWith(fake, h2hView("TOURB"), "TOURB");

    expect(screen.getByText("Start Round")).toBeTruthy();
    expect(screen.getByText("End Tournament")).toBeTruthy();
  });

  // V4b — reporting is PLAYER authority, not organizer authority. An organizer
  // token authorizes a round start and refuses a report every time, so an
  // organizer-gated Report button would be dead UI.
  it("offers no Report affordance to an organizer-only credential", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({ tournamentCredentials: { TOUR01: ORGANIZER } });
    await mountWith(fake, h2hView());

    expect(screen.queryAllByText("Report Result")).toHaveLength(0);
    // ...while the organizer's own controls DO render, so this is not a page
    // that simply failed to render its controls.
    expect(screen.getByText("Start Round")).toBeTruthy();
    expect(screen.getByText("You have no match this round.")).toBeTruthy();
  });

  it("offers exactly one Report affordance to a seated player, inside Your Match", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: playerCredential("alice") },
    });
    await mountWith(fake, h2hView());

    // Page-wide count: a stray `onReport` on the full-history list would make
    // this 2, which is exactly what RC5 mutates.
    expect(screen.getAllByText("Report Result")).toHaveLength(1);
    expect(within(yourMatchSection()).getAllByText("Report Result")).toHaveLength(1);
    expect(within(fullPairingsSection()).queryAllByText("Report Result")).toHaveLength(0);
  });

  it("also gates the organizer controls on the organizer role alone", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: playerCredential("alice") },
    });
    await mountWith(fake, h2hView());

    expect(screen.queryByText("Start Round")).toBeNull();
    expect(screen.getAllByText("Report Result")).toHaveLength(1);
  });
});

// ── V22b — the C2 (dropped) conjunct as rendered UI ──────────────────────

describe("TournamentPage dropped-entrant gating", () => {
  it("offers a dropped entrant neither player affordance", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: playerCredential("alice") },
    });
    const { container } = await mountWith(fake, podView());

    expect(screen.queryAllByText("Report Result")).toHaveLength(0);
    expect(screen.queryByText("Drop")).toBeNull();

    // C1 and C3 both PASSED — the page is showing Alice her pairing while
    // withholding the action, which is what proves C2 is the refusing
    // conjunct rather than a token or seat mismatch.
    expect(within(yourMatchSection()).getAllByText("Alice")).not.toHaveLength(0);
    expect(screen.getByText("Dropped")).toBeTruthy();
    expectCatalogValuePresent(container, "Your Match");
  });

  it("offers an active entrant in the SAME fixture exactly one of each", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: playerCredential("bob") },
    });
    const { container } = await mountWith(fake, podView());

    // The paired positive: a page that rendered neither button for anybody,
    // or failed to render at all, satisfies Alice's case trivially.
    expect(screen.getAllByText("Report Result")).toHaveLength(1);
    expect(screen.getAllByText("Drop")).toHaveLength(1);
    expectCatalogValuePresent(container, "Your Match");
  });
});

// ── V5 — only the viewer's own pairing is offered ────────────────────────

describe("TournamentPage Your Match narrowing", () => {
  function twoPairingView(): TournamentView {
    return {
      summary: summaryFor("TOUR01", { player_count: 4 }),
      players: [ALICE, BOB, CARA, DANA],
      pairings: [
        { id: 1, round: 1, players: [BOB, CARA], outcome: null },
        { id: 2, round: 1, players: [ALICE, DANA], outcome: null },
      ] satisfies TournamentPairingView[],
      standings: [
        standing("alice", "Alice"),
        standing("bob", "Bob"),
        standing("cara", "Cara"),
        standing("dana", "Dana"),
      ],
    };
  }

  it("narrows Your Match to the viewer's pairing while the full list keeps both", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: playerCredential("alice") },
    });
    await mountWith(fake, twoPairingView());

    const mine = within(yourMatchSection());
    expect(mine.getByText("Table 2")).toBeTruthy();
    expect(mine.queryByText("Table 1")).toBeNull();
    expect(mine.getByText("Alice")).toBeTruthy();

    // Reach-guard: the narrowing is the filter's, not a broken view.
    const all = within(fullPairingsSection());
    expect(all.getByText("Table 1")).toBeTruthy();
    expect(all.getByText("Table 2")).toBeTruthy();
  });

  it("tells a viewer holding no player key that they have no match", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, twoPairingView());

    expect(screen.getByText("You have no match this round.")).toBeTruthy();
    expect(screen.queryAllByText("Report Result")).toHaveLength(0);
    // The same reach-guard: the full list still renders both pairings.
    const all = within(fullPairingsSection());
    expect(all.getByText("Table 1")).toBeTruthy();
    expect(all.getByText("Table 2")).toBeTruthy();
  });
});

// ── V14 — the dialog submits the pairing it is SHOWING ───────────────────

describe("TournamentPage report dialog freshness", () => {
  it("re-derives the dialog's pairing from the live view", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: playerCredential("alice") },
    });
    const stale: TournamentView = {
      summary: summaryFor("TOUR01", { arity: 4, player_count: 3 }),
      players: [ALICE, BOB, CARA],
      pairings: [{ id: 1, round: 1, players: [ALICE, BOB, CARA], outcome: null }],
      standings: [standing("alice", "Alice", false, POD)],
    };
    await mountWith(fake, stale);

    await user.click(
      within(yourMatchSection()).getByRole("button", { name: "Report Result" }),
    );
    expect(within(screen.getByRole("dialog")).getByLabelText("Cara")).toBeTruthy();

    // A broadcast arrives while the dialog is open, carrying a CHANGED seat
    // list for the same pairing id.
    const fresh: TournamentView = {
      ...stale,
      players: [ALICE, BOB, DANA],
      pairings: [{ id: 1, round: 1, players: [ALICE, BOB, DANA], outcome: null }],
    };
    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR01", view: fresh });
    });
    await settle();

    const dialog = within(screen.getByRole("dialog"));
    // The departed seat is gone and the new one is selectable — only possible
    // if the dialog is being handed the LIVE pairing, not the latched one.
    expect(dialog.queryByLabelText("Cara")).toBeNull();
    await user.click(dialog.getByLabelText("Dana"));
    await user.click(screen.getByRole("button", { name: "Submit Result" }));
    await settle();

    expect(fake.tally("ReportMatchResult")).toBe(1);
    const sent = fake.frame("ReportMatchResult")?.data as {
      pairing_id: number;
      outcome: { Decisive: { winner: string } };
    };
    expect(sent.pairing_id).toBe(1);
    expect(sent.outcome.Decisive.winner).toBe("dana");
    expect(fresh.pairings[0].players.map((p) => p.player_key)).toContain(
      sent.outcome.Decisive.winner,
    );
  });
});

// ── V15 — unmount during an in-flight connect (#4615) ────────────────────

describe("TournamentPage subscription lifecycle", () => {
  it("detaches when the page unmounts before the connect resolves", async () => {
    const fake = makeFakeSocket();
    let resolveOpen: (socket: unknown) => void = () => {};
    primeSocket(
      fake,
      new Promise((resolve) => {
        resolveOpen = resolve;
      }),
    );

    const { unmount } = renderPage();
    await settle();
    expect(fake.listenerCount("message")).toBe(0);

    unmount();
    await act(async () => {
      resolveOpen(fake.socket);
    });
    await settle();

    expect(fake.listenerCount("message")).toBe(0);
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
  });

  it("keeps the subscription attached while mounted", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, h2hView());

    expect(fake.listenerCount("message")).toBeGreaterThanOrEqual(1);
    expect(fake.tally("UnsubscribeLobby")).toBe(0);
    expect(screen.getByText("Event TOUR01")).toBeTruthy();
  });

  it("renders the offline copy when the subscription cannot be opened", async () => {
    primeOfflineSocket();
    renderPage();
    await settle();

    expect(
      screen.getByText("Lost connection to the lobby. Check your server address."),
    ).toBeTruthy();
  });
});

// ── V13 — no raw key path leaks into a rendered text node ────────────────

describe("TournamentPage catalog completeness", () => {
  /**
   * Deliberately rich: `InProgress`, four standings rows, a bye, a
   * head-to-head, a 4-seat pod, and all four `PairingOutcome` arms plus a
   * pending `null`, with one dropped entrant.
   */
  function richView(): TournamentView {
    const droppedDana = seat("dana", "Dana", true);
    return {
      summary: summaryFor("TOUR01", {
        arity: 4,
        player_count: 3,
        current_round: 3,
        bracket: "SingleElimination",
      }),
      players: [ALICE, BOB, CARA, droppedDana],
      pairings: [
        { id: 1, round: 1, players: [ALICE], outcome: "Bye" },
        {
          id: 2,
          round: 1,
          players: [BOB, CARA],
          outcome: { Reported: { Decisive: { winner: "bob", game_wins: { bob: 2, cara: 1 } } } },
        },
        { id: 3, round: 2, players: [ALICE, BOB], outcome: { Reported: "Draw" } },
        { id: 4, round: 2, players: [CARA, droppedDana], outcome: { Forfeit: { winner: "cara" } } },
        { id: 5, round: 3, players: [ALICE, BOB, CARA, droppedDana], outcome: null },
      ],
      standings: [
        standing("bob", "Bob", false, POD),
        standing("alice", "Alice", false, POD),
        standing("cara", "Cara", false, H2H),
        standing("dana", "Dana", true, POD),
      ],
    };
  }

  it("renders no unresolved key paths for a viewer holding both credentials", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        TOUR01: {
          organizerToken: "org-token",
          playerToken: "player-token",
          playerKey: "alice",
          updatedAt: 1,
        },
      },
    });
    const { container } = await mountWith(fake, richView());

    // The playing organizer keeps BOTH control blocks.
    expect(screen.getByText("Start Round")).toBeTruthy();
    expect(screen.getByText("Drop")).toBeTruthy();

    // Open the dialog too, so its copy is in the same sweep.
    await user.click(
      within(yourMatchSection()).getByRole("button", { name: "Report Result" }),
    );

    expectNoRawKeyPaths(container);
    expectNoRawKeyPaths(document.body);
    expectCatalogValuePresent(container, "Standings");
    expectCatalogValuePresent(container, "Your Match");
    // The display precedence is organizer-dominant even holding both tokens.
    expectCatalogValuePresent(container, "Organizer");
  });

  it("renders no unresolved key paths for a viewer holding no credential", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    const { container } = await mountWith(fake, richView());

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "Spectating");
    expectCatalogValuePresent(container, "Standings");
  });

  it("renders standings in the broker's array order, never re-sorted", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    await mountWith(fake, richView());

    const rows = within(
      screen.getByRole("heading", { name: "Standings" }).closest("section") as HTMLElement,
    ).getAllByRole("row");
    // Header row first, then the four standings rows in the order the broker
    // gave them — which is deliberately NOT the match-points order, so any
    // client-side sort would reshuffle this. Dana's cell also carries her
    // `labels.dropped` chip, hence the prefix match rather than equality.
    const bodyRows = rows.slice(1);
    expect(bodyRows).toHaveLength(4);
    ["Bob", "Alice", "Cara", "Dana"].forEach((name, index) => {
      const cell = within(bodyRows[index]).getAllByRole("cell")[1];
      expect(cell.textContent).toMatch(new RegExp(`^${name}`));
    });
  });
});

// ── One action in flight holds EVERY control ─────────────────────────────
//
// A different axis from the `:code` block below. Those tests are about
// cross-TOURNAMENT staleness (one page, two codes); these are about
// cross-KIND interference (one page, one code, two different action types).
// `busy` is a single slot, so a control gated on `busy === "<its own kind>"`
// is re-enabled the instant a DIFFERENT action claims the slot — with its own
// request still unanswered.

describe("TournamentPage concurrent action gating", () => {
  it("holds End Tournament while Start Round is in flight, and releases both after", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({ tournamentCredentials: { TOUR01: ORGANIZER } });
    await mountWith(fake, h2hView());

    const start = () =>
      screen.getByRole("button", { name: /^(Start Round|Starting…)$/ }) as HTMLButtonElement;
    const end = () =>
      screen.getByRole("button", {
        name: /^(End Tournament|Ending…)$/,
      }) as HTMLButtonElement;

    // Reach-guard: both controls are live before anything is dispatched, so
    // the disabled assertions below cannot pass against a page that simply
    // renders every button disabled.
    expect(start().disabled).toBe(false);
    expect(end().disabled).toBe(false);

    await user.click(start());
    await settle();
    // Reach-guard: Start's request really is in flight and unanswered.
    expect(fake.tally("StartTournamentRound")).toBe(1);

    // The label stays kind-specific — only Start says it is running...
    expect(start().textContent).toBe("Starting…");
    expect(end().textContent).toBe("End Tournament");
    // ...while the disabled state is page-wide.
    expect(start().disabled).toBe(true);
    expect(end().disabled).toBe(true);

    // The actual exploit: clicking End here used to dispatch a second frame
    // AND, via `setBusy("end")`, re-enable Start's own control mid-flight.
    await user.click(end());
    await settle();
    expect(fake.tally("EndTournament")).toBe(0);
    expect(start().textContent).toBe("Starting…");
    expect(start().disabled).toBe(true);

    // Paired positive: once Start's OWN response settles (its correlated
    // `TournamentActionAck`), both controls come back. Without this, a page
    // that could never re-enable anything would satisfy every assertion above.
    await act(async () => {
      fake.deliver("TournamentActionAck", {
        request_id: (fake.frame("StartTournamentRound")?.data as {
          request_id: number;
        }).request_id,
        code: "TOUR01",
        view: h2hView(),
      });
    });
    await settle();
    expect(start().disabled).toBe(false);
    expect(end().disabled).toBe(false);
    expect(start().textContent).toBe("Start Round");

    // ...and End really is dispatchable again, which is what proves the hold
    // was the guard rather than a control that had died.
    await user.click(end());
    await settle();
    expect(fake.tally("EndTournament")).toBe(1);
  });

  it("holds the organizer controls and the report dialog while a Drop is in flight", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    // A playing organizer, so all four controls this page owns are on screen
    // at once: Start, End, Drop, and the report dialog's submit.
    useMultiplayerStore.setState({
      tournamentCredentials: {
        TOUR01: {
          organizerToken: "org-token",
          playerToken: "player-token",
          playerKey: "alice",
          updatedAt: 1,
        },
      },
    });
    await mountWith(fake, h2hView());

    const drop = () =>
      screen.getByRole("button", { name: /^(Drop|Dropping…)$/ }) as HTMLButtonElement;
    expect(drop().disabled).toBe(false);

    await user.click(drop());
    await settle();
    expect(fake.tally("DropFromTournament")).toBe(1);
    expect(drop().textContent).toBe("Dropping…");

    // Both organizer controls are held by an action neither of them owns.
    expect(
      (screen.getByRole("button", { name: "Start Round" }) as HTMLButtonElement).disabled,
    ).toBe(true);
    expect(
      (screen.getByRole("button", { name: "End Tournament" }) as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    // The dialog is reachable while the drop is in flight (opening it is not
    // itself a dispatch), and its submit control is held too — labelled for
    // the OTHER action, which is the accepted cost of `submitting` being one
    // boolean driving both the disabled state and the label.
    await user.click(
      within(yourMatchSection()).getByRole("button", { name: "Report Result" }),
    );
    const dialog = within(screen.getByRole("dialog"));
    // A winner is selected first: without one the submit control is disabled
    // for an unrelated reason (`selection === null`) and the assertion would
    // hold with or without this fix.
    await user.click(dialog.getByLabelText("Alice"));
    const submit = dialog.getByRole("button", {
      name: "Submitting…",
    }) as HTMLButtonElement;
    expect(submit.disabled).toBe(true);

    await user.click(submit);
    await settle();
    expect(fake.tally("ReportMatchResult")).toBe(0);

    // Paired positive: the drop settles (its correlated `TournamentActionAck`),
    // and the same dialog becomes submittable — report frame going out for real.
    await act(async () => {
      fake.deliver("TournamentActionAck", {
        request_id: (fake.frame("DropFromTournament")?.data as {
          request_id: number;
        }).request_id,
        code: "TOUR01",
        view: h2hView(),
      });
    });
    await settle();
    const released = within(screen.getByRole("dialog")).getByRole("button", {
      name: "Submit Result",
    }) as HTMLButtonElement;
    expect(released.disabled).toBe(false);
    await user.click(released);
    await settle();
    expect(fake.tally("ReportMatchResult")).toBe(1);
  });
});

// ── A `:code` change re-scopes the whole page ────────────────────────────
//
// The route param is the page's ONE identity binding, and nothing else in
// this file exercises a change to it. All three tests below navigate for
// real, within one mounted `TournamentPage` instance.

describe("TournamentPage :code navigation", () => {
  it("re-seeds for the new code and clears the previous tournament first", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    const { router } = await mountRoutedWith(fake, "TOUR01");

    // Reach-guards: the first tournament really rendered, from its own seed.
    expect(screen.getByText("Event TOUR01")).toBeTruthy();
    expect(fake.tally("GetTournament")).toBe(1);

    // Put a list push on the wire BEFORE navigating, so the store is holding a
    // `tournamentListSnapshot` when the subscription is released. Its own
    // re-seed (tally 1 → 2) is the reach-guard that the snapshot is really
    // cached: `subscribeTournaments` fans a cached snapshot straight into a
    // newly attached handlers object, so if the release did not drop it, the
    // re-acquire below would fire a SECOND seed for the new code.
    await act(async () => {
      fake.deliver("TournamentListUpdate", { tournaments: [] });
    });
    await settle();
    expect(fake.tally("GetTournament")).toBe(2);
    fake.clearSent();

    await navigateToCode(router, "TOUR02");

    // (b) The previous tournament is off the screen BEFORE the new one loads —
    // this is the effect's `setView(null)` reset, not a slow re-render.
    expect(screen.queryByText("Event TOUR01")).toBeNull();
    expect(screen.getByText("Loading tournament…")).toBeTruthy();
    expect(screen.getByText("Tournament TOUR02")).toBeTruthy();

    // (a) ...and a fresh `GetTournament` went out, for the NEW code.
    //
    // What this pins is that the subscription effect RE-RUNS on a `:code`
    // change — measured, not assumed: with the effect's dependency array cut
    // to `[subscribeTournaments]` all three tests in this block redden. It is
    // deliberately not phrased as "`code` is in the deps", because two deps
    // carry that today (`code` itself, and `seed`, which closes over `code`),
    // so removing either one alone leaves the effect re-running and every
    // assertion here green.
    //
    // EXACTLY one is the second half of the assertion: it is also what proves
    // the release dropped its cached list snapshot, since a re-fanned snapshot
    // would have added a second `GetTournament` for TOUR02 here.
    expect(fake.tally("GetTournament")).toBe(1);
    expect((fake.frame("GetTournament")?.data as { code: string }).code).toBe(
      "TOUR02",
    );

    // The re-subscribe the effect's comment describes, measured rather than
    // asserted: with this page as the sole subscriber the shared refcount does
    // reach 0 (one `UnsubscribeLobby` from the old cleanup) and is then
    // re-acquired (one fresh `SubscribeLobby`) — the mount's own subscribe is
    // excluded by the `clearSent()` above.
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
    expect(fake.tally("SubscribeLobby")).toBe(1);

    // And the new code's broadcast lands on the re-attached handlers, which is
    // what makes that transient release harmless.
    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR02", view: h2hView("TOUR02") });
    });
    await settle();
    expect(screen.getByText("Event TOUR02")).toBeTruthy();
  });

  it("drops an in-flight failure for the tournament the viewer left", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: ORGANIZER, TOUR02: ORGANIZER },
    });
    const { router } = await mountRoutedWith(fake, "TOUR01");

    await user.click(screen.getByRole("button", { name: "End Tournament" }));
    await settle();
    // Reach-guard: TOUR01's request really is in flight, so the rejection
    // frame below has something of TOUR01's to settle.
    expect(fake.tally("EndTournament")).toBe(1);

    await navigateToCode(router, "TOUR02");
    // Render TOUR02 from its broadcast so the page below is showing TOUR02.
    // (TOUR01's rejection is now correlated to TOUR01's own request_id, so it
    // cannot bleed onto TOUR02's in-flight seed — see `tournamentClient.ts`.)
    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR02", view: h2hView("TOUR02") });
    });
    await settle();
    expect(screen.getByText("Event TOUR02")).toBeTruthy();

    // TOUR01's correlated rejection arrives now, against a page showing TOUR02.
    await act(async () => {
      fake.deliver("TournamentActionRejected", {
        request_id: (fake.frame("EndTournament", 0)?.data as { request_id: number })
          .request_id,
        message: "TOUR01 said no",
      });
    });
    await settle();

    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText(/TOUR01 said no/)).toBeNull();

    // Paired positive control, same page and same frame type: TOUR02's own
    // rejection DOES render. Without it, a page that had simply lost its alert
    // region would satisfy the two assertions above.
    await user.click(screen.getByRole("button", { name: "End Tournament" }));
    await settle();
    expect(fake.tally("EndTournament")).toBe(2);
    await act(async () => {
      fake.deliver("TournamentActionRejected", {
        request_id: (fake.frame("EndTournament", 1)?.data as { request_id: number })
          .request_id,
        message: "TOUR02 said no",
      });
    });
    await settle();
    expect(screen.getByRole("alert").textContent).toBe(
      "The server rejected that: TOUR02 said no",
    );
  });

  // The same scoping on the OTHER writer. `seed` is the second of the two
  // `setFailure` call sites, and it settles on a different frame from `run`'s.
  it("drops an in-flight seed failure for the tournament the viewer left", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    const { router } = renderRoutedPage("TOUR01");
    await settle();
    // TOUR01's mount seed is deliberately left UNANSWERED, so it is still in
    // flight across the navigation below.
    expect(fake.tally("GetTournament")).toBe(1);

    await navigateToCode(router, "TOUR02");
    // TOUR02's own seed is answered first, for the same reason as above: the
    // `Error` frame settles everything still in flight.
    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR02", view: h2hView("TOUR02") });
    });
    await settle();
    expect(screen.getByText("Event TOUR02")).toBeTruthy();

    await act(async () => {
      fake.deliver("Error", { message: "TOUR01 seed said no" });
    });
    await settle();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText(/TOUR01 seed said no/)).toBeNull();

    // Paired positive control on the seed path itself: a list push re-seeds
    // TOUR02, and THAT failure renders.
    await act(async () => {
      fake.deliver("TournamentListUpdate", { tournaments: [] });
    });
    await settle();
    expect(fake.tally("GetTournament")).toBe(3);
    await act(async () => {
      fake.deliver("Error", { message: "TOUR02 seed said no" });
    });
    await settle();
    expect(screen.getByRole("alert").textContent).toBe(
      "The server rejected that: TOUR02 seed said no",
    );
  });

  // The remaining three continuations, one test each. Every one of them leaves
  // TOUR01's request UNSETTLED across the navigation, which is only possible
  // because each gated helper filters its reply on `code`
  // (`tournamentClient.ts`, `matchReply`): a `TournamentUpdate` for TOUR02
  // cannot settle a TOUR01 request, and vice versa. That is what lets a stale
  // settlement be delivered on its own, with nothing of the successor's in the
  // same frame.

  it("clears a busy control left in flight by the tournament the viewer left", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: ORGANIZER, TOUR02: ORGANIZER },
    });
    const { router } = await mountRoutedWith(fake, "TOUR01");

    await user.click(screen.getByRole("button", { name: "End Tournament" }));
    await settle();
    // Two reach-guards: TOUR01's request really is in flight, and the control
    // really is holding the busy label. Without the second, the assertion after
    // the navigation would also pass against a page where `busy` was never set.
    expect(fake.tally("EndTournament")).toBe(1);
    expect(screen.getByRole("button", { name: "Ending…" })).toBeTruthy();

    await navigateToCode(router, "TOUR02");
    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR02", view: h2hView("TOUR02") });
    });
    await settle();
    expect(screen.getByText("Event TOUR02")).toBeTruthy();

    // TOUR02 dispatched nothing, so nothing of TOUR02's may be held. TOUR01's
    // `EndTournament` is still in flight at this point — the clear under test
    // is the subscription effect's reset, not a settlement.
    expect(fake.tally("EndTournament")).toBe(1);
    const end = screen.getByRole("button", { name: "End Tournament" });
    expect((end as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByRole("button", { name: "Ending…" })).toBeNull();
  });

  it("keeps the current tournament's busy control held when a stale action settles", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: { TOUR01: ORGANIZER, TOUR02: ORGANIZER },
    });
    const { router } = await mountRoutedWith(fake, "TOUR01");

    await user.click(screen.getByRole("button", { name: "End Tournament" }));
    await settle();
    expect(fake.tally("EndTournament")).toBe(1);

    await navigateToCode(router, "TOUR02");
    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR02", view: h2hView("TOUR02") });
    });
    await settle();

    // TOUR02 dispatches its OWN action, so `busy` now belongs to TOUR02 and
    // its control is deliberately held disabled against a second dispatch.
    await user.click(screen.getByRole("button", { name: "End Tournament" }));
    await settle();
    expect(fake.tally("EndTournament")).toBe(2);
    expect((fake.frame("EndTournament", 1)?.data as { code: string }).code).toBe(
      "TOUR02",
    );
    expect(screen.getByRole("button", { name: "Ending…" })).toBeTruthy();

    // TOUR01's stale action settles now. Correlated to TOUR01's own End
    // request, so it answers that request and leaves TOUR02's in flight — the
    // assertions below pin that a settled TOUR02 request would make vacuous.
    await act(async () => {
      fake.deliver("TournamentActionAck", {
        request_id: (fake.frame("EndTournament", 0)?.data as { request_id: number })
          .request_id,
        code: "TOUR01",
        view: h2hView("TOUR01"),
      });
    });
    await settle();

    expect(screen.getByRole("button", { name: "Ending…" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "End Tournament" })).toBeNull();

    // Paired positive control: TOUR02's OWN settlement does release the
    // control. Without it, a page that could never re-enable the button at all
    // would satisfy the two assertions above.
    await act(async () => {
      fake.deliver("TournamentActionAck", {
        request_id: (fake.frame("EndTournament", 1)?.data as { request_id: number })
          .request_id,
        code: "TOUR02",
        view: h2hView("TOUR02"),
      });
    });
    await settle();
    expect(screen.getByRole("button", { name: "End Tournament" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Ending…" })).toBeNull();
  });

  it("keeps the successor tournament's open report dialog when a stale report settles", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        TOUR01: playerCredential("alice"),
        TOUR02: playerCredential("alice"),
      },
    });
    const { router } = await mountRoutedWith(fake, "TOUR01");

    // A report is submitted on TOUR01 and left in flight.
    await user.click(
      within(yourMatchSection()).getByRole("button", { name: "Report Result" }),
    );
    await user.click(within(screen.getByRole("dialog")).getByLabelText("Alice"));
    await user.click(screen.getByRole("button", { name: "Submit Result" }));
    await settle();
    expect(fake.tally("ReportMatchResult")).toBe(1);

    await navigateToCode(router, "TOUR02");
    // A distinct pairing id, so the dialog opened below is unambiguously
    // TOUR02's own rather than a latched TOUR01 pairing that happens to match.
    const tour02 = h2hView("TOUR02", {
      pairings: [{ id: 2, round: 1, players: [ALICE, BOB], outcome: null }],
    });
    await act(async () => {
      fake.deliver("TournamentUpdate", { code: "TOUR02", view: tour02 });
    });
    await settle();
    // Reach-guard: the navigation already dismissed TOUR01's dialog, via the
    // subscription effect's `setReporting(null)`.
    expect(screen.queryByRole("dialog")).toBeNull();

    // The viewer opens TOUR02's dialog and enters a selection into it.
    await user.click(
      within(yourMatchSection()).getByRole("button", { name: "Report Result" }),
    );
    await user.click(within(screen.getByRole("dialog")).getByLabelText("Bob"));
    expect(
      (within(screen.getByRole("dialog")).getByLabelText("Bob") as HTMLInputElement)
        .checked,
    ).toBe(true);

    // TOUR01's stale report settles SUCCESSFULLY now — the `ok` branch (its
    // correlated `TournamentActionAck`) is the one that clears `reporting`, so
    // an unscoped clear would fire here.
    await act(async () => {
      fake.deliver("TournamentActionAck", {
        request_id: (fake.frame("ReportMatchResult", 0)?.data as {
          request_id: number;
        }).request_id,
        code: "TOUR01",
        view: h2hView("TOUR01"),
      });
    });
    await settle();

    // TOUR02's dialog is untouched: still open, and the selection survives.
    const dialog = within(screen.getByRole("dialog"));
    expect((dialog.getByLabelText("Bob") as HTMLInputElement).checked).toBe(true);

    // Paired positive control: TOUR02's OWN report does close TOUR02's dialog,
    // so the assertions above are not measuring a dialog that can never close.
    // The submitted frame is also what proves the surviving dialog was TOUR02's
    // own — pairing 2, TOUR02 — and not a latched TOUR01 one.
    await user.click(screen.getByRole("button", { name: "Submit Result" }));
    await settle();
    expect(fake.tally("ReportMatchResult")).toBe(2);
    const sent = fake.frame("ReportMatchResult", 1)?.data as {
      code: string;
      pairing_id: number;
      outcome: { Decisive: { winner: string } };
    };
    expect(sent.code).toBe("TOUR02");
    expect(sent.pairing_id).toBe(2);
    expect(sent.outcome.Decisive.winner).toBe("bob");
    await act(async () => {
      fake.deliver("TournamentActionAck", {
        request_id: (fake.frame("ReportMatchResult", 1)?.data as {
          request_id: number;
        }).request_id,
        code: "TOUR02",
        view: tour02,
      });
    });
    await settle();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
