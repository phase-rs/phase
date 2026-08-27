import { describe, expect, it } from "vitest";
import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import {
  createDraftWorkspaceState,
  reconcileWorkspaceState,
  updateWorkspacePlacement,
} from "../workspace/workspacePlacement";
import type { DraftWorkspaceState } from "../workspace/types";

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
});