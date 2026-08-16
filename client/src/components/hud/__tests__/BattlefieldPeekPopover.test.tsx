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

function setState(objects: GameObject[], unboundedPile: number[] = []) {
  useGameStore.setState({
    gameState: buildGameState({
      players: buildPlayers([1]),
      objects: buildObjectMap(...objects),
      battlefield: objects.map((object) => object.id),
      exile: [],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
      // CR 732.2a: engine-authored ∞-pile membership, mirrored from
      // `DerivedViews::unbounded_pile`. Only present when the loop is active.
      derived: unboundedPile.length > 0 ? { unbounded_pile: unboundedPile } : undefined,
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

  // CR 732.2a: an accepted object-growth loop's members render `∞`, not `×N`.
  // Discriminating: the pile group (Elf Warrior) and the non-pile group
  // (Elvish Mystic) have the SAME count (2). Only pile membership decides the
  // badge, so removing the `unboundedPileIds` thread in the popover flips the
  // ∞ badge to a second `×2` and the `getByText("∞")` assertion throws.
  it("renders ∞ for unbounded-pile members while non-pile groups keep ×N", () => {
    setState(
      [
        makeObject(1, "Elf Warrior", 2, 2),
        makeObject(2, "Elf Warrior", 2, 2),
        makeObject(3, "Elvish Mystic", 1, 1),
        makeObject(4, "Elvish Mystic", 1, 1),
      ],
      [1, 2],
    );
    render(
      <BattlefieldPeekPopover
        playerId={1}
        opponentName="Lathril"
        seatColor="#a78bfa"
        isTargeting={false}
        legalTargetIds={[]}
      />,
    );

    // Pile group: ∞ instead of ×2.
    expect(screen.getByText("∞")).toBeInTheDocument();
    // Non-pile group of the same count still reads ×2 (proves ∞ is not blanket).
    expect(screen.getByText("×2")).toBeInTheDocument();
  });

  // CR 732.2a: pile membership is count-independent — a single visible member
  // still reads `∞` (main-board "singleton trap", GroupedPermanent.tsx). A plain
  // singleton renders no badge, so a bare `getByText("∞")` here is non-vacuous.
  it("renders ∞ for a single-member unbounded pile (count-independent)", () => {
    setState([makeObject(7, "Pest", 1, 1)], [7]);
    render(
      <BattlefieldPeekPopover
        playerId={1}
        opponentName="Lathril"
        seatColor="#a78bfa"
        isTargeting={false}
        legalTargetIds={[]}
      />,
    );

    expect(screen.getByText("∞")).toBeInTheDocument();
    expect(screen.queryByText("×1")).not.toBeInTheDocument();
  });
});
