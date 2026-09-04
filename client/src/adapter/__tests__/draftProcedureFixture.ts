import type { DraftProcedure } from "../draft-adapter";

export function draftProcedureFixture(overrides: Partial<DraftProcedure> = {}): DraftProcedure {
  return {
    pod_size: 8,
    human_seats: 1,
    min_pod_size: 2,
    max_pod_size: 8,
    allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
    packs_per_player: 3,
    cards_per_pick: 1,
    pick_selection_mode: "Direct",
    distribution: "PickAndPass",
    min_deck_size: 40,
    cube_min_deck_size: 40,
    commanders_required: 0,
    post_draft_play: "TournamentPairings",
    launch_capability: "None",
    match_config: { match_type: "Bo3" },
    ...overrides,
  };
}