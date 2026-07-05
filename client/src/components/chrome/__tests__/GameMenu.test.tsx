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

  it("renders a split/legacy board layout toggle", () => {
    const { onToggleMultiplayerBoardLayout } = renderGameMenu();

    fireEvent.click(screen.getByRole("button", { name: "Switch to legacy focused view" }));

    expect(screen.getByText("Split")).toBeInTheDocument();
    expect(onToggleMultiplayerBoardLayout).toHaveBeenCalledTimes(1);
  });

  it("labels the legacy state with the split destination", () => {
    renderGameMenu({ multiplayerBoardLayout: "focused" });

    expect(screen.getByRole("button", { name: "Switch to split table view" })).toHaveTextContent("Legacy");
  });

  it("opens sandbox tools from the sandbox dropdown", () => {
    const onSandboxToolsClick = vi.fn();
    renderGameMenu({ showSandboxTools: true, onSandboxToolsClick });

    const sandboxButton = screen.getByRole("button", { name: "Sandbox Tools" });

    expect(sandboxButton).toHaveAttribute("aria-haspopup", "menu");
    expect(sandboxButton).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(sandboxButton);

    expect(sandboxButton).toHaveAttribute("aria-expanded", "true");

    fireEvent.click(screen.getByRole("menuitem", { name: "Open Sandbox Tools" }));

    expect(onSandboxToolsClick).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("menuitem", { name: "Open Sandbox Tools" })).not.toBeInTheDocument();
  });

  it("toggles the floating click mode button from the sandbox dropdown", () => {
    const onToggleDebugClickModeButtonVisible = vi.fn();
    renderGameMenu({
      showSandboxTools: true,
      onSandboxToolsClick: vi.fn(),
      onToggleDebugClickModeButtonVisible,
    });

    fireEvent.click(screen.getByRole("button", { name: "Sandbox Tools" }));
    fireEvent.click(screen.getByRole("menuitemcheckbox", { name: "Click Mode Button Hidden" }));

    expect(onToggleDebugClickModeButtonVisible).toHaveBeenCalledTimes(1);
  });

  it("shows the floating click mode button as pinned in the sandbox dropdown", () => {
    renderGameMenu({
      showSandboxTools: true,
      onSandboxToolsClick: vi.fn(),
      debugClickModeButtonVisible: true,
      onToggleDebugClickModeButtonVisible: vi.fn(),
    });

    fireEvent.click(screen.getByRole("button", { name: "Sandbox Tools" }));

    expect(screen.getByRole("menuitemcheckbox", { name: "Click Mode Button Shown" })).toBeChecked();
  });
});
