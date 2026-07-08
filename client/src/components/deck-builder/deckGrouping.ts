import type { ScryfallCard } from "../../services/scryfall";

/** How the main deck is sub-grouped: by card type or by color. Lands stay
 *  their own section in both modes. */
export type GroupMode = "type" | "color";

export const TYPE_GROUP_ORDER = [
  "Creatures",
  "Planeswalkers",
  "Instants",
  "Sorceries",
  "Artifacts",
  "Enchantments",
  "Battles",
  "Lands",
  "Other",
] as const;

export const COLOR_GROUP_ORDER = [
  "White",
  "Blue",
  "Black",
  "Red",
  "Green",
  "Multicolor",
  "Colorless",
  "Lands",
] as const;

const COLOR_NAMES: Record<string, string> = {
  W: "White",
  U: "Blue",
  B: "Black",
  R: "Red",
  G: "Green",
};

/** First-match precedence: land-before-creature preserves the prior
 *  getTypeRank precedence, and MDFC `"//"` type lines classify by the first
 *  matching face (pre-existing behavior, intentional). */
export function classifyByType(card: ScryfallCard | undefined): string {
  const typeLine = card?.type_line.toLowerCase() ?? "";
  if (typeLine.includes("land")) return "Lands";
  if (typeLine.includes("creature")) return "Creatures";
  if (typeLine.includes("planeswalker")) return "Planeswalkers";
  if (typeLine.includes("instant")) return "Instants";
  if (typeLine.includes("sorcery")) return "Sorceries";
  if (typeLine.includes("artifact")) return "Artifacts";
  if (typeLine.includes("enchantment")) return "Enchantments";
  if (typeLine.includes("battle")) return "Battles";
  return "Other";
}

export function classifyByColor(card: ScryfallCard | undefined): string {
  if (!card) return "Colorless";
  if (card.type_line.toLowerCase().includes("land")) return "Lands";
  const ci = card.color_identity;
  if (ci.length === 0) return "Colorless";
  if (ci.length > 1) return "Multicolor";
  return COLOR_NAMES[ci[0]] ?? "Colorless";
}

export function groupKey(mode: GroupMode, card: ScryfallCard | undefined): string {
  return mode === "type" ? classifyByType(card) : classifyByColor(card);
}

export function groupOrder(mode: GroupMode): readonly string[] {
  return mode === "type" ? TYPE_GROUP_ORDER : COLOR_GROUP_ORDER;
}

export function groupRank(mode: GroupMode, card: ScryfallCard | undefined): number {
  const order = groupOrder(mode);
  const index = order.indexOf(groupKey(mode, card));
  return index === -1 ? order.length : index;
}

export function groupTitleKey(mode: GroupMode, key: string): string {
  return (mode === "type" ? "group." : "colorGroup.") + key;
}

/** A section-header accent: a small leading bar color and a matching title
 *  tint. Shared by the list and stack views so both modes' headers read the
 *  same. Color hues follow the deck-builder color language (ManaCurve's
 *  COLOR_MAP); type sections get distinct category accents. Classes are
 *  enumerated as full literals so Tailwind's JIT keeps them. */
export interface GroupAccent {
  bar: string;
  text: string;
}

const GROUP_ACCENTS: Record<string, GroupAccent> = {
  // Color mode (WUBRG mirror ManaCurve's COLOR_MAP, lightened for contrast on
  // the dark header background).
  White: { bar: "bg-amber-200", text: "text-amber-200" },
  Blue: { bar: "bg-blue-500", text: "text-blue-300" },
  Black: { bar: "bg-zinc-400", text: "text-zinc-300" },
  Red: { bar: "bg-red-500", text: "text-red-300" },
  Green: { bar: "bg-green-500", text: "text-green-300" },
  Multicolor: { bar: "bg-amber-400", text: "text-amber-300" },
  Colorless: { bar: "bg-slate-400", text: "text-slate-300" },
  // Type mode.
  Creatures: { bar: "bg-emerald-500", text: "text-emerald-300" },
  Planeswalkers: { bar: "bg-fuchsia-500", text: "text-fuchsia-300" },
  Instants: { bar: "bg-sky-500", text: "text-sky-300" },
  Sorceries: { bar: "bg-indigo-500", text: "text-indigo-300" },
  Artifacts: { bar: "bg-slate-400", text: "text-slate-300" },
  Enchantments: { bar: "bg-amber-300", text: "text-amber-200" },
  Battles: { bar: "bg-orange-500", text: "text-orange-300" },
  Other: { bar: "bg-gray-500", text: "text-gray-400" },
  // Shared by both modes.
  Lands: { bar: "bg-stone-500", text: "text-stone-300" },
};

/** Neutral fallback for headers with no group key (sideboard/maybeboard lanes
 *  in the stack view) or any unrecognized key. */
const NEUTRAL_ACCENT: GroupAccent = { bar: "bg-white/15", text: "text-slate-400" };

/** Resolve a header accent. Accepts either a raw group key (`"White"`, as the
 *  list view holds) or a title sub-path (`"colorGroup.White"` / `"group.Lands"`
 *  / `"sideboardGroup"`, as the stack view stores on each group). */
export function groupAccent(key: string): GroupAccent {
  const raw = key.includes(".") ? key.slice(key.lastIndexOf(".") + 1) : key;
  return GROUP_ACCENTS[raw] ?? NEUTRAL_ACCENT;
}
