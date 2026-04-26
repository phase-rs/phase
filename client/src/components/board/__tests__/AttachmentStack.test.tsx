import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useCardHover } from "../../../hooks/useCardHover.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { AttachmentStack } from "../AttachmentStack.tsx";

const firedRef = { current: false };

vi.mock("../../../hooks/useCardHover.ts", () => ({
  useCardHover: vi.fn(() => ({
    handlers: { "data-card-hover": true, onMouseEnter: vi.fn(), onMouseLeave: vi.fn() },
    firedRef,
  })),
}));

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(() => Promise.resolve()),
}));

// CardImage / ArtCropCard render a real <img> chain in production; replace
// with stub markers so the test asserts presence/aria-label without binding
// to the image component's internals.
vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div data-card-image aria-label={`card-image:${cardName}`} />
  ),
}));
vi.mock("../../card/ArtCropCard.tsx", () => ({
  ArtCropCard: ({ objectId }: { objectId: number }) => (
    <div data-art-crop aria-label={`art-crop:${objectId}`} />
  ),
}));

const mockedDispatchAction = vi.mocked(dispatchAction);
const mockedUseCardHover = vi.mocked(useCardHover);

function makeAttachment(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 50,
    card_id: 200,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: 1,
    attachments: [],
    counters: {},
    name: "Bonesplitter",
    power: null,
    toughness: null,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: null,
    base_toughness: null,
    base_keywords: [],
    base_color: [],
    timestamp: 1,
    entered_battlefield_turn: 1,
    ...overrides,
  };
}

function injectObjects(objects: GameObject[]) {
  const map: Record<number, GameObject> = {};
  for (const obj of objects) map[obj.id] = obj;
  useGameStore.setState({ gameState: { objects: map } as never });
}

function targetingState(legalIds: number[], player = 0): WaitingFor {
  return {
    type: "TargetSelection",
    data: {
      player,
      pending_cast: {} as never,
      target_slots: [],
      selection: {
        current_legal_targets: legalIds.map((id) => ({ Object: id })),
      } as never,
    },
  } as WaitingFor;
}

describe("AttachmentStack", () => {
  beforeEach(() => {
    firedRef.current = false;
    mockedUseCardHover.mockClear();
    mockedDispatchAction.mockClear();
    useUiStore.setState({ selectedObjectId: null });
    useGameStore.setState({ waitingFor: null });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders nothing when there are no attachments", () => {
    injectObjects([]);
    const { container } = render(<AttachmentStack objectIds={[]} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders one peek-card per attached object", () => {
    injectObjects([
      makeAttachment({ id: 50, name: "Bonesplitter" }),
      makeAttachment({ id: 51, name: "Sword of Fire and Ice", card_id: 201 }),
    ]);
    render(<AttachmentStack objectIds={[50, 51]} />);

    expect(screen.getByLabelText("Bonesplitter")).toBeInTheDocument();
    expect(screen.getByLabelText("Sword of Fire and Ice")).toBeInTheDocument();
  });

  it("dispatches ChooseTarget when a peek-card is clicked and is a legal target", () => {
    injectObjects([makeAttachment()]);
    useGameStore.setState({ waitingFor: targetingState([50]) });

    render(<AttachmentStack objectIds={[50]} />);
    fireEvent.click(screen.getByLabelText("Bonesplitter"));

    expect(mockedDispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 50 } },
    });
    expect(useUiStore.getState().selectedObjectId).toBeNull();
  });

  it("does not dispatch when the opponent is the prompted player (multiplayer gate)", () => {
    injectObjects([makeAttachment()]);
    useGameStore.setState({ waitingFor: targetingState([50], 1) });

    render(<AttachmentStack objectIds={[50]} />);
    fireEvent.click(screen.getByLabelText("Bonesplitter"));

    expect(mockedDispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().selectedObjectId).toBe(50);
  });

  it("falls through to selectObject when no targeting prompt matches the peek-card", () => {
    injectObjects([makeAttachment()]);
    render(<AttachmentStack objectIds={[50]} />);
    fireEvent.click(screen.getByLabelText("Bonesplitter"));

    expect(mockedDispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().selectedObjectId).toBe(50);
  });

  it("stops click propagation so the host card's onClick does not also fire", () => {
    injectObjects([makeAttachment()]);
    const hostClick = vi.fn();
    render(
      <div onClick={hostClick}>
        <AttachmentStack objectIds={[50]} />
      </div>,
    );

    fireEvent.click(screen.getByLabelText("Bonesplitter"));

    expect(hostClick).not.toHaveBeenCalled();
  });

  it("renders the targeting glow ring when the peek-card is a legal target", () => {
    injectObjects([makeAttachment()]);
    useGameStore.setState({ waitingFor: targetingState([50]) });

    render(<AttachmentStack objectIds={[50]} />);
    expect(screen.getByLabelText("Bonesplitter").className).toContain("ring-amber-300");
  });

  it("applies a subtype-tinted frame ring (amber for Aura, zinc for Equipment)", () => {
    injectObjects([
      makeAttachment({
        id: 50,
        name: "Bonesplitter",
        card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      }),
      makeAttachment({
        id: 51,
        card_id: 201,
        name: "Holy Mantle",
        card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
      }),
    ]);
    render(<AttachmentStack objectIds={[50, 51]} />);

    expect(screen.getByLabelText("Bonesplitter").className).toContain("ring-zinc-300");
    expect(screen.getByLabelText("Holy Mantle").className).toContain("ring-amber-400");
  });

  it("preserves the data-card-hover invariant for usePreviewDismiss", () => {
    injectObjects([makeAttachment()]);
    render(<AttachmentStack objectIds={[50]} />);

    expect(screen.getByLabelText("Bonesplitter")).toHaveAttribute("data-card-hover");
  });
});
