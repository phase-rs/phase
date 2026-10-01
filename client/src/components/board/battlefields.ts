import type { ManaColor } from "../../adapter/types.ts";
import { ARENA_ART } from "./arenaBackgrounds.ts";

export interface BattlefieldConfig {
  id: string;
  label: string;
  color: ManaColor;
  image: string;
}

export const BATTLEFIELDS: BattlefieldConfig[] = [
  { id: "air_angelic_sky",            label: "Angelic Sky",        color: "White", image: ARENA_ART.White },
  { id: "water_moonlit_ocean_temple", label: "Ocean Temple",       color: "Blue",  image: ARENA_ART.Blue },
  { id: "shadow_moon_coven_sanctum",  label: "Moon Coven Sanctum", color: "Black", image: ARENA_ART.Black },
  { id: "fire_molten",               label: "Molten",             color: "Red",   image: ARENA_ART.Red },
  { id: "earth_snowy_forest",        label: "Snowy Forest",       color: "Green", image: ARENA_ART.Green },
];

export const BATTLEFIELD_MAP: Record<string, BattlefieldConfig> = Object.fromEntries(
  BATTLEFIELDS.map((b) => [b.id, b]),
);
