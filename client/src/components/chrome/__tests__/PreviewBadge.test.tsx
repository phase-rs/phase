import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { isBundledTauriOriginMock, isTauriMock } = vi.hoisted(() => ({
  isBundledTauriOriginMock: vi.fn(),
  isTauriMock: vi.fn(),
}));

vi.mock("../../../services/platform", () => ({
  isBundledTauriOrigin: isBundledTauriOriginMock,
  isTauri: isTauriMock,
}));

vi.mock("../../../services/channelPreference", () => ({
  rememberChannelPreference: vi.fn(),
}));

vi.mock("../../../services/openExternal", () => ({
  openExternal: vi.fn(),
}));

import { PreviewBadge } from "../PreviewBadge";

beforeEach(() => {
  isTauriMock.mockReturnValue(true);
  isBundledTauriOriginMock.mockReturnValue(false);
});

describe("PreviewBadge", () => {
  it("keeps preview navigation in the remote Tauri webview", () => {
    render(<PreviewBadge />);

    const link = screen.getByRole("link", { name: /try preview/i });
    expect(link).toHaveAttribute("href", "https://preview.phase-rs.dev");
    expect(link).not.toHaveAttribute("target");
  });
});
