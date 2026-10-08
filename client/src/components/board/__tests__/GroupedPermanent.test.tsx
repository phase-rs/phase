import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AttackerInfo, GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObject,
  buildObjectMap,
  gameObjectFactory,
} from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPendingCast,
  buildPlayers,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  buildTargetSelectionWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { toCardProps } from "../../../viewmodel/cardProps.ts";
import type { GroupedPermanent as GroupedPermanentType } from "../../../viewmodel/battlefieldProps.ts";
import { ActionButton } from "../ActionButton.tsx";
import { BattlefieldRow } from "../BattlefieldRow.tsx";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { getGroupRenderMode } from "../groupRenderMode.ts";
import { GroupedPermanentDisplay } from "../GroupedPermanent.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
}));

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div aria-label={cardName} style={{ height: "var(--card-h)", width: "var(--card-w)" }} />
  ),
}));

function makeObject(id: number): GameObject {
  return buildGameObject({
    id,
    card_id: 100,
    name: "Saproling",
    power: 1,
    toughness: 1,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Saproling"] },
    color: ["Green"],
    base_power: 1,
    base_toughness: 1,
    base_color: ["Green"],
    timestamp: id,
  });
}

function makeState(waitingFor: WaitingFor): GameState {
  const objects = buildObjectMap(
    ...[1, 2, 3, 4, 5].map((id) => makeObject(id)),
  );
  return buildGameState({
    objects,
    battlefield: [1, 2, 3, 4, 5],
    waiting_for: waitingFor,
  });
}

function makeGroup(ids = [1, 2, 3, 4, 5]): GroupedPermanentType {
  return {
    name: "Saproling",
    ids,
    count: ids.length,
    representative: toCardProps(makeObject(1)),
    isUnboundedPile: false,
  };
}

function renderGroup(options: {
  boardChoiceObjectIds?: Set<number>;
  validAttackerIds?: Set<number>;
  validTargetObjectIds?: Set<number>;
  committedAttackerIds?: Set<number>;
  group?: GroupedPermanentType;
} = {}) {
  const group = options.group ?? makeGroup();
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        blockableAttackerIds: new Set(),
        boardChoiceObjectIds: options.boardChoiceObjectIds ?? new Set(),
        committedAttackerIds: options.committedAttackerIds ?? new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set(),
        selectableSacrificeObjectIds: new Set(),
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds: new Set(),
        validAttackerIds: options.validAttackerIds ?? new Set(),
        validTargetObjectIds: options.validTargetObjectIds ?? new Set(),
      }}
    >
      <GroupedPermanentDisplay
        group={group}
        rowType="creatures"
        renderMode={getGroupRenderMode(group, {
          manualExpanded: false,
          containsBlockableAttackerDuringBlockers: false,
        })}
        onExpand={vi.fn()}
      />
    </BoardInteractionContext.Provider>,
  );
}

function renderCreatureRow() {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        blockableAttackerIds: new Set(),
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
      <BattlefieldRow groups={[makeGroup()]} rowType="creatures" />
    </BoardInteractionContext.Provider>,
  );
}

describe("GroupedPermanentDisplay collapsed creature groups", () => {
  beforeEach(() => {
    const waitingFor: WaitingFor = {
      type: "DeclareAttackers",
      data: { player: 0, valid_attacker_ids: [1, 2, 3, 4, 5] },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
      legalActions: [],
      legalActionsByObject: {},
      manaPaymentPreviewSourceIds: [],
      spellCosts: {},
    });
    useUiStore.setState({
      selectedObjectId: null,
      hoveredObjectId: null,
      inspectedObjectId: null,
      combatMode: null,
      selectedAttackers: [],
      pendingBlocker: null,
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

  it("renders five matching creatures as one representative with a prominent count badge", () => {
    const { container } = renderGroup();

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Expand Saproling group" })).toHaveTextContent("×5");
  });

  // DESIGN STEP 4 (∞-pile): an accepted object-growth loop's pile renders ∞, not ×N.
  it("renders ∞ instead of ×N for a collapsed unbounded-pile group", () => {
    renderGroup({ group: { ...makeGroup([1, 2, 3, 4, 5]), isUnboundedPile: true } });

    expect(
      screen.getByRole("button", { name: "Expand Saproling group" }),
    ).toHaveTextContent("∞");
  });

  // SHOULD-FIX #1 (singleton trap): count <= 1 → "single" mode renders no count
  // badge, but ∞ is COUNT-INDEPENDENT, so a 1-member pile must still show ∞.
  it("renders ∞ for a single-member unbounded-pile group", () => {
    renderGroup({ group: { ...makeGroup([1]), isUnboundedPile: true } });

    expect(screen.getByText("∞")).toBeInTheDocument();
  });

  it("renders a melded permanent inside the oversized melded frame", () => {
    const melded = gameObjectFactory
      .creature(9, 10)
      .named("Brisela, Voice of Nightmares")
      .withId(6)
      .melded([6, 7])
      .build();
    useGameStore.setState({
      gameState: buildGameState({ objects: buildObjectMap(melded), battlefield: [6] }),
    });

    const { container } = renderGroup({
      group: {
        name: melded.name,
        ids: [6],
        count: 1,
        representative: toCardProps(melded),
        isUnboundedPile: false,
      },
    });

    const frame = container.querySelector("[data-melded-card]");
    expect(frame).not.toBeNull();
    expect(frame?.querySelector('[data-object-id="6"]')).not.toBeNull();
  });

  it("renders an ordinary permanent at the row's card size", () => {
    const { container } = renderGroup({ group: makeGroup([1]) });

    expect(container.querySelector("[data-melded-card]")).toBeNull();
    expect(container.querySelector('[data-object-id="1"]')).not.toBeNull();
  });

  it("renders ×N (not ∞) when a group is not an unbounded pile", () => {
    renderGroup({ group: makeGroup([1, 2, 3, 4, 5]) });

    const badge = screen.getByRole("button", { name: "Expand Saproling group" });
    expect(badge).toHaveTextContent("×5");
    expect(badge).not.toHaveTextContent("∞");
  });

  it("regroups manually expanded duplicate creature groups from a stable row control", () => {
    const { container } = renderCreatureRow();

    fireEvent.click(screen.getByRole("button", { name: "Expand Saproling group" }));

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(5);

    fireEvent.click(screen.getByRole("button", { name: "Regroup duplicate creature groups" }));

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(1);
  });

  it("lifts an engine-selected mana source above the other cards in a staggered group", () => {
    useGameStore.setState({ manaPaymentPreviewSourceIds: [1] });

    const { container } = renderGroup({ group: makeGroup([1, 2]) });

    const selectedSource = container.querySelector('[data-object-id="1"]') as HTMLElement;
    const coveredCard = container.querySelector('[data-object-id="2"]') as HTMLElement;

    expect(selectedSource.parentElement?.style.zIndex).toBe("2");
    expect(coveredCard.parentElement?.style.zIndex).toBe("1");
  });

  it("opens an attacker picker that replaces only this group's selected attackers", () => {
    useUiStore.setState({ combatMode: "attackers", selectedAttackers: [99] });
    renderGroup({ validAttackerIds: new Set([1, 2, 3, 4, 5]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    fireEvent.click(screen.getByRole("button", { name: "+1" }));

    expect(useUiStore.getState().selectedAttackers).toEqual([99, 1]);

    fireEvent.click(screen.getByRole("button", { name: "All" }));

    expect(useUiStore.getState().selectedAttackers).toEqual([99, 1, 2, 3, 4, 5]);
  });

  it("dispatches a concrete target choice from the picker", () => {
    const waitingFor = buildTargetSelectionWaitingFor({
      data: {
        player: 0,
        pending_cast: buildPendingCast(),
        target_slots: [
          buildTargetSelectionSlot({
            legal_targets: [{ Object: 1 }, { Object: 2 }, { Object: 3 }],
          }),
        ],
        selection: buildTargetSelectionProgress({
          current_legal_targets: [{ Object: 1 }, { Object: 2 }, { Object: 3 }],
        }),
      },
    });
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ validTargetObjectIds: new Set([1, 2, 3]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    fireEvent.click(screen.getByRole("button", { name: "#3" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 3 } },
    });
  });

  it("dispatches a concrete equip target from the picker", () => {
    const waitingFor: WaitingFor = {
      type: "EquipTarget",
      data: {
        player: 0,
        equipment_id: 42,
        valid_targets: [1, 2, 3],
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ validTargetObjectIds: new Set([1, 2, 3]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    fireEvent.click(screen.getByRole("button", { name: "#2" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "Equip",
      data: { equipment_id: 42, target_id: 2 },
    });
  });

  it("dispatches an immediate board choice from a collapsed group picker", () => {
    const waitingFor: WaitingFor = {
      type: "StationTarget",
      data: {
        player: 0,
        spacecraft_id: 42,
        eligible_creatures: [1, 2, 3],
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ boardChoiceObjectIds: new Set([1, 2, 3]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    // All eligible creatures in a collapsed group are visually identical, so the
    // picker resolves with a single labelled action instead of a #1..#N list.
    fireEvent.click(screen.getByRole("button", { name: "Station" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ActivateStation",
      data: { spacecraft_id: 42, creature_id: 1 },
    });
  });

  it("uses delegated untap authority for a collapsed group picker", () => {
    const waitingFor: WaitingFor = {
      type: "UntapChoice",
      data: { player: 1, candidates: [1] },
    };
    const gameState = {
      ...makeState(waitingFor),
      turn_decision_controller: 0,
      active_player: 1,
    };
    useGameStore.setState({ gameState, waitingFor });
    renderGroup({ boardChoiceObjectIds: new Set([1]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    fireEvent.click(screen.getByRole("button", { name: "Untap" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseUntap",
      data: { object_id: 1, untap: true },
    });
  });

  it("sacrifices one of many identical tokens with a single action (no #1-#N list) — #4375", () => {
    const waitingFor: WaitingFor = {
      type: "EffectZoneChoice",
      data: {
        player: 0,
        cards: [1, 2, 3, 4, 5],
        count: 1,
        source_id: 99,
        effect_kind: "Sacrifice",
        zone: "Battlefield",
        destination: null,
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ boardChoiceObjectIds: new Set([1, 2, 3, 4, 5]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));

    // No numbered per-token list — the indistinguishable tokens collapse to one
    // action button labelled by intent.
    expect(screen.queryByRole("button", { name: "#1" })).toBeNull();
    expect(screen.queryByRole("button", { name: "#5" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Sacrifice" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [1] },
    });
  });

  it("picks a quantity of identical tokens via the stepper for a multi sacrifice — #4375", () => {
    const waitingFor: WaitingFor = {
      type: "EffectZoneChoice",
      data: {
        player: 0,
        cards: [1, 2, 3, 4, 5],
        count: 2,
        source_id: 99,
        effect_kind: "Sacrifice",
        zone: "Battlefield",
        destination: null,
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ boardChoiceObjectIds: new Set([1, 2, 3, 4, 5]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));

    // Count stepper replaces the #1..#N toggle grid.
    expect(screen.queryByRole("button", { name: "#1" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [1, 2] },
    });
  });

});

// Through BattlefieldRow, not GroupedPermanentDisplay directly: BattlefieldRow
// is the single computation of renderMode, and its `blockableAttackerIds`
// conjunct is what replaced the defect this suite used to encode — a group
// containing the defender's OWN valid blockers could never legitimately share
// a groupKey with a committed attacker (a group is per-controller).
describe("BattlefieldRow render mode during blocker declaration", () => {
  function renderRowWithContext(
    group: GroupedPermanentType,
    context: { blockableAttackerIds?: Set<number>; committedAttackerIds?: Set<number> } = {},
  ) {
    return render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          blockableAttackerIds: context.blockableAttackerIds ?? new Set(),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: context.committedAttackerIds ?? new Set(),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <BattlefieldRow groups={[group]} rowType="creatures" />
      </BoardInteractionContext.Provider>,
    );
  }

  beforeEach(() => {
    useUiStore.setState({ combatMode: "blockers" });
  });

  afterEach(() => {
    cleanup();
  });

  it("keeps a large blockable attacker pile collapsed during blocker declaration", () => {
    const ids = [1, 2, 3, 4, 5, 6];
    const { container } = renderRowWithContext(makeGroup(ids), {
      blockableAttackerIds: new Set(ids),
    });

    const cards = container.querySelectorAll("[data-object-id]");
    expect(cards).toHaveLength(1);
    expect(cards[0].getAttribute("data-grouped-ids")).toBe(ids.join(" "));
  });

  it("expands a small blockable attacker group during blocker declaration", () => {
    const ids = [1, 2, 3];
    const { container } = renderRowWithContext(makeGroup(ids), {
      blockableAttackerIds: new Set(ids),
    });

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(3);
    expect(screen.getByRole("button", { name: "Collapse Saproling group" })).toBeInTheDocument();
  });

  it("does not expand a small group whose attackers this player cannot block", () => {
    const ids = [1, 2, 3];
    const { container } = renderRowWithContext(makeGroup(ids), {
      committedAttackerIds: new Set(ids),
    });

    // Staggered mode still mounts every member, so member count alone does
    // not distinguish the two; the badge does.
    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(3);
    expect(screen.getByRole("button", { name: "Expand Saproling group" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Collapse Saproling group" })).not.toBeInTheDocument();
  });
});

// Real component path: <ActionButton/> (blocker click + Confirm), the pile
// through <BattlefieldRow>, and the two blockers through <PermanentCard/>, all
// under one BoardInteractionContext.Provider. The suite mocks only
// game/dispatch.ts and card/CardImage.tsx (top of file); dispatchAction is the
// observed seam. Fixture:
//   pile 11..17, controller 1 ("Scute Swarm"); attackers 11-15 -> Player 0
//   (the local defender); 16,17 -> Planeswalker 50. Blockers 100 ("Grizzly
//   Bears") and 101 ("Runeclaw Bear"), controller 0.
//   valid_block_targets: {100: [14,15,16,17], 101: [11,12,13,14,15,16,17]}
//   block_requirements: {17: {count: 2}} (CR 509.1b/702.111b)
//   must_be_blocked_targets: {100: [15], 101: [15]} (CR 509.1c)
// For blocker 100 this gives four singleton stacks — {14} You; {15} You +
// must-be-blocked; {16} Planeswalker; {17} Planeswalker + Needs 2 — so 15 is
// the non-lowest legal Player-target member for 100, and 11-13 are legal only
// for 101 (so blocker 100's assignments never touch them).
describe("collapsed attacker pile blocker-assignment picker (integration)", () => {
  const PILE_IDS = [11, 12, 13, 14, 15, 16, 17];

  const DEFAULT_ATTACKERS: AttackerInfo[] = [
    { object_id: 11, defending_player: 0, attack_target: { type: "Player", data: 0 } },
    { object_id: 12, defending_player: 0, attack_target: { type: "Player", data: 0 } },
    { object_id: 13, defending_player: 0, attack_target: { type: "Player", data: 0 } },
    { object_id: 14, defending_player: 0, attack_target: { type: "Player", data: 0 } },
    { object_id: 15, defending_player: 0, attack_target: { type: "Player", data: 0 } },
    { object_id: 16, defending_player: 0, attack_target: { type: "Planeswalker", data: 50 } },
    { object_id: 17, defending_player: 0, attack_target: { type: "Planeswalker", data: 50 } },
  ];

  function attackerObject(id: number): GameObject {
    return buildGameObject({
      id,
      card_id: 900,
      name: "Scute Swarm",
      owner: 1,
      controller: 1,
      power: 1,
      toughness: 1,
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Insect"] },
      color: ["Green"],
      base_power: 1,
      base_toughness: 1,
      base_color: ["Green"],
      timestamp: id,
    });
  }

  function blockerObject(id: number, name: string): GameObject {
    return buildGameObject({
      id,
      card_id: 901 + id,
      name,
      owner: 0,
      controller: 0,
      power: 2,
      toughness: 2,
      card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Bear"] },
      color: ["Green"],
      base_power: 2,
      base_toughness: 2,
      base_color: ["Green"],
      timestamp: id,
    });
  }

  function pileGroup(): GroupedPermanentType {
    return {
      name: "Scute Swarm",
      ids: PILE_IDS,
      count: PILE_IDS.length,
      representative: toCardProps(attackerObject(PILE_IDS[0])),
      isUnboundedPile: false,
    };
  }

  function blockersPrompt(
    overrides: Partial<Extract<WaitingFor, { type: "DeclareBlockers" }>["data"]> = {},
  ): WaitingFor {
    return {
      type: "DeclareBlockers",
      data: {
        player: 0,
        valid_blocker_ids: [100, 101],
        valid_block_targets: { 100: [14, 15, 16, 17], 101: [11, 12, 13, 14, 15, 16, 17] },
        block_requirements: { 17: { count: 2 } },
        must_be_blocked_targets: { 100: [15], 101: [15] },
        // 100 is an ordinary one-blocker creature; 101 may block two.
        block_capacities: { 100: 1, 101: 2 },
        ...overrides,
      },
    };
  }

  function renderBoard(waitingFor: WaitingFor, attackers: AttackerInfo[] = DEFAULT_ATTACKERS) {
    const objects = buildObjectMap(
      ...PILE_IDS.map(attackerObject),
      blockerObject(100, "Grizzly Bears"),
      blockerObject(101, "Runeclaw Bear"),
    );
    const gameState = buildGameState({
      objects,
      battlefield: [...PILE_IDS, 100, 101],
      players: buildPlayers([{ id: 0 }, { id: 1 }]),
      waiting_for: waitingFor,
      combat: {
        attackers,
        blocker_assignments: {},
        blocker_to_attacker: {},
        blockers_declared_by: [],
        pending_blocker_declaration_events: [],
        damage_assignments: {},
        first_strike_done: false,
        damage_step_index: null,
        pending_damage: [],
        regular_damage_done: false,
      },
    });
    useGameStore.setState({ gameState, waitingFor, legalActions: [] });

    return render(
      <BoardInteractionContext.Provider
        value={{
          activatableObjectIds: new Set(),
          blockableAttackerIds: new Set(PILE_IDS),
          boardChoiceObjectIds: new Set(),
          committedAttackerIds: new Set(PILE_IDS),
          incomingAttackerCounts: new Map(),
          manaTappableObjectIds: new Set(),
          selectableSacrificeObjectIds: new Set(),
          selectableManaCostCreatureIds: new Set(),
          undoableTapObjectIds: new Set(),
          validAttackerIds: new Set(),
          validTargetObjectIds: new Set(),
        }}
      >
        <ActionButton />
        <BattlefieldRow groups={[pileGroup()]} rowType="creatures" />
        <PermanentCard objectId={100} />
        <PermanentCard objectId={101} />
      </BoardInteractionContext.Provider>,
    );
  }

  function clickPermanent(container: HTMLElement, id: number) {
    fireEvent.click(container.querySelector(`[data-object-id="${id}"]`) as HTMLElement);
  }

  function groupWhere(predicate: (label: string) => boolean) {
    return screen.getAllByRole("group").find((g) => predicate(g.getAttribute("aria-label") ?? ""))!;
  }

  beforeEach(() => {
    useUiStore.setState({
      combatMode: null,
      selectedAttackers: [],
      pendingBlocker: null,
      blockerAssignments: new Map(),
      combatClickHandler: null,
    });
    vi.mocked(dispatchAction).mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("offers no picker on the pile before a blocker is pending", () => {
    renderBoard(blockersPrompt());

    expect(screen.queryByRole("button", { name: "Choose Scute Swarm token" })).not.toBeInTheDocument();
  });

  it("assigns the must-be-blocked stack member, not the lowest legal id", () => {
    const { container } = renderBoard(blockersPrompt());

    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));

    // Reach guard: 11-13 are illegal for blocker 100, so its plain "You"
    // stack holds only 14.
    const plainYou = groupWhere((label) => label.startsWith("You") && !label.includes("Must be blocked"));
    expect(within(plainYou).getByText("0 / 1")).toBeInTheDocument();

    const mustBeBlockedGroup = groupWhere((label) => label.includes("Must be blocked"));
    fireEvent.click(within(mustBeBlockedGroup).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));

    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 15]] },
    });
  });

  it("distinguishes stacks by attack target, must-be-blocked, and minimum-blocker count, one axis at a time", () => {
    const { container } = renderBoard(blockersPrompt());
    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));

    const needsTwo = () => groupWhere((label) => label.includes("Needs 2"));
    const plainPlaneswalker = () =>
      groupWhere((label) => label.includes("(Planeswalker)") && !label.includes("Needs"));
    const plainYou = () =>
      groupWhere((label) => label.startsWith("You") && !label.includes("Must be blocked"));

    fireEvent.click(within(needsTwo()).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 17]] },
    });

    fireEvent.click(within(needsTwo()).getByRole("button", { name: "-1" }));
    fireEvent.click(within(plainPlaneswalker()).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 16]] },
    });

    fireEvent.click(within(plainPlaneswalker()).getByRole("button", { name: "-1" }));
    fireEvent.click(within(plainYou()).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 14]] },
    });
  });

  it("keeps a prior assignment when switching the pending blocker, splitting by attack target and other-assigned blockers", () => {
    const { container } = renderBoard(blockersPrompt());
    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));
    const plainYou = groupWhere((label) => label.startsWith("You") && !label.includes("Must be blocked"));
    fireEvent.click(within(plainYou).getByRole("button", { name: "+1" }));

    // Switch the pending blocker without confirming — 100 keeps its pick (14),
    // so switching to 101 must split its own-attack-target "You" stack around
    // 14 rather than merging it back in.
    clickPermanent(container, 101);

    const blockedByGroup = () => groupWhere((label) => label.includes("Blocked by Grizzly Bears"));
    const plainGroup = () =>
      groupWhere(
        (label) => label.startsWith("You") && !label.includes("Blocked by") && !label.includes("Must be blocked"),
      );
    expect(within(blockedByGroup()).getByText("0 / 1")).toBeInTheDocument();
    // 101 "Runeclaw Bear" may block two (block_capacities), so its own
    // unblocked stack of 3 candidates reads a ceiling of 2, not its size.
    expect(plainGroup().getAttribute("aria-label")).toContain("Unblocked");
    expect(within(plainGroup()).getByText("0 / 2")).toBeInTheDocument();

    fireEvent.click(within(plainGroup()).getByRole("button", { name: "+1" }));
    fireEvent.click(within(plainGroup()).getByRole("button", { name: "+1" }));
    expect(plainGroup().getAttribute("aria-label")).not.toContain("Unblocked");
    expect(within(plainGroup()).getByText("2 / 2")).toBeInTheDocument();
    fireEvent.click(within(plainGroup()).getByRole("button", { name: "-1" }));
    expect(within(plainGroup()).getByText("1 / 2")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 14], [101, 11]] },
    });

    // A second block on an attacker already assigned a blocker is not gated
    // client-side: 101 also blocks 14, which 100 already blocks — and this
    // stays within 101's own published capacity of 2 (11 and 14).
    fireEvent.click(within(blockedByGroup()).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 14], [101, 11], [101, 14]] },
    });

    // 101 has now spent its whole capacity (11, 14), so its plain stack's
    // remaining member (12, 13) reads a ceiling of exactly its one assignment.
    expect(within(plainGroup()).getByText("1 / 1")).toBeInTheDocument();
    expect(within(plainGroup()).getByRole("button", { name: "+1" })).toBeDisabled();

    // The badge reflects both pile members with direct block selections (11, 14).
    expect(screen.getByText("block targets 2")).toBeInTheDocument();
  });

  it("assigns a blocker-side must-block requirement (no must-be-blocked axis)", () => {
    const { container } = renderBoard(
      blockersPrompt({
        must_be_blocked_targets: {},
        blocker_constraints: { 100: { kind: "MustBlock", attackers: [14] } },
      }),
    );
    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));

    const mustBlockGroup = groupWhere((label) => /Must block/.test(label));

    fireEvent.click(within(mustBlockGroup).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));

    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 14]] },
    });
  });

  it("splits stacks by band membership (CR 702.22c)", () => {
    // Isolate the band axis: three of blocker 100's legal targets share every
    // other axis (Player target, no requirement, no must-block/must-be-blocked,
    // no other blocker assigned) but differ only by band.
    const bandedAttackers: AttackerInfo[] = DEFAULT_ATTACKERS.map((attacker) => {
      if (attacker.object_id === 14) return { ...attacker, band_id: 5 };
      if (attacker.object_id === 16) return { ...attacker, band_id: 2, attack_target: { type: "Player", data: 0 } };
      if (attacker.object_id === 17) return { ...attacker, band_id: null, attack_target: { type: "Player", data: 0 } };
      return attacker;
    });
    const { container } = renderBoard(
      blockersPrompt({
        valid_block_targets: { 100: [14, 16, 17], 101: [11, 12, 13, 14, 15, 16, 17] },
        block_requirements: {},
        must_be_blocked_targets: {},
      }),
      bandedAttackers,
    );
    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));

    // Band ids are relabelled to a 1-based ordinal over the distinct ids
    // present, ascending: band 2 (member 16) is "Band 1", band 5 (member 14)
    // is "Band 2".
    const bandOne = groupWhere((label) => label.includes("Band 1"));
    const bandTwo = groupWhere((label) => label.includes("Band 2"));
    const bandless = groupWhere((label) => label.startsWith("You") && !label.includes("Band"));
    expect(within(bandOne).getByText("0 / 1")).toBeInTheDocument();
    expect(within(bandTwo).getByText("0 / 1")).toBeInTheDocument();
    expect(within(bandless).getByText("0 / 1")).toBeInTheDocument();

    fireEvent.click(within(bandTwo).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));

    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 14]] },
    });
  });

  it("shows a band-mate of another blocker's pick by its own direct assignments", () => {
    // Attackers 14 and 15 share band 5 and both attack Player 0 (their
    // DEFAULT_ATTACKERS target already), isolating the band axis.
    const bandedAttackers: AttackerInfo[] = DEFAULT_ATTACKERS.map((attacker) =>
      attacker.object_id === 14 || attacker.object_id === 15
        ? { ...attacker, band_id: 5 }
        : attacker,
    );
    const { container } = renderBoard(
      blockersPrompt({
        valid_block_targets: { 100: [14, 15], 101: [14, 15] },
        block_requirements: {},
        must_be_blocked_targets: {},
      }),
      bandedAttackers,
    );

    // Blocker 101 directly assigns 14 (lowest-id member).
    clickPermanent(container, 101);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));
    const bandStackFor101 = groupWhere((label) => label.includes("Band 1"));
    fireEvent.click(within(bandStackFor101).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));

    // The pile's badge counts only the one direct block target (14); 15 has none.
    expect(screen.getByText("block targets 1")).toBeInTheDocument();

    // Blocker 100's picker shows 14 and 15 in two separate stacks (each keeps
    // its own direct assignments), both still carrying the "Band 1" chip.
    clickPermanent(container, 100);
    expect(screen.getAllByRole("group")).toHaveLength(2);
    const blockedGroup = groupWhere(
      (label) => label.includes("Band 1") && label.includes("Blocked by Runeclaw Bear"),
    );
    const neutralGroup = groupWhere(
      (label) => label.includes("Band 1") && !label.includes("Blocked by"),
    );
    expect(within(blockedGroup).getByText("0 / 1")).toBeInTheDocument();
    expect(neutralGroup.getAttribute("aria-label")).not.toContain("Unblocked");
    expect(within(neutralGroup).getByText("0 / 1")).toBeInTheDocument();

    fireEvent.click(within(blockedGroup).getByRole("button", { name: "All" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[101, 14], [100, 14]] },
    });

    expect(within(groupWhere((label) => label.includes("Blocked by Runeclaw Bear"))).getByText(
      "1 / 1",
    )).toBeInTheDocument();
    const stillNeutral = groupWhere((label) => label.includes("Band 1") && !label.includes("Blocked by"));
    expect(stillNeutral.getAttribute("aria-label")).not.toContain("Unblocked");
    expect(within(stillNeutral).getByText("0 / 0")).toBeInTheDocument();
    expect(within(stillNeutral).getByRole("button", { name: "+1" })).toBeDisabled();

    expect(screen.getByText("block targets 1")).toBeInTheDocument();
  });

  it("keeps a band's ordinal stable across different pending blockers instead of renumbering per picker (CR 702.22c)", () => {
    const bandedAttackers: AttackerInfo[] = DEFAULT_ATTACKERS.map((attacker) => {
      if (attacker.object_id === 14) return { ...attacker, band_id: 5 };
      if (attacker.object_id === 16) return { ...attacker, band_id: 2, attack_target: { type: "Player", data: 0 } };
      if (attacker.object_id === 17) return { ...attacker, band_id: null, attack_target: { type: "Player", data: 0 } };
      return attacker;
    });
    const { container } = renderBoard(
      blockersPrompt({
        // Blocker 100 never sees band 2 (member 16) at all — only band 5.
        valid_block_targets: { 100: [14, 17], 101: [14, 16, 17] },
        block_requirements: {},
        must_be_blocked_targets: {},
      }),
      bandedAttackers,
    );

    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));
    // Ordinals are assigned over every attacker in combat (ascending band id:
    // 2 then 5), not just this picker's own stacks — so band 5 (member 14)
    // reads "Band 2" even though blocker 100's picker has no "Band 1" stack.
    const bandTwoFor100 = groupWhere((label) => label.includes("Band 2"));
    expect(within(bandTwoFor100).getByText("0 / 1")).toBeInTheDocument();

    clickPermanent(container, 101);
    // Switching to blocker 101 (whose stacks add band 2, member 16) must not
    // renumber band 5: it stays "Band 2".
    const bandOneFor101 = groupWhere((label) => label.includes("Band 1"));
    const bandTwoFor101 = groupWhere((label) => label.includes("Band 2"));
    expect(within(bandOneFor101).getByText("0 / 1")).toBeInTheDocument();
    expect(within(bandTwoFor101).getByText("0 / 1")).toBeInTheDocument();
  });

  it("clamps 'All' to an ordinary blocker's published capacity", () => {
    const { container } = renderBoard(
      blockersPrompt({ block_capacities: { 100: 1, 101: 1 } }),
    );
    clickPermanent(container, 101);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));

    const plainGroup = groupWhere(
      (label) => label.startsWith("You") && !label.includes("Blocked by") && !label.includes("Must be blocked"),
    );
    expect(plainGroup.getAttribute("aria-label")).toContain("Unblocked");
    expect(within(plainGroup).getByText("0 / 1")).toBeInTheDocument();

    fireEvent.click(within(plainGroup).getByRole("button", { name: "All" }));
    expect(plainGroup.getAttribute("aria-label")).not.toContain("Unblocked");
    expect(within(plainGroup).getByText("1 / 1")).toBeInTheDocument();
    expect(within(plainGroup).getByRole("button", { name: "+1" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[101, 11]] },
    });
  });

  it("keeps the pending blocker's own band pick and its band-mates in one stack, and counts only the direct pick", () => {
    const bandedAttackers: AttackerInfo[] = DEFAULT_ATTACKERS.map((attacker) =>
      attacker.object_id === 14 || attacker.object_id === 15 || attacker.object_id === 16
        ? { ...attacker, band_id: 5, attack_target: { type: "Player" as const, data: 0 } }
        : attacker,
    );
    const { container } = renderBoard(
      blockersPrompt({
        valid_block_targets: { 100: [14, 15, 16] },
        block_requirements: {},
        must_be_blocked_targets: {},
      }),
      bandedAttackers,
    );

    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));
    expect(screen.getAllByRole("group")).toHaveLength(1);
    expect(within(groupWhere(() => true)).getByText("0 / 1")).toBeInTheDocument();

    fireEvent.click(within(groupWhere(() => true)).getByRole("button", { name: "All" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 14]] },
    });

    expect(screen.getAllByRole("group")).toHaveLength(1);
    const stack = groupWhere(() => true);
    expect(within(stack).getByText("1 / 1")).toBeInTheDocument();
    expect(within(stack).getByRole("button", { name: "+1" })).toBeDisabled();
    expect(stack.getAttribute("aria-label")).not.toContain("Unblocked");

    // The pile's badge counts only the direct pick (14).
    expect(screen.getByText("block targets 1")).toBeInTheDocument();

    fireEvent.click(within(stack).getByRole("button", { name: "-1" }));
    expect(screen.getAllByRole("group")).toHaveLength(1);
    expect(within(groupWhere(() => true)).getByText("0 / 1")).toBeInTheDocument();
  });

  it("enters both direct blocks a capacity-2 blocker owes a must-be-blocked band (CR 509.1c)", () => {
    const bandedAttackers: AttackerInfo[] = DEFAULT_ATTACKERS.map((attacker) =>
      attacker.object_id === 14 || attacker.object_id === 15
        ? { ...attacker, band_id: 5, attack_target: { type: "Player" as const, data: 0 } }
        : attacker,
    );
    const { container } = renderBoard(
      blockersPrompt({
        valid_block_targets: { 101: [14, 15] },
        must_be_blocked_targets: { 101: [14, 15] },
        block_requirements: {},
      }),
      bandedAttackers,
    );

    clickPermanent(container, 101);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));
    const mustBeBlockedGroup = groupWhere((label) => label.includes("Must be blocked"));
    expect(within(mustBeBlockedGroup).getByText("0 / 2")).toBeInTheDocument();

    fireEvent.click(within(mustBeBlockedGroup).getByRole("button", { name: "All" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (2)" }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[101, 14], [101, 15]] },
    });
    expect(within(mustBeBlockedGroup).getByText("2 / 2")).toBeInTheDocument();

    fireEvent.click(within(mustBeBlockedGroup).getByRole("button", { name: "-1" }));
    expect(screen.getAllByRole("group")).toHaveLength(1);
    const stillMustBeBlocked = groupWhere((label) => label.includes("Must be blocked"));
    expect(within(stillMustBeBlocked).getByText("1 / 2")).toBeInTheDocument();
    expect(within(stillMustBeBlocked).getByRole("button", { name: "+1" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[101, 14]] },
    });

    fireEvent.click(within(groupWhere((label) => label.includes("Must be blocked"))).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (2)" }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[101, 14], [101, 15]] },
    });
  });

  it("offers no picker on the pile when the prompt belongs to another player", () => {
    renderBoard(blockersPrompt({ player: 1 }));
    // Isolate the waitingForPlayer gate from the (also-null, since ActionButton
    // never enters "combat-blockers" mode for another player's prompt) combat
    // mode: force combatMode as if a blocker were already pending, flushed so
    // the picker's memo recomputes against it.
    act(() => {
      useUiStore.setState({ pendingBlocker: 100, combatMode: "blockers" });
    });

    expect(screen.queryByRole("button", { name: "Choose Scute Swarm token" })).not.toBeInTheDocument();
  });

  it("offers the picker on the pile when the prompt belongs to the local player", () => {
    renderBoard(blockersPrompt({ player: 0 }));
    act(() => {
      useUiStore.setState({ pendingBlocker: 100, combatMode: "blockers" });
    });

    expect(screen.getByRole("button", { name: "Choose Scute Swarm token" })).toBeInTheDocument();
  });
});
