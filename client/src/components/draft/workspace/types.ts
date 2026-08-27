export const DRAFT_WORKSPACE_SCHEMA_VERSION = 1 as const;
export const DRAFT_WORKSPACE_COLUMN_MAX = 20;
export const MAX_MATERIALIZED_VIRTUAL_BASICS = 1000;
export const MAX_DRAFT_WORKSPACE_NETWORK_PLACEMENTS = 4096;

export type DraftZone = "deck" | "sideboard";

export type DraftWorkspaceFilter = "combined" | DraftZone;

export interface DraftCardPlacement {
  zone: DraftZone;
  row: number;
  column: number;
  order: number;
}

export interface DraftWorkspaceVirtualBasic {
  instanceId: string;
  name: string;
}

export interface DraftWorkspaceState {
  schemaVersion: typeof DRAFT_WORKSPACE_SCHEMA_VERSION;
  placements: Record<string, DraftCardPlacement>;
  virtualBasics: DraftWorkspaceVirtualBasic[];
}

export function isPlainRecord(value: unknown): value is Record<PropertyKey, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function hasExactEnumerableKeys(
  value: Record<PropertyKey, unknown>,
  expected: readonly string[],
): boolean {
  const keys = Reflect.ownKeys(value);
  return keys.length === expected.length
    && keys.every((key) =>
      typeof key === "string"
      && expected.includes(key)
      && Object.prototype.propertyIsEnumerable.call(value, key)
    );
}

function isOrdinaryArray(value: unknown): value is unknown[] {
  return Array.isArray(value) && Object.getPrototypeOf(value) === Array.prototype;
}

function isSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value);
}

export function validateWorkspaceState(
  raw: unknown,
  { maxPlacementCount }: { maxPlacementCount?: number } = {},
): DraftWorkspaceState | { error: string } {
  try {
    if (!isPlainRecord(raw)) return { error: "workspace state must be a plain object" };
    if (!hasExactEnumerableKeys(raw, ["schemaVersion", "placements", "virtualBasics"])) {
      return { error: "workspace state has invalid fields" };
    }
    if (raw.schemaVersion !== DRAFT_WORKSPACE_SCHEMA_VERSION) {
      return { error: `workspace schema version must be ${DRAFT_WORKSPACE_SCHEMA_VERSION}` };
    }
    const rawPlacements = raw.placements;
    if (!isPlainRecord(rawPlacements)) return { error: "placements must be a plain object" };
    const placementKeys = Reflect.ownKeys(rawPlacements);
    if (maxPlacementCount !== undefined && placementKeys.length > maxPlacementCount) {
      return { error: `placements cannot exceed ${maxPlacementCount} entries` };
    }

    const placements: Record<string, DraftCardPlacement> = {};
    for (const key of placementKeys) {
      if (
        typeof key !== "string"
        || key.trim().length === 0
        || !Object.prototype.propertyIsEnumerable.call(rawPlacements, key)
      ) {
        return { error: "placement instance IDs must be nonblank enumerable strings" };
      }

      const placement = rawPlacements[key];
      if (!isPlainRecord(placement)) {
        return { error: `placement ${key} must be a plain object` };
      }
      if (!hasExactEnumerableKeys(placement, ["zone", "row", "column", "order"])) {
        return { error: `placement ${key} has invalid fields` };
      }
      if (placement.zone !== "deck" && placement.zone !== "sideboard") {
        return { error: `placement ${key} has an invalid zone` };
      }
      if (!isSafeInteger(placement.row) || placement.row < 0 || placement.row > 1) {
        return { error: `placement ${key} has an invalid row` };
      }
      if (
        !isSafeInteger(placement.column)
        || placement.column < 0
        || placement.column >= DRAFT_WORKSPACE_COLUMN_MAX
      ) {
        return { error: `placement ${key} has an invalid column` };
      }
      if (!isSafeInteger(placement.order) || placement.order < 0) {
        return { error: `placement ${key} has an invalid order` };
      }

      placements[key] = {
        zone: placement.zone,
        row: placement.row,
        column: placement.column,
        order: placement.order,
      };
    }

    if (!isOrdinaryArray(raw.virtualBasics)) {
      return { error: "virtualBasics must be an ordinary array" };
    }
    if (raw.virtualBasics.length > MAX_MATERIALIZED_VIRTUAL_BASICS) {
      return { error: `virtualBasics cannot exceed ${MAX_MATERIALIZED_VIRTUAL_BASICS} entries` };
    }

    const virtualBasics: DraftWorkspaceVirtualBasic[] = [];
    const virtualInstanceIds = new Set<string>();
    for (let index = 0; index < raw.virtualBasics.length; index += 1) {
      const basic = raw.virtualBasics[index];
      if (!isPlainRecord(basic)) {
        return { error: `virtualBasics[${index}] must be a plain object` };
      }
      if (!hasExactEnumerableKeys(basic, ["instanceId", "name"])) {
        return { error: `virtualBasics[${index}] has invalid fields` };
      }
      if (typeof basic.instanceId !== "string" || basic.instanceId.trim().length === 0) {
        return { error: `virtualBasics[${index}] has an invalid instanceId` };
      }
      if (typeof basic.name !== "string" || basic.name.trim().length === 0) {
        return { error: `virtualBasics[${index}] has an invalid name` };
      }
      if (virtualInstanceIds.has(basic.instanceId)) {
        return { error: `virtualBasics[${index}] has a duplicate instanceId` };
      }

      virtualInstanceIds.add(basic.instanceId);
      virtualBasics.push({ instanceId: basic.instanceId, name: basic.name });
    }

    return { schemaVersion: DRAFT_WORKSPACE_SCHEMA_VERSION, placements, virtualBasics };
  } catch {
    return { error: "workspace state could not be inspected" };
  }
}
