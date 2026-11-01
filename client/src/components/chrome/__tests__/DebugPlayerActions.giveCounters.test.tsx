import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers";
import { DebugPlayerActions } from "../DebugPlayerActions";

// Seat 0 is the local perspective in solo mode, so `getPlayerDisplayName`
// labels it "You" and seat 1 "Opp 2" (en `game.player.you` / `game.player.opponent`).
const LOCAL_SEAT_LABEL = "You";
const SEAT_ONE_LABEL = "Opp 2";

function renderForm() {
  const onDispatch = vi.fn();
  render(<DebugPlayerActions onDispatch={onDispatch} />);
  return onDispatch;
}

// The accordion header's accessible name carries a +/- suffix, so match by prefix.
function openSection(label: string) {
  fireEvent.click(screen.getByRole("button", { name: new RegExp(`^${label}`) }));
}

function selectSeatOne() {
  fireEvent.click(screen.getByRole("button", { name: LOCAL_SEAT_LABEL }));
  fireEvent.click(screen.getByRole("option", { name: SEAT_ONE_LABEL }));
}

describe("DebugPlayerActions give-counters forms", () => {
  beforeEach(() => {
    setGameStoreForTest({
      gameState: gameStateFactory.withPlayers({ id: 0 }, { id: 1 }).build(),
    });
  });

  afterEach(cleanup);

  it("dispatches GivePlayerCounters with the chosen player, kind and count", () => {
    const onDispatch = renderForm();
    openSection("Give Counters");
    selectSeatOne();
    fireEvent.click(screen.getByRole("button", { name: "Poison" }));
    fireEvent.click(screen.getByRole("option", { name: "Experience" }));
    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "3" } });

    expect(onDispatch).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Give Counters" }));

    expect(onDispatch).toHaveBeenCalledTimes(1);
    expect(onDispatch.mock.calls[0][0]).toEqual({
      type: "GivePlayerCounters",
      data: { player_id: 1, counter_kind: "Experience", count: 3 },
    });
  });

  it("dispatches GiveEnergy with the chosen player and count only", () => {
    const onDispatch = renderForm();
    openSection("Give Energy");
    selectSeatOne();
    fireEvent.change(screen.getByRole("spinbutton"), { target: { value: "5" } });

    expect(onDispatch).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Give Energy" }));

    expect(onDispatch).toHaveBeenCalledTimes(1);
    expect(onDispatch.mock.calls[0][0]).toEqual({
      type: "GiveEnergy",
      data: { player_id: 1, count: 5 },
    });
  });
});
