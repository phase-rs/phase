import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { LobbyGame } from "../../../adapter/types";
import { GameListItem } from "../GameListItem";

const baseGame: LobbyGame = {
  game_code: "ABCD1",
  host_name: "Alice",
  created_at: 1_700_000_000,
  has_password: false,
  format: "Standard",
  current_players: 1,
  max_players: 2,
  host_build_commit: "testhash",
};

describe("GameListItem", () => {
  it("disables the row for the current player's hosted game", async () => {
    const user = userEvent.setup();
    const onJoin = vi.fn();

    render(
      <GameListItem
        game={baseGame}
        onJoin={onJoin}
        hostGameCode={baseGame.game_code}
      />,
    );

    const row = screen.getByRole("button", { name: /Hosting/ });
    expect(row).toBeDisabled();
    expect(row).toHaveAttribute("title", "You are hosting this game.");

    await user.click(row);

    expect(onJoin).not.toHaveBeenCalled();
  });

  it("allows joining a different game hosted by the same display name", async () => {
    const user = userEvent.setup();
    const onJoin = vi.fn();

    render(
      <GameListItem
        game={baseGame}
        onJoin={onJoin}
        hostGameCode="WXYZ9"
      />,
    );

    await user.click(screen.getByRole("button", { name: /Join/ }));

    expect(onJoin).toHaveBeenCalledWith(baseGame.game_code, baseGame.format);
  });
});
