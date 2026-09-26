export interface PlainBackgroundConfig {
  id: string;
  label: string;
  css: string;
}

export const PLAIN_BACKGROUNDS: PlainBackgroundConfig[] = [
  { id: "plain_slate",     label: "Slate",     css: "#0c1420" },
  { id: "plain_midnight",  label: "Midnight",  css: "#0b1220" },
  { id: "plain_charcoal",  label: "Charcoal",  css: "#18181b" },
  { id: "plain_forest",    label: "Forest",    css: "#14532d" },
  { id: "plain_indigo",    label: "Indigo",    css: "#312e81" },
  { id: "plain_crimson",   label: "Crimson",   css: "#7f1d1d" },
];

export const PLAIN_BACKGROUND_MAP: Record<string, PlainBackgroundConfig> = Object.fromEntries(
  PLAIN_BACKGROUNDS.map((b) => [b.id, b]),
);
