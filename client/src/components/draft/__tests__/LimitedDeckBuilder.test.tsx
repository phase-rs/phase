import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

import { LimitedDeckBuilder } from "../LimitedDeckBuilder";

afterEach(cleanup);

vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      view: null,
      mainDeck: [],
      landCounts: {},
      addToDeck: () => {},
      removeFromDeck: () => {},
      setLandCount: () => {},
      autoSuggestDeck: async () => {},
      autoSuggestLands: async () => {},
      submitDeck: async () => {},
    }),
}));

vi.mock("../../card/HoverCardPreview", () => ({
  HoverCardPreview: ({ card }: { card: { name: string } | null }) => (
    <div data-testid="hover-preview">{card?.name}</div>
  ),
}));

type BuilderView = NonNullable<NonNullable<Parameters<typeof LimitedDeckBuilder>[0]>["view"]>;

const TEST_VIEW: BuilderView = {
  status: "Deckbuilding",
  kind: "Quick",
  current_pack_number: 1,
  pick_number: 1,
  pass_direction: "Left",
  current_pack: null,
  pool: [
    {
      instance_id: "card-1",
      name: "Wind Drake",
      set_code: "dmu",
      collector_number: "58",
      rarity: "common",
      colors: ["U"],
      cmc: 3,
      type_line: "Creature - Drake",
    },
  ],
  seats: [],
  cards_per_pack: 14,
  pack_count: 3,
  min_deck_size: 40,
  addable_cards: ["Plains", "Island", "Academy Ruins"],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
  tournament_format: "Swiss",
  pod_policy: "Competitive",
  pairings: [],
  match_config: { match_type: "Bo1" },
};

function Harness() {
  const [mainDeck, setMainDeck] = useState<string[]>([]);

  return (
    <LimitedDeckBuilder
      view={TEST_VIEW}
      mainDeck={mainDeck}
      landCounts={{}}
      onAddToDeck={(cardName) => setMainDeck((prev) => [...prev, cardName])}
      onRemoveFromDeck={(cardName) =>
        setMainDeck((prev) => {
          const idx = prev.indexOf(cardName);
          if (idx < 0) return prev;
          const next = prev.slice();
          next.splice(idx, 1);
          return next;
        })
      }
      onSetLandCount={() => {}}
      onSubmitDeck={() => {}}
      showSuggestions={false}
    />
  );
}

describe("LimitedDeckBuilder", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("updates mana curve when a card is added from pool", () => {
    render(<Harness />);

    const threeDropBucket = screen.getByRole("meter", { name: "Mana value 3" });
    expect(threeDropBucket).toHaveAttribute("aria-valuenow", "0");

    fireEvent.click(screen.getByRole("button", { name: /wind drake/i }));

    expect(threeDropBucket).toHaveAttribute("aria-valuenow", "1");
  });

  it("filters custom addable cards by name", () => {
    render(<Harness />);

    fireEvent.change(screen.getByPlaceholderText("Search addable cards..."), {
      target: { value: "academy" },
    });

    expect(screen.getByText("Academy Ruins")).toBeInTheDocument();
    expect(screen.queryByText("Plains")).not.toBeInTheDocument();
    expect(screen.queryByText("Island")).not.toBeInTheDocument();
  });

  it("does not substitute basic lands when the engine exposes no addable cards", () => {
    render(
      <LimitedDeckBuilder
        view={{ ...TEST_VIEW, addable_cards: [] }}
        mainDeck={[]}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    expect(screen.queryByRole("button", { name: "Add Plains" })).not.toBeInTheDocument();
  });

  it("opens a preview on touch long press without moving the card", () => {
    vi.useFakeTimers();
    render(<Harness />);

    const card = screen.getByRole("button", { name: /wind drake/i });
    fireEvent.pointerDown(card, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(500));
    fireEvent.click(card, { detail: 0 });

    expect(screen.getByTestId("hover-preview")).toHaveTextContent("Wind Drake");
    expect(screen.getByRole("meter", { name: "Mana value 3" })).toHaveAttribute(
      "aria-valuenow",
      "0",
    );
  });

  it("does not suppress activation after a canceled long press", () => {
    vi.useFakeTimers();
    render(<Harness />);

    const card = screen.getByRole("button", { name: /wind drake/i });
    fireEvent.pointerDown(card, {
      button: 0,
      clientX: 10,
      clientY: 10,
      isPrimary: true,
      pointerId: 1,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(500));
    fireEvent.pointerCancel(card, { pointerId: 1, pointerType: "touch" });
    fireEvent.click(card, { detail: 0 });

    expect(screen.getByRole("meter", { name: "Mana value 3" })).toHaveAttribute(
      "aria-valuenow",
      "1",
    );
  });

  it("shows the engine validation reason when deck submission fails", async () => {
    render(
      <LimitedDeckBuilder
        view={TEST_VIEW}
        mainDeck={Array.from({ length: 40 }, () => "Wind Drake")}
        landCounts={{}}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={async () => {
          throw new Error("card 'Watery Grave' is not in the drafted pool");
        }}
        showSuggestions={false}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Submit Deck" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Deck needs attention: card 'Watery Grave' is not in the drafted pool",
    );
  });
});
