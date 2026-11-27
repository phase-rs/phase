import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameFormat } from "../../../adapter/types";
import { DeckList } from "../DeckList";

const mocks = vi.hoisted(() => ({
  canonicalCardNames: vi.fn(
    async (names: string[]): Promise<(string | null)[]> => names.map(() => null),
  ),
  inferCommander: vi.fn<typeof import("../../../services/engineRuntime").inferCommander>(),
}));

vi.mock("../../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ canonicalCardNames: mocks.canonicalCardNames }),
}));

vi.mock("../../../services/engineRuntime", () => ({
  inferCommander: mocks.inferCommander,
  sideboardPolicyForFormat: async () => ({ type: "Forbidden" }),
}));

beforeEach(() => {
  mocks.canonicalCardNames.mockReset();
  mocks.canonicalCardNames.mockImplementation(async (names: string[]) => names.map(() => null));
  mocks.inferCommander.mockReset();
  mocks.inferCommander.mockResolvedValue(null);
});

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
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
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
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
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
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
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

    expect(
      await screen.findByText(/couldn't find any cards/i),
    ).toBeInTheDocument();
    expect(onImport).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /^parse$/i })).toBeInTheDocument();
  });

  it("imports pasted card names in the engine's canonical spelling", async () => {
    mocks.canonicalCardNames.mockImplementation(async (names: string[]) =>
      names.map((name) => (name === "Revival/Revenge" ? "Revival // Revenge" : null)),
    );
    const onImport = vi.fn();
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={onImport}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    fireEvent.change(screen.getByPlaceholderText(/paste deck list/i), {
      target: { value: "1 Revival/Revenge" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));

    await waitFor(() => expect(onImport).toHaveBeenCalled());
    expect(onImport.mock.calls[0][0].main).toEqual([
      { count: 1, name: "Revival // Revenge" },
    ]);
  });
});

describe("DeckList commander inference on paste", () => {
  const gollum = { name: "Gollum, Silent Slinker", count: 1 };
  const sengir = { name: "Sengir Vampire", count: 1 };
  const filler = [
    { name: "Relentless Rats", count: 58 },
    { name: "Swamp", count: 40 },
  ];
  const rows = [gollum, sengir, ...filler];
  const paste = rows.map(({ count, name }) => `${count} ${name}`).join("\n");

  beforeEach(() => {
    // Same counted rows and answers as the real-WASM Commander integration.
    mocks.inferCommander.mockImplementation(async (_rows, format) =>
      format === "PauperCommander" ? sengir.name : gollum.name,
    );
  });

  function renderList(selectedFormat?: GameFormat) {
    const onImport = vi.fn();
    const list = (format?: GameFormat) => (
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={onImport}
        format={format ?? "Standard"}
        commanderInferenceFormat={format}
        cardDataCache={new Map()}
        groupMode="type"
      />
    );
    const { rerender } = render(list(selectedFormat));
    return { onImport, selectFormat: (format: GameFormat) => rerender(list(format)) };
  }

  function pasteCountedDeck() {
    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    fireEvent.change(screen.getByPlaceholderText(/paste deck list/i), {
      target: { value: paste },
    });
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));
  }

  it("imports the inferred commander for the selected format", async () => {
    const { onImport } = renderList("PauperCommander");
    pasteCountedDeck();

    await waitFor(() => expect(onImport).toHaveBeenCalledTimes(1));
    expect.soft(mocks.inferCommander.mock.calls).toEqual([[rows, "PauperCommander"]]);
    expect(onImport).toHaveBeenCalledWith(expect.objectContaining({
      commander: [sengir.name],
      main: [gollum, ...filler],
    }));
  });

  it("discovers an ordinary commander without an inference context", async () => {
    const { onImport } = renderList();
    pasteCountedDeck();

    await waitFor(() => expect(onImport).toHaveBeenCalledTimes(1));
    expect(mocks.inferCommander.mock.calls).toEqual([[rows]]);
    expect(onImport).toHaveBeenCalledWith(expect.objectContaining({
      commander: [gollum.name],
      main: [sengir, ...filler],
    }));
  });

  it("retires a pending inference when the selected format changes", async () => {
    let resolveInference!: (name: string) => void;
    mocks.inferCommander.mockReturnValueOnce(new Promise((resolve) => {
      resolveInference = resolve;
    }));
    const { onImport, selectFormat } = renderList("PauperCommander");
    pasteCountedDeck();
    await waitFor(() => expect(mocks.inferCommander.mock.calls).toEqual([[rows, "PauperCommander"]]));

    selectFormat("Commander");
    await act(async () => resolveInference(sengir.name));

    expect(onImport).not.toHaveBeenCalled();
    expect(screen.getByPlaceholderText(/paste deck list/i)).toHaveValue(paste);
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));
    await waitFor(() => expect(onImport).toHaveBeenCalledTimes(1));
    expect(mocks.inferCommander.mock.calls).toEqual([[rows, "PauperCommander"], [rows, "Commander"]]);
    expect(onImport).toHaveBeenCalledWith(expect.objectContaining({
      commander: [gollum.name],
      main: [sengir, ...filler],
    }));
  });
});

describe("DeckList export", () => {
  it("leaves export to the deck builder toolbar", () => {
    render(
      <DeckList
        deck={{ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [] }}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    expect(screen.getByRole("button", { name: "Import" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /export/i })).not.toBeInTheDocument();
  });
});
