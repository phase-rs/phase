import { act, cleanup, fireEvent, render } from "@testing-library/react";
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
  CardImage: ({
    cardName,
    faceDown,
    oracleText,
    tokenFilters,
  }: {
    cardName: string;
    faceDown?: boolean;
    oracleText?: string;
    tokenFilters?: { subtypes?: string[] };
  }) => (
    <div
      aria-label={faceDown ? "Face-down card" : cardName}
      data-face-down={faceDown ? "true" : "false"}
      data-oracle-text={oracleText ?? ""}
      data-token-subtypes={tokenFilters?.subtypes?.join(",") ?? ""}
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
    attached_to: { type: "Object", data: 1 },
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
    attached_to: { type: "Object", data: 2 },
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

function renderPermanent(
  validTargetObjectIds = new Set<number>(),
  selectableSacrificeObjectIds = new Set<number>(),
  boardChoiceObjectIds = new Set<number>(),
  activatableObjectIds = new Set<number>(),
  undoableTapObjectIds = new Set<number>(),
) {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds,
        boardChoiceObjectIds,
        committedAttackerIds: new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set(),
        selectableSacrificeObjectIds,
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds,
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
    window.matchMedia = ((query: string) => ({
      matches: query === "(hover: hover)" || query === "(any-hover: hover)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
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

  it("collapses multiple direct attachments until the host is hovered", () => {
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent();

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).toBeNull();
    expect(container.textContent).toContain("+1");

    act(() => {
      useUiStore.setState({ inspectedObjectId: 1 });
    });
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();

    act(() => {
      useUiStore.setState({ inspectedObjectId: null });
    });
    fireEvent.mouseEnter(container.querySelector('[data-object-id="1"]') as HTMLElement);

    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("auto-expands collapsed attachments when one is a valid target", () => {
    // Regression: Moira Brown's "put a quest counter on target nonland
    // permanent you control" offers the host's attached Equipment/Auras as
    // targets. Collapsed behind the host they are unclickable, so the counter
    // lands on the host creature instead of the chosen attachment. A host with
    // an actionable attachment must open WITHOUT requiring a hover.
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    // Attachment 4 is a valid target — both attachments must render even though
    // the host is neither hovered nor inspected.
    const { container } = renderPermanent(new Set([4]));

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("auto-expands collapsed attachments when one is activatable (re-equip)", () => {
    // Regression: an attached Equipment whose Equip ability is activatable must
    // be reachable so it can be moved to another creature. Collapsed behind the
    // host it cannot be clicked, so equip appears stuck once attached.
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent(
      new Set(),
      new Set(),
      new Set(),
      new Set([4]),
    );

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("auto-expands collapsed attachments when one has an undoable mana tap", () => {
    // Regression: an attachment tapped for mana that can still be untapped
    // (undo) is actionable. Collapsed behind its host the undo affordance is
    // unclickable, stranding the tapped mana source.
    const secondEquipment = makeObject({
      id: 4,
      card_id: 400,
      attached_to: { type: "Object", data: 1 },
      name: "Second Equipment",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Equipment"] },
      color: [],
      base_color: [],
    });
    const gameState = makeState();
    gameState.objects[1].attachments = [2, 4];
    gameState.objects[4] = secondEquipment;
    gameState.battlefield = [1, 2, 3, 4];
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container } = renderPermanent(
      new Set(),
      new Set(),
      new Set(),
      new Set(),
      new Set([4]),
    );

    expect(container.querySelector('[data-object-id="2"]')).not.toBeNull();
    expect(container.querySelector('[data-object-id="4"]')).not.toBeNull();
  });

  it("collapses multiple exiled cards hosted by one permanent until hover", () => {
    const exiledOne = makeObject({
      id: 10,
      card_id: 1000,
      zone: "Exile",
      name: "Exiled One",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
    });
    const exiledTwo = makeObject({
      id: 11,
      card_id: 1001,
      zone: "Exile",
      name: "Exiled Two",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
    });
    const gameState = {
      ...makeState(),
      objects: {
        ...makeState().objects,
        10: exiledOne,
        11: exiledTwo,
      },
      exile: [10, 11],
      exile_links: [
        { exiled_id: 10, source_id: 1, kind: "TrackedBySource" },
        { exiled_id: 11, source_id: 1, kind: "TrackedBySource" },
      ],
    } as unknown as GameState;
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });

    const { container, queryByLabelText } = renderPermanent();

    expect(queryByLabelText("Exiled One")).not.toBeNull();
    expect(queryByLabelText("Exiled Two")).toBeNull();
    expect(container.textContent).toContain("+1");

    fireEvent.mouseEnter(container.querySelector('[data-object-id="1"]') as HTMLElement);

    expect(queryByLabelText("Exiled Two")).not.toBeNull();
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

  it("dispatches a target click even when a stale combat mode lingers during target selection", () => {
    // Regression: a spell's TargetSelection must win over a leftover
    // `combatMode` UI flag. PermanentCard routed combat clicks on `combatMode`
    // alone — unlike GroupedPermanent, which also requires the matching combat
    // WaitingFor (`waitingFor.type === "DeclareBlockers"`). So a stale
    // `combatMode` from a just-finished combat step swallowed bounce/target
    // clicks: targets glowed (validTargetObjectIds) but the click hit the dead
    // blocker branch and `ChooseTarget` never fired. Reported on Chain of Vapor
    // cast during combat.
    const gameState = {
      ...makeState(),
      waiting_for: {
        type: "TargetSelection",
        data: {
          pending_cast: { object_id: 99 },
          target_slots: [],
          selection: { current_slot: 0, current_legal_targets: [{ Object: 1 }] },
        },
      },
    } as unknown as GameState;
    useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    const staleBlockerHandler = vi.fn();
    useUiStore.setState({
      combatMode: "blockers",
      combatClickHandler: staleBlockerHandler,
    });

    const { container } = renderPermanent(new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(staleBlockerHandler).not.toHaveBeenCalled();
    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 1 } },
    });
  });

  it("submits a single battlefield sacrifice choice from the board", () => {
    const gameState = {
      ...makeState(),
      waiting_for: {
        type: "EffectZoneChoice",
        data: {
          player: 0,
          cards: [1],
          count: 1,
          source_id: 99,
          effect_kind: "Sacrifice",
          zone: "Battlefield",
          destination: null,
        },
      },
    } as unknown as GameState;
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
    const { container } = renderPermanent(new Set(), new Set(), new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [1] },
    });
  });

  it("submits immediate board choices from the board", () => {
    const gameState = {
      ...makeState(),
      waiting_for: {
        type: "StationTarget",
        data: {
          player: 0,
          spacecraft_id: 9,
          eligible_creatures: [1],
        },
      },
    } as unknown as GameState;
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
    const { container } = renderPermanent(new Set(), new Set(), new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ActivateStation",
      data: { spacecraft_id: 9, creature_id: 1 },
    });
  });

  it("counts only active board-choice selections when enforcing count limits", () => {
    const gameState = {
      ...makeState(),
      waiting_for: {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "ReturnToHand" },
          choices: [1],
          count: 1,
          min_count: 1,
          resume: {
            type: "Spell",
            Spell: {
              object_id: 9,
              card_id: 90,
              ability: { targets: [] },
              cost: { type: "NoCost" },
            },
          },
        },
      },
    } as unknown as GameState;
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
    useUiStore.setState({ selectedCardIds: [99] });
    const { container } = renderPermanent(new Set(), new Set(), new Set([1]));
    const permanent = container.querySelector('[data-object-id="1"]') as HTMLElement;

    fireEvent.click(permanent);

    expect(useUiStore.getState().selectedCardIds).toEqual([99, 1]);
  });

  it("renders action affordance highlights above the card face", () => {
    const { container } = renderPermanent(new Set([1]));
    const highlight = container.querySelector(
      '[data-card-affordance-highlight="true"]',
    );

    expect(highlight).toBeTruthy();
    expect(highlight?.className).toContain("absolute");
    expect(highlight?.className).toContain("z-30");
    expect(highlight?.className).toContain("pointer-events-none");
  });

  it("renders the summoning sickness art overlay when marked by the engine", () => {
    const gameState = makeState();
    gameState.objects[1] = {
      ...gameState.objects[1],
      has_summoning_sickness: true,
    };
    useGameStore.setState({ gameState });

    const { container } = renderPermanent();

    expect(container.querySelector('[data-summoning-sickness-underwater="true"]')).toBeTruthy();
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
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([39]),
          selectableSacrificeObjectIds: new Set(),
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
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([40]),
          selectableSacrificeObjectIds: new Set(),
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
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set([41]),
          selectableSacrificeObjectIds: new Set(),
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
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
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

  it("forwards engine-provided token rules text and subtypes to the card image", () => {
    const lander = makeObject({
      id: 70,
      name: "Lander",
      display_source: "Token",
      power: null,
      toughness: null,
      base_power: null,
      base_toughness: null,
      card_types: { supertypes: [], core_types: ["Artifact"], subtypes: ["Lander"] },
      color: [],
      base_color: [],
      token_rules_text:
        "{2}, {T}, Sacrifice this token: Search your library for a basic land card, put it onto the battlefield tapped, then shuffle.",
    } as Partial<GameObject>);

    const gameState = {
      ...makeState(),
      objects: { 70: lander },
      battlefield: [70],
    } as unknown as GameState;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={70} />
      </BoardInteractionContext.Provider>,
    );

    const image = container.querySelector("[data-oracle-text]") as HTMLElement;
    expect(image.getAttribute("data-oracle-text")).toContain("basic land");
    expect(image.getAttribute("data-token-subtypes")).toBe("Lander");
  });

  // #506: a lone card-consuming ActivateAbility (consumes_source true) must
  // surface the choice modal instead of auto-firing on a single click. With
  // the resolveSingleActionDispatch gate reverted this test fails — the
  // action auto-dispatches.
  it("opens the choice modal for a lone card-consuming activated ability", () => {
    const sacker = makeObject({
      id: 80,
      name: "Self-Sacrifice Permanent",
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "Sacrifice this permanent: Draw a card.",
          effect: { type: "Draw" },
          consumes_source: true,
        },
      ] as unknown as GameObject["abilities"],
    });

    const gameState = {
      ...makeState(),
      objects: { 80: sacker },
      battlefield: [80],
    } as unknown as GameState;
    const abilityAction = {
      type: "ActivateAbility",
      data: { source_id: 80, ability_index: 0 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [abilityAction],
      legalActionsByObject: { 80: [abilityAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set([80]),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={80} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="80"]') as HTMLElement);

    expect(dispatchAction).not.toHaveBeenCalled();
    expect(useUiStore.getState().pendingAbilityChoice).toEqual({
      objectId: 80,
      actions: [abilityAction],
    });
  });

  // #506 guard: a lone benign activated ability (consumes_source false) must
  // still auto-dispatch — the fix does not regress repeatable tap abilities.
  it("auto-dispatches a lone benign activated ability", () => {
    const scryer = makeObject({
      id: 81,
      name: "Benign Scry Permanent",
      abilities: [
        {
          kind: "Activated",
          cost: { type: "Tap" },
          description: "{T}: Scry 1.",
          effect: { type: "Scry" },
          consumes_source: false,
        },
      ] as unknown as GameObject["abilities"],
    });

    const gameState = {
      ...makeState(),
      objects: { 81: scryer },
      battlefield: [81],
    } as unknown as GameState;
    const abilityAction = {
      type: "ActivateAbility",
      data: { source_id: 81, ability_index: 0 },
    } as const;

    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [abilityAction],
      legalActionsByObject: { 81: [abilityAction] },
      spellCosts: {},
    });

    const { container } = render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set([81]),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <PermanentCard objectId={81} />
      </BoardInteractionContext.Provider>,
    );

    fireEvent.click(container.querySelector('[data-object-id="81"]') as HTMLElement);

    expect(dispatchAction).toHaveBeenCalledWith(abilityAction);
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
  });
});
