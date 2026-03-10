import type { ObjectId } from "../adapter/types";
import type { GroupedPermanent } from "./battlefieldProps";

/**
 * Reorder the player's creature groups so assigned blockers sit opposite
 * their target attackers in the opponent row, minimizing line crossings
 * in BlockAssignmentLines.
 *
 * Blockers are sorted by their assigned attacker's visual column index.
 * Unassigned creatures are appended in their original order.
 */
export function sortCreaturesForBlockers(
  playerCreatures: GroupedPermanent[],
  opponentCreatures: GroupedPermanent[],
  blockerAssignments: Map<ObjectId, ObjectId>,
): GroupedPermanent[] {
  if (blockerAssignments.size === 0) return playerCreatures;

  // Map each attacker ObjectId → its visual column index in the opponent row
  const attackerColumn = new Map<ObjectId, number>();
  let col = 0;
  for (const group of opponentCreatures) {
    for (const id of group.ids) {
      attackerColumn.set(id, col);
    }
    col++;
  }

  // Split into blocker groups and non-blocker groups
  const blockerGroups: GroupedPermanent[] = [];
  const nonBlockerGroups: GroupedPermanent[] = [];

  for (const group of playerCreatures) {
    const isBlocker = group.ids.some((id) => blockerAssignments.has(id));
    if (isBlocker) {
      blockerGroups.push(group);
    } else {
      nonBlockerGroups.push(group);
    }
  }

  // Sort blocker groups by the column of their assigned attacker
  blockerGroups.sort((a, b) => {
    const colA = getMinAttackerColumn(a, blockerAssignments, attackerColumn);
    const colB = getMinAttackerColumn(b, blockerAssignments, attackerColumn);
    return colA - colB;
  });

  return [...blockerGroups, ...nonBlockerGroups];
}

/** Get the minimum attacker column for any blocker in this group. */
function getMinAttackerColumn(
  group: GroupedPermanent,
  blockerAssignments: Map<ObjectId, ObjectId>,
  attackerColumn: Map<ObjectId, number>,
): number {
  let min = Infinity;
  for (const id of group.ids) {
    const attackerId = blockerAssignments.get(id);
    if (attackerId !== undefined) {
      const col = attackerColumn.get(attackerId) ?? Infinity;
      if (col < min) min = col;
    }
  }
  return min;
}
