import type { DeckListData } from "./seatTypes";

/** Every card-name list of the engine's `DeckData`, in its declaration order.
 *  Pinned to the engine's serialized field set by
 *  `client/src/test/fixtures/seat-deck-echo-wire.json`. */
export const DECK_LIST_FIELDS = [
  "main_deck", "sideboard", "commander", "companion", "attraction_deck",
  "planar_deck", "scheme_deck", "contraption_deck", "sticker_sheets", "signature_spell",
] as const satisfies readonly (keyof DeckListData)[];
export type DeckListField = (typeof DECK_LIST_FIELDS)[number];

type ListKeys = {
  [K in keyof DeckListData]-?: NonNullable<DeckListData[K]> extends string[] ? K : never;
}[keyof DeckListData];
// A list added to `DeckListData` but not to `DECK_LIST_FIELDS` is a type error here.
// Must stay an exported, assigned const: `noUnusedLocals` rejects an unused local.
export const DECK_LIST_FIELDS_ARE_EXHAUSTIVE: [Exclude<ListKeys, DeckListField>] extends [never] ? true : never = true;

/** Which deck a `DeckChoice::DeckList` names, comparable across the client's
 *  spelling and the engine's echo (every list present, engine field order,
 *  `bracket_tier` always serialized). Reads the lists only, each in its own
 *  order, as the echo returns them; an absent list is empty, as the engine
 *  reads it. The declared bracket tier is not part of which deck it is. */
export function deckListIdentityKey(data: DeckListData): string {
  return JSON.stringify(DECK_LIST_FIELDS.map((field) => data[field] ?? []));
}
