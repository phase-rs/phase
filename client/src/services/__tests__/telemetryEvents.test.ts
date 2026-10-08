import { beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../stores/gameStore";
import { buildFormatConfig, buildGameState } from "../../test/factories/gameStateFactory";

const { flushNow, installTelemetryLifecycle, trackEvent } = vi.hoisted(() => ({
  flushNow: vi.fn(),
  installTelemetryLifecycle: vi.fn(),
  trackEvent: vi.fn(),
}));

vi.mock("../telemetry", () => ({
  flushNow,
  installTelemetryLifecycle,
  trackEvent,
}));

import { installTelemetry } from "../telemetryEvents";

describe("telemetry game event tracking", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    vi.clearAllMocks();
    installTelemetry();
    trackEvent.mockClear();
  });

  it("reports native mode when it is committed before the first game snapshot", () => {
    useGameStore.setState({
      gameId: "engine-mode-telemetry",
      gameMode: "ai",
      engineMode: "native",
      aiSeatIds: [1],
    });
    useGameStore.setState({
      gameState: buildGameState({
        format_config: buildFormatConfig({ format: "Commander" }),
        turn_number: 12,
      }),
    });

    expect(trackEvent).toHaveBeenCalledWith(
      "game_start",
      expect.objectContaining({
        engine_mode: "native",
      }),
    );
  });

  it("reports the store's engine mode and fallback reason when a game ends", () => {
    useGameStore.setState({
      gameId: "engine-mode-telemetry-fallback",
      gameMode: "ai",
      engineMode: "wasm",
      nativeEngineFallbackReason: "native_engine_unavailable",
      aiSeatIds: [1],
    });
    useGameStore.setState({
      gameState: buildGameState({
        format_config: buildFormatConfig({ format: "Commander" }),
        turn_number: 12,
      }),
    });
    useGameStore.setState({ waitingFor: { type: "GameOver", data: { winner: 0 } } });

    expect(trackEvent).toHaveBeenCalledWith(
      "game_start",
      expect.objectContaining({
        engine_mode: "wasm",
        native_fallback_reason: "native_engine_unavailable",
      }),
    );
    expect(trackEvent).toHaveBeenCalledWith(
      "game_end",
      expect.objectContaining({
        engine_mode: "wasm",
        native_fallback_reason: "native_engine_unavailable",
      }),
    );
  });
});
