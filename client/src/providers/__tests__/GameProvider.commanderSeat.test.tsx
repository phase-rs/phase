/**
 * Seat identity for a Commander pod's launched game.
 *
 * `setupDraftMatchAvatars` is the SEAT AUTHORITY for the whole `draft-match`
 * mode, not avatar cosmetics: its write of `activePlayerId` is the only one in
 * that mode, and `activePlayerId` is what every wire-assigned seat lookup
 * resolves to. Its 1v1 derivation (`matchPairing?.type === "HumanGuest" ? 1 : 0`)
 * hands every seat of an N-player Commander game `0`, because that launch leaves
 * `matchPairing` null by design — so all four players would render and act as
 * the HOST. A test asserting only "the guest reached the game" passes throughout.
 *
 * The function is module-private, so the only way to exercise it is to render
 * the provider. Precedent for the setup: `GameProvider.visualAvatars.test.tsx`.
 */

import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../services/serverDetection.ts", () => ({
  detectServerUrl: vi.fn(() => new Promise<string>(() => {})),
}));

import { useGameStore } from "../../stores/gameStore.ts";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore.ts";
import { useMultiplayerStore } from "../../stores/multiplayerStore.ts";
import {
  buildCommanderFormatConfig,
  buildGameState,
  buildPlayers,
} from "../../test/factories/gameStateFactory.ts";
import { GameProvider } from "../GameProvider.tsx";

const GAME_ID = "commander-pod-game";

function commanderLaunch(gameId = GAME_ID) {
  return {
    gameId,
    roomCode: "ABCDE-commander-abcd1234",
    localDeck: { main_deck: ["Sol Ring"], sideboard: [], commander: ["Kinnan, Bonder Prodigy"] },
    playerCount: 4,
    draftSetCodes: ["CMM"],
  };
}

/** A four-seat commander game, one commander per owner. */
function seedFourSeatCommanderGame() {
  useGameStore.setState({
    adapter: {} as never,
    gameId: GAME_ID,
    gameState: {
      ...buildGameState({ players: buildPlayers([{ id: 0 }, { id: 1 }, { id: 2 }, { id: 3 }]) }),
      format_config: buildCommanderFormatConfig(),
      command_zone: [30, 31, 32, 33],
      objects: {
        30: { name: "Kinnan, Bonder Prodigy", owner: 0, is_commander: true },
        31: { name: "Muldrotha, the Gravetide", owner: 1, is_commander: true },
        32: { name: "Atraxa, Praetors' Voice", owner: 2, is_commander: true },
        33: { name: "Niv-Mizzet Reborn", owner: 3, is_commander: true },
      },
    } as never,
  });
}

describe("GameProvider Commander pod seat identity", () => {
  beforeEach(() => {
    localStorage.clear();
    useMultiplayerStore.setState({
      playerNames: new Map(),
      playerAvatars: new Map(),
      activePlayerId: 0,
    });
    useMultiplayerDraftStore.setState({
      matchPairing: null,
      commanderLaunch: null,
      commanderSeat: null,
    });
    // WITHOUT this the `draft-match` branch bails to `onNoDeck` before it
    // registers the unmount cleanup, and the remount case below would then pass
    // against the one-shot write it exists to catch.
    seedFourSeatCommanderGame();
  });

  afterEach(() => {
    cleanup();
    useMultiplayerDraftStore.setState({ commanderLaunch: null, commanderSeat: null });
    vi.restoreAllMocks();
  });

  /**
   * Seat 2, never 1: a seat of 1 would also be produced by the 1v1
   * `HumanGuest` derivation, and 0 by every other way of getting this wrong.
   *
   * The explicit unmount/remount cycle is the point. It is deliberately NOT
   * `<StrictMode>`, whose double-mount is silently absent under a production
   * React build — which would make the case vacuous against exactly the
   * one-shot write it targets. The unmount runs `clearWireAssignedSeat()`, so a
   * seat consumed once rather than re-derived comes back null.
   */
  it("keeps a guest on its own seat across a remount", () => {
    useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunch(), commanderSeat: 2 });

    const first = render(
      <GameProvider gameId={GAME_ID} mode="draft-match">
        <div />
      </GameProvider>,
    );
    expect(useMultiplayerStore.getState().activePlayerId).toBe(2);

    first.unmount();
    expect(useMultiplayerStore.getState().activePlayerId).toBeNull();

    render(
      <GameProvider gameId={GAME_ID} mode="draft-match">
        <div />
      </GameProvider>,
    );
    expect(useMultiplayerStore.getState().activePlayerId).toBe(2);
  });

  /**
   * `commanderLaunch` outlives its game, so an unfenced branch would claim a
   * LATER draft-match game's seat from a stale launch.
   */
  it("writes no Commander seat when the launch names a different game", () => {
    useMultiplayerDraftStore.setState({
      commanderLaunch: commanderLaunch("some-other-game"),
      commanderSeat: 2,
    });

    render(
      <GameProvider gameId={GAME_ID} mode="draft-match">
        <div />
      </GameProvider>,
    );

    // The ordinary pod-match derivation answers instead — an unpaired match is
    // seat 0 — and it is emphatically not the stale launch's 2.
    expect(useMultiplayerStore.getState().activePlayerId).toBe(0);
  });

  /**
   * The seat write and the identity maps are separate halves, and the half that
   * breaks silently is this one: `setupDraftMatchAvatars` ends in a WHOLESALE
   * replacement of `playerNames`/`playerAvatars` built for exactly two players,
   * so a Commander branch that fell through to it would erase four commander
   * identities while leaving `activePlayerId` — written by that same call —
   * correct. Only an assertion on the map count can see that.
   */
  it("leaves a guest with an identity for every seat, not two", () => {
    useMultiplayerDraftStore.setState({ commanderLaunch: commanderLaunch(), commanderSeat: 2 });

    render(
      <GameProvider gameId={GAME_ID} mode="draft-match">
        <div />
      </GameProvider>,
    );

    const state = useMultiplayerStore.getState();
    expect(state.playerAvatars.size).toBe(4);
    expect(state.playerAvatars.get(2)).toEqual({
      kind: "card",
      cardName: "Atraxa, Praetors' Voice",
    });
    // Identity is computed from the viewer, never stored: a literal "You" in
    // the map would label the HOST's seat "You" on a seat-2 screen.
    expect([...state.playerNames.values()]).not.toContain("You");
  });
});
