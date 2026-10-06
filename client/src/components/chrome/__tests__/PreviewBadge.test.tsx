import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { isBundledTauriOriginMock, isTauriMock, openExternalMock } = vi.hoisted(() => ({
  isBundledTauriOriginMock: vi.fn(),
  isTauriMock: vi.fn(),
  openExternalMock: vi.fn(),
}));

vi.mock("../../../services/platform", () => ({
  isBundledTauriOrigin: isBundledTauriOriginMock,
  isTauri: isTauriMock,
}));

vi.mock("../../../services/channelPreference", () => ({
  rememberChannelPreference: vi.fn(),
}));

// Only openExternal is replaced: the badge validates its runtime target with the
// real isOpenableExternalUrl, so a stubbed validator would test nothing.
vi.mock("../../../services/openExternal", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/openExternal")>()),
  openExternal: openExternalMock,
}));

import { PreviewBadge } from "../PreviewBadge";

const SELF_PREVIEW = "https://phase-preview.example.test/";

function setRuntimeConfig(config: unknown) {
  (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__ = config;
}

function previewLink() {
  return screen.getByRole("link", { name: /try preview/i });
}

beforeEach(() => {
  isTauriMock.mockReturnValue(true);
  isBundledTauriOriginMock.mockReturnValue(false);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  delete (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__;
  vi.clearAllMocks();
});

describe("PreviewBadge", () => {
  it("keeps the build-time preview site and in-webview navigation in the remote Tauri shell, whatever /config.js sets", () => {
    setRuntimeConfig({ previewSiteUrl: SELF_PREVIEW });
    render(<PreviewBadge />);

    const link = previewLink();
    expect(link).toHaveAttribute("href", "https://preview.phase-rs.dev");
    expect(link).not.toHaveAttribute("target");
  });
});

describe("PreviewBadge on a release web build", () => {
  beforeEach(() => {
    isTauriMock.mockReturnValue(false);
    vi.stubGlobal("__IS_RELEASE_BUILD__", true);
  });

  it.each([[SELF_PREVIEW], ["http://192.168.1.5:8080/"]])(
    "links to and opens the preview site /config.js sets: %s",
    (value) => {
      // Otherwise "runtime value wins" and "runtime value ignored" look the same.
      expect(value).not.toBe(__PREVIEW_SITE_URL__);
      setRuntimeConfig({ previewSiteUrl: value });
      render(<PreviewBadge />);

      const link = previewLink();
      expect(link).toHaveAttribute("href", value);
      expect(link).toHaveAttribute("target", "_blank");
      fireEvent.click(link);
      expect(openExternalMock).toHaveBeenCalledWith(value);
    },
  );

  it.each([
    ["no scheme", { previewSiteUrl: "phase-preview.example.test" }],
    ["a websocket scheme", { previewSiteUrl: "wss://phase-preview.example.test" }],
    ["a javascript: URL", { previewSiteUrl: "javascript:alert(1)" }],
    ["an empty string", { previewSiteUrl: "" }],
    // Stringifies to an openable URL, so only the reader's typeof guard refuses it.
    ["a non-string", { previewSiteUrl: [SELF_PREVIEW] }],
    ["no config object", undefined],
  ])("falls back to the build-time preview site for %s", (_label, config) => {
    if (config !== undefined) setRuntimeConfig(config);
    render(<PreviewBadge />);

    const link = previewLink();
    expect(link).toHaveAttribute("href", __PREVIEW_SITE_URL__);
    fireEvent.click(link);
    expect(openExternalMock).toHaveBeenCalledWith(__PREVIEW_SITE_URL__);
  });

  it("stays hidden on a non-release web build even when /config.js sets a preview site", () => {
    vi.stubGlobal("__IS_RELEASE_BUILD__", false);
    setRuntimeConfig({ previewSiteUrl: SELF_PREVIEW });
    render(<PreviewBadge />);

    expect(screen.queryByRole("link")).toBeNull();
  });
});
