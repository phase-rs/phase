import type { Keyword } from "../adapter/types";
import { SHARD_ABBREVIATION } from "./costLabel";

/**
 * Standard reminder text for common keywords, keyed by the display name
 * returned by getKeywordName(). Used as title/tooltip text in the UI.
 */
const KEYWORD_REMINDER_TEXT: Partial<Record<string, string>> = {
  "Flying":        "Can't be blocked except by creatures with flying or reach.",
  "Reach":         "Can block creatures with flying.",
  "First Strike":  "Deals combat damage before creatures without first strike.",
  "Double Strike": "Deals both first-strike and regular combat damage.",
  "Deathtouch":    "Any amount of damage it deals to a creature is enough to destroy it.",
  "Trample":       "Excess combat damage is dealt to the player or planeswalker it's attacking.",
  "Lifelink":      "Damage it deals also causes its controller to gain that much life.",
  "Vigilance":     "Attacking doesn't cause this creature to tap.",
  "Haste":         "Can attack and activate {T} abilities the turn it enters the battlefield.",
  "Menace":        "Can't be blocked except by two or more creatures.",
  "Defender":      "Can't attack.",
  "Hexproof":      "Can't be the target of spells or abilities your opponents control.",
  "Shroud":        "Can't be the target of spells or abilities.",
  "Indestructible": "'Destroy' effects and lethal damage don't destroy this permanent.",
  "Ward":          "Whenever this becomes the target of a spell or ability an opponent controls, counter it unless that player pays the ward cost.",
  "Protection":    "Can't be blocked, targeted, dealt damage, enchanted, or equipped by anything with the stated quality.",
  "Flash":         "Can be cast any time you could cast an instant.",
  "Crew":          "Tap creatures you control with total power at least the crew value to make this Vehicle an artifact creature until end of turn.",
  "Saddle":        "Tap creatures you control with total power at least the saddle value to saddle this Mount. Activate only as a sorcery.",
  "Persist":       "When put into the graveyard from the battlefield with no -1/-1 counters, returns with a -1/-1 counter.",
  "Undying":       "When put into the graveyard from the battlefield with no +1/+1 counters, returns with a +1/+1 counter.",
  "Cascade":       "When cast, exile cards from your library until you find a cheaper nonland card and cast it for free.",
  "Convoke":       "Tap your creatures to help pay this spell's mana cost.",
  "Delve":         "Exile cards from your graveyard to pay {1} each while casting this.",
  "Prowess":       "Gets +1/+1 until end of turn whenever you cast a noncreature spell.",
  "Riot":          "Enters with your choice of a +1/+1 counter or haste.",
  "Phasing":       "Phases in or out before your untap step. While phased out, treated as nonexistent.",
  "Regenerate":    "The next time this would be destroyed this turn, tap it and remove all damage instead.",
  "Dredge":        "When you would draw, you may mill cards equal to its dredge value and return this card from your graveyard to your hand.",
  "Flashback":     "Cast from your graveyard for its flashback cost, then exile it.",
  "Cycling":       "Discard this card: draw a card.",
  "Kicker":        "You may pay an additional kicker cost for an enhanced effect.",
  "Equip":         "Attach to target creature you control. Activate only as a sorcery.",
  "Morph":         "Cast face down as a 2/2 creature for {3}. Turn face up for its morph cost.",
  "Megamorph":     "Cast face down as a 2/2 creature for {3}. Turn face up for its megamorph cost to also put a +1/+1 counter on it.",
  "Ninjutsu":      "Return an unblocked attacker you control to hand: put this card onto the battlefield tapped and attacking.",
  "Bushido":       "Gets +N/+N until end of turn whenever it blocks or becomes blocked.",
  "Annihilator":   "Whenever this attacks, the defending player sacrifices that many permanents.",
  "Shadow":        "Can only block or be blocked by creatures with shadow.",
  "Skulk":         "Can't be blocked by creatures with greater power.",
  "Madness":       "If you discard this card, you may cast it for its madness cost instead.",
  "Escape":        "Cast from your graveyard for its escape cost by also exiling other cards from your graveyard.",
  "Evoke":         "Cast for its evoke cost and sacrifice it when it enters the battlefield.",
  "Embalm":        "Exile from your graveyard: create a white Zombie token copy of this card.",
  "Eternalize":    "Exile from your graveyard: create a 4/4 black Zombie token copy of this card.",
  "Foretell":      "Pay {2} during your turn to exile face down. Cast for its foretell cost on a later turn.",
  "Dash":          "Cast for its dash cost to give it haste, returning it to your hand at the next end step.",
  "Mutate":        "Cast for its mutate cost below or above a non-Human creature you own to merge.",
  "Overload":      "Cast for its overload cost to affect all valid targets instead of one.",
  "Spectacle":     "Can be cast for its spectacle cost if an opponent lost life this turn.",
  "Surge":         "Can be cast for its surge cost if you or a teammate has cast another spell this turn.",
  "Emerge":        "Cast by sacrificing a creature and reducing its cost by that creature's mana value.",
  "Awaken":        "Cast for its awaken cost to also put +1/+1 counters on a target land and make it a 0/0 creature.",
  "Renown":        "When this deals combat damage to a player, if not yet renowned, put +1/+1 counters on it and it becomes renowned.",
  "Fabricate":     "When this enters the battlefield, put +1/+1 counters on it or create that many 1/1 Servo artifact creature tokens.",
  "Modular":       "Enters with +1/+1 counters. When it dies, put its counters on a target artifact creature.",
  "Graft":         "Enters with +1/+1 counters. Whenever another creature enters, you may move a counter from this to it.",
  "Fading":        "Enters with fade counters. At the beginning of your upkeep, remove one. Sacrifice it when the last is removed.",
  "Vanishing":     "Enters with time counters. At the beginning of your upkeep, remove one. Sacrifice it when the last is removed.",
  "Bloodthirst":   "Enters with +1/+1 counters if an opponent was dealt damage this turn.",
  "Poisonous":     "Whenever this deals combat damage to a player, that player gets poison counters.",
  "Toxic":         "Whenever this deals combat damage to a player, that player gets poison counters.",
  "Buyback":       "Pay the buyback cost to return this card to your hand instead of the graveyard after casting.",
  "Echo":          "At the beginning of your upkeep, if this entered since your last upkeep, sacrifice it unless you pay its echo cost.",
  "Scavenge":      "Exile this from your graveyard: put +1/+1 counters on target creature equal to this card's power.",
  "Unearth":       "Return from your graveyard to the battlefield with haste. Exile it at end of turn or if it would leave.",
  "Split Second":  "While this is on the stack, players can't cast spells or activate non-mana abilities.",
  "Totem Armor":   "If enchanted permanent would be destroyed, instead remove all damage from it and destroy this aura.",
  "Living Weapon": "Enters the battlefield attached to a 0/0 black Germ token.",
  "Banding":       "Creatures with banding can form a band when attacking or blocking; you assign damage for the band.",
  "Affinity":      "This spell costs {1} less to cast for each relevant permanent you control.",
  "Tribute":       "As this enters the battlefield, an opponent may put +1/+1 counters on it; if they don't, you get a triggered effect.",
  "Devour":        "As this enters the battlefield, you may sacrifice any number of creatures. It enters with +1/+1 counters equal to their total power.",
  "Amplify":       "As this enters, reveal creature cards in hand to put +1/+1 counters on it.",
  "Soulshift":     "When this dies, return target Spirit card with lesser mana value from your graveyard to your hand.",
  "Prowl":         "If a creature of the relevant type dealt combat damage this turn, you may cast this for its prowl cost.",
  "Backup":        "When this enters, put a +1/+1 counter on target creature. That creature gains the listed ability until end of turn.",
  "Offspring":     "Pay the offspring cost as you cast this to also create a 1/1 token copy.",
  "Disguise":      "Cast face down as a 2/2 for {3} with ward {2}. Turn face up for its disguise cost.",
  "Plot":          "Pay this card's plot cost to exile it. Cast it for free on a later turn.",
  "Impending":     "Cast for its impending cost with time counters. It isn't a creature until the last counter is removed.",
  "Double Team":   "When this attacks, if not yet doubled, exile and return it to your hand, then create a token copy.",
};

/**
 * Returns the reminder text for a keyword, or null if none is defined.
 * Parameterized keywords (Ward, Protection, etc.) get their reminder text
 * by name only — the cost/qualifier is already part of the display text.
 */
export function getKeywordReminderText(kw: Keyword): string | null {
  return KEYWORD_REMINDER_TEXT[getKeywordName(kw)] ?? null;
}

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

/**
 * CR 702.73a: Changeling makes an object every creature type. The engine
 * expands the object's subtypes to the full creature-type list at layer
 * evaluation; the display layer uses this to collapse that list to "Changeling"
 * rather than rendering the overflow wall of types. Changeling serializes as the
 * simple string keyword "Changeling".
 */
export function isChangeling(keywords: Keyword[]): boolean {
  return keywords.includes("Changeling");
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
