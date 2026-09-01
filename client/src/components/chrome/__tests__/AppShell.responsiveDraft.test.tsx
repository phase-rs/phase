import { useMemo, useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { AppShell } from "../AppShell";
import {
  useDraftShellChrome,
  type DraftShellChromeMode,
  type DraftShellTopAction,
} from "../ShellContext";

const chromeControlProps: Array<{ hideVolume?: boolean; hideLanguage?: boolean }> = [];
const phoneActionClick = vi.fn();
const pauseActionClick = vi.fn();
const endActionClick = vi.fn();

vi.mock("../../../hooks/useChangelog", () => ({
  useChangelog: () => ({
    hasUnread: false,
    entries: [],
    loading: false,
    failed: false,
    openAndLoad: vi.fn(),
  }),
}));
vi.mock("../../menu/MenuParticles", () => ({ SceneParticles: () => null }));
vi.mock("../../modal/WhatsNewModal", () => ({ WhatsNewModal: () => null }));
vi.mock("../CardDataLoadingBar", () => ({ CardDataLoadingBar: () => null }));
vi.mock("../ChromeControls", () => ({ ChromeControls: (props: { hideVolume?: boolean; hideLanguage?: boolean }) => {
  chromeControlProps.push(props);
  return <div data-testid="chrome-controls" />;
} }));
vi.mock("../Rail", () => ({ Rail: () => <nav aria-label="Desktop navigation" /> }));
vi.mock("../TabBar", () => ({ TabBar: () => <nav aria-label="Mobile navigation" /> }));
vi.mock("../SocialBar", () => ({ SocialBar: () => <div data-testid="social-bar" /> }));

function DraftChromeProbe() {
  const [mode, setMode] = useState<DraftShellChromeMode>("phone-drafting");
  const [showProgress, setShowProgress] = useState(true);
  const phoneAction = useMemo(() => ({
    icon: <span data-testid="pod-icon" />,
    label: "Pod Draft in Progress",
    onClick: phoneActionClick,
  }), []);
  const topActions = useMemo<readonly DraftShellTopAction[]>(() => [
    { id: "pause-resume", label: "Pause Draft", tone: "neutral", onClick: pauseActionClick },
    { id: "end-draft", label: "End Draft", tone: "danger", onClick: endActionClick },
  ], []);
  useDraftShellChrome(mode, phoneAction, "pod", showProgress, topActions);
  return (
    <div>
      <button type="button" onClick={() => setMode("phone-drafting")}>Phone draft mode</button>
      <button type="button" onClick={() => setMode("phone-deckbuilding")}>Phone builder mode</button>
      <button type="button" onClick={() => setMode("tablet-drafting")}>Tablet mode</button>
      <button type="button" onClick={() => setMode("tablet-deckbuilding")}>Tablet builder mode</button>
      <button type="button" onClick={() => setShowProgress(false)}>Hide progress</button>
      <button type="button" onClick={() => setMode("default")}>Default mode</button>
    </div>
  );
}

describe("AppShell responsive draft chrome", () => {
  afterEach(() => {
    cleanup();
    chromeControlProps.length = 0;
    phoneActionClick.mockClear();
    pauseActionClick.mockClear();
    endActionClick.mockClear();
  });

  it("replaces phone navigation and socials with a top-row Home button", async () => {
    render(
      <MemoryRouter initialEntries={["/draft"]}>
        <Routes>
          <Route element={<AppShell />}>
            <Route path="/draft" element={<DraftChromeProbe />} />
          </Route>
        </Routes>
      </MemoryRouter>,
    );

    await waitFor(() => expect(screen.getByRole("link", { name: "Home" })).toBeInTheDocument());
    expect(screen.getByRole("link", { name: "Home" })).toHaveTextContent("Home");
    const phoneChromeRow = screen.getByRole("link", { name: "Home" }).parentElement;
    expect(phoneChromeRow).toHaveClass(
      "min-h-[calc(env(safe-area-inset-top)+52px)]",
      "pt-[calc(env(safe-area-inset-top)+1rem)]",
    );
    fireEvent.click(screen.getByRole("button", { name: "Pod Draft in Progress" }));
    expect(phoneActionClick).toHaveBeenCalledOnce();
    expect(screen.getByTestId("pod-icon")).toBeInTheDocument();
    const phoneChromeOrder = [...phoneChromeRow!.children]
      .flatMap((element) => element.getAttribute("aria-label") ?? []);
    expect(phoneChromeOrder).toEqual(["Home", "Pod Draft in Progress", "Pause Draft", "End Draft"]);
    expect(screen.getByRole("button", { name: "Pause Draft" })).toHaveTextContent("Pause Draft");
    expect(screen.getByRole("button", { name: "End Draft" })).toHaveTextContent("End Draft");
    fireEvent.click(screen.getByRole("button", { name: "Pause Draft" }));
    fireEvent.click(screen.getByRole("button", { name: "End Draft" }));
    expect(pauseActionClick).toHaveBeenCalledOnce();
    expect(endActionClick).toHaveBeenCalledOnce();
    expect(screen.queryByText("Choose Set")).not.toBeInTheDocument();
    expect(screen.getByText("Draft")).toHaveAttribute("aria-current", "step");
    const shellSteps = document.querySelector("[data-shell-draft-steps]");
    expect(shellSteps).toHaveTextContent("Draft");
    expect(shellSteps).toHaveClass("absolute", "inset-x-0", "z-0", "justify-center", "pointer-events-none");
    expect(screen.getByRole("link", { name: "Home" })).toHaveClass("relative", "z-10");
    expect(screen.getByText("Draft")).toHaveAttribute("aria-current", "step");
    expect(screen.queryByTestId("social-bar")).not.toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Desktop navigation" })).not.toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Mobile navigation" })).not.toBeInTheDocument();
    expect(screen.getByTestId("chrome-controls")).toBeInTheDocument();
    expect(chromeControlProps[chromeControlProps.length - 1]).toMatchObject({
      hideVolume: true,
      hideLanguage: true,
    });

    fireEvent.click(screen.getByRole("button", { name: "Phone builder mode" }));
    await waitFor(() => expect(screen.getByText("Build Deck")).toHaveAttribute("aria-current", "step"));

    fireEvent.click(screen.getByRole("button", { name: "Hide progress" }));
    await waitFor(() => expect(document.querySelector("[data-shell-draft-steps]")).not.toBeInTheDocument());
  });

  it("hides tablet socials while retaining navigation and restores default chrome", async () => {
    render(
      <MemoryRouter initialEntries={["/draft"]}>
        <Routes>
          <Route element={<AppShell />}>
            <Route path="/draft" element={<DraftChromeProbe />} />
          </Route>
        </Routes>
      </MemoryRouter>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Tablet mode" }));
    await waitFor(() => expect(screen.getByRole("link", { name: "Home" })).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Pod Draft in Progress" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Pod Draft in Progress" }));
    expect(phoneActionClick).toHaveBeenCalledOnce();
    const tabletChromeRow = screen.getByRole("link", { name: "Home" }).parentElement!;
    expect([...tabletChromeRow.children].flatMap((element) => element.getAttribute("aria-label") ?? []))
      .toEqual(["Home", "Pod Draft in Progress", "Pause Draft", "End Draft"]);
    expect(screen.getByRole("button", { name: "Pause Draft" })).toHaveTextContent("Pause Draft");
    expect(screen.getByRole("button", { name: "End Draft" })).toHaveTextContent("End Draft");
    expect(document.querySelector(".menu-scene")).toHaveClass("h-dvh", "min-h-0", "overflow-y-hidden");
    expect(document.querySelector("main.shell-content")).toHaveClass("min-h-0", "overflow-hidden");
    expect(screen.queryByTestId("social-bar")).not.toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Desktop navigation" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Mobile navigation" })).toBeInTheDocument();
    expect(document.querySelectorAll("[data-step-arrow]")).toHaveLength(0);
    expect(chromeControlProps[chromeControlProps.length - 1]).toMatchObject({
      hideVolume: false,
      hideLanguage: false,
    });

    fireEvent.click(screen.getByRole("button", { name: "Tablet builder mode" }));
    await waitFor(() => expect(screen.getByText("Build Deck")).toHaveAttribute("aria-current", "step"));
    expect(screen.queryByRole("link", { name: "Home" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause Draft" })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Default mode" }));
    await waitFor(() => expect(screen.getByTestId("social-bar")).toBeInTheDocument());
    expect(screen.getByRole("navigation", { name: "Desktop navigation" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Mobile navigation" })).toBeInTheDocument();
  });
});
