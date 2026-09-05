use crate::game::targeting;
use crate::types::ability::Duration;
use crate::types::ability::{
    ContinuousModification, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.12a: Exchange control of two permanents, or a permanent and a spell
/// (CR 701.12a + CR 400.7a — see `control_is_exchangeable` below).
///
/// Object resolution for each slot:
/// - A context-ref filter (`SelfRef` — "this artifact and target …", Avarice
///   Totem / Eyes Everywhere / Phyrexian Infiltrator; `TriggeringSource` —
///   "that spell", Perplexing Chimera) → resolved through the single 4-tier
///   authority `targeting::resolved_targets`.
/// - Any other filter → consumed in order from `ability.targets`.
///
/// CR 701.12a: If the entire exchange can't be completed (missing object,
/// off-battlefield/off-stack), no part of the exchange occurs (all-or-nothing).
/// CR 701.12b: If both permanents are controlled by the same player, the
/// exchange effect does nothing.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ExchangeControl { target_a, target_b } = &ability.effect else {
        // Should not be reached: dispatcher in effects/mod.rs only routes
        // ExchangeControl variants here.
        return Ok(());
    };

    // Diagnostic: both slot filters being `Any` indicates either an
    // old-format `card-data.json` row that deserialised via the serde default,
    // or a parser gap. A bare `Any/Any` slot set plus a slot-less
    // `ability.targets` produces a silent no-op — flag it so regressions are
    // visible in logs rather than disappearing into the CR 701.12a
    // all-or-nothing branch.
    if matches!(target_a, TargetFilter::Any) && matches!(target_b, TargetFilter::Any) {
        tracing::warn!(
            source_id = ?ability.source_id,
            "ExchangeControl resolved with both target filters = Any — check for a parser gap"
        );
    }

    // Each non-context-ref slot consumes one TargetRef::Object from
    // ability.targets, in declaration order. Context-ref slots (SelfRef,
    // TriggeringSource) are resolved through `targeting::resolved_targets`.
    let mut object_targets = ability.targets.iter().filter_map(|t| match t {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    });
    // CR 608.2k + CR 608.2c: a context-ref slot surfaces no target and is bound at
    // resolution time by the single 4-tier authority `targeting::resolved_targets` —
    // its tier-1 short-circuit owns the resolution-local anaphors (`SelfRef`, and
    // with it the CR 400.7 `self_ref_is_current` check), and its pure-event-context
    // tier owns `TriggeringSource` AHEAD of the `ability.targets` tier, so per-slot
    // index discipline survives a mixed declared/context-ref pair. It delegates the
    // event tier to `targeting::resolve_event_context_target`; there is no second
    // resolver here.
    //
    // SCOPE OF THAT GUARANTEE: it holds for the filters `resolved_targets`
    // owns a tier for — `SelfRef`, `SourceOrPaired`, `CostPaidObject`,
    // `AmassedArmy`, `ParentTarget{,Slot}`, and the
    // `is_pure_event_context_filter` group (which covers `TriggeringSource`).
    // Of those, `SelfRef` and `TriggeringSource` are the only context refs the
    // corpus produces in an `ExchangeControl` slot. `is_context_ref()` admits
    // more than that, and any filter WITHOUT a tier falls through to
    // `resolved_targets`' terminal `ability.targets.clone()` — so it would
    // return the sibling slot's declared target and both slots would resolve
    // to the same object (CR 701.12b no-op). That is a latent shape, not a
    // reachable one; see the matching note in `ability_utils.rs`'s slot
    // builder. Adding a new context-ref filter to an ExchangeControl parse
    // means giving it a tier in `resolved_targets` first.
    // NOTE: `resolve_event_context_target` must NOT be called directly — it has no
    // `SelfRef` arm, so it would silently break the Avarice Totem / Eyes Everywhere /
    // Phyrexian Infiltrator class.
    let resolve_slot = |filter: &TargetFilter, iter: &mut dyn Iterator<Item = ObjectId>| {
        if !filter.is_context_ref() {
            return iter.next();
        }
        targeting::resolved_targets(ability, filter, state)
            .into_iter()
            .find_map(|t| match t {
                TargetRef::Object(id) => Some(id),
                // CR 701.12a: a player-valued ref cannot be an exchange subject —
                // the exchange can't be completed, so no part of it occurs.
                TargetRef::Player(_) => None,
            })
    };

    let Some(id_a) = resolve_slot(target_a, &mut object_targets) else {
        // CR 701.12a: Can't complete exchange — do nothing.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ExchangeControl,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };
    let Some(id_b) = resolve_slot(target_b, &mut object_targets) else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ExchangeControl,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    };

    // CR 701.12a + CR 400.7a: control of an object can be exchanged wherever
    // control is a meaningful characteristic — the battlefield (CR 110.2) and
    // the stack (CR 112.2, CR 109.4: "Only objects on the stack or on the
    // battlefield have a controller"). A SPELL subject is legal precisely
    // because CR 400.7a carries the control change through onto the permanent
    // that spell becomes, and CR 110.2b assigns that permanent's by-default
    // controller to the player who put the spell onto the stack. Any other zone
    // (an object that has already left the stack — countered in response)
    // cannot complete the exchange, so per CR 701.12a no part of it occurs.
    fn control_is_exchangeable(zone: Zone) -> bool {
        matches!(zone, Zone::Battlefield | Zone::Stack)
    }

    // CR 701.12a: Both objects must exist and be in an exchangeable zone. The
    // controller read below is what makes this depend on the stack seed
    // (`layers::evaluate_layers`'s CR 112.2 base + CR 613.1b re-derivation): for
    // a stack subject, `obj.controller` is origin-zone data before that seed and
    // the live, re-derived controller after.
    let (controller_a, controller_b) = {
        let Some(obj_a) = state.objects.get(&id_a) else {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ExchangeControl,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        };
        let Some(obj_b) = state.objects.get(&id_b) else {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ExchangeControl,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        };
        if !control_is_exchangeable(obj_a.zone) || !control_is_exchangeable(obj_b.zone) {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ExchangeControl,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
        (obj_a.controller, obj_b.controller)
    };

    // CR 701.12b: Same controller → no effect. CR 701.12b is written for two
    // PERMANENTS; the permanent-and-spell case rests on CR 701.12a's general
    // all-or-nothing principle plus CR 701.12b's same-controller principle —
    // there is no separate rule for a spell whose live controller already
    // matches the permanent's.
    if controller_a == controller_b {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::ExchangeControl,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 701.12a: Bidirectional control exchange via two transient continuous effects.
    // Object A gets controller_b, object B gets controller_a. Duration honours
    // the resolved ability (e.g. "until end of turn") with `Permanent` as the
    // default — mirrors `gain_control::resolve`.
    let duration = ability.duration.clone().unwrap_or(Duration::Permanent);
    state.add_transient_continuous_effect(
        ability.source_id,
        controller_b,
        duration.clone(),
        TargetFilter::SpecificObject { id: id_a },
        vec![ContinuousModification::ChangeController],
        None,
    );
    state.add_transient_continuous_effect(
        ability.source_id,
        controller_a,
        duration,
        TargetFilter::SpecificObject { id: id_b },
        vec![ContinuousModification::ChangeController],
        None,
    );

    // CR 613.1b + CR 603.2: publish the control change so the event exists for the
    // rules that key off it. Every OTHER Layer-2 control-change path already does
    // (`gain_control::resolve`, `::resolve_all`, `::resolve_give`,
    // `apply_permanent_control_change`, and the until-EOT control reversion in
    // `turns.rs`) — `match_changes_controller`'s doc enumerates that set as
    // complete, and this resolver was the missing member.
    //
    // Emitted on the SUCCESS path only, which is what makes it the authoritative
    // "the exchange happened" witness for the CR 608.2c "if you do" / "if you
    // don't or can't" riders on Perplexing Chimera, Gilded Drake, Volatile
    // Stormdrake and Arteeoh. The SIX REACHABLE no-op returns above each emit
    // `EffectResolved` and no `ControllerChanged`, so the two are distinguishable.
    // (A seventh `return Ok(())` guards the `Effect::ExchangeControl` destructure
    // at the top of this function; it emits nothing and is unreachable — the only
    // caller is `resolve_effect`'s `Effect::ExchangeControl` arm, which matches the
    // same variant.)
    //
    // The `controller_a != controller_b` inequality is guaranteed HERE BY CODE —
    // the early return above at the `controller_a == controller_b` check — not by
    // a rule. CR 701.12b is why that return exists ("if those permanents are
    // controlled by the same player, the exchange effect does nothing"), and the
    // return is what makes neither event a no-op self-handoff; unlike the sibling
    // resolvers, no `old != new` guard is needed at this point.
    //
    // CR 109.4: a stack subject legitimately has a controller, so a spell half
    // emits too. It is inert to `match_changes_controller` NOT primarily because
    // of `valid_card_matches`: when the exchanged spell is itself the tracked
    // object (Perplexing Chimera exchanging control with a cast Khârn the
    // Betrayer), `valid_card: SelfRef` MATCHES the spell, and the
    // `source_id == *object_id` branch in `match_changes_controller`
    // (trigger_matchers.rs) short-circuits straight to `true` — `valid_card`
    // does not gate that case.
    //
    // The guard that actually holds is CR 113.6 ("Abilities of all other
    // objects usually function only while that object is on the battlefield"):
    // the collection loop's zone gate (triggers.rs, keyed on `trigger_zones`)
    // enforces it, and every printed `ChangesController` producer (Khârn the
    // Betrayer, Duplicity, Gustha's Scepter, Stolen Uniform) declares
    // `trigger_zones: ["Battlefield"]`, so none of them ever scan this
    // spell-half event on the stack. `valid_card` remains a real, secondary
    // scope for the in-zone case — it is just not what protects the
    // self-tracked-spell case above.
    //
    // WARNING for a future `ChangesController` producer written to function
    // from the stack (an explicit non-battlefield `trigger_zones` per CR
    // 113.6b): neither guard here protects it. Check any such trigger by hand
    // against this spell-half emission before shipping it.
    events.push(GameEvent::ControllerChanged {
        object_id: id_a,
        old_controller: controller_a,
        new_controller: controller_b,
    });
    events.push(GameEvent::ControllerChanged {
        object_id: id_b,
        old_controller: controller_b,
        new_controller: controller_a,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExchangeControl,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_exchange_ability(target_a: ObjectId, target_b: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ExchangeControl {
                target_a: TargetFilter::Any,
                target_b: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_a), TargetRef::Object(target_b)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// CR 613.1b: the directed control handoffs this resolution published.
    fn controller_changes(events: &[GameEvent]) -> Vec<(ObjectId, PlayerId, PlayerId)> {
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

    /// Every no-op return still reports the effect as resolved (CR 608.2c) —
    /// what distinguishes them from success is the ABSENCE of
    /// `ControllerChanged`, which is exactly what
    /// `mandatory_parent_effect_performed`'s `Effect::ExchangeControl` arm reads.
    fn exchange_resolved_count(events: &[GameEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::ExchangeControl,
                        ..
                    }
                )
            })
            .count()
    }

    /// Asserts the shared shape of every REACHABLE no-op return: the effect
    /// resolved exactly once, published no control change, and installed no
    /// Layer-2 effect.
    ///
    /// There are SIX such returns (CR 701.12a slot A unresolvable, slot B
    /// unresolvable, object A missing, object B missing, a subject in a zone
    /// where control is not a characteristic; CR 701.12b same controller), and
    /// this module covers one row per branch. A SEVENTH `return Ok(())` guards
    /// the `Effect::ExchangeControl` destructure at the top of `resolve`; it is
    /// deliberately uncovered because it is unreachable by dispatcher contract —
    /// `resolve_effect`'s `Effect::ExchangeControl` arm is its only caller and
    /// matches the same variant — and it pushes no event at all, so it is not a
    /// member of the "every no-op return emits `EffectResolved`" claim.
    fn assert_noop(state: &GameState, events: &[GameEvent]) {
        assert!(
            state.transient_continuous_effects.is_empty(),
            "a no-op exchange installs no Layer-2 control effect"
        );
        assert_eq!(
            exchange_resolved_count(events),
            1,
            "a no-op exchange still reports EffectResolved (CR 608.2c)"
        );
        assert!(
            controller_changes(events).is_empty(),
            "a no-op exchange must publish NO ControllerChanged — that absence is the \
             signal `mandatory_parent_effect_performed` reads (events were {events:?})"
        );
    }

    #[test]
    fn exchange_control_swaps_controllers() {
        let mut state = GameState::new_two_player(42);
        let obj_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );

        let ability = make_exchange_ability(obj_a, obj_b);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should create two transient continuous effects (bidirectional ChangeController)
        assert_eq!(state.transient_continuous_effects.len(), 2);

        // First effect: Object A gets controller_b (PlayerId(1))
        let tce_a = state
            .transient_continuous_effects
            .iter()
            .find(|e| e.affected == TargetFilter::SpecificObject { id: obj_a })
            .expect("Should have effect for obj_a");
        assert_eq!(tce_a.controller, PlayerId(1));
        assert_eq!(
            tce_a.modifications,
            vec![ContinuousModification::ChangeController]
        );

        // Second effect: Object B gets controller_a (PlayerId(0))
        let tce_b = state
            .transient_continuous_effects
            .iter()
            .find(|e| e.affected == TargetFilter::SpecificObject { id: obj_b })
            .expect("Should have effect for obj_b");
        assert_eq!(tce_b.controller, PlayerId(0));

        // CR 613.1b + CR 603.2: the success path publishes exactly two DIRECTED
        // control handoffs. Asserting the directions (not just the count) is what
        // catches a both-to-one regression — the same class of bug the layer
        // pipeline row below pins on the state side.
        assert_eq!(
            controller_changes(&events),
            vec![
                (obj_a, PlayerId(0), PlayerId(1)),
                (obj_b, PlayerId(1), PlayerId(0)),
            ],
            "each subject hands off to the OTHER subject's controller (events were {events:?})"
        );
        assert_eq!(exchange_resolved_count(&events), 1);
    }

    #[test]
    fn exchange_control_same_controller_is_noop() {
        let mut state = GameState::new_two_player(42);
        let obj_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Wolf".to_string(),
            Zone::Battlefield,
        );

        let ability = make_exchange_ability(obj_a, obj_b);
        let mut events = Vec::new();

        // CR 701.12b: Same controller → do nothing.
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            state.transient_continuous_effects.is_empty(),
            "Should create no transient effects for same-controller exchange"
        );
        assert_noop(&state, &events);
    }

    #[test]
    fn exchange_control_missing_target_is_noop() {
        let mut state = GameState::new_two_player(42);
        let obj_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // CR 701.12a: One target missing → all-or-nothing, do nothing.
        // This is the `state.objects.get(&id_b)` branch.
        let ability = make_exchange_ability(obj_a, ObjectId(999));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.transient_continuous_effects.is_empty());
        assert_noop(&state, &events);
    }

    /// CR 701.12a: the mirror branch — `state.objects.get(&id_a)` is the one
    /// that misses. Covered separately so a regression that reorders the two
    /// existence checks cannot hide behind the other row.
    #[test]
    fn exchange_control_missing_first_target_is_noop() {
        let mut state = GameState::new_two_player(42);
        let obj_b = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );

        let ability = make_exchange_ability(ObjectId(999), obj_b);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        assert_noop(&state, &events);
    }

    /// CR 701.12a + CR 109.4: a subject that is neither on the stack nor on the
    /// battlefield has no controller, so the exchange can't be completed and no
    /// part of it occurs. This is the `control_is_exchangeable` branch — the one
    /// no-op return no other row in this module reaches.
    #[test]
    fn exchange_control_unexchangeable_zone_is_noop() {
        let mut state = GameState::new_two_player(42);
        let obj_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Graveyard,
        );

        // REACH GUARD: the two subjects have DIFFERENT controllers, so this row
        // is stopped by the zone gate and not by CR 701.12b's same-controller
        // return further down.
        assert_ne!(
            state.objects.get(&obj_a).unwrap().controller,
            state.objects.get(&obj_b).unwrap().controller
        );

        let ability = make_exchange_ability(obj_a, obj_b);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        assert_noop(&state, &events);
    }

    #[test]
    fn exchange_control_fewer_than_two_targets() {
        let mut state = GameState::new_two_player(42);
        let obj_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Only one target — can't complete exchange.
        let ability = ResolvedAbility::new(
            Effect::ExchangeControl {
                target_a: TargetFilter::Any,
                target_b: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_a)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.transient_continuous_effects.is_empty());
        assert_noop(&state, &events);
    }

    /// CR 701.12a: no targets at all — the FIRST `resolve_slot` runs dry. The
    /// row above covers the second slot; this one covers the first.
    #[test]
    fn exchange_control_no_targets_is_noop() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::ExchangeControl {
                target_a: TargetFilter::Any,
                target_b: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_noop(&state, &events);
    }

    /// CR 613.1b + CR 701.12a: End-to-end layer pipeline test. Resolves an
    /// exchange-control effect then runs `evaluate_layers` and asserts the two
    /// targets' `controller` fields are ACTUALLY swapped — not merely that
    /// transient effects exist. This is the regression guard for Bug B:
    /// previously both `ChangeController` effects read `source.controller`
    /// (the caster) and set both objects to the caster instead of swapping.
    #[test]
    fn exchange_control_layer_pipeline_actually_swaps_controllers() {
        let mut state = GameState::new_two_player(42);
        let obj_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        // Source is controlled by PlayerId(0) (the caster) — deliberately chosen
        // to match the old buggy behaviour (source.controller == caster) so the
        // test would FAIL pre-fix (both objects would end up under PlayerId(0)).
        let source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Switcheroo".to_string(),
            Zone::Stack,
        );

        let ability = ResolvedAbility::new(
            Effect::ExchangeControl {
                target_a: TargetFilter::Any,
                target_b: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_a), TargetRef::Object(obj_b)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Run the layer pipeline (CR 613).
        crate::game::layers::evaluate_layers(&mut state);

        assert_eq!(
            state.objects.get(&obj_a).unwrap().controller,
            PlayerId(1),
            "obj_a should now be controlled by PlayerId(1) after swap"
        );
        assert_eq!(
            state.objects.get(&obj_b).unwrap().controller,
            PlayerId(0),
            "obj_b should now be controlled by PlayerId(0) after swap"
        );
        // CR 603.2: and the swap the layer pipeline just performed was PUBLISHED,
        // in both directions, so `ChangesController` triggers and the CR 608.2c
        // "if you do" riders can key off it.
        assert_eq!(
            controller_changes(&events),
            vec![
                (obj_a, PlayerId(0), PlayerId(1)),
                (obj_b, PlayerId(1), PlayerId(0)),
            ]
        );
    }
}
