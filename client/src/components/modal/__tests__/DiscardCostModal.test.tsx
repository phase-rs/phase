import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPendingCast } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type PayCostWaitingFor = Extract<WaitingFor, { type: "PayCost" }>;
type EffectZoneChoiceWaitingFor = Extract<WaitingFor, { type: "EffectZoneChoice" }>;

const buildPayCostWaitingFor = (
  data: PayCostWaitingFor["data"],
): PayCostWaitingFor => ({
  type: "PayCost",
  data,
});

const buildEffectZoneChoiceWaitingFor = (
  data: EffectZoneChoiceWaitingFor["data"],
): EffectZoneChoiceWaitingFor => ({
  type: "EffectZoneChoice",
  data,
});

const cancellablePrompts: Array<[string, WaitingFor]> = [
  [
    "PayCost ExileFromZone",
    buildPayCostWaitingFor({
      player: 0,
      kind: { type: "ExileFromZone", zone: "Graveyard" },
      choices: [],
      count: 1,
      min_count: 0,
      resume: { type: "Spell", Spell: buildPendingCast() },
    }),
  ],
  [
    "CollectEvidenceChoice",
    {
      type: "CollectEvidenceChoice",
      data: {
        player: 0,
        minimum_mana_value: 1,
        cards: [],
        resume: {},
      },
    },
  ],
];

function makeObject(id: number, name: string, zone: GameObject["zone"] = "Hand"): GameObject {
  return buildGameObject({
    id,
    card_id: id,
    zone,
    name,
    timestamp: id,
  });
}

function setWaitingFor(waitingFor: WaitingFor, objects?: Record<string, GameObject>) {
  const state = buildGameState({
    objects: objects ?? {},
    waiting_for: waitingFor,
    has_pending_cast: true,
    next_object_id: 100,
  });
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

describe("Discard cost modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("allows cancelling discard costs", () => {
    setWaitingFor(
      buildPayCostWaitingFor({
        player: 0,
        kind: { type: "Discard" },
        choices: [],
        count: 1,
        min_count: 0,
        resume: { type: "Spell", Spell: buildPendingCast() },
      }),
    );

    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it.each(cancellablePrompts)("allows cancelling %s", (_label, waitingFor) => {
    setWaitingFor(waitingFor);

    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it.each([
    [
      "PayCost Sacrifice",
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "Sacrifice" },
          choices: [10],
          count: 1,
          min_count: 0,
          resume: { type: "Spell", Spell: buildPendingCast() },
        },
      } satisfies WaitingFor,
      { 10: makeObject(10, "Food Token", "Battlefield") },
    ],
    [
      "PayCost ReturnToHand",
      {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ReturnToHand" },
          choices: [10],
          count: 1,
          min_count: 0,
          resume: { type: "Spell", Spell: buildPendingCast() },
        },
      } satisfies WaitingFor,
      { 10: makeObject(10, "Kor Skyfisher", "Battlefield") },
    ],
    [
      "BlightChoice",
      {
        type: "BlightChoice",
        data: {
          player: 0,
          count: 1,
          creatures: [],
          pending_cast: buildPendingCast(),
        },
      } satisfies WaitingFor,
      {},
    ],
    [
      "HarmonizeTapChoice",
      {
        type: "HarmonizeTapChoice",
        data: {
          player: 0,
          eligible_creatures: [],
          pending_cast: buildPendingCast(),
        },
      } satisfies WaitingFor,
      {},
    ],
  ])("suppresses the modal for board-native %s", (_label, waitingFor, objects) => {
    setWaitingFor(waitingFor, objects);

    render(<CardChoiceModal />);

    expect(screen.queryByRole("button")).toBeNull();
  });

  it("handles discard prompts for mana ability costs", () => {
    setWaitingFor(
      buildPayCostWaitingFor({
        player: 0,
        kind: { type: "Discard" },
        choices: [],
        count: 1,
        min_count: 0,
        resume: { type: "ManaAbility", ManaAbility: {} },
      }),
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Discard for mana ability")).toBeInTheDocument();
  });

  it("describes untap selection without saying sacrifice", () => {
    setWaitingFor(
      buildEffectZoneChoiceWaitingFor({
          player: 0,
          cards: [10, 11],
          count: 5,
          min_count: 0,
          up_to: true,
          source_id: 1,
          effect_kind: "Untap",
          zone: "Battlefield",
      }),
      {
        10: { ...makeObject(10, "Island"), zone: "Battlefield" },
        11: { ...makeObject(11, "Forest"), zone: "Battlefield" },
      },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Untap")).toBeInTheDocument();
    expect(screen.getByText("Choose up to 5 permanents to untap")).toBeInTheDocument();
    expect(screen.queryByText(/sacrifice/i)).not.toBeInTheDocument();
  });

  it("describes optional attach selection without saying sacrifice and allows decline", () => {
    setWaitingFor(
      buildEffectZoneChoiceWaitingFor({
          player: 0,
          cards: [10, 11],
          count: 2,
          min_count: 0,
          up_to: true,
          source_id: 19,
          effect_kind: "Attach",
          zone: "Battlefield",
      }),
      {
        10: { ...makeObject(10, "S.H.I.E.L.D. Spy Kit"), zone: "Battlefield" },
        11: { ...makeObject(11, "Vibranium Energy Daggers"), zone: "Battlefield" },
      },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Attach")).toBeInTheDocument();
    expect(screen.getByText("Choose up to 2 Equipment to attach")).toBeInTheDocument();
    expect(screen.queryByText(/sacrifice/i)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Decline" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [] },
    });
  });

  it("describes library placement without saying battlefield", () => {
    setWaitingFor(
      buildEffectZoneChoiceWaitingFor({
        player: 0,
        cards: [],
        count: 2,
        min_count: 0,
        up_to: false,
        source_id: 1,
        effect_kind: "PutAtLibraryPosition",
        zone: "Hand",
      }),
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Put on Library")).toBeInTheDocument();
    expect(screen.getByText("Choose 2 cards to put on top of your library")).toBeInTheDocument();
    expect(screen.queryByText(/battlefield/i)).not.toBeInTheDocument();
  });

  it("describes exile selection without falling back to battlefield", () => {
    setWaitingFor(
      buildEffectZoneChoiceWaitingFor({
        player: 0,
        cards: [10],
        count: 1,
        min_count: 0,
        up_to: false,
        source_id: 1,
        effect_kind: "Exile",
        zone: "Hand",
        destination: "Exile",
      }),
      { 10: makeObject(10, "Exile Target") },
    );

    render(<CardChoiceModal />);

    expect(screen.getByText("Exile")).toBeInTheDocument();
    expect(screen.getByText("Choose 1 card to exile")).toBeInTheDocument();
    expect(screen.queryByText(/battlefield/i)).not.toBeInTheDocument();
  });

  it("allocates any-combination mana with color steppers", () => {
    setWaitingFor({
      type: "ChooseManaColor",
      data: {
        player: 0,
        choice: {
          type: "AnyCombination",
          data: { count: 3, options: ["White", "Blue"] },
        },
        context: { type: "ManaAbility", data: {} },
      },
    });

    render(<CardChoiceModal />);

    const confirm = screen.getByRole("button", { name: "Confirm" });
    expect(confirm).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Add White mana" }));
    fireEvent.click(screen.getByRole("button", { name: "Add White mana" }));
    fireEvent.click(screen.getByRole("button", { name: "Add Blue mana" }));

    expect(screen.getByText("3 / 3 mana selected")).toBeInTheDocument();
    expect(confirm).toBeEnabled();
    expect(screen.getByRole("button", { name: "Add White mana" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Remove Blue mana" }));
    expect(screen.getByText("2 / 3 mana selected")).toBeInTheDocument();
    expect(confirm).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Add Blue mana" }));

    fireEvent.click(confirm);

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseManaColor",
      data: {
        choice: { type: "Combination", data: ["White", "White", "Blue"] },
      },
    });
  });

  it("suppresses battlefield return choices for board-native selection", () => {
    setWaitingFor(
      buildEffectZoneChoiceWaitingFor({
          player: 0,
          cards: [10],
          count: 1,
          min_count: 0,
          up_to: false,
          source_id: 1,
          effect_kind: "ReturnToHand",
          zone: "Battlefield",
          destination: "Hand",
      }),
      {
        10: { ...makeObject(10, "Kor Skyfisher"), zone: "Battlefield" },
      },
    );

    render(<CardChoiceModal />);

    expect(screen.queryByRole("button")).toBeNull();
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("shows topdeck order and dispatches selected cards in click order", () => {
    setWaitingFor(
      buildEffectZoneChoiceWaitingFor({
          player: 0,
          cards: [10, 11],
          count: 2,
          min_count: 0,
          up_to: false,
          source_id: 1,
          effect_kind: "PutAtLibraryPosition",
          zone: "Hand",
      }),
      {
        10: makeObject(10, "First Card"),
        11: makeObject(11, "Second Card"),
      },
    );

    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: /Second Card/i }));
    fireEvent.click(screen.getByRole("button", { name: /First Card/i }));

    expect(screen.getByText("2nd")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Put on top (Top -> 2nd)" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [11, 10] },
    });
  });
});
