import { describe, expect, it } from "vitest";

import type {
  GameObject,
  ObjectAttribution,
  TransientContinuousEffect,
} from "../../adapter/types";
import { buildGameObject, buildObjectMap } from "../../test/factories/gameObjectFactory";
import {
  buildGrantedKeywordSources,
  buildPTSources,
  formatPTDelta,
} from "../attribution";

function makeObject(overrides: Partial<GameObject>): GameObject {
  return buildGameObject({
    id: 1,
    card_id: 0,
    name: "Object",
    card_types: { supertypes: [], core_types: [], subtypes: [] },
    ...overrides,
  });
}

describe("buildPTSources", () => {
  it("returns empty array when there is no ModifyPT bucket", () => {
    const attribution: ObjectAttribution = { by_layer: {} };
    const deref = { objects: {}, transientContinuousEffects: [] };
    expect(buildPTSources(attribution, 1, deref)).toEqual([]);
  });

  it("aggregates AddPower and AddToughness from a single static source", () => {
    // Lord (id=10) has a single StaticDefinition with two modifications:
    // AddPower{+1} and AddToughness{+1}. The target (id=1) gets both.
    const lord = makeObject({
      id: 10,
      name: "Lord",
      static_definitions: [
        {
          modifications: [
            { type: "AddPower", value: 1 },
            { type: "AddToughness", value: 1 },
          ],
        },
      ],
    });
    const attribution: ObjectAttribution = {
      by_layer: {
        ModifyPT: [
          { type: "Static", data: { source: 10, def_index: 0, mod_index: 0 } },
          { type: "Static", data: { source: 10, def_index: 0, mod_index: 1 } },
        ],
      },
    };
    const deref = {
      objects: buildObjectMap(lord),
      transientContinuousEffects: [],
    };
    const sources = buildPTSources(attribution, 1, deref);
    expect(sources).toHaveLength(1);
    expect(sources[0]).toEqual({
      sourceName: "Lord",
      deltaPower: 1,
      deltaToughness: 1,
    });
  });

  it("keeps multiple sources separate (anthem stacking)", () => {
    const anthemA = makeObject({
      id: 20,
      name: "Anthem A",
      static_definitions: [
        {
          modifications: [
            { type: "AddPower", value: 1 },
            { type: "AddToughness", value: 1 },
          ],
        },
      ],
    });
    const anthemB = makeObject({
      id: 21,
      name: "Anthem B",
      static_definitions: [
        {
          modifications: [{ type: "AddPower", value: 2 }],
        },
      ],
    });
    const attribution: ObjectAttribution = {
      by_layer: {
        ModifyPT: [
          { type: "Static", data: { source: 20, def_index: 0, mod_index: 0 } },
          { type: "Static", data: { source: 20, def_index: 0, mod_index: 1 } },
          { type: "Static", data: { source: 21, def_index: 0, mod_index: 0 } },
        ],
      },
    };
    const deref = {
      objects: buildObjectMap(anthemA, anthemB),
      transientContinuousEffects: [],
    };
    const sources = buildPTSources(attribution, 1, deref);
    expect(sources).toHaveLength(2);
    expect(sources).toContainEqual({
      sourceName: "Anthem A",
      deltaPower: 1,
      deltaToughness: 1,
    });
    expect(sources).toContainEqual({
      sourceName: "Anthem B",
      deltaPower: 2,
      deltaToughness: 0,
    });
  });

  it("resolves transient effects (Giant Growth pattern)", () => {
    const tce: TransientContinuousEffect = {
      id: 7,
      source_id: 99,
      controller: 0,
      timestamp: 1,
      source_name: "Giant Growth",
      modifications: [
        { type: "AddPower", value: 3 },
        { type: "AddToughness", value: 3 },
      ],
    };
    const attribution: ObjectAttribution = {
      by_layer: {
        ModifyPT: [
          { type: "Transient", data: { id: 7, mod_index: 0 } },
          { type: "Transient", data: { id: 7, mod_index: 1 } },
        ],
      },
    };
    const sources = buildPTSources(attribution, 1, {
      objects: {},
      transientContinuousEffects: [tce],
    });
    expect(sources).toEqual([
      { sourceName: "Giant Growth", deltaPower: 3, deltaToughness: 3 },
    ]);
  });

  it("filters out self-grants (a creature's own +N static)", () => {
    const self = makeObject({
      id: 1,
      name: "Self",
      static_definitions: [
        {
          modifications: [{ type: "AddPower", value: 1 }],
        },
      ],
    });
    const attribution: ObjectAttribution = {
      by_layer: {
        ModifyPT: [
          { type: "Static", data: { source: 1, def_index: 0, mod_index: 0 } },
        ],
      },
    };
    expect(
      buildPTSources(attribution, 1, {
        objects: { "1": self },
        transientContinuousEffects: [],
      }),
    ).toEqual([]);
  });

  it("ignores non-PT modifications in the ModifyPT bucket (defensive)", () => {
    const source = makeObject({
      id: 30,
      name: "Mystery",
      static_definitions: [
        {
          modifications: [{ type: "AddDynamicPower", quantity: "stuff" }],
        },
      ],
    });
    const attribution: ObjectAttribution = {
      by_layer: {
        ModifyPT: [
          { type: "Static", data: { source: 30, def_index: 0, mod_index: 0 } },
        ],
      },
    };
    expect(
      buildPTSources(attribution, 1, {
        objects: { "30": source },
        transientContinuousEffects: [],
      }),
    ).toEqual([]);
  });
});

describe("formatPTDelta", () => {
  it("signs each component independently", () => {
    expect(
      formatPTDelta({ sourceName: "X", deltaPower: 1, deltaToughness: 1 }),
    ).toBe("+1/+1");
    expect(
      formatPTDelta({ sourceName: "X", deltaPower: -1, deltaToughness: 2 }),
    ).toBe("-1/+2");
    expect(
      formatPTDelta({ sourceName: "X", deltaPower: 0, deltaToughness: -3 }),
    ).toBe("+0/-3");
  });
});

describe("buildGrantedKeywordSources still works with refactored helper", () => {
  it("returns keyword → source map and excludes self-grants", () => {
    const lord = makeObject({
      id: 10,
      name: "Lord",
      static_definitions: [
        {
          modifications: [{ type: "AddKeyword", keyword: "Flying" }],
        },
      ],
    });
    const attribution: ObjectAttribution = {
      by_layer: {
        Ability: [
          { type: "Static", data: { source: 10, def_index: 0, mod_index: 0 } },
        ],
      },
    };
    const map = buildGrantedKeywordSources(attribution, 1, {
      objects: buildObjectMap(lord),
      transientContinuousEffects: [],
    });
    expect(map.get("Flying")).toBe("Lord");
  });
});
