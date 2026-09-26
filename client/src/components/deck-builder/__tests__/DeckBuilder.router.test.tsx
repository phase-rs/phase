import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect } from "react";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter, Routes, Route, useLocation, useNavigate } from "react-router";
import userEvent from "@testing-library/user-event";

import { DeckBuilder } from "../DeckBuilder";
import { loadPreconDeckMap } from "../../../hooks/useDecks";
import { resolveCommander } from "../../../services/deckParser";
import { STORAGE_KEY_PREFIX } from "../../../constants/storage";
import { withSavedDeckLibrary } from "../../../services/savedDeckTransaction";
import {
  installFifoWebLocks,
  resetSavedDeckLibraryForTests,
  uninstallWebLocks,
} from "../../../test/helpers/webLocks";

vi.mock("../../../hooks/useIsMobile", () => ({ useIsMobile: vi.fn(() => false) }));
vi.mock("../../../hooks/useDeckCardData", () => ({
  useDeckCardData: () => ({ cardDataCache: new Map(), cacheCards: vi.fn() }),
}));
vi.mock("../../../hooks/useDecks", () => ({ loadPreconDeckMap: vi.fn() }));
vi.mock("../../../services/deckParser", async () => {
  const actual = await vi.importActual<typeof import("../../../services/deckParser")>("../../../services/deckParser");
  return { ...actual, resolveCommander: vi.fn(async (deck) => deck) };
});
vi.mock("../CardSearch", () => ({
  CardSearch: ({ onResults }: { onResults: (cards: unknown[], total: number) => void }) => {
    useEffect(() => { onResults([], 0); }, [onResults]);
    return <div>Card Search</div>;
  },
}));
vi.mock("../DeckStack", () => ({ DeckStack: () => <div>Deck Stack</div> }));
vi.mock("../DeckList", () => ({
  DeckList: ({
    deck,
    onRemoveCard,
  }: {
    deck: { main: Array<{ name: string; count: number }> };
    onRemoveCard: (name: string, section: "main" | "sideboard") => void;
  }) => (
    <div>
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
vi.mock("../ManaCurve", () => ({ ManaCurve: () => <div>Mana Curve</div> }));
vi.mock("../FormatFilter", () => ({ FormatFilter: () => <div>Format Filter</div> }));
vi.mock("../CommanderPanel", () => ({ CommanderPanel: () => <div>Commander Panel</div> }));

let currentPath = "";
let goBack!: () => void;
function LocationSpy() {
  currentPath = useLocation().pathname;
  const navigate = useNavigate();
  goBack = () => navigate(-1);
  return null;
}

const builderElement = (
  <DeckBuilder
    format="Standard"
    onFormatChange={vi.fn()}
    initialDeckName="Deck A"
    backPath="/back"
    searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
    onSearchFiltersChange={vi.fn()}
    onResetSearch={vi.fn()}
  />
);

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

function seed() {
  localStorage.setItem(
    STORAGE_KEY_PREFIX + "Deck A",
    JSON.stringify({ main: [{ name: "Alpha", count: 1 }, { name: "Beta", count: 1 }], sideboard: [], format: "Standard" }),
  );
}

beforeEach(async () => {
  installFifoWebLocks();
  await resetSavedDeckLibraryForTests();
});

afterEach(() => {
  cleanup();
  uninstallWebLocks();
  localStorage.clear();
  currentPath = "";
  vi.mocked(resolveCommander).mockReset();
  vi.mocked(resolveCommander).mockImplementation(async (deck) => deck);
  vi.mocked(loadPreconDeckMap).mockReset();
});

describe("Save & continue that outlives the builder's presence in the router tree", () => {
  it("leaving the builder via browser back while Save & continue waits does not navigate once the save completes", async () => {
    seed();
    const user = userEvent.setup();
    render(
      <MemoryRouter initialEntries={["/start", "/db"]} initialIndex={1}>
        <LocationSpy />
        <Routes>
          <Route path="/db" element={builderElement} />
          <Route path="/start" element={<div>start page</div>} />
          <Route path="/back" element={<div>back page</div>} />
        </Routes>
      </MemoryRouter>,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
    await user.click(screen.getByRole("button", { name: "remove-Beta" }));

    const release = await holdLibrary();
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    await user.click(screen.getByRole("button", { name: "Save & continue" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    // Navigate the router away from the builder while its save is still queued.
    await act(async () => {
      goBack();
    });
    await screen.findByText("start page");
    const pathAfterLeaving = currentPath;

    await release();

    expect(currentPath).toBe(pathAfterLeaving);
  });

  it("staying on the builder while Save & continue waits navigates once the save completes (paired positive)", async () => {
    seed();
    const user = userEvent.setup();
    render(
      <MemoryRouter initialEntries={["/start", "/db"]} initialIndex={1}>
        <LocationSpy />
        <Routes>
          <Route path="/db" element={builderElement} />
          <Route path="/start" element={<div>start page</div>} />
          <Route path="/back" element={<div>back page</div>} />
        </Routes>
      </MemoryRouter>,
    );
    const nameInput = await screen.findByRole("textbox", { name: "Deck name" });
    await waitFor(() => expect(nameInput).toHaveValue("Deck A"));
    await user.click(screen.getByRole("button", { name: "remove-Beta" }));

    const release = await holdLibrary();
    await user.click(screen.getByRole("button", { name: /Menu/ }));
    await user.click(screen.getByRole("button", { name: "Save & continue" }));
    await vi.waitFor(async () => {
      expect((await navigator.locks.query()).pending).toHaveLength(1);
    });

    await release();
    await waitFor(() => expect(currentPath).toBe("/back"));
  });
});

