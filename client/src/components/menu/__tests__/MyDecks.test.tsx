import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { MyDecks } from "../MyDecks";
import {
  RANDOM_DECK_SELECTION,
  createFolder,
  saveFeedSubscriptions,
  saveDeckOrigins,
  setDeckFolder,
  STORAGE_KEY_PREFIX,
} from "../../../constants/storage";
import type { ParsedDeck } from "../../../services/deckParser";
import { evaluateDeckCompatibilityBatch } from "../../../services/deckCompatibility";
import { setCachedFeed } from "../../../services/feedPersistence";
import { loadPreconDeckMap } from "../../../hooks/useDecks";
import { useConnectivityStore } from "../../../stores/connectivityStore";
import * as feedService from "../../../services/feedService";

const { useCardImage, useSetSymbol, advanceSetSource } = vi.hoisted(() => ({
  useCardImage: vi.fn(),
  useSetSymbol: vi.fn(),
  advanceSetSource: vi.fn(),
}));

vi.mock("../../../hooks/useCardImage", () => ({ useCardImage }));

vi.mock("../../../hooks/useBracketEstimate", () => ({
  useBracketEstimate: () => ({ estimate: null, loading: false, unsupported: false }),
}));

vi.mock("../../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({}),
}));

vi.mock("../../../hooks/useSetSymbols", () => ({
  useSetSymbol,
}));

vi.mock("../../../services/deckCompatibility", () => ({
  evaluateDeckCompatibilityBatch: vi.fn(),
}));

vi.mock("../../../hooks/useDecks", () => ({
  loadPreconDeckMap: vi.fn(),
  isCommanderPreconDeck: (deck: { type: string }) => deck.type === "Commander Deck",
  useDecks: vi.fn(() => ({ decks: null, status: "loading" as const })),
}));

function saveDeck(name: string, deck: ParsedDeck): void {
  localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(deck));
}

describe("MyDecks", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    useCardImage.mockReturnValue({ src: null, isLoading: false });
    useSetSymbol.mockImplementation((setCode: string | undefined) => ({
      src: setCode ? `visual-pack://set/${setCode.toLowerCase()}` : null,
      isLoading: false,
      advanceFailedSource: advanceSetSource,
    }));
    vi.mocked(loadPreconDeckMap).mockResolvedValue({});
    vi.stubGlobal("IntersectionObserver", class {
      private readonly callback: IntersectionObserverCallback;

      constructor(callback: IntersectionObserverCallback) {
        this.callback = callback;
      }

      observe(target: Element) {
        this.callback([{ isIntersecting: true, target } as IntersectionObserverEntry], this as unknown as IntersectionObserver);
      }

      disconnect() {}
      unobserve() {}
      takeRecords(): IntersectionObserverEntry[] { return []; }
    });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    vi.unstubAllGlobals();
  });

  it("keeps cached subscriptions usable while offline and re-enables refresh on reconnect", async () => {
    const feedDeck = {
      name: "Offline Feed Deck",
      colors: ["U"],
      main: [{ name: "Island", count: 60 }],
      sideboard: [],
    };
    saveDeck("Offline Feed Deck", { main: feedDeck.main, sideboard: [] });
    saveDeckOrigins({ "Offline Feed Deck": "offline-feed" });
    await setCachedFeed("offline-feed", {
      id: "offline-feed",
      name: "Offline Feed",
      version: 1,
      updated: "2026-01-01T00:00:00Z",
      decks: [feedDeck],
    });
    saveFeedSubscriptions([{
      sourceId: "offline-feed",
      url: "https://example.com/offline-feed.json",
      type: "remote",
      subscribedAt: 1,
      lastRefreshedAt: 1,
      lastVersion: 1,
    }]);
    const refreshAllFeeds = vi.spyOn(feedService, "refreshAllFeeds");
    const onEditDeck = vi.fn();
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});
    const user = userEvent.setup();
    render(
      <MyDecks
        mode="manage"
        activeDeckName={null}
        onCreateDeck={vi.fn()}
        onEditDeck={onEditDeck}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Subscriptions" }));
    expect(await screen.findByText("Offline Feed")).toBeInTheDocument();
    expect(screen.getByText("Offline Feed Deck")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Refresh All" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Manage Feeds" })).toBeEnabled();

    act(() => useConnectivityStore.getState().setForcedOffline(true));

    expect(screen.getByText(/Feed updates are unavailable while offline/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Refresh All" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Manage Feeds" })).toBeEnabled();
    await user.click(screen.getByText("Offline Feed Deck"));
    expect(onEditDeck).toHaveBeenCalledWith("Offline Feed Deck");
    vi.stubGlobal("prompt", vi.fn(() => "Offline Feed Copy"));
    await user.click(screen.getByRole("button", { name: "Copy to My Decks" }));
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Offline Feed Copy")).not.toBeNull();
    expect(feedService.getDeckFeedOrigin("Offline Feed Copy")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Refresh All" }));
    expect(refreshAllFeeds).not.toHaveBeenCalled();

    act(() => useConnectivityStore.getState().setForcedOffline(false));

    expect(screen.getByRole("button", { name: "Refresh All" })).toBeEnabled();
  });

  it("checks commander selection context and can reveal incompatible decks on demand", async () => {
    saveDeck("Commander Ready", {
      main: [{ name: "Island", count: 99 }],
      sideboard: [],
      commander: ["Atraxa, Praetors' Voice"],
    });
    saveDeck("Off Format", {
      main: [{ name: "Lightning Bolt", count: 60 }],
      sideboard: [],
      commander: [],
    });

    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({
      "Commander Ready": {
        standard: { compatible: false, reasons: [] },
        commander: { compatible: true, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: true,
        selected_format_reasons: [],
        color_identity: ["U"],
      },
      "Off Format": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: ["Not Commander legal"] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: false,
        selected_format_reasons: ["Not Commander legal"],
        color_identity: ["R"],
      },
    });

    render(
      <MyDecks
        mode="select"
        selectedFormat="Commander"
        activeDeckName={null}
        onSelectDeck={vi.fn()}
        onConfirmSelection={vi.fn()}
      />,
    );

    expect(await screen.findByText("Commander Ready")).toBeInTheDocument();
    await waitFor(() =>
      expect(evaluateDeckCompatibilityBatch).toHaveBeenCalledWith(
        expect.arrayContaining([
          expect.objectContaining({ name: "Commander Ready" }),
          expect.objectContaining({ name: "Off Format" }),
        ]),
        expect.objectContaining({ selectedFormat: "Commander" }),
      ),
    );
    expect(screen.getByText("Off Format")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show all decks" })).toBeInTheDocument();
  });

  it("does not prefilter in free-for-all context", async () => {
    saveDeck("Deck Alpha", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    saveDeck("Deck Beta", { main: [{ name: "Mountain", count: 60 }], sideboard: [] });

    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({
      "Deck Alpha": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: true,
        selected_format_reasons: [],
        color_identity: ["U"],
      },
      "Deck Beta": {
        standard: { compatible: false, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: true,
        selected_format_reasons: [],
        color_identity: ["R"],
      },
    });

    render(
      <MyDecks
        mode="select"
        selectedFormat="FreeForAll"
        activeDeckName={null}
        onSelectDeck={vi.fn()}
        onConfirmSelection={vi.fn()}
      />,
    );

    expect(await screen.findByText("Deck Alpha")).toBeInTheDocument();
    expect(screen.getByText("Deck Beta")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Show all decks" })).not.toBeInTheDocument();
  });

  it("renders only compatible format badges from engine evaluation", async () => {
    saveDeck("Badge Deck", { main: [{ name: "Island", count: 60 }], sideboard: [] });

    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({
      "Badge Deck": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: ["Missing commander"] },
        bo3_ready: true,
        unknown_cards: ["Mystery Card"],
        selected_format_compatible: null,
        selected_format_reasons: [],
        color_identity: ["U"],
      },
    });

    render(
      <MyDecks
        mode="select"
        selectedFormat="Standard"
        activeDeckName="Badge Deck"
        onSelectDeck={vi.fn()}
        onConfirmSelection={vi.fn()}
      />,
    );

    expect(await screen.findAllByText("Badge Deck")).not.toHaveLength(0);
    expect(await screen.findByText("STD")).toBeInTheDocument();
    expect(screen.queryByText("CMD")).not.toBeInTheDocument();
    expect(await screen.findByText("BO3", { selector: "span" })).toBeInTheDocument();
    expect(await screen.findByText("Unknown 1")).toBeInTheDocument();
  });

  it("uses supported game formats as deck filters without offering BO3 as a format", async () => {
    saveDeck("PDH Ready", {
      main: [{ name: "Island", count: 99 }],
      sideboard: [],
      commander: ["Tatyova, Benthic Druid"],
    });
    saveDeck("Not PDH", {
      main: [{ name: "Lightning Bolt", count: 60 }],
      sideboard: [],
      commander: [],
    });

    vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation(async (_decks, options) => ({
      "PDH Ready": {
        standard: { compatible: false, reasons: [] },
        commander: { compatible: true, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: options?.selectedFormat === "PauperCommander" ? true : null,
        selected_format_reasons: [],
        color_identity: ["U", "G"],
      },
      "Not PDH": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: options?.selectedFormat === "PauperCommander" ? false : null,
        selected_format_reasons: [],
        color_identity: ["R"],
      },
    }));

    render(
      <MyDecks
        mode="manage"
        activeDeckName={null}
        onCreateDeck={vi.fn()}
        onEditDeck={vi.fn()}
      />,
    );

    expect(await screen.findByText("PDH Ready")).toBeInTheDocument();
    expect(screen.getByText("Not PDH")).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Format" }));
    expect(screen.getByRole("option", { name: "Pauper Commander" })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: "BO3" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("option", { name: "Pauper Commander" }));

    expect(await screen.findByText("Not PDH")).toBeInTheDocument();
    expect(screen.getByText("PDH Ready")).toBeInTheDocument();
    expect(vi.mocked(evaluateDeckCompatibilityBatch).mock.calls).toContainEqual([
      expect.any(Array),
      expect.objectContaining({
        selectedFormat: "PauperCommander",
        selectedMatchType: undefined,
      }),
    ]);
  });

  it("keeps the import tile available when a format has no decks", async () => {
    render(
      <MyDecks
        mode="manage"
        activeDeckName={null}
        onCreateDeck={vi.fn()}
        onEditDeck={vi.fn()}
      />,
    );

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Pioneer" }));

    expect(await screen.findByRole("button", { name: "Import Deck" })).toBeInTheDocument();
  });

  it("moves focus to deck search after deleting a folder in selection mode", async () => {
    saveDeck("Filed Deck", {
      main: [{ name: "Island", count: 60 }],
      sideboard: [],
    });
    const folder = createFolder("Archive");
    expect(folder).not.toBeNull();
    setDeckFolder("Filed Deck", folder!.id);
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});

    render(
      <MyDecks
        mode="select"
        activeDeckName={null}
        onSelectDeck={vi.fn()}
      />,
    );

    expect(await screen.findByText("Archive")).toBeInTheDocument();
    const folderTrigger = screen.getByRole("button", { name: "Folder options" });
    fireEvent.click(folderTrigger);
    const deleteItem = screen.getByRole("menuitem", { name: "Delete" });
    deleteItem.focus();
    fireEvent.click(deleteItem);

    const confirmation = await screen.findByRole("alertdialog", {
      name: "Delete",
    });
    fireEvent.click(within(confirmation).getByRole("button", { name: "Delete" }));

    await waitFor(() => expect(confirmation).not.toBeInTheDocument());
    expect(screen.queryByText("Archive")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveFocus();
  });

  it("uses trusted feed format metadata before background coverage filters unknown saved decks", async () => {
    saveDeck("Known Standard", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    saveDeck("Unknown User Deck", { main: [{ name: "Mountain", count: 60 }], sideboard: [] });
    saveDeckOrigins({ "Known Standard": "mtggoldfish-standard" });

    vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation(async (_decks, options) => ({
      "Unknown User Deck": {
        standard: { compatible: false, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: options?.selectedFormat === "Standard" ? false : null,
        selected_format_reasons: options?.selectedFormat === "Standard" ? ["Not Standard legal"] : [],
        color_identity: ["R"],
      },
    }));

    render(
      <MyDecks
        mode="manage"
        activeDeckName={null}
        onCreateDeck={vi.fn()}
        onEditDeck={vi.fn()}
      />,
    );

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Standard" }));

    expect(await screen.findByText("Known Standard")).toBeInTheDocument();
    expect(screen.getByText("Unknown User Deck")).toBeInTheDocument();
    const standardCalls = vi.mocked(evaluateDeckCompatibilityBatch).mock.calls.filter(
      ([, options]) => options?.selectedFormat === "Standard",
    );
    expect(standardCalls.length).toBeGreaterThan(0);
  });

  it("offers an edit action in selection mode without selecting the deck", async () => {
    saveDeck("Selectable Deck", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({
      "Selectable Deck": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: null,
        selected_format_reasons: [],
        color_identity: ["U"],
      },
    });
    const onSelectDeck = vi.fn();
    const onEditDeck = vi.fn();

    render(
      <MyDecks
        mode="select"
        activeDeckName={null}
        onSelectDeck={onSelectDeck}
        onEditDeck={onEditDeck}
      />,
    );

    expect(await screen.findByText("Selectable Deck")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Edit Selectable Deck" }));

    expect(onEditDeck).toHaveBeenCalledWith("Selectable Deck");
    expect(onSelectDeck).not.toHaveBeenCalled();
  });

  it("shows legal precons in a newest-first load-more section and saves one when selected", async () => {
    const setSymbolResult = {
      src: "visual-pack://set/sos" as string | null,
      isLoading: false,
      advanceFailedSource: advanceSetSource,
    };
    useSetSymbol.mockReturnValue(setSymbolResult);
    vi.mocked(loadPreconDeckMap).mockResolvedValue({
      ...Object.fromEntries(Array.from({ length: 12 }, (_, i) => [`deck-${i}`, {
        code: `P${i}`,
        name: `Precon ${i}`,
        type: "Commander Deck",
        releaseDate: `2026-01-${String(i + 1).padStart(2, "0")}`,
        coveragePct: 100,
        mainBoard: [{ name: "Island", count: 99 }],
        sideBoard: [],
        commander: [{ name: "Zimone, Mystery Unraveler", count: 1 }],
      }])),
      secrets: {
        code: "SOS",
        name: "Secrets of Strixhaven",
        type: "Commander Deck",
        releaseDate: "2026-02-01",
        coveragePct: 100,
        mainBoard: [{ name: "Island", count: 99 }],
        sideBoard: [],
        commander: [{ name: "Zimone, Mystery Unraveler", count: 1 }],
      },
    });
    vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation(async (decks) => {
      return Object.fromEntries(decks.map(({ name }) => [name, {
        standard: { compatible: false, reasons: [] },
        commander: { compatible: true, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: true,
        selected_format_reasons: [],
        color_identity: ["U"],
      }]));
    });
    const onSelectDeck = vi.fn();
    const onEditDeck = vi.fn();
    let activeDeckName: string | null = null;

    const renderDecks = () => (
      <MyDecks
        mode="select"
        selectedFormat="Commander"
        activeDeckName={activeDeckName}
        onSelectDeck={onSelectDeck}
        onEditDeck={onEditDeck}
      />
    );
    const { rerender } = render(renderDecks());

    expect(await screen.findByText("Secrets of Strixhaven (SOS)")).toBeInTheDocument();
    const installed = screen.getByAltText("SOS set icon");
    expect(installed).toHaveAttribute("src", "visual-pack://set/sos");
    fireEvent.error(installed);
    expect(advanceSetSource).toHaveBeenCalledWith("visual-pack://set/sos");

    setSymbolResult.src = "https://svgs.scryfall.io/sets/sos.svg";
    activeDeckName = "[Pre-built] Secrets of Strixhaven (SOS)";
    rerender(renderDecks());
    const remote = screen.getByAltText("SOS set icon");
    expect(remote).toHaveAttribute("src", "https://svgs.scryfall.io/sets/sos.svg");
    fireEvent.error(remote);
    expect(advanceSetSource).toHaveBeenLastCalledWith(
      "https://svgs.scryfall.io/sets/sos.svg",
    );

    setSymbolResult.src = null;
    activeDeckName = null;
    rerender(renderDecks());
    expect(screen.queryByAltText("SOS set icon")).not.toBeInTheDocument();
    expect(screen.getByTitle("SOS")).toHaveTextContent("SOS");

    expect(screen.queryByText("Precon 0 (P0)")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Load More" }));
    expect(await screen.findByText("Precon 0 (P0)")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Edit Secrets of Strixhaven (SOS)" }));
    expect(onEditDeck).toHaveBeenCalledWith("[Pre-built] Secrets of Strixhaven (SOS)");
    expect(localStorage.getItem(`${STORAGE_KEY_PREFIX}[Pre-built] Secrets of Strixhaven (SOS)`)).toBeNull();

    await userEvent.click(screen.getByText("Secrets of Strixhaven (SOS)"));

    expect(onSelectDeck).toHaveBeenCalledWith("[Pre-built] Secrets of Strixhaven (SOS)");
    expect(localStorage.getItem(`${STORAGE_KEY_PREFIX}[Pre-built] Secrets of Strixhaven (SOS)`)).toBeTruthy();
    expect(loadPreconDeckMap).toHaveBeenCalled();
  });

  it("does not fall through to saved-deck art for a basic-only precon override", async () => {
    let resolvePrecons: (
      value: Awaited<ReturnType<typeof loadPreconDeckMap>>,
    ) => void = () => {};
    vi.mocked(loadPreconDeckMap).mockReturnValue(new Promise((resolve) => {
      resolvePrecons = resolve;
    }));

    render(
      <MyDecks
        mode="select"
        selectedFormat="Commander"
        activeDeckName={null}
        onSelectDeck={vi.fn()}
      />,
    );

    // The catalog already captured its saved-deck-name snapshot. Adding this
    // same-name deck now creates the exact hostile condition: a precon tile
    // exists, but an incorrect representative branch could still consult
    // local storage after finding no non-basic precon card.
    saveDeck("Basics Only (BAS)", {
      main: [{ name: "Saved Fallback", count: 1 }],
      sideboard: [],
    });
    resolvePrecons({
      basics: {
        code: "BAS",
        name: "Basics Only",
        type: "Commander Deck",
        releaseDate: "2026-02-01",
        coveragePct: 100,
        mainBoard: [{ name: "Forest", count: 100 }],
        sideBoard: [],
      },
    });

    expect(await screen.findByText("Basics Only (BAS)")).toBeInTheDocument();
    expect(useCardImage).toHaveBeenCalledWith("", {
      size: "art_crop",
      sourcePrinting: undefined,
    });
    expect(useCardImage).not.toHaveBeenCalledWith("Saved Fallback", expect.anything());
  });

  it("filters selection decks by source and precon set", async () => {
    saveDeck("User Commander", {
      main: [{ name: "Island", count: 99 }],
      sideboard: [],
      commander: ["Zimone, Mystery Unraveler"],
    });
    saveDeck("[Pre-built] Secrets of Strixhaven (SOS)", {
      main: [{ name: "Island", count: 99 }],
      sideboard: [],
      commander: ["Zimone, Mystery Unraveler"],
    });
    vi.mocked(loadPreconDeckMap).mockResolvedValue({
      sos: {
        code: "SOS",
        name: "Secrets of Strixhaven",
        type: "Commander Deck",
        releaseDate: "2026-02-01",
        coveragePct: 100,
        mainBoard: [{ name: "Island", count: 99 }],
        sideBoard: [],
        commander: [{ name: "Zimone, Mystery Unraveler", count: 1 }],
      },
      p0: {
        code: "P0",
        name: "Precon Zero",
        type: "Commander Deck",
        releaseDate: "2026-01-01",
        coveragePct: 100,
        mainBoard: [{ name: "Forest", count: 99 }],
        sideBoard: [],
        commander: [{ name: "Tatyova, Benthic Druid", count: 1 }],
      },
    });

    render(
      <MyDecks
        mode="select"
        selectedFormat="Commander"
        activeDeckName={null}
        onSelectDeck={vi.fn()}
      />,
    );

    expect(await screen.findByText("User Commander")).toBeInTheDocument();
    expect(await screen.findByText("Secrets of Strixhaven (SOS)")).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Deck source" }));
    await user.click(screen.getByRole("option", { name: "My decks" }));

    expect(screen.getByText("User Commander")).toBeInTheDocument();
    expect(screen.queryByText("Secrets of Strixhaven (SOS)")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Deck source" }));
    await user.click(screen.getByRole("option", { name: "Precons" }));

    expect(screen.queryByText("User Commander")).not.toBeInTheDocument();
    expect(screen.getByText("Secrets of Strixhaven (SOS)")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Precon set" }));
    await user.click(screen.getByRole("option", { name: "P0" }));

    expect(screen.queryByText("Secrets of Strixhaven (SOS)")).not.toBeInTheDocument();
    expect(screen.getByText("Precon Zero (P0)")).toBeInTheDocument();
  });

  it("random selection prefers exact-format feed decks over incompatible user decks", async () => {
    saveDeck("Known Standard", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    saveDeck("User Pauper", { main: [{ name: "Lightning Bolt", count: 60 }], sideboard: [] });
    saveDeckOrigins({ "Known Standard": "mtggoldfish-standard" });
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({
      "User Pauper": {
        standard: { compatible: false, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: false,
        selected_format_reasons: ["Not Standard legal"],
        color_identity: ["R"],
      },
    });
    const onSelectDeck = vi.fn();

    render(
      <MyDecks
        mode="select"
        selectedFormat="Standard"
        activeDeckName={null}
        onSelectDeck={onSelectDeck}
      />,
    );

    expect(await screen.findByText("Known Standard")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Random Deck" }));

    await waitFor(() => expect(onSelectDeck).toHaveBeenCalledWith("Known Standard"));
    expect(onSelectDeck).not.toHaveBeenCalledWith("User Pauper");
  });

  it("can defer random selection without materializing a concrete deck", async () => {
    saveDeck("Known Standard", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    const onSelectDeck = vi.fn();

    render(
      <MyDecks
        mode="select"
        selectedFormat="Standard"
        activeDeckName={null}
        onSelectDeck={onSelectDeck}
        randomSelectionMode="defer"
      />,
    );

    expect(await screen.findByText("Known Standard")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Random Deck" }));

    expect(onSelectDeck).toHaveBeenCalledWith(RANDOM_DECK_SELECTION);
    expect(evaluateDeckCompatibilityBatch).not.toHaveBeenCalledWith(
      expect.any(Array),
      expect.objectContaining({ summaryOnly: true }),
    );
  });

  it("preserves saved cover printing identity and advances the installed art crop", async () => {
    const advanceFailedSource = vi.fn();
    useCardImage.mockReturnValue({
      src: "visual-pack://installed/deck-art",
      isLoading: false,
      advanceFailedSource,
    });
    saveDeck("Printed Deck", {
      main: [
        { name: "Island", count: 20 },
        {
          name: "Opt",
          count: 4,
          sourcePrinting: { setCode: "DAR", collectorNumber: "60" },
        },
      ],
      sideboard: [],
    });
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({
      "Printed Deck": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: true,
        selected_format_reasons: [],
        color_identity: ["U"],
      },
    });

    const { container } = render(
      <MyDecks
        mode="select"
        selectedFormat="Standard"
        activeDeckName={null}
        onSelectDeck={vi.fn()}
      />,
    );

    expect(await screen.findByText("Printed Deck")).toBeInTheDocument();
    expect(useCardImage).toHaveBeenCalledWith("Opt", {
      size: "art_crop",
      sourcePrinting: { setCode: "DAR", collectorNumber: "60" },
    });
    const image = container.querySelector('img[src="visual-pack://installed/deck-art"]');
    expect(image).not.toBeNull();
    fireEvent.error(image!);
    expect(advanceFailedSource).toHaveBeenCalledWith("visual-pack://installed/deck-art");
  });
});
