import { describe, expect, it } from "vitest";

import type { GameObject, PlayerId } from "../../adapter/types";
import { buildGameObject, buildObjectMap } from "../../test/factories/gameObjectFactory";
import {
  buildCommanderFormatConfig,
  buildFormatConfig,
  buildGameState,
} from "../../test/factories/gameStateFactory";
import { commanderDamageEntriesFor, commandersInZone } from "../commanderColumn";

function obj(overrides: Partial<GameObject>): GameObject {
  return buildGameObject({
    id: 1,
    owner: 0,
    is_commander: false,
    zone: "Battlefield",
    ...overrides,
  });
}

function stateWith(objects: GameObject[], commandZone: number[]) {
  return buildGameState({
    command_zone: commandZone,
    objects: buildObjectMap(...objects),
  });
}

describe("commandersInZone", () => {
  it("returns only this player's commanders that are still in the command zone", () => {
    const mine = obj({ id: 1, owner: 0, is_commander: true, zone: "Command" });
    const cast = obj({ id: 2, owner: 0, is_commander: true, zone: "Battlefield" });
    const opponent = obj({ id: 3, owner: 1, is_commander: true, zone: "Command" });
    const nonCommander = obj({ id: 4, owner: 0, is_commander: false, zone: "Command" });
    const state = stateWith([mine, cast, opponent, nonCommander], [1, 2, 3, 4]);

    expect(commandersInZone(state, 0 as PlayerId).map((o) => o.id)).toEqual([1]);
  });

  it("is empty once the commander has left the command zone", () => {
    const cast = obj({ id: 1, owner: 0, is_commander: true, zone: "Battlefield" });
    expect(commandersInZone(stateWith([cast], [1]), 0 as PlayerId)).toEqual([]);
  });
});

describe("commanderDamageEntriesFor", () => {
  function damageState(
    byAttacker: Record<string, Array<{ victim: PlayerId; commander: number; damage: number }>>,
    formatThreshold?: number,
  ) {
    return buildGameState({
      format_config:
        formatThreshold == null
          ? buildFormatConfig()
          : buildCommanderFormatConfig({ commander_damage_threshold: formatThreshold }),
      derived: { commander_damage_by_attacker: byAttacker },
    });
  }

  it("surfaces live damage entries for the victim even when no format threshold is set", () => {
    // Regression: the PlayerArea wrapper once gated on the format flag while
    // CommanderDamage renders on damage alone (fallback threshold). The shared
    // selector must NOT require commander_damage_threshold, or the wrapper hides
    // a render path its child supports.
    const state = damageState({ "1": [{ victim: 0, commander: 99, damage: 7 }] });
    expect(commanderDamageEntriesFor(state, 0 as PlayerId)).toEqual([
      { attacker: "1", views: [{ victim: 0, commander: 99, damage: 7 }] },
    ]);
  });

  it("filters out zero-damage entries and other victims", () => {
    const state = damageState(
      {
        "1": [
          { victim: 0, commander: 10, damage: 0 },
          { victim: 1, commander: 11, damage: 5 },
        ],
      },
      21,
    );
    expect(commanderDamageEntriesFor(state, 0 as PlayerId)).toEqual([]);
  });

  it("is empty when there are no derived entries", () => {
    expect(commanderDamageEntriesFor(damageState({}), 0 as PlayerId)).toEqual([]);
  });
});
