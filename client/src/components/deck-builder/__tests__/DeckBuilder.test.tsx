import { afterEach, describe, expect, it, vi } from "vitest";
import { useEffect } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { DeckBuilder } from "../DeckBuilder";
import { loadPreconDeckMap } from "../../../hooks/useDecks";
import { resolveCommander } from "../../../services/deckParser";
import { useIsMobile } from "../../../hooks/useIsMobile";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../../../constants/storage";

const cacheCardsMock = vi.fn();

vi.mock("react-router", () => ({
  useNavigate: () => vi.fn(),
}));

// Default to desktop (matches jsdom's 1024px innerWidth); individual tests opt
// into the mobile overlay path where the filter sheet becomes a focus-trapped
// dialog.
vi.mock("../../../hooks/useIsMobile", () => ({
  useIsMobile: vi.fn(() => false),
}));

vi.mock("../../../hooks/useDeckCardData", () => ({
  useDeckCardData: () => ({ cardDataCache: new Map(), cacheCards: cacheCardsMock }),
}));

vi.mock("../../../hooks/useDecks", () => ({
  loadPreconDeckMap: vi.fn(),
}));

vi.mock("../../../services/deckParser", async () => {
  const actual = await vi.importActual<typeof import("../../../services/deckParser")>("../../../services/deckParser");
  return {
    ...actual,
    resolveCommander: vi.fn(async (deck) => deck),
  };
});

vi.mock("../CardSearch", () => ({
  CardSearch: ({ onResults }: { onResults: (cards: unknown[], total: number) => void }) => {
    useEffect(() => {
      onResults([], 0);
    }, [onResults]);
    return <div>Card Search</div>;
  },
}));

vi.mock("../DeckStack", () => ({
  DeckStack: ({ deck, commanders }: { deck: { main: Array<{ name: string; count: number }> }; commanders: string[] }) => (
    <div>
      <div>Deck Stack</div>
      {commanders.map((name) => <div key={name}>{name}</div>)}
      {deck.main.map((entry) => <div key={entry.name}>{entry.count} {entry.name}</div>)}
    </div>
  ),
}));

vi.mock("../DeckList", () => ({
  DeckList: ({
    deck,
    onRemoveCard,
  }: {
    deck: { main: Array<{ name: string; count: number }>; commander?: string[] };
    onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  }) => (
    <div>
      <div>Deck List</div>
      {deck.commander?.map((name) => <div key={name}>{name}</div>)}
      {deck.main.map((entry) => (
        <div key={entry.name}>
          <span>{entry.count} {entry.name}</span>
          <button type="button" onClick={() => onRemoveCard(entry.name, "main")}>
            remove-{entry.name}
          </button>
        </div>
      ))}
    </div>
  ),
}));

vi.mock("../ManaCurve", () => ({
  ManaCurve: () => <div>Mana Curve</div>,
}));

vi.mock("../FormatFilter", () => ({
  FormatFilter: () => <div>Format Filter</div>,
}));

vi.mock("../CommanderPanel", () => ({
  CommanderPanel: () => <div>Commander Panel</div>,
}));

describe("DeckBuilder", () => {
  afterEach(() => {
    cleanup();
    cacheCardsMock.mockClear();
    vi.mocked(loadPreconDeckMap).mockReset();
    vi.mocked(resolveCommander).mockReset();
    vi.mocked(resolveCommander).mockImplementation(async (deck) => deck);
    vi.mocked(useIsMobile).mockReturnValue(false);
    localStorage.clear();
  });

  it("runs commander inference at save-time and persists the result", async () => {
    const user = userEvent.setup();
    // A 100-singleton Commander-shaped precon with NO explicit commander —
    // exactly the case where save-time inference must fire.
    const mainBoard = Array.from({ length: 100 }, (_, i) => ({
      name: `Card ${i + 1}`,
      count: 1,
    }));
    vi.mocked(loadPreconDeckMap).mockResolvedValue({
      orphans: {
        code: "ORF",
        name: "Orphan Precon",
        type: "Commander",
        coveragePct: 100,
        mainBoard,
        sideBoard: [],
        commander: [],
      },
    });
    // Mock chain: load path returns the precon as-is (no inference) so the
    // editor starts commander-less, mirroring the user's mid-edit state. The
    // second call (from handleSave) is the one we want to verify performs
    // inference and produces a commander.
    vi.mocked(resolveCommander)
      .mockImplementationOnce(async (deck) => deck)
      .mockImplementationOnce(async (deck) => ({
        ...deck,
        main: deck.main.filter((e) => e.name !== "Card 1"),
        commander: ["Card 1"],
      }));
    localStorage.clear();

    render(
      <DeckBuilder
        format="Commander"
        onFormatChange={vi.fn()}
        initialDeckName="[Pre-built] Orphan Precon (ORF)"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    // Wait for precon load to complete — Save becomes enabled once deckName is set.
    const saveButton = await screen.findByRole("button", { name: "Save" });
    await waitFor(() => expect(saveButton).not.toBeDisabled());

    // Pre-save sanity: load path called resolveCommander once and returned a
    // commander-less deck (the mock returns as-is for the load call because
    // commander.length === 0 path of the mock implementation doesn't apply
    // until save when currentDeck.commander is also empty — see mock above).
    expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(1);

    await user.click(saveButton);

    // Save triggered a second resolveCommander call which inferred Card 1.
    await waitFor(() => {
      expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(2);
    });
    await waitFor(() => {
      // The precon loader sets deckName to "<name> (<code>)" without the
      // [Pre-built] prefix — saving stores under that bare key.
      const persisted = JSON.parse(
        localStorage.getItem("phase-deck:Orphan Precon (ORF)") ?? "{}",
      );
      expect(persisted.commander).toEqual(["Card 1"]);
    });
  });

  it("renames an existing saved deck instead of duplicating it", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Old Deck",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 4 }],
        sideboard: [],
        format: "Standard",
      }),
    );
    localStorage.setItem(ACTIVE_DECK_KEY, "Old Deck");

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Old Deck"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Old Deck"));
    await user.clear(nameInput);
    await user.type(nameInput, "Renamed Deck");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Old Deck")).toBeNull();
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Renamed Deck")).not.toBeNull();
    });
    expect(localStorage.getItem(ACTIVE_DECK_KEY)).toBe("Renamed Deck");
  });

  it("warns about unsaved changes when leaving after an edit", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Dirty Deck",
      JSON.stringify({
        main: [{ name: "Forest", count: 10 }],
        sideboard: [],
        format: "Standard",
      }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Dirty Deck"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Dirty Deck"));

    // A freshly loaded deck is clean — no confirmation owed yet. Make an edit.
    await user.click(screen.getByRole("button", { name: "remove-Forest" }));

    // Leaving now must prompt to save.
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();

    // Cancel keeps you in the editor.
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("button", { name: "Discard" })).not.toBeInTheDocument();
  });

  it("toggles between Deck and Info surfaces via the tab bar", async () => {
    const user = userEvent.setup();
    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    // Deck-first: the builder opens on the Deck surface.
    const deckTab = screen.getByRole("tab", { name: /deck/i });
    const infoTab = screen.getByRole("tab", { name: /info/i });
    expect(deckTab).toHaveAttribute("aria-selected", "true");

    await user.click(infoTab);
    expect(infoTab).toHaveAttribute("aria-selected", "true");
    expect(deckTab).toHaveAttribute("aria-selected", "false");

    await user.click(deckTab);
    expect(deckTab).toHaveAttribute("aria-selected", "true");
    expect(infoTab).toHaveAttribute("aria-selected", "false");
  });

  it("navigates the surface tabs with the arrow keys (APG tablist)", async () => {
    const user = userEvent.setup();
    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const deckTab = screen.getByRole("tab", { name: /deck/i });
    const infoTab = screen.getByRole("tab", { name: /info/i });

    // Roving tabindex: only the selected tab is in the tab sequence.
    expect(deckTab).toHaveAttribute("tabindex", "0");
    expect(infoTab).toHaveAttribute("tabindex", "-1");

    deckTab.focus();
    await user.keyboard("{ArrowRight}");
    // Automatic activation: arrow moves both selection and focus.
    expect(infoTab).toHaveAttribute("aria-selected", "true");
    expect(infoTab).toHaveFocus();
    expect(infoTab).toHaveAttribute("tabindex", "0");

    await user.keyboard("{ArrowRight}");
    // Wraps back to the first tab.
    expect(deckTab).toHaveAttribute("aria-selected", "true");
    expect(deckTab).toHaveFocus();
  });

  it("traps focus in the mobile filter sheet and restores it on close", async () => {
    vi.mocked(useIsMobile).mockReturnValue(true);
    const user = userEvent.setup();
    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const searchTrigger = screen.getByRole("button", { name: "Search" });
    await user.click(searchTrigger);

    // Opening the sheet exposes it as a modal dialog and moves focus inside it.
    const dialog = screen.getByRole("dialog", { name: "Filters" });
    expect(dialog).toHaveAttribute("aria-modal", "true");
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));

    // The trap keeps Tab within the dialog — with the keydown listener removed,
    // Tab would escape to a control behind the overlay. This is the assertion
    // that actually discriminates "trap present" from "trap absent".
    await user.tab();
    expect(dialog.contains(document.activeElement)).toBe(true);

    // Closing returns focus to the control that opened it.
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(screen.queryByRole("dialog", { name: "Filters" })).not.toBeInTheDocument();
    expect(searchTrigger).toHaveFocus();
  });

  it("opens and closes the mobile filter sheet", async () => {
    vi.mocked(useIsMobile).mockReturnValue(true);
    const user = userEvent.setup();
    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    // The overlay backdrop only renders while the sheet is open. The trigger is
    // the "Search" button in the main canvas header.
    expect(screen.queryByRole("button", { name: "Close filters" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Search" }));
    expect(screen.getByRole("button", { name: "Close filters" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Close filters" }));
    expect(screen.queryByRole("button", { name: "Close filters" })).not.toBeInTheDocument();
  });

  it("clones a deck into a new copy without deleting the original", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "My Deck",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 4 }],
        sideboard: [],
        format: "Standard",
      }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="My Deck"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("My Deck"));

    await user.click(screen.getByRole("button", { name: "Clone" }));

    // Clone creates a new copy and leaves the original intact (unlike rename).
    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "My Deck")).not.toBeNull();
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "My Deck copy")).not.toBeNull();
    });
    expect(nameInput).toHaveValue("My Deck copy");
  });

  it("does not reactively auto-resolve a commander mid-edit", async () => {
    // Regression: the reactive auto-resolve effect was deleted in favour of
    // save-time inference. Loading a Commander-shaped 100-singleton precon
    // with no explicit commander must NOT trigger a second resolveCommander
    // call — that call used to immediately re-populate the commander after
    // any user Remove, forcing users to cycle through legendary creatures.
    const mainBoard = Array.from({ length: 100 }, (_, i) => ({
      name: `Card ${i + 1}`,
      count: 1,
    }));
    vi.mocked(loadPreconDeckMap).mockResolvedValue({
      orphans: {
        code: "ORF",
        name: "Orphan Precon",
        type: "Commander",
        coveragePct: 100,
        mainBoard,
        sideBoard: [],
        commander: [],
      },
    });
    // Identity mock — if the reactive effect still existed, it would call
    // resolveCommander a second time after the load-path applyDeckToEditor.
    vi.mocked(resolveCommander).mockImplementation(async (deck) => deck);

    render(
      <DeckBuilder
        format="Commander"
        onFormatChange={vi.fn()}
        initialDeckName="[Pre-built] Orphan Precon (ORF)"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    // Wait for load to complete via the Save button becoming enabled.
    const saveButton = await screen.findByRole("button", { name: "Save" });
    await waitFor(() => expect(saveButton).not.toBeDisabled());

    // Exactly one call: the load path. No reactive re-fire on the empty
    // commanders state — pre-deletion, the effect would have called twice.
    expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(1);
  });

  it("loads virtual precons into the editor without requiring saved storage", async () => {
    vi.mocked(loadPreconDeckMap).mockResolvedValue({
      secrets: {
        code: "SOS",
        name: "Secrets of Strixhaven",
        type: "Commander",
        coveragePct: 100,
        mainBoard: [{ name: "Island", count: 99 }],
        sideBoard: [],
        commander: [{ name: "Zimone, Mystery Unraveler", count: 1 }],
      },
    });

    render(
      <DeckBuilder
        format="Commander"
        onFormatChange={vi.fn()}
        initialDeckName="[Pre-built] Secrets of Strixhaven (SOS)"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    expect(await screen.findByText("99 Island")).toBeInTheDocument();
    expect(screen.getByText("Zimone, Mystery Unraveler")).toBeInTheDocument();
    // Loading a deck foregrounds the Deck surface (replaces the old
    // "Show Browser"/"Expand Deck View" toggle assertion).
    expect(screen.getByRole("tab", { name: /deck/i })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });
});
