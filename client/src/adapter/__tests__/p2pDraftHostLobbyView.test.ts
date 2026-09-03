import { describe, expect, it, vi } from "vitest";

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftKind, DraftProcedure } from "../draft-adapter";

/**
 * The pre-draft lobby view must report the ENGINE's per-kind axes rather than
 * re-deriving them in TypeScript (CLAUDE.md: the frontend is a display layer,
 * not a logic layer).
 *
 * This is a multi-authority row: the placeholder lobby view and the real
 * post-start session view are two producers of the same axes, and a Commander
 * pod is where they used to disagree — the lobby advertised the CR 100.2b
 * 40-card limited floor for a CR 903.13f(1) 60-card format.
 */
describe("P2PDraftHost pre-draft lobby view", () => {
  const COMMANDER_PROCEDURE: DraftProcedure = {
    pod_size: 4,
    human_seats: 1,
    min_pod_size: 3,
    max_pod_size: 8,
    allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
    packs_per_player: 3,
    cards_per_pick: 2,
    pick_selection_mode: "Ordered",
    distribution: "PickAndPass",
    min_deck_size: 60,
    commanders_required: 1,
    post_draft_play: "CompleteImmediately",
    launch_capability: "CommanderMultiplayer",
    match_config: { match_type: "Bo1" },
  };

  const TRADITIONAL_PROCEDURE: DraftProcedure = {
    pod_size: 8,
    human_seats: 8,
    min_pod_size: 2,
    max_pod_size: 8,
    allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
    packs_per_player: 3,
    cards_per_pick: 1,
    pick_selection_mode: "Direct",
    distribution: "PickAndPass",
    min_deck_size: 40,
    commanders_required: 0,
    post_draft_play: "TournamentPairings",
    launch_capability: "None",
    match_config: { match_type: "Bo3" },
  };

  async function lobbyViewFor(
    kind: Exclude<DraftKind, "Quick">,
    procedure: DraftProcedure,
    podSize: number,
  ) {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      kind,
      podSize,
      "Host",
      "Swiss",
      "Competitive",
    );
    const draftProcedure = vi.fn(async () => procedure);
    (host as unknown as { adapter: unknown }).adapter = { draftProcedure };

    await host.initialize();
    const view = await host.getHostView();
    return { view, draftProcedure };
  }

  /**
   * REVERT-PROBE: re-introduce `min_deck_size: 40` (or `pack_count: 3`) as a
   * literal in `buildLobbyView` and this reds.
   */
  it("reports a Commander pod's engine-owned axes, not the limited defaults", async () => {
    const { view, draftProcedure } = await lobbyViewFor(
      "CommanderDraft",
      COMMANDER_PROCEDURE,
      4,
    );

    expect(draftProcedure).toHaveBeenCalledWith("CommanderDraft", "Swiss");
    expect(view.min_deck_size).toBe(60); // CR 903.13f(1)
    expect(view.pack_count).toBe(3); // CR 903.13b
    // 0, not 2: the lobby seat has no pending pack, which is exactly what
    // `filter_for_player` publishes for one. A `procedure.cards_per_pick`
    // placeholder here would disagree with the real view.
    expect(view.required_pick_count).toBe(0);
    // VM row 10 — B1's pin. `0` in BOTH kinds, and that is the discriminator:
    // a TS derivation would publish 7 for this `cards_per_pick: 2` Commander
    // procedure and 14 for the `cards_per_pick: 1` Traditional one below.
    // IDENTICAL values across two different procedures are what prove no
    // derivation happened — a single-kind assertion could not tell a literal
    // `0` from a coincidence. The assertions above are the reach-guard: they
    // prove `buildLobbyView` ran and read the procedure at all, so this `0`
    // cannot be an artifact of a view that was never built.
    expect(view.pick_steps_per_pack).toBe(0);
    expect(view.launch_capability).toBe("CommanderMultiplayer");
    expect(view.match_config.match_type).toBe("Bo1");
    // Identity, not just equality: the view must PASS THROUGH the engine's
    // object. The old `this.kind === "Traditional" ? "Bo3" : "Bo1"` built a
    // fresh literal and would fail this even though its value matched.
    expect(view.match_config).toBe(COMMANDER_PROCEDURE.match_config);
  });

  /**
   * The paired control: a limited kind still reports 40 and Bo3, so the change
   * reads the procedure rather than hardcoding the Commander values.
   */
  it("still reports a Traditional pod's limited axes", async () => {
    const { view } = await lobbyViewFor("Traditional", TRADITIONAL_PROCEDURE, 8);

    expect(view.min_deck_size).toBe(40); // CR 100.2b
    expect(view.pack_count).toBe(3);
    // The other half of row 10's cross-fixture identity: `0` here too, under a
    // procedure whose `cards_per_pick` differs from the Commander one.
    expect(view.pick_steps_per_pack).toBe(0);
    expect(view.launch_capability).toBe("None");
    expect(view.match_config.match_type).toBe("Bo3");
    expect(view.match_config).toBe(TRADITIONAL_PROCEDURE.match_config);
  });

  it("forwards the WASM launch capability without recomputing it from procedure axes", async () => {
    const { view } = await lobbyViewFor(
      "CommanderDraft",
      { ...COMMANDER_PROCEDURE, launch_capability: "None" },
      4,
    );

    expect(view.launch_capability).toBe("None");
  });

  /**
   * The public `getHostView()` is the one `buildLobbyView` caller that
   * `initialize()`'s ordering does NOT guarantee, so the throw is the guard
   * that covers it. A silent 40-card default here is precisely the defect this
   * phase exists to prevent.
   */
  it("throws rather than defaulting when the view is built before initialize", async () => {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      "CommanderDraft",
      4,
      "Host",
      "Swiss",
      "Competitive",
    );

    await expect(host.getHostView()).rejects.toThrow("before initialize()");
  });
});
