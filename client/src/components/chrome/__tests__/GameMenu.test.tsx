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
});
