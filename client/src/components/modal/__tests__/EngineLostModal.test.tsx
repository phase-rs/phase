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

const { copyText } = vi.hoisted(() => ({ copyText: vi.fn() }));
vi.mock("../../../services/copyText", () => ({ copyText }));

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
  vi.useRealTimers();
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
      // This is the recovery path: a snapshot nothing confirmed must not send
      // the player looking for a file that may not exist.
      "a shell that never confirmed the export does not read as exported",
      { kind: "requested", filename: "game-state.zip" },
      "Export unconfirmed",
      "Exported",
    ],
    [
      "a confirmed download reads as exported",
      { kind: "saved", filename: "game-state.zip", path: "~/Downloads/game-state.zip" },
      "Exported",
      "Export unconfirmed",
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
    expect(await screen.findByRole("button", { name: "Export unconfirmed" })).toBeEnabled();
  });

  // Fake timers only here: the point is what happens when the earlier export's
  // reset comes due, which real timers would buy at two seconds of wall clock.
  it("keeps a later export's result when the earlier export's reset comes due", async () => {
    vi.useFakeTimers();
    setGameStoreForTest({ gameId: GAME_ID, gameMode: "ai" });
    renderModal();

    act(() => notifyEngineLost("submitAction"));
    const exportButton = screen.getByRole("button", { name: "Export client snapshot" });
    const exportOnce = async (result: DownloadResult) => {
      const settled = Promise.resolve(result);
      exportGameStateDebugZip.mockReturnValueOnce(settled);
      fireEvent.click(exportButton);
      // Gate on the commit, not on the mock: the result lands in a promise
      // continuation React has not rendered yet.
      await act(async () => {
        await settled;
      });
    };

    await exportOnce({
      kind: "saved",
      filename: "game-state.zip",
      path: "~/Downloads/game-state.zip",
    });
    expect(screen.getByRole("button", { name: "Exported" })).toBeInTheDocument();

    act(() => vi.advanceTimersByTime(1_500));
    await exportOnce({ kind: "failed", filename: "game-state.zip" });
    expect(screen.getByRole("button", { name: "Export failed" })).toBeInTheDocument();

    // The first export's reset is due here and has no claim on this result.
    act(() => vi.advanceTimersByTime(600));
    expect(screen.getByRole("button", { name: "Export failed" })).toBeInTheDocument();

    // The later export's own reset still clears it.
    act(() => vi.advanceTimersByTime(1_500));
    expect(
      screen.getByRole("button", { name: "Export client snapshot" }),
    ).toBeInTheDocument();
  });

  // Same class, same idiom: the copy confirmation shares the two-second reset.
  it("keeps a later copy's confirmation when the earlier copy's reset comes due", async () => {
    vi.useFakeTimers();
    copyText.mockResolvedValue(true);
    renderModal();

    act(() => notifyEngineLost("submitAction-retry-panic", "panicked at engine.rs:1: broken"));
    const copyButton = screen.getByRole("button", { name: "Copy diagnostic" });
    const copyOnce = async () => {
      const settled = Promise.resolve(true);
      copyText.mockReturnValueOnce(settled);
      fireEvent.click(copyButton);
      await act(async () => {
        await settled;
      });
    };

    await copyOnce();
    expect(screen.getByRole("button", { name: "Copied!" })).toBeInTheDocument();

    act(() => vi.advanceTimersByTime(1_500));
    await copyOnce();

    // The first copy's reset is due here and has no claim on this confirmation.
    act(() => vi.advanceTimersByTime(600));
    expect(screen.getByRole("button", { name: "Copied!" })).toBeInTheDocument();

    // The later copy's own reset still clears it.
    act(() => vi.advanceTimersByTime(1_500));
    expect(screen.getByRole("button", { name: "Copy diagnostic" })).toBeInTheDocument();
  });

  // The export outlives the modal easily — under the shell it waits up to ten
  // seconds — and the unmount cleanup has already run by the time it settles,
  // so a reset timer armed after it has nothing left to clear it.
  it("arms no reset timer when the modal unmounts mid-export", async () => {
    vi.useFakeTimers();
    let settle!: (result: DownloadResult) => void;
    const pending = new Promise<DownloadResult>((resolve) => {
      settle = resolve;
    });
    exportGameStateDebugZip.mockReturnValueOnce(pending);
    setGameStoreForTest({ gameId: GAME_ID, gameMode: "ai" });
    renderModal();

    act(() => notifyEngineLost("submitAction"));
    const exportButton = screen.getByRole("button", { name: "Export client snapshot" });
    fireEvent.click(exportButton);
    // Gate on the committed disabled state: the export is only actually in
    // flight once React has rendered the click's state update.
    await act(async () => {});
    expect(exportButton).toBeDisabled();

    cleanup();
    settle({ kind: "saved", filename: "game-state.zip", path: "~/Downloads/game-state.zip" });
    await act(async () => {
      await pending;
    });

    expect(vi.getTimerCount()).toBe(0);
  });
});
