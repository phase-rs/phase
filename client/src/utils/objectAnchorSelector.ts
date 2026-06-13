import type { ObjectId } from "../adapter/types.ts";

export function objectAnchorSelector(objectId: ObjectId): string {
  return `[data-object-id="${objectId}"], [data-grouped-ids~="${objectId}"]`;
}
