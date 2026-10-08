import { describe, expect, it } from "vitest";
import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import { createDraftWorkspaceState } from "../workspace/workspacePlacement";
import {
  addVirtualBasic,
  makeVirtualBasicInstanceId,
  projectDeckNames,
  removeVirtualBasic,
} from "../workspace/workspaceProjection";
import type { DraftWorkspaceState } from "../workspace/types";

function card(instanceId: string, name: string): DraftCardInstance {
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

describe("workspace projection", () => {
  it("projects_only_deck_instances_and_stable_virtual_basics", () => {
    const pool = [card("second", "Second"), card("first", "First")];
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        first: { zone: "deck", row: 8, column: 4, order: 0 },
        second: { zone: "sideboard", row: 0, column: 0, order: 0 },
        stale: { zone: "deck", row: 0, column: 0, order: 1 },
        "workspace-basic:token": { zone: "deck", row: 0, column: 0, order: 2 },
        "workspace-basic:side": { zone: "sideboard", row: 0, column: 0, order: 0 },
      },
      virtualBasics: [
        { instanceId: "workspace-basic:token", name: "Island" },
        { instanceId: "workspace-basic:side", name: "Plains" },
      ],
    };

    expect(makeVirtualBasicInstanceId("token")).toBe("workspace-basic:token");
    expect(projectDeckNames(state, pool)).toEqual(["First", "Island"]);
    expect(projectDeckNames(state, [...pool].reverse())).toEqual(["First", "Island"]);
  });

  it("adds virtual basics with stable identities and deterministic default order", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        deck: { zone: "deck", row: 0, column: 0, order: 4 },
        ignored: { zone: "sideboard", row: 0, column: 0, order: 99 },
      },
    };
    const basic = { instanceId: makeVirtualBasicInstanceId("uuid"), name: "Forest" };

    const added = addVirtualBasic(state, [card("deck", "Spell")], basic);
    expect(added.virtualBasics).toEqual([basic]);
    expect(added.placements[basic.instanceId]).toEqual({
      zone: "deck", row: 0, column: 0, order: 5,
    });
    expect(addVirtualBasic(added, [], basic)).toBe(added);
    expect(addVirtualBasic(state, [card(basic.instanceId, "Collision")], basic)).toBe(state);
  });

  it("removes a virtual basic and its placement together", () => {
    const basic = { instanceId: makeVirtualBasicInstanceId("remove"), name: "Swamp" };
    const added = addVirtualBasic(createDraftWorkspaceState(), [], basic);
    const removed = removeVirtualBasic(added, basic.instanceId);

    expect(removed.virtualBasics).toEqual([]);
    expect(removed.placements).not.toHaveProperty(basic.instanceId);
    expect(removeVirtualBasic(removed, basic.instanceId)).toBe(removed);
  });

  it("rejects empty and colliding virtual identities", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: { occupied: { zone: "deck", row: 0, column: 0, order: 0 } },
    };
    expect(addVirtualBasic(state, [], { instanceId: "", name: "Island" })).toBe(state);
    expect(addVirtualBasic(state, [], { instanceId: "new", name: "" })).toBe(state);
    expect(addVirtualBasic(state, [], { instanceId: "occupied", name: "Island" })).toBe(state);
  });
});