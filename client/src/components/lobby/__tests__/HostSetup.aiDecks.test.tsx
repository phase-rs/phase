import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type * as Scryfall from "../../../services/scryfall";
import { detectAndParseDeck } from "../../../services/deckParser";

const testPreparationEpoch = { current: 0, invalidate() { this.current += 1; } };

// Same WebStorage shadowing workaround as HostSetup.test.tsx.
const localStorageItems = vi.hoisted(() => {
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
  return items;
});

const art = vi.hoisted(() => ({ face: vi.fn(), printings: vi.fn(), bounds: vi.fn() }));
vi.mock("../../../adapter/wasm-adapter", () => ({
  getHostAdapter: () => ({}),
  getSharedAdapter: () => ({ getCardFaceData: art.face, checkDeckPrintingsBounds: art.bounds }),
}));
vi.mock("../../../services/scryfall", async (importOriginal) => ({
  ...await importOriginal<typeof Scryfall>(),
  getCardPrintings: art.printings,
}));

vi.mock("../../../services/engineRuntime", async () => ({
  ...(await vi.importActual<typeof import("../../../services/engineRuntime")>("../../../services/engineRuntime")),
  bestOfThreeCeilingForFormat: vi.fn(async () => "Bo1"),
  deckSupplyForFormat: vi.fn(async () => "PlayerBuilt"),
}));

const catalogState = vi.hoisted(() => ({ candidates: [] as unknown[] }));
vi.mock("../../../services/aiDeckCatalog", async () => ({
  ...(await vi.importActual<typeof import("../../../services/aiDeckCatalog")>("../../../services/aiDeckCatalog")),
  useAiDeckCatalog: () => ({ candidates: catalogState.candidates, allCandidates: catalogState.candidates, loading: false, error: null }),
}));

import { HostSetup } from "../HostSetup";
import * as serverDirectory from "../../../services/serverDirectory";
import type { AiDeckCandidate } from "../../../services/aiDeckCatalog";
import { expandParsedDeck } from "../../../services/deckParser";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION } from "../../../adapter/ws-adapter";
import { DEFAULT_MULTIPLAYER_SERVER_URL } from "../../../config/multiplayerServer";
import { refuseRealWebSockets } from "../../../test/helpers/refusingWebSocket";

function candidate(
  id: string,
  overrides: Partial<AiDeckCandidate> = {},
): AiDeckCandidate {
  return {
    id,
    name: id,
    source: { type: "precon", deckId: id, code: "TST" },
    deck: { main: [{ name: `${id} Card`, count: 60 }], sideboard: [] },
    coveragePct: 100,
    archetype: null,
    bracket: null,
    ...overrides,
  };
}

const ALPHA = candidate("Alpha");
const BRAVO = candidate("Bravo");
const CHARLIE = candidate("Charlie");

/** A three-player Free-for-All table with seats 2 and 3 set to AI. The seats are
 *  restored without a `deckId`, as a pre-feature remembered config would be. */
function rememberTwoAiSeats() {
  useMultiplayerStore.setState({
    lastHostConfig: {
      format: "FreeForAll",
      formatConfig: FORMAT_DEFAULTS.FreeForAll,
      savedCustomFormatId: null,
      playerCount: 3,
      matchType: "Bo1",
      loopDetection: { type: "Off" },
      isPublic: true,
      startWhenFull: true,
      ranked: false,
      aiSeats: [
        { seatIndex: 1, difficulty: "Medium", deckName: null },
        { seatIndex: 2, difficulty: "Hard", deckName: null },
      ],
    },
  });
}

function renderHostSetup(onHost = vi.fn().mockResolvedValue(false)) {
  render(<HostSetup preparationEpoch={testPreparationEpoch} onHost={onHost} onBack={vi.fn()} connectionMode="server" onConnectionModeChange={vi.fn()} />);
  return onHost;
}

function deckPickers(): HTMLElement[] {
  return screen.getAllByRole("button", { name: "Deck" });
}

describe("HostSetup — per-seat AI decks", () => {
  beforeEach(() => {
    refuseRealWebSockets();
    vi.spyOn(serverDirectory, "refreshServerDirectory").mockResolvedValue(undefined);
    vi.spyOn(useMultiplayerStore.getState(), "ensureSubscriptionSocket").mockResolvedValue(null);
    localStorageItems.clear();
    usePreferencesStore.setState({ artChain: [], artOverrides: {}, cedhMode: false });
    art.face.mockReset().mockImplementation(async (name: string) => ({ scryfall_oracle_id: name }));
    art.printings.mockReset().mockResolvedValue([
      { id: "b", set: "bbb", collector_number: "002", released_at: "2001-01-01", frame_effects: [], faces: [] },
      { id: "a", set: "aaa", collector_number: "001", released_at: "2000-01-01", frame_effects: [], faces: [] },
    ]);
    art.bounds.mockReset().mockResolvedValue(undefined);
    catalogState.candidates = [ALPHA, BRAVO, CHARLIE];
    usePreferencesStore.getState().setAiArchetypeFilter("Any");
    usePreferencesStore.getState().setAiCoverageFloor(50);
    usePreferencesStore.getState().setAiBracketFilter([]);
    useMultiplayerStore.setState({
      displayName: "",
      formatConfig: null,
      lastHostConfig: null,
      userLobbySources: [],
      sourceStatus: new Map([[DEFAULT_MULTIPLAYER_SERVER_URL, {
        state: "open", playerCount: 0,
        serverInfo: {
          version: "test", buildCommit: "test", mode: "Full",
          protocolVersion: PROTOCOL_VERSION, lobbyProtocolVersion: LOBBY_PROTOCOL_VERSION,
        },
      }]]),
      directorySources: [],
      disabledDirectorySources: [],
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("shows a deck picker per AI seat defaulting to Random", async () => {
    rememberTwoAiSeats();
    renderHostSetup();

    await waitFor(() => expect(deckPickers()).toHaveLength(2));
    for (const picker of deckPickers()) {
      expect(picker).toHaveTextContent("Random (3)");
    }
  });

  it("gives a newly toggled AI seat a Random deck picker", async () => {
    const user = userEvent.setup();
    renderHostSetup();

    expect(screen.queryByRole("button", { name: "Deck" })).toBeNull();
    await user.click(screen.getAllByRole("button", { name: "Human" })[0]);

    expect(await screen.findByRole("button", { name: "Deck" })).toHaveTextContent("Random (3)");
  });

  it("lists Random first, then each catalog deck with its source and coverage", async () => {
    const user = userEvent.setup();
    rememberTwoAiSeats();
    catalogState.candidates = [candidate("Alpha", { archetype: "Aggro", coveragePct: 87 }), BRAVO];
    renderHostSetup();

    await user.click((await waitFor(() => deckPickers()))[0]);
    const options = within(screen.getByRole("listbox")).getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual([
      "Random (2)",
      "Alpha — Precon · Aggro · 87%",
      "Bravo — Precon · 100%",
    ]);
  });

  it("submits each seat's own pinned deck", async () => {
    const user = userEvent.setup();
    rememberTwoAiSeats();
    const onHost = renderHostSetup();

    await waitFor(() => expect(deckPickers()).toHaveLength(2));
    await user.click(deckPickers()[0]);
    await user.click(screen.getByRole("option", { name: /^Alpha/ }));
    await user.click(deckPickers()[1]);
    await user.click(screen.getByRole("option", { name: /^Bravo/ }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledTimes(1);
    const { aiSeats } = onHost.mock.calls[0][0];
    expect(aiSeats).toEqual([
      expect.objectContaining({
        seatIndex: 1,
        difficulty: "Medium",
        deckId: "Alpha",
        deck: { type: "DeckList", data: { ...expandParsedDeck(ALPHA.deck), bracket_tier: "core" } },
      }),
      expect.objectContaining({
        seatIndex: 2,
        difficulty: "Hard",
        deckId: "Bravo",
        deck: { type: "DeckList", data: { ...expandParsedDeck(BRAVO.deck), bracket_tier: "core" } },
      }),
    ]);
  });

  it("resolves Random seats to distinct concrete decklists", async () => {
    const user = userEvent.setup();
    rememberTwoAiSeats();
    vi.spyOn(Math, "random").mockReturnValue(0);
    const onHost = renderHostSetup();

    await waitFor(() => expect(deckPickers()).toHaveLength(2));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    const { aiSeats } = onHost.mock.calls[0][0];
    // The selection stays Random; only `deck` carries the concrete decklist.
    expect(aiSeats.map((seat: { deckId: string }) => seat.deckId)).toEqual(["Random", "Random"]);
    const decks = aiSeats.map((seat: { deck: { type: string; data: unknown } }) => seat.deck);
    for (const deck of decks) expect(deck.type).toBe("DeckList");
    expect(decks[0].data).not.toEqual(decks[1].data);
    const catalogDecks = [ALPHA, BRAVO, CHARLIE].map((c) => ({ ...expandParsedDeck(c.deck), bracket_tier: "core" }));
    for (const deck of decks) expect(catalogDecks).toContainEqual(deck.data);
  });

  it("draws Random seats from the single-player pool preferences", async () => {
    const user = userEvent.setup();
    rememberTwoAiSeats();
    catalogState.candidates = [
      candidate("Alpha", { coveragePct: 60 }),
      candidate("Bravo", { coveragePct: 95 }),
      candidate("Charlie", { coveragePct: 95 }),
    ];
    usePreferencesStore.getState().setAiCoverageFloor(90);
    const onHost = renderHostSetup();

    await waitFor(() => expect(deckPickers()[0]).toHaveTextContent("Random (2)"));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    const { aiSeats } = onHost.mock.calls[0][0];
    const poolDecks = [BRAVO, CHARLIE].map((c) => ({ ...expandParsedDeck(c.deck), bracket_tier: "core" }));
    expect(aiSeats).toHaveLength(2);
    for (const seat of aiSeats) {
      expect(seat.deckId).toBe("Random");
      expect(poolDecks).toContainEqual(seat.deck.data);
    }
    expect(aiSeats[0].deck.data).not.toEqual(aiSeats[1].deck.data);
  });

  it("lists the full legal catalog for pinning when soft preferences empty the filtered pool", async () => {
    const user = userEvent.setup();
    rememberTwoAiSeats();
    // Every deck falls under the coverage floor, so the soft preferences match
    // nothing — but the effective pool falls back to the curated pool (both
    // decks), which is what Random seats draw from.
    catalogState.candidates = [candidate("Alpha", { coveragePct: 60 }), candidate("Bravo", { coveragePct: 70 })];
    usePreferencesStore.getState().setAiCoverageFloor(90);
    const onHost = renderHostSetup();

    await waitFor(() => expect(deckPickers()).toHaveLength(2));
    expect(deckPickers()[0]).toHaveTextContent("Random (2)");
    await user.click(deckPickers()[0]);
    expect(within(screen.getByRole("listbox")).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Random (2)",
      "Alpha — Precon · 60%",
      "Bravo — Precon · 70%",
    ]);
    await user.click(screen.getByRole("option", { name: /^Bravo/ }));
    await user.click(deckPickers()[1]);
    await user.click(screen.getByRole("option", { name: /^Alpha/ }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    const { aiSeats } = onHost.mock.calls[0][0];
    expect(aiSeats[0].deckId).toBe("Bravo");
    expect(aiSeats[0].deck.data).toEqual({
      ...expandParsedDeck(candidate("Bravo").deck),
      bracket_tier: "core",
    });
    expect(aiSeats[1].deckId).toBe("Alpha");
  });

  it("carries each picked deck's declared bracket tier onto the seat payload", async () => {
    const user = userEvent.setup();
    rememberTwoAiSeats();
    catalogState.candidates = [candidate("Alpha", { bracket: 5 }), BRAVO];
    const onHost = renderHostSetup();

    await waitFor(() => expect(deckPickers()).toHaveLength(2));
    await user.click(deckPickers()[0]);
    await user.click(screen.getByRole("option", { name: /^Alpha/ }));
    await user.click(deckPickers()[1]);
    await user.click(screen.getByRole("option", { name: /^Bravo/ }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    const { aiSeats } = onHost.mock.calls[0][0];
    expect(aiSeats[0].deck.data.bracket_tier).toBe("cedh");
    expect(aiSeats[1].deck.data.bracket_tier).toBe("core");
  });

  it("remembers the selection, not the resolved deck, so Random stays Random", async () => {
    const user = userEvent.setup();
    rememberTwoAiSeats();
    renderHostSetup();

    await waitFor(() => expect(deckPickers()).toHaveLength(2));
    await user.click(deckPickers()[1]);
    await user.click(screen.getByRole("option", { name: /^Charlie/ }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    const remembered = useMultiplayerStore.getState().lastHostConfig?.aiSeats;
    expect(remembered?.map((seat) => seat.deckId)).toEqual([undefined, "Charlie"]);
  });

  it("blocks hosting while the catalog has no decks for an AI seat", async () => {
    rememberTwoAiSeats();
    catalogState.candidates = [];
    renderHostSetup();

    await waitFor(() => expect(deckPickers()).toHaveLength(2));
    expect(screen.getByRole("button", { name: "Host Game" })).toBeDisabled();
  });

  describe("cEDH table", () => {
    /** A Commander table with one remembered CEDH-difficulty AI seat, on Random unless pinned. */
    function rememberCedhRandomSeat(deckId?: string) {
      useMultiplayerStore.setState({
        lastHostConfig: {
          format: "Commander",
          formatConfig: FORMAT_DEFAULTS.Commander,
          savedCustomFormatId: null,
          playerCount: 2,
          matchType: "Bo1",
          loopDetection: { type: "Off" },
          isPublic: true,
          startWhenFull: true,
          ranked: false,
          aiSeats: [{ seatIndex: 1, difficulty: "CEDH", deckName: null, deckId }],
        },
      });
    }

    it("blocks hosting a Random seat when no bracket-5 deck exists", async () => {
      rememberCedhRandomSeat();
      catalogState.candidates = [candidate("Alpha", { bracket: 3 }), candidate("Bravo", { bracket: 4 })];
      renderHostSetup();

      await waitFor(() => expect(deckPickers()).toHaveLength(1));
      expect(screen.getByRole("status")).toHaveTextContent(
        "No bracket-5 decks available. Random AI seats cannot be filled.",
      );
      expect(screen.getByRole("button", { name: "Host Game" })).toBeDisabled();
    });

    it("allows hosting once the catalog holds a bracket-5 deck", async () => {
      rememberCedhRandomSeat();
      catalogState.candidates = [candidate("Alpha", { bracket: 3 }), candidate("Bravo", { bracket: 5 })];
      renderHostSetup();

      await waitFor(() => expect(deckPickers()).toHaveLength(1));
      expect(screen.queryByRole("status")).toBeNull();
      expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled();
    });

    it("blocks hosting a seat pinned to a non-bracket-5 deck", async () => {
      rememberCedhRandomSeat("Alpha");
      catalogState.candidates = [candidate("Alpha", { bracket: 4 }), candidate("Bravo", { bracket: 5 })];
      renderHostSetup();

      await waitFor(() => expect(deckPickers()).toHaveLength(1));
      expect(deckPickers()[0]).toHaveTextContent("Alpha");
      expect(screen.getByRole("status")).toHaveTextContent(
        "All opponents play cEDH. Every deck must be bracket 5.",
      );
      expect(screen.getByRole("button", { name: "Host Game" })).toBeDisabled();
    });

    it("allows hosting a seat pinned to a bracket-5 deck", async () => {
      rememberCedhRandomSeat("Bravo");
      catalogState.candidates = [candidate("Alpha", { bracket: 4 }), candidate("Bravo", { bracket: 5 })];
      renderHostSetup();

      await waitFor(() => expect(deckPickers()).toHaveLength(1));
      expect(deckPickers()[0]).toHaveTextContent("Bravo");
      expect(screen.queryByRole("status")).toBeNull();
      expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled();
    });
  });
  describe("owner preparation lifetime (V7)", () => {
    // ES2021 test compilation cannot use Promise.withResolvers.
    function heldMetadata() {
      let resolve!: () => void;
      const promise = new Promise<void>((done) => { resolve = done; });
      return { promise, resolve };
    }
    function prepareSeats() {
      rememberTwoAiSeats();
      catalogState.candidates = [
        candidate("Alpha", { deck: detectAndParseDeck("Deck\n2 Forest (AAA) 001") }),
        candidate("Bravo", { deck: detectAndParseDeck("Deck\n3 Island (BBB) 002") }),
        candidate("Charlie", { deck: detectAndParseDeck("Deck\n4 Mountain (AAA) 001") }),
        candidate("Delta", { deck: detectAndParseDeck("Deck\n5 Plains (BBB) 002") }),
      ];
      const remembered = useMultiplayerStore.getState().lastHostConfig!;
      useMultiplayerStore.setState({ hostingStatus: "idle", lastHostConfig: {
        ...remembered, playerCount: 5,
        aiSeats: [
          { seatIndex: 1, difficulty: "Medium", deckName: null, deckId: "Alpha" },
          { seatIndex: 2, difficulty: "Hard", deckName: null, deckId: "Bravo" },
          { seatIndex: 3, difficulty: "Easy", deckName: null, deckId: "Random" },
          { seatIndex: 4, difficulty: "Hard", deckName: null, deckId: "Random" },
        ],
      } });
    }

    it("submits two explicit and two random source decks under one snapshot and remembers selections only", async () => {
      prepareSeats();
      const held = heldMetadata();
      art.face.mockImplementation(async (name: string) => {
        await held.promise;
        return { scryfall_oracle_id: name };
      });
      const onHost = renderHostSetup();
      await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
      await userEvent.click(screen.getByRole("button", { name: "Host Game" }));
      await waitFor(() => expect(art.face).toHaveBeenCalledTimes(4));
      expect(onHost).not.toHaveBeenCalled();
      expect(screen.getByRole("button", { name: /Opening/ })).toBeDisabled();
      act(() => usePreferencesStore.setState({ artChain: [{ type: "newest" }] }));
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(onHost).toHaveBeenCalledOnce());
      const seats = onHost.mock.calls[0][0].aiSeats;
      expect(seats.map((seat: { seatIndex: number; difficulty: string; deckId: string }) =>
        [seat.seatIndex, seat.difficulty, seat.deckId])).toEqual([
        [1, "Medium", "Alpha"], [2, "Hard", "Bravo"], [3, "Easy", "Random"], [4, "Hard", "Random"],
      ]);
      const sources = { Forest: ["aaa", "001", 2], Island: ["bbb", "002", 3], Mountain: ["aaa", "001", 4], Plains: ["bbb", "002", 5] } as const;
      for (const seat of seats) {
        const name = seat.deck.data.main_deck[0] as keyof typeof sources;
        const [set_code, collector_number, count] = sources[name];
        expect(seat.deck.data.printings).toEqual([{ section: "main_deck", name, count, printing: { set_code, collector_number } }]);
        expect(seat.deck.data.bracket_tier).toBe("core");
      }
      expect(useMultiplayerStore.getState().lastHostConfig!.aiSeats).toEqual([
        { seatIndex: 1, difficulty: "Medium", deckName: null, deckId: "Alpha" },
        { seatIndex: 2, difficulty: "Hard", deckName: null, deckId: "Bravo" },
        { seatIndex: 3, difficulty: "Easy", deckName: null, deckId: "Random" },
        { seatIndex: 4, difficulty: "Hard", deckName: null, deckId: "Random" },
      ]);
      await userEvent.click(screen.getByRole("button", { name: "Host Game" }));
      await waitFor(() => expect(onHost).toHaveBeenCalledTimes(2));
      expect(onHost.mock.calls[1][0].aiSeats[0].deck.data.printings).toEqual([
        { section: "main_deck", name: "Forest", count: 2, printing: { set_code: "bbb", collector_number: "002" } },
      ]);
    });

    it.each(["back", "unmount", "replacement", "supersede"] as const)("invalidates %s without stale host/config writes, while a live attempt submits", async (stop) => {
      prepareSeats();
      const held = heldMetadata();
      art.face.mockImplementation(async (name: string) => {
        await held.promise;
        return { scryfall_oracle_id: name };
      });
      const onHost = vi.fn().mockResolvedValue(false);
      const onBack = vi.fn();
      const view = render(<HostSetup preparationEpoch={testPreparationEpoch} onHost={onHost} onBack={onBack} connectionMode="server" onConnectionModeChange={vi.fn()} />);
      await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
      const form = screen.getByRole("button", { name: "Host Game" }).closest("form")!;
      const remembered = useMultiplayerStore.getState().lastHostConfig;
      if (stop === "supersede") {
        // Both submits enter before React publishes isSubmitting; the latter owns completion.
        act(() => { fireEvent.submit(form); fireEvent.submit(form); });
        await waitFor(() => expect(art.face).toHaveBeenCalledTimes(8));
      } else {
        await userEvent.click(screen.getByRole("button", { name: "Host Game" }));
        await waitFor(() => expect(art.face).toHaveBeenCalledTimes(4));
        if (stop === "back") await userEvent.click(screen.getByRole("button", { name: "Back" }));
        if (stop === "unmount") view.unmount();
        if (stop === "replacement") view.rerender(<HostSetup preparationEpoch={testPreparationEpoch} onHost={onHost} onBack={onBack} connectionMode="p2p" onConnectionModeChange={vi.fn()} />);
      }
      await act(async () => { held.resolve(); });
      if (stop === "supersede") {
        await waitFor(() => expect(onHost).toHaveBeenCalledOnce());
      } else {
        expect(onHost).not.toHaveBeenCalled();
        expect(useMultiplayerStore.getState().lastHostConfig).toBe(remembered);
      }
      cleanup();
      prepareSeats();
      renderHostSetup(onHost);
      await waitFor(() => expect(screen.getByRole("button", { name: "Host Game" })).toBeEnabled());
      await userEvent.click(screen.getByRole("button", { name: "Host Game" }));
      await waitFor(() => expect(onHost).toHaveBeenCalledTimes(stop === "supersede" ? 2 : 1));
      expect(onHost.mock.calls[onHost.mock.calls.length - 1][0].aiSeats[0].deck.data.printings).toEqual([
        { section: "main_deck", name: "Forest", count: 2, printing: { set_code: "aaa", collector_number: "001" } },
      ]);
    });
  });
});
