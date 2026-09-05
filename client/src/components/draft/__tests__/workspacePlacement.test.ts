import { describe, expect, it } from "vitest";
import type { DraftCardInstance, DraftPoolGroups } from "../../../adapter/draft-adapter";
import {
  appendWorkspaceInstanceToResolvedDestination,
  createDraftWorkspaceState,
  moveWorkspaceInstance,
  reconcileWorkspaceState,
  updateWorkspacePlacement,
} from "../workspace/workspacePlacement";
import type { DraftWorkspaceState, DraftZone } from "../workspace/types";
import type { DraftBoardPreferences } from "../workspace/workspacePreferences";

function card(instanceId: string, name = "Shared Name"): DraftCardInstance {
  return {
    instance_id: instanceId,
    name,
    set_code: "TST",
    collector_number: instanceId,
    rarity: "common",
    colors: [],
    cmc: 1,
    type_line: "Card",
  };
}

function groups(): DraftPoolGroups {
  return {
    color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
    type_filter_options: [], color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: null },
    workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
  };
}

function boardPreferences(): Record<DraftZone, DraftBoardPreferences> {
  return {
    deck: { sort: "cmc", columnCount: 6, rows: "two", showHeaders: true },
    sideboard: { sort: "cmc", columnCount: 6, rows: "two", showHeaders: true },
  };
}

describe("workspace placement", () => {
  it("reconciles_authoritative_instances_without_losing_manual_placement", () => {
    const manual = { zone: "sideboard", row: 1, column: 4, order: 7 } as const;
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        first: manual,
        removed: { zone: "deck", row: 0, column: 0, order: 40 },
      },
      virtualBasics: [{ instanceId: "virtual", name: "Island" }],
    };

    const reconciled = reconcileWorkspaceState(state, [card("second"), card("first")]);

    expect(reconciled.placements.first).toBe(manual);
    expect(reconciled.placements).not.toHaveProperty("removed");
    expect(reconciled.placements.second).toEqual({ zone: "deck", row: 0, column: 0, order: 0 });
    expect(reconciled.placements.virtual).toEqual({ zone: "deck", row: 0, column: 0, order: 1 });
    expect(reconciled.virtualBasics).toEqual([{ instanceId: "virtual", name: "Island" }]);
  });

  it("keeps distinct same-name identities through pool reordering", () => {
    const initial = reconcileWorkspaceState(createDraftWorkspaceState(), [card("one"), card("two")]);
    const moved = updateWorkspacePlacement(
      initial,
      [card("one"), card("two")],
      "two",
      { zone: "sideboard", row: 0, column: 0, order: 0 },
    );

    const reconciled = reconcileWorkspaceState(moved, [card("two"), card("one")]);
    expect(reconciled).toBe(moved);
    expect(reconciled.placements.one.zone).toBe("deck");
    expect(reconciled.placements.two.zone).toBe("sideboard");
  });

  it("uses only Deck row-zero column-zero orders for missing instances", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        deck: { zone: "deck", row: 0, column: 0, order: 6 },
        otherRow: { zone: "deck", row: 1, column: 0, order: 99 },
        sideboard: { zone: "sideboard", row: 0, column: 0, order: 200 },
      },
    };

    const reconciled = reconcileWorkspaceState(state, [
      card("deck"), card("otherRow"), card("sideboard"), card("missing"),
    ]);
    expect(reconciled.placements.missing.order).toBe(7);
  });

  it("drops virtual identities that collide with the authoritative pool", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: { collision: { zone: "sideboard", row: 0, column: 0, order: 0 } },
      virtualBasics: [
        { instanceId: "collision", name: "Island" },
        { instanceId: "virtual", name: "Plains" },
        { instanceId: "virtual", name: "Duplicate" },
      ],
    };

    const reconciled = reconcileWorkspaceState(state, [card("collision")]);
    expect(reconciled.virtualBasics).toEqual([{ instanceId: "virtual", name: "Plains" }]);
    expect(reconciled.placements.collision.zone).toBe("sideboard");
    expect(reconciled.placements.virtual.zone).toBe("deck");
  });

  it("rejects unknown identities and invalid coordinates", () => {
    const state = reconcileWorkspaceState(createDraftWorkspaceState(), [card("known")]);
    const placement = { zone: "sideboard", row: 0, column: 0, order: 0 } as const;

    expect(updateWorkspacePlacement(state, [card("known")], "unknown", placement)).toBe(state);
    expect(updateWorkspacePlacement(
      state,
      [card("known")],
      "known",
      { ...placement, row: -1 },
    )).toBe(state);
  });

  it("appends_a_reconciled_pick_to_the_exact_destination_without_touching_unrelated_stacks", () => {
    const pool = [card("source"), card("source-sibling"), card("first"), card("second"), card("unrelated")];
    const state = reconcileWorkspaceState({
      ...createDraftWorkspaceState(),
      placements: {
        source: { zone: "sideboard", column: 2, row: 1, order: 4 },
        "source-sibling": { zone: "sideboard", column: 2, row: 1, order: 9 },
        first: { zone: "deck", column: 4, row: 0, order: 3 },
        second: { zone: "deck", column: 4, row: 0, order: 8 },
        unrelated: { zone: "deck", column: 1, row: 0, order: 17 },
      },
    }, pool);

    const moved = appendWorkspaceInstanceToResolvedDestination(state, pool, "source", {
      zone: "deck", column: 4, row: 0,
    });

    expect(moved.placements["source-sibling"]).toEqual({ zone: "sideboard", column: 2, row: 1, order: 0 });
    expect(moved.placements.first).toEqual({ zone: "deck", column: 4, row: 0, order: 0 });
    expect(moved.placements.second).toEqual({ zone: "deck", column: 4, row: 0, order: 1 });
    expect(moved.placements.source).toEqual({ zone: "deck", column: 4, row: 0, order: 2 });
    expect(moved.placements.unrelated).toBe(state.placements.unrelated);
  });

  it("keeps_anchored_keyboard_reorder_and_rejects_invalid_anchors", () => {
    const pool = [card("source"), card("first"), card("second")];
    const state = reconcileWorkspaceState({
      ...createDraftWorkspaceState(),
      placements: {
        source: { zone: "deck", column: 3, row: 0, order: 1 },
        first: { zone: "deck", column: 3, row: 0, order: 0 },
        second: { zone: "deck", column: 3, row: 0, order: 2 },
      },
    }, pool);

    const moved = moveWorkspaceInstance(state, pool, groups(), boardPreferences(), "source", {
      zone: "deck", column: 3, row: 0, beforeInstanceId: "first",
    });
    expect(moved.placements.source.order).toBe(0);
    expect(moved.placements.first.order).toBe(1);
    expect(moved.placements.second.order).toBe(2);

    expect(moveWorkspaceInstance(state, pool, groups(), boardPreferences(), "source", {
      zone: "deck", column: 3, row: 0, beforeInstanceId: "missing",
    })).toBe(state);
    expect(moveWorkspaceInstance(state, pool, groups(), boardPreferences(), "source", {
      zone: "deck", column: 3, row: 0, beforeInstanceId: "source",
    })).toBe(state);
  });
});
