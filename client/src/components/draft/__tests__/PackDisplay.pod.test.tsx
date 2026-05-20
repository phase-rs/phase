import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { DraftPlayerView } from "../../../adapter/draft-adapter";

vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector({
      view: null,
      selectedCard: null,
      selectCard: vi.fn(),
      confirmPick: vi.fn(),
      autoPickCard: vi.fn(),
    }),
}));

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

import { PackDisplay } from "../PackDisplay";

const view: DraftPlayerView = {
  status: "Drafting",
  kind: "Premier",
  current_pack_number: 0,
  pick_number: 0,
  pass_direction: "Left",
  current_pack: [
    {
      instance_id: "card-1",
      name: "Lightning Bolt",
      set_code: "tst",
      collector_number: "1",
      rarity: "common",
      colors: ["R"],
      cmc: 1,
      type_line: "Instant",
    },
  ],
  pool: [],
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

describe("PackDisplay pod state", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders an explicit pod pack and dispatches pod pick actions", () => {
    const onSelectCard = vi.fn();
    const onConfirmPick = vi.fn();
    const { rerender } = render(
      <PackDisplay
        view={view}
        selectedCard={null}
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Lightning Bolt" }));

    expect(onSelectCard).toHaveBeenCalledWith("card-1");

    rerender(
      <PackDisplay
        view={view}
        selectedCard="card-1"
        onSelectCard={onSelectCard}
        onConfirmPick={onConfirmPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Confirm Pick" }));

    expect(onConfirmPick).toHaveBeenCalledTimes(1);
  });

  it("renders pod auto-pick and dispatches the pod auto-pick action", () => {
    const onAutoPick = vi.fn();

    render(
      <PackDisplay
        view={view}
        selectedCard={null}
        onSelectCard={vi.fn()}
        onConfirmPick={vi.fn()}
        showAutoPick
        onAutoPick={onAutoPick}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Auto-pick" }));

    expect(onAutoPick).toHaveBeenCalledTimes(1);
  });
});
