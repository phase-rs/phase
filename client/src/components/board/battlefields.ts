import type { ManaColor } from "../../adapter/types.ts";

const BASE = import.meta.env.BASE_URL;

function img(name: string): string {
  return `${BASE}battlefield/${name}.webp`;
}

export interface BattlefieldConfig {
  id: string;
  label: string;
  color: ManaColor;
  image: string;
}

export const BATTLEFIELDS: BattlefieldConfig[] = [
  { id: "air_angelic_sky",            label: "Angelic Sky",        color: "White", image: img("air_angelic_sky") },
  { id: "water_moonlit_ocean_temple", label: "Ocean Temple",       color: "Blue",  image: img("water_moonlit_ocean_temple") },
  { id: "water_frozen_aurora",        label: "Frozen Aurora",      color: "Blue",  image: img("water_frozen_aurora") },
  { id: "shadow_haunted_graveyard",   label: "Haunted Graveyard",  color: "Black", image: img("shadow_haunted_graveyard") },
  { id: "shadow_ruined_archway",      label: "Ruined Archway",     color: "Black", image: img("shadow_ruined_archway") },
  { id: "shadow_moon_coven_sanctum",  label: "Moon Coven Sanctum", color: "Black", image: img("shadow_moon_coven_sanctum") },
  { id: "fire_molten",                label: "Molten",             color: "Red",   image: img("fire_molten") },
  { id: "earth_jurassic",             label: "Jurassic",           color: "Green", image: img("earth_jurassic") },
  { id: "earth_snowy_forest",         label: "Snowy Forest",       color: "Green", image: img("earth_snowy_forest") },
];

export const BATTLEFIELD_MAP: Record<string, BattlefieldConfig> = Object.fromEntries(
  BATTLEFIELDS.map((b) => [b.id, b]),
);

const BY_COLOR: Record<ManaColor, BattlefieldConfig[]> = {
  White: [], Blue: [], Black: [], Red: [], Green: [],
};
for (const bf of BATTLEFIELDS) {
  BY_COLOR[bf.color].push(bf);
}

/** Pick a random battlefield for a mana color (used in auto mode). */
export function getRandomBattlefield(color: ManaColor): BattlefieldConfig {
  const candidates = BY_COLOR[color];
  return candidates[Math.floor(Math.random() * candidates.length)];
}
