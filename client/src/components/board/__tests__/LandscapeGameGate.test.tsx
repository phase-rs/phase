import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  LandscapeGameBoundary,
  LandscapeGameGate,
} from "../LandscapeGameGate.tsx";

describe("LandscapeGameGate", () => {
  afterEach(cleanup);

  it("blocks portrait gameplay and keeps an exit available", async () => {
    const onExit = vi.fn();
    render(<LandscapeGameGate onExit={onExit} />);

    expect(
      screen.getByRole("heading", { name: "Rotate your device" }),
    ).toBeVisible();
    expect(screen.getByRole("dialog")).toHaveAttribute(
      "data-landscape-game-gate",
    );

    await userEvent.click(screen.getByRole("button", { name: "Back to menu" }));
    expect(onExit).toHaveBeenCalledOnce();
  });

  it("defers a new session, then preserves it behind the portrait gate", async () => {
    const onExit = vi.fn();
    const session = <div data-testid="game-session">Connected game</div>;
    const { rerender } = render(
      <LandscapeGameBoundary
        requiresLandscape
        sessionId="game-1"
        onExit={onExit}
      >
        {session}
      </LandscapeGameBoundary>,
    );

    expect(screen.queryByTestId("game-session")).not.toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeVisible();

    rerender(
      <LandscapeGameBoundary
        requiresLandscape={false}
        sessionId="game-1"
        onExit={onExit}
      >
        {session}
      </LandscapeGameBoundary>,
    );
    await waitFor(() => {
      expect(screen.getByTestId("game-session")).toBeVisible();
    });

    rerender(
      <LandscapeGameBoundary
        requiresLandscape
        sessionId="game-1"
        onExit={onExit}
      >
        {session}
      </LandscapeGameBoundary>,
    );
    expect(screen.getByTestId("game-session")).toBeVisible();
    expect(screen.getByRole("dialog")).toBeVisible();
  });
});
