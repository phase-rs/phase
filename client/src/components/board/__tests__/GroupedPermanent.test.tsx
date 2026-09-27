import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
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
// is the single computation of renderMode (D2), and its `blockableAttackerIds`
// conjunct is what replaced the defect this suite used to encode — a group
// containing the defender's OWN valid blockers could never legitimately share
// a groupKey with a committed attacker (a group is per-controller), so that
// fixture is gone rather than fixed.
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

    // Staggered mode still mounts every member (like expanded — see the
    // building-block comment on `getGroupRenderMode`), so member count alone
    // does not distinguish the two; the badge does.
    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(3);
    expect(screen.getByRole("button", { name: "Expand Saproling group" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Collapse Saproling group" })).not.toBeInTheDocument();
  });
});

// Real component path: <ActionButton/> (blocker click + Confirm), the pile
// through <BattlefieldRow>, and the two blockers through <PermanentCard/>, all
// under one BoardInteractionContext.Provider. The suite mocks only
// game/dispatch.ts and card/CardImage.tsx (top of file); dispatchAction is the
// observed seam. Fixture, verbatim as CR anchors:
//   pile 11..15, controller 1 ("Scute Swarm"); attackers 11,12,13 -> Player 0
//   (the local defender); 14,15 -> Planeswalker 50. Blockers 100 ("Grizzly
//   Bears") and 101 ("Runeclaw Bear"), controller 0.
//   valid_block_targets: {100: [12,13,14,15], 101: [11,12,13,14,15]}
//   block_requirements: {15: {count: 2}} (CR 509.1b/702.111b)
//   must_be_blocked_targets: {100: [13], 101: [13]} (CR 509.1c)
// For blocker 100 this gives four singleton stacks — {12} You; {13} You +
// must-be-blocked; {14} Planeswalker; {15} Planeswalker + Needs 2 — so 13 is
// the non-lowest legal Player-target member for 100.
describe("collapsed attacker pile blocker-assignment picker (integration)", () => {
  const PILE_IDS = [11, 12, 13, 14, 15];

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
        valid_block_targets: { 100: [12, 13, 14, 15], 101: [11, 12, 13, 14, 15] },
        block_requirements: { 15: { count: 2 } },
        must_be_blocked_targets: { 100: [13], 101: [13] },
        ...overrides,
      },
    };
  }

  function renderBoard(waitingFor: WaitingFor) {
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
        attackers: [
          { object_id: 11, defending_player: 0, attack_target: { type: "Player", data: 0 } },
          { object_id: 12, defending_player: 0, attack_target: { type: "Player", data: 0 } },
          { object_id: 13, defending_player: 0, attack_target: { type: "Player", data: 0 } },
          { object_id: 14, defending_player: 0, attack_target: { type: "Planeswalker", data: 50 } },
          { object_id: 15, defending_player: 0, attack_target: { type: "Planeswalker", data: 50 } },
        ],
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
          blockableAttackerIds: new Set([11, 12, 13, 14, 15]),
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

    const groups = screen.getAllByRole("group");
    const mustBeBlockedGroup = groups.find((g) => /Must be blocked/.test(g.getAttribute("aria-label") ?? ""));
    expect(mustBeBlockedGroup).toBeDefined();

    fireEvent.click(within(mustBeBlockedGroup!).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));

    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 13]] },
    });
  });

  it("distinguishes stacks by attack target and by minimum-blocker count", () => {
    const { container } = renderBoard(blockersPrompt());
    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));

    const groupWhere = (predicate: (label: string) => boolean) =>
      screen.getAllByRole("group").find((g) => predicate(g.getAttribute("aria-label") ?? ""))!;
    const needsTwo = () => groupWhere((label) => label.includes("Needs 2"));
    const plainPlaneswalker = () =>
      groupWhere((label) => label.includes("(Planeswalker)") && !label.includes("Needs"));
    const plainYou = () =>
      groupWhere((label) => label.startsWith("You") && !label.includes("Must be blocked"));

    fireEvent.click(within(needsTwo()).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 15]] },
    });

    fireEvent.click(within(needsTwo()).getByRole("button", { name: "-1" }));
    fireEvent.click(within(plainPlaneswalker()).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 14]] },
    });

    fireEvent.click(within(plainPlaneswalker()).getByRole("button", { name: "-1" }));
    fireEvent.click(within(plainYou()).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));
    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 12]] },
    });
  });

  it("keeps a prior assignment when switching the pending blocker, and splits by other-assigned blockers", () => {
    const { container } = renderBoard(blockersPrompt());
    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));
    const groupWhere = (predicate: (label: string) => boolean) =>
      screen.getAllByRole("group").find((g) => predicate(g.getAttribute("aria-label") ?? ""))!;
    const plainYou = groupWhere((label) => label.startsWith("You") && !label.includes("Must be blocked"));
    fireEvent.click(within(plainYou).getByRole("button", { name: "+1" }));

    // Switch the pending blocker without confirming — 100 keeps its pick (12).
    clickPermanent(container, 101);
    const blockedByGroup = groupWhere((label) => label.includes("Blocked by Grizzly Bears"));
    const unblockedGroup = groupWhere(
      (label) => label.startsWith("You") && label.includes("Unblocked") && !label.includes("Must be blocked"),
    );
    expect(blockedByGroup).toBeDefined();
    expect(unblockedGroup).toBeDefined();
    expect(within(blockedByGroup).getByText("0 / 1")).toBeInTheDocument();
    expect(within(unblockedGroup).getByText("0 / 1")).toBeInTheDocument();

    fireEvent.click(within(unblockedGroup!).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm Blockers/ }));

    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: expect.arrayContaining([[100, 12], [101, 11]]) },
    });
    const calls = vi.mocked(dispatchAction).mock.calls;
    const call = calls[calls.length - 1][0] as {
      data: { assignments: [number, number][] };
    };
    expect(call.data.assignments).toHaveLength(2);

    // The blocked-count badge reflects both now-assigned pile members.
    expect(screen.getByText("blk 2")).toBeInTheDocument();
  });

  it("assigns a blocker-side must-block requirement (no must-be-blocked axis)", () => {
    const { container } = renderBoard(
      blockersPrompt({
        must_be_blocked_targets: {},
        blocker_constraints: { 100: { kind: "MustBlock", attackers: [13] } },
      }),
    );
    clickPermanent(container, 100);
    fireEvent.click(screen.getByRole("button", { name: "Choose Scute Swarm token" }));

    const mustBlockGroup = screen
      .getAllByRole("group")
      .find((g) => /Must block/.test(g.getAttribute("aria-label") ?? ""));
    expect(mustBlockGroup).toBeDefined();

    fireEvent.click(within(mustBlockGroup!).getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm Blockers (1)" }));

    expect(dispatchAction).toHaveBeenLastCalledWith({
      type: "DeclareBlockers",
      data: { assignments: [[100, 13]] },
    });
  });

  it("offers no picker on the pile when the prompt belongs to another player", () => {
    renderBoard(blockersPrompt({ player: 1 }));
    // Isolate the waitingForPlayer gate from the (also-null, since ActionButton
    // never enters "combat-blockers" mode for another player's prompt) combat
    // mode: force combatMode as if a blocker were already pending.
    useUiStore.setState({ pendingBlocker: 100, combatMode: "blockers" });

    expect(screen.queryByRole("button", { name: "Choose Scute Swarm token" })).not.toBeInTheDocument();
  });
});
