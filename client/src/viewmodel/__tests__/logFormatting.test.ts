import { describe, expect, it } from "vitest";

import type { GameEvent } from "../../adapter/types";
import { classifyEventColor, filterByVerbosity, formatEvent } from "../logFormatting";

describe("formatEvent", () => {
  it("formats GameStarted", () => {
    expect(formatEvent({ type: "GameStarted" })).toBe("Game started");
  });

  it("formats TurnStarted with player and turn number", () => {
    expect(formatEvent({ type: "TurnStarted", data: { player_id: 0, turn_number: 3 } })).toBe(
      "Turn 3 -- Player 1",
    );
  });

  it("formats LifeChanged with sign prefix", () => {
    expect(formatEvent({ type: "LifeChanged", data: { player_id: 0, amount: 3 } })).toContain("+3");
    expect(formatEvent({ type: "LifeChanged", data: { player_id: 0, amount: -2 } })).toContain("-2");
  });

  it("formats DamageDealt to player", () => {
    const event: GameEvent = {
      type: "DamageDealt",
      data: { source_id: 1, target: { Player: 0 }, amount: 3 },
    };
    expect(formatEvent(event)).toContain("3 damage");
    expect(formatEvent(event)).toContain("Player 1");
  });

  it("formats DamageDealt to object", () => {
    const event: GameEvent = {
      type: "DamageDealt",
      data: { source_id: 1, target: { Object: 5 }, amount: 2 },
    };
    expect(formatEvent(event)).toContain("2 damage");
    expect(formatEvent(event)).toContain("object 5");
  });

  it("formats GameOver with winner", () => {
    expect(formatEvent({ type: "GameOver", data: { winner: 0 } })).toContain("Player 1 wins");
  });

  it("formats GameOver as draw", () => {
    expect(formatEvent({ type: "GameOver", data: { winner: null } })).toContain("Draw");
  });
});

describe("classifyEventColor", () => {
  it("classifies combat events as red", () => {
    expect(classifyEventColor({ type: "AttackersDeclared", data: { attacker_ids: [], defending_player: 1 } })).toBe("red");
    expect(classifyEventColor({ type: "BlockersDeclared", data: { assignments: [] } })).toBe("red");
    expect(classifyEventColor({ type: "DamageDealt", data: { source_id: 1, target: { Player: 0 }, amount: 3 } })).toBe("red");
  });

  it("classifies spell events as blue", () => {
    expect(classifyEventColor({ type: "SpellCast", data: { card_id: 1, controller: 0 } })).toBe("blue");
    expect(classifyEventColor({ type: "StackPushed", data: { object_id: 1 } })).toBe("blue");
    expect(classifyEventColor({ type: "StackResolved", data: { object_id: 1 } })).toBe("blue");
    expect(classifyEventColor({ type: "SpellCountered", data: { object_id: 1, countered_by: 2 } })).toBe("blue");
  });

  it("classifies life gain as green", () => {
    expect(classifyEventColor({ type: "LifeChanged", data: { player_id: 0, amount: 3 } })).toBe("green");
  });

  it("classifies life loss as red", () => {
    expect(classifyEventColor({ type: "LifeChanged", data: { player_id: 0, amount: -3 } })).toBe("red");
  });

  it("classifies zone events as gray", () => {
    expect(classifyEventColor({ type: "ZoneChanged", data: { object_id: 1, from: "Hand", to: "Battlefield" } })).toBe("gray");
    expect(classifyEventColor({ type: "Discarded", data: { player_id: 0, object_id: 1 } })).toBe("gray");
  });

  it("classifies unknown events as gray", () => {
    expect(classifyEventColor({ type: "PhaseChanged", data: { phase: "Draw" } })).toBe("gray");
  });
});

describe("filterByVerbosity", () => {
  const events: GameEvent[] = [
    { type: "GameStarted" },
    { type: "TurnStarted", data: { player_id: 0, turn_number: 1 } },
    { type: "PriorityPassed", data: { player_id: 0 } },
    { type: "ManaAdded", data: { player_id: 0, mana_type: "Green", source_id: 1 } },
    { type: "SpellCast", data: { card_id: 1, controller: 0 } },
    { type: "PermanentTapped", data: { object_id: 1 } },
    { type: "PermanentUntapped", data: { object_id: 1 } },
    { type: "DamageCleared", data: { object_id: 1 } },
    { type: "DamageDealt", data: { source_id: 1, target: { Player: 0 }, amount: 3 } },
    { type: "LifeChanged", data: { player_id: 0, amount: -3 } },
    { type: "AttackersDeclared", data: { attacker_ids: [1], defending_player: 1 } },
    { type: "BlockersDeclared", data: { assignments: [] } },
    { type: "CreatureDestroyed", data: { object_id: 1 } },
    { type: "TokenCreated", data: { object_id: 2, name: "Goblin" } },
    { type: "GameOver", data: { winner: 0 } },
  ];

  it("full returns all events", () => {
    expect(filterByVerbosity(events, "full")).toHaveLength(events.length);
  });

  it("compact excludes PriorityPassed, ManaAdded, PermanentTapped, PermanentUntapped, DamageCleared", () => {
    const compact = filterByVerbosity(events, "compact");
    const types = compact.map((e) => e.type);

    expect(types).not.toContain("PriorityPassed");
    expect(types).not.toContain("ManaAdded");
    expect(types).not.toContain("PermanentTapped");
    expect(types).not.toContain("PermanentUntapped");
    expect(types).not.toContain("DamageCleared");
    expect(compact).toHaveLength(events.length - 5);
  });

  it("minimal only includes key events", () => {
    const minimal = filterByVerbosity(events, "minimal");
    const types = minimal.map((e) => e.type);

    expect(types).toEqual([
      "GameStarted",
      "TurnStarted",
      "SpellCast",
      "DamageDealt",
      "LifeChanged",
      "AttackersDeclared",
      "BlockersDeclared",
      "CreatureDestroyed",
      "TokenCreated",
      "GameOver",
    ]);
  });
});
