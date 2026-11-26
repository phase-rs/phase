import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

import type { GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
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

describe("ArchenemyPanel hover preview", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useGameStore.setState({ gameState: null });
    usePreferencesStore.setState({ cardPreviewHoverDelayMs: 0 });
    useUiStore.setState({ inspectedObjectId: null, hoveredObjectId: null, previewSticky: false });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    cleanup();
    useGameStore.setState({ gameState: null });
  });

  function hoverTarget(name: string): HTMLElement {
    const target = screen.getByRole("img", { name }).closest("[data-card-hover]");
    expect(target).not.toBeNull();
    return target as HTMLElement;
  }

  it("previews the active scheme on hover so its effect text can be read", () => {
    useGameStore.setState({ gameState: stateWithArchenemy() });

    render(<ArchenemyPanel />);
    fireEvent.pointerEnter(hoverTarget("Your Puny Minds Cannot Fathom"), {
      pointerType: "mouse",
    });

    expect(useUiStore.getState().inspectedObjectId).toBe(77);
  });

  it("closes the preview when the pointer leaves the thumbnail", () => {
    useGameStore.setState({ gameState: stateWithArchenemy() });

    render(<ArchenemyPanel />);
    const target = hoverTarget("Your Puny Minds Cannot Fathom");

    fireEvent.pointerEnter(target, { pointerType: "mouse" });
    fireEvent.pointerLeave(target, { pointerType: "mouse", relatedTarget: document.body });

    // The clear is deferred so a spurious leave from a layout shift can be
    // cancelled by a re-enter in the same frame.
    act(() => vi.advanceTimersByTime(50));
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });

  it("renders every accumulated ongoing scheme as its own hoverable thumbnail", () => {
    useGameStore.setState({
      gameState: buildGameState({
        objects: buildObjectMap(
          schemeObject(),
          schemeObject({ id: 78, card_id: 178, name: "Fear My Authority" }),
        ),
        next_object_id: 79,
        waiting_for: buildPriorityWaitingFor(),
        next_timestamp: 2,
        derived: {
          archenemy: {
            archenemy: 0,
            scheme_deck_count: 18,
            active_scheme_ids: [77, 78],
            hero_player_ids: [1, 2],
          },
        },
      }),
    });

    render(<ArchenemyPanel />);
    fireEvent.pointerEnter(hoverTarget("Fear My Authority"), { pointerType: "mouse" });

    expect(useUiStore.getState().inspectedObjectId).toBe(78);
  });
});
