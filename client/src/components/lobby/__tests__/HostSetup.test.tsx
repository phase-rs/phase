import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { HostSetup } from "../HostSetup";
import { FORMAT_DEFAULTS, useMultiplayerStore } from "../../../stores/multiplayerStore";

describe("HostSetup", () => {
  beforeEach(() => {
    useMultiplayerStore.setState({
      displayName: "",
      formatConfig: null,
      lastHostConfig: null,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("uses P2P labeling/theme and hides server-only lobby listing in p2p mode", () => {
    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="p2p"
      />,
    );

    // The screen heading now lives on the page shell (MultiplayerPage); the
    // form itself is distinguished by its P2P submit-button labeling.
    expect(screen.getByRole("button", { name: "Host P2P Game" })).toBeInTheDocument();
    expect(screen.queryByText("List in lobby")).not.toBeInTheDocument();
    expect(screen.queryByText("P2P currently supports 2-player Standard.")).not.toBeInTheDocument();
  });

  it("keeps server labeling and lobby listing in server mode", () => {
    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    // Heading now lives on the page shell; the form is distinguished by its
    // server-mode submit button + the server-only "List in lobby" toggle.
    expect(screen.getByRole("button", { name: "Host Game" })).toBeInTheDocument();
    expect(screen.getByText("List in lobby")).toBeInTheDocument();
  });

  it("allows Free-for-All hosts to choose 40-card deck size", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    render(
      <HostSetup
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Free-for-All" }));
    await user.click(screen.getByRole("button", { name: "40" }));
    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({
          format: "FreeForAll",
          deck_size: 40,
        }),
      }),
    );
  });

  it("submits Two-Headed Giant as a four-seat human-only team format", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();

    render(
      <HostSetup
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Format" }));
    await user.click(screen.getByRole("option", { name: "Two-Headed Giant" }));

    expect(screen.queryByRole("button", { name: "Human" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "AI difficulty" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({
          format: "TwoHeadedGiant",
          team_based: true,
          min_players: 4,
          max_players: 4,
        }),
        aiSeats: [],
      }),
    );
  });

  it("ignores stale restored AI seats for Two-Headed Giant", async () => {
    const user = userEvent.setup();
    const onHost = vi.fn();
    useMultiplayerStore.setState({
      lastHostConfig: {
        format: "TwoHeadedGiant",
        formatConfig: FORMAT_DEFAULTS.TwoHeadedGiant,
        playerCount: 4,
        matchType: "Bo1",
        isPublic: true,
        startWhenFull: true,
        ranked: false,
        aiSeats: [{ seatIndex: 1, difficulty: "Hard", deckName: null }],
      },
    });

    render(
      <HostSetup
        onHost={onHost}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    expect(screen.queryByRole("button", { name: "Human" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "AI difficulty" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Host Game" }));

    expect(onHost).toHaveBeenCalledWith(
      expect.objectContaining({
        formatConfig: expect.objectContaining({ format: "TwoHeadedGiant" }),
        aiSeats: [],
      }),
    );
    expect(useMultiplayerStore.getState().lastHostConfig?.aiSeats).toEqual([]);
  });

  it("renders and updates AI seat difficulty with translated labels", async () => {
    const user = userEvent.setup();

    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    await user.click(screen.getByRole("button", { name: "Human" }));

    const difficultyButton = screen.getByRole("button", { name: "AI difficulty" });
    expect(difficultyButton).toHaveTextContent("Medium");
    expect(screen.queryByText("VeryHard")).not.toBeInTheDocument();

    await user.click(difficultyButton);
    await user.click(screen.getByRole("option", { name: "Very Hard" }));

    expect(difficultyButton).toHaveTextContent("Very Hard");
    expect(screen.queryByText("VeryHard")).not.toBeInTheDocument();
  });
});
