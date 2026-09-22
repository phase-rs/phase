import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { SealedPackOpening } from "../SealedPackOpening";
import type { DraftPlayerView } from "../../../adapter/draft-adapter";

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

afterEach(cleanup);

const VIEW: DraftPlayerView = {
  status: "Deckbuilding",
  kind: "Sealed",
  launch_capability: "None",
  distribution: "PickAndPass",
  commanders_required: 0,
  current_pack_number: 0,
  pick_number: 0,
  pass_direction: "Left",
  current_pack: null,
  required_pick_count: 0,
  pick_selection_mode: "Direct",
  pool: [
    {
      instance_id: "creature",
      name: "Silvercoat Lion",
      set_code: "m19",
      collector_number: "31",
      rarity: "common",
      colors: ["W"],
      cmc: 2,
      type_line: "Creature — Cat",
    },
    {
      instance_id: "instant",
      name: "Shock",
      set_code: "m19",
      collector_number: "156",
      rarity: "common",
      colors: ["R"],
      cmc: 1,
      type_line: "Instant",
    },
    {
      instance_id: "sorcery",
      name: "Lightning Bolt",
      set_code: "m19",
      collector_number: "157",
      rarity: "common",
      colors: ["R"],
      cmc: 1,
      type_line: "Instant",
    },
  ],
  draft_effects: [],
  pool_groups: {
    color_groups: [],
    type_groups: [
      { kind: "creature", total: 1, cards: [{ card: {
        instance_id: "creature",
        name: "Silvercoat Lion",
        set_code: "m19",
        collector_number: "31",
        rarity: "common",
        colors: ["W"],
        cmc: 2,
        type_line: "Creature — Cat",
      }, count: 1, instance_ids: ["creature"] }] },
      { kind: "instant", total: 2, cards: [
        {
          card: {
            instance_id: "instant",
            name: "Shock",
            set_code: "m19",
            collector_number: "156",
            rarity: "common",
            colors: ["R"],
            cmc: 1,
            type_line: "Instant",
          },
          count: 1,
          instance_ids: ["instant"],
        },
        {
          card: {
            instance_id: "sorcery",
            name: "Lightning Bolt",
            set_code: "m19",
            collector_number: "157",
            rarity: "common",
            colors: ["R"],
            cmc: 1,
            type_line: "Instant",
          },
          count: 1,
          instance_ids: ["sorcery"],
        },
      ] },
    ],
    cmc_groups: [],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 1, blue: 0, black: 0, red: 2, green: 0 },
    workspace_capabilities: {
      rarity_group_order: ["mythic", "rare", "uncommon", "common", "rarity_other"],
    },
    workspace_row_classification: {
      creature_instance_ids: ["creature"],
      noncreature_instance_ids: ["instant", "sorcery"],
    },
  },
  sealed_packs: [],
  seats: [],
  cards_per_pack: 1,
  pack_sizes: [1, 1, 1],
  pack_set_codes: ["TST", "TST", "TST"],
  pack_pick_steps: [1, 1, 1],
  pick_steps_per_pack: 1,
  pack_count: 2,
  min_deck_size: 40,
  addable_cards: ["Plains"],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
  next_pairing_round: 1,
  tournament_format: "Swiss",
  pod_policy: "Competitive",
  pairings: [],
  match_config: { match_type: "Bo1" },
};

describe("SealedPackOpening", () => {
  it("reveals each engine-provided pack before showing the type-grouped pool", async () => {
    const onComplete = vi.fn();
    render(
      <SealedPackOpening
        view={{ ...VIEW, sealed_packs: [[VIEW.pool[0]], [VIEW.pool[1]]] }}
        onComplete={onComplete}
      />,
    );

    expect(screen.getByText("Open 2 packs, one at a time.")).toBeInTheDocument();
    expect(screen.getByText("Pack 1 of 2")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open pack" }));
    expect(await screen.findAllByText("Silvercoat Lion")).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "Next pack" }));
    expect(await screen.findByText("Pack 2 of 2")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Open pack" }));
    expect(await screen.findAllByText("Shock")).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "View your pool" }));
    expect(await screen.findByRole("heading", { name: "Your sealed pool" })).toBeInTheDocument();
    expect(screen.getByText("Creature (1)")).toBeInTheDocument();
    expect(screen.getByText("Instant (2)")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Build deck" }));
    expect(onComplete).toHaveBeenCalledOnce();
  });

  it("uses singular copy for one engine-provided pack", () => {
    render(
      <SealedPackOpening
        view={{ ...VIEW, pack_count: 1, sealed_packs: [[VIEW.pool[0]]] }}
        onComplete={vi.fn()}
      />,
    );

    expect(screen.getByText("Open 1 pack.")).toBeInTheDocument();
  });

  it("reveals every remaining pack at once via Open all packs and still reaches the pool review", async () => {
    const onComplete = vi.fn();
    render(
      <SealedPackOpening
        view={{
          ...VIEW,
          pack_count: 3,
          sealed_packs: [[VIEW.pool[0]], [VIEW.pool[1]], [VIEW.pool[2]]],
        }}
        onComplete={onComplete}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Open all packs" }));

    expect(await screen.findAllByText("Silvercoat Lion")).toHaveLength(2);
    expect(screen.getAllByText("Shock")).toHaveLength(2);
    expect(screen.getAllByText("Lightning Bolt")).toHaveLength(2);
    expect(screen.getByText("Pack 1 of 3")).toBeInTheDocument();
    expect(screen.getByText("Pack 2 of 3")).toBeInTheDocument();
    expect(screen.getByText("Pack 3 of 3")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "View your pool" }));
    expect(await screen.findByRole("heading", { name: "Your sealed pool" })).toBeInTheDocument();
    expect(screen.getByText("Creature (1)")).toBeInTheDocument();
    expect(screen.getByText("Instant (2)")).toBeInTheDocument();
    expect(screen.getAllByText("Shock")).toHaveLength(2);
    expect(screen.getAllByText("Lightning Bolt")).toHaveLength(2);

    fireEvent.click(screen.getByRole("button", { name: "Build deck" }));
    expect(onComplete).toHaveBeenCalledOnce();
  });

  it("hides Open all packs while the current pack is individually opened, and only opens the remaining packs when used afterward", async () => {
    render(
      <SealedPackOpening
        view={{
          ...VIEW,
          pack_count: 3,
          sealed_packs: [[VIEW.pool[0]], [VIEW.pool[1]], [VIEW.pool[2]]],
        }}
        onComplete={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Open all packs" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Open pack" }));
    expect(await screen.findAllByText("Silvercoat Lion")).toHaveLength(2);
    expect(screen.queryByRole("button", { name: "Open all packs" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Next pack" }));
    expect(await screen.findByText("Pack 2 of 3")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open all packs" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Open all packs" }));

    expect(await screen.findAllByText("Shock")).toHaveLength(2);
    expect(screen.getAllByText("Lightning Bolt")).toHaveLength(2);
    expect(screen.queryByText("Silvercoat Lion")).not.toBeInTheDocument();
    expect(screen.getByText("Pack 2 of 3")).toBeInTheDocument();
    expect(screen.getByText("Pack 3 of 3")).toBeInTheDocument();
    expect(screen.queryByText("Pack 1 of 3")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "View your pool" }));
    expect(await screen.findByRole("heading", { name: "Your sealed pool" })).toBeInTheDocument();
    expect(screen.getAllByText("Shock")).toHaveLength(2);
    expect(screen.getAllByText("Lightning Bolt")).toHaveLength(2);
  });
});
