import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameState, WaitingFor } from "../../../adapter/types.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  opponentMayChoiceWaitingForFactory,
  optionalEffectChoiceWaitingForFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { OptionalEffectModalContent } from "../OptionalEffectModal.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn(() => ({
    src: null,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));

type OptionalEffectWaitingFor = Extract<
  WaitingFor,
  { type: "OptionalEffectChoice" | "OpponentMayChoice" }
>;
type OptionalEffectChoiceWaitingFor = Extract<WaitingFor, { type: "OptionalEffectChoice" }>;

function optionalWaitingFor(
  mayTriggerKey?: OptionalEffectChoiceWaitingFor["data"]["may_trigger_key"],
  sameCardAvailable = false,
): OptionalEffectChoiceWaitingFor {
  return optionalEffectChoiceWaitingForFactory
    .withData({
      player: 0,
      source_id: 100,
      description: "You may gain 1 life.",
      may_trigger_key: mayTriggerKey,
      same_card_may_trigger_choice_available: sameCardAvailable,
    })
    .build();
}

function renderModal(waitingFor: OptionalEffectWaitingFor, objects?: GameState["objects"]) {
  const dispatch = vi.fn<(action: GameAction) => void>();
  render(
    <OptionalEffectModalContent
      waitingFor={waitingFor}
      objects={objects}
      dispatch={dispatch}
    />,
  );
  return dispatch;
}

afterEach(() => {
  cleanup();
});

describe("OptionalEffectModalContent", () => {
  it.each([
    ["OptionalEffectChoice", optionalEffectChoiceWaitingForFactory],
    ["OpponentMayChoice", opponentMayChoiceWaitingForFactory],
  ] as const)("titles %s from its source while previewing its decision subject", (_type, factory) => {
    const bre = gameObjectFactory
      .creature(2, 2)
      .legendary()
      .named("Bre of Clan Stoutarm")
      .withId(100)
      .build();
    const bears = gameObjectFactory
      .creature(2, 2)
      .inExile()
      .named("Grizzly Bears")
      .withId(44)
      .build();
    const waitingFor = factory
      .withData({ source_id: bre.id, decision_subject_id: bears.id })
      .build();

    renderModal(waitingFor, { [bre.id]: bre, [bears.id]: bears });

    expect(
      screen.getByRole("dialog", { name: "Bre of Clan Stoutarm - Optional Effect" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("img", { name: "Grizzly Bears" })).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "Bre of Clan Stoutarm" })).not.toBeInTheDocument();
  });

  it.each([undefined, 999])(
    "falls back to the source preview when the decision subject is %s",
    (decisionSubjectId) => {
      const bre = gameObjectFactory
        .creature(2, 2)
        .legendary()
        .named("Bre of Clan Stoutarm")
        .withId(100)
        .build();
      const waitingFor = optionalEffectChoiceWaitingForFactory
        .withData({ source_id: bre.id, decision_subject_id: decisionSubjectId })
        .build();

      renderModal(waitingFor, { [bre.id]: bre });

      expect(screen.getByRole("img", { name: "Bre of Clan Stoutarm" })).toBeInTheDocument();
    },
  );

  it("hides remember control for unkeyed prompts and dispatches the existing action", () => {
    const dispatch = renderModal(optionalWaitingFor());

    expect(screen.queryByLabelText("Don't ask again this game")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: true },
    });
  });

  it("keeps keyed prompts on the existing action while remember is unchecked", () => {
    const dispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    expect(screen.getByLabelText("Don't ask again this game")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "No" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffect",
      data: { accept: false },
    });
  });

  it("dispatches remembered accept and decline actions when checked", () => {
    const dispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    fireEvent.click(screen.getByLabelText("Don't ask again this game"));
    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "ExactInstance" } },
    });

    cleanup();
    const declineDispatch = renderModal(
      optionalWaitingFor({
        player: 0,
        source_id: 100,
        origin: { type: "Printed", trigger_index: 0 },
      }),
    );

    fireEvent.click(screen.getByLabelText("Don't ask again this game"));
    fireEvent.click(screen.getByRole("button", { name: "No" }));

    expect(declineDispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Decline" }, scope: { type: "ExactInstance" } },
    });
  });

  it("shows the engine-gated same-card checkbox and dispatches that scope", () => {
    const dispatch = renderModal(
      optionalWaitingFor(
        {
          player: 0,
          source_id: 100,
          origin: { type: "Printed", trigger_index: 0 },
        },
        true,
      ),
    );

    const rememberCheckbox = screen.getByLabelText("Don't ask again this game");
    const sameCardCheckbox = screen.getByLabelText(
      "Use this choice for every copy of this card this game",
    );

    fireEvent.click(sameCardCheckbox);

    expect(sameCardCheckbox).toBeChecked();
    expect(rememberCheckbox).toBeChecked();

    fireEvent.click(screen.getByRole("button", { name: "Yes" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "DecideOptionalEffectAndRemember",
      data: { choice: { type: "Accept" }, scope: { type: "SameCard" } },
    });
  });

  it("resets the same-card checkbox for each prompt", () => {
    const keyed: NonNullable<OptionalEffectChoiceWaitingFor["data"]["may_trigger_key"]> = {
      player: 0,
      source_id: 100,
      origin: { type: "Printed", trigger_index: 0 },
    };
    const { rerender } = render(
      <OptionalEffectModalContent waitingFor={optionalWaitingFor(keyed, true)} dispatch={vi.fn()} />,
    );

    const sameCardLabel = "Use this choice for every copy of this card this game";
    const rememberLabel = "Don't ask again this game";
    fireEvent.click(screen.getByLabelText(sameCardLabel));
    expect(screen.getByLabelText(sameCardLabel)).toBeChecked();
    expect(screen.getByLabelText(rememberLabel)).toBeChecked();

    rerender(
      <OptionalEffectModalContent
        waitingFor={{ ...optionalWaitingFor(keyed, true), data: { ...optionalWaitingFor(keyed, true).data, source_id: 101 } }}
        dispatch={vi.fn()}
      />,
    );
    expect(screen.getByLabelText(sameCardLabel)).not.toBeChecked();
    expect(screen.getByLabelText(rememberLabel)).not.toBeChecked();
  });
});
