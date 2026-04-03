import type { Keyword } from "../adapter/types";
import { SHARD_ABBREVIATION } from "./costLabel";

/** Combat-relevant keywords displayed first, in this order. */
const KEYWORD_DISPLAY_ORDER: string[] = [
  "Flying", "First Strike", "Double Strike", "Deathtouch", "Trample",
  "Lifelink", "Vigilance", "Haste", "Reach", "Menace", "Defender",
  "Hexproof", "Indestructible", "Ward", "Flash",
];

/** PascalCase names that don't split naturally. */
const NAME_OVERRIDES: Record<string, string> = {
  EtbCounter: "ETB Counter",
  LivingWeapon: "Living Weapon",
  LivingMetal: "Living Metal",
  TotemArmor: "Totem Armor",
  SplitSecond: "Split Second",
  DoubleTeam: "Double Team",
  ReadAhead: "Read Ahead",
  WebSlinging: "Web-Slinging",
  LevelUp: "Level Up",
};

/** Split PascalCase into words: "FirstStrike" -> "First Strike". */
function splitPascalCase(s: string): string {
  return NAME_OVERRIDES[s] ?? s.replace(/([a-z])([A-Z])/g, "$1 $2");
}

/** Extract the display name from a Keyword value. */
export function getKeywordName(kw: Keyword): string {
  if (typeof kw === "string") return splitPascalCase(kw);
  const key = Object.keys(kw)[0];
  if (key === "Unknown") return String(kw[key]);
  if (key === "Typecycling") {
    const subtype = kw[key]?.subtype ?? "";
    return `${subtype}cycling`;
  }
  // CR 702.124: Partner family — variant-specific display names
  if (key === "Partner") {
    const partnerVal = (kw as Record<string, unknown>)[key] as { type?: string } | null;
    switch (partnerVal?.type) {
      case "FriendsForever": return "Friends Forever";
      case "CharacterSelect": return "Character Select";
      case "DoctorsCompanion": return "Doctor's Companion";
      case "ChooseABackground": return "Choose a Background";
    }
  }
  return splitPascalCase(key);
}

/**
 * Format a ManaCost for keyword display.
 *
 * ManaCost uses externally-tagged serde (no #[serde(tag)]):
 *   NoCost      → "NoCost"
 *   SelfManaCost → "SelfManaCost"
 *   Cost { shards, generic } → { "Cost": { "shards": [...], "generic": N } }
 */
export function formatKeywordManaCost(cost: unknown): string {
  if (cost === "NoCost") return "{0}";
  if (cost === "SelfManaCost") return "its mana cost";
  if (cost && typeof cost === "object") {
    const inner = (cost as Record<string, { shards?: string[]; generic?: number }>).Cost;
    if (inner) {
      const parts: string[] = [];
      if (inner.generic) parts.push(`{${inner.generic}}`);
      for (const shard of inner.shards ?? []) {
        parts.push(`{${SHARD_ABBREVIATION[shard] ?? shard}}`);
      }
      return parts.join("") || "{0}";
    }
  }
  return "";
}

/** Keywords parameterized with ManaCost. */
const MANA_COST_KEYWORDS = new Set([
  "Kicker", "Cycling", "Flashback", "Equip", "Unearth", "Reconfigure",
  "Bestow", "Embalm", "Eternalize", "Ninjutsu", "Prowl", "Morph",
  "Megamorph", "Madness", "Dash", "Emerge", "Escape", "Evoke", "Foretell",
  "Mutate", "Disturb", "Disguise", "Blitz", "Overload", "Spectacle",
  "Surge", "Encore", "Buyback", "Echo", "Outlast", "Scavenge", "Fortify",
  "Prototype", "Plot", "Craft", "Offspring", "Impending", "LevelUp",
  "Warp", "Sneak", "WebSlinging", "Squad",
]);

/** Keywords parameterized with a u32. */
const U32_KEYWORDS = new Set([
  "Dredge", "Modular", "Renown", "Fabricate", "Annihilator", "Bushido",
  "Tribute", "Afterlife", "Fading", "Vanishing", "Rampage", "Absorb",
  "Crew", "Hideaway", "Poisonous", "Bloodthirst", "Amplify", "Graft",
  "Devour", "Toxic", "Saddle", "Soulshift", "Backup", "Firebending",
]);

/** Extract human-readable detail for parameterized keywords, or null. */
export function getKeywordDetail(kw: Keyword): string | null {
  if (typeof kw === "string") return null;
  const key = Object.keys(kw)[0];
  const val = kw[key];

  if (MANA_COST_KEYWORDS.has(key)) return formatKeywordManaCost(val);
  if (U32_KEYWORDS.has(key)) return String(val);

  if (key === "Protection") return formatProtection(val);
  if (key === "Ward") return formatWard(val);
  if (key === "Typecycling") return formatKeywordManaCost(val?.cost);
  if (key === "EtbCounter") {
    const ct = val?.counter_type ?? "unknown";
    const count = val?.count ?? 0;
    return `enters with ${count} ${formatCounterName(ct)} counter${count !== 1 ? "s" : ""}`;
  }
  if (key === "Mobilize") {
    // QuantityExpr uses #[serde(tag = "type")]: { type: "Fixed", value: N }
    if (val && typeof val === "object" && val.type === "Fixed") {
      return String(val.value);
    }
    return null;
  }
  if (key === "Partner") {
    if (!val) return null;
    if (val.type === "With") return `with ${val.data}`;
    return null;
  }
  if (key === "Landwalk") return val;
  if (key === "Enchant" || key === "Companion") return null;

  return null;
}

function formatProtection(val: unknown): string {
  if (typeof val === "string") {
    if (val === "Multicolored") return "from multicolored";
    if (val === "ChosenColor") return "from chosen color";
    return `from ${val.toLowerCase()}`;
  }
  if (val && typeof val === "object") {
    const obj = val as Record<string, string>;
    if ("Color" in obj) return `from ${obj.Color.toLowerCase()}`;
    if ("CardType" in obj) return `from ${obj.CardType.toLowerCase()}s`;
    if ("Quality" in obj) return `from ${obj.Quality}`;
  }
  return "";
}

function formatWard(val: unknown): string {
  if (!val || typeof val !== "object") return "";
  const w = val as { type: string; data?: unknown };
  if (w.type === "Mana") return formatKeywordManaCost(w.data);
  if (w.type === "PayLife") return `pay ${w.data} life`;
  if (w.type === "DiscardCard") return "discard a card";
  if (w.type === "Sacrifice") {
    const d = w.data as { count: number } | undefined;
    const n = d?.count ?? 1;
    return n > 1 ? `sacrifice ${n} permanents` : "sacrifice a permanent";
  }
  if (w.type === "Waterbend") return `waterbend ${formatKeywordManaCost(w.data)}`;
  return "";
}

function formatCounterName(type: string): string {
  if (type === "Plus1Plus1") return "+1/+1";
  if (type === "Minus1Minus1") return "-1/-1";
  return type.toLowerCase();
}

/** Combine name + detail into a single display string. */
export function getKeywordDisplayText(kw: Keyword): string {
  const name = getKeywordName(kw);
  const detail = getKeywordDetail(kw);
  if (!detail) return name;
  return `${name} ${detail}`;
}

/** True if the keyword is in current keywords but not in base_keywords. */
export function isGrantedKeyword(kw: Keyword, baseKeywords: Keyword[]): boolean {
  const name = getKeywordName(kw);
  return !baseKeywords.some((bk) => getKeywordName(bk) === name);
}

/** Sort keywords by combat-relevance priority, then alphabetically. */
export function sortKeywords(keywords: Keyword[]): Keyword[] {
  return [...keywords].sort((a, b) => {
    const nameA = getKeywordName(a);
    const nameB = getKeywordName(b);
    const idxA = KEYWORD_DISPLAY_ORDER.indexOf(nameA);
    const idxB = KEYWORD_DISPLAY_ORDER.indexOf(nameB);
    const prioA = idxA >= 0 ? idxA : KEYWORD_DISPLAY_ORDER.length;
    const prioB = idxB >= 0 ? idxB : KEYWORD_DISPLAY_ORDER.length;
    if (prioA !== prioB) return prioA - prioB;
    return nameA.localeCompare(nameB);
  });
}
