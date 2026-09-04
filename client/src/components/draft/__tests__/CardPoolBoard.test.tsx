import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  DraftCardInstance,
  DraftPoolGroupKind,
  DraftPoolGroups,
} from "../../../adapter/draft-adapter";
import {
  activateWorkspaceInstance,
  buildCardPoolBoardModel,
  createDraftWorkspaceState,
  moveWorkspaceInstance,
  normalizeWorkspaceForBoardGeometry,
  rebuildWorkspaceZone,
  resolveAvailableBoardSort,
  resolveWorkspacePickPlacement,
  resolveWorkspaceSortColumn,
} from "../workspace/workspacePlacement";
import type { DraftWorkspaceState } from "../workspace/types";
import type { DraftBoardPreferences } from "../workspace/workspacePreferences";
import { CardPoolBoard } from "../workspace/CardPoolBoard";
import { WorkspaceCard } from "../workspace/WorkspaceCard";
import type { DraftWorkspaceDragController } from "../workspace/useDraftWorkspaceDrag";

const previewProps = vi.hoisted(() => ({
  current: null as { card: { name: string } | null; mode?: string; hoverDelayMs?: number } | null,
}));
const workspaceImageState = vi.hoisted(() => ({
  defaultSrc: "/card.png" as string | null,
  sources: {} as Record<string, string | null>,
  faceSources: {} as Record<string, string | null>,
}));
const workspaceAlternateFaceState = vi.hoisted(() => ({
  values: {} as Record<string, { name: string; faceIndex: number; side: "front" | "back" } | null>,
}));

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: (cardName: string, options?: { faceIndex?: number }) => ({
    src: Object.prototype.hasOwnProperty.call(
      workspaceImageState.faceSources,
      `${cardName}:${options?.faceIndex ?? 0}`,
    )
      ? workspaceImageState.faceSources[`${cardName}:${options?.faceIndex ?? 0}`]
      : Object.prototype.hasOwnProperty.call(workspaceImageState.sources, cardName)
        ? workspaceImageState.sources[cardName]
        : workspaceImageState.defaultSrc,
    isLoading: false,
    isFlip: false,
    isRotated: false,
  }),
}));

vi.mock("../../../services/scryfall", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../../services/scryfall")>(),
  resolveAlternateCardFaceSync: (cardName: string) => (
    Object.prototype.hasOwnProperty.call(workspaceAlternateFaceState.values, cardName)
      ? workspaceAlternateFaceState.values[cardName]
      : undefined
  ),
}));

vi.mock("../../card/HoverCardPreview", () => ({
  HoverCardPreview: (props: { card: { name: string } | null; mode?: string; hoverDelayMs?: number }) => {
    previewProps.current = props;
    return <div data-testid="workspace-preview">{props.card?.name}</div>;
  },
}));

afterEach(() => {
  cleanup();
  workspaceImageState.defaultSrc = "/card.png";
  workspaceImageState.sources = {};
  workspaceImageState.faceSources = {};
  workspaceAlternateFaceState.values = {};
});

function card(instanceId: string): DraftCardInstance {
  return {
    instance_id: instanceId,
    name: instanceId,
    set_code: "TST",
    collector_number: instanceId,
    rarity: "common",
    colors: [],
    cmc: 1,
    type_line: "Card",
  };
}

function cardWithCmc(instanceId: string, cmc: number): DraftCardInstance {
  return { ...card(instanceId), cmc };
}

function groups(instanceIds: string[]): DraftPoolGroups {
  const cards = instanceIds.map((instanceId) => ({
    card: card(instanceId),
    count: 1,
    instance_ids: [instanceId],
  }));
  const group = { kind: "mana_value1" as const, total: cards.length, cards };
  return {
    color_groups: [],
    type_groups: [],
    cmc_groups: [group],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: null },
    workspace_row_classification: {
      creature_instance_ids: [],
      noncreature_instance_ids: instanceIds,
    },
  };
}

function groupedAxis(
  specs: ReadonlyArray<{ kind: DraftPoolGroupKind; bundles: string[][] }>,
  classificationIds = specs.flatMap((spec) => spec.bundles.flat()),
): DraftPoolGroups {
  const cmc_groups = specs.map((spec) => ({
    kind: spec.kind,
    total: spec.bundles.reduce((total, bundle) => total + bundle.length, 0),
    cards: spec.bundles.map((instanceIds) => ({
      card: card(instanceIds[0]),
      count: instanceIds.length,
      instance_ids: instanceIds,
    })),
  }));
  return {
    ...groups(classificationIds),
    cmc_groups,
    workspace_row_classification: {
      creature_instance_ids: [],
      noncreature_instance_ids: classificationIds,
    },
  };
}

function placedState(
  instanceIds: readonly string[],
  zone: "deck" | "sideboard" = "deck",
): DraftWorkspaceState {
  return {
    ...createDraftWorkspaceState(),
    placements: Object.fromEntries(instanceIds.map((instanceId, order) => [
      instanceId,
      { zone, row: 0, column: 0, order },
    ])),
  };
}

const preferences: DraftBoardPreferences = {
  sort: "cmc",
  columnCount: 3,
  rows: "two",
  showHeaders: true,
};

const boardPreferences = { deck: preferences, sideboard: preferences } as const;

function createDragController(
  overrides: Partial<DraftWorkspaceDragController> = {},
): DraftWorkspaceDragController {
  return {
    announcement: "",
    activeTarget: null,
    dragPreview: null,
    handlePointerDown: vi.fn(),
    handleWorkspacePointerDown: vi.fn(),
    handlePointerMove: vi.fn(),
    handlePointerUp: vi.fn(),
    handlePointerCancel: vi.fn(),
    handleLostPointerCapture: vi.fn(),
    consumeCompatibilityActivation: vi.fn(() => false),
    registerBoard: vi.fn(() => vi.fn()),
    registerColumn: vi.fn(() => vi.fn()),
    registerCollapsedSideboard: vi.fn(),
    dropState: vi.fn(() => ({ zoneActive: false, column: null, row: null })),
    invalidateGeometry: vi.fn(),
    dispose: vi.fn(),
    ...overrides,
  };
}

function firstWorkspaceCard() {
  const pool = [card("first")];
  const model = buildCardPoolBoardModel(
    "deck",
    pool,
    groups(["first"]),
    placedState(["first"]),
    preferences,
  );
  return { card: model.columns[0].rows[0].cards[0], pool };
}

describe("card pool board primitives", () => {
  it("workspace_card_omits_all_drag_work_without_a_drag_capability", () => {
    const model = firstWorkspaceCard().card;
    const onActivate = vi.fn();
    const makeSource = vi.fn();
    const controller = createDragController();
    render(
      <WorkspaceCard
        card={model}
        stackIndex={0}
        onActivate={onActivate}
      />,
    );

    const button = screen.getByRole("button", { name: "Inspect first" });
    fireEvent.pointerDown(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerMove(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerUp(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerCancel(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.lostPointerCapture(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.click(button);

    expect(makeSource).not.toHaveBeenCalled();
    expect(controller.handleWorkspacePointerDown).not.toHaveBeenCalled();
    expect(controller.handlePointerMove).not.toHaveBeenCalled();
    expect(controller.handlePointerUp).not.toHaveBeenCalled();
    expect(controller.handlePointerCancel).not.toHaveBeenCalled();
    expect(controller.handleLostPointerCapture).not.toHaveBeenCalled();
    expect(controller.consumeCompatibilityActivation).not.toHaveBeenCalled();
    expect(onActivate).toHaveBeenCalledWith(model);
  });

  it("workspace_card_uses_the_complete_drag_capability_and_forwards_pointer_lifecycle", () => {
    const { card: model, pool } = firstWorkspaceCard();
    const returnedSource = {
      kind: "workspace" as const,
      instanceIds: [model.instanceId] as const,
      cards: [pool[0]] as const,
      previewWidth: 90,
      previewHeight: 126,
      onDrop: vi.fn(),
    };
    const makeSource = vi.fn(() => returnedSource);
    const controller = createDragController({
      consumeCompatibilityActivation: vi.fn(() => true),
    });
    const onActivate = vi.fn();
    render(
      <WorkspaceCard
        card={model}
        stackIndex={0}
        onActivate={onActivate}
        drag={{ controller, makeSource }}
      />,
    );

    const button = screen.getByRole("button", { name: "Inspect first" });
    button.getBoundingClientRect = () => ({
      width: 90,
      height: 126,
      top: 0,
      right: 90,
      bottom: 126,
      left: 0,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    fireEvent.pointerDown(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerMove(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerUp(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.pointerCancel(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.lostPointerCapture(button, { pointerId: 1, pointerType: "mouse" });
    fireEvent.click(button, { detail: 1 });

    expect(makeSource).toHaveBeenCalledOnce();
    expect(makeSource).toHaveBeenCalledWith(model, 90, 126);
    expect(controller.handleWorkspacePointerDown).toHaveBeenCalledWith(
      expect.objectContaining({ type: "pointerdown" }),
      returnedSource,
    );
    expect(controller.handlePointerMove).toHaveBeenCalledOnce();
    expect(controller.handlePointerUp).toHaveBeenCalledOnce();
    expect(controller.handlePointerCancel).toHaveBeenCalledOnce();
    expect(controller.handleLostPointerCapture).toHaveBeenCalledOnce();
    expect(onActivate).not.toHaveBeenCalled();
  });

  it("wires_complete_drag_capability_through_card_pool_board_column_and_workspace_card", () => {
    const pool = [card("first")];
    const controller = createDragController();
    render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(["first"])}
        workspace={placedState(["first"])}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        dragController={controller}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const button = screen.getByRole("button", { name: "Inspect first" });
    button.getBoundingClientRect = () => ({
      width: 92,
      height: 128,
      top: 0,
      right: 92,
      bottom: 128,
      left: 0,
      x: 0,
      y: 0,
      toJSON: () => ({}),
    });
    fireEvent.pointerDown(button, { pointerId: 3, pointerType: "mouse" });
    fireEvent.pointerMove(button, { pointerId: 3, pointerType: "mouse" });

    expect(controller.handleWorkspacePointerDown).toHaveBeenCalledOnce();
    const source = vi.mocked(controller.handleWorkspacePointerDown).mock.calls[0][1];
    expect(source).toMatchObject({
      kind: "workspace",
      instanceIds: ["first"],
      cards: [pool[0]],
      previewWidth: 92,
      previewHeight: 128,
      onDrop: expect.any(Function),
    });
    expect(controller.handlePointerMove).toHaveBeenCalledOnce();
  });

  it("removes_virtual_basics_but_toggles_drafted_basic_lands_between_zones", () => {
    const draftedLand = { ...card("drafted-land"), type_line: "Basic Land — Island" };
    const initial: DraftWorkspaceState = {
      ...placedState([draftedLand.instance_id, "virtual-land"]),
      virtualBasics: [{ instanceId: "virtual-land", name: "Island" }],
    };
    const poolGroups = groups([draftedLand.instance_id]);

    const removed = activateWorkspaceInstance(
      initial,
      [draftedLand],
      poolGroups,
      boardPreferences,
      "virtual-land",
    );
    expect(removed.virtualBasics).toEqual([]);
    expect(removed.placements["virtual-land"]).toBeUndefined();
    expect(removed.placements["drafted-land"].zone).toBe("deck");

    const movedToSideboard = activateWorkspaceInstance(
      initial,
      [draftedLand],
      poolGroups,
      boardPreferences,
      "drafted-land",
    );
    expect(movedToSideboard.virtualBasics).toEqual(initial.virtualBasics);
    expect(movedToSideboard.placements["drafted-land"].zone).toBe("sideboard");
    expect(activateWorkspaceInstance(
      movedToSideboard,
      [draftedLand],
      poolGroups,
      boardPreferences,
      "drafted-land",
    ).placements["drafted-land"].zone).toBe("deck");
  });

  it("rebuilds_only_the_changed_zone_for_sort_rows_or_column_count", () => {
    const deckPlacement = { zone: "deck", row: 0, column: 2, order: 9 } as const;
    const sideboardPlacement = { zone: "sideboard", row: 1, column: 7, order: 4 } as const;
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: { deck: deckPlacement, sideboard: sideboardPlacement },
    };

    const rebuilt = rebuildWorkspaceZone(
      state,
      "deck",
      [card("deck"), card("sideboard")],
      groups(["deck", "sideboard"]),
      preferences,
    );

    expect(rebuilt.placements.deck).toEqual({ zone: "deck", row: 0, column: 1, order: 0 });
    expect(rebuilt.placements.sideboard).toBe(sideboardPlacement);
  });

  it("appends_only_for_an_explicit_null_anchor_in_the_internally_resolved_row", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        moving: { zone: "deck", row: 0, column: 0, order: 0 },
        first: { zone: "sideboard", row: 1, column: 1, order: 4 },
        second: { zone: "sideboard", row: 1, column: 1, order: 4 },
      },
    };
    const pool = [card("moving"), card("second"), card("first")];

    const moved = moveWorkspaceInstance(
      state,
      pool,
      groups(["moving", "second", "first"]),
      boardPreferences,
      "moving",
      { zone: "sideboard", column: 1, beforeInstanceId: null },
    );

    expect(moved.placements.first).toEqual({ zone: "sideboard", row: 1, column: 1, order: 1 });
    expect(moved.placements.second).toEqual({ zone: "sideboard", row: 1, column: 1, order: 0 });
    expect(moved.placements.moving).toEqual({ zone: "sideboard", row: 1, column: 1, order: 2 });
  });

  it.each([
    [0, 1],
    [1, 0],
  ])("moves_a_workspace_card_from_row_%i_to_explicit_row_%i", (sourceRow, targetRow) => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        moving: { zone: "deck", row: sourceRow, column: 0, order: 0 },
        destination: { zone: "deck", row: targetRow, column: 2, order: 0 },
      },
    };
    const moved = moveWorkspaceInstance(
      state,
      [card("moving"), card("destination")],
      groups(["moving", "destination"]),
      boardPreferences,
      "moving",
      { zone: "deck", column: 2, row: targetRow, beforeInstanceId: null },
    );

    expect(moved.placements.moving).toEqual({
      zone: "deck",
      row: targetRow,
      column: 2,
      order: 1,
    });
  });

  it("preserves_valid_explicit_rows_while_normalizing_board_geometry", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        explicit: { zone: "deck", row: 0, column: 0, order: 0 },
        invalid: { zone: "deck", row: 9, column: 0, order: 1 },
      },
    };
    const poolGroups = groups(["explicit", "invalid"]);
    const normalized = normalizeWorkspaceForBoardGeometry(
      state,
      [card("explicit"), card("invalid")],
      poolGroups,
      { deck: preferences, sideboard: preferences },
    );

    expect(normalized.placements.explicit.row).toBe(0);
    expect(normalized.placements.invalid.row).toBe(1);

    const collapsed = normalizeWorkspaceForBoardGeometry(
      normalized,
      [card("explicit"), card("invalid")],
      poolGroups,
      {
        deck: { ...preferences, rows: "one" },
        sideboard: { ...preferences, rows: "one" },
      },
    );
    expect(Object.values(collapsed.placements).map((placement) => placement.row)).toEqual([0, 0]);
  });

  it.each([
    { deckPreferences: preferences, row: -1 },
    { deckPreferences: preferences, row: 2 },
    { deckPreferences: { ...preferences, rows: "one" as const }, row: 1 },
  ])("rejects_destination_row_$row_outside_the_board_geometry", ({ deckPreferences, row }) => {
    const state = placedState(["moving"]);
    const moved = moveWorkspaceInstance(
      state,
      [card("moving")],
      groups(["moving"]),
      { deck: deckPreferences, sideboard: preferences },
      "moving",
      { zone: "deck", column: 1, row, beforeInstanceId: null },
    );

    expect(moved).toBe(state);
  });

  it("rejects_every_invalid_non_null_anchor_as_an_exact_state_no_op", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        moving: { zone: "deck", row: 1, column: 0, order: 0 },
        destination: { zone: "deck", row: 1, column: 1, order: 0 },
        wrongRow: { zone: "deck", row: 0, column: 1, order: 0 },
        wrongColumn: { zone: "deck", row: 1, column: 2, order: 0 },
        wrongZone: { zone: "sideboard", row: 1, column: 1, order: 0 },
        stale: { zone: "deck", row: 1, column: 1, order: 1 },
      },
    };
    const pool = Object.keys(state.placements)
      .filter((instanceId) => instanceId !== "stale")
      .map(card);
    const poolGroups = groups(pool.map((entry) => entry.instance_id));

    for (const beforeInstanceId of [
      "moving",
      "missing",
      "wrongRow",
      "wrongColumn",
      "wrongZone",
      "stale",
    ]) {
      expect(moveWorkspaceInstance(
        state,
        pool,
        poolGroups,
        boardPreferences,
        "moving",
        { zone: "deck", column: 1, beforeInstanceId },
      )).toBe(state);
    }
    expect(moveWorkspaceInstance(
      state,
      pool,
      poolGroups,
      boardPreferences,
      "moving",
      { zone: "invalid", column: 1, beforeInstanceId: null } as never,
    )).toBe(state);
  });

  it("normalizes_same_stack_moves_by_order_pool_rank_and_instance_id", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        third: { zone: "deck", row: 1, column: 0, order: 7 },
        first: { zone: "deck", row: 1, column: 0, order: 7 },
        second: { zone: "deck", row: 1, column: 0, order: 7 },
      },
    };
    const pool = [card("first"), card("second"), card("third")];

    const moved = moveWorkspaceInstance(
      state,
      pool,
      groups(["first", "second", "third"]),
      boardPreferences,
      "third",
      { zone: "deck", column: 0, beforeInstanceId: "second" },
    );

    expect(Object.entries(moved.placements)
      .sort(([, left], [, right]) => left.order - right.order)
      .map(([instanceId]) => instanceId)).toEqual(["first", "third", "second"]);
  });

  it("allocates_one_board_wide_column_per_color_and_leaves_surplus_columns_empty", () => {
    const ids = ["a0", "a1", "a2", "a3", "a4", "b0", "b1"];
    const poolGroups = {
      ...groups(ids),
      color_groups: [
        { kind: "white" as const, total: 5, cards: ids.slice(0, 5).map((instanceId) => ({ card: card(instanceId), count: 1, instance_ids: [instanceId] })) },
        { kind: "blue" as const, total: 2, cards: ids.slice(5).map((instanceId) => ({ card: card(instanceId), count: 1, instance_ids: [instanceId] })) },
      ],
    };
    const rebuilt = rebuildWorkspaceZone(
      placedState(ids),
      "deck",
      ids.map(card),
      poolGroups,
      { ...preferences, sort: "color", rows: "one", columnCount: 5 },
    );
    const model = buildCardPoolBoardModel(
      "deck",
      ids.map(card),
      poolGroups,
      rebuilt,
      { ...preferences, sort: "color", rows: "one", columnCount: 5 },
    );

    expect(model.columns.map((column) => column.rows[0].cards.map((entry) => entry.instanceId)))
      .toEqual([["a0", "a1", "a2", "a3", "a4"], ["b0", "b1"], [], [], []]);
    expect(model.columns.slice(0, 2).map((column) => column.header.descriptors[0])).toMatchObject([
      { kind: "engine-group", groupKind: "white" },
      { kind: "engine-group", groupKind: "blue" },
    ]);
  });

  it("shares_one_color_and_gold_column_across_both_rows", () => {
    const pool = [
      { ...card("white-creature"), colors: ["W"], type_line: "Creature" },
      { ...card("white-spell"), colors: ["W"], type_line: "Instant" },
      { ...card("gold-creature"), colors: ["W", "U"], type_line: "Creature" },
      { ...card("gold-spell"), colors: ["W", "U"], type_line: "Sorcery" },
    ];
    const poolGroups = {
      ...groups(pool.map((entry) => entry.instance_id)),
      color_groups: [
        {
          kind: "white" as const,
          total: 2,
          cards: pool.slice(0, 2).map((entry) => ({ card: entry, count: 1, instance_ids: [entry.instance_id] })),
        },
        {
          kind: "multicolor" as const,
          total: 2,
          cards: pool.slice(2).map((entry) => ({ card: entry, count: 1, instance_ids: [entry.instance_id] })),
        },
      ],
    };
    const workspace = placedState([]);
    workspace.placements = {
      "white-creature": { zone: "deck", row: 0, column: 4, order: 0 },
      "white-spell": { zone: "deck", row: 1, column: 3, order: 0 },
      "gold-creature": { zone: "deck", row: 0, column: 2, order: 0 },
      "gold-spell": { zone: "deck", row: 1, column: 1, order: 0 },
    };

    const rebuilt = rebuildWorkspaceZone(
      workspace,
      "deck",
      pool,
      poolGroups,
      { ...preferences, sort: "color", rows: "two", columnCount: 5 },
    );

    expect(rebuilt.placements["white-creature"].column).toBe(0);
    expect(rebuilt.placements["white-spell"].column).toBe(0);
    expect(rebuilt.placements["gold-creature"].column).toBe(1);
    expect(rebuilt.placements["gold-spell"].column).toBe(1);
  });

  it.each([
    ["type", "instant", "Instant"],
    ["rarity", "rare", "Instant"],
  ] as const)("shares_one_%s_group_column_across_both_rows", (sort, groupKind, typeLine) => {
    const pool = [
      { ...card("first"), rarity: "rare", type_line: typeLine },
      { ...card("second"), rarity: "rare", type_line: typeLine },
      { ...card("third"), rarity: "rare", type_line: typeLine },
    ];
    const groupedCards = pool.map((entry) => ({
      card: entry,
      count: 1,
      instance_ids: [entry.instance_id],
    }));
    const poolGroups = {
      ...groups(pool.map((entry) => entry.instance_id)),
      type_groups: sort === "type"
        ? [{ kind: groupKind as "instant", total: pool.length, cards: groupedCards }]
        : [],
      rarity_groups: sort === "rarity"
        ? [{ kind: groupKind as "rare", total: pool.length, cards: groupedCards }]
        : [],
      workspace_capabilities: { rarity_group_order: ["rare" as const] },
    };
    const workspace = placedState([]);
    workspace.placements = {
      first: { zone: "deck", row: 0, column: 3, order: 0 },
      second: { zone: "deck", row: 1, column: 2, order: 0 },
      third: { zone: "deck", row: 1, column: 1, order: 1 },
    };

    const rebuilt = rebuildWorkspaceZone(
      workspace,
      "deck",
      pool,
      poolGroups,
      { ...preferences, sort, rows: "two", columnCount: 4 },
    );

    expect(pool.map((entry) => rebuilt.placements[entry.instance_id].column))
      .toEqual([0, 0, 0]);
  });

  it("uses_the_gold_circle_symbol_for_multicolor_sort_headers", () => {
    const multicolorCard = { ...card("gold"), colors: ["W", "U"] };
    const poolGroups = {
      ...groups([multicolorCard.instance_id]),
      color_groups: [{
        kind: "multicolor" as const,
        total: 1,
        cards: [{ card: multicolorCard, count: 1, instance_ids: [multicolorCard.instance_id] }],
      }],
    };
    const model = buildCardPoolBoardModel(
      "deck",
      [multicolorCard],
      poolGroups,
      placedState([multicolorCard.instance_id]),
      { ...preferences, sort: "color" },
    );

    const descriptor = model.columns[0].header.descriptors.find((candidate) => (
      candidate.kind === "engine-group" && candidate.groupKind === "multicolor"
    ));
    expect(descriptor).toMatchObject({
      kind: "engine-group",
      groupKind: "multicolor",
      presentation: {
        kind: "mana-font",
        iconClass: "ms-multicolor ms-duo ms-duo-color ms-grad",
      },
    });
  });

  it("renders_the_gold_circle_in_a_multicolor_column_header", async () => {
    const multicolorCard = { ...card("gold"), colors: ["W", "U"] };
    const poolGroups = {
      ...groups([multicolorCard.instance_id]),
      color_groups: [{
        kind: "multicolor" as const,
        total: 1,
        cards: [{ card: multicolorCard, count: 1, instance_ids: [multicolorCard.instance_id] }],
      }],
    };
    render(
      <CardPoolBoard
        zone="deck"
        pool={[multicolorCard]}
        poolGroups={poolGroups}
        workspace={placedState([multicolorCard.instance_id])}
        preferences={{ ...preferences, sort: "color" }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const header = screen.getByRole("banner", { name: /Multicolor/ });
    await waitFor(() => {
      expect(header.querySelector(".ms-multicolor.ms-duo.ms-duo-color.ms-grad"))
        .toHaveClass("h-[17px]", "w-[17px]", "!text-[17px]");
    });
  });

  it("rebuilds_mana_value_sort_into_fixed_zero_based_columns_with_right_edge_overflow", () => {
    const pool = [
      cardWithCmc("zero", 0),
      cardWithCmc("two", 2),
      cardWithCmc("seven", 7),
    ];
    const next = rebuildWorkspaceZone(
      placedState(pool.map((card) => card.instance_id)),
      "deck",
      pool,
      groups(pool.map((card) => card.instance_id)),
      { ...preferences, rows: "one", columnCount: 4 },
    );

    expect(next.placements.zero.column).toBe(0);
    expect(next.placements.two.column).toBe(2);
    expect(next.placements.seven.column).toBe(3);
  });

  it("coalesces_five_ordered_groups_over_two_columns_without_reordering", () => {
    const ids = ["zero", "one", "two", "three", "four"];
    const colorKinds = ["white", "blue", "black", "red", "green"] as const;
    const poolGroups = {
      ...groups(ids),
      color_groups: ids.map((instanceId, index) => ({
        kind: colorKinds[index],
        total: 1,
        cards: [{ card: card(instanceId), count: 1, instance_ids: [instanceId] }],
      })),
    };
    const next = rebuildWorkspaceZone(
      placedState(ids),
      "deck",
      ids.map(card),
      poolGroups,
      { ...preferences, sort: "color", rows: "one", columnCount: 2 },
    );

    expect(ids.map((instanceId) => next.placements[instanceId].column)).toEqual([0, 0, 0, 1, 1]);
    expect(ids.map((instanceId) => next.placements[instanceId].order)).toEqual([0, 1, 2, 0, 1]);
  });

  it("renders_allocated_color_empty_columns_with_their_set_color", () => {
    const ids = ["first", "second"];
    const poolGroups = {
      ...groups(ids),
      color_groups: [{
        kind: "white" as const,
        total: 2,
        cards: ids.map((instanceId) => ({ card: card(instanceId), count: 1, instance_ids: [instanceId] })),
      }],
    };
    const next = rebuildWorkspaceZone(
      placedState(ids),
      "deck",
      ids.map(card),
      poolGroups,
      { ...preferences, sort: "color", rows: "one", columnCount: 4 },
    );
    const model = buildCardPoolBoardModel(
      "deck",
      ids.map(card),
      poolGroups,
      next,
      { ...preferences, sort: "color", rows: "one", columnCount: 4 },
    );

    expect(model.columns.map((column) => column.count)).toEqual([2, 0, 0, 0]);
    expect(model.columns[1].header.descriptors).toEqual([{
      kind: "engine-group",
      groupKind: "blue",
      labelKey: "pool.groups.blue",
      presentation: { kind: "mana-symbol", shard: "U" },
    }]);
    expect(model.columns[3].header.descriptors).toEqual([{
      kind: "engine-group",
      groupKind: "red",
      labelKey: "pool.groups.red",
      presentation: { kind: "mana-symbol", shard: "R" },
    }]);

    // Sorts without a fixed column identity still fall back to the ordinal.
    const byType = buildCardPoolBoardModel(
      "deck",
      ids.map(card),
      poolGroups,
      next,
      { ...preferences, sort: "type", rows: "one", columnCount: 4 },
    );
    expect(byType.columns[3].header.descriptors).toEqual([{
      kind: "empty-ordinal",
      labelKey: "workspace.headers.emptyOrdinal",
      ordinal: 4,
    }]);
  });

  it("assigns_each_color_column_a_set_color_and_reverts_to_it_when_emptied", () => {
    const blue = { ...card("blue-card"), colors: ["U"] };
    const poolGroups = {
      ...groups([blue.instance_id]),
      color_groups: [{
        kind: "blue" as const,
        total: 1,
        cards: [{ card: blue, count: 1, instance_ids: [blue.instance_id] }],
      }],
    };
    const setColorsFor = (placements: DraftWorkspaceState["placements"]) => buildCardPoolBoardModel(
      "deck",
      [blue],
      poolGroups,
      { ...placedState([]), placements },
      { ...preferences, sort: "color", rows: "one", columnCount: 9 },
    ).columns.map((column) => {
      const descriptor = column.header.descriptors[0];
      return descriptor?.kind === "engine-group" ? descriptor.groupKind : null;
    });

    // Columns past gold fall back to colorless.
    expect(setColorsFor({})).toEqual([
      "white", "blue", "black", "red", "green", "colorless", "multicolor", "colorless", "colorless",
    ]);

    // An occupied column shows its cards' color, wherever it sits.
    expect(setColorsFor({ "blue-card": { zone: "deck", row: 0, column: 4, order: 0 } })[4])
      .toBe("blue");

    // Emptying it restores the column's own color.
    expect(setColorsFor({})[4]).toBe("green");
  });

  it("anchors_mana_value_headers_to_zero_based_columns_and_hides_moved_mismatches", () => {
    const twoDrop = cardWithCmc("two-drop", 2);
    const threeDrop = cardWithCmc("three-drop", 3);
    const pool = [twoDrop, threeDrop];
    const poolGroups = groupedAxis([
      { kind: "mana_value2", bundles: [["two-drop"]] },
      { kind: "mana_value3", bundles: [["three-drop"]] },
    ]);
    const fixedPreferences = { ...preferences, rows: "one" as const, columnCount: 4 };
    const workspace = placedState([]);
    workspace.placements = {
      "two-drop": { zone: "deck", row: 0, column: 2, order: 0 },
      "three-drop": { zone: "deck", row: 0, column: 3, order: 0 },
    };
    const moved = moveWorkspaceInstance(
      workspace,
      pool,
      poolGroups,
      { deck: fixedPreferences, sideboard: fixedPreferences },
      "two-drop",
      { zone: "deck", column: 3, beforeInstanceId: null },
    );
    const model = buildCardPoolBoardModel(
      "deck",
      pool,
      poolGroups,
      moved,
      fixedPreferences,
    );

    expect(moved.placements["two-drop"].column).toBe(3);
    expect(model.columns.map((column) => column.header.descriptors)).toMatchObject([
      [{ kind: "mana-value-column", manaValue: 0 }],
      [{ kind: "mana-value-column", manaValue: 1 }],
      [{ kind: "mana-value-column", manaValue: 2 }],
      [],
    ]);
    const expanded = buildCardPoolBoardModel(
      "deck",
      [],
      groupedAxis([]),
      placedState([]),
      { ...preferences, rows: "one", columnCount: 8 },
    );
    expect(expanded.columns[7].header.descriptors).toMatchObject([
      { kind: "mana-value-column", manaValue: 7 },
    ]);
  });

  it("allows_twenty_columns_and_clamps_larger_counts", () => {
    const pool = [card("first")];
    const poolGroups = groups(["first"]);
    const workspace = placedState(["first"]);

    expect(buildCardPoolBoardModel(
      "deck",
      pool,
      poolGroups,
      workspace,
      { ...preferences, columnCount: 20 },
    ).columns).toHaveLength(20);
    expect(buildCardPoolBoardModel(
      "deck",
      pool,
      poolGroups,
      workspace,
      { ...preferences, columnCount: 21 },
    ).columns).toHaveLength(20);
  });

  it("keeps_the_desktop_control_at_twenty_columns", () => {
    const pool = [card("first")];
    const poolGroups = groups(["first"]);
    const workspace = placedState(["first"]);
    const onPreferencesChange = vi.fn();
    const rendered = render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={poolGroups}
        workspace={workspace}
        preferences={{ ...preferences, columnCount: 19 }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={onPreferencesChange}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Add column" }));
    expect(onPreferencesChange).toHaveBeenCalledWith({ ...preferences, columnCount: 20 });

    rendered.rerender(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={poolGroups}
        workspace={workspace}
        preferences={{ ...preferences, columnCount: 20 }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={onPreferencesChange}
      />,
    );
    expect(screen.getByRole("button", { name: "Add column" })).toBeDisabled();
  });

  it("projects_complete_models_and_supplemental_fallback_headers", () => {
    const pool = [card("classified"), card("missing")];
    const state: DraftWorkspaceState = {
      ...placedState(["classified", "missing"]),
      placements: {
        classified: { zone: "deck", row: 1, column: 2, order: 0 },
        missing: { zone: "deck", row: 1, column: 2, order: 1 },
        basic: { zone: "deck", row: 1, column: 2, order: 2 },
      },
      virtualBasics: [{ instanceId: "basic", name: "Island" }],
    };
    const poolGroups = groupedAxis([{ kind: "mana_value1", bundles: [["classified"]] }]);
    const model = buildCardPoolBoardModel(
      "deck",
      pool,
      poolGroups,
      state,
      preferences,
      { zoneActive: true, column: 2, row: 1 },
    );

    expect(model).toMatchObject({
      key: "workspace-board:deck",
      zone: "deck",
      requestedSort: "cmc",
      effectiveSort: "cmc",
      columnCount: 3,
      rowCount: 2,
      count: 3,
      drop: { state: "active", active: true },
    });
    expect(model.columns[2]).toMatchObject({
      key: "deck:column:2",
      count: 3,
      header: { key: "deck:column:2:header", count: 3 },
      drop: { state: "active" },
    });
    expect(model.columns[2].header.descriptors.map((descriptor) => descriptor.kind))
      .toEqual(["mana-value-column", "added-basics", "unclassified"]);
    expect(model.columns[2].header.descriptors[0]).toMatchObject({ manaValue: 1 });
    expect(model.columns[2].rows[1].drop.state).toBe("active");
    expect(model.columns[2].rows[1].cards[0]).toMatchObject({
      key: "classified",
      instanceId: "classified",
      sourcePrinting: { setCode: "TST", collectorNumber: "classified" },
      image: { cardName: "classified", draggable: false },
      preview: { name: "classified" },
      isVirtualBasic: false,
    });
    expect(model.columns[2].rows[1].cards[2]).toMatchObject({
      key: "basic",
      sourcePrinting: undefined,
      isVirtualBasic: true,
    });
  });

  it("uses_the_fixed_column_value_instead_of_engine_cmc_membership", () => {
    const cardWithDifferentMembership = cardWithCmc("three", 3);
    const workspace = placedState([]);
    workspace.placements.three = { zone: "deck", row: 0, column: 3, order: 0 };
    const model = buildCardPoolBoardModel(
      "deck",
      [cardWithDifferentMembership],
      groupedAxis([{ kind: "mana_value1", bundles: [["three"]] }]),
      workspace,
      { ...preferences, rows: "one", columnCount: 4 },
    );

    expect(model.columns[3].header.descriptors).toMatchObject([
      { kind: "mana-value-column", manaValue: 3 },
    ]);
  });

  it("omits_numeric_cmc_descriptors_for_ambiguous_multi_membership", () => {
    const pool = [cardWithCmc("ambiguous", 3)];
    const poolGroups = groupedAxis([
      { kind: "mana_value2", bundles: [["ambiguous"]] },
      { kind: "mana_value3", bundles: [["ambiguous"]] },
    ]);
    const model = buildCardPoolBoardModel(
      "deck",
      pool,
      poolGroups,
      placedState(["ambiguous"]),
      preferences,
    );

    expect(model.columns[0].header.descriptors.filter((descriptor) => (
      descriptor.kind === "engine-group" && descriptor.presentation.kind === "numeric-badge"
    ))).toEqual([]);
  });

  it("omits_numeric_cmc_descriptors_for_missing_membership", () => {
    const model = buildCardPoolBoardModel(
      "deck",
      [cardWithCmc("missing", 4)],
      groupedAxis([]),
      placedState(["missing"]),
      preferences,
    );

    expect(model.columns[0].header.descriptors.filter((descriptor) => (
      descriptor.kind === "engine-group" && descriptor.presentation.kind === "numeric-badge"
    ))).toEqual([]);
  });

  it("omits_numeric_cmc_descriptors_for_an_all_virtual_column", () => {
    const workspace = placedState(["basic-a", "basic-b"]);
    workspace.virtualBasics = [
      { instanceId: "basic-a", name: "Island" },
      { instanceId: "basic-b", name: "Forest" },
    ];
    const model = buildCardPoolBoardModel(
      "deck",
      [],
      groupedAxis([]),
      workspace,
      preferences,
    );

    expect(model.columns[0].header.descriptors.filter((descriptor) => (
      descriptor.kind === "engine-group" && descriptor.presentation.kind === "numeric-badge"
    ))).toEqual([]);
  });

  it("falls_back_only_unsupported_rarity_and_preserves_rows_while_sorting_by_type", () => {
    const unsupported = { rarity_group_order: null } as const;
    const supported = { rarity_group_order: [] };
    expect(resolveAvailableBoardSort("rarity", unsupported)).toBe("cmc");
    expect(resolveAvailableBoardSort("rarity", supported)).toBe("rarity");
    expect(["cmc", "color", "type"].map((sort) => (
      resolveAvailableBoardSort(sort as DraftBoardPreferences["sort"], unsupported)
    ))).toEqual(["cmc", "color", "type"]);

    const typeGroups = {
      ...groups(["spell"]),
      type_groups: [{
        kind: "other" as const,
        total: 1,
        cards: [{ card: card("spell"), count: 1, instance_ids: ["spell"] }],
      }],
    };
    const next = rebuildWorkspaceZone(
      placedState(["spell"]),
      "deck",
      [card("spell")],
      typeGroups,
      { ...preferences, sort: "type", rows: "two" },
    );
    expect(next.placements.spell.row).toBe(0);
  });

  it("splits_and_merges_classified_rows_in_the_same_columns_with_vertical_headers", () => {
    const pool = [
      { ...card("creature"), type_line: "Creature" },
      { ...card("spell"), type_line: "Instant" },
    ];
    const poolGroups = {
      ...groups(["creature", "spell"]),
      workspace_row_classification: {
        creature_instance_ids: ["creature"],
        noncreature_instance_ids: ["spell"],
      },
    };
    const initial: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        creature: { zone: "deck", row: 0, column: 1, order: 0 },
        spell: { zone: "deck", row: 0, column: 1, order: 1 },
      },
    };
    const workspaceChanges = vi.fn();

    function Harness() {
      const [workspace, setWorkspace] = useState(initial);
      const [layout, setLayout] = useState<DraftBoardPreferences>({ ...preferences, rows: "one" });
      return (
        <CardPoolBoard
          zone="deck"
          pool={pool}
          poolGroups={poolGroups}
          workspace={workspace}
          preferences={layout}
          cardPreviewMode="none"
          cardPreviewHoverDelayMs={0}
          onWorkspaceChange={(next) => {
            workspaceChanges(next);
            setWorkspace(next);
          }}
          onPreferencesChange={setLayout}
        />
      );
    }

    const { container } = render(<Harness />);
    expect(container.querySelector("[data-row-headers]")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Two rows" }));

    const creaturesHeader = screen.getByRole("heading", { name: "Creatures", level: 3 });
    const nonCreaturesHeader = screen.getByRole("heading", { name: "Non-Creatures", level: 3 });
    expect(creaturesHeader.querySelector("span")).toHaveClass("[writing-mode:vertical-rl]", "rotate-180");
    expect(nonCreaturesHeader.querySelector("span")).toHaveClass("[writing-mode:vertical-rl]", "rotate-180");
    expect(screen.getByRole("button", { name: "Inspect creature" }).closest("[data-board-row]"))
      .toHaveAttribute("data-board-row", "0");
    expect(screen.getByRole("button", { name: "Inspect spell" }).closest("[data-board-row]"))
      .toHaveAttribute("data-board-row", "1");
    const cardArea = container.querySelector<HTMLElement>("[data-card-area]")!;
    expect(cardArea).toHaveClass("row-start-2", "row-span-2", "grid-rows-subgrid");
    for (const row of container.querySelectorAll("[data-board-row]")) {
      expect(row).toHaveClass("border", "border-hairline");
      expect(row).toHaveClass(row.getAttribute("data-board-row") === "1"
        ? "rounded-[7px]"
        : "relative");
      expect(row).toHaveStyle({ gridRow: String(Number(row.getAttribute("data-board-row")) + 1) });
    }
    expect(workspaceChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      placements: expect.objectContaining({
        creature: { zone: "deck", row: 0, column: 1, order: 0 },
        spell: { zone: "deck", row: 1, column: 1, order: 0 },
      }),
    }));

    fireEvent.change(screen.getByRole("combobox", { name: "Sort board" }), {
      target: { value: "color" },
    });

    expect(screen.getByRole("button", { name: "Inspect creature" }).closest("[data-board-row]"))
      .toHaveAttribute("data-board-row", "0");
    expect(screen.getByRole("button", { name: "Inspect spell" }).closest("[data-board-row]"))
      .toHaveAttribute("data-board-row", "1");
    const sortedWorkspace = workspaceChanges.mock.calls[workspaceChanges.mock.calls.length - 1][0] as DraftWorkspaceState;
    expect(sortedWorkspace.placements.creature.row).toBe(0);
    expect(sortedWorkspace.placements.spell.row).toBe(1);

    fireEvent.click(screen.getByRole("button", { name: "One row" }));

    expect(container.querySelector("[data-row-headers]")).not.toBeInTheDocument();
    for (const row of container.querySelectorAll("[data-board-row]")) {
      expect(row).not.toHaveClass("border", "border-hairline", "bg-black/28");
    }
    expect(workspaceChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      placements: expect.objectContaining({
        creature: { zone: "deck", row: 0, column: 2, order: 0 },
        spell: { zone: "deck", row: 0, column: 2, order: 1 },
      }),
    }));
  });

  it("resolves_confirmed_picks_to_a_retained_sort_designation_or_the_leftmost_column", () => {
    const retained = {
      ...card("retained"),
      colors: ["U"],
      cmc: 4,
      rarity: "rare",
      type_line: "Artifact Creature",
    };
    const selected = { ...retained, instance_id: "selected" };
    const poolGroups = {
      ...groups(["retained"]),
      color_groups: [{ kind: "blue" as const, total: 1, cards: [{ card: retained, count: 1, instance_ids: ["retained"] }] }],
      cmc_groups: [{ kind: "mana_value4" as const, total: 1, cards: [{ card: retained, count: 1, instance_ids: ["retained"] }] }],
      rarity_groups: [{ kind: "rare" as const, total: 1, cards: [{ card: retained, count: 1, instance_ids: ["retained"] }] }],
      type_groups: [{ kind: "creature" as const, total: 1, cards: [{ card: retained, count: 1, instance_ids: ["retained"] }] }],
      workspace_capabilities: { rarity_group_order: ["rare" as const] },
    };
    const workspace = {
      ...placedState([]),
      placements: { retained: { zone: "deck" as const, row: 0, column: 2, order: 0 } },
    };

    for (const sort of ["cmc", "color", "rarity", "type"] as const) {
      expect(resolveWorkspaceSortColumn(
        selected,
        "deck",
        [retained],
        poolGroups,
        workspace,
        { ...preferences, sort },
      )).toBe(2);
    }

    expect(resolveWorkspaceSortColumn(
      selected,
      "deck",
      [retained],
      poolGroups,
      workspace,
      { ...preferences, sort: "cmc", columnCount: 7 },
    )).toBe(4);

    expect(resolveWorkspaceSortColumn(
      { ...selected, colors: ["G"] },
      "deck",
      [retained],
      poolGroups,
      workspace,
      { ...preferences, sort: "color" },
    )).toBe(0);
  });

  it("resolves_a_new_color_to_its_set_column", () => {
    const blue = { ...card("blue"), colors: ["U"] };
    const red = { ...card("red"), colors: ["R"] };
    const poolGroups = {
      ...groups([blue.instance_id]),
      color_groups: [{
        kind: "blue" as const,
        total: 1,
        cards: [{ card: blue, count: 1, instance_ids: [blue.instance_id] }],
      }],
    };
    const workspace: DraftWorkspaceState = {
      ...placedState([]),
      placements: { blue: { zone: "deck", row: 0, column: 0, order: 0 } },
    };

    expect(resolveWorkspaceSortColumn(
      red,
      "deck",
      [blue],
      poolGroups,
      workspace,
      { ...preferences, sort: "color", columnCount: 4 },
    )).toBe(3);
  });

  it("prefers_an_existing_color_column_over_an_earlier_empty_column", () => {
    const red = { ...card("red"), colors: ["R"] };
    const selectedRed = { ...red, instance_id: "selected-red" };
    const poolGroups = {
      ...groups([red.instance_id]),
      color_groups: [{
        kind: "red" as const,
        total: 1,
        cards: [{ card: red, count: 1, instance_ids: [red.instance_id] }],
      }],
    };
    const workspace: DraftWorkspaceState = {
      ...placedState([]),
      placements: { red: { zone: "deck", row: 0, column: 2, order: 0 } },
    };

    expect(resolveWorkspaceSortColumn(
      selectedRed,
      "deck",
      [red],
      poolGroups,
      workspace,
      { ...preferences, sort: "color", columnCount: 4 },
    )).toBe(2);
  });

  it("resolves_the_pick_row_before_using_shared_color_column_designations", () => {
    const redSpell = { ...card("red-spell"), colors: ["R"], type_line: "Instant" };
    const redCreature = { ...card("selected-red-creature"), colors: ["R"], type_line: "Creature — Goblin" };
    const poolGroups = {
      ...groups([redSpell.instance_id]),
      color_groups: [{
        kind: "red" as const,
        total: 1,
        cards: [{ card: redSpell, count: 1, instance_ids: [redSpell.instance_id] }],
      }],
    };
    const workspace: DraftWorkspaceState = {
      ...placedState([]),
      placements: { [redSpell.instance_id]: { zone: "deck", row: 1, column: 2, order: 0 } },
    };

    expect(resolveWorkspacePickPlacement(
      redCreature,
      "deck",
      [redSpell],
      poolGroups,
      workspace,
      { ...preferences, sort: "color", rows: "two", columnCount: 4 },
    )).toEqual({ column: 2, row: 0 });
  });

  it("does_not_treat_a_mixed_shared_header_as_a_matching_color_column", () => {
    const whiteCreature = { ...card("white-creature"), colors: ["W"], type_line: "Creature — Human" };
    const redSpell = { ...card("red-spell"), colors: ["R"], type_line: "Instant" };
    const selectedRedSpell = { ...redSpell, instance_id: "selected-red-spell" };
    const poolGroups = {
      ...groups([whiteCreature.instance_id, redSpell.instance_id]),
      color_groups: [
        { kind: "white" as const, total: 1, cards: [{ card: whiteCreature, count: 1, instance_ids: [whiteCreature.instance_id] }] },
        { kind: "red" as const, total: 1, cards: [{ card: redSpell, count: 1, instance_ids: [redSpell.instance_id] }] },
      ],
    };
    const workspace: DraftWorkspaceState = {
      ...placedState([]),
      placements: {
        [whiteCreature.instance_id]: { zone: "deck", row: 0, column: 0, order: 0 },
        [redSpell.instance_id]: { zone: "deck", row: 1, column: 0, order: 0 },
      },
    };

    expect(resolveWorkspacePickPlacement(
      selectedRedSpell,
      "deck",
      [whiteCreature, redSpell],
      poolGroups,
      workspace,
      { ...preferences, sort: "color", rows: "two", columnCount: 4 },
    )).toEqual({ column: 3, row: 1 });
  });

  it("keeps_header_toggles_out_of_placement_rebuilds_and_moves_with_exact_keyboard_anchors", () => {
    const pool = [card("first"), card("second")];
    const poolGroups = groups(["first", "second"]);
    const initial: DraftWorkspaceState = {
      ...placedState(["first", "second"]),
      placements: {
        first: { zone: "deck", row: 1, column: 0, order: 0 },
        second: { zone: "deck", row: 1, column: 0, order: 1 },
      },
    };
    const workspaceChanges = vi.fn();

    function Harness() {
      const [state, setState] = useState(initial);
      const [layout, setLayout] = useState(preferences);
      return (
        <CardPoolBoard
          zone="deck"
          pool={pool}
          poolGroups={poolGroups}
          workspace={state}
          preferences={layout}
          cardPreviewMode="side"
          cardPreviewHoverDelayMs={0}
          onWorkspaceChange={(next) => {
            workspaceChanges(next);
            setState(next);
          }}
          onPreferencesChange={setLayout}
        />
      );
    }

    render(<Harness />);
    expect(screen.queryByRole("button", { name: /Actions/ })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("checkbox", { name: "Show headers" }));
    expect(workspaceChanges).not.toHaveBeenCalled();

    const second = screen.getByRole("button", { name: "Inspect second" });
    fireEvent.mouseEnter(second);
    expect(previewProps.current).toMatchObject({
      card: { name: "second" },
      mode: "side",
      hoverDelayMs: 0,
    });
    second.focus();
    fireEvent.keyDown(second, { key: "ArrowUp", ctrlKey: true });
    expect(workspaceChanges).toHaveBeenCalledTimes(1);
    expect(workspaceChanges.mock.calls[0][0].placements).toMatchObject({
      second: { zone: "deck", row: 1, column: 0, order: 0 },
      first: { zone: "deck", row: 1, column: 0, order: 1 },
    });
    expect(document.activeElement).toHaveAccessibleName("Inspect second");
  });

  it("activates_card_clicks_for_deck_sideboard_and_virtual_basic_removal", () => {
    const draftedLand = { ...card("drafted-land"), type_line: "Basic Land — Island" };
    const poolGroups = groups([draftedLand.instance_id]);
    const deckChanges = vi.fn();
    render(
      <CardPoolBoard
        zone="deck"
        pool={[draftedLand]}
        poolGroups={poolGroups}
        workspace={placedState([draftedLand.instance_id])}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        oppositeZonePreferences={preferences}
        onWorkspaceChange={deckChanges}
        onPreferencesChange={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Inspect drafted-land" }));
    expect(deckChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      placements: expect.objectContaining({
        "drafted-land": expect.objectContaining({ zone: "sideboard" }),
      }),
    }));
    cleanup();

    const sideboardChanges = vi.fn();
    render(
      <CardPoolBoard
        zone="sideboard"
        pool={[draftedLand]}
        poolGroups={poolGroups}
        workspace={placedState([draftedLand.instance_id], "sideboard")}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        oppositeZonePreferences={preferences}
        onWorkspaceChange={sideboardChanges}
        onPreferencesChange={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Inspect drafted-land" }));
    expect(sideboardChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      placements: expect.objectContaining({
        "drafted-land": expect.objectContaining({ zone: "deck" }),
      }),
    }));
    cleanup();

    const basicChanges = vi.fn();
    const virtualWorkspace = placedState(["virtual-land"]);
    virtualWorkspace.virtualBasics = [{ instanceId: "virtual-land", name: "Island" }];
    render(
      <CardPoolBoard
        zone="deck"
        pool={[]}
        poolGroups={groups([])}
        workspace={virtualWorkspace}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        oppositeZonePreferences={preferences}
        onWorkspaceChange={basicChanges}
        onPreferencesChange={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Inspect Island" }));
    expect(basicChanges).toHaveBeenLastCalledWith(expect.objectContaining({
      placements: {},
      virtualBasics: [],
    }));
  });

  it("fits_all_columns_within_the_available_board_width", () => {
    const pool = [card("first")];
    const { container } = render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(["first"])}
        workspace={placedState(["first"])}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const columns = container.querySelector<HTMLElement>("[data-board-columns]");
    expect(columns?.style.gridTemplateColumns).toBe(
      "repeat(3, minmax(0, 1fr))",
    );
    expect(columns).toHaveClass("min-w-0", "flex-1");
    expect(columns).not.toHaveClass("min-w-max");
    expect(columns?.parentElement).toHaveClass("p-6");
    expect(columns?.parentElement?.parentElement).toHaveClass("overflow-x-hidden");
  });

  it("groups_phone_columns_without_changing_logical_placement_or_two_row_structure", () => {
    const pool = Array.from({ length: 7 }, (_, index) => card(`card-${index}`));
    const workspace = {
      ...placedState(pool.map((entry) => entry.instance_id)),
      placements: Object.fromEntries(pool.map((entry, index) => [
        entry.instance_id,
        { zone: "deck" as const, row: index % 2, column: index, order: 0 },
      ])),
    };
    const { container, rerender } = render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(pool.map((entry) => entry.instance_id))}
        workspace={workspace}
        preferences={{ ...preferences, columnCount: 7, showHeaders: false }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        visualColumnCap={3}
        forceShowHeaders
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const visualGroups = [...container.querySelectorAll<HTMLElement>("[data-board-column-group]")];
    expect(visualGroups).toHaveLength(3);
    expect(visualGroups.map((group) => group.querySelectorAll("[data-board-column]").length))
      .toEqual([3, 3, 1]);
    expect(visualGroups.map((group) => group.querySelectorAll("[data-row-headers]").length))
      .toEqual([1, 1, 1]);
    expect([...container.querySelectorAll<HTMLElement>("[data-board-column]")]
      .map((column) => column.dataset.boardColumn)).toEqual(["0", "1", "2", "3", "4", "5", "6"]);
    expect(container.querySelectorAll("header[aria-label^='Column ']")).toHaveLength(7);
    expect(container.querySelector("[data-board-columns]")).toHaveClass("p-6");

    rerender(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(pool.map((entry) => entry.instance_id))}
        workspace={workspace}
        preferences={{ ...preferences, columnCount: 7, showHeaders: false }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        visualColumnCap={6}
        forceShowHeaders
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );
    const landscapeGroups = [...container.querySelectorAll<HTMLElement>("[data-board-column-group]")];
    expect(landscapeGroups.map((group) => group.querySelectorAll("[data-board-column]").length))
      .toEqual([6, 1]);
  });

  it("glows_the_entire_active_one_row_card_area_without_its_header", () => {
    const pool = [card("first")];
    const { container } = render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(["first"])}
        workspace={placedState(["first"])}
        preferences={{ ...preferences, rows: "one" }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        dropState={{ zoneActive: true, column: 1, row: null }}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const columns = container.querySelectorAll<HTMLElement>("section[data-drop-state]");
    const board = container.querySelector<HTMLElement>("[data-board-columns]")?.parentElement;
    const panel = container.firstElementChild;
    const cardArea = columns[1].querySelector<HTMLElement>("[data-card-area]")!;
    const header = columns[1].querySelector("header")!;
    expect(columns[1]).toHaveAttribute("data-drop-state", "active");
    expect(columns[1]).toHaveClass("border-hairline");
    expect(columns[1]).not.toHaveClass("border-white", "bg-white/10");
    expect(cardArea).toHaveClass("draft-card-area-drop-active");
    expect(header.contains(cardArea)).toBe(false);
    expect(columns[1].querySelector('[data-drop-highlight="active"]')).not.toBeInTheDocument();
    expect(columns[0]).toHaveAttribute("data-drop-state", "idle");
    expect(columns[0].querySelector("[data-card-area]")).not.toHaveClass("draft-card-area-drop-active");
    expect(panel).toHaveClass("border-hairline");
    expect(board).not.toHaveClass("border-dashed", "border-amber-300");
  });

  it("glows_the_entire_active_two_row_card_area_while_preserving_row_targeting", () => {
    const pool = [card("first")];
    const { container, rerender } = render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(["first"])}
        workspace={placedState(["first"])}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        dropState={{ zoneActive: true, column: 1, row: 1 }}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const activeColumn = container.querySelectorAll<HTMLElement>("section[data-drop-state]")[1];
    const rows = activeColumn.querySelectorAll<HTMLElement>("[data-board-row]");
    const cardArea = activeColumn.querySelector<HTMLElement>("[data-card-area]")!;
    expect(activeColumn).toHaveAttribute("data-drop-state", "active");
    expect(rows[0]).toHaveAttribute("data-drop-state", "idle");
    expect(rows[1]).toHaveAttribute("data-drop-state", "active");
    expect(cardArea).toHaveClass("draft-card-area-drop-active", "row-start-2", "row-span-2");
    expect(activeColumn.querySelector("header")?.contains(cardArea)).toBe(false);
    expect(activeColumn.querySelector("header")).toHaveClass("z-10");
    expect(activeColumn.querySelector('[data-drop-highlight="active"]')).not.toBeInTheDocument();
    expect(rows[0]).toHaveStyle({ gridRow: "1" });
    expect(rows[1]).toHaveStyle({ gridRow: "2" });

    rerender(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(["first"])}
        workspace={placedState(["first"])}
        preferences={{ ...preferences, showHeaders: false }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        dropState={{ zoneActive: true, column: 1, row: 1 }}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );
    expect(container.querySelector<HTMLElement>("[data-card-area]")).toHaveClass("rounded-[8px]");
  });

  it("reveals_sixteen_percent_of_the_card_width_between_stacked_cards", () => {
    const pool = [card("first"), card("second")];
    render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={groups(["first", "second"])}
        workspace={placedState(["first", "second"])}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Inspect first" }).parentElement?.style.marginTop).toBe("");
    const second = screen.getByRole("button", { name: "Inspect second" });
    const secondWrapper = second.parentElement;
    expect(second.closest("section")?.querySelector("[data-card-area]")).toHaveClass("grid-rows-subgrid", "row-span-2");
    expect(second.closest("section")?.querySelector("[data-card-area]")).not.toHaveClass("p-2");
    expect(secondWrapper?.style.marginTop).toBe("-123.3442622951%");
    second.getBoundingClientRect = () => ({
      top: 100,
      left: 0,
      right: 100,
      bottom: 239,
      width: 100,
      height: 139,
      x: 0,
      y: 100,
      toJSON: () => ({}),
    });
    fireEvent.pointerMove(second, { clientY: 115, pointerType: "mouse" });
    expect(secondWrapper).toHaveClass("z-10");
    fireEvent.pointerMove(second, { clientY: 117, pointerType: "mouse" });
    expect(secondWrapper).not.toHaveClass("z-10");
  });

  it.each(["deck", "sideboard"] as const)(
    "shows_a_face_toggle_and_swaps_%s_card_faces_when_clicked",
    (zone) => {
      const doubleFaced = { ...card("dfc"), name: "Front // Back" };
      workspaceAlternateFaceState.values = {
        "Front // Back": { name: "Back", faceIndex: 1, side: "back" },
      };
      workspaceImageState.sources = {
        "Front // Back": "/front.png",
        "": null,
      };
      workspaceImageState.faceSources = { "Front // Back:1": "/back.png" };
      render(
        <CardPoolBoard
          zone={zone}
          pool={[doubleFaced]}
          poolGroups={groups(["dfc"])}
          workspace={placedState(["dfc"], zone)}
          preferences={preferences}
          cardPreviewMode="none"
          cardPreviewHoverDelayMs={0}
          onWorkspaceChange={vi.fn()}
          onPreferencesChange={vi.fn()}
        />,
      );

      expect(screen.queryByText("Hold Ctrl for back face")).not.toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Show other face of Front // Back" }));
      expect(screen.getByRole("img", { name: "Back" })).toHaveAttribute("src", "/back.png");
      fireEvent.click(screen.getByRole("button", { name: "Show other face of Front // Back" }));
      expect(screen.getByRole("img", { name: "Front // Back" })).toHaveAttribute("src", "/front.png");
    },
  );

  it("shows_the_face_toggle_for_a_double_faced_workspace_card_without_an_image", () => {
    const doubleFaced = { ...card("dfc"), name: "Front // Back" };
    workspaceAlternateFaceState.values = {
      "Front // Back": { name: "Back", faceIndex: 1, side: "back" },
    };
    workspaceImageState.defaultSrc = null;
    workspaceImageState.sources = { "Front // Back": null, Back: null, "": null };
    workspaceImageState.faceSources = { "Front // Back:1": null };
    render(
      <CardPoolBoard
        zone="deck"
        pool={[doubleFaced]}
        poolGroups={groups(["dfc"])}
        workspace={placedState(["dfc"])}
        preferences={preferences}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    expect(screen.getByText("Front // Back")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show other face of Front // Back" })).toBeInTheDocument();
  });

  it("renders_compact_single_line_headers_with_only_one_valid_sort_presentation", () => {
    const pool = [card("blue-card"), card("red-card")];
    const blueGroup = { kind: "blue" as const, total: 1, cards: [{ card: pool[0], count: 1, instance_ids: ["blue-card"] }] };
    const redGroup = { kind: "red" as const, total: 1, cards: [{ card: pool[1], count: 1, instance_ids: ["red-card"] }] };
    const poolGroups = { ...groups(["blue-card", "red-card"]), color_groups: [blueGroup, redGroup] };
    const { rerender } = render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={poolGroups}
        workspace={placedState(["blue-card"])}
        preferences={{ ...preferences, sort: "color" }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const validHeader = screen.getByRole("banner", { name: /Column 1: Blue and 1 CMC, 1 card/ });
    expect(validHeader.closest("section")).toHaveClass("select-none", "caret-transparent");
    expect(validHeader).toHaveClass("h-8", "min-h-8", "overflow-hidden", "whitespace-nowrap");
    expect(validHeader.querySelector("[data-sort-designation]")).toHaveClass("absolute", "left-1/2", "-translate-x-1/2");
    expect(validHeader.querySelector("img[alt='U']"))
      .toHaveClass("!h-[17px]", "!w-[17px]");
    expect(within(validHeader).queryByText("Blue")).not.toBeInTheDocument();
    const cardCount = within(validHeader).getByText("1");
    expect(cardCount).toHaveAttribute("data-card-count");
    expect(cardCount).toHaveClass("text-sm", "text-fg");
    expect(cardCount).not.toHaveClass("text-fg-meta");
    expect(within(validHeader).queryByText("(1)")).not.toBeInTheDocument();
    expect(within(validHeader).getByRole("button", { name: "Remove column 1" })).toHaveClass("h-6", "w-6");
    expect(screen.queryByText("Empty column 2")).not.toBeInTheDocument();

    rerender(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={poolGroups}
        workspace={placedState(["blue-card", "red-card"])}
        preferences={{ ...preferences, sort: "color" }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );
    const mixedHeader = screen.getByRole("banner", { name: /Column 1: .*Blue.*Red.*2 cards/ });
    expect(mixedHeader.querySelector("img[alt='U']")).not.toBeInTheDocument();
    expect(mixedHeader.querySelector("img[alt='R']")).not.toBeInTheDocument();
    expect(within(mixedHeader).queryByText(/Blue|Red/)).not.toBeInTheDocument();
    expect(within(mixedHeader).getByText("2")).toHaveAttribute("data-card-count");
    expect(within(mixedHeader).queryByText("(2)")).not.toBeInTheDocument();
  });

  it("renders_mana_value_headers_with_numeric_mana_symbols", () => {
    const pool = [cardWithCmc("three-drop", 3), cardWithCmc("high-drop", 7)];
    const poolGroups = groupedAxis([
      { kind: "mana_value3", bundles: [["three-drop"]] },
      { kind: "mana_value6_plus", bundles: [["high-drop"]] },
    ]);
    const { rerender } = render(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={poolGroups}
        workspace={{ ...placedState([]), placements: { "three-drop": { zone: "deck", row: 0, column: 3, order: 0 } } }}
        preferences={{ ...preferences, sort: "cmc", columnCount: 8 }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const threeHeader = screen.getByRole("banner", { name: /Mana value 3, 1 card/ });
    expect(threeHeader.querySelector('[data-mana-value-badge="3"] img[alt="3"]'))
      .toHaveClass("!h-[17px]", "!w-[17px]");

    rerender(
      <CardPoolBoard
        zone="deck"
        pool={pool}
        poolGroups={poolGroups}
        workspace={{ ...placedState([]), placements: { "high-drop": { zone: "deck", row: 0, column: 7, order: 0 } } }}
        preferences={{ ...preferences, sort: "cmc", columnCount: 8 }}
        cardPreviewMode="none"
        cardPreviewHoverDelayMs={0}
        onWorkspaceChange={vi.fn()}
        onPreferencesChange={vi.fn()}
      />,
    );

    const highHeader = screen.getByRole("banner", { name: /Mana value 7, 1 card/ });
    expect(highHeader.querySelector('[data-mana-value-badge="7"] img[alt="7"]')).toBeInTheDocument();
  });

  it("adopts_a_shared_mana_value_per_column_and_reverts_when_emptied", () => {
    const pool = [cardWithCmc("five-a", 5), cardWithCmc("five-b", 5), cardWithCmc("two", 2)];
    const poolGroups = groupedAxis([
      { kind: "mana_value5", bundles: [["five-a"], ["five-b"]] },
      { kind: "mana_value2", bundles: [["two"]] },
    ]);
    const columnHeader = (placements: DraftWorkspaceState["placements"]) => buildCardPoolBoardModel(
      "deck",
      pool,
      poolGroups,
      { ...placedState([]), placements },
      { ...preferences, sort: "cmc", columnCount: 8 },
    ).columns[3].header.descriptors.find((descriptor) => descriptor.kind === "mana-value-column");

    // Uniform occupants override the column's own value.
    expect(columnHeader({
      "five-a": { zone: "deck", row: 0, column: 3, order: 0 },
      "five-b": { zone: "deck", row: 0, column: 3, order: 1 },
    })).toMatchObject({ manaValue: 5, presentation: { text: "5" } });

    // Mixed occupants leave the column undesignated.
    expect(columnHeader({
      "five-a": { zone: "deck", row: 0, column: 3, order: 0 },
      two: { zone: "deck", row: 0, column: 3, order: 1 },
    })).toBeUndefined();

    // Emptying the column restores its original value.
    expect(columnHeader({})).toMatchObject({ manaValue: 3, presentation: { text: "3" } });
  });

  it("announces_the_final_column_removed_from_a_header_control", () => {
    const pool = [card("first")];
    const poolGroups = groups(["first"]);

    function Harness() {
      const [state, setState] = useState(placedState(["first"]));
      const [layout, setLayout] = useState({ ...preferences, columnCount: 5 });
      return (
        <CardPoolBoard
          zone="deck"
          pool={pool}
          poolGroups={poolGroups}
          workspace={state}
          preferences={layout}
          cardPreviewMode="none"
          cardPreviewHoverDelayMs={0}
          onWorkspaceChange={setState}
          onPreferencesChange={setLayout}
        />
      );
    }

    const { container } = render(<Harness />);
    fireEvent.click(screen.getByRole("button", { name: "Remove column 3" }));
    expect(container.querySelector('[aria-atomic="true"]')).toHaveTextContent(
      "Removed column 5 and rebuilt the board.",
    );
  });
});
