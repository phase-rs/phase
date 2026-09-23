// The /lfg format table. The bot's Docker context is scripts/card-bot only, so it
// cannot import the client's FORMAT_REGISTRY at runtime; this mirrors the fields
// /lfg needs, in registry order. __tests__/formats.test.ts deep-equals it against
// client/src/data/formatRegistry.ts, so a registry change fails a test, not a game.

export interface LfgFormat {
  /** `GameFormat` key, as the client's `format=` link parameter expects it. */
  format: string;
  label: string;
  min_players: number;
  max_players: number;
}

/** Who hosts an /lfg game: the creator's browser, or a dedicated phase-server. */
export type LfgMode = "p2p" | "server";

export const FORMATS: readonly LfgFormat[] = [
  { format: "Standard", label: "Standard", min_players: 2, max_players: 2 },
  { format: "Pioneer", label: "Pioneer", min_players: 2, max_players: 2 },
  { format: "Modern", label: "Modern", min_players: 2, max_players: 2 },
  { format: "Premodern", label: "Premodern", min_players: 2, max_players: 2 },
  { format: "Legacy", label: "Legacy", min_players: 2, max_players: 2 },
  { format: "Vintage", label: "Vintage", min_players: 2, max_players: 2 },
  { format: "Historic", label: "Historic", min_players: 2, max_players: 2 },
  { format: "Timeless", label: "Timeless", min_players: 2, max_players: 2 },
  { format: "Pauper", label: "Pauper", min_players: 2, max_players: 2 },
  { format: "Freeform", label: "Freeform", min_players: 2, max_players: 2 },
  { format: "Commander", label: "Commander", min_players: 2, max_players: 6 },
  { format: "DuelCommander", label: "Duel Commander", min_players: 2, max_players: 2 },
  { format: "PauperCommander", label: "Pauper Commander", min_players: 2, max_players: 6 },
  { format: "TinyLeaders", label: "Tiny Leaders: Reborn", min_players: 2, max_players: 2 },
  { format: "Oathbreaker", label: "Oathbreaker", min_players: 2, max_players: 4 },
  { format: "Brawl", label: "Brawl", min_players: 2, max_players: 2 },
  { format: "HistoricBrawl", label: "Historic Brawl", min_players: 2, max_players: 2 },
  { format: "CommanderDraft", label: "Commander Draft", min_players: 3, max_players: 8 },
  { format: "FreeformCommander", label: "Freeform Commander", min_players: 2, max_players: 4 },
  { format: "FreeForAll", label: "Free-for-All", min_players: 2, max_players: 6 },
  { format: "TwoHeadedGiant", label: "Two-Headed Giant", min_players: 4, max_players: 4 },
  { format: "Archenemy", label: "Archenemy", min_players: 2, max_players: 6 },
  { format: "Planechase", label: "Planechase", min_players: 2, max_players: 4 },
  { format: "Limited", label: "Limited", min_players: 2, max_players: 2 },
  { format: "Momir", label: "Momir's Madness", min_players: 2, max_players: 2 },
];

export function findFormat(key: string): LfgFormat | undefined {
  return FORMATS.find((f) => f.format === key);
}

/** Mirrors `P2P_MAX_PEERS` in client/src/components/lobby/HostSetup.tsx (the P2P
 *  hub-and-spoke seat cap); pinned by __tests__/formats.test.ts. */
export const P2P_MAX_PEERS = 6;

/** The largest seat count any format allows (the `/lfg seats` option's maximum). */
export const MAX_SEATS = Math.max(...FORMATS.map((f) => f.max_players));

/** Most seats an /lfg game of this format can have in this mode. */
export function seatCap(f: LfgFormat, mode: LfgMode): number {
  return mode === "p2p" ? Math.min(f.max_players, P2P_MAX_PEERS) : f.max_players;
}

/** Formats whose usual table is smaller than their cap. Kept apart from FORMATS,
 *  which mirrors the client registry and has no such field. */
const PREFERRED_SEATS: Readonly<Record<string, number>> = { Commander: 4 };

/** Seats an /lfg game gets when the `seats` option is omitted. */
export function defaultSeats(f: LfgFormat, mode: LfgMode): number {
  const cap = seatCap(f, mode);
  const preferred = PREFERRED_SEATS[f.format];
  return preferred === undefined ? cap : Math.min(preferred, cap);
}
