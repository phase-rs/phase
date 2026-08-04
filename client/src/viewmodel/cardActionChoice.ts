import type { GameAction, GameObject, ObjectId, WaitingFor } from "../adapter/types.ts";

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
    || a.type === "CastSpellForFree"
    || a.type === "CastSpellAsSneak"
    || a.type === "CastSpellAsWebSlinging"
    || a.type === "CastSpellAsMiracle"
    || a.type === "CastSpellAsMadness"
    || a.type === "PlayFaceDown"
    || a.type === "PlayLand"
  );
}

/**
 * Resolve the one action that a release-to-cast gesture may dispatch directly.
 *
 * The engine-provided per-object bucket is the authority for legality. We then
 * reuse the existing auto-dispatch guard (single action, no confirmation) and
 * the shared play/cast family projection. A card with another hand action or
 * multiple casting modes therefore stays inspectable/playable, but never turns
 * gold with a promise that releasing will cast immediately.
 */
export function resolveDirectPlayOrCastAction(
  legalActionsByObject: Record<string, GameAction[]> | undefined,
  object: GameObject | undefined,
): GameAction | null {
  if (!object) return null;
  const directAction = resolveSingleActionDispatch(
    collectObjectActions(legalActionsByObject, object.id),
    object,
  );
  if (!directAction) return null;
  return playOrCastActionsForObject(legalActionsByObject, object.id).includes(directAction)
    ? directAction
    : null;
}

export interface ActivationAffordances {
  /** Objects with >=1 NON-mana action, offered only where the engine accepts an
   *  activation: CR 113.3b — "A player may activate such an ability whenever they
   *  have priority". Membership-queried by id; NOT zone-scoped (a hand card with a
   *  CastSpell is a member) — never enumerate this set to build a zone listing. */
  activatableObjectIds: Set<number>;
  /** Objects with >=1 mana action (CR 605.1a), additionally offered during the
   *  cost-payment states whose `apply` arms accept a mana activation. */
  manaTappableObjectIds: Set<number>;
}

/**
 * THE single authority for "which objects may be activated right now, and through
 * which ring". Every deciding activation surface consumes this — none re-derives it.
 *
 * Lifted verbatim from `GameBoard`'s `boardInteractionState` memo, with the three
 * `gameState.objects[objectId]` reads rewritten to `objects[objectId]` because the
 * parameter is the objects map, not the game state.
 */
export function deriveActivationAffordances(
  waitingFor: WaitingFor | null,
  canActForWaitingState: boolean,
  legalActionsByObject: Record<string, GameAction[]>,
  objects: Record<string, GameObject> | undefined,
): ActivationAffordances {
  const activatableObjectIds = new Set<number>();
  const manaTappableObjectIds = new Set<number>();
  if (!objects) return { activatableObjectIds, manaTappableObjectIds };

  const playerCanAct =
    waitingFor != null
    && (
      (waitingFor.type === "Priority" && canActForWaitingState)
      || (waitingFor.type === "ManaPayment" && canActForWaitingState)
      || (waitingFor.type === "UnlessPayment" && canActForWaitingState)
      // CR 118.12a: Disjunctive unless-cost — same input enablement as
      // UnlessPayment (player chooses among sub-costs).
      || (waitingFor.type === "UnlessPaymentChooseCost" && canActForWaitingState)
    );

  if (waitingFor?.type === "Priority" && canActForWaitingState) {
    // The engine owns the "which permanent does this action act on" mapping
    // via GameAction::source_object(), exposed as `legalActionsByObject`.
    // The cyan activatable ring surfaces permanents with at least one non-mana
    // action; mana abilities are handled by the separate mana ring below. This
    // iteration is variant-agnostic — a future keyword activation needs zero
    // frontend changes.
    for (const [idStr, actions] of Object.entries(legalActionsByObject)) {
      const objectId = Number(idStr);
      const object = objects[objectId];
      if (!object) continue;
      if (actions.some((action) => !isManaObjectAction(action, object))) {
        activatableObjectIds.add(objectId);
      }
    }
  }

  if (playerCanAct) {
    for (const [idStr, actions] of Object.entries(legalActionsByObject)) {
      const objectId = Number(idStr);
      const object = objects[objectId];
      if (!object) continue;
      if (actions.some((action) => isManaObjectAction(action, object))) {
        manaTappableObjectIds.add(objectId);
      }
    }
  }

  return { activatableObjectIds, manaTappableObjectIds };
}

export type ObjectActivation =
  | { kind: "dispatch"; action: GameAction }
  | { kind: "choose"; actions: GameAction[] }
  | { kind: "none" };

/**
 * THE single authority for "given one object's engine bucket, what does a click do".
 *
 * Owns the CR 605.1a mana/non-mana partition; EVERY dispatch decision — mana and
 * non-mana alike — goes through resolveSingleActionDispatch (#506). CR 113.3b
 * keyword activations (Crew/Station/Equip/Saddle) are surfaced alongside activated
 * abilities. The merged order is [abilities, keywords, mana].
 *
 * Takes the WHOLE `ActivationAffordances` pair rather than a loose boolean per
 * ring: each ring is dropped by the same authority that painted it, so a caller
 * cannot hand this function half of its own gate. An earlier signature took only
 * the mana bit, which merged the non-mana partition unconditionally and let a
 * cost-payment prompt offer an activation the board itself refuses.
 */
export function resolveObjectActivation(
  actions: GameAction[],
  object: GameObject | undefined,
  affordances: ActivationAffordances,
  objectId: ObjectId,
): ObjectActivation {
  const abilityActions: GameAction[] = [];
  const manaActions: GameAction[] = [];
  const keywordActions: GameAction[] = [];
  for (const action of actions) {
    if (isManaObjectAction(action, object)) {
      manaActions.push(action);
    } else if (action.type === "ActivateAbility") {
      abilityActions.push(action);
    } else {
      keywordActions.push(action);
    }
  }

  const merged: GameAction[] = [];
  // CR 113.3b: an activated ability that isn't a mana ability may be activated
  // only when its controller has priority. `activatableObjectIds` is populated
  // for exactly that state, so a cost-payment prompt (ManaPayment /
  // UnlessPayment / UnlessPaymentChooseCost) drops the non-mana partition
  // instead of offering an activation the engine would refuse.
  if (affordances.activatableObjectIds.has(objectId)) {
    merged.push(...abilityActions, ...keywordActions);
  }
  // CR 605.1a: mana abilities are additionally offered during those payment
  // states, whose `apply` arms accept a mana activation.
  if (affordances.manaTappableObjectIds.has(objectId)) merged.push(...manaActions);
  if (merged.length === 0) return { kind: "none" };

  const auto = resolveSingleActionDispatch(merged, object);
  return auto ? { kind: "dispatch", action: auto } : { kind: "choose", actions: merged };
}
