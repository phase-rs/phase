import { Component, type ReactNode } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Stand-ins for a stale deploy: the old hashed 3D chunk is gone, so Vite's
// preload-error handling (`preventDefault`) makes `import()` resolve `undefined`
// (Dice3D) or a module without the expected export (Coin3D).
vi.mock("../dice3d/loaders", () => ({
  loadDice3DModule: () => Promise.resolve(undefined),
  loadCoin3DModule: () => Promise.resolve({ Coin3D: undefined }),
}));

// A missing chunk is absorbed by `safeLazy`; the local render boundary is only
// the backstop for a throw, and reports to telemetry when it fires.
const reportBoundaryError = vi.hoisted(() => vi.fn());
vi.mock("../../../services/telemetryEvents", () => ({ reportBoundaryError }));

import { DiceRollOverlay } from "../DiceRollOverlay";
import { useUiStore } from "../../../stores/uiStore";
import type { DiceRollPayload } from "../../../stores/uiStore";

const IN_GAME_ROLL: DiceRollPayload = {
  kind: "die",
  context: "ability",
  sides: 20,
  rolls: [{ playerId: 0, value: 17 }],
} as DiceRollPayload;

const COIN_FLIP: DiceRollPayload = {
  kind: "coin",
  playerId: 0,
  won: true,
} as DiceRollPayload;

/** Stand-in for the app-level ErrorBoundary: records anything that escapes the overlay. */
class EscapeProbe extends Component<{ children: ReactNode; onEscape: (e: Error) => void }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  componentDidCatch(error: Error) {
    this.props.onEscape(error);
  }
  render() {
    return this.state.failed ? <div data-testid="app-error-boundary" /> : this.props.children;
  }
}

/** Resolve the mocked lazy module and its React effects without wall-clock sleeps. */
async function flushLazyImports(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

describe("DiceRollOverlay with an unavailable 3D chunk", () => {
  beforeEach(() => {
    // Make the WebGL probe succeed so the overlay takes the lazy 3D path.
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({} as never);
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.spyOn(console, "warn").mockImplementation(() => {});
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    reportBoundaryError.mockClear();
    vi.useRealTimers();
    useUiStore.setState({ diceRoll: null, diceRollQueue: [] });
  });

  it("(d) renders the static die face instead of throwing when the Dice3D export is missing", async () => {
    const onEscape = vi.fn();
    render(
      <EscapeProbe onEscape={onEscape}>
        <DiceRollOverlay />
      </EscapeProbe>,
    );

    await act(async () => {
      useUiStore.setState({ diceRoll: IN_GAME_ROLL });
    });
    await flushLazyImports();

    expect(onEscape).not.toHaveBeenCalled();
    expect(screen.queryByTestId("app-error-boundary")).toBeNull();
    // The Suspense fallback already shows "17"; only the settled static face
    // also pops the result badge, so two matches prove the safeLazy fallback
    // replaced the pending placeholder and reported itself settled.
    await waitFor(() => expect(screen.getAllByText("17")).toHaveLength(2));
    // ...and it was safeLazy's fallback, not the render boundary catching a throw.
    expect(reportBoundaryError).not.toHaveBeenCalled();
  });

  it("(d) renders a static coin face when the Coin3D export is missing", async () => {
    const onEscape = vi.fn();
    render(
      <EscapeProbe onEscape={onEscape}>
        <DiceRollOverlay />
      </EscapeProbe>,
    );

    await act(async () => {
      useUiStore.setState({ diceRoll: COIN_FLIP });
    });
    await flushLazyImports();

    expect(onEscape).not.toHaveBeenCalled();
    expect(screen.queryByTestId("app-error-boundary")).toBeNull();
    expect(screen.getAllByRole("status").length).toBeGreaterThan(0);
    // The static face is the localized "heads" label (a win maps to heads).
    expect(screen.getAllByText(/heads/i).length).toBeGreaterThan(0);
    expect(reportBoundaryError).not.toHaveBeenCalled();
  });

  it("advances a missing-chunk contest through tied rounds and remains dismissible without unmounting its game parent", async () => {
    vi.useFakeTimers();
    const onEscape = vi.fn();
    render(<EscapeProbe onEscape={onEscape}><div data-testid="game-parent" /><DiceRollOverlay /></EscapeProbe>);
    const parent = screen.getByTestId("game-parent");
    await act(async () => {
      useUiStore.setState({ diceRoll: {
        kind: "die", context: "startingPlayer", sides: 20, winner: 0,
        rounds: [
          [{ playerId: 0, value: 20 }, { playerId: 1, value: 20 }],
          [{ playerId: 0, value: 7 }, { playerId: 1, value: 3 }],
        ],
        rolls: [{ playerId: 0, value: 7 }, { playerId: 1, value: 3 }],
      } });
    });
    await flushLazyImports();
    expect(screen.queryByText(/you play first/i)).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(1100); });
    expect(screen.getByText(/you play first/i)).toBeInTheDocument();
    expect(screen.getByTestId("game-parent")).toBe(parent);
    expect(onEscape).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(useUiStore.getState().diceRoll).toBeNull();
    expect(screen.getByTestId("game-parent")).toBe(parent);
  });

  it("(d) logs the degraded 3D chunk exactly once, however many dice render", async () => {
    // `React.lazy` caches its settled module for the life of the module graph,
    // so earlier tests would already have consumed (and warned for) the missing
    // chunk. A fresh graph makes the load happen inside this test.
    vi.resetModules();
    const { DiceRollOverlay: FreshOverlay } = await import("../DiceRollOverlay");
    const { useUiStore: freshUiStore } = await import("../../../stores/uiStore");
    const { resetWarnOnce } = await import("../dice3d/safeLazy");
    resetWarnOnce();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    render(<FreshOverlay />);

    await act(async () => {
      freshUiStore.setState({
        diceRoll: {
          ...IN_GAME_ROLL,
          rolls: [
            { playerId: 0, value: 3 },
            { playerId: 0, value: 9 },
          ],
        } as DiceRollPayload,
      });
    });
    await flushLazyImports();

    await waitFor(() => expect(screen.getAllByText("3")).toHaveLength(2));
    expect(screen.getAllByText("9")).toHaveLength(2);
    const diceWarnings = warn.mock.calls.filter((call) => String(call[0]).includes("Dice3D"));
    expect(diceWarnings).toHaveLength(1);
    expect(reportBoundaryError).not.toHaveBeenCalled();
    freshUiStore.setState({ diceRoll: null, diceRollQueue: [] });
  });
});
