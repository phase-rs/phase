use crate::game::ability_utils::append_to_sub_chain;
use crate::game::effects::deal_damage::{apply_damage_to_target, DamageContext, DamageResult};
use crate::game::effects::{append_to_pending_continuation, mark_pending_continuation_parent};
use crate::types::ability::{
    Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.14a: Resolve the subject creature for a fight effect.
/// - `SelfRef` → the ability's source object (default: "~ fights").
/// - `AttachedTo` → the permanent this Aura/Equipment is attached to
///   ("enchanted creature fights" / "equipped creature fights").
fn resolve_fight_subject(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Result<ObjectId, EffectError> {
    let subject = match &ability.effect {
        Effect::Fight { subject, .. } => subject,
        _ => return Ok(ability.source_id),
    };
    if refers_to_attached(subject) {
        // CR 303.4 + CR 701.14a: "Enchanted creature fights" requires an Object
        // host (a creature). If the source is attached to a player (CR 303.4 +
        // CR 702.5d, Curse cycle), there's no creature subject — surface the
        // same MissingParam error as the unattached case.
        state
            .objects
            .get(&ability.source_id)
            .and_then(|obj| obj.attached_to)
            .and_then(|t| t.as_object())
            .ok_or_else(|| {
                EffectError::MissingParam("Fight subject: source not attached to anything".into())
            })
    } else {
        Ok(ability.source_id)
    }
}

/// Returns true if this filter refers to the permanent the source is attached to
/// (enchanted creature / equipped creature).
fn refers_to_attached(filter: &TargetFilter) -> bool {
    use crate::types::ability::FilterProp;
    matches!(filter, TargetFilter::AttachedTo)
        || matches!(filter, TargetFilter::Typed(tf) if tf.properties.iter().any(|p|
            matches!(p, FilterProp::EnchantedBy | FilterProp::EquippedBy)
        ))
}

/// CR 701.14a + CR 120.10: Resolve the two fighters as `(subject, fought
/// creature)` without mutating state. This is the single authority for the
/// fighter split, so the damage resolver and the "[the fought creature] is
/// dealt excess damage this way" recipient lookup
/// (`previous_effect_excess_amount_from_events`, The Last Agni Kai) never
/// disagree on which object receives the fight damage.
///
/// - Two chosen object targets ("target creature you control fights target
///   creature an opponent controls") → both are fighters and the fought
///   creature is the *second* target.
/// - One chosen object target ("~ fights target creature" / "enchanted creature
///   fights target creature") → the subject is the ability source / attached
///   host and the sole object target is the fought creature.
///
/// `Ok(None)` means the fight can't happen (a dual-target fight reduced to a
/// lone surviving fighter) and must resolve with no damage.
pub(crate) fn resolve_fight_fighters(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Result<Option<(ObjectId, ObjectId)>, EffectError> {
    let object_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            _ => None,
        })
        .collect();

    // CR 701.14a + CR 608.2c: "those creatures fight each other" — a two-fighter
    // chain (Malamet/Longstalk/Duel) whose declared fighters live in the two
    // earliest chain slots. Under most-recent-only chain propagation the Fight
    // node's LOCAL `targets` carry at most one object, so recover BOTH fighters
    // from the flattened chain root. Fire ONLY when the root genuinely declares
    // two distinct object slots; otherwise fall through untouched, protecting
    // incumbent single-fighter `Fight{ParentTarget}` cards (time to feed,
    // ezuri's predation, joust, …) whose root has <2 object slots (slot 1 → None).
    if object_targets.len() < 2 {
        if let (Some(TargetRef::Object(a)), Some(TargetRef::Object(b))) = (
            crate::game::targeting::resolve_parent_slot_from_root(state, ability, 0),
            crate::game::targeting::resolve_parent_slot_from_root(state, ability, 1),
        ) {
            if a != b {
                return Ok(Some((a, b)));
            }
        }
    }

    if object_targets.len() >= 2 {
        return Ok(Some((object_targets[0], object_targets[1])));
    }
    if let Effect::Fight { subject, .. } = &ability.effect {
        if crate::game::ability_utils::fight_subject_needs_target_slot(subject) {
            // CR 701.14a: Dual-target fights require both chosen fighters; do
            // not reinterpret a lone survivor as "~ fights target creature".
            return Ok(None);
        }
    }
    let source_id = resolve_fight_subject(state, ability)?;
    let target_id = object_targets
        .first()
        .copied()
        .ok_or_else(|| EffectError::MissingParam("Fight target".to_string()))?;
    Ok(Some((source_id, target_id)))
}

/// CR 701.14b + CR 702.26b: A creature can fight only while it is on the
/// battlefield, still a creature, and phased in. A phased-out permanent is
/// treated as though it does not exist (CR 702.26b), so a creature that phased
/// out in response to the fight (e.g. via Teferi's Protection) deals and takes
/// no damage even though its `zone` is still `Battlefield`.
fn fight_eligible(state: &GameState, id: ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| {
        obj.zone == Zone::Battlefield
            && obj.card_types.core_types.contains(&CoreType::Creature)
            && obj.is_phased_in()
    })
}

/// CR 701.14a: Fight — each creature deals damage equal to its power to the other.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 701.14a: Resolve the two fighters.
    // - "~ fights target creature" / "it fights target …": one chosen object target;
    //   the subject (~ / enchanted creature) is the other fighter.
    // - "Target creature you control fights another target creature": two chosen
    //   object targets are the fighters; the ability's source (e.g. Ulvenwald
    //   Tracker) is not a participant.
    let (source_id, target_id) = match resolve_fight_fighters(state, ability)? {
        Some(pair) => pair,
        None => {
            // CR 701.14a: Dual-target fight reduced to a lone fighter — no fight,
            // no damage. Emit the parent Fight event so downstream "when a
            // creature fights" triggers observe the (no-op) resolution.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Fight,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
    };

    // CR 701.14b: If either fighter left the battlefield or is no longer a creature, no damage.
    if !fight_eligible(state, source_id) || !fight_eligible(state, target_id) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // Read power and controller for both creatures before mutable damage phase.
    let (source_power, source_controller) = {
        let obj = state
            .objects
            .get(&source_id)
            .ok_or(EffectError::ObjectNotFound(source_id))?;
        (obj.power.unwrap_or(0), obj.controller)
    };
    let (target_power, target_controller) = {
        let obj = state
            .objects
            .get(&target_id)
            .ok_or(EffectError::ObjectNotFound(target_id))?;
        (obj.power.unwrap_or(0), obj.controller)
    };

    // CR 701.14c: Self-fight deals twice the creature's power as a single damage instance.
    if source_id == target_id {
        if source_power > 0 {
            let amount = (source_power as u32).saturating_mul(2);
            let source_ctx = DamageContext::from_source(state, source_id)
                .unwrap_or_else(|| DamageContext::fallback(source_id, source_controller));
            if let DamageResult::NeedsChoice = apply_damage_to_target(
                state,
                &source_ctx,
                TargetRef::Object(source_id),
                amount,
                false,
                events,
            )? {
                if let Some(sub) = ability.sub_ability.as_ref() {
                    append_to_pending_continuation(state, Some(Box::new(sub.as_ref().clone())));
                }
                mark_pending_continuation_parent(state, EffectKind::Fight);
                return Ok(());
            }
        }
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 701.14a + CR 120.2b: Fight damage is not combat damage.
    // Source deals damage to target (power of source → target's damage)
    if source_power > 0 {
        let source_ctx = DamageContext::from_source(state, source_id)
            .unwrap_or_else(|| DamageContext::fallback(source_id, source_controller));
        if let DamageResult::NeedsChoice = apply_damage_to_target(
            state,
            &source_ctx,
            TargetRef::Object(target_id),
            source_power as u32,
            false,
            events,
        )? {
            // CR 701.14a: First direction is waiting on a replacement choice.
            // Stash a continuation so the second direction (target → source) resumes
            // after the choice resolves. The parent's sub_ability (if any) is appended
            // to the continuation's tail so downstream effects still fire.
            if target_power > 0 {
                // Second direction: target_id (the fight target) deals damage equal
                // to its power to source_id (the fighter).
                let mut second = build_fight_damage_node(
                    target_id,
                    source_id,
                    target_power as u32,
                    target_controller,
                );
                if let Some(sub) = ability.sub_ability.as_ref() {
                    append_to_sub_chain(&mut second, sub.as_ref().clone());
                }
                append_to_pending_continuation(state, Some(Box::new(second)));
            } else if let Some(sub) = ability.sub_ability.as_ref() {
                append_to_pending_continuation(state, Some(Box::new(sub.as_ref().clone())));
            }
            // CR 701.14a: Tag the stashed chain with the parent `EffectKind::Fight`
            // so the drain re-emits the parent `EffectResolved` that the non-pause
            // tail (line ~154) fires. "When a creature fights" triggers in
            // `trigger_matchers.rs::match_fight` key on this event.
            mark_pending_continuation_parent(state, EffectKind::Fight);
            return Ok(());
        }
    }

    // Target deals damage to source (power of target → source's damage)
    if target_power > 0 {
        let target_ctx = DamageContext::from_source(state, target_id)
            .unwrap_or_else(|| DamageContext::fallback(target_id, target_controller));
        if let DamageResult::NeedsChoice = apply_damage_to_target(
            state,
            &target_ctx,
            TargetRef::Object(source_id),
            target_power as u32,
            false,
            events,
        )? {
            // CR 701.14a: Second direction is waiting on a replacement choice — no more
            // damage to deal, but propagate the parent's sub_ability if present.
            if let Some(sub) = ability.sub_ability.as_ref() {
                append_to_pending_continuation(state, Some(Box::new(sub.as_ref().clone())));
            }
            // CR 701.14a: Tag the stashed chain (the sub_ability, if any) with the
            // parent `EffectKind::Fight` so the drain re-emits the parent event the
            // non-pause tail (line ~154) fires. If there is no sub_ability the chain
            // is absent and the parent event cannot ride anything out — the replacement
            // delivery still resolves the fight's second direction, but fight triggers
            // are lost in that corner case (a 0-power fighter with no follow-up sub).
            mark_pending_continuation_parent(state, EffectKind::Fight);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.14a: Build a one-shot, single-target non-combat `DealDamage` node representing
/// one direction of a fight. `source_id` deals `amount` damage to `target_id`.
/// Used as a continuation when the first direction of fight damage pauses for a
/// replacement choice.
fn build_fight_damage_node(
    source_id: ObjectId,
    target_id: ObjectId,
    amount: u32,
    controller: crate::types::player::PlayerId,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed {
                value: amount as i32,
            },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Object(target_id)],
        source_id,
        controller,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, Comparator, Effect, ManaContribution, ManaProduction, QuantityExpr,
        QuantityRef, TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaColor, ManaType};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    fn make_fight_ability(source: ObjectId, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Fight {
                target: TargetFilter::Any,
                subject: TargetFilter::SelfRef,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        )
    }

    #[test]
    fn test_fight_mutual_damage() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let ability = make_fight_ability(bear, wolf);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Bear (3/3) deals 3 damage to Wolf -> Wolf has 3 damage
        assert_eq!(state.objects[&wolf].damage_marked, 3);
        // Wolf (2/2) deals 2 damage to Bear -> Bear has 2 damage
        assert_eq!(state.objects[&bear].damage_marked, 2);
    }

    #[test]
    fn dual_target_fight_uses_both_chosen_creatures_not_ability_source() {
        // CR 701.14a: "Target creature you control fights another target creature"
        // — both chosen creatures fight; the activated source is not a fighter.
        let mut state = GameState::new_two_player(42);
        let tracker = make_creature(&mut state, PlayerId(0), "Ulvenwald Tracker", 1, 1);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let ability = ResolvedAbility::new(
            Effect::Fight {
                target: TargetFilter::Any,
                subject: TargetFilter::SelfRef,
            },
            vec![TargetRef::Object(bear), TargetRef::Object(wolf)],
            tracker,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&wolf].damage_marked, 3);
        assert_eq!(state.objects[&bear].damage_marked, 2);
        assert_eq!(state.objects[&tracker].damage_marked, 0);
    }

    #[test]
    fn test_fight_emits_damage_events() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let ability = make_fight_ability(bear, wolf);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have 2 DamageDealt events + 1 EffectResolved
        let damage_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .collect();
        assert_eq!(damage_events.len(), 2);
    }

    #[test]
    fn fight_threads_target_excess_damage_to_followup_quantity() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wall = make_creature(&mut state, PlayerId(1), "Wall", 0, 2);
        let mut ability = make_fight_ability(bear, wall);
        let mut mana = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            bear,
            PlayerId(0),
        );
        mana.condition = Some(AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::GT,
            rhs: QuantityExpr::Fixed { value: 0 },
            channel: crate::types::ability::DamageChannel::Excess,
        });
        ability.sub_ability = Some(Box::new(mana));

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
    }

    #[test]
    fn fight_excess_damage_followup_condition_rejects_zero_excess() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let wall = make_creature(&mut state, PlayerId(1), "Wall", 0, 3);
        let mut ability = make_fight_ability(bear, wall);
        let mut mana = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            bear,
            PlayerId(0),
        );
        mana.condition = Some(AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::GT,
            rhs: QuantityExpr::Fixed { value: 0 },
            channel: crate::types::ability::DamageChannel::Excess,
        });
        ability.sub_ability = Some(Box::new(mana));

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 0);
    }

    /// CR 120.10 + CR 701.14a: In a *dual-target* fight (The Last Agni Kai —
    /// "target creature you control fights target creature an opponent controls.
    /// If the creature the opponent controls is dealt excess damage this way,
    /// add that much {R}"), both the excess-gate condition and the "that much"
    /// quantity must read excess dealt to the FOUGHT creature (the second chosen
    /// object target), never the fighting subject (the first).
    ///
    /// Discriminates the fix: the attacker (5/5) is fat and takes no excess from
    /// the 0/1 victim, so the old first-object-target lookup read 0 excess off
    /// the subject and produced no mana. The victim receives 5 damage — 4 excess
    /// beyond its 1 lethal — so the correct recipient lookup adds 4 red mana.
    #[test]
    fn dual_target_fight_excess_reads_fought_creature_not_subject() {
        let mut state = GameState::new_two_player(42);
        let attacker = make_creature(&mut state, PlayerId(0), "Attacker", 5, 5);
        let victim = make_creature(&mut state, PlayerId(1), "Victim", 0, 1);

        // Dual-target fight: both fighters are chosen object targets, ordered
        // [your creature, opponent's creature].
        let mut ability = ResolvedAbility::new(
            Effect::Fight {
                target: TargetFilter::Any,
                subject: TargetFilter::Any,
            },
            vec![TargetRef::Object(attacker), TargetRef::Object(victim)],
            attacker,
            PlayerId(0),
        );
        let mut mana = ResolvedAbility::new(
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    color_options: vec![ManaColor::Red],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
            vec![],
            attacker,
            PlayerId(0),
        );
        mana.condition = Some(AbilityCondition::PreviousEffectAmount {
            comparator: Comparator::GT,
            rhs: QuantityExpr::Fixed { value: 0 },
            channel: crate::types::ability::DamageChannel::Excess,
        });
        ability.sub_ability = Some(Box::new(mana));

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // 4 excess dealt to the fought creature → "add that much {R}" = 4 red.
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 4);
        // The subject took no excess (victim has 0 power) — the old lookup would
        // have read 0 here and added no mana.
        assert_eq!(state.objects[&attacker].damage_marked, 0);
        assert_eq!(state.objects[&victim].damage_marked, 5);
    }

    #[test]
    fn test_fight_zero_power_no_damage() {
        let mut state = GameState::new_two_player(42);
        let wall = make_creature(&mut state, PlayerId(0), "Wall", 0, 5);
        let bear = make_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let ability = make_fight_ability(wall, bear);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Wall has 0 power, deals no damage to Bear
        assert_eq!(state.objects[&bear].damage_marked, 0);
        // Bear has 2 power, deals 2 damage to Wall
        assert_eq!(state.objects[&wall].damage_marked, 2);
    }

    #[test]
    fn fight_lifelink_gains_life() {
        let mut state = GameState::new_two_player(42);
        let lifelinker = make_creature(&mut state, PlayerId(0), "Lifelinker", 3, 3);
        state
            .objects
            .get_mut(&lifelinker)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Lifelink);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let ability = make_fight_ability(lifelinker, wolf);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 702.15b: Lifelink — controller gains life equal to damage dealt.
        assert_eq!(state.objects[&wolf].damage_marked, 3);
        assert_eq!(state.objects[&lifelinker].damage_marked, 2);
        assert_eq!(state.players[0].life, 23); // 20 + 3 from lifelink
        assert_eq!(state.players[1].life, 20); // unchanged (no lifelink)
    }

    #[test]
    fn fight_aura_enchanted_creature_is_subject() {
        // "Enchanted creature fights target creature" — the Aura is the source,
        // but the enchanted creature should be the fighter, not the Aura.
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        // Create an Aura attached to the bear
        let aura_card_id = CardId(state.next_object_id);
        let aura_id = create_object(
            &mut state,
            aura_card_id,
            PlayerId(0),
            "Test Aura".to_string(),
            Zone::Battlefield,
        );
        let aura = state.objects.get_mut(&aura_id).unwrap();
        aura.card_types.core_types.push(CoreType::Enchantment);
        aura.attached_to = Some(crate::game::game_object::AttachTarget::Object(bear));

        // Fight with subject = enchanted creature (Typed filter with EnchantedBy)
        let ability = ResolvedAbility::new(
            Effect::Fight {
                target: TargetFilter::Any,
                subject: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature()
                        .properties(vec![crate::types::ability::FilterProp::EnchantedBy]),
                ),
            },
            vec![TargetRef::Object(wolf)],
            aura_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Bear (3/3) should fight Wolf (2/2), not the Aura
        assert_eq!(state.objects[&wolf].damage_marked, 3);
        assert_eq!(state.objects[&bear].damage_marked, 2);
    }

    /// CR 120.3 + CR 616.1e: When the first direction of fight damage pauses on a
    /// replacement choice, the second direction must be stashed as
    /// `pending_continuation` so it resumes after the choice is answered — not
    /// silently vanish.
    #[test]
    fn fight_with_damage_replacement_on_first_direction() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{ReplacementDefinition, ReplacementMode};
        use crate::types::game_state::WaitingFor;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        // Install an Optional DamageDone replacement on a host object so the first
        // damage event (bear → wolf) pauses for a player choice.
        let shield_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut shield = GameObject::new(
            shield_id,
            CardId(99),
            PlayerId(1),
            "Shield".to_string(),
            Zone::Battlefield,
        );
        shield.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Shield".to_string()),
        );
        state.objects.insert(shield_id, shield);
        state.battlefield.push_back(shield_id);

        let ability = make_fight_ability(bear, wolf);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // First direction paused on the replacement choice.
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        // A continuation was stashed for the second direction — previously this
        // branch silently returned Ok(()) and the second direction was dropped.
        let cont = state
            .active_ability_continuation()
            .expect("expected pending_continuation for second-direction fight damage");
        // Continuation is a single-target DealDamage from wolf to bear.
        match &cont.chain.effect {
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value },
                ..
            } => {
                assert_eq!(*value, 2, "wolf.power = 2 should drive second direction");
            }
            other => panic!("expected DealDamage continuation, got {other:?}"),
        }
        assert_eq!(
            cont.chain.source_id, wolf,
            "wolf deals the second-direction damage"
        );
        assert_eq!(cont.chain.targets, vec![TargetRef::Object(bear)]);
        assert_eq!(
            cont.parent_kind,
            Some(EffectKind::Fight),
            "fight pause must record parent kind so the drain re-emits the Fight event",
        );
        // Bear hasn't taken damage yet — second direction is still pending.
        assert_eq!(state.objects[&bear].damage_marked, 0);
    }

    /// CR 120.3 + CR 616.1e: End-to-end fight+replacement accept delivery.
    /// After accepting the replacement for the first direction, BOTH the
    /// first direction (bear → wolf, previously dropped by handle_replacement_choice)
    /// AND the second direction (wolf → bear, via pending_continuation) must land.
    #[test]
    fn fight_replacement_accepted_delivers_both_directions() {
        use crate::game::engine::apply_as_current;
        use crate::game::game_object::GameObject;
        use crate::types::ability::{ReplacementDefinition, ReplacementMode};
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        // Optional DamageDone replacement — fires once, so the first direction pauses
        // for a choice but the second direction applies without prompting.
        let shield_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut shield = GameObject::new(
            shield_id,
            CardId(99),
            PlayerId(1),
            "Shield".to_string(),
            Zone::Battlefield,
        );
        shield.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .mode(ReplacementMode::Optional { decline: None })
                .description("Shield".to_string()),
        );
        state.objects.insert(shield_id, shield);
        state.battlefield.push_back(shield_id);

        // Ensure the controlling player matches the waiting_for player so apply() accepts the action.
        state.priority_player = PlayerId(1);
        state.active_player = PlayerId(1);

        let ability = make_fight_ability(bear, wolf);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let WaitingFor::ReplacementChoice { player, .. } = &state.waiting_for else {
            panic!("expected ReplacementChoice, got {:?}", state.waiting_for);
        };
        // Set the priority/active player to match the replacement choice player.
        state.priority_player = *player;
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: *player,
            candidate_count: 1,
            candidates: vec![crate::types::game_state::ReplacementCandidateSummary {
                source_id: bear,
                source_name: String::new(),
                description: "Shield".to_string(),
            }],
        };

        // Accept the replacement for bear → wolf (first direction).
        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept replacement");
        let mut all_events = result.events;

        // First direction must have landed (previously this was silently dropped).
        assert_eq!(
            state.objects[&wolf].damage_marked, 3,
            "first direction (bear → wolf, 3 damage) must apply on accept"
        );

        // The continuation (wolf → bear) also triggers the Optional Shield replacement.
        // Accept it so the second direction lands.
        if matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
            let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
                .expect("accept second-direction replacement");
            all_events.extend(result.events);
        }

        assert_eq!(
            state.objects[&bear].damage_marked, 2,
            "second direction (wolf → bear, 2 damage) must apply via pending_continuation + accept"
        );
        assert!(
            state.active_ability_continuation().is_none(),
            "continuation must be consumed"
        );

        // CR 701.14a: The parent `EffectKind::Fight` event must be emitted on the
        // pause-and-resume path so "when a creature fights" triggers (see
        // `trigger_matchers.rs::match_fight`) fire the same way they do on the
        // non-pause tail. Without parent_kind on PendingContinuation, the drain
        // would emit only per-node DealDamage events and the Fight event would
        // be silently lost.
        let fight_events = all_events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Fight,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            fight_events, 1,
            "exactly one EffectKind::Fight event must fire across the pause-and-resume path; got events = {all_events:#?}",
        );
    }

    #[test]
    fn fight_no_damage_when_source_left_battlefield() {
        use crate::game::zones::move_to_zone;

        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let mut events = Vec::new();
        move_to_zone(&mut state, bear, Zone::Graveyard, &mut events);

        let ability = make_fight_ability(bear, wolf);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&wolf].damage_marked, 0);
        assert_eq!(state.objects[&bear].damage_marked, 0);
    }

    #[test]
    fn fight_no_damage_when_source_is_no_longer_creature() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        state
            .objects
            .get_mut(&bear)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let ability = make_fight_ability(bear, wolf);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&wolf].damage_marked, 0);
        assert_eq!(state.objects[&bear].damage_marked, 0);
    }

    #[test]
    fn fight_no_damage_when_target_left_battlefield() {
        use crate::game::zones::move_to_zone;

        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let mut events = Vec::new();
        move_to_zone(&mut state, wolf, Zone::Exile, &mut events);

        let ability = make_fight_ability(bear, wolf);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&wolf].damage_marked, 0);
        assert_eq!(state.objects[&bear].damage_marked, 0);
    }

    /// CR 701.14b + CR 702.26b: a fighter that phased out in response to the
    /// fight (e.g. via Teferi's Protection) is treated as though it does not
    /// exist — neither creature deals damage, even though its zone is still
    /// `Battlefield` and it is still a creature. This discriminates the
    /// `is_phased_in()` gate: without it, the stale phased-out creature would
    /// pass `fight_eligible` and wrongly deal/take damage.
    #[test]
    fn fight_no_damage_when_source_phased_out() {
        use crate::game::game_object::{PhaseOutCause, PhaseStatus};

        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);
        // Bear phases out (still on the battlefield, still a creature) before the
        // fight resolves.
        state.objects.get_mut(&bear).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };

        let ability = make_fight_ability(bear, wolf);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&wolf].damage_marked, 0,
            "wolf takes no damage"
        );
        assert_eq!(
            state.objects[&bear].damage_marked, 0,
            "phased-out bear deals no damage"
        );
    }

    /// CR 701.14a + CR 608.2c: the b2 dual-fight divert must fire ONLY when the
    /// flattened chain root declares two distinct object slots. A 1-slot root
    /// (`resolve_parent_slot_from_root(1) == None`) falls through to the
    /// single-target path; a 2-slot root recovers both fighters. Synthetic,
    /// zero card-data dependency: the two cases differ only in whether a second
    /// chain slot exists. Reverting to a naive `len<2 && ParentTarget` guard
    /// makes the 1-slot case wrongly reinterpret the lone target as a divert.
    #[test]
    fn dual_fight_divert_guarded_on_two_distinct_root_slots() {
        let mut state = GameState::new_two_player(42);
        let a = make_creature(&mut state, PlayerId(0), "A", 3, 3);
        let b = make_creature(&mut state, PlayerId(1), "B", 2, 2);

        // 1-slot root: top node is the Fight with a single local object target and
        // NO sub-chain. flatten_targets_in_chain == [a] → slot 1 is None → divert
        // off → single-target fall-through (subject SelfRef → source, target a).
        let one_slot = ResolvedAbility::new(
            Effect::Fight {
                target: TargetFilter::ParentTarget,
                subject: TargetFilter::SelfRef,
            },
            vec![TargetRef::Object(a)],
            a, // source
            PlayerId(0),
        );
        let fighters = resolve_fight_fighters(&state, &one_slot).unwrap();
        assert_eq!(
            fighters,
            Some((a, a)),
            "1-slot root: no divert; SelfRef subject + sole target → (source, target)"
        );

        // 2-slot root: top node holds [a], a sub-ability node holds [b]. The
        // flattened root is [a, b] → both slots Some & distinct → divert fires.
        let mut two_slot = ResolvedAbility::new(
            Effect::Fight {
                target: TargetFilter::ParentTarget,
                subject: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(a)],
            a,
            PlayerId(0),
        );
        let sub = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(b)],
            a,
            PlayerId(0),
        );
        two_slot.sub_ability = Some(Box::new(sub));
        let fighters = resolve_fight_fighters(&state, &two_slot).unwrap();
        assert_eq!(
            fighters,
            Some((a, b)),
            "2-slot root: divert recovers both declared fighters (slot0, slot1)"
        );
    }

    #[test]
    fn fight_self_deals_twice_power() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);

        let ability = make_fight_ability(bear, bear);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear].damage_marked, 6);

        let damage_events: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamageDealt { amount, .. } => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(damage_events, vec![6]);
    }
}
