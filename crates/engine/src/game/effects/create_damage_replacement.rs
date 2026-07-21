use crate::game::effects::choose_damage_source;
use crate::game::effects::prevent_damage::resolve_source_filter;
use crate::types::ability::{
    DamageRedirectTarget, Effect, EffectError, EffectKind, PreventionAmount, ReplacementDefinition,
    ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingContinuation, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// CR 614.9 + CR 614.1a + CR 615: Resolve `Effect::CreateDamageReplacement` —
/// build a one-shot "the next time [source] would deal [combat] damage [to X]
/// this turn, [modify/redirect] instead" damage-replacement shield.
///
/// Mirrors `prevent_damage::resolve`: it constructs a `ReplacementDefinition`
/// for `ReplacementEvent::DamageDone` carrying the effect's match filters
/// (source / target / combat scope) and tags it with a one-shot `ShieldKind`
/// (`DamageReplacementOneShot` for the amount form, `Redirection` for the
/// redirect form). The shield is consumed after its single use by the
/// `damage_done_applier` (CR 614.5) and dropped at end-of-turn cleanup.
///
/// Distinct from a continuous static `damage_modification` replacement (Furnace
/// of Rath): that is a permanent characteristic on the card with
/// `ShieldKind::None`, re-applied to every damage event; this one-shot is
/// created by an activated/triggered ability at resolution and expires.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (
        source_filter,
        combat_scope,
        target_filter,
        modification,
        redirect_to,
        redirect_amount,
        recipient_object_filter,
    ) = match &ability.effect {
        Effect::CreateDamageReplacement {
            source_filter,
            combat_scope,
            target_filter,
            modification,
            redirect_to,
            redirect_amount,
            // `redirect_object_filter` is consumed by the targeting layer
            // (`ability_utils::collect_target_slots`), not the resolver — the
            // resolved object arrives via `ability.targets`.
            redirect_object_filter: _,
            recipient_object_filter,
        } => (
            source_filter.clone(),
            combat_scope.clone(),
            target_filter.clone(),
            modification.clone(),
            *redirect_to,
            *redirect_amount,
            recipient_object_filter.clone(),
        ),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected CreateDamageReplacement effect".to_string(),
            ))
        }
    };

    // CR 609.7a + CR 614.9: "a source of your choice" / "that source" — the
    // damage source is a player choice. Resolve it to a concrete object NOW so
    // the shield can match damage later this turn via a durable `SpecificObject`
    // filter (the transient `last_chosen_damage_source` is cleared once the
    // continuation drains, so a `ChosenDamageSource` shield would never match a
    // later damage event). When no source has been chosen yet, prompt the choice
    // and re-enter this resolver as a continuation; on the second pass the choice
    // is recorded and we proceed.
    let resolved_source_filter = match &source_filter {
        Some(TargetFilter::ChosenDamageSource { filter: qualifier }) => {
            match state.last_chosen_damage_source.as_ref() {
                Some(_choice) => {
                    // CR 609.7b: Resolve the chosen damage source filter to check if
                    // the source matches the filter (including any color/type
                    // qualifier carried on the variant, rechecked live per 609.7b).
                    let resolved = resolve_source_filter(
                        &TargetFilter::ChosenDamageSource {
                            filter: qualifier.clone(),
                        },
                        state,
                        ability.source_id,
                        &ability.targets,
                    );
                    if matches!(resolved, TargetFilter::None) {
                        None
                    } else {
                        Some(resolved)
                    }
                }
                None => {
                    // CR 609.7 + CR 609.7a: prompt the source choice; stash self so
                    // the shield is built on the second pass with the choice known.
                    // The bare "a source of your choice" form admits ANY damage
                    // source; the qualified form ("a blue source of your choice")
                    // restricts the LEGAL candidates to the qualifier. A single
                    // `prompt_filter` binding drives BOTH candidate enumeration and
                    // the `WaitingFor` prompt so they cannot diverge.
                    let prompt_filter = qualifier.as_deref().cloned().unwrap_or(TargetFilter::Any);
                    let options =
                        choose_damage_source::damage_source_options(state, ability, &prompt_filter);
                    // If no legal source exists, the replacement does nothing
                    // (CR 609.7a) — fall through with no source filter rather
                    // than wedging on an empty prompt.
                    if !options.is_empty() {
                        state.park_ability_continuation(PendingContinuation::new(
                            Box::new(ability.clone()),
                            state,
                        ));
                        state.waiting_for = WaitingFor::DamageSourceChoice {
                            player: ability.controller,
                            source_filter: prompt_filter,
                            options,
                        };
                        events.push(GameEvent::EffectResolved {
                            kind: EffectKind::CreateDamageReplacement,
                            source_id: ability.source_id,
                            subject: None,
                        });
                        return Ok(());
                    }
                    None
                }
            }
        }
        other => other.clone(),
    };

    let mut shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .description("One-shot damage replacement".to_string());

    // CR 614.1a: Match filters — which damage source / recipient / kind this
    // one-shot replaces. SelfRef ("it"/"~"/"this creature") matches the host;
    // ChosenDamageSource was resolved above to a concrete SpecificObject.
    if let Some(filter) = resolved_source_filter {
        shield = shield.damage_source_filter(filter);
    }
    if let Some(filter) = target_filter {
        shield = shield.damage_target_filter(filter);
    }
    if let Some(scope) = combat_scope {
        shield = shield.combat_scope(scope);
    }

    // CR 614.9: Decide where to host the shield and whether the original
    // recipient consumed a declared object target slot. Hosting on an object
    // with `valid_card: SelfRef` (set below) makes the shield fire only on
    // damage to that object (mirrors `prevent_damage::resolve`'s host-on-target
    // pattern).
    //   * `Some(SelfRef)` ("...dealt to ~" — the en-Kor cycle): the recipient is
    //     the ability's own source. Host on the source (`valid_card: SelfRef` is
    //     set below) so the shield fires only on damage to it; it surfaces NO
    //     target slot, so a `ChosenObjectTarget` redirect reads the FIRST object
    //     target.
    //   * `Some(other)` ("...dealt to target creature" — Jade Monolith): the
    //     recipient is a chosen target object, consuming the first slot; the
    //     redirect reads the SECOND.
    let recipient_is_self = matches!(recipient_object_filter, Some(TargetFilter::SelfRef));
    let recipient_consumes_slot = recipient_object_filter.is_some() && !recipient_is_self;
    let recipient_host = if recipient_is_self {
        Some(ability.source_id)
    } else if recipient_object_filter.is_some() {
        chosen_target_object(ability, /*skip*/ 0)
    } else {
        None
    };

    // CR 614.5 + CR 614.9: Tag the shield as the appropriate one-shot kind.
    // Exactly one of `modification` / `redirect_to` is `Some` (parser invariant).
    match (modification, redirect_to) {
        (Some(modification), None) => {
            // CR 614.1a: amount-modifying one-shot (Desperate Gambit "deals
            // double that damage instead"). The amount formula reuses the
            // existing `DamageModification` axis; the shield kind classifies it
            // as one-shot so `damage_done_applier` consumes it after one use.
            shield = shield
                .damage_modification(modification)
                .damage_replacement_oneshot_shield();
        }
        (None, Some(recipient)) => {
            // CR 614.9: redirection one-shot (Soltari Guerrillas, Beacon of
            // Destiny, Jade Monolith, Goblin Psychopath). `Controller` and
            // `SourceObject` resolve from the shield host at damage-apply time;
            // `ChosenObjectTarget` ("to target creature instead") captures the
            // chosen creature now into the shield's `redirect_target` field for
            // the applier to read back.
            shield = shield
                .redirection_shield(recipient, redirect_amount.unwrap_or(PreventionAmount::All));
            if recipient == DamageRedirectTarget::ChosenObjectTarget {
                // The redirect target is the LAST declared object slot — the
                // original-recipient slot (Jade Monolith) is declared first when
                // both are present, though no single card has both today.
                if let Some(id) = chosen_redirect_object(ability, recipient_consumes_slot) {
                    shield = shield.redirect_target(TargetFilter::SpecificObject { id });
                }
            }
        }
        (Some(_), Some(_)) | (None, None) => {
            return Err(EffectError::InvalidParam(
                "CreateDamageReplacement requires exactly one of modification / redirect_to"
                    .to_string(),
            ))
        }
    }

    // CR 614.1a + CR 514.2: The shield is a replacement effect with a "this
    // turn" lifetime (ends at cleanup, CR 514.2). Placement below is engine
    // plumbing — store it where `find_applicable_replacements` can reach it
    // (Battlefield/Command-zone objects + the pending registry):
    //   * Jade Monolith ("to target creature") → host on the chosen creature
    //     with `valid_card: SelfRef` so it fires only on damage to it.
    //   * A permanent source (Beacon / Soltari / Goblin Psychopath) → host on
    //     the source object on the battlefield.
    //   * An instant/sorcery source mid-resolution (Desperate Gambit) → host in
    //     the game-level pending registry so the shield outlives stack resolution.
    if let Some(host_id) = recipient_host {
        if shield.valid_card.is_none() {
            shield.valid_card = Some(TargetFilter::SelfRef);
        }
        if let Some(obj) = state.objects.get_mut(&host_id) {
            obj.replacement_definitions.push(shield);
        }
    } else {
        let is_permanent_on_battlefield = state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield);
        if is_permanent_on_battlefield {
            if let Some(obj) = state.objects.get_mut(&ability.source_id) {
                obj.replacement_definitions.push(shield);
            }
        } else {
            // CR 109.4 + CR 614.1a: Anchor the installing controller so a
            // controller-relative `damage_source_filter` (e.g. Desperate Gambit's
            // chosen "source you control" recheck) matches under the sentinel host.
            if shield.source_controller.is_none() {
                shield.source_controller = Some(ability.controller);
            }
            state.pending_damage_replacements.push(shield);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreateDamageReplacement,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// Return the `skip`-th object target declared for this ability, if any.
fn chosen_target_object(ability: &ResolvedAbility, skip: usize) -> Option<ObjectId> {
    ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .nth(skip)
}

/// Return the object target slot for a `ChosenObjectTarget` redirect recipient.
/// When the original recipient is itself a chosen target object (Jade Monolith —
/// `recipient_consumed_slot` is `true`), the redirect slot is the *second*
/// object target; otherwise (no recipient slot, or a self recipient like the
/// en-Kor cycle) it is the first.
fn chosen_redirect_object(
    ability: &ResolvedAbility,
    recipient_consumed_slot: bool,
) -> Option<ObjectId> {
    let skip = if recipient_consumed_slot { 1 } else { 0 };
    chosen_target_object(ability, skip)
}

/// CR 614.9: Resolve a redirection recipient to a concrete `TargetRef` against
/// the live game state, at damage-apply time. `Controller` → the replacement
/// source's controller; `SourceObject` → the source object itself;
/// `ChosenObjectTarget` → `chosen_object`, captured at resolution time into the
/// shield's `redirect_target` field (the shield host does not retain the
/// creating ability's targets, so the applier reads them back from there).
///
/// Used by `replacement::damage_done_applier` to rewrite the damage event's
/// recipient. Returns `None` when no concrete recipient can be resolved.
pub(crate) fn resolve_redirect_recipient(
    state: &GameState,
    recipient: DamageRedirectTarget,
    source_id: ObjectId,
    chosen_object: Option<ObjectId>,
) -> Option<TargetRef> {
    match recipient {
        DamageRedirectTarget::Controller => state
            .objects
            .get(&source_id)
            .map(|obj| TargetRef::Player(obj.controller)),
        DamageRedirectTarget::SourceObject => Some(TargetRef::Object(source_id)),
        DamageRedirectTarget::ChosenObjectTarget => chosen_object.map(TargetRef::Object),
    }
}

/// CR 614.9: A redirected-damage recipient is legal only if it is still a
/// battle, creature, or planeswalker on the battlefield (object recipients), or
/// still in the game (player recipients). On failure the redirection "does
/// nothing" — the damage is dealt to the original recipient. Mirrors the
/// `is_convoke_eligible` core-types-membership style in `game_object.rs`.
pub(crate) fn redirect_recipient_is_legal(state: &GameState, recipient: &TargetRef) -> bool {
    match recipient {
        TargetRef::Object(id) => state.objects.get(id).is_some_and(|obj| {
            obj.zone == Zone::Battlefield
                && (obj.card_types.core_types.contains(&CoreType::Creature)
                    || obj.card_types.core_types.contains(&CoreType::Planeswalker)
                    || obj.card_types.core_types.contains(&CoreType::Battle))
        }),
        // CR 614.9: a player recipient must still be in the game (not conceded).
        TargetRef::Player(pid) => state.players.iter().any(|p| p.id == *pid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::effects::deal_damage;
    use crate::game::zones::create_object;
    use crate::types::ability::{DamageModification, ShieldKind, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;

    fn create_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(state, CardId(1), owner, name.to_string(), Zone::Battlefield);
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        id
    }

    fn amount_oneshot_ability(source: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                // SelfRef source filter: the shield fires on damage dealt *by*
                // the shield host (Desperate Gambit's chosen source ≡ host here).
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: None,
                target_filter: None,
                modification: Some(DamageModification::Double),
                redirect_to: None,
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: None,
            },
            vec![],
            source,
            controller,
        )
    }

    /// CR 614.5 + CR 614.1a: A one-shot amount replacement doubles exactly one
    /// damage event, then is consumed — the *second* event from the same source
    /// is unmodified. This is the discriminating contract distinguishing a
    /// one-shot (Desperate Gambit) from a continuous static (Furnace of Rath).
    #[test]
    fn amount_oneshot_doubles_once_then_is_consumed() {
        let mut state = GameState::new_two_player(42);
        let source = create_creature(&mut state, PlayerId(0), "Chosen Source");

        let ability = amount_oneshot_ability(source, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Shield is hosted on the battlefield source object, tagged one-shot.
        let host = state.objects.get(&source).unwrap();
        assert_eq!(host.replacement_definitions.len(), 1);
        assert!(matches!(
            host.replacement_definitions[0].shield_kind,
            ShieldKind::DamageReplacementOneShot
        ));

        // First damage: 3 → doubled to 6 (opponent 20 → 14).
        let ctx = deal_damage::DamageContext::from_source(&state, source).unwrap();
        let mut events = Vec::new();
        let r1 = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(PlayerId(1)),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(r1, deal_damage::DamageResult::Applied(6)),
            "first event must double 3 → 6"
        );
        assert_eq!(state.players[1].life, 14);

        // Shield consumed after one use (CR 614.5).
        assert!(
            state.objects.get(&source).unwrap().replacement_definitions[0].is_consumed,
            "one-shot must be consumed after its single use"
        );

        // Second damage: 3 → UNMODIFIED 3 (one-shot spent). 14 → 11.
        let mut events = Vec::new();
        let r2 = deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(PlayerId(1)),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert!(
            matches!(r2, deal_damage::DamageResult::Applied(3)),
            "second event must NOT be doubled (one-shot consumed)"
        );
        assert_eq!(state.players[1].life, 11);
    }

    /// CR 614.9: A redirection one-shot redirects the recipient (here to the
    /// controller) and is consumed after one use.
    #[test]
    fn redirect_oneshot_redirects_recipient_then_is_consumed() {
        let mut state = GameState::new_two_player(42);
        // Damage source is controlled by player 0; redirect "to you" → player 0.
        let source = create_creature(&mut state, PlayerId(0), "Redirector");
        let victim = create_creature(&mut state, PlayerId(1), "Victim");

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(matches!(
            state.objects.get(&source).unwrap().replacement_definitions[0].shield_kind,
            ShieldKind::Redirection {
                recipient: DamageRedirectTarget::Controller,
                amount: PreventionAmount::All
            }
        ));

        // Damage 4 aimed at the opponent's creature is redirected to player 0
        // (the source's controller): the creature takes 0, player 0 loses 4.
        let ctx = deal_damage::DamageContext::from_source(&state, source).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(victim),
            4,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&victim).unwrap().damage_marked,
            0,
            "redirected damage must not land on the original creature"
        );
        assert_eq!(
            state.players[0].life, 16,
            "controller takes the 4 redirected damage"
        );
        assert!(
            state.objects.get(&source).unwrap().replacement_definitions[0].is_consumed,
            "redirection one-shot must be consumed after one use"
        );
    }

    /// CR 614.9: The en-Kor cycle — "the next N damage that would be dealt to ~
    /// this turn is dealt to target creature you control instead." The original
    /// recipient is the source itself (`recipient_object_filter: SelfRef`), so the
    /// shield is hosted on the source and fires on damage TO it; incoming damage
    /// is redirected to the chosen creature.
    #[test]
    fn redirect_oneshot_self_recipient_redirects_incoming_damage_to_chosen() {
        let mut state = GameState::new_two_player(42);
        let en_kor = create_creature(&mut state, PlayerId(0), "Nomads en-Kor");
        let chosen = create_creature(&mut state, PlayerId(0), "Chosen Creature");
        let attacker = create_creature(&mut state, PlayerId(1), "Attacker");

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: None,
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: Some(PreventionAmount::Next(1)),
                redirect_object_filter: Some(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature(),
                )),
                recipient_object_filter: Some(TargetFilter::SelfRef),
            },
            vec![TargetRef::Object(chosen)],
            en_kor,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 614.9: the shield is hosted on the en-Kor source (recipient `~`),
        // scoped to damage to it (valid_card SelfRef), redirecting to `chosen`.
        let host = state.objects.get(&en_kor).unwrap();
        assert_eq!(
            host.replacement_definitions.len(),
            1,
            "shield is hosted on the source, not the redirect target"
        );
        let shield = &host.replacement_definitions[0];
        assert!(matches!(
            shield.shield_kind,
            ShieldKind::Redirection {
                recipient: DamageRedirectTarget::ChosenObjectTarget,
                amount: PreventionAmount::Next(1)
            }
        ));
        assert_eq!(shield.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            shield.redirect_target,
            Some(TargetFilter::SpecificObject { id: chosen }),
            "the chosen creature is captured as the redirect recipient"
        );
        assert!(
            state
                .objects
                .get(&chosen)
                .unwrap()
                .replacement_definitions
                .is_empty(),
            "no shield is hosted on the redirect target"
        );

        // CR 614.9: only "the next 1 damage" is redirected. From a 3-damage
        // event, en-Kor still takes 2 and the chosen creature takes 1.
        let ctx = deal_damage::DamageContext::from_source(&state, attacker).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(en_kor),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&en_kor).unwrap().damage_marked,
            2,
            "only one damage is redirected away from the en-Kor creature"
        );
        assert_eq!(
            state.objects.get(&chosen).unwrap().damage_marked,
            1,
            "the chosen creature receives exactly the redirected damage"
        );
        assert!(
            state.objects.get(&en_kor).unwrap().replacement_definitions[0].is_consumed,
            "the one-shot is consumed after its single use"
        );
    }

    /// CR 614.7a: A source dealing 0 damage has no event to replace — the
    /// redirection does nothing and the shield is NOT consumed.
    #[test]
    fn redirect_oneshot_zero_damage_does_not_consume() {
        let mut state = GameState::new_two_player(42);
        let source = create_creature(&mut state, PlayerId(0), "Redirector");
        let victim = create_creature(&mut state, PlayerId(1), "Victim");

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let ctx = deal_damage::DamageContext::from_source(&state, source).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(victim),
            0,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(state.players[0].life, 20, "no damage means no redirection");
        assert!(
            !state.objects.get(&source).unwrap().replacement_definitions[0].is_consumed,
            "CR 614.7a: a 0-damage event must not spend the one-shot opportunity"
        );
    }

    /// CR 614.9: When the redirect recipient is an object no longer on the
    /// battlefield (illegal), the redirection does nothing — damage stays on the
    /// original recipient — but the spent one-shot is still consumed.
    #[test]
    fn redirect_oneshot_illegal_object_recipient_falls_through() {
        let mut state = GameState::new_two_player(42);
        let source = create_creature(&mut state, PlayerId(0), "Redirector");
        let victim = create_creature(&mut state, PlayerId(1), "Victim");
        let chosen = create_creature(&mut state, PlayerId(0), "Chosen Redirect Target");

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: None,
                redirect_object_filter: Some(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .with_type(crate::types::ability::TypeFilter::Creature),
                )),
                recipient_object_filter: None,
            },
            vec![TargetRef::Object(chosen)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        // The chosen object id is captured in redirect_target.
        assert_eq!(
            state.objects.get(&source).unwrap().replacement_definitions[0].redirect_target,
            Some(TargetFilter::SpecificObject { id: chosen })
        );

        // Move the chosen recipient off the battlefield → illegal.
        state.objects.get_mut(&chosen).unwrap().zone = Zone::Graveyard;

        let ctx = deal_damage::DamageContext::from_source(&state, source).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(victim),
            5,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&victim).unwrap().damage_marked,
            5,
            "illegal recipient → damage stays on the original victim"
        );
        assert!(
            state.objects.get(&source).unwrap().replacement_definitions[0].is_consumed,
            "the one-shot opportunity is spent even when redirection does nothing"
        );
    }

    /// CR 614.9: An amount-capped redirection with an illegal destination does
    /// nothing to the original event; the capped amount must not disappear.
    #[test]
    fn amount_capped_redirection_illegal_recipient_keeps_original_damage() {
        let mut state = GameState::new_two_player(42);
        let en_kor = create_creature(&mut state, PlayerId(0), "Nomads en-Kor");
        let chosen = create_creature(&mut state, PlayerId(0), "Chosen Creature");
        let attacker = create_creature(&mut state, PlayerId(1), "Attacker");

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: None,
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: Some(PreventionAmount::Next(1)),
                redirect_object_filter: Some(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature(),
                )),
                recipient_object_filter: Some(TargetFilter::SelfRef),
            },
            vec![TargetRef::Object(chosen)],
            en_kor,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        state.objects.get_mut(&chosen).unwrap().zone = Zone::Graveyard;

        let ctx = deal_damage::DamageContext::from_source(&state, attacker).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(en_kor),
            3,
            false,
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.objects.get(&en_kor).unwrap().damage_marked,
            3,
            "illegal redirect target means no damage is redirected or lost"
        );
        assert!(
            state.objects.get(&en_kor).unwrap().replacement_definitions[0].is_consumed,
            "the one-shot opportunity is still spent"
        );
    }

    /// Discriminating contrast: a continuous static `damage_modification`
    /// (Furnace of Rath shape, `ShieldKind::None`) doubles *every* event and is
    /// never consumed — proving the one-shot tagging is what gates consumption.
    #[test]
    fn continuous_static_doubles_every_event_and_is_never_consumed() {
        use crate::types::replacements::ReplacementEvent;
        let mut state = GameState::new_two_player(42);
        let source = create_creature(&mut state, PlayerId(0), "Furnace of Rath");
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::DamageDone)
                    .damage_modification(DamageModification::Double)
                    .damage_source_filter(TargetFilter::SelfRef),
            );

        let ctx = deal_damage::DamageContext::from_source(&state, source).unwrap();
        for _ in 0..2 {
            let mut events = Vec::new();
            let r = deal_damage::apply_damage_to_target(
                &mut state,
                &ctx,
                TargetRef::Player(PlayerId(1)),
                2,
                false,
                &mut events,
            )
            .unwrap();
            assert!(
                matches!(r, deal_damage::DamageResult::Applied(4)),
                "continuous static must double every event"
            );
        }
        assert!(
            !state.objects.get(&source).unwrap().replacement_definitions[0].is_consumed,
            "continuous static (ShieldKind::None) must never be consumed"
        );
    }

    fn chosen_source_redirect_ability(host: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                // "a source of your choice" → ChosenDamageSource.
                source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: None,
            },
            vec![],
            host,
            controller,
        )
    }

    /// CR 609.7a + CR 614.9 (Defect 2): An inline "a source of your choice"
    /// one-shot prompts the source choice when none is recorded, then on the
    /// continuation pass captures the chosen source into a DURABLE
    /// `SpecificObject` filter. The shield must then fire on the chosen source's
    /// damage and must NOT fire on a different source's damage — even though
    /// `last_chosen_damage_source` is cleared by the time damage is dealt.
    #[test]
    fn chosen_source_prompts_then_captures_durably_and_scopes_to_chosen_source() {
        use crate::types::game_state::{ChosenDamageSource, WaitingFor};
        let mut state = GameState::new_two_player(42);
        let host = create_creature(&mut state, PlayerId(0), "Beacon");
        let chosen_source = create_creature(&mut state, PlayerId(1), "Chosen Attacker");
        let other_source = create_creature(&mut state, PlayerId(1), "Other Attacker");

        // First pass: no source chosen yet → resolver prompts DamageSourceChoice
        // and stashes itself as a continuation.
        let ability = chosen_source_redirect_ability(host, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::DamageSourceChoice { options, .. } => {
                assert!(
                    options.contains(&chosen_source) && options.contains(&other_source),
                    "both candidate sources must be offered"
                );
            }
            other => panic!("expected DamageSourceChoice prompt, got {other:?}"),
        }
        assert!(
            state
                .objects
                .get(&host)
                .unwrap()
                .replacement_definitions
                .is_empty(),
            "no shield must be built until the source is chosen"
        );

        // Simulate the player's choice + the continuation drain: the handler sets
        // last_chosen_damage_source, then drains the stashed continuation (= this
        // resolver) while the choice is live, then clears it.
        state.last_chosen_damage_source = Some(ChosenDamageSource {
            source_id: chosen_source,
            source_filter: TargetFilter::ChosenDamageSource { filter: None },
        });
        let frame = state
            .take_active_ability_continuation()
            .expect("fixture cannot consume a buried continuation")
            .expect("self-continuation stashed");
        let mut events = Vec::new();
        resolve(&mut state, &frame.pending.chain, &mut events).unwrap();
        state.last_chosen_damage_source = None; // mirror the handler clearing it.

        // The shield captured a DURABLE SpecificObject filter for the chosen source.
        let shield = &state.objects.get(&host).unwrap().replacement_definitions[0];
        assert_eq!(
            shield.damage_source_filter,
            Some(TargetFilter::SpecificObject { id: chosen_source }),
            "chosen source must be captured durably, not left as ChosenDamageSource"
        );

        // Damage from the CHOSEN source is redirected to the controller (PlayerId 0).
        let victim = create_creature(&mut state, PlayerId(0), "Victim");
        let ctx = deal_damage::DamageContext::from_source(&state, chosen_source).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(victim),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&victim).unwrap().damage_marked,
            0,
            "chosen-source damage must be redirected away from the victim"
        );
        assert_eq!(
            state.players[0].life, 17,
            "controller takes the 3 redirected damage"
        );
    }

    /// CR 609.7b: A prior `ChooseDamageSource { You }` threads its candidate
    /// filter into the captured one-shot shield alongside the chosen object id.
    #[test]
    fn chosen_source_with_you_control_filter_threads_recheck() {
        use crate::types::ability::ControllerRef;
        use crate::types::game_state::ChosenDamageSource;

        let mut state = GameState::new_two_player(42);
        let source = create_creature(&mut state, PlayerId(0), "Chosen Source");
        let you_control = TargetFilter::Typed(
            crate::types::ability::TypedFilter::default().controller(ControllerRef::You),
        );
        state.last_chosen_damage_source = Some(ChosenDamageSource {
            source_id: source,
            source_filter: you_control.clone(),
        });

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
                combat_scope: None,
                target_filter: None,
                modification: Some(DamageModification::Double),
                redirect_to: None,
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: None,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        state.last_chosen_damage_source = None;

        assert_eq!(
            state.objects.get(&source).unwrap().replacement_definitions[0].damage_source_filter,
            Some(TargetFilter::And {
                filters: vec![TargetFilter::SpecificObject { id: source }, you_control],
            })
        );
    }

    /// CR 609.7a (Defect 2 negative): the chosen-source shield must NOT fire on a
    /// DIFFERENT source's damage.
    #[test]
    fn chosen_source_shield_ignores_other_sources() {
        use crate::types::game_state::ChosenDamageSource;
        let mut state = GameState::new_two_player(42);
        let host = create_creature(&mut state, PlayerId(0), "Beacon");
        let chosen_source = create_creature(&mut state, PlayerId(1), "Chosen Attacker");
        let other_source = create_creature(&mut state, PlayerId(1), "Other Attacker");
        let victim = create_creature(&mut state, PlayerId(0), "Victim");

        // Drive directly to the captured state (choice already made).
        state.last_chosen_damage_source = Some(ChosenDamageSource {
            source_id: chosen_source,
            source_filter: TargetFilter::ChosenDamageSource { filter: None },
        });
        let ability = chosen_source_redirect_ability(host, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        state.last_chosen_damage_source = None;

        // Damage from the OTHER source is unaffected — victim takes it, controller safe.
        let ctx = deal_damage::DamageContext::from_source(&state, other_source).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(victim),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&victim).unwrap().damage_marked,
            3,
            "non-chosen-source damage must NOT be redirected"
        );
        assert_eq!(state.players[0].life, 20, "controller must be untouched");
    }

    /// CR 115.1 + CR 614.9 (Defect 1): the "to target creature instead" redirect
    /// recipient is captured from `ability.targets` into the shield, and the
    /// redirect lands on that chosen creature. (Soltari Guerrillas.)
    #[test]
    fn redirect_to_target_creature_lands_on_chosen_creature() {
        let mut state = GameState::new_two_player(42);
        let host = create_creature(&mut state, PlayerId(0), "Soltari");
        let opponent_victim_player = PlayerId(1);
        let redirect_dest = create_creature(&mut state, PlayerId(0), "Chosen Redirect Creature");

        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: Some(crate::types::ability::CombatDamageScope::CombatOnly),
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: None,
                redirect_object_filter: Some(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .with_type(crate::types::ability::TypeFilter::Creature),
                )),
                recipient_object_filter: None,
            },
            // The targeting layer selected the redirect creature.
            vec![TargetRef::Object(redirect_dest)],
            host,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        let shield = &state.objects.get(&host).unwrap().replacement_definitions[0];
        assert_eq!(
            shield.redirect_target,
            Some(TargetFilter::SpecificObject { id: redirect_dest }),
            "redirect recipient must be captured from ability.targets"
        );

        // Combat damage from the host to the opponent is redirected to the chosen creature.
        let ctx = deal_damage::DamageContext::from_source(&state, host).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Player(opponent_victim_player),
            3,
            true,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.players[1].life, 20,
            "opponent must not take the redirected combat damage"
        );
        assert_eq!(
            state.objects.get(&redirect_dest).unwrap().damage_marked,
            3,
            "redirected combat damage must land on the chosen creature"
        );
    }

    /// CR 609.7a (Defect 2, END-TO-END through `apply`): the inline source-choice
    /// round-trip works through the REAL engine — the resolver prompts, the
    /// player's `GameAction::ChooseDamageSource` drains the stashed continuation,
    /// and the shield is built durably. Not a hand-simulated handler.
    #[test]
    fn chosen_source_round_trip_through_apply() {
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        let mut state = GameState::new_two_player(42);
        let host = create_creature(&mut state, PlayerId(0), "Beacon");
        let chosen_source = create_creature(&mut state, PlayerId(1), "Chosen Attacker");
        // Put the controller at priority so `apply` accepts the choice response.
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Resolver prompts (this is what the activated ability's resolution does).
        let ability = chosen_source_redirect_ability(host, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ));

        // Drive the REAL engine: player chooses the source. `apply` runs the
        // DamageSourceChoice handler → sets last_chosen_damage_source → drains
        // the stashed continuation (= our resolver) → builds the durable shield.
        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseDamageSource {
                source: chosen_source,
            },
        )
        .unwrap();

        // last_chosen_damage_source must be cleared by the handler, yet the shield
        // captured the concrete source durably.
        assert!(
            state.last_chosen_damage_source.is_none(),
            "handler must clear the transient choice"
        );
        let shield = &state.objects.get(&host).unwrap().replacement_definitions[0];
        assert_eq!(
            shield.damage_source_filter,
            Some(TargetFilter::SpecificObject { id: chosen_source }),
            "shield must capture the chosen source durably after the real round-trip"
        );

        // And it actually redirects the chosen source's damage to the controller.
        let victim = create_creature(&mut state, PlayerId(0), "Victim");
        let ctx = deal_damage::DamageContext::from_source(&state, chosen_source).unwrap();
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(victim),
            3,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(state.objects.get(&victim).unwrap().damage_marked, 0);
        assert_eq!(state.players[0].life, 17);
    }

    /// CR 614.9 (Nit 1, END-TO-END): Jade Monolith's "would deal damage to
    /// target creature ... that source deals that damage to you instead" must
    /// host on the chosen creature and fire ONLY on damage to it — damage to a
    /// DIFFERENT creature is untouched. This is the rules-correctness contract
    /// the dropped recipient scope violated.
    #[test]
    fn jade_monolith_recipient_scope_fires_only_on_chosen_creature() {
        use crate::types::game_state::ChosenDamageSource;
        let mut state = GameState::new_two_player(42);
        let host = create_creature(&mut state, PlayerId(0), "Jade Monolith");
        let chosen_source = create_creature(&mut state, PlayerId(1), "Attacker");
        let protected = create_creature(&mut state, PlayerId(0), "Protected");
        let bystander = create_creature(&mut state, PlayerId(0), "Bystander");

        // Source already chosen (covered separately by the source-choice tests).
        state.last_chosen_damage_source = Some(ChosenDamageSource {
            source_id: chosen_source,
            source_filter: TargetFilter::ChosenDamageSource { filter: None },
        });
        let ability = ResolvedAbility::new(
            Effect::CreateDamageReplacement {
                source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
                combat_scope: None,
                target_filter: None,
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: Some(TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default()
                        .with_type(crate::types::ability::TypeFilter::Creature),
                )),
            },
            // The targeting layer selected the protected creature.
            vec![TargetRef::Object(protected)],
            host,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        state.last_chosen_damage_source = None;

        // Shield must be hosted ON the protected creature, scoped via SelfRef —
        // NOT on Jade Monolith.
        assert!(
            state
                .objects
                .get(&host)
                .unwrap()
                .replacement_definitions
                .is_empty(),
            "shield must not host on Jade Monolith itself"
        );
        let shield = &state
            .objects
            .get(&protected)
            .unwrap()
            .replacement_definitions[0];
        assert_eq!(shield.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            shield.shield_kind,
            ShieldKind::Redirection {
                recipient: DamageRedirectTarget::Controller,
                amount: PreventionAmount::All
            }
        ));

        let ctx = deal_damage::DamageContext::from_source(&state, chosen_source).unwrap();

        // Damage from the chosen source to the BYSTANDER is NOT redirected.
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(bystander),
            2,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&bystander).unwrap().damage_marked,
            2,
            "damage to a non-targeted creature must NOT be redirected"
        );
        assert_eq!(
            state.players[0].life, 20,
            "controller untouched by bystander damage"
        );

        // Damage from the chosen source to the PROTECTED creature IS redirected
        // to the controller; the creature takes none.
        let mut events = Vec::new();
        deal_damage::apply_damage_to_target(
            &mut state,
            &ctx,
            TargetRef::Object(protected),
            5,
            false,
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects.get(&protected).unwrap().damage_marked,
            0,
            "damage to the targeted creature must be redirected away"
        );
        assert_eq!(
            state.players[0].life, 15,
            "controller takes the 5 redirected damage"
        );
    }
}
