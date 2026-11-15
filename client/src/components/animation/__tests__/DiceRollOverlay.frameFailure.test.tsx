import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

// The real Coin3D / Dice3D mount fine; only a later animation frame fails, as a
// lost WebGL context would. That throw happens outside React's render.
const renderFrame = vi.hoisted(() => vi.fn());
vi.mock("three", async (importOriginal) => {
  const actual = await importOriginal<typeof import("three")>();
  class FailingFrameRenderer {
    domElement = document.createElement("canvas");
    setSize() {}
    setPixelRatio() {}
    render() {
      renderFrame();
      throw new Error("WebGL context lost");
    }
    dispose() {}
  }
  return { ...actual, WebGLRenderer: FailingFrameRenderer };
});

const reportBoundaryError = vi.hoisted(() => vi.fn());
vi.mock("../../../services/telemetryEvents", () => ({ reportBoundaryError }));

import { DiceRollOverlay } from "../DiceRollOverlay";
import { useUiStore } from "../../../stores/uiStore";
import type { DiceRollPayload } from "../../../stores/uiStore";

let frames: FrameRequestCallback[] = [];

/** A permissive 2D/WebGL context: every method is a no-op returning another stub,
 *  so the face textures draw and the WebGL probe passes. */
const stubContext: unknown = new Proxy(function () {}, {
  get: () => stubContext,
  apply: () => stubContext,
});

async function showRoll(diceRoll: DiceRollPayload) {
  await act(async () => {
    useUiStore.setState({ diceRoll });
  });
  // Resolve the real lazy chunk and let the renderer's mount effect run.
  await waitFor(() => expect(frames.length).toBeGreaterThan(0));
}

async function runPendingFrames() {
  await act(async () => {
    const pending = frames;
    frames = [];
    for (const frame of pending) frame(performance.now());
  });
}

beforeEach(() => {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(stubContext as never);
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.stubGlobal("requestAnimationFrame", (frame: FrameRequestCallback) => {
    frames.push(frame);
    return frames.length;
  });
  vi.stubGlobal("cancelAnimationFrame", () => {});
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  frames = [];
  renderFrame.mockClear();
  reportBoundaryError.mockClear();
  useUiStore.setState({ diceRoll: null, diceRollQueue: [] });
});

it("falls back to the static coin result when a coin animation frame throws", async () => {
  render(<DiceRollOverlay />);
  await showRoll({ kind: "coin", playerId: 0, won: true } as DiceRollPayload);
  expect(renderFrame).not.toHaveBeenCalled();

  await runPendingFrames();

  expect(renderFrame).toHaveBeenCalledTimes(1);
  expect(frames).toHaveLength(0);
  expect(reportBoundaryError).toHaveBeenCalledWith(
    expect.objectContaining({ message: "WebGL context lost" }),
  );
  expect(screen.getByText(/you won the flip/i)).toBeInTheDocument();
  expect(screen.getAllByText(/heads/i).length).toBeGreaterThan(0);
});

it("falls back to the static die result when a die animation frame throws", async () => {
  render(<DiceRollOverlay />);
  await showRoll({
    kind: "die", context: "ability", sides: 20, rolls: [{ playerId: 0, value: 17 }],
  } as DiceRollPayload);

  await runPendingFrames();

  expect(renderFrame).toHaveBeenCalledTimes(1);
  expect(frames).toHaveLength(0);
  expect(reportBoundaryError).toHaveBeenCalledWith(
    expect.objectContaining({ message: "WebGL context lost" }),
  );
  expect(screen.getAllByText("17").length).toBeGreaterThan(0);
});
