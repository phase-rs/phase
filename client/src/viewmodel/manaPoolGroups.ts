import type {
  ManaRestriction,
  ManaSpellGrant,
  ManaType,
  ManaUnit,
} from "../adapter/types.ts";

// Discriminant key of a `ManaRestriction` — the bare string for unit variants,
// or the single object key for data variants. Exhaustive so adding a new
// restriction variant forces an update to the label map below.
type ExternallyTaggedVariantName<T> = T extends string
  ? T
  : T extends Record<infer K, unknown>
    ? Extract<K, string>
    : never;

export type ManaRestrictionTag = ExternallyTaggedVariantName<ManaRestriction>;

// i18n key (under the `manaPool` group) per restriction variant. Exhaustive
// `Record` — adding a `ManaRestriction` variant without a tooltip key here is a
// type error.
export const RESTRICTION_LABEL_KEYS: Record<ManaRestrictionTag, string> = {
  OnlyForSpell: "manaPool.onlyForSpell",
  OnlyForSpellType: "manaPool.onlyForSpellType",
  OnlyForCreatureType: "manaPool.onlyForCreatureType",
  OnlyForTypeSpellsOrAbilities: "manaPool.onlyForTypeSpellsOrAbilities",
  OnlyForTaggedActivation: "manaPool.onlyForTaggedActivation",
  OnlyForSpellWithKeywordKind: "manaPool.onlyForSpellWithKeywordKind",
  OnlyForSpellWithKeywordKindFromZone: "manaPool.onlyForSpellWithKeywordKindFromZone",
  OnlyForSpellWithManaValue: "manaPool.onlyForSpellWithManaValue",
  OnlyForSpellMatchingCostCriteria: "manaPool.onlyForSpellMatchingCostCriteria",
  OnlyForSpellWithColorCount: "manaPool.onlyForSpellWithColorCount",
  OnlyForSpellFromZone: "manaPool.onlyForSpellFromZone",
  OnlyForFaceDownSpell: "manaPool.onlyForFaceDownSpell",
  OnlyForActivation: "manaPool.onlyForActivation",
  OnlyForXCosts: "manaPool.onlyForXCosts",
  OnlyForAny: "manaPool.onlyForAny",
  OnlyForSpecialAction: "manaPool.onlyForSpecialAction",
  ConvokePayment: "manaPool.convokePayment",
};

export function restrictionTag(restriction: ManaRestriction): ManaRestrictionTag {
  return typeof restriction === "string"
    ? restriction
    : (Object.keys(restriction)[0] as ManaRestrictionTag);
}

// Canonical, payload-inclusive string for a restriction — so "Legendary-only
// green" and "Creature-only green" hash to distinct group keys.
export function canonRestriction(restriction: ManaRestriction): string {
  return typeof restriction === "string"
    ? restriction
    : JSON.stringify(restriction);
}

export function canonGrant(grant: ManaSpellGrant): string {
  return typeof grant === "string" ? grant : JSON.stringify(grant);
}

const MANA_ORDER: ManaType[] = ["White", "Blue", "Black", "Red", "Green", "Colorless"];

/**
 * A run of fungible pool units — same color, spend restrictions, and spell
 * grants. Per CR 118.3a the engine treats identical units interchangeably, so
 * the UI groups them and tracks the concrete `pip_id`s (in pool order) so a
 * specific unit can be pinned for payment.
 */
export interface ManaPoolGroup {
  color: ManaType;
  restrictions: ManaRestriction[];
  grants: ManaSpellGrant[];
  /** Carries spend restrictions or spell grants — rendered distinctly. */
  special: boolean;
  /** Concrete pip ids in this group, in pool order — the pin targets. */
  pipIds: number[];
}

/**
 * Group a player's pool units by (color, restrictions, grants), excluding the
 * internal `ConvokePayment` markers (not spendable mana). Stable display order:
 * by color, with plain (unrestricted) groups before restricted/granting ones of
 * the same color.
 */
export function groupManaPoolUnits(units: ManaUnit[]): ManaPoolGroup[] {
  const groups = new Map<string, ManaPoolGroup>();
  for (const unit of units) {
    if (unit.restrictions.includes("ConvokePayment")) continue;
    const grants = unit.grants ?? [];
    const key = JSON.stringify([
      unit.color,
      [...unit.restrictions].map(canonRestriction).sort(),
      [...grants].map(canonGrant).sort(),
    ]);
    const existing = groups.get(key);
    if (existing) {
      existing.pipIds.push(unit.pip_id);
    } else {
      groups.set(key, {
        color: unit.color,
        restrictions: unit.restrictions,
        grants,
        special: unit.restrictions.length > 0 || grants.length > 0,
        pipIds: [unit.pip_id],
      });
    }
  }
  return [...groups.values()].sort((a, b) => {
    const colorDelta = MANA_ORDER.indexOf(a.color) - MANA_ORDER.indexOf(b.color);
    if (colorDelta !== 0) return colorDelta;
    return (a.special ? 1 : 0) - (b.special ? 1 : 0);
  });
}

/**
 * Human-readable tooltip for a group's restrictions and grants, or `undefined`
 * for plain mana. `CantBeCountered` is surfaced specifically (Cavern of Souls)
 * so the player knows which mana makes the spell uncounterable; other grants
 * fall back to a generic label.
 */
export function manaGroupTooltip(
  translate: (key: string) => string,
  group: Pick<ManaPoolGroup, "restrictions" | "grants" | "special">,
): string | undefined {
  if (!group.special) return undefined;
  const parts = group.restrictions.map((r) =>
    translate(RESTRICTION_LABEL_KEYS[restrictionTag(r)]),
  );
  for (const grant of group.grants) {
    parts.push(
      grant === "CantBeCountered"
        ? translate("manaPool.grantCantBeCountered")
        : translate("manaPool.grantsProperty"),
    );
  }
  return parts.join("; ");
}
