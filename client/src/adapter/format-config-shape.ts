/**
 * Structural validation for an untyped `FormatConfig` blob.
 *
 * TypeScript's `FormatConfig` is erased at runtime, and both places a config
 * enters this client from outside — `localStorage` rehydration and a broker's
 * `JoinTargetInfo`/`PeerInfo` frame — produce it with `JSON.parse`, which
 * validates nothing. A stale or hostile blob would otherwise be handed straight
 * to code that reads `.min_players`, `.deck_size.type`, etc.
 *
 * What this proves and what it does NOT:
 *
 * - It proves the blob still matches TODAY'S serialization schema — every field
 *   present, every field the right runtime shape.
 * - It does NOT prove the engine still considers the rules legal. It cannot:
 *   that answer lives behind `FormatConfig`'s Rust `Deserialize`, which
 *   re-derives a Custom config from its own `custom_rules` with
 *   `FormatConfig::for_custom_rules` and demands exact equality. Any config
 *   that reaches a real engine boundary is checked there regardless.
 *
 * A shape check is sufficient at these two call sites because a saved
 * `CustomFormatRules` is immutable once saved in this phase — there is no edit
 * flow that could make a previously-legal rule set illegal behind the client's
 * back.
 */

import type {
  CommandZoneMode,
  CustomFormatRules,
  DeckCopyLimit,
  DeckSizeRule,
  FormatConfig,
  GameFormat,
  RangeOfInfluenceConfig,
  SideboardPolicy,
} from "./types";
import { isCustomGameFormat } from "./types";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value);
}

function isOptionalInteger(value: unknown): value is number | null {
  return value === null || value === undefined || isInteger(value);
}

/**
 * For a Rust `Option<T>` field with NO `#[serde(skip_serializing_if =
 * "Option::is_none")]` (unlike `archenemy_player`, which has one): the engine
 * always serializes the key, as `null` for `None` or an integer for `Some`.
 * An absent key is therefore not a value this field can legitimately take —
 * it means the blob was never a real serialized `FormatConfig`/`CommandZoneMode`
 * at all, not that the value is `None`. Unlike {@link isOptionalInteger},
 * `undefined` (a missing key) is rejected, not treated as an accepted absence.
 */
function isRequiredNullableInteger(value: unknown): value is number | null {
  return value === null || isInteger(value);
}

/** CR 100.5 / CR 903.5a: the variant is authoritative — never infer it. */
function isDeckSizeRule(value: unknown): value is DeckSizeRule {
  if (!isRecord(value)) return false;
  return (value.type === "Minimum" || value.type === "Exactly") && isInteger(value.data);
}

/** CR 100.4 / CR 100.4a. `Forbidden`/`Unlimited` are unit variants and carry
 *  no `data` field under serde's tag/content representation. */
function isSideboardPolicy(value: unknown): value is SideboardPolicy {
  if (!isRecord(value)) return false;
  if (value.type === "Forbidden" || value.type === "Unlimited") return true;
  return value.type === "Limited" && isInteger(value.data);
}

/** CR 100.2a / CR 100.2b / CR 903.5b. */
function isDeckCopyLimit(value: unknown): value is DeckCopyLimit {
  if (!isRecord(value)) return false;
  if (value.type === "Unlimited") return true;
  return value.type === "UpTo" && isInteger(value.data);
}

function isRangeOfInfluenceConfig(value: unknown): value is RangeOfInfluenceConfig {
  if (!isRecord(value)) return false;
  if (!isInteger(value.default_range)) return false;
  if (!isRecord(value.player_overrides)) return false;
  return Object.values(value.player_overrides).every(isInteger);
}

/** CR 408.1 / CR 903.10a. Externally tagged: `"Disabled"` or
 *  `{ Enabled: { ... } }`. */
function isCommandZoneMode(value: unknown): value is CommandZoneMode {
  if (value === "Disabled") return true;
  if (!isRecord(value) || !isRecord(value.Enabled)) return false;
  const enabled = value.Enabled;
  return (
    isRequiredNullableInteger(enabled.commander_damage_threshold)
    && (enabled.eligibility_rule === "Standard"
      || enabled.eligibility_rule === "TinyLeaders"
      || enabled.eligibility_rule === "OathbreakerSignatureSpell"
      || enabled.eligibility_rule === "BrawlColorIdentity")
  );
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === "string");
}

function isLegacyRuleSet(value: unknown): boolean {
  if (!isRecord(value)) return false;
  return (
    (value.mana_burn === "Modern" || value.mana_burn === "Obsolete")
    && (value.damage_timing === "Modern" || value.damage_timing === "OnStack")
    && (value.wish_scope === "PostM10SideboardOnly" || value.wish_scope === "PreM10ReachesExile")
    && (value.legend_rule_scope === "Modern" || value.legend_rule_scope === "PreM14AnyController")
  );
}

/**
 * Structural check for a `CustomFormatRules` blob. Exported so the saved-format
 * store can validate a persisted definition with the same predicate the config
 * guard uses.
 */
export function isCustomFormatRulesShape(value: unknown): value is CustomFormatRules {
  if (!isRecord(value)) return false;
  if (!isInteger(value.id)) return false;

  const structural = value.structural;
  if (!isRecord(structural)) return false;
  if (
    !isInteger(structural.starting_life)
    || !isInteger(structural.min_players)
    || !isInteger(structural.max_players)
    || !isDeckSizeRule(structural.deck_size)
    || typeof structural.singleton !== "boolean"
    || !isCommandZoneMode(structural.command_zone_mode)
    || typeof structural.team_based !== "boolean"
    || !isSideboardPolicy(structural.sideboard_policy)
    || !isDeckCopyLimit(structural.default_deck_copy_limit)
  ) {
    return false;
  }
  // `#[serde(default)]` on the engine side, so absent is legal; present must
  // still be the right shape.
  const roi = structural.range_of_influence;
  if (roi !== null && roi !== undefined && !isRangeOfInfluenceConfig(roi)) return false;

  const legality = value.legality;
  if (!isRecord(legality)) return false;
  const legalSets = legality.legal_sets;
  // `Option<Vec<SetCode>>`: `null` means unrestricted, which is NOT the same
  // claim as "restricted to nothing" — both are legal, a missing key is not.
  if (legalSets !== null && !isStringArray(legalSets)) return false;
  return (
    isStringArray(legality.banned)
    && isStringArray(legality.restricted)
    && isLegacyRuleSet(legality.legacy)
  );
}

/**
 * Structural check for a `FormatConfig` blob, exhaustive against the engine
 * struct's real current field list (`crates/engine/src/types/format.rs`).
 *
 * The three easy-to-miss ones are checked explicitly because they are
 * serde-optional and therefore invisible in most payloads:
 * `range_of_influence`, `archenemy_player`, `supplies_fixed_deck`.
 */
export function isFormatConfigShape(value: unknown): value is FormatConfig {
  if (!isRecord(value)) return false;
  if (typeof value.format !== "string") return false;

  if (
    !isInteger(value.starting_life)
    || !isInteger(value.min_players)
    || !isInteger(value.max_players)
    || !isDeckSizeRule(value.deck_size)
    || typeof value.singleton !== "boolean"
    || typeof value.command_zone !== "boolean"
    || !isRequiredNullableInteger(value.commander_damage_threshold)
    || typeof value.team_based !== "boolean"
    || typeof value.uses_commander !== "boolean"
    || !isSideboardPolicy(value.sideboard_policy)
    || !isDeckCopyLimit(value.default_deck_copy_limit)
    || typeof value.allow_debug_actions !== "boolean"
  ) {
    return false;
  }

  // Serde-optional trio. Absent is legal; present must be well-formed.
  const roi = value.range_of_influence;
  if (roi !== null && roi !== undefined && !isRangeOfInfluenceConfig(roi)) return false;
  if (!isOptionalInteger(value.archenemy_player)) return false;
  if (
    value.supplies_fixed_deck !== undefined
    && typeof value.supplies_fixed_deck !== "boolean"
  ) {
    return false;
  }

  // The engine's `validate_custom_rules_consistency` invariant, mirrored:
  // `format == Custom(id) <=> custom_rules == Some(rules with that id)`. Both
  // directions, because a built-in config carrying rules is just as malformed
  // as a Custom one missing them.
  const format = value.format as GameFormat;
  const rules = value.custom_rules;
  if (isCustomGameFormat(format)) {
    if (!isCustomFormatRulesShape(rules)) return false;
    const declaredId = Number(format.slice("Custom:".length));
    return Number.isInteger(declaredId) && rules.id === declaredId;
  }
  return rules === null || rules === undefined;
}
