import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { GameMenu } from "../GameMenu";

vi.mock("../../../hooks/useCardDataMeta.ts", () => ({
  useCardDataMeta: () => null,
}));

function renderGameMenu(
  props: Partial<React.ComponentProps<typeof GameMenu>> = {},
) {
  const onToggleMultiplayerBoardLayout = vi.fn();

  render(
    <MemoryRouter initialEntries={["/game/test-game"]}>
      <GameMenu
        gameId="test-game"
        isAiMode={false}
        isOnlineMode={false}
        showAiHand={false}
        onToggleAiHand={vi.fn()}
        onSettingsClick={vi.fn()}
        onHelpClick={vi.fn()}
        multiplayerBoardLayout="split"
        onToggleMultiplayerBoardLayout={onToggleMultiplayerBoardLayout}
        {...props}
      />
    </MemoryRouter>,
  );

  return { onToggleMultiplayerBoardLayout };
}

describe("GameMenu", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders the split/legacy board layout toggle inside the menu", () => {
    const { onToggleMultiplayerBoardLayout } = renderGameMenu();

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Switch to legacy focused view Split" }));

    expect(onToggleMultiplayerBoardLayout).toHaveBeenCalledTimes(1);
  });

  it("provides its own touch scroll region on short game viewports", () => {
    renderGameMenu();

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    const menuPanel = screen.getByRole("button", { name: "Concede" }).closest(
      '[aria-label="Game menu"]',
    );

    expect(menuPanel).toHaveClass(
      "game-menu-scroll",
      "overflow-y-auto",
      "overscroll-contain",
      "touch-pan-y",
    );
  });

  it("labels the legacy state with the split destination", () => {
    renderGameMenu({ multiplayerBoardLayout: "focused" });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Switch to split table view Legacy" })).toBeInTheDocument();
  });

  it("opens sandbox tools from the collapsed menu", () => {
    const onSandboxToolsClick = vi.fn();
    renderGameMenu({ showSandboxTools: true, onSandboxToolsClick });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Open Sandbox Tools" }));

    expect(onSandboxToolsClick).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: "Open Sandbox Tools" })).not.toBeInTheDocument();
  });

  it("toggles the floating click mode button from the collapsed menu", () => {
    const onToggleDebugClickModeButtonVisible = vi.fn();
    renderGameMenu({
      showSandboxTools: true,
      onSandboxToolsClick: vi.fn(),
      onToggleDebugClickModeButtonVisible,
    });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Click Mode Button Hidden" }));

    expect(onToggleDebugClickModeButtonVisible).toHaveBeenCalledTimes(1);
  });

  it("shows the floating click mode button as pinned in the collapsed menu", () => {
    renderGameMenu({
      showSandboxTools: true,
      onSandboxToolsClick: vi.fn(),
      debugClickModeButtonVisible: true,
      onToggleDebugClickModeButtonVisible: vi.fn(),
    });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    expect(screen.getByRole("button", { name: "Click Mode Button Shown" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  // The takeback item's gate used to be `isOnlineMode && onRequestTakeback`,
  // which hid it for desktop solo-vs-AI (`native-ai` reaches GamePage as
  // `mode=ai`, so `isOnlineMode` is false) even though that game has a real
  // server-authoritative takeback and no client-side undo. GamePage is now the
  // single authority and gates on the transport; the menu only asks whether it
  // was given a handler.
  it("offers takeback whenever a handler is supplied, even outside online mode", () => {
    const onRequestTakeback = vi.fn();
    // `isOnlineMode: false` is the whole point — this is the desktop-solo
    // shape, and it is what fails if the `isOnlineMode &&` gate comes back.
    renderGameMenu({ isOnlineMode: false, onRequestTakeback });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));
    fireEvent.click(screen.getByRole("button", { name: "Request Takeback" }));

    expect(onRequestTakeback).toHaveBeenCalledTimes(1);
  });

  it("hides takeback when no handler is supplied", () => {
    // Paired negative: without it, "render always" would pass the test above.
    renderGameMenu({ isOnlineMode: true, onRequestTakeback: undefined });

    fireEvent.click(screen.getByRole("button", { name: "Game menu" }));

    // Reach guard: the menu really did open, so the absence below is the gate
    // rather than an unrendered menu.
    expect(screen.getByRole("button", { name: "Concede" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Request Takeback" })).toBeNull();
  });

});
