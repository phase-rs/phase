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
});
