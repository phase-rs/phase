import { cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { GameAction } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../test/factories/gameStateFactory.ts";
import { useCastableZoneObjects } from "../useCastableZoneObjects.ts";

const flashback: GameAction = {
  type: "CastSpell",
  data: { object_id: 7, card_id: 700, targets: [] },
};

function seed(sharedHolder: number | null) {
  const card = buildGameObject({ id: 7, card_id: 700, zone: "Graveyard", owner: 0 });
  const gameState = buildGameState({
    players: buildPlayers([{ id: 0, graveyard: [card.id] }, { id: 1, graveyard: [] }]),
    objects: buildObjectMap(card),
    battlefield: [],
    exile: [],
    stack: [],
    ...(sharedHolder == null
      ? {}
      : { derived: { shared_piles: { library: sharedHolder, graveyard: sharedHolder } } }),
  });
  useGameStore.setState({
    gameState,
    legalActionsByObject: { "7": [flashback] },
  });
}

afterEach(() => {
  cleanup();
  useGameStore.setState({ gameState: null, legalActionsByObject: {} });
});

describe("useCastableZoneObjects graveyard", () => {
  it("offers the shared graveyard's castable cards to the seat that does not hold it", () => {
    seed(0);
    const { result } = renderHook(() => useCastableZoneObjects("graveyard", 1));
    expect(result.current.map((obj) => obj.id)).toEqual([7]);
  });

  it("reads only the seat's own graveyard when nothing is shared", () => {
    seed(null);
    expect(
      renderHook(() => useCastableZoneObjects("graveyard", 0)).result.current.map((obj) => obj.id),
    ).toEqual([7]);
    expect(renderHook(() => useCastableZoneObjects("graveyard", 1)).result.current).toEqual([]);
  });
});
