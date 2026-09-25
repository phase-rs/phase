import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { GameAction, GameObject, WaitingFor } from "../../../adapter/types.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CastingVariantModal } from "../CastingVariantModal.tsx";

const normalRightAction: GameAction = {
  type: "ChooseCastingVariant",
  data: { index: 1 },
};
const fuseAction: GameAction = {
  type: "ChooseCastingVariant",
  data: { index: 2 },
};

function castingVariantState(player = 0) {
  const breaking = gameObjectFactory
    .sorcery()
    .inHand()
    .withId(157)
    .named("Breaking")
    .withCost(["Blue", "Black"])
    .build();
  const splitCard: GameObject = {
    ...breaking,
    back_face: {
      name: "Entering",
      power: null,
      toughness: null,
      mana_cost: { type: "Cost", shards: ["Black", "Red"], generic: 4 },
      card_types: { supertypes: [], core_types: ["Sorcery"], subtypes: [] },
      keywords: [],
      abilities: [],
      color: ["Black", "Red"],
    },
  };
  const waitingFor: WaitingFor = {
    type: "CastingVariantChoice",
    data: {
      player,
      object_id: splitCard.id,
      card_id: splitCard.card_id,
      options: [
        {
          variant: { type: "Normal" },
          face: "Left",
          mana_cost: { type: "Cost", shards: ["Blue", "Black"], generic: 0 },
        },
        {
          variant: { type: "Normal" },
          face: "Right",
          mana_cost: { type: "Cost", shards: ["Black", "Red"], generic: 4 },
        },
        {
          variant: { type: "Fuse" },
          face: "Left",
          mana_cost: {
            type: "Cost",
            shards: ["Blue", "Black", "Black", "Red"],
            generic: 4,
          },
        },
      ],
    },
  };

  return gameStateFactory
    .withPlayers(0, 1)
    .withObjects(splitCard)
    .waitingFor(waitingFor)
    .build();
}

describe("CastingVariantModal", () => {
  afterEach(cleanup);

  it("renders engine-authorized split tuples with raw faces and translated variant labels", () => {
    const gameState = castingVariantState();
    setGameStoreForTest({
      gameState,
      legalActions: [normalRightAction, fuseAction],
    });

    render(<CastingVariantModal />);

    expect(
      screen.queryByRole("button", { name: /Cast Normally Breaking/ }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Cast Normally Entering/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Cast with Fuse Breaking/ })).toBeInTheDocument();
  });

  it("dispatches the exact engine-issued right-half action", () => {
    const { dispatch } = setGameStoreForTest({
      gameState: castingVariantState(),
      legalActions: [normalRightAction, fuseAction],
    });

    render(<CastingVariantModal />);

    fireEvent.click(screen.getByRole("button", { name: /Cast Normally Entering/ }));
    expect(dispatch).toHaveBeenNthCalledWith(1, normalRightAction);
  });

  it("does not render for another player's casting-variant prompt", () => {
    const gameState = castingVariantState(1);
    setGameStoreForTest({ gameState, legalActions: [normalRightAction, fuseAction] });

    render(<CastingVariantModal />);

    expect(screen.queryByRole("heading", { name: "Choose Cast" })).not.toBeInTheDocument();
  });
});
