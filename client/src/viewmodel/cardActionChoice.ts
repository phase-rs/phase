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
  // CR 702.51a (convoke) + CR 701.67 (waterbend): tapping a creature/artifact
  // pays the spell's mana cost. The engine emits this only during
  // `WaitingFor::ManaPayment { convoke_mode: Some(_) }`, so it routes through
  // the same mana-tap ring as land taps — without this branch, convoke
  // creatures get no clickable affordance and the player cannot tap them.
  if (action.type === "TapForConvoke") return true;
  if (action.type !== "ActivateAbility") return false;
  // CR 605.1a: the engine classifies mana abilities (mana_abilities::is_mana_ability)
  // and exposes the verdict as the derived `is_mana_ability` key on the serialized
  // ability — the frontend reads the flag rather than introspecting the effect AST.
  return object?.abilities?.[action.data.ability_index]?.is_mana_ability === true;
}

/**
 * An action with meaningful one-shot consequences that must NOT be
 * auto-dispatched on a single tap — the player must confirm via the choice
 * modal even when it is the only legal action.
 *
 * CR 702.29a: a cycling ability's cost includes "Discard this card"; firing it
 * silently destroys a card the player may have intended to play. The same is
 * true of any hand-zone activated ability that discards itself (Channel).
 *
 * Card-consuming judgment is made by the engine and exposed as
 * `ability.consumes_source` (AbilityDefinition::consumes_source); the frontend
 * never inspects the cost tree. Prepared-copy casting is also gated here
 * because casting the copy unprepares the permanent, so a lone prepared action
 * needs an explicit "Cast <spell>" affordance instead of a silent click.
 *
 * Benign repeatable abilities ({T}: Scry 1, pingers, mana dorks) have
 * `consumes_source === false` and continue to auto-dispatch on a single tap.
 */
export function requiresConfirmation(
  action: GameAction,
  object: GameObject | undefined,
): boolean {
  if (action.type === "CastPreparedCopy") return true;
  if (action.type !== "ActivateAbility") return false;
  return object?.abilities?.[action.data.ability_index]?.consumes_source === true;
}

/**
 * The single authority for "given the legal actions for one object, should the
 * lone action auto-dispatch, or must the choice modal be shown?"
 *
 * Returns the action to auto-dispatch, or `null` if the choice modal must be
 * shown (either there is not exactly one action, or the one action needs
 * explicit player confirmation — see requiresConfirmation). Every interaction
 * call site delegates here; the decision is never re-implemented inline. Issue
 * #506: the bug existed because this branch was duplicated across five call
 * sites.
 */
export function resolveSingleActionDispatch(
  actions: GameAction[],
  object: GameObject | undefined,
): GameAction | null {
  if (actions.length !== 1) return null;
  return requiresConfirmation(actions[0], object) ? null : actions[0];
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
