import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import type { DeckCardCount } from "../../../adapter/types";
import type {
  DraftCardPlacement,
  DraftWorkspaceState,
  DraftWorkspaceVirtualBasic,
} from "./types";

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

export function makeVirtualBasicInstanceId(token: string): string {
  return `workspace-basic:${token}`;
}

export function addVirtualBasic(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
  basic: DraftWorkspaceVirtualBasic,
): DraftWorkspaceState {
  if (basic.instanceId.length === 0 || basic.name.length === 0) return state;

  const identityExists = pool.some((card) => card.instance_id === basic.instanceId)
    || state.virtualBasics.some((current) => current.instanceId === basic.instanceId)
    || state.placements[basic.instanceId] !== undefined;
  if (identityExists) return state;

  return {
    ...state,
    placements: {
      ...state.placements,
      [basic.instanceId]: {
        zone: "deck",
        row: 0,
        column: 0,
        order: nextDefaultOrder(state.placements),
      },
    },
    virtualBasics: [...state.virtualBasics, basic],
  };
}

export function removeVirtualBasic(
  state: DraftWorkspaceState,
  instanceId: string,
): DraftWorkspaceState {
  const index = state.virtualBasics.findIndex((basic) => basic.instanceId === instanceId);
  if (index < 0) return state;

  const virtualBasics = state.virtualBasics.filter((_, currentIndex) => currentIndex !== index);
  const placements = { ...state.placements };
  delete placements[instanceId];
  return { ...state, placements, virtualBasics };
}

export function projectDeckNames(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
): string[] {
  return projectWorkspacePartition(state, pool).mainDeck;
}

export interface DraftWorkspacePartition {
  mainDeck: string[];
  sideboard: string[];
}

export function projectWorkspacePartition(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
): DraftWorkspacePartition {
  const partition: DraftWorkspacePartition = { mainDeck: [], sideboard: [] };
  for (const card of pool) {
    const zone = state.placements[card.instance_id]?.zone;
    if (zone === "deck") partition.mainDeck.push(card.name);
    if (zone === "sideboard") partition.sideboard.push(card.name);
  }
  for (const basic of state.virtualBasics) {
    const zone = state.placements[basic.instanceId]?.zone;
    if (zone === "deck") partition.mainDeck.push(basic.name);
    if (zone === "sideboard") partition.sideboard.push(basic.name);
  }
  return partition;
}

export function countProjectedNames(names: readonly string[]): DeckCardCount[] {
  const counts = new Map<string, number>();
  for (const name of names) counts.set(name, (counts.get(name) ?? 0) + 1);
  return [...counts].map(([name, count]) => ({ name, count }));
}

export function projectWorkspaceMainDeck(
  state: DraftWorkspaceState,
  pool: readonly DraftCardInstance[],
): string[] {
  return pool
    .filter((card) => state.placements[card.instance_id]?.zone === "deck")
    .map((card) => card.name);
}

export function projectWorkspaceLandCounts(
  state: DraftWorkspaceState,
): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const basic of state.virtualBasics) {
    if (state.placements[basic.instanceId]?.zone === "deck") {
      counts[basic.name] = (counts[basic.name] ?? 0) + 1;
    }
  }
  return counts;
}