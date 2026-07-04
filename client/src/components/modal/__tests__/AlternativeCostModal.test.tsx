import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  GameObject,
  ManaCost,
  WaitingFor,
} from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { AlternativeCostModal } from "../AlternativeCostModal.tsx";

const dispatchMock = vi.fn();

const RED_COST: ManaCost = { type: "Cost", shards: ["Red"], generic: 0 };

function makeObject(id: number, name: string): GameObject {
  return buildGameObjectWithCoreTypes(["Sorcery"], {
    id,
    card_id: id,
    zone: "Hand",
    name,
    mana_cost: RED_COST,
    color: ["Red"],
    base_color: ["Red"],
    timestamp: 1,
    entered_battlefield_turn: null,
  });
}

type AltKeyword = Extract<
  WaitingFor,
  { type: "AlternativeCastChoice" }
>["data"]["keyword"]["type"];

function setSpectacleChoice(keyword: AltKeyword) {
  const waitingFor: WaitingFor = {
    type: "AlternativeCastChoice",
    data: {
      player: 0,
      object_id: 52,
      card_id: 52,
      keyword: { type: keyword },
      normal_cost: { type: "Cost", shards: ["Red"], generic: 3 },
      alternative_cost: RED_COST,
      alternative_additional_cost: null,
    },
  };

  const gameState = buildGameState({
    active_player: 0,
    objects: buildObjectMap(makeObject(52, "Light Up the Stage")),
    priority_player: 0,
    turn_decision_controller: 0,
    waiting_for: waitingFor,
  });

  useGameStore.setState({
    gameState,
    waitingFor,
    dispatch: dispatchMock,
    gameMode: "ai",
  });
}

describe("AlternativeCostModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  // Regression for issue #2939: the engine emits `keyword.type === "Spectacle"`
  // for Light Up the Stage, but the modal's keyword switch did not handle it and
  // returned `undefined`, which threw on `copy.eyebrow` and black-screened the
  // client. The Spectacle case must now render without throwing.
  it("renders the Spectacle alternative-cost prompt without crashing", () => {
    setSpectacleChoice("Spectacle");

    expect(() => render(<AlternativeCostModal />)).not.toThrow();

    expect(screen.getByText("Spectacle")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Cast with Spectacle/ }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /Cast Normally/ }),
    ).toBeInTheDocument();
  });

  it("dispatches the Alternative choice when the Spectacle button is clicked", () => {
    setSpectacleChoice("Spectacle");
    render(<AlternativeCostModal />);

    fireEvent.click(screen.getByRole("button", { name: /Cast with Spectacle/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseAlternativeCast",
      data: { choice: { type: "Alternative" } },
    });

    fireEvent.click(screen.getByRole("button", { name: /Cast Normally/ }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "ChooseAlternativeCast",
      data: { choice: { type: "Normal" } },
    });
  });

  // The other engine `AlternativeCastKeyword` variants that the FE switch had
  // also been missing must likewise render without throwing.
  it.each<AltKeyword>(["Emerge", "Impending", "Prototype", "Prowl"])(
    "renders the %s prompt without crashing",
    (keyword) => {
      setSpectacleChoice(keyword);
      expect(() => render(<AlternativeCostModal />)).not.toThrow();
      expect(
        screen.getByRole("button", { name: /Cast Normally/ }),
      ).toBeInTheDocument();
    },
  );
});
