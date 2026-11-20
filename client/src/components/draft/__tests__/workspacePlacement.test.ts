import { describe, expect, it } from "vitest";
import type {
  DraftCardInstance,
  DraftPoolGroupKind,
  DraftPoolGroups,
} from "../../../adapter/draft-adapter";
import {
  appendWorkspaceInstanceToResolvedDestination,
  createDraftWorkspaceState,
  moveWorkspaceInstance,
  placeArrivingPoolCards,
  resolveWorkspacePickPlacement,
  rebuildWorkspaceZone,
  reconcileWorkspaceState,
  updateWorkspacePlacement,
} from "../workspace/workspacePlacement";
import type { DraftWorkspaceState, DraftZone } from "../workspace/types";
import type { DraftBoardPreferences } from "../workspace/workspacePreferences";

function card(instanceId: string, name = "Shared Name"): DraftCardInstance {
  return {
    instance_id: instanceId,
    name,
    set_code: "TST",
    collector_number: instanceId,
    rarity: "common",
    colors: [],
    cmc: 1,
    type_line: "Card",
  };
}

function groups(): DraftPoolGroups {
  return {
    color_groups: [], type_groups: [], cmc_groups: [], rarity_groups: [],
    type_filter_options: [], color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: null },
    workspace_row_classification: { creature_instance_ids: [], noncreature_instance_ids: [] },
  };
}

function boardPreferences(): Record<DraftZone, DraftBoardPreferences> {
  return {
    deck: { sort: "cmc", columnCount: 6, rows: "two", showHeaders: true },
    sideboard: { sort: "cmc", columnCount: 6, rows: "two", showHeaders: true },
  };
}

describe("workspace placement", () => {

  describe("color sort with fewer columns than color groups", () => {
    const COLOR_KINDS: DraftPoolGroupKind[] = [
      "white", "blue", "black", "red", "green", "multicolor", "colorless",
    ];

    function sideboardColumns(columnCount: number): Record<string, number> {
      const pool = COLOR_KINDS.map((kind) => card(kind, kind));
      const colorGroups: DraftPoolGroups["color_groups"] = pool.map((entry, index) => ({
        kind: COLOR_KINDS[index],
        total: 1,
        cards: [{ card: entry, count: 1, instance_ids: [entry.instance_id] }],
      }));
      const state: DraftWorkspaceState = {
        ...createDraftWorkspaceState(),
        placements: Object.fromEntries(
          pool.map((entry) => [entry.instance_id, { zone: "sideboard", row: 0, column: 0, order: 0 }]),
        ),
      };
      const rebuilt = rebuildWorkspaceZone(
        state,
        "sideboard",
        pool,
        { ...groups(), color_groups: colorGroups },
        { sort: "color", columnCount, rows: "one", showHeaders: true },
      );
      return Object.fromEntries(
        COLOR_KINDS.map((kind) => [kind, rebuilt.placements[kind].column]),
      );
    }

    it("keeps_white_and_blue_in_their_own_columns_at_six_columns", () => {
      const columns = sideboardColumns(6);

      // Seven color groups on six columns: only the catch-all pair shares.
      expect(columns).toEqual({
        white: 0, blue: 1, black: 2, red: 3, green: 4, multicolor: 5, colorless: 5,
      });
    });

    // A later pick joins the column its group was sorted into, including a
    // column the allocator shared between groups, for every sort and count.
    function laterPicksJoinTheirGroupColumn(
      sort: "color" | "type",
      kinds: readonly DraftPoolGroupKind[],
      columnCount: number,
      traits: Record<string, Partial<DraftCardInstance>>,
    ): Record<string, [number, number]> {
      const pool = kinds.map((kind) => ({ ...card(kind, kind), ...traits[kind] }));
      const axis = pool.map((entry, index) => ({
        kind: kinds[index],
        total: 1,
        cards: [{ card: entry, count: 1, instance_ids: [entry.instance_id] }],
      }));
      const poolGroups = sort === "color"
        ? { ...groups(), color_groups: axis }
        : { ...groups(), type_groups: axis };
      const preferences: DraftBoardPreferences = { sort, columnCount, rows: "one", showHeaders: true };
      const state: DraftWorkspaceState = {
        ...createDraftWorkspaceState(),
        placements: Object.fromEntries(
          pool.map((entry) => [entry.instance_id, { zone: "sideboard", row: 0, column: 0, order: 0 }]),
        ),
      };
      const rebuilt = rebuildWorkspaceZone(state, "sideboard", pool, poolGroups, preferences);
      return Object.fromEntries(pool.map((entry) => [
        entry.name,
        [
          rebuilt.placements[entry.instance_id].column,
          resolveWorkspacePickPlacement(
            { ...entry, instance_id: `later-${entry.instance_id}` },
            "sideboard", pool, poolGroups, rebuilt, preferences,
          ).column,
        ] as [number, number],
      ]));
    }

    const COLOR_TRAITS: Record<string, Partial<DraftCardInstance>> = {
      white: { colors: ["W"] }, blue: { colors: ["U"] }, black: { colors: ["B"] },
      red: { colors: ["R"] }, green: { colors: ["G"] },
      multicolor: { colors: ["W", "U"] }, colorless: { colors: [] },
    };

    it("sends_later_picks_to_the_merged_column_after_a_six_column_sort", () => {
      const placed = laterPicksJoinTheirGroupColumn("color", COLOR_KINDS, 6, COLOR_TRAITS);
      expect(placed.multicolor).toEqual([5, 5]);
      expect(placed.colorless).toEqual([5, 5]);
      expect(placed.white).toEqual([0, 0]);
      expect(placed.blue).toEqual([1, 1]);
    });

    it("sends_later_picks_to_a_shared_column_when_the_pair_is_not_multicolor_and_colorless", () => {
      const noMulticolor = COLOR_KINDS.filter((kind) => kind !== "multicolor");
      const placed = laterPicksJoinTheirGroupColumn("color", noMulticolor, 5, COLOR_TRAITS);
      for (const [column, later] of Object.values(placed)) expect(later).toBe(column);
      // The tail pair really shares a column here, so this is the merged case.
      expect(placed.green[0]).toBe(placed.colorless[0]);
    });

    it("sends_later_type_sorted_picks_to_a_shared_column_with_a_planeswalker", () => {
      const kinds: DraftPoolGroupKind[] = [
        "creature", "instant", "sorcery", "enchantment", "artifact", "planeswalker", "land",
      ];
      const traits = Object.fromEntries(kinds.map((kind) => [kind, { type_line: kind }]));
      const placed = laterPicksJoinTheirGroupColumn("type", kinds, 6, traits);
      for (const [column, later] of Object.values(placed)) expect(later).toBe(column);
      expect(new Set(Object.values(placed).map(([column]) => column)).size).toBe(6);
    });

    // The board is sorted once and then edited, so the groups on screen drift
    // from the groups the sort laid out. A merged column must stay a destination.
    function sortedBoard(
      sort: "color" | "type",
      kinds: readonly DraftPoolGroupKind[],
      columnCount: number,
      traits: Record<string, Partial<DraftCardInstance>>,
      zone: "deck" | "sideboard",
    ) {
      const pool = kinds.map((kind) => ({ ...card(kind, kind), ...traits[kind] }));
      const poolGroups = groupsFor(sort, pool, kinds);
      const preferences: DraftBoardPreferences = { sort, columnCount, rows: "one", showHeaders: true };
      const state: DraftWorkspaceState = {
        ...createDraftWorkspaceState(),
        placements: Object.fromEntries(
          pool.map((entry) => [entry.instance_id, { zone, row: 0, column: 0, order: 0 }]),
        ),
      };
      return { pool, poolGroups, preferences, rebuilt: rebuildWorkspaceZone(state, zone, pool, poolGroups, preferences) };
    }

    function groupsFor(
      sort: "color" | "type",
      pool: readonly DraftCardInstance[],
      kinds: readonly DraftPoolGroupKind[],
    ): DraftPoolGroups {
      // One group per kind, in first-seen order, like the engine reports them.
      const byKind = new Map<DraftPoolGroupKind, DraftCardInstance[]>();
      pool.forEach((entry, index) => byKind.set(kinds[index], [...(byKind.get(kinds[index]) ?? []), entry]));
      const axis = [...byKind].map(([kind, entries]) => ({
        kind,
        total: entries.length,
        cards: entries.map((entry) => ({ card: entry, count: 1, instance_ids: [entry.instance_id] })),
      }));
      return sort === "color"
        ? { ...groups(), color_groups: axis }
        : { ...groups(), type_groups: axis };
    }

    const column = (placed: DraftWorkspaceState, id: string) => placed.placements[id].column;
    // The new card sits in the zone's first column, as reconcile defaults it.
    const withArrival = (
      state: DraftWorkspaceState, id: string, zone: "deck" | "sideboard",
    ): DraftWorkspaceState => ({
      ...state,
      placements: { ...state.placements, [id]: { zone, row: 0, column: 0, order: 0 } },
    });

    it("keeps_the_merged_column_a_destination_after_a_group_empties_out", () => {
      const { pool, poolGroups, preferences, rebuilt } = sortedBoard(
        "color", COLOR_KINDS, 6, COLOR_TRAITS, "sideboard",
      );
      // The only black card leaves for the deck: the zone now holds six groups.
      const moved: DraftWorkspaceState = {
        ...rebuilt,
        placements: { ...rebuilt.placements, black: { ...rebuilt.placements.black, zone: "deck" } },
      };
      for (const kind of ["multicolor", "colorless"]) {
        const later = { ...pool.find((entry) => entry.name === kind)!, instance_id: `later-${kind}` };
        expect(resolveWorkspacePickPlacement(
          later, "sideboard", pool, poolGroups, moved, preferences,
        ).column).toBe(column(rebuilt, kind));
      }
      expect(column(rebuilt, "multicolor")).toBe(5);
    });

    it("keeps_the_merged_column_a_destination_after_a_new_group_arrives", () => {
      const noMulticolor = COLOR_KINDS.filter((kind) => kind !== "multicolor");
      const sorted = sortedBoard("color", noMulticolor, 5, COLOR_TRAITS, "sideboard");
      // A multicolor card then joins the pool, so the engine now reports seven groups.
      const arrival = { ...card("arrival", "multicolor"), colors: ["W", "U"] };
      const pool = [...sorted.pool, arrival];
      const poolGroups = groupsFor("color", pool, [...noMulticolor, "multicolor"]);
      const board = withArrival(sorted.rebuilt, "arrival", "sideboard");
      const shared = column(sorted.rebuilt, "green");
      expect(column(sorted.rebuilt, "colorless")).toBe(shared);
      for (const kind of ["green", "colorless"]) {
        const later = { ...pool.find((entry) => entry.name === kind)!, instance_id: `later-${kind}` };
        expect(resolveWorkspacePickPlacement(
          later, "sideboard", pool, poolGroups, board, sorted.preferences,
        ).column).toBe(shared);
      }
    });

    it("keeps_a_type_sort_merged_column_a_destination_after_a_new_type_group_arrives", () => {
      const kinds: DraftPoolGroupKind[] = [
        "creature", "instant", "sorcery", "enchantment", "artifact", "planeswalker", "land",
      ];
      const traits = Object.fromEntries(kinds.map((kind) => [kind, { type_line: kind }]));
      const sorted = sortedBoard("type", kinds, 6, traits, "sideboard");
      const battle = { ...card("battle"), type_line: "Battle" };
      const pool = [...sorted.pool, battle];
      const poolGroups = groupsFor("type", pool, [...kinds, "other"]);
      const board = withArrival(sorted.rebuilt, "battle", "sideboard");
      for (const kind of ["planeswalker", "land"]) {
        const later = { ...pool.find((entry) => entry.name === kind)!, instance_id: `later-${kind}` };
        expect(resolveWorkspacePickPlacement(
          later, "sideboard", pool, poolGroups, board, sorted.preferences,
        ).column).toBe(column(sorted.rebuilt, kind));
      }
    });

    it("places_a_batch_of_arrivals_into_the_merged_column_after_a_group_empties_out", () => {
      const { pool, preferences, rebuilt } = sortedBoard(
        "color", COLOR_KINDS, 6, COLOR_TRAITS, "deck",
      );
      const arrivals = ["multicolor", "colorless"].map((kind) => (
        { ...pool.find((entry) => entry.name === kind)!, instance_id: `arrive-${kind}` }
      ));
      const grown = [...pool, ...arrivals];
      const grownGroups = groupsFor("color", grown, [
        ...COLOR_KINDS, "multicolor", "colorless",
      ]);
      expect(grownGroups.color_groups).toHaveLength(7);
      const state: DraftWorkspaceState = {
        ...rebuilt,
        placements: {
          ...rebuilt.placements,
          black: { ...rebuilt.placements.black, zone: "sideboard" },
          ...Object.fromEntries(arrivals.map((entry) => [
            entry.instance_id, { zone: "deck", row: 0, column: 0, order: 0 },
          ])),
        },
      };
      const placed = placeArrivingPoolCards(
        state, arrivals.map((entry) => entry.instance_id), grown, grownGroups, preferences,
      );
      expect(column(placed, "arrive-multicolor")).toBe(5);
      expect(column(placed, "arrive-colorless")).toBe(5);
    });

    // Boards built from named cards: [id, color group, zone].
    type Spec = readonly [string, DraftPoolGroupKind, "deck" | "sideboard"];
    function colorBoard(specs: readonly Spec[], columnCount: number) {
      const pool = specs.map(([id, kind]) => ({ ...card(id, id), colors: COLOR_TRAITS[kind].colors ?? [] }));
      const byKind = new Map<DraftPoolGroupKind, DraftCardInstance[]>();
      specs.forEach(([, kind], index) => byKind.set(kind, [...(byKind.get(kind) ?? []), pool[index]]));
      const poolGroups: DraftPoolGroups = {
        ...groups(),
        // The engine reports color groups in its fixed order, whatever the pool holds.
        color_groups: COLOR_KINDS.flatMap((kind) => (byKind.has(kind) ? [[kind, byKind.get(kind)!] as const] : [])).map(([kind, entries]) => ({
          kind,
          total: entries.length,
          cards: entries.map((entry) => ({ card: entry, count: 1, instance_ids: [entry.instance_id] })),
        })),
      };
      const preferences: DraftBoardPreferences = {
        sort: "color", columnCount, rows: "one", showHeaders: true,
      };
      let state: DraftWorkspaceState = {
        ...createDraftWorkspaceState(),
        placements: Object.fromEntries(specs.map(([id, , zone]) => [
          id, { zone, row: 0, column: 0, order: 0 },
        ])),
      };
      state = rebuildWorkspaceZone(state, "deck", pool, poolGroups, preferences);
      return { pool, poolGroups, preferences, state };
    }
    const pickFor = (
      board: ReturnType<typeof colorBoard>, state: DraftWorkspaceState, id: string, zone: "deck" | "sideboard",
    ) => resolveWorkspacePickPlacement(
      board.pool.find((entry) => entry.instance_id === id)!,
      zone, board.pool, board.poolGroups, state, board.preferences,
    ).column;
    const dragTo = (state: DraftWorkspaceState, id: string, destination: number): DraftWorkspaceState => ({
      ...state,
      placements: { ...state.placements, [id]: { ...state.placements[id], column: destination } },
    });
    const sendTo = (state: DraftWorkspaceState, id: string, zone: "deck" | "sideboard"): DraftWorkspaceState => ({
      ...state,
      placements: { ...state.placements, [id]: { ...state.placements[id], zone } },
    });

    it("prefers_the_column_holding_only_the_group_over_a_column_the_player_mixed", () => {
      const board = colorBoard([
        ["opt", "blue", "deck"], ["plains", "colorless", "deck"], ["island", "colorless", "deck"],
        ["forest", "colorless", "sideboard"], ["pacifism", "white", "sideboard"],
      ], 2);
      expect([column(board.state, "opt"), column(board.state, "island")]).toEqual([0, 1]);
      // Plains is dragged into Opt's column: blue and colorless now share column 0 by hand.
      const mixed = dragTo(board.state, "plains", 0);
      expect(pickFor(board, mixed, "forest", "deck")).toBe(1);
    });

    it("keeps_a_pure_group_column_ahead_of_a_hand_mixed_column_with_five_colors", () => {
      const board = colorBoard([
        ["pacifism", "white", "deck"], ["opt", "blue", "deck"], ["night", "black", "deck"],
        ["shock", "red", "deck"], ["elves", "green", "deck"], ["bears", "green", "deck"],
        ["satyr", "multicolor", "sideboard"], ["manalith", "colorless", "sideboard"],
      ], 5);
      // Grizzly Bears is dragged into the red column.
      const mixed = dragTo(board.state, "bears", 3);
      expect(pickFor(board, mixed, "elves", "deck")).toBe(4);
    });

    it("keeps_the_other_group_a_destination_after_one_of_a_merged_pair_leaves", () => {
      const specs: readonly Spec[] = [
        ["pacifism", "white", "deck"], ["opt", "blue", "deck"], ["night", "black", "deck"],
        ["shock", "red", "deck"], ["elves", "green", "deck"],
        ["satyr", "multicolor", "deck"], ["manalith", "colorless", "deck"],
      ];
      const board = colorBoard(specs, 6);
      expect(column(board.state, "satyr")).toBe(5);
      expect(column(board.state, "manalith")).toBe(5);
      // Manalith leaves: a later colorless card rejoins Satyr's column, and back.
      expect(pickFor(board, sendTo(board.state, "manalith", "sideboard"), "manalith", "deck")).toBe(5);
      expect(pickFor(board, sendTo(board.state, "satyr", "sideboard"), "satyr", "deck")).toBe(5);
    });

    it("folds_the_trailing_groups_first_as_columns_get_scarcer", () => {
      expect(sideboardColumns(5)).toEqual({
        white: 0, blue: 1, black: 2, red: 3, green: 3, multicolor: 4, colorless: 4,
      });
    });

    it("gives_each_group_a_column_when_there_is_room_and_spreads_when_far_short", () => {
      expect(sideboardColumns(7)).toEqual({
        white: 0, blue: 1, black: 2, red: 3, green: 4, multicolor: 5, colorless: 6,
      });
      // Two columns cannot be reached by pairing the tail, so the even spread stays.
      expect(sideboardColumns(2)).toEqual({
        white: 0, blue: 0, black: 0, red: 0, green: 1, multicolor: 1, colorless: 1,
      });
    });
  });
  // `draftStore.installWorkspace` and `multiplayerDraftStore.performPick` both
  // call `placeArrivingPoolCards` ahead of their own `applyDestination`. What
  // keeps a sideboard-bound pick out of the deck-sorted columns on those paths
  // is their own `ownPlacement` arm, not the deck-zone guard the second case
  // below pins: the card still carries reconcile's `"deck"` default when the
  // pass runs, so the zone test never fires for them. That guard is the
  // independent belt for a DIRECT caller that hands over an id already in the
  // sideboard, which is what the second case exercises.
  it("places_arriving_cards_into_the_columns_the_sort_means", () => {
    const pool = [{ ...card("cheap"), cmc: 1 }, { ...card("costly"), cmc: 5 }];
    const base = reconcileWorkspaceState(createDraftWorkspaceState(), pool);

    const placed = placeArrivingPoolCards(
      base, ["cheap", "costly"], pool, groups(), boardPreferences().deck,
    );

    // Six columns under a `cmc` sort, so `manaValueColumn` truncates and clamps
    // to 5 — a card reconcile had just defaulted to column 0.
    expect(base.placements.cheap.column).toBe(0);
    expect(placed.placements.cheap.column).toBe(1);
    expect(placed.placements.costly.column).toBe(5);
  });

  it("leaves_an_arriving_card_alone_when_its_placement_is_in_the_sideboard", () => {
    const pool = [{ ...card("costly"), cmc: 5 }];
    const sideboarded = { zone: "sideboard", row: 0, column: 0, order: 0 } as const;
    const base: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: { costly: sideboarded },
    };

    const placed = placeArrivingPoolCards(
      base, ["costly"], pool, groups(), boardPreferences().deck,
    );

    expect(placed.placements.costly).toEqual(sideboarded);
  });

  it("creates_no_placement_for_an_id_the_pool_does_not_hold", () => {
    // The id must already HOLD a deck placement, or this reaches the
    // `placement === undefined` guard instead and passes without ever
    // exercising the pool lookup it is named for. `reconcileWorkspaceState`
    // would strip a placement whose id is absent from the pool, so the base is
    // built directly.
    const pool = [card("present")];
    const ghost = { zone: "deck", row: 0, column: 0, order: 0 } as const;
    const base: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: { ghost },
    };

    const placed = placeArrivingPoolCards(
      base, ["ghost"], pool, groups(), boardPreferences().deck,
    );

    expect(placed.placements.ghost).toEqual(ghost);
  });

  it("reconciles_authoritative_instances_without_losing_manual_placement", () => {
    const manual = { zone: "sideboard", row: 1, column: 4, order: 7 } as const;
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        first: manual,
        removed: { zone: "deck", row: 0, column: 0, order: 40 },
      },
      virtualBasics: [{ instanceId: "virtual", name: "Island" }],
    };

    const reconciled = reconcileWorkspaceState(state, [card("second"), card("first")]);

    expect(reconciled.placements.first).toBe(manual);
    expect(reconciled.placements).not.toHaveProperty("removed");
    expect(reconciled.placements.second).toEqual({ zone: "deck", row: 0, column: 0, order: 0 });
    expect(reconciled.placements.virtual).toEqual({ zone: "deck", row: 0, column: 0, order: 1 });
    expect(reconciled.virtualBasics).toEqual([{ instanceId: "virtual", name: "Island" }]);
  });

  it("keeps distinct same-name identities through pool reordering", () => {
    const initial = reconcileWorkspaceState(createDraftWorkspaceState(), [card("one"), card("two")]);
    const moved = updateWorkspacePlacement(
      initial,
      [card("one"), card("two")],
      "two",
      { zone: "sideboard", row: 0, column: 0, order: 0 },
    );

    const reconciled = reconcileWorkspaceState(moved, [card("two"), card("one")]);
    expect(reconciled).toBe(moved);
    expect(reconciled.placements.one.zone).toBe("deck");
    expect(reconciled.placements.two.zone).toBe("sideboard");
  });

  it("uses only Deck row-zero column-zero orders for missing instances", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: {
        deck: { zone: "deck", row: 0, column: 0, order: 6 },
        otherRow: { zone: "deck", row: 1, column: 0, order: 99 },
        sideboard: { zone: "sideboard", row: 0, column: 0, order: 200 },
      },
    };

    const reconciled = reconcileWorkspaceState(state, [
      card("deck"), card("otherRow"), card("sideboard"), card("missing"),
    ]);
    expect(reconciled.placements.missing.order).toBe(7);
  });

  it("drops virtual identities that collide with the authoritative pool", () => {
    const state: DraftWorkspaceState = {
      ...createDraftWorkspaceState(),
      placements: { collision: { zone: "sideboard", row: 0, column: 0, order: 0 } },
      virtualBasics: [
        { instanceId: "collision", name: "Island" },
        { instanceId: "virtual", name: "Plains" },
        { instanceId: "virtual", name: "Duplicate" },
      ],
    };

    const reconciled = reconcileWorkspaceState(state, [card("collision")]);
    expect(reconciled.virtualBasics).toEqual([{ instanceId: "virtual", name: "Plains" }]);
    expect(reconciled.placements.collision.zone).toBe("sideboard");
    expect(reconciled.placements.virtual.zone).toBe("deck");
  });

  it("rejects unknown identities and invalid coordinates", () => {
    const state = reconcileWorkspaceState(createDraftWorkspaceState(), [card("known")]);
    const placement = { zone: "sideboard", row: 0, column: 0, order: 0 } as const;

    expect(updateWorkspacePlacement(state, [card("known")], "unknown", placement)).toBe(state);
    expect(updateWorkspacePlacement(
      state,
      [card("known")],
      "known",
      { ...placement, row: -1 },
    )).toBe(state);
  });

  it("appends_a_reconciled_pick_to_the_exact_destination_without_touching_unrelated_stacks", () => {
    const pool = [card("source"), card("source-sibling"), card("first"), card("second"), card("unrelated")];
    const state = reconcileWorkspaceState({
      ...createDraftWorkspaceState(),
      placements: {
        source: { zone: "sideboard", column: 2, row: 1, order: 4 },
        "source-sibling": { zone: "sideboard", column: 2, row: 1, order: 9 },
        first: { zone: "deck", column: 4, row: 0, order: 3 },
        second: { zone: "deck", column: 4, row: 0, order: 8 },
        unrelated: { zone: "deck", column: 1, row: 0, order: 17 },
      },
    }, pool);

    const moved = appendWorkspaceInstanceToResolvedDestination(state, pool, "source", {
      zone: "deck", column: 4, row: 0,
    });

    expect(moved.placements["source-sibling"]).toEqual({ zone: "sideboard", column: 2, row: 1, order: 0 });
    expect(moved.placements.first).toEqual({ zone: "deck", column: 4, row: 0, order: 0 });
    expect(moved.placements.second).toEqual({ zone: "deck", column: 4, row: 0, order: 1 });
    expect(moved.placements.source).toEqual({ zone: "deck", column: 4, row: 0, order: 2 });
    expect(moved.placements.unrelated).toBe(state.placements.unrelated);
  });

  it("keeps_anchored_keyboard_reorder_and_rejects_invalid_anchors", () => {
    const pool = [card("source"), card("first"), card("second")];
    const state = reconcileWorkspaceState({
      ...createDraftWorkspaceState(),
      placements: {
        source: { zone: "deck", column: 3, row: 0, order: 1 },
        first: { zone: "deck", column: 3, row: 0, order: 0 },
        second: { zone: "deck", column: 3, row: 0, order: 2 },
      },
    }, pool);

    const moved = moveWorkspaceInstance(state, pool, groups(), boardPreferences(), "source", {
      zone: "deck", column: 3, row: 0, beforeInstanceId: "first",
    });
    expect(moved.placements.source.order).toBe(0);
    expect(moved.placements.first.order).toBe(1);
    expect(moved.placements.second.order).toBe(2);

    expect(moveWorkspaceInstance(state, pool, groups(), boardPreferences(), "source", {
      zone: "deck", column: 3, row: 0, beforeInstanceId: "missing",
    })).toBe(state);
    expect(moveWorkspaceInstance(state, pool, groups(), boardPreferences(), "source", {
      zone: "deck", column: 3, row: 0, beforeInstanceId: "source",
    })).toBe(state);
  });
});
