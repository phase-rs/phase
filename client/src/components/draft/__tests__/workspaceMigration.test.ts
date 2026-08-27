import { describe, expect, it } from "vitest";
import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import {
  makeLegacyVirtualBasicInstanceId,
  migrateLegacyWorkspace,
} from "../workspace/workspaceMigration";

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

describe("workspace migration", () => {
  it("migrates_duplicate_names_and_drafted_basics_deterministically", () => {
    const pool = [
      card("bolt-1", "Bolt"),
      card("island-1", "Island"),
      card("bolt-2", "Bolt"),
      card("island-2", "Island"),
    ];
    const legacy = {
      mainDeck: ["Bolt", "Island", "Missing", "bolt"],
      landCounts: { Island: 2, Plains: 1 },
    };

    const first = migrateLegacyWorkspace(pool, legacy);
    const second = migrateLegacyWorkspace(pool, legacy);

    expect(second).toEqual(first);
    expect(first.placements["bolt-1"].zone).toBe("deck");
    expect(first.placements["bolt-2"].zone).toBe("sideboard");
    expect(first.placements["island-1"].zone).toBe("deck");
    expect(first.placements["island-2"].zone).toBe("sideboard");
    expect(first.virtualBasics.map((basic) => basic.name)).toEqual(["Island", "Island", "Plains"]);
    expect(first.virtualBasics.map((basic) => basic.instanceId)).toEqual([
      "workspace-basic:legacy:Island:0",
      "workspace-basic:legacy:Island:1",
      "workspace-basic:legacy:Plains:0",
    ]);
  });

  it("consumes duplicate requests only up to authoritative availability", () => {
    const pool = [card("one", "Bolt"), card("two", "Bolt")];

    expect(migrateLegacyWorkspace(pool, { mainDeck: [], landCounts: {} }).placements)
      .toMatchObject({ one: { zone: "sideboard" }, two: { zone: "sideboard" } });
    expect(migrateLegacyWorkspace(pool, { mainDeck: ["Bolt", "Bolt"], landCounts: {} }).placements)
      .toMatchObject({ one: { zone: "deck" }, two: { zone: "deck" } });
    expect(migrateLegacyWorkspace(pool, {
      mainDeck: ["Bolt", "Bolt", "Bolt"], landCounts: {},
    }).placements).toMatchObject({ one: { zone: "deck" }, two: { zone: "deck" } });
  });

  it("normalizes land counts without depending on object insertion order", () => {
    const first = migrateLegacyWorkspace([], {
      mainDeck: [],
      landCounts: { Swamp: Number.POSITIVE_INFINITY, Forest: 2.9, Island: 0, Mountain: -1, Plains: Number.NaN },
    });
    const second = migrateLegacyWorkspace([], {
      mainDeck: [],
      landCounts: { Plains: Number.NaN, Mountain: -1, Island: 0, Forest: 2.9, Swamp: Number.POSITIVE_INFINITY },
    });

    expect(second).toEqual(first);
    expect(first.virtualBasics).toEqual([
      { instanceId: "workspace-basic:legacy:Forest:0", name: "Forest" },
      { instanceId: "workspace-basic:legacy:Forest:1", name: "Forest" },
    ]);
  });

  it("rechecks every collision candidate without changing ordinal progression", () => {
    const base = "workspace-basic:legacy:Island:0";
    const pool = [
      card(base, "Collision A"),
      card(`${base}~1`, "Collision B"),
    ];
    const legacy = { mainDeck: [], landCounts: { Island: 2 } };

    const first = migrateLegacyWorkspace(pool, legacy);
    expect(migrateLegacyWorkspace(pool, legacy)).toEqual(first);
    expect(first.virtualBasics.map((basic) => basic.instanceId)).toEqual([
      `${base}~2`,
      "workspace-basic:legacy:Island:1",
    ]);
  });

  it("encodes exact names and checks the evolving used-id set", () => {
    const used = new Set(["workspace-basic:legacy:Snow%20Land:0"]);
    const generated = makeLegacyVirtualBasicInstanceId("Snow Land", 0, used);
    expect(generated).toBe("workspace-basic:legacy:Snow%20Land:0~1");
    used.add(generated);
    expect(makeLegacyVirtualBasicInstanceId("Snow Land", 0, used))
      .toBe("workspace-basic:legacy:Snow%20Land:0~2");
  });
});