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

  // CR 601.2f-h: a menu option that pays an alternative cost with a non-mana
  // part (Tenacious Underdog's Blitz: "{2}{B}{B}, Pay 2 life") shows that part,
  // exactly as the engine provides it; an option without one shows none.
  it("renders the engine-provided non-mana part of an option's cost", () => {
    const underdog = gameObjectFactory
      .creature()
      .inGraveyard()
      .withId(42)
      .named("Tenacious Underdog")
      .withCost(["Black"])
      .build();
    const waitingFor: WaitingFor = {
      type: "CastingVariantChoice",
      data: {
        player: 0,
        object_id: underdog.id,
        card_id: underdog.card_id,
        options: [
          {
            variant: { type: "Escape" },
            face: "Current",
            mana_cost: { type: "Cost", shards: ["Black"], generic: 1 },
          },
          {
            variant: { type: "Blitz" },
            face: "Current",
            mana_cost: { type: "Cost", shards: ["Black", "Black"], generic: 2 },
            additional_cost: { type: "PayLife", amount: { type: "Fixed", value: 2 } },
          },
        ],
      },
    };
    setGameStoreForTest({
      gameState: gameStateFactory
        .withPlayers(0, 1)
        .withObjects(underdog)
        .waitingFor(waitingFor)
        .build(),
      legalActions: [
        { type: "ChooseCastingVariant", data: { index: 0 } },
        { type: "ChooseCastingVariant", data: { index: 1 } },
      ],
    });

    render(<CastingVariantModal />);

    const blitz = screen.getByRole("button", { name: /Cast with Blitz/ });
    expect(blitz).toHaveTextContent("+ Pay 2 life");
    const escape = screen.getByRole("button", { name: /Escape/ });
    expect(escape).not.toHaveTextContent("Pay");
  });

  // CR 702.103a + CR 601.2h: Detective's Phoenix's Bestow option shows its
  // "Collect evidence 6" with the engine-provided threshold.
  it("renders the engine-provided collect-evidence threshold of a bestow option", () => {
    const phoenix = gameObjectFactory
      .creature()
      .inGraveyard()
      .withId(43)
      .named("Detective's Phoenix")
      .withCost(["Red"], 2)
      .build();
    const waitingFor: WaitingFor = {
      type: "CastingVariantChoice",
      data: {
        player: 0,
        object_id: phoenix.id,
        card_id: phoenix.card_id,
        options: [
          {
            variant: { type: "Escape" },
            face: "Current",
            mana_cost: { type: "Cost", shards: ["Red"], generic: 2 },
          },
          {
            variant: { type: "Bestow" },
            face: "Current",
            mana_cost: { type: "Cost", shards: ["Red"], generic: 0 },
            additional_cost: { type: "CollectEvidence", amount: 6 },
          },
        ],
      },
    };
    setGameStoreForTest({
      gameState: gameStateFactory
        .withPlayers(0, 1)
        .withObjects(phoenix)
        .waitingFor(waitingFor)
        .build(),
      legalActions: [
        { type: "ChooseCastingVariant", data: { index: 0 } },
        { type: "ChooseCastingVariant", data: { index: 1 } },
      ],
    });

    render(<CastingVariantModal />);

    expect(screen.getByRole("button", { name: /Cast with Bestow/ })).toHaveTextContent(
      "+ Collect evidence 6",
    );
  });
});
