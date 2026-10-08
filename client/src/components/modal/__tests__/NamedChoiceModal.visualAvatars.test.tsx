import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types.ts";

const { advanceFailedSource, cardImage, dispatch, useCardImage } = vi.hoisted(() => {
  const advanceFailedSource = vi.fn();
  const cardImage: {
    src: string | null;
    isLoading: boolean;
    source: { kind: "installed" | "remote" | "fallback"; src: string | null };
    advanceFailedSource: typeof advanceFailedSource;
  } = {
    src: "visual-pack://installed-choice-avatar",
    isLoading: false,
    source: { kind: "installed", src: "visual-pack://installed-choice-avatar" },
    advanceFailedSource,
  };
  return {
    advanceFailedSource,
    cardImage,
    dispatch: vi.fn(),
    useCardImage: vi.fn(() => cardImage),
  };
});

vi.mock("../../../hooks/useCardImage.ts", () => ({ useCardImage }));
vi.mock("../../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => dispatch }));

import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { NamedChoiceModal } from "../NamedChoiceModal.tsx";

type NamedChoiceData = Extract<WaitingFor, { type: "NamedChoice" }>["data"];

const playerChoice: NamedChoiceData = {
  player: 0,
  choice_type: "Player",
  options: ["1"],
};

describe("NamedChoiceModal visual avatars", () => {
  beforeEach(() => {
    advanceFailedSource.mockReset();
    dispatch.mockReset();
    cardImage.src = "visual-pack://installed-choice-avatar";
    cardImage.isLoading = false;
    cardImage.source = { kind: "installed", src: "visual-pack://installed-choice-avatar" };
    useMultiplayerStore.setState({
      activePlayerId: 0,
      playerNames: new Map([[1, "Public Twin"]]),
      playerAvatars: new Map([
        [1, { kind: "card", cardName: "PRIVATE CHOICE CARD" }],
      ]),
    });
  });

  afterEach(() => cleanup());

  it("uses exact player-id card identity, captured errors, privacy fallback, and raw choice id", () => {
    const view = render(<NamedChoiceModal data={playerChoice} />);

    const option = screen.getByRole("button", { name: "Public Twin" });
    const installed = option.querySelector("img");
    expect(installed).toHaveAttribute("src", "visual-pack://installed-choice-avatar");
    fireEvent.error(installed!);
    expect(advanceFailedSource).toHaveBeenCalledWith("visual-pack://installed-choice-avatar");

    cardImage.src = "https://cards.example/remote-choice.jpg";
    cardImage.source = { kind: "remote", src: "https://cards.example/remote-choice.jpg" };
    view.rerender(<NamedChoiceModal data={playerChoice} />);
    const remote = screen.getByRole("button", { name: "Public Twin" }).querySelector("img");
    expect(remote).toHaveAttribute("src", "https://cards.example/remote-choice.jpg");

    cardImage.src = null;
    cardImage.source = { kind: "fallback", src: null };
    view.rerender(<NamedChoiceModal data={playerChoice} />);
    expect(screen.getByRole("button", { name: "Public Twin" })).toHaveTextContent("P");
    expect(view.container.innerHTML).not.toContain("PRIVATE CHOICE CARD");

    fireEvent.click(screen.getByRole("button", { name: "Public Twin" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseOption",
      data: { choice: "1" },
    });
  });

  it("renders exact external identity then reaches the public-name initial", () => {
    useMultiplayerStore.setState({
      playerAvatars: new Map([
        [1, { kind: "external", url: "https://provider.example/choice.png" }],
      ]),
    });
    render(<NamedChoiceModal data={playerChoice} />);

    expect(useCardImage).toHaveBeenLastCalledWith("", { size: "art_crop" });
    const option = screen.getByRole("button", { name: "Public Twin" });
    const image = option.querySelector("img");
    expect(image).toHaveAttribute("src", "https://provider.example/choice.png");
    fireEvent.error(image!);
    expect(option).toHaveTextContent("P");
    expect(option.querySelector("img")).not.toBeInTheDocument();
  });

  it("keeps same-name player identities isolated by exact id", () => {
    useMultiplayerStore.setState({
      playerNames: new Map([[1, "Same Name"], [2, "Same Name"]]),
      playerAvatars: new Map([
        [1, { kind: "external", url: "https://provider.example/one.png" }],
        [2, { kind: "external", url: "https://provider.example/two.png" }],
      ]),
    });
    render(<NamedChoiceModal data={{ ...playerChoice, options: ["1", "2"] }} />);

    const [first, second] = screen.getAllByRole("button", { name: "Same Name" });
    expect(first.querySelector("img")).toHaveAttribute(
      "src",
      "https://provider.example/one.png",
    );
    expect(second.querySelector("img")).toHaveAttribute(
      "src",
      "https://provider.example/two.png",
    );
    fireEvent.error(first.querySelector("img")!);
    expect(first.querySelector("img")).not.toBeInTheDocument();
    expect(second.querySelector("img")).toHaveAttribute(
      "src",
      "https://provider.example/two.png",
    );
  });

  it("shows neither stale art nor a premature initial while card identity loads", () => {
    cardImage.src = null;
    cardImage.isLoading = true;
    cardImage.source = { kind: "fallback", src: null };
    const view = render(<NamedChoiceModal data={playerChoice} />);
    const option = screen.getByRole("button", { name: "Public Twin" });
    expect(option.querySelector("img")).not.toBeInTheDocument();
    expect(option).toHaveTextContent(/^Public Twin$/);

    cardImage.isLoading = false;
    view.rerender(<NamedChoiceModal data={playerChoice} />);
    expect(screen.getByRole("button", { name: "Public Twin" }))
      .toHaveTextContent(/^PPublic Twin$/);
  });
});
