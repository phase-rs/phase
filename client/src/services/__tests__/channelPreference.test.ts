import { beforeEach, describe, expect, it, vi } from "vitest";

const { invokeMock, isBundledTauriOriginMock, isTauriMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  isBundledTauriOriginMock: vi.fn(),
  isTauriMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("../platform", () => ({
  isBundledTauriOrigin: isBundledTauriOriginMock,
  isTauri: isTauriMock,
}));

import { rememberChannelPreference } from "../channelPreference";

beforeEach(() => {
  vi.clearAllMocks();
  invokeMock.mockResolvedValue(undefined);
  isBundledTauriOriginMock.mockReturnValue(false);
  isTauriMock.mockReturnValue(true);
});

describe("rememberChannelPreference", () => {
  it("persists the selected channel from a remote Tauri shell", () => {
    rememberChannelPreference("preview");

    expect(invokeMock).toHaveBeenCalledWith("set_channel_preference", { channel: "preview" });
  });

  it("does nothing on the web or bundled Tauri origin", () => {
    isTauriMock.mockReturnValue(false);
    rememberChannelPreference("release");

    isTauriMock.mockReturnValue(true);
    isBundledTauriOriginMock.mockReturnValue(true);
    rememberChannelPreference("release");

    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("silently ignores an unavailable shell command", async () => {
    invokeMock.mockRejectedValue(new Error("unknown command"));

    expect(() => rememberChannelPreference("release")).not.toThrow();
    await Promise.resolve();
  });
});
