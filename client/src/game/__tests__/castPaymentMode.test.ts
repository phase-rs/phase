import { beforeEach, describe, expect, it } from "vitest";

import type { GameAction } from "../../adapter/types";
import { applySpellPaymentPreference } from "../castPaymentMode";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";

const castAction: GameAction = {
  type: "CastSpell",
  data: { object_id: 1 },
} as GameAction;

const activateAction: GameAction = {
  type: "ActivateAbility",
  data: { source_id: 7, ability_index: 1 },
};

type ActivateAbilityAction = Extract<GameAction, { type: "ActivateAbility" }>;

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

  it("stamps every cast-family action with the sacrificial-mana preference", () => {
    usePreferencesStore.setState({ spellPaymentMode: "autoExceptSacrificialMana" });
    const castTypes = [
      "CastSpell",
      "CastSpellForFree",
      "CastSpellAsMiracle",
      "CastSpellAsMadness",
      "CastSpellAsSneak",
      "CastSpellAsWebSlinging",
    ] as const;

    for (const type of castTypes) {
      const action = { type, data: {} } as GameAction;
      const result = applySpellPaymentPreference(action) as GameAction & {
        data: { payment_mode?: unknown };
      };
      expect(result.data.payment_mode).toEqual({ type: "AutoExceptSacrificialMana" });
    }
  });

  it("lets the per-game manual override dominate the sacrificial-mana preference", () => {
    usePreferencesStore.setState({ spellPaymentMode: "autoExceptSacrificialMana" });
    useUiStore.setState({ manualManaOverride: true });

    const result = applySpellPaymentPreference(castAction);

    expect((result as Extract<GameAction, { type: "CastSpell" }>).data.payment_mode).toEqual({
      type: "Manual",
    });
  });
});

describe("applySpellPaymentPreference for ability activations", () => {
  beforeEach(() => {
    usePreferencesStore.setState({ spellPaymentMode: "auto" });
    useUiStore.setState({ manualManaOverride: false });
  });

  it("returns the original activation when the saved preference is auto", () => {
    expect(applySpellPaymentPreference(activateAction)).toBe(activateAction);
  });

  it("stamps Manual onto an activation under a saved manual preference, keeping its payload", () => {
    usePreferencesStore.setState({ spellPaymentMode: "manual" });
    const result = applySpellPaymentPreference(activateAction) as ActivateAbilityAction;
    expect(result).not.toBe(activateAction);
    expect(result).toEqual({
      type: "ActivateAbility",
      data: { source_id: 7, ability_index: 1, payment_mode: { type: "Manual" } },
    });
    // The engine-offered action is not mutated in place.
    expect((activateAction as ActivateAbilityAction).data.payment_mode).toBeUndefined();
  });

  it("stamps Manual onto an activation when only the per-game override is on", () => {
    useUiStore.setState({ manualManaOverride: true });
    const result = applySpellPaymentPreference(activateAction) as ActivateAbilityAction;
    expect(result.data).toEqual({
      source_id: 7,
      ability_index: 1,
      payment_mode: { type: "Manual" },
    });
  });

  it("lets the per-game override dominate the sacrificial-mana preference for an activation", () => {
    usePreferencesStore.setState({ spellPaymentMode: "autoExceptSacrificialMana" });
    useUiStore.setState({ manualManaOverride: true });
    const result = applySpellPaymentPreference(activateAction) as ActivateAbilityAction;
    expect(result.data.payment_mode).toEqual({ type: "Manual" });
  });

  it("forwards the sacrificial-mana preference to an activation unchanged", () => {
    usePreferencesStore.setState({ spellPaymentMode: "autoExceptSacrificialMana" });
    const result = applySpellPaymentPreference(activateAction) as ActivateAbilityAction;
    expect(result.data.payment_mode).toEqual({ type: "AutoExceptSacrificialMana" });
  });

  it("keeps an engine-offered Manual activation Manual under every preference", () => {
    const manualOffer: GameAction = {
      type: "ActivateAbility",
      data: { source_id: 7, ability_index: 1, payment_mode: { type: "Manual" } },
    };
    for (const spellPaymentMode of ["auto", "autoExceptSacrificialMana", "manual"] as const) {
      usePreferencesStore.setState({ spellPaymentMode });
      const result = applySpellPaymentPreference(manualOffer) as ActivateAbilityAction;
      expect(result.data.payment_mode).toEqual({ type: "Manual" });
    }
  });

  it("still leaves non-payment actions untouched under a manual preference", () => {
    usePreferencesStore.setState({ spellPaymentMode: "manual" });
    const pass: GameAction = { type: "PassPriority" };
    expect(applySpellPaymentPreference(pass)).toBe(pass);
  });
});
