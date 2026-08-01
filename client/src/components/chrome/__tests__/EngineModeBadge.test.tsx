import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  canAttemptNativeEngine: vi.fn(() => true),
  gameState: {
    engineMode: null as "native" | "wasm" | null,
    nativeEngineFallbackReason: null as string | null,
  },
  preferences: { nativeEngineEnabled: true },
}));

vi.mock("../../../services/nativeEngine", () => ({
  canAttemptNativeEngine: mocks.canAttemptNativeEngine,
}));

vi.mock("../../../stores/gameStore", () => ({
  useGameStore: (selector: (state: typeof mocks.gameState) => unknown) => selector(mocks.gameState),
}));

vi.mock("../../../stores/preferencesStore", () => ({
  usePreferencesStore: (selector: (state: typeof mocks.preferences) => unknown) =>
    selector(mocks.preferences),
}));

import { EngineModeBadge } from "../EngineModeBadge";

describe("EngineModeBadge", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.canAttemptNativeEngine.mockReturnValue(true);
    mocks.gameState.engineMode = null;
    mocks.gameState.nativeEngineFallbackReason = null;
    mocks.preferences.nativeEngineEnabled = true;
  });

  afterEach(cleanup);

  it("names the engine actually driving a native game", () => {
    mocks.gameState.engineMode = "native";

    render(<EngineModeBadge />);

    expect(screen.getByText("Native engine")).toBeInTheDocument();
  });

  it("explains a silent fallback to the in-browser engine", () => {
    mocks.gameState.engineMode = "wasm";
    mocks.gameState.nativeEngineFallbackReason = "native_engine_unavailable";

    render(<EngineModeBadge />);

    expect(screen.getByText("In-browser engine")).toHaveAttribute(
      "title",
      "Native engine unavailable — this game is running in-browser.",
    );
  });

  it("distinguishes a version mismatch from an unavailable engine", () => {
    mocks.gameState.engineMode = "wasm";
    mocks.gameState.nativeEngineFallbackReason = "server_version_mismatch";

    render(<EngineModeBadge />);

    expect(screen.getByText("In-browser engine")).toHaveAttribute(
      "title",
      "Native engine version mismatch — this game is running in-browser.",
    );
  });

  it("does not blame the engine for a game that never asked for it", () => {
    // Draft matches, a chosen first player, and resuming a WASM save all set
    // "wasm" with no reason. Claiming unavailability there would be false on a
    // machine whose native engine works fine.
    mocks.gameState.engineMode = "wasm";
    mocks.gameState.nativeEngineFallbackReason = null;

    render(<EngineModeBadge />);

    expect(screen.getByText("In-browser engine")).toHaveAttribute(
      "title",
      "This game is running in-browser — the native engine wasn't used for it.",
    );
  });

  it("still reports an unrecognized fallback reason as a failure", () => {
    // Every non-null reason means an attempt failed. A reason this build has
    // not heard of must not be downgraded to "never asked for it".
    mocks.gameState.engineMode = "wasm";
    mocks.gameState.nativeEngineFallbackReason = "some_future_reason";

    render(<EngineModeBadge />);

    expect(screen.getByText("In-browser engine")).toHaveAttribute(
      "title",
      "Native engine unavailable — this game is running in-browser.",
    );
  });

  it("stays out of games that never had a native option", () => {
    // Web builds and non-AI games are in-browser by definition, so the badge
    // would be permanent noise rather than a signal.
    mocks.gameState.engineMode = "wasm";
    mocks.canAttemptNativeEngine.mockReturnValue(false);

    const { container } = render(<EngineModeBadge />);

    expect(container).toBeEmptyDOMElement();
  });

  it("stays out of games with no engine selection at all", () => {
    mocks.gameState.engineMode = null;

    const { container } = render(<EngineModeBadge />);

    expect(container).toBeEmptyDOMElement();
  });
});
