import type { GameAction, GameObject, ObjectId } from "../adapter/types.ts";

/**
 * Look up the legal actions whose `source_object()` is `objectId`.
 *
 * Per CLAUDE.md "the frontend is a display layer, not a logic layer", the
 * mapping from `GameAction` variant to "the permanent it acts on" is owned
 * by the engine via `GameAction::source_object()`. This function is now a
 * trivial map lookup over the engine-provided `legalActionsByObject` field
 * — never a client-side discriminated-union introspection.
 */
export function collectObjectActions(
  legalActionsByObject: Record<string, GameAction[]> | undefined,
  objectId: ObjectId,
): GameAction[] {
  if (!legalActionsByObject) return [];
  return legalActionsByObject[String(objectId)] ?? [];
}

export function isManaObjectAction(action: GameAction, object: GameObject | undefined): boolean {
  if (action.type === "TapLandForMana") return true;
  if (action.type !== "ActivateAbility") return false;
  return object?.abilities?.[action.data.ability_index]?.effect?.type === "Mana";
}

/**
 * Filter `legalActionsByObject` entries for a zone-viewable card to the
 * play-or-cast actions only.
 *
 * Engine authority — covers Adventure, Foretell, Plot, Suspend, Warp, and any
 * future exile-cast permission (cast-family variants), plus `PlayLand` for
 * Future Sight / Bolas's Citadel / Magus of the Future top-of-library land
 * plays. The frontend renders whatever the engine reports — no per-mechanic
 * permission inspection.
 */
export function playOrCastActionsForObject(
  legalActionsByObject: Record<string, GameAction[]> | undefined,
  objectId: ObjectId,
): GameAction[] {
  return collectObjectActions(legalActionsByObject, objectId).filter((a) =>
    a.type === "CastSpell"
    || a.type === "CastSpellAsSneak"
    || a.type === "CastSpellAsWebSlinging"
    || a.type === "CastSpellAsMiracle"
    || a.type === "CastSpellAsMadness"
    || a.type === "PlayLand"
  );
}
