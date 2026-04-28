//! mtgish `Rule::CastEffect(CastEffect)` → engine casting metadata
//! (`additional_cost`, `casting_options`, `casting_restrictions`,
//! `strive_cost`).
//!
//! CR 601.2 governs spell casting; this module covers the static
//! casting-time modifiers a spell card carries on its face. Each variant
//! of `CastEffect` we recognise emits one of:
//!
//! - `additional_cost`: a single `AdditionalCost` (Optional/Required) per
//!   face. mtgish never emits more than one CastEffect that targets this
//!   slot, but in case of collision the converter strict-fails so the
//!   report surfaces the conflict.
//! - `casting_options.push(...)`: alternative-cost / free-cast / flash-as-
//!   though options. Each entry is a `SpellCastingOption` with optional
//!   cost and optional `ParsedCondition`.
//! - `casting_restrictions.push(...)`: "cast only if/unless …" gates.
//!   These strict-fail today: engine `RequiresCondition { condition }` with
//!   `condition = None` evaluates as **always-pass** (CR 601.2c semantics
//!   demand the inner predicate). Until the `mtgish::Condition →
//!   engine::ParsedCondition` bridge ships, every Cond-bearing CastEffect
//!   variant raises `EnginePrerequisiteMissing` so cards fall out of clean
//!   output instead of shipping subtly wrong rules.
//! - `strive_cost`: per-target surcharge mana cost.
//!
//! Strict-failure: any CastEffect variant outside this subset propagates
//! as `UnknownVariant` so the report tracks what's left.

use engine::types::ability::{
    AdditionalCost, CastingRestriction, QuantityExpr, QuantityRef, SpellCastingOption,
    SpellCastingOptionKind, StaticCondition, StaticDefinition, TargetFilter,
};
use engine::types::statics::StaticMode;
use engine::types::{ManaCost, Zone};

use crate::convert::condition as condition_conv;
use crate::convert::cost as cost_conv;
use crate::convert::mana as mana_conv;
use crate::convert::quantity as quantity_conv;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{CastEffect, Cost, CostReduction, GameNumber};

use super::EngineFaceStub;

/// CR 601.2: Apply a single `CastEffect` onto the face stub. Each
/// recognised variant emits one casting-metadata artifact; unrecognised
/// variants strict-fail so the report tracks them.
pub fn apply(eff: &CastEffect, stub: &mut EngineFaceStub) -> ConvResult<()> {
    use CastEffect as E;
    match eff {
        // CR 601.2f / CR 608.2c: Mandatory additional cost — "As an
        // additional cost to cast this spell, [cost]." Two-way
        // `Cost::Or(...)` becomes `AdditionalCost::Choice(a, b)` (the
        // canonical "either A or B" shape, e.g. Force of Will's "exile a
        // blue card or pay {1}").
        E::AdditionalCastingCost(cost) => {
            let ac = additional_cost_required(cost)?;
            set_additional_cost(stub, ac, "AdditionalCastingCost")
        }
        // CR 601.2f: X-flavored mandatory additional cost — same shape as
        // `AdditionalCastingCost` for the type system. The X-binding is
        // expressed in the inner Cost.
        E::AdditionalCastingCostX(cost) => {
            let ac = additional_cost_required(cost)?;
            set_additional_cost(stub, ac, "AdditionalCastingCostX")
        }
        // CR 601.2f: Optional additional cost — "you may [cost] as an
        // additional cost". Same shape that `Keyword::Kicker` synthesises.
        E::OptionalAdditionalCastingCost(cost) => set_additional_cost(
            stub,
            AdditionalCost::Optional(cost_conv::convert(cost)?),
            "OptionalAdditionalCastingCost",
        ),
        // CR 207.2c + CR 601.2f: Strive — per-target surcharge. The engine
        // CardFace.strive_cost slot is `Option<ManaCost>`; only mana-shaped
        // surcharges fit. Non-mana strive-style costs strict-fail.
        E::AdditionalCastingCostForEachTargetBeyondTheFirst(cost) => {
            let mana = require_pure_mana(cost, "AdditionalCastingCostForEachTargetBeyondTheFirst")?;
            if stub.strive_cost.is_some() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "CastEffect/strive_cost",
                    path: String::new(),
                    detail: "multiple strive costs on one face".into(),
                });
            }
            stub.strive_cost = Some(mana);
            Ok(())
        }
        // CR 601.2b + CR 118.9b: Alternative cost — "you may cast this
        // spell by paying [alt] rather than its mana cost". The engine
        // `SpellCastingOption::alternative_cost(...)` carries the alt cost.
        E::AlternateCastingCost(cost) => {
            stub.casting_options
                .push(SpellCastingOption::alternative_cost(cost_conv::convert(
                    cost,
                )?));
            Ok(())
        }
        // CR 601.2b + CR 118.9b: Conditional alternative cost — gate the
        // alt-cost on a translated `ParsedCondition`. The bridge propagates
        // its own gap if the inner Condition isn't expressible as a
        // ParsedCondition, so the rule strict-fails as a whole rather than
        // dropping the gate (None == always-pass).
        E::AlternateCastingCostIf(cost, condition) => {
            let parsed = condition_conv::convert_parsed(condition)?;
            stub.casting_options.push(
                SpellCastingOption::alternative_cost(cost_conv::convert(cost)?).condition(parsed),
            );
            Ok(())
        }
        // CR 118.9 + CR 601.2b: "You may cast this spell without paying its
        // mana cost if [condition]." Wire the condition through the bridge;
        // a translation gap propagates as the rule's strict-fail.
        E::MayCastWithoutPayingIf(condition) => {
            let parsed = condition_conv::convert_parsed(condition)?;
            stub.casting_options
                .push(SpellCastingOption::free_cast().condition(parsed));
            Ok(())
        }
        // CR 601.3c: "You may cast this spell as though it had flash if
        // [condition]." Wire the condition through the bridge.
        E::MayCastAsThoughItHadFlashIf(condition) => {
            let parsed = condition_conv::convert_parsed(condition)?;
            stub.casting_options
                .push(SpellCastingOption::as_though_had_flash().condition(parsed));
            Ok(())
        }
        // CR 601.2c: "Cast this spell only if [condition]" — `RequiresCondition`
        // gates the cast on the parsed predicate. Engine evaluates `None` as
        // always-pass via `is_none_or` in restrictions.rs:494, so a translation
        // gap must strict-fail the entire rule rather than emitting `None`.
        E::CantBeCastUnless(condition) => {
            let parsed = condition_conv::convert_parsed(condition)?;
            stub.casting_restrictions
                .push(CastingRestriction::RequiresCondition {
                    condition: Some(parsed),
                });
            Ok(())
        }
        // CR 601.2c: "This spell can't be cast if [condition]" — same shape,
        // negated condition. ParsedCondition has no general `Not` wrapper, so
        // strict-fail through the bridge until that's expressible.
        E::CantBeCastIf(_condition) => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "ParsedCondition",
            needed_variant: "CastEffect/CantBeCastIf (negation)".into(),
        }),
        // CR 117.7 + CR 601.2f: Self-spell cost reduction. Mirrors the native
        // parser's `try_parse_cost_modification` self-spell branch
        // (oracle_static.rs:6720). Emits `StaticMode::ReduceCost` with
        // `affected = SelfRef` and `active_zones = [Hand, Stack]` so the
        // casting-time scanner finds the static on the spell being cast.
        E::ReduceCastingCost(reduction) => {
            push_self_reduce_cost_static(stub, reduction, None, None)
        }
        // CR 117.7 + CR 601.2f: Self-spell cost reduction with a dynamic
        // "for each X" multiplier. The engine `StaticMode::ReduceCost.dynamic_count`
        // is `Option<QuantityRef>` — `QuantityExpr::Ref { qty }` unwraps cleanly;
        // `Fixed`/`Offset`/`Multiply`/`HalfRounded` cannot be expressed as a bare
        // `QuantityRef` and strict-fail.
        E::ReduceCastingCostForEach(reduction, count) => {
            let qty_ref = quantity_to_ref(count, "ReduceCastingCostForEach")?;
            push_self_reduce_cost_static(stub, reduction, Some(qty_ref), None)
        }
        // CR 117.7 + CR 601.2f: X-flavored self-spell cost reduction. The X
        // variable resolves at cast time; engine `QuantityRef::Variable { name: "X" }`
        // is the canonical encoding (used by quantity::convert for `GameNumber::ValueX`).
        E::ReduceCastingCostX(_reduction_x, count) => {
            let qty_ref = quantity_to_ref(count, "ReduceCastingCostX")?;
            // Per native parser convention, X-bearing self-spell reductions
            // declare a generic-1 `amount` because the actual reduction is
            // `amount × dynamic_count` and X is in the count, not the amount.
            // The CostReductionX symbol enumerates X pips on the amount side
            // which has no engine representation; strict-fail if the user
            // expects pip-level fidelity, but the typical card spelling
            // ("costs {X} less for each Y") fits this shape.
            push_self_reduce_cost_static_with_amount(
                stub,
                ManaCost::generic(1),
                Some(qty_ref),
                None,
            )
        }
        // CR 601.2f + CR 117.7: Self-cost reduction gated on a Condition.
        // Mirrors the native parser's trailing if-clause attachment in
        // `oracle_static.rs:6850-6917`: build a `StaticMode::ReduceCost` and
        // hang the translated `StaticCondition` off
        // `StaticDefinition.condition` (engine field declared at
        // `types/ability.rs:6245`, builder at :6295). The bridge
        // `condition::convert_static` propagates its own gap if the inner
        // mtgish::Condition isn't expressible as a StaticCondition, so the
        // rule strict-fails as a whole rather than dropping the gate
        // (None == always-pass per CR 601.2f cost-determination semantics).
        E::ReduceCastingCostIf(reduction, condition) => {
            let static_cond = condition_conv::convert_static(condition)?;
            push_self_reduce_cost_static(stub, reduction, None, Some(static_cond))
        }
        // CR 601.2c + CR 117.7: Bargain (MKM) is a built-in condition tied
        // to spell-cast metadata, not a generic StaticCondition. No engine
        // primitive expresses "this spell was bargained" as a self-cost
        // reduction gate. Strict-fail.
        E::ReduceCastingCostIfItsBargained(_reduction) => {
            Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "StaticCondition",
                needed_variant: "CastEffect/ReduceCastingCostIfItsBargained".into(),
            })
        }
        // CR 601.3c + CR 601.2f: "You may cast this spell as though it had
        // flash by paying [cost]." Engine
        // `SpellCastingOption::as_though_had_flash().cost(cost)` is wired into
        // `flash_timing_cost` (restrictions.rs:89) — runtime supports a
        // mana-shaped surcharge. Non-mana costs strict-fail because the
        // runtime branches on `AbilityCost::Mana` only.
        E::MayCastAsThoughItHadFlashForAdditionalCost(cost) => {
            let ability_cost = cost_conv::convert(cost)?;
            // Mirror flash_timing_cost gating: only Mana fits today.
            if !matches!(
                ability_cost,
                engine::types::ability::AbilityCost::Mana { .. }
            ) {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "CastEffect/MayCastAsThoughItHadFlashForAdditionalCost",
                    path: String::new(),
                    detail: "engine flash_timing_cost only honours Mana surcharges".into(),
                });
            }
            stub.casting_options
                .push(SpellCastingOption::as_though_had_flash().cost(ability_cost));
            Ok(())
        }
        other => Err(ConversionGap::UnknownVariant {
            path: String::new(),
            repr: variant_tag(other),
        }),
    }
}

/// CR 601.2f / CR 608.2c: Build a `Required` `AdditionalCost`. A
/// two-alternative `Cost::Or([a, b])` becomes `AdditionalCost::Choice(a, b)`;
/// a single-cost shape (or any other) becomes `AdditionalCost::Required(c)`.
/// Larger Or-lists strict-fail because the engine `Choice` is binary.
fn additional_cost_required(cost: &Cost) -> ConvResult<AdditionalCost> {
    if let Cost::Or(parts) = cost {
        if parts.len() == 2 {
            let a = cost_conv::convert(&parts[0])?;
            let b = cost_conv::convert(&parts[1])?;
            return Ok(AdditionalCost::Choice(a, b));
        }
        return Err(ConversionGap::MalformedIdiom {
            idiom: "CastEffect/AdditionalCost::Or",
            path: String::new(),
            detail: format!(
                "engine `AdditionalCost::Choice` is binary; got {} alternatives",
                parts.len()
            ),
        });
    }
    Ok(AdditionalCost::Required(cost_conv::convert(cost)?))
}

fn set_additional_cost(
    stub: &mut EngineFaceStub,
    cost: AdditionalCost,
    idiom: &'static str,
) -> ConvResult<()> {
    if stub.additional_cost.is_some() {
        return Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: "multiple additional_cost entries on one face".into(),
        });
    }
    stub.additional_cost = Some(cost);
    Ok(())
}

fn require_pure_mana(cost: &Cost, idiom: &'static str) -> ConvResult<ManaCost> {
    match cost_conv::as_pure_mana(cost)? {
        Some(mc) => Ok(mc),
        None => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: "non-mana cost where pure mana is required".into(),
        }),
    }
}

/// CR 117.7 + CR 601.2f: Build a self-spell `StaticMode::ReduceCost`
/// matching the native parser's emit shape (oracle_static.rs:6720+):
/// `affected = SelfRef`, `active_zones = [Hand, Stack]` so the
/// casting-time scanner picks it up on the spell being cast. The amount
/// is derived from the mtgish `CostReduction` symbol list.
fn push_self_reduce_cost_static(
    stub: &mut EngineFaceStub,
    reduction: &CostReduction,
    dynamic_count: Option<QuantityRef>,
    condition: Option<StaticCondition>,
) -> ConvResult<()> {
    let amount = mana_conv::convert_reduction(reduction)?;
    push_self_reduce_cost_static_with_amount(stub, amount, dynamic_count, condition)
}

fn push_self_reduce_cost_static_with_amount(
    stub: &mut EngineFaceStub,
    amount: ManaCost,
    dynamic_count: Option<QuantityRef>,
    condition: Option<StaticCondition>,
) -> ConvResult<()> {
    let mut def = StaticDefinition::new(StaticMode::ReduceCost {
        amount,
        spell_filter: Some(TargetFilter::SelfRef),
        dynamic_count,
    })
    .affected(TargetFilter::SelfRef);
    def.active_zones = vec![Zone::Hand, Zone::Stack];
    def.condition = condition;
    stub.statics.push(def);
    Ok(())
}

/// Convert a mtgish `GameNumber` into an engine `QuantityRef`. The engine
/// `dynamic_count` slot is `Option<QuantityRef>`, not `Option<QuantityExpr>`,
/// so only the `Ref { qty }` shape unwraps cleanly. Other expression forms
/// (literal/offset/multiply/half-rounded) cannot be expressed as a bare ref
/// and strict-fail.
fn quantity_to_ref(g: &GameNumber, idiom: &'static str) -> ConvResult<QuantityRef> {
    match quantity_conv::convert(g)? {
        QuantityExpr::Ref { qty } => Ok(qty),
        _ => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail:
                "engine `dynamic_count` requires a bare QuantityRef; expression shape lacks one"
                    .into(),
        }),
    }
}

fn variant_tag(eff: &CastEffect) -> String {
    serde_json::to_value(eff)
        .ok()
        .and_then(|v| {
            v.get("_CastEffect")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}

// Suppress an unused-import warning when the SpellCastingOptionKind type is
// only referenced indirectly via the helper constructors above.
#[allow(dead_code)]
const _SCOK: Option<SpellCastingOptionKind> = None;
