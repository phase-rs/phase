/**
 * Regression test for the pod-draft guest's collapsed seat.
 *
 * `usePlayerId`/`getPlayerId` are the single authority for "which seat is this
 * client's own" — 54 production call sites read one of them, from the mulligan
 * modal's `pending.find` in `GamePage` to `gameLoopController`'s priority gate
 * and `useConcedeHandler`'s `player_id`. Both resolved the answer from a
 * hand-typed list of game modes that predates `"draft-match"`, so a pod-draft
 * guest — correctly told `activePlayerId: 1` by `setupDraftMatchAvatars`
 * (`GameProvider.tsx`) — was answered seat 0, the host's seat.
 *
 * The class guard below is the one that would have caught it: for EVERY mode
 * that seats other humans, a client told "you are seat 1" must not answer 0.
 * It reads the mode census (`GAME_MODE_TRAITS`) rather than a second hand-typed
 * list, so a mode added later is covered on the day it is added.
 *
 * What these rows do NOT prove: they do not render `GamePage`, so they do not
 * show the mulligan modal appearing for the guest. They pin the seat authority
 * that modal's `pending.find(e => e.player === playerId)` reads.
 */
import { renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { PLAYER_ID, SPECTATOR_PLAYER_ID } from "../../constants/game";
import type { GameMode } from "../../stores/gameStore";
import { GAME_MODE_TRAITS, seatSource, useGameStore } from "../../stores/gameStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { getPlayerId, usePlayerId } from "../usePlayerId";

/** Derived from the census itself, never hand-typed: a mode added to
 *  `GAME_MODE_TRAITS` joins this list without anyone remembering to. */
const ALL_MODES = Object.keys(GAME_MODE_TRAITS) as GameMode[];

function seatMe(gameMode: GameMode, activePlayerId: number | null) {
  useGameStore.setState({ gameMode });
  useMultiplayerStore.setState({ activePlayerId, isSpectator: false });
}

describe("local seat resolution", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });

  afterEach(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
  });

  it.each(ALL_MODES.filter((mode) => seatSource(mode) === "wire-assigned"))(
    "%s: a client told it holds seat 1 is never answered seat 0",
    (mode) => {
      seatMe(mode, 1);
      // Spectators answer `SPECTATOR_PLAYER_ID` here rather than 1; that is
      // still not seat 0, so this assertion needs no by-name exception and
      // stays honest for every mode the census grows.
      expect(getPlayerId()).not.toBe(PLAYER_ID);
    },
  );

  it("keeps the P2P host at seat 0 despite a stale wire assignment", () => {
    seatMe("p2p-host", 2);
    const { result } = renderHook(() => usePlayerId());
    expect(result.current).toBe(PLAYER_ID);
    expect(getPlayerId()).toBe(PLAYER_ID);
  });

  it("seats a pod-draft guest at the seat the pod gave them", () => {
    seatMe("draft-match", 1);
    const { result } = renderHook(() => usePlayerId());
    expect(result.current).toBe(1);
    expect(getPlayerId()).toBe(1);
  });

  it("still seats the pod-draft host at 0", () => {
    seatMe("draft-match", 0);
    const { result } = renderHook(() => usePlayerId());
    expect(result.current).toBe(PLAYER_ID);
    expect(getPlayerId()).toBe(PLAYER_ID);
  });

  // Counter-direction (the expensive collateral is over-correction): a solo
  // game must keep answering 0 even when a previous online game left a stale
  // `activePlayerId` behind. These rows are green with and without the fix —
  // they nail the behaviour down, they do not evidence it.
  it.each(ALL_MODES.filter((mode) => seatSource(mode) === "seat-zero"))(
    "%s: a solo game ignores a stale wire seat",
    (mode) => {
      seatMe(mode, 1);
      const { result } = renderHook(() => usePlayerId());
      expect(result.current).toBe(PLAYER_ID);
      expect(getPlayerId()).toBe(PLAYER_ID);
    },
  );

  // Also green either way: the spectator split is a documented contract, not a
  // side effect. `HudBadges.tsx` states outright that `usePlayerId()` returns
  // `PLAYER_ID` in spectate mode and gates its second-person copy on
  // `useSpectatorMode()` because of it.
  it("keeps the spectator split: 255 from the getter, 0 from the hook", () => {
    seatMe("spectate", SPECTATOR_PLAYER_ID);
    const { result } = renderHook(() => usePlayerId());
    expect(getPlayerId()).toBe(SPECTATOR_PLAYER_ID);
    expect(result.current).toBe(PLAYER_ID);
  });
});
