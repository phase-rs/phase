import type { GameObject, ObjectId } from "../../../adapter/types.ts";
import { manaValueOfObject } from "./manaValue.ts";

type ObjLookup = Record<ObjectId, GameObject | undefined>;

export type SortKey = "none" | "name" | "cmc" | "type" | "color";
export type GroupKey = "none" | "type" | "color";

function primaryType(obj: GameObject | undefined): string {
  return obj?.card_types.core_types[0] ?? "";
}

function primaryColor(obj: GameObject | undefined): string {
  // Engine-provided color array; "Colorless" bucket for empty. Display-only.
  return obj && obj.color.length > 0 ? obj.color[0] : "Colorless";
}

// Stable sort: returns a new array; never mutates the input.
export function orderCards(cards: ObjectId[], objects: ObjLookup, sort: SortKey): ObjectId[] {
  if (sort === "none") return [...cards];
  const keyed = cards.map((id, index) => ({ id, index, obj: objects[id] }));
  keyed.sort((a, b) => {
    let cmp = 0;
    switch (sort) {
      case "name":
        cmp = (a.obj?.name ?? "").localeCompare(b.obj?.name ?? "");
        break;
      case "cmc":
        cmp =
          (a.obj ? manaValueOfObject(a.obj) : 0) -
          (b.obj ? manaValueOfObject(b.obj) : 0);
        break;
      case "type":
        cmp = primaryType(a.obj).localeCompare(primaryType(b.obj));
        break;
      case "color":
        cmp = primaryColor(a.obj).localeCompare(primaryColor(b.obj));
        break;
    }
    return cmp !== 0 ? cmp : a.index - b.index; // stable on ties
  });
  return keyed.map((k) => k.id);
}

// Groups while preserving first-appearance order of both groups and members.
export function groupCards(
  ordered: ObjectId[],
  objects: ObjLookup,
  group: GroupKey,
): { key: string; ids: ObjectId[] }[] {
  if (group === "none") return [{ key: "", ids: [...ordered] }];
  const keyOf = (id: ObjectId) =>
    group === "type" ? primaryType(objects[id]) : primaryColor(objects[id]);
  const order: string[] = [];
  const byKey = new Map<string, ObjectId[]>();
  for (const id of ordered) {
    const k = keyOf(id);
    if (!byKey.has(k)) {
      byKey.set(k, []);
      order.push(k);
    }
    byKey.get(k)!.push(id);
  }
  return order.map((k) => ({ key: k, ids: byKey.get(k)! }));
}

export function applyBulk(
  action: "all" | "invert" | "clear",
  ordered: ObjectId[],
  value: Set<ObjectId>,
  cap: number,
): Set<ObjectId> {
  if (action === "clear") return new Set();
  if (action === "all") return new Set(ordered.slice(0, cap));
  // invert: complement within `ordered`, truncated to cap in display order.
  const complement = ordered.filter((id) => !value.has(id));
  return new Set(complement.slice(0, cap));
}

export function rangeAdd(
  ordered: ObjectId[],
  fromIdx: number,
  toIdx: number,
  value: Set<ObjectId>,
  cap: number,
): Set<ObjectId> {
  // Clamp both endpoints into [0, maxIdx] before iterating. A stale shift anchor
  // or an unmapped (-1) ordered-index must never index out of bounds — `ordered`
  // is a dense ObjectId[], so clamped access can't yield `undefined` and pollute
  // the set, which would fail the engine's set-membership check on dispatch.
  const maxIdx = ordered.length - 1;
  if (maxIdx < 0) return new Set(value);
  const clampedFrom = Math.max(0, Math.min(maxIdx, fromIdx));
  const clampedTo = Math.max(0, Math.min(maxIdx, toIdx));
  const lo = Math.min(clampedFrom, clampedTo);
  const hi = Math.max(clampedFrom, clampedTo);
  const next = new Set(value);
  for (let i = lo; i <= hi && next.size < cap; i++) {
    next.add(ordered[i]);
  }
  return next;
}
