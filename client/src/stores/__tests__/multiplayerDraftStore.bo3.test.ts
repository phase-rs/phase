import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DraftPlayerView } from "../../adapter/draft-adapter";
import { countProjectedNames, projectWorkspacePartition } from "../../components/draft/workspace/workspaceProjection";
import { useMultiplayerDraftStore } from "../multiplayerDraftStore";

const mocks = vi.hoisted(() => ({
  eventHandler: null as ((event: unknown) => void) | null,
  updateWorkspace: vi.fn(async () => {}),
  submitDeck: vi.fn(),
  submitAuthorized: vi.fn(),
  saveCommands: vi.fn(async () => {}),
  loadCommands: vi.fn(async () => []),
}));

vi.mock("../../adapter/draftPodHostAdapter", () => ({
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    return {
      onEvent: vi.fn((handler: (event: unknown) => void) => { mocks.eventHandler = handler; return vi.fn(); }),
      initialize: vi.fn(async () => {}),
      submitDeck: mocks.submitDeck,
      updateWorkspace: mocks.updateWorkspace,
      submitAuthorized: mocks.submitAuthorized,
      dispose: vi.fn(async () => {}),
      roomCode: null,
    };
  }),
}));

vi.mock("../../adapter/draftPodGuestAdapter", () => ({
  DraftPodGuestAdapter: vi.fn(),
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../services/draftPersistence")>(),
  loadDraftIntergameCommands: mocks.loadCommands,
  saveDraftIntergameCommands: mocks.saveCommands,
}));

function card(instanceId: string): DraftPlayerView["pool"][number] {
  return {
    instance_id: instanceId, name: "Twin", set_code: "TST", collector_number: instanceId,
    rarity: "common", colors: [], cmc: 1, type_line: "Card",
  };
}

function view(): DraftPlayerView {
  return {
    status: "Deckbuilding", kind: "Traditional", pool: [card("twin-a"), card("twin-b")],
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    current_pack: null, draft_effects: [],
    pool_groups: {
      color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
      type_filter_options: [], color_filter_options: [],
      color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
      workspace_capabilities: { rarity_group_order: null },
      workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
    },
    seats: [], current_pack_number: 3, pick_number: 15, pass_direction: "Left",
    cards_per_pack: 14, required_pick_count: 0, pick_selection_mode: "Direct", pick_steps_per_pack: 14, pack_count: 3, min_deck_size: 1, addable_cards: ["Island", "Forest"],
    timer_remaining_ms: null, standings: [], current_round: 1, next_pairing_round: 2, tournament_format: "Swiss",
    pod_policy: "Competitive", pairings: [], match_config: { match_type: "Bo3" },
  };
}

const hostConfig = {
  poolInput: { type: "Set" as const, data: { set_pool_json: "{}" } },
  kind: "Traditional" as const,
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss" as const,
  podPolicy: "Competitive" as const,
};

/**
 * A launch as the P2P host builds it: the registered sideboard is the pool
 * minus the main deck (`sideboardFromPool`), so it never carries a virtual
 * basic. `matchAuthoritySeat: 1` makes `retryDraftSettlement` return at its
 * first line.
 */
function hostLaunch(matchId: string, round: number, mainDeck: string[]) {
  return {
    type: "HumanHost" as const,
    matchId,
    matchRoomCode: `ROOM-${matchId}`,
    round,
    localSeat: 0,
    opponentSeat: 1,
    opponentName: "Guest",
    matchHostPeerId: "peer-0",
    deckPayload: {
      player: { main_deck: mainDeck, sideboard: ["Twin"], commander: [] },
      opponent: { main_deck: [], sideboard: [], commander: [] },
      ai_decks: [],
    },
    matchConfig: { match_type: "Bo3" as const },
    binding: {
      podId: "pod-1", matchId, round, sessionKey: "session-1",
      lease: "lease-1", nonce: "nonce-1", revision: 0, matchAuthoritySeat: 1,
    },
  };
}

function promptGame(matchId: string, gameNumber: number) {
  useMultiplayerDraftStore.getState().handleBetweenGamesPrompt({
    matchId, gameNumber,
    score: { p0_wins: 1, p1_wins: 0, draws: 0 }, loserSeat: 1, timerMs: 0,
  });
}

describe("multiplayerDraftStore Bo3", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.eventHandler = null;
    mocks.updateWorkspace.mockResolvedValue(undefined);
    mocks.saveCommands.mockResolvedValue(undefined);
    mocks.loadCommands.mockResolvedValue([]);
    useMultiplayerDraftStore.getState().reset();
  });

  afterEach(async () => {
    await useMultiplayerDraftStore.getState().leave();
  });

  describe("BO3-01: match result reporting", () => {
    it.todo("reports match result only when MatchPhase is Completed");
    it.todo("does not report match result after game 1 in Bo3");
  });

  describe("BO3-04: both-submitted gate", () => {
    it.todo("play/draw prompt fires only after both players submit sideboards");
    it.todo(
      "does not send play/draw prompt when only one player has submitted",
    );

    it("seeds_the_submitted_partition_and_uses_only_the_durable_intergame_command", async () => {
      const draftView = view();
      mocks.submitDeck.mockResolvedValue(draftView);
      await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
      mocks.eventHandler!({ type: "workspaceRestored", workspaceState: null });
      mocks.eventHandler!({ type: "viewUpdated", view: draftView });
      useMultiplayerDraftStore.getState().setWorkspacePlacement("twin-b", {
        zone: "sideboard", row: 0, column: 0, order: 0,
      });
      useMultiplayerDraftStore.getState().addBasicLand("Island");
      useMultiplayerDraftStore.getState().addBasicLand("Forest");
      const forest = useMultiplayerDraftStore.getState().workspaceState!.virtualBasics.find(
        (basic) => basic.name === "Forest",
      )!;
      useMultiplayerDraftStore.getState().setWorkspacePlacement(forest.instanceId, {
        zone: "sideboard", row: 0, column: 0, order: 1,
      });
      await useMultiplayerDraftStore.getState().submitDeck();

      expect(mocks.submitDeck).toHaveBeenCalledWith(["Twin", "Island"], []);
      expect(useMultiplayerDraftStore.getState().submittedPartition).toEqual({
        mainDeck: ["Twin", "Island"], sideboard: ["Twin", "Forest"],
      });
      useMultiplayerDraftStore.setState({
        seatIndex: 0,
        matchPairing: {
          type: "HumanHost",
          matchId: "match-1",
          matchRoomCode: "ROOM1",
          round: 1,
          localSeat: 0,
          opponentSeat: 1,
          opponentName: "Guest",
          matchHostPeerId: "peer-0",
          deckPayload: {
            player: { main_deck: ["Twin", "Island"], sideboard: ["Twin", "Forest"], commander: [] },
            opponent: { main_deck: [], sideboard: [], commander: [] },
            ai_decks: [],
          },
          matchConfig: { match_type: "Bo3" },
          binding: {
            podId: "pod-1", matchId: "match-1", round: 1, sessionKey: "session-1",
            lease: "lease-1", nonce: "nonce-1", revision: 0, matchAuthoritySeat: 0,
          },
        },
      });
      useMultiplayerDraftStore.getState().handleBetweenGamesPrompt({
        matchId: "match-1", gameNumber: 2,
        score: { p0_wins: 1, p1_wins: 0, draws: 0 }, loserSeat: 1, timerMs: 60_000,
      });

      const seeded = useMultiplayerDraftStore.getState().intergameWorkspaceState!;
      expect(seeded.placements["twin-b"].zone).toBe("sideboard");
      mocks.updateWorkspace.mockClear();
      const moved = {
        ...seeded,
        placements: {
          ...seeded.placements,
          "twin-a": { ...seeded.placements["twin-a"], zone: "sideboard" as const },
          "twin-b": { ...seeded.placements["twin-b"], zone: "deck" as const },
        },
      };
      useMultiplayerDraftStore.getState().setIntergameWorkspaceState(moved);
      const partition = projectWorkspacePartition(moved, draftView.pool);
      useMultiplayerDraftStore.getState().submitSideboard(
        "match-1", partition.mainDeck, countProjectedNames(partition.sideboard),
      );

      await vi.waitFor(() => expect(mocks.submitAuthorized).toHaveBeenCalledTimes(1));
      expect(mocks.saveCommands.mock.invocationCallOrder[0])
        .toBeLessThan(mocks.submitAuthorized.mock.invocationCallOrder[0]);
      expect(mocks.updateWorkspace).not.toHaveBeenCalled();
      expect(mocks.submitAuthorized.mock.calls[0][1].payload).toEqual({
        type: "SubmitSideboard",
        main: [{ name: "Twin", count: 1 }, { name: "Island", count: 1 }],
        sideboard: [{ name: "Twin", count: 1 }, { name: "Forest", count: 1 }],
      });

      useMultiplayerDraftStore.getState().submitSideboard(
        "match-1", ["Forged", "Island"], [{ name: "Twin", count: 1 }, { name: "Forest", count: 1 }],
      );
      await Promise.resolve();
      expect(mocks.submitAuthorized).toHaveBeenCalledTimes(1);
      expect(useMultiplayerDraftStore.getState().error).toMatch(/registered match pool/);
    });
  });

  /**
   * V11. MTR 7.3: the deck a player brings to a new match is the one they last
   * chose between matches, so game 2 of a later match is seeded from that
   * match's registered deck (the latest submission), never from the previous
   * match's sideboard configuration. A re-delivered start for the SAME match
   * (reconnect) keeps the in-match configuration for the next game.
   *
   * REVERT-PROBE: restore the matchStart arms' plain
   * `set({ matchPairing, phase })` and match-2's game-2 seed is match-1's
   * configuration: the multiset check fails, the seed is null and `error`
   * names the registered match pool.
   */
  describe("per-match between-games seed", () => {
    it("seeds_a_new_match_from_the_latest_submission_and_keeps_a_reconnected_match_configuration", async () => {
      const draftView = view();
      mocks.submitDeck.mockResolvedValue(draftView);
      await useMultiplayerDraftStore.getState().hostDraft(hostConfig);
      mocks.eventHandler!({ type: "workspaceRestored", workspaceState: null });
      mocks.eventHandler!({ type: "viewUpdated", view: draftView });
      const store = () => useMultiplayerDraftStore.getState();
      store().setWorkspacePlacement("twin-b", { zone: "sideboard", row: 0, column: 0, order: 0 });
      store().addBasicLand("Island");
      await store().submitDeck();
      expect(mocks.submitDeck).toHaveBeenLastCalledWith(["Twin", "Island"], []);

      mocks.eventHandler!({ type: "matchStart", launch: hostLaunch("match-1", 1, ["Twin", "Island"]) });
      promptGame("match-1", 2);
      const matchOneSeed = store().intergameWorkspaceState!;
      store().setIntergameWorkspaceState({
        ...matchOneSeed,
        placements: {
          ...matchOneSeed.placements,
          "twin-a": { ...matchOneSeed.placements["twin-a"], zone: "sideboard" },
          "twin-b": { ...matchOneSeed.placements["twin-b"], zone: "deck" },
        },
      });
      expect(store().intergameWorkspaceState!.placements["twin-a"].zone).toBe("sideboard");

      // Between rounds: swap the main-deck Island for a Forest (deck B).
      store().removeBasicLand("Island");
      store().addBasicLand("Forest");
      await store().submitDeck();
      expect(mocks.submitDeck).toHaveBeenLastCalledWith(["Twin", "Forest"], []);
      const revised = store().submittedPartition;
      expect(revised).toEqual({ mainDeck: ["Twin", "Forest"], sideboard: ["Twin"] });

      mocks.eventHandler!({ type: "matchStart", launch: hostLaunch("match-2", 2, ["Twin", "Forest"]) });
      promptGame("match-2", 2);

      const matchTwoSeed = store().intergameWorkspaceState;
      expect(store().error).toBeNull();
      expect(matchTwoSeed).not.toBeNull();
      expect(projectWorkspacePartition(matchTwoSeed!, draftView.pool)).toEqual(revised);

      // Same-match sibling: a reconnect re-delivers match-2's start, and game 3
      // keeps game 2's edited placement.
      store().setIntergameWorkspaceState({
        ...matchTwoSeed!,
        placements: {
          ...matchTwoSeed!.placements,
          "twin-a": { ...matchTwoSeed!.placements["twin-a"], zone: "sideboard" },
        },
      });
      mocks.eventHandler!({ type: "matchStart", launch: hostLaunch("match-2", 2, ["Twin", "Forest"]) });
      promptGame("match-2", 3);

      expect(store().error).toBeNull();
      expect(store().intergameWorkspaceState!.placements["twin-a"].zone).toBe("sideboard");
    });
  });

  describe("BO3-05: play/draw auto-choose", () => {
    it.todo("auto-chooses play on 10s timer expiry");
  });
});
