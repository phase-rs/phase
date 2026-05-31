//! Oracle static ability parser (CR 604 / CR 613).

use std::borrow::Cow;
use std::str::FromStr;

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::{tag, tag_no_case, take_until};
use nom::character::complete::{alpha1, space0, space1};
use nom::combinator::{all_consuming, eof, map, opt, recognize, rest, value};
use nom::multi::{many0, separated_list1};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::subject::{parse_restriction_modes, static_mode_needs_grant_propagation};
use super::oracle_effect::{parse_effect_chain, strip_trailing_duration};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::static_ir::StaticIr;
use super::oracle_nom::bridge::nom_on_lower;
use super::oracle_nom::condition as nom_condition;
use super::oracle_nom::error::OracleResult;
use super::oracle_nom::filter as nom_filter;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::target as nom_target;
use super::oracle_quantity::{
    parse_cda_quantity, parse_event_context_quantity, parse_for_each_clause, parse_quantity_ref,
};
use super::oracle_target::{
    parse_combat_status_prefix, parse_counter_suffix, parse_mana_value_suffix, parse_target,
    parse_that_clause_suffix, parse_type_phrase,
};
use super::oracle_util::{
    has_unconsumed_conditional, infer_core_type_for_subtype, parse_comparator_prefix,
    parse_mana_symbols, parse_number, parse_subtype, strip_after, strip_reminder_text, TextPair,
    SELF_REF_PARSE_ONLY_PHRASES, SELF_REF_TYPE_PHRASES,
};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, ActivationRestriction, AttachmentKind,
    BasicLandType, CardPlayMode, ChosenSubtypeKind, Comparator, ContinuousModification,
    ControllerRef, CostCategory, CountScope, FilterProp, ObjectScope, ParsedCondition, PtStat,
    PtValueScope, QuantityExpr, QuantityRef, StaticCondition, StaticDefinition, TargetFilter,
    TypeFilter, TypedFilter,
};
use crate::types::card_type::{noncreature_subtype_set, CoreType, SubtypeSet, Supertype};
use crate::types::counter::{parse_counter_type, CounterMatch};
use crate::types::keywords::{Keyword, KeywordKind};
use crate::types::mana::{ManaColor, ManaCost, ManaType};
use crate::types::phase::Phase;
use crate::types::statics::{
    ActivationExemption, BlockExceptionKind, CastFrequency, CastingProhibitionCondition,
    CostPaymentProhibition, ExileCastCost, HandSizeModification, ProhibitionScope, StaticMode,
    TriggerCause,
};
use crate::types::zones::Zone;

include!("shared.rs");
include!("shared_2.rs");
include!("shared_3.rs");
include!("restriction.rs");
include!("evasion.rs");
include!("mana_transform.rs");
include!("cost_mod.rs");
include!("keyword_grant.rs");
include!("type_change.rs");
include!("loyalty.rs");
include!("anthem.rs");
include!("cda.rs");
mod dispatch;

use dispatch::{parse_static_line_inner, InvertedAsLongAs};

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
    populate_active_zones_from_condition(&mut def);
    def
}

#[cfg(test)]
include!("tests.inc.rs");
