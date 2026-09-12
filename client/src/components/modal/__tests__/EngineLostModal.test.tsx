import "fake-indexeddb/auto";

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { notifyEngineLost, notifyEngineSlow } from "../../../game/engineRecovery";
import type { DownloadResult } from "../../../services/fileDownload";
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

const { exportGameStateDebugZip } = vi.hoisted(() => ({ exportGameStateDebugZip: vi.fn() }));
vi.mock("../../../services/gameStateExport", () => ({ exportGameStateDebugZip }));

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

  it.each<[string, DownloadResult, string, string]>([
    [
      "a failed download is not reported as an export",
      { kind: "failed", filename: "game-state.zip" },
      "Export failed",
      "Exported",
    ],
    [
      // The button has two states, so a download still running under the shell
      // reads as exported; only an outright failure must not.
      "a shell that never reported still reads as exported",
      { kind: "requested", filename: "game-state.zip" },
      "Exported",
      "Export failed",
    ],
  ])("%s", async (_name, result, shown, hidden) => {
    exportGameStateDebugZip.mockResolvedValue(result);
    setGameStoreForTest({ gameId: GAME_ID, gameMode: "ai" });
    renderModal();

    act(() => notifyEngineLost("submitAction"));
    fireEvent.click(screen.getByRole("button", { name: "Export client snapshot" }));

    // Gate on the commit, not on the mock: awaiting the call alone would read a
    // DOM the promise continuation has not updated yet, and pass either way.
    expect(await screen.findByRole("button", { name: shown })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: hidden })).not.toBeInTheDocument();
  });

  it("refuses a second export while the first is still in flight", async () => {
    let settle!: (result: DownloadResult) => void;
    exportGameStateDebugZip.mockReturnValue(
      new Promise<DownloadResult>((resolve) => {
        settle = resolve;
      }),
    );
    exportGameStateDebugZip.mockClear();
    setGameStoreForTest({ gameId: GAME_ID, gameMode: "ai" });
    renderModal();

    act(() => notifyEngineLost("submitAction"));
    const exportButton = screen.getByRole("button", { name: "Export client snapshot" });
    fireEvent.click(exportButton);

    // Gate on the committed disabled state, not on the mock having been called.
    await waitFor(() => expect(exportButton).toBeDisabled());
    fireEvent.click(exportButton);
    expect(exportGameStateDebugZip).toHaveBeenCalledTimes(1);

    settle({ kind: "requested", filename: "game-state.zip" });
    expect(await screen.findByRole("button", { name: "Exported" })).toBeEnabled();
  });
});
