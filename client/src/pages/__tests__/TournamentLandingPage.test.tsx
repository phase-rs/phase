import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The store's persist middleware touches `localStorage` at import time, and
// this worktree's Node build carries the standing `--localstorage-file` issue.
// Copied from `stores/__tests__/multiplayerStore.tournament.test.ts:3-25`.
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

// `ScreenChrome` reaches `ChromeControls -> AccountControl`, unrelated to
// anything under test here.
vi.mock("../../components/chrome/ScreenChrome", () => ({
  ScreenChrome: () => null,
}));

// ONLY the transport seam is faked. `multiplayerStore`, `tournamentClient` and
// `brokerClient` are deliberately real: mocking any of them would make every
// frame assertion below vacuous.
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

import type { TournamentSummary, TournamentView } from "../../adapter/types";
import { expectCatalogValuePresent, expectNoRawKeyPaths } from "../../components/tournament/__tests__/tournamentTestUtils";
import { openPhaseSocket, withReconnect } from "../../services/openPhaseSocket";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { TournamentLandingPage } from "../TournamentLandingPage";

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
    /** Exact parsed-tag equality — never a regex (`UnsubscribeLobby`'s
     *  lowercase `s` defeats the obvious pattern). */
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

let driver: { fire: (state: "open" | "reconnecting" | "offline") => void } | null =
  null;

/** Wires the transport so `ensureSubscriptionSocket` resolves `fake`. */
function primeSocket(fake: FakeSocket, openWith?: Promise<unknown>): void {
  vi.mocked(openPhaseSocket).mockImplementation(
    () =>
      (openWith ?? Promise.resolve(fake.socket)) as ReturnType<
        typeof openPhaseSocket
      >,
  );
  vi.mocked(withReconnect).mockImplementation((factory, opts) => {
    let current: Awaited<ReturnType<typeof factory>> | null = null;
    void (async () => {
      current = await factory(0);
      opts?.onStateChange?.("open");
    })();
    driver = { fire: (state) => opts?.onStateChange?.(state) };
    return { current: () => current, close: vi.fn() };
  });
}

/** Wires the transport so the reconnect handle goes straight to `offline`. */
function primeOfflineSocket(): void {
  vi.mocked(openPhaseSocket).mockRejectedValue(new Error("unreachable"));
  vi.mocked(withReconnect).mockImplementation((_factory, opts) => {
    void (async () => {
      opts?.onStateChange?.("offline");
    })();
    driver = { fire: (state) => opts?.onStateChange?.(state) };
    return { current: () => null, close: vi.fn() };
  });
}

/** Lets the store's async continuations settle inside `act`. */
async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

const store = () => useMultiplayerStore.getState();

function summaryFor(
  code: string,
  overrides: Partial<TournamentSummary> = {},
): TournamentSummary {
  return {
    code,
    name: `Event ${code}`,
    arity: 2,
    bracket: "Swiss",
    status: "Registration",
    player_count: 2,
    current_round: 0,
    total_rounds: 3,
    created_at: 1_700_000_000,
    ...overrides,
  };
}

function viewFor(code: string): TournamentView {
  return { summary: summaryFor(code), players: [], pairings: [], standings: [] };
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={["/tournament"]}>
      <TournamentLandingPage />
    </MemoryRouter>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(openPhaseSocket).mockReset();
  vi.mocked(withReconnect).mockReset();
  driver = null;
  store().closeSubscriptionSocket();
  localStorageMock.items.clear();
  useMultiplayerStore.setState({
    tournamentCredentials: {},
    hostingServer: "ws://localhost:8787",
    displayName: "",
  });
});

afterEach(() => {
  cleanup();
  store().closeSubscriptionSocket();
});

// ── V16 / V17 — the list is the server's, verbatim ───────────────────────

describe("TournamentLandingPage list", () => {
  it("renders the list from onListUpdate, in the array order given", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    // Reach-guard: the shared subscription really was acquired.
    expect(fake.tally("SubscribeLobby")).toBe(1);
    expect(screen.getByText("Loading tournaments…")).toBeTruthy();

    // Non-monotonic `player_count` on purpose: if the page sorted by anything,
    // this order would not survive.
    await act(async () => {
      fake.deliver("TournamentListUpdate", {
        tournaments: [
          summaryFor("TOUR01", { name: "Zulu Open", player_count: 8 }),
          summaryFor("TOUR02", { name: "Alpha Open", player_count: 2 }),
        ],
      });
    });

    const rows = screen.getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("Zulu Open");
    expect(rows[1].textContent).toContain("Alpha Open");
  });

  it("renders the empty state when the broker reports no tournaments", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await act(async () => {
      fake.deliver("TournamentListUpdate", { tournaments: [] });
    });

    expect(screen.getByText("No tournaments right now.")).toBeTruthy();
    expect(screen.queryAllByRole("listitem")).toHaveLength(0);
  });

  // V17 — the store's own contract: `TournamentRemoved` carries no list delta,
  // so filtering client-side would invent a protocol the broker does not speak.
  it("does not filter the list on TournamentRemoved", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await act(async () => {
      fake.deliver("TournamentListUpdate", {
        tournaments: [summaryFor("TOUR01"), summaryFor("TOUR02")],
      });
    });
    expect(screen.getAllByRole("listitem")).toHaveLength(2);

    await act(async () => {
      fake.deliver("TournamentRemoved", { code: "TOUR01" });
    });
    expect(screen.getAllByRole("listitem")).toHaveLength(2);

    // Reach-guard: the list DOES track the server — it just tracks it whole.
    await act(async () => {
      fake.deliver("TournamentListUpdate", { tournaments: [summaryFor("TOUR02")] });
    });
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
  });

  // V7's rendered half: the badge is the page's, since `TournamentListItem`'s
  // props are frozen.
  it("badges rows this browser organizes or has entered, and no others", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        TOUR01: { organizerToken: "o", updatedAt: 1 },
        TOUR02: { playerToken: "p", playerKey: "alice", updatedAt: 1 },
      },
    });
    renderPage();
    await settle();

    await act(async () => {
      fake.deliver("TournamentListUpdate", {
        tournaments: [summaryFor("TOUR01"), summaryFor("TOUR02"), summaryFor("TOUR03")],
      });
    });

    expect(screen.getAllByText("Organizer")).toHaveLength(1);
    expect(screen.getAllByText("Entered")).toHaveLength(1);
    // The spectating relation is suppressed in a list — 20 rows all reading
    // "Spectating" is noise.
    expect(screen.queryByText("Spectating")).toBeNull();
  });
});

// ── V18 / V19 — the reply's code is the only navigation authority ─────────

describe("TournamentLandingPage create and join", () => {
  it("navigates to the code the broker minted, not to anything typed", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament name"), "Friday Night Magic");
    await user.click(screen.getByRole("button", { name: "Create Tournament" }));
    await settle();

    expect(fake.tally("CreateTournament")).toBe(1);
    const sent = fake.frame("CreateTournament")?.data as {
      name: string;
      arity: number;
      scoring: { win_points: number };
      bracket: string;
    };
    expect(sent.name).toBe("Friday Night Magic");
    expect(sent.arity).toBe(2);
    expect(sent.bracket).toBe("Swiss");
    expect(sent.scoring.win_points).toBe(3);

    // A code that matches nothing the client could have derived.
    await act(async () => {
      fake.deliver("TournamentCreated", {
        code: "NEWC01",
        organizer_token: "org-token",
        view: viewFor("NEWC01"),
      });
    });
    await settle();

    expect(mocks.navigate).toHaveBeenCalledWith("/tournament/NEWC01");
  });

  it("sends the typed join code and display name, and navigates on the reply", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament code"), "TOUR01");
    await user.type(screen.getByLabelText("Display name"), "Alice");
    await user.click(screen.getByRole("button", { name: "Join" }));
    await settle();

    expect(fake.tally("JoinTournament")).toBe(1);
    const sent = fake.frame("JoinTournament")?.data as {
      code: string;
      display_name: string;
    };
    expect(sent.code).toBe("TOUR01");
    expect(sent.display_name).toBe("Alice");

    await act(async () => {
      fake.deliver("TournamentJoined", {
        code: "TOUR01",
        player_token: "player-token",
        view: viewFor("TOUR01"),
      });
    });
    await settle();

    expect(mocks.navigate).toHaveBeenCalledWith("/tournament/TOUR01");
  });

  it("sends a join with an empty display name rather than pre-rejecting it", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament code"), "TOUR01");
    await user.click(screen.getByRole("button", { name: "Join" }));
    await settle();

    // The store falls back to `displayName`/`"Player"`; validation is the
    // broker's, and a second copy of it here would drift.
    expect(fake.tally("JoinTournament")).toBe(1);
    expect((fake.frame("JoinTournament")?.data as { display_name: string }).display_name).toBe(
      "Player",
    );
  });
});

// ── A reply that lands after the page is gone writes nothing ─────────────
//
// The same staleness class `TournamentPage`'s `shownCode` guard covers, in the
// shape this page needs: no `:code` to compare against, so the identity is
// "still mounted". `navigate` is the write that matters — a `setState` after
// unmount is a silent no-op, but a `navigate` genuinely moves the viewer off
// whatever route they had gone to.

describe("TournamentLandingPage stale continuations", () => {
  it("navigates on a reply that lands while the page is still up", async () => {
    // The paired positive for the two tests below: `navigate` IS reachable in
    // this harness, so "never navigates" cannot be what makes them pass.
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament name"), "Friday Night Magic");
    await user.click(screen.getByRole("button", { name: "Create Tournament" }));
    await settle();
    expect(fake.tally("CreateTournament")).toBe(1);

    await act(async () => {
      fake.deliver("TournamentCreated", {
        code: "NEWC01",
        organizer_token: "org-token",
        view: viewFor("NEWC01"),
      });
    });
    await settle();

    expect(mocks.navigate).toHaveBeenCalledWith("/tournament/NEWC01");
  });

  it("does not navigate when a create reply lands after the page is gone", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    const { unmount } = renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament name"), "Friday Night Magic");
    await user.click(screen.getByRole("button", { name: "Create Tournament" }));
    await settle();
    // Reach-guard: the request really is in flight across the unmount below,
    // so the reply has something of this page's to settle.
    expect(fake.tally("CreateTournament")).toBe(1);
    expect(mocks.navigate).not.toHaveBeenCalled();

    unmount();

    await act(async () => {
      fake.deliver("TournamentCreated", {
        code: "NEWC01",
        organizer_token: "org-token",
        view: viewFor("NEWC01"),
      });
    });
    await settle();

    expect(mocks.navigate).not.toHaveBeenCalledWith("/tournament/NEWC01");
    expect(mocks.navigate).not.toHaveBeenCalled();
    // Second reach-guard, and the reason declining is lossless rather than
    // merely quiet: the RPC really did settle `ok` — the store recorded the
    // minted credential — so the navigation was reached and refused, not
    // skipped because nothing ever arrived.
    expect(store().tournamentCredentials.NEWC01?.organizerToken).toBe("org-token");
  });

  it("does not navigate when a join reply lands after the page is gone", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    const { unmount } = renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament code"), "TOUR01");
    await user.type(screen.getByLabelText("Display name"), "Alice");
    await user.click(screen.getByRole("button", { name: "Join" }));
    await settle();
    expect(fake.tally("JoinTournament")).toBe(1);
    expect(mocks.navigate).not.toHaveBeenCalled();

    unmount();

    await act(async () => {
      fake.deliver("TournamentJoined", {
        code: "TOUR01",
        player_token: "player-token",
        view: viewFor("TOUR01"),
      });
    });
    await settle();

    expect(mocks.navigate).not.toHaveBeenCalled();
    expect(store().tournamentCredentials.TOUR01?.playerToken).toBe("player-token");
  });
});

// ── V20 — failures render catalog copy, with the server's text interpolated ─

describe("TournamentLandingPage failures", () => {
  it("wraps a broker rejection in the serverRejected template", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament name"), "Friday Night Magic");
    await user.click(screen.getByRole("button", { name: "Create Tournament" }));
    await settle();

    await act(async () => {
      fake.deliver("Error", { message: "Tournament name is empty" });
    });
    await settle();

    // The template is the discriminating half: the store never produces this
    // wrapper, and the raw server text must survive inside it untranslated.
    const alert = screen.getByRole("alert");
    expect(alert.textContent).toBe(
      "The server rejected that: Tournament name is empty",
    );
    expect(mocks.navigate).not.toHaveBeenCalled();
  });

  it("renders the aborted copy when a reconnect cuts an in-flight request short", async () => {
    const user = userEvent.setup();
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    await user.type(screen.getByLabelText("Tournament name"), "Friday Night Magic");
    await user.click(screen.getByRole("button", { name: "Create Tournament" }));
    await settle();
    // Reach-guard: the request really was in flight when the abort landed.
    expect(fake.tally("CreateTournament")).toBe(1);

    await act(async () => {
      driver?.fire("reconnecting");
    });
    await settle();

    expect(screen.getByRole("alert").textContent).toBe("That request was cancelled.");
  });

  // V21 — `detach === null` is a real branch and must be rendered, not
  // swallowed into a permanent "Loading tournaments…".
  it("renders the offline copy when the subscription cannot be opened", async () => {
    primeOfflineSocket();
    renderPage();
    await settle();

    expect(screen.getByRole("alert").textContent).toBe(
      "Lost connection to the lobby. Check your server address.",
    );
  });

  it("does not render the offline copy against a working socket", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    const { container } = renderPage();
    await settle();

    expect(
      screen.queryByText("Lost connection to the lobby. Check your server address."),
    ).toBeNull();
    // Reach-guard: the page really rendered.
    expectCatalogValuePresent(container, "Open Tournaments");
  });
});

// ── V15 — unmount during an in-flight connect (#4615) ────────────────────

describe("TournamentLandingPage subscription lifecycle", () => {
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
    // Nothing has attached yet — the connect is still in flight.
    expect(fake.listenerCount("message")).toBe(0);

    unmount();
    await act(async () => {
      resolveOpen(fake.socket);
    });
    await settle();

    expect(fake.listenerCount("message")).toBe(0);
    expect(fake.tally("UnsubscribeLobby")).toBe(1);
  });

  it("keeps the subscription attached and delivering while mounted", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    renderPage();
    await settle();

    // The paired positive: without unmounting, listeners stay bound and a
    // broadcast reaches the page.
    expect(fake.listenerCount("message")).toBeGreaterThanOrEqual(1);
    await act(async () => {
      fake.deliver("TournamentListUpdate", {
        tournaments: [summaryFor("TOUR01", { name: "Friday Night Magic" })],
      });
    });
    expect(screen.getByText("Friday Night Magic")).toBeTruthy();
    expect(fake.tally("UnsubscribeLobby")).toBe(0);
  });
});

// ── V13 — no raw key path leaks into a rendered text node ────────────────

describe("TournamentLandingPage catalog completeness", () => {
  const rich: TournamentSummary[] = [
    summaryFor("TOUR01", { name: "Friday Night Magic", status: "Registration" }),
    summaryFor("TOUR02", {
      name: "Commander Pods",
      status: "InProgress",
      arity: 4,
      bracket: "SingleElimination",
      current_round: 2,
      player_count: 1,
    }),
    summaryFor("TOUR03", { name: "Regional Final", status: "Completed", current_round: 3 }),
    summaryFor("TOUR04", { name: "Lapsed Event", status: "Abandoned", player_count: 0 }),
  ];

  it("renders no unresolved key paths for a viewer holding credentials", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    useMultiplayerStore.setState({
      tournamentCredentials: {
        TOUR01: { organizerToken: "o", updatedAt: 1 },
        TOUR02: { playerToken: "p", playerKey: "alice", updatedAt: 1 },
      },
    });
    const { container } = renderPage();
    await settle();
    await act(async () => {
      fake.deliver("TournamentListUpdate", { tournaments: rich });
    });

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "Open Tournaments");
    expectCatalogValuePresent(container, "Join by Code");
    expectCatalogValuePresent(container, "Create Tournament");
  });

  it("renders no unresolved key paths for a viewer holding none", async () => {
    const fake = makeFakeSocket();
    primeSocket(fake);
    const { container } = renderPage();
    await settle();
    await act(async () => {
      fake.deliver("TournamentListUpdate", { tournaments: rich });
    });

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "Open Tournaments");
  });
});
