import type { ManaColor } from "../../adapter/types.ts";

const ROOT = `${import.meta.env.BASE_URL}battlefield/arenas/`;

export const ARENA_ART: Record<ManaColor, string> = {
  White: `${ROOT}angelic-sky-topdown.webp`,
  Blue: `${ROOT}ocean-temple-topdown.webp`,
  Black: `${ROOT}moon-coven-topdown.webp`,
  Red: `${ROOT}molten-topdown.webp`,
  Green: `${ROOT}elderwood-topdown.webp`,
};
