import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ConcedeDialog } from "../ConcedeDialog";

describe("ConcedeDialog", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("keeps ordinary-game and whole-match concessions distinct", () => {
    const concedeGame = vi.fn();
    const concedeMatch = vi.fn();
    render(
      <ConcedeDialog
        isOpen
        gameAction={{
          kind: "game",
          consequence: "best-of-three-game",
          onConfirm: concedeGame,
        }}
        matchAction={{ kind: "match", onConfirm: concedeMatch }}
        onCancel={vi.fn()}
      />,
    );

    expect(
      screen.getByText("You concede only the current game. If the match is not decided, sideboarding or the next game follows."),
    ).toBeInTheDocument();
    expect(
      screen.getByText("You concede the match. Your opponent wins the match and no further games will be played."),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Concede this game" }));
    expect(concedeGame).toHaveBeenCalledOnce();
    expect(concedeMatch).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "Concede entire match" }));
    expect(concedeMatch).toHaveBeenCalledOnce();
  });

  it("does not expose the match action without the capability", () => {
    render(
      <ConcedeDialog
        isOpen
        gameAction={{ kind: "game", consequence: "ordinary-game", onConfirm: vi.fn() }}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.getByText("Your opponent wins this game.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Concede entire match" })).not.toBeInTheDocument();
  });

  it("contains focus, closes on Escape, and restores the opener", async () => {
    function Harness() {
      const [open, setOpen] = useState(false);
      return (
        <>
          <button type="button" onClick={() => setOpen(true)}>
            Open concede
          </button>
          <ConcedeDialog
            isOpen={open}
            gameAction={{
              kind: "game",
              consequence: "ordinary-game",
              onConfirm: vi.fn(),
            }}
            onCancel={() => setOpen(false)}
          />
        </>
      );
    }

    render(<Harness />);
    const opener = screen.getByRole("button", { name: "Open concede" });
    opener.focus();
    fireEvent.click(opener);
    const dialog = await screen.findByRole("alertdialog", { name: "Concede Game?" });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus(),
    );

    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog", { name: "Concede Game?" })).not.toBeInTheDocument(),
    );
    expect(opener).toHaveFocus();
  });
});
