import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CardGrid } from "../CardGrid";
import { DeckStack } from "../DeckStack";
import { PrintingPickerModal } from "../PrintingPickerModal";
import { getCardPrintings } from "../../../services/scryfall";
import type { ScryfallCard } from "../../../services/scryfall";

const { useCardImage } = vi.hoisted(() => ({ useCardImage: vi.fn() }));

vi.mock("../../../hooks/useCardImage", () => ({ useCardImage }));
vi.mock("../../../services/scryfall", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../services/scryfall")>();
  return { ...actual, getCardPrintings: vi.fn() };
});

class ResizeObserverMock {
  observe(): void {}
  disconnect(): void {}
  unobserve(): void {}
}

function card(name: string): ScryfallCard {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    oracle_id: "22222222-2222-4222-8222-222222222222",
    name,
    mana_cost: "{R}",
    cmc: 1,
    type_line: "Instant",
    color_identity: ["R"],
    legalities: {},
  };
}

describe("Phase 08 deck-builder visual consumers", () => {
  afterEach(() => {
    cleanup();
    useCardImage.mockReset();
    vi.unstubAllGlobals();
  });

  it("submits exact CardGrid printing identity and advances the rendered art crop", () => {
    const advanceFailedSource = vi.fn();
    useCardImage.mockReturnValue({
      src: "visual-pack://installed/crop",
      isLoading: false,
      advanceFailedSource,
    });
    const result = card("Lightning Bolt");

    render(<CardGrid cards={[result]} onAddCard={vi.fn()} />);

    expect(useCardImage).toHaveBeenCalledWith("Lightning Bolt", {
      oracleId: result.oracle_id,
      scryfallId: result.id,
      size: "art_crop",
    });
    fireEvent.error(screen.getByRole("img", { name: "Lightning Bolt" }));
    expect(advanceFailedSource).toHaveBeenCalledOnce();
    expect(advanceFailedSource).toHaveBeenCalledWith("visual-pack://installed/crop");
  });

  it("keeps the CardGrid named fallback when no source resolves", () => {
    useCardImage.mockReturnValue({ src: null, isLoading: false });

    render(<CardGrid cards={[card("Lightning Bolt")]} onAddCard={vi.fn()} />);

    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    expect(screen.getAllByText("Lightning Bolt")).toHaveLength(2);
  });

  it("submits source printing without a cache printing ID and couples installed rungs", () => {
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
    const advanceFailedSource = vi.fn();
    useCardImage.mockReturnValue({
      src: "visual-pack://installed/normal",
      isLoading: false,
      rungs: {
        small: "visual-pack://installed/small",
        normal: "visual-pack://installed/normal",
      },
      advanceFailedSource,
    });

    render(
      <DeckStack
        deck={{
          main: [{
            name: "Lightning Bolt",
            count: 1,
            sourcePrinting: { setCode: "2XM", collectorNumber: "117" },
          }],
          sideboard: [],
        }}
        commanders={[]}
        cardDataCache={new Map([["Lightning Bolt", card("Lightning Bolt")]])}
        onAddCard={vi.fn()}
        canAddCard={() => true}
        onRemoveCard={vi.fn()}
        onMoveCard={vi.fn()}
        onRemoveCommander={vi.fn()}
        groupMode="type"
      />,
    );

    expect(useCardImage).toHaveBeenCalledWith("Lightning Bolt", {
      size: "normal",
      sourcePrinting: { setCode: "2XM", collectorNumber: "117" },
    });
    const image = screen.getByRole("img", { name: "Lightning Bolt" });
    expect(image).toHaveAttribute(
      "srcset",
      "visual-pack://installed/small 146w, visual-pack://installed/normal 488w",
    );
    expect(image).toHaveAttribute("sizes", "auto, 200px");
    fireEvent.error(image);
    expect(advanceFailedSource).toHaveBeenCalledWith("visual-pack://installed/normal");
  });

  it("keeps picker printing identities distinct and advances only the failed tile source", async () => {
    const firstAdvance = vi.fn();
    const secondAdvance = vi.fn();
    vi.mocked(getCardPrintings).mockResolvedValue([
      {
        id: "33333333-3333-4333-8333-333333333333",
        set: "one",
        set_name: "Set One",
        collector_number: "1",
        released_at: "2024-01-01",
        border_color: "black",
        frame_effects: [],
        full_art: false,
        faces: [{
          normal: "https://example.invalid/one-normal.jpg",
          art_crop: "https://example.invalid/one-crop.jpg",
        }],
      },
      {
        id: "44444444-4444-4444-8444-444444444444",
        set: "two",
        set_name: "Set Two",
        collector_number: "2",
        released_at: "2023-01-01",
        border_color: "black",
        frame_effects: [],
        full_art: false,
        faces: [{
          normal: "https://example.invalid/two-normal.jpg",
          art_crop: "https://example.invalid/two-crop.jpg",
        }],
      },
    ]);
    useCardImage.mockImplementation((_name, options) => ({
      src: `visual-pack://installed/${options.scryfallId}`,
      isLoading: false,
      rungs: {
        small: `visual-pack://small/${options.scryfallId}`,
        normal: `visual-pack://installed/${options.scryfallId}`,
      },
      advanceFailedSource: options.scryfallId?.startsWith("3") ? firstAdvance : secondAdvance,
    }));

    render(
      <PrintingPickerModal
        cardName="Opt"
        oracleId="55555555-5555-4555-8555-555555555555"
        onClose={vi.fn()}
      />,
    );

    await waitFor(() => expect(useCardImage).toHaveBeenCalledTimes(2));
    expect(useCardImage).toHaveBeenCalledWith("Opt", {
      oracleId: "55555555-5555-4555-8555-555555555555",
      scryfallId: "33333333-3333-4333-8333-333333333333",
      size: "normal",
    });
    expect(useCardImage).toHaveBeenCalledWith("Opt", {
      oracleId: "55555555-5555-4555-8555-555555555555",
      scryfallId: "44444444-4444-4444-8444-444444444444",
      size: "normal",
    });

    const first = screen.getByRole("img", { name: "Opt — Set One #1" });
    expect(first).toHaveAttribute(
      "srcset",
      "visual-pack://small/33333333-3333-4333-8333-333333333333 146w, visual-pack://installed/33333333-3333-4333-8333-333333333333 488w",
    );
    fireEvent.error(first);
    expect(firstAdvance).toHaveBeenCalledWith(
      "visual-pack://installed/33333333-3333-4333-8333-333333333333",
    );
    expect(secondAdvance).not.toHaveBeenCalled();
  });
});
