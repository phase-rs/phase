import { useState } from "react";
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, within } from "@testing-library/react";

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

function bucketCount(curveRoot: HTMLElement, label: string): string {
  const labelNode = within(curveRoot).getByText(label);
  const bucket = labelNode.parentElement;
  if (!bucket) return "";
  const countNode = bucket.querySelector("span.h-4");
  return countNode?.textContent ?? "";
}

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

    const curveTitle = screen.getByText(/mana curve/i);
    const curveRoot = curveTitle.parentElement;
    expect(curveRoot).not.toBeNull();
    if (!curveRoot) return;

    expect(bucketCount(curveRoot, "3")).toBe("");

    fireEvent.click(screen.getByRole("button", { name: /wind drake/i }));

    expect(bucketCount(curveRoot, "3")).toBe("1");
  });
});