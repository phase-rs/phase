import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router";
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
    serverAddress: "ws://localhost:8787",
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

      // The `Error` frame settles the RPC `{ok:false}`.
      await act(async () => {
        fake.deliver("Error", { message: "broker said no" });
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
