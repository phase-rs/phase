import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { ExilePile } from "../ExilePile.tsx";
import { GraveyardPile } from "../GraveyardPile.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

afterEach(() => {
  cleanup();
  useGameStore.setState({ gameState: null, waitingFor: null, legalActionsByObject: {} });
});

describe("zone pile contextual anchors", () => {
  it("groups the public graveyard members represented by its pile", () => {
    const first = buildGameObject({ id: 101, zone: "Graveyard", entered_battlefield_turn: null });
    const top = buildGameObject({ id: 102, zone: "Graveyard", entered_battlefield_turn: null });
    const gameState = buildGameState({
      players: buildPlayers([{ id: 0, graveyard: [first.id, top.id] }, 1]),
      objects: buildObjectMap(first, top),
      battlefield: [],
      exile: [],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
    });
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for, legalActionsByObject: {} });

    const { container } = render(<GraveyardPile playerId={0} onClick={vi.fn()} />);

    expect(container.querySelector('[data-graveyard-pile="0"]')).toHaveAttribute(
      "data-grouped-ids",
      "101 102",
    );
  });

  it("anchors a shared graveyard on its holder whichever seat the pile is asked for", () => {
    const first = buildGameObject({ id: 101, zone: "Graveyard", entered_battlefield_turn: null });
    const top = buildGameObject({ id: 102, zone: "Graveyard", entered_battlefield_turn: null });
    const gameState = buildGameState({
      players: buildPlayers([{ id: 0, graveyard: [first.id, top.id] }, { id: 1, graveyard: [] }]),
      objects: buildObjectMap(first, top),
      battlefield: [],
      exile: [],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
      derived: { shared_piles: { library: 0, graveyard: 0 } },
    });
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for, legalActionsByObject: {} });

    const { container } = render(<GraveyardPile playerId={1} onClick={vi.fn()} />);

    expect(container.querySelector('[data-graveyard-pile="0"]')).toHaveAttribute(
      "data-grouped-ids",
      "101 102",
    );
    expect(container.querySelector('[data-graveyard-pile="1"]')).toBeNull();
  });

  it("keeps a per-seat graveyard on its own seat when no holder is published", () => {
    const own = buildGameObject({ id: 201, zone: "Graveyard", entered_battlefield_turn: null });
    const gameState = buildGameState({
      players: buildPlayers([{ id: 0, graveyard: [] }, { id: 1, graveyard: [own.id] }]),
      objects: buildObjectMap(own),
      battlefield: [],
      exile: [],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
    });
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for, legalActionsByObject: {} });

    const { container } = render(<GraveyardPile playerId={1} onClick={vi.fn()} />);

    expect(container.querySelector('[data-graveyard-pile="1"]')).toHaveAttribute(
      "data-grouped-ids",
      "201",
    );
  });

  it("groups only exile cards whose identities the pile can represent", () => {
    const faceUp = buildGameObject({ id: 201, zone: "Exile", entered_battlefield_turn: null });
    const hidden = buildGameObject({
      id: 202,
      zone: "Exile",
      face_down: true,
      display_visible_to_viewer: false,
      entered_battlefield_turn: null,
    });
    const gameState = buildGameState({
      players: buildPlayers([0, 1]),
      objects: buildObjectMap(faceUp, hidden),
      battlefield: [],
      exile: [faceUp.id, hidden.id],
      stack: [],
      waiting_for: buildPriorityWaitingFor(),
      derived: { visible_exile_object_ids: { 0: [faceUp.id] } },
    });
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for, legalActionsByObject: {} });

    const { container } = render(<ExilePile playerId={0} onClick={vi.fn()} />);

    expect(container.querySelector("button")).toHaveAttribute("data-grouped-ids", "201");
  });
});
