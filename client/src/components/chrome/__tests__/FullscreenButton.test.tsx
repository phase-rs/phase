import { act, cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { getCurrentWindowMock, isTauriMock } = vi.hoisted(() => ({
  getCurrentWindowMock: vi.fn(),
  isTauriMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: getCurrentWindowMock }));
vi.mock("../../../services/platform", () => ({ isTauri: isTauriMock }));

import { FullscreenButton } from "../FullscreenButton";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("FullscreenButton Tauri synchronization", () => {
  it("unlistens when onResized resolves after unmount and absorbs rejected resize sync", async () => {
    let resolveListener: ((unlisten: () => void) => void) | undefined;
    let onResize: (() => void) | undefined;
    const unlisten = vi.fn();
    const isFullscreen = vi.fn()
      .mockResolvedValueOnce(false)
      .mockRejectedValueOnce(new Error("window closed"));
    getCurrentWindowMock.mockReturnValue({
      isFullscreen,
      onResized: vi.fn((listener: () => void) => {
        onResize = listener;
        return new Promise((resolve) => { resolveListener = resolve; });
      }),
    });
    isTauriMock.mockReturnValue(true);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});

    const view = render(<FullscreenButton variant="chrome" />);
    await act(async () => {});
    view.unmount();
    await act(async () => { resolveListener?.(unlisten); });
    expect(unlisten).toHaveBeenCalledOnce();

    await act(async () => { onResize?.(); });
    expect(warn).toHaveBeenCalledWith(
      "[phase.rs] Could not synchronize Tauri fullscreen state.",
      expect.any(Error),
    );
  });
});
