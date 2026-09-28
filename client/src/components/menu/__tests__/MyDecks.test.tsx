import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { MyDecks } from "../MyDecks";
import {
  ACTIVE_DECK_KEY,
  RANDOM_DECK_SELECTION,
  createFolder,
  getDeckMeta,
  listFolders,
  removeDeckMeta,
  removeSavedDeckData,
  saveFeedSubscriptions,
  saveDeckOrigins,
  setDeckFolder,
  stampDeckMeta,
  toggleDeckStar,
  writeSavedDeckData,
  STORAGE_KEY_PREFIX,
} from "../../../constants/storage";
import type { ParsedDeck } from "../../../services/deckParser";
import {
  awaitSavedDeckLibraryIdle,
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  testSavedDeckTxn,
  uninstallWebLocks,
} from "../../../test/helpers/webLocks";
import { setSavedDeckTxnLockWaitForTests, withSavedDeckLibrary } from "../../../services/savedDeckTransaction";
import { useAppNotificationStore } from "../../../stores/appToastStore";
import { evaluateDeckCompatibilityBatch } from "../../../services/deckCompatibility";
import { setCachedFeed } from "../../../services/feedPersistence";
import { loadPreconDeckMap, useDecks } from "../../../hooks/useDecks";
import type { DeckMap } from "../../../hooks/useDecks";
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

function commanderPrecon() {
  return {
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
  };
}

function compatibleBatch(decks: Array<{ name: string }>) {
  return Object.fromEntries(decks.map(({ name }) => [name, {
    standard: { compatible: false, reasons: [] },
    commander: { compatible: true, reasons: [] },
    bo3_ready: false,
    unknown_cards: [],
    selected_format_compatible: true,
    selected_format_reasons: [],
    color_identity: ["U"],
    color_distribution: [],
  }]));
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
        color_distribution: [],
      },
      "Off Format": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: ["Not Commander legal"] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: false,
        selected_format_reasons: ["Not Commander legal"],
        color_identity: ["R"],
        color_distribution: [],
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
        color_distribution: [],
      },
      "Deck Beta": {
        standard: { compatible: false, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: true,
        selected_format_reasons: [],
        color_identity: ["R"],
        color_distribution: [],
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
        color_distribution: [],
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
        color_distribution: [],
      },
      "Not PDH": {
        standard: { compatible: true, reasons: [] },
        commander: { compatible: false, reasons: [] },
        bo3_ready: false,
        unknown_cards: [],
        selected_format_compatible: options?.selectedFormat === "PauperCommander" ? false : null,
        selected_format_reasons: [],
        color_identity: ["R"],
        color_distribution: [],
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
    const folder = createFolder(testSavedDeckTxn, "Archive");
    expect(folder).not.toBeNull();
    setDeckFolder(testSavedDeckTxn, "Filed Deck", folder!.id);
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
        color_distribution: [],
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
        color_distribution: [],
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
        color_distribution: [],
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

  it("selecting a precon refused by a busy library shows the busy toast and does not select the deck", async () => {
    vi.mocked(loadPreconDeckMap).mockResolvedValue({
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
        color_distribution: [],
      }]));
    });
    const onSelectDeck = vi.fn();

    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });

    render(
      <MyDecks
        mode="select"
        selectedFormat="Commander"
        activeDeckName={null}
        onSelectDeck={onSelectDeck}
      />,
    );
    expect(await screen.findByText("Secrets of Strixhaven (SOS)")).toBeInTheDocument();

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    setSavedDeckTxnLockWaitForTests(20);
    await userEvent.click(screen.getByText("Secrets of Strixhaven (SOS)"));

    await vi.waitFor(() => {
      expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't save deck");
    });
    expect(onSelectDeck).not.toHaveBeenCalled();

    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    releaseHolder();
    await holder;
    uninstallWebLocks();
  });

  it("a delete queued behind a transaction that replaces the deck leaves the replacement's data, metadata and active pointer", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests(); // clears localStorage; seed the deck after
    saveDeck("Deck Alpha", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    stampDeckMeta(testSavedDeckTxn, "Deck Alpha", 1000);
    localStorage.setItem(ACTIVE_DECK_KEY, "Deck Alpha");
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });

    render(<MyDecks mode="manage" activeDeckName={null} onCreateDeck={vi.fn()} onEditDeck={vi.fn()} />);
    expect(await screen.findByText("Deck Alpha")).toBeInTheDocument();

    let release!: () => void;
    const held = new Promise<void>((r) => { release = r; });
    const holder = withSavedDeckLibrary(async (txn) => {
      await held;
      removeSavedDeckData(txn, "Deck Alpha");
      removeDeckMeta(txn, "Deck Alpha");
      writeSavedDeckData(txn, "Deck Alpha", JSON.stringify({ main: [{ name: "Mountain", count: 60 }], sideboard: [] }));
      stampDeckMeta(txn, "Deck Alpha", 2000);
      toggleDeckStar(txn, "Deck Alpha");
      localStorage.setItem(ACTIVE_DECK_KEY, "Deck Alpha");
    });
    await vi.waitFor(async () => { expect((await navigator.locks.query()).held).toHaveLength(1); });

    await userEvent.click(screen.getByTitle("Delete deck"));
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));
    await vi.waitFor(async () => { expect((await navigator.locks.query()).pending).toHaveLength(1); });

    release();
    await holder;
    await awaitSavedDeckLibraryIdle();

    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck Alpha") ?? "{}").main).toEqual([
      { name: "Mountain", count: 60 },
    ]);
    expect(getDeckMeta("Deck Alpha")).toEqual({ addedAt: 2000, starred: true });
    expect(localStorage.getItem(ACTIVE_DECK_KEY)).toBe("Deck Alpha");
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Couldn't delete deck",
      description: "This deck changed before your action ran, so nothing was changed. Check the deck and try again.",
    });
    uninstallWebLocks();
  });

  it("a delete queued behind a transaction that leaves the deck unchanged still deletes it", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
    saveDeck("Deck Alpha", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    stampDeckMeta(testSavedDeckTxn, "Deck Alpha", 1000);
    localStorage.setItem(ACTIVE_DECK_KEY, "Deck Alpha");
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });

    render(<MyDecks mode="manage" activeDeckName={null} onCreateDeck={vi.fn()} onEditDeck={vi.fn()} />);
    expect(await screen.findByText("Deck Alpha")).toBeInTheDocument();

    let release!: () => void;
    const held = new Promise<void>((r) => { release = r; });
    // The control: the same holder shape, minus the replacement — this queued action still
    // finds "Deck Alpha" unchanged and deletes it.
    const holder = withSavedDeckLibrary(async () => { await held; });
    await vi.waitFor(async () => { expect((await navigator.locks.query()).held).toHaveLength(1); });

    await userEvent.click(screen.getByTitle("Delete deck"));
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));
    await vi.waitFor(async () => { expect((await navigator.locks.query()).pending).toHaveLength(1); });

    release();
    await holder;
    await awaitSavedDeckLibraryIdle();

    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck Alpha")).toBeNull();
    expect(getDeckMeta("Deck Alpha")).toBeNull();
    expect(localStorage.getItem(ACTIVE_DECK_KEY)).toBeNull();
    expect(useAppNotificationStore.getState().notification).toBeNull();
    uninstallWebLocks();
  });

  it("deleting a precon tile that is not saved writes nothing and shows no toast", async () => {
    vi.mocked(loadPreconDeckMap).mockResolvedValue(commanderPrecon());
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });

    render(
      <MyDecks mode="manage" selectedFormat="Commander" activeDeckName={null} onCreateDeck={vi.fn()} onEditDeck={vi.fn()} />,
    );
    // Positive: the precon tile renders, so the click below reaches handleDeleteDeck. Scoped to
    // this tile — the bundled cEDH demo decks render "Delete deck" controls of their own.
    const tileText = await screen.findByText("Secrets of Strixhaven (SOS)");
    const tile = tileText.closest("[role=\"button\"]") as HTMLElement;
    await userEvent.click(within(tile).getByTitle("Delete deck"));
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));

    expect(useAppNotificationStore.getState().notification).toBeNull();
    expect(localStorage.getItem(`${STORAGE_KEY_PREFIX}[Pre-built] Secrets of Strixhaven (SOS)`)).toBeNull();
  });

  it("deleting a deck refused by a busy library keeps the deck and shows the busy toast", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests(); // clears localStorage; seed the deck after
    saveDeck("Deck Alpha", { main: [{ name: "Island", count: 60 }], sideboard: [] });
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });

    render(
      <MyDecks
        mode="manage"
        activeDeckName={null}
        onCreateDeck={vi.fn()}
        onEditDeck={vi.fn()}
      />,
    );
    expect(await screen.findByText("Deck Alpha")).toBeInTheDocument();

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    setSavedDeckTxnLockWaitForTests(20);
    await userEvent.click(screen.getByTitle("Delete deck"));
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));

    await vi.waitFor(() => {
      expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't delete deck");
    });
    expect(screen.getByText("Deck Alpha")).toBeInTheDocument();
    expect(localStorage.getItem(`${STORAGE_KEY_PREFIX}Deck Alpha`)).toBeTruthy();

    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    releaseHolder();
    await holder;
    uninstallWebLocks();
  });

  it("adopting a feed deck refused by a busy library shows the busy toast and saves nothing", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests(); // clears localStorage; seed the feed deck after
    const feedDeck = {
      name: "Feed Deck",
      colors: ["U"],
      main: [{ name: "Island", count: 60 }],
      sideboard: [],
    };
    saveDeck("Feed Deck", { main: feedDeck.main, sideboard: [] });
    saveDeckOrigins({ "Feed Deck": "some-feed" });
    await setCachedFeed("some-feed", {
      id: "some-feed",
      name: "Some Feed",
      version: 1,
      updated: "2026-01-01T00:00:00Z",
      decks: [feedDeck],
    });
    saveFeedSubscriptions([{
      sourceId: "some-feed",
      url: "https://example.com/some-feed.json",
      type: "remote",
      subscribedAt: 1,
      lastRefreshedAt: 1,
      lastVersion: 1,
    }]);
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    vi.stubGlobal("prompt", vi.fn(() => "Feed Deck Copy"));

    render(
      <MyDecks
        mode="manage"
        activeDeckName={null}
        onCreateDeck={vi.fn()}
        onEditDeck={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Subscriptions" }));
    expect(await screen.findByText("Feed Deck")).toBeInTheDocument();

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    setSavedDeckTxnLockWaitForTests(20);
    await userEvent.click(screen.getByRole("button", { name: "Copy to My Decks" }));

    await vi.waitFor(() => {
      expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't save deck");
    });
    expect(localStorage.getItem(`${STORAGE_KEY_PREFIX}Feed Deck Copy`)).toBeNull();

    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    releaseHolder();
    await holder;
    uninstallWebLocks();
  });

  it("refreshing all feeds refused by a busy library shows the busy toast without an unhandled rejection", async () => {
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests(); // clears localStorage; seed the subscription after
    await setCachedFeed("some-feed", {
      id: "some-feed",
      name: "Some Feed",
      version: 1,
      updated: "2026-01-01T00:00:00Z",
      decks: [{ name: "Some Feed Deck", colors: [], main: [{ name: "Island", count: 60 }], sideboard: [] }],
    });
    saveFeedSubscriptions([{
      sourceId: "some-feed",
      url: "https://example.com/some-feed.json",
      type: "remote",
      subscribedAt: 1,
      lastRefreshedAt: 1,
      lastVersion: 1,
    }]);
    vi.mocked(evaluateDeckCompatibilityBatch).mockResolvedValue({});
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      statusText: "OK",
      json: () => Promise.resolve({
        id: "some-feed",
        name: "Some Feed",
        version: 2,
        updated: "2026-01-02T00:00:00Z",
        decks: [{ name: "Fresh Feed Deck", colors: [], main: [{ name: "Island", count: 60 }], sideboard: [] }],
      }),
    }));

    render(
      <MyDecks
        mode="manage"
        activeDeckName={null}
        onCreateDeck={vi.fn()}
        onEditDeck={vi.fn()}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Subscriptions" }));
    expect(await screen.findByText("Some Feed")).toBeInTheDocument();

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    setSavedDeckTxnLockWaitForTests(20);
    await userEvent.click(screen.getByRole("button", { name: "Refresh All" }));

    await vi.waitFor(() => {
      expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't update deck feeds");
    });
    expect(localStorage.getItem(`${STORAGE_KEY_PREFIX}Fresh Feed Deck`)).toBeNull();

    setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    releaseHolder();
    await holder;
    uninstallWebLocks();
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
        color_distribution: [],
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
        color_distribution: [],
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

  describe("a newer choice supersedes one still saving or checking compatibility", () => {
    beforeEach(async () => {
      installFifoWebLocks();
      await resetSavedDeckLibraryForTests();
    });
    afterEach(() => uninstallWebLocks());

    it("a precon whose save finishes after the user picks another deck does not replace that pick", async () => {
      saveDeck("Mine", { main: [{ name: "Island", count: 99 }], sideboard: [], format: "Commander", commander: ["Zimone, Mystery Unraveler"] } as ParsedDeck);
      vi.mocked(loadPreconDeckMap).mockResolvedValue(commanderPrecon());
      vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation(async (decks) => compatibleBatch(decks));
      const onSelectDeck = vi.fn();

      render(<MyDecks mode="select" selectedFormat="Commander" activeDeckName={null} onSelectDeck={onSelectDeck} />);
      await screen.findByText("Secrets of Strixhaven (SOS)");
      await screen.findByText("Mine");

      let release!: () => void;
      const held = new Promise<void>((r) => { release = r; });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => { expect((await navigator.locks.query()).held).toHaveLength(1); });

      await userEvent.click(screen.getByText("Secrets of Strixhaven (SOS)"));
      await vi.waitFor(async () => { expect((await navigator.locks.query()).pending).toHaveLength(1); });
      await userEvent.click(screen.getByText("Mine"));

      release();
      await holder;
      await awaitSavedDeckLibraryIdle();

      expect(onSelectDeck.mock.calls).toEqual([["Mine"]]);
    });

    it("a random pick whose compatibility check finishes after the user picks a deck does not replace that pick", async () => {
      saveDeck("Mine", { main: [{ name: "Island", count: 60 }], sideboard: [] });
      saveDeck("Other", { main: [{ name: "Mountain", count: 60 }], sideboard: [] });
      let release!: (v: Record<string, ReturnType<typeof compatibleBatch>[string]>) => void;
      vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation(
        () => new Promise((r) => { release = r; }),
      );
      const onSelectDeck = vi.fn();

      render(<MyDecks mode="select" selectedFormat="Standard" activeDeckName={null} onSelectDeck={onSelectDeck} />);
      await screen.findByText("Mine");

      await userEvent.click(screen.getByRole("button", { name: "Random Deck" }));
      await vi.waitFor(() => expect(vi.mocked(evaluateDeckCompatibilityBatch)).toHaveBeenCalled());
      await userEvent.click(screen.getByText("Mine"));

      release({});
      await waitFor(() => expect(screen.getByRole("button", { name: "Random Deck" })).toBeEnabled());

      expect(onSelectDeck.mock.calls).toEqual([["Mine"]]);
    });
  });

  describe("an import selected through the Preconstructed catalog modal", () => {
    const catalogDecks: DeckMap = {
      aggro: {
        code: "SET",
        name: "Aggro Deck",
        type: "Commander Deck",
        releaseDate: "2026-01-01",
        coveragePct: 100,
        mainBoard: [{ name: "Mountain", count: 40 }],
        sideBoard: [],
        commander: [{ name: "Krenko", count: 1 }],
      },
    };

    beforeEach(async () => {
      installFifoWebLocks();
      await resetSavedDeckLibraryForTests();
      vi.mocked(useDecks).mockReturnValue({ decks: catalogDecks, status: "success" as const });
      // A different deck from a different catalog: it keeps MyDecks' own legal-precon
      // listing (and hence the "Preconstructed" CTA) non-empty without also
      // rendering "Aggro Deck" outside the modal under test.
      vi.mocked(loadPreconDeckMap).mockResolvedValue(commanderPrecon());
      vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation(async (decks) => compatibleBatch(decks));
    });
    afterEach(() => {
      uninstallWebLocks();
      vi.mocked(useDecks).mockReturnValue({ decks: null, status: "loading" as const });
    });

    it("a precon imported after its modal was dismissed is listed but not selected", async () => {
      vi.stubGlobal("prompt", vi.fn(() => "Aggro Deck (SET)"));
      const onSelectDeck = vi.fn();

      render(<MyDecks mode="select" selectedFormat="Commander" activeDeckName={null} onSelectDeck={onSelectDeck} />);
      await screen.findByText("Secrets of Strixhaven (SOS)");
      await userEvent.click(screen.getByRole("button", { name: "Preconstructed" }));

      let release!: () => void;
      const held = new Promise<void>((r) => { release = r; });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => { expect((await navigator.locks.query()).held).toHaveLength(1); });

      await userEvent.click(screen.getByRole("button", { name: /^Aggro Deck/ }));
      await vi.waitFor(async () => { expect((await navigator.locks.query()).pending).toHaveLength(1); });

      await userEvent.keyboard("{Escape}");
      release();
      await holder;
      await awaitSavedDeckLibraryIdle();

      expect(onSelectDeck).not.toHaveBeenCalled();
      expect(await screen.findByText("Aggro Deck (SET)")).toBeInTheDocument();
    });

    it("a precon imported while its modal stays open is selected (paired positive)", async () => {
      vi.stubGlobal("prompt", vi.fn(() => "Aggro Deck (SET)"));
      const onSelectDeck = vi.fn();

      render(<MyDecks mode="select" selectedFormat="Commander" activeDeckName={null} onSelectDeck={onSelectDeck} />);
      await screen.findByText("Secrets of Strixhaven (SOS)");
      await userEvent.click(screen.getByRole("button", { name: "Preconstructed" }));

      let release!: () => void;
      const held = new Promise<void>((r) => { release = r; });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => { expect((await navigator.locks.query()).held).toHaveLength(1); });

      await userEvent.click(screen.getByRole("button", { name: /^Aggro Deck/ }));
      await vi.waitFor(async () => { expect((await navigator.locks.query()).pending).toHaveLength(1); });

      release();
      await holder;

      await vi.waitFor(() => expect(onSelectDeck).toHaveBeenCalledWith("Aggro Deck (SET)"));
    });

    it("an import selected while a random pick is pending is not replaced by that pick", async () => {
      // No `format`, matching "random selection prefers exact-format feed decks…" above:
      // an explicit format would resolve `knownFormat` synchronously and skip the batch call.
      saveDeck("Mine", { main: [{ name: "Island", count: 99 }], sideboard: [] });
      saveDeck("Other", { main: [{ name: "Mountain", count: 99 }], sideboard: [] });
      vi.stubGlobal("prompt", vi.fn(() => "Aggro Deck (SET)"));
      let releaseRandom!: (v: Record<string, ReturnType<typeof compatibleBatch>[string]>) => void;
      vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation((decks, opts) => {
        // The background coverage scan also passes `summaryOnly: true` but drives
        // its result through `onResult`/`onStatus`; the random pick's own call
        // awaits the returned promise directly with neither — that is the one to hold.
        if (opts?.summaryOnly && !opts.onResult && !opts.onStatus) {
          return new Promise((r) => { releaseRandom = r; });
        }
        return Promise.resolve(compatibleBatch(decks));
      });
      const onSelectDeck = vi.fn();

      render(<MyDecks mode="select" selectedFormat="Commander" activeDeckName={null} onSelectDeck={onSelectDeck} />);
      await screen.findByText("Mine");

      await userEvent.click(screen.getByRole("button", { name: "Random Deck" }));
      await vi.waitFor(() => expect(releaseRandom).toBeDefined());

      await userEvent.click(screen.getByRole("button", { name: "Preconstructed" }));
      await userEvent.click(screen.getByRole("button", { name: /^Aggro Deck/ }));
      await vi.waitFor(() => expect(onSelectDeck).toHaveBeenCalledWith("Aggro Deck (SET)"));

      releaseRandom({});
      await waitFor(() => expect(screen.getByRole("button", { name: "Random Deck" })).toBeEnabled());
      // The button re-enables (`finally` clears `isPickingRandomDeck`) before
      // `handleTileClick`'s own precon save settles the library lock; wait for
      // that too, or a stale `selectionRequest` bump race goes undetected.
      await awaitSavedDeckLibraryIdle();

      expect(onSelectDeck.mock.calls).toEqual([["Aggro Deck (SET)"]]);
    });
  });

  describe("MyDecks unmounts while a select-mode choice waits", () => {
    beforeEach(async () => {
      installFifoWebLocks();
      await resetSavedDeckLibraryForTests();
    });
    afterEach(() => uninstallWebLocks());

    it("a precon pick whose save finishes after MyDecks unmounts does not select", async () => {
      vi.mocked(loadPreconDeckMap).mockResolvedValue({
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
      vi.mocked(evaluateDeckCompatibilityBatch).mockImplementation(async (decks) => compatibleBatch(decks));
      const onSelectDeck = vi.fn();

      const r = render(<MyDecks mode="select" selectedFormat="Commander" activeDeckName={null} onSelectDeck={onSelectDeck} />);
      await screen.findByText("Secrets of Strixhaven (SOS)");

      let release!: () => void;
      const held = new Promise<void>((res) => { release = res; });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => { expect((await navigator.locks.query()).held).toHaveLength(1); });

      await userEvent.click(screen.getByText("Secrets of Strixhaven (SOS)"));
      await vi.waitFor(async () => { expect((await navigator.locks.query()).pending).toHaveLength(1); });

      r.unmount();
      release();
      await holder;
      await awaitSavedDeckLibraryIdle();

      expect(onSelectDeck).not.toHaveBeenCalled();
    });
  });

  describe("a folder created for a deck does not undo a move made while it waits", () => {
    beforeEach(async () => {
      installFifoWebLocks();
      await resetSavedDeckLibraryForTests();
    });
    afterEach(() => uninstallWebLocks());

    it("New folder… on a deck that is then moved to another folder before the create completes keeps both", async () => {
      saveDeck("Mine", { main: [{ name: "Island", count: 60 }], sideboard: [] });
      const folderG = createFolder(testSavedDeckTxn, "G");
      expect(folderG).not.toBeNull();

      render(<MyDecks mode="manage" activeDeckName={null} />);
      await screen.findByText("Mine");

      let release!: () => void;
      const held = new Promise<void>((r) => { release = r; });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => { expect((await navigator.locks.query()).held).toHaveLength(1); });

      await userEvent.click(screen.getByRole("button", { name: "Deck options" }));
      await userEvent.click(await screen.findByRole("menuitem", { name: "New folder…" }));
      await userEvent.type(await screen.findByLabelText("Folder name:"), "F");
      await userEvent.click(screen.getByRole("button", { name: "Create" }));

      await userEvent.click(screen.getByRole("button", { name: "Deck options" }));
      await userEvent.click(await screen.findByRole("menuitemradio", { name: /^G/ }));

      await vi.waitFor(async () => { expect((await navigator.locks.query()).pending).toHaveLength(2); });
      release();
      await holder;
      await awaitSavedDeckLibraryIdle();

      await vi.waitFor(() => {
        expect(listFolders().map((f) => f.name)).toContain("F");
        expect(getDeckMeta("Mine")?.folderId).toBe(folderG!.id);
      });
    });

    it("New folder… moves the deck into the new folder", async () => {
      saveDeck("Mine", { main: [{ name: "Island", count: 60 }], sideboard: [] });

      render(<MyDecks mode="manage" activeDeckName={null} />);
      await screen.findByText("Mine");

      await userEvent.click(screen.getByRole("button", { name: "Deck options" }));
      await userEvent.click(await screen.findByRole("menuitem", { name: "New folder…" }));
      await userEvent.type(await screen.findByLabelText("Folder name:"), "F");
      await userEvent.click(screen.getByRole("button", { name: "Create" }));

      await waitFor(() => {
        const folder = listFolders().find((f) => f.name === "F");
        expect(folder).toBeDefined();
        expect(getDeckMeta("Mine")?.folderId).toBe(folder!.id);
      });
    });
  });
});
