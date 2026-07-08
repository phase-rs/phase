import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { DeckList } from "../DeckList";

afterEach(cleanup);

const emptyDeck = { main: [], sideboard: [] };

describe("DeckList commander section", () => {
  it("shows designated commanders in list view and demotes them via the remove button", () => {
    // The bug this guards: once set, a commander is filtered out of deck.main,
    // so without this pinned section it's invisible/unremovable in list view
    // (the Info-panel CommanderPanel is on a different tab on mobile).
    const onRemoveCommander = vi.fn();
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        commanders={["Krenko, Mob Boss"]}
        onRemoveCommander={onRemoveCommander}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    expect(screen.getByText("Krenko, Mob Boss")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: /remove krenko, mob boss as commander/i }),
    );
    expect(onRemoveCommander).toHaveBeenCalledWith("Krenko, Mob Boss");
  });

  it("self-hides the commander section outside commander formats (no commanders)", () => {
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );
    expect(screen.queryByText(/as commander/i)).not.toBeInTheDocument();
  });
});

describe("DeckList import modal", () => {
  it("shows an error and keeps the modal open when pasted text has no cards", async () => {
    const onImport = vi.fn();
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onMoveCard={vi.fn()}
        onImport={onImport}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    fireEvent.change(screen.getByPlaceholderText(/paste deck list/i), {
      target: { value: "asdasd" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));

    await waitFor(() => {
      expect(screen.getByText(/couldn't find any cards/i)).toBeInTheDocument();
    });
    expect(onImport).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /^parse$/i })).toBeInTheDocument();
  });
});
