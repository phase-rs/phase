import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  castOfferWaitingForFactory,
  gameStateFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { AdventureCastModal } from "../AdventureCastModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

const creatureAction = {
  type: "ChooseAdventureFace" as const,
  data: { creature: true },
};
const adventureAction = {
  type: "ChooseAdventureFace" as const,
  data: { creature: false },
};

function adventureState(player = 0) {
  const hildibrand = gameObjectFactory
    .creature(2, 2)
    .inHand()
    .withId(157)
    .named("Hildibrand Manderville")
    .build();

  return gameStateFactory
    .withPlayers(0, 1)
    .withObjects(hildibrand)
    .waitingFor(
      castOfferWaitingForFactory
        .forPlayer(player)
        .adventure(hildibrand.id, hildibrand.card_id)
        .build(),
    )
    .build();
}

describe("AdventureCastModal", () => {
  afterEach(() => {
    cleanup();
    dispatchMock.mockReset();
  });

  it("renders and dispatches only the engine-authorized Adventure face", () => {
    const gameState = adventureState();
    setGameStoreForTest({ gameState, legalActions: [adventureAction] });

    render(<AdventureCastModal />);

    expect(
      screen.queryByRole("button", { name: /^Cast Hildibrand Manderville \(Creature\)$/ }),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^Cast Adventure \(Adventure\)$/ }));
    expect(dispatchMock).toHaveBeenCalledWith(adventureAction);
  });

  it("renders both engine-authorized faces and preserves their exact actions", () => {
    const gameState = adventureState();
    setGameStoreForTest({ gameState, legalActions: [creatureAction, adventureAction] });

    render(<AdventureCastModal />);

    fireEvent.click(
      screen.getByRole("button", { name: /^Cast Hildibrand Manderville \(Creature\)$/ }),
    );
    fireEvent.click(screen.getByRole("button", { name: /^Cast Adventure \(Adventure\)$/ }));

    expect(dispatchMock).toHaveBeenNthCalledWith(1, creatureAction);
    expect(dispatchMock).toHaveBeenNthCalledWith(2, adventureAction);
  });

  it("does not render for another player's CastOffer", () => {
    const gameState = adventureState(1);
    setGameStoreForTest({ gameState, legalActions: [adventureAction] });

    render(<AdventureCastModal />);

    expect(screen.queryByRole("heading", { name: "Choose a Face" })).not.toBeInTheDocument();
  });
});
