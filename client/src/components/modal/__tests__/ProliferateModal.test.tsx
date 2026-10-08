import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildCommanderFormatConfig,
  buildGameState,
  buildPlayer,
} from "../../../test/factories/gameStateFactory.ts";

type ChooseObjectsWaitingFor = Extract<WaitingFor, { type: "ChooseObjectsSelection" }>;

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string) {
  return buildGameObject({
    id,
    card_id: 1,
    counters: { "+1/+1": 1 },
    name,
    power: 1,
    toughness: 1,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    base_power: 1,
    base_toughness: 1,
    timestamp: 1,
    entered_battlefield_turn: 1,
  });
}

function makeState(eligible: TargetRef[]): GameState {
  return buildGameState({
    players: [buildPlayer({ id: 0, life: 40 }), buildPlayer({ id: 1, life: 40 })],
    format_config: buildCommanderFormatConfig(),
    objects: buildObjectMap(
      makeObject(42, "Walking Ballista"),
      makeObject(43, "Hangarback Walker"),
      makeObject(44, "Triskelion"),
    ),
    next_object_id: 100,
    battlefield: [42, 43, 44],
    waiting_for: {
      type: "ProliferateChoice",
      data: { player: 0, eligible },
    },
    next_timestamp: 2,
  });
}

function setUpWaitingFor(waitingFor: ChooseObjectsWaitingFor) {
  const state = makeState(waitingFor.data.eligible);
  state.waiting_for = waitingFor;
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

function setUp(eligible: TargetRef[]) {
  const state = makeState(eligible);
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor: state.waiting_for,
  });
}

describe("ProliferateModal (via CardChoiceModal)", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders mixed Object + Player eligible labels", () => {
    setUp([{ Object: 42 }, { Object: 43 }, { Player: 1 }]);
    render(<CardChoiceModal />);

    expect(screen.getByText(/Proliferate/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Walking Ballista" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Hangarback Walker" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Opp 2" })).toBeInTheDocument();
  });

  it("defaults to all eligible selected and dispatches the full set", () => {
    const eligible: TargetRef[] = [{ Object: 42 }, { Player: 1 }];
    setUp(eligible);
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: eligible },
    });
  });

  it("allows deselecting all and dispatching zero targets (CR 701.34a)", () => {
    setUp([{ Object: 42 }, { Player: 1 }]);
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Walking Ballista" }));
    fireEvent.click(screen.getByRole("button", { name: "Opp 2" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: [] },
    });
  });

  it("dispatches the partial subset after toggling one off", () => {
    setUp([{ Object: 42 }, { Object: 43 }, { Player: 1 }]);
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Hangarback Walker" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: [{ Object: 42 }, { Player: 1 }] },
    });
  });

  it("renders nothing when the current player cannot act", () => {
    useMultiplayerStore.setState({ activePlayerId: 1 });
    setUp([{ Object: 42 }]);
    const { container } = render(<CardChoiceModal />);
    expect(container).toBeEmptyDOMElement();
  });

  it("caps ChooseObjects default, toggles, and Select All at max", () => {
    const eligible: TargetRef[] = [{ Object: 42 }, { Object: 43 }, { Object: 44 }];
    setUpWaitingFor({
      type: "ChooseObjectsSelection",
      data: { player: 0, eligible, min: 0, max: 2 },
    });
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Triskelion" }));
    fireEvent.click(screen.getByRole("button", { name: "All" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: eligible.slice(0, 2) },
    });
  });

  it("disables confirmation below a required minimum without locking reselection", () => {
    const eligible: TargetRef[] = [{ Object: 42 }, { Object: 43 }, { Object: 44 }];
    setUpWaitingFor({
      type: "ChooseObjectsSelection",
      data: { player: 0, eligible, min: 2, max: 3 },
    });
    render(<CardChoiceModal />);

    expect(screen.queryByRole("button", { name: "None" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Triskelion" }));
    fireEvent.click(screen.getByRole("button", { name: "Walking Ballista" }));
    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Triskelion" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: eligible.slice(1) },
    });
  });

  it("can replace a default target when the exact minimum equals the maximum", () => {
    const eligible: TargetRef[] = [{ Object: 42 }, { Object: 43 }, { Object: 44 }];
    setUpWaitingFor({
      type: "ChooseObjectsSelection",
      data: { player: 0, eligible, min: 2, max: 2 },
    });
    render(<CardChoiceModal />);

    expect(screen.getByRole("button", { name: "Triskelion" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Walking Ballista" }));
    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Triskelion" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Triskelion" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: eligible.slice(1) },
    });
  });
});
