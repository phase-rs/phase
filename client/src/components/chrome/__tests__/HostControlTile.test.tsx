import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router";

import { HostControlTile } from "../HostControlTile";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import type { DeckChoice, PlayerSlot } from "../../../stores/multiplayerStore";
import { emptySeatDeck } from "../../../services/pileSource";
import { expandParsedDeck } from "../../../services/deckParser";
import type { SeatKind } from "../../../multiplayer/seatTypes";
import echoWire from "../../../test/fixtures/seat-deck-echo-wire.json";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { UNFILED_POOL_GROUP, emptyPoolSelection } from "../../../services/randomPoolSelection";

const catalogMocks = vi.hoisted(() => ({
  candidates: [] as unknown[],
}));

vi.mock("../../../services/aiDeckCatalog", () => ({
  useAiDeckCatalog: () => ({
    candidates: catalogMocks.candidates,
    allCandidates: catalogMocks.candidates,
    loading: false,
    error: null,
  }),
}));

const twoHeadedGiantSlots: PlayerSlot[] = [
  {
    playerId: 0,
    name: "Host",
    kind: { type: "HostHuman" },
    teamInfo: { teamIndex: 0, positionInTeam: 0 },
  },
  {
    playerId: 1,
    name: "Partner",
    kind: { type: "JoinedHuman" },
    teamInfo: { teamIndex: 0, positionInTeam: 1 },
  },
  {
    playerId: 2,
    name: "",
    kind: { type: "WaitingHuman" },
    teamInfo: { teamIndex: 1, positionInTeam: 0 },
  },
  {
    playerId: 3,
    name: "AI",
    kind: { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } },
    teamInfo: { teamIndex: 1, positionInTeam: 1 },
  },
];

function renderHostControlTile(playerSlots: PlayerSlot[]) {
  useMultiplayerStore.setState({
    hostGameCode: "ABCD1",
    hostingStatus: "waiting",
    hostSession: {
      formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
      timerSeconds: null,
      matchType: "Bo1",
    },
    playerSlots,
    serverInfo: null,
  });

  render(
    <MemoryRouter initialEntries={["/multiplayer"]}>
      <HostControlTile />
    </MemoryRouter>,
  );
}

describe("HostControlTile", () => {
  afterEach(() => {
    cleanup();
    catalogMocks.candidates = [];
    useMultiplayerStore.setState({
      hostGameCode: null,
      hostingStatus: "idle",
      hostSession: null,
      playerSlots: [],
      serverInfo: null,
    });
    vi.clearAllMocks();
  });

  describe("AI seat deck promotion", () => {
    const CATALOG_DECK = {
      id: "cat-1",
      name: "Catalog Deck",
      source: "bundled",
      deck: { main: [{ count: 1, name: "Forest" }], sideboard: [] },
      coveragePct: null,
      archetype: null,
      bracket: null,
    };

    function renderWithAiSeat(deck: DeckChoice) {
      const seatMutate = vi.fn();
      catalogMocks.candidates = [CATALOG_DECK];
      useMultiplayerStore.setState({
        hostGameCode: "ABCD1",
        hostingStatus: "waiting",
        hostSession: {
          formatConfig: FORMAT_DEFAULTS.Standard,
          timerSeconds: null,
          matchType: "Bo1",
        },
        playerSlots: [
          { playerId: 0, name: "Host", kind: { type: "HostHuman" } },
          {
            playerId: 1,
            name: "AI",
            kind: { type: "Ai", data: { difficulty: "Medium", deck } },
          },
        ] as PlayerSlot[],
        serverInfo: null,
        seatMutate,
      });
      render(
        <MemoryRouter initialEntries={["/multiplayer"]}>
          <HostControlTile />
        </MemoryRouter>,
      );
      return seatMutate;
    }

    it("stops re-sending once the server echoes the installed deck", () => {
      // Guards the re-resolution loop.
      const onRandom = renderWithAiSeat({ type: "Random" });
      expect(onRandom).toHaveBeenCalledTimes(1);

      cleanup();

      const onDeckList = renderWithAiSeat({
        type: "DeckList",
        data: { main_deck: ["Forest"], sideboard: [], commander: [] },
      });
      expect(onDeckList).not.toHaveBeenCalled();
    });
  });

  describe("AI pool fallback warning", () => {
    const EXCLUDED_DECK = {
      id: "saved:Excluded Deck",
      name: "Excluded Deck",
      source: { type: "saved" },
      deck: { main: [{ count: 1, name: "Forest" }], sideboard: [] },
      coveragePct: null,
      archetype: null,
      bracket: null,
    };

    it("warns and still assigns a deck when the AI pool selection matches nothing", async () => {
      catalogMocks.candidates = [EXCLUDED_DECK];
      const seatMutateAsync = vi.fn().mockResolvedValue(undefined);
      const restoreSelection = usePreferencesStore.getState().aiPoolSelection;
      act(() => {
        usePreferencesStore.setState({
          // The only legal deck is excluded: live assignments fall back to
          // the full catalog.
          aiPoolSelection: { ...emptyPoolSelection(), excludedGroups: [UNFILED_POOL_GROUP] },
        });
      });
      try {
        useMultiplayerStore.setState({
          hostGameCode: "ABCD1",
          hostingStatus: "waiting",
          hostSession: {
            formatConfig: FORMAT_DEFAULTS.Standard,
            timerSeconds: null,
            matchType: "Bo1",
          },
          playerSlots: [
            { playerId: 0, name: "Host", kind: { type: "HostHuman" } },
            { playerId: 1, name: "", kind: { type: "WaitingHuman" } },
          ] as PlayerSlot[],
          serverInfo: null,
          seatMutate: vi.fn(),
          seatMutateAsync,
        });
        render(
          <MemoryRouter initialEntries={["/multiplayer"]}>
            <HostControlTile />
          </MemoryRouter>,
        );

        // The tile warns that Fill with AI draws from the full catalog …
        expect(screen.getByRole("status")).toHaveTextContent(/Everything is excluded/);

        // … and the fallback assignment still succeeds with the excluded deck.
        fireEvent.click(screen.getByRole("button", { name: /fill with ai/i }));
        await vi.waitFor(() => expect(seatMutateAsync).toHaveBeenCalled());
        const assignment = seatMutateAsync.mock.calls[0][0];
        expect(assignment.type).toBe("SetKind");
        expect(assignment.data.kind.type).toBe("Ai");
        expect(assignment.data.kind.data.deck.type).toBe("DeckList");
        expect(assignment.data.kind.data.deck.data.main_deck).toContain("Forest");
      } finally {
        act(() => {
          usePreferencesStore.setState({ aiPoolSelection: restoreSelection });
        });
      }
    });
  });

  it("renders team badges only for slots with team metadata", () => {
    renderHostControlTile(twoHeadedGiantSlots);

    expect(screen.getAllByText("Team 1")).toHaveLength(2);
    expect(screen.getAllByText("Team 2")).toHaveLength(2);

    cleanup();
    renderHostControlTile(twoHeadedGiantSlots.map(({ teamInfo: _teamInfo, ...slot }) => slot));

    expect(screen.queryByText("Team 1")).not.toBeInTheDocument();
    expect(screen.queryByText("Team 2")).not.toBeInTheDocument();
  });

  describe("join-link copy", () => {
    const realClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");

    function renderWithJoinLink(showToast: (message: string) => void) {
      useMultiplayerStore.setState({
        hostGameCode: "ABCD1",
        hostingStatus: "waiting",
        hostSession: {
          formatConfig: FORMAT_DEFAULTS.Standard,
          timerSeconds: null,
          matchType: "Bo1",
        },
        playerSlots: [],
        serverInfo: {
          version: "0.60.0",
          buildCommit: "abc1234",
          protocolVersion: 33,
          mode: "Full",
          publicUrl: "https://play.example.com",
        },
        showToast,
      });
      render(
        <MemoryRouter initialEntries={["/multiplayer"]}>
          <HostControlTile />
        </MemoryRouter>,
      );
      return screen.getByTitle(/ABCD1@play\.example\.com/);
    }

    afterEach(() => {
      if (realClipboard) Object.defineProperty(navigator, "clipboard", realClipboard);
      Reflect.deleteProperty(document, "execCommand");
    });

    it("confirms only when the clipboard actually took the link", async () => {
      Object.defineProperty(navigator, "clipboard", {
        value: { writeText: () => Promise.resolve() },
        configurable: true,
      });
      const showToast = vi.fn();

      fireEvent.click(renderWithJoinLink(showToast));
      await vi.waitFor(() => expect(showToast).toHaveBeenCalledWith("Join link copied"));

      cleanup();
      // Same click, a webview that cannot write: no confirmation may appear.
      Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
      Object.defineProperty(document, "execCommand", { value: () => false, configurable: true });
      const silentToast = vi.fn();

      fireEvent.click(renderWithJoinLink(silentToast));
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(silentToast).not.toHaveBeenCalled();
    });
  });

  describe("supplied-deck format seats", () => {
    const EMPTY: DeckChoice = { type: "DeckList", data: emptySeatDeck() };
    const CATALOG_DECK = {
      id: "cat-1",
      name: "Catalog Deck",
      source: "bundled",
      deck: { main: [{ count: 1, name: "Forest" }], sideboard: [] },
      coveragePct: null,
      archetype: null,
      bracket: null,
    };
    const host: PlayerSlot = { playerId: 0, name: "Host", kind: { type: "HostHuman" } };
    const waiting: PlayerSlot = { playerId: 1, name: "", kind: { type: "WaitingHuman" } };
    const joined: PlayerSlot = { playerId: 1, name: "Guest", kind: { type: "JoinedHuman" } };
    const aiWith = (deck: DeckChoice): PlayerSlot => ({
      playerId: 1, name: "AI", kind: { type: "Ai", data: { difficulty: "Medium", deck } },
    });

    function renderSeats(format: "Dandan" | "Standard", second: PlayerSlot) {
      const seatMutate = vi.fn();
      const seatMutateAsync = vi.fn().mockResolvedValue(undefined);
      useMultiplayerStore.setState({
        hostGameCode: "ABCD1",
        hostingStatus: "waiting",
        hostSession: { formatConfig: FORMAT_DEFAULTS[format], timerSeconds: null, matchType: "Bo1" },
        playerSlots: [host, second],
        serverInfo: null,
        seatMutate,
        seatMutateAsync,
      });
      render(
        <MemoryRouter initialEntries={["/multiplayer"]}>
          <HostControlTile />
        </MemoryRouter>,
      );
      return { seatMutate, seatMutateAsync };
    }

    afterEach(() => {
      vi.unstubAllGlobals();
    });

    const setAi = (deck: DeckChoice) => ({
      type: "SetKind",
      data: { seatIndex: 1, kind: { type: "Ai", data: { difficulty: "Medium", deck } } },
    });

    it("Add AI seats an empty deck for Dandan with an empty catalog", () => {
      const { seatMutate } = renderSeats("Dandan", waiting);
      fireEvent.click(screen.getByRole("button", { name: "Add AI" }));
      expect(seatMutate).toHaveBeenCalledWith({
        ...setAi(EMPTY),
        data: { ...setAi(EMPTY).data, expectedKind: { type: "WaitingHuman" } },
      });
    });

    it("Replace AI seats an empty deck for Dandan with an empty catalog", () => {
      vi.stubGlobal("confirm", vi.fn(() => true));
      const { seatMutate } = renderSeats("Dandan", joined);
      fireEvent.click(screen.getByRole("button", { name: "Replace AI" }));
      expect(seatMutate).toHaveBeenCalledWith(setAi(EMPTY));
    });

    it("Fill With AI seats an empty deck and starts for Dandan with an empty catalog", async () => {
      const { seatMutateAsync } = renderSeats("Dandan", waiting);
      fireEvent.click(screen.getByRole("button", { name: "Fill With AI" }));
      await vi.waitFor(() => expect(seatMutateAsync).toHaveBeenCalledWith({ type: "Start" }));
      expect(seatMutateAsync).toHaveBeenNthCalledWith(1, {
        ...setAi(EMPTY),
        data: { ...setAi(EMPTY).data, expectedKind: { type: "WaitingHuman" } },
      });
    });

    it("an AI seat holding Random is promoted to the empty deck for Dandan", () => {
      const { seatMutate } = renderSeats("Dandan", aiWith({ type: "Random" }));
      expect(seatMutate).toHaveBeenCalledWith(setAi(EMPTY));
    });

    it("a Dandan AI row offers no deck dropdown", () => {
      renderSeats("Dandan", aiWith(EMPTY));
      expect(screen.getAllByRole("combobox")).toHaveLength(1);
    });

    it("Standard with an empty catalog disables every AI deck consumer", () => {
      vi.stubGlobal("confirm", vi.fn(() => true));
      const waitingRender = renderSeats("Standard", waiting);
      expect(screen.getByRole("button", { name: "Add AI" })).toBeDisabled();
      expect(screen.getByRole("button", { name: "Fill With AI" })).toBeDisabled();
      expect(waitingRender.seatMutate).not.toHaveBeenCalled();
      cleanup();
      renderSeats("Standard", joined);
      expect(screen.getByRole("button", { name: "Replace AI" })).toBeDisabled();
      cleanup();
      const random = renderSeats("Standard", aiWith({ type: "Random" }));
      expect(random.seatMutate).not.toHaveBeenCalled();
    });

    it("Standard with a catalog deck shows the dropdown and promotes Random", () => {
      catalogMocks.candidates = [CATALOG_DECK];
      const { seatMutate } = renderSeats("Standard", aiWith({ type: "Random" }));
      expect(screen.getAllByRole("combobox")).toHaveLength(2);
      expect(seatMutate).toHaveBeenCalledWith(
        setAi({ type: "DeckList", data: expect.objectContaining({ main_deck: ["Forest"] }) }),
      );
    });
  });

  describe("AI deck dropdown selection after the echo", () => {
    // The engine's own echo of each deck, written by the seat-reducer golden
    // test: every list present, engine field order, `bracket_tier` serialized.
    function echoedDeck(seat: SeatKind): DeckChoice {
      if (seat.type !== "Ai" || seat.data.deck.type !== "DeckList") {
        throw new Error("the golden entry is not an AI seat holding a DeckList");
      }
      return seat.data.deck;
    }
    const echo = {
      plain: echoedDeck(echoWire.plain as SeatKind),
      companion: echoedDeck(echoWire.companion as SeatKind),
      declaredTier: echoedDeck(echoWire.declaredTier as SeatKind),
      forestIsland: echoedDeck(echoWire.forestIsland as SeatKind),
      islandForest: echoedDeck(echoWire.islandForest as SeatKind),
    };

    const catalogEntry = (name: string, deck: { main: { count: number; name: string }[]; companion?: string }) => ({
      id: `cat-${name}`,
      name,
      source: "bundled",
      deck: { ...deck, sideboard: [] },
      coveragePct: null,
      archetype: null,
      bracket: null,
    });
    const DECK_A = catalogEntry("A", { main: [{ count: 1, name: "Forest" }] });
    const DECK_B = catalogEntry("B", { main: [{ count: 1, name: "Island" }] });
    const DECK_C = catalogEntry("C", {
      main: [{ count: 1, name: "Island" }],
      companion: "Lurrus of the Dream-Den",
    });
    const DECK_FI = catalogEntry("FI", {
      main: [{ count: 1, name: "Forest" }, { count: 1, name: "Island" }],
    });
    const DECK_IF = catalogEntry("IF", {
      main: [{ count: 1, name: "Island" }, { count: 1, name: "Forest" }],
    });

    const host: PlayerSlot = { playerId: 0, name: "Host", kind: { type: "HostHuman" } };
    const aiWith = (deck: DeckChoice): PlayerSlot => ({
      playerId: 1, name: "AI", kind: { type: "Ai", data: { difficulty: "Medium", deck } },
    });

    function renderSeat(candidates: unknown[], deck: DeckChoice) {
      const seatMutate = vi.fn();
      catalogMocks.candidates = candidates;
      useMultiplayerStore.setState({
        hostGameCode: "ABCD1",
        hostingStatus: "waiting",
        hostSession: { formatConfig: FORMAT_DEFAULTS.Standard, timerSeconds: null, matchType: "Bo1" },
        playerSlots: [host, aiWith(deck)],
        serverInfo: null,
        seatMutate,
      });
      render(
        <MemoryRouter initialEntries={["/multiplayer"]}>
          <HostControlTile />
        </MemoryRouter>,
      );
      return seatMutate;
    }

    /** The AI seat's deck dropdown: the combobox that offers the catalog decks. */
    function deckSelect(firstLabel: string): HTMLSelectElement {
      const select = screen
        .getAllByRole("combobox")
        .find((element) => within(element).queryByRole("option", { name: firstLabel }));
      if (!select) throw new Error(`no dropdown offers a deck labelled ${firstLabel}`);
      return select as HTMLSelectElement;
    }
    // `selectedIndex`, not `selectedOptions`: jsdom's live collection goes stale
    // across a controlled re-render.
    const selectedLabel = (select: HTMLSelectElement) =>
      select.options[select.selectedIndex]?.textContent;
    const echoSeat = (deck: DeckChoice) => {
      act(() => useMultiplayerStore.setState({ playerSlots: [host, aiWith(deck)] }));
    };

    it("selects the seat's deck from the engine echo, not the first option", () => {
      renderSeat([DECK_A, DECK_B, DECK_C], echo.plain);

      const select = deckSelect("A");
      // Reach guard: all three decks are offered, and the first is "A".
      expect(Array.from(select.options, (option) => option.textContent)).toEqual(["A", "B", "C"]);
      expect(selectedLabel(select)).toBe("B");
    });

    it("keeps the picked deck selected once the engine echoes it", () => {
      const seatMutate = renderSeat([DECK_A, DECK_B, DECK_C], echo.plain);
      expect(seatMutate).not.toHaveBeenCalled();

      const select = deckSelect("A");
      const optionC = Array.from(select.options).find((option) => option.textContent === "C")!;
      fireEvent.change(select, { target: { value: optionC.value } });

      // Positive control: the pick reaches the catalog deck, companion and all.
      expect(seatMutate).toHaveBeenCalledTimes(1);
      expect(seatMutate).toHaveBeenCalledWith({
        type: "SetKind",
        data: {
          seatIndex: 1,
          kind: {
            type: "Ai",
            data: {
              difficulty: "Medium",
              deck: { type: "DeckList", data: expandParsedDeck(DECK_C.deck) },
            },
          },
        },
      });

      echoSeat(echo.companion);
      expect(selectedLabel(deckSelect("A"))).toBe("C");
    });

    it("tells apart decks that differ only in an optional list, and ignores the declared tier", () => {
      renderSeat([DECK_A, DECK_B, DECK_C], echo.companion);
      expect(selectedLabel(deckSelect("A"))).toBe("C");

      echoSeat(echo.declaredTier);
      expect(selectedLabel(deckSelect("A"))).toBe("B");
    });

    it("keeps decks that differ only in card order apart through pick and echo", () => {
      const seatMutate = renderSeat([DECK_FI, DECK_IF], echo.forestIsland);

      const select = deckSelect("FI");
      // Reach guard: both ordered decks are offered, and the seat holds the first.
      expect(Array.from(select.options, (option) => option.textContent)).toEqual(["FI", "IF"]);
      expect(selectedLabel(select)).toBe("FI");
      expect(select.options[0].value).not.toBe(select.options[1].value);

      fireEvent.change(select, { target: { value: select.options[1].value } });
      expect(seatMutate).toHaveBeenCalledTimes(1);
      expect(seatMutate).toHaveBeenCalledWith({
        type: "SetKind",
        data: {
          seatIndex: 1,
          kind: {
            type: "Ai",
            data: {
              difficulty: "Medium",
              deck: { type: "DeckList", data: expandParsedDeck(DECK_IF.deck) },
            },
          },
        },
      });
      expect(expandParsedDeck(DECK_IF.deck).main_deck).toEqual(["Island", "Forest"]);

      echoSeat(echo.islandForest);
      expect(selectedLabel(deckSelect("FI"))).toBe("IF");
    });
  });
});
