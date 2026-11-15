import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { ModalFaceModal } from "../ModalFaceModal.tsx";

const frontAction = {
  type: "ChooseModalFace" as const,
  data: { back_face: false },
};
const backAction = {
  type: "ChooseModalFace" as const,
  data: { back_face: true },
};
const cancelAction = { type: "CancelCast" as const };

function modalFaceState(player = 0) {
  const front = gameObjectFactory
    .sorcery()
    .inExile()
    .withId(157)
    .named("Front Spell")
    .build();
  const card = {
    ...front,
    back_face: {
      name: "Back Spell",
      power: null,
      toughness: null,
      mana_cost: { type: "NoCost" },
      card_types: { supertypes: [], core_types: ["Instant"], subtypes: [] },
      keywords: [],
      abilities: [],
      color: [],
    } satisfies NonNullable<GameObject["back_face"]>,
  };

  return gameStateFactory
    .withPlayers(0, 1)
    .withObjects(card)
    .waitingFor({
      type: "ModalFaceChoice",
      data: { player, object_id: card.id, card_id: card.card_id },
    })
    .build();
}

describe("ModalFaceModal", () => {
  afterEach(cleanup);

  it("renders and dispatches only the engine-issued back-face action", () => {
    const { dispatch } = setGameStoreForTest({
      gameState: modalFaceState(),
      legalActions: [backAction],
    });

    render(<ModalFaceModal />);

    expect(screen.queryByRole("button", { name: /^Cast Front Spell/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^Cast Back Spell/ }));
    expect(dispatch).toHaveBeenCalledWith(backAction);
  });

  it("preserves each engine-issued face action", () => {
    const { dispatch } = setGameStoreForTest({
      gameState: modalFaceState(),
      legalActions: [frontAction, backAction, cancelAction],
    });

    render(<ModalFaceModal />);

    fireEvent.click(screen.getByRole("button", { name: /^Cast Front Spell/ }));
    fireEvent.click(screen.getByRole("button", { name: /^Cast Back Spell/ }));
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(dispatch).toHaveBeenNthCalledWith(1, frontAction);
    expect(dispatch).toHaveBeenNthCalledWith(2, backAction);
    expect(dispatch).toHaveBeenNthCalledWith(3, cancelAction);
  });
});
