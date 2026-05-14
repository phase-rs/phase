import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
}));

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName, faceDown }: { cardName: string; faceDown?: boolean }) => (
    <div
      aria-label={faceDown ? "Face-down card" : cardName}
      data-face-down={faceDown ? "true" : "false"}
      style={{ height: "var(--card-h)", width: "var(--card-w)" }}
    />
  ),
}));

function makeObject(overrides: Partial<GameObject> = {}): GameObject {
  return {
    id: 1,
    card_id: 100,
    owner: 0,
    controller: 0,
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
    name: "Test Creature",
    power: 2,
    toughness: 2,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "Cost", shards: ["Green"], generic: 1 },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Green"],
    base_power: 2,
    base_toughness: 2,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: 1,
    entered_battlefield_turn: null,
    ...overrides,
  };
}

function makeState(): GameState {
  const host = makeObject({ id: 1, attachments: [2] });
  const equipment = makeObject({
    id: 2,
    card_id: 200,
    attached_to: 1,
    attachments: [3],
    name: "Test Equipment",
    power: null,
    toughness: null,
    base_power: null,
    base_toughness: null,
    card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
    color: [],
    base_color: [],
  });
  const aura = makeObject({
    id: 3,
    card_id: 300,
    attached_to: 2,
    attachments: [],
    name: "Test Aura",
    power: null,
    toughness: null,
    base_power: null,
    base_toughness: null,
    card_types: { supertypes: [], core_types: ["Enchantment"], subtypes: ["Aura"] },
    color: ["Blue"],
    base_color: ["Blue"],
  });

  return {
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    objects: { 1: host, 2: equipment, 3: aura },
    battlefield: [1, 2, 3],
    exile: [],
    stack: [],
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
  } as unknown as GameState;
}

function renderPermanent(validTargetObjectIds = new Set<number>()) {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        committedAttackerIds: new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set(),
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds: new Set(),
        validAttackerIds: new Set(),
        validTargetObjectIds,
      }}
    >
      <PermanentCard objectId={1} />
    </BoardInteractionContext.Provider>,
  );
}

describe("PermanentCard attachments", () => {
  beforeEach(() => {
    const gameState = makeState();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });
    useUiStore.setState({
      selectedObjectId: null,
      hoveredObjectId: null,
      inspectedObjectId: null,
      combatMode: null,
      selectedAttackers: [],
      blockerAssignments: new Map(),
      combatClickHandler: null,
      selectedCardIds: [],
      pendingAbilityChoice: null,
    });
    usePreferencesStore.setState({
      battlefieldCardDisplay: "full_card",
      showKeywordStrip: false,
      tapRotation: "classic",
    });
    vi.mocked(dispatchAction).mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("lifts the permanent tree above siblings while keeping attachments behind the host", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;
    const attachment = container.querySelector('[data-object-id="2"]') as HTMLElement;
    const attachmentLayer = attachment.parentElement as HTMLElement;
    const nestedAttachment = container.querySelector('[data-object-id="3"]') as HTMLElement;
    const nestedAttachmentLayer = nestedAttachment.parentElement as HTMLElement;

    expect(host.style.zIndex).toBe("");
    expect(attachmentLayer.style.zIndex).toBe("5");
    expect(nestedAttachmentLayer.style.zIndex).toBe("5");

    fireEvent.mouseEnter(host);

    expect(host.style.zIndex).toBe("80");
    expect(attachmentLayer.style.zIndex).toBe("5");
    expect(nestedAttachmentLayer.style.zIndex).toBe("5");
  });

  it("keeps the attachment tree lifted while a nested attachment is hovered", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;
    const nestedAttachment = container.querySelector('[data-object-id="3"]') as HTMLElement;

    fireEvent.mouseEnter(nestedAttachment);

    expect(host.style.zIndex).toBe("80");
  });

  it("restores host preview when moving from an attachment back to its host", () => {
    const { container } = renderPermanent();
    const host = container.querySelector('[data-object-id="1"]') as HTMLElement;
    const attachment = container.querySelector('[data-object-id="2"]') as HTMLElement;

    fireEvent.mouseEnter(host);
    expect(useUiStore.getState().inspectedObjectId).toBe(1);

    fireEvent.mouseEnter(attachment);
    expect(useUiStore.getState().inspectedObjectId).toBe(2);

    fireEvent.mouseLeave(attachment, { relatedTarget: host });
    expect(useUiStore.getState().inspectedObjectId).toBe(1);
    expect(useUiStore.getState().hoveredObjectId).toBe(1);
  });

  it("targets the attached permanent itself when the attachment is clicked", () => {
    const { container } = renderPermanent(new Set([2]));
    const attachment = container.querySelector('[data-object-id="2"]') as HTMLElement;

    fireEvent.click(attachment);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 2 } },
    });
  });

  it("opens the ability picker when a land has mana actions plus a non-mana activated ability", () => {
    const kessig = makeObject({
      id: 39,
      name: "Kessig Wolf Run",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: {
        supertypes: [],
        core_types: ["Land"],
        subtypes: ["Plains", "Island", "Swamp", "Mountain", "Forest"],
      },
      mana_cost: { type: "NoCost" },
      color: [],
      base_color: [],
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "{T}: Add {C}.",
          effect: {
            type: "Mana",
            produced: { type: "Colorless" },
          },
        },
        {
          kind: "Activated",
          cost: {
            type: "Composite",
            costs: [
              {
                type: "Mana",
                cost: { type: "Cost", shards: ["X", "Red", "Green"], generic: 0 },
              },
              { type: "Tap" },
            ],
          },
          description: "{X}{R}{G}, {T}: Target creature gets +X/+0 and gains trample until end of turn.",
          effect: { type: "GenericEffect" },
        },
      ] as unknown as GameObject["abilities"],
    });

    const gameState = {
      ...makeState(),
      objects: { 39: kessig },
      battlefield: [39],
    } as unknown as GameState;
    const manaAction = { type: "TapLandForMana", data: { object_id: 39 } } as const;
    const abilityAction = {
      type: "ActivateAbility",
      data: { source_id: 39, ability_index: 1 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [manaAction, abilityAction],
      legalActionsByObject: { 39: [manaAction, abilityAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set([39]),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([39]),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={39} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="39"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 39,
      actions: [abilityAction, manaAction],
    });
  });

  it("opens the ability picker when a land has multiple mana abilities", () => {
    const holdout = makeObject({
      id: 40,
      name: "Holdout Settlement",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: {
        supertypes: [],
        core_types: ["Land"],
        subtypes: [],
      },
      mana_cost: { type: "NoCost" },
      color: [],
      base_color: [],
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "{T}: Add {C}.",
          effect: {
            type: "Mana",
            produced: { type: "Colorless" },
          },
        },
        {
          kind: "Activated",
          cost: {
            type: "Composite",
            costs: [
              { type: "Tap" },
              {
                type: "TapCreatures",
                count: 1,
              },
            ],
          },
          description: "{T}, Tap an untapped creature you control: Add one mana of any color.",
          effect: {
            type: "Mana",
            produced: {
              type: "AnyOneColor",
              count: { type: "Fixed", value: 1 },
              color_options: ["White", "Blue", "Black", "Red", "Green"],
            },
          },
        },
      ] as unknown as GameObject["abilities"],
    });

    const gameState = {
      ...makeState(),
      objects: { 40: holdout },
      battlefield: [40],
    } as unknown as GameState;
    const colorlessAction = {
      type: "ActivateAbility",
      data: { source_id: 40, ability_index: 0 },
    } as const;
    const anyColorAction = {
      type: "ActivateAbility",
      data: { source_id: 40, ability_index: 1 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [colorlessAction, anyColorAction],
      legalActionsByObject: { 40: [colorlessAction, anyColorAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([40]),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={40} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="40"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 40,
      actions: [colorlessAction, anyColorAction],
    });
  });

  it("opens the ability picker when a convoke creature can pay colored or generic mana", () => {
    const helper = makeObject({
      id: 41,
      name: "Conclave Helper",
      color: ["Green"],
      base_color: ["Green"],
    });

    const gameState = {
      ...makeState(),
      objects: { 41: helper },
      battlefield: [41],
    } as unknown as GameState;
    const genericAction = {
      type: "TapForConvoke",
      data: { object_id: 41, mana_type: "Colorless" },
    } as const;
    const greenAction = {
      type: "TapForConvoke",
      data: { object_id: 41, mana_type: "Green" },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: {
        type: "ManaPayment",
        data: { player: 0, convoke_mode: "Convoke" },
      },
      legalActions: [genericAction, greenAction],
      legalActionsByObject: { 41: [genericAction, greenAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([41]),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={41} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="41"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 41,
      actions: [genericAction, greenAction],
    });
  });

  it("renders face-down permanents with the card back in full-card mode", () => {
    const faceDownPermanent = makeObject({
      id: 54,
      name: "Shredder's Technique",
      face_down: true,
      color: [],
      base_color: [],
    });

    const gameState = {
      ...makeState(),
      objects: { 54: faceDownPermanent },
      battlefield: [54],
    } as unknown as GameState;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });

    const { getByLabelText } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={54} />
      </BoardInteractionContext.Provider>,
    );

    expect(getByLabelText("Face-down card")).toHaveAttribute("data-face-down", "true");
  });
});
