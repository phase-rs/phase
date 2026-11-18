import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

// Stand-ins for the WebGL renderers. Each records its `onSettle`, so a test
// lands a die or coin exactly when it chooses, as a real tumble that starts
// late (a slow chunk load or WebGL start-up) would.
const renderers = vi.hoisted(() => {
  /** Every die or coin rendered, by its settle callback, with the face it shows. */
  const mounted = new Map<() => void, string>();
  const landed = new Set<() => void>();
  function FakeRenderer(props: { result?: number; face?: string; onSettle?: () => void }) {
    if (props.onSettle) mounted.set(props.onSettle, String(props.result ?? props.face));
    return null;
  }
  return { mounted, landed, FakeRenderer };
});
vi.mock("../dice3d/loaders", () => ({
  loadDice3DModule: () => Promise.resolve({ Dice3D: renderers.FakeRenderer }),
  loadCoin3DModule: () => Promise.resolve({ Coin3D: renderers.FakeRenderer }),
}));

import { DiceRollOverlay } from "../DiceRollOverlay";
import {
  DICE_RESULT_HOLD_MS,
  DICE_RESULT_MIN_HOLD_MS,
  DICE_ROLL_LANDING_TIMEOUT_MS,
} from "../../../animation/types";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { useUiStore } from "../../../stores/uiStore";
import type { DiceRollPayload } from "../../../stores/uiStore";

/** Fresh payloads per test: the store tells rolls apart by identity. */
function lostCoinFlip(): DiceRollPayload {
  return { kind: "coin", playerId: 0, won: false, context: "ability" };
}

function twoDiceRoll(): DiceRollPayload {
  return {
    kind: "die",
    sides: 20,
    context: "ability",
    rolls: [{ playerId: 0, value: 3 }, { playerId: 0, value: 9 }],
  };
}

function tiedContest(): DiceRollPayload {
  return {
    kind: "die",
    sides: 20,
    context: "startingPlayer",
    winner: 0,
    rounds: [
      [{ playerId: 0, value: 20 }, { playerId: 1, value: 20 }],
      [{ playerId: 0, value: 7 }, { playerId: 1, value: 3 }],
    ],
    rolls: [{ playerId: 0, value: 7 }, { playerId: 1, value: 3 }],
  };
}

/** Show a roll through the store's production entry point, which arms its countdown. */
async function showRoll(payload: DiceRollPayload): Promise<void> {
  await act(async () => {
    useUiStore.getState().flashDiceRoll(payload);
  });
}

/** Land every rendered die or coin showing `face` that has not landed yet. */
async function land(face: string): Promise<void> {
  const settles = [...renderers.mounted]
    .filter(([settle, shown]) => shown === face && !renderers.landed.has(settle))
    .map(([settle]) => settle);
  // Reach guard: the renderer really is mounted and waiting to land.
  expect(settles.length).toBeGreaterThan(0);
  await act(async () => {
    for (const settle of settles) {
      renderers.landed.add(settle);
      settle();
    }
  });
}

async function advance(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeAll(async () => {
  // React.lazy resolves each 3D chunk once per module graph. Resolving both
  // here, on real timers, leaves each test's fake clock driving only the
  // overlay's own timers and the store's countdown, never a pending import.
  // The WebGL probe is cached on its first call, so it passes for every test.
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({} as never);
  render(<DiceRollOverlay />);
  const warmUps: [DiceRollPayload, string][] = [[lostCoinFlip(), "tails"], [twoDiceRoll(), "3"]];
  for (const [payload, face] of warmUps) {
    await act(async () => {
      useUiStore.setState({ diceRoll: payload });
    });
    await waitFor(() => expect([...renderers.mounted.values()]).toContain(face));
  }
  cleanup();
  useUiStore.getState().resetDiceRoll();
  renderers.mounted.clear();
});

afterAll(() => {
  vi.restoreAllMocks();
});

beforeEach(() => {
  vi.useFakeTimers();
  usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
});

afterEach(() => {
  cleanup();
  useUiStore.getState().resetDiceRoll();
  usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
  renderers.mounted.clear();
  renderers.landed.clear();
  vi.clearAllTimers();
  vi.useRealTimers();
});

describe("DiceRollOverlay reveals a roll's result when it lands", () => {
  it("keeps the coin's result colour, glow and banner back until it lands (#11003)", async () => {
    render(<DiceRollOverlay />);
    await showRoll(lostCoinFlip());

    expect(screen.getByText(/coin flip/i)).toBeInTheDocument();
    expect(screen.queryByText(/lost the flip/i)).toBeNull();
    expect(screen.queryByTestId("die-emphasis")).toBeNull();

    await land("tails");

    expect(screen.getByText(/you lost the flip/i)).toBeInTheDocument();
    expect(screen.getByTestId("die-emphasis")).toBeInTheDocument();
  });

  it("shows the banner at a fast speed and counts down from the landing (#8992)", async () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0.25 });
    render(<DiceRollOverlay />);
    const coin = lostCoinFlip();
    await showRoll(coin);

    // The coin lands a second after the show, well past the 600 ms a countdown
    // from the show allowed at this speed.
    await advance(1000);
    expect(useUiStore.getState().diceRoll).toBe(coin);
    expect(screen.queryByText(/lost the flip/i)).toBeNull();

    await land("tails");
    expect(screen.getByText(/you lost the flip/i)).toBeInTheDocument();

    await advance(DICE_RESULT_MIN_HOLD_MS - 1);
    expect(useUiStore.getState().diceRoll).toBe(coin);
    expect(screen.getByText(/you lost the flip/i)).toBeInTheDocument();
    await advance(1);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });

  it("shows the coin's static result when it never lands, then counts down", async () => {
    render(<DiceRollOverlay />);
    const coin = lostCoinFlip();
    await showRoll(coin);

    // The renderer never reports a landing (a chunk that never arrives).
    await advance(DICE_ROLL_LANDING_TIMEOUT_MS - 1);
    expect(screen.queryByText(/lost the flip/i)).toBeNull();
    await advance(1);
    expect(screen.getByText("Tails")).toBeInTheDocument();
    expect(screen.getByText(/you lost the flip/i)).toBeInTheDocument();
    expect(screen.getByTestId("die-emphasis")).toBeInTheDocument();

    await advance(DICE_RESULT_HOLD_MS - 1);
    expect(useUiStore.getState().diceRoll).toBe(coin);
    await advance(1);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });

  it("shows stalled dice as landed static faces, with their badges and the ignored die marked", async () => {
    render(<DiceRollOverlay />);
    const roll: DiceRollPayload = {
      kind: "die",
      sides: 20,
      context: "ability",
      rolls: [{ playerId: 0, value: 14 }, { playerId: 0, value: 4, ignored: true }],
    };
    await showRoll(roll);

    // The dice never report a landing.
    await advance(DICE_ROLL_LANDING_TIMEOUT_MS - 1);
    expect(screen.queryByText("14")).toBeNull();
    await advance(1);
    // Each value shows on its static face and on its landed badge.
    expect(screen.getAllByText("14")).toHaveLength(2);
    expect(screen.getAllByText("4")).toHaveLength(2);
    expect(screen.getByText(/ignored/i)).toBeInTheDocument();

    await advance(DICE_RESULT_HOLD_MS);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });

  it("starts an in-game roll's countdown only once its last die lands", async () => {
    render(<DiceRollOverlay />);
    const roll = twoDiceRoll();
    await showRoll(roll);

    // The dice start late, and one lands before the other.
    await advance(2000);
    await land("3");
    await advance(DICE_RESULT_HOLD_MS);
    expect(useUiStore.getState().diceRoll).toBe(roll);

    await land("9");
    await advance(DICE_RESULT_HOLD_MS - 1);
    expect(useUiStore.getState().diceRoll).toBe(roll);
    await advance(1);
    expect(useUiStore.getState().diceRoll).toBeNull();
  });

  it("holds a tied contest round for a beat scaled by the speed before the reroll", async () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0.5 });
    render(<DiceRollOverlay />);
    await showRoll(tiedContest());
    expect(screen.getByText(/rolling for first player/i)).toBeInTheDocument();

    await land("20");
    // 1100 ms at speed 1, so 550 ms at 0.5.
    await advance(549);
    expect(screen.queryByText(/reroll/i)).toBeNull();
    await advance(1);
    expect(screen.getByText(/reroll/i)).toBeInTheDocument();
  });
});
