import { describe, expect, it } from "vitest";

import type { GameState, ObjectId } from "../../../../../adapter/types.ts";
import { GALLERY_SCENARIOS, galleryBoard } from "../galleryScenarios.ts";

/** Every list that holds `id`, named by the zone it stands for. */
function listsHolding(state: GameState, id: ObjectId): string[] {
  const lists: [string, ObjectId[]][] = [
    ["Battlefield", state.battlefield],
    ["Exile", state.exile],
    ["Stack", state.stack.map((entry) => entry.id)],
    ...state.players.flatMap((player): [string, ObjectId[]][] => [
      ["Hand", player.hand],
      ["Library", player.library],
      ["Graveyard", player.graveyard],
    ]),
  ];
  return lists.filter(([, ids]) => ids.includes(id)).map(([zone]) => zone);
}

function expectConsistent(state: GameState) {
  for (const object of Object.values(state.objects)) {
    expect(listsHolding(state, object.id), `${object.name} (${object.id})`).toEqual([object.zone]);
  }
}

describe("gallery scenarios", () => {
  it("start from a board whose zone lists match every card's zone", () => {
    expectConsistent(galleryBoard());
  });

  it.each(Object.entries(GALLERY_SCENARIOS))("%s keeps every card in exactly its zone's list after each update", (_, scenario) => {
    let state = galleryBoard();
    for (const batch of scenario.batches) {
      const next = structuredClone(state);
      const events = batch(next);
      expect(events.length).toBeGreaterThan(0);
      expectConsistent(next);
      state = next;
    }
  });
});
