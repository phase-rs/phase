use crate::types::ability::{
    ChoiceAssignmentSide, ContinuousModification, Duration, Effect, EffectError, EffectKind,
    ObjectPlayerAssignment, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 613.3: GainControl creates a transient continuous effect that changes the
/// target permanent's controller through the layer system (Layer 2).
///
/// The duration comes from the resolved ability: "until end of turn" → UntilEndOfTurn,
/// permanent control change → Permanent (indefinite). The layer system handles
/// reverting control when the effect expires.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 613.1b: Layer 2 — control-changing effects are applied.
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);

    let Effect::GainControl { target } = &ability.effect else {
        return Err(EffectError::InvalidParam(
            "expected GainControl effect".to_string(),
        ));
    };

    let new_controller = gain_control_controller(ability, target);
    let object_ids = gain_control_object_targets(state, ability, target);

    for obj_id in object_ids {
        let Some(old_controller) = state.objects.get(&obj_id).map(|obj| obj.controller) else {
            return Err(EffectError::ObjectNotFound(obj_id));
        };

        // CR 613.3: Create a transient continuous effect at Layer 2 (Control).
        state.add_transient_continuous_effect(
            ability.source_id,
            new_controller,
            duration.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::ChangeController],
            None,
        );
        mark_echo_due_for_new_controller(state, obj_id);

        // CR 613.1b: emit the control-change event so "when you lose control"
        // triggers on the *previous* controller observe the loss (mirrors
        // `GainControlAll` and `GiveControl`). Skip no-op self-handoffs.
        if old_controller != new_controller {
            events.push(GameEvent::ControllerChanged {
                object_id: obj_id,
                old_controller,
                new_controller,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

pub(crate) fn apply_permanent_control_change(
    state: &mut GameState,
    source_id: ObjectId,
    object_id: ObjectId,
    new_controller: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let old_controller = state.objects.get(&object_id).map(|obj| obj.controller);
    state.add_transient_continuous_effect(
        source_id,
        new_controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: object_id },
        vec![ContinuousModification::ChangeController],
        None,
    );
    mark_echo_due_for_new_controller(state, object_id);
    if let Some(old_controller) = old_controller.filter(|old| *old != new_controller) {
        events.push(GameEvent::ControllerChanged {
            object_id,
            old_controller,
            new_controller,
        });
    }
}

/// CR 613.1b: Mass control-change (Layer 2 — control-changing effects) — gain
/// control of EVERY battlefield permanent matching the effect's `target` filter
/// (the untargeted "all" counterpart of [`resolve`], mirroring
/// `destroy::resolve_all`). Hellkite Tyrant's "gain control of all artifacts
/// that player controls": the filter is enumerated against the battlefield with
/// an ability-bound [`FilterContext`], so a `controller: TargetPlayer` clause
/// resolves to the effect's player target (e.g. the player dealt combat damage),
/// and one Layer-2 control TCE is registered per match.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);

    let Effect::GainControlAll { target } = &ability.effect else {
        return Err(EffectError::InvalidParam(
            "expected GainControlAll effect".to_string(),
        ));
    };

    // "you gain control" — the ability's controller takes control.
    let new_controller = ability.controller;

    // Ability-context filter evaluation, identical to `destroy::resolve_all`:
    // `resolved_object_filter` binds anaphoric scopes (e.g. `controller:
    // TargetPlayer`) from the ability before matching.
    let effective_filter = crate::game::effects::resolved_object_filter(state, ability, target);
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter(|id| {
            crate::game::filter::matches_target_filter(state, **id, &effective_filter, &ctx)
        })
        .copied()
        .collect();

    for obj_id in matching {
        let old_controller = state.objects.get(&obj_id).map(|obj| obj.controller);
        // CR 613.1b: register a Layer 2 (Control) transient continuous effect.
        state.add_transient_continuous_effect(
            ability.source_id,
            new_controller,
            duration.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::ChangeController],
            None,
        );
        mark_echo_due_for_new_controller(state, obj_id);
        if let Some(old_controller) = old_controller.filter(|old| *old != new_controller) {
            events.push(GameEvent::ControllerChanged {
                object_id: obj_id,
                old_controller,
                new_controller,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 613.3: The player who gains control. Normally the ability controller;
/// after a resolution-scoped `Choose(Opponent)` whose dependent effect is
/// `GainControl { SelfRef }`, the chosen opponent is the recipient
/// (Wishclaw Talisman — "an opponent gains control of ~").
fn gain_control_controller(ability: &ResolvedAbility, target: &TargetFilter) -> PlayerId {
    if matches!(target, TargetFilter::SelfRef) {
        if let Some(&recipient) = ability.chosen_players.last() {
            return recipient;
        }
    }
    // CR 701.38d + CR 108.3: When resolving per-ballot in a vote tally
    // (Expropriate), the iteration rebinds `controller` to the voter so
    // voter-referential filters scope correctly. The *original* controller
    // (the spell caster) is the one who gains control. Fall back to
    // `ability.controller` for non-vote contexts where `original_controller`
    // is None.
    ability.original_controller.unwrap_or(ability.controller)
}

fn gain_control_object_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    // CR 608.2c: `SelfRef` binds to the ability source even when target
    // propagation has populated `ability.targets`.
    if matches!(filter, TargetFilter::SelfRef) {
        return vec![ability.source_id];
    }

    // CR 608.2c: a precise slot anaphor ("gain control of that Equipment" →
    // slot 1) indexes the whole resolving chain's declared targets. The
    // per-clause `ability.targets` may carry only the nearest propagated target,
    // so route through the root-chain slot authority. The slot answer is final:
    // falling through would index the local targets and then `resolved_targets`'
    // whole root chain, taking control of every declared object. CR 608.2b: a
    // slot whose target was illegal at resolution (or whose pinned referent
    // departed, CR 400.7) gains control of nothing.
    if let TargetFilter::ParentTargetSlot { index } = filter {
        return crate::game::targeting::resolve_live_parent_slot_from_root(state, ability, *index)
            .and_then(|target| match target {
                TargetRef::Object(id) => Some(id),
                TargetRef::Player(_) => None,
            })
            .into_iter()
            .collect();
    }

    // CR 400.7 + CR 603.7c: a delayed gain-control whose pinned referent became
    // a new object controls nothing. This read is RAW and returns below before
    // `resolved_targets` — the chokepoint the targeting guard covers — is ever
    // reached, so the substitution MUST happen here or the pin is never checked
    // at all. A delayed ParentTarget trigger's `targets` are non-empty by
    // construction, so the early return below always fires for it.
    //
    // No early return is needed, and that is verified rather than assumed: if
    // `chosen_objects` empties, control falls to `resolved_targets` (which also
    // yields empty), `resolve` then iterates an empty list, skips the loop body,
    // and falls to its UNCONDITIONAL `EffectResolved` push. An emptied list is
    // already a clean no-op with the event.
    //
    // Slot carve-out does NOT apply here: `ParentTargetSlot` returns above
    // through `resolve_live_parent_slot_from_root` and never reaches this read.
    // Adding a `matches!` guard would be dead code.
    let live_targets = ability.live_object_targets(state);
    let chosen_objects = super::effect_object_targets(filter, &live_targets);

    if !chosen_objects.is_empty() {
        return chosen_objects;
    }

    crate::game::targeting::resolved_targets(ability, filter, state)
        .into_iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(id),
            TargetRef::Player(_) => None,
        })
        .collect()
}

/// CR 110.2 + CR 608.2c: who receives control of each object a `GiveControl`
/// hands.
enum ControlRecipient<'a> {
    /// One player receives every handed object (every pre-existing recipient form).
    Single(PlayerId),
    /// CR 608.2c + CR 608.2d: each object goes to the player this ability chose
    /// for it ("… gains control of the permanent for which they were chosen").
    PerObject(&'a [ObjectPlayerAssignment]),
}

impl ControlRecipient<'_> {
    /// The player who receives `object`. CR 609.3 + CR 800.4b: none when no
    /// player was chosen for it, or when the chosen player has left the game —
    /// that object doesn't change control and no effect is installed for it,
    /// while the other entries still apply.
    fn for_object(&self, state: &GameState, object: ObjectId) -> Option<PlayerId> {
        match self {
            Self::Single(player) => Some(*player),
            Self::PerObject(entries) => entries
                .iter()
                .find(|entry| entry.object == object)
                .map(|entry| entry.player)
                .filter(|player| crate::game::players::is_alive(state, *player)),
        }
    }
}

/// CR 110.2 + CR 608.2c: the recipient authority of `GiveControl`.
///
/// The per-object reference (`ChoiceAssignment { side: PlayerForObject }`) is
/// answered first and alone: each handed object's recipient is the player this
/// ability's own assignment recorded for it, so a stray player in
/// `ability.targets` must not override it. Every other recipient form resolves
/// to one player before any object is handed, exactly as before.
fn give_control_recipient<'a>(
    state: &GameState,
    ability: &'a ResolvedAbility,
    recipient: &TargetFilter,
) -> Result<ControlRecipient<'a>, EffectError> {
    if let TargetFilter::ChoiceAssignment {
        side: ChoiceAssignmentSide::PlayerForObject,
    } = recipient
    {
        return Ok(ControlRecipient::PerObject(
            &ability.context.object_player_assignment,
        ));
    }

    // CR 110.2 + CR 613.3: The recipient is the player target when one is
    // explicitly in ability.targets (normal targeting path). When no player
    // target is present — e.g. a post-replacement continuation whose target
    // list only carries the damaged object — resolve the effect's `recipient`
    // filter only if it identifies exactly one legal player. CR 608.2d choices
    // are made while applying the effect; arbitrary first-match selection would
    // be wrong in multiplayer when several opponents are legal.
    let recipient_id = if let Some(pid) = ability.targets.iter().find_map(|t| {
        if let TargetRef::Player(pid) = t {
            Some(*pid)
        } else {
            None
        }
    }) {
        pid
    } else {
        unique_recipient_from_filter(state, recipient, ability)?
    };
    Ok(ControlRecipient::Single(recipient_id))
}

/// CR 110.2: Give control of target permanent to a specified recipient player.
/// Unlike `resolve` (controller takes), this transfers to a different player
/// specified by the recipient target: one player for every handed object
/// (`ControlRecipient::Single`), or, for the per-object reference, the player
/// this ability chose for each object (`ControlRecipient::PerObject`).
pub fn resolve_give(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);

    let Effect::GiveControl { target, recipient } = &ability.effect else {
        return Err(EffectError::MissingParam("GiveControl".to_string()));
    };

    let recipient = give_control_recipient(state, ability, recipient)?;

    let object_ids = give_control_object_targets(state, ability, target);

    // CR 608.2e + CR 608.2f: one application processes every handed object;
    // each change applies in layer 2 (CR 613.1b) and lasts as stated, else
    // indefinitely (CR 611.2a). CR 613.7b: each effect is timestamped as it is
    // created; the objects are distinct, so their order is immaterial.
    for obj_id in object_ids {
        if !state.objects.contains_key(&obj_id) {
            return Err(EffectError::ObjectNotFound(obj_id));
        }

        // CR 609.3 + CR 800.4b: no player chosen for this object, or the
        // chosen player has left the game — it stays with its controller and
        // no effect is installed for it; the other objects still change.
        let Some(recipient_id) = recipient.for_object(state, obj_id) else {
            continue;
        };

        let old_controller = state.objects.get(&obj_id).map(|obj| obj.controller);

        // CR 613.3: Create a transient continuous effect at Layer 2 (Control)
        // with the recipient as the new controller.
        state.add_transient_continuous_effect(
            ability.source_id,
            recipient_id,
            duration.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            vec![ContinuousModification::ChangeController],
            None,
        );
        mark_echo_due_for_new_controller(state, obj_id);

        // CR 110.2: Record the handoff for downstream "if they do" riders and
        // `ControllerChanged` triggers (mirrors `resolve_all`).
        if old_controller.is_some_and(|old| old != recipient_id) {
            events.push(GameEvent::ControllerChanged {
                object_id: obj_id,
                old_controller: old_controller.unwrap(),
                new_controller: recipient_id,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GiveControl,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 611.2c: the objects whose controller this effect changes, fixed when the
/// control-change continuous effect begins.
///
/// SINGLE AUTHORITY: `resolve_give` hands control over exactly this list, and
/// `effects::affected_objects_from_events` publishes exactly this list as the
/// chain tracked set. The `ControllerChanged` event is deliberately NOT the
/// authority: `resolve_give` emits it only when the controller actually changed,
/// while CR 608.2c makes "those creatures" name the objects the earlier text
/// named — Domineering Will's "up to three target nonattacking creatures … Untap
/// those creatures" must untap a target the recipient already controlled, which
/// produces no event. The per-object assignment's objects
/// (`ChoiceAssignment { side: Objects }`) are answered here too, from the
/// resolving ability's own record.
pub(crate) fn give_control_object_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    // CR 608.2c: `SelfRef` ("this artifact") binds to the ability source even
    // when target propagation has populated `ability.targets`.
    if matches!(filter, TargetFilter::SelfRef) {
        return vec![ability.source_id];
    }

    // CR 608.2c + CR 608.2d: "the permanent for which they were chosen" — the
    // objects this ability's own object→player assignment pairs with a player, in
    // assignment order. CR 400.7: an object no longer on the battlefield is a new
    // object and is not handed. A player projection names no object.
    if let TargetFilter::ChoiceAssignment { side } = filter {
        return match side {
            ChoiceAssignmentSide::Objects => assigned_battlefield_objects(state, ability),
            ChoiceAssignmentSide::PlayerForObject => Vec::new(),
        };
    }

    // CR 400.7 + CR 603.7c: identical shape to `gain_control_object_targets`
    // above — a RAW read that returns before the chokepoint. `GiveControl` is
    // Tier C (1 pinned pair, `burning cinder fury of crimson chaos fire`, whose
    // node carries BOTH an object `target` and a player `recipient`;
    // `live_object_targets` passes `TargetRef::Player` through by construction,
    // so the recipient is untouched).
    //
    // No early return needed, re-verified at `resolve_give` rather than copied:
    // an emptied list skips the loop and reaches the unconditional
    // `EffectResolved` push.
    //
    // Slot carve-out DOES apply here — unlike `gain_control_object_targets`,
    // this function has no `ParentTargetSlot` pre-arm, so a slot filter can
    // reach the positional indexer. Pass the raw list for that shape.
    let live_targets = ability.live_object_targets(state);
    let pool: &[TargetRef] = if matches!(filter, TargetFilter::ParentTargetSlot { .. }) {
        &ability.targets
    } else {
        &live_targets
    };
    let chosen_objects = super::effect_object_targets(filter, pool);

    if !chosen_objects.is_empty() {
        return chosen_objects;
    }

    crate::game::targeting::resolved_targets(ability, filter, state)
        .into_iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(id),
            TargetRef::Player(_) => None,
        })
        .collect()
}

/// CR 608.2c + CR 608.2d: the objects the resolving ability's object→player
/// assignment pairs with a player, in assignment order, each once. CR 400.7:
/// an assigned object that has left the battlefield is a new object, so it is
/// not one of them.
fn assigned_battlefield_objects(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let mut objects: Vec<ObjectId> = Vec::new();
    for entry in &ability.context.object_player_assignment {
        let on_battlefield = state
            .objects
            .get(&entry.object)
            .is_some_and(|obj| obj.zone == Zone::Battlefield);
        if on_battlefield && !objects.contains(&entry.object) {
            objects.push(entry.object);
        }
    }
    objects
}

fn unique_recipient_from_filter(
    state: &GameState,
    filter: &TargetFilter,
    ability: &ResolvedAbility,
) -> Result<PlayerId, EffectError> {
    if let TargetFilter::ScopedPlayer = filter {
        return ability
            .scoped_player
            .ok_or_else(|| EffectError::MissingParam("GiveControl scoped recipient".to_string()));
    }

    let source_controller = ability.controller;

    if let TargetFilter::SpecificPlayer { id } = filter {
        return state
            .players
            .iter()
            .find(|p| p.id == *id && !p.is_eliminated)
            .map(|p| p.id)
            .ok_or_else(|| EffectError::MissingParam("GiveControl recipient".to_string()));
    }

    // CR 613.3 + CR 102.1: "the player to your left/right" resolves to a single
    // living seating-neighbor (CR 101.4 / CR 103.1). `game::players::neighbor`
    // already skips eliminated players, so this returns one player and bypasses
    // the generic ambiguity loop below.
    if let TargetFilter::Neighbor { direction } = filter {
        return Ok(crate::game::players::neighbor(
            state,
            source_controller,
            *direction,
        ));
    }

    // CR 603.7c + CR 608.2c: Event-context player anaphors on triggered
    // abilities. `TriggeringPlayer` ("that player") binds to the player involved
    // in the trigger (Coveted Jewel: the attacking opponent);
    // `TriggeringSourceController` ("the attacking player") binds to the
    // controller of the triggering event's source object (Contested Game Ball:
    // the player whose creature dealt combat damage). The stateless player
    // matcher cannot resolve event-context refs, so bind from the active trigger
    // event.
    if matches!(
        filter,
        TargetFilter::TriggeringPlayer | TargetFilter::TriggeringSourceController
    ) {
        return crate::game::targeting::resolve_event_context_target(
            state,
            filter,
            ability.source_id,
        )
        .and_then(|target| match target {
            TargetRef::Player(player_id) => Some(player_id),
            _ => None,
        })
        .ok_or_else(|| EffectError::MissingParam("GiveControl recipient".to_string()));
    }

    let mut matching = state
        .players
        .iter()
        .filter(|p| {
            !p.is_eliminated
                && crate::game::filter::player_matches_target_filter_in_state(
                    state,
                    filter,
                    p.id,
                    Some(source_controller),
                    Some(ability.source_id),
                )
        })
        .map(|p| p.id);

    let Some(recipient) = matching.next() else {
        return Err(EffectError::MissingParam(
            "GiveControl recipient".to_string(),
        ));
    };

    if matching.next().is_some() {
        return Err(EffectError::MissingParam(
            "ambiguous GiveControl recipient".to_string(),
        ));
    }
    Ok(recipient)
}

fn mark_echo_due_for_new_controller(state: &mut GameState, obj_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        if obj.keywords.iter().any(|kw| matches!(kw, Keyword::Echo(_))) {
            obj.echo_due = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Effect, TargetFilter, TargetRef, TypedFilter};
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::zones::Zone;

    fn make_gain_control_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// CR 611.2b + CR 122.1c: Shield Broker — "put a shield counter on target
    /// noncommander creature you don't control. You gain control of that
    /// creature for as long as it has a shield counter on it." The control TCE's
    /// `ForAsLongAs { RecipientHasCounters(shield) }` duration must be evaluated
    /// against the CONTROLLED creature (the recipient), not the source (Shield
    /// Broker has no shield counter). Issue #2855: pre-fix the source-scoped
    /// `HasCounters` check failed immediately, so control never transferred.
    #[test]
    fn shield_broker_etb_places_counter_and_transfers_control() {
        use crate::game::ability_utils::build_resolved_from_def_with_targets;
        use crate::game::effects::resolve_ability_chain;
        use crate::game::layers::evaluate_layers;
        use crate::parser::oracle_trigger::parse_trigger_line;
        use crate::types::card_type::CoreType;
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let broker = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shield Broker".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let o = state.objects.get_mut(&target).unwrap();
            o.card_types.core_types = vec![CoreType::Creature];
            o.base_card_types = o.card_types.clone();
            o.power = Some(2);
            o.toughness = Some(2);
            o.base_power = Some(2);
            o.base_toughness = Some(2);
        }

        let def = parse_trigger_line(
            "When this creature enters, put a shield counter on target noncommander creature you don't control. You gain control of that creature for as long as it has a shield counter on it.",
            "Shield Broker",
        );
        let ability = def.execute.as_ref().expect("execute ability");
        let resolved = build_resolved_from_def_with_targets(
            ability,
            broker,
            PlayerId(0),
            vec![TargetRef::Object(target)],
        );
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).expect("ETB resolves");
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        let obj = &state.objects[&target];
        assert_eq!(
            obj.counters.get(&CounterType::Shield).copied().unwrap_or(0),
            1,
            "a shield counter must be placed on the target"
        );
        assert_eq!(
            obj.controller,
            PlayerId(0),
            "control of the target must transfer to Shield Broker's controller while it has a shield counter"
        );

        state
            .objects
            .get_mut(&target)
            .expect("target remains on battlefield")
            .counters
            .remove(&CounterType::Shield);
        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&target].controller,
            PlayerId(1),
            "control must revert when the recipient no longer has a shield counter"
        );
    }

    /// CR 613.1b: Hellkite Tyrant — "gain control of all artifacts that player
    /// controls". The mass `GainControlAll` enumerates the battlefield, binds
    /// `controller: TargetPlayer` to the effect's player target (the player
    /// dealt combat damage), takes control of EVERY matching artifact, and
    /// leaves non-artifacts (and the controller's own artifacts) untouched.
    #[test]
    fn gain_control_all_takes_every_matching_artifact_of_target_player() {
        use crate::types::ability::{ControllerRef, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);

        // Hellkite Tyrant (the source), controlled by P0.
        let hellkite = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hellkite Tyrant".to_string(),
            Zone::Battlefield,
        );

        // Two artifacts controlled by the target player P1.
        let make_artifact = |state: &mut GameState, cid: u32, name: &str| {
            let id = create_object(
                state,
                CardId(cid.into()),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Artifact);
            id
        };
        let boots = make_artifact(&mut state, 2, "Swiftfoot Boots");
        let banana = make_artifact(&mut state, 3, "Banana");

        // A non-artifact creature P1 controls — must NOT be taken.
        let bear = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        // An artifact P0 already controls — already theirs; control is unchanged.
        let own_artifact = make_artifact(&mut state, 5, "Sol Ring");
        state.objects.get_mut(&own_artifact).unwrap().controller = PlayerId(0);
        state
            .objects
            .get_mut(&own_artifact)
            .unwrap()
            .base_controller = Some(PlayerId(0));

        // "all artifacts that player controls", with the damaged player (P1) as
        // the effect's player target — exactly what the parsed Hellkite trigger
        // resolves to.
        let ability = ResolvedAbility::new(
            Effect::GainControlAll {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Artifact],
                    controller: Some(ControllerRef::TargetPlayer),
                    properties: vec![],
                }),
            },
            vec![TargetRef::Player(PlayerId(1))],
            hellkite,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&boots).unwrap().controller,
            PlayerId(0),
            "P1's artifact (Boots) must transfer to Hellkite's controller",
        );
        assert_eq!(
            state.objects.get(&banana).unwrap().controller,
            PlayerId(0),
            "P1's artifact (Banana) must transfer too — ALL artifacts, not one",
        );
        assert_eq!(
            state.objects.get(&bear).unwrap().controller,
            PlayerId(1),
            "P1's non-artifact creature must NOT be taken",
        );
        assert_eq!(
            state.objects.get(&own_artifact).unwrap().controller,
            PlayerId(0),
            "P0's own artifact stays with P0 (the filter is the target player's)",
        );
    }

    #[test]
    fn gain_control_creates_transient_effect() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_gain_control_ability(target_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Verify a transient continuous effect was created
        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.controller, PlayerId(0));
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: target_id });
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::ChangeController]
        );
        assert!(state.layers_dirty.is_dirty());
    }

    /// CR 613.1b: Non-regression for Bug B (layer fix). After switching the
    /// ChangeController layer arm to trust `effect.controller` instead of
    /// `source.controller`, the standard gain-control flow (where caster is
    /// also source.controller) must still transfer control correctly through
    /// the full layer pipeline.
    #[test]
    fn gain_control_layer_pipeline_transfers_control() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        // Source (the Control Magic aura) is controlled by PlayerId(0) (the caster),
        // matching the real gain-control shape where source.controller == new controller.
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Control Magic".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(0),
            "target should now be controlled by the caster after gain_control"
        );
    }

    /// CR 110.2 + CR 613.1b: End-to-end layer pipeline test for
    /// `resolve_give` (Donate-style "give target permanent to target player").
    /// The recipient differs from both the caster and the source's controller,
    /// so this specifically exercises the post-Bug-B invariant that
    /// `effect.controller` is the single authority. Pre-fix, the layer read
    /// `source.controller` and ignored the resolver's recipient choice,
    /// silently giving the permanent to the caster instead of the recipient.
    #[test]
    fn give_control_layer_pipeline_transfers_to_recipient() {
        let mut state = GameState::new_two_player(42);
        // Target: the permanent to be donated. Initially controlled by the caster.
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gift".to_string(),
            Zone::Battlefield,
        );
        // Source (e.g. Donate on the stack) — controlled by the caster.
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Donate".to_string(),
            Zone::Stack,
        );
        // Recipient is the OPPONENT (PlayerId(1)), distinct from both caster
        // and source.controller. Pre-fix, layer pipeline would read
        // source.controller (= caster) and leave target with caster.
        let recipient = PlayerId(1);
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::Any,
                recipient: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id), TargetRef::Player(recipient)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            recipient,
            "target should now be controlled by the recipient, not the caster or source.controller"
        );
    }

    /// CR 603.7c + CR 608.2c: `GiveControl` with `recipient: TriggeringPlayer`
    /// must resolve from the active trigger event, not the stateless player
    /// matcher (Coveted Jewel — "that player gains control of this artifact").
    #[test]
    fn give_control_triggering_player_recipient_resolves_from_event() {
        use crate::types::events::GameEvent;

        let mut state = GameState::new_two_player(42);
        let jewel = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Coveted Jewel".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Raider".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(0),
            attacks: vec![],
            declaration_records: Vec::new(),
        });
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::TriggeringPlayer,
            },
            vec![],
            jewel,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&jewel).unwrap().controller,
            PlayerId(1),
            "TriggeringPlayer recipient must be the attacking opponent"
        );
    }

    /// CR 608.2d + CR 613.1b: When an untargeted "opponent gains control"
    /// effect has exactly one legal recipient, the resolver may derive that
    /// recipient from game state. This covers two-player Khârn continuations,
    /// whose inherited target list carries only the damaged object.
    #[test]
    fn give_control_derives_single_opponent_recipient() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kharn the Betrayer".to_string(),
            Zone::Battlefield,
        );
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Kharn Trigger".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(1)
        );
    }

    /// CR 601.2c + CR 608.2c: explicit GiveControl object targets are the
    /// selected spell/ability targets. A live trigger event may also make
    /// `ParentTarget` resolvable, but that event context must not override the
    /// chosen target when `ability.targets` already carries one.
    #[test]
    fn give_control_prefers_chosen_object_over_live_event_context() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Blocking Source".to_string(),
            Zone::Battlefield,
        );
        let chosen_target = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Chosen Gift".to_string(),
            Zone::Battlefield,
        );
        let event_context_target = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Blocked Attacker".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::BlockersDeclared {
            assignments: vec![(source, event_context_target)],
        });
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::Any,
            },
            vec![
                TargetRef::Object(chosen_target),
                TargetRef::Player(PlayerId(1)),
            ],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&chosen_target).unwrap().controller,
            PlayerId(1),
            "the explicitly chosen object must be transferred"
        );
        assert_eq!(
            state.objects.get(&event_context_target).unwrap().controller,
            PlayerId(0),
            "live event context must not replace the chosen object target"
        );
    }

    /// CR 608.2d: If several opponents are legal for an untargeted recipient
    /// choice, resolving by iteration order would make a choice the player never
    /// made. The resolver fails closed until a proper resolution-time choice is
    /// available.
    #[test]
    fn give_control_rejects_ambiguous_opponent_recipient() {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kharn the Betrayer".to_string(),
            Zone::Battlefield,
        );
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Kharn Trigger".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve_give(&mut state, &ability, &mut events);

        assert!(matches!(
            result,
            Err(EffectError::MissingParam(message)) if message == "ambiguous GiveControl recipient"
        ));
        assert!(events.is_empty());
    }

    /// CR 102.1 + CR 103.1 + CR 613.3: "The player to your right gains control
    /// of this artifact" (Bucknard's Everfull Purse). Drives the real recipient
    /// path: `resolve_give` → `unique_recipient_from_filter` →
    /// `players::neighbor(Right)` = `previous_player`. In a 3-player game with
    /// seat_order [P0,P1,P2] and controller P0, RIGHT = P2 (previous seat),
    /// distinct from LEFT = P1 (next seat) — so this discriminates the seat
    /// direction AND proves the single-recipient (no-ambiguity) resolution.
    #[test]
    fn give_control_to_player_to_the_right_targets_previous_seat() {
        use crate::types::ability::SeatDirection;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2)]
        );

        // The artifact (Bucknard's) controlled by the activator P0.
        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bucknard's Everfull Purse".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
            },
            vec![TargetRef::Object(artifact)],
            artifact,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&artifact).unwrap().controller,
            PlayerId(2),
            "player to your right = previous seat (P2), not next seat (P1)"
        );
    }

    /// Issue #2915: upkeep "that player gains control" binds `ScopedPlayer` to the
    /// active player; `GiveControl` must read `ability.scoped_player`.
    #[test]
    fn give_control_scoped_player_uses_active_player_binding() {
        let mut state = GameState::new_two_player(42);
        let alexios = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Alexios, Deimos of Kosmos".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::ScopedPlayer,
            },
            vec![TargetRef::Object(alexios)],
            alexios,
            PlayerId(0),
        );
        ability.scoped_player = Some(PlayerId(1));

        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&alexios).unwrap().controller,
            PlayerId(1),
            "ScopedPlayer recipient must resolve to the bound active player"
        );
    }

    /// CR 706.2 + CR 102.1 + CR 103.1 + CR 613.3: End-to-end resolution of
    /// Bucknard's Everfull Purse's activated ability
    /// (`{1}, {T}: Roll a d4 and create a number of Treasure tokens equal to
    /// the result. The player to your right gains control of this artifact.`)
    /// through the REAL chain pipeline. This is the combined-ability test the
    /// two unit tests (token-count parse + 3/4-player Neighbor{Right} recipient)
    /// don't cover individually: it drives `RollDie → Token{count:
    /// EventContextAmount} → GiveControl{recipient: Neighbor{Right}}` through
    /// `resolve_ability_chain` and asserts both effects on the post-resolution
    /// state.
    ///
    /// (a) CR 706.2: exactly N Treasures are created where N is the d4 result
    ///     READ FROM the emitted `GameEvent::DieRolled` (not hard-coded), so the
    ///     `EventContextAmount` count snapshot is proven to flow from the roll.
    /// (b) CR 102.1 + CR 103.1 + CR 613.3: control of the Purse transfers to the
    ///     controller's RIGHT neighbor = previous seat. In [P0,P1,P2] with
    ///     controller P0, RIGHT = P2 (previous seat), distinct from LEFT = P1.
    #[test]
    fn bucknards_everfull_purse_full_chain_rolls_treasures_and_passes_right() {
        use crate::game::players::previous_player;
        use crate::types::ability::{PtValue, QuantityExpr, QuantityRef, SeatDirection};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2)]
        );

        // Bucknard's Everfull Purse, controlled by the activator P0.
        let purse = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bucknard's Everfull Purse".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&purse).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
        }

        // Build the resolved ability EXACTLY as the parser produces it:
        //   RollDie{sides:4}
        //     └─ Token{name:"Treasure", count: EventContextAmount, owner: Controller}
        //          └─ GiveControl{target: SelfRef, recipient: Neighbor{Right}}
        let give_control = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
            },
            vec![TargetRef::Object(purse)],
            purse,
            PlayerId(0),
        );
        let create_treasures = ResolvedAbility::new(
            Effect::Token {
                name: "Treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                // CR 706.2: "a number of Treasure tokens equal to the result"
                // parses to an EventContextAmount count, snapshotted from the
                // preceding RollDie's DieRolled event.
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            purse,
            PlayerId(0),
        )
        .sub_ability(give_control);
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 4,
                results: vec![],
                modifier: None,
            },
            vec![],
            purse,
            PlayerId(0),
        )
        .sub_ability(create_treasures);

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        // (a) CR 706.2: read N from the emitted DieRolled event — never hard-coded.
        let roll = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => result.map(usize::from),
                _ => None,
            })
            .expect("RollDie must emit a DieRolled event");
        assert!((1..=4).contains(&roll), "d4 result out of range: {roll}");

        // Count Treasure tokens controlled by the activator (owner = Controller).
        // After the GiveControl sub-effect the Purse moves to P2, so filtering on
        // the Treasure subtype (not all P0-controlled artifacts) isolates the
        // tokens from the artifact itself.
        let treasure_count = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| {
                o.card_types.subtypes.contains(&"Treasure".to_string())
                    && o.controller == PlayerId(0)
            })
            .count();
        assert_eq!(
            treasure_count, roll,
            "must create exactly N={roll} Treasures (the d4 result), not a hard-coded count",
        );
        // Treasure tokens are colorless artifacts — sanity-check the type line so
        // a future Token-shape regression can't silently pass the count check.
        assert!(
            state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
                .filter(|o| o.card_types.subtypes.contains(&"Treasure".to_string()))
                .all(
                    |o| o.card_types.core_types.contains(&CoreType::Artifact) && o.color.is_empty()
                ),
            "Treasure tokens must be colorless artifacts",
        );

        // (b) CR 102.1 + CR 103.1 + CR 613.3: control passed to the RIGHT
        // neighbor = previous seat = P2 (distinct from LEFT = P1).
        assert_eq!(
            previous_player(&state, PlayerId(0)),
            PlayerId(2),
            "right neighbor of P0 in [P0,P1,P2] is the previous seat P2",
        );
        assert_eq!(
            state.objects.get(&purse).unwrap().controller,
            PlayerId(2),
            "the Purse must transfer to the player on the controller's right (P2), not the left (P1)",
        );
        assert_ne!(
            state.objects.get(&purse).unwrap().controller,
            PlayerId(1),
            "control must NOT go to the LEFT neighbor (next seat P1)",
        );
    }

    /// CR 102.1 + CR 800.4b: When the immediate right-neighbor has left the
    /// game, "the player to your right" skips to the next living seat
    /// counter-clockwise. In a 4-player game [P0,P1,P2,P3] with controller P0,
    /// the immediate right is P3; eliminating P3 routes control to P2.
    #[test]
    fn give_control_to_the_right_skips_eliminated_neighbor() {
        use crate::types::ability::SeatDirection;

        let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2), PlayerId(3)]
        );
        // Eliminate the immediate right neighbor (P3).
        state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(3))
            .unwrap()
            .is_eliminated = true;

        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bucknard's Everfull Purse".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
            },
            vec![TargetRef::Object(artifact)],
            artifact,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&artifact).unwrap().controller,
            PlayerId(2),
            "eliminated right neighbor (P3) is skipped; control passes to P2"
        );
    }

    /// CR 611.2b + CR 110.5d + CR 613.1b: Callous Oppressor regression (issue
    /// #498). A `ForAsLongAs { SourceIsTapped }` gain-control effect must end
    /// when the tapped source leaves the battlefield — an off-battlefield card
    /// is neither tapped nor untapped, so the duration condition becomes false
    /// and the Layer 2 base-controller reset reverts control to the owner.
    ///
    /// Reverted-fix-discriminating: pre-fix the graveyard Oppressor still has
    /// `tapped == true`, `SourceIsTapped` returns `true`, the `ChangeController`
    /// TCE keeps applying, and the final assertion fails.
    #[test]
    fn gain_control_for_as_long_as_tapped_ends_when_source_leaves_battlefield() {
        use crate::types::ability::{Duration, StaticCondition};

        let mut state = GameState::new_two_player(42);

        // The Oppressor: controlled by PlayerId(0), on the battlefield, tapped.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Callous Oppressor".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().tapped = true;

        // The stolen creature: owned/controlled by PlayerId(1).
        let target_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(
            state.objects.get(&target_id).unwrap().base_controller,
            Some(PlayerId(1)),
            "target's base controller should be its owner",
        );

        let mut ability = ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            source,
            PlayerId(0),
        );
        ability.duration = Some(Duration::ForAsLongAs {
            condition: StaticCondition::SourceIsTapped,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(0),
            "control should be gained while the tapped Oppressor is on the battlefield",
        );

        // The Oppressor dies (or is otherwise removed) while still tapped.
        crate::game::zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut events);

        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&target_id).unwrap().controller,
            PlayerId(1),
            "control must revert to the owner once the tapped source leaves the battlefield",
        );
    }

    /// Issue #564: Wishclaw Talisman encodes "an opponent gains control of ~"
    /// as `Choose(Opponent)` → `GainControl { SelfRef }`. The chosen opponent
    /// must receive control, not the activator.
    #[test]
    fn issue_564_gain_control_self_ref_uses_chosen_opponent() {
        let mut state = GameState::new_two_player(42);
        let talisman = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Wishclaw Talisman".to_string(),
            Zone::Battlefield,
        );
        let mut ability = ResolvedAbility::new(
            Effect::GainControl {
                target: TargetFilter::SelfRef,
            },
            vec![],
            talisman,
            PlayerId(0),
        );
        ability.chosen_players = vec![PlayerId(1)];

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&talisman).unwrap().controller,
            PlayerId(1),
            "SelfRef GainControl after Choose(Opponent) must transfer to the chosen opponent"
        );
    }

    #[test]
    fn gain_control_nonexistent_target_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability = make_gain_control_ability(ObjectId(999));
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_err());
    }

    /// CR 603.7c + CR 608.2c (issue #1335): `TriggeringPlayer` on combat-damage
    /// triggers must bind from `DamageDealt`, not only `AttackersDeclared`.
    #[test]
    fn give_control_triggering_player_recipient_resolves_from_damage_dealt_event() {
        use crate::types::events::GameEvent;

        let mut state = GameState::new_two_player(42);
        let kain = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kain, Traitorous Dragoon".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: kain,
            target: TargetRef::Player(PlayerId(1)),
            amount: 2,
            is_combat: true,
            excess: 0,
        });
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::TriggeringPlayer,
            },
            vec![],
            kain,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&kain).unwrap().controller,
            PlayerId(1),
            "TriggeringPlayer on DamageDealt must be the damaged player"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::ControllerChanged { .. })),
            "successful handoff must emit ControllerChanged"
        );
    }

    /// Issue #1335: end-to-end parsed Kain trigger chain through
    /// `resolve_ability_chain` with a combat-damage trigger event.
    #[test]
    fn issue_1335_kain_full_chain_transfers_control_and_riders() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::effects::resolve_ability_chain;
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::card_type::CoreType;
        use crate::types::events::GameEvent;

        const KAIN_ORACLE: &str = "Jump — During your turn, Kain has flying.\n\
Whenever Kain deals combat damage to a player, that player gains control of Kain. \
If they do, you draw that many cards, create that many tapped Treasure tokens, \
then lose that much life.";

        let parsed = parse_oracle_text(
            KAIN_ORACLE,
            "Kain, Traitorous Dragoon",
            &[],
            &["Creature".to_string()],
            &["Human".to_string(), "Knight".to_string()],
        );
        let execute = parsed.triggers[0]
            .execute
            .as_ref()
            .expect("combat damage trigger");

        let mut state = GameState::new_two_player(42);
        for (idx, name) in ["Card A", "Card B", "Card C"].into_iter().enumerate() {
            create_object(
                &mut state,
                CardId((idx + 10) as u64),
                PlayerId(0),
                name.to_string(),
                Zone::Library,
            );
        }
        let kain = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Kain, Traitorous Dragoon".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: kain,
            target: TargetRef::Player(PlayerId(1)),
            amount: 2,
            is_combat: true,
            excess: 0,
        });

        let resolved = build_resolved_from_def(execute, kain, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).expect("Kain chain");
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(state.objects[&kain].controller, PlayerId(1));
        assert_eq!(state.players[0].hand.len(), 2);
        assert_eq!(state.players[0].life, 18);

        let treasures = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| {
                obj.controller == PlayerId(0)
                    && obj.card_types.subtypes.iter().any(|st| st == "Treasure")
                    && obj.card_types.core_types.contains(&CoreType::Artifact)
            })
            .count();
        assert_eq!(treasures, 2, "attacker receives two tapped Treasures");
        assert!(
            state
                .battlefield
                .iter()
                .filter_map(|id| state.objects.get(id))
                .filter(|obj| obj.card_types.subtypes.iter().any(|st| st == "Treasure"))
                .all(|obj| obj.tapped),
            "Kain's Treasures must enter tapped"
        );
    }

    /// Issue #1987: chained GiveControl with `target: SelfRef` and empty
    /// `ability.targets` must still transfer the source permanent.
    #[test]
    fn issue_1987_give_control_self_ref_with_empty_targets_transfers_source() {
        use crate::types::ability::SeatDirection;

        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        let artifact = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bucknard's Everfull Purse".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::Neighbor {
                    direction: SeatDirection::Right,
                },
            },
            vec![],
            artifact,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_give(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&artifact).unwrap().controller,
            PlayerId(2),
            "SelfRef GiveControl must transfer the source with empty targets"
        );
    }

    /// Issue #1987: parsed activated ability must chain
    /// RollDie → Token(EventContextAmount) → GiveControl(Neighbor Right).
    #[test]
    fn issue_1987_bucknards_parsed_ability_chain_shape() {
        use crate::parser::parse_oracle_text;
        use crate::types::ability::{QuantityExpr, QuantityRef, SeatDirection};

        let parsed = parse_oracle_text(
            "{1}, {T}: Roll a d4 and create a number of Treasure tokens equal to the result. The player to your right gains control of this artifact.",
            "Bucknard's Everfull Purse",
            &[],
            &["Artifact".to_string()],
            &[],
        );
        assert_eq!(parsed.abilities.len(), 1, "parsed: {parsed:#?}");
        let ability = &parsed.abilities[0];

        let Effect::RollDie { sides, .. } = ability.effect.as_ref() else {
            panic!("head must be RollDie, got {:?}", ability.effect);
        };
        assert_eq!(*sides, 4);

        let token_ability = ability.sub_ability.as_ref().expect("RollDie sub = Token");
        let Effect::Token { count, .. } = token_ability.effect.as_ref() else {
            panic!("RollDie sub must be Token, got {:?}", token_ability.effect);
        };
        assert_eq!(
            count,
            &QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            }
        );

        let give_ability = token_ability
            .sub_ability
            .as_ref()
            .expect("Token sub = GiveControl");
        let Effect::GiveControl { target, recipient } = give_ability.effect.as_ref() else {
            panic!(
                "Token sub must be GiveControl, got {:?}",
                give_ability.effect
            );
        };
        assert_eq!(target, &TargetFilter::SelfRef);
        assert_eq!(
            recipient,
            &TargetFilter::Neighbor {
                direction: SeatDirection::Right
            }
        );
    }

    // ── The per-object handoff: `GiveControl` over this ability's own
    // object→player assignment ("Each opponent gains control of the permanent
    // for which they were chosen"). The assignment is built through Phase 3a's
    // record type, not through prompts. ──

    const P1: PlayerId = PlayerId(1);
    const P2: PlayerId = PlayerId(2);
    const P3: PlayerId = PlayerId(3);

    /// Four players. P0 controls the battlefield objects `a`, `b` and `c`;
    /// `g` is in P0's graveyard; `source` is the resolving spell.
    struct HandoffBoard {
        state: GameState,
        a: ObjectId,
        b: ObjectId,
        c: ObjectId,
        g: ObjectId,
        source: ObjectId,
    }

    fn handoff_board() -> HandoffBoard {
        let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
        let mut add = |id: u64, name: &str, zone: Zone| {
            create_object(&mut state, CardId(id), PlayerId(0), name.to_string(), zone)
        };
        let a = add(1, "Object A", Zone::Battlefield);
        let b = add(2, "Object B", Zone::Battlefield);
        let c = add(3, "Object C", Zone::Battlefield);
        let g = add(4, "Object G", Zone::Graveyard);
        let source = add(5, "Handoff Spell", Zone::Stack);
        HandoffBoard {
            state,
            a,
            b,
            c,
            g,
            source,
        }
    }

    fn entry(object: ObjectId, player: PlayerId) -> ObjectPlayerAssignment {
        ObjectPlayerAssignment { object, player }
    }

    fn assignment(side: ChoiceAssignmentSide) -> TargetFilter {
        TargetFilter::ChoiceAssignment { side }
    }

    /// The handoff Phase 3c lowers to: the assignment's objects, each to the
    /// player chosen for it.
    fn handoff_effect() -> Effect {
        Effect::GiveControl {
            target: assignment(ChoiceAssignmentSide::Objects),
            recipient: assignment(ChoiceAssignmentSide::PlayerForObject),
        }
    }

    fn with_assignment(
        effect: Effect,
        targets: Vec<TargetRef>,
        source: ObjectId,
        entries: Vec<ObjectPlayerAssignment>,
    ) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(effect, targets, source, PlayerId(0));
        ability.context.object_player_assignment = entries;
        ability
    }

    fn handoff_ability(
        source: ObjectId,
        entries: Vec<ObjectPlayerAssignment>,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        with_assignment(handoff_effect(), targets, source, entries)
    }

    /// `(object, old, new)` of every control-change event.
    fn control_changes(events: &[GameEvent]) -> Vec<(ObjectId, PlayerId, PlayerId)> {
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ControllerChanged {
                    object_id,
                    old_controller,
                    new_controller,
                } => Some((*object_id, *old_controller, *new_controller)),
                _ => None,
            })
            .collect()
    }

    fn give_control_resolutions(events: &[GameEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::GiveControl,
                        ..
                    }
                )
            })
            .count()
    }

    /// `(controller, duration)` of every control effect affecting `object`.
    fn control_effects_on(state: &GameState, object: ObjectId) -> Vec<(PlayerId, Duration)> {
        state
            .transient_continuous_effects
            .iter()
            .filter(|tce| tce.affected == TargetFilter::SpecificObject { id: object })
            .map(|tce| {
                assert_eq!(
                    tce.modifications,
                    vec![ContinuousModification::ChangeController]
                );
                (tce.controller, tce.duration.clone())
            })
            .collect()
    }

    fn controller_of(state: &mut GameState, object: ObjectId) -> PlayerId {
        crate::game::layers::evaluate_layers(state);
        state.objects[&object].controller
    }

    /// V3b.1 (A3.6 handoff leg): one application gives each assigned object to
    /// exactly its assigned player through layer 2 — the control changes are
    /// processed together (CR 608.2e + CR 608.2f), last indefinitely (CR 611.2a)
    /// and apply in layer 2 (CR 613.1b) — and every control-change event comes
    /// from that one application.
    #[test]
    fn per_object_handoff_gives_each_object_to_its_assigned_player() {
        let HandoffBoard {
            mut state,
            a,
            b,
            c,
            source,
            ..
        } = handoff_board();
        let ability = handoff_ability(source, vec![entry(a, P1), entry(b, P2)], vec![]);
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).expect("the handoff resolves");

        assert_eq!(
            state.transient_continuous_effects.len(),
            2,
            "one control effect per assigned object"
        );
        assert_eq!(control_effects_on(&state, a), [(P1, Duration::Permanent)]);
        assert_eq!(control_effects_on(&state, b), [(P2, Duration::Permanent)]);
        assert_eq!(
            control_changes(&events),
            [(a, PlayerId(0), P1), (b, PlayerId(0), P2)]
        );
        assert_eq!(give_control_resolutions(&events), 1, "one application");
        assert_eq!(controller_of(&mut state, a), P1);
        assert_eq!(controller_of(&mut state, b), P2);
        assert_eq!(
            controller_of(&mut state, c),
            PlayerId(0),
            "an object with no entry is untouched"
        );
    }

    /// V3b.1(b) (labelled guard for the class): the reference does not assume
    /// distinct players — two objects assigned to one player both go to them.
    #[test]
    fn per_object_handoff_admits_one_player_for_several_objects() {
        let HandoffBoard {
            mut state,
            a,
            b,
            source,
            ..
        } = handoff_board();
        let ability = handoff_ability(source, vec![entry(a, P1), entry(b, P1)], vec![]);
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).expect("the handoff resolves");

        assert_eq!(state.transient_continuous_effects.len(), 2);
        assert_eq!(
            control_changes(&events),
            [(a, PlayerId(0), P1), (b, PlayerId(0), P1)]
        );
        assert_eq!(controller_of(&mut state, a), P1);
        assert_eq!(controller_of(&mut state, b), P1);
    }

    /// V3b.1(c) (CR 110.2; existing rule): an entry naming the object's current
    /// controller installs its control effect but emits no control-change event.
    #[test]
    fn per_object_handoff_to_the_current_controller_emits_no_event() {
        let HandoffBoard {
            mut state,
            a,
            source,
            ..
        } = handoff_board();
        let ability = handoff_ability(source, vec![entry(a, PlayerId(0))], vec![]);
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).expect("the handoff resolves");

        assert_eq!(
            control_effects_on(&state, a),
            [(PlayerId(0), Duration::Permanent)]
        );
        assert!(control_changes(&events).is_empty());
        assert_eq!(give_control_resolutions(&events), 1);
    }

    /// V3b.2 (A3.6 (ii), C3.11; revert-failing — the single-recipient form
    /// refuses the whole effect when its recipient has left the game): a player
    /// who has left the game gets nothing for their object and no effect is
    /// installed for it, while the other entries still apply (CR 800.4b +
    /// CR 609.3). The refusal does not depend on the entry's position.
    #[test]
    fn per_object_handoff_skips_only_a_departed_players_object() {
        for departed_first in [false, true] {
            let HandoffBoard {
                mut state,
                a,
                b,
                source,
                ..
            } = handoff_board();
            state.players[2].is_eliminated = true;
            let entries = if departed_first {
                vec![entry(b, P2), entry(a, P1)]
            } else {
                vec![entry(a, P1), entry(b, P2)]
            };
            let ability = handoff_ability(source, entries, vec![]);
            let mut events = Vec::new();
            resolve_give(&mut state, &ability, &mut events)
                .expect("a departed player's entry does not refuse the effect");

            assert_eq!(
                control_effects_on(&state, a),
                [(P1, Duration::Permanent)],
                "reach-guard: the other entry applied (departed first: {departed_first})"
            );
            assert!(
                control_effects_on(&state, b).is_empty(),
                "CR 800.4b: no effect is installed for the departed player's object"
            );
            assert_eq!(
                control_changes(&events),
                [(a, PlayerId(0), P1)],
                "no control-change event for the departed player's object"
            );
            assert_eq!(controller_of(&mut state, a), P1);
            assert_eq!(controller_of(&mut state, b), PlayerId(0));
        }
    }

    /// V3b.3 (A3.6 (iii)): an empty assignment hands nothing.
    #[test]
    fn per_object_handoff_with_an_empty_assignment_hands_nothing() {
        let HandoffBoard {
            mut state,
            a,
            b,
            source,
            ..
        } = handoff_board();
        let ability = handoff_ability(source, vec![], vec![]);
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).expect("the handoff resolves");

        assert_eq!(
            give_control_resolutions(&events),
            1,
            "reach-guard: the application ran to completion"
        );
        assert!(state.transient_continuous_effects.is_empty());
        assert!(control_changes(&events).is_empty());
        assert_eq!(controller_of(&mut state, a), PlayerId(0));
        assert_eq!(controller_of(&mut state, b), PlayerId(0));
    }

    /// V3b.4 (multi-authority): a stray player in `ability.targets` does not
    /// override the per-object recipient; every other recipient form keeps the
    /// declared player target's precedence (Donate form, CR 115.1 + CR 608.2c).
    #[test]
    fn per_object_recipient_is_not_overridden_by_a_player_target() {
        let HandoffBoard {
            mut state,
            a,
            b,
            source,
            ..
        } = handoff_board();
        let ability = handoff_ability(
            source,
            vec![entry(a, P1), entry(b, P2)],
            vec![TargetRef::Player(P3)],
        );
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).expect("the handoff resolves");

        assert_eq!(state.transient_continuous_effects.len(), 2);
        assert!(
            state
                .transient_continuous_effects
                .iter()
                .all(|tce| tce.controller != P3),
            "the stray player target receives nothing"
        );
        assert_eq!(controller_of(&mut state, a), P1);
        assert_eq!(controller_of(&mut state, b), P2);

        // Single-form sibling: the declared player target is the recipient.
        let HandoffBoard {
            mut state,
            a,
            source,
            ..
        } = handoff_board();
        let donate = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::Any,
                recipient: TargetFilter::Any,
            },
            vec![TargetRef::Object(a), TargetRef::Player(P3)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_give(&mut state, &donate, &mut events).expect("Donate form resolves");
        assert_eq!(controller_of(&mut state, a), P3);
    }

    /// V3b.5: the objects projection names only assigned objects still on the
    /// battlefield (CR 400.7) — the single authority `resolve_give` hands and
    /// `affected_objects_from_events` publishes (CR 611.2c).
    #[test]
    fn per_object_handoff_hands_only_assigned_battlefield_objects() {
        let HandoffBoard {
            mut state,
            a,
            c,
            g,
            source,
            ..
        } = handoff_board();
        let ability = handoff_ability(source, vec![entry(a, P1), entry(g, P3)], vec![]);
        assert_eq!(
            give_control_object_targets(
                &state,
                &ability,
                &assignment(ChoiceAssignmentSide::Objects)
            ),
            [a],
            "the published set is the assigned battlefield objects"
        );
        let mut events = Vec::new();
        resolve_give(&mut state, &ability, &mut events).expect("the handoff resolves");

        assert_eq!(
            control_effects_on(&state, a),
            [(P1, Duration::Permanent)],
            "reach-guard: the battlefield entry is handed"
        );
        assert!(
            control_effects_on(&state, g).is_empty(),
            "CR 400.7: an assigned object that left the battlefield is not handed"
        );
        assert!(
            control_effects_on(&state, c).is_empty(),
            "an unassigned battlefield object is untouched"
        );
        assert_eq!(control_changes(&events), [(a, PlayerId(0), P1)]);
    }

    /// V3b.6: a projection used on the wrong side fails closed, and a declared
    /// object with no entry gets no recipient (CR 609.3).
    #[test]
    fn misused_projections_fail_closed() {
        // A player projection names no object.
        let HandoffBoard {
            mut state,
            a,
            b,
            source,
            ..
        } = handoff_board();
        let both_players = with_assignment(
            Effect::GiveControl {
                target: assignment(ChoiceAssignmentSide::PlayerForObject),
                recipient: assignment(ChoiceAssignmentSide::PlayerForObject),
            },
            vec![],
            source,
            vec![entry(a, P1), entry(b, P2)],
        );
        let mut events = Vec::new();
        assert!(resolve_give(&mut state, &both_players, &mut events).is_ok());
        assert!(state.transient_continuous_effects.is_empty());

        // The objects projection names no player.
        let HandoffBoard {
            mut state,
            a,
            b,
            source,
            ..
        } = handoff_board();
        let both_objects = with_assignment(
            Effect::GiveControl {
                target: assignment(ChoiceAssignmentSide::Objects),
                recipient: assignment(ChoiceAssignmentSide::Objects),
            },
            vec![],
            source,
            vec![entry(a, P1), entry(b, P2)],
        );
        let mut events = Vec::new();
        assert!(matches!(
            resolve_give(&mut state, &both_objects, &mut events),
            Err(EffectError::MissingParam(_))
        ));
        assert!(state.transient_continuous_effects.is_empty());

        // A declared object with no entry: no player was chosen for it.
        let HandoffBoard {
            mut state,
            a,
            b,
            source,
            ..
        } = handoff_board();
        let declared = with_assignment(
            Effect::GiveControl {
                target: TargetFilter::Any,
                recipient: assignment(ChoiceAssignmentSide::PlayerForObject),
            },
            vec![TargetRef::Object(a), TargetRef::Object(b)],
            source,
            vec![entry(a, P1)],
        );
        let mut events = Vec::new();
        resolve_give(&mut state, &declared, &mut events).expect("the handoff resolves");
        assert_eq!(
            control_effects_on(&state, a),
            [(P1, Duration::Permanent)],
            "reach-guard: the entry's object is handed"
        );
        assert!(control_effects_on(&state, b).is_empty());
        assert_eq!(control_changes(&events), [(a, PlayerId(0), P1)]);
        assert_eq!(controller_of(&mut state, b), PlayerId(0));
    }

    /// V3b.7 (§3 classification pins): the reference is never a target
    /// (CR 115.10a), is member-bound on the fail-closed side (CR 603.10a), and
    /// reads no event, sibling or projected state.
    #[test]
    fn choice_assignment_classification() {
        use crate::game::ability_rw::ability_rw_profile;
        use crate::game::ability_scan::{
            ability_reads_projected_resource, ability_reads_sibling_mutable,
            ability_uses_event_context,
        };
        use crate::types::identifiers::TrackedSetId;

        let destroy = |target: TargetFilter| {
            ResolvedAbility::new(
                Effect::Destroy {
                    target,
                    cant_regenerate: false,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            )
        };
        // Positive and negative controls: the instrument fires and is not
        // constant.
        assert!(ability_rw_profile(&destroy(TargetFilter::TrackedSet {
            id: TrackedSetId(0)
        }))
        .reads_member_bound());
        assert!(!ability_rw_profile(&destroy(TargetFilter::SelfRef)).reads_member_bound());
        for side in [
            ChoiceAssignmentSide::Objects,
            ChoiceAssignmentSide::PlayerForObject,
        ] {
            assert!(
                ability_rw_profile(&destroy(assignment(side))).reads_member_bound(),
                "{side:?}: fail-closed member-bound"
            );
            assert!(
                assignment(side).is_context_ref(),
                "{side:?}: never a target"
            );
        }

        let event_reader = ResolvedAbility::new(
            Effect::GiveControl {
                target: TargetFilter::SelfRef,
                recipient: TargetFilter::TriggeringPlayer,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        assert!(
            ability_uses_event_context(&event_reader),
            "positive control: the event axis fires"
        );

        let HandoffBoard { a, b, source, .. } = handoff_board();
        let handoff = handoff_ability(source, vec![entry(a, P1), entry(b, P2)], vec![]);
        assert!(!ability_uses_event_context(&handoff));
        assert!(!ability_reads_projected_resource(&handoff));
        assert!(!ability_reads_sibling_mutable(&handoff));
        let profile = ability_rw_profile(&handoff);
        assert!(!profile.reads_and_writes_event_object());
        // Recorded, not asserted: decided by `rw_effect`'s pre-existing control
        // arm, which consults neither filter.
        println!(
            "V3b.7 RECORD handoff reads_member_bound = {}",
            profile.reads_member_bound()
        );
    }
}
