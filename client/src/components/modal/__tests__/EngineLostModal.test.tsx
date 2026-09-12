import "fake-indexeddb/auto";

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { notifyEngineLost, notifyEngineSlow } from "../../../game/engineRecovery";
import {
  clearGame,
  loadActiveGame,
  loadGame,
  saveActiveGame,
  saveGame,
  useGameStore,
} from "../../../stores/gameStore";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers";
import { EngineLostModal } from "../EngineLostModal";

const GAME_ID = "resumable-engine-loss";
const activeGame = {
  id: GAME_ID,
  mode: "ai" as const,
  difficulty: "medium",
};

function renderModal() {
  render(
    <MemoryRouter initialEntries={["/game/test-game"]}>
      <EngineLostModal />
      <Routes>
        <Route path="/game/:id" element={<div>Game route</div>} />
        <Route path="/" element={<div>Main menu route</div>} />
      </Routes>
    </MemoryRouter>,
  );
}

afterEach(async () => {
  cleanup();
  useGameStore.setState({ gameId: null, gameMode: null, gameState: null });
  await clearGame(GAME_ID);
});

describe("EngineLostModal", () => {
  it.each([
    ["a crash", "submitAction-retry-panic", "panicked at engine.rs:1: broken"],
    ["a lost connection", "submitAction", undefined],
    ["a terminal timeout", "submitAction-timeout", undefined],
  ])(
    "dismisses %s and navigates to the main menu",
    async (_presentation, reason, panic) => {
      const { gameState } = setGameStoreForTest({
        gameId: GAME_ID,
        gameMode: "ai",
      });
      await saveGame(GAME_ID, gameState);
      saveActiveGame(activeGame);
      renderModal();

      act(() => notifyEngineLost(reason, panic));
      fireEvent.click(screen.getByRole("button", { name: "Main Menu" }));

      expect(screen.getByText("Main menu route")).toBeInTheDocument();
      expect(screen.queryByText("Main Menu")).not.toBeInTheDocument();
      expect(useGameStore.getState()).toMatchObject({
        gameId: GAME_ID,
        gameMode: "ai",
        gameState,
      });
      expect(await loadGame(GAME_ID)).toEqual(gameState);
      expect(loadActiveGame()).toEqual(activeGame);
    },
  );

  it("does not offer the main menu while a slow request can still complete", () => {
    renderModal();

    act(() => notifyEngineSlow("submitAction-slow"));

    expect(
      screen.getByRole("button", { name: "Continue waiting" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Main Menu" })).not.toBeInTheDocument();
  });
});
