//! Oracle static ability parser (CR 604 / CR 613).

mod prelude {
    #![allow(unused_imports)]

    pub(super) use std::borrow::Cow;
    pub(super) use std::str::FromStr;

    pub(super) use crate::parser::oracle_nom::error::OracleError;
    pub(super) use nom::branch::alt;
    pub(super) use nom::bytes::complete::{tag, tag_no_case, take_until};
    pub(super) use nom::character::complete::{alpha1, space0, space1};
    pub(super) use nom::combinator::{all_consuming, eof, map, opt, recognize, rest, value};
    pub(super) use nom::multi::{many0, separated_list1};
    pub(super) use nom::sequence::{preceded, terminated};
    pub(super) use nom::Parser;

    pub(super) use super::super::oracle_cost::parse_oracle_cost;
    pub(super) use super::super::oracle_effect::subject::{
        parse_restriction_modes, static_mode_needs_grant_propagation,
    };
    pub(super) use super::super::oracle_effect::{parse_effect_chain, strip_trailing_duration};
    pub(super) use super::super::oracle_ir::context::ParseContext;
    pub(super) use super::super::oracle_ir::static_ir::StaticIr;
    pub(super) use super::super::oracle_nom::bridge::nom_on_lower;
    pub(super) use super::super::oracle_nom::condition as nom_condition;
    pub(super) use super::super::oracle_nom::error::OracleResult;
    pub(super) use super::super::oracle_nom::filter as nom_filter;
    pub(super) use super::super::oracle_nom::primitives as nom_primitives;
    pub(super) use super::super::oracle_nom::target as nom_target;
    pub(super) use super::super::oracle_quantity::{
        parse_cda_quantity, parse_event_context_quantity, parse_for_each_clause, parse_quantity_ref,
    };
    pub(super) use super::super::oracle_target::{
        distribute_controller_to_or, parse_combat_status_prefix, parse_counter_suffix,
        parse_mana_value_suffix, parse_target, parse_that_clause_suffix, parse_type_phrase,
    };
    pub(super) use super::super::oracle_util::{
        has_unconsumed_conditional, infer_core_type_for_subtype, parse_comparator_prefix,
        parse_mana_symbols, parse_number, parse_subtype, strip_after, strip_reminder_text,
        TextPair, SELF_REF_PARSE_ONLY_PHRASES, SELF_REF_TYPE_PHRASES,
    };
    pub(super) use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, ActivationRestriction,
        AttachmentKind, BasicLandType, CardPlayMode, ChosenSubtypeKind, Comparator,
        ContinuousModification, ControllerRef, CostCategory, CountScope, FilterProp, ObjectScope,
        ParsedCondition, PtStat, PtValueScope, QuantityExpr, QuantityRef, SharedQuality,
        SharedQualityRelation, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
        TypedFilter,
    };
    pub(super) use crate::types::card_type::{
        noncreature_subtype_set, CoreType, SubtypeSet, Supertype,
    };
    pub(super) use crate::types::counter::{parse_counter_type, CounterMatch};
    pub(super) use crate::types::keywords::{Keyword, KeywordKind};
    pub(super) use crate::types::mana::{ManaColor, ManaCost, ManaType};
    pub(super) use crate::types::phase::Phase;
    pub(super) use crate::types::statics::{
        ActivationExemption, BlockExceptionKind, CastFreeOrigin, CastFrequency,
        CastingProhibitionCondition, CostModifyMode, CostPaymentProhibition, CrewAction,
        CrewContributionKind, ExileCardPool, ExileCastCost, ExileCastTiming, HandSizeModification,
        ProhibitionScope, StaticMode, TriggerCause,
    };
    pub(super) use crate::types::zones::Zone;
}

pub(super) use super::{
    oracle_effect, oracle_keyword, oracle_nom, oracle_quantity, oracle_trigger,
};

mod anthem;
mod cda;
mod cost_mod;
mod dispatch;
mod evasion;
mod grammar;
mod keyword_grant;
mod loyalty;
mod mana_transform;
mod restriction;
mod shared;
mod static_helpers;
mod type_change;

pub(crate) use shared::parse_commander_subject_filter_prefix;

use dispatch::{parse_static_line_inner, InvertedAsLongAs};
use prelude::StaticIr;

mod support {
    pub(super) use super::anthem::{
        bind_where_x_in_quantity_expr, parse_base_pt_dynamic, parse_base_pt_mana_value_dynamic,
        parse_base_pt_mod, parse_continuous_gets_has,
        parse_controlled_compound_continuous_subject_filter,
        parse_dynamic_for_each_pt_modifications, parse_dynamic_pt_in_text,
        parse_typed_you_control_subject_filter,
    };
    pub(super) use super::cost_mod::parse_cost_payment_prohibition_statics;
    pub(super) use super::evasion::{
        classify_block_exception, parse_compound_subject_keyword_static,
        parse_compound_subject_rule_static, parse_property_descriptor,
        parse_rule_static_separator_nom, try_parse_compound_subtypes,
        try_parse_scoped_must_attack_block, try_split_and_can_attack_despite_defender,
        try_split_and_can_block_additional, try_split_and_cant_activate_abilities,
        try_split_and_cant_attack, try_split_and_cant_attack_or_block,
        try_split_and_cant_be_attached, try_split_and_cant_be_blocked,
        try_split_and_cant_be_sacrificed, try_split_and_cant_be_targeted, try_split_and_cant_block,
        try_split_and_doesnt_untap, try_split_and_must_attack_block,
    };
    pub(super) use super::grammar::*;
    pub(super) use super::keyword_grant::{
        apply_spell_keyword_subject_constraints, parse_chosen_qualifier_subject,
        parse_continuous_modifications, parse_quoted_ability_modifications,
        push_grant_clause_modifications, split_keyword_list, RuleStaticPredicate,
    };
    pub(super) use super::restriction::{
        parse_cant_be_activated_exemption_in_text, parse_cast_and_activate_only_during,
        strip_casting_prohibition_subject,
    };
    pub(super) use super::shared::*;
    pub(super) use super::static_helpers::*;
    pub(super) use super::type_change::{
        parse_additive_type_clause_modifications,
        parse_bare_becomes_type_replacement_modifications,
        parse_becomes_type_addition_modifications, parse_enchanted_is_type,
    };
    pub(super) use super::{lower_static_ir, parse_static_line};
}

pub(crate) use cost_mod::{
    parse_alternative_keyword_cost, parse_cast_spells_alternative_cost_multi,
    parse_collect_evidence_alt_cost, parse_spells_alternative_cost,
};
pub(crate) use evasion::classify_block_exception;
pub(crate) use keyword_grant::{
    classify_quoted_inner, parse_chosen_qualifier_subject, parse_continuous_modifications,
    parse_quoted_ability_modifications, split_keyword_list,
    try_parse_graveyard_keyword_grant_clause,
};
pub(crate) use mana_transform::try_parse_retain_unspent_mana_static;
pub(crate) use restriction::parse_cant_be_activated_exemption_in_text;
pub use shared::parse_static_line_multi;
pub(crate) use shared::parse_subtype_or_list_insensitive_prefix;
pub(crate) use shared::GraveyardGrantedKeywordKind;
pub(crate) use type_change::{
    parse_additive_type_clause_modifications, parse_chosen_creature_type_static_prefix,
    parse_every_creature_type_static_prefix,
};

/// Parse a static/continuous ability line into a `StaticDefinition`.
#[tracing::instrument(level = "debug")]
pub fn parse_static_line(text: &str) -> Option<crate::types::ability::StaticDefinition> {
    let ir = parse_static_line_ir(text)?;
    Some(lower_static_ir(&ir))
}

/// IR production: parse a static line into `StaticIr` (pre-lowering).
pub(crate) fn parse_static_line_ir(text: &str) -> Option<StaticIr> {
    let definition = parse_static_line_inner(text, InvertedAsLongAs::Allow)?;
    Some(StaticIr {
        definition,
        source_text: text.to_string(),
        body_ir: None,
    })
}

/// Lowering: apply post-parse transforms to produce the final `StaticDefinition`.
pub(crate) fn lower_static_ir(ir: &StaticIr) -> crate::types::ability::StaticDefinition {
    let mut def = ir.definition.clone();
    shared::populate_active_zones_from_condition(&mut def);
    def
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod snapshot_tests;
