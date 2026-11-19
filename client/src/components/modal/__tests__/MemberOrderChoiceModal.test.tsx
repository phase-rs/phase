import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameObjectFactory, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatch = vi.fn();
vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatch,
}));

afterEach(cleanup);

it("asks which permanent gets its replacement effects considered next", () => {
  const bear = gameObjectFactory.creature().onBattlefield().named("Grizzly Bears").withId(10).build();
  const vine = gameObjectFactory.creature().onBattlefield().named("Wall of Vines").withId(11).build();
  const waitingFor: WaitingFor = {
    type: "MemberOrderChoice",
    data: { player: 0, choices: [bear.id, vine.id], purpose: "DepartureReplacements" },
  };
  useMultiplayerStore.setState({ activePlayerId: 0 });
  useGameStore.setState({
    gameMode: "online",
    waitingFor,
    gameState: buildGameState({
      waiting_for: waitingFor,
      objects: buildObjectMap(bear, vine),
      battlefield: [bear.id, vine.id],
    }),
  });

  render(<CardChoiceModal />);
  expect(screen.getByText("Choose which permanent’s replacement effects to consider next")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: /Wall of Vines/ }));
  expect(dispatch).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [vine.id] } });
});
