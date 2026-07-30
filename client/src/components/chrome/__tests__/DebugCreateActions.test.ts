import { describe, expect, it } from "vitest";

import type { TokenPreset } from "../../../services/engineRuntime";
import {
  buildCatalogTokenDebugAction,
  tokenPresetHasSourceDefinedPt,
} from "../DebugCreateActions";

function preset(ptProvenance: TokenPreset["pt_provenance"]): TokenPreset {
  return {
    id: "ooze-preset",
    category: "Creature",
    fidelity: "Full",
    pt_provenance: ptProvenance,
    body: {
      display_name: "Ooze",
      power: null,
      toughness: null,
      core_types: ["Creature"],
      subtypes: ["Ooze"],
      supertypes: [],
      colors: ["Green"],
      keywords: [],
    },
  };
}

describe("DebugCreateActions catalog token payloads", () => {
  it("detects source-defined P/T provenance", () => {
    expect(tokenPresetHasSourceDefinedPt(preset(undefined))).toBe(false);
    expect(
      tokenPresetHasSourceDefinedPt(
        preset({ SourceDefinedOrDynamic: { power: "X", toughness: "X" } }),
      ),
    ).toBe(true);
  });

  it("omits P/T overrides for fixed or absent catalog presets", () => {
    const action = buildCatalogTokenDebugAction({
      preset: preset(undefined),
      owner: 0,
      counterType: "P1P1",
      counterCount: 0,
      runEtb: true,
    });

    expect(action?.data.request.data).toEqual({
      preset_id: "ooze-preset",
      owner: 0,
      enter_with_counters: [],
    });
  });

  it("requires explicit P/T overrides for source-defined catalog presets", () => {
    const sourceDefined = preset({
      SourceDefinedOrDynamic: { power: "X", toughness: "X" },
    });

    expect(
      buildCatalogTokenDebugAction({
        preset: sourceDefined,
        owner: 0,
        counterType: "P1P1",
        counterCount: 0,
        runEtb: true,
        powerOverride: 4,
      }),
    ).toBeNull();

    expect(
      buildCatalogTokenDebugAction({
        preset: sourceDefined,
        owner: 0,
        counterType: "P1P1",
        counterCount: 0,
        runEtb: true,
        powerOverride: 4,
        toughnessOverride: 5,
      })?.data.request.data,
    ).toEqual({
      preset_id: "ooze-preset",
      owner: 0,
      power_override: 4,
      toughness_override: 5,
      enter_with_counters: [],
    });
  });
});
