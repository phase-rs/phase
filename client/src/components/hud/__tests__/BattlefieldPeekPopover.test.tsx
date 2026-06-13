import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { BattlefieldPeekPopover } from "../BattlefieldPeekPopover.tsx";

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div data-card-name={cardName} />
  ),
}));

function makeObject(id: number, name: string, power = 1, toughness = 1): GameObject {
  return {
    id,
    card_id: id,
    owner: 1,
    controller: 1,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power,
    toughness,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Green"],
    base_power: power,
    base_toughness: toughness,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: id,
    entered_battlefield_turn: null,
  };
}

function setState(objects: Record<number, GameObject>) {
  useGameStore.setState({
    gameState: {
      players: [{ id: 1 }],
      objects,
      battlefield: Object.keys(objects).map(Number),
      exile: [],
      stack: [],
      combat: null,
      waiting_for: { type: "Priority", data: { player: 0 } },
    } as unknown as GameState,
  });
}

describe("BattlefieldPeekPopover", () => {
  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameState: null });
  });

  it("groups identical battlefield objects behind one representative", () => {
    setState({
      1: makeObject(1, "Elf Warrior", 2, 2),
      2: makeObject(2, "Elf Warrior", 2, 2),
      3: makeObject(3, "Elvish Mystic", 1, 1),
    });
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
