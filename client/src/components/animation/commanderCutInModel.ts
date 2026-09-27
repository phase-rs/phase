import type { GameObject, GameState, ManaColor, ObjectId, PlayerId } from "../../adapter/types.ts";
import type { AnimationStep } from "../../animation/types.ts";

// Keep the entrance quick while leaving a generous hold for long names/art.
export const COMMANDER_CUT_IN_DURATION_MS = 3_200;

export interface CommanderCutInPayload {
  objectId: ObjectId;
  owner: PlayerId;
  name: string;
  colors: ManaColor[];
  oracleId?: string;
  faceName?: string;
}

function commanderForEvent(
  objectId: ObjectId,
  previousState: GameState | null,
  nextState: GameState | null,
): GameObject | null {
  const object = nextState?.objects[objectId] ?? previousState?.objects[objectId];
  return object?.is_commander === true ? object : null;
}

/**
 * Selects only engine-authored commander play moments from an animation step.
 * A cast is the normal path. Command -> Battlefield covers commander ninjutsu,
 * which is an activated ability rather than a spell cast. Stack -> Battlefield
 * is deliberately excluded so a normally cast commander is not shown twice.
 */
export function commanderCutInsForStep(
  step: AnimationStep,
  previousState: GameState | null,
  nextState: GameState | null,
): CommanderCutInPayload[] {
  const seen = new Set<ObjectId>();
  const cutIns: CommanderCutInPayload[] = [];

  for (const { event } of step.effects) {
    const objectId =
      event.type === "SpellCast"
        ? event.data.object_id
        : event.type === "ZoneChanged"
          && event.data.from === "Command"
          && event.data.to === "Battlefield"
          ? event.data.object_id
          : null;
    if (objectId == null || seen.has(objectId)) continue;

    const commander = commanderForEvent(objectId, previousState, nextState);
    if (!commander) continue;

    seen.add(objectId);
    cutIns.push({
      objectId,
      owner: commander.owner,
      name: commander.name,
      colors: commander.color,
      oracleId: commander.printed_ref?.oracle_id,
      faceName: commander.printed_ref?.face_name,
    });
  }

  return cutIns;
}
