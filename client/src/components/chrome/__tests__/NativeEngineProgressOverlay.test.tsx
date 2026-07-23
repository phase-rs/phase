import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { NativeEngineProgress } from "../../../services/nativeEngine";

const mocks = vi.hoisted(() => {
  let listener: ((progress: NativeEngineProgress) => void) | undefined;
  const unlisten = vi.fn();

  return {
    emitProgress(progress: NativeEngineProgress) {
      listener?.(progress);
    },
    subscribeNativeEngineProgress: vi.fn(async (next: (progress: NativeEngineProgress) => void) => {
      listener = next;
      return unlisten;
    }),
    getNativeEngineProgress: vi.fn(async (): Promise<NativeEngineProgress | null> => null),
    unlisten,
  };
});

vi.mock("../../../services/nativeEngine", () => ({
  getNativeEngineProgress: mocks.getNativeEngineProgress,
  subscribeNativeEngineProgress: mocks.subscribeNativeEngineProgress,
}));

import { NativeEngineProgressOverlay } from "../NativeEngineProgressOverlay";

describe("NativeEngineProgressOverlay", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("clearly shows native server downloads and their artifact key", async () => {
    render(<NativeEngineProgressOverlay />);
    await vi.waitFor(() => expect(mocks.subscribeNativeEngineProgress).toHaveBeenCalledOnce());

    act(() => {
      mocks.emitProgress({
        phase: "downloading_binary",
        detail: "preview-0123456789abcdef",
      });
    });

    expect(screen.getByRole("status")).toHaveTextContent("Updating native engine");
    expect(screen.getByRole("status")).toHaveTextContent("Downloading updated server…");
    expect(screen.getByRole("status")).toHaveTextContent("preview-0123456789abcdef");
  });

  it("shows native-engine completion as a non-busy status", async () => {
    render(<NativeEngineProgressOverlay />);
    await vi.waitFor(() => expect(mocks.subscribeNativeEngineProgress).toHaveBeenCalledOnce());

    act(() => {
      mocks.emitProgress({ phase: "ready" });
    });
    const status = screen.getByRole("status");
    expect(status).toHaveTextContent("Native engine ready");
    expect(status).toHaveAttribute("aria-busy", "false");
  });

  it("replays progress emitted before the overlay mounted", async () => {
    mocks.getNativeEngineProgress.mockResolvedValueOnce({ phase: "resolving" });

    render(<NativeEngineProgressOverlay />);

    expect(await screen.findByRole("status")).toHaveTextContent("Finding the correct local server…");
  });

  it("keeps live progress when the replay snapshot is stale", async () => {
    mocks.subscribeNativeEngineProgress.mockImplementationOnce(async (next) => {
      next({ phase: "downloading_data" });
      return mocks.unlisten;
    });
    mocks.getNativeEngineProgress.mockResolvedValueOnce({ phase: "resolving" });

    render(<NativeEngineProgressOverlay />);

    expect(await screen.findByRole("status")).toHaveTextContent("Downloading game data…");
  });
});
