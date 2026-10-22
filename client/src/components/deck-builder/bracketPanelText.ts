import type { TFunction } from "i18next";

import {
  BRACKET_TIER_NUMERIC,
  type BracketAxis,
  type BracketReason,
  type ComboOutcome,
  type ComboResource,
} from "../../types/bracket";

// Bracket policy is WotC Commander Format Panel guidance, not the Comprehensive
// Rules, so no CR annotations apply here. Pure display maps: every value shown
// comes from the engine's explanation; this module only picks i18n keys.

export const AXIS_I18N_KEY: Record<BracketAxis, string> = {
  game_changers: "bracket.axis.gameChangers",
  mass_land_denial: "bracket.axis.massLandDenial",
  extra_turns: "bracket.axis.extraTurns",
  efficient_tutors: "bracket.axis.efficientTutors",
};

export const COMBO_RESOURCE_I18N_KEY: Record<ComboResource, string> = {
  mana: "bracket.combo.resource.mana",
  damage: "bracket.combo.resource.damage",
  life_loss: "bracket.combo.resource.lifeLoss",
  lifegain: "bracket.combo.resource.lifegain",
  mill: "bracket.combo.resource.mill",
  draw: "bracket.combo.resource.draw",
  tokens: "bracket.combo.resource.tokens",
  combat: "bracket.combo.resource.combat",
  turns: "bracket.combo.resource.turns",
};

export function reasonTitle(t: TFunction, reason: BracketReason): string {
  const { source } = reason;
  switch (source.kind) {
    case "card_rule":
      return t("bracket.card.reason.cardRule", {
        axis: t(AXIS_I18N_KEY[source.axis]),
        count: source.observed,
      });
    case "combo_floor":
      return source.trigger.kind === "early_two_card"
        ? t("bracket.card.reason.comboEarly")
        : t("bracket.card.reason.comboStandalone");
    case "owner_combo_answer":
      return t("bracket.card.reason.ownerAnswer");
    default: {
      const unreachable: never = source;
      return unreachable;
    }
  }
}

export function reasonExplanation(t: TFunction, reason: BracketReason): string {
  const { source } = reason;
  switch (source.kind) {
    case "card_rule":
      return t(
        source.allowed_below === 0
          ? "bracket.card.explain.cardRuleNone"
          : "bracket.card.explain.cardRule",
        {
          tier: BRACKET_TIER_NUMERIC[source.tier_below],
          allowed: source.allowed_below,
          observed: source.observed,
        },
      );
    case "combo_floor":
      return source.trigger.kind === "early_two_card"
        ? t("bracket.card.explain.comboEarly", { ceiling: source.trigger.assemble_ceiling })
        : t("bracket.card.explain.comboStandalone");
    case "owner_combo_answer":
      return t("bracket.card.explain.ownerAnswer");
    default: {
      const unreachable: never = source;
      return unreachable;
    }
  }
}

function comboOutcomeText(t: TFunction, outcome: ComboOutcome): string {
  switch (outcome.kind) {
    case "wins":
      return t("bracket.combo.outcome.wins");
    case "unbounded":
      return t("bracket.combo.outcome.unbounded", {
        resource: t(COMBO_RESOURCE_I18N_KEY[outcome.resource]),
      });
    default: {
      const unreachable: never = outcome;
      return unreachable;
    }
  }
}

export function comboOutcomesText(t: TFunction, outcomes: ComboOutcome[]): string {
  return outcomes
    .map((outcome) => comboOutcomeText(t, outcome))
    .join(t("bracket.card.listComma"));
}

/**
 * A reason's React key, stable across deck edits so an expanded row stays on its
 * own reason: floor, source kind, then the axis, trigger kind or "owner". No index
 * suffix is needed. Within one list the engine emits at most one card rule per axis
 * and floor (`FLOOR_RULES` has one row per pair), one combo floor per trigger kind
 * and floor, and one owner answer.
 */
export function reasonKey(reason: BracketReason): string {
  const { floor, source } = reason;
  switch (source.kind) {
    case "card_rule":
      return `${floor}-${source.kind}-${source.axis}`;
    case "combo_floor":
      return `${floor}-${source.kind}-${source.trigger.kind}`;
    case "owner_combo_answer":
      return `${floor}-${source.kind}-owner`;
    default: {
      const unreachable: never = source;
      return unreachable;
    }
  }
}
