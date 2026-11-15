import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { advanceFailedSource, cardImage, useCardImage } = vi.hoisted(() => {
  const advanceFailedSource = vi.fn();
  const cardImage: {
    src: string | null;
    isLoading: boolean;
    source: { kind: "installed" | "remote" | "fallback"; src: string | null };
    advanceFailedSource: typeof advanceFailedSource;
  } = {
    src: "visual-pack://installed-seat-avatar",
    isLoading: false,
    source: { kind: "installed", src: "visual-pack://installed-seat-avatar" },
    advanceFailedSource,
  };
  return { advanceFailedSource, cardImage, useCardImage: vi.fn(() => cardImage) };
});

vi.mock("../../../hooks/useCardImage.ts", () => ({ useCardImage }));

import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { OpponentSeatHeader } from "../OpponentSeatHeader.tsx";

describe("OpponentSeatHeader visual avatars", () => {
  beforeEach(() => {
    advanceFailedSource.mockReset();
    cardImage.src = "visual-pack://installed-seat-avatar";
    cardImage.isLoading = false;
    cardImage.source = { kind: "installed", src: "visual-pack://installed-seat-avatar" };
    useMultiplayerStore.setState({
      activePlayerId: 0,
      isSpectator: false,
      connectionStatus: "connected",
      playerNames: new Map([[1, "Public Twin"]]),
      playerAvatars: new Map([
        [1, { kind: "card", cardName: "PRIVATE SEAT CARD" }],
      ]),
    });
    useGameStore.setState({
      gameMode: null,
      waitingFor: null,
      gameState: buildGameState({
        players: buildPlayers([{ id: 0 }, { id: 1 }]),
        seat_order: [0, 1],
      }),
    });
  });

  afterEach(() => cleanup());

  it("advances captured card sources and reaches only the public-name initial", () => {
    const view = render(<OpponentSeatHeader playerId={1} />);

    const installed = screen.getByRole("img", { name: "Public Twin" });
    expect(installed).toHaveAttribute("src", "visual-pack://installed-seat-avatar");
    fireEvent.error(installed);
    expect(advanceFailedSource).toHaveBeenCalledWith("visual-pack://installed-seat-avatar");

    cardImage.src = "https://cards.example/remote-seat.jpg";
    cardImage.source = { kind: "remote", src: "https://cards.example/remote-seat.jpg" };
    view.rerender(<OpponentSeatHeader playerId={1} />);
    const remote = screen.getByRole("img", { name: "Public Twin" });
    expect(remote).toHaveAttribute("src", "https://cards.example/remote-seat.jpg");
    fireEvent.error(remote);
    expect(advanceFailedSource).toHaveBeenLastCalledWith(
      "https://cards.example/remote-seat.jpg",
    );

    cardImage.src = null;
    cardImage.source = { kind: "fallback", src: null };
    view.rerender(<OpponentSeatHeader playerId={1} />);
    expect(screen.queryByRole("img", { name: "Public Twin" })).not.toBeInTheDocument();
    expect(screen.getByText("P")).toBeInTheDocument();
    expect(view.container.innerHTML).not.toContain("PRIVATE SEAT CARD");
  });

  it("bypasses card resolution for external identity and resets to public fallback", () => {
    useMultiplayerStore.setState({
      playerAvatars: new Map([
        [1, { kind: "external", url: "https://provider.example/seat.png" }],
      ]),
    });
    render(<OpponentSeatHeader playerId={1} />);

    expect(useCardImage).toHaveBeenLastCalledWith("", { size: "art_crop" });
    const image = screen.getByRole("img", { name: "Public Twin" });
    expect(image).toHaveAttribute("src", "https://provider.example/seat.png");
    fireEvent.error(image);
    expect(screen.queryByRole("img", { name: "Public Twin" })).not.toBeInTheDocument();
    expect(screen.getByText("P")).toBeInTheDocument();
  });

  it("distinguishes loading from terminal and preserves immediate no-identity fallback", () => {
    cardImage.src = null;
    cardImage.isLoading = true;
    cardImage.source = { kind: "fallback", src: null };
    const view = render(<OpponentSeatHeader playerId={1} />);

    expect(screen.queryByText("P")).not.toBeInTheDocument();
    cardImage.isLoading = false;
    view.rerender(<OpponentSeatHeader playerId={1} />);
    expect(screen.getByText("P")).toBeInTheDocument();

    useMultiplayerStore.setState({ playerAvatars: new Map() });
    expect(screen.getByText("P")).toBeInTheDocument();
  });
});
