import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { ArchenemyPanel } from "../ArchenemyPanel.tsx";

function schemeObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObject({
    id: 77,
    card_id: 177,
    zone: "Command",
    name: "Your Puny Minds Cannot Fathom",
    card_types: { supertypes: [], core_types: ["Scheme"], subtypes: [] },
    entered_battlefield_turn: null,
    is_commander: false,
    commander_tax: 0,
    ...overrides,
  });
}

function stateWithArchenemy(overrides = {}) {
  return buildGameState({
    objects: buildObjectMap(schemeObject()),
    next_object_id: 78,
    waiting_for: buildPriorityWaitingFor(),
    next_timestamp: 2,
    derived: {
      archenemy: {
        archenemy: 0,
        scheme_deck_count: 19,
        active_scheme_ids: [77],
        hero_player_ids: [1, 2],
      },
    },
    ...overrides,
  });
}

describe("ArchenemyPanel", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: null });
  });

  afterEach(() => {
    cleanup();
    useGameStore.setState({ gameState: null });
  });

  it("does not render outside Archenemy", () => {
    const { container } = render(<ArchenemyPanel />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders the engine-derived active scheme view", () => {
    useGameStore.setState({ gameState: stateWithArchenemy() });

    render(<ArchenemyPanel />);

    expect(screen.getByText("Active scheme")).toBeInTheDocument();
    expect(screen.getByText("Your Puny Minds Cannot Fathom")).toBeInTheDocument();
    expect(screen.getByText("19 in deck")).toBeInTheDocument();
    expect(screen.getByText("2 heroes")).toBeInTheDocument();
  });
});
