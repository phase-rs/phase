import { beforeEach, describe, expect, it } from "vitest";

import type { GameAction } from "../../adapter/types";
import { applySpellPaymentPreference } from "../castPaymentMode";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";

const castAction: GameAction = {
  type: "CastSpell",
  data: { object_id: 1 },
} as GameAction;

describe("applySpellPaymentPreference", () => {
  beforeEach(() => {
    usePreferencesStore.setState({ spellPaymentMode: "auto" });
    useUiStore.setState({ manualManaOverride: false });
  });

  it("leaves the action unchanged when both the preference and override are off", () => {
    const result = applySpellPaymentPreference(castAction);
    expect(result).toBe(castAction);
  });

  it("stamps Manual when the persisted preference is manual", () => {
    usePreferencesStore.setState({ spellPaymentMode: "manual" });
    const result = applySpellPaymentPreference(castAction);
    expect(result).not.toBe(castAction);
    expect((result as Extract<GameAction, { type: "CastSpell" }>).data.payment_mode).toEqual({
      type: "Manual",
    });
  });

  it("stamps Manual when only the per-game override is on", () => {
    useUiStore.setState({ manualManaOverride: true });
    const result = applySpellPaymentPreference(castAction);
    expect(result).not.toBe(castAction);
    expect((result as Extract<GameAction, { type: "CastSpell" }>).data.payment_mode).toEqual({
      type: "Manual",
    });
  });
});
