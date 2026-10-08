import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import { createDraftWorkspaceState } from "./workspacePlacement";
import { MAX_MATERIALIZED_VIRTUAL_BASICS, type DraftWorkspaceState } from "./types";

export { MAX_MATERIALIZED_VIRTUAL_BASICS };

export function normalizeVirtualBasicCount(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0
    ? Math.floor(value)
    : 0;
}

export interface LegacyWorkspaceInput {
  mainDeck: readonly string[];
  landCounts: Readonly<Record<string, number>>;
}

export function makeLegacyVirtualBasicInstanceId(
  name: string,
  ordinal: number,
  usedInstanceIds: ReadonlySet<string>,
): string {
  const base = `workspace-basic:legacy:${encodeURIComponent(name)}:${ordinal}`;
  if (!usedInstanceIds.has(base)) return base;

  for (let suffix = 1; suffix <= usedInstanceIds.size + 1; suffix += 1) {
    const candidate = `${base}~${suffix}`;
    if (!usedInstanceIds.has(candidate)) return candidate;
  }
  throw new Error("Unable to allocate a legacy virtual basic identity");
}

export function migrateLegacyWorkspace(
  pool: readonly DraftCardInstance[],
  legacy: LegacyWorkspaceInput,
): DraftWorkspaceState {
  const state = createDraftWorkspaceState();
  const remainingDeckNames = new Map<string, number>();
  for (const name of legacy.mainDeck) {
    remainingDeckNames.set(name, (remainingDeckNames.get(name) ?? 0) + 1);
  }

  let deckOrder = 0;
  let sideboardOrder = 0;
  for (const card of pool) {
    const remaining = remainingDeckNames.get(card.name) ?? 0;
    const inDeck = remaining > 0;
    if (inDeck) remainingDeckNames.set(card.name, remaining - 1);
    state.placements[card.instance_id] = {
      zone: inDeck ? "deck" : "sideboard",
      row: 0,
      column: 0,
      order: inDeck ? deckOrder++ : sideboardOrder++,
    };
  }

  const usedInstanceIds = new Set(pool.map((card) => card.instance_id));
  let remainingCapacity = MAX_MATERIALIZED_VIRTUAL_BASICS;
  for (const name of Object.keys(legacy.landCounts).sort()) {
    const rawCount = legacy.landCounts[name];
    const count = Math.min(normalizeVirtualBasicCount(rawCount), remainingCapacity);
    for (let ordinal = 0; ordinal < count; ordinal += 1) {
      const instanceId = makeLegacyVirtualBasicInstanceId(name, ordinal, usedInstanceIds);
      usedInstanceIds.add(instanceId);
      state.virtualBasics.push({ instanceId, name });
      state.placements[instanceId] = {
        zone: "deck",
        row: 0,
        column: 0,
        order: deckOrder++,
      };
    }
    remainingCapacity -= count;
    if (remainingCapacity === 0) break;
  }

  return state;
}