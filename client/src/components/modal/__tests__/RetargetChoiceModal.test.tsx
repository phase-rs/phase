import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import {
  buildCommanderFormatConfig,
  buildGameState,
  buildPlayer,
  retargetChoiceWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";

type RetargetChoiceWaitingFor = Extract<WaitingFor, { type: "RetargetChoice" }>;

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function setUp(data: Partial<RetargetChoiceWaitingFor["data"]>) {
  const waitingFor = retargetChoiceWaitingForFactory.withData(data).build();
  const state: GameState = buildGameState({
    players: [buildPlayer({ id: 0, life: 40 }), buildPlayer({ id: 1, life: 40 })],
    format_config: buildCommanderFormatConfig(),
    waiting_for: waitingFor,
  });
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

const opp1: TargetRef = { Player: 0 };
const opp2: TargetRef = { Player: 1 };

describe("RetargetChoiceModal (via CardChoiceModal)", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  // CR 115.7d + INVARIANT SC (phase-rs/phase#8355 round-8 review finding
  // MED-2): admission for an `All`-scope submission is PER-SLOT
  // (`engine::apply_retarget`'s `pool_for`), but this modal used to render
  // the FLAT UNION for whichever slot was active — measured on a multi-slot
  // prompt where the union had more entries than the active slot's own
  // pool, so a rendered choice could be rejected by the reducer on click.
  // Only `All`-scope prompts reach this modal at all (`CardChoiceModal`
  // routes `Single` scope to the board's `TargetingOverlay` instead), so
  // this is the modal's only reachable retarget shape.
  it("renders only the active slot's own pool, not the flat union", () => {
    setUp({
      player: 0,
      scope: { type: "All" },
      current_targets: [opp2, opp1],
      slot_pools: [[opp2], [opp1, opp2]],
      legal_new_targets: [opp1, opp2],
    });
    render(<CardChoiceModal />);

    // Active slot defaults to 0, whose OWN pool (`slot_pools[0]`) is
    // `[opp2]` only. `opp1` is in the flat union but NOT slot 0's pool, so
    // it must not be offered as a choice for this slot. `opp2` is also the
    // default selection, so its button carries an extra "New target" badge
    // — match on a name PREFIX rather than the exact label.
    expect(screen.getByRole("button", { name: /^Opp 2/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Opp 1/ })).not.toBeInTheDocument();
  });

  // Paired positive control: an outer-empty `slot_pools` (a compat payload
  // predating the field, INVARIANT SC) falls back to the union — the fix
  // above must not turn this row's absence into a silent "offer nothing."
  it("falls back to the flat union when slot_pools is outer-empty (compat payload)", () => {
    setUp({
      player: 0,
      scope: { type: "All" },
      current_targets: [opp2, opp1],
      slot_pools: [],
      legal_new_targets: [opp1, opp2],
    });
    render(<CardChoiceModal />);

    expect(screen.getByRole("button", { name: /^Opp 1/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^Opp 2/ })).toBeInTheDocument();
  });
});
