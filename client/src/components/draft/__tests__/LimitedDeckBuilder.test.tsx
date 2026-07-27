import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { LimitedDeckBuilder } from "../LimitedDeckBuilder";

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

afterEach(() => {
  cleanup();
});

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
  addable_cards: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
  tournament_format: "Swiss",
  pod_policy: "Competitive",
  pairings: [],
};

const COPY_VIEW: BuilderView = {
  ...TEST_VIEW,
  pool: [
    ...TEST_VIEW.pool,
    {
      instance_id: "card-2",
      name: "Eager Cadet",
      set_code: "dmu",
      collector_number: "1",
      rarity: "common",
      colors: ["W"],
      cmc: 1,
      type_line: "Creature - Human Soldier",
    },
  ],
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
  it("updates mana curve when a card is added from pool", () => {
    render(<Harness />);

    const threeDropBucket = screen.getByRole("meter", { name: "Mana value 3" });
    expect(threeDropBucket).toHaveAttribute("aria-valuenow", "0");

    fireEvent.click(screen.getByRole("button", { name: /wind drake/i }));

    expect(threeDropBucket).toHaveAttribute("aria-valuenow", "1");
  });

  it("copies the current deck list to the clipboard", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });

    render(
      <LimitedDeckBuilder
        view={COPY_VIEW}
        mainDeck={["Wind Drake"]}
        landCounts={{ Island: 2, Plains: 0, Forest: 1 }}
        onAddToDeck={() => {}}
        onRemoveFromDeck={() => {}}
        onSetLandCount={() => {}}
        onSubmitDeck={() => {}}
        showSuggestions={false}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Copy Deck List" }));

    expect(writeText).toHaveBeenCalledWith(
      [
        "Deck",
        "1 Wind Drake",
        "2 Island",
        "1 Forest",
        "",
        "Sideboard",
        "1 Eager Cadet",
      ].join("\n"),
    );
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Copied!" })).toBeInTheDocument(),
    );
  });
});
