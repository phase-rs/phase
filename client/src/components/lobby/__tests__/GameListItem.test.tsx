import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { LobbyGame } from "../../../adapter/types";
import { SERVER_PRESETS } from "../../../services/serverDetection";
import type { LobbyGameEntry, LobbySource } from "../../../stores/multiplayerStore";
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

const userSource: LobbySource = {
  url: "wss://play.example.com/ws",
  name: "play.example.com",
  origin: "user",
  kind: "Full",
};

const officialSource: LobbySource = {
  url: SERVER_PRESETS[0].url,
  name: "lobby.phase-rs.dev",
  origin: "official",
};

function entry(source: LobbySource, game: LobbyGame = baseGame): LobbyGameEntry {
  return { game, source };
}

describe("GameListItem", () => {
  // This suite renders more than once per file and vitest is not configured
  // with auto-cleanup, so unmount between cases or queries match stale trees.
  afterEach(cleanup);

  it("disables the row for the current player's hosted game", async () => {
    const user = userEvent.setup();
    const onJoin = vi.fn();

    render(
      <GameListItem
        entry={entry(officialSource)}
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
    const row = entry(officialSource);

    render(<GameListItem entry={row} onJoin={onJoin} hostGameCode="WXYZ9" />);

    await user.click(screen.getByRole("button", { name: /Join/ }));

    // The whole entry travels to the join handler: the row's authority is
    // what the join must open on.
    expect(onJoin).toHaveBeenCalledWith(row);
  });

  it("renders the listing source beside the row", () => {
    render(<GameListItem entry={entry(userSource)} onJoin={vi.fn()} />);

    expect(screen.getByText("play.example.com")).toBeInTheDocument();
    expect(
      screen.getByTitle(/Listed by play\.example\.com \(game server\)/),
    ).toBeInTheDocument();
  });

  it("labels a built-in source with its picker label", () => {
    render(<GameListItem entry={entry(officialSource)} onJoin={vi.fn()} />);

    expect(screen.getByText("Official")).toBeInTheDocument();
  });

  // V-U19f
  it("renders the health badge from the prop, and nothing without one", () => {
    render(<GameListItem entry={entry(userSource)} onJoin={vi.fn()} healthHint="slow" />);
    expect(screen.getByText("SLOW")).toBeInTheDocument();
    // Paired WITHIN the component: the origin badge is asserted present in all
    // three renders, so "no hint" is never "nothing rendered at all".
    expect(screen.getByText("play.example.com")).toBeInTheDocument();

    cleanup();
    render(<GameListItem entry={entry(userSource)} onJoin={vi.fn()} healthHint={null} />);
    expect(screen.queryByText("SLOW")).not.toBeInTheDocument();
    expect(screen.queryByText("UNRELIABLE")).not.toBeInTheDocument();
    expect(screen.getByText("play.example.com")).toBeInTheDocument();

    cleanup();
    render(<GameListItem entry={entry(userSource)} onJoin={vi.fn()} />);
    expect(screen.queryByText("SLOW")).not.toBeInTheDocument();
    expect(screen.queryByText("UNRELIABLE")).not.toBeInTheDocument();
    expect(screen.getByText("play.example.com")).toBeInTheDocument();

    // The other verdict renders its own label, so the badge is keyed on the
    // value and not merely on the prop being truthy.
    cleanup();
    render(
      <GameListItem entry={entry(userSource)} onJoin={vi.fn()} healthHint="unreliable" />,
    );
    expect(screen.getByText("UNRELIABLE")).toBeInTheDocument();
    expect(screen.queryByText("SLOW")).not.toBeInTheDocument();
  });

  it("omits the server kind until that source's handshake has landed", () => {
    const unknownKind: LobbySource = { ...userSource, kind: undefined };

    render(<GameListItem entry={entry(unknownKind)} onJoin={vi.fn()} />);

    // Paired with the `kind: "Full"` case above: the suffix appears only
    // when the kind is actually known.
    expect(screen.getByTitle("Listed by play.example.com")).toBeInTheDocument();
    expect(screen.queryByTitle(/game server/)).not.toBeInTheDocument();
  });
});
