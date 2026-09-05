import type {
  DraftCardInstance,
  DraftPoolGroup,
  DraftPoolGroupKind,
  DraftPoolGroups,
  DraftWorkspaceCapabilities,
} from "../../../adapter/draft-adapter";
import type { SourcePrinting } from "../../../hooks/useCardImage";
import type { CardHoverInfo } from "../../card/CardPreview";
import { POOL_GROUP_LABEL_KEYS } from "../poolGroupLabels";
import { removeVirtualBasic } from "./workspaceProjection";
import {
  DRAFT_WORKSPACE_SCHEMA_VERSION,
  type DraftCardPlacement,
  type DraftWorkspaceState,
  type DraftZone,
} from "./types";
import {
  DRAFT_WORKSPACE_COLUMN_MAX,
  DRAFT_WORKSPACE_COLUMN_MIN,
  type DraftBoardPreferences,
  type DraftBoardSort,
} from "./workspacePreferences";

const DEFAULT_PLACEMENT = {
  zone: "deck",
  row: 0,
  column: 0,
} as const;

const UUID_ATTEMPTS = 4;

export function makeInteractiveVirtualBasicInstanceId(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
): string {
  const usedIds = new Set([
    ...pool.map((card) => card.instance_id),
    ...state.virtualBasics.map((basic) => basic.instanceId),
    ...Object.keys(state.placements),
  ]);
  for (let attempt = 0; attempt < UUID_ATTEMPTS; attempt += 1) {
    const candidate = `workspace-basic:interactive:${crypto.randomUUID()}`;
    if (!usedIds.has(candidate)) return candidate;
  }
  for (let suffix = 0; suffix <= usedIds.size; suffix += 1) {
    const candidate = `workspace-basic:interactive:fallback:${suffix}`;
    if (!usedIds.has(candidate)) return candidate;
  }
  throw new Error("Unable to allocate an interactive virtual basic identity");
}

function isValidPlacement(placement: DraftCardPlacement): boolean {
  return [placement.row, placement.column, placement.order]
    .every((value) => Number.isInteger(value) && value >= 0);
}

function nextDefaultOrder(placements: Readonly<Record<string, DraftCardPlacement>>): number {
  let maximum = -1;
  for (const placement of Object.values(placements)) {
    if (
      placement.zone === "deck"
      && placement.row === 0
      && placement.column === 0
    ) {
      maximum = Math.max(maximum, placement.order);
    }
  }
  return maximum + 1;
}

function placementsEqual(
  left: Readonly<Record<string, DraftCardPlacement>>,
  right: Readonly<Record<string, DraftCardPlacement>>,
): boolean {
  const leftEntries = Object.entries(left);
  const rightEntries = Object.entries(right);
  return leftEntries.length === rightEntries.length
    && leftEntries.every(([instanceId, placement]) => right[instanceId] === placement);
}

export function createDraftWorkspaceState(): DraftWorkspaceState {
  return {
    schemaVersion: DRAFT_WORKSPACE_SCHEMA_VERSION,
    placements: {},
    virtualBasics: [],
  };
}

export function reconcileWorkspaceState(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
): DraftWorkspaceState {
  const poolInstanceIds = new Set(pool.map((card) => card.instance_id));
  const virtualInstanceIds = new Set<string>();
  const virtualBasics = state.virtualBasics.filter((basic) => {
    if (poolInstanceIds.has(basic.instanceId) || virtualInstanceIds.has(basic.instanceId)) {
      return false;
    }
    virtualInstanceIds.add(basic.instanceId);
    return true;
  });

  const placements: Record<string, DraftCardPlacement> = {};
  for (const [instanceId, placement] of Object.entries(state.placements)) {
    if (poolInstanceIds.has(instanceId) || virtualInstanceIds.has(instanceId)) {
      placements[instanceId] = placement;
    }
  }

  let order = nextDefaultOrder(placements);
  const addDefaultPlacement = (instanceId: string) => {
    if (placements[instanceId] !== undefined) return;
    placements[instanceId] = { ...DEFAULT_PLACEMENT, order };
    order += 1;
  };

  for (const card of pool) addDefaultPlacement(card.instance_id);
  for (const basic of virtualBasics) addDefaultPlacement(basic.instanceId);

  if (
    virtualBasics.length === state.virtualBasics.length
    && virtualBasics.every((basic, index) => basic === state.virtualBasics[index])
    && placementsEqual(state.placements, placements)
  ) {
    return state;
  }

  return { ...state, placements, virtualBasics };
}

export function normalizeWorkspaceForBoardGeometry(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
  poolGroups: DraftPoolGroups,
  preferences: Readonly<Record<DraftZone, DraftBoardPreferences>>,
): DraftWorkspaceState {
  const reconciled = reconcileWorkspaceState(state, pool);
  const ranks = fallbackRanks(reconciled, pool);
  const stacks = new Map<string, string[]>();
  const destinations = new Map<string, { zone: DraftZone; column: number; row: number }>();

  for (const [instanceId, placement] of Object.entries(reconciled.placements)) {
    const effective = clampedPreferences(
      preferences[placement.zone],
      poolGroups.workspace_capabilities,
    );
    const column = Number.isInteger(placement.column) && placement.column >= 0
      ? Math.min(placement.column, effective.columnCount - 1)
      : 0;
    const row = effective.rows === "one"
      ? 0
      : Number.isInteger(placement.row) && placement.row >= 0 && placement.row < 2
        ? placement.row
        : resolvedRow(instanceId, effective, poolGroups);
    const stackKey = `${placement.zone}:${column}:${row}`;
    destinations.set(instanceId, { zone: placement.zone, column, row });
    stacks.set(stackKey, [...(stacks.get(stackKey) ?? []), instanceId]);
  }

  const placements = { ...reconciled.placements };
  for (const instanceIds of stacks.values()) {
    instanceIds.sort((leftId, rightId) => {
      const leftOrder = reconciled.placements[leftId].order;
      const rightOrder = reconciled.placements[rightId].order;
      const validLeftOrder = Number.isInteger(leftOrder) && leftOrder >= 0;
      const validRightOrder = Number.isInteger(rightOrder) && rightOrder >= 0;
      if (validLeftOrder !== validRightOrder) return validLeftOrder ? -1 : 1;
      return (validLeftOrder ? leftOrder - rightOrder : 0)
        || compareInstanceIds(leftId, rightId, ranks);
    });
    instanceIds.forEach((instanceId, order) => {
      const destination = destinations.get(instanceId)!;
      const current = reconciled.placements[instanceId];
      if (
        current.zone !== destination.zone
        || current.column !== destination.column
        || current.row !== destination.row
        || current.order !== order
      ) {
        placements[instanceId] = { ...destination, order };
      }
    });
  }

  return placementsEqual(reconciled.placements, placements)
    ? reconciled
    : { ...reconciled, placements };
}

export function updateWorkspacePlacement(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
  instanceId: string,
  placement: DraftCardPlacement,
): DraftWorkspaceState {
  const isPoolInstance = pool.some((card) => card.instance_id === instanceId);
  const isVirtualInstance = state.virtualBasics.some((basic) => basic.instanceId === instanceId);
  if ((!isPoolInstance && !isVirtualInstance) || !isValidPlacement(placement)) return state;

  const current = state.placements[instanceId];
  if (
    current?.zone === placement.zone
    && current.row === placement.row
    && current.column === placement.column
    && current.order === placement.order
  ) {
    return state;
  }

  return {
    ...state,
    placements: { ...state.placements, [instanceId]: placement },
  };
}

export type WorkspaceLayoutChangeReason = "sort" | "rows" | "column-count" | "headers";

export interface WorkspaceMoveTarget {
  zone: DraftZone;
  column: number;
  row?: number;
  beforeInstanceId: string | null;
}

export interface ResolvedWorkspaceDestination {
  zone: DraftZone;
  column: number;
  row: number;
}

export interface WorkspaceDropState {
  zoneActive: boolean;
  column: number | null;
  row: number | null;
}

export type WorkspaceHeaderPresentation =
  | { kind: "mana-symbol"; shard: "W" | "U" | "B" | "R" | "G" | "C" }
  | {
      kind: "mana-font";
      iconClass:
        | "ms-multicolor ms-duo ms-duo-color ms-grad"
        | "ms-creature"
        | "ms-instant"
        | "ms-sorcery"
        | "ms-enchantment"
        | "ms-artifact"
        | "ms-planeswalker"
        | "ms-land";
      fallbackText: string;
    }
  | { kind: "numeric-badge"; text: string }
  | { kind: "text-only" };

export type WorkspaceHeaderDescriptor =
  | {
      kind: "engine-group";
      groupKind: DraftPoolGroupKind;
      labelKey: string;
      presentation: WorkspaceHeaderPresentation;
    }
  | { kind: "added-basics"; labelKey: "workspace.headers.addedBasics" }
  | { kind: "unclassified"; labelKey: "workspace.headers.unclassified" }
  | {
      kind: "mana-value-column";
      manaValue: number;
      labelKey: "manaCurve.bucketLabel";
      presentation: WorkspaceHeaderPresentation;
    }
  | {
      kind: "empty-ordinal";
      labelKey: "workspace.headers.emptyOrdinal";
      ordinal: number;
    };

export interface WorkspaceHeaderModel {
  key: string;
  descriptors: readonly WorkspaceHeaderDescriptor[];
  count: number;
}

export interface WorkspaceCardImageModel {
  cardName: string;
  sourcePrinting: SourcePrinting | undefined;
  alt: string;
  draggable: false;
}

export interface WorkspaceCardEntryModel {
  key: string;
  instanceId: string;
  name: string;
  sourcePrinting: SourcePrinting | undefined;
  image: WorkspaceCardImageModel;
  preview: CardHoverInfo;
  isVirtualBasic: boolean;
  placement: DraftCardPlacement;
  order: number;
}

export interface WorkspaceDropPresentation {
  state: "idle" | "active";
  active: boolean;
  descriptionKey: string | null;
}

export interface WorkspaceBoardRowModel {
  key: string;
  row: number;
  count: number;
  cards: readonly WorkspaceCardEntryModel[];
  drop: WorkspaceDropPresentation;
}

export interface WorkspaceBoardColumnModel {
  key: string;
  column: number;
  count: number;
  header: WorkspaceHeaderModel;
  rows: readonly WorkspaceBoardRowModel[];
  drop: WorkspaceDropPresentation;
}

export interface WorkspaceBoardModel {
  key: string;
  zone: DraftZone;
  requestedSort: DraftBoardSort;
  effectiveSort: DraftBoardSort;
  columnCount: number;
  rowCount: 1 | 2;
  count: number;
  showHeaders: boolean;
  columns: readonly WorkspaceBoardColumnModel[];
  drop: WorkspaceDropPresentation;
}

const HEADER_PRESENTATIONS: Record<DraftPoolGroupKind, WorkspaceHeaderPresentation> = {
  white: { kind: "mana-symbol", shard: "W" },
  blue: { kind: "mana-symbol", shard: "U" },
  black: { kind: "mana-symbol", shard: "B" },
  red: { kind: "mana-symbol", shard: "R" },
  green: { kind: "mana-symbol", shard: "G" },
  colorless: { kind: "mana-symbol", shard: "C" },
  multicolor: {
    kind: "mana-font",
    iconClass: "ms-multicolor ms-duo ms-duo-color ms-grad",
    fallbackText: "M",
  },
  creature: { kind: "mana-font", iconClass: "ms-creature", fallbackText: "C" },
  instant: { kind: "mana-font", iconClass: "ms-instant", fallbackText: "I" },
  sorcery: { kind: "mana-font", iconClass: "ms-sorcery", fallbackText: "S" },
  enchantment: { kind: "mana-font", iconClass: "ms-enchantment", fallbackText: "E" },
  artifact: { kind: "mana-font", iconClass: "ms-artifact", fallbackText: "A" },
  planeswalker: { kind: "mana-font", iconClass: "ms-planeswalker", fallbackText: "P" },
  land: { kind: "mana-font", iconClass: "ms-land", fallbackText: "L" },
  other: { kind: "text-only" },
  mythic: { kind: "text-only" },
  rare: { kind: "text-only" },
  uncommon: { kind: "text-only" },
  common: { kind: "text-only" },
  rarity_other: { kind: "text-only" },
  mana_value0: { kind: "numeric-badge", text: "0" },
  mana_value1: { kind: "numeric-badge", text: "1" },
  mana_value2: { kind: "numeric-badge", text: "2" },
  mana_value3: { kind: "numeric-badge", text: "3" },
  mana_value4: { kind: "numeric-badge", text: "4" },
  mana_value5: { kind: "numeric-badge", text: "5" },
  mana_value6_plus: { kind: "numeric-badge", text: "6+" },
};

const IDLE_DROP: WorkspaceDropPresentation = {
  state: "idle",
  active: false,
  descriptionKey: null,
};

export function resolveAvailableBoardSort(
  requested: DraftBoardSort,
  capabilities: DraftWorkspaceCapabilities,
): DraftBoardSort {
  if (requested === "rarity" && capabilities.rarity_group_order === null) return "cmc";
  return requested;
}

function axisGroups(sort: DraftBoardSort, groups: DraftPoolGroups): readonly DraftPoolGroup[] {
  switch (sort) {
    case "cmc": return groups.cmc_groups;
    case "color": return groups.color_groups;
    case "rarity": return groups.rarity_groups;
    case "type": return groups.type_groups;
  }
}

const SORT_GROUP_KINDS: Readonly<Record<DraftBoardSort, ReadonlySet<DraftPoolGroupKind>>> = {
  cmc: new Set(["mana_value0", "mana_value1", "mana_value2", "mana_value3", "mana_value4", "mana_value5", "mana_value6_plus"]),
  color: new Set(["white", "blue", "black", "red", "green", "multicolor", "colorless"]),
  rarity: new Set(["mythic", "rare", "uncommon", "common", "rarity_other"]),
  type: new Set(["creature", "instant", "sorcery", "enchantment", "artifact", "planeswalker", "land", "other"]),
};

function cardSortGroup(card: DraftCardInstance, sort: DraftBoardSort): DraftPoolGroupKind {
  switch (sort) {
    case "cmc":
      return card.cmc >= 0 && card.cmc <= 5
        ? `mana_value${card.cmc}` as DraftPoolGroupKind
        : "mana_value6_plus";
    case "color":
      if (card.colors.length === 0) return "colorless";
      if (card.colors.length > 1) return "multicolor";
      return ({ W: "white", U: "blue", B: "black", R: "red", G: "green" } as const)[card.colors[0]]
        ?? "colorless";
    case "rarity": {
      const rarity = card.rarity.toLowerCase();
      return rarity === "mythic" || rarity === "rare" || rarity === "uncommon" || rarity === "common"
        ? rarity
        : "rarity_other";
    }
    case "type": {
      const typeLine = card.type_line.toLowerCase();
      return (["creature", "instant", "sorcery", "enchantment", "artifact", "planeswalker", "land"] as const)
        .find((kind) => typeLine.includes(kind)) ?? "other";
    }
  }
}

function manaValueColumn(cmc: number, columnCount: number): number {
  if (!Number.isFinite(cmc)) return columnCount - 1;
  return Math.min(columnCount - 1, Math.max(0, Math.trunc(cmc)));
}

const COLUMN_COLOR_ORDER = [
  "white",
  "blue",
  "black",
  "red",
  "green",
  "colorless",
  "multicolor",
] as const satisfies readonly DraftPoolGroupKind[];

function columnSetColor(column: number): DraftPoolGroupKind {
  return COLUMN_COLOR_ORDER[column] ?? "colorless";
}

function resolvedRow(
  instanceId: string,
  preferences: DraftBoardPreferences,
  groups: DraftPoolGroups,
): number {
  if (preferences.rows === "one") return 0;
  if (groups.workspace_row_classification.creature_instance_ids.includes(instanceId)) return 0;
  return 1;
}

function fallbackRanks(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
): ReadonlyMap<string, number> {
  const ranks = new Map<string, number>();
  pool.forEach((card, index) => ranks.set(card.instance_id, index));
  state.virtualBasics.forEach((basic, index) => ranks.set(basic.instanceId, pool.length + index));
  return ranks;
}

function compareInstanceIds(
  left: string,
  right: string,
  ranks: ReadonlyMap<string, number>,
): number {
  const residualBase = ranks.size;
  const rankDifference = (ranks.get(left) ?? residualBase) - (ranks.get(right) ?? residualBase);
  return rankDifference || left.localeCompare(right);
}

function clampedPreferences(
  preferences: DraftBoardPreferences,
  capabilities: DraftWorkspaceCapabilities,
): DraftBoardPreferences {
  return {
    ...preferences,
    sort: resolveAvailableBoardSort(preferences.sort, capabilities),
    columnCount: Math.min(
      DRAFT_WORKSPACE_COLUMN_MAX,
      Math.max(DRAFT_WORKSPACE_COLUMN_MIN, preferences.columnCount),
    ),
  };
}

interface AllocatableGroup {
  group: DraftPoolGroup;
  bundles: string[][];
  columns: number[];
}

function allocateGroups(groups: AllocatableGroup[], columnCount: number): void {
  if (groups.length > columnCount) {
    groups.forEach((group, index) => {
      group.columns = [Math.floor(index * columnCount / groups.length)];
    });
    return;
  }
  if (groups.length === 0) return;
  groups.forEach((group, index) => {
    group.columns = [index];
  });
}

export function rebuildWorkspaceZone(
  state: DraftWorkspaceState,
  zone: DraftZone,
  pool: readonly DraftCardInstance[],
  poolGroups: DraftPoolGroups,
  preferences: DraftBoardPreferences,
): DraftWorkspaceState {
  const effective = clampedPreferences(preferences, poolGroups.workspace_capabilities);
  const poolIds = new Set(pool.map((card) => card.instance_id));
  const poolById = new Map(pool.map((card) => [card.instance_id, card]));
  const affectedIds = new Set(
    Object.entries(state.placements)
      .filter(([, placement]) => placement.zone === zone)
      .map(([instanceId]) => instanceId),
  );
  const ranks = fallbackRanks(state, pool);
  const nextPlacements = { ...state.placements };
  const rowCount = effective.rows === "two" ? 2 : 1;
  const rowFor = (instanceId: string) => rowCount === 1
    ? 0
    : Math.min(1, Math.max(0, state.placements[instanceId]?.row ?? 0));
  const sortColumns = new Map<DraftPoolGroupKind, number[]>();
  if (effective.sort !== "cmc") {
    const boardGroups = axisGroups(effective.sort, poolGroups).flatMap((group) => {
      const bundles = group.cards.flatMap((entry) => {
        const ids = entry.instance_ids.filter((instanceId) => (
          poolIds.has(instanceId) && affectedIds.has(instanceId)
        ));
        return ids.length === 0 ? [] : [ids];
      });
      return bundles.length === 0 ? [] : [{ group, bundles, columns: [] }];
    });
    allocateGroups(boardGroups, effective.columnCount);
    boardGroups.forEach(({ group, columns }) => sortColumns.set(group.kind, columns));
  }

  for (let row = 0; row < rowCount; row += 1) {
    const rowIds = new Set([...affectedIds].filter((instanceId) => rowFor(instanceId) === row));
    if (effective.sort === "cmc") {
      const columnIds = Array.from({ length: effective.columnCount }, () => [] as string[]);
      [...rowIds]
        .sort((left, right) => compareInstanceIds(left, right, ranks))
        .forEach((instanceId) => {
          const card = poolById.get(instanceId);
          const column = card === undefined
            ? effective.columnCount - 1
            : manaValueColumn(card.cmc, effective.columnCount);
          columnIds[column].push(instanceId);
        });
      columnIds.forEach((instanceIds, column) => {
        instanceIds.forEach((instanceId, order) => {
          nextPlacements[instanceId] = { zone, row, column, order };
        });
      });
      continue;
    }
    const assignedIds = new Set<string>();
    const allocatable: AllocatableGroup[] = axisGroups(effective.sort, poolGroups).flatMap((group) => {
      const bundles = group.cards.flatMap((entry) => {
        const ids = entry.instance_ids.filter((instanceId) => (
          poolIds.has(instanceId) && rowIds.has(instanceId)
        ));
        ids.forEach((instanceId) => assignedIds.add(instanceId));
        return ids.length === 0 ? [] : [ids];
      });
      return bundles.length === 0 ? [] : [{ group, bundles, columns: [] }];
    });
    allocatable.forEach(({ group, columns }) => {
      columns.push(...(sortColumns.get(group.kind) ?? []));
    });

    const columnIds = Array.from({ length: effective.columnCount }, () => [] as string[]);
    for (const { bundles, columns } of allocatable) {
      const quotient = Math.floor(bundles.length / columns.length);
      const remainder = bundles.length % columns.length;
      columns.forEach((column, localColumn) => {
        const count = quotient + (localColumn < remainder ? 1 : 0);
        const offset = localColumn * quotient + Math.min(localColumn, remainder);
        for (const bundle of bundles.slice(offset, offset + count)) columnIds[column].push(...bundle);
      });
    }

    const missingAdapterIds = [...rowIds]
      .filter((instanceId) => poolIds.has(instanceId) && !assignedIds.has(instanceId))
      .sort((left, right) => compareInstanceIds(left, right, ranks));
    const virtualIds = state.virtualBasics
      .map((basic) => basic.instanceId)
      .filter((instanceId) => rowIds.has(instanceId))
      .sort((left, right) => compareInstanceIds(left, right, ranks));
    columnIds[effective.columnCount - 1].push(...missingAdapterIds, ...virtualIds);

    columnIds.forEach((instanceIds, column) => {
      instanceIds.forEach((instanceId, order) => {
        nextPlacements[instanceId] = { zone, row, column, order };
      });
    });
  }
  return { ...state, placements: nextPlacements };
}

export function rebuildWorkspaceZoneRows(
  state: DraftWorkspaceState,
  zone: DraftZone,
  poolGroups: DraftPoolGroups,
  preferences: DraftBoardPreferences,
): DraftWorkspaceState {
  const entries = Object.entries(state.placements)
    .filter(([, placement]) => placement.zone === zone)
    .sort(([leftId, left], [rightId, right]) => (
      left.column - right.column
      || left.row - right.row
      || left.order - right.order
      || leftId.localeCompare(rightId)
    ));
  const placements = { ...state.placements };
  const nextOrders = new Map<string, number>();
  let changed = false;

  for (const [instanceId, placement] of entries) {
    const row = resolvedRow(instanceId, preferences, poolGroups);
    const stackKey = `${placement.column}:${row}`;
    const order = nextOrders.get(stackKey) ?? 0;
    nextOrders.set(stackKey, order + 1);
    if (placement.row === row && placement.order === order) continue;
    placements[instanceId] = { ...placement, row, order };
    changed = true;
  }

  return changed ? { ...state, placements } : state;
}

function sortedStackIds(
  state: DraftWorkspaceState,
  zone: DraftZone,
  column: number,
  row: number,
  ranks: ReadonlyMap<string, number>,
  liveIds: ReadonlySet<string>,
): string[] {
  return Object.entries(state.placements)
    .filter(([instanceId, placement]) => (
      liveIds.has(instanceId)
      && placement.zone === zone
      && placement.column === column
      && placement.row === row
    ))
    .sort(([leftId, left], [rightId, right]) => (
      left.order - right.order || compareInstanceIds(leftId, rightId, ranks)
    ))
    .map(([instanceId]) => instanceId);
}

function writeNormalizedStack(
  placements: Record<string, DraftCardPlacement>,
  instanceIds: readonly string[],
  zone: DraftZone,
  column: number,
  row: number,
): void {
  instanceIds.forEach((instanceId, order) => {
    const current = placements[instanceId];
    if (
      current.zone !== zone
      || current.column !== column
      || current.row !== row
      || current.order !== order
    ) {
      placements[instanceId] = { zone, column, row, order };
    }
  });
}

/**
 * Moves one known workspace identity into an already-resolved board stack and
 * normalizes the affected source and destination stacks. Board geometry and
 * sort selection are deliberately outside this primitive: callers that have
 * already resolved a destination must retain that exact target.
 */
function orderWorkspaceInstanceAtResolvedDestination(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
  instanceId: string,
  target: ResolvedWorkspaceDestination & Pick<WorkspaceMoveTarget, "beforeInstanceId">,
): DraftWorkspaceState {
  if (target.zone !== "deck" && target.zone !== "sideboard") return state;
  if (
    !Number.isInteger(target.column)
    || target.column < 0
    || !Number.isInteger(target.row)
    || target.row < 0
  ) {
    return state;
  }

  const isKnown = pool.some((card) => card.instance_id === instanceId)
    || state.virtualBasics.some((basic) => basic.instanceId === instanceId);
  const source = state.placements[instanceId];
  if (!isKnown || source === undefined) return state;

  const ranks = fallbackRanks(state, pool);
  const liveIds = new Set([
    ...pool.map((card) => card.instance_id),
    ...state.virtualBasics.map((basic) => basic.instanceId),
  ]);
  const destinationStack = sortedStackIds(
    state,
    target.zone,
    target.column,
    target.row,
    ranks,
    liveIds,
  );
  if (
    target.beforeInstanceId !== null
    && (
      target.beforeInstanceId === instanceId
      || !destinationStack.includes(target.beforeInstanceId)
    )
  ) {
    return state;
  }

  const sameStack = source.zone === target.zone
    && source.column === target.column
    && source.row === target.row;
  const sourceStack = sameStack
    ? destinationStack
    : sortedStackIds(state, source.zone, source.column, source.row, ranks, liveIds);
  const nextSourceStack = sourceStack.filter((id) => id !== instanceId);
  const nextDestinationStack = (sameStack ? nextSourceStack : destinationStack)
    .filter((id) => id !== instanceId);
  const insertionIndex = target.beforeInstanceId === null
    ? nextDestinationStack.length
    : nextDestinationStack.indexOf(target.beforeInstanceId);
  if (insertionIndex < 0) return state;
  nextDestinationStack.splice(insertionIndex, 0, instanceId);

  const placements = { ...state.placements };
  if (!sameStack) {
    writeNormalizedStack(
      placements,
      nextSourceStack,
      source.zone,
      source.column,
      source.row,
    );
  }
  writeNormalizedStack(
    placements,
    nextDestinationStack,
    target.zone,
    target.column,
    target.row,
  );
  return { ...state, placements };
}

/**
 * Appends a known, reconciled workspace identity to the end of an exact
 * destination stack. This does not recalculate the destination or its sort.
 */
export function appendWorkspaceInstanceToResolvedDestination(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
  instanceId: string,
  destination: ResolvedWorkspaceDestination,
): DraftWorkspaceState {
  return orderWorkspaceInstanceAtResolvedDestination(state, pool, instanceId, {
    ...destination,
    beforeInstanceId: null,
  });
}

export function moveWorkspaceInstance(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
  poolGroups: DraftPoolGroups,
  preferences: Readonly<Record<DraftZone, DraftBoardPreferences>>,
  instanceId: string,
  target: WorkspaceMoveTarget,
): DraftWorkspaceState {
  if (target.zone !== "deck" && target.zone !== "sideboard") return state;
  const isKnown = pool.some((card) => card.instance_id === instanceId)
    || state.virtualBasics.some((basic) => basic.instanceId === instanceId);
  if (!isKnown || state.placements[instanceId] === undefined) return state;

  const destinationPreferences = clampedPreferences(
    preferences[target.zone],
    poolGroups.workspace_capabilities,
  );
  if (
    !Number.isInteger(target.column)
    || target.column < 0
    || target.column >= destinationPreferences.columnCount
  ) {
    return state;
  }

  const destinationRow = target.row ?? resolvedRow(instanceId, destinationPreferences, poolGroups);
  const destinationRowCount = destinationPreferences.rows === "two" ? 2 : 1;
  if (
    !Number.isInteger(destinationRow)
    || destinationRow < 0
    || destinationRow >= destinationRowCount
  ) {
    return state;
  }
  return orderWorkspaceInstanceAtResolvedDestination(state, pool, instanceId, {
    zone: target.zone,
    column: target.column,
    row: destinationRow,
    beforeInstanceId: target.beforeInstanceId,
  });
}

export function activateWorkspaceInstance(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
  poolGroups: DraftPoolGroups,
  preferences: Readonly<Record<DraftZone, DraftBoardPreferences>>,
  instanceId: string,
): DraftWorkspaceState {
  if (state.virtualBasics.some((basic) => basic.instanceId === instanceId)) {
    return removeVirtualBasic(state, instanceId);
  }

  const source = state.placements[instanceId];
  if (source === undefined || !pool.some((card) => card.instance_id === instanceId)) return state;
  const targetZone: DraftZone = source.zone === "deck" ? "sideboard" : "deck";
  const targetPreferences = clampedPreferences(
    preferences[targetZone],
    poolGroups.workspace_capabilities,
  );
  return moveWorkspaceInstance(state, pool, poolGroups, preferences, instanceId, {
    zone: targetZone,
    column: Math.min(source.column, targetPreferences.columnCount - 1),
    beforeInstanceId: null,
  });
}

function dropPresentation(active: boolean, descriptionKey: string): WorkspaceDropPresentation {
  return active ? { state: "active", active: true, descriptionKey } : IDLE_DROP;
}

export function buildCardPoolBoardModel(
  zone: DraftZone,
  pool: readonly DraftCardInstance[],
  poolGroups: DraftPoolGroups,
  workspace: DraftWorkspaceState,
  preferences: DraftBoardPreferences,
  dropState: WorkspaceDropState = { zoneActive: false, column: null, row: null },
): WorkspaceBoardModel {
  const effective = clampedPreferences(preferences, poolGroups.workspace_capabilities);
  const rowCount = effective.rows === "two" ? 2 : 1;
  const poolById = new Map(pool.map((card) => [card.instance_id, card]));
  const basicsById = new Map(workspace.virtualBasics.map((basic) => [basic.instanceId, basic]));
  const ranks = fallbackRanks(workspace, pool);
  const activeGroups = axisGroups(effective.sort, poolGroups);
  const memberships = new Map<string, DraftPoolGroupKind[]>();
  for (const group of activeGroups) {
    for (const entry of group.cards) {
      for (const instanceId of entry.instance_ids) {
        const kinds = memberships.get(instanceId) ?? [];
        if (!kinds.includes(group.kind)) memberships.set(instanceId, [...kinds, group.kind]);
      }
    }
  }
  const cmcMemberships = new Map<string, DraftPoolGroupKind[]>();
  for (const group of poolGroups.cmc_groups) {
    for (const entry of group.cards) {
      for (const instanceId of entry.instance_ids) {
        const kinds = cmcMemberships.get(instanceId) ?? [];
        if (!kinds.includes(group.kind)) cmcMemberships.set(instanceId, [...kinds, group.kind]);
      }
    }
  }

  const columns: WorkspaceBoardColumnModel[] = [];
  for (let column = 0; column < effective.columnCount; column += 1) {
    const rows: WorkspaceBoardRowModel[] = [];
    for (let row = 0; row < rowCount; row += 1) {
      const instanceIds = Object.entries(workspace.placements)
        .filter(([, placement]) => (
          placement.zone === zone && placement.column === column && placement.row === row
        ))
        .sort(([leftId, left], [rightId, right]) => (
          left.order - right.order || compareInstanceIds(leftId, rightId, ranks)
        ))
        .map(([instanceId]) => instanceId);
      const cards = instanceIds.flatMap((instanceId): WorkspaceCardEntryModel[] => {
        const placement = workspace.placements[instanceId];
        const card = poolById.get(instanceId);
        const basic = basicsById.get(instanceId);
        if (card === undefined && basic === undefined) return [];
        const name = card?.name ?? basic!.name;
        const sourcePrinting = card === undefined ? undefined : {
          setCode: card.set_code,
          collectorNumber: card.collector_number,
        };
        return [{
          key: instanceId,
          instanceId,
          name,
          sourcePrinting,
          image: { cardName: name, sourcePrinting, alt: name, draggable: false },
          preview: { name, sourcePrinting },
          isVirtualBasic: card === undefined,
          placement,
          order: placement.order,
        }];
      });
      const rowActive = dropState.zoneActive
        && dropState.column === column
        && dropState.row === row;
      rows.push({
        key: `${zone}:column:${column}:row:${row}`,
        row,
        count: cards.length,
        cards,
        drop: dropPresentation(rowActive, "workspace.drop.row"),
      });
    }

    const renderedCards = rows.flatMap((row) => row.cards);
    const descriptors: WorkspaceHeaderDescriptor[] = [];
    if (effective.sort === "cmc") {
      const manaValues = renderedCards.flatMap((card) => (
        card.isVirtualBasic ? [] : [poolById.get(card.instanceId)?.cmc ?? Number.NaN]
      ));
      // An occupied column adopts its cards' shared mana value; an empty one reverts to its own.
      const headerManaValue = manaValues.length === 0
        ? column
        : manaValues.every((value) => value === manaValues[0]) ? manaValues[0] : null;
      if (headerManaValue !== null && Number.isFinite(headerManaValue)) {
        descriptors.push({
          kind: "mana-value-column",
          manaValue: headerManaValue,
          labelKey: "manaCurve.bucketLabel",
          presentation: { kind: "numeric-badge", text: String(headerManaValue) },
        });
      }
    } else if (renderedCards.length === 0) {
      // Colour columns keep a fixed identity, so an emptied one falls back to it.
      if (effective.sort === "color") {
        const setColor = columnSetColor(column);
        descriptors.push({
          kind: "engine-group",
          groupKind: setColor,
          labelKey: POOL_GROUP_LABEL_KEYS[setColor],
          presentation: HEADER_PRESENTATIONS[setColor],
        });
      } else {
        descriptors.push({
          kind: "empty-ordinal",
          labelKey: "workspace.headers.emptyOrdinal",
          ordinal: column + 1,
        });
      }
    } else {
      const renderedIds = new Set(renderedCards.map((card) => card.instanceId));
      for (const group of activeGroups) {
        if (HEADER_PRESENTATIONS[group.kind].kind === "numeric-badge") continue;
        const contributes = [...renderedIds].some((instanceId) => (
          memberships.get(instanceId)?.includes(group.kind) === true
        ));
        if (contributes && !descriptors.some((descriptor) => (
          descriptor.kind === "engine-group" && descriptor.groupKind === group.kind
        ))) {
          descriptors.push({
            kind: "engine-group",
            groupKind: group.kind,
            labelKey: POOL_GROUP_LABEL_KEYS[group.kind],
            presentation: HEADER_PRESENTATIONS[group.kind],
          });
        }
      }
      const nonVirtualCards = renderedCards.filter((card) => !card.isVirtualBasic);
      const cmcKinds = nonVirtualCards.map((card) => cmcMemberships.get(card.instanceId) ?? []);
      const homogeneousCmc = nonVirtualCards.length > 0
        && cmcKinds.every((kinds) => kinds.length === 1 && kinds[0] === cmcKinds[0]?.[0])
        ? cmcKinds[0][0]
        : null;
      if (
        homogeneousCmc !== null
        && !descriptors.some((descriptor) => (
          descriptor.kind === "engine-group" && HEADER_PRESENTATIONS[descriptor.groupKind].kind === "numeric-badge"
        ))
      ) {
        descriptors.push({
          kind: "engine-group",
          groupKind: homogeneousCmc,
          labelKey: POOL_GROUP_LABEL_KEYS[homogeneousCmc],
          presentation: HEADER_PRESENTATIONS[homogeneousCmc],
        });
      }
    }
    if (renderedCards.some((card) => card.isVirtualBasic)) {
      descriptors.push({ kind: "added-basics", labelKey: "workspace.headers.addedBasics" });
    }
    if (renderedCards.some((card) => (
      !card.isVirtualBasic && !memberships.has(card.instanceId)
    ))) {
      descriptors.push({ kind: "unclassified", labelKey: "workspace.headers.unclassified" });
    }
    const columnActive = dropState.zoneActive && dropState.column === column;
    columns.push({
      key: `${zone}:column:${column}`,
      column,
      count: renderedCards.length,
      header: {
        key: `${zone}:column:${column}:header`,
        descriptors,
        count: renderedCards.length,
      },
      rows,
      drop: dropPresentation(columnActive, "workspace.drop.column"),
    });
  }

  const count = columns.reduce((total, column) => total + column.count, 0);
  return {
    key: `workspace-board:${zone}`,
    zone,
    requestedSort: preferences.sort,
    effectiveSort: effective.sort,
    columnCount: effective.columnCount,
    rowCount,
    count,
    showHeaders: preferences.showHeaders,
    columns,
    drop: dropPresentation(dropState.zoneActive, "workspace.drop.zone"),
  };
}

export function resolveWorkspaceSortColumn(
  card: DraftCardInstance,
  zone: DraftZone,
  pool: readonly DraftCardInstance[],
  poolGroups: DraftPoolGroups,
  workspace: DraftWorkspaceState,
  preferences: DraftBoardPreferences,
): number {
  const model = buildCardPoolBoardModel(zone, pool, poolGroups, workspace, preferences);
  if (model.effectiveSort === "cmc") {
    return manaValueColumn(card.cmc, model.columnCount);
  }
  const groupKind = cardSortGroup(card, model.effectiveSort);
  const matches = model.columns.filter((column) => {
    if (column.header.descriptors.some((descriptor) => descriptor.kind !== "engine-group")) {
      return false;
    }
    const sortGroupKinds = column.header.descriptors.flatMap((descriptor) => (
      descriptor.kind === "engine-group"
      && SORT_GROUP_KINDS[model.effectiveSort].has(descriptor.groupKind)
        ? [descriptor.groupKind]
        : []
    ));
    return sortGroupKinds.length === 1 && sortGroupKinds[0] === groupKind;
  });
  // A column already holding the group wins over an empty one merely reserved for it.
  const matchingColumn = matches.find((column) => column.count > 0) ?? matches[0];
  if (matchingColumn !== undefined) return matchingColumn.column;
  if (model.effectiveSort === "color") {
    return model.columns.find((column) => column.count === 0)?.column ?? 0;
  }
  return 0;
}

export function resolveWorkspacePickPlacement(
  card: DraftCardInstance,
  zone: DraftZone,
  pool: readonly DraftCardInstance[],
  poolGroups: DraftPoolGroups,
  workspace: DraftWorkspaceState,
  preferences: DraftBoardPreferences,
): { column: number; row?: number } {
  const row = preferences.rows === "two"
    ? (/\bcreature\b/i.test(card.type_line) ? 0 : 1)
    : undefined;
  const column = resolveWorkspaceSortColumn(
    card,
    zone,
    pool,
    poolGroups,
    workspace,
    preferences,
  );
  return row === undefined ? { column } : { column, row };
}
