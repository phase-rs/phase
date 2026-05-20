import { afterEach, describe, expect, it, vi } from "vitest";
import { useEffect } from "react";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { DeckBuilder } from "../DeckBuilder";
import { loadPreconDeckMap } from "../../../hooks/useDecks";
import { resolveCommander } from "../../../services/deckParser";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../../../constants/storage";

const cacheCardsMock = vi.fn();

vi.mock("react-router", () => ({
  useNavigate: () => vi.fn(),
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
  DeckList: () => <div>Deck List</div>,
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

    const nameInput = await screen.findByPlaceholderText("Deck name...");
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
    expect(screen.getByRole("button", { name: "Show Browser" })).toBeInTheDocument();
  });
});
