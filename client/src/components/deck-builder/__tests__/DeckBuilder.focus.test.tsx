import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { DeckBuilder } from "../DeckBuilder";

vi.mock("react-router", () => ({
  useNavigate: () => vi.fn(),
}));

vi.mock("../../../hooks/useIsMobile", () => ({
  useIsMobile: () => false,
}));

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

vi.mock("../../../hooks/usePrintingsLoaded", () => ({
  usePrintingsLoaded: () => true,
}));

vi.mock("../../../services/scryfall", () => ({
  IMAGE_SIZE_WIDTHS: { small: 146, normal: 488 },
  deriveImageUrl: (src: string) => src,
  resolveOracleIdSync: () => "oracle-1",
  hasAlternatePrintingsSync: () => true,
  imageUrlSize: () => null,
  getCardPrintings: () => Promise.resolve([]),
}));

vi.mock("../CardSearch", () => ({
  CardSearch: () => <div>Card Search</div>,
}));

vi.mock("../StatsPanel", () => ({
  StatsPanel: () => <div>Deck statistics</div>,
}));

vi.mock("../useDeckBuilder", async () => {
  const { useState } = await vi.importActual<typeof import("react")>("react");
  const card = {
    id: "sol-ring",
    name: "Sol Ring",
    mana_cost: "{1}",
    cmc: 1,
    type_line: "Artifact",
    color_identity: [],
    legalities: {},
    oracle_text: "{T}: Add {C}{C}.",
  };

  return {
    useDeckBuilder() {
      const deck = { main: [{ name: "Sol Ring", count: 1 }], sideboard: [] };
      const [activeSurface, setActiveSurface] = useState<"deck" | "info">("deck");
      const [deckView, setDeckView] = useState<"list" | "stack">("list");
      const [groupMode, setGroupMode] = useState<"type" | "color">("type");
      const [listContextMenu, setListContextMenu] = useState<{
        cardName: string;
        x: number;
        y: number;
      } | null>(null);
      const [listPickerCard, setListPickerCard] = useState<{
        cardName: string;
        oracleId: string;
      } | null>(null);

      return {
        deck,
        searchResults: [],
        deckName: "Focus test deck",
        setDeckName: vi.fn(),
        bracket: null,
        setBracket: vi.fn(),
        savedDecks: [],
        justSaved: false,
        setJustSaved: vi.fn(),
        commanders: [],
        activeSurface,
        setActiveSurface,
        deckView,
        setDeckView,
        groupMode,
        setGroupMode,
        dirty: false,
        cardDataCache: new Map([["Sol Ring", card]]),
        compatibility: null,
        artOverrides: {},
        listContextMenu,
        setListContextMenu,
        listPickerCard,
        setListPickerCard,
        currentDeck: deck,
        isCommander: false,
        deckSizeRule: { type: "Minimum", data: 60 },
        estimate: null,
        auditEmptyReason: "not-commander" as const,
        cmcValues: [1],
        colorDistribution: [],
        cardCounts: new Map([["Sol Ring", 1]]),
        warnings: [],
        handleListContextMenu: (cardName: string, x: number, y: number) => {
          setListContextMenu({ cardName, x, y });
        },
        handleListChooseArt: () => {
          if (!listContextMenu) return;
          setListPickerCard({
            cardName: listContextMenu.cardName,
            oracleId: "oracle-1",
          });
        },
        handleListClearOverride: vi.fn(),
        handleOpenArtPicker: (cardName: string) => {
          setListPickerCard({ cardName, oracleId: "oracle-1" });
        },
        handleScrollToCard: vi.fn(),
        handleSearchResults: vi.fn(),
        handleSearchTrigger: vi.fn(),
        handleAddCard: vi.fn(),
        handleAddCardByName: vi.fn(),
        handleRemoveCard: vi.fn(),
        handleIncrementCard: vi.fn(),
        canIncrement: () => true,
        handleMoveCard: vi.fn(),
        handleImport: vi.fn(),
        handleSave: vi.fn(),
        handleClone: vi.fn(),
        handleLoad: vi.fn(),
        handleSetCommander: vi.fn(),
        isCommanderEligible: () => false,
        handleRemoveCommander: vi.fn(),
        signatureSpellCandidates: null,
        companionCandidateNames: null,
        handleSetSignatureSpell: vi.fn(),
        handleRemoveSignatureSpell: vi.fn(),
        handleSetCompanion: vi.fn(),
        handleRemoveCompanion: vi.fn(),
      };
    },
  };
});

class ResizeObserverMock {
  observe(): void {}
  disconnect(): void {}
  unobserve(): void {}
}

function renderBuilder() {
  return render(
    <DeckBuilder
      format="Standard"
      onFormatChange={vi.fn()}
      searchFilters={{ text: "", colors: [], type: "", sets: [], browseFormat: "all" }}
      onSearchFiltersChange={vi.fn()}
      onResetSearch={vi.fn()}
    />,
  );
}

function getStackTile(cardName: string): HTMLElement {
  const removeButton = screen.getByTitle(`Remove one ${cardName}`);
  const controls = removeButton.parentElement;
  const tile = controls?.parentElement;
  if (!tile) throw new Error(`Missing stack tile for ${cardName}`);
  return tile;
}

describe("DeckBuilder art-picker focus restoration", () => {
  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("returns the list picker to the exact persistent alternate-art button", async () => {
    renderBuilder();
    const launcher = screen.getByRole("button", { name: "Choose art for Sol Ring" });
    const deckPanel = screen.getByRole("tabpanel", { name: /deck/i });

    screen.getByRole("button", { name: "Search" }).focus();
    fireEvent.click(launcher);

    const dialog = screen.getByRole("dialog", { name: "Choose Art" });
    expect(dialog).toBeInTheDocument();
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() => expect(launcher).toHaveFocus());
    expect(deckPanel).not.toHaveFocus();
  });

  it("returns the stack picker to the deck tabpanel after its transient menu closes", async () => {
    renderBuilder();
    fireEvent.click(screen.getByRole("button", { name: "Stack view" }));

    const deckPanel = screen.getByRole("tabpanel", { name: /deck/i });
    fireEvent.contextMenu(getStackTile("Sol Ring"), { clientX: 20, clientY: 20 });
    const transientMenuItem = screen.getByRole("menuitem", { name: "Choose Art…" });
    transientMenuItem.focus();
    expect(transientMenuItem).toHaveFocus();
    fireEvent.click(transientMenuItem);

    const dialog = screen.getByRole("dialog", { name: "Choose Art" });
    expect(dialog).toBeInTheDocument();
    expect(transientMenuItem).not.toBeInTheDocument();
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() => expect(deckPanel).toHaveFocus());
  });

  it("returns the list context-menu picker to the durable deck tabpanel", async () => {
    renderBuilder();
    const deckPanel = screen.getByRole("tabpanel", { name: /deck/i });
    const row = document.querySelector<HTMLElement>(
      '[data-card-name="sol ring"] > div',
    );
    expect(row).not.toBeNull();
    fireEvent.contextMenu(row!, { clientX: 20, clientY: 20 });

    const transientMenuItem = screen.getByRole("menuitem", {
      name: "Choose Art…",
    });
    transientMenuItem.focus();
    fireEvent.click(transientMenuItem);

    const dialog = screen.getByRole("dialog", { name: "Choose Art" });
    expect(transientMenuItem).not.toBeInTheDocument();
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() => expect(deckPanel).toHaveFocus());
  });
});
