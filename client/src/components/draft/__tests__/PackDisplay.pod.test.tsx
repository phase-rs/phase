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
      pickCardWithDraftEffect: vi.fn(),
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
  draft_effects: [],
  pool_groups: {
    color_groups: [],
    type_groups: [],
    cmc_groups: [],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
  },
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
  match_config: { match_type: "Bo1" },
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

  it("shows draft effects only when the engine provides drafted effect cards", () => {
    const effectView: DraftPlayerView = {
      ...view,
      current_pack: [
        view.current_pack![0],
        { ...view.current_pack![0], instance_id: "card-2", name: "Island" },
      ],
      draft_effects: [
        {
          instance_id: "cogwork-1",
          name: "Cogwork Librarian",
          set_code: "cns",
          collector_number: "58",
          rarity: "common",
          colors: [],
          cmc: 4,
          type_line: "Artifact Creature — Construct",
          draft_effect: "additional_pick",
        },
      ],
    };

    const { rerender } = render(
      <PackDisplay
        view={effectView}
        enableDraftEffects
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.getByText("Draft effects:")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Cogwork Librarian" })).toBeInTheDocument();

    rerender(
      <PackDisplay
        view={{ ...effectView, draft_effects: [] }}
        enableDraftEffects
        onCardHover={vi.fn()}
      />,
    );

    expect(screen.queryByText("Draft effects:")).not.toBeInTheDocument();
  });

  it("dispatches a pod draft-effect pick through its injected callback", () => {
    const onPickWithDraftEffect = vi.fn();
    const effectView: DraftPlayerView = {
      ...view,
      current_pack: [
        view.current_pack![0],
        { ...view.current_pack![0], instance_id: "card-2", name: "Island" },
      ],
      draft_effects: [
        {
          instance_id: "cogwork-1",
          name: "Cogwork Librarian",
          set_code: "cns",
          collector_number: "58",
          rarity: "common",
          colors: [],
          cmc: 4,
          type_line: "Artifact Creature — Construct",
          draft_effect: "additional_pick",
        },
      ],
    };
    const { rerender } = render(
      <PackDisplay
        view={effectView}
        selectedCard={null}
        enableDraftEffects
        onPickWithDraftEffect={onPickWithDraftEffect}
        onCardHover={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("checkbox", { name: "Cogwork Librarian" }));
    rerender(
      <PackDisplay
        view={effectView}
        selectedCard="card-1"
        enableDraftEffects
        onPickWithDraftEffect={onPickWithDraftEffect}
        onCardHover={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Island" }));
    fireEvent.click(screen.getAllByRole("button", { name: "Confirm Pick" })[0]);

    expect(onPickWithDraftEffect).toHaveBeenCalledWith("cogwork-1", ["card-1", "card-2"]);
  });
});
