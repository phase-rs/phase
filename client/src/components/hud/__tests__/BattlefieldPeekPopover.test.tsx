import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { BattlefieldPeekPopover } from "../BattlefieldPeekPopover.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div data-card-name={cardName} />
  ),
}));

function makeObject(id: number, name: string, power = 1, toughness = 1): GameObject {
  return buildGameObjectWithCoreTypes(["Creature"], {
    id,
    card_id: id,
    owner: 1,
    controller: 1,
    zone: "Battlefield",
    name,
    power,
    toughness,
    color: ["Green"],
    base_power: power,
    base_toughness: toughness,
    base_color: ["Green"],
    timestamp: id,
    entered_battlefield_turn: null,
  });
}

function setState(objects: GameObject[]) {
  useGameStore.setState({
    gameState: buildGameState({
      players: buildPlayers([1]),
      objects: buildObjectMap(...objects),
      battlefield: objects.map((object) => object.id),
      exile: [],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
    }),
  });
}

describe("BattlefieldPeekPopover", () => {
  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameState: null });
  });

  it("groups identical battlefield objects behind one representative", () => {
    setState([
      makeObject(1, "Elf Warrior", 2, 2),
      makeObject(2, "Elf Warrior", 2, 2),
      makeObject(3, "Elvish Mystic", 1, 1),
    ]);
    const { container } = render(
      <BattlefieldPeekPopover
        playerId={1}
        opponentName="Lathril"
        seatColor="#a78bfa"
        isTargeting={false}
        legalTargetIds={[]}
      />,
    );

    expect(container.querySelectorAll('[data-card-name="Elf Warrior"]')).toHaveLength(1);
    expect(container.querySelectorAll("[data-card-name]")).toHaveLength(2);
    expect(screen.getByText("×2")).toBeInTheDocument();
  });
});
