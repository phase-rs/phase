import { describe, expect, it } from "vitest";

import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildTabletopCardPresentation,
  countAttributedModifiers,
} from "../tabletopCardPresentation.ts";

describe("buildTabletopCardPresentation", () => {
  it("projects current engine characteristics into the shared renderer model", () => {
    const object = gameObjectFactory
      .creature(5, 4)
      .legendary()
      .inHand()
      .named("Live Test Card")
      .withCost(["Green", "Green"], 2)
      .withId(17)
      .build();

    const presentation = buildTabletopCardPresentation(
      object,
      { type: "Cost", shards: ["Green"], generic: 1 },
      undefined,
      "Current rules surface",
    );

    expect(presentation).toMatchObject({
      objectId: 17,
      name: "Live Test Card",
      manaSymbols: ["1", "G"],
      manaCostReduced: false,
      power: 5,
      toughness: 4,
      powerColor: "white",
      toughnessColor: "white",
      rulesText: "Current rules surface",
    });
    expect(presentation.typeLine).toContain("Legendary Creature");
  });

  it("projects increased and decreased live stats into separate numeral colors", () => {
    const object = gameObjectFactory
      .creature(3, 4)
      .params({ power: 5, toughness: 2 })
      .build();

    expect(buildTabletopCardPresentation(object)).toMatchObject({
      powerColor: "blue",
      toughnessColor: "red",
    });
  });

  it("keeps a visible zero pip for an engine-reduced free cast", () => {
    const object = gameObjectFactory.instant().withCost(["Green"], 1).build();

    expect(
      buildTabletopCardPresentation(
        object,
        { type: "NoCost" },
        undefined,
        null,
        true,
      ),
    ).toMatchObject({
      manaSymbols: ["0"],
      manaCostReduced: true,
    });
  });

  it("uses the public identity for face-down objects", () => {
    const object = gameObjectFactory
      .creature()
      .named("Hidden Identity")
      .faceDown()
      .build();

    expect(buildTabletopCardPresentation(object).name).toBe("Face-down card");
  });
});

describe("countAttributedModifiers", () => {
  it("counts the engine-authored effect references across layers", () => {
    expect(
      countAttributedModifiers({
        by_layer: {
          Ability: [
            {
              type: "Static",
              data: { source: 3, def_index: 0, mod_index: 1 },
            },
          ],
          ModifyPT: [
            {
              type: "Transient",
              data: { id: 9, mod_index: 0 },
            },
            {
              type: "Transient",
              data: { id: 9, mod_index: 1 },
            },
          ],
        },
      }),
    ).toBe(3);
  });
});
