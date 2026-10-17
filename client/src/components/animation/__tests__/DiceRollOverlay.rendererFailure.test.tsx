import { useState } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DiceRollOverlay } from "../DiceRollOverlay";
import { useUiStore } from "../../../stores/uiStore";
import type { DiceRollPayload } from "../../../stores/uiStore";

const renderer = vi.hoisted(() => vi.fn(() => { throw new Error("WebGL renderer failed"); }));
const coinRenderer = vi.hoisted(() => vi.fn(() => { throw new Error("WebGL coin failed"); }));
const reportBoundaryError = vi.hoisted(() => vi.fn());

vi.mock("../dice3d/loaders", () => ({
  loadDice3DModule: () => Promise.resolve({ Dice3D: renderer }),
  loadCoin3DModule: () => Promise.resolve({ Coin3D: coinRenderer }),
}));
vi.mock("../../../services/telemetryEvents", () => ({ reportBoundaryError }));

function GameParent() {
  const [turn, setTurn] = useState(1);
  return <><button onClick={() => setTurn((value) => value + 1)}>Parent turn {turn}</button><DiceRollOverlay /></>;
}

async function showRoll(diceRoll: DiceRollPayload) {
  await act(async () => {
    useUiStore.setState({ diceRoll });
    await Promise.resolve();
  });
}

beforeEach(() => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({} as never);
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  renderer.mockClear();
  coinRenderer.mockClear();
  reportBoundaryError.mockClear();
  useUiStore.setState({ diceRoll: null, diceRollQueue: [] });
});

it("contains a throwing renderer, preserves the mounted parent state and lets Escape dismiss the overlay", async () => {
  render(<GameParent />);
  fireEvent.click(screen.getByRole("button", { name: "Parent turn 1" }));
  const parent = screen.getByRole("button", { name: "Parent turn 2" });
  await showRoll({
    kind: "die", context: "ability", sides: 20, rolls: [{ playerId: 0, value: 17 }],
  } as DiceRollPayload);
  expect(renderer).toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Parent turn 2" })).toBe(parent);
  expect(useUiStore.getState().diceRoll).not.toBeNull();
  fireEvent.keyDown(window, { key: "Escape" });
  expect(useUiStore.getState().diceRoll).toBeNull();
  expect(screen.getByRole("button", { name: "Parent turn 2" })).toBe(parent);
});

it("reports a throwing renderer to telemetry so a real WebGL crash is not swallowed", async () => {
  render(<DiceRollOverlay />);
  await showRoll({
    kind: "die", context: "ability", sides: 20, rolls: [{ playerId: 0, value: 17 }],
  } as DiceRollPayload);
  expect(reportBoundaryError).toHaveBeenCalledTimes(1);
  expect(reportBoundaryError).toHaveBeenCalledWith(
    expect.objectContaining({ message: "WebGL renderer failed" }),
  );
});

it("keeps the starting-player result visible after the renderer throws", async () => {
  render(<DiceRollOverlay />);
  await showRoll({
    kind: "die", context: "startingPlayer", sides: 20, winner: 0,
    rounds: [[{ playerId: 0, value: 17 }, { playerId: 1, value: 4 }]],
    rolls: [{ playerId: 0, value: 17 }, { playerId: 1, value: 4 }],
    turnOrder: [
      { slot_index: 0, player: 0, turn_number: 1, is_viewer: true, is_starting_player: true },
      { slot_index: 1, player: 1, turn_number: 2, is_viewer: false, is_starting_player: false },
    ],
  } as unknown as DiceRollPayload);
  expect(renderer).toHaveBeenCalled();
  expect(reportBoundaryError).toHaveBeenCalled();
  // The caption, the rolled values and the turn order all come from the static fallback.
  expect(screen.getByText(/you play first/i)).toBeInTheDocument();
  expect(screen.getAllByText("17").length).toBeGreaterThan(0);
  expect(screen.getAllByText("4").length).toBeGreaterThan(0);
  expect(screen.getByText(/turn order/i)).toBeInTheDocument();
});

it("keeps the coin outcome visible after the coin renderer throws", async () => {
  render(<DiceRollOverlay />);
  await showRoll({ kind: "coin", playerId: 0, won: true } as DiceRollPayload);
  expect(coinRenderer).toHaveBeenCalled();
  expect(reportBoundaryError).toHaveBeenCalledWith(
    expect.objectContaining({ message: "WebGL coin failed" }),
  );
  expect(screen.getByText(/you won the flip/i)).toBeInTheDocument();
  expect(screen.getAllByText(/heads/i).length).toBeGreaterThan(0);
});
