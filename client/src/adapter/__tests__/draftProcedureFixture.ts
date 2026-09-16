import { isSharedStackDistribution } from "../draft-adapter";
import type { DraftProcedure } from "../draft-adapter";

export function draftProcedureFixture(overrides: Partial<DraftProcedure> = {}): DraftProcedure {
  const distribution = overrides.distribution ?? "PickAndPass";
  return {
    pod_size: 8,
    human_seats: 1,
    min_pod_size: 2,
    max_pod_size: 8,
    allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
    packs_per_player: 3,
    cards_per_pick: 1,
    pick_selection_mode: "Direct",
    distribution,
    // DERIVED from the distribution, mirroring
    // `DraftProcedure::allowed_set_layouts`: a shared stack opens every booster
    // into one pile, so there is no per-seat, per-round slot a Chaos assignment
    // could fill. Derived rather than defaulted so a fixture that overrides the
    // distribution cannot silently keep the other shape's answer and hand the
    // client a procedure the engine would never publish. An explicit override
    // still wins, for tests that want exactly that skew.
    allowed_set_layouts: isSharedStackDistribution(distribution)
      ? ["UniformByRound"]
      : ["UniformByRound", "Chaos"],
    min_deck_size: 40,
    cube_min_deck_size: 40,
    commanders_required: 0,
    post_draft_play: "TournamentPairings",
    launch_capability: "None",
    match_config: { match_type: "Bo3" },
    ...overrides,
  };
}