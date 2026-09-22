import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { OpenDesktopPage } from "../OpenDesktopPage";

const ARRIVAL = "/multiplayer?join=AB12CD%40wss%3A%2F%2Flobby.phase-rs.dev%2Fws";
const DOWNLOAD = "https://github.com/phase-rs/phase/releases/latest";
const originalLocation = window.location;
const assign = vi.fn();

function desktopLink(path: string): string {
  return `phase://open?${new URLSearchParams({ site: "release", path })}`;
}

/** The page's router state, as last rendered. */
let entryState: unknown;
function EntryState() {
  entryState = useLocation().state;
  return null;
}

/** Stands in for the multiplayer arrival; its button is the browser's Back. */
function Arrival() {
  const navigate = useNavigate();
  return <button onClick={() => navigate(-1)}>Back</button>;
}

function renderPage(to: string | null, state?: unknown) {
  const search = to === null ? "" : `?${new URLSearchParams({ to })}`;
  render(
    <MemoryRouter initialEntries={[{ pathname: "/open-desktop", search, state }]}>
      <Routes>
        <Route
          path="/open-desktop"
          element={
            <>
              <OpenDesktopPage />
              <EntryState />
            </>
          }
        />
        <Route path="/multiplayer" element={<Arrival />} />
      </Routes>
    </MemoryRouter>,
  );
}

const openApp = () => screen.queryByRole("link", { name: "Open desktop app" });
const continueInBrowser = () => screen.queryByRole("link", { name: "Didn't open? Continue in browser" });
const invalid = () => screen.queryByText("This link can't be opened in the desktop app.");

function expectDownloadLink() {
  expect(screen.getByRole("link", { name: "Get the desktop app" })).toHaveAttribute("href", DOWNLOAD);
}

describe("OpenDesktopPage", () => {
  beforeEach(() => {
    assign.mockReset();
    entryState = undefined;
    Object.defineProperty(window, "location", {
      configurable: true,
      writable: true,
      value: { ...originalLocation, assign },
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    Object.defineProperty(window, "location", {
      configurable: true,
      writable: true,
      value: originalLocation,
    });
  });

  it("hands a phase://open? link to the OS once and offers every fallback", () => {
    const to = desktopLink(ARRIVAL);
    renderPage(to);

    expect(assign).toHaveBeenCalledTimes(1);
    expect(assign).toHaveBeenCalledWith(to);
    expect(entryState).toEqual({ desktopHandOff: true });
    expect(openApp()).toHaveAttribute("href", to);
    expect(continueInBrowser()).toHaveAttribute("href", ARRIVAL);
    expect(invalid()).toBeNull();
    expectDownloadLink();
  });

  it("marks the entry without dropping its existing state", () => {
    renderPage(desktopLink(ARRIVAL), { from: "discord" });

    expect(assign).toHaveBeenCalledTimes(1);
    expect(entryState).toEqual({ from: "discord", desktopHandOff: true });
  });

  it("skips the hand-off on an entry that already handed off (a Back that reloads the page)", () => {
    const to = desktopLink(ARRIVAL);
    renderPage(to, { desktopHandOff: true });

    expect(assign).not.toHaveBeenCalled();
    expect(openApp()).toHaveAttribute("href", to);
    expect(continueInBrowser()).toHaveAttribute("href", ARRIVAL);
    expectDownloadLink();
  });

  it("does not hand off again on an in-app Back from Continue in browser", () => {
    const to = desktopLink(ARRIVAL);
    renderPage(to);
    expect(assign).toHaveBeenCalledTimes(1);

    fireEvent.click(continueInBrowser()!);
    // Reach guard: the page left for the multiplayer arrival.
    expect(openApp()).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));

    expect(openApp()).toHaveAttribute("href", to);
    expect(assign).toHaveBeenCalledTimes(1);
  });

  it("offers no browser fallback for a path outside the multiplayer arrival", () => {
    const to = desktopLink("/game/1");
    renderPage(to);

    expect(assign).toHaveBeenCalledWith(to);
    expect(openApp()).toHaveAttribute("href", to);
    expect(continueInBrowser()).toBeNull();
    expectDownloadLink();
  });

  it.each([
    ["an https URL", "https://evil.example/multiplayer?join=x"],
    ["another phase:// host", `phase://evil?${new URLSearchParams({ site: "release", path: ARRIVAL })}`],
    ["a javascript: URL", "javascript:alert(1)"],
    ["a missing to", null],
  ])("refuses %s: no assign, no desktop or browser link, invalid text", (_label, to) => {
    renderPage(to);

    expect(assign).not.toHaveBeenCalled();
    expect(openApp()).toBeNull();
    expect(continueInBrowser()).toBeNull();
    expect(invalid()).toBeInTheDocument();
    expectDownloadLink();
  });
});
