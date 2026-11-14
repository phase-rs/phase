import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router";

import { deckChoiceKey, HostControlTile } from "../HostControlTile";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";
import type { DeckChoice, PlayerSlot } from "../../../stores/multiplayerStore";
import { emptySeatDeck } from "../../../services/pileSource";
import type { SeatKind } from "../../../multiplayer/seatTypes";
import echoWire from "../../../test/fixtures/seat-deck-echo-wire.json";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { UNFILED_POOL_GROUP, emptyPoolSelection } from "../../../services/randomPoolSelection";
import { detectAndParseDeck, expandParsedDeck } from "../../../services/deckParser";
import type * as Scryfall from "../../../services/scryfall";
import type * as WasmAdapter from "../../../adapter/wasm-adapter";
import type { AiDeckCandidate } from "../../../services/aiDeckCatalog";
import type { SeatMutation } from "../../../stores/multiplayerStore";

/** `join-link copy` replaces the store's `showToast` action and never restores
 *  it; keep the real action so toast assertions observe what the tile shows. */
const realShowToast = useMultiplayerStore.getState().showToast;

const art = vi.hoisted(() => ({ face: vi.fn(), printings: vi.fn(), bounds: vi.fn() }));
vi.mock("../../../adapter/wasm-adapter", async (importOriginal) => ({
  ...await importOriginal<typeof WasmAdapter>(),
  getSharedAdapter: () => ({ getCardFaceData: art.face, checkDeckPrintingsBounds: art.bounds }),
}));
vi.mock("../../../services/scryfall", async (importOriginal) => ({
  ...await importOriginal<typeof Scryfall>(),
  getCardPrintings: art.printings,
}));

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
  beforeEach(() => {
    usePreferencesStore.setState({ artChain: [], artOverrides: {}, aiPoolSelection: emptyPoolSelection() });
    art.face.mockReset().mockImplementation(async (name: string) => ({ scryfall_oracle_id: name }));
    art.printings.mockReset().mockResolvedValue([
      { id: "a", set: "aaa", collector_number: "001", released_at: "2000-01-01", frame_effects: [], faces: [] },
      { id: "b", set: "bbb", collector_number: "002", released_at: "2001-01-01", frame_effects: [], faces: [] },
    ]);
    art.bounds.mockReset().mockResolvedValue(undefined);
    vi.stubGlobal("confirm", vi.fn(() => true));
  });
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
    vi.restoreAllMocks();
    vi.clearAllMocks();
    vi.unstubAllGlobals();
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
        seatMutateAsync: seatMutate,
      });
      render(
        <MemoryRouter initialEntries={["/multiplayer"]}>
          <HostControlTile />
        </MemoryRouter>,
      );
      return seatMutate;
    }

    it("does not promote an already installed concrete deck", async () => {
      // Guards the re-resolution loop.
      const onRandom = renderWithAiSeat({ type: "Random" });
      await waitFor(() => expect(onRandom).toHaveBeenCalledTimes(1));

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
      const seatMutate = vi.fn().mockResolvedValue(undefined);
      const seatMutateAsync = seatMutate;
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

    it("Add AI seats an empty deck for Dandan with an empty catalog", async () => {
      const { seatMutate } = renderSeats("Dandan", waiting);
      fireEvent.click(screen.getByRole("button", { name: "Add AI" }));
      await waitFor(() => expect(seatMutate).toHaveBeenCalledWith({
        ...setAi(EMPTY),
        data: { ...setAi(EMPTY).data, expectedKind: { type: "WaitingHuman" } },
      }));
    });

    it("Replace AI seats an empty deck for Dandan with an empty catalog", async () => {
      vi.stubGlobal("confirm", vi.fn(() => true));
      const { seatMutate } = renderSeats("Dandan", joined);
      fireEvent.click(screen.getByRole("button", { name: "Replace AI" }));
      await waitFor(() => expect(seatMutate).toHaveBeenCalledWith(setAi(EMPTY)));
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

    it("an AI seat holding Random is promoted to the empty deck for Dandan", async () => {
      const { seatMutate } = renderSeats("Dandan", aiWith({ type: "Random" }));
      await waitFor(() => expect(seatMutate).toHaveBeenCalledWith(setAi(EMPTY)));
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

    it("Standard with a catalog deck shows the dropdown and promotes Random", async () => {
      catalogMocks.candidates = [CATALOG_DECK];
      const { seatMutate } = renderSeats("Standard", aiWith({ type: "Random" }));
      expect(screen.getAllByRole("combobox")).toHaveLength(2);
      await waitFor(() => expect(seatMutate).toHaveBeenCalledWith(
        setAi({ type: "DeckList", data: expect.objectContaining({ main_deck: ["Forest"] }) }),
      ));
    });
  });
  describe("owner choices and preparation fence (V8/V8b)", () => {
    // ES2021 test compilation cannot use Promise.withResolvers.
    function heldMetadata() {
      let resolve!: () => void;
      const promise = new Promise<void>((done) => { resolve = done; });
      return { promise, resolve };
    }
    const A: AiDeckCandidate = {
      id: "saved:A", name: "A", source: { type: "saved" },
      deck: detectAndParseDeck("Deck\n2 Forest (AAA) 001"), coveragePct: 100, archetype: null, bracket: null,
    };
    const B: AiDeckCandidate = { ...A, id: "saved:B", name: "B", deck: detectAndParseDeck("Deck\n2 Forest (BBB) 002") };
    const host: PlayerSlot = { playerId: 0, name: "Host", kind: { type: "HostHuman" } };
    const waiting: PlayerSlot = { playerId: 1, name: "", kind: { type: "WaitingHuman" } };
    const joined: PlayerSlot = { playerId: 2, name: "Guest", kind: { type: "JoinedHuman" } };
    const printedA = [{ section: "main_deck" as const, name: "Forest", count: 2, printing: { set_code: "aaa", collector_number: "001" } }];
    const printedB = [{ section: "main_deck" as const, name: "Forest", count: 2, printing: { set_code: "bbb", collector_number: "002" } }];
    const seated: PlayerSlot = {
      playerId: 3, name: "AI",
      kind: { type: "Ai", data: { difficulty: "Hard", deck: { type: "DeckList", data: { ...expandParsedDeck(A.deck), printings: printedA } } } },
    };

    function mount(slots = [host, waiting, joined, seated]) {
      catalogMocks.candidates = [A, B];
      const dispatch = vi.fn<(mutation: SeatMutation) => Promise<void>>().mockResolvedValue(undefined);
      const immediate = vi.fn();
      const startNow = vi.fn().mockResolvedValue(undefined);
      useMultiplayerStore.setState({
        hostGameCode: "ABCDE", hostingStatus: "waiting",
        hostSession: { formatConfig: FORMAT_DEFAULTS.FreeForAll, timerSeconds: null, matchType: "Bo1" },
        playerSlots: slots, serverInfo: null, seatMutateAsync: dispatch, seatMutate: immediate,
        startLobbyWithCurrentPlayers: startNow,
      });
      const rendered = render(<MemoryRouter><HostControlTile /></MemoryRouter>);
      return { ...rendered, dispatch, immediate, startNow };
    }

    it("selects by candidate ID when names collide, excludes only artwork from matching and preserves seated art on difficulty changes", async () => {
      const { dispatch, immediate } = mount([host, seated]);
      const picker = screen.getAllByRole("combobox")[1];
      expect(picker).toHaveValue(A.id);
      fireEvent.change(picker, { target: { value: B.id } });
      await waitFor(() => expect(dispatch).toHaveBeenCalledOnce());
      const mutation = dispatch.mock.calls[0][0];
      expect(mutation).toEqual({ type: "SetKind", data: {
        seatIndex: 3, kind: { type: "Ai", data: { difficulty: "Hard", deck: { type: "DeckList", data: { ...expandParsedDeck(B.deck), printings: printedB } } } },
      } });
      if (mutation.type !== "SetKind") throw new Error("Expected assignment");
      act(() => useMultiplayerStore.setState({ playerSlots: [host, { ...seated, kind: mutation.data.kind }] }));
      act(() => usePreferencesStore.setState({ artChain: [{ type: "oldest" }] }));
      expect(picker).toHaveValue(B.id);
      expect(dispatch).toHaveBeenCalledOnce();
      fireEvent.change(screen.getAllByRole("combobox")[0], { target: { value: "Easy" } });
      expect(immediate).toHaveBeenCalledWith({ type: "SetKind", data: {
        seatIndex: 3, kind: { type: "Ai", data: { difficulty: "Easy", deck: { type: "DeckList", data: { ...expandParsedDeck(B.deck), printings: printedB } } } },
      } });
      // Optional nonprinting sections still distinguish otherwise equal lists.
      const planar = { ...B, id: "saved:Planar", deck: { ...B.deck, planar_deck: ["Plane"] } };
      catalogMocks.candidates = [A, B, planar];
      act(() => useMultiplayerStore.setState({ playerSlots: [host, {
        ...seated, kind: { type: "Ai", data: { difficulty: "Hard", deck: {
          type: "DeckList", data: { ...expandParsedDeck(planar.deck), printings: printedB },
        } } },
      }] }));
      expect(picker).toHaveValue(planar.id);
    });

    it.each(["add", "replace", "promote", "fill"] as const)("prepares %s before sending and binds the initial snapshot", async (action) => {
      const held = heldMetadata();
      art.face.mockImplementation(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      vi.spyOn(Math, "random").mockReturnValue(0);
      const slots = action === "promote"
        ? [host, { ...seated, kind: { type: "Ai" as const, data: { difficulty: "Hard", deck: { type: "Random" as const } } } }]
        : [host, waiting, joined, seated];
      const { dispatch } = mount(slots);
      if (action === "replace") vi.spyOn(window, "confirm").mockReturnValue(true);
      if (action !== "promote") fireEvent.click(screen.getByRole("button", { name: action === "add" ? "Add AI" : action === "replace" ? "Replace AI" : "Fill With AI" }));
      await waitFor(() => expect(art.face).toHaveBeenCalled());
      expect(dispatch).not.toHaveBeenCalled();
      act(() => usePreferencesStore.setState({ artOverrides: { Forest: { scryfallId: "b", setCode: "bbb", collectorNumber: "002" } } }));
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(action === "fill" ? 2 : 1));
      const assignment = dispatch.mock.calls[0][0];
      expect(assignment).toMatchObject({ type: "SetKind", data: { kind: { data: { deck: { data: { printings: printedA } } } } } });
      if (action === "fill") expect(dispatch.mock.calls[1][0]).toEqual({ type: "Start" });
    });

    it.each(["room", "session", "same-room"] as const)("F2 promotes the %s publication once after old preparation releases in the same tile", async (replacement) => {
      const held = heldMetadata();
      art.face.mockImplementationOnce(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      vi.spyOn(Math, "random").mockReturnValue(0);
      const { dispatch } = mount([host, waiting]);
      fireEvent.click(screen.getByRole("button", { name: "Add AI" }));
      await waitFor(() => expect(art.face).toHaveBeenCalledOnce());
      // Publish B and flush its effects BEFORE releasing A. Do not remount.
      await act(async () => {
        usePreferencesStore.setState({ artOverrides: { Forest: { scryfallId: "b", setCode: "bbb", collectorNumber: "002" } } });
        useMultiplayerStore.setState({
          ...(replacement === "room" ? { hostGameCode: "ROOMB" } : {}),
          ...(replacement !== "same-room" ? { hostSession: { ...useMultiplayerStore.getState().hostSession! } } : {}),
          playerSlots: [host, { ...waiting, name: "AI B", kind: {
            type: "Ai", data: { difficulty: "Hard", deck: replacement === "session" ? { type: "Named", data: "B" } : { type: "Random" } },
          } }],
        });
      });
      expect(screen.getByRole("button", { name: "Start Game" })).toBeDisabled();
      expect(dispatch).not.toHaveBeenCalled();
      expect(art.face).toHaveBeenCalledOnce();
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(dispatch).toHaveBeenCalledOnce());
      expect(dispatch).toHaveBeenCalledWith({ type: "SetKind", data: {
        seatIndex: 1, kind: { type: "Ai", data: {
          difficulty: "Hard", deck: { type: "DeckList", data: { ...expandParsedDeck(A.deck), printings: printedB } },
        } },
      } });
      await waitFor(() => expect(screen.getByRole("button", { name: "Start Game" })).toBeEnabled());
      // No transport publication: releasing B's latch cannot promote B again.
      await act(async () => { usePreferencesStore.setState({ artOverrides: {} }); });
      expect(dispatch).toHaveBeenCalledOnce();
      expect(art.face).toHaveBeenCalledTimes(2);
    });

    it("F2 unchanged-room promotion succeeds once without a transport echo", async () => {
      const held = heldMetadata();
      art.face.mockImplementationOnce(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      vi.spyOn(Math, "random").mockReturnValue(0);
      const { dispatch } = mount([host, { ...waiting, name: "AI", kind: {
        type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } },
      } }]);
      await waitFor(() => expect(art.face).toHaveBeenCalledOnce());
      expect(dispatch).not.toHaveBeenCalled();
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(screen.getByRole("button", { name: "Start Game" })).toBeEnabled());
      expect(dispatch).toHaveBeenCalledOnce();
      expect(dispatch.mock.calls[0][0]).toMatchObject({ type: "SetKind", data: { kind: { data: { deck: { data: { printings: printedA } } } } } });
      await act(async () => { usePreferencesStore.setState({ artOverrides: {} }); });
      expect(dispatch).toHaveBeenCalledOnce();
      expect(art.face).toHaveBeenCalledOnce();
    });

    it("disables every seat/Start/Fill control while held and refuses same-tick overlap through the ref", async () => {
      const held = heldMetadata();
      art.face.mockImplementation(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      const { dispatch, immediate, startNow } = mount();
      const add = screen.getByRole("button", { name: "Add AI" });
      const fill = screen.getByRole("button", { name: "Fill With AI" });
      act(() => { fireEvent.click(add); fireEvent.click(fill); fireEvent.click(add); });
      await waitFor(() => expect(art.face).toHaveBeenCalledOnce());
      for (const name of ["Add AI", "Replace AI", "Kick", "Human", "Start Now", "Fill With AI"]) {
        const control = screen.getByRole("button", { name });
        expect(control).toBeDisabled();
        fireEvent.click(control);
      }
      for (const control of screen.getAllByRole("button", { name: "Remove" })) expect(control).toBeDisabled();
      for (const control of screen.getAllByRole("combobox")) {
        expect(control).toBeDisabled();
        fireEvent.change(control, { target: { value: "Easy" } });
      }
      expect(immediate).not.toHaveBeenCalled();
      expect(startNow).not.toHaveBeenCalled();
      expect(dispatch).not.toHaveBeenCalled();
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(dispatch).toHaveBeenCalledOnce());
      expect(add).toBeEnabled();
      fireEvent.click(add);
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(2));
    });

    it.each(["unmount", "room", "session", "human", "remove", "renumber", "replace"] as const)("drops prepared work after %s changes the published target; a live sibling sends", async (change) => {
      const held = heldMetadata();
      art.face.mockImplementation(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      const { dispatch, unmount } = mount();
      if (change === "replace") vi.spyOn(window, "confirm").mockReturnValue(true);
      fireEvent.click(screen.getByRole("button", { name: change === "replace" ? "Replace AI" : "Fill With AI" }));
      await waitFor(() => expect(art.face).toHaveBeenCalledOnce());
      act(() => {
        if (change === "unmount") unmount();
        if (change === "room") useMultiplayerStore.setState({ hostGameCode: "OTHER" });
        if (change === "session") useMultiplayerStore.setState({ hostSession: { ...useMultiplayerStore.getState().hostSession! } });
        if (change === "human") useMultiplayerStore.setState({ playerSlots: [host, { ...waiting, name: "New guest", kind: { type: "JoinedHuman" } }, joined, seated] });
        if (change === "remove") useMultiplayerStore.setState({ playerSlots: [host, joined, seated] });
        if (change === "renumber") useMultiplayerStore.setState({ playerSlots: [host, { ...waiting, playerId: 4 }, joined, seated] });
        if (change === "replace") useMultiplayerStore.setState({ playerSlots: [host, waiting, { ...joined, name: "Different guest" }, seated] });
      });
      await act(async () => { held.resolve(); });
      expect(dispatch).not.toHaveBeenCalled();
      cleanup();
      const live = mount();
      fireEvent.click(screen.getByRole("button", { name: "Fill With AI" }));
      await waitFor(() => expect(live.dispatch).toHaveBeenLastCalledWith({ type: "Start" }));
      expect(live.dispatch.mock.calls[0][0]).toMatchObject({ type: "SetKind", data: { seatIndex: 1 } });
    });

    it.each(["single", "fill"] as const)("releases both latch halves after a rejected %s send and does not send the remaining batch or Start", async (kind) => {
      const { dispatch } = mount([host, waiting, { ...waiting, playerId: 2 }, { ...waiting, playerId: 3 }]);
      const error = new Error("send rejected");
      if (kind === "fill") dispatch.mockResolvedValueOnce(undefined).mockRejectedValueOnce(error);
      else dispatch.mockRejectedValueOnce(error);
      const control = kind === "fill" ? screen.getByRole("button", { name: "Fill With AI" }) : screen.getAllByRole("button", { name: "Add AI" })[0];
      fireEvent.click(control);
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(kind === "fill" ? 2 : 1));
      await waitFor(() => expect(control).toBeEnabled());
      expect(dispatch.mock.calls.some(([mutation]) => mutation.type === "Start")).toBe(false);
      dispatch.mockClear();
      fireEvent.click(control);
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(kind === "fill" ? 4 : 1));
      if (kind === "fill") expect(dispatch).toHaveBeenLastCalledWith({ type: "Start" });
      else expect(dispatch.mock.calls[0][0]).toMatchObject({ type: "SetKind", data: { seatIndex: 1 } });
    });

    const aiRandom: PlayerSlot = {
      ...waiting, name: "AI",
      kind: { type: "Ai", data: { difficulty: "Medium", deck: { type: "Random" } } },
    };

    it("retries a rejected AI promotion of the unchanged publication once and sends the concrete deck", async () => {
      useMultiplayerStore.setState({ toasts: new Map(), showToast: realShowToast }); // earlier tests leave toasts and a mock behind
      // The first pick draws A; the retry must draw fresh, so it draws B.
      vi.spyOn(Math, "random").mockReturnValueOnce(0).mockReturnValue(0.99);
      const held = heldMetadata();
      art.face.mockImplementationOnce(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      const { dispatch } = mount([host, aiRandom]);
      await waitFor(() => expect(art.face).toHaveBeenCalledOnce());
      expect(dispatch).not.toHaveBeenCalled(); // the promotion is held before its send
      const error = new Error("promotion rejected");
      dispatch.mockRejectedValueOnce(error);
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(2));
      await waitFor(() => expect(screen.getByRole("button", { name: "Start Game" })).toBeEnabled());
      expect(Array.from(useMultiplayerStore.getState().toasts.values(), ({ message }) => message)).toContain("promotion rejected");
      const assignments = dispatch.mock.calls.map(([mutation]) => mutation);
      expect(assignments[0]).toMatchObject({ type: "SetKind", data: { seatIndex: 1, kind: { type: "Ai", data: {
        difficulty: "Medium", deck: { type: "DeckList", data: { printings: printedA } } } } } });
      expect(assignments[1]).toMatchObject({ type: "SetKind", data: { seatIndex: 1, kind: { type: "Ai", data: {
        difficulty: "Medium", deck: { type: "DeckList", data: { printings: printedB } } } } } });
      await act(async () => {}); // flush pending effects
      expect(art.face).toHaveBeenCalledTimes(2); // the retry ran a fresh preparation; no third
      expect(dispatch).toHaveBeenCalledTimes(2); // the successful retry is not resubmitted (no echo)
    });

    it("stops after the retry is also rejected and promotes again when transport republishes the table", async () => {
      useMultiplayerStore.setState({ toasts: new Map() });
      vi.spyOn(Math, "random").mockReturnValue(0);
      const held = heldMetadata();
      art.face.mockImplementationOnce(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      const { dispatch } = mount([host, aiRandom]);
      await waitFor(() => expect(art.face).toHaveBeenCalledOnce());
      expect(dispatch).not.toHaveBeenCalled();
      const error = new Error("promotion rejected");
      dispatch.mockRejectedValueOnce(error).mockRejectedValueOnce(error);
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(2));
      await waitFor(() => expect(screen.getByRole("button", { name: "Start Game" })).toBeEnabled());
      await act(async () => {}); // flush pending effects
      expect(art.face).toHaveBeenCalledTimes(2); // bounded: no third preparation of the same publication
      expect(dispatch).toHaveBeenCalledTimes(2); // bounded: no third send
      await act(async () => {
        useMultiplayerStore.setState({ playerSlots: JSON.parse(JSON.stringify(useMultiplayerStore.getState().playerSlots)) });
      });
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(3));
      await waitFor(() => expect(screen.getByRole("button", { name: "Start Game" })).toBeEnabled());
      await act(async () => {}); // flush pending effects
      expect(art.face).toHaveBeenCalledTimes(3);
      expect(dispatch).toHaveBeenCalledTimes(3); // the republished promotion succeeded once and is not resubmitted
      expect(dispatch.mock.calls[2][0]).toMatchObject({ type: "SetKind", data: { seatIndex: 1 } });
    });

    it("retries a promotion whose rejection lands before the busy latch commits", async () => {
      useMultiplayerStore.setState({ toasts: new Map() });
      vi.spyOn(Math, "random").mockReturnValue(0);
      // No held metadata: preparation and the rejection settle on microtasks.
      // Guards "a rejection always wakes the retry"; under act() the latch alone
      // would also wake it, so this does not reproduce the batching itself.
      const { dispatch } = mount([host, aiRandom]);
      dispatch.mockRejectedValueOnce(new Error("promotion rejected"));
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(2));
      await waitFor(() => expect(screen.getByRole("button", { name: "Start Game" })).toBeEnabled());
      await act(async () => {}); // flush pending effects
      expect(dispatch).toHaveBeenCalledTimes(2); // exactly one send and one retry, no third and no transport echo
      expect(art.face).toHaveBeenCalledTimes(2); // one preparation per send
    });

    it("keeps local keys invariant under printing and tier changes, and distinct under list changes", () => {
      const names = expandParsedDeck(A.deck);
      const key = deckChoiceKey({ type: "DeckList", data: names });
      for (const printings of [[], printedA, printedB]) {
        expect(deckChoiceKey({ type: "DeckList", data: { ...names, printings } })).toBe(key);
      }
      // The declared tier is not part of which deck it is (upstream's deck identity key).
      expect(deckChoiceKey({ type: "DeckList", data: { ...names, bracket_tier: "core" } })).toBe(key);
      expect(deckChoiceKey({ type: "DeckList", data: { ...names, planar_deck: ["Plane"] } })).not.toBe(key);
      expect(deckChoiceKey({ type: "Random" })).toBe("Random");
      expect(deckChoiceKey({ type: "Named", data: "A" })).toBe("Named:A");
    });

    it("prepares the complete Fill batch once, tolerates unchanged republished seats and ignores later preference object edits", async () => {
      const held = heldMetadata();
      const override = { scryfallId: "a", setCode: "aaa", collectorNumber: "001" };
      usePreferencesStore.setState({ artOverrides: { Forest: override } });
      art.face.mockImplementation(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      const { dispatch } = mount([host, waiting, { ...waiting, playerId: 2 }, { ...waiting, playerId: 3 }]);
      fireEvent.click(screen.getByRole("button", { name: "Fill With AI" }));
      await waitFor(() => expect(art.face).toHaveBeenCalledTimes(3));
      expect(dispatch).not.toHaveBeenCalled();
      act(() => {
        override.scryfallId = "b";
        usePreferencesStore.setState({ artOverrides: { Forest: override } });
        useMultiplayerStore.setState({ playerSlots: JSON.parse(JSON.stringify(useMultiplayerStore.getState().playerSlots)) });
      });
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(dispatch).toHaveBeenCalledTimes(4));
      expect(dispatch.mock.calls.map(([mutation]) => mutation.type === "SetKind" ? mutation.data.seatIndex : mutation.type)).toEqual([1, 2, 3, "Start"]);
      for (const [mutation] of dispatch.mock.calls.slice(0, 3)) {
        expect(mutation).toMatchObject({ data: { kind: { data: { deck: { data: { printings: printedA } } } } } });
      }
    });

    it("gates Start Game during a deck change, then admits it after preparation", async () => {
      const held = heldMetadata();
      art.face.mockImplementation(async () => { await held.promise; return { scryfall_oracle_id: "Forest" }; });
      const { dispatch, immediate } = mount([host, seated]);
      fireEvent.change(screen.getAllByRole("combobox")[1], { target: { value: B.id } });
      await waitFor(() => expect(art.face).toHaveBeenCalledOnce());
      const start = screen.getByRole("button", { name: "Start Game" });
      expect(start).toBeDisabled();
      fireEvent.click(start);
      expect(immediate).not.toHaveBeenCalled();
      await act(async () => { held.resolve(); });
      await waitFor(() => expect(dispatch).toHaveBeenCalledOnce());
      expect(start).toBeEnabled();
      fireEvent.click(start);
      expect(immediate).toHaveBeenCalledWith({ type: "Start" });
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
      // The pick prepares its art and sends through the awaited path, so both entry points share one mock.
      const seatMutate = vi.fn().mockResolvedValue(undefined);
      catalogMocks.candidates = candidates;
      useMultiplayerStore.setState({
        hostGameCode: "ABCD1",
        hostingStatus: "waiting",
        hostSession: { formatConfig: FORMAT_DEFAULTS.Standard, timerSeconds: null, matchType: "Bo1" },
        playerSlots: [host, aiWith(deck)],
        serverInfo: null,
        seatMutate,
        seatMutateAsync: seatMutate,
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

    it("keeps the picked deck selected once the engine echoes it", async () => {
      const seatMutate = renderSeat([DECK_A, DECK_B, DECK_C], echo.plain);
      expect(seatMutate).not.toHaveBeenCalled();

      const select = deckSelect("A");
      const optionC = Array.from(select.options).find((option) => option.textContent === "C")!;
      fireEvent.change(select, { target: { value: optionC.value } });

      // Positive control: the pick reaches the catalog deck, companion and all. The pick
      // prepares its art first, so the send lands after the preparation settles.
      await waitFor(() => expect(seatMutate).toHaveBeenCalledTimes(1));
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

    it("keeps decks that differ only in card order apart through pick and echo", async () => {
      const seatMutate = renderSeat([DECK_FI, DECK_IF], echo.forestIsland);

      const select = deckSelect("FI");
      // Reach guard: both ordered decks are offered, and the seat holds the first.
      expect(Array.from(select.options, (option) => option.textContent)).toEqual(["FI", "IF"]);
      expect(selectedLabel(select)).toBe("FI");
      expect(select.options[0].value).not.toBe(select.options[1].value);

      fireEvent.change(select, { target: { value: select.options[1].value } });
      await waitFor(() => expect(seatMutate).toHaveBeenCalledTimes(1));
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
