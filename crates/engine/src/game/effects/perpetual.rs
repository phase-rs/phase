//! Digital-only Alchemy (no CR entry): `Effect::ApplyPerpetual` — apply a
//! "perpetually" modification that permanently edits a card and follows it
//! across every zone.
//!
//! Like [`super::intensify`], the change is recorded on the object
//! (`GameObject::perpetual_mods`) and edits a persistent characteristic, so it
//! survives zone changes and serialization. Increment 1 covers base
//! power/toughness ("perpetually become(s)/has base power and toughness P/T",
//! e.g. High Fae Prankster, Three Tree Battalion, Blood Age Muster).
//!
//! Target resolution routes through `resolved_targets` so ParentTarget anaphora
//! (Stationed/VehicleCrewed events, chain propagation) bind the correct object;
//! `Any` falls back to the source when no referent is available.

use crate::types::ability::{
    Effect, EffectError, EffectKind, ParentTargetMissingReason, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind};

/// CR 702.184a/702.122/702.171: object referent for Stationed/VehicleCrewed/Saddled
/// trigger anaphora while a triggered ability is resolving.
fn parent_object_from_trigger_event(
    event: Option<&GameEvent>,
) -> Option<crate::types::identifiers::ObjectId> {
    match event? {
        GameEvent::Stationed { creature_id, .. } => Some(*creature_id),
        GameEvent::VehicleCrewed { vehicle_id, .. } => Some(*vehicle_id),
        GameEvent::Saddled { mount_id, .. } => Some(*mount_id),
        _ => None,
    }
}

fn parent_object_from_resolution_trigger_context(
    state: &GameState,
) -> Option<crate::types::identifiers::ObjectId> {
    parent_object_from_trigger_event(state.current_trigger_event.as_ref())
        .or_else(|| {
            state
                .current_trigger_events
                .iter()
                .find_map(|event| parent_object_from_trigger_event(Some(event)))
        })
        .or_else(|| {
            state
                .resolving_stack_entry
                .as_ref()
                .and_then(|entry| match &entry.kind {
                    StackEntryKind::TriggeredAbility {
                        trigger_event: Some(te),
                        ..
                    } => parent_object_from_trigger_event(Some(te)),
                    _ => None,
                })
        })
}

fn perpetual_target_object_ids(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<crate::types::identifiers::ObjectId> {
    // CR 702.184a/702.122/702.171: Stationed/VehicleCrewed/Saddled anaphora
    // binds before propagated chain targets — a stale source-only fallback in
    // `ability.targets` must not beat the live trigger event.
    if matches!(target, TargetFilter::ParentTarget) {
        if let Some(id) = parent_object_from_resolution_trigger_context(state) {
            return vec![id];
        }
    }

    if !ability.targets.is_empty() {
        let propagated = super::effect_object_targets(target, &ability.targets);
        if !propagated.is_empty()
            && !(matches!(target, TargetFilter::ParentTarget) && propagated == [ability.source_id])
        {
            return propagated;
        }
    }

    // CR 609.3 + CR 608.2c: a ChooseFromZone with no eligible card has no
    // referent for a ParentTarget perpetual rider. This typed child-field
    // suppresses only that skipped-choice handoff, leaving the shared
    // unresolved ParentTarget source fallback intact for other effect shapes.
    if matches!(target, TargetFilter::ParentTarget)
        && ability.targets.is_empty()
        && ability.parent_target_missing_reason == Some(ParentTargetMissingReason::ChooseFromZone)
    {
        return Vec::new();
    }

    let effective_targets = crate::game::targeting::resolved_targets(ability, target, state);
    let mut ids = super::effect_object_targets(target, &effective_targets);

    if matches!(target, TargetFilter::ParentTarget) && ids == [ability.source_id] {
        if let Some(id) = parent_object_from_resolution_trigger_context(state) {
            return vec![id];
        }
    }

    // Mass perpetual over a non-battlefield zone (CR 601.2f hand-card cost grants:
    // "[type] cards in [your/that player's] hand perpetually gain …"). When the
    // filter pins an `InZone` other than the battlefield, enumerate that zone and
    // keep every object matching the filter — NOT a single declared target and
    // NOT the source fallback (the grant edits a set of cards, possibly empty, in
    // a hidden zone). Mirrors `layers::apply_continuous_effect_filtered`.
    if let Some(zone) = target.extract_in_zone() {
        if zone != crate::types::zones::Zone::Battlefield {
            let ctx = crate::game::filter::FilterContext::from_source(state, ability.source_id);
            return crate::game::targeting::zone_object_ids(state, zone)
                .into_iter()
                .filter(|&id| crate::game::filter::matches_target_filter(state, id, target, &ctx))
                .collect();
        }
    }

    // CR 609.3 ("If an effect attempts to do something impossible, it does only
    // as much as possible"): an anaphoric filter that resolved to NOTHING has no
    // referent, so the perpetual edit applies to nothing — it must NOT fall back
    // to the ability source, which is a different object entirely.
    //
    // `ParentTarget` ("it", the parent instruction's target) and `LastCreated`
    // ("it", the object the previous clause just made — `state.last_created_token_ids`,
    // see `publishes_chain_created_referent` in `oracle_effect/lower.rs`) are both
    // anaphors: they NAME an antecedent rather than describing a set. When the
    // antecedent does not exist — a `Conjure` that conjured zero cards, a token
    // producer that made none — the clause has no subject and does nothing.
    //
    // The source fallback below is for `TargetFilter::Any` and friends, where a
    // perpetual rider with no declared target genuinely means "this object"
    // (Mutable Pupa's self-grant).
    if ids.is_empty()
        && matches!(
            target,
            TargetFilter::ParentTarget | TargetFilter::LastCreated
        )
    {
        return ids;
    }

    if ids.is_empty() {
        ids.push(ability.source_id);
    }
    ids
}

/// Target resolution: uses the effect's `target` filter through the shared
/// `resolved_targets` machinery (ParentTarget event anaphora, chain propagation,
/// or source fallback for `Any`).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ApplyPerpetual {
        target,
        modification,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("ApplyPerpetual".to_string()));
    };
    let modification = modification.clone();
    let target = target.clone();

    let ids = perpetual_target_object_ids(state, ability, &target);
    let all_creature_types = &state.all_creature_types;

    let mut changed = false;
    for id in ids {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.apply_perpetual_modification(&modification, all_creature_types);
            changed = true;
        }
    }

    if changed {
        // CR 613.1: a perpetual edit to base power/toughness changes a
        // characteristic that the layer pass derives live P/T from, so the board
        // must be re-evaluated — otherwise `obj.power`/`obj.toughness` and public
        // state stay at their pre-effect values until some unrelated future
        // layer-dirtying event. The `Full` flush also marks public state dirty.
        crate::game::layers::mark_layers_full(state);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::game::effects::resolve_ability_chain;
    use crate::game::scenario::GameRunner;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        CardSelectionMode, Chooser, Effect, PerpetualModification, ResolvedAbility, TargetFilter,
        TargetRef, ZoneOwner,
    };
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn perpetual_sets_base_power_toughness_and_records_it() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Three Tree Battalion Duplicate".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().base_power = Some(5);
        state.objects.get_mut(&id).unwrap().base_toughness = Some(5);

        let modification = PerpetualModification::SetBasePowerToughness {
            power: 1,
            toughness: 1,
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.base_power, Some(1));
        assert_eq!(obj.base_toughness, Some(1));
        assert!(obj.perpetual_mods.contains(&modification));
    }

    /// CR 613.1: the perpetual base-P/T edit must dirty layers so the live,
    /// publicly visible `power`/`toughness` are recomputed at the next flush —
    /// not just the persistent `base_*` fields. Mirrors the rules/display
    /// boundary (`flush_layers`, a no-op unless `layers_dirty` is set).
    #[test]
    fn perpetual_base_pt_updates_live_pt_after_layer_flush() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "High Fae Prankster".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        // Establish the pre-effect live P/T through the normal layer pass.
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);
        assert_eq!(state.objects.get(&id).unwrap().power, Some(2));

        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: PerpetualModification::SetBasePowerToughness {
                    power: 4,
                    toughness: 1,
                },
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        // The resolver must have dirtied layers; flushing recomputes live P/T.
        crate::game::layers::flush_layers(&mut state);
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(1));
    }

    #[test]
    fn perpetual_modify_pt_adds_to_base_and_updates_live_pt() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Heir to Dragonfire".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
        }
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);

        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: PerpetualModification::ModifyPowerToughness {
                    power_delta: 3,
                    toughness_delta: 3,
                },
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        crate::game::layers::flush_layers(&mut state);
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.base_power, Some(4));
        assert_eq!(obj.base_toughness, Some(4));
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(4));
    }

    #[test]
    fn perpetual_grant_keywords_adds_to_object() {
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monoist Gravliner".to_string(),
            Zone::Battlefield,
        );

        let modification = PerpetualModification::GrantKeywords {
            keywords: vec![Keyword::Deathtouch, Keyword::Lifelink],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.has_keyword(&Keyword::Deathtouch));
        assert!(obj.has_keyword(&Keyword::Lifelink));
        assert!(obj.perpetual_mods.contains(&modification));
        assert!(obj.base_keywords.contains(&Keyword::Deathtouch));
        assert!(obj.base_keywords.contains(&Keyword::Lifelink));
    }

    #[test]
    fn perpetual_grant_keywords_survives_layer_flush() {
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monoist Gravliner".to_string(),
            Zone::Battlefield,
        );

        let modification = PerpetualModification::GrantKeywords {
            keywords: vec![Keyword::Deathtouch],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: crate::types::ability::TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::flush_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.has_keyword(&Keyword::Deathtouch));
        assert!(obj.base_keywords.contains(&Keyword::Deathtouch));
    }

    /// Karlach, Tiefling Berserker cycle shape: a quoted grant that classifies
    /// to `AddStaticMode` (CR 509.1b "can't block") must install onto BOTH the
    /// live `static_definitions` and the persistent `base_static_definitions`,
    /// mirroring `perpetual_grant_keywords_adds_to_object`.
    #[test]
    fn perpetual_grant_ability_installs_static_mode() {
        use crate::types::ability::PerpetualGrantModification;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Karlach, Tiefling Berserker".to_string(),
            Zone::Battlefield,
        );

        let modification = PerpetualModification::GrantAbility {
            modifications: vec![PerpetualGrantModification::AddStaticMode {
                mode: StaticMode::CantBlock,
            }],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::flush_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert!(
            obj.static_definitions
                .iter_all()
                .any(|sd| sd.mode == StaticMode::CantBlock),
            "CantBlock static must be live on the object"
        );
        assert!(
            obj.base_static_definitions
                .iter()
                .any(|sd| sd.mode == StaticMode::CantBlock),
            "CantBlock static must survive the layer flush via base_static_definitions"
        );
        assert!(obj.perpetual_mods.contains(&modification));
    }

    /// Agent of Raffine shape: a quoted grant that classifies to a NESTED
    /// `ContinuousModification::GrantAbility` (a full spell/activated ability,
    /// not a bare keyword or restriction static — "You may spend mana as
    /// though it were mana of any color to cast this spell.") must install
    /// the whole `AbilityDefinition` onto both `abilities` (so a from-hand
    /// cast sees it immediately) and `base_abilities` (so it survives the
    /// battlefield layer reset), and must not double-install on a second
    /// application of the identical modification (structural-equality dedup,
    /// mirroring the non-perpetual layer-6 `GrantAbility` apply).
    #[test]
    fn perpetual_grant_ability_installs_nested_ability_and_survives_layer_flush() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, PerpetualGrantModification};

        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Agent of Raffine".to_string(),
            Zone::Hand,
        );

        let granted = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        );
        let modification = PerpetualModification::GrantAbility {
            modifications: vec![PerpetualGrantModification::GrantAbility {
                definition: Box::new(granted.clone()),
            }],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        {
            let obj = state.objects.get(&id).unwrap();
            assert!(
                obj.abilities.iter().any(|a| a == &granted),
                "granted ability must be live on the object immediately (from-hand visibility)"
            );
            assert!(
                obj.base_abilities.iter().any(|a| a == &granted),
                "granted ability must be recorded on base_abilities"
            );
        }

        // Re-applying the identical modification (e.g. a second layer pass
        // re-deriving the same perpetual mod) must not duplicate the grant.
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.apply_perpetual_modification(&modification, &[]);
        }
        let obj = state.objects.get(&id).unwrap();
        assert_eq!(
            obj.abilities.iter().filter(|a| *a == &granted).count(),
            1,
            "re-applying the same grant must be idempotent"
        );
        assert_eq!(
            obj.base_abilities.iter().filter(|a| *a == &granted).count(),
            1,
            "re-applying the same grant must be idempotent on base_abilities too"
        );
    }

    /// End-to-end wiring test (not just parser shape / manual-target install):
    /// drives the REAL `Effect::Conjure` resolver first, then `ApplyPerpetual`
    /// with `TargetFilter::LastCreated`, and confirms the grant lands on the
    /// object `Conjure` actually just created — not on the ability's source —
    /// proving `state.last_created_token_ids` is genuinely consulted at
    /// grant-installation time (Agent of Raffine's "Conjure a duplicate ...
    /// into your hand. It perpetually gains ..." shape). The parser-level test
    /// (`perpetual_grant_after_conjure_binds_last_created_not_parent_target` in
    /// `parser::oracle_effect::tests`) proves the AST comes out right; this
    /// proves the runtime target resolution does too.
    #[test]
    fn perpetual_grant_after_conjure_installs_on_conjured_object_not_source() {
        use crate::types::ability::{ConjureCard, ConjureSource, PerpetualGrantModification};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(7);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Agent of Raffine".to_string(),
            Zone::Battlefield,
        );

        let conjure_ability = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Named {
                        name: "Conjured Duplicate".to_string(),
                    },
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Hand,
                tapped: false,
                library_position: None,
                library_players: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::conjure::resolve(&mut state, &conjure_ability, &mut events).unwrap();

        let conjured_id = *state
            .last_created_token_ids
            .first()
            .expect("Conjure must publish the conjured object as the chain-created referent");
        assert_ne!(
            conjured_id, source_id,
            "the conjured object must be a distinct object from the ability source"
        );

        let modification = PerpetualModification::GrantAbility {
            modifications: vec![PerpetualGrantModification::AddStaticMode {
                mode: StaticMode::CantBlock,
            }],
        };
        // `targets` is deliberately EMPTY here (not `vec![TargetRef::Object(conjured_id)]`):
        // `perpetual_target_object_ids` short-circuits on a non-empty `ability.targets`
        // and returns it directly WITHOUT ever calling `resolved_targets` — the
        // actual `TargetFilter::LastCreated => state.last_created_token_ids` lookup
        // this test exists to exercise. A real "It perpetually gains ..." clause
        // reaches this resolver with empty targets (Conjure declares none), so an
        // empty vec here is the production-faithful fixture, not a shortcut.
        let grant_ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::LastCreated,
                modification: modification.clone(),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        super::resolve(&mut state, &grant_ability, &mut events).unwrap();

        let conjured_obj = state.objects.get(&conjured_id).unwrap();
        assert!(
            conjured_obj
                .static_definitions
                .iter_all()
                .any(|sd| sd.mode == StaticMode::CantBlock),
            "the CONJURED object must receive the CantBlock grant"
        );
        let source_obj = state.objects.get(&source_id).unwrap();
        assert!(
            !source_obj
                .static_definitions
                .iter_all()
                .any(|sd| sd.mode == StaticMode::CantBlock),
            "the ability SOURCE must not receive the grant meant for the conjured duplicate"
        );
    }

    /// Runtime regression (PR #8494 Blocker 2, matthewevans): Agent of
    /// Raffine's exact granted body -- verified against MTGJSON's
    /// `AtomicCards.json` -- "{2}, {T}: Choose target opponent. Conjure a
    /// duplicate of the top card of their library into your hand. It
    /// perpetually gains \"You may spend mana as though it were mana of any
    /// color to cast this spell.\" Then they exile the top card of their
    /// library face down." routes its quoted grant body through
    /// `classify_quoted_inner`'s default `GrantAbility` fallback, wrapping
    /// `Effect::GenericEffect { static_abilities:
    /// [SpendManaAsAnyColor { spell_filter: None, .. }], target:
    /// Some(Controller), .. }`. Before this fix,
    /// `PerpetualGrantModification::try_from`'s `GrantAbility` arm rejected
    /// only an `Effect::Unimplemented`-bearing tree (Blocker 1); a
    /// `GenericEffect` is not `Unimplemented`, so it was ACCEPTED -- a green
    /// `Effect::ApplyPerpetual` that pushed the whole granted
    /// `AbilityDefinition` onto the conjured duplicate's
    /// `abilities`/`base_abilities` (wrongly board-wide in scope, AND never
    /// even checked: `static_abilities.rs`'s
    /// `player_can_spend_as_any_color_for_spell_object` only ever scans
    /// `game_active_statics` -- battlefield + command zone -- never hand or
    /// the stack, per CR 113.6e the very zones this self-cast concession
    /// would need to function in).
    ///
    /// Drives the REAL resolvers end to end: a real `Effect::Conjure`
    /// (mirroring `perpetual_grant_after_conjure_installs_on_conjured_object_not_source`
    /// above) produces the actual duplicate object with a real `{U}` mana
    /// cost copied from the duplicated card, the real parser classifies the
    /// exact quoted grant text, and -- matching production
    /// (`effects/mod.rs` dispatches `Effect::ApplyPerpetual` to
    /// `perpetual::resolve` and treats `Effect::Unimplemented` as a no-op,
    /// per `effects/mod.rs`'s `Effect::Unimplemented` arm) -- whatever the
    /// parse produces is applied (or not) exactly as a real game would. The
    /// duplicate is then CAST for real with only off-color mana in the pool.
    ///
    /// Mutation-tested: reverting the `types/ability.rs` gate makes
    /// `parse_effect` return `Effect::ApplyPerpetual` again, so this test's
    /// `match` takes the "install the modification" arm, the duplicate
    /// receives the extra granted ability, and the first assertion below
    /// (no extra ability on the duplicate) fails.
    #[test]
    fn perpetual_grant_ability_rejects_agent_of_raffine_spend_any_color_to_cast_this_spell() {
        use crate::game::scenario::GameScenario;
        use crate::types::ability::{ConjureCard, ConjureSource};
        use crate::types::actions::GameAction;
        use crate::types::card_type::CoreType;
        use crate::types::game_state::CastPaymentMode;
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let source_id = create_object(
            &mut scenario.state,
            CardId(1),
            PlayerId(0),
            "Agent of Raffine".to_string(),
            Zone::Battlefield,
        );
        // The card Agent of Raffine's ability duplicates -- a real object so
        // the conjured copy inherits a genuine mana cost.
        let filler_id = create_object(
            &mut scenario.state,
            CardId(2),
            PlayerId(1),
            "Filler Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let filler = scenario.state.objects.get_mut(&filler_id).unwrap();
            filler.card_types.core_types.push(CoreType::Creature);
            filler.base_card_types = filler.card_types.clone();
            let cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            };
            filler.mana_cost = cost.clone();
            // `Effect::Conjure`'s duplicate snapshot reads INTRINSIC copiable
            // values (`printed_cards::intrinsic_copiable_values`), which pulls
            // `base_mana_cost`, not the live `mana_cost` -- both must be set
            // or the conjured duplicate inherits the (unset, zero) default
            // cost instead of the filler's real {U}.
            filler.base_mana_cost = cost;
        }

        let conjure_ability = ResolvedAbility::new(
            Effect::Conjure {
                cards: vec![ConjureCard {
                    source: ConjureSource::Duplicate {
                        duplicate_of: TargetFilter::SpecificObject { id: filler_id },
                    },
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                }],
                destination: Zone::Hand,
                tapped: false,
                library_position: None,
                library_players: None,
            },
            vec![TargetRef::Object(filler_id)],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::conjure::resolve(&mut scenario.state, &conjure_ability, &mut events)
            .unwrap();

        let duplicate_id = *scenario
            .state
            .last_created_token_ids
            .first()
            .expect("Conjure must publish the conjured object as the chain-created referent");
        let abilities_before = scenario.state.objects[&duplicate_id].abilities.len();

        // MTGJSON AtomicCards.json (verified 2026-09-06): Agent of Raffine's
        // exact granted quoted body.
        let granted_text =
            "you may spend mana as though it were mana of any color to cast this spell.";
        let sentence = format!("that card perpetually gains \"{granted_text}\"");
        let effect = crate::parser::oracle_effect::parse_effect(&sentence);

        match effect {
            Effect::Unimplemented { .. } => {
                // Fail-closed: nothing is installed, matching production (the
                // whole clause never reaches Effect::ApplyPerpetual).
            }
            Effect::ApplyPerpetual { modification, .. } => {
                // Pre-fix behavior: install exactly as
                // effects/perpetual.rs::resolve would for this modification.
                scenario
                    .state
                    .objects
                    .get_mut(&duplicate_id)
                    .unwrap()
                    .apply_perpetual_modification(&modification, &[]);
            }
            other => panic!("expected ApplyPerpetual or Unimplemented, got {other:?}"),
        }

        {
            let duplicate = scenario.state.objects.get(&duplicate_id).unwrap();
            assert_eq!(
                duplicate.abilities.len(),
                abilities_before,
                "the conjured duplicate must receive no extra granted ability -- \
                 Agent of Raffine's mana concession must fail closed, not install \
                 a board-wide, never-checked no-op ability"
            );
            assert!(
                duplicate.perpetual_mods.is_empty(),
                "the conjured duplicate must record no perpetual modification"
            );
        }

        // Cast the actual recipient object with only off-color (Green) mana
        // in the pool. `CastPaymentMode::Auto` requires
        // `can_pay_cost_after_auto_tap` to prove the pool actually covers the
        // {U} cost before the cast is allowed to proceed at all
        // (`casting_costs.rs`); with only Green available it cannot, and the
        // action is rejected outright. If the (rejected) grant had somehow
        // leaked an any-color concession, {U} would be payable with only
        // Green in the pool and this cast would succeed; an honest failure
        // here proves no false green.
        scenario.with_mana_pool(
            PlayerId(0),
            vec![ManaUnit::new(
                ManaType::Green,
                ObjectId(9_999),
                false,
                vec![],
            )],
        );
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&duplicate_id].card_id;
        let result = runner.act(GameAction::CastSpell {
            object_id: duplicate_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        assert!(
            result.is_err(),
            "casting the conjured duplicate with only off-color mana must fail -- \
             no any-color concession may have leaked from the rejected grant, got {:?}",
            result
        );
        assert_eq!(
            runner.state().objects[&duplicate_id].zone,
            Zone::Hand,
            "a rejected cast must leave the card in hand, not on the stack"
        );
    }

    #[test]
    fn perpetual_grant_keywords_parent_target_uses_stationed_event() {
        use crate::types::ability::TargetFilter;
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let spacecraft = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monoist Gravliner".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Stationing Creature".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::Stationed {
            spacecraft_id: spacecraft,
            creature_id: creature,
            counters_added: 1,
        });

        let modification = PerpetualModification::GrantKeywords {
            keywords: vec![Keyword::Deathtouch, Keyword::Lifelink],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::ParentTarget,
                modification: modification.clone(),
            },
            vec![],
            spacecraft,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        let stationer = state.objects.get(&creature).unwrap();
        assert!(stationer.has_keyword(&Keyword::Deathtouch));
        assert!(stationer.has_keyword(&Keyword::Lifelink));
        assert!(!state
            .objects
            .get(&spacecraft)
            .unwrap()
            .has_keyword(&Keyword::Deathtouch));
    }

    #[test]
    fn perpetual_grant_keywords_parent_target_overrides_source_only_propagation() {
        use crate::types::ability::TargetFilter;
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        let spacecraft = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Monoist Gravliner".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Stationing Creature".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::Stationed {
            spacecraft_id: spacecraft,
            creature_id: creature,
            counters_added: 1,
        });

        let modification = PerpetualModification::GrantKeywords {
            keywords: vec![Keyword::Deathtouch],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::ParentTarget,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(spacecraft)],
            spacecraft,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state
            .objects
            .get(&creature)
            .unwrap()
            .has_keyword(&Keyword::Deathtouch));
        assert!(!state
            .objects
            .get(&spacecraft)
            .unwrap()
            .has_keyword(&Keyword::Deathtouch));
    }

    #[test]
    fn perpetual_parent_target_base_pt_uses_propagated_chain_target() {
        use crate::types::ability::TargetFilter;

        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Blood Age Muster".to_string(),
            Zone::Stack,
        );
        let duplicate = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Conjured Duplicate".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&duplicate).unwrap();
            obj.base_power = Some(5);
            obj.base_toughness = Some(5);
        }

        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::ParentTarget,
                modification: PerpetualModification::SetBasePowerToughness {
                    power: 2,
                    toughness: 2,
                },
            },
            vec![TargetRef::Object(duplicate)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&duplicate).unwrap();
        assert_eq!(obj.base_power, Some(2));
        assert_eq!(obj.base_toughness, Some(2));
        assert_eq!(state.objects.get(&source).unwrap().base_power, None);
    }

    #[test]
    fn empty_choose_from_zone_parent_target_perpetual_noops() {
        let mut state = GameState::new_two_player(7);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bloodsprout Talisman".to_string(),
            Zone::Battlefield,
        );

        let grant = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::ParentTarget,
                modification: PerpetualModification::GrantKeywords {
                    keywords: vec![Keyword::Menace],
                },
            },
            vec![],
            source,
            PlayerId(0),
        );
        let choose = ResolvedAbility::new(
            Effect::ChooseFromZone {
                count: 1,
                zone: Zone::Hand,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller.into(),
                candidate_source: crate::types::ability::ZoneChoiceCandidateSource::Legacy,
                reciprocal_role: None,
                up_to: false,
                selection: CardSelectionMode::Chosen,
                constraint: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .sub_ability(grant);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &choose, &mut events, 0).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert!(
            !obj.has_keyword(&Keyword::Menace),
            "empty ChooseFromZone must not bind ParentTarget to the source"
        );
        assert!(
            obj.perpetual_mods.is_empty(),
            "source must not receive a perpetual modification when no card was chosen"
        );
    }

    /// Blood Age Muster-style anaphor: the parsed conjure → "its base power and
    /// toughness perpetually become …" chain must bind ParentTarget to the
    /// conjured creature through forward_result propagation, not the spell source.
    #[test]
    fn parsed_conjure_its_perpetual_base_pt_binds_conjured_creature() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::effects::resolve_ability_chain;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::{AbilityKind, PtValue, TargetFilter};
        use crate::types::card::CardFace;
        use crate::types::card_type::{CardType, CoreType};
        use std::collections::HashMap;
        use std::sync::Arc;

        let def = parse_effect_chain(
            "conjure a card named Grizzly Bears onto the battlefield. \
             Its base power and toughness perpetually become 2/2.",
            AbilityKind::Spell,
        );

        let perpetual = def.sub_ability.as_ref().expect("conjure -> perpetual sub");
        match &*perpetual.effect {
            Effect::ApplyPerpetual {
                target,
                modification,
                ..
            } => {
                assert!(
                    matches!(target, TargetFilter::ParentTarget),
                    "Blood Age Muster-style 'its' must parse as ParentTarget, got {target:?}"
                );
                assert!(matches!(
                    modification,
                    PerpetualModification::SetBasePowerToughness {
                        power: 2,
                        toughness: 2,
                    }
                ));
            }
            other => panic!("expected ApplyPerpetual sub, got {other:?}"),
        }
        assert!(
            def.forward_result,
            "conjure parent must forward the conjured object to the perpetual sub"
        );

        let mut state = GameState::new_two_player(42);
        state.card_face_registry = Arc::new(HashMap::from([(
            "grizzly bears".to_string(),
            CardFace {
                name: "Grizzly Bears".to_string(),
                power: Some(PtValue::Fixed(5)),
                toughness: Some(PtValue::Fixed(5)),
                card_type: CardType {
                    core_types: vec![CoreType::Creature],
                    ..Default::default()
                },
                ..Default::default()
            },
        )]));

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Blood Age Muster".to_string(),
            Zone::Stack,
        );

        let resolved = build_resolved_from_def(&def, source, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &resolved, &mut events, 0).unwrap();
        crate::game::layers::flush_layers(&mut state);

        let conjured_id = state
            .battlefield
            .iter()
            .copied()
            .find(|&id| id != source && state.objects[&id].name == "Grizzly Bears")
            .expect("conjured Grizzly Bears must enter the battlefield");
        let conjured = state.objects.get(&conjured_id).unwrap();
        assert_eq!(conjured.base_power, Some(2));
        assert_eq!(conjured.base_toughness, Some(2));
        assert_eq!(conjured.power, Some(2));
        assert_eq!(conjured.toughness, Some(2));

        let spell = state.objects.get(&source).unwrap();
        assert!(
            spell.base_power != Some(2) || spell.base_toughness != Some(2),
            "spell source must not receive the perpetual base P/T edit"
        );
        assert!(
            spell.perpetual_mods.is_empty(),
            "spell source must not carry perpetual modifications"
        );
    }

    /// Three Tree Battalion end-to-end: cast the real parsed spell, put a
    /// library creature onto the battlefield, and verify the conjured duplicate
    /// — not the spell — receives the perpetual 1/1 base P/T edit.
    #[test]
    fn three_tree_battalion_perpetual_duplicate_base_pt_end_to_end() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::actions::GameAction;
        use crate::types::game_state::CastPaymentMode;
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;

        const ORACLE: &str = "Look at the top six cards of your library. You may put a \
            creature card with mana value 3 or less from among them onto the battlefield, \
            then conjure a duplicate of that card onto the battlefield. The duplicate \
            perpetually has base power and toughness 1/1. Put the rest on the bottom of \
            your library in a random order.";

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        for i in 0..5 {
            scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
        }
        let library_creature = scenario
            .add_spell_to_library_top(P0, "Grizzly Bears", false)
            .as_creature()
            .with_mana_cost(ManaCost::generic(2))
            .id();
        {
            let obj = scenario.state.objects.get_mut(&library_creature).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }

        let spell_id = scenario
            .add_spell_to_hand_from_oracle(P0, "Three Tree Battalion", true, ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            })
            .id();
        let card_id = scenario.state.objects[&spell_id].card_id;

        let parsed_perpetual_target = {
            let spell = &scenario.state.objects[&spell_id];
            let ability = spell
                .abilities
                .first()
                .expect("Three Tree Battalion must parse a spell ability");
            fn find_apply_perpetual(
                def: &crate::types::ability::AbilityDefinition,
            ) -> Option<TargetFilter> {
                if matches!(*def.effect, Effect::ApplyPerpetual { .. }) {
                    return match &*def.effect {
                        Effect::ApplyPerpetual { target, .. } => Some(target.clone()),
                        _ => None,
                    };
                }
                def.sub_ability
                    .as_deref()
                    .and_then(find_apply_perpetual)
                    .or_else(|| def.else_ability.as_deref().and_then(find_apply_perpetual))
            }
            find_apply_perpetual(ability).expect("parsed chain must contain ApplyPerpetual")
        };
        assert!(
            matches!(parsed_perpetual_target, TargetFilter::ParentTarget),
            "Three Tree Battalion 'the duplicate' must parse as ParentTarget"
        );

        scenario.with_mana_pool(
            P0,
            (0..5)
                .map(|_| ManaUnit::new(ManaType::White, ObjectId(9000), false, Vec::new()))
                .collect(),
        );

        let mut runner = scenario.build();
        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast Three Tree Battalion");

        drive_three_tree_battalion_resolution(&mut runner, library_creature);

        crate::game::layers::flush_layers(runner.state_mut());

        let state = runner.state();
        assert_eq!(
            state.objects[&spell_id].zone,
            Zone::Graveyard,
            "spell must resolve to the graveyard"
        );

        let original = state.objects.get(&library_creature).unwrap();
        assert_eq!(original.zone, Zone::Battlefield);
        assert_eq!(original.base_power, Some(2));
        assert_eq!(original.base_toughness, Some(2));

        let duplicate_id = state
            .battlefield
            .iter()
            .copied()
            .find(|&id| id != library_creature && state.objects[&id].name == "Grizzly Bears")
            .expect("conjured duplicate must be on the battlefield");
        let duplicate = state.objects.get(&duplicate_id).unwrap();
        assert_eq!(duplicate.base_power, Some(1));
        assert_eq!(duplicate.base_toughness, Some(1));
        assert_eq!(duplicate.power, Some(1));
        assert_eq!(duplicate.toughness, Some(1));
        assert!(duplicate.perpetual_mods.iter().any(|m| matches!(
            m,
            PerpetualModification::SetBasePowerToughness {
                power: 1,
                toughness: 1,
            }
        )));

        let spell = state.objects.get(&spell_id).unwrap();
        assert!(
            spell.base_power != Some(1) || spell.base_toughness != Some(1),
            "spell must not receive the duplicate's perpetual base P/T edit"
        );
        assert!(
            spell.perpetual_mods.is_empty(),
            "spell must not carry perpetual modifications"
        );

        // Both players pass — no further stack objects.
        assert!(
            state.stack.is_empty(),
            "stack must be empty after resolution"
        );
        let _ = P1;
    }

    fn drive_three_tree_battalion_resolution(runner: &mut GameRunner, library_creature: ObjectId) {
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;

        for _ in 0..80 {
            match runner.state().waiting_for.clone() {
                WaitingFor::OptionalEffectChoice { .. } => {
                    runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .expect("accept optional battlefield put");
                }
                WaitingFor::DigChoice { .. } => {
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![library_creature],
                        })
                        .expect("put the eligible library creature onto the battlefield");
                }
                WaitingFor::Priority { .. } => {
                    if runner.state().stack.is_empty() {
                        return;
                    }
                    runner.act(GameAction::PassPriority).expect("pass priority");
                    runner.act(GameAction::PassPriority).expect("pass priority");
                }
                WaitingFor::OrderTriggers { .. } => {
                    runner
                        .act(GameAction::OrderTriggers { order: vec![0] })
                        .ok();
                }
                _ if runner.state().stack.is_empty() => return,
                _ => {
                    runner.act(GameAction::PassPriority).ok();
                }
            }
        }
        panic!(
            "Three Tree Battalion resolution did not complete; waiting_for={:?}, stack={}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
    }

    /// CR 613.1d + CR 613.1f + CR 613.4b: perpetual type-change replaces
    /// creature subtypes, grants keywords, sets base P/T, and recomputes live
    /// characteristics after flush.
    #[test]
    fn perpetual_become_updates_types_pt_and_keywords_after_layer_flush() {
        use crate::types::ability::TargetFilter;
        use crate::types::card_type::CoreType;
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(7);
        state.all_creature_types =
            vec!["Pig".to_string(), "Boar".to_string(), "Spirit".to_string()];
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Second Little Pig".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Pig".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        crate::game::layers::mark_layers_full(&mut state);
        crate::game::layers::flush_layers(&mut state);

        let modification = PerpetualModification::Become {
            creature_subtypes: vec!["Boar".to_string(), "Spirit".to_string()],
            power: 4,
            toughness: 4,
            keywords: vec![Keyword::Flying],
        };
        let ability = ResolvedAbility::new(
            Effect::ApplyPerpetual {
                target: TargetFilter::Any,
                modification: modification.clone(),
            },
            vec![TargetRef::Object(id)],
            id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        super::resolve(&mut state, &ability, &mut events).unwrap();
        crate::game::layers::flush_layers(&mut state);

        let obj = state.objects.get(&id).unwrap();
        assert!(obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Boar")));
        assert!(obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Spirit")));
        assert!(!obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Pig")));
        assert_eq!(obj.power, Some(4));
        assert_eq!(obj.toughness, Some(4));
        assert!(obj.has_keyword(&Keyword::Flying));
        assert!(obj.perpetual_mods.contains(&modification));
    }
}
