import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PromptSourceBinding } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();
vi.mock("../../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => dispatchMock }));

function binding(incarnation: number, display_name = "Same source"): PromptSourceBinding {
  return { identity: { reference: { object_id: 42, incarnation }, expected_zone: "Battlefield" }, controller: 0, display_name };
}

function seed(options: PromptSourceBinding[], player = 0) {
  const gameState = gameStateFactory.withPlayers(0, 1).damageSourceChoice({ player, source_filter: { type: "Any" }, options }).build();
  setGameStoreForTest({ gameState, gameMode: "online" });
}

describe("exact damage source options", () => {
  beforeEach(() => { dispatchMock.mockClear(); useMultiplayerStore.setState({ activePlayerId: 0 }); });
  afterEach(() => {
    cleanup();
    useGameStore.setState(useGameStore.getInitialState(), true);
    useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
  });

  it("visibly and accessibly distinguishes two births without current objects", async () => {
    seed([binding(7), binding(9)]);
    render(<CardChoiceModal />);
    await waitFor(() => {
      expect(screen.getByText("Source #42, incarnation 7")).toBeVisible();
      expect(screen.getByText("Source #42, incarnation 9")).toBeVisible();
    });
    fireEvent.click(screen.getByRole("button", { name: /Same source.*incarnation 9/ }));
    expect(dispatchMock).toHaveBeenLastCalledWith({ type: "ChooseDamageSource", data: { source: { object_id: 42, incarnation: 9 } } });
    fireEvent.click(screen.getByRole("button", { name: /Same source.*incarnation 7/ }));
    expect(dispatchMock).toHaveBeenLastCalledWith({ type: "ChooseDamageSource", data: { source: { object_id: 42, incarnation: 7 } } });
  });

  it("keeps an unnamed battlefield source usable without hidden identity", async () => {
    seed([binding(11, "")]);
    render(<CardChoiceModal />);
    await waitFor(() => expect(screen.getByText("Unnamed source")).toBeVisible());
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Unnamed source.*incarnation 11/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseDamageSource", data: { source: { object_id: 42, incarnation: 11 } } });
  });

  it("renders a lawful face-up command binding as supplied by the engine", async () => {
    seed([{ ...binding(13, "Command source"), identity: { ...binding(13).identity, expected_zone: "Command" } }]);
    render(<CardChoiceModal />);
    await waitFor(() => expect(screen.getByRole("button", { name: /Command source.*incarnation 13.*Command/ })).toBeVisible());
  });

  it("keeps another player's prompt gated", async () => {
    seed([binding(7)], 1);
    render(<CardChoiceModal />);
    expect(screen.queryByRole("button", { name: /incarnation 7/ })).not.toBeInTheDocument();
    expect(dispatchMock).not.toHaveBeenCalled();
    seed([binding(7)], 0);
    await waitFor(() => expect(screen.getByRole("button", { name: /incarnation 7/ })).toBeVisible());
    fireEvent.click(screen.getByRole("button", { name: /incarnation 7/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChooseDamageSource", data: { source: { object_id: 42, incarnation: 7 } } });
  });
});
