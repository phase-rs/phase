import { describe, expect, it } from "vitest";

import type { AnimationStep } from "../../../animation/types.ts";
import { buildCommanderGameObject, buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import {
  COMMANDER_CUT_IN_DURATION_MS,
  commanderCutInsForStep,
} from "../commanderCutInModel.ts";

function step(...events: AnimationStep["effects"][number]["event"][]): AnimationStep {
  return {
    effects: events.map((event) => ({ event, duration: 400 })),
    duration: 400,
  };
}

describe("commanderCutInsForStep", () => {
  it("holds the cinematic long enough to read the commander name", () => {
    expect(COMMANDER_CUT_IN_DURATION_MS).toBe(3_200);
  });

  it("selects a commander spell cast from engine-authored object metadata", () => {
    const commander = buildCommanderGameObject({
      id: 12,
      name: "Aesi, Tyrant of Gyre Strait",
      color: ["Green", "Blue"],
      printed_ref: { oracle_id: "oracle-aesi", face_name: "Aesi, Tyrant of Gyre Strait" },
    });
    const previous = gameStateFactory.commander().withObjects(commander).build();
    const next = gameStateFactory.commander().withObjects({ ...commander, zone: "Stack" }).build();

    expect(
      commanderCutInsForStep(
        step({ type: "SpellCast", data: { card_id: commander.card_id, controller: 0, object_id: 12 } }),
        previous,
        next,
      ),
    ).toEqual([
      {
        objectId: 12,
        owner: 0,
        name: "Aesi, Tyrant of Gyre Strait",
        colors: ["Green", "Blue"],
        oracleId: "oracle-aesi",
        faceName: "Aesi, Tyrant of Gyre Strait",
      },
    ]);
  });

  it("covers commander ninjutsu moving directly from command to battlefield", () => {
    const commander = buildCommanderGameObject({ id: 21, name: "Yuriko, the Tiger's Shadow" });
    const previous = gameStateFactory.commander().withObjects(commander).build();
    const next = gameStateFactory.commander().withObjects({ ...commander, zone: "Battlefield" }).build();

    expect(
      commanderCutInsForStep(
        step({ type: "ZoneChanged", data: { object_id: 21, from: "Command", to: "Battlefield" } }),
        previous,
        next,
      ),
    ).toHaveLength(1);
  });

  it("does not replay when a normally cast commander resolves from the stack", () => {
    const commander = buildCommanderGameObject({ id: 31, zone: "Battlefield" });
    const state = gameStateFactory.commander().withObjects(commander).build();

    expect(
      commanderCutInsForStep(
        step({ type: "ZoneChanged", data: { object_id: 31, from: "Stack", to: "Battlefield" } }),
        state,
        state,
      ),
    ).toEqual([]);
  });

  it("ignores ordinary spells and permanents", () => {
    const spell = buildGameObject({ id: 44, name: "Sol Ring", zone: "Stack", is_commander: false });
    const state = gameStateFactory.withObjects(spell).build();

    expect(
      commanderCutInsForStep(
        step({ type: "SpellCast", data: { card_id: spell.card_id, controller: 0, object_id: 44 } }),
        state,
        state,
      ),
    ).toEqual([]);
  });

  it("deduplicates multiple qualifying records for the same commander in one step", () => {
    const commander = buildCommanderGameObject({ id: 55, zone: "Battlefield" });
    const state = gameStateFactory.commander().withObjects(commander).build();

    expect(
      commanderCutInsForStep(
        step(
          { type: "SpellCast", data: { card_id: commander.card_id, controller: 0, object_id: 55 } },
          { type: "ZoneChanged", data: { object_id: 55, from: "Command", to: "Battlefield" } },
        ),
        state,
        state,
      ),
    ).toHaveLength(1);
  });
});
