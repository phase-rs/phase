import type { GameFormat } from "../adapter/types";
import type { DeckChoice } from "../multiplayer/seatTypes";
import { loadSavedDeck } from "../constants/storage";
import { expandParsedDeck, type ExpandedDeck } from "./deckParser";
import { deckSupplyForFormat } from "./engineRuntime";

/** The host's pile for a `HostPile` format: the engine's default list or one of their saved decks. */
export type PileSource = { type: "Default" } | { type: "SavedDeck"; name: string };

export const DEFAULT_PILE_SOURCE: PileSource = { type: "Default" };

/** The host's pile pick as a start form holds it. */
export interface PileChoice {
  source: PileSource;
  /** False while a named pile's engine verdict is pending or refuses it. */
  legal: boolean;
}

export const DEFAULT_PILE_CHOICE: PileChoice = { source: DEFAULT_PILE_SOURCE, legal: true };

/** The `pile` URL parameter names a saved deck; its absence is the default pile. */
export function pileSourceFromParam(pile: string | null | undefined): PileSource {
  return pile ? { type: "SavedDeck", name: pile } : DEFAULT_PILE_SOURCE;
}

export function pileSourceParam(source: PileSource): string | null {
  return source.type === "SavedDeck" ? source.name : null;
}

/** A seat that submits nothing; the engine supplies or defaults its deck. */
export function emptySeatDeck(): ExpandedDeck {
  return expandParsedDeck({ main: [], sideboard: [] });
}

/** The AI-seat deck entry for a format whose deck the player does not build. */
export function suppliedAiDeckChoice(): { id: string; label: string; choice: DeckChoice } {
  return { id: "supplied", label: "supplied", choice: { type: "DeckList", data: emptySeatDeck() } };
}

/**
 * The pile seat's submission: a named saved deck only when the engine says the
 * host supplies the pile, else the empty deck; `null` when that saved deck is gone.
 */
export async function pileSeatDeck(format: GameFormat, source: PileSource): Promise<ExpandedDeck | null> {
  if (source.type === "Default") return emptySeatDeck();
  if ((await deckSupplyForFormat(format)) !== "HostPile") return emptySeatDeck();
  const deck = loadSavedDeck(source.name);
  return deck ? expandParsedDeck(deck) : null;
}
