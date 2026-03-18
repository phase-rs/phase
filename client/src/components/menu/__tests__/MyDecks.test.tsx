import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { MyDecks } from "../MyDecks";
import { STORAGE_KEY_PREFIX } from "../../../constants/storage";
import type { ParsedDeck } from "../../../services/deckParser";
import { evaluateDeckCompatibilityBatch } from "../../../services/deckCompatibility";

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

vi.mock("../../../services/deckCompatibility", () => ({
  evaluateDeckCompatibilityBatch: vi.fn(),
}));

function saveDeck(name: string, deck: ParsedDeck): void {
  localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify(deck));
}

describe("MyDecks", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it("prefilters commander selection context and allows show-all override", async () => {
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

    await waitFor(() => {
      expect(evaluateDeckCompatibilityBatch).toHaveBeenCalled();
      expect(screen.queryByText("Off Format")).not.toBeInTheDocument();
    });
    expect(screen.getByText("Commander Ready")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Show all decks" }));
    expect(screen.getByText("Off Format")).toBeInTheDocument();
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
        activeDeckName={null}
        onSelectDeck={vi.fn()}
        onConfirmSelection={vi.fn()}
      />,
    );

    expect(await screen.findByText("Badge Deck")).toBeInTheDocument();
    expect(screen.getByText("STD")).toBeInTheDocument();
    expect(screen.queryByText("CMD")).not.toBeInTheDocument();
    expect(screen.getByText("BO3", { selector: "span" })).toBeInTheDocument();
    expect(screen.getByText("Unknown 1")).toBeInTheDocument();
  });
});
