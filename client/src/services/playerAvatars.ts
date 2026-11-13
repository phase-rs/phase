interface AvatarIdentity {
  name: string;
  cardName: string;
}

const PLANESWALKER_IDENTITIES: AvatarIdentity[] = [
  { name: "Jace", cardName: "Jace, the Mind Sculptor" },
  { name: "Liliana", cardName: "Liliana of the Veil" },
  { name: "Chandra", cardName: "Chandra, Torch of Defiance" },
  { name: "Nissa", cardName: "Nissa, Who Shakes the World" },
  { name: "Gideon", cardName: "Gideon, Ally of Zendikar" },
  { name: "Ajani", cardName: "Ajani Vengeant" },
  { name: "Sorin", cardName: "Sorin, Lord of Innistrad" },
  { name: "Elspeth", cardName: "Elspeth, Sun's Champion" },
  { name: "Teferi", cardName: "Teferi, Hero of Dominaria" },
  { name: "Karn", cardName: "Karn Liberated" },
  { name: "Ashiok", cardName: "Ashiok, Nightmare Weaver" },
  { name: "Vraska", cardName: "Vraska, Golgari Queen" },
  { name: "Nahiri", cardName: "Nahiri, the Harbinger" },
  { name: "Tamiyo", cardName: "Tamiyo, Field Researcher" },
  { name: "Narset", cardName: "Narset, Parter of Veils" },
  { name: "Vivien", cardName: "Vivien, Monsters' Advocate" },
];

/**
 * Survivor-seat identities for Turtle Team-Up Horde games, in seat order:
 * the four Turtles, each pinned to a real `tmt` printing whose art resolves
 * through the same card-name path as the planeswalker roster above. A name
 * missing from an older local card database degrades to the initial tile —
 * the same fallback every other name-based avatar already has.
 */
export const TURTLE_TEAMUP_SURVIVOR_IDENTITIES: readonly AvatarIdentity[] = [
  { name: "Leonardo", cardName: "Leonardo, Sewer Samurai" },
  { name: "Raphael", cardName: "Raphael, Ninja Destroyer" },
  { name: "Donatello", cardName: "Donatello, Gadget Master" },
  { name: "Michelangelo", cardName: "Michelangelo, Improviser" },
];

export interface PlayerAvatar {
  name: string;
  cardName: string;
}

export type PlayerAvatarIdentity =
  | { kind: "card"; cardName: string }
  | { kind: "external"; url: string };

export function assignRandomAvatars(playerCount: number, seed?: number | string): PlayerAvatar[] {
  const shuffled = [...PLANESWALKER_IDENTITIES];
  const numericSeed = typeof seed === "string"
    ? hashStringToSeed(seed)
    : seed ?? Date.now();
  const random = mulberry32(numericSeed);
  for (let i = shuffled.length - 1; i > 0; i--) {
    const j = Math.floor(random() * (i + 1));
    [shuffled[i], shuffled[j]] = [shuffled[j], shuffled[i]];
  }
  return shuffled.slice(0, playerCount).map((id) => ({
    name: id.name,
    cardName: id.cardName,
  }));
}

export function assignAvatarForSeat(
  playerCount: number,
  seat: number,
  seed?: number | string,
): PlayerAvatar | null {
  return assignRandomAvatars(playerCount, seed)[seat] ?? null;
}

export function avatarCardNameForName(name: string): string | null {
  return PLANESWALKER_IDENTITIES.find((id) => id.name === name)?.cardName ?? null;
}

/**
 * Direct art URL for a Scryfall printing id (a Boss avatar's pinned
 * `token_image_ref`). Bosses are token-layout cards with no card-database
 * entry, so the card-name path cannot resolve them — the printing id is the
 * only art channel. `art_crop` matches the square avatar tile.
 */
export function scryfallPrintingImageUrl(scryfallId: string): string {
  return `https://api.scryfall.com/cards/${scryfallId}?format=image&version=art_crop`;
}

function hashStringToSeed(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h * 33) ^ s.charCodeAt(i)) | 0;
  }
  return h >>> 0;
}

function mulberry32(seed: number): () => number {
  let t = seed | 0;
  return () => {
    t = (t + 0x6d2b79f5) | 0;
    let x = Math.imul(t ^ (t >>> 15), 1 | t);
    x = (x + Math.imul(x ^ (x >>> 7), 61 | x)) ^ x;
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}
