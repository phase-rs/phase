//! Shared building blocks for reactive self-protection policies.
//!
//! Classifies "save yourself / your permanents" effect signatures and assesses
//! whether an immediate threat justifies spending a cost now. Consumed by
//! `ReactiveSelfProtectionPolicy` (spells + activations) and
//! `SacrificeLandProtectionPolicy` (land-sacrifice defensive outlets such as
//! Sylvan Safekeeper — issue #771).

use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, ControllerRef, Effect,
    StaticDefinition, TargetFilter,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

use engine::game::combat::get_valid_block_targets_for_player;
use engine::game::combat_damage::participates_in_pending_combat_damage_substep;
use engine::game::effects::effect::generic_effect_application_filter;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::functioning_abilities::active_static_definitions;
use engine::game::keywords::{object_has_effective_keyword_kind, source_matches_protection_target};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::TargetRef;
use engine::types::card_type::CoreType;
use engine::types::keywords::{HexproofFilter, KeywordKind, ProtectionTarget};

use crate::ability_chain::collect_chain_effects;
use crate::eval::threat_level;
use crate::features::landfall::ability_searches_library_for_land;
use crate::features::mana_ramp::target_filter_references_land;
use crate::policies::context::collect_ability_effects;
use crate::policies::effect_classify::{
    effect_polarity, extract_target_filter, lethal_to_creature, EffectPolarity,
};

/// Threat-level threshold above which protection casts/activations are unblocked.
pub(crate) const THREAT_FLOOR: f64 = 0.45;

/// Returns true if any of four threat signals is present:
///   - Stack contains an opponent-controlled object whose targets include
///     the AI player or any AI-controlled permanent (CR 117.1a).
///   - Stack contains an opponent-controlled untargeted mass-removal effect.
///   - The AI's own life total is below 40% of starting life.
///   - On the opponent's turn, some opponent's `threat_level` is at or above
///     `THREAT_FLOOR` (board pressure that can attack this turn).
pub(crate) fn any_immediate_threat(state: &GameState, ai_player: PlayerId) -> bool {
    if any_stack_targets_ai_or_ai_permanent(state, ai_player) {
        return true;
    }
    if any_stack_has_untargeted_mass_threat(state, ai_player) {
        return true;
    }
    let starting_life = state.format_config.starting_life.max(1) as f64;
    let life_ratio = state.players[ai_player.0 as usize].life as f64 / starting_life;
    if life_ratio < 0.4 {
        return true;
    }
    if state.active_player == ai_player {
        return false;
    }
    state.players.iter().any(|p| {
        if p.id == ai_player || p.is_eliminated {
            return false;
        }
        threat_level(state, ai_player, p.id) >= THREAT_FLOOR
    })
}

/// CR 508/509/510: protective grants have a real payoff during combat steps
/// where creatures are attacking, blocking, or dealing damage.
pub(crate) fn combat_step_allows_protection(state: &GameState) -> bool {
    matches!(
        state.phase,
        Phase::DeclareAttackers | Phase::DeclareBlockers | Phase::CombatDamage
    )
}

/// Effect-signature classifier: returns true when an `Effect` represents
/// "save yourself / your permanents."
pub(crate) fn is_self_protection_effect(effect: &Effect) -> bool {
    match effect {
        Effect::PhaseOut { target } => target_filter_self_scoped(target),
        Effect::PreventDamage { .. } => true,
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => static_abilities
            .iter()
            .any(|sd| static_definition_is_self_protection(sd, target.as_ref())),
        _ => false,
    }
}

/// True when any effect in the ability chain is a self-protection grant.
pub(crate) fn ability_grants_self_protection(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(|effect| is_self_protection_effect(effect))
}

/// CR 701.21: activated ability sacrifices a land (not a fetchland) to grant
/// self-protection — Sylvan Safekeeper and the whole "sacrifice a land: target
/// creature you control gains shroud until end of turn" class (issue #771).
pub(crate) fn is_land_sacrifice_self_protection_activation(ability: &AbilityDefinition) -> bool {
    use engine::types::ability::CostCategory;

    if !ability
        .cost_categories()
        .contains(&CostCategory::SacrificesPermanent)
    {
        return false;
    }
    if !cost_sacrifices_land(ability.cost.as_ref()) {
        return false;
    }
    if ability_searches_library_for_land(ability) {
        return false;
    }
    ability_grants_self_protection(ability)
}

fn static_definition_is_self_protection(
    sd: &StaticDefinition,
    parent_target: Option<&TargetFilter>,
) -> bool {
    let affects_self = match sd.affected.as_ref() {
        Some(TargetFilter::ParentTarget) => parent_target.is_some_and(target_filter_self_scoped),
        Some(f) => target_filter_self_scoped(f),
        None => false,
    };
    if !affects_self {
        return false;
    }
    if static_mode_is_defensive(&sd.mode) {
        return true;
    }
    sd.modifications.iter().any(modification_is_defensive)
}

fn static_mode_is_defensive(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::CantBeTargeted
            | StaticMode::CantBeBlocked
            | StaticMode::CantLoseLife
            | StaticMode::Protection
            | StaticMode::Shroud
            | StaticMode::Hexproof
    )
}

fn modification_is_defensive(m: &ContinuousModification) -> bool {
    match m {
        ContinuousModification::AddKeyword { keyword } => keyword_is_defensive(keyword),
        ContinuousModification::AddStaticMode { mode } => static_mode_is_defensive(mode),
        // CR 613.1f: Layer 6 applies ability-adding effects — inner static defs often
        // omit `affected` because the granted payload applies to ~.
        ContinuousModification::GrantAbility { definition } => {
            ability_has_defensive_payload(definition)
        }
        _ => false,
    }
}

fn static_definition_has_defensive_payload(sd: &StaticDefinition) -> bool {
    if static_mode_is_defensive(&sd.mode) {
        return true;
    }
    sd.modifications
        .iter()
        .any(modification_has_defensive_payload)
}

fn modification_has_defensive_payload(m: &ContinuousModification) -> bool {
    match m {
        ContinuousModification::AddKeyword { keyword } => keyword_is_defensive(keyword),
        ContinuousModification::AddStaticMode { mode } => static_mode_is_defensive(mode),
        ContinuousModification::GrantAbility { definition } => {
            ability_has_defensive_payload(definition)
        }
        _ => false,
    }
}

fn ability_has_defensive_payload(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(|effect| match effect {
            Effect::PreventDamage { .. } => true,
            Effect::GenericEffect {
                static_abilities, ..
            } => static_abilities
                .iter()
                .any(static_definition_has_defensive_payload),
            _ => false,
        })
}

fn keyword_is_defensive(keyword: &Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Indestructible
            | Keyword::Hexproof
            | Keyword::HexproofFrom(_)
            | Keyword::Shroud
            | Keyword::Protection(_)
    )
}

pub(crate) fn target_filter_self_scoped(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Controller | TargetFilter::SelfRef => true,
        TargetFilter::Typed(tf) => matches!(tf.controller, Some(ControllerRef::You)),
        _ => false,
    }
}

fn cost_sacrifices_land(cost: Option<&AbilityCost>) -> bool {
    match cost {
        None => false,
        Some(AbilityCost::Sacrifice(sacrifice)) => target_filter_references_land(&sacrifice.target),
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|c| cost_sacrifices_land(Some(c)))
        }
        _ => false,
    }
}

fn any_stack_has_untargeted_mass_threat(state: &GameState, ai_player: PlayerId) -> bool {
    use engine::types::zones::Zone;
    state.stack.iter().any(|entry| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        matches!(
            &ability.effect,
            Effect::DestroyAll { .. }
                | Effect::DamageAll { .. }
                | Effect::BounceAll { .. }
                | Effect::ChangeZoneAll {
                    destination: Zone::Exile | Zone::Graveyard | Zone::Hand,
                    ..
                }
        )
    })
}

fn any_stack_targets_ai_or_ai_permanent(state: &GameState, ai_player: PlayerId) -> bool {
    use engine::types::ability::TargetRef;
    state.stack.iter().any(|entry| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        ability.targets.iter().any(|t| match t {
            TargetRef::Player(pid) => *pid == ai_player,
            TargetRef::Object(obj_id) => state
                .objects
                .get(obj_id)
                .is_some_and(|obj| obj.controller == ai_player),
        })
    })
}

/// Defensive quality an activation would grant — used to match stack threats the
/// grant can actually answer.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DefensiveGrant {
    /// CR 702.18a / CR 702.11a: shroud or unqualified hexproof.
    CantBeTargeted,
    /// CR 702.11d: hexproof from a specific quality.
    HexproofFrom(HexproofFilter),
    /// CR 702.16: protection from a specific quality.
    Protection(ProtectionTarget),
    /// CR 702.12a: indestructible.
    Indestructible,
    /// CR 615.1a: Effects that use "prevent" are prevention effects.
    PreventDamage,
}

fn extract_defensive_grants(ability: &AbilityDefinition) -> Vec<DefensiveGrant> {
    let mut grants = Vec::new();
    for effect in collect_chain_effects(ability) {
        match effect {
            Effect::PreventDamage { .. } => grants.push(DefensiveGrant::PreventDamage),
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } => {
                for sd in static_abilities {
                    if !static_definition_affects_self_grant(sd, target.as_ref()) {
                        continue;
                    }
                    grants.extend(grant_from_static_mode(&sd.mode));
                    for m in &sd.modifications {
                        grants.extend(grants_from_modification(m));
                    }
                }
            }
            _ => {}
        }
    }
    grants
}

fn static_definition_affects_self_grant(
    sd: &StaticDefinition,
    parent_target: Option<&TargetFilter>,
) -> bool {
    match sd.affected.as_ref() {
        Some(TargetFilter::ParentTarget) => parent_target.is_some_and(target_filter_self_scoped),
        Some(f) => target_filter_self_scoped(f),
        None => false,
    }
}

fn grant_from_static_mode(mode: &StaticMode) -> Vec<DefensiveGrant> {
    match mode {
        StaticMode::CantBeTargeted | StaticMode::Shroud | StaticMode::Hexproof => {
            vec![DefensiveGrant::CantBeTargeted]
        }
        StaticMode::Protection => Vec::new(),
        _ => Vec::new(),
    }
}

fn grants_from_modification(m: &ContinuousModification) -> Vec<DefensiveGrant> {
    match m {
        ContinuousModification::AddKeyword { keyword } => grant_from_keyword(keyword),
        ContinuousModification::AddStaticMode { mode } => grant_from_static_mode(mode),
        ContinuousModification::GrantAbility { definition } => {
            extract_defensive_payload_grants(definition)
        }
        _ => Vec::new(),
    }
}

/// Extract defensive grants from a granted ability without re-requiring self-scope
/// on inner static definitions (see `modification_is_defensive` GrantAbility arm).
fn extract_defensive_payload_grants(ability: &AbilityDefinition) -> Vec<DefensiveGrant> {
    let mut grants = Vec::new();
    for effect in collect_chain_effects(ability) {
        match effect {
            Effect::PreventDamage { .. } => grants.push(DefensiveGrant::PreventDamage),
            Effect::GenericEffect {
                static_abilities, ..
            } => {
                for sd in static_abilities {
                    grants.extend(grant_from_static_mode(&sd.mode));
                    for m in &sd.modifications {
                        grants.extend(grants_from_modification(m));
                    }
                }
            }
            _ => {}
        }
    }
    grants
}

fn grant_from_keyword(keyword: &Keyword) -> Vec<DefensiveGrant> {
    match keyword {
        Keyword::Shroud | Keyword::Hexproof => vec![DefensiveGrant::CantBeTargeted],
        Keyword::HexproofFrom(filter) => vec![DefensiveGrant::HexproofFrom(filter.clone())],
        Keyword::Protection(pt) => vec![DefensiveGrant::Protection(pt.clone())],
        Keyword::Indestructible => vec![DefensiveGrant::Indestructible],
        _ => Vec::new(),
    }
}

/// CR 702.18a / CR 702.11a: targeting immunity answers only harmful effects
/// that select the protected permanent as a target — not player burn, beneficial
/// buffs, or untargeted mass removal.
fn any_stack_harmful_answerable_by_grants(
    state: &GameState,
    ai_player: PlayerId,
    grants: &[DefensiveGrant],
) -> bool {
    if grants.is_empty() {
        return false;
    }
    state.stack.iter().any(|entry| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        ability.targets.iter().any(|t| {
            let TargetRef::Object(obj_id) = t else {
                return false;
            };
            let Some(obj) = state.objects.get(obj_id) else {
                return false;
            };
            if obj.controller != ai_player
                || !obj.card_types.core_types.contains(&CoreType::Creature)
            {
                return false;
            }
            collect_ability_effects(ability).iter().any(|effect| {
                harmful_effect_answerable_by_grants(
                    effect,
                    grants,
                    obj,
                    state.objects.get(&entry.source_id),
                )
            })
        })
    })
}

fn harmful_effect_answerable_by_grants(
    effect: &Effect,
    grants: &[DefensiveGrant],
    protected: &engine::game::game_object::GameObject,
    source: Option<&engine::game::game_object::GameObject>,
) -> bool {
    if !matches!(effect_polarity(effect), EffectPolarity::Harmful) {
        return false;
    }
    grants
        .iter()
        .any(|grant| grant_answers_harmful_effect(grant, effect, protected, source))
}

fn grant_answers_harmful_effect(
    grant: &DefensiveGrant,
    effect: &Effect,
    protected: &engine::game::game_object::GameObject,
    source: Option<&engine::game::game_object::GameObject>,
) -> bool {
    match grant {
        DefensiveGrant::CantBeTargeted => harmful_effect_uses_object_targeting(effect),
        DefensiveGrant::HexproofFrom(filter) => {
            harmful_effect_uses_object_targeting(effect)
                && source.is_some_and(|src| hexproof_from_blocks_source(filter, protected, src))
        }
        DefensiveGrant::Protection(pt) => {
            harmful_effect_uses_object_targeting(effect)
                && source.is_some_and(|src| source_matches_protection_target(pt, protected, src))
        }
        DefensiveGrant::Indestructible => matches!(effect, Effect::Destroy { .. }),
        DefensiveGrant::PreventDamage => matches!(effect, Effect::DealDamage { .. }),
    }
}

/// Harmful single-target effects that select a permanent (answered by shroud /
/// hexproof / protection when the source is not exempt).
fn harmful_effect_uses_object_targeting(effect: &Effect) -> bool {
    !matches!(extract_target_filter(effect), Some(TargetFilter::Player))
        && extract_target_filter(effect).is_some()
}

fn hexproof_from_blocks_source(
    filter: &HexproofFilter,
    protected: &engine::game::game_object::GameObject,
    source: &engine::game::game_object::GameObject,
) -> bool {
    use engine::game::keywords::{source_matches_card_type, source_matches_quality};

    match filter {
        HexproofFilter::Color(color) => source.color.contains(color),
        HexproofFilter::CardType(type_name) => source_matches_card_type(source, type_name),
        HexproofFilter::Quality(quality) => source_matches_quality(source, quality),
        HexproofFilter::ChosenColor => protected
            .chosen_color()
            .is_some_and(|color| source.color.contains(&color)),
    }
}

#[derive(Debug, Clone)]
struct DefensiveOpportunity {
    grant: DefensiveGrant,
    recipients: Vec<ObjectId>,
}

/// Exact, effect-specific payoff for an object-scoped defensive grant.
///
/// `None` is deliberate: the classifier cannot prove the recipient or outcome,
/// so callers must preserve the existing fail-open behavior. This keeps player
/// protection, conditional grants, and other unsupported shapes playable while
/// allowing exact gating for the self-indestructible/shroud/hexproof/protection
/// family.
pub(crate) fn self_protection_effect_payoff(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effect: &Effect,
) -> Option<bool> {
    let opportunities = defensive_opportunities(state, ai_player, source_id, effect)?;
    let mut ambiguous = false;

    for opportunity in &opportunities {
        match stack_payoff_for_opportunity(state, ai_player, opportunity) {
            Some(true) => return Some(true),
            Some(false) => {}
            None => ambiguous = true,
        }
        match combat_payoff_for_opportunity(state, opportunity) {
            Some(true) => return Some(true),
            Some(false) => {}
            None => ambiguous = true,
        }
    }

    if ambiguous {
        None
    } else {
        Some(false)
    }
}

/// Exact activation payoff when every effect in the linear primary/sub-ability
/// chain is a supported object-scoped self-protection effect. Alternate and
/// unsupported branches fail open because the activation candidate has not yet
/// selected which branch will resolve.
pub(crate) fn self_protection_activation_payoff(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> Option<bool> {
    let mut node = Some(ability);
    while let Some(current) = node {
        if current.else_ability.is_some()
            || current.modal.is_some()
            || !current.mode_abilities.is_empty()
        {
            return None;
        }
        node = current.sub_ability.as_deref();
    }

    let effects = collect_chain_effects(ability);
    if effects.is_empty()
        || effects
            .iter()
            .any(|effect| !is_self_protection_effect(effect))
    {
        return None;
    }

    let mut ambiguous = false;
    for effect in effects {
        match self_protection_effect_payoff(state, ai_player, source_id, effect) {
            Some(true) => return Some(true),
            Some(false) => {}
            None => ambiguous = true,
        }
    }
    if ambiguous {
        None
    } else {
        Some(false)
    }
}

fn defensive_opportunities(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    effect: &Effect,
) -> Option<Vec<DefensiveOpportunity>> {
    let Effect::GenericEffect {
        static_abilities,
        target,
        ..
    } = effect
    else {
        return None;
    };
    if static_abilities.is_empty() {
        return None;
    }

    let mut opportunities = Vec::new();
    for static_def in static_abilities {
        if static_def.condition.is_some() || static_def.per_player_condition.is_some() {
            return None;
        }
        if !static_definition_is_self_protection(static_def, target.as_ref()) {
            return None;
        }
        if !matches!(
            &static_def.mode,
            StaticMode::Continuous
                | StaticMode::CantBeTargeted
                | StaticMode::Shroud
                | StaticMode::Hexproof
        ) && static_mode_is_defensive(&static_def.mode)
        {
            return None;
        }
        if static_def
            .modifications
            .iter()
            .any(|modification| !modification_is_defensive(modification))
        {
            return None;
        }

        let recipients = generic_effect_object_recipients(
            state,
            ai_player,
            source_id,
            target.as_ref(),
            static_def.affected.as_ref(),
        )?;
        let mut grants = grant_from_static_mode(&static_def.mode);
        for modification in &static_def.modifications {
            grants.extend(grants_from_modification(modification));
        }
        if grants.is_empty() {
            return None;
        }
        opportunities.extend(grants.into_iter().map(|grant| DefensiveOpportunity {
            grant,
            recipients: recipients.clone(),
        }));
    }
    Some(opportunities)
}

fn generic_effect_object_recipients(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    target: Option<&TargetFilter>,
    affected: Option<&TargetFilter>,
) -> Option<Vec<ObjectId>> {
    let application = generic_effect_application_filter(target, affected)?;

    if let Some(selection) = target.filter(|filter| !filter.is_context_ref()) {
        let legal = find_legal_targets(state, selection, ai_player, source_id);
        if legal
            .iter()
            .any(|target| matches!(target, TargetRef::Player(_)))
        {
            return None;
        }
        return Some(
            legal
                .into_iter()
                .filter_map(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                })
                .collect(),
        );
    }

    match application {
        TargetFilter::SelfRef => Some(vec![source_id]),
        TargetFilter::SpecificObject { id } => Some(vec![*id]),
        TargetFilter::Typed(typed)
            if typed.controller.as_ref() == Some(&ControllerRef::You)
                && (!typed.type_filters.is_empty() || !typed.properties.is_empty()) =>
        {
            let ctx = FilterContext::from_source_with_controller(source_id, ai_player);
            Some(
                state
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|id| matches_target_filter(state, *id, application, &ctx))
                    .collect(),
            )
        }
        _ => None,
    }
}

fn stack_payoff_for_opportunity(
    state: &GameState,
    ai_player: PlayerId,
    opportunity: &DefensiveOpportunity,
) -> Option<bool> {
    let mut ambiguous = false;
    for entry in &state.stack {
        if entry.controller == ai_player {
            continue;
        }
        let Some(root) = entry.ability() else {
            continue;
        };
        let source = state.objects.get(&entry.source_id);
        let mut node = Some(root);
        while let Some(ability) = node {
            if ability.condition.is_some() || ability.else_ability.is_some() {
                ambiguous = true;
            } else {
                for target in &ability.targets {
                    let TargetRef::Object(recipient_id) = target else {
                        continue;
                    };
                    if !opportunity.recipients.contains(recipient_id)
                        || grant_already_effective(state, *recipient_id, &opportunity.grant)
                    {
                        continue;
                    }
                    match grant_answers_targeted_effect(
                        state,
                        &opportunity.grant,
                        &ability.effect,
                        *recipient_id,
                        source,
                    ) {
                        Some(true) => return Some(true),
                        Some(false) => {}
                        None => ambiguous = true,
                    }
                }

                if let Some(filter) = harmful_mass_filter(&ability.effect) {
                    let ctx = FilterContext::from_source_with_controller(
                        entry.source_id,
                        entry.controller,
                    );
                    for recipient_id in &opportunity.recipients {
                        if grant_already_effective(state, *recipient_id, &opportunity.grant)
                            || !matches_target_filter(state, *recipient_id, filter, &ctx)
                        {
                            continue;
                        }
                        match grant_answers_mass_effect(
                            state,
                            &opportunity.grant,
                            &ability.effect,
                            *recipient_id,
                            source,
                        ) {
                            Some(true) => return Some(true),
                            Some(false) => {}
                            None => ambiguous = true,
                        }
                    }
                }
            }
            node = ability.sub_ability.as_deref();
        }
    }
    if ambiguous {
        None
    } else {
        Some(false)
    }
}

fn grant_answers_targeted_effect(
    state: &GameState,
    grant: &DefensiveGrant,
    effect: &Effect,
    recipient_id: ObjectId,
    source: Option<&engine::game::game_object::GameObject>,
) -> Option<bool> {
    if matches!(
        effect,
        Effect::Fight { .. }
            | Effect::EachDealsDamageEqualToPower { .. }
            | Effect::EachSourceDealsDamage { .. }
    ) {
        return None;
    }
    let protected = state.objects.get(&recipient_id)?;
    match effect_polarity(effect) {
        EffectPolarity::Beneficial => return Some(false),
        EffectPolarity::Contextual => {
            let source = source?;
            // CR 303.4a + CR 702.18a: an Aura spell targets, so shroud (and
            // matching hexproof/protection) can make its announced target illegal.
            if !source
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Aura")
            {
                return None;
            }
            return match grant {
                DefensiveGrant::CantBeTargeted => Some(true),
                DefensiveGrant::HexproofFrom(filter) => {
                    Some(hexproof_from_blocks_source(filter, protected, source))
                }
                DefensiveGrant::Protection(protection) => Some(source_matches_protection_target(
                    protection, protected, source,
                )),
                DefensiveGrant::Indestructible | DefensiveGrant::PreventDamage => None,
            };
        }
        EffectPolarity::Harmful => {}
    }
    match grant {
        DefensiveGrant::CantBeTargeted => Some(harmful_effect_uses_object_targeting(effect)),
        DefensiveGrant::HexproofFrom(filter) => Some(
            harmful_effect_uses_object_targeting(effect)
                && source
                    .is_some_and(|source| hexproof_from_blocks_source(filter, protected, source)),
        ),
        DefensiveGrant::Protection(protection) => {
            let targeting_answer = harmful_effect_uses_object_targeting(effect)
                && source.is_some_and(|source| {
                    source_matches_protection_target(protection, protected, source)
                });
            if targeting_answer {
                Some(true)
            } else if matches!(
                effect,
                Effect::DealDamage {
                    damage_source: Some(_),
                    ..
                }
            ) {
                None
            } else {
                Some(false)
            }
        }
        DefensiveGrant::Indestructible => match effect {
            Effect::Destroy { .. } => Some(true),
            Effect::DealDamage {
                damage_source: Some(_),
                ..
            } => None,
            Effect::DealDamage { .. } => {
                if damage_becomes_marked(state, source) == Some(false) {
                    return if source_has_effective_deathtouch(state, source) {
                        None
                    } else {
                        Some(false)
                    };
                }
                match lethal_to_creature(state, recipient_id, &[effect]) {
                    Some(true) if damage_becomes_marked(state, source) == Some(true) => Some(true),
                    Some(true) | Some(false) | None => None,
                }
            }
            _ => Some(false),
        },
        DefensiveGrant::PreventDamage => None,
    }
}

/// CR 702.2b + CR 704.5h: damage from a deathtouch source destroys a
/// positive-toughness creature as an SBA, which indestructible can prevent.
fn source_has_effective_deathtouch(
    state: &GameState,
    source: Option<&engine::game::game_object::GameObject>,
) -> bool {
    source.is_some_and(|source| {
        object_has_effective_keyword_kind(state, source.id, KeywordKind::Deathtouch)
    })
}

fn damage_may_make_indestructible_relevant(
    state: &GameState,
    source: Option<&engine::game::game_object::GameObject>,
) -> bool {
    damage_becomes_marked(state, source) != Some(false)
        || source_has_effective_deathtouch(state, source)
}

/// Whether damage from this source uses ordinary marked-damage semantics.
///
/// CR 120.3d-e: wither/infect damage to creatures becomes -1/-1 counters;
/// other creature damage is marked. CR 702.12b does not let indestructible
/// prevent a creature from dying for having toughness 0 or less.
fn damage_becomes_marked(
    state: &GameState,
    source: Option<&engine::game::game_object::GameObject>,
) -> Option<bool> {
    let source = source?;
    Some(
        !object_has_effective_keyword_kind(state, source.id, KeywordKind::Wither)
            && !object_has_effective_keyword_kind(state, source.id, KeywordKind::Infect),
    )
}

fn harmful_mass_filter(effect: &Effect) -> Option<&TargetFilter> {
    use engine::types::zones::Zone;
    match effect {
        Effect::DestroyAll { target, .. }
        | Effect::DamageAll { target, .. }
        | Effect::BounceAll { target, .. } => Some(target),
        Effect::ChangeZoneAll {
            destination: Zone::Exile | Zone::Graveyard | Zone::Hand,
            target,
            ..
        } => Some(target),
        _ => None,
    }
}

fn grant_answers_mass_effect(
    state: &GameState,
    grant: &DefensiveGrant,
    effect: &Effect,
    recipient_id: ObjectId,
    source: Option<&engine::game::game_object::GameObject>,
) -> Option<bool> {
    let protected = state.objects.get(&recipient_id)?;
    match (grant, effect) {
        (DefensiveGrant::Indestructible, Effect::DestroyAll { .. }) => Some(true),
        (
            DefensiveGrant::Indestructible,
            Effect::DamageAll {
                damage_source: Some(_),
                ..
            },
        ) => None,
        (DefensiveGrant::Indestructible, Effect::DamageAll { .. }) => {
            if damage_becomes_marked(state, source) == Some(false) {
                return if source_has_effective_deathtouch(state, source) {
                    None
                } else {
                    Some(false)
                };
            }
            match lethal_to_creature(state, recipient_id, &[effect]) {
                Some(true) if damage_becomes_marked(state, source) == Some(true) => Some(true),
                Some(true) | Some(false) | None => None,
            }
        }
        (
            DefensiveGrant::Protection(_),
            Effect::DamageAll {
                damage_source: Some(_),
                ..
            },
        ) => None,
        (DefensiveGrant::Protection(protection), Effect::DamageAll { .. }) => {
            source.map(|source| source_matches_protection_target(protection, protected, source))
        }
        (DefensiveGrant::PreventDamage, Effect::DamageAll { .. }) => None,
        _ => Some(false),
    }
}

fn grant_already_effective(
    state: &GameState,
    recipient_id: ObjectId,
    grant: &DefensiveGrant,
) -> bool {
    let Some(object) = state.objects.get(&recipient_id) else {
        return false;
    };
    match grant {
        DefensiveGrant::CantBeTargeted => {
            object.has_keyword(&Keyword::Shroud)
                || object.has_keyword(&Keyword::Hexproof)
                || active_static_definitions(state, object)
                    .any(|def| matches!(&def.mode, StaticMode::CantBeTargeted))
        }
        DefensiveGrant::HexproofFrom(filter) => {
            object.has_keyword(&Keyword::Shroud)
                || object.has_keyword(&Keyword::Hexproof)
                || object
                    .keywords
                    .contains(&Keyword::HexproofFrom(filter.clone()))
                || active_static_definitions(state, object)
                    .any(|def| matches!(&def.mode, StaticMode::CantBeTargeted))
        }
        DefensiveGrant::Protection(protection) => object.keywords.iter().any(|keyword| {
            matches!(
                keyword,
                Keyword::Protection(existing)
                    if existing == protection || *existing == ProtectionTarget::Everything
            )
        }),
        DefensiveGrant::Indestructible => object.has_keyword(&Keyword::Indestructible),
        DefensiveGrant::PreventDamage => false,
    }
}

fn combat_pair_ids(
    combat: &engine::game::combat::CombatState,
    recipient_id: ObjectId,
) -> Vec<ObjectId> {
    combat
        .blocker_assignments
        .get(&recipient_id)
        .into_iter()
        .flatten()
        .chain(
            combat
                .blocker_to_attacker
                .get(&recipient_id)
                .into_iter()
                .flatten(),
        )
        .copied()
        .collect()
}

fn combat_payoff_for_opportunity(
    state: &GameState,
    opportunity: &DefensiveOpportunity,
) -> Option<bool> {
    let Some(combat) = state.combat.as_ref() else {
        return Some(false);
    };
    match state.phase {
        // CR 509.1a-b: before blockers are declared, protection can remove a
        // genuinely legal blocker from the defending player's choices.
        Phase::DeclareAttackers => {
            let DefensiveGrant::Protection(protection) = &opportunity.grant else {
                return Some(false);
            };
            for recipient_id in &opportunity.recipients {
                if grant_already_effective(state, *recipient_id, &opportunity.grant) {
                    continue;
                }
                let Some(attacker) = combat
                    .attackers
                    .iter()
                    .find(|attacker| attacker.object_id == *recipient_id)
                else {
                    continue;
                };
                let blockers = get_valid_block_targets_for_player(state, attacker.defending_player);
                for (blocker_id, targets) in blockers {
                    if !targets.contains(recipient_id) {
                        continue;
                    }
                    let Some(protected) = state.objects.get(recipient_id) else {
                        continue;
                    };
                    if state.objects.get(&blocker_id).is_some_and(|blocker| {
                        source_matches_protection_target(protection, protected, blocker)
                    }) {
                        return Some(true);
                    }
                }
            }
            Some(false)
        }
        // CR 509.1g + CR 510.1: after declaration, only actual attacker/blocker
        // pairs can create a combat payoff. Indestructible lethality is not
        // derivable from pairing alone, so that case deliberately fails open.
        Phase::DeclareBlockers => {
            let mut saw_indestructible_pair = false;
            for recipient_id in &opportunity.recipients {
                if grant_already_effective(state, *recipient_id, &opportunity.grant) {
                    continue;
                }
                let paired_ids = combat_pair_ids(combat, *recipient_id);
                if paired_ids.is_empty() {
                    continue;
                }
                match &opportunity.grant {
                    DefensiveGrant::Protection(protection) => {
                        let Some(protected) = state.objects.get(recipient_id) else {
                            continue;
                        };
                        if paired_ids.iter().any(|paired_id| {
                            state.objects.get(paired_id).is_some_and(|paired| {
                                source_matches_protection_target(protection, protected, paired)
                            })
                        }) {
                            return Some(true);
                        }
                    }
                    DefensiveGrant::Indestructible => {
                        saw_indestructible_pair |= paired_ids.iter().any(|paired_id| {
                            damage_may_make_indestructible_relevant(
                                state,
                                state.objects.get(paired_id),
                            )
                        });
                    }
                    DefensiveGrant::CantBeTargeted
                    | DefensiveGrant::HexproofFrom(_)
                    | DefensiveGrant::PreventDamage => {}
                }
            }
            if saw_indestructible_pair {
                None
            } else {
                Some(false)
            }
        }
        // CR 510.4: before regular damage completes, first/double-strike
        // participation is source-specific. Fail open only for a protected
        // combatant whose grant could matter against an actual paired source.
        Phase::CombatDamage if !combat.regular_damage_done => {
            let mut ambiguous = false;
            for recipient_id in &opportunity.recipients {
                if grant_already_effective(state, *recipient_id, &opportunity.grant) {
                    continue;
                }
                let paired_ids: Vec<_> = combat_pair_ids(combat, *recipient_id)
                    .into_iter()
                    .filter(|paired_id| {
                        participates_in_pending_combat_damage_substep(state, *paired_id)
                    })
                    .collect();
                if paired_ids.is_empty() {
                    continue;
                }
                match &opportunity.grant {
                    DefensiveGrant::Protection(protection) => {
                        let Some(protected) = state.objects.get(recipient_id) else {
                            continue;
                        };
                        if paired_ids.iter().any(|paired_id| {
                            state.objects.get(paired_id).is_some_and(|paired| {
                                source_matches_protection_target(protection, protected, paired)
                            })
                        }) {
                            return Some(true);
                        }
                    }
                    DefensiveGrant::Indestructible => {
                        ambiguous |= paired_ids.iter().any(|paired_id| {
                            damage_may_make_indestructible_relevant(
                                state,
                                state.objects.get(paired_id),
                            )
                        });
                    }
                    DefensiveGrant::PreventDamage => ambiguous = true,
                    DefensiveGrant::CantBeTargeted | DefensiveGrant::HexproofFrom(_) => {}
                }
            }
            if ambiguous {
                None
            } else {
                Some(false)
            }
        }
        Phase::CombatDamage => Some(false),
        _ => Some(false),
    }
}

/// Whether a land-sacrifice self-protection activation has a concrete payoff
/// right now. Requires a harmful stack effect answerable by the actual grant;
/// protection also has combat-step payoff (CR 509.1b color dodge). Deliberately
/// excludes low life, board pressure, and untargeted mass effects — sacrificing
/// a land to shroud one creature does not answer those threats.
pub(crate) fn any_land_sacrifice_protection_payoff(
    state: &GameState,
    ai_player: PlayerId,
    ability: &AbilityDefinition,
) -> bool {
    let grants = extract_defensive_grants(ability);
    if any_stack_harmful_answerable_by_grants(state, ai_player, &grants) {
        return true;
    }
    if ability_grants_combat_step_protection(ability) && combat_step_allows_protection(state) {
        return true;
    }
    false
}

/// Protection-from-color grants can matter during combat (dodge a blocker).
fn ability_grants_combat_step_protection(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(|effect| match effect {
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } => static_abilities.iter().any(|sd| {
                let affects_self = match sd.affected.as_ref() {
                    Some(TargetFilter::ParentTarget) => {
                        target.as_ref().is_some_and(target_filter_self_scoped)
                    }
                    Some(f) => target_filter_self_scoped(f),
                    None => false,
                };
                affects_self
                    && sd.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AddKeyword {
                                keyword: Keyword::Protection(_)
                            }
                        )
                    })
            }),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{AbilityKind, ControllerRef, TypedFilter};
    use engine::types::keywords::ProtectionTarget;

    fn grant_effect(
        affected: Option<TargetFilter>,
        target: Option<TargetFilter>,
        keyword: Keyword,
    ) -> Effect {
        use engine::types::ability::StaticDefinition;
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected.unwrap_or(TargetFilter::ParentTarget))
                .modifications(vec![ContinuousModification::AddKeyword { keyword }])],
            target,
            duration: None,
        }
    }

    #[test]
    fn classifier_recognises_parent_target_shroud_grant() {
        assert!(is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            )),
            Keyword::Shroud,
        )));
    }

    #[test]
    fn classifier_recognises_static_mode_shroud() {
        use engine::types::ability::StaticDefinition;
        let effect = Effect::GenericEffect {
            static_abilities: vec![
                StaticDefinition::new(StaticMode::Shroud).affected(TargetFilter::ParentTarget)
            ],
            target: Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
            duration: None,
        };
        assert!(is_self_protection_effect(&effect));
    }

    #[test]
    fn classifier_recognises_grant_ability_wrapped_shroud() {
        use engine::types::ability::{AbilityDefinition, StaticDefinition};
        let inner = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous().modifications(vec![
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Shroud,
                    },
                ])],
                target: None,
                duration: None,
            },
        );
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::ParentTarget)
                .modifications(vec![ContinuousModification::GrantAbility {
                    definition: Box::new(inner),
                }])],
            target: Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
            duration: None,
        };
        assert!(is_self_protection_effect(&effect));
    }

    #[test]
    fn land_sacrifice_classifier_matches_safekeeper_shape() {
        use engine::types::ability::{SacrificeCost, SacrificeRequirement};
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                Keyword::Shroud,
            ),
        );
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost {
            target: TargetFilter::Typed(TypedFilter::new(engine::types::ability::TypeFilter::Land)),
            requirement: SacrificeRequirement::count(1),
        }));
        assert!(is_land_sacrifice_self_protection_activation(&ability));
    }

    #[test]
    fn land_sacrifice_classifier_rejects_fetchland() {
        use engine::types::ability::{
            ControllerRef, QuantityExpr, SacrificeCost, SearchSelectionConstraint,
        };
        use engine::types::zones::Zone;
        let search = Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::land()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
            source_zones: vec![Zone::Library],
        };
        let put_in_play = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        );
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, search);
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::SelfRef,
            1,
        )));
        ability.sub_ability = Some(Box::new(put_in_play));
        assert!(!is_land_sacrifice_self_protection_activation(&ability));
    }

    #[test]
    fn protection_keyword_grant_is_self_scoped() {
        assert!(is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            )),
            Keyword::Protection(ProtectionTarget::ChosenColor),
        )));
    }

    #[test]
    fn shroud_payoff_requires_harmful_creature_target_not_player_burn() {
        use engine::types::ability::{
            ResolvedAbility, SacrificeCost, SacrificeRequirement, TargetRef, TypeFilter,
        };
        use engine::types::game_state::{StackEntry, StackEntryKind};
        use engine::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        state.players[ai.0 as usize].life = 5;

        let safekeeper = AbilityDefinition::new(
            AbilityKind::Activated,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                Keyword::Shroud,
            ),
        );
        let mut safekeeper = safekeeper;
        safekeeper.cost = Some(AbilityCost::Sacrifice(SacrificeCost {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            requirement: SacrificeRequirement::count(1),
        }));

        let spell_id = ObjectId(99);
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: engine::types::ability::QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Player(ai)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert!(!any_land_sacrifice_protection_payoff(
            &state,
            ai,
            &safekeeper
        ));
    }

    #[test]
    fn shroud_payoff_rejects_beneficial_pump_on_ai_creature() {
        use engine::types::ability::{
            PtValue, ResolvedAbility, SacrificeCost, SacrificeRequirement, TargetRef, TypeFilter,
        };
        use engine::types::game_state::{StackEntry, StackEntryKind};
        use engine::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);

        let safekeeper = {
            let mut ability = AbilityDefinition::new(
                AbilityKind::Activated,
                grant_effect(
                    Some(TargetFilter::ParentTarget),
                    Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::You),
                    )),
                    Keyword::Shroud,
                ),
            );
            ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                requirement: SacrificeRequirement::count(1),
            }));
            ability
        };

        let creature = create_test_creature(&mut state, ai);
        let spell_id = ObjectId(99);
        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(creature)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert!(!any_land_sacrifice_protection_payoff(
            &state,
            ai,
            &safekeeper
        ));
    }

    #[test]
    fn shroud_payoff_allows_harmful_removal_on_ai_creature() {
        use engine::types::ability::{
            ResolvedAbility, SacrificeCost, SacrificeRequirement, TargetRef, TypeFilter,
        };
        use engine::types::game_state::{StackEntry, StackEntryKind};
        use engine::types::identifiers::{CardId, ObjectId};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);

        let safekeeper = {
            let mut ability = AbilityDefinition::new(
                AbilityKind::Activated,
                grant_effect(
                    Some(TargetFilter::ParentTarget),
                    Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::You),
                    )),
                    Keyword::Shroud,
                ),
            );
            ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost {
                target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                requirement: SacrificeRequirement::count(1),
            }));
            ability
        };

        let creature = create_test_creature(&mut state, ai);
        let spell_id = ObjectId(99);
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert!(any_land_sacrifice_protection_payoff(
            &state,
            ai,
            &safekeeper
        ));
    }

    fn create_test_creature(
        state: &mut GameState,
        controller: PlayerId,
    ) -> engine::types::identifiers::ObjectId {
        use engine::game::zones::create_object;
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;
        let id = create_object(
            state,
            CardId(2),
            controller,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    fn push_opponent_damage(
        state: &mut GameState,
        recipient: engine::types::identifiers::ObjectId,
        source_keywords: &[Keyword],
        mass: bool,
        amount: i32,
    ) {
        use engine::types::ability::{QuantityExpr, ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let opponent = PlayerId(1);
        let source_id = engine::game::zones::create_object(
            state,
            CardId(99),
            opponent,
            "Counter Damage".to_string(),
            Zone::Stack,
        );
        let source = state.objects.get_mut(&source_id).unwrap();
        source.base_keywords.extend(source_keywords.iter().cloned());
        source.keywords.extend(source_keywords.iter().cloned());
        let effect = if mass {
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: amount },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::Opponent),
                ),
                player_filter: None,
                damage_source: None,
            }
        } else {
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: amount },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            }
        };
        let targets = (!mass)
            .then_some(vec![TargetRef::Object(recipient)])
            .unwrap_or_default();
        let ability = ResolvedAbility::new(effect, targets, source_id, opponent);
        state.stack.push_back(StackEntry {
            id: source_id,
            source_id,
            controller: opponent,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    #[test]
    fn indestructible_does_not_answer_targeted_infect_damage() {
        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let recipient = create_test_creature(&mut state, ai);
        state.objects.get_mut(&recipient).unwrap().toughness = Some(3);
        push_opponent_damage(&mut state, recipient, &[Keyword::Infect], false, 3);
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, recipient, &effect),
            Some(false)
        );
    }

    #[test]
    fn indestructible_does_not_answer_mass_wither_damage() {
        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let recipient = create_test_creature(&mut state, ai);
        state.objects.get_mut(&recipient).unwrap().toughness = Some(3);
        push_opponent_damage(&mut state, recipient, &[Keyword::Wither], true, 3);
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, recipient, &effect),
            Some(false)
        );
    }

    #[test]
    fn indestructible_may_answer_targeted_infect_deathtouch_damage() {
        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let recipient = create_test_creature(&mut state, ai);
        state.objects.get_mut(&recipient).unwrap().toughness = Some(3);
        push_opponent_damage(
            &mut state,
            recipient,
            &[Keyword::Infect, Keyword::Deathtouch],
            false,
            1,
        );
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, recipient, &effect),
            None,
            "deathtouch destruction can be stopped even when infect uses counters"
        );
    }

    #[test]
    fn indestructible_may_answer_mass_wither_deathtouch_damage() {
        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let recipient = create_test_creature(&mut state, ai);
        state.objects.get_mut(&recipient).unwrap().toughness = Some(3);
        push_opponent_damage(
            &mut state,
            recipient,
            &[Keyword::Wither, Keyword::Deathtouch],
            true,
            1,
        );
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, recipient, &effect),
            None,
            "deathtouch destruction can be stopped even when wither uses counters"
        );
    }

    #[test]
    fn stronger_targeting_and_protection_grants_are_redundant() {
        use engine::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let recipient = create_test_creature(&mut state, PlayerId(0));
        let object = state.objects.get_mut(&recipient).unwrap();
        object.keywords.push(Keyword::Hexproof);
        object
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Everything));

        assert!(grant_already_effective(
            &state,
            recipient,
            &DefensiveGrant::HexproofFrom(HexproofFilter::Color(ManaColor::Red))
        ));
        assert!(grant_already_effective(
            &state,
            recipient,
            &DefensiveGrant::Protection(ProtectionTarget::Color(ManaColor::Red))
        ));
    }

    #[test]
    fn fight_damage_is_ambiguous_for_indestructible_payoff() {
        let mut state = GameState::new_two_player(42);
        let recipient = create_test_creature(&mut state, PlayerId(0));
        let source = create_test_creature(&mut state, PlayerId(1));
        let effect = Effect::Fight {
            target: TargetFilter::Any,
            subject: TargetFilter::SelfRef,
        };

        assert_eq!(
            grant_answers_targeted_effect(
                &state,
                &DefensiveGrant::Indestructible,
                &effect,
                recipient,
                state.objects.get(&source),
            ),
            None
        );
    }

    #[test]
    fn shroud_answers_opposing_aura_targeting_protected_creature() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opponent = PlayerId(1);
        let recipient = create_test_creature(&mut state, ai);
        let aura_id = engine::game::zones::create_object(
            &mut state,
            CardId(88),
            opponent,
            "Opposing Aura".to_string(),
            Zone::Stack,
        );
        let aura = state.objects.get_mut(&aura_id).unwrap();
        aura.card_types.core_types.push(CoreType::Enchantment);
        aura.card_types.subtypes.push("Aura".to_string());
        let ability = ResolvedAbility::new(
            Effect::unimplemented("aura spell", "Enchant creature"),
            vec![TargetRef::Object(recipient)],
            aura_id,
            opponent,
        );
        state.stack.push_back(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: opponent,
            kind: StackEntryKind::Spell {
                card_id: CardId(88),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Shroud);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, recipient, &effect),
            Some(true)
        );
    }

    #[test]
    fn protection_evasion_requires_an_untapped_legal_blocker() {
        use engine::game::combat::{AttackerInfo, CombatState};
        use engine::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        let attacker = create_test_creature(&mut state, ai);
        let blocker = create_test_creature(&mut state, opp);
        state.objects.get_mut(&blocker).unwrap().color = vec![ManaColor::Red];
        state.phase = Phase::DeclareAttackers;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, opp)],
            ..Default::default()
        });
        let effect = grant_effect(
            Some(TargetFilter::SelfRef),
            None,
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)),
        );

        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(true)
        );

        state.objects.get_mut(&blocker).unwrap().tapped = true;
        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(false),
            "the engine's legal-blocker population excludes tapped creatures"
        );
    }

    #[test]
    fn self_indestructible_has_no_payoff_after_empty_blocker_declaration() {
        use engine::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        let attacker = create_test_creature(&mut state, ai);
        state.phase = Phase::DeclareBlockers;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, opp)],
            blockers_declared_by: vec![opp],
            ..Default::default()
        });
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(false)
        );
    }

    #[test]
    fn self_indestructible_has_no_payoff_against_infect_blocker() {
        use engine::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        let attacker = create_test_creature(&mut state, ai);
        state.objects.get_mut(&attacker).unwrap().toughness = Some(3);
        let blocker = create_test_creature(&mut state, opp);
        let blocker_object = state.objects.get_mut(&blocker).unwrap();
        blocker_object.power = Some(1);
        blocker_object.keywords.push(Keyword::Infect);
        state.phase = Phase::DeclareBlockers;
        let mut combat = CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, opp)],
            blockers_declared_by: vec![opp],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(false)
        );

        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .keywords
            .push(Keyword::Deathtouch);
        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            None,
            "deathtouch destruction remains preventable by indestructible"
        );
    }

    #[test]
    fn combat_damage_after_regular_damage_has_no_protection_payoff() {
        use engine::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        let attacker = create_test_creature(&mut state, ai);
        let blocker = create_test_creature(&mut state, opp);
        state.phase = Phase::CombatDamage;
        let mut combat = CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, opp)],
            regular_damage_done: true,
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(false)
        );
    }

    #[test]
    fn pending_combat_damage_is_not_a_payoff_for_sidelined_recipient() {
        use engine::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        let recipient = create_test_creature(&mut state, ai);
        let attacker = create_test_creature(&mut state, ai);
        let blocker = create_test_creature(&mut state, opp);
        state.phase = Phase::CombatDamage;
        let mut combat = CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, opp)],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, recipient, &effect),
            Some(false)
        );
    }

    #[test]
    fn pending_combat_damage_is_not_a_payoff_for_shroud() {
        use engine::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        let attacker = create_test_creature(&mut state, ai);
        let blocker = create_test_creature(&mut state, opp);
        state.phase = Phase::CombatDamage;
        let mut combat = CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, opp)],
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Shroud);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(false)
        );
    }

    #[test]
    fn regular_substep_excludes_first_strike_only_but_includes_double_strike() {
        use engine::game::combat::{AttackerInfo, CombatState};

        let mut state = GameState::new_two_player(42);
        let ai = PlayerId(0);
        let opp = PlayerId(1);
        let attacker = create_test_creature(&mut state, ai);
        let blocker = create_test_creature(&mut state, opp);
        let blocker_object = state.objects.get_mut(&blocker).unwrap();
        blocker_object.power = Some(3);
        blocker_object.keywords.push(Keyword::FirstStrike);
        state.phase = Phase::CombatDamage;
        let mut combat = CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, opp)],
            first_strike_done: true,
            first_strike_participants: Some(std::collections::HashSet::from([blocker])),
            ..Default::default()
        };
        combat.blocker_assignments.insert(attacker, vec![blocker]);
        combat.blocker_to_attacker.insert(blocker, vec![attacker]);
        state.combat = Some(combat);
        let effect = grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible);

        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(false),
            "first-strike-only blocker has no pending regular damage"
        );

        let blocker_object = state.objects.get_mut(&blocker).unwrap();
        blocker_object.keywords.clear();
        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            Some(false),
            "losing first strike does not add the source to regular damage"
        );

        state.combat.as_mut().unwrap().first_strike_participants =
            Some(std::collections::HashSet::from([attacker]));
        state
            .objects
            .get_mut(&blocker)
            .unwrap()
            .keywords
            .push(Keyword::FirstStrike);
        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            None,
            "gaining first strike does not remove a normal source from regular damage"
        );

        state.combat.as_mut().unwrap().first_strike_participants =
            Some(std::collections::HashSet::from([blocker]));
        let blocker_object = state.objects.get_mut(&blocker).unwrap();
        blocker_object.keywords.clear();
        blocker_object.keywords.push(Keyword::DoubleStrike);
        assert_eq!(
            self_protection_effect_payoff(&state, ai, attacker, &effect),
            None,
            "double strike blocker still participates in regular damage"
        );
    }
}
