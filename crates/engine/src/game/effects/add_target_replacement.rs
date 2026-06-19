use crate::game::targeting::resolve_event_context_target;
use crate::types::ability::{
    DamageTargetFilter, DamageTargetPlayerScope, Duration, Effect, EffectError, EffectKind,
    ReplacementDefinition, ResolvedAbility, RestrictionExpiry, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

pub(crate) fn expiry_from_duration(
    duration: Option<&Duration>,
    controller: crate::types::player::PlayerId,
) -> Option<RestrictionExpiry> {
    match duration {
        Some(Duration::UntilEndOfTurn) => Some(RestrictionExpiry::EndOfTurn),
        Some(Duration::UntilEndOfCombat) => Some(RestrictionExpiry::EndOfCombat),
        Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        }) => Some(RestrictionExpiry::UntilPlayerNextTurn { player: controller }),
        _ => None,
    }
}

fn replacement_with_ability_expiry(
    replacement: &ReplacementDefinition,
    ability: &ResolvedAbility,
) -> ReplacementDefinition {
    let mut replacement = replacement.clone();
    if replacement.expiry.is_none() {
        replacement.expiry = expiry_from_duration(ability.duration.as_ref(), ability.controller);
    }
    // CR 109.4 + CR 614.1a: Anchor the installing player onto the replacement so
    // global pending damage replacements (pushed under the sentinel `ObjectId(0)`,
    // which has no controller in `state.objects`) can resolve a controller-relative
    // `damage_source_filter` ("a source you control"). Without this anchor,
    // `ControllerRef::You` never matches because the sentinel source has no
    // controller, so the boost silently never fires (I Call for Slaughter, Rankle
    // and Torbran, Taii Wakeen's +X boost). Guarded on `is_none` so a replacement
    // that already specified a controller is never clobbered.
    if replacement.source_controller.is_none() {
        replacement.source_controller = Some(ability.controller);
    }
    freeze_damage_modification_x(&mut replacement, ability);
    replacement
}

/// CR 107.3a + CR 601.2b: Freeze the announced value of X into a "deals that
/// much damage plus X" replacement at activation time. The parser emits
/// `DamageModification::Plus { value: 0 }` as a placeholder (the `u32`-typed
/// modification cannot carry a symbolic X); here the announced X (held on the
/// activating ability as `chosen_x`) replaces the placeholder so the replacement
/// applies the locked-in value for the rest of the turn (Taii Wakeen's second
/// ability). The `chosen_x.is_some()` guard ensures a genuine literal "plus 0"
/// (no X in the cost) is never clobbered. (CR 107.3a: an activated ability's X
/// equals its announced value while on the stack and beyond.)
fn freeze_damage_modification_x(
    replacement: &mut ReplacementDefinition,
    ability: &ResolvedAbility,
) {
    if let (Some(crate::types::ability::DamageModification::Plus { value }), Some(chosen_x)) =
        (replacement.damage_modification.as_mut(), ability.chosen_x)
    {
        if *value == 0 {
            *value = chosen_x;
        }
    }
}

fn replacement_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<TargetRef> {
    if matches!(target, TargetFilter::Any) {
        return ability.targets.clone();
    }

    // CR 201.5: SelfRef resolves to the ability's source object — text that
    // refers to the object it's on by name (or "~") means that particular
    // object. Used by self-installing replacements (Crafty Cutpurse: "When ~
    // enters, [until end of turn] each token that would be created under an
    // opponent's control is created under your control instead.") so the
    // trigger anchors the replacement on its own source without needing to
    // consult the target pipeline.
    if matches!(target, TargetFilter::SelfRef) {
        return vec![TargetRef::Object(ability.source_id)];
    }

    resolve_event_context_target(state, target, ability.source_id)
        .into_iter()
        .collect()
}

/// CR 614.1a + CR 514.2: Push a replacement effect onto the parent
/// ability's target object or player at resolution time. Used by riders like
/// "If that creature would die this turn, exile it instead." attached to
/// damage-dealing spells/abilities. The carried `ReplacementDefinition`
/// is appended to each targeted object's `replacement_definitions`, or to
/// GameState pending damage replacements for player-scoped damage effects.
///
/// Multiple targets each receive their own copy of the replacement —
/// `valid_card: SelfRef` inside the carried definition naturally binds
/// to the carrying object, so each instance fires only for its host.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::AddTargetReplacement {
        replacement,
        target,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "AddTargetReplacement replacement".to_string(),
        ));
    };

    let mut attached = 0usize;

    // CR 614.1a: `TargetFilter::None` is the "no per-target binding" signal —
    // the carried replacement is self-contained (its own source/target filters
    // already constrain when it fires) and is pushed directly to the global
    // pending_damage_replacements. Used by triggered creation of turn-bound
    // damage-modification replacements (Rankle and Torbran's "If a source
    // would deal damage to a player or battle this turn..."; I Call for
    // Slaughter's "If a source you control would deal damage this turn,
    // it deals that much damage plus 1 instead.").
    if matches!(target, TargetFilter::None) {
        let replacement = replacement_with_ability_expiry(replacement, ability);
        state.pending_damage_replacements.push(replacement);
        attached += 1;
    } else {
        for resolved_target in replacement_targets(state, ability, target) {
            match resolved_target {
                TargetRef::Object(obj_id) => {
                    let mut replacement = replacement_with_ability_expiry(replacement, ability);
                    replacement.fix_legacy_parse_time_consumed_flag();
                    if let Some(obj) = state.objects.get_mut(&obj_id) {
                        obj.replacement_definitions.push(replacement);
                        attached += 1;
                    }
                }
                TargetRef::Player(player) => {
                    let mut replacement = replacement_with_ability_expiry(replacement, ability);
                    if matches!(
                        replacement.event,
                        crate::types::replacements::ReplacementEvent::DamageDone
                    ) && replacement.damage_target_filter.is_none()
                    {
                        replacement.damage_target_filter =
                            Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                                player: DamageTargetPlayerScope::Specific(player),
                            });
                    }
                    state.pending_damage_replacements.push(replacement);
                    attached += 1;
                }
            }
        }
    }

    if attached > 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::AddTargetReplacement,
            source_id: ability.source_id,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::replacement::{replace_event, ReplacementResult};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, DamageModification, DamageTargetPlayerScope, Duration,
        ReplacementDefinition, RestrictionExpiry, TargetFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::proposed_event::ProposedEvent;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn damage_to(target: TargetRef, amount: u32) -> ProposedEvent {
        ProposedEvent::Damage {
            source_id: ObjectId(99),
            target,
            amount,
            is_combat: false,
            applied: Default::default(),
        }
    }

    #[test]
    fn die_exile_rider_with_legacy_is_consumed_applies_exile_redirect() {
        use crate::types::ability::{AbilityKind, Effect, TargetFilter};
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let target = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let mut repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Graveyard)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                },
            ));
        repl.is_consumed = true;
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);
        repl.fix_legacy_parse_time_consumed_flag();

        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(0),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
            target,
            Zone::Battlefield,
            Zone::Graveyard,
            None,
        );
        let result = crate::game::replacement::replace_event(&mut state, proposed, &mut events);
        match result {
            crate::game::replacement::ReplacementResult::Execute(
                crate::types::proposed_event::ProposedEvent::ZoneChange { to, .. },
            ) => assert_eq!(to, Zone::Exile),
            other => panic!("expected exile redirect, got {other:?}"),
        }
        assert!(
            state.objects.get(&target).unwrap().replacement_definitions[0].is_consumed,
            "one-shot rider must consume after applying"
        );
    }

    #[test]
    fn pushes_eot_replacement_onto_target_object() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Graveyard);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(id)],
            ObjectId(0),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.replacement_definitions.iter_all().count(), 1);
        assert_eq!(
            obj.replacement_definitions[0].expiry,
            Some(RestrictionExpiry::EndOfTurn)
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::AddTargetReplacement,
                ..
            }
        )));
    }

    #[test]
    fn pushes_damage_replacement_for_triggering_player() {
        let mut state = GameState::new_two_player(42);
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        });

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Double);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::TriggeringPlayer,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        let pending = &state.pending_damage_replacements[0];
        assert_eq!(
            pending.damage_target_filter,
            Some(DamageTargetFilter::PlayerOrPermanentsControlledBy {
                player: DamageTargetPlayerScope::Specific(PlayerId(1))
            })
        );
        assert_eq!(
            pending.expiry,
            Some(RestrictionExpiry::UntilPlayerNextTurn {
                player: PlayerId(0)
            })
        );

        let proposed = damage_to(TargetRef::Player(PlayerId(1)), 2);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified damage event, got {result:?}");
        };
        assert_eq!(amount, 4);

        let permanent = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Permanent".to_string(),
            Zone::Battlefield,
        );
        let proposed = damage_to(TargetRef::Object(permanent), 3);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified permanent damage event, got {result:?}");
        };
        assert_eq!(amount, 6);
    }

    #[test]
    fn pending_damage_replacement_expires_on_controllers_next_turn() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(7),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        });

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Double);
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::TriggeringPlayer,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.pending_damage_replacements.len(), 1);

        crate::game::turns::execute_untap(&mut state, &mut events);
        assert!(state.pending_damage_replacements.is_empty());

        let proposed = damage_to(TargetRef::Player(PlayerId(1)), 2);
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected unmodified damage event, got {result:?}");
        };
        assert_eq!(amount, 2);
    }

    #[test]
    fn target_filter_none_pushes_global_replacement_without_inference() {
        // CR 614.1a: `TargetFilter::None` is the no-binding mode used by
        // self-contained turn-bound damage-modification replacements
        // (Rankle and Torbran, I Call for Slaughter). The resolver must
        // push the carried replacement directly to
        // `pending_damage_replacements` WITHOUT inferring a
        // `damage_target_filter` from a player target — the carried
        // replacement's own source/target/scope filters are the source
        // of truth.
        let mut state = GameState::new_two_player(42);
        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Plus { value: 1 })
            .damage_source_filter(TargetFilter::Typed(
                crate::types::ability::TypedFilter::default()
                    .controller(crate::types::ability::ControllerRef::You),
            ));
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        let pending = &state.pending_damage_replacements[0];
        // Critical: damage_target_filter must remain None — no per-target
        // inference (which would scope to a specific player).
        assert_eq!(pending.damage_target_filter, None);
        assert_eq!(pending.expiry, Some(RestrictionExpiry::EndOfTurn));
    }

    /// CR 109.4 + CR 614.1a: discriminating runtime test for the
    /// controller-anchor fix. A global "If a source you control would deal
    /// damage this turn, it deals that much damage plus 1 instead." replacement
    /// (`damage_source_filter = controller You`) is pushed under the sentinel
    /// `ObjectId(0)`. The boost MUST fire for damage from a source controlled by
    /// the installing player, and MUST NOT fire for damage from an opponent's
    /// source.
    ///
    /// The boosted-amount assertion (`amount, 3`) flips if the anchor read at
    /// `replacement.rs` is reverted: without it, `from_source(state, ObjectId(0))`
    /// yields `source_controller = None`, `ControllerRef::You` never matches, and
    /// the replacement is skipped (amount stays 2).
    #[test]
    fn global_source_you_control_boost_fires_for_own_source_only() {
        use crate::types::ability::{ControllerRef, TypedFilter};

        let mut state = GameState::new_two_player(42);
        // A source we control, and a source the opponent controls.
        let my_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "My Bear".to_string(),
            Zone::Battlefield,
        );
        let their_source = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Their Bear".to_string(),
            Zone::Battlefield,
        );
        let victim = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Victim".to_string(),
            Zone::Battlefield,
        );

        let replacement = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::Plus { value: 1 })
            .damage_source_filter(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
        let mut ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(replacement),
                target: TargetFilter::None,
            },
            Vec::new(),
            // Installing ability controlled by PlayerId(0) — the anchor source.
            ObjectId(7),
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].source_controller,
            Some(PlayerId(0)),
            "install chokepoint must stamp the activating ability's controller"
        );

        // Positive: damage from OUR source is boosted 2 -> 3.
        let proposed = ProposedEvent::Damage {
            source_id: my_source,
            target: TargetRef::Object(victim),
            amount: 2,
            is_combat: false,
            applied: Default::default(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected modified damage event, got {result:?}");
        };
        assert_eq!(
            amount, 3,
            "a source we control must deal damage plus 1 (anchor read at the match site)"
        );

        // Negative: damage from the OPPONENT's source is unchanged.
        let proposed = ProposedEvent::Damage {
            source_id: their_source,
            target: TargetRef::Object(victim),
            amount: 2,
            is_combat: false,
            applied: Default::default(),
        };
        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
            panic!("expected unmodified damage event, got {result:?}");
        };
        assert_eq!(
            amount, 2,
            "an opponent's source must not be boosted by 'a source you control'"
        );
    }

    // Crafty Cutpurse end-to-end: a self-installed CreateToken replacement
    // with `token_owner_scope: Opponent` and `token_owner_redirect: You`
    // redirects opponent-created tokens to the source's controller.
    // Covers CR 111.2 (token controller redirection — "the token enters the
    // battlefield under that player's control") + CR 614.1a (replacement
    // ordering: redirect applies before the token materializes).
    #[test]
    fn crafty_cutpurse_self_install_redirects_opponent_tokens_to_controller() {
        use crate::types::ability::ControllerRef;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let cutpurse_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Crafty Cutpurse".to_string(),
            Zone::Battlefield,
        );

        // Build the replacement that the parsed trigger would install.
        let mut repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::Opponent)
            .token_owner_redirect(ControllerRef::You);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let install_ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();

        // Sanity: replacement landed on Cutpurse, marked EOT-expiring.
        let installed = &state.objects[&cutpurse_id].replacement_definitions;
        assert_eq!(installed.iter_all().count(), 1);
        assert_eq!(
            installed[0].token_owner_scope,
            Some(ControllerRef::Opponent)
        );
        assert_eq!(installed[0].token_owner_redirect, Some(ControllerRef::You));
        assert_eq!(installed[0].expiry, Some(RestrictionExpiry::EndOfTurn));

        // Opponent (PlayerId(1)) proposes creating a Treasure token under their control.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
                core_types: vec![crate::types::card_type::CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
                supertypes: Vec::new(),
                colors: Vec::new(),
                keywords: Vec::new(),
            },
            script_name: "Treasure".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(50),
            controller: PlayerId(1),
            attach_to: None,
        };
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            spec: Box::new(token_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::CreateToken {
            owner, ref spec, ..
        }) = result
        else {
            panic!("expected modified CreateToken event, got {result:?}");
        };
        assert_eq!(
            owner,
            PlayerId(0),
            "Crafty Cutpurse should redirect opponent's token to its controller"
        );
        // CR 111.2: `spec.controller` is consumed by the apply path
        // (combat::enter_attacking defending-player resolution, ETB-counter
        // accounting) and must move with the redirected owner — otherwise an
        // enters-attacking Goblin Rabblemaster token would compute its
        // defender against the original effect controller (the opponent) and
        // end up attacking its new controller.
        assert_eq!(
            spec.controller,
            PlayerId(0),
            "spec.controller must follow the redirected owner under CR 111.2"
        );
    }

    // Crafty Cutpurse + Goblin Rabblemaster class: an opponent creates a token
    // *that's tapped and attacking*. The redirect rewires owner to Cutpurse's
    // controller; `spec.controller` must follow so the apply path's
    // `enter_attacking` lookup picks a defending player from the redirected
    // controller's opponents — not from the original effect's controller.
    #[test]
    fn crafty_cutpurse_redirects_spec_controller_for_enters_attacking_token() {
        use crate::types::ability::ControllerRef;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let cutpurse_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Crafty Cutpurse".to_string(),
            Zone::Battlefield,
        );

        let mut repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::Opponent)
            .token_owner_redirect(ControllerRef::You);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let install_ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();

        // Opponent's Rabblemaster-style "create a 1/1 Goblin that's tapped
        // and attacking" — `enters_attacking: true`, `spec.controller: P1`.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Goblin".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Goblin".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::Red],
                keywords: Vec::new(),
            },
            script_name: "Goblin".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: true,
            enters_attacking: true,
            sacrifice_at: None,
            source_id: ObjectId(70),
            controller: PlayerId(1),
            attach_to: None,
        };
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(1),
            spec: Box::new(token_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::CreateToken {
            owner, ref spec, ..
        }) = result
        else {
            panic!("expected modified CreateToken event, got {result:?}");
        };
        assert_eq!(owner, PlayerId(0));
        assert_eq!(
            spec.controller,
            PlayerId(0),
            "redirected enters-attacking token must carry the new controller \
             so enter_attacking picks the correct defender"
        );
    }

    // Symmetry guard: tokens already created under our control are untouched.
    // Without the `token_owner_scope: Opponent` filter the redirect would also
    // fire on our own tokens — but `find_applicable_replacements` skips the
    // entry when the proposed owner does not match the scope, so this is the
    // existing matcher's job; here we just make sure that's still true.
    #[test]
    fn crafty_cutpurse_does_not_redirect_own_tokens() {
        use crate::types::ability::ControllerRef;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let cutpurse_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Crafty Cutpurse".to_string(),
            Zone::Battlefield,
        );

        let mut repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .token_owner_scope(ControllerRef::Opponent)
            .token_owner_redirect(ControllerRef::You);
        repl.expiry = Some(RestrictionExpiry::EndOfTurn);

        let install_ability = ResolvedAbility::new(
            Effect::AddTargetReplacement {
                replacement: Box::new(repl),
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            cutpurse_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &install_ability, &mut events).unwrap();

        // Our own token creation — must not be intercepted.
        let token_spec = TokenSpec {
            characteristics: crate::types::proposed_event::TokenCharacteristics {
                display_name: "Saproling".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Saproling".to_string()],
                supertypes: Vec::new(),
                colors: vec![crate::types::mana::ManaColor::Green],
                keywords: Vec::new(),
            },
            script_name: "Saproling".to_string(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(60),
            controller: PlayerId(0),
            attach_to: None,
        };
        let proposed = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(token_spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let result = replace_event(&mut state, proposed, &mut events);
        let ReplacementResult::Execute(ProposedEvent::CreateToken { owner, .. }) = result else {
            panic!("expected unmodified CreateToken event, got {result:?}");
        };
        assert_eq!(
            owner,
            PlayerId(0),
            "our own token creation must not be redirected by our own Crafty Cutpurse"
        );
    }
}
