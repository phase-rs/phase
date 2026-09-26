import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect, useState } from "react";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { DeckBuilder } from "../DeckBuilder";
import type { GameFormat } from "../../../adapter/types";
import { loadPreconDeckMap } from "../../../hooks/useDecks";
import { resolveCommander } from "../../../services/deckParser";
import { useIsMobile } from "../../../hooks/useIsMobile";
import {
  ACTIVE_DECK_KEY,
  STORAGE_KEY_PREFIX,
  createFolder,
  deleteFolder,
  getDeckMeta,
  listFolders,
  removeDeckMeta,
  removeSavedDeckData,
  setDeckFolder,
  stampDeckMeta,
  toggleDeckStar,
  writeDraftAutosaveDeck,
  writeSavedDeckData,
} from "../../../constants/storage";
import { useAppNotificationStore } from "../../../stores/appToastStore";
import {
  setSavedDeckTxnGateForTests,
  setSavedDeckTxnLockWaitForTests,
  withSavedDeckLibrary,
} from "../../../services/savedDeckTransaction";
import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  testSavedDeckTxn,
  uninstallWebLocks,
} from "../../../test/helpers/webLocks";

const cacheCardsMock = vi.fn();

const { navigateMock } = vi.hoisted(() => ({ navigateMock: vi.fn() }));
vi.mock("react-router", () => ({
  useNavigate: () => navigateMock,
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
  beforeEach(async () => {
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
    installFifoWebLocks();
    await resetSavedDeckLibraryForTests();
  });

  afterEach(() => {
    cleanup();
    navigateMock.mockReset();
    cacheCardsMock.mockClear();
    vi.mocked(loadPreconDeckMap).mockReset();
    vi.mocked(resolveCommander).mockReset();
    vi.mocked(resolveCommander).mockImplementation(async (deck) => deck);
    vi.mocked(useIsMobile).mockReturnValue(false);
    uninstallWebLocks();
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
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Deck saved",
      description: '"Renamed Deck" was saved to your decks.',
    });
  });

  it.each([false, true])("a rename preserves a replacement of the old name, including a same-name Load while queued (reload=%s)", async (reload) => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "P",
      JSON.stringify({ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [], format: "Standard" }),
    );
    stampDeckMeta(testSavedDeckTxn, "P", 1000);
    localStorage.setItem(ACTIVE_DECK_KEY, "P");

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="P"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("P"));

    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let publishReplacement!: () => void;
    const replacing = new Promise<void>((resolve) => {
      publishReplacement = resolve;
    });
    let replacementPublished!: () => void;
    const published = new Promise<void>((resolve) => {
      replacementPublished = resolve;
    });
    const holder = withSavedDeckLibrary(async (txn) => {
      await replacing;
      removeSavedDeckData(txn, "P");
      removeDeckMeta(txn, "P");
      writeSavedDeckData(txn, "P", JSON.stringify({ main: [{ name: "Mountain", count: 60 }], sideboard: [] }));
      stampDeckMeta(txn, "P", 2000);
      toggleDeckStar(txn, "P");
      localStorage.setItem(ACTIVE_DECK_KEY, "P");
      replacementPublished();
      await held;
    });
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    await user.clear(nameInput);
    await user.type(nameInput, "N");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    publishReplacement();
    await published;
    if (reload) {
      await user.click(screen.getByRole("button", { name: "Load deck..." }));
      await user.click(screen.getByRole("option", { name: "P" }));
      await user.click(screen.getByRole("button", { name: "Discard" }));
      await screen.findByDisplayValue("P");
    }
    release();
    await holder;
    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "N")).not.toBeNull();
    });

    const replacement = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "P") ?? "{}");
    expect(replacement.main).toEqual([{ name: "Mountain", count: 60 }]);
    expect(getDeckMeta("P")).toEqual({ addedAt: 2000, starred: true });
    expect(localStorage.getItem(ACTIVE_DECK_KEY)).toBe("P");

    const renamed = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "N") ?? "{}");
    expect(renamed.main).toEqual([{ name: "Lightning Bolt", count: 4 }]);
    const renamedMeta = getDeckMeta("N");
    expect(renamedMeta?.starred).toBeFalsy();
    expect(renamedMeta?.addedAt).not.toBe(2000);
    if (reload) {
      expect(nameInput).toHaveValue("P");
      expect(screen.getByRole("button", { name: "remove-Mountain" })).toBeInTheDocument();
    } else {
      expect(useAppNotificationStore.getState().notification).toEqual({
        title: "Deck saved",
        description: '"N" was saved to your decks.',
      });
    }
  });

  it("renaming after an in-place save moves the deck", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "P",
      JSON.stringify({ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="P"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("P"));

    await user.click(await screen.findByRole("button", { name: "remove-Lightning Bolt" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "P") ?? "{}").main).toEqual([
        { name: "Lightning Bolt", count: 3 },
      ]);
    });

    // Discriminates the ref refresh after Save: without it, this rename's "previous" is stale
    // (captured at Load, before the in-place save above wrote count 3) and refuses to move "P".
    await user.clear(nameInput);
    await user.type(nameInput, "N");
    await user.click(screen.getByRole("button", { name: /^(Save|Saved ✓)$/ }));

    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "P")).toBeNull();
    });
    const persisted = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "N") ?? "{}");
    expect(persisted.main).toEqual([{ name: "Lightning Bolt", count: 3 }]);
  });

  it("renaming a clone moves the clone", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "P",
      JSON.stringify({ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="P"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("P"));

    await user.click(screen.getByRole("button", { name: "Clone" }));
    await waitFor(() => expect(nameInput).toHaveValue("P copy"));

    // Discriminates the snapshot Clone's own transaction writes into the ref: without it, this
    // rename's "previous" is null (or still "P") and does not vacate "P copy".
    await user.clear(nameInput);
    await user.type(nameInput, "N");
    await user.click(screen.getByRole("button", { name: /^(Save|Saved ✓)$/ }));

    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "P copy")).toBeNull();
    });
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "N")).not.toBeNull();
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "P")).not.toBeNull();
  });

  it("an edit saved in place, then renamed, while both waited on the same held lock leaves one deck, not two", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "DeckA",
      JSON.stringify({ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="DeckA"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("DeckA"));
    await user.click(await screen.findByRole("button", { name: "remove-Lightning Bolt" }));

    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    // S1: in-place save of the edited deck, queued behind the held lock.
    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    // S2: renamed (no further edit), queued behind S1.
    await user.clear(nameInput);
    await user.type(nameInput, "DeckN");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(2);
    });

    release();
    await holder;
    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "DeckA")).toBeNull();
    });
    const persisted = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "DeckN") ?? "{}");
    expect(persisted.main).toEqual([{ name: "Lightning Bolt", count: 3 }]);
  });

  it("preserves a saved planar deck through editor load and save", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Planechase Deck",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 4 }],
        sideboard: [],
        planar_deck: ["The Aether Flues", "Spatial Merging"],
        format: "Planechase",
      }),
    );

    render(
      <DeckBuilder
        format="Planechase"
        onFormatChange={vi.fn()}
        initialDeckName="Planechase Deck"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Planechase Deck"));
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      const persisted = JSON.parse(
        localStorage.getItem(STORAGE_KEY_PREFIX + "Planechase Deck") ?? "{}",
      );
      expect(persisted.planar_deck).toEqual(["The Aether Flues", "Spatial Merging"]);
    });
  });

  it("persists a signature spell only when saving as Oathbreaker", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Oath Deck",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 1 }],
        sideboard: [],
        commander: ["The Oathbreaker"],
        signature_spell: ["Lightning Bolt"],
        format: "Oathbreaker",
      }),
    );
    const props = {
      onFormatChange: vi.fn(),
      initialDeckName: "Oath Deck",
      searchFilters: { text: "", colors: [], type: "", sets: [], browseFormat: "all" as const },
      onSearchFiltersChange: vi.fn(),
      onResetSearch: vi.fn(),
    };

    const { rerender } = render(<DeckBuilder format="Oathbreaker" {...props} />);

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Oath Deck"));
    await user.click(screen.getByRole("button", { name: /^(Save|Saved ✓)$/ }));
    await waitFor(() => {
      const persisted = JSON.parse(
        localStorage.getItem(STORAGE_KEY_PREFIX + "Oath Deck") ?? "{}",
      );
      expect(persisted.signature_spell).toEqual(["Lightning Bolt"]);
    });

    rerender(<DeckBuilder format="Modern" {...props} />);
    await user.click(screen.getByRole("button", { name: /^(Save|Saved ✓)$/ }));
    await waitFor(() => {
      const persisted = JSON.parse(
        localStorage.getItem(STORAGE_KEY_PREFIX + "Oath Deck") ?? "{}",
      );
      expect("signature_spell" in persisted).toBe(false);
    });
  });

  it("does not restore Two-Headed Giant as a persisted deck-builder format", async () => {
    const onFormatChange = vi.fn();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Team Deck",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 4 }],
        sideboard: [],
        format: "TwoHeadedGiant",
      }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={onFormatChange}
        initialDeckName="Team Deck"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Deck name" })).toHaveValue("Team Deck"),
    );
    expect(onFormatChange).not.toHaveBeenCalled();
  });

  it("preserves folder and star membership across a rename", async () => {
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
    const folder = createFolder(testSavedDeckTxn, "Aggro")!;
    setDeckFolder(testSavedDeckTxn, "Old Deck", folder.id);
    toggleDeckStar(testSavedDeckTxn, "Old Deck");

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

    await waitFor(() =>
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Renamed Deck")).not.toBeNull(),
    );
    // Organization follows the deck to its new name; the old entry is gone.
    const meta = getDeckMeta("Renamed Deck");
    expect(meta?.folderId).toBe(folder.id);
    expect(meta?.starred).toBe(true);
    expect(getDeckMeta("Old Deck")).toBeNull();
  });

  it("makes an edited autosave a user deck, freeing its slot for the next autosave", async () => {
    const user = userEvent.setup();
    await writeDraftAutosaveDeck(
      "Sealed",
      "[Autosave] Sealed",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 1 }],
        sideboard: [],
        format: "Limited",
      }),
    );

    render(
      <DeckBuilder
        format="Limited"
        onFormatChange={vi.fn()}
        initialDeckName="[Autosave] Sealed"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("[Autosave] Sealed"));
    await user.click(await screen.findByRole("button", { name: "remove-Lightning Bolt" }));
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBeUndefined());
    const persisted = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
    expect(persisted.main).toEqual([]);

    const nextAutosaveResult = await writeDraftAutosaveDeck("Sealed", "[Autosave] Sealed", "fresh-autosave-data");
    expect(nextAutosaveResult).toEqual({ status: "committed", value: "[Autosave] Sealed (2)" });
    // The user's edit at the original name is untouched by the new autosave.
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}").main).toEqual([]);
  });

  it("makes a renamed autosave a user deck, carrying its folder", async () => {
    const user = userEvent.setup();
    await writeDraftAutosaveDeck(
      "Sealed",
      "[Autosave] Sealed",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 4 }],
        sideboard: [],
        format: "Limited",
      }),
    );
    const folder = createFolder(testSavedDeckTxn, "Drafts")!;
    setDeckFolder(testSavedDeckTxn, "[Autosave] Sealed", folder.id);

    render(
      <DeckBuilder
        format="Limited"
        onFormatChange={vi.fn()}
        initialDeckName="[Autosave] Sealed"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("[Autosave] Sealed"));
    await user.clear(nameInput);
    await user.type(nameInput, "My Sealed");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(localStorage.getItem(STORAGE_KEY_PREFIX + "My Sealed")).not.toBeNull());
    const meta = getDeckMeta("My Sealed");
    expect(meta?.autosaveSlot).toBeUndefined();
    expect(meta?.folderId).toBe(folder.id);
  });

  it("claims an autosave's name for a fresh deck saved under it", async () => {
    const user = userEvent.setup();
    await writeDraftAutosaveDeck(
      "Sealed",
      "[Autosave] Sealed",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 4 }],
        sideboard: [],
        format: "Limited",
      }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    // user-event's `type` treats `[`/`]` as special-key syntax; `{[}`/`{]}` type the literal characters.
    await user.type(nameInput, "{[}Autosave{]} Sealed");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBeUndefined());
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

  it("loading another deck while a save is pending on the lock keeps both decks intact", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck B",
      JSON.stringify({ main: [{ name: "Counterspell", count: 4 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );

    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
    await user.click(screen.getByRole("button", { name: "remove-Lightning Bolt" }));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    // Load Deck B while Deck A's save is still queued behind the lock. The deck is dirty
    // (from the edit above), so this routes through the discard-confirmation dialog.
    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck B" }));
    await user.click(screen.getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(nameInput).toHaveValue("Deck B"));

    releaseHolder();
    await holder;
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(0);
      expect((await navigator.locks.query()).pending).toHaveLength(0);
    });

    // Deck A's save completed with the edited payload.
    const savedA = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}");
    expect(savedA.main).toEqual([{ name: "Lightning Bolt", count: 3 }]);
    // Deck B's data is untouched, and the editor still shows Deck B as open — the late-arriving
    // save of Deck A must not have reverted the deck name back to "Deck A".
    const savedB = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck B") ?? "{}");
    expect(savedB.main).toEqual([{ name: "Counterspell", count: 4 }]);
    expect(nameInput).toHaveValue("Deck B");

    // Editing and saving Deck B now must not take the rename branch against a stale
    // "savedDeckRef: Deck A" — that would move Deck A's data onto Deck B and delete it.
    await user.click(await screen.findByRole("button", { name: "remove-Counterspell" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck B") ?? "{}").main).toEqual([
        { name: "Counterspell", count: 3 },
      ]),
    );
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A")).not.toBeNull();
  });

  it("an edit made while a save waits for the lock keeps the deck dirty after that save completes", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Dirty Deck",
      JSON.stringify({ main: [{ name: "Forest", count: 10 }], sideboard: [], format: "Standard" }),
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

    await user.click(screen.getByRole("button", { name: "remove-Forest" })); // payload will hold 9

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    await user.click(await screen.findByRole("button", { name: "remove-Forest" })); // editor now holds 8

    releaseHolder();
    await holder;
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(0);
      expect((await navigator.locks.query()).pending).toHaveLength(0);
    });

    // The save completed with the payload from the first click.
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Dirty Deck") ?? "{}");
    expect(saved.main).toEqual([{ name: "Forest", count: 9 }]);

    // The second edit landed after the save's payload was captured, so the deck is still dirty.
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));

    // Paired positive: saving again with no contention clears dirty.
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Dirty Deck") ?? "{}").main).toEqual([
        { name: "Forest", count: 8 },
      ]),
    );
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    expect(screen.queryByRole("button", { name: "Discard" })).not.toBeInTheDocument();
  });

  describe("Save & continue while its save waits", () => {
    function seedThreeDecks() {
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck A",
        JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }, { name: "Delta", count: 1 }], sideboard: [], format: "Standard" }),
      );
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck B",
        JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Standard" }),
      );
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck C",
        JSON.stringify({ main: [{ name: "Omega", count: 1 }], sideboard: [], format: "Standard" }),
      );
    }
    async function mountBuilder() {
      seedThreeDecks();
      render(
        <DeckBuilder
          format="Standard"
          onFormatChange={vi.fn()}
          initialDeckName="Deck A"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />,
      );
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
      return nameInput as HTMLInputElement;
    }
    async function holdLibrary() {
      let release!: () => void;
      const held = new Promise<void>((resolve) => { release = resolve; });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });
      return async () => {
        release();
        await holder;
        await vi.waitFor(async () => {
          const q = await navigator.locks.query();
          expect(q.held).toHaveLength(0);
          expect(q.pending).toHaveLength(0);
        });
      };
    }
    async function loadDeck(user: ReturnType<typeof userEvent.setup>, name: string) {
      await user.click(screen.getByRole("button", { name: "Load deck..." }));
      await user.click(screen.getByRole("option", { name }));
    }

    it("cancelling Save & continue while its save waits keeps the deck open and a later edit unsaved", async () => {
      const user = userEvent.setup();
      const nameInput = await mountBuilder();
      await user.click(screen.getByRole("button", { name: "remove-Beta" }));

      const release = await holdLibrary();
      await loadDeck(user, "Deck B");
      await user.click(screen.getByRole("button", { name: "Save & continue" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      await user.click(screen.getByRole("button", { name: "Cancel" }));
      await user.click(await screen.findByRole("button", { name: "remove-Delta" }));

      await release();
      await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
      expect(screen.getByText("1 Alpha")).toBeInTheDocument();
      expect(screen.queryByText("1 Delta")).not.toBeInTheDocument();
      expect(screen.queryByText("1 Gamma")).not.toBeInTheDocument();
      // The save committed its pre-cancel snapshot: reach guard that the save actually ran.
      const savedA = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}");
      expect(savedA.main).toEqual([{ name: "Alpha", count: 1 }, { name: "Delta", count: 1 }]);

      await user.click(screen.getByRole("button", { name: /Menu/ }));
      expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
    });

    it("Save & continue that is not cancelled loads the requested deck once its save completes (paired positive)", async () => {
      const user = userEvent.setup();
      const nameInput = await mountBuilder();
      await user.click(screen.getByRole("button", { name: "remove-Beta" }));

      const release = await holdLibrary();
      await loadDeck(user, "Deck B");
      await user.click(screen.getByRole("button", { name: "Save & continue" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      await release();
      await waitFor(() => expect(nameInput).toHaveValue("Deck B"));
      expect(screen.getByText("1 Gamma")).toBeInTheDocument();
      const savedA = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}");
      expect(savedA.main).toEqual([{ name: "Alpha", count: 1 }, { name: "Delta", count: 1 }]);
    });

    it("a newer request replaces one whose Save & continue is still waiting", async () => {
      const user = userEvent.setup();
      const nameInput = await mountBuilder();
      await user.click(screen.getByRole("button", { name: "remove-Beta" }));

      const release = await holdLibrary();
      await loadDeck(user, "Deck B");
      await user.click(screen.getByRole("button", { name: "Save & continue" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });
      await user.click(screen.getByRole("button", { name: "Cancel" }));
      await loadDeck(user, "Deck C");

      await release();
      await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
      expect(await screen.findByRole("button", { name: "Save & continue" })).toBeInTheDocument();

      await user.click(screen.getByRole("button", { name: "Save & continue" }));
      await waitFor(() => expect(nameInput).toHaveValue("Deck C"));
      expect(screen.getByText("1 Omega")).toBeInTheDocument();
    });

    it("an edit that reaches the deck while the Save & continue dialog stays open keeps the deck open", async () => {
      const user = userEvent.setup();
      const nameInput = await mountBuilder();
      await user.click(screen.getByRole("button", { name: "remove-Beta" }));

      const release = await holdLibrary();
      await loadDeck(user, "Deck B");
      await user.click(screen.getByRole("button", { name: "Save & continue" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      // The dialog does not trap focus, so a keyboard user can still edit the deck behind it.
      screen.getByRole("button", { name: "remove-Delta" }).focus();
      await user.keyboard("{Enter}");

      await release();
      expect(screen.getByRole("dialog", { name: "Unsaved changes" })).toBeInTheDocument();
      expect(nameInput).toHaveValue("Deck A");
      expect(screen.queryByText("1 Delta")).not.toBeInTheDocument();
      const savedA = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}");
      expect(savedA.main).toEqual([{ name: "Alpha", count: 1 }, { name: "Delta", count: 1 }]);
    });
  });

  describe("persisted metadata edits", () => {
    // Holds the format the way DeckBuilderPage's URL does, so choosing a format re-renders the builder.
    function StatefulFormatBuilder({ initialFormat }: { initialFormat: GameFormat }) {
      const [format, setFormat] = useState<GameFormat>(initialFormat);
      return (
        <DeckBuilder
          format={format}
          onFormatChange={setFormat}
          initialDeckName="Deck A"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />
      );
    }

    function seed(format: GameFormat, extraA: Record<string, unknown> = {}) {
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck A",
        JSON.stringify({
          main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }],
          sideboard: [],
          format,
          ...extraA,
        }),
      );
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck B",
        JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format }),
      );
    }

    async function mount(format: GameFormat) {
      seed(format);
      render(<StatefulFormatBuilder initialFormat={format} />);
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
      return nameInput as HTMLInputElement;
    }

    async function holdLibrary() {
      let release!: () => void;
      const held = new Promise<void>((resolve) => { release = resolve; });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });
      return async () => {
        release();
        await holder;
        await vi.waitFor(async () => {
          const q = await navigator.locks.query();
          expect(q.held).toHaveLength(0);
          expect(q.pending).toHaveLength(0);
        });
      };
    }

    function storedA() {
      return JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}");
    }

    async function saveAndContinueToDeckB(user: ReturnType<typeof userEvent.setup>) {
      await user.click(screen.getByRole("button", { name: "remove-Beta" }));
      const release = await holdLibrary();
      await user.click(screen.getByRole("button", { name: "Load deck..." }));
      await user.click(screen.getByRole("option", { name: "Deck B" }));
      await user.click(screen.getByRole("button", { name: "Save & continue" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });
      return release;
    }

    async function expectLeavePrompts(user: ReturnType<typeof userEvent.setup>, prompts: boolean) {
      await user.click(screen.getByRole("button", { name: /Menu/ }));
      if (prompts) {
        expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
        expect(navigateMock).not.toHaveBeenCalled();
        await user.click(screen.getByRole("button", { name: "Cancel" }));
      } else {
        expect(screen.queryByRole("button", { name: "Discard" })).not.toBeInTheDocument();
        expect(navigateMock).toHaveBeenCalled();
      }
    }

    it("a rename typed behind the Save & continue dialog while its save waits keeps the renamed deck open", async () => {
      const user = userEvent.setup();
      const nameInput = await mount("Standard");
      const release = await saveAndContinueToDeckB(user);

      // The dialog does not trap focus, so a keyboard user can still edit the deck behind it.
      nameInput.focus();
      nameInput.setSelectionRange(0, nameInput.value.length);
      await user.keyboard("Deck Renamed");

      await release();
      expect(screen.getByRole("dialog", { name: "Unsaved changes" })).toBeInTheDocument();
      expect(nameInput).toHaveValue("Deck Renamed");
      expect(screen.queryByText("1 Gamma")).not.toBeInTheDocument();
      expect(storedA().main).toEqual([{ name: "Alpha", count: 1 }]);
    });

    it("a format chosen behind the Save & continue dialog while its save waits keeps the deck open in that format", async () => {
      const user = userEvent.setup();
      const nameInput = await mount("Standard");
      const release = await saveAndContinueToDeckB(user);

      screen.getByRole("button", { name: "Format" }).focus();
      await user.keyboard("{Enter}");
      const modernOption = await screen.findByRole("option", { name: "Modern" });
      modernOption.focus();
      await user.keyboard("{Enter}");

      await release();
      expect(screen.getByRole("dialog", { name: "Unsaved changes" })).toBeInTheDocument();
      expect(nameInput).toHaveValue("Deck A");
      expect(screen.getByRole("button", { name: "Format" })).toHaveTextContent("Modern");
      const stored = storedA();
      expect(stored.main).toEqual([{ name: "Alpha", count: 1 }]);
      expect(stored.format).toBe("Standard");
    });

    it("a bracket chosen behind the Save & continue dialog while its save waits keeps the deck open with that bracket", async () => {
      const user = userEvent.setup();
      const nameInput = await mount("Commander");
      const release = await saveAndContinueToDeckB(user);

      const upgradedButton = await screen.findByRole("button", { name: /Upgraded/ });
      upgradedButton.focus();
      await user.keyboard("{Enter}");

      await release();
      expect(screen.getByRole("dialog", { name: "Unsaved changes" })).toBeInTheDocument();
      expect(nameInput).toHaveValue("Deck A");
      expect(await screen.findByRole("button", { name: /Upgraded/ })).toHaveAttribute("aria-pressed", "true");
      const stored = storedA();
      expect(stored.main).toEqual([{ name: "Alpha", count: 1 }]);
      expect(stored.bracket).toBeUndefined();
    });

    it("Save & continue with no metadata change loads the requested deck once its save completes", async () => {
      const user = userEvent.setup();
      const nameInput = await mount("Standard");
      const release = await saveAndContinueToDeckB(user);

      await release();
      await waitFor(() => expect(nameInput).toHaveValue("Deck B"));
      expect(screen.getByText("1 Gamma")).toBeInTheDocument();
      expect(storedA().main).toEqual([{ name: "Alpha", count: 1 }]);
    });

    it("an ordinary Save that completes after a rename leaves the renamed deck unsaved", async () => {
      const user = userEvent.setup();
      const nameInput = await mount("Standard");
      await user.click(screen.getByRole("button", { name: "remove-Beta" }));
      const release = await holdLibrary();
      await user.click(screen.getByRole("button", { name: "Save" }));
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      nameInput.focus();
      nameInput.setSelectionRange(0, nameInput.value.length);
      await user.keyboard("Deck Renamed");

      await release();
      await waitFor(() => expect(storedA().main).toEqual([{ name: "Alpha", count: 1 }]));
      expect(screen.getByRole("button", { name: "Save" })).toBeInTheDocument();
      await expectLeavePrompts(user, true);
    });

    it("renaming the deck marks it unsaved", async () => {
      const user = userEvent.setup();
      await mount("Standard");
      const nameInput = screen.getByRole("textbox", { name: "Deck name" });
      await user.clear(nameInput);
      await user.type(nameInput, "Deck Renamed");
      await expectLeavePrompts(user, true);
    });

    it("choosing another format marks the deck unsaved", async () => {
      const user = userEvent.setup();
      await mount("Standard");
      await user.click(screen.getByRole("button", { name: "Format" }));
      await user.click(await screen.findByRole("option", { name: "Modern" }));
      await expectLeavePrompts(user, true);
    });

    it("choosing another bracket marks the deck unsaved", async () => {
      const user = userEvent.setup();
      await mount("Commander");
      await user.click(await screen.findByRole("button", { name: /Upgraded/ }));
      await expectLeavePrompts(user, true);
    });

    it("choosing the format already selected leaves the deck saved", async () => {
      const user = userEvent.setup();
      await mount("Standard");
      await user.click(screen.getByRole("button", { name: "Format" }));
      await user.click(await screen.findByRole("option", { name: "Standard" }));
      await expectLeavePrompts(user, false);
    });

    it("choosing the bracket already selected leaves the deck saved", async () => {
      const user = userEvent.setup();
      await mount("Commander");
      const unratedButton = await screen.findByRole("button", { name: "Unrated" });
      expect(unratedButton).toHaveAttribute("aria-pressed", "true");
      await user.click(unratedButton);
      await expectLeavePrompts(user, false);
    });

    it("loading a saved deck whose format and bracket differ from the editor's leaves it saved", async () => {
      const user = userEvent.setup();
      seed("Commander", { bracket: 3 });
      render(<StatefulFormatBuilder initialFormat="Standard" />);
      await waitFor(() => expect(screen.getByRole("button", { name: "Format" })).toHaveTextContent("Commander"));
      await waitFor(() =>
        expect(screen.getByRole("button", { name: /Upgraded/ })).toHaveAttribute("aria-pressed", "true"),
      );
      await expectLeavePrompts(user, false);
    });
  });

  describe("cross-tab saved-deck transactions", () => {
    const AUTOSAVE_V2 = JSON.stringify({
      main: [{ name: "Mountain", count: 2 }],
      sideboard: [],
      format: "Limited",
    });

    afterEach(() => {
      setSavedDeckTxnGateForTests(null);
      setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
    });

    it("a manual save refused while the autosave holds the ownership transition writes nothing, stays dirty, and succeeds on retry", async () => {
      const user = userEvent.setup();
      await seedAutosave();

      render(
        <DeckBuilder
          format="Limited"
          onFormatChange={vi.fn()}
          initialDeckName="[Autosave] Sealed"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />,
      );
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("[Autosave] Sealed"));

      let reachedResolve!: () => void;
      const reached = new Promise<void>((resolve) => {
        reachedResolve = resolve;
      });
      let releaseGate!: () => void;
      const gateHeld = new Promise<void>((resolve) => {
        releaseGate = resolve;
      });
      setSavedDeckTxnGateForTests((phase) => {
        if (phase === "draft-autosave-after-owner-selection") {
          reachedResolve();
          return gateHeld;
        }
      });

      const autosave = writeDraftAutosaveDeck("Sealed", "[Autosave] Sealed", AUTOSAVE_V2);
      await reached;

      await user.click(await screen.findByRole("button", { name: "remove-Lightning Bolt" }));
      setSavedDeckTxnLockWaitForTests(50);
      await user.click(screen.getByRole("button", { name: "Save" }));

      await vi.waitFor(() => {
        expect(useAppNotificationStore.getState().notification).toEqual({
          title: "Couldn't save deck",
          description: "Another Phase tab is busy. Close other Phase tabs and try again.",
        });
      });

      const stillHeld = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
      expect(stillHeld.main).toEqual([{ name: "Lightning Bolt", count: 1 }]);
      expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBe("Sealed");

      await user.click(screen.getByRole("button", { name: /Menu/ }));
      expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
      await user.click(screen.getByRole("button", { name: "Cancel" }));

      setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
      releaseGate();
      await expect(autosave).resolves.toEqual({ status: "committed", value: "[Autosave] Sealed" });
      const autosaved = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
      expect(autosaved.main).toEqual([{ name: "Mountain", count: 2 }]);
      expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBe("Sealed");

      await user.click(screen.getByRole("button", { name: "Save" }));
      await waitFor(() => {
        const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
        expect(stored.main).toEqual([]);
      });
      expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBeUndefined();
      await vi.waitFor(() => {
        expect(useAppNotificationStore.getState().notification?.title).toBe("Deck saved");
      });
    });

    it("Save & continue refused by a busy library keeps the dialog open and the edits in place", async () => {
      const user = userEvent.setup();
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck A",
        JSON.stringify({ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [], format: "Standard" }),
      );
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck B",
        JSON.stringify({ main: [{ name: "Counterspell", count: 4 }], sideboard: [], format: "Standard" }),
      );

      render(
        <DeckBuilder
          format="Standard"
          onFormatChange={vi.fn()}
          initialDeckName="Deck A"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />,
      );
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
      await user.click(screen.getByRole("button", { name: "remove-Lightning Bolt" }));

      let releaseHolder!: () => void;
      const held = new Promise<void>((resolve) => {
        releaseHolder = resolve;
      });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });

      setSavedDeckTxnLockWaitForTests(50);
      await user.click(screen.getByRole("button", { name: "Load deck..." }));
      await user.click(screen.getByRole("option", { name: "Deck B" }));
      await user.click(screen.getByRole("button", { name: "Save & continue" }));

      await vi.waitFor(() => {
        expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't save deck");
      });
      expect(nameInput).toHaveValue("Deck A");
      expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
      const savedA = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}");
      expect(savedA.main).toEqual([{ name: "Lightning Bolt", count: 4 }]);

      setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
      releaseHolder();
      await holder;
    });

    it("Clone refused by a busy library writes no copy and leaves the editor alone", async () => {
      const user = userEvent.setup();
      localStorage.setItem(
        STORAGE_KEY_PREFIX + "Deck A",
        JSON.stringify({ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [], format: "Standard" }),
      );

      render(
        <DeckBuilder
          format="Standard"
          onFormatChange={vi.fn()}
          initialDeckName="Deck A"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />,
      );
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

      let releaseHolder!: () => void;
      const held = new Promise<void>((resolve) => {
        releaseHolder = resolve;
      });
      const holder = withSavedDeckLibrary(() => held);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(1);
      });

      setSavedDeckTxnLockWaitForTests(50);
      await user.click(screen.getByRole("button", { name: "Clone" }));

      await vi.waitFor(() => {
        expect(useAppNotificationStore.getState().notification?.title).toBe("Couldn't clone deck");
      });
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy")).toBeNull();
      expect(nameInput).toHaveValue("Deck A");

      setSavedDeckTxnLockWaitForTests(Number.POSITIVE_INFINITY);
      releaseHolder();
      await holder;
    });

    async function seedAutosave() {
      await writeDraftAutosaveDeck(
        "Sealed",
        "[Autosave] Sealed",
        JSON.stringify({
          main: [{ name: "Lightning Bolt", count: 1 }],
          sideboard: [],
          format: "Limited",
        }),
      );
    }

    // The maintainer's test: pause the autosave (tab A) right after it selects the deck it
    // owns, manually save the same deck in tab B, then resume the autosave and confirm the
    // manual data stays intact and unmarked.
    it("the autosave, paused after owner selection, does not clobber a manual save of the same deck", async () => {
      const user = userEvent.setup();
      await seedAutosave();

      render(
        <DeckBuilder
          format="Limited"
          onFormatChange={vi.fn()}
          initialDeckName="[Autosave] Sealed"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />,
      );
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("[Autosave] Sealed"));

      let reachedResolve!: () => void;
      const reached = new Promise<void>((resolve) => {
        reachedResolve = resolve;
      });
      let releaseGate!: () => void;
      const gateHeld = new Promise<void>((resolve) => {
        releaseGate = resolve;
      });
      setSavedDeckTxnGateForTests((phase) => {
        if (phase === "draft-autosave-after-owner-selection") {
          reachedResolve();
          return gateHeld;
        }
      });

      const autosave = writeDraftAutosaveDeck("Sealed", "[Autosave] Sealed", AUTOSAVE_V2);
      await reached;

      await user.click(await screen.findByRole("button", { name: "remove-Lightning Bolt" }));
      await user.click(screen.getByRole("button", { name: "Save" }));

      await vi.waitFor(async () => {
        const pending = ((await navigator.locks.query()).pending ?? []).length === 1;
        const stored = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
        expect(pending || (stored.main ?? null)?.length === 0).toBe(true);
      });

      releaseGate();
      await autosave;
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(0);
        expect((await navigator.locks.query()).pending).toHaveLength(0);
      });

      const persisted = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
      expect(persisted.main).toEqual([]);
      expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBeUndefined();
      await expect(autosave).resolves.toEqual({ status: "committed", value: "[Autosave] Sealed" });
    });

    // The mirror: pause the manual save (tab B) right after its data write, let the autosave
    // (tab A) run to completion, then resume the manual save.
    it("a manual save, paused after its data write, does not lose to a concurrent autosave", async () => {
      const user = userEvent.setup();
      await seedAutosave();

      render(
        <DeckBuilder
          format="Limited"
          onFormatChange={vi.fn()}
          initialDeckName="[Autosave] Sealed"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />,
      );
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("[Autosave] Sealed"));
      await user.click(await screen.findByRole("button", { name: "remove-Lightning Bolt" }));

      let reachedResolve!: () => void;
      const reached = new Promise<void>((resolve) => {
        reachedResolve = resolve;
      });
      let releaseGate!: () => void;
      const gateHeld = new Promise<void>((resolve) => {
        releaseGate = resolve;
      });
      setSavedDeckTxnGateForTests((phase) => {
        if (phase === "builder-save-after-data-write") {
          reachedResolve();
          return gateHeld;
        }
      });

      await user.click(screen.getByRole("button", { name: "Save" }));
      await reached;

      const autosave = writeDraftAutosaveDeck("Sealed", "[Autosave] Sealed", AUTOSAVE_V2);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      releaseGate();
      await autosave;
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(0);
        expect((await navigator.locks.query()).pending).toHaveLength(0);
      });

      const manual = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
      expect(manual.main).toEqual([]);
      expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBeUndefined();
      await expect(autosave).resolves.toEqual({ status: "committed", value: "[Autosave] Sealed (2)" });
      expect(getDeckMeta("[Autosave] Sealed (2)")?.autosaveSlot).toBe("Sealed");
    });

    // Rename: the manual rename is paused after the move and data write, and the autosave
    // arrives while it waits.
    it("a manual rename, paused after its data write, is not overtaken by a concurrent autosave", async () => {
      const user = userEvent.setup();
      await seedAutosave();

      render(
        <DeckBuilder
          format="Limited"
          onFormatChange={vi.fn()}
          initialDeckName="[Autosave] Sealed"
          searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
          onSearchFiltersChange={vi.fn()}
          onResetSearch={vi.fn()}
        />,
      );
      const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
      await waitFor(() => expect(nameInput).toHaveValue("[Autosave] Sealed"));
      await user.clear(nameInput);
      await user.type(nameInput, "My Sealed");

      let reachedResolve!: () => void;
      const reached = new Promise<void>((resolve) => {
        reachedResolve = resolve;
      });
      let releaseGate!: () => void;
      const gateHeld = new Promise<void>((resolve) => {
        releaseGate = resolve;
      });
      setSavedDeckTxnGateForTests((phase) => {
        if (phase === "builder-save-after-data-write") {
          reachedResolve();
          return gateHeld;
        }
      });

      await user.click(screen.getByRole("button", { name: "Save" }));
      await reached;

      const autosave = writeDraftAutosaveDeck("Sealed", "[Autosave] Sealed", AUTOSAVE_V2);
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).pending).toHaveLength(1);
      });

      releaseGate();
      await autosave;
      await vi.waitFor(async () => {
        expect((await navigator.locks.query()).held).toHaveLength(0);
        expect((await navigator.locks.query()).pending).toHaveLength(0);
      });

      const renamed = localStorage.getItem(STORAGE_KEY_PREFIX + "My Sealed");
      expect(renamed).not.toBeNull();
      expect(JSON.parse(renamed ?? "{}").main).toEqual([{ name: "Lightning Bolt", count: 1 }]);
      expect(getDeckMeta("My Sealed")?.autosaveSlot).toBeUndefined();

      const autosaved = JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "[Autosave] Sealed") ?? "{}");
      expect(autosaved.main).toEqual([{ name: "Mountain", count: 2 }]);
      expect(getDeckMeta("[Autosave] Sealed")?.autosaveSlot).toBe("Sealed");
      await expect(autosave).resolves.toEqual({ status: "committed", value: "[Autosave] Sealed" });
    });
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
    expect(useAppNotificationStore.getState().notification).toEqual({
      title: "Deck cloned",
      description: 'A copy was saved as "My Deck copy".',
    });
  });

  it("a Load that lands while save-time commander inference is pending does not apply the inferred deck", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Commander" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck B",
      JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Commander" }),
    );

    render(
      <DeckBuilder
        format="Commander"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    // The initial Load already called resolveCommander once; mock only the call the pending Save makes.
    let resolveInference!: (deck: unknown) => void;
    const deferred = new Promise((resolve) => {
      resolveInference = resolve;
    });
    vi.mocked(resolveCommander).mockImplementationOnce(() => deferred as never);

    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(() => expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(2));

    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck B" }));
    await waitFor(() => expect(nameInput).toHaveValue("Deck B"));

    resolveInference({
      main: [{ name: "Beta", count: 1 }],
      sideboard: [],
      commander: ["Alpha"],
    });

    await waitFor(() => {
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}").commander).toEqual(["Alpha"]);
    });
    expect(screen.getByText("1 Gamma")).toBeInTheDocument();
    expect(screen.queryByText("1 Beta")).not.toBeInTheDocument();
    expect(nameInput).toHaveValue("Deck B");
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck B") ?? "{}").main).toEqual([
      { name: "Gamma", count: 1 },
    ]);
  });

  it("an older Load that resolves after a newer Load does not overwrite the editor", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck B",
      JSON.stringify({ main: [{ name: "Beta", count: 1 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck C",
      JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    // The initial Load already called resolveCommander once; hold only the call Load B makes.
    let resolveB!: (deck: unknown) => void;
    const held = new Promise((resolve) => {
      resolveB = resolve;
    });
    vi.mocked(resolveCommander).mockImplementationOnce(() => held as never);

    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck B" }));
    await vi.waitFor(() => expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(2));

    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck C" }));
    await waitFor(() => expect(nameInput).toHaveValue("Deck C"));

    // Let Load B's now-resolved promise, and any state updates it triggers, settle
    // before asserting nothing changed.
    await act(async () => {
      resolveB({ main: [{ name: "Beta", count: 1 }], sideboard: [] });
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(nameInput).toHaveValue("Deck C");
    expect(screen.getByText("1 Gamma")).toBeInTheDocument();
    expect(screen.queryByText("1 Beta")).not.toBeInTheDocument();
  });

  it("a precon Load that resolves after a newer Load does not overwrite the editor", async () => {
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck C",
      JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Standard" }),
    );
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

    const { rerender } = render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    // The initial Load already called resolveCommander once; hold only the call the precon Load makes.
    let resolvePrecon!: (deck: unknown) => void;
    const held = new Promise((resolve) => {
      resolvePrecon = resolve;
    });
    vi.mocked(resolveCommander).mockImplementationOnce(() => held as never);

    rerender(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="[Pre-built] Secrets of Strixhaven (SOS)"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    await vi.waitFor(() => expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(2));

    rerender(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck C"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    await waitFor(() => expect(nameInput).toHaveValue("Deck C"));

    // Let the precon Load's now-resolved promise, and any state updates it triggers, settle
    // before asserting nothing changed.
    await act(async () => {
      resolvePrecon({ main: [{ name: "Island", count: 99 }], sideboard: [] });
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(nameInput).toHaveValue("Deck C");
    expect(screen.getByText("1 Gamma")).toBeInTheDocument();
    expect(screen.queryByText("99 Island")).not.toBeInTheDocument();
  });

  it("a precon Load that resolves after the user edits the current deck does not overwrite that edit", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Standard" }),
    );
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

    const { rerender } = render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    // The initial Load already called resolveCommander once; hold only the call the precon Load makes.
    let resolvePrecon!: (deck: unknown) => void;
    const held = new Promise((resolve) => {
      resolvePrecon = resolve;
    });
    vi.mocked(resolveCommander).mockImplementationOnce(() => held as never);

    rerender(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="[Pre-built] Secrets of Strixhaven (SOS)"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    await vi.waitFor(() => expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(2));

    // The user edits the still-displayed Deck A while the precon Load awaits.
    await user.click(await screen.findByRole("button", { name: "remove-Beta" }));
    expect(screen.queryByText("1 Beta")).not.toBeInTheDocument();

    // Let the precon Load's now-resolved promise, and any state updates it triggers, settle
    // before asserting the edit was not discarded.
    await act(async () => {
      resolvePrecon({ main: [{ name: "Island", count: 99 }], sideboard: [] });
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(nameInput).toHaveValue("Deck A");
    expect(screen.getByText("1 Alpha")).toBeInTheDocument();
    expect(screen.queryByText("1 Beta")).not.toBeInTheDocument();
    expect(screen.queryByText("99 Island")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
  });

  it("a Load that resolves after the user edits the current deck does not overwrite that edit", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck B",
      JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    // The initial Load already called resolveCommander once; hold only the call Load B makes.
    let resolveB!: (deck: unknown) => void;
    const held = new Promise((resolve) => {
      resolveB = resolve;
    });
    vi.mocked(resolveCommander).mockImplementationOnce(() => held as never);

    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck B" }));
    await vi.waitFor(() => expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(2));

    // The user edits the still-displayed Deck A while Load B awaits.
    await user.click(await screen.findByRole("button", { name: "remove-Beta" }));
    expect(screen.queryByText("1 Beta")).not.toBeInTheDocument();

    // Let Load B's now-resolved promise, and any state updates it triggers, settle
    // before asserting the edit was not discarded.
    await act(async () => {
      resolveB({ main: [{ name: "Gamma", count: 1 }], sideboard: [] });
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(nameInput).toHaveValue("Deck A");
    expect(screen.getByText("1 Alpha")).toBeInTheDocument();
    expect(screen.queryByText("1 Gamma")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
  });

  it("an edit made while save-time commander inference is pending does not apply the inferred deck", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Commander" }),
    );

    render(
      <DeckBuilder
        format="Commander"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    // The initial Load already called resolveCommander once; mock only the call the pending Save makes.
    let resolveInference!: (deck: unknown) => void;
    const deferred = new Promise((resolve) => {
      resolveInference = resolve;
    });
    vi.mocked(resolveCommander).mockImplementationOnce(() => deferred as never);

    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(() => expect(vi.mocked(resolveCommander)).toHaveBeenCalledTimes(2));

    await user.click(await screen.findByRole("button", { name: "remove-Beta" }));

    resolveInference({
      main: [{ name: "Beta", count: 1 }],
      sideboard: [],
      commander: ["Alpha"],
    });

    await waitFor(() => {
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}").commander).toEqual(["Alpha"]);
    });
    expect(screen.getByText("1 Alpha")).toBeInTheDocument();
    expect(screen.queryByText("1 Beta")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
  });

  it("with neither a Load nor an edit pending, the inferred deck is applied to the editor", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Commander" }),
    );
    render(
      <DeckBuilder
        format="Commander"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    // The initial Load already called resolveCommander once; mock only the call the Save makes.
    vi.mocked(resolveCommander).mockImplementationOnce(async () => ({
      main: [{ name: "Beta", count: 1 }],
      sideboard: [],
      commander: ["Alpha"],
    }));

    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => {
      expect(screen.getByText("1 Beta")).toBeInTheDocument();
      expect(screen.queryByText("1 Alpha")).not.toBeInTheDocument();
    });
  });

  it("a Load during a pending Clone leaves the clone under its own name and the editor on the loaded deck", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck B",
      JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck B" }));
    await waitFor(() => expect(nameInput).toHaveValue("Deck B"));

    releaseHolder();
    await holder;

    await waitFor(() => {
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy") ?? "{}").main).toEqual([
        { name: "Alpha", count: 1 },
        { name: "Beta", count: 1 },
      ]);
    });
    expect(nameInput).toHaveValue("Deck B");
    expect(screen.getByText("1 Gamma")).toBeInTheDocument();
    expect(useAppNotificationStore.getState().notification?.title).toBe("Deck cloned");

    // Editing and saving the now-loaded Deck B must not touch the clone the pending Clone left
    // under its own name.
    await user.click(await screen.findByRole("button", { name: "remove-Gamma" }));
    await user.click(screen.getByRole("button", { name: /^(Save|Saved ✓)$/ }));
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck B") ?? "{}").main).toEqual([]),
    );
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy") ?? "{}").main).toEqual([
      { name: "Alpha", count: 1 },
      { name: "Beta", count: 1 },
    ]);
  });

  it("an edit during a pending Clone keeps the deck dirty and leaves the clone under its own name", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Standard" }),
    );

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    await user.click(await screen.findByRole("button", { name: "remove-Beta" }));

    releaseHolder();
    await holder;

    await waitFor(() => {
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy") ?? "{}").main).toEqual([
        { name: "Alpha", count: 1 },
        { name: "Beta", count: 1 },
      ]);
    });
    expect(nameInput).toHaveValue("Deck A");
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    expect(await screen.findByRole("button", { name: "Discard" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(useAppNotificationStore.getState().notification?.title).toBe("Deck cloned");

    // Saving now must go to Deck A, not the clone: the edit during the pending Clone must have
    // stopped handleClone's in-transaction write from claiming the ref for the copy.
    await user.click(screen.getByRole("button", { name: /^(Save|Saved ✓)$/ }));
    await waitFor(() =>
      expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A") ?? "{}").main).toEqual([
        { name: "Alpha", count: 1 },
      ]),
    );
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy") ?? "{}").main).toEqual([
      { name: "Alpha", count: 1 },
      { name: "Beta", count: 1 },
    ]);
  });

  it("a Load during a pending Clone keeps the clone in the source's folder, not the loaded deck's", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck B",
      JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Standard" }),
    );
    const folderA = createFolder(testSavedDeckTxn, "FA")!;
    const folderB = createFolder(testSavedDeckTxn, "FB")!;
    setDeckFolder(testSavedDeckTxn, "Deck A", folderA.id);
    setDeckFolder(testSavedDeckTxn, "Deck B", folderB.id);

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck B" }));
    await waitFor(() => expect(nameInput).toHaveValue("Deck B"));

    releaseHolder();
    await holder;

    await waitFor(() =>
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy")).not.toBeNull(),
    );
    // The copy belongs beside its source (Deck A, in folder A) — a Load of Deck B that lands
    // mid-wait must not retarget the copy's folder onto Deck B's folder.
    expect(getDeckMeta("Deck A copy")?.folderId).toBe(folderA.id);
  });

  it("a rename-Save queued ahead of a Clone still leaves the clone beside the renamed deck's folder", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }], sideboard: [], format: "Standard" }),
    );
    const folderA = createFolder(testSavedDeckTxn, "FA")!;
    setDeckFolder(testSavedDeckTxn, "Deck A", folderA.id);

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    // S1: rename-Save of Deck A -> Deck A2, queued behind the held lock.
    await user.clear(nameInput);
    await user.type(nameInput, "Deck A2");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    // C1: Clone, queued behind S1.
    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(2);
    });

    releaseHolder();
    await holder;

    await waitFor(() =>
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A2 copy")).not.toBeNull(),
    );
    // S1 moved Deck A's metadata (including its folder) onto Deck A2 before C1's transaction
    // ran; the clone must land in that folder, not with no folder at all.
    expect(getDeckMeta("Deck A2 copy")?.folderId).toBe(folderA.id);
    expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy")).toBeNull();
  });

  it("a rename-Save queued ahead of a Clone, then a Load while both wait, still lands the clone in the click-time folder", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }], sideboard: [], format: "Standard" }),
    );
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck B",
      JSON.stringify({ main: [{ name: "Gamma", count: 1 }], sideboard: [], format: "Standard" }),
    );
    const folderA = createFolder(testSavedDeckTxn, "FA")!;
    setDeckFolder(testSavedDeckTxn, "Deck A", folderA.id);

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    // S1: rename-Save of Deck A -> Deck A2, queued behind the held lock.
    await user.clear(nameInput);
    await user.type(nameInput, "Deck A2");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    // C1: Clone, queued behind S1.
    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(2);
    });

    // A Load of Deck B lands while S1 and C1 both still wait on the held lock. The queued
    // rename left the editor dirty, so the Load is behind the unsaved-changes confirm — discard
    // to load directly rather than queuing yet another Save.
    await user.click(screen.getByRole("button", { name: "Load deck..." }));
    await user.click(screen.getByRole("option", { name: "Deck B" }));
    await user.click(await screen.findByRole("button", { name: "Discard" }));
    await waitFor(() => expect(nameInput).toHaveValue("Deck B"));

    releaseHolder();
    await holder;

    await waitFor(() =>
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A2 copy")).not.toBeNull(),
    );
    // The clone's folder was decided at the Clone click, before the Load — it must land in
    // FA regardless of the Load that raced it to the lock.
    expect(getDeckMeta("Deck A2 copy")?.folderId).toBe(folderA.id);
  });

  it("clicking Clone twice while a rename-Save is queued ahead of both leaves both copies in the click-time folder", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }], sideboard: [], format: "Standard" }),
    );
    const folderA = createFolder(testSavedDeckTxn, "FA")!;
    setDeckFolder(testSavedDeckTxn, "Deck A", folderA.id);

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    // S1: rename-Save of Deck A -> Deck A2, queued behind the held lock.
    await user.clear(nameInput);
    await user.type(nameInput, "Deck A2");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    // C1, C2: Clone clicked twice, both queued behind S1.
    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(2);
    });
    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(3);
    });

    releaseHolder();
    await holder;

    await waitFor(() =>
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A2 copy 2")).not.toBeNull(),
    );
    expect(getDeckMeta("Deck A2 copy")?.folderId).toBe(folderA.id);
    expect(getDeckMeta("Deck A2 copy 2")?.folderId).toBe(folderA.id);
  });

  it("a folder deleted between the Clone click and its transaction leaves the copy with no folder", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "Deck A",
      JSON.stringify({ main: [{ name: "Alpha", count: 1 }], sideboard: [], format: "Standard" }),
    );
    const folderA = createFolder(testSavedDeckTxn, "FA")!;
    setDeckFolder(testSavedDeckTxn, "Deck A", folderA.id);

    render(
      <DeckBuilder
        format="Standard"
        onFormatChange={vi.fn()}
        initialDeckName="Deck A"
        searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
        onSearchFiltersChange={vi.fn()}
        onResetSearch={vi.fn()}
      />,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));

    let releaseHolder!: () => void;
    const held = new Promise<void>((resolve) => {
      releaseHolder = resolve;
    });
    const holder = withSavedDeckLibrary(() => held);
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).held).toHaveLength(1);
    });

    // D1: delete folder FA through the real transaction path, queued behind the held lock —
    // this must run before the Clone click below so its transaction sees FA already gone.
    const deletion = withSavedDeckLibrary((txn) => deleteFolder(txn, folderA.id));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    // C1: Clone, clicked (and its folder captured) while FA still exists, queued behind D1.
    await user.click(screen.getByRole("button", { name: "Clone" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(2);
    });

    releaseHolder();
    await holder;
    await deletion;

    await waitFor(() =>
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "Deck A copy")).not.toBeNull(),
    );
    expect(listFolders().some((f) => f.id === folderA.id)).toBe(false);
    // The folder existed when Clone was clicked but was gone by the time its transaction ran —
    // the copy must not carry the now-dangling id.
    expect(getDeckMeta("Deck A copy")?.folderId).toBeUndefined();
  });

  it("clones into the source's folder but starts the copy unstarred", async () => {
    const user = userEvent.setup();
    localStorage.setItem(
      STORAGE_KEY_PREFIX + "My Deck",
      JSON.stringify({
        main: [{ name: "Lightning Bolt", count: 4 }],
        sideboard: [],
        format: "Standard",
      }),
    );
    const folder = createFolder(testSavedDeckTxn, "Commander")!;
    setDeckFolder(testSavedDeckTxn, "My Deck", folder.id);
    toggleDeckStar(testSavedDeckTxn, "My Deck");

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

    await waitFor(() =>
      expect(localStorage.getItem(STORAGE_KEY_PREFIX + "My Deck copy")).not.toBeNull(),
    );
    // The clone inherits the folder, but the star is a deliberate per-deck pin.
    const meta = getDeckMeta("My Deck copy");
    expect(meta?.folderId).toBe(folder.id);
    expect(meta?.starred).toBeUndefined();
    // Source deck keeps its own star.
    expect(getDeckMeta("My Deck")?.starred).toBe(true);
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
