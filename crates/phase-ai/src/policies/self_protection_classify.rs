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
use engine::game::effects::effect::generic_effect_application_filter;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::functioning_abilities::active_static_definitions;
use engine::game::keywords::source_matches_protection_target;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::TargetRef;
use engine::types::card_type::CoreType;
use engine::types::keywords::{HexproofFilter, ProtectionTarget};

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

/// Exact activation payoff when every effect in the chain is a supported
/// object-scoped self-protection effect. Mixed and unsupported chains fail open.
pub(crate) fn self_protection_activation_payoff(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> Option<bool> {
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
    if !matches!(effect_polarity(effect), EffectPolarity::Harmful) {
        return Some(false);
    }
    let protected = state.objects.get(&recipient_id)?;
    match grant {
        DefensiveGrant::CantBeTargeted => Some(harmful_effect_uses_object_targeting(effect)),
        DefensiveGrant::HexproofFrom(filter) => Some(
            harmful_effect_uses_object_targeting(effect)
                && source
                    .is_some_and(|source| hexproof_from_blocks_source(filter, protected, source)),
        ),
        DefensiveGrant::Protection(protection) => Some(
            harmful_effect_uses_object_targeting(effect)
                && source.is_some_and(|source| {
                    source_matches_protection_target(protection, protected, source)
                }),
        ),
        DefensiveGrant::Indestructible => match effect {
            Effect::Destroy { .. } => Some(true),
            Effect::DealDamage { .. } => match lethal_to_creature(state, recipient_id, &[effect]) {
                Some(true) => Some(true),
                Some(false) | None => None,
            },
            _ => Some(false),
        },
        DefensiveGrant::PreventDamage => None,
    }
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
        (DefensiveGrant::Indestructible, Effect::DamageAll { .. }) => {
            match lethal_to_creature(state, recipient_id, &[effect]) {
                Some(true) => Some(true),
                Some(false) | None => None,
            }
        }
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
        DefensiveGrant::HexproofFrom(filter) => object
            .keywords
            .contains(&Keyword::HexproofFrom(filter.clone())),
        DefensiveGrant::Protection(protection) => object
            .keywords
            .contains(&Keyword::Protection(protection.clone())),
        DefensiveGrant::Indestructible => object.has_keyword(&Keyword::Indestructible),
        DefensiveGrant::PreventDamage => false,
    }
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
                let paired_ids: Vec<ObjectId> = combat
                    .blocker_assignments
                    .get(recipient_id)
                    .into_iter()
                    .flatten()
                    .chain(
                        combat
                            .blocker_to_attacker
                            .get(recipient_id)
                            .into_iter()
                            .flatten(),
                    )
                    .copied()
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
                    DefensiveGrant::Indestructible => saw_indestructible_pair = true,
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
        // participation is source-specific. The engine has no public query for
        // that distinction, so fail open rather than duplicate the rule here.
        Phase::CombatDamage if !combat.regular_damage_done => None,
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
}
