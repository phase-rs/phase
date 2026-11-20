import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  DraftShellChromeProvider,
  type DraftShellChromeConfig,
} from "../../components/chrome/ShellContext";
import type { DraftPlayerView } from "../../adapter/draft-adapter";

/*
 * The pod page's layout for a Review view, on the real page, lifecycle store and
 * host controls. A Review view cannot reach this store over P2P, since both
 * roles refuse it, so each row seats the store's state itself. Only the page
 * chrome's visuals and the workspace's panels are doubles.
 */
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/draft/PackDisplay", () => ({ PackDisplay: () => <div data-testid="pack" /> }));
vi.mock("../../components/draft/workspace/DraftWorkspace", () => ({ DraftWorkspace: () => null }));
vi.mock("../../components/draft/DraftIntro", () => ({
  DraftIntro: ({ onContinue }: { onContinue(): void }) => <button onClick={onContinue}>Continue</button>,
}));
vi.mock("../../components/menu/MenuShell", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../components/menu/MenuShell")>()),
  MenuShell: ({ children, compactTopPadding }: { children: ReactNode; compactTopPadding?: boolean }) => (
    <div data-testid="menu-shell" data-compact={String(compactTopPadding)}>{children}</div>
  ),
}));

import { DraftPodPage } from "../DraftPodPage";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

const fixturePath = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../fixtures/draft-contract/fifo_legacy_admission.json");
const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as { review_view: DraftPlayerView };
const initialStore = useMultiplayerDraftStore.getState();

let chrome: DraftShellChromeConfig | null = null;

function renderPod(phase: "drafting" | "review", role: "host" | "guest", width: number, height: number) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
  useMultiplayerDraftStore.setState({ role, phase, view: fixture.review_view });
  const rendered = render(
    <DraftShellChromeProvider value={(config) => { chrome = config; }}>
      <MemoryRouter initialEntries={["/draft-pod"]}><DraftPodPage /></MemoryRouter>
    </DraftShellChromeProvider>,
  );
  // The workspace arm renders behind its intro.
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  expect(screen.getByTestId("pack")).toBeInTheDocument();
  return rendered;
}

const layouts = [
  ["phone-portrait", 430, 932, "phone-drafting", "h-dvh", true],
  ["phone-landscape", 924, 412, "phone-drafting", "h-dvh", true],
  ["tablet-portrait", 768, 1024, "tablet-drafting", "h-full", false],
  ["tablet-landscape", 1024, 768, "tablet-drafting", "h-full", false],
] as const;

describe("DraftPodPage Review layout", () => {
  beforeEach(() => {
    chrome = null;
    useMultiplayerDraftStore.setState(initialStore, true);
  });

  afterEach(() => {
    cleanup();
    useMultiplayerDraftStore.setState(initialStore, true);
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  describe.each(["drafting", "review"] as const)("%s", (phase) => {
    it.each(layouts)("takes the workspace's chrome on %s", (_layout, width, height, mode, rootClass, compact) => {
      const { container } = renderPod(phase, "host", width, height);

      expect(chrome?.mode).toBe(mode);
      expect(chrome?.phoneAction?.label).toBe("Pod Draft in Progress");
      expect(chrome?.topActions?.map(({ id }) => id)).toEqual(["pause-resume", "end-draft"]);
      expect(container.querySelector(".menu-scene")).toHaveClass(rootClass);
      expect(screen.getByTestId("menu-shell")).toHaveAttribute("data-compact", String(compact));
      // The compact layouts carry both actions in the shell, not in the panel.
      expect(screen.queryByRole("button", { name: "End Draft" })).toBeNull();
    });

    it("keeps one Pause Draft and one End Draft in the desktop panel", () => {
      const { container } = renderPod(phase, "host", 1440, 900);

      expect(chrome?.mode).toBe("default");
      expect(container.querySelector(".menu-scene")).toHaveClass("min-h-screen");
      expect(screen.getByTestId("menu-shell")).toHaveAttribute("data-compact", "false");
      expect(screen.getAllByRole("button", { name: "Pause Draft" })).toHaveLength(1);
      expect(screen.getAllByRole("button", { name: "End Draft" })).toHaveLength(1);
    });

    it("gives a guest the layout and no host action", () => {
      const { container } = renderPod(phase, "guest", 430, 932);

      expect(chrome?.mode).toBe("phone-drafting");
      expect(chrome?.topActions).toEqual([]);
      expect(container.querySelector(".menu-scene")).toHaveClass("h-dvh");
      expect(screen.queryByRole("button", { name: "End Draft" })).toBeNull();
    });
  });

  it("pauses and resumes a Review, and ends it from its End Draft", async () => {
    const requestPause = vi.fn();
    const requestResume = vi.fn();
    const leave = vi.fn(async () => {});
    useMultiplayerDraftStore.setState({ requestPause, requestResume, leave });
    vi.stubGlobal("confirm", vi.fn(() => true));
    const user = userEvent.setup();
    renderPod("review", "host", 1440, 900);

    await user.click(screen.getByRole("button", { name: "Pause Draft" }));
    expect(requestPause).toHaveBeenCalledOnce();

    act(() => useMultiplayerDraftStore.setState({ paused: true }));
    await user.click(screen.getByRole("button", { name: "Resume Draft" }));
    expect(requestResume).toHaveBeenCalledOnce();

    await user.click(screen.getByRole("button", { name: "End Draft" }));
    expect(leave).toHaveBeenCalledOnce();
    expect(leave).toHaveBeenCalledWith(false);
  });
});
