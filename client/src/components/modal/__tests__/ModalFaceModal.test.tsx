import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  gameStateFactory,
  modalFaceChoiceWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { ModalFaceModal } from "../ModalFaceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

const frontAction = {
  type: "ChooseModalFace" as const,
  data: { back_face: false },
};
const backAction = {
  type: "ChooseModalFace" as const,
  data: { back_face: true },
};

/**
 * "Tony Stark // The Invincible Iron Man" — a modal DFC whose two faces
 * are separately payable ({1}{U} front, {4}{U}{R} back), so the engine can
 * legalize either one, both, or (mid-cast, never from a UI-reachable state)
 * neither.
 */
function modalFaceState(player = 0) {
  const tonyStark = gameObjectFactory
    .creature(1, 3)
    .legendary()
    .inHand()
    .withId(42)
    .named("Tony Stark")
    .withCost(["Blue"], 1)
    .params({
      card_types: { subtypes: ["Human", "Artificer", "Hero"] },
      back_face: {
        name: "The Invincible Iron Man",
        power: 5,
        toughness: 5,
        card_types: {
          supertypes: ["Legendary"],
          core_types: ["Artifact", "Creature"],
          subtypes: ["Human", "Hero"],
        },
        mana_cost: { type: "Cost", shards: ["Blue", "Red"], generic: 4 },
        keywords: [],
        abilities: [],
        color: [],
        layout_kind: "Modal",
      },
    })
    .build();

  return gameStateFactory
    .withPlayers(0, 1)
    .withObjects(tonyStark)
    .waitingFor(
      modalFaceChoiceWaitingForFactory
        .forPlayer(player)
        .forObject(tonyStark.id, tonyStark.card_id)
        .build(),
    )
    .build();
}

const frontButton = () => screen.queryByRole("button", { name: /Cast Tony Stark/ });
const backButton = () =>
  screen.queryByRole("button", { name: /Cast The Invincible Iron Man/ });

describe("ModalFaceModal", () => {
  afterEach(() => {
    cleanup();
    dispatchMock.mockReset();
  });

  it("renders only the front face when the engine legalized only the front face", () => {
    setGameStoreForTest({ gameState: modalFaceState(), legalActions: [frontAction] });

    render(<ModalFaceModal />);

    expect(backButton()).not.toBeInTheDocument();

    fireEvent.click(frontButton()!);
    expect(dispatchMock).toHaveBeenCalledWith(frontAction);
    expect(dispatchMock).toHaveBeenCalledTimes(1);
  });

  it("renders both faces and preserves their exact actions when both are legal", () => {
    setGameStoreForTest({
      gameState: modalFaceState(),
      legalActions: [frontAction, backAction],
    });

    render(<ModalFaceModal />);

    fireEvent.click(frontButton()!);
    fireEvent.click(backButton()!);

    expect(dispatchMock).toHaveBeenNthCalledWith(1, frontAction);
    expect(dispatchMock).toHaveBeenNthCalledWith(2, backAction);
  });

  it("renders only the back face when the engine legalized only the back face", () => {
    setGameStoreForTest({ gameState: modalFaceState(), legalActions: [backAction] });

    render(<ModalFaceModal />);

    expect(frontButton()).not.toBeInTheDocument();

    fireEvent.click(backButton()!);
    expect(dispatchMock).toHaveBeenCalledWith(backAction);
    expect(dispatchMock).toHaveBeenCalledTimes(1);
  });

  it("offers no face when the engine legalized neither", () => {
    setGameStoreForTest({ gameState: modalFaceState(), legalActions: [] });

    render(<ModalFaceModal />);

    // Reach guard: the dialog itself rendered, so the two absent buttons are
    // absent because no `ChooseModalFace` was legal — not because the overlay
    // bailed out before reaching the face list.
    expect(screen.getByRole("heading", { name: "Choose a Face" })).toBeInTheDocument();
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });

  it("does not render for another player's ModalFaceChoice", () => {
    setGameStoreForTest({
      gameState: modalFaceState(1),
      legalActions: [frontAction, backAction],
    });

    render(<ModalFaceModal />);

    expect(screen.queryByRole("heading", { name: "Choose a Face" })).not.toBeInTheDocument();
  });
});
