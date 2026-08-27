import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { PoolPanel } from "../PoolPanel";
import type { DraftPlayerView } from "../../../adapter/draft-adapter";

// The panel reads its disclosure state from the store, so the collapsed
// sibling below is driven through the same authority production uses.
const storeState = {
  view: null as DraftPlayerView | null,
  poolSortMode: "color" as const,
  poolPanelOpen: true,
  setPoolSortMode: () => {},
  togglePoolPanel: () => {},
};
vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector(storeState as unknown as Record<string, unknown>),
}));

const TEST_VIEW = {
  status: "Deckbuilding",
  kind: "CommanderDraft",
  current_pack_number: 1,
  pick_number: 1,
  pass_direction: "Left",
  current_pack: null,
  required_pick_count: 0,
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
  draft_effects: [],
  pool_groups: {
    color_groups: [],
    type_groups: [],
    cmc_groups: [],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 0, blue: 1, black: 0, red: 0, green: 0 },
  },
  seats: [],
  cards_per_pack: 14,
  pack_count: 3,
  min_deck_size: 60,
  addable_cards: [],
  timer_remaining_ms: null,
  standings: [],
  current_round: 0,
  next_pairing_round: 1,
  tournament_format: "Swiss",
  pod_policy: "Competitive",
  pairings: [],
  match_config: { match_type: "Bo1" },
} as unknown as DraftPlayerView;

const GRANTED = "+ up to 2 × Faceless One (commander only)";

describe("PoolPanel — CR 903.13e granted filler", () => {
  afterEach(() => {
    cleanup();
    storeState.poolPanelOpen = true;
  });

  /**
   * V10 — the grant line renders the ENGINE's `card_name` and `max_copies`,
   * interpolated. A hard-coded line would survive a different grant; this
   * fixture names a card the pool does not contain.
   */
  it("renders the grant line with the engine's card name and cap", () => {
    render(
      <PoolPanel
        view={{
          ...TEST_VIEW,
          grantable_commander_filler: { card_name: "Faceless One", max_copies: 2 },
        }}
      />,
    );

    expect(screen.getByText(GRANTED)).toBeInTheDocument();
  });

  it("renders no grant line when the draft's set grants none", () => {
    render(<PoolPanel view={TEST_VIEW} />);

    // Reach-guard: the panel rendered at all.
    expect(screen.getByText("1 cards drafted")).toBeInTheDocument();
    expect(screen.queryByText(GRANTED)).toBeNull();
  });

  /**
   * The collapsed sibling. The grant line is a statement about pool CONTENTS,
   * so it belongs inside the disclosure body — the collapsed header is
   * deliberately a one-line summary. This row discriminates the placement:
   * an implementation that put the line outside the `poolPanelOpen` fragment
   * would render it here.
   */
  it("hides the grant line while the pool panel is collapsed", () => {
    storeState.poolPanelOpen = false;
    render(
      <PoolPanel
        view={{
          ...TEST_VIEW,
          grantable_commander_filler: { card_name: "Faceless One", max_copies: 2 },
        }}
      />,
    );

    // Reach-guard: the collapsed header still renders.
    expect(screen.getByText("1 cards drafted")).toBeInTheDocument();
    expect(screen.queryByText(GRANTED)).toBeNull();
  });
});
