import { fetchCardImageUrl } from "./scryfall.ts";

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

export interface PlayerAvatar {
  name: string;
  cardName: string;
}

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

export async function fetchAvatarArtUrl(cardName: string): Promise<string | null> {
  try {
    return await fetchCardImageUrl(cardName, 0, "art_crop");
  } catch {
    return null;
  }
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
