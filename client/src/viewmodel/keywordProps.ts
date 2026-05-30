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
  JobSelect: "Job Select",
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

/**
 * Extract the N parameter from a Crew(N) keyword on this object, or null if
 * the object has no Crew keyword. Mirrors the Saddle accessor below.
 *
 * CR 702.122a — Crew is parameterized: "Crew N" gates which creature subsets
 * can pay the cost. The frontend reads this for the modal label only.
 */
export function getCrewPower(keywords: Keyword[]): number | null {
  for (const kw of keywords) {
    if (typeof kw === "object" && kw !== null && "Crew" in kw) {
      const value = (kw as Record<string, unknown>).Crew;
      // CR 702.122: Crew carries `{ power, once_per_turn }`.
      if (typeof value === "object" && value !== null && "power" in value) {
        const power = (value as Record<string, unknown>).power;
        if (typeof power === "number") return power;
      }
    }
  }
  return null;
}

/**
 * Extract the N parameter from a Saddle(N) keyword on this object, or null if
 * the object has no Saddle keyword. CR 702.171a parameterized keyword.
 */
export function getSaddlePower(keywords: Keyword[]): number | null {
  for (const kw of keywords) {
    if (typeof kw === "object" && kw !== null && "Saddle" in kw) {
      const value = (kw as Record<string, unknown>).Saddle;
      if (typeof value === "number") return value;
    }
  }
  return null;
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
  "Warp", "Sneak", "WebSlinging", "Squad", "Cleave",
]);

/** Keywords parameterized with a u32. */
const U32_KEYWORDS = new Set([
  "Dredge", "Modular", "Renown", "Fabricate", "Annihilator", "Bushido",
  "Tribute", "Afterlife", "Fading", "Vanishing", "Rampage", "Absorb",
  "Hideaway", "Poisonous", "Bloodthirst", "Amplify", "Graft",
  "Devour", "Toxic", "Saddle", "Soulshift", "Backup",
]);

function formatQuantityKeywordDetail(val: unknown): string | null {
  if (typeof val === "number") return String(val);
  if (val && typeof val === "object" && "type" in val && val.type === "Fixed") {
    const value = (val as { value?: unknown }).value;
    return typeof value === "number" ? String(value) : null;
  }
  if (val && typeof val === "object" && "type" in val) return "X";
  return null;
}

/** Extract human-readable detail for parameterized keywords, or null. */
export function getKeywordDetail(kw: Keyword): string | null {
  if (typeof kw === "string") return null;
  const key = Object.keys(kw)[0];
  const val = kw[key];

  if (MANA_COST_KEYWORDS.has(key)) return formatKeywordManaCost(val);
  if (U32_KEYWORDS.has(key)) return String(val);

  // CR 702.122: Crew carries `{ power, once_per_turn }` — show the power.
  if (key === "Crew") {
    if (val && typeof val === "object" && "power" in val) {
      const power = (val as { power?: unknown }).power;
      return typeof power === "number" ? String(power) : null;
    }
    return null;
  }
  if (key === "Protection") return formatProtection(val);
  if (key === "Ward") return formatWard(val);
  if (key === "Typecycling") return formatKeywordManaCost(val?.cost);
  if (key === "EtbCounter") {
    const ct = val?.counter_type ?? "unknown";
    const count = val?.count ?? 0;
    return `enters with ${count} ${formatCounterName(ct)} counter${count !== 1 ? "s" : ""}`;
  }
  if (key === "Mobilize") {
    return formatQuantityKeywordDetail(val);
  }
  if (key === "Firebending") {
    return formatQuantityKeywordDetail(val);
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
  if (type === "P1P1") return "+1/+1";
  if (type === "M1M1") return "-1/-1";
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
