use crate::parser::oracle_nom::error::{OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_till, take_until};
use nom::character::complete::{one_of, space0, space1};
use nom::combinator::{all_consuming, eof, map, not, opt, peek, rest, value};
use nom::error::ParseError;
use nom::sequence::{preceded, terminated};
use nom::Parser;

use super::counter::{
    parse_counter_anaphor, try_parse_double_effect, try_parse_move_counters_from,
    try_parse_multiply_pt_effect, try_parse_put_counter, try_parse_remove_counter,
};
use super::lower::{
    parse_for_each_multiplier_prefix, parse_multi_target_count_expr,
    parse_where_x_quantity_expression, strip_leading_quantifier, strip_trailing_where_x,
};
use super::mana::{try_parse_activate_only_condition, try_parse_add_mana_effect_with_context};
use super::token::try_parse_token;
use super::{
    attach_controller_if_absent, is_bare_object_pronoun, resolve_it_pronoun, ParseContext,
};
use crate::parser::oracle_ir::ast::*;
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::parser::oracle_nom::bridge::{nom_on_lower, nom_parse_lower, split_once_on_lower};
use crate::parser::oracle_nom::primitives as nom_primitives;
use crate::parser::oracle_nom::quantity as nom_quantity;
use crate::parser::oracle_static::parse_activated_ability_cost_head;
use crate::parser::oracle_static::{
    parse_continuous_modifications, parse_may_look_at_face_down_filter,
    parse_quoted_ability_modifications,
};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, BounceSelection, CardSelectionMode,
    CategoryChooserScope, ChoiceType, Chooser, ContinuousModification, ControlWindow,
    ControllerRef, CopyRetargetPermission, CounterAdjustment, DigSource, DoorLockOp, Duration,
    Effect, EffectScope, FaceDownProfile, FilterProp, GrantedAbilityScope, LibraryPosition,
    MultiTargetSpec, OutsideGameSourcePool, PlayerScope, PreventionAmount, PreventionScope, PtStat,
    PtValue, QuantityExpr, QuantityRef, ReassembleControlMode, SearchSelectionConstraint,
    StaticDefinition, StickerTicketCostPayment, TapStateChange, TargetFilter, TargetSelectionMode,
    TypeFilter, TypedFilter, ZoneOwner,
};
use crate::types::card_type::CoreType;
use crate::types::phase::Phase;
use crate::types::player::PlayerCounterKind;
use crate::types::statics::{ActivationExemption, CostModifyMode, StaticMode};
use crate::types::zones::Zone;

use super::super::oracle_target::{
    parse_anaphoric_target_ref, parse_event_context_ref, parse_fight_target, parse_mass_type_union,
    parse_target, parse_target_with_ctx, parse_target_with_syntax, parse_type_phrase,
    parse_type_phrase_with_ctx, parse_word_bounded, resolve_pronoun_target,
    resolve_singular_exiled_card_target, TargetSyntax,
};
use super::super::oracle_util::{
    contains_possessive, contains_self_or_object_pronoun, parse_count_expr, parse_mana_symbols,
    parse_ordinal, parse_rounding_suffix_only, rewrite_quantity_expr_rounding, split_around,
    starts_with_possessive, TextPair,
};

/// CR 611.2 + CR 601.2f + CR 118.7: Parse the transient (this-turn)
/// activated-ability cost-reduction effect — "activated abilities of <subject>
/// cost {N} less to activate [this turn]" (The Dining Car's chaos ability).
///
/// This lowers to the SAME `StaticMode::ReduceAbilityCost` the printed static
/// (Training Grounds) produces, wrapped in a `GenericEffect` that installs it as
/// a `Duration::UntilEndOfTurn` continuous effect (CR 611.2). There is no
/// separate transient cost-reduction pathway: the cost hook
/// (`casting::reduce_activated_ability_cost`, the single CR 601.2f/CR 118.5 cost
/// authority) reads this duration-scoped continuous effect right alongside
/// battlefield statics. The reduction rides as an `AddStaticMode` modification —
/// mirroring the `MayLookAtFaceDown` duration-bound permission (Lumbering
/// Laundry) — so `effect.rs::register_transient_effect` keeps the `affected`
/// source filter intact on the TCE (dynamic per CR 611.2c: a rules-modifying
/// effect's affected set is re-evaluated each activation, so tokens created
/// later this turn are still covered) and `layers.rs` does NOT graft it onto
/// individual objects.
///
/// Reuses the shared static grammar head `parse_activated_ability_cost_head`, so
/// the static and transient forms share one authority. Only the fixed-amount,
/// `Reduce`, `"activated"` transient case has a real driver today; loyalty /
/// `Raise` / variable-`{X}` transient wordings are left unparsed (→
/// `Unimplemented`) rather than emitting a speculative effect.
///
/// The static/transient separation is a DISPATCH-SITE invariant, not a textual
/// one: a standalone printed line (Training Grounds) is claimed by
/// `parse_static_line` before `parse_imperative_effect` ever runs, so this arm
/// only fires for a trigger's effect body. The turn scope is enforced by the
/// `UntilEndOfTurn` duration (CR 514.2 cleanup expires the continuous effect),
/// never by text keyed here (the "this turn" is already stripped upstream before
/// the body reaches this parser).
pub(crate) fn try_parse_activated_ability_cost_reduction_effect(
    tp: TextPair,
    ctx: &mut ParseContext,
) -> Option<Effect> {
    let (_rest, (keyword, subject, amount, is_x, mode)) =
        parse_activated_ability_cost_head(tp.lower).ok()?;
    if keyword != "activated" || is_x || !matches!(mode, CostModifyMode::Reduce) {
        return None;
    }
    // "artifact tokens you control" → Typed[Artifact, You, FilterProp::Token].
    let (source_filter, _after) = parse_type_phrase_with_ctx(subject, ctx);
    // CR 601.2f + CR 118.7: identical shape to Training Grounds' printed static
    // (dispatch.rs). `keyword: "activated"` matches every activated ability;
    // `exemption`/`activator`/`minimum_mana`/`dynamic_count` carry no clause on
    // any known transient reducer, so they take the static's defaults.
    let reduce_mode = StaticMode::ReduceAbilityCost {
        mode: CostModifyMode::Reduce,
        keyword: "activated".to_string(),
        amount,
        minimum_mana: None,
        dynamic_count: None,
        exemption: ActivationExemption::None,
        activator: None,
    };
    // CR 611.2c: The reduction rides as an `AddStaticMode` modification (read off
    // the TCE by the cost hook), with the source filter in `affected`. `target:
    // None` + `duration: UntilEndOfTurn` = a battlefield-wide, this-turn effect.
    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::new(reduce_mode.clone())
            .affected(source_filter)
            .modifications(vec![ContinuousModification::AddStaticMode {
                mode: reduce_mode,
            }])],
        duration: Some(Duration::UntilEndOfTurn),
        target: None,
    })
}

/// CR 702.26: Phasing direction used by the "phase in"/"phase out" dispatch.
#[derive(Copy, Clone)]
enum PhaseDir {
    In,
    Out,
}

/// Earthbend keyword action default target: "target land you control".
/// Used when the Earthbend verb appears without an explicit target (e.g., after
/// reminder text stripping removes the parenthetical that contains the target).
pub(super) fn default_earthbend_target() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You))
}

/// CR 115.1 + Whitemane Lion ruling: True iff the filter constrains by a
/// controller scope (`controller: Some(...)`) at the top level (or inside an
/// `And`/`Or`/`Not` composition). This is the precondition for treating a
/// non-targeted bounce as a controller-choice EffectZoneChoice instead of a
/// no-op — without a controller scope, the resolver has no eligible-set to
/// enumerate. Mirrors the shape of `filter_targets_zone` in `effects/bounce.rs`.
pub(super) fn filter_has_controller_scope(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.controller.is_some(),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_has_controller_scope)
        }
        TargetFilter::Not { filter } => filter_has_controller_scope(filter),
        _ => false,
    }
}

fn parse_dig_library_owner(rest_lower: &str) -> TargetFilter {
    if preceded(
        take_until::<_, _, OracleError<'_>>("target player's library"),
        tag::<_, _, OracleError<'_>>("target player's library"),
    )
    .parse(rest_lower)
    .is_ok()
    {
        return TargetFilter::Player;
    }

    if preceded(
        take_until::<_, _, OracleError<'_>>("that player's library"),
        tag::<_, _, OracleError<'_>>("that player's library"),
    )
    .parse(rest_lower)
    .is_ok()
    {
        return TargetFilter::ParentTarget;
    }

    if preceded(
        take_until::<_, _, OracleError<'_>>("that opponent's library"),
        tag::<_, _, OracleError<'_>>("that opponent's library"),
    )
    .parse(rest_lower)
    .is_ok()
    {
        return TargetFilter::ParentTarget;
    }

    // CR 608.2c + CR 400.3: "that library" — anaphoric to a library
    // identified earlier in the instruction (Chaos Warp: owner's library
    // after shuffle).
    if preceded(
        take_until::<_, _, OracleError<'_>>("that library"),
        tag::<_, _, OracleError<'_>>("that library"),
    )
    .parse(rest_lower)
    .is_ok()
    {
        return TargetFilter::ParentTargetOwner;
    }

    TargetFilter::Controller
}

fn try_parse_put_sticker_effect(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<crate::types::ability::Effect> {
    let text = lower.trim().trim_end_matches('.');
    let (target_text, (count, kind, max_ticket_cost)) = parse_put_sticker_clause(text).ok()?;
    if max_ticket_cost.is_some() && kind != Some(crate::types::stickers::StickerKind::Ability) {
        return None;
    }
    let (_, (target_text, ticket_cost_payment)) =
        parse_put_sticker_target_tail(target_text).ok()?;
    let (target, rem) = parse_target_with_ctx(target_text.trim(), ctx);
    if !rem.trim().is_empty() {
        return None;
    }
    Some(Effect::PutSticker {
        target,
        kind,
        count,
        max_ticket_cost,
        ticket_cost_payment,
    })
}

fn parse_put_sticker_clause(
    input: &str,
) -> OracleResult<
    '_,
    (
        QuantityExpr,
        Option<crate::types::stickers::StickerKind>,
        Option<QuantityExpr>,
    ),
> {
    let (input, _) = tag::<_, _, OracleError<'_>>("put ").parse(input)?;
    let (input, count) = parse_sticker_count_expr(input)?;
    let (input, kind) = parse_sticker_kind(input)?;
    let (input, max_ticket_cost) = opt(preceded(
        tag::<_, _, OracleError<'_>>(" with ticket cost "),
        parse_sticker_max_ticket_cost,
    ))
    .parse(input)?;
    let (input, _) = tag::<_, _, OracleError<'_>>(" on ").parse(input)?;
    Ok((input, (count, kind, max_ticket_cost)))
}

fn parse_sticker_count_expr(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        map(
            terminated(
                preceded(
                    tag::<_, _, OracleError<'_>>("up to "),
                    parse_sticker_count_atom,
                ),
                tag(" "),
            ),
            QuantityExpr::up_to,
        ),
        terminated(parse_sticker_count_atom, tag(" ")),
    ))
    .parse(input)
}

fn parse_sticker_count_atom(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        value(
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            tag::<_, _, OracleError<'_>>("x"),
        ),
        map(nom_primitives::parse_number, |value| QuantityExpr::Fixed {
            value: value as i32,
        }),
    ))
    .parse(input)
}

fn parse_sticker_kind(
    input: &str,
) -> OracleResult<'_, Option<crate::types::stickers::StickerKind>> {
    let (input, kind) = alt((
        value(
            Some(crate::types::stickers::StickerKind::PowerToughness),
            tag::<_, _, OracleError<'_>>("power and toughness sticker"),
        ),
        value(
            Some(crate::types::stickers::StickerKind::Name),
            tag::<_, _, OracleError<'_>>("name sticker"),
        ),
        value(
            Some(crate::types::stickers::StickerKind::Art),
            tag::<_, _, OracleError<'_>>("art sticker"),
        ),
        value(
            Some(crate::types::stickers::StickerKind::Ability),
            tag::<_, _, OracleError<'_>>("ability sticker"),
        ),
        value(None, tag::<_, _, OracleError<'_>>("sticker")),
    ))
    .parse(input)?;
    let (input, _) = opt(tag::<_, _, OracleError<'_>>("s")).parse(input)?;
    Ok((input, kind))
}

fn parse_sticker_max_ticket_cost(input: &str) -> OracleResult<'_, QuantityExpr> {
    let (rest, cost_text) = terminated(
        take_until::<_, _, OracleError<'_>>(" or less"),
        tag::<_, _, OracleError<'_>>(" or less"),
    )
    .parse(input)?;
    let Some((expr, trailing)) = parse_count_expr(cost_text.trim()) else {
        return Err(nom::Err::Error(OracleError::from_error_kind(
            input,
            nom::error::ErrorKind::Verify,
        )));
    };
    if !trailing.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::from_error_kind(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }
    Ok((rest, expr))
}

fn parse_manifest_count_card_words(input: &str) -> OracleResult<'_, QuantityExpr> {
    alt((
        value(
            QuantityExpr::Fixed { value: 1 },
            alt((
                tag::<_, _, OracleError<'_>>("cards"),
                tag::<_, _, OracleError<'_>>("card"),
            )),
        ),
        map(
            terminated(
                nom_primitives::parse_number,
                preceded(
                    space1::<_, OracleError<'_>>,
                    alt((
                        tag::<_, _, OracleError<'_>>("cards"),
                        tag::<_, _, OracleError<'_>>("card"),
                    )),
                ),
            ),
            |n| QuantityExpr::Fixed { value: n as i32 },
        ),
    ))
    .parse(input)
}

fn parse_direct_manifest_clause<'a>(
    input: &'a str,
    ctx: &ParseContext,
) -> OracleResult<'a, (TargetFilter, QuantityExpr, Option<ControllerRef>)> {
    let attach_to_it_continuation = value(
        (),
        preceded(
            space1::<_, OracleError<'_>>,
            (
                tag::<_, _, OracleError<'_>>("and attach "),
                take_until::<_, _, OracleError<'_>>(" to it"),
                tag(" to it"),
            ),
        ),
    );
    all_consuming((
        tag::<_, _, OracleError<'_>>("manifest the top "),
        parse_manifest_count_card_words,
        space1::<_, OracleError<'_>>,
        alt((
            value(TargetFilter::Controller, tag("of your library")),
            value(
                that_player_library_filter(ctx),
                tag("of that player's library"),
            ),
        )),
        opt(preceded(
            space1::<_, OracleError<'_>>,
            tag("under your control"),
        )),
        opt(attach_to_it_continuation),
        opt(tag(".")),
    ))
    .parse(input)
    .map(|(rest, (_, count, _, target, _, _, _))| (rest, (target, count, Some(ControllerRef::You))))
}

fn parse_put_sticker_target_tail(
    input: &str,
) -> OracleResult<'_, (&str, StickerTicketCostPayment)> {
    all_consuming(alt((
        map(
            terminated(
                take_until::<_, _, OracleError<'_>>(" without paying that sticker's ticket cost"),
                tag::<_, _, OracleError<'_>>(" without paying that sticker's ticket cost"),
            ),
            |target_text| (target_text, StickerTicketCostPayment::WithoutPaying),
        ),
        map(rest, |target_text| {
            (target_text, StickerTicketCostPayment::PayNormally)
        }),
    )))
    .parse(input)
}

/// Shared ControlNextTurn suffix parser (CR 723.1). Called after a prefix
/// combinator ("you control " or "gain control of ") has matched; parses the
/// target, then the "during <possessive> next turn" duration clause, then the
/// optional extra-turn tail (CR 723.1 doesn't require it; some cards like
/// Emrakul grant it).
///
/// CR 723.1: "control another player during that player's next turn." The
/// possessive is a single grammatical axis — "that player's" when the target
/// noun is "player" (Mindslaver), "their" when the target noun is "opponent"
/// (Construct a Cosmic Cube). Both refer to the targeted player, so they lower
/// to the same effect; they are composed as one `alt()` over the possessive,
/// never enumerated as separate full-string arms.
///
/// Returns `None` when the suffix doesn't apply, allowing the caller to treat
/// the match as a different effect (e.g., plain `GainControl`).
fn try_parse_control_next_turn_suffix(
    _text: &str,
    rest: &str,
) -> Option<(TargetFilter, bool, ControlWindow)> {
    let (target_text, _) = super::strip_optional_target_prefix(rest);
    // CR 608.2k: "that player" is an anaphoric reference to the already-chosen
    // parent target (Secret of Bloodbending's paid override), not a fresh target
    // phrase — resolve it via the shared event-context building block first, then
    // fall back to standard target parsing ("target opponent", "target player").
    let (target, rem) =
        parse_event_context_ref(target_text).unwrap_or_else(|| parse_target(target_text));
    let rem_lower = rem.to_ascii_lowercase();
    // CR 723.1 / CR 723.2: the duration is one grammatical axis — possessive ×
    // window. Compose the window as a single `alt()` (next turn vs next combat
    // phase), never enumerate full-string permutations.
    let (consumed, window) = preceded(
        tag::<_, _, OracleError<'_>>(" during "),
        preceded(
            alt((tag("that player's"), tag("their"), tag("its"))),
            alt((
                value(ControlWindow::NextTurn, tag(" next turn")),
                value(ControlWindow::NextCombatPhase, tag(" next combat phase")),
            )),
        ),
    )
    .parse(rem_lower.as_str())
    .map(|(remainder, window)| (rem_lower.len() - remainder.len(), window))
    .ok()?;
    let rem_after_during = &rem[consumed..];
    let rem_after_during_lower = rem_after_during.to_ascii_lowercase();
    let (_tail, grant_extra_turn_after) = if let Ok((tail, _)) = alt((
        tag::<_, _, OracleError<'_>>(". after that turn, that player takes an extra turn"),
        tag(" after that turn, that player takes an extra turn"),
        tag("after that turn, that player takes an extra turn"),
    ))
    .parse(rem_after_during_lower.as_str())
    {
        (
            &rem_after_during[rem_after_during.len() - tail.len()..],
            true,
        )
    } else {
        (rem_after_during, false)
    };
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_tail, _text);
    Some((target, grant_extra_turn_after, window))
}

/// Parse "earthbend [N] [target <type>]" from the text after "earthbend ".
/// Returns `(target, power, toughness)`. Defaults to "target land you control"
/// when no explicit target remains (reminder text stripped, sequence connectors,
/// or variable amounts like "X, where X is...").
///
/// Shared by both the single-imperative parser (`parse_targeted_action_ast`)
/// and the sequence-level parser (`try_parse_verb_and_target` in `mod.rs`).
pub(super) fn parse_earthbend_params(text: &str, lower_rest: &str) -> (TargetFilter, i32, i32) {
    // Delegate to nom combinator (input already lowercase from lower_rest parameter).
    let parsed_number = nom_primitives::parse_number.parse(lower_rest).ok();
    let (pt, target_text) = parsed_number
        .map(|(rem, n)| (n as i32, rem.trim_start()))
        .unwrap_or((0, lower_rest));
    let target = resolve_earthbend_target(text, target_text, parsed_number.is_some());
    (target, pt, pt)
}

/// Parse "earthbend [N | X[, where X is …]] [target <type>]" returning the
/// counter count as a `QuantityExpr` so dynamic amounts (Toph's "earthbend X,
/// where X is the number of experience counters you have") flow through to
/// `Effect::PutCounter` instead of collapsing to `Fixed { value: 0 }`.
///
/// Dispatch order:
/// 1. Literal N (`parse_number` succeeds) → `QuantityExpr::Fixed`.
/// 2. Recognized `"x, where X is <dynamic quantity>"` clauses →
///    `QuantityExpr::Ref { … }` (player counters, object counts, and event
///    context quantities). An unrecognized present where-clause returns `None`
///    so the caller can emit an honest `Effect::unimplemented` gap.
/// 3. Bare `"x"` (no where-clause) → `QuantityExpr::Ref { qty: Variable { "X" } }`,
///    matching the spell-cost X resolution path.
/// 4. None of the above → `Fixed { value: 0 }` with the default target,
///    preserving the prior behaviour for unsupported text shapes.
///
/// Used only by `try_parse_earthbend_clause` (the full-expansion path that
/// emits Animate + PutCounter + delayed return). The literal-N AST path
/// retains `parse_earthbend_params` to avoid perturbing its two callers.
///
/// `text` is the full original-case clause (including the leading
/// "Earthbend "); `lower_rest` is the lowercased remainder after that prefix.
/// The shared `resolve_earthbend_target` helper recovers the original-case
/// target slice as `text[text.len() - target_text.len()..]`, which only lands
/// at the correct byte boundary because ASCII lowercasing preserves byte
/// length and the entire Oracle-text dispatcher operates on ASCII.
pub(super) fn parse_earthbend_count_expr(
    text: &str,
    lower_rest: &str,
) -> Option<(TargetFilter, QuantityExpr)> {
    if let Ok((rem, n)) = nom_primitives::parse_number.parse(lower_rest) {
        let target_text = rem.trim_start();
        let target = resolve_earthbend_target(text, target_text, true);
        return Some((target, QuantityExpr::Fixed { value: n as i32 }));
    }
    if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("x").parse(lower_rest) {
        // CR 701.66a + CR 107.3: "X, where X is <dynamic quantity>" — the general
        // where-clause binding. Earthbend N puts N +1/+1 counters on the land
        // (CR 701.66a); the "where X is …" clause is the ability defining the
        // value of X (CR 107.3), evaluated against live game state, so it must
        // route through the shared quantity parsers rather than a single-shape
        // special case:
        //   - player-counter counts (Toph-style "the number of experience
        //     counters you have"),
        //   - object counts with restrictions (The Boulder: "the number of
        //     creatures you control with power 4 or greater"; Rockalanche: "the
        //     number of Forests you control") — handled by `parse_quantity_ref`,
        //   - mana spent to cast the source (Toph, Greatest Earthbender: "the
        //     amount of mana spent to cast her") — handled by
        //     `parse_event_context_quantity`.
        // Before this, only player-counter counts were recognized; every other
        // shape fell through to the bare-X branch below, which bound X to
        // `Variable{X}` (resolves to 0 for a triggered ability) AND mis-read the
        // trailing "where X is …" text as an explicit target, degrading it to
        // `TargetFilter::Any` (issue #4729). The where-clause is a quantity
        // definition that consumes the entire tail — earthbend has no explicit
        // target in templated text — so `default_earthbend_target` ("target land
        // you control", CR 701.66a) is the correct, invariant target.
        //
        // A recognized where-body binds its typed quantity against the default
        // target. An UNRECOGNIZED where-body returns `None` (strict failure):
        // accepting the clause with a fabricated `Variable{X}` (→ 0 for a
        // triggered ability) would report the card as supported while applying
        // the wrong counter count — a well-typed lie. The caller
        // (`try_parse_earthbend_clause`) turns that `None` into an honest
        // `Effect::unimplemented` gap so coverage counts the card as unsupported.
        // (matthewevans review, PR #5881.)
        if let Ok((where_body, _)) = tag::<_, _, OracleError<'_>>(", where x is ").parse(rem) {
            let count = crate::parser::oracle_quantity::parse_quantity_ref(where_body)
                .map(|qty| QuantityExpr::Ref { qty })
                .or_else(|| {
                    crate::parser::oracle_quantity::parse_event_context_quantity(where_body)
                })?;
            return Some((default_earthbend_target(), count));
        }
        // CR 107.3a + CR 601.2b: bare X (no where-clause) resolves through the
        // spell-cost path — this is a genuine `{X}` announced at cast time, not an
        // ability-defined value, so `Variable{X}` is correct here.
        let target_text = rem.trim_start();
        let target = resolve_earthbend_target(text, target_text, true);
        return Some((
            target,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
        ));
    }
    Some((default_earthbend_target(), QuantityExpr::Fixed { value: 0 }))
}

/// Shared target-text reduction for earthbend parsing. Distinguishes between
/// "explicit target follows the numeric slot" and "use the default target".
/// Factored out so `parse_earthbend_params` and `parse_earthbend_count_expr`
/// can't drift in target detection.
fn resolve_earthbend_target(
    text: &str,
    target_text: &str,
    parsed_numeric_slot: bool,
) -> TargetFilter {
    let has_explicit_target = if !parsed_numeric_slot {
        false
    } else {
        let trimmed =
            target_text.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
        !trimmed.is_empty()
            && tag::<_, _, OracleError<'_>>("then ")
                .parse(trimmed)
                .is_err()
            && tag::<_, _, OracleError<'_>>("and ").parse(trimmed).is_err()
    };
    if has_explicit_target {
        let (t, _) = parse_target(&text[text.len() - target_text.len()..]);
        t
    } else {
        default_earthbend_target()
    }
}

/// Parse a dynamic count phrase that begins with "cards"/"a card" or "that
/// many" against an already-lowercased tail. Returns the resolved
/// `QuantityExpr` or `None` if no recognized shape matches.
///
/// Covers the cross-verb dynamic-count idioms shared by Draw/Mill/Discard:
/// - "cards equal to <ref>"  / "a card equal to <ref>"  → Ref{<ref>}
/// - "that many cards"       / "that many"              → Ref{EventContextAmount}
///
/// CR 121.1 / CR 701.13a / CR 701.8a — chained-effect amounts and target-
/// relative quantity refs route through one combinator instead of being
/// re-implemented per verb.
fn parse_dynamic_count_phrase(lower: &str) -> Option<QuantityExpr> {
    if let Ok((qty_tail, _)) = alt((
        tag::<_, _, OracleError<'_>>("cards equal to "),
        tag("a card equal to "),
    ))
    .parse(lower)
    {
        // CR 113.3 + CR 121.1: A granted-ability quotation closes at a comma
        // ("gains \"… draw cards equal to its toughness,\""), so the dynamic
        // count phrase can carry a trailing comma when the granted ability is
        // re-parsed from its body text (The Pride of Hull Clade). Strip trailing
        // sentence punctuation (period or comma) before the anaphoric-ref match.
        let qty_text = qty_tail.trim_end_matches(['.', ',']).trim();
        // CR 107.1a + CR 121.1: "cards equal to half the number of cards in
        // their library" — fraction-led dynamic draw count, rounded per CR
        // 107.1a. `qty_text` is already lowercase + trimmed, so call
        // `parse_fraction_rounded` directly (no `nom_on_lower` bridge), and
        // require full consumption before accepting the fraction expression.
        if let Ok((rest, expr)) =
            crate::parser::oracle_nom::quantity::parse_fraction_rounded(qty_text)
        {
            if rest.trim().is_empty() {
                return Some(expr);
            }
        }
        if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
            return Some(QuantityExpr::Ref { qty });
        }
    }
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("that many cards"),
        tag("that many"),
    ))
    .parse(lower)
    {
        // M3 guard: only match when the tail is empty/punctuation. Avoids
        // false positives like "that many cards from the top of their library".
        if rest.trim_start_matches('.').trim().is_empty() {
            return Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            });
        }
    }
    None
}

fn parse_life_verb_remainder<'a>(
    text: &'a str,
    lower: &str,
    verb: &str,
    third_person: &str,
) -> Option<&'a str> {
    let you_verb = format!("you {verb} ");
    let bare_verb = format!("{verb} ");
    let direct_third_person = format!("{third_person} ");
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value(
            (),
            alt((
                tag(you_verb.as_str()),
                tag(bare_verb.as_str()),
                tag(direct_third_person.as_str()),
            )),
        )
        .parse(input)
    }) {
        return Some(rest);
    }

    let subject_predicate = format!(" {third_person} ");
    nom_on_lower(text, lower, |input| {
        let (rest, _) = (
            take_until::<_, _, OracleError<'_>>(subject_predicate.as_str()),
            tag(subject_predicate.as_str()),
        )
            .parse(input)?;
        Ok((rest, ()))
    })
    .map(|(_, rest)| rest)
}

/// CR 119.3 + CR 115.1: "the amount of life [they|that player] [lost|gained]
/// this turn" in a *targeted* life-change context ("Target opponent loses life
/// equal to the amount of life they lost this turn" — Astarion, the Decadent's
/// Feed/Friends modes). The third-person "they"/"that player" anaphor refers to
/// the effect's player target, so it resolves through `PlayerScope::Target`
/// (read from `ability.targets` by `resolve_quantity_with_targets`) — the same
/// player the surrounding LoseLife/GainLife affects, so a target opponent loses
/// life equal to *their own* life lost this turn.
///
/// The shared `parse_life_lost_ref` "they lost"/"that player lost" arms now also
/// emit `PlayerScope::Target` (not `Controller`), so the bare article-only
/// each-opponent phrasing ("the life they lost this turn", Archfiend of Despair /
/// Wound Reflection) is `Target`-scoped at the leaf and then rebound to
/// `ScopedPlayer` by `rewrite_player_scope_refs` under the per-opponent
/// `player_scope` loop. This recognizer still runs first for the "amount of"
/// gloss (Astarion), yielding the identical `Target` mapping, so both phrasings
/// resolve to the affected player's own life lost this turn.
fn parse_target_relative_life_change_this_turn(qty_text: &str) -> Option<QuantityExpr> {
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("the "))
        .parse(qty_text)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("amount of life ")
        .parse(rest)
        .ok()?;
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("they "), tag("that player ")))
        .parse(rest)
        .ok()?;
    let (rest, gained) = alt((
        value(false, tag::<_, _, OracleError<'_>>("lost")),
        value(true, tag("gained")),
    ))
    .parse(rest)
    .ok()?;
    // "this turn" is the canonical duration; tolerate its absence for callers
    // that strip trailing durations before this point.
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(" this turn"))
        .parse(rest)
        .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let qty = if gained {
        QuantityRef::LifeGainedThisTurn {
            player: PlayerScope::Target,
        }
    } else {
        QuantityRef::LifeLostThisTurn {
            player: PlayerScope::Target,
        }
    };
    Some(QuantityExpr::Ref { qty })
}

fn parse_life_equal_quantity(after_verb_lower: &str) -> Option<QuantityExpr> {
    let (qty_text, _) = tag::<_, _, OracleError<'_>>("life equal to ")
        .parse(after_verb_lower)
        .ok()?;
    let qty_text = qty_text.trim_end_matches('.').trim();
    // CR 115.1: target-relative "they/that player lost/gained this turn" → Target
    // scope. Tried before the generic delegation, which would otherwise map the
    // third-person anaphor to the controller (see helper doc).
    if let Some(qty) = parse_target_relative_life_change_this_turn(qty_text) {
        return Some(qty);
    }
    if let Some(qty) = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text) {
        return Some(qty);
    }
    crate::parser::oracle_quantity::parse_quantity_ref(qty_text)
        .map(|qty| QuantityExpr::Ref { qty })
}

/// CR 119.3 + CR 102.1: "gain 1 life for each player" (a/an/1) → the count of
/// every player in the game (`PlayerFilter::All`). Narrow to the one-life-per
/// form, the only shape that occurs (Benediction of Moons); other multipliers
/// fall through to the generic quantity paths. `text` is the remainder after
/// the "gain" verb, lowercased.
fn parse_gain_life_per_player(after_gain_lower: &str) -> Option<QuantityExpr> {
    // `(1|a|an) life for each player(s)` — `players` first so the longer tag
    // wins; the optional trailing period is consumed so `all_consuming` accepts
    // both the sentence-final and mid-clause forms.
    all_consuming((
        alt((
            tag::<_, _, OracleError<'_>>("1 life for each "),
            tag("a life for each "),
            tag("an life for each "),
        )),
        alt((tag("players"), tag("player"))),
        opt(tag(".")),
    ))
    .parse(after_gain_lower.trim())
    .ok()
    .map(|_| QuantityExpr::Ref {
        qty: QuantityRef::PlayerCount {
            filter: crate::types::ability::PlayerFilter::All,
        },
    })
}

pub(super) fn parse_numeric_imperative_ast(
    text: &str,
    lower: &str,
) -> Option<NumericImperativeAst> {
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| value((), tag("draw ")).parse(input))
        .or_else(|| {
            nom_on_lower(text, lower, |input| {
                let (rest, _) = (
                    take_until::<_, _, OracleError<'_>>(" draws "),
                    tag(" draws "),
                )
                    .parse(input)?;
                Ok((rest, ()))
            })
        })
    {
        // CR 608.2d + CR 121.1: "draw up to N cards" — opt-choice draw,
        // mirrors the up_to pattern on Discard / Sacrifice. Strip the prefix
        // before count parsing so "up to two cards" → count=Fixed{2}, up_to=true.
        let (rest, up_to) = match nom_on_lower(rest, &rest.to_ascii_lowercase(), |i| {
            value((), tag("up to ")).parse(i)
        }) {
            Some((_, after)) => (after, true),
            None => (rest, false),
        };
        // CR 121.1 / CR 107.1: dynamic-count tails — "cards equal to <ref>",
        // "a card equal to <ref>", "that many cards", "that many". The count is a
        // game-state integer reference, not the CR 609.3 "do as much as possible"
        // rule.
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(count) = parse_dynamic_count_phrase(rest_lower.as_str()) {
            return Some(NumericImperativeAst::Draw { count, up_to });
        }
        // CR 121.1: Bare plural "draw cards" / "draw card" with no quantifier is
        // only produced as an aggregate/for-each head after the dynamic count tail
        // ("... equal to the number of <X> votes" / "... for each <X> vote") has
        // already been stripped by the vote parser. With the dynamic tail removed
        // the residue carries no quantity, so a Fixed{1} placeholder cannot hide a
        // dynamic-quantity gap (the coverage concern guarded just below): the vote
        // aggregate binder immediately rebinds this slot to QuantityRef::VoteCount.
        // "draw a card" / "draw N cards" are unaffected — they reach
        // parse_count_expr with a parseable count; only a bare "card" / "cards"
        // remainder (nothing else) trips this arm.
        if all_consuming(value(
            (),
            alt((tag::<_, _, OracleError<'_>>("cards"), tag("card"))),
        ))
        .parse(rest.trim().trim_end_matches('.'))
        .is_ok()
        {
            return Some(NumericImperativeAst::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                up_to,
            });
        }
        // CR 121.1: When the verb committed but the quantity phrase
        // can't be classified, return None so the line surfaces as
        // `Effect::Unimplemented` upstream. Silently substituting Fixed{1} hides
        // dynamic-quantity gaps from the coverage report.
        let (mut count, remainder) = parse_count_expr(rest)?;
        // CR 121.1 + CR 107.1: a trailing "for each <countable>" multiplier scales
        // the draw count ("draw a card for each spell you've cast this turn …") by
        // an integer per-each quantity — count templating, not the CR 609.3 "do as
        // much as possible" rule. Attach it from the count parser's exact
        // remainder via the shared anchored for-each authority, rebinding the
        // base Fixed count to factor × <for-each>.
        // Only upgrades a successfully parsed Fixed count — preserves Fixed(1) when
        // no/unparsed `for each` tail, and keeps a dynamic base unchanged.
        let remainder = remainder.trim().trim_end_matches('.').trim();
        if let Some(for_each_expr) = parse_for_each_multiplier_prefix(remainder) {
            count = replace_fixed_quantity(count, for_each_expr);
        } else if let Some(where_x_count) = parse_draw_cards_where_x_tail(remainder) {
            count = where_x_count;
        } else if !draw_count_tail_is_complete(remainder) {
            return None;
        }
        return Some(NumericImperativeAst::Draw { count, up_to });
    }

    if let Some(after_gain) = parse_life_verb_remainder(text, lower, "gain", "gains") {
        let after_lower = after_gain.to_ascii_lowercase();
        // CR 119.3 + CR 102.1: "gain N life for each player" — the life gained is
        // the number of players (the per-each amount is 1). Benediction of Moons.
        // Probed before the bare-quantity fallback, which would otherwise parse
        // "1" and silently drop the "for each player" multiplier.
        if let Some(amount) = parse_gain_life_per_player(&after_lower) {
            return Some(NumericImperativeAst::GainLife { amount });
        }
        // CR 119.3: Handle "life equal to {quantity}" — dynamic amount from game state.
        // CR 119.3: target-relative quantity refs ("target creature's
        // power/toughness/mana value"). Mirrors LoseLife. Soul's Grace,
        // Heron's Grace Champion, Lifeblood Hydra, etc.
        if let Some(amount) = parse_life_equal_quantity(after_lower.as_str()) {
            return Some(NumericImperativeAst::GainLife { amount });
        }
        // CR 119.3: "gain that much life" / "gain that many life" —
        // amount is the triggering event's amount (Exquisite Blood). Extract the
        // amount phrase before " life" and route through the event-context
        // quantity parser so "that much" resolves to `EventContextAmount`
        // rather than defaulting to 1.
        // CR 614.1a: First try the full phrase including any " life plus N" /
        // " life minus N" suffix. The Offset-aware combinator in
        // `parse_event_context_quantity` recognises the post-quantifier noun
        // ("that much life plus 1" / "that many life minus 2"), so cards
        // like Heron of Hope, Angel of Vitality, Leyline of Hope must be
        // probed BEFORE the bare-quantifier path strips " life" via
        // `take_until` (which would discard the offset clause).
        //
        // Strip the trailing " instead" rider via PATTERNS.md §2a:
        // `terminated(take_until(...), opt(tag(...)))` — the parser stops at
        // the suffix and consumes it (when present), leaving the body for
        // the offset combinator. Falls back to the trimmed full phrase
        // when the suffix is absent (Exquisite Blood-class non-replacement
        // gain-life).
        let full_phrase = after_lower.trim_end_matches('.').trim();
        let full_phrase_no_instead = terminated(
            take_until::<_, _, OracleError<'_>>(" instead"),
            opt(tag(" instead")),
        )
        .parse(full_phrase)
        .map(|(_rem, body)| body.trim_end())
        .unwrap_or(full_phrase);
        if let Some(qty) =
            crate::parser::oracle_quantity::parse_event_context_quantity(full_phrase_no_instead)
        {
            return Some(NumericImperativeAst::GainLife { amount: qty });
        }
        let amount_phrase = take_until::<_, _, OracleError<'_>>(" life")
            .parse(after_lower.as_str())
            .map(|(_, before)| before.trim())
            .unwrap_or(full_phrase);
        if let Some(qty) =
            crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
        {
            return Some(NumericImperativeAst::GainLife { amount: qty });
        }
        // CR 119.3: GainLife committed but quantity phrase unclassified —
        // surface as Unimplemented rather than fabricating Fixed{1}.
        let amount = parse_count_expr(after_gain).map(|(q, _)| q)?;
        return Some(NumericImperativeAst::GainLife { amount });
    }

    if let Some(after_lose) = parse_life_verb_remainder(text, lower, "lose", "loses") {
        let after_lower = after_lose.to_ascii_lowercase();
        if let Some(expr) = try_parse_half_life_amount(after_lower.as_str()) {
            return Some(NumericImperativeAst::LoseLife { amount: expr });
        }
        // CR 119.3: Handle "life equal to {quantity}" — dynamic amount from game state.
        // CR 119.3: target-relative quantity refs ("target creature's
        // power/toughness/mana value", etc.) — Final Punishment, Tomb
        // Blade-class drain, Genesis of the Daleks. Delegates to the
        // shared `parse_quantity_ref` building block.
        if let Some(amount) = parse_life_equal_quantity(after_lower.as_str()) {
            return Some(NumericImperativeAst::LoseLife { amount });
        }
        // CR 119.3: "lose that much life" / "lose that many life" —
        // amount is the triggering event's amount. Probe for event-context phrases
        // before falling back to the numeric last-word extractor.
        if let Ok((_, before_life)) =
            take_until::<_, _, OracleError<'_>>("life").parse(after_lower.as_str())
        {
            let amount_phrase = take_until::<_, _, OracleError<'_>>(" life")
                .parse(after_lower.as_str())
                .map(|(_, before)| before.trim())
                .unwrap_or_else(|_: nom::Err<OracleError<'_>>| {
                    after_lower.trim_end_matches('.').trim()
                });
            if let Some(qty) =
                crate::parser::oracle_quantity::parse_event_context_quantity(amount_phrase)
            {
                return Some(NumericImperativeAst::LoseLife { amount: qty });
            }
            if let Some((amount, remainder)) = parse_count_expr(amount_phrase) {
                if remainder.trim().is_empty() {
                    return Some(NumericImperativeAst::LoseLife { amount });
                }
            }
            // CR 119.3: LoseLife committed but neither the event-context phrase
            // nor a numeric tail parsed — return None so the line lands in
            // `Effect::Unimplemented` upstream instead of fabricating Fixed{1}.
            // (This is the Keen Duelist class: "lose life equal to <unparsed>".)
            let before_life = before_life.trim();
            let last_word = before_life.split_whitespace().next_back().unwrap_or("");
            let amount = parse_count_expr(last_word).map(|(q, _)| q)?;
            return Some(NumericImperativeAst::LoseLife { amount });
        }
        return None;
    }

    if nom_primitives::scan_contains(lower, "gets +")
        || nom_primitives::scan_contains(lower, "gets -")
        || nom_primitives::scan_contains(lower, "get +")
        || nom_primitives::scan_contains(lower, "get -")
    {
        // Accept any pump — discard the target. Callers that need subject threading
        // (e.g., try_parse_for_each_effect) extract the subject separately via
        // thread_for_each_subject after lowering the AST.
        if let Some(Effect::Pump {
            power, toughness, ..
        }) = super::try_parse_pump(lower, text)
        {
            return Some(NumericImperativeAst::Pump { power, toughness });
        }
    }

    // Keyword action verbs with numeric count: scry N, surveil N, mill N.
    // CR 701.22a + CR 701.25a: Oracle uses third-person conjugations
    // ("Target player scries 2", "Target opponent surveils 1") — match both
    // the bare-form imperative and the conjugated form. `mills` is included
    // for symmetry with the "Target player mills N" pattern.
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value("scry", alt((tag("scry "), tag("scries ")))),
            value("surveil", alt((tag("surveil "), tag("surveils ")))),
            value("mill", alt((tag("mill "), tag("mills ")))),
        ))
        .parse(input)
    }) {
        // CR 121.1 / CR 107.1 / CR 701.13a: dynamic-count tails for the
        // shared mill/scry/surveil family. The count is a game-state integer
        // reference, not the CR 609.3 "do as much as possible" rule.
        let rest_lower = rest.to_ascii_lowercase();
        if let Some(count) = parse_dynamic_count_phrase(rest_lower.as_str()) {
            // allow-noncombinator: dispatch on already-parsed verb tag (combinator output, not Oracle text)
            return match verb {
                "scry" => Some(NumericImperativeAst::Scry { count }), // allow-noncombinator: combinator-output dispatch
                "surveil" => Some(NumericImperativeAst::Surveil { count }), // allow-noncombinator: combinator-output dispatch
                "mill" => Some(NumericImperativeAst::Mill { count }), // allow-noncombinator: combinator-output dispatch
                _ => unreachable!(),
            };
        }
        // CR 701.22a / CR 701.25a / CR 701.13a: Scry/Surveil/Mill verbs always
        // require a count. If the count phrase doesn't parse, return None so the
        // line surfaces as Unimplemented rather than silently scrying/milling 1.
        let count = parse_count_expr(rest).map(|(q, _)| q)?;
        return match verb {
            "scry" => Some(NumericImperativeAst::Scry { count }),
            "surveil" => Some(NumericImperativeAst::Surveil { count }),
            "mill" => Some(NumericImperativeAst::Mill { count }),
            _ => unreachable!(),
        };
    }

    None
}

/// CR 107.1a: Parse "lose(s) half [possessive] life, rounded up/down" →
/// `DivideRounded` expression by delegating to the shared quantity combinator.
///
/// Strips the `lose(s) ` verb prefix, then runs
/// [`super::super::oracle_nom::quantity::parse_half_rounded`] over the
/// remainder so every possessive quantity the combinator recognizes
/// (`"half their life"`, `"half your life total"`, `"half his or her life"`,
/// …) unlocks a typed amount. Previously this helper hand-rolled a small
/// `their life` / `your life` dispatch that (a) dropped "their life total"
/// and (b) silently mis-bound the nom remainder. Both bugs disappear by
/// routing through the shared combinator.
fn try_parse_half_life_amount(lower: &str) -> Option<QuantityExpr> {
    // Delegate to the shared "half ..." combinator. This picks up the
    // possessive inner ref AND the rounding suffix in one call.
    let (_, expr) =
        super::super::oracle_nom::quantity::parse_half_rounded(lower.trim_start()).ok()?;
    Some(expr)
}

/// CR 606.3: Recognize the printed Chain Veil class — a single ability that
/// raises the per-permanent CR 606.3 loyalty-activation cap by N for every
/// planeswalker the controller controls. Two surface forms are accepted:
///
///   - "for each planeswalker you control, you may activate one of its loyalty
///     abilities once this turn as though none of its loyalty abilities have
///     been activated this turn." — the printed Chain Veil wording. The
///     "for each planeswalker you control" preamble identifies the beneficiaries
///     (each planeswalker gets +1 cap), not a repeat count;
///     `strip_for_each_prefix` bails out on this pattern so the imperative
///     dispatch sees the full text here.
///   - "activate each planeswalker's loyalty ability an additional time this
///     turn" / "an additional N times this turn" — future-proof generalization
///     after the outer "you may " has been stripped by
///     `strip_optional_effect_prefix`. Numeric variant supports cards that grant
///     more than one extra activation in a single resolution.
fn parse_grant_extra_loyalty_activations(lower: &str) -> Option<QuantityExpr> {
    if let Ok((_, amount)) = parse_chain_veil_for_each_form(lower) {
        return Some(amount);
    }
    parse_each_planeswalker_additional_form(lower)
        .map(|(_, amount)| amount)
        .ok()
}

/// CR 606.3: "For each planeswalker you control, you may activate one of its
/// loyalty abilities once this turn as though none of its loyalty abilities
/// have been activated this turn." — printed Chain Veil text. Always grants
/// +1 per planeswalker (the printed wording has no numeric variant).
fn parse_chain_veil_for_each_form(
    lower: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    let (rest, _) = (
        tag("for each "),
        tag("planeswalker"),
        tag(" you control, "),
        parse_single_loyalty_permission,
    )
        .parse(lower)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    Ok((rest, QuantityExpr::Fixed { value: 1 }))
}

fn parse_single_loyalty_permission(lower: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    let (rest, _) = (
        tag("you may activate one of its "),
        tag("loyalty abilities"),
        tag(" once this turn"),
    )
        .parse(lower)?;
    let (rest, _) = opt((
        tag(" as though none of its "),
        tag("loyalty abilities"),
        tag(" have been activated this turn"),
    ))
    .parse(rest)?;
    Ok((rest, ()))
}

/// CR 606.3: "activate each planeswalker's loyalty ability an additional time
/// this turn" / "an additional N times this turn" — generalization after
/// `strip_optional_effect_prefix` has consumed the outer "you may ".
fn parse_each_planeswalker_additional_form(
    lower: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    let (rest, _) = (
        tag("activate "),
        tag("each planeswalker"),
        tag("'s "),
        tag("loyalty ability "),
    )
        .parse(lower)?;
    alt((
        // "an additional N times this turn" → +N
        map(
            (
                tag::<_, _, OracleError<'_>>("an additional "),
                nom_primitives::parse_number,
                tag(" times this turn"),
            ),
            |(_, n, _)| QuantityExpr::Fixed { value: n as i32 },
        ),
        // "an additional time this turn" → +1
        value(
            QuantityExpr::Fixed { value: 1 },
            tag::<_, _, OracleError<'_>>("an additional time this turn"),
        ),
    ))
    .parse(rest)
}

pub(super) fn lower_numeric_imperative_ast(ast: NumericImperativeAst) -> Effect {
    match ast {
        // CR 121.1: Default `target: TargetFilter::Controller` — the imperative
        // path doesn't see the subject, which is later threaded via
        // `inject_subject_target` for "target player draws ..." patterns
        // (CR 601.2c per-mode targeting).
        // CR 121.1 + CR 608.2d: Lower the AST `up_to: bool` into the typed
        // `count: QuantityExpr::UpTo { max }` wrapper. Plain `up_to: false`
        // leaves the count expression unchanged; `up_to: true` wraps it so
        // the resolver peels it back at runtime.
        NumericImperativeAst::Draw { count, up_to } => Effect::Draw {
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            target: TargetFilter::Controller,
        },
        NumericImperativeAst::GainLife { amount } => Effect::GainLife {
            amount,
            player: TargetFilter::Controller,
        },
        NumericImperativeAst::LoseLife { amount } => Effect::LoseLife {
            amount,
            target: None,
        },
        // CR 608.2c: Pump uses TargetFilter::Any as a sentinel — callers
        // (inject_subject_target, thread_for_each_subject) replace it with the
        // parsed subject's target. No warning here; Any is an expected intermediate.
        NumericImperativeAst::Pump { power, toughness } => Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        },
        // CR 701.22a + CR 601.2c: Default Controller target — `inject_subject_target`
        // upgrades to `TargetFilter::Player` for "target player scrys ..." subjects.
        NumericImperativeAst::Scry { count } => Effect::Scry {
            count,
            target: TargetFilter::Controller,
        },
        // CR 701.25a + CR 601.2c: Same Controller default; subject promotion
        // wires "target opponent surveils ..." through inject_subject_target.
        NumericImperativeAst::Surveil { count } => Effect::Surveil {
            count,
            target: TargetFilter::Controller,
        },
        NumericImperativeAst::Mill { count } => Effect::Mill {
            count,
            // CR 701.17a: "Mill" with no subject defaults to the controller.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        },
    }
}

/// Strip leading "a " / "an " article from target text before passing to `parse_target`.
/// Follows the same pattern used by `oracle_cost.rs` for sacrifice cost parsing.
fn strip_article(text: &str) -> &str {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, nom_primitives::parse_article)
        .map(|(_, rest)| rest)
        .unwrap_or(text)
}

/// CR 107.1a + CR 701.21a: Extract the typed filter embedded in an
/// `ObjectCount` quantity expression. Used by the sacrifice AST builder to
/// lift "half the permanents they control" → ObjectCount's filter into the
/// effect's target, so eligibility matches the same set the count was
/// computed against. Recurses through `DivideRounded` / `Multiply` / `Offset`
/// wrappers since the filter belongs to the innermost ObjectCount; returns
/// `None` for expressions that carry no filter (Fixed, Variable(X), etc.).
fn extract_object_count_filter(expr: &QuantityExpr) -> Option<TargetFilter> {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } => Some(filter.clone()),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Offset { inner, .. } => extract_object_count_filter(inner),
        _ => None,
    }
}

/// CR 608.2c: Extract "unless you discard a [type] card" suffix from discard text.
/// Returns the text with the suffix stripped and the parsed filter, or the original text
/// with None if no "unless you discard" clause is present.
///
/// Handles: creature, artifact, instant or sorcery, basic land, enchantment, subtype cards.
fn parse_discard_unless_filter<'a>(
    lower: &'a str,
    _original: &'a str,
) -> (&'a str, Option<TargetFilter>) {
    let Some((before, after_unless)) = split_around(lower, " unless you discard ") else {
        return (lower, None);
    };

    // Strip leading article "a " / "an "
    let type_text = strip_article(after_unless);
    // Strip trailing " card" / " card." — parse_target expects type phrase without "card"
    let type_text = type_text
        .strip_suffix(" card.")
        .or_else(|| type_text.strip_suffix(" card"))
        .unwrap_or(type_text)
        .trim_end_matches('.');

    let (filter, _) = parse_target(type_text);
    if matches!(filter, TargetFilter::Any) {
        // parse_target couldn't parse the type — don't strip
        return (lower, None);
    }
    (before, Some(filter))
}

/// CR 701.9a + CR 608.2c: Parse the card-type filter portion of a discard phrase.
///
/// Recognizes "a <type> card" / "an <type> card" / "<N> <type> cards" where the
/// type portion is anything `parse_target` understands (subtypes, core types,
/// "instant or sorcery", etc.). Returns `None` when no type qualifier appears
/// (plain "a card" / "N cards" means any card is legal).
///
/// Mirrors `AbilityCost::Discard.filter` so the trigger-effect discard on
/// Dokuchi Silencer ("you may discard a creature card") preserves the same
/// filter data as cost-form discards like "Discard a creature card:".
/// Extract the type qualifier from the post-count tail of a discard noun phrase.
///
/// Caller contract: `tail` is the text **after** the count token has already
/// been consumed by `parse_count_expr`. So for "discard two creature cards"
/// the count parser eats "two " and hands "creature cards" here. For "a card"
/// (count = 1, no type qualifier) the count parser eats "a " and hands
/// "card" here, which has no leading type word and returns `None`.
///
/// Mirrors `AbilityCost::Discard.filter` so the trigger-effect discard on
/// Dokuchi Silencer ("you may discard a creature card") preserves the same
/// filter data as cost-form discards like "Discard a creature card:".
pub(crate) fn parse_discard_card_filter(tail: &str) -> Option<TargetFilter> {
    // Find the " card" / " cards" suffix — the type phrase lies before it.
    // No suffix or empty before-suffix → no type qualifier.
    let type_phrase = tail
        .strip_suffix(" cards") // allow-noncombinator: structural suffix cleanup on pre-chunked sub-phrase (PATTERNS.md §9)
        .or_else(|| tail.strip_suffix(" card"))? // allow-noncombinator: see line above
        .trim();
    if type_phrase.is_empty() {
        return None;
    }
    let (filter, remainder) = parse_target(type_phrase);
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some(filter)
}

/// CR 701.21a + CR 608.2k: When a targeted-action body has been stripped of an
/// actor prefix ("you (may) ", "an opponent (may) ", "each opponent ", "each
/// player "), `ctx.actor` carries the resolving player's controller-ref. This
/// helper defaults `TargetFilter::Typed.controller` to that actor whenever the
/// parsed target phrase didn't supply one. Without the default, the resolver
/// treats `controller: None` as Any — letting the actor sacrifice / discard /
/// return any object on the battlefield, violating CR 701.21a (sacrifice) and
/// the analogous owner / controller restrictions on other actor-bound verbs.
fn apply_actor_default(filter: &mut TargetFilter, ctx: &mut ParseContext) {
    if let Some(actor) = ctx.actor.as_ref() {
        attach_controller_if_absent(filter, actor.clone());
    }
}

/// CR 701.21a/b: "of their choice" / "of your choice" confirms who chooses
/// what to sacrifice; it is not part of the sacrificed-object filter. Preserve
/// trailing relative clauses so `parse_target` can consume predicates like
/// "that shares a card type with it".
fn strip_sacrifice_choice_marker(target_text: &str) -> String {
    let lower = target_text.to_lowercase();

    if let Some((_, rest)) = nom_on_lower(target_text, &lower, |input| {
        value((), alt((tag("of their choice"), tag("of your choice")))).parse(input)
    }) {
        return rest.trim_start().to_string();
    }

    if let Some((before_len, rest)) = nom_on_lower(target_text, &lower, |input| {
        let (rest, before) = alt((
            terminated(take_until(" of their choice"), tag(" of their choice")),
            terminated(take_until(" of your choice"), tag(" of your choice")),
        ))
        .parse(input)?;
        Ok((rest, before.len()))
    }) {
        let before = &target_text[..before_len];
        return format!("{before}{rest}");
    }

    target_text.to_string()
}

fn strip_sacrifice_count_suffix(target_text: &str) -> String {
    let trimmed = target_text.trim_start();
    let lower = trimmed.to_lowercase();
    if nom_on_lower(trimmed, &lower, |input| {
        value(
            (),
            (
                alt((
                    tag::<_, _, OracleError<'_>>(", rounded up"),
                    tag(", rounded down"),
                    tag(", round up"),
                    tag(", round down"),
                )),
                opt(tag(".")),
                eof,
            ),
        )
        .parse(input)
    })
    .is_some()
    {
        String::new()
    } else {
        target_text.to_string()
    }
}

/// CR 701.21a + CR 107.1c: Parse "one or more [objects]" / "any number of
/// [objects]" sacrifice choices. `UpTo(ObjectCount(filter))` provides the
/// dynamic upper bound; `min_count` carries the printed lower bound — `1` for
/// "one or more", and `0` for "any number of" (CR 107.1c: a player choosing
/// "any number" may choose any positive number or zero).
pub(super) fn parse_one_or_more_sacrifice(
    text: &str,
    ctx: &mut ParseContext,
) -> Option<(QuantityExpr, TargetFilter, usize)> {
    let lower = text.to_lowercase();
    let (min_count, filter_text) = nom_on_lower(text, &lower, |input| {
        alt((
            value(1usize, tag("one or more ")),
            value(0usize, tag("any number of ")),
        ))
        .parse(input)
    })?;
    let target_text = strip_sacrifice_count_suffix(&strip_sacrifice_choice_marker(
        strip_article(filter_text.trim_start()).trim_end_matches('.'),
    ));
    let (mut target, remainder) = parse_type_phrase(target_text.trim());
    if !remainder.trim().is_empty() || matches!(target, TargetFilter::Any) {
        return None;
    }
    apply_actor_default(&mut target, ctx);
    let count = QuantityExpr::up_to(QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: target.clone(),
        },
    });
    Some((count, target, min_count))
}

/// CR 701.21a: "sacrifice all <filter>" carries a mandatory count equal to the
/// eligible object pool (`QuantityRef::ObjectCount` over the parsed filter).
/// This lets the sacrifice resolver's existing mandatory-all fast path perform
/// every legal sacrifice without a one-card special case.
pub(super) fn parse_all_sacrifice<'a>(
    text: &'a str,
    ctx: &mut ParseContext,
) -> Option<(QuantityExpr, TargetFilter, &'a str)> {
    let lower = text.to_lowercase();
    let ((), rest) = nom_on_lower(text, &lower, |input| value((), tag("all ")).parse(input))?;
    let (mut target, rem) = parse_target_with_ctx(rest.trim_start(), ctx);
    if matches!(target, TargetFilter::Any) {
        return None;
    }
    apply_actor_default(&mut target, ctx);
    let count = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: target.clone(),
        },
    };
    Some((count, target, rem))
}

fn add_another_to_sacrifice_filter(filter: &mut TargetFilter) {
    match filter {
        TargetFilter::Typed(typed) if !typed.properties.contains(&FilterProp::Another) => {
            typed.properties.push(FilterProp::Another);
        }
        TargetFilter::Typed(_) => {}
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for leg in filters {
                add_another_to_sacrifice_filter(leg);
            }
        }
        _ => {}
    }
}

/// NOTE: Shares verb prefixes with `try_parse_verb_and_target` in `mod.rs`.
/// When adding a new targeted verb here, check if it also needs to be added there
/// (for compound action splitting like "tap target creature and put a counter on it").
pub(super) fn parse_targeted_action_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<TargetedImperativeAst> {
    // CR 701.26a/b: Tap/untap all — mass variants must be checked before single-target
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("tap all "), tag("tap each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::TapAll { target });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("untap all "), tag("untap each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::UntapAll { target });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("goad all "), tag("goad each ")))).parse(input)
    }) {
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::GoadAll { target });
    }
    // CR 701.21a: "sacrifice [count] <filter> [of their choice]" —
    // delegates to `parse_count_expr` so "a"/"an"/"X"/"half the permanents
    // they control" all flow through one authority. "Of their choice" is
    // redundant because only a permanent's controller can sacrifice it; strip
    // it as a confirmation suffix rather than bleeding into the filter.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("sacrifice ")).parse(input)
    }) {
        if let Some((count, target, _rem)) = parse_all_sacrifice(rest, ctx) {
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_rem, text);
            return Some(TargetedImperativeAst::Sacrifice {
                target,
                count,
                min_count: 0,
            });
        }
        if let Some((count, target, min_count)) = parse_one_or_more_sacrifice(rest, ctx) {
            return Some(TargetedImperativeAst::Sacrifice {
                target,
                count,
                min_count,
            });
        }
        // Use the exclusion-aware sibling so a source-exclusion "another"
        // ("sacrifice another creature or land") is reported as a typed
        // `CountWord::SourceExclusion` instead of being silently consumed as a
        // bare count of 1. `parse_count_expr` discards the word's identity, so
        // without this the parsed target never regains `FilterProp::Another`
        // and the source could sacrifice itself (Morkrut Necropod, #4513).
        let (mut count, after_count, count_word) =
            super::super::oracle_util::parse_count_expr_with_exclusion(rest).unwrap_or((
                crate::types::ability::QuantityExpr::Fixed { value: 1 },
                rest,
                super::super::oracle_util::CountWord::Plain,
            ));
        let (target_text, _) = super::strip_optional_target_prefix(after_count.trim_start());
        // Strip the "of their choice" / "of your choice" confirmation suffix —
        // CR 701.21a makes the controller the sacrificing player, so the phrase is a no-op
        // that must be consumed so it doesn't bleed into the filter. Two
        // shapes exist: (1) the filter precedes the phrase ("permanents
        // they control of their choice" — split at the leading space), and
        // (2) the count subsumes the filter and only the phrase is left
        // ("of their choice" — treat the entire remainder as the phrase).
        let target_text = strip_sacrifice_choice_marker(target_text);
        // CR 107.1a: Parse and apply standalone trailing rounding suffix.
        if let Some(rounding) = parse_rounding_suffix_only(&target_text) {
            rewrite_quantity_expr_rounding(&mut count, rounding);
        }
        let target_text = strip_sacrifice_count_suffix(&target_text);
        // CR 107.2: Skip `parse_target` on an empty remainder — the count
        // subsumed the filter ("sacrifice half the permanents they control
        // of their choice"), so there is nothing left to classify. Avoids
        // emitting a `target-fallback` parse warning for a well-formed parse.
        //
        // CR 608.2k: When the remainder is a bare object pronoun ("it",
        // "itself", "them", "him", "her") AND the parse context carries an
        // explicit trigger subject, resolve the pronoun against that subject
        // instead of defaulting to `ParentTarget`. On a self-ETB trigger
        // ("When Phlage enters, sacrifice it unless it escaped"; "When
        // Azorius Herald enters, sacrifice it unless {U} was spent to cast it")
        // the subject is `SelfRef` and there is no outer targeted object, so
        // "it" binds to the source permanent. For context-free parses (e.g.
        // the populate anaphor chain "populate. … sacrifice it at the
        // beginning of the next end step") the antecedent is set later by
        // `rewrite_parent_target_to_last_created`, so we must preserve
        // `ParentTarget` when no subject is provided.
        //
        // **Helper choice:** routes through `resolve_it_pronoun`, NOT
        // `resolve_pronoun_target`. The latter returns `ParentTarget` for
        // `Some(SelfRef)` subjects to support parent-target chains
        // ("tap target creature. exile it") — but the sacrifice imperative
        // here is a trigger sub-effect with no parent-target chain in scope.
        // Switching to `resolve_pronoun_target` would break the Azorius
        // Herald / Balduvian Horde / Phlage class. See the
        // `self_etb_sacrifice_it_anaphor_binds_to_self_ref` regression test
        // in `oracle_trigger.rs` for the lock-in.
        let target = if target_text.trim().is_empty() {
            TargetFilter::Any
        } else if ctx.subject.is_some() && is_bare_object_pronoun(target_text.trim()) {
            resolve_it_pronoun(ctx)
        } else {
            let (target, _rem) = parse_target_with_ctx(&target_text, ctx);
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_rem, text);
            target
        };
        // CR 701.21a: When the count expression already carries a typed filter
        // ("half the permanents they control" → ObjectCount{Typed[Permanent,
        // controller:You]}) and the target text didn't yield a filter, lift the
        // count's filter into `target` so eligibility matches the same set the
        // count was computed against. Without this lift, Sacrifice would fall
        // back to `Any` and the parser-warned filter would be silently dropped.
        let mut target = if matches!(target, TargetFilter::Any) {
            extract_object_count_filter(&count).unwrap_or(target)
        } else {
            target
        };
        // CR 701.21a: Default the sacrificed permanent's controller to the
        // resolving player when the target phrase didn't specify one. "You may
        // sacrifice a non-Demon creature" must restrict the prompt to the
        // actor's permanents — sacrificing requires controlling the permanent.
        apply_actor_default(&mut target, ctx);
        // Re-apply the source exclusion the count word stripped. The count
        // grammar consumes "another " as a bare count of 1, discarding the
        // exclusion, so the filter built from the remainder ("creature or land")
        // carries no `FilterProp::Another` and would let the source sacrifice
        // itself (Morkrut Necropod, #4513).
        //
        // Apply the exclusion to every typed leg. "Another" means "not this
        // source permanent"; it is required on any leg the source's type could
        // match and is harmless (vacuous) on legs it cannot.
        //
        // No CR annotation here: this is parser-grammar scoping of the word
        // "another"; the exclusion itself is CR-annotated at the filter layer
        // (`game/filter.rs` `FilterProp::Another`).
        if matches!(
            count_word,
            super::super::oracle_util::CountWord::SourceExclusion
        ) {
            add_another_to_sacrifice_filter(&mut target);
        }
        return Some(TargetedImperativeAst::Sacrifice {
            target,
            count,
            min_count: 0,
        });
    }
    // Simple targeted verbs: tap, untap — parse target after verb prefix.
    // CR 701.26a/b: dispatch on the typed `TapStateChange` marker produced by the
    // combinator (not a stringly-typed verb), so the AST split is exhaustive.
    if let Some((state, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value(TapStateChange::Tap, tag("tap ")),
            value(TapStateChange::Untap, tag("untap ")),
        ))
        .parse(input)
    }) {
        // CR 115.1d + CR 701.26a: Preserve the "up to N target" count instead of
        // discarding it — "tap up to that many target creatures" (Nyssa of Traken)
        // carries a dynamic `EventContextAmount` count that the lowering threads
        // onto `ParsedEffectClause.multi_target` so the right number of optional
        // target slots is surfaced.
        let (target_text, multi_target) = super::strip_optional_target_prefix(strip_article(rest));
        // CR 608.2k: A bare object pronoun ("untaps it") in a subject-bearing
        // clause is an anaphor, not a parent-target chain. Route it through
        // `resolve_it_pronoun` — identical to the sacrifice/counter clauses in
        // the same instruction — so "that player gains control of ~, untaps it,
        // and puts a +1/+1 counter on it" (Alexios, Deimos of Kosmos) binds the
        // untap to `SelfRef` (the named source), and observer triggers like
        // "whenever a permanent you control enters tapped, untap it" (Amulet of
        // Vigor) bind to `TriggeringSource` (the entering permanent). Without a
        // subject (a true parent-target chain, "tap target creature. untap it")
        // the guard falls through to `parse_target_with_ctx` → `ParentTarget`.
        let target = if ctx.subject.is_some() && is_bare_object_pronoun(target_text.trim()) {
            resolve_it_pronoun(ctx)
        } else {
            let (target, _rem) = parse_target_with_ctx(target_text, ctx);
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_rem, text);
            target
        };
        return Some(match state {
            TapStateChange::Tap => TargetedImperativeAst::Tap {
                target,
                multi_target,
            },
            TapStateChange::Untap => TargetedImperativeAst::Untap {
                target,
                multi_target,
            },
        });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| value((), tag("goad ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(strip_article(rest));
        let (target, _rem) = parse_target_with_ctx(target_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Goad { target });
    }
    // CR 709.5f-g + CR 709.5j: "lock"/"unlock"/"lock or unlock [a] [locked] door
    // of [up to one] target Room you control". Compose the operation, the
    // optional "a " article, the optional "locked " narrowing (CR 709.5f: unlock
    // chooses a locked half — present on Ghostly Keybearer, absent on the
    // lock-or-unlock cards), the "door of " connective, and the "up to one "
    // optional-target flag with nom combinators, then hand the Room phrase to
    // the shared target parser. The op alternatives are ordered longest-first so
    // "lock or unlock " wins over the bare "lock " prefix. The eligible half is
    // chosen at resolution, so only the operation and the Room `TargetFilter`
    // are captured here.
    if let Some((op, rest)) = nom_on_lower(text, lower, |input| {
        let (input, op) = alt((
            value(DoorLockOp::LockOrUnlock, tag("lock or unlock ")),
            value(DoorLockOp::Unlock, tag("unlock ")),
            value(DoorLockOp::Lock, tag("lock ")),
        ))
        .parse(input)?;
        let (input, _) = opt(tag("a ")).parse(input)?;
        let (input, _) = opt(tag("locked ")).parse(input)?;
        let (input, _) = tag("door of ").parse(input)?;
        let (input, _) = opt(tag("up to one ")).parse(input)?;
        Ok((input, op))
    }) {
        let (target_text, _) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_target_with_ctx(target_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::SetRoomDoorLock { op, target });
    }
    if let Some((_, after_discard_orig)) =
        nom_on_lower(text, lower, |input| value((), tag("discard ")).parse(input))
    {
        let after_discard = &lower[lower.len() - after_discard_orig.len()..];
        // CR 701.9a: Back-reference discard — "discard that card" / "discard
        // those cards" / "discard it" target a specific card identified by the
        // parent effect (Seek/Conjure/Reveal-Choose populate ParentTarget at
        // runtime; Binding Negotiation: "You may choose a nonland card from it.
        // If you do, they discard it"). Must be checked before the
        // player-choice count-based discard path, since the anaphor is not a
        // count phrase. `all_consuming` keeps the bare pronoun arm from
        // shadowing "it at random" or longer phrases that begin with "it".
        if all_consuming(alt((
            tag::<_, _, OracleError<'_>>("that card"),
            tag("those cards"),
            tag("it"),
        )))
        .parse(after_discard.trim_end_matches(['.', ',']).trim_end())
        .is_ok()
        {
            return Some(TargetedImperativeAst::DiscardCard {
                target: TargetFilter::ParentTarget,
            });
        }
        // CR 701.9a: Detect "at random" suffix for random discard effects.
        let random = nom_primitives::scan_contains(after_discard, "at random");
        // CR 701.9b: Detect "up to" prefix for optional partial discard.
        let (after_discard, up_to) =
            match tag::<_, _, OracleError<'_>>("up to ").parse(after_discard) {
                Ok((rest, _)) => (rest, true),
                Err(_) => (after_discard, false),
            };
        // Strip "all the cards in " / "all cards in " prefix compositionally for
        // patterns like "discard all the cards in your hand" / "discards all cards in their hand".
        let after_discard = alt((
            tag::<_, _, OracleError<'_>>("all the cards in "),
            tag("all cards in "),
        ))
        .parse(after_discard)
        .map(|(rest, _)| rest)
        .unwrap_or(after_discard);
        // CR 109.5 + CR 115.10: Whole-hand discard. The possessive pronoun
        // disambiguates the hand-size owner. "Your hand" is the caster's hand
        // (`Controller`); "their hand" / "his or her hand" is the subject's
        // hand, represented here as `Target`. Under an each-player wrapper,
        // `rewrite_player_scope_refs` rewrites that target-scoped hand-size to
        // `ScopedPlayer`, so Windfall-style effects bind to the iterating
        // player instead of the caster.
        if let Ok((_, hand_owner)) = terminated(
            alt((
                value(
                    PlayerScope::Controller,
                    tag::<_, _, OracleError<'_>>("your"),
                ),
                value(PlayerScope::Target, tag("their")),
                value(PlayerScope::Target, tag("his or her")),
            )),
            tag(" hand"),
        )
        .parse(after_discard)
        {
            return Some(TargetedImperativeAst::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize { player: hand_owner },
                },
                random,
                up_to,
                unless_filter: None,
                filter: None,
            });
        }
        // CR 701.8a: "discard any number of [filter] cards" — opt-choice
        // discard where the player picks any 0..hand-size. Encoded as
        // count = HandSize with up_to = true so the controller chooses how
        // many to actually discard. Mind Maggots ("discard any number of
        // creature cards"), Fervent Mastery, Sirocco-class chains.
        if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("any number of ").parse(after_discard) {
            let filter = parse_discard_card_filter(rest);
            return Some(TargetedImperativeAst::Discard {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
                },
                random,
                up_to: true,
                unless_filter: None,
                filter,
            });
        }
        // CR 608.2c: Strip "unless you discard a [type] card" suffix before count parsing.
        // Compute original-case offset before the unless strip narrows the slice.
        let original_after = &text[text.len() - after_discard.len()..];
        let (after_discard, unless_filter) =
            parse_discard_unless_filter(after_discard, original_after);
        // Re-derive original_after for the narrowed (unless-stripped) text.
        let original_after = &original_after[..after_discard.len()];
        // CR 121.1 / CR 107.1 / CR 701.8a: dynamic-count tails for Discard
        // — Fervent Mastery, Hordewing Skaab discard sub-ability, Sirocco
        // chains, etc. The count is a game-state integer reference, not the
        // CR 609.3 "do as much as possible" rule.
        let after_discard_lower = after_discard.to_ascii_lowercase();
        if let Some(mut count) = parse_dynamic_count_phrase(after_discard_lower.as_str()) {
            // CR 608.2c: "If you do, discard that many cards" anaphorizes the
            // count from the preceding draw (Hordewing Skaab). EventContextAmount
            // incorrectly prefers the combat-damage trigger's match count and
            // can discard the entire hand (issue #3296).
            if matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }
            ) {
                count = QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount {
                        channel: crate::types::ability::DamageChannel::Total,
                    },
                };
            }
            let filter = parse_discard_card_filter(after_discard);
            return Some(TargetedImperativeAst::Discard {
                count,
                random,
                up_to,
                unless_filter,
                filter,
            });
        }
        // CR 701.8a: Discard count must be explicit (or the implicit 1 from
        // "a/an" inside `parse_count_expr`). If the count phrase doesn't parse,
        // return None so the line surfaces as Unimplemented.
        // Forward the post-count remainder to the filter probe so it never
        // re-sees the count token — the type qualifier (if any) is whatever
        // is left after the count was eaten.
        let (count, after_count) = parse_count_expr(original_after)?;
        // CR 701.9a + CR 608.2c: Extract card-type filter from phrases like
        // "a creature card" / "an artifact card". Mirrors the filter slot on
        // `AbilityCost::Discard` so trigger-effect discards carry the same
        // restriction data as cost discards (Dokuchi Silencer's "you may
        // discard a creature card").
        let filter = parse_discard_card_filter(after_count.trim_start());
        return Some(TargetedImperativeAst::Discard {
            count,
            random,
            up_to,
            unless_filter,
            filter,
        });
    }
    // CR 400.7 + CR 611.2c: Unified `return [all|each]?` dispatcher. Consumes
    // the verb plus an optional `all`/`each` plural quantifier, then routes
    // by destination + origin. Mass-bounce ("return all creatures to their
    // owners' hands") promotes to `ReturnAll` ⇒ `Effect::BounceAll`.
    // Battlefield-targeted return-all ("return all artifact and enchantment
    // cards from all graveyards to the battlefield") routes to ChangeZoneAll
    // so the resolver scans the full object set. Mirrors the `tap all`/`tap
    // each` precheck arms above plus the bare `tag("return ")` arm's
    // destination routing.
    // The combinator captures the plurality directly: `true` = mass quantifier
    // (`all`/`each`), `false` = single-target. Avoids post-hoc string
    // equality on the consumed head (parser-combinator gate).
    if let Some((is_mass, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value(true, alt((tag("return all "), tag("return each ")))),
            value(false, tag("return ")),
        ))
        .parse(input)
    }) {
        // CR 115.1d + CR 601.2c: Strip a bare, BOUNDED "up to N" quantifier
        // immediately after the verb before any destination or target parsing
        // runs below. Left in place, the quantifier's own embedded "to"
        // collides with the trailing "to <hand>" destination scan
        // (`strip_return_destination_ext` just below) and derails BOTH
        // destination recognition and the count together — Ill-Gotten Gains's
        // "returns up to three cards from their graveyard to their hand"
        // otherwise silently returns the ENTIRE graveyard instead of up to
        // three cards. `strip_leading_quantifier` is pure slice arithmetic, so
        // the remainder stays a true subslice of `rest` — no reconstruction
        // needed. It ALSO recognizes the unrelated unbounded "any number of"
        // quantifier (`max: None`) — that shape already has its own working
        // mechanism a few lines below (`any_number_prefix` → `return_up_to` →
        // `ReturnToZone.up_to`, the Grave Sifter class), so only act here on
        // the genuinely bounded `max: Some(_)` result to avoid double-handling
        // it or disturbing that already-correct path.
        let (stripped_rest, return_multi_target) = strip_leading_quantifier(rest);
        let return_multi_target = return_multi_target.filter(|spec| spec.max.is_some());
        let rest = if return_multi_target.is_some() {
            stripped_rest
        } else {
            rest
        };
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (trailing_target_text, trailing_dest) = super::strip_return_destination_ext(rest);
        let (leading_target_text, leading_dest) = super::strip_leading_return_destination_ext(rest);
        let (target_text, dest) = if leading_dest.is_some() {
            (leading_target_text, leading_dest)
        } else {
            (trailing_target_text, trailing_dest)
        };
        let (is_mass, target_text) = if let Some((_, rest)) =
            nom_on_lower(target_text, &target_text.to_ascii_lowercase(), |input| {
                value((), alt((tag("all "), tag("each ")))).parse(input)
            }) {
            (true, rest)
        } else {
            (is_mass, target_text)
        };
        // CR 107.1c: "return any number of [filter] cards" — strip the quantifier
        // before target parsing and carry `up_to` onto `ReturnToZone` (Grave Sifter).
        let target_lower = target_text.to_ascii_lowercase();
        let any_number_prefix = tag::<_, _, OracleError<'_>>("any number of ")
            .parse(target_lower.as_str())
            .ok()
            .map(|(rest, _)| target_lower.len() - rest.len());
        let (target_text, return_up_to) = match any_number_prefix {
            Some(consumed) => (&target_text[consumed..], true),
            None => (target_text, false),
        };
        let counted_return = parse_count_expr(target_text).and_then(|(mut count, after_count)| {
            let filter = extract_object_count_filter(&count)?;
            if nom_primitives::scan_contains(rest_lower, "rounded up") {
                count = match count {
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: _,
                    } => QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: crate::types::ability::RoundingMode::Up,
                    },
                    other => other,
                };
            } else if nom_primitives::scan_contains(rest_lower, "rounded down") {
                count = match count {
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: _,
                    } => QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding: crate::types::ability::RoundingMode::Down,
                    },
                    other => other,
                };
            }
            if after_count.trim().is_empty() {
                Some((filter, count))
            } else {
                None
            }
        });
        let count = counted_return.as_ref().map(|(_, count)| count.clone());
        // CR 115.1 + Whitemane Lion ruling: Use `parse_target_with_syntax` so
        // the "target"-keyword vs descriptor discriminator flows back from
        // this parse alone, with no cross-clause residue to clear.
        let (target, target_syntax, _count_for_shape) = match counted_return {
            Some((target, c)) => (target, TargetSyntax::TargetKeyword, c),
            None => {
                let (target, _rem, syntax) = parse_target_with_syntax(target_text, ctx);
                #[cfg(debug_assertions)]
                assert_no_compound_remainder(_rem, text);
                (target, syntax, QuantityExpr::Fixed { value: 0 })
            }
        };
        // CR 115.1: A bounce resolves at-resolution iff the Oracle text omitted
        // the word "target" AND the filter has a controller scope to enumerate
        // against (Whitemane Lion's "a creature you control" — the controller
        // picks at resolution time via EffectZoneChoice).
        let selection = if matches!(target_syntax, TargetSyntax::Descriptor)
            && filter_has_controller_scope(&target)
        {
            BounceSelection::AtResolution
        } else {
            BounceSelection::Targeted
        };
        let is_mass = is_mass || count.is_some();
        let origin = super::infer_origin_zone(rest_lower);
        // CR 400.7: A returned card's target filter must be scoped to its origin
        // zone. Without this, "return target ... card from your graveyard to the
        // battlefield" enumerates legal targets on the battlefield (the default
        // zone for a bare permanent filter) instead of the graveyard. Mirrors the
        // sibling `try_parse_verb_and_target` handler in `mod.rs`, which stamps
        // the same `InZone`/`Owned` constraints (e.g. Dance of the Manse's
        // graveyard-scoped "artifact and/or non-Aura enchantment cards"). Only
        // stamp when `parse_type_phrase` did not already capture the origin zone
        // itself ("... creature card from your graveyard" parses the zone + owner
        // inline) — re-stamping an already-zoned filter would double-scope it.
        let target = if target.extract_in_zone().is_none() {
            super::add_inferred_origin_constraints_to_target(target, origin, rest_lower)
        } else {
            target
        };

        // CR 400.7: Single-object battlefield destinations use ChangeZone;
        // mass destinations use ChangeZoneAll. Only pure mass-bounce
        // (battlefield⇒hand, no graveyard/library origin) promotes to
        // `ReturnAll` ⇒ `Effect::BounceAll`.
        return match dest {
            Some(d) if d.zone == Zone::Battlefield => {
                // CR 400.7: Mass returns to the battlefield route to
                // `ChangeZoneAll` regardless of any `enter_with_counters` — the
                // counters are threaded through so "return each creature card
                // from your graveyard to the battlefield. They enter with a
                // finality counter" (Shilgengar) applies the finality counter
                // (CR 122.1h) to every returned object, not just one.
                if is_mass {
                    Some(TargetedImperativeAst::ReturnAllToZone {
                        target,
                        origin,
                        destination: Zone::Battlefield,
                        enters_under: d.enters_under,
                        enter_tapped: d.enter_tapped,
                        enter_with_counters: d.enter_with_counters,
                    })
                } else {
                    Some(TargetedImperativeAst::ReturnToBattlefield {
                        target,
                        origin,
                        enter_transformed: d.transformed,
                        enters_under: d.enters_under,
                        enter_tapped: d.enter_tapped,
                        enters_attacking: d.enters_attacking,
                        enter_with_counters: d.enter_with_counters,
                        face_down: d.face_down,
                    })
                }
            }
            Some(d) if d.zone == Zone::Hand => {
                // Mass return only when the source zone is implicit (battlefield):
                // "return all <filter> from your graveyard to your hand" must
                // remain `ChangeZone { origin: Graveyard, destination: Hand }`,
                // not `BounceAll` (whose resolver only scans the battlefield).
                if is_mass && origin.is_none() {
                    Some(TargetedImperativeAst::ReturnAll { target, count })
                } else if is_mass {
                    Some(TargetedImperativeAst::ReturnAllToZone {
                        target,
                        origin,
                        destination: Zone::Hand,
                        enters_under: None,
                        enter_tapped: false,
                        enter_with_counters: vec![],
                    })
                } else if origin.is_some() {
                    Some(TargetedImperativeAst::ReturnToZone {
                        target,
                        origin,
                        destination: Zone::Hand,
                        up_to: return_up_to,
                        multi_target: return_multi_target,
                    })
                } else {
                    Some(TargetedImperativeAst::Return {
                        target,
                        selection,
                        multi_target: return_multi_target,
                    })
                }
            }
            Some(d) => {
                if is_mass {
                    Some(TargetedImperativeAst::ReturnAllToZone {
                        target,
                        origin,
                        destination: d.zone,
                        enters_under: None,
                        enter_tapped: false,
                        enter_with_counters: vec![],
                    })
                } else {
                    Some(TargetedImperativeAst::ReturnToZone {
                        target,
                        origin,
                        destination: d.zone,
                        up_to: return_up_to,
                        multi_target: return_multi_target,
                    })
                }
            }
            // No explicit destination phrase. "Return all <filter>" with no
            // "to ..." tail still defaults to owner's hand for the
            // mass-bounce class. Single-target "return <X>" likewise defaults
            // to hand — preserves the pre-existing behavior.
            None => {
                if is_mass {
                    Some(TargetedImperativeAst::ReturnAll { target, count })
                } else {
                    Some(TargetedImperativeAst::Return {
                        target,
                        selection,
                        multi_target: return_multi_target,
                    })
                }
            }
        };
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("fight ")).parse(input))
    {
        // CR 115.6: "fights up to one target creature …" allows zero targets.
        // Preserve the optional-target spec through the AST; it is stamped onto
        // the clause in `lower_imperative_family_ast`.
        let (target_text, multi_target) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_fight_target(target_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::Fight {
            target,
            multi_target,
        });
    }
    // CR 723.1: "You control target player during that player's next turn"
    // (Mindslaver), or "you control target opponent during their next turn"
    // (Construct a Cosmic Cube). Declarative form — "you" is not stripped as an
    // imperative subject because this isn't a verb-on-controller pattern. Must
    // match before the "gain control of" branch below since the prefixes differ.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("you control ")).parse(input)
    }) {
        if let Some((target, grant_extra_turn_after, window)) =
            try_parse_control_next_turn_suffix(text, rest)
        {
            return Some(TargetedImperativeAst::ControlNextTurn {
                target,
                grant_extra_turn_after,
                window,
            });
        }
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("gain control of ")).parse(input)
    }) {
        // Check for ControlNextTurn suffix first (rare phrasing combining both
        // forms) before falling back to the standard GainControl effect.
        if let Some((target, grant_extra_turn_after, window)) =
            try_parse_control_next_turn_suffix(text, rest)
        {
            return Some(TargetedImperativeAst::ControlNextTurn {
                target,
                grant_extra_turn_after,
                window,
            });
        }
        // CR 613.1b: "gain control of all/each <filter>" is the untargeted mass
        // form (Hellkite Tyrant) — mirrors "destroy all". Detect the mass
        // pluralizer; `parse_target_with_ctx` still consumes the "all "/"each "
        // prefix into the population filter, so only the flag is threaded.
        let all = nom_on_lower(text, lower, |input| {
            value(
                (),
                alt((tag("gain control of all "), tag("gain control of each "))),
            )
            .parse(input)
        })
        .is_some();
        let (target_text, _) = super::strip_optional_target_prefix(rest);
        let (target, _rem) = parse_target_with_ctx(target_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(TargetedImperativeAst::GainControl { target, all });
    }
    // Earthbend: "[you ]earthbend [N] [target <type>]". The optional "you "
    // subject (Fatal Fissure's delayed trigger "you earthbend 4") names the
    // controller performing the keyword action; earthbend is a player action, so
    // the subject carries no extra targeting and is simply consumed.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), preceded(opt(tag("you ")), tag("earthbend "))).parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        let (target, power, toughness) = parse_earthbend_params(text, rest_lower);
        return Some(TargetedImperativeAst::Earthbend {
            target,
            power,
            toughness,
        });
    }
    // Airbend: "airbend target <type> <mana_cost>" → GrantCastingPermission(ExileWithAltCost)
    if let Some((_, original_rest)) =
        nom_on_lower(text, lower, |input| value((), tag("airbend ")).parse(input))
    {
        let (target_text, _) = super::strip_optional_target_prefix(original_rest);
        let (target, after_target) = parse_target_with_ctx(target_text, ctx);
        let cost = parse_mana_symbols(after_target.trim_start())
            .map(|(c, _)| c)
            .unwrap_or(crate::types::mana::ManaCost::Cost {
                generic: 2,
                shards: vec![],
            });
        return Some(TargetedImperativeAst::Airbend { target, cost });
    }
    None
}

pub(super) fn lower_targeted_action_ast(ast: TargetedImperativeAst) -> Effect {
    match ast {
        // CR 701.26a/b: map the parser AST tap/untap variants onto the
        // parameterized `Effect::SetTapState` (scope = Single/All, state = Tap/Untap).
        // CR 701.26a: The variable target count (`multi_target`) is carried on the
        // CLAUSE, not the Effect, so it is threaded in `lower_imperative_family_ast`
        // (the bare-Effect path here cannot hold it).
        TargetedImperativeAst::Tap { target, .. } => Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
        TargetedImperativeAst::Untap { target, .. } => Effect::SetTapState {
            target,
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        },
        TargetedImperativeAst::TapAll { target } => Effect::SetTapState {
            target,
            scope: EffectScope::All,
            state: TapStateChange::Tap,
        },
        TargetedImperativeAst::UntapAll { target } => Effect::SetTapState {
            target,
            scope: EffectScope::All,
            state: TapStateChange::Untap,
        },
        TargetedImperativeAst::Goad { target } => Effect::Goad { target },
        TargetedImperativeAst::GoadAll { target } => Effect::GoadAll { target },
        // CR 709.5f-g: lock/unlock a door of the targeted Room.
        TargetedImperativeAst::SetRoomDoorLock { op, target } => {
            Effect::SetRoomDoorLock { op, target }
        }
        TargetedImperativeAst::Sacrifice {
            target,
            count,
            min_count,
        } => Effect::Sacrifice {
            target,
            count,
            min_count,
        },
        // CR 701.9b + CR 608.2d: Lower the AST `up_to: bool` into the typed
        // `count: QuantityExpr::UpTo { max }` wrapper.
        TargetedImperativeAst::Discard {
            count,
            random,
            up_to,
            unless_filter,
            filter,
        } => Effect::Discard {
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            // CR 701.9a: "Discard" with no subject defaults to the controller.
            // Subject injection overrides this for "target player discards" patterns.
            target: TargetFilter::Controller,
            selection: if random {
                crate::types::ability::CardSelectionMode::Random
            } else {
                crate::types::ability::CardSelectionMode::Chosen
            },
            unless_filter,
            filter,
        },
        // CR 701.9a: Back-reference discard — "discard that card" / "discard those
        // cards" — discards specific cards via ParentTarget binding. Count is
        // implicit (1 per parent-affected ID; the runtime expands ParentTarget
        // into the full set at rebind time).
        TargetedImperativeAst::DiscardCard { target } => Effect::DiscardCard { count: 1, target },
        // CR 115.1d + CR 601.2c: the "up to N" cardinality is an ability-level
        // field (`ParsedEffectClause.multi_target`), not an `Effect::Bounce`
        // field. It is recovered at the clause layer in
        // `lower_imperative_family_ast`; this bare-Effect lowering
        // deliberately ignores `multi_target`.
        TargetedImperativeAst::Return {
            target,
            selection,
            multi_target: _,
        } => Effect::Bounce {
            target,
            destination: None,
            selection,
        },
        // CR 400.7 + CR 611.2c: "Return all/each [filter]" mass-bounce — the
        // resolver iterates every matching permanent. Class filter is preserved
        // as-is; single-object refs (SelfRef / TriggeringSource / AttachedTo /
        // ParentTarget) cannot reach this AST variant because the bare
        // `tag("return ")` arm above handles those.
        TargetedImperativeAst::ReturnAll { target, count } => Effect::BounceAll {
            target,
            destination: None,
            count,
        },
        // CR 400.7: Return to battlefield is a zone change, not a bounce.
        TargetedImperativeAst::ReturnToBattlefield {
            target,
            origin,
            enter_transformed,
            enters_under,
            enter_tapped,
            enters_attacking,
            enter_with_counters,
            face_down,
        } => Effect::ChangeZone {
            origin,
            destination: Zone::Battlefield,
            target,
            owner_library: false,
            enter_transformed,
            enters_under,
            enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
            enters_attacking,
            up_to: false,
            enter_with_counters,
            conditional_enter_with_counters: vec![],
            // CR 708.2a + CR 708.3: a "face down" return seeds the default
            // vanilla-2/2 face-down profile; a trailing "It's a <type>" sentence
            // (Yedora's "It's a Forest land.") refines it via FaceDownProfileSpec.
            face_down_profile: face_down.then(crate::types::ability::FaceDownProfile::vanilla_2_2),
            enters_modified_if: None,
        },
        // CR 400.6: Return to a non-hand, non-battlefield zone (graveyard, library).
        // CR 115.1d + CR 601.2c: `multi_target` (the bounded "up to N" count)
        // is an ability-level field (`ParsedEffectClause.multi_target`), not
        // an `Effect::ChangeZone` field — recovered at the clause layer in
        // `lower_imperative_family_ast`; this bare-Effect lowering
        // deliberately ignores it.
        TargetedImperativeAst::ReturnToZone {
            target,
            origin,
            destination,
            up_to,
            multi_target: _,
        } => Effect::ChangeZone {
            origin,
            destination,
            target,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        TargetedImperativeAst::ReturnAllToZone {
            target,
            origin,
            destination,
            enters_under,
            enter_tapped,
            enter_with_counters,
        } => {
            let origin = if matches!(target, TargetFilter::ExiledBySource) {
                Some(Zone::Exile)
            } else {
                origin
            };
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
                // CR 122.1 + CR 122.1h: each returned object enters with these
                // counters (e.g. a finality counter on Shilgengar's mass return).
                enter_with_counters,
                face_down_profile: None,
                library_position: None,
                random_order: false,
            }
        }
        // CR 115.6: the "up to N" target cardinality is an ability-level field
        // (`ParsedEffectClause.multi_target`), not an `Effect::Fight` field. It is
        // recovered at the clause layer in `lower_imperative_family_ast`; this
        // bare-Effect lowering deliberately ignores `multi_target`.
        TargetedImperativeAst::Fight {
            target,
            multi_target: _,
        } => Effect::Fight {
            target,
            subject: TargetFilter::SelfRef,
        },
        TargetedImperativeAst::GainControl { target, all } => {
            if all {
                Effect::GainControlAll { target }
            } else {
                Effect::GainControl { target }
            }
        }
        TargetedImperativeAst::ControlNextTurn {
            target,
            grant_extra_turn_after,
            window,
        } => Effect::ControlNextTurn {
            target,
            grant_extra_turn_after,
            window,
        },
        TargetedImperativeAst::Earthbend {
            target,
            power,
            toughness,
        } => Effect::Animate {
            power: Some(PtValue::Fixed(power)),
            toughness: Some(PtValue::Fixed(toughness)),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target,
            keywords: vec![crate::types::keywords::Keyword::Haste],
        },
        TargetedImperativeAst::Airbend { target, cost } => Effect::GrantCastingPermission {
            permission: crate::types::ability::CastingPermission::ExileWithAltCost {
                cost,
                cast_transformed: false,
                constraint: None,
                // CR 611.2a: airbend grants cast permission to each exiled
                // object's owner, not the airbender's controller.
                granted_to: None,
                resolution_cleanup: None,
                duration: None,
                graveyard_replacement: None,
                enters_with_counter: None,
                enters_with_modifications: Vec::new(),
                mana_spend_permission: None,
            },
            target,
            grantee: crate::types::ability::PermissionGrantee::ObjectOwner,
        },
        TargetedImperativeAst::ZoneCounterProxy(ast) => lower_zone_counter_ast(*ast),
    }
}

/// CR 400.7 + CR 701.23 + CR 701.24: Recognize the multi-zone same-name exile
/// pattern used by Deadly Cover-Up and the Lost Legacy class.
///
/// The matched grammar is, in BNF-like form:
///
/// ```text
/// "search " <possessive>
///     ("graveyard, hand, and library" | <permutation>)
///     " for " "all cards"
///     " with that name and exile them"
/// ```
///
/// The `quantifier` axis distinguishes the mandatory "all cards" mass exile
/// (`Effect::ChangeZoneAll`) from the interactive "any number of cards" / "up to
/// N cards" forms (`Effect::SearchLibrary`; CR 701.23b — the searcher may fail
/// to find), so both classes share this one recognizer's grammar and correct
/// same-name semantics (The End, Deadly Cover-Up, Crumble to Dust, Surgical
/// Extraction, Test of Talents, Deicide).
///
/// R1 object-relative guard (CR 108.3 / CR 109.4): the interactive quantifier
/// arms match ONLY when `owner` is object-relative (`ParentTargetOwner` /
/// `ParentTargetController`), because this recognizer hard-codes
/// `SameNameAsParentTarget`. Player-possessive "any number of cards with that
/// name" spells (Lost Legacy, Unmoored Ego, Memoricide, …) name their card via
/// a "Choose a card name" clause (`HasChosenName`), NOT the parent target — they
/// must decline here and route through the general `HasChosenName` search path.
/// The `All` arm keeps accepting every possessive (unchanged).
///
/// Returns `Some((owner, quantifier))` on match. Returns `None` for any other
/// shape so the regular library-search branch can run.
pub(super) fn try_parse_multi_zone_same_name_exile(
    lower: &str,
) -> Option<(ControllerRef, MultiZoneExileQuantifier)> {
    fn run(
        input: &str,
    ) -> Result<(&str, (ControllerRef, MultiZoneExileQuantifier)), nom::Err<OracleError<'_>>> {
        // search <possessive> graveyard, hand, and library
        let (input, _) = tag::<_, _, OracleError<'_>>("search ").parse(input)?;
        let (input, owner) = alt((
            value(
                ControllerRef::ParentTargetOwner,
                tag::<_, _, OracleError<'_>>("its owner's "),
            ),
            value(
                ControllerRef::ParentTargetController,
                tag("its controller's "),
            ),
            value(ControllerRef::TargetPlayer, tag("their ")),
            value(ControllerRef::TargetPlayer, tag("that player's ")),
            value(ControllerRef::TargetPlayer, tag("target player's ")),
            value(ControllerRef::Opponent, tag("target opponent's ")),
            value(ControllerRef::Opponent, tag("an opponent's ")),
            value(ControllerRef::You, tag("your ")),
        ))
        .parse(input)?;
        let (input, _) = alt((
            tag::<_, _, OracleError<'_>>("graveyard, hand, and library"),
            tag("graveyard, hand and library"),
            tag("graveyard, library, and hand"),
            tag("hand, graveyard, and library"),
            tag("library, graveyard, and hand"),
        ))
        .parse(input)?;
        // for <quantifier> cards with that name and exile them
        let (input, _) = tag::<_, _, OracleError<'_>>(" for ").parse(input)?;
        // CR 107.1c + CR 701.23b: one composed `alt` axis over the quantifier —
        // "all cards" (mandatory), "any number of cards" (interactive), or
        // "up to N cards" (interactive, capped). `parse_number_or_x` reuses the
        // shared numeric primitive (X resolves to 0 for the count cap).
        let (input, quantifier) = alt((
            value(MultiZoneExileQuantifier::All, tag("all cards")),
            value(
                MultiZoneExileQuantifier::AnyNumber,
                tag("any number of cards"),
            ),
            map(
                preceded(
                    tag("up to "),
                    terminated(nom_primitives::parse_number_or_x, tag(" cards")),
                ),
                MultiZoneExileQuantifier::UpTo,
            ),
        ))
        .parse(input)?;
        // R1 (CR 108.3 / CR 109.4): the interactive arms only claim
        // object-relative possessives. A typed `match` on the already-parsed
        // `owner` — nom-compliant (no string dispatch). Player-possessive "any
        // number"/"up to N" decline here → general HasChosenName search path.
        match (quantifier, &owner) {
            (MultiZoneExileQuantifier::All, _) => {}
            (_, ControllerRef::ParentTargetOwner | ControllerRef::ParentTargetController) => {}
            _ => {
                return Err(nom::Err::Error(OracleError::from_error_kind(
                    input,
                    nom::error::ErrorKind::Verify,
                )))
            }
        }
        // Match the trailing same-name suffix. The name source is either a
        // previously-named card ("with that name", Lost Legacy / Deadly Cover-Up)
        // or the spell's exiled/countered target referenced by its card type
        // ("with the same name as that card/creature/land/spell/…", Surgical
        // Extraction / Eradicate / Crumble to Dust / Counterbore / Deicide). The
        // card-type is composed as one `alt` axis rather than enumerated as full
        // strings; all forms lower to the same `SameNameAsParentTarget` effect.
        let (input, _) = alt((
            value(
                (),
                tag::<_, _, OracleError<'_>>(" with that name and exile them"),
            ),
            value(
                (),
                (
                    tag::<_, _, OracleError<'_>>(" with the same name as that "),
                    alt((
                        tag("creature"),
                        tag("permanent"),
                        tag("planeswalker"),
                        tag("artifact"),
                        tag("enchantment"),
                        tag("land"),
                        tag("spell"),
                        tag("card"),
                    )),
                    tag(" and exile them"),
                ),
            ),
        ))
        .parse(input)?;
        Ok((input, (owner, quantifier)))
    }
    run(lower).ok().map(|(_, result)| result)
}

/// Parse output of the multi-zone player-exile recognizer: remaining input paired
/// with the owner axis and the origin-zone union. Named so the inner `nom`
/// combinator signature stays under `clippy::type_complexity`.
type MultiZonePlayerExileParse<'a> = (&'a str, (ControllerRef, Vec<Zone>));

/// CR 400.3 + CR 404.1 + CR 406.2 + CR 108.2: "exile all cards from `<possessive>` `<zone>` and
/// `<zone>`" — mass exile of every card a player owns across a *union* of zones
/// (Identity Crisis: "target player's hand and graveyard"). Mirrors the
/// multi-zone origin handling of [`try_parse_multi_zone_same_name_exile`]: the
/// zone union is encoded on the target filter via `InAnyZone`, and the
/// `ChangeZoneAll` resolver reads the multi-zone origin from the filter (so the
/// lowering passes `origin: None`).
///
/// Returns the owner axis and the origin zones (always `>= 2`). Declines
/// (`None`) on a single zone so the generic single-origin `exile all` path keeps
/// handling those, and on any trailing fragment so nothing is silently dropped.
/// The leading noun is fixed to "cards"/"card" (CR 108.2 — every card, any
/// type); a type-qualified variant ("all creature cards from …") is not claimed.
pub(super) fn try_parse_multi_zone_player_exile(
    rest_lower: &str,
) -> Option<(ControllerRef, Vec<Zone>)> {
    fn zone_word(input: &str) -> Result<(&str, Zone), nom::Err<OracleError<'_>>> {
        alt((
            value(Zone::Graveyard, tag::<_, _, OracleError<'_>>("graveyard")),
            value(Zone::Hand, tag("hand")),
            value(Zone::Library, tag("library")),
        ))
        .parse(input)
    }
    fn run(input: &str) -> Result<MultiZonePlayerExileParse<'_>, nom::Err<OracleError<'_>>> {
        let (input, _) = alt((
            tag::<_, _, OracleError<'_>>("cards from "),
            tag("card from "),
        ))
        .parse(input)?;
        let (input, owner) = alt((
            value(
                ControllerRef::ParentTargetOwner,
                tag::<_, _, OracleError<'_>>("its owner's "),
            ),
            value(
                ControllerRef::ParentTargetController,
                tag("its controller's "),
            ),
            value(ControllerRef::TargetPlayer, tag("their ")),
            value(ControllerRef::TargetPlayer, tag("that player's ")),
            value(ControllerRef::TargetPlayer, tag("target player's ")),
            value(ControllerRef::Opponent, tag("target opponent's ")),
            value(ControllerRef::Opponent, tag("an opponent's ")),
            value(ControllerRef::You, tag("your ")),
        ))
        .parse(input)?;
        let (mut input, first) = zone_word(input)?;
        let mut zones = vec![first];
        // Additional zones joined by " and " / ", and " / ", " (oxford comma).
        loop {
            let Ok((after_sep, _)) = alt((
                tag::<_, _, OracleError<'_>>(" and "),
                tag(", and "),
                tag(", "),
            ))
            .parse(input) else {
                break;
            };
            let Ok((after_zone, zone)) = zone_word(after_sep) else {
                break;
            };
            if !zones.contains(&zone) {
                zones.push(zone);
            }
            input = after_zone;
        }
        Ok((input, (owner, zones)))
    }
    let (rem, (owner, zones)) = run(rest_lower).ok()?;
    if zones.len() < 2 {
        return None;
    }
    // The clause must be fully consumed (only sentence punctuation may remain),
    // so no trailing fragment is orphaned into an unsupported child node.
    let tail = rem.trim_start().trim_start_matches('.').trim(); // allow-noncombinator: punctuation cleanup after typed terminator
    if !tail.is_empty() {
        return None;
    }
    Some((owner, zones))
}

pub(super) fn parse_search_and_creation_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<SearchCreationImperativeAst> {
    if let Some(ast) = parse_search_outside_game_ast(lower, ctx) {
        return Some(ast);
    }
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("seek ")).parse(input)) {
        let details = super::parse_seek_details(lower, ctx);
        return Some(SearchCreationImperativeAst::Seek {
            filter: details.filter,
            count: details.count,
            from_top: details.from_top,
            destination: details.destination,
            enter_tapped: details.enter_tapped,
            extra_filters: details.extra_filters,
        });
    }
    // CR 400.7 + CR 701.23 + CR 701.24: "search [possessive] graveyard, hand,
    // and library for [filter] and exile them" — multi-zone exile of every card
    // matching the filter. Recognized before the single-zone library search
    // because both patterns share the "search " prefix; multi-zone wins on match.
    if let Some((owner, quantifier)) = try_parse_multi_zone_same_name_exile(lower) {
        return Some(SearchCreationImperativeAst::MultiZoneSameNameExile { owner, quantifier });
    }
    if starts_with_possessive(lower, "search", "library")
        // CR 701.23a: God-Pharaoh's-Gift-class multi-zone tutors ("search your
        // graveyard, hand, and/or library for ...") — the word after the
        // possessive is a non-library zone, so `starts_with_possessive` misses
        // them; the zone-list detector routes them through the same lowering.
        || super::parse_multi_search_zones(lower).is_some()
        || nom_on_lower(lower, lower, |i| {
            alt((
                value((), tag("search target opponent's library")),
                value((), tag("search target player's library")),
                value((), tag("search an opponent's library")),
            ))
            .parse(i)
        })
        .is_some()
    {
        let details = super::parse_search_library_details(lower, ctx);
        return Some(SearchCreationImperativeAst::SearchLibrary {
            filter: details.filter,
            count: details.count,
            reveal: details.reveal,
            target_player: details.target_player,
            up_to: details.up_to,
            selection_constraint: details.selection_constraint,
            reference_target: details.reference_target,
            extra_filters: details.extra_filters,
            multi_destination: details.multi_destination,
            multi_enter_tapped: details.multi_enter_tapped,
            split: details.split,
            source_zones: details.source_zones,
        });
    }
    // CR 701.20e + CR 701.20a: "look at the top N" (private) and "reveal the top N" (public)
    // both produce Dig — the reveal flag distinguishes visibility semantics.
    if let Some((reveal, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value(false, tag("look at the top ")),
            value(false, tag("looks at the top ")),
            value(true, tag("reveal the top ")),
            value(true, tag("reveals the top ")),
        ))
        .parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        // Try numeric count first ("three cards"), then "x" as a variable
        // resolved later by apply_where_x_effect_expression.
        let count = if let Ok((_, n)) = nom_primitives::parse_number.parse(rest_lower) {
            QuantityExpr::Fixed { value: n as i32 }
        } else if tag::<_, _, OracleError<'_>>("x").parse(rest_lower).is_ok() {
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }
        } else {
            QuantityExpr::Fixed { value: 1 }
        };
        let player = parse_dig_library_owner(rest_lower);
        // CR 701.20e + CR 701.13a + CR 406.3: "look at the top card ... and
        // exiles it face down" (Gonti, Night Minister) — fuse into ExileTop so
        // the card leaves the library and the trailing play grant can bind to
        // the tracked set.
        if preceded(
            take_until::<_, _, OracleError<'_>>("and exiles it face down"),
            tag::<_, _, OracleError<'_>>("and exiles it face down"),
        )
        .parse(rest_lower)
        .is_ok()
        {
            return Some(SearchCreationImperativeAst::ExileTopLookedAt {
                player,
                count,
                face_down: true,
            });
        }
        return Some(SearchCreationImperativeAst::Dig {
            count,
            reveal,
            player,
        });
    }
    // CR 701.20a + CR 701.20e: "look at/reveal <count> cards from the top of
    // <owner>'s library" — the count-leading word order as opposed to the
    // "look at the top N cards" form handled above. `reveal` distinguishes the
    // public reveal (CR 701.20a) from the private look (CR 701.20e).
    //
    // Coverage-honesty guard, split by mode:
    //   * private look (CR 701.20e): `Effect::Dig.count` is a `QuantityExpr`
    //     resolved at runtime and dynamic keep counts now flow through
    //     `Dig.keep_count_expr`, so ANY count expression is accepted (Stargaze's
    //     "look at twice X cards ... put X cards from among them ...");
    //   * reveal (CR 701.20a) is demoted in `lower.rs` to
    //     `Effect::RevealTop { count: u32 }`, which has no dynamic-count path and
    //     collapses every non-`Fixed` count to 1 — so a variable-count *reveal*
    //     stays Fixed-only and otherwise falls through to remain honestly
    //     Unimplemented until RevealTop gains a `QuantityExpr` count.
    if let Some((reveal, remainder)) = nom_on_lower(text, lower, |input| {
        alt((
            value(false, tag("look at ")),
            value(false, tag("looks at ")),
            value(true, tag("reveal ")),
            value(true, tag("reveals ")),
        ))
        .parse(input)
    }) {
        // `parse_count_expr` lowercases internally, but its returned remainder
        // mirrors the case of its input. The trailing "cards from the top of "
        // tags below match lowercase, so feed the lowercase slice (not the
        // original-case `remainder`) to keep the remainder lowercase too.
        let rest_lower = &lower[lower.len() - remainder.len()..];
        if let Some((count, after_count)) = parse_count_expr(rest_lower) {
            // CR 701.20e: `Effect::Dig.count` is runtime-resolved, so a private
            // "look at ..." accepts ANY count expression (e.g. Stargaze's
            // "twice X"). A reveal-form stays Fixed-only: an unpatched reveal-Dig
            // demotes to `Effect::RevealTop` (lower.rs), which has no dynamic-count
            // path, so a variable-count *reveal* would resolve wrong — keep it
            // honestly Unimplemented until RevealTop gains a QuantityExpr count.
            let count_ok = !reveal || matches!(count, QuantityExpr::Fixed { .. });
            let after_count = after_count.trim_start();
            if count_ok {
                if let Ok((owner_lower, _)) = alt((
                    tag::<_, _, OracleError<'_>>("cards from the top of "),
                    tag::<_, _, OracleError<'_>>("card from the top of "),
                ))
                .parse(after_count)
                {
                    let player = parse_dig_library_owner(owner_lower);
                    return Some(SearchCreationImperativeAst::Dig {
                        count,
                        reveal,
                        player,
                    });
                }
            }
        }
    }
    // CR 701.20e: "look at that many cards from the top of your library" — variable-count dig
    // where "that many" references the result of a previous effect (e.g., damage dealt).
    if let Some((reveal, _)) = nom_on_lower(text, lower, |input| {
        alt((
            value(
                false,
                tag("look at that many cards from the top of your library"),
            ),
            value(
                true,
                tag("reveal that many cards from the top of your library"),
            ),
        ))
        .parse(input)
    }) {
        let count = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        return Some(SearchCreationImperativeAst::Dig {
            count,
            reveal,
            player: TargetFilter::Controller,
        });
    }
    if let Some((_, _)) = nom_on_lower(text, lower, |input| value((), tag("create ")).parse(input))
    {
        return match try_parse_token(lower, text, ctx) {
            // `owner` is absorbed by `..`: this search/creation path always
            // yields the `TargetFilter::Controller` default, and the lowering
            // below re-emits it. A "target [player] creates" subject is lifted
            // by `inject_subject_target` after lowering.
            Some(Effect::CopyTokenOf {
                target,
                source_filter,
                enters_attacking,
                tapped,
                count,
                extra_keywords,
                additional_modifications,
                ..
            }) => Some(SearchCreationImperativeAst::CopyTokenOf {
                target,
                count,
                source_filter,
                enters_attacking,
                tapped,
                extra_keywords,
                additional_modifications,
            }),
            Some(Effect::Token {
                name,
                power,
                toughness,
                types,
                supertypes,
                colors,
                keywords,
                tapped,
                count,
                attach_to,
                static_abilities,
                enters_attacking,
                ..
            }) => Some(SearchCreationImperativeAst::Token {
                token: Box::new(TokenDescription {
                    name,
                    power: Some(power),
                    toughness: Some(toughness),
                    types,
                    supertypes,
                    colors,
                    keywords,
                    tapped,
                    count,
                    attach_to,
                    static_abilities,
                    enters_attacking,
                }),
            }),
            _ => None,
        };
    }
    None
}

fn parse_search_outside_game_ast(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<SearchCreationImperativeAst> {
    // CR 400.11 + CR 400.11a + CR 701.23j: The outside-the-game pool (a player's
    // sideboard / wishboard) is selected by any of three surface verbs:
    //   - "reveal a … card you own from outside the game …" — Burning Wish,
    //     Cunning Wish, Living Wish (reveal to hand).
    //   - "play a … card you own from outside the game this turn" — Wish (M19),
    //     which grants the controller permission to play the chosen card.
    //   - "cast a … card you own from outside the game …" — same play class for
    //     spells specifically.
    // All three pull from the same pool (CR 400.11b brings the chosen card into
    // the game); the verb only changes whether the card is revealed and whether
    // a "this turn" play window is attached. Routing the play/cast forms here —
    // rather than letting them fall to `try_parse_cast_effect` — is what keeps
    // Wish from misparsing into a `CastFromZone` that targets an in-game
    // permanent the controller already owns (issue #1976).
    //
    // CR 406.3 + CR 400.11: Two source pools may appear under a single
    // "reveal … or choose a face-up … card you own in exile" disjunction
    // (Karn, the Great Creator; Coax from the Blind Eternities). The
    // controller picks one card from the union of (a) the owned outside-the-
    // game collection and (b) face-up exile cards they own matching the
    // filter. The destination clause may be inline (" and put it into your
    // hand") or a sibling sentence handled by the chain splitter.
    // (filter_text, destination, source_pool, reveal) — extracted into a
    // type alias so the parser's return type stays under the
    // clippy::type_complexity threshold.
    type OutsideGameParseFields<'a> = (&'a str, Zone, OutsideGameSourcePool, bool);
    fn parse_clause<'a>(
        input: &'a str,
    ) -> Result<(&'a str, OutsideGameParseFields<'a>), nom::Err<OracleError<'a>>> {
        // CR 701.20 / CR 701.23j vs CR 305.1 + CR 601.2a: The verb determines
        // whether the chosen card is revealed. "reveal" makes it public;
        // "play"/"cast" bring it into the game to be played without revealing.
        let (rest, reveal) = alt((
            value(true, tag("reveal ")),
            value(false, tag("play ")),
            value(false, tag("cast ")),
        ))
        .parse(input)?;
        // Article: "a "/"an " — varies by filter head noun (artifact → an).
        let (rest, _) = alt((tag("a "), tag("an "))).parse(rest)?;
        // Outside-the-game branch is mandatory and yields filter_text. Use a
        // leading-space-free anchor so the bare "a card …" form (Wish, no type
        // adjective) captures an empty filter region that lowers to
        // `TargetFilter::Any`; typed forms ("sorcery card …") capture the
        // adjective with a trailing space that the filter parser trims.
        let (rest, filter_text) = take_until("card you own from outside the game").parse(rest)?;
        let (rest, _) = tag("card you own from outside the game").parse(rest)?;
        // Optional face-up exile disjunction. Re-uses the same filter phrase
        // (Karn and Coax both repeat the filter literally in both branches);
        // we discard the second filter_text since the outside-game one is
        // canonical for the unified pool's filter.
        let (rest, face_up_exile_branch) = opt(parse_face_up_exile_branch).parse(rest)?;
        let source_pool = if face_up_exile_branch.is_some() {
            OutsideGameSourcePool::SideboardAndFaceUpExile
        } else {
            OutsideGameSourcePool::Sideboard
        };
        // Optional inline destination clause. When absent, the destination
        // arrives as a follow-up "Put that card into your hand." chunk
        // routed through the chain splitter into a ChangeZone sub-ability.
        let (rest, destination) = opt(alt((
            value(Zone::Hand, tag(" and put it into your hand")),
            value(Zone::Hand, tag(" and put that card into your hand")),
        )))
        .parse(rest)?;
        // CR 611.2a: The play/cast forms carry a trailing "this turn" window.
        // The card is brought to hand (CR 400.11b) where it can be played; the
        // window is consumed here so the clause parses to EOF cleanly rather
        // than leaving an unparsed tail that would re-route the whole line.
        let (rest, _) = opt(tag(" this turn")).parse(rest)?;
        let (rest, _) = opt(tag(".")).parse(rest)?;
        let (rest, _) = eof.parse(rest)?;
        Ok((
            rest,
            (
                filter_text,
                destination.unwrap_or(Zone::Hand),
                source_pool,
                reveal,
            ),
        ))
    }

    // CR 406.3: A "face-up ... card you own in exile" branch refers to an
    // in-game exile-zone card that is visible by default unless an effect
    // exiled it face down.
    // " or choose a face-up <filter> card you own in exile". English
    // grammar always pairs "a face-up" (the head noun begins with the
    // consonant /f/), so no "an face-up" variant exists in MTGJSON.
    fn parse_face_up_exile_branch(input: &str) -> Result<(&str, ()), nom::Err<OracleError<'_>>> {
        let (rest, _) = tag(" or choose a face-up ").parse(input)?;
        let (rest, _filter_text) = take_until(" card you own in exile").parse(rest)?;
        let (rest, _) = tag(" card you own in exile").parse(rest)?;
        Ok((rest, ()))
    }

    let (_, (filter_text, destination, source_pool, reveal)) = parse_clause(lower).ok()?;
    let filter = super::search::parse_search_filter(filter_text, ctx);
    Some(SearchCreationImperativeAst::SearchOutsideGame {
        filter,
        count: QuantityExpr::Fixed { value: 1 },
        reveal,
        destination,
        up_to: true,
        source_pool,
    })
}

/// CR 400.11 + CR 400.11a + CR 701.23j: Route "play/cast a … card you own from
/// outside the game …" (Wish, M19) to the outside-game pool selector, lowering
/// the AST to `Effect::SearchOutsideGame`. The verb-keyed imperative dispatcher
/// only sends "reveal"/"search" lines through `parse_search_and_creation_ast`,
/// so the "play"/"cast" surface forms must be probed explicitly before the
/// generic `try_parse_cast_effect` parser — otherwise Wish misparses into a
/// `CastFromZone` that targets an in-game permanent (issue #1976).
pub(super) fn try_parse_play_from_outside_game(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<Effect> {
    let ast = parse_search_outside_game_ast(lower, ctx)?;
    Some(lower_search_and_creation_ast(ast))
}

pub(super) fn lower_search_and_creation_ast(ast: SearchCreationImperativeAst) -> Effect {
    match ast {
        SearchCreationImperativeAst::SearchLibrary {
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            reference_target: _,
            // Extras are consumed in `lower_imperative_family_ast` via
            // `lower_multi_filter_search_library`, which builds a chained
            // `ParsedEffectClause`. At this bare-Effect lowering site, multiple
            // filters collapse to the primary — but that path is unreachable
            // for multi-filter searches because the family-level lowering
            // intercepts them first.
            extra_filters: _,
            multi_destination: _,
            multi_enter_tapped: _,
            split,
            source_zones,
        } => Effect::SearchLibrary {
            filter,
            // CR 107.1c + CR 701.23d: Lower the AST `up_to: bool` into the
            // typed `count: QuantityExpr::UpTo { max }` wrapper.
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            reveal,
            target_player,
            selection_constraint,
            split,
            source_zones,
        },
        SearchCreationImperativeAst::SearchOutsideGame {
            filter,
            count,
            reveal,
            destination,
            up_to,
            source_pool,
        } => Effect::SearchOutsideGame {
            filter,
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            reveal,
            destination,
            source_pool,
        },
        SearchCreationImperativeAst::Dig {
            count,
            reveal,
            player,
        } => Effect::Dig {
            player,
            count,
            destination: None,
            keep_count: None,
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            rest_destination: None,
            reveal,
            enter_tapped: false,
            source: DigSource::Library,
        },
        SearchCreationImperativeAst::ExileTopLookedAt {
            player,
            count,
            face_down,
        } => Effect::ExileTop {
            player,
            count,
            face_down,
        },
        SearchCreationImperativeAst::CopyTokenOf {
            target,
            count,
            source_filter,
            enters_attacking,
            tapped,
            extra_keywords,
            additional_modifications,
        } => Effect::CopyTokenOf {
            target,
            owner: TargetFilter::Controller,
            source_filter,
            enters_attacking,
            tapped,
            count,
            extra_keywords,
            additional_modifications,
        },
        SearchCreationImperativeAst::Token { token } => Effect::Token {
            name: token.name,
            power: token.power.unwrap_or(PtValue::Fixed(0)),
            toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
            types: token.types,
            colors: token.colors,
            keywords: token.keywords,
            tapped: token.tapped,
            count: token.count,
            owner: TargetFilter::Controller,
            attach_to: token.attach_to,
            enters_attacking: token.enters_attacking,
            // CR 205.4a: Preserve parsed token supertypes (legendary/snow).
            supertypes: token.supertypes,
            static_abilities: token.static_abilities,
            enter_with_counters: vec![],
        },
        SearchCreationImperativeAst::Seek {
            filter,
            count,
            from_top,
            destination,
            enter_tapped,
            extra_filters: _,
        } => Effect::Seek {
            filter,
            count,
            from_top,
            destination,
            enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
        },
        // CR 400.7 + CR 701.23 + CR 701.24: Multi-zone same-name exile. The
        // quantifier axis selects the lowering (§9 R1 / D2).
        //
        // `All` — mandatory mass exile → `Effect::ChangeZoneAll`. The target
        // filter encodes both the zone union (graveyard, hand, library) via
        // `InAnyZone` and the name match against the parent target via
        // `SameNameAsParentTarget`. The ChangeZoneAll resolver reads multi-zone
        // origins from the filter and per-object zone-of-origin to track
        // hand-origin exiles for the downstream draw count.
        SearchCreationImperativeAst::MultiZoneSameNameExile {
            owner,
            quantifier: MultiZoneExileQuantifier::All,
        } => Effect::ChangeZoneAll {
            origin: None,
            destination: Zone::Exile,
            target: TargetFilter::Typed(TypedFilter::default().controller(owner).properties(vec![
                crate::types::ability::FilterProp::InAnyZone {
                    zones: vec![Zone::Graveyard, Zone::Hand, Zone::Library],
                },
                crate::types::ability::FilterProp::SameNameAsParentTarget,
            ])),
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            random_order: false,
        },
        // CR 107.1c + CR 701.23a + CR 701.23b: "any number" / "up to N" →
        // interactive `Effect::SearchLibrary`. The searcher (caster, CR 701.23a
        // asymmetric) chooses a subset and may find none. `source_zones` carries
        // the zone union; the filter is the bare `SameNameAsParentTarget` name
        // match; `target_player` carries the searched player as an object-relative
        // controller-ref, resolved by `resolve_library_owner` (W4). Keeping
        // `target_player` a `Typed` wrapper (not the bare `ParentTargetController`
        // variant) makes `searcher_is_library_owner` return false so the caster —
        // not the searched player — makes the fail-to-find choice.
        // The intrinsic SearchDestination continuation (`sequence.rs`) attaches
        // the `ChangeZone { destination: Exile }` that removes the found set.
        SearchCreationImperativeAst::MultiZoneSameNameExile { owner, quantifier } => {
            let cap = match quantifier {
                // "any number" is bounded only by the matching-set size; the
                // resolver floors `UpTo` to the actual candidates.
                MultiZoneExileQuantifier::AnyNumber => i32::MAX,
                MultiZoneExileQuantifier::UpTo(n) => n as i32,
                MultiZoneExileQuantifier::All => unreachable!("All handled above"),
            };
            Effect::SearchLibrary {
                source_zones: vec![Zone::Graveyard, Zone::Hand, Zone::Library],
                filter: TargetFilter::Typed(TypedFilter::default().properties(vec![
                    crate::types::ability::FilterProp::SameNameAsParentTarget,
                ])),
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: cap }),
                reveal: false,
                target_player: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(owner),
                )),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
            }
        }
    }
}

pub(super) fn parse_hand_reveal_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<HandRevealImperativeAst> {
    // CR 406.6: Private look at source-linked exile (Scroll Rack) — no "hand" in phrase.
    if let Some((_, after_look_at)) =
        nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input))
    {
        let after_look_at_lower = &lower[lower.len() - after_look_at.len()..];
        if alt((
            tag::<_, _, OracleError<'_>>("the exiled cards"),
            tag("the cards exiled this way"),
        ))
        .parse(after_look_at_lower)
        .is_ok()
        {
            return Some(HandRevealImperativeAst::LookAt {
                target: TargetFilter::ExiledBySource,
                count: None,
                random: false,
            });
        }
    }

    if nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input)).is_some()
        && nom_primitives::scan_contains(lower, "hand")
    {
        // CR 400.1/400.2 + CR 508.5 + CR 608.2c: Possessive hand phrases are
        // player references, not object targets. Map the reusable player axes
        // explicitly so combat-trigger forms like "defending player's hand" and
        // random-card forms like "a card at random in target player's hand" do
        // not fall back to parsing "card" as the effect target.
        if let Some(((target, count, random), _)) = nom_on_lower(text, lower, |input| {
            let (rest, _) = tag("look at ").parse(input)?;
            let (rest, random_count) = opt(value(
                QuantityExpr::Fixed { value: 1 },
                alt((
                    tag::<_, _, OracleError<'_>>("a card at random in "),
                    tag("one card at random in "),
                )),
            ))
            .parse(rest)?;
            let (rest, target) = parse_hand_possessive_target(rest)?;
            Ok((rest, (target, random_count.clone(), random_count.is_some())))
        }) {
            return Some(HandRevealImperativeAst::LookAt {
                target,
                count,
                random,
            });
        }

        let (_, after_look_at) =
            nom_on_lower(text, lower, |input| value((), tag("look at ")).parse(input))?;
        let (target, _) = parse_target(after_look_at);
        return Some(HandRevealImperativeAst::LookAt {
            target,
            count: None,
            random: false,
        });
    }

    let (_, after_reveal) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("reveal "), tag("reveals ")))).parse(input)
    })?;

    // CR 701.20a: Back-reference reveal — reveals a specific card identified by
    // the parent effect's affected IDs. Common in "look at top → reveal it"
    // sequences (Frost Augur, Archghoul of Thraben, Leaf-Crowned Elder).
    //
    // The pronoun/demonstrative forms ("it" / "that card" / "those cards") are
    // matched as a clause prefix, as they have been historically: no real card
    // continues them with a divergent effect, so a prefix match is safe.
    let after_reveal_lower = &lower[lower.len() - after_reveal.len()..];
    if alt((
        tag::<_, _, OracleError<'_>>("it"),
        tag("that card"),
        tag("those cards"),
    ))
    .parse(after_reveal_lower)
    .is_ok()
    {
        return Some(HandRevealImperativeAst::RevealBackRef);
    }

    // CR 701.20a: the definite-article forms ("the card" / "the cards") are the
    // same back-reference, but far more collision-prone than the pronoun forms:
    // "reveal the cards you want to splice onto it" and "reveal the cards in your
    // library" are distinct effects that merely share the leading tokens. Match
    // the whole remaining clause with `all_consuming` ("cards" first so the
    // longer tag wins; the optional trailing period is consumed) so only a bare
    // "Reveal the card." / "Reveal the cards." clause lowers to the
    // back-reference, while compound reveal clauses fall through to their own
    // recognizers.
    if all_consuming((
        alt((tag::<_, _, OracleError<'_>>("the cards"), tag("the card"))),
        opt(tag(".")),
    ))
    .parse(after_reveal_lower.trim())
    .is_ok()
    {
        return Some(HandRevealImperativeAst::RevealBackRef);
    }

    // CR 701.20: "Reveal target <object>" — reveal a specific object selected by
    // a target phrase (Hauntwoods Shrieker — "Reveal target face-down
    // permanent"). The "target"/"a"/"each" determiner distinguishes an object
    // reveal from the hand reveals handled below; "hand" forms are excluded so
    // possessive-hand phrases ("reveal your hand") keep their RevealHand path.
    if !nom_primitives::scan_contains(after_reveal_lower, "hand")
        && alt((tag::<_, _, OracleError<'_>>("target "), tag("each ")))
            .parse(after_reveal_lower)
            .is_ok()
    {
        let (target, _) = parse_target(after_reveal);
        if !matches!(target, TargetFilter::None) {
            return Some(HandRevealImperativeAst::RevealObject { target });
        }
    }

    // CR 701.20a: "reveals a number of cards from their hand equal to X"
    if nom_primitives::scan_contains(lower, "hand")
        && nom_primitives::scan_contains(lower, "equal to ")
    {
        if let Some((_, qty_text)) = lower.split_once("equal to ") {
            let qty_text = qty_text.trim_end_matches('.');
            // CR 701.20a: "reveals a number of cards ... equal to X" — the reveal
            // count is a full dynamic quantity expression, not just a bare ref.
            // CR 107.1: integer arithmetic; parse_cda_quantity composes the
            // offset/arithmetic grammar ("one plus the number of creature cards in
            // your graveyard" -> Offset{ Ref(ZoneCardCount), +1 }, Klaw) and
            // subsumes the single-ref case. CR 107.1b: a negative result is
            // clamped to 0 at resolution (reveal_hand.rs .max(0)).
            if let Some(count) = super::super::oracle_quantity::parse_cda_quantity(qty_text) {
                return Some(HandRevealImperativeAst::RevealPartial { count });
            }
        }
    }

    // "reveal the top N" is now handled by parse_search_and_creation_ast → Dig path.
    // This function only handles hand-related reveals.

    if nom_primitives::scan_contains(lower, "hand") {
        let (target, card_filter) =
            parse_hand_reveal_target_and_card_filter(after_reveal_lower, ctx);
        return Some(HandRevealImperativeAst::RevealAll {
            target,
            card_filter,
        });
    }

    None
}

fn parse_hand_reveal_target_and_card_filter(
    after_reveal_lower: &str,
    ctx: &mut ParseContext,
) -> (TargetFilter, TargetFilter) {
    // CR 701.20a + reflexive choose: "<possessive> hand and you choose a [filter]
    // card from it" names the revealing player's hand directly, then the
    // controller chooses a filtered card from it (Biting-Palm Ninja: "that player
    // reveals their hand and you choose a nonland card from it."). The fused choose
    // clause must populate `card_filter`; an empty `None` filter matches nothing, so
    // without it the RevealHand chooses and exiles nothing (a silent no-op).
    if let Ok((rest, target)) = parse_hand_possessive_target(after_reveal_lower) {
        if let Ok((_, choose)) = preceded(
            (space0, tag::<_, _, OracleError<'_>>("and "), space0),
            nom::combinator::rest,
        )
        .parse(rest)
        {
            let chooses_card_from_it = nom_primitives::scan_contains(choose, "card from it")
                && alt((tag::<_, _, OracleError<'_>>("you choose "), tag("choose ")))
                    .parse(choose)
                    .is_ok();

            if chooses_card_from_it {
                return (target, super::parse_choose_filter(choose, ctx));
            }
        }
    }

    if let Ok((after_all, _)) = tag::<_, _, OracleError<'_>>("all ").parse(after_reveal_lower) {
        let Ok((hand_phrase, descriptor)) = terminated(
            take_until::<_, _, OracleError<'_>>(" cards"),
            alt((
                tag::<_, _, OracleError<'_>>(" cards in "),
                tag(" cards from "),
            )),
        )
        .parse(after_all) else {
            return (TargetFilter::Any, TargetFilter::None);
        };
        let target = parse_hand_possessive_target(hand_phrase)
            .map(|(_, target)| target)
            .unwrap_or(TargetFilter::Any);
        if descriptor.trim().is_empty() {
            return (target, TargetFilter::Any);
        }
        let singular = format!("{} card", descriptor.trim());
        let (filter, rem) = parse_type_phrase(&singular);
        if rem.trim().is_empty() && matches!(filter, TargetFilter::Typed(_)) {
            return (target, filter);
        }
        return (target, TargetFilter::None);
    }

    // CR 701.20a: "reveal a card from your hand" / "reveal an [type] card from ..."
    let Ok((after_article, _)) =
        alt((tag::<_, _, OracleError<'_>>("a "), tag("an "))).parse(after_reveal_lower)
    else {
        return (TargetFilter::Any, TargetFilter::None);
    };
    if let Ok((hand_phrase, _)) = tag::<_, _, OracleError<'_>>("card from ").parse(after_article) {
        let target = parse_hand_possessive_target(hand_phrase)
            .map(|(_, target)| target)
            .unwrap_or(TargetFilter::Any);
        return (target, TargetFilter::Any);
    }
    let Ok((hand_phrase, descriptor)) = terminated(
        take_until::<_, _, OracleError<'_>>(" card from "),
        tag(" card from "),
    )
    .parse(after_article) else {
        return (TargetFilter::Any, TargetFilter::None);
    };
    let target = parse_hand_possessive_target(hand_phrase)
        .map(|(_, target)| target)
        .unwrap_or(TargetFilter::Any);
    let singular = format!("{} card", descriptor.trim());
    let (filter, rem) = parse_type_phrase(&singular);
    if rem.trim().is_empty() && matches!(filter, TargetFilter::Typed(_)) {
        (target, filter)
    } else {
        (target, TargetFilter::None)
    }
}

pub(super) fn lower_hand_reveal_ast(ast: HandRevealImperativeAst) -> Effect {
    match ast {
        HandRevealImperativeAst::LookAt {
            target,
            count,
            random,
        } => Effect::RevealHand {
            target,
            card_filter: TargetFilter::None,
            count,
            selection: if random {
                crate::types::ability::CardSelectionMode::Random
            } else {
                crate::types::ability::CardSelectionMode::Chosen
            },
            choice_optional: false,
            reveal: false,
        },
        HandRevealImperativeAst::RevealAll {
            target,
            card_filter,
        } => Effect::RevealHand {
            target,
            card_filter,
            count: None,
            selection: crate::types::ability::CardSelectionMode::Chosen,
            choice_optional: false,
            reveal: true,
        },
        HandRevealImperativeAst::RevealPartial { count } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: TargetFilter::None,
            count: Some(count),
            selection: crate::types::ability::CardSelectionMode::Chosen,
            choice_optional: false,
            reveal: true,
        },
        // CR 701.20a: Back-reference reveal — distinct from RevealHand (zone-wide).
        // ParentTarget binds at runtime to the parent ability's affected IDs.
        HandRevealImperativeAst::RevealBackRef => Effect::Reveal {
            target: TargetFilter::ParentTarget,
        },
        // CR 701.20: Reveal a targeted object (Hauntwoods Shrieker).
        HandRevealImperativeAst::RevealObject { target } => Effect::Reveal { target },
    }
}

fn parse_hand_possessive_target(input: &str) -> nom::IResult<&str, TargetFilter, OracleError<'_>> {
    alt((
        value(TargetFilter::Controller, tag("your hand")),
        value(TargetFilter::Player, tag("target player's hand")),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("target opponent's hand"),
        ),
        // "look at an opponent's hand" (Anointed Peacekeeper ETB) — a
        // non-targeted controller choice. Ordered AFTER the longer "target …"
        // tags so those still win. Routes the LookAt path to
        // `Effect::RevealHand { reveal: false, target: Typed(Opponent) }`.
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("an opponent's hand"),
        ),
        value(TargetFilter::TriggeringPlayer, tag("that player's hand")),
        value(TargetFilter::TriggeringPlayer, tag("their hand")),
        value(
            TargetFilter::DefendingPlayer,
            tag("defending player's hand"),
        ),
    ))
    .parse(input)
}

/// CR 122.1 + CR 115.1: "[a|one|any] counter on <target>" — designate the object
/// whose counters a later clause adjusts (Clockspinning sentence 1: "Choose a
/// counter on target permanent or suspended card"). The chosen counter KIND is
/// decided at resolution by sentence 2 (`Effect::ChooseCounterAdjustment`), so
/// this clause only fixes the TARGET and lowers to `Effect::TargetOnly`.
///
/// `parse_target` does not forward-scan past the "counter on " lead-in (the lead
/// is an effect noun, not a target descriptor), so it is stripped with a nom
/// `tag` here before delegating to the shared target parser — mirroring the
/// "for each kind of counter on " + `parse_target` structure in
/// `try_parse_proliferate_target`. A bare `Any` target (no concrete filter) is
/// rejected so this never swallows non-counter "choose" clauses; any leftover
/// after the target phrase (beyond trailing punctuation) likewise rejects.
fn try_parse_choose_counter_on_target(rest: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;
    let lower = rest.to_lowercase();
    let (_, after_on) = nom_on_lower(rest, &lower, |i| {
        value(
            (),
            alt((
                tag::<_, _, E>("a counter on "),
                tag("one counter on "),
                tag("any counter on "),
            )),
        )
        .parse(i)
    })?;
    let (target, remainder) = parse_target(after_on);
    if matches!(target, TargetFilter::Any) {
        return None;
    }
    if !remainder
        .trim_start_matches([',', ' '])
        .trim_end_matches(['.', ' '])
        .is_empty()
    {
        return None;
    }
    Some(ChooseImperativeAst::TargetOnly { target })
}

/// CR 701.9a + CR 107.1b: "[Target player] chooses a card in their hand and
/// discards the rest" (Monomania). The chosen player keeps exactly one card of
/// their choice and discards every other card in hand -- equivalently, discards
/// `hand size - 1` cards, chooser-directed. Routes to the existing
/// `Effect::Discard` (via `TargetedImperativeAst::Discard`) using the canonical
/// `ClampMin { Offset { HandSize, -1 }, 0 }` "N - 1, floored at 0" count
/// (CR 107.1b keeps the amount non-negative when the hand holds one or zero
/// cards); no new effect is introduced. Whole-clause anchored with `eof` so a
/// bare "choose a card ..." prefix cannot hijack unrelated choose clauses. The
/// leading verb has already been de-inflected to "choose" by the subject-
/// stripping layer, but `opt("s")` also accepts a raw "chooses" for
/// context-free callers. The possessive selects the `HandSize` scope;
/// `Effect::Discard.target` is left as `Controller` for `inject_subject_target`
/// to rewrite to the discarding player, exactly as the sibling "discard their
/// hand" path does.
fn parse_choose_discard_rest_of_hand(text: &str, lower: &str) -> Option<TargetedImperativeAst> {
    let (hand_owner, _) = nom_on_lower(text, lower, |input| {
        let (input, _) = tag("choose").parse(input)?;
        let (input, _) = opt(tag("s")).parse(input)?;
        let (input, _) = tag(" a card in ").parse(input)?;
        let (input, hand_owner) = alt((
            value(PlayerScope::Controller, tag("your hand")),
            value(PlayerScope::Target, tag("their hand")),
            value(PlayerScope::Target, tag("his or her hand")),
        ))
        .parse(input)?;
        let (input, _) = tag(" and discard").parse(input)?;
        let (input, _) = opt(tag("s")).parse(input)?;
        let (input, _) = tag(" the rest").parse(input)?;
        let (input, _) = opt(tag(".")).parse(input)?;
        let (input, _) = eof.parse(input)?;
        Ok((input, hand_owner))
    })?;
    Some(TargetedImperativeAst::Discard {
        count: QuantityExpr::ClampMin {
            inner: Box::new(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::HandSize { player: hand_owner },
                }),
                offset: -1,
            }),
            minimum: 0,
        },
        random: false,
        up_to: false,
        unless_filter: None,
        filter: None,
    })
}

/// CR 701.13a + CR 109.5 + CR 608.2c: "choose a[n] <type> they control and
/// [verb]s it" — the per-actor edict where the chooser both selects and acts on
/// one of their own permanents (Sothera, the Supervoid: "each opponent chooses a
/// creature they control and exiles it"). The "each opponent" subject and its
/// leading verb are already stripped / de-inflected to "choose" by the
/// player-scope layer, so this operates on the post-"choose " body; the inner
/// verb keeps its third-person "-s" inflection ("exiles it").
///
/// Reparses to the controller-scoped edict ("exile a creature you control",
/// controller `You`). `You` is CORRECT at parse time: the each-actor driver
/// rebinds `ability.controller` to each iterated player at runtime
/// (`set_controller_recursive`), and `FilterContext::from_ability` resolves the
/// reparsed filter's `ControllerRef::You` to the ITERATING actor — so each actor
/// exiles from their OWN permanents (CR 109.5). The verb axis is a single
/// `alt()`, covering the edict class (exile / sacrifice / destroy) without
/// proliferating recognizers.
fn try_parse_choose_and_verb_it_edict(rest_lower: &str) -> Option<ChooseImperativeAst> {
    let (_, (_, type_text, _, verb, _, _, _)) = (
        alt((tag::<_, _, OracleError<'_>>("a "), tag("an "))),
        take_until::<_, _, OracleError<'_>>(" they control and "),
        tag::<_, _, OracleError<'_>>(" they control and "),
        alt((
            value("exile", tag::<_, _, OracleError<'_>>("exiles")),
            value("sacrifice", tag("sacrifices")),
            value("destroy", tag("destroys")),
        )),
        tag::<_, _, OracleError<'_>>(" it"),
        opt(tag::<_, _, OracleError<'_>>(".")),
        eof,
    )
        .parse(rest_lower)
        .ok()?;
    // Validate the captured phrase is a real object type with no leftover text —
    // keeps the recognizer precise so it never hijacks unrelated "choose a …"
    // bodies whose type phrase is junk or carries a trailing predicate.
    let (filter, filter_rem) = parse_type_phrase(type_text);
    if !filter_rem.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some(ChooseImperativeAst::Reparse {
        text: format!("{verb} a {type_text} you control"),
    })
}

pub(super) fn parse_choose_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ChooseImperativeAst> {
    if let Some(ast) = try_parse_choose_from_zone(lower, ctx) {
        return Some(ast);
    }

    // CR 702.62b + CR 608.2c: "choose a suspended card you own" (Amy Pond) — a
    // zone-implicit Exile selection lacking the "in/from <zone>" connector
    // `try_parse_choose_from_zone` requires. Checked before the "choose " strip +
    // `is_choose_as_targeting` fallback so the clause routes to the interactive
    // ChooseFromZone seam instead of being treated as targeting.
    if let Some(ast) = try_parse_choose_suspended_card(lower) {
        return Some(ast);
    }

    // CR 608.2c + CR 603.7 / CR 610.3 + CR 406.6: "choose a card [at random]
    // exiled this way / exiled with ~" — the impulse-exile choose anaphor. The
    // "exiled this way" referent is the chain's tracked set (the cards exiled by
    // a preceding clause in this resolution, e.g. End-Blaze Epiphany); the
    // "exiled with ~/it" referent is the source's linked-exile set, scanned in
    // Exile by `TargetFilter::ExiledBySource` (Omenpath Journey). Checked before
    // the bare "choose " strip so it never misroutes to the targeting fallback.
    if let Some(ast) = try_parse_choose_exiled_anaphor(lower) {
        return Some(ast);
    }

    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("choose ")).parse(input))
    {
        let rest_lower = &lower[lower.len() - rest.len()..];

        // CR 609.7a: "choose a source you control" — damage-source selection,
        // not permanent targeting. Must precede `is_choose_as_targeting`, which
        // would misroute this shape to `TargetOnly { Any }`.
        if let Some(ast) = try_parse_choose_damage_source(rest) {
            return Some(ast);
        }

        // CR 101.4 + CR 701.21a: "choose from among ... an artifact, a creature, ..."
        // or "choose an artifact, a creature, ... from among ..."
        // Must be checked before is_choose_as_targeting since these are NOT targeting.
        if let Some(ast) = parse_category_and_sacrifice_rest(rest_lower) {
            return Some(ast);
        }

        // CR 108.3 + CR 701.38d: "choose a permanent owned by the voter" —
        // voter-referential ownership scoping on the Battlefield. This must be
        // checked BEFORE is_choose_as_targeting so it routes to the interactive
        // ChooseFromZone seam (which pauses for player choice) instead of the
        // non-interactive TargetOnly path.
        if let Some(ast) = try_parse_choose_owned_by_voter(rest, rest_lower, ctx) {
            return Some(ast);
        }

        // CR 608.2d + CR 122.1: "choose a counter on it / that permanent" —
        // pick one of the distinct counter kinds on the anaphoric object
        // (The Caves of Androzani II/III). Anaphoric form only; the declared-
        // target form ("a counter on target permanent", Ichormoon Gauntlet)
        // is handled by the target parser below.
        if let Some(ast) = try_parse_choose_counter_kind(rest_lower) {
            return Some(ast);
        }

        // CR 122.1 + CR 115.1: "Choose a counter on target <permanent/card>" only
        // designates the object whose counters a later clause adjusts
        // (Clockspinning). The "a counter on " lead-in is not a target descriptor,
        // so `parse_target` does not forward-scan past it (it would fall through to
        // `Any`); strip it with a nom `tag` before delegating to the shared target
        // parser, mirroring `try_parse_proliferate_target`'s
        // "for each kind of counter on " + `parse_target` structure.
        if let Some(ast) = try_parse_choose_counter_on_target(rest) {
            return Some(ast);
        }

        // CR 701.13a + CR 109.5 + CR 608.2c: "choose a <type> they control and
        // [verb]s it" — the per-actor edict (Sothera, the Supervoid). Must
        // precede `is_choose_as_targeting`, which would otherwise Reparse the
        // whole conjoined body (parse_effect fails → `Effect::Unimplemented`)
        // and drop the exile. Operates on `rest_lower` (the "choose " strip has
        // de-inflected the leading actor verb; the inner "exiles" keeps its -s).
        if let Some(ast) = try_parse_choose_and_verb_it_edict(rest_lower) {
            return Some(ast);
        }

        if super::is_choose_as_targeting(rest_lower) {
            // CR 115.1c + CR 601.2c: "Choose target X and target Y" declares
            // two independent target slots on the same activated/triggered
            // ability. Detect the compound "target ... and target ..." shape
            // BEFORE falling through to `parse_effect` (which collapses
            // target slot B into the surrounding effect text and yields a
            // single-target Reparse) so the second slot (e.g., Goblin
            // Welder's "artifact card in that player's graveyard") is
            // preserved instead of being silently dropped.
            if let Some(ast) = try_parse_two_targets(rest, ctx) {
                return Some(ast);
            }
            let inner = super::parse_effect(rest);
            if !matches!(inner, Effect::Unimplemented { .. }) {
                return Some(ChooseImperativeAst::Reparse {
                    text: rest.to_string(),
                });
            }
            let (target, _) = parse_target(rest);
            return Some(ChooseImperativeAst::TargetOnly { target });
        }
    }

    if let Some(choice_type) = super::try_parse_named_choice(lower) {
        // CR 608.2d (override) + CR 701.9b (analogous): "choose a player at
        // random" (Strax) — the game selects the referent, not the controller.
        let selection = if nom_primitives::scan_contains(lower, "at random") {
            TargetSelectionMode::Random
        } else {
            TargetSelectionMode::Chosen
        };
        // CR 608.2c: stash the committed domain so a later "an opponent guesses
        // which [value] you chose" clause in this same ability resolution embeds
        // it in `GuessSubject::CommittedChoice` (in-order instructions within one
        // ability — not a CR 607.2d link between two distinct printed abilities).
        ctx.pending_choice_type = Some(choice_type.clone());
        return Some(ChooseImperativeAst::NamedChoice {
            choice_type,
            selection,
        });
    }

    if nom_on_lower(text, lower, |input| value((), tag("choose ")).parse(input)).is_some()
        && nom_primitives::scan_contains(lower, "card from it")
    {
        let choice_optional = nom_on_lower(text, lower, |input| {
            value((), tag("you may choose ")).parse(input)
        })
        .is_some();
        return Some(ChooseImperativeAst::RevealHandFilter {
            card_filter: super::parse_choose_filter(lower, ctx),
            choice_optional,
        });
    }

    // "choose N of them/those [cards]" / "you choose N of those cards" /
    // "an opponent chooses N of them" — anaphoric reference to a previously
    // revealed/exiled set, producing ChooseFromZone.
    if let Some((count, chooser, selection)) = parse_choose_anaphoric(lower) {
        return Some(ChooseImperativeAst::FromTrackedSet {
            count,
            chooser,
            selection,
        });
    }

    None
}

/// CR 609.7a: "choose a source [you control|...]" lowers to `ChooseDamageSource`.
fn try_parse_choose_damage_source(rest: &str) -> Option<ChooseImperativeAst> {
    let source_filter =
        crate::parser::oracle_replacement::parse_choose_damage_source_candidate(rest)?;
    Some(ChooseImperativeAst::DamageSource { source_filter })
}

/// CR 608.2d + CR 122.1: "a counter on <anaphor>" — the counter-kind choice
/// head for the anaphoric form (`it` / `that permanent` / `that creature` →
/// `ParentTarget`, The Caves of Androzani II/III). Combinator-based: `tag("a
/// counter on ")` then the anaphor phrase, consuming the entire residual.
///
/// The declared-target form ("a counter on target permanent", Ichormoon
/// Gauntlet) is intentionally excluded: its consumer ("put one more counter of
/// that kind on that permanent or remove one of those counters from it") is not
/// yet implemented, so parsing only the head would create a partial and
/// rules-incorrect parse. Left as an honest Unimplemented gap.
fn try_parse_choose_counter_kind(rest_lower: &str) -> Option<ChooseImperativeAst> {
    let anaphor = |i| -> OracleResult<'_, ()> {
        let (i, _) = tag("a counter on ").parse(i)?;
        let (i, _) = alt((tag("it"), tag("that permanent"), tag("that creature"))).parse(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        let (i, _) = eof.parse(i)?;
        Ok((i, ()))
    };
    if anaphor(rest_lower.trim()).is_ok() {
        return Some(ChooseImperativeAst::CounterKind {
            target: TargetFilter::ParentTarget,
        });
    }
    None
}

/// CR 108.3 + CR 701.38d: Detect "a <type> owned by the voter" and emit
/// `ChooseFromZone { Battlefield, ScopedPlayer }` so the interactive choice
/// seam handles mid-resolution target binding for per-ballot vote effects.
fn try_parse_choose_owned_by_voter(
    _text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ChooseImperativeAst> {
    // Match: "a permanent owned by the voter", "a creature owned by the voter", etc.
    // The chain splitter has already stripped any " and gain control of it" continuation.
    // Uses nom `scan_preceded` + `tag` to locate the ownership suffix compositionally.
    type E<'a> = OracleError<'a>;
    let (filter_text, _, _suffix) =
        nom_primitives::scan_preceded(lower, tag::<_, _, E>("owned by the voter"))?;
    let filter_text = filter_text.trim_end();
    let filter = super::search::parse_search_filter(filter_text, ctx);
    // CR 108.3: Inject ownership filter so choose_from_zone restricts
    // candidates to permanents owned by the scoped player (voter).
    let filter = match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties.push(FilterProp::Owned {
                controller: ControllerRef::ScopedPlayer,
            });
            TargetFilter::Typed(tf)
        }
        TargetFilter::Any => {
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::Owned {
                controller: ControllerRef::ScopedPlayer,
            }]))
        }
        other => other,
    };
    Some(ChooseImperativeAst::FromZone {
        count: 1,
        zones: vec![Zone::Battlefield],
        zone_owner: ZoneOwner::ScopedPlayer,
        filter,
        chooser: Chooser::Controller,
        up_to: false,
        // CR 608.2d: per-ballot voter choice is controller-directed, never random.
        selection: crate::types::ability::CardSelectionMode::Chosen,
    })
}

/// CR 608.2c + CR 603.7 / CR 610.3 + CR 406.6: Parse "choose a card [at random]
/// exiled this way / exiled with ~ / exiled with it" — the impulse-exile choose
/// anaphor.
///
/// Two referents, distinguished by the anaphor tail:
/// - "exiled this way" → the chain's tracked set (cards exiled by a preceding
///   clause this resolution). Lowered via [`ChooseImperativeAst::FromTrackedSet`]
///   → `Effect::ChooseFromZone` reading the chain tracked set (End-Blaze
///   Epiphany).
/// - "exiled with ~" / "exiled with it" → the source's linked-exile set,
///   scanned in `Zone::Exile` by [`TargetFilter::ExiledBySource`]. Lowered via
///   [`ChooseImperativeAst::FromZone`] with [`ZoneOwner::AllOwners`] so the
///   runtime scans the whole shared exile zone (CR 400.1) and the linked-exile
///   filter (CR 607.2a) does all scoping — opponent-owned cards Koh exiled are
///   in the pool, not just the controller's own (Omenpath Journey, Koh).
///
/// The optional "at random" qualifier sets [`CardSelectionMode::Random`]
/// (CR 608.2d override): the game selects, the controller does not.
fn try_parse_choose_exiled_anaphor(lower: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    // CR 608.2c + CR 608.2d: A standalone "Choose one." / "Choose one card."
    // clause (empty tail) in a resolution chain is the impulse-exile reduction
    // idiom — a preceding clause exiled one or more cards and a following clause
    // grants permission to play one of them ("Exile the top three cards of your
    // library. Choose one. You may play that card this turn." — Chandra,
    // Flameshaper). The anaphor referent is the chain's tracked set, mirroring
    // "choose one of them" but without the explicit anaphor suffix. The modal
    // header "Choose one —" is consumed earlier by the modal-block dispatch, so
    // any "choose one" reaching the effect parser is this reduction form.
    if let Ok((tail, ())) = preceded(
        alt((tag::<_, _, E>("choose "), tag("you choose "))),
        value((), alt((tag::<_, _, E>("one card"), tag("one")))),
    )
    .parse(lower)
    {
        if tail.is_empty() {
            return Some(ChooseImperativeAst::FromTrackedSet {
                count: 1,
                chooser: Chooser::Controller,
                selection: CardSelectionMode::Chosen,
            });
        }
    }

    // "choose " / "you choose ", then the singular card anaphor "a [type] card" /
    // "one [type] card". The optional card-type qualifier (Koh, the Face Stealer's
    // "a creature card exiled with ~") narrows the choice; the bare "a card" form
    // leaves it untyped. The type is intersected with the linked-exile set below.
    let (rest_after, card_type) = preceded(
        alt((tag::<_, _, E>("choose "), tag("you choose "))),
        preceded(
            alt((tag("a "), tag("one "))),
            parse_choose_card_type_qualifier,
        ),
    )
    .parse(lower)
    .ok()?;

    // Optional " at random" qualifier (CR 608.2d override).
    let (rest_after, selection) = match tag::<_, _, E>(" at random").parse(rest_after) {
        Ok((rest, _)) => (rest, CardSelectionMode::Random),
        Err(_) => (rest_after, CardSelectionMode::Chosen),
    };

    // "exiled this way" — the chain tracked set. Only the untyped form is attested
    // (impulse-exile reductions choose from the whole chain set); a typed
    // restriction here would need `TrackedSetFiltered`, so fall through honestly.
    if card_type.is_none() {
        if let Ok((tail, _)) = tag::<_, _, E>(" exiled this way").parse(rest_after) {
            if tail.is_empty() {
                return Some(ChooseImperativeAst::FromTrackedSet {
                    count: 1,
                    chooser: Chooser::Controller,
                    selection,
                });
            }
        }
    }

    // "exiled with ~" / "exiled with it" — the source's linked-exile set
    // (`ExiledBySource`), intersected with the optional card-type qualifier so the
    // pool is exactly the matching cards exiled with the host (Koh: creature cards
    // exiled with Koh).
    if let Ok((tail, _)) =
        alt((tag::<_, _, E>(" exiled with ~"), tag(" exiled with it"))).parse(rest_after)
    {
        if tail.is_empty() {
            let filter = match card_type {
                Some(tf) => TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(tf)),
                        TargetFilter::ExiledBySource,
                    ],
                },
                None => TargetFilter::ExiledBySource,
            };
            // CR 400.1 + CR 607.2a: "exiled with [source]" is owner-agnostic —
            // exile is a zone shared by all players (CR 400.1) and the
            // linked-exile reference (CR 607.2a) is defined by linkage, not
            // ownership. Koh exiles opponents' creatures, so the candidate pool
            // must span every owner; `ZoneOwner::AllOwners` scans the whole
            // exile zone and lets `ExiledBySource` (in `filter`) do all scoping.
            return Some(ChooseImperativeAst::FromZone {
                count: 1,
                zones: vec![Zone::Exile],
                zone_owner: ZoneOwner::AllOwners,
                filter,
                chooser: Chooser::Controller,
                up_to: false,
                selection,
            });
        }
    }

    None
}

/// CR 702.62b + CR 608.2c: "choose a suspended card [you own]" — an interactive
/// selection of a suspended card (exile + suspend + time counter), with an optional
/// ownership qualifier. No explicit `in/from <zone>` connector, so
/// `try_parse_choose_from_zone` does not claim it. Routes to the
/// `ChooseFromZone { Exile }` seam so the runtime pauses for the player's pick
/// before any chained continuation resolves. Checked before `is_choose_as_targeting`
/// so the clause is claimed here. Composed entirely from nom combinators; the
/// candidate filter reuses the shared `suspended_card_filter` building block.
///
/// Attested forms:
/// - "choose a suspended card you own" (Amy Pond, CR 108.3 — owned by controller)
/// - "choose a suspended card an opponent owns" (sibling coverage, CR 108.3)
/// - "choose a suspended card" (no ownership restriction — any player's suspended card)
fn try_parse_choose_suspended_card(lower: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    let (rest, _) = alt((tag::<_, _, E>("choose "), tag("you choose ")))
        .parse(lower)
        .ok()?;
    let (rest, _) = alt((tag::<_, _, E>("a "), tag("an "))).parse(rest).ok()?;
    let (rest, _) = alt((tag::<_, _, E>("suspended cards"), tag("suspended card")))
        .parse(rest)
        .ok()?;
    // Parse optional ownership qualifier.  Supported forms:
    //   "you own"            → Some(ControllerRef::You)
    //   "an opponent owns"   → Some(ControllerRef::Opponent)
    //   <no qualifier>       → None (any player's suspended card)
    // Require the clause to end here: if a chain failed to split, an unparsed
    // trailing continuation ("… and remove that many time counters from it") would
    // be left over — bail so the line falls to a documented strict failure rather
    // than a silent misparse that drops the counter clause.
    let (rest, owner) = opt(alt((
        value(ControllerRef::You, tag::<_, _, E>(" you own")),
        value(ControllerRef::Opponent, tag(" an opponent owns")),
    )))
    .parse(rest)
    .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    Some(ChooseImperativeAst::FromZone {
        count: 1,
        zones: vec![Zone::Exile],
        zone_owner: ZoneOwner::Controller,
        // CR 108.3: ownership restricted by the parsed qualifier (None = any player).
        filter: crate::parser::oracle_quantity::suspended_card_filter(owner),
        chooser: Chooser::Controller,
        up_to: false,
        // CR 608.2d: controller-directed selection, never random.
        selection: CardSelectionMode::Chosen,
    })
}

/// CR 205.2a: Parse the card-type qualifier of a singular card anaphor — one card
/// type word followed by " card" (→ `Some(type)`), or the bare "card" (→ `None`).
/// Used by [`try_parse_choose_exiled_anaphor`] for "choose a [type] card exiled
/// with ~". A single type covers the attested class (Koh's "creature card");
/// multi-type qualifiers ("artifact creature card") are not yet attested.
fn parse_choose_card_type_qualifier(input: &str) -> OracleResult<'_, Option<TypeFilter>> {
    alt((
        map(
            terminated(
                alt((
                    value(TypeFilter::Creature, tag("creature")),
                    value(TypeFilter::Artifact, tag("artifact")),
                    value(TypeFilter::Enchantment, tag("enchantment")),
                    value(TypeFilter::Land, tag("land")),
                    value(TypeFilter::Planeswalker, tag("planeswalker")),
                    value(TypeFilter::Instant, tag("instant")),
                    value(TypeFilter::Sorcery, tag("sorcery")),
                    value(TypeFilter::Battle, tag("battle")),
                )),
                tag(" card"),
            ),
            Some,
        ),
        value(None, tag("card")),
    ))
    .parse(input)
}

fn try_parse_choose_from_zone(lower: &str, ctx: &mut ParseContext) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    let (_, choice_text) = alt((
        preceded(tag::<_, _, E>("choose "), rest),
        preceded(tag("you choose "), rest),
    ))
    .parse(lower)
    .ok()?;

    // CR 608.2d (override) + CR 701.9b (analogous): "choose ... at random" — the
    // game selects the card(s), not the controller. Captured as a typed
    // `CardSelectionMode` (was previously a bail-out that dropped the qualifier).
    let selection = if nom_primitives::scan_contains(choice_text, "at random") {
        crate::types::ability::CardSelectionMode::Random
    } else {
        crate::types::ability::CardSelectionMode::Chosen
    };

    // The "at random" qualifier is now captured in `selection`; strip it so it
    // does not leak into the downstream zone/filter parse. Otherwise a pre-zone
    // qualifier ("a creature card at random from target opponent's graveyard")
    // lands in the search-filter prefix, which `parse_search_filter` can't
    // classify, emitting a spurious "search-filter-suffix unmatched"
    // TargetFallback (Tariel, Reckoner of Souls; Deadbridge Chant; Higure).
    // allow-noncombinator: strip a captured free-floating qualifier before sub-parse
    let choice_text_owned = choice_text.replace(" at random", "");
    let choice_text = choice_text_owned.as_str();

    let (filter_prefix, (zone_owner, zones), zone_suffix) =
        nom_primitives::scan_preceded(choice_text, parse_choose_zone_connector)?;
    if filter_prefix.trim().is_empty() {
        return None;
    }

    // The post-zone suffix ("with mana value 3 or greater") is an optional
    // search-filter restriction. `parse_search_filter`'s suffix dispatch is
    // strict: an unmodeled clause ("without blitz", "that hasn't been chosen")
    // emits a `search-filter-suffix unmatched` TargetFallback diagnostic. Probe
    // the prefix+suffix on a throwaway context first; only fold the suffix into
    // the real parse when it classifies cleanly. An unmodeled suffix is dropped
    // (the type filter still parses) rather than surfacing a false regression.
    let bare_filter_text = strip_choose_article(filter_prefix.trim())?.to_string();
    let suffix = zone_suffix.trim();
    let filter_text = if suffix.is_empty() {
        bare_filter_text
    } else {
        let with_suffix = format!("{bare_filter_text} {suffix}");
        let mut probe = ParseContext::default();
        super::search::parse_search_filter(&with_suffix, &mut probe);
        let suffix_classified = !probe.diagnostics.iter().any(|d| {
            matches!(
                d,
                OracleDiagnostic::TargetFallback { context, .. }
                    if context == "search-filter-suffix unmatched"
            )
        });
        if suffix_classified {
            with_suffix
        } else {
            bare_filter_text
        }
    };

    let filter = super::search::parse_search_filter(&filter_text, ctx);
    Some(ChooseImperativeAst::FromZone {
        count: 1,
        zones,
        zone_owner,
        filter,
        chooser: Chooser::Controller,
        up_to: false,
        selection,
    })
}

/// CR 101.4 + CR 608.2c: "For each player, choose a `<filter>` card in that
/// player's `<zone>`" — the spell's controller picks one card from EVERY
/// player's zone in APNAP order, accumulating the picks for a downstream "put
/// those cards onto the battlefield" reanimation. The leading "for each player,"
/// is stripped here (the chain splitter keeps the prefix attached because the
/// comma-delimited body is the clause), then the shared `try_parse_choose_from_zone`
/// combinator extracts the filter + zone from "that player's `<zone>`"; the
/// per-player iteration is encoded by overriding `zone_owner` to `EachPlayer`.
/// Building block for Breach the Multiverse (issue #3302).
pub(super) fn parse_for_each_player_choose_from_zone(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    let (_, body) = preceded(
        alt((tag::<_, _, E>("for each player, "), tag("for each player "))),
        rest,
    )
    .parse(lower)
    .ok()?;

    // The body's zone reference is "that player's <zone>", which
    // `parse_choose_zone_connector` maps to `TargetedPlayer`; the "for each
    // player" prefix promotes it to per-player iteration (`EachPlayer`).
    if let Some(ChooseImperativeAst::FromZone {
        count,
        zones,
        zone_owner: ZoneOwner::TargetedPlayer,
        filter,
        chooser,
        up_to,
        selection,
    }) = try_parse_choose_from_zone(body, ctx)
    {
        return Some(ChooseImperativeAst::FromZone {
            count,
            zones,
            zone_owner: ZoneOwner::EachPlayer,
            filter,
            chooser,
            up_to,
            selection,
        });
    }

    // NOTE: the battlefield-control choose-only form ("for each player, choose
    // [up to one] <type> that player controls" — Winnowing, Unstable Glyphbridge,
    // Kitesail Larcenist) is intentionally NOT handled here. Their CHOOSE clause
    // parses cleanly via `parse_controlled_battlefield_body`, but each card's
    // downstream PAYLOAD is not yet supported end-to-end (Winnowing: per-player
    // `ParentTarget` binding in the sacrifice sweep; Glyphbridge: "destroy all
    // except creatures chosen this way" tracked-set exclusion; Kitesail: a
    // duration-bound continuous effect on the chosen set). Emitting the choose
    // here would yield a silently-wrong chain (the payload over-acts), which is
    // strictly worse than an honest residual gap — so these stay Unimplemented
    // until their payloads land. The combined choose+exile NON-target form (Kaya,
    // below) is verified end-to-end and IS handled; its printed-`target` variant
    // (CR 115.1c + CR 601.2c) needs announced per-iterated-player target slots the
    // engine cannot yet model, so it is intentionally Unimplemented.
    None
}

/// CR 608.2c + CR 608.2k: Parse the body shared by the per-player
/// battlefield-control choose ("choose [up to one] [other] `<type>` that player
/// controls") and exile ("exile [up to one] `<type>` that player controls",
/// Kaya) forms — everything AFTER the verb head. Returns `(up_to, filter)`.
///
/// "up to one" → `up_to = true` (zero-or-one); a bare article ("a"/"an"/"one")
/// is the mandatory single pick. "other " folds into the filter
/// (`FilterProp::Another`, excluding the source).
///
/// The NON-target form is a resolution-scoped per-player choice (CR 608.2c +
/// CR 115.10a: the affected permanent is chosen on resolution and is NOT a
/// target). The printed-`target` form is different in kind — CR 115.1c +
/// CR 601.2c require its targets to be announced as the ability is activated,
/// chosen through the targeting machinery (legality / hexproof / shroud /
/// protection enforced). The engine has no machinery for a variable,
/// per-iterated-player set of announced target slots, so the printed-`target`
/// variant is NOT representable as a resolution choice: this body rejects a
/// leading `target ` (returns `None`) and the dispatcher falls through to
/// `Effect::unimplemented`. The trailing "that player controls" relative-control
/// clause is required (it is what distinguishes the battlefield-control form from
/// the zone-noun form).
fn parse_controlled_battlefield_body(
    after_verb: &str,
    ctx: &mut ParseContext,
) -> Option<(bool, TargetFilter)> {
    type E<'a> = OracleError<'a>;

    let (after_qty, up_to) = alt((
        value(true, tag::<_, _, E>("up to one ")),
        value(false, tag("a ")),
        value(false, tag("an ")),
        value(false, tag("one ")),
    ))
    .parse(after_verb)
    .ok()?;

    let (after_other, exclude_source) = opt(value((), tag::<_, _, E>("other ")))
        .parse(after_qty)
        .ok()?;
    // CR 115.1c + CR 601.2c: a printed `target` requires targets announced as the
    // ability is activated, not a resolution choice. There is no machinery for a
    // variable per-iterated-player set of announced target slots, so reject the
    // printed-`target` variant here → dispatcher emits `Effect::unimplemented`.
    if tag::<_, _, E>("target ").parse(after_other).is_ok() {
        return None;
    }

    let (control_clause_start, _) = nom_primitives::scan_split_at_phrase(after_other, |i| {
        value((), tag::<_, _, E>("that player controls")).parse(i)
    })?;
    let type_phrase = control_clause_start.trim_end();
    if type_phrase.is_empty() {
        return None;
    }

    let mut filter = super::search::parse_search_filter(type_phrase, ctx);
    if matches!(filter, TargetFilter::Any) {
        return None;
    }
    if exclude_source.is_some() {
        filter = add_filter_prop(filter, FilterProp::Another);
    }
    Some((up_to, filter))
}

/// CR 101.4 + CR 102.2 + CR 608.2c: "For each [other] player, exile [up to one]
/// [target] `<type-phrase>` that player controls" — Kaya, Spirits' Justice's −2.
/// The combined choose+exile per-iterated-player form: each iterated player's
/// controlled permanent matching `<type-phrase>` is chosen (one per player,
/// optionally zero via "up to one"), accumulated into the chain's tracked set,
/// then ALL chosen permanents are exiled (`ChangeZoneAll { TrackedSet }`).
///
/// "for each player" iterates every player (`ZoneOwner::EachPlayer`); "for each
/// other player" excludes the controller (`ZoneOwner::EachOpponent`). Emitted as
/// a `ChooseFromZone { EachPlayer/EachOpponent }` clause with the mass-exile as
/// its `sub_ability`, mirroring how the choose-only cards chain a separate
/// "exile those" sentence.
pub(super) fn parse_for_each_player_exile_controlled(
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ParsedEffectClause> {
    type E<'a> = OracleError<'a>;

    let (after_prefix, iter_scope) = alt((
        value(
            ZoneOwner::EachOpponent,
            tag::<_, _, E>("for each other player, "),
        ),
        value(ZoneOwner::EachOpponent, tag("for each other player ")),
        value(ZoneOwner::EachPlayer, tag("for each player, ")),
        value(ZoneOwner::EachPlayer, tag("for each player ")),
    ))
    .parse(lower)
    .ok()?;

    let (after_verb, _) = tag::<_, _, E>("exile ").parse(after_prefix).ok()?;
    let (up_to, filter) = parse_controlled_battlefield_body(after_verb, ctx)?;

    let choose = Effect::ChooseFromZone {
        count: 1,
        zone: Zone::Battlefield,
        additional_zones: Vec::new(),
        zone_owner: iter_scope,
        filter: Some(filter),
        chooser: Chooser::Controller,
        up_to,
        selection: CardSelectionMode::Chosen,
        constraint: None,
    };
    // CR 400.7 + CR 608.2c: exile EVERY chosen permanent (`ChangeZoneAll` over
    // the tracked set the per-player choose accumulated).
    let exile_all = Effect::ChangeZoneAll {
        origin: Some(Zone::Battlefield),
        destination: Zone::Exile,
        target: TargetFilter::TrackedSet {
            id: crate::types::identifiers::TrackedSetId(0),
        },
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_with_counters: vec![],
        face_down_profile: None,
        library_position: None,
        random_order: false,
    };
    let sub = AbilityDefinition::new(AbilityKind::Spell, exile_all);

    let mut clause = parsed_clause(choose);
    clause.sub_ability = Some(Box::new(sub));
    Some(clause)
}

/// Append a `FilterProp` (deduplicated) to a `Typed` filter. Non-`Typed` filters
/// (`Any`, etc.) are returned unchanged — the callers only inject `Another` onto
/// an already-typed `<type> that player controls` filter. Mirrors the
/// property-injection idiom in `try_parse_choose_owned_by_voter`.
fn add_filter_prop(filter: TargetFilter, prop: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            if !tf.properties.contains(&prop) {
                tf.properties.push(prop);
            }
            TargetFilter::Typed(tf)
        }
        other => other,
    }
}

fn strip_choose_article(input: &str) -> Option<&str> {
    type E<'a> = OracleError<'a>;

    alt((
        preceded(tag::<_, _, E>("a "), rest),
        preceded(tag("an "), rest),
        preceded(tag("one "), rest),
    ))
    .parse(input)
    .map(|(_, stripped)| stripped)
    .ok()
}

fn parse_choose_zone_connector(
    input: &str,
) -> nom::IResult<&str, (ZoneOwner, Vec<Zone>), OracleError<'_>> {
    type E<'a> = OracleError<'a>;

    preceded(
        alt((tag::<_, _, E>("in "), tag("from "))),
        alt((
            map(preceded(tag("your "), parse_choose_zone_list), |zones| {
                (ZoneOwner::Controller, zones)
            }),
            map(
                preceded(tag("that player's "), parse_choose_zone_list),
                |zones| (ZoneOwner::TargetedPlayer, zones),
            ),
            map(
                preceded(tag("target opponent's "), parse_choose_zone_list),
                |zones| (ZoneOwner::TargetedPlayer, zones),
            ),
            map(
                preceded(tag("an opponent's "), parse_choose_zone_list),
                |zones| (ZoneOwner::Opponent, zones),
            ),
        )),
    )
    .parse(input)
}

fn parse_choose_zone_list(input: &str) -> nom::IResult<&str, Vec<Zone>, OracleError<'_>> {
    type E<'a> = OracleError<'a>;

    let (rest, first) = parse_choose_zone(input)?;
    let (rest, second) = opt(preceded(tag::<_, _, E>(" or "), parse_choose_zone)).parse(rest)?;
    let mut zones = vec![first];
    if let Some(second) = second {
        zones.push(second);
    }
    Ok((rest, zones))
}

fn parse_choose_zone(input: &str) -> nom::IResult<&str, Zone, OracleError<'_>> {
    type E<'a> = OracleError<'a>;

    alt((
        value(Zone::Graveyard, tag::<_, _, E>("graveyard")),
        value(Zone::Library, tag("library")),
        value(Zone::Hand, tag("hand")),
        value(Zone::Exile, tag("exile")),
    ))
    .parse(input)
}

/// CR 115.1c + CR 601.2c + CR 608.2c: Detect "target X and target Y" wording
/// after a "Choose " prefix and split it into two independent target slots.
///
/// CR 115.1c: "An activated ability is targeted if it identifies something it
/// will affect by using the phrase 'target [something]' …" — both halves are
/// part of the same activated ability.
///
/// CR 601.2c: "If the spell uses the word 'target' in multiple places, the
/// same object or player can be chosen once for each instance of the word
/// 'target' (as long as it fits the targeting criteria)."
///
/// Strategy: scan the lowercased text for an "and target " (or "and another
/// target ") connector at a word boundary using nom combinators. If present,
/// split the text there and run `parse_target` independently on each side —
/// the prefix becomes slot A's filter, the suffix becomes slot B's. This
/// keeps the second slot intact even when `parse_target` on the prefix
/// stops short of "a player controls" (an unrecognized controller suffix
/// today): the connector is anchored on "and target", not on the precise
/// length of slot A's filter.
///
/// The combinator-based "and target " split also rejects non-target
/// continuations ("target creature and put a counter on it") that would
/// otherwise look like compound targeting.
///
/// Returns `None` when the connector is absent, when either target parses
/// as `TargetFilter::Any` (failed extraction), or when the prefix isn't a
/// targeting phrase (`is_choose_as_targeting`-style check) — caller handles
/// the single-target fallback.
fn try_parse_two_targets(rest: &str, ctx: &mut ParseContext) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    // CR 601.2c connector parser: "and target " or "and another target ".
    // `scan_split_at_phrase` advances at word boundaries (jumping past each
    // space), so the connector body itself is matched without a leading
    // space — the word boundary is enforced by the scan loop. Trailing
    // space is required so the next character is the start of the second
    // target's type/quantity phrase.
    fn parse_connector(input: &str) -> nom::IResult<&str, (), E<'_>> {
        value((), alt((tag("and target "), tag("and another target ")))).parse(input)
    }

    let lower = rest.to_ascii_lowercase();
    let (lower_prefix, lower_match_start) =
        nom_primitives::scan_split_at_phrase(lower.as_str(), parse_connector)?;

    // Map both the prefix and the match-start back to original-case slices
    // so `parse_target` operates on unmodified text. The prefix ends at the
    // word boundary just before "and target …"; the trailing space (if any)
    // is part of the prefix.
    let prefix_orig = &rest[..lower_prefix.len()];
    let match_start_orig = &rest[rest.len() - lower_match_start.len()..];

    // CR 115.1c slot A: the prefix must be a targeting phrase. `parse_target`
    // returning `Any` means "no recognized target" — we refuse to split.
    let (target_a, _rem_a) = parse_target(prefix_orig.trim_end());
    if matches!(target_a, TargetFilter::Any) {
        return None;
    }

    // CR 115.1c slot B: skip the leading "and " on the matched connector
    // and parse the second target. `tag("and ").parse(input)` returns
    // `(remainder, matched)` so we bind the first element.
    let (after_and_orig, _) = tag::<_, _, E>("and ").parse(match_start_orig).ok()?;
    let (target_b, _rem_b) = parse_target(after_and_orig);
    if matches!(target_b, TargetFilter::Any) {
        return None;
    }

    // CR 601.2c + CR 608.2c: Register the two announced slot filters (A then B)
    // on the chain's declared-target-slot registry so later clauses in the same
    // chain can resolve definite anaphors ("that Equipment", "the chosen
    // creature", "the artifact card") to `ParentTargetSlot { index }`. The chunk
    // loop in `parse_effect_chain_ir` threads this across chunks. Assigned (not
    // appended) so a re-parse of the same declaration chunk is idempotent — a
    // classification pass and the real parse share one `chunk_ctx`, and a
    // duplicated Equipment slot would make "that Equipment" ambiguously match
    // two slots. (No current card declares two separate "target … and target …"
    // heads in one chain; the single-declaration form is the whole class.)
    ctx.declared_target_slots = vec![target_a.clone(), target_b.clone()];

    Some(ChooseImperativeAst::TwoTargets { target_a, target_b })
}

/// Parse anaphoric "choose N of them/those [cards]" patterns using nom combinators.
/// Returns (count, chooser) if the pattern matches.
fn parse_choose_anaphoric(lower: &str) -> Option<(u32, Chooser, CardSelectionMode)> {
    type E<'a> = OracleError<'a>;

    // Determine chooser from prefix: "an opponent chooses" / "target opponent chooses" → Opponent,
    // "you choose" / bare "choose" → Controller.
    let (rest, chooser) = alt((
        value(
            Chooser::Opponent,
            alt((
                tag::<_, _, E>("an opponent chooses "),
                tag("target opponent chooses "),
            )),
        ),
        value(
            Chooser::Controller,
            alt((tag::<_, _, E>("you choose "), tag("choose "))),
        ),
    ))
    .parse(lower)
    .ok()?;

    // Optional "up to " prefix.
    let rest = tag::<_, _, E>("up to ")
        .parse(rest)
        .map(|(r, _)| r)
        .unwrap_or(rest);

    // Parse count (one/two/three/N).
    let (rest, count) = nom_primitives::parse_number.parse(rest).ok()?;

    // Must be followed by " of them" or " of those" (optionally with trailing type noun).
    let (after_anaphor, ()) = alt((
        value((), tag::<_, _, E>(" of them")),
        value((), tag(" of those")),
    ))
    .parse(rest)
    .ok()?;

    // CR 608.2d (override) + CR 701.9b (analogous): "choose one of them at
    // random" (River Song's Diary) — the game picks, not the chooser. Scan the
    // remainder after the anaphor for the qualifier.
    let selection = if nom_primitives::scan_contains(after_anaphor, "at random") {
        CardSelectionMode::Random
    } else {
        CardSelectionMode::Chosen
    };

    Some((count, chooser, selection))
}

/// Public entry for Tragic Arrogance-style patterns where the chooser_scope is ControllerForAll.
/// Called from `parse_effect_clause` when "for each player, you choose " prefix is detected.
pub(super) fn parse_category_and_sacrifice_rest_pub(
    rest_lower: &str,
) -> Option<ChooseImperativeAst> {
    parse_single_category_and_sacrifice_rest(rest_lower)
        .or_else(|| parse_category_and_sacrifice_rest(rest_lower))
        .map(|ast| match ast {
            ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                choose_filter,
                sacrifice_filter,
                total_power_cap,
                ..
            } => ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope: CategoryChooserScope::ControllerForAll,
                choose_filter,
                sacrifice_filter,
                total_power_cap,
            },
            other => other,
        })
}

/// CR 608.2d + CR 701.21a: Parse the single-category form specific to the
/// "for each player, you choose" entry point: "a <type> [they/you/that player]
/// control[s]". Keeping this arm outside the generic `choose` parser prevents
/// ordinary target-selection text such as "choose a creature they control" from
/// being misclassified as a choose-and-sacrifice-rest effect.
fn parse_single_category_and_sacrifice_rest(rest_lower: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    let (rest, core_type) = parse_category_item(rest_lower).ok()?;
    alt((
        tag::<_, _, E>(" they control"),
        tag(" you control"),
        tag(" that player controls"),
    ))
    .parse(rest)
    .ok()?;

    let filter = TargetFilter::Typed(TypedFilter::new(
        super::conditions::core_type_to_type_filter(core_type),
    ));
    Some(ChooseImperativeAst::CategoryAndSacrificeRest {
        categories: vec![core_type],
        chooser_scope: CategoryChooserScope::EachPlayerSelf,
        choose_filter: filter.clone(),
        sacrifice_filter: filter,
        total_power_cap: None,
    })
}

/// CR 110.4: The six permanent types — the expansion of the "of each permanent type" idiom.
const PERMANENT_TYPE_CATEGORIES: [CoreType; 6] = [
    CoreType::Artifact,
    CoreType::Battle,
    CoreType::Creature,
    CoreType::Enchantment,
    CoreType::Land,
    CoreType::Planeswalker,
];

/// CR 101.4 + CR 701.21a: Parse the "from among ... an artifact, a creature, ..."
/// or "an artifact, a creature, ... from among ..." pattern after "choose " has been stripped.
///
/// Handles two word orders:
/// 1. "from among the permanents they control an artifact, a creature, ..." (Cataclysm)
/// 2. "an artifact, a creature, ... from among ..." (Cataclysmic Gearhulk)
///
/// Parser structure (nom combinators):
/// - `tag("from among")` detects pattern 1
/// - `parse_category_list_prefix` consumes pattern 2's category list and returns the remainder
/// - Category list: `parse_category_item` composed with comma + "and" separator
fn parse_category_and_sacrifice_rest(rest_lower: &str) -> Option<ChooseImperativeAst> {
    type E<'a> = OracleError<'a>;

    // Pattern 3 (Liliana, Dreadhorde General): generalized "a permanent [they/you/that
    // player] control[s] of each permanent type" — no enumerated category list, no
    // "from among". CR 110.4 + CR 701.21a: expands to the six permanent types; "the
    // rest" are sacrificed. Matching only through "of each permanent type" and
    // discarding the remainder mirrors patterns 1 and 2 (no "sacrifice" token matched).
    if let Ok((rest, _)) = tag::<_, _, E>("a permanent ").parse(rest_lower) {
        let controller = alt((
            tag::<_, _, E>("they control"),
            tag("you control"),
            tag("that player controls"),
        ))
        .parse(rest);
        if let Ok((rest, _)) = controller {
            if tag::<_, _, E>(" of each permanent type")
                .parse(rest)
                .is_ok()
            {
                return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                    categories: PERMANENT_TYPE_CATEGORIES.to_vec(),
                    chooser_scope: CategoryChooserScope::EachPlayerSelf,
                    choose_filter: permanent_filter(),
                    sacrifice_filter: permanent_filter(),
                    total_power_cap: None,
                });
            }
        }
    }

    // Pattern 1: "from among the permanents [they/that player] control[s] an artifact, ..."
    if let Ok((after_from_among, _)) = tag::<_, _, E>("from among ").parse(rest_lower) {
        // Skip past "the permanents they control" / "the permanents that player controls"
        // to find the category list.
        let (categories_text, choose_filter) = parse_choose_domain(after_from_among).ok()?;
        let categories = parse_category_list(categories_text)?;
        return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
            sacrifice_filter: choose_filter.clone(),
            choose_filter,
            total_power_cap: None,
        });
    }

    // Pattern 2: "an artifact, a creature, ... from among [the nonland] permanents they control"
    if let Ok((after_categories, categories)) = parse_category_list_prefix(rest_lower) {
        let (_, choose_filter) = preceded(
            preceded(opt(tag::<_, _, E>(",")), tag(" from among ")),
            parse_choose_domain,
        )
        .parse(after_categories)
        .ok()?;
        return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope: CategoryChooserScope::EachPlayerSelf,
            sacrifice_filter: choose_filter.clone(),
            choose_filter,
            total_power_cap: None,
        });
    }

    // Pattern 4 (Slaughter the Strong): "any number of creatures they control with
    // total power N or less" — CR 107.1c + CR 701.21a. Each chooser keeps a chosen
    // subset of their creatures whose combined power is at most N, then sacrifices
    // all other creatures they control. `categories` is empty in this mode; the
    // total-power cap drives an interactive subset choice in the resolver.
    if let Ok((rest, _)) =
        preceded(opt(tag::<_, _, E>("any number of ")), tag("creatures ")).parse(rest_lower)
    {
        if let Ok((rest, _)) = alt((
            tag::<_, _, E>("they control"),
            tag("you control"),
            tag("that player controls"),
        ))
        .parse(rest)
        {
            if let Ok((rest, cap)) = preceded(
                tag::<_, _, E>(" with total power "),
                nom_primitives::parse_number,
            )
            .parse(rest)
            {
                if tag::<_, _, E>(" or less").parse(rest).is_ok() {
                    let creatures = TargetFilter::Typed(TypedFilter::creature());
                    return Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                        categories: Vec::new(),
                        chooser_scope: CategoryChooserScope::EachPlayerSelf,
                        choose_filter: creatures.clone(),
                        sacrifice_filter: creatures,
                        total_power_cap: Some(QuantityExpr::Fixed { value: cap as i32 }),
                    });
                }
            }
        }
    }

    None
}

fn permanent_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::permanent())
}

fn nonland_permanent_filter() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::permanent().with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
    )
}

/// Parse "the permanents they control" / "the [nonland] permanents that player controls"
/// domains and return the category-list remainder plus the domain filter.
fn parse_choose_domain(input: &str) -> OracleResult<'_, TargetFilter> {
    type E<'a> = OracleError<'a>;

    let (rest, _) = tag::<_, _, E>("the ").parse(input)?;
    let (rest, nonland) = opt(tag::<_, _, E>("nonland ")).parse(rest)?;
    let (rest, _) = tag::<_, _, E>("permanents ").parse(rest)?;
    let (rest, _) = alt((
        tag::<_, _, E>("they control"),
        tag("you control"),
        tag("that player controls"),
    ))
    .parse(rest)?;
    let (rest, _) = opt(alt((tag::<_, _, E>(", "), tag(" ")))).parse(rest)?;
    Ok((
        rest,
        if nonland.is_some() {
            nonland_permanent_filter()
        } else {
            permanent_filter()
        },
    ))
}

/// Parse a comma-separated category list: "an artifact, a creature, an enchantment, and a land"
/// Uses nom combinators for each category item.
fn parse_category_list(input: &str) -> Option<Vec<CoreType>> {
    type E<'a> = OracleError<'a>;

    let (remaining, categories) = parse_category_list_prefix(input).ok()?;
    let remaining = remaining.trim_start();
    if remaining.is_empty()
        || tag::<_, _, E>(", then ").parse(remaining).is_ok()
        || tag::<_, _, E>(". then ").parse(remaining).is_ok()
        || tag::<_, _, E>(".").parse(remaining).is_ok()
    {
        return Some(categories);
    }

    None
}

fn parse_category_list_prefix(input: &str) -> OracleResult<'_, Vec<CoreType>> {
    type E<'a> = OracleError<'a>;

    let (mut remaining, first) = parse_category_item(input)?;
    let mut categories = vec![first];

    while let Ok((after_separator, _)) = alt((
        tag::<_, _, E>(", and "),
        tag(", "),
        tag(" and "),
        tag("and "),
    ))
    .parse(remaining)
    {
        let Ok((after_item, core_type)) = parse_category_item(after_separator) else {
            break;
        };
        categories.push(core_type);
        remaining = after_item;
    }

    Ok((remaining, categories))
}

fn parse_category_item(input: &str) -> OracleResult<'_, CoreType> {
    type E<'a> = OracleError<'a>;

    let (input, _) = alt((tag::<_, _, E>("an "), tag("a "))).parse(input)?;
    parse_core_type_name(input).ok_or_else(|| {
        nom::Err::Error(OracleError::from_error_kind(
            input,
            nom::error::ErrorKind::Alt,
        ))
    })
}

/// Parse a core type name from lowercase text using nom combinators.
fn parse_core_type_name(input: &str) -> Option<(&str, CoreType)> {
    type E<'a> = OracleError<'a>;

    // Ordered longest-first to prevent prefix collisions.
    alt((
        value(CoreType::Planeswalker, tag::<_, _, E>("planeswalker")),
        value(CoreType::Enchantment, tag("enchantment")),
        value(CoreType::Artifact, tag("artifact")),
        value(CoreType::Creature, tag("creature")),
        value(CoreType::Land, tag("land")),
    ))
    .parse(input)
    .ok()
}

pub(super) fn lower_choose_ast(ast: ChooseImperativeAst) -> Effect {
    match ast {
        ChooseImperativeAst::DamageSource { source_filter } => {
            Effect::ChooseDamageSource { source_filter }
        }
        ChooseImperativeAst::TargetOnly { target } => Effect::TargetOnly { target },
        ChooseImperativeAst::Reparse { text } => super::parse_effect(&text),
        ChooseImperativeAst::NamedChoice {
            choice_type,
            selection,
        } => Effect::Choose {
            selection,
            // CR 201.3 / CR 113.6 / CR 205.2a / CR 614.12c: A chosen attribute
            // must persist on the source whenever a later clause refers back to
            // it. CardName choices persist for "with the chosen name" filters
            // (Petrified Hamlet, Cheering Fanatic); CreatureType for "of the
            // chosen type" creature filters; CardType and the restricted
            // card-type Labeled form ("Choose creature or land", Winding Way)
            // for "all cards of the chosen type ..." partitions, which read the
            // chosen card type via `FilterProp::IsChosenCardType`. Persisting a
            // Labeled choice is also what `ChosenLabelIs` companion conditions
            // rely on (CR 614.12c), so it is uniformly safe.
            // CR 608.2d + CR 113.3: A `Keyword` choice ("choose first strike,
            // vigilance, or lifelink") persists so a later "creatures you
            // control gain that ability" clause can read the typed
            // `ChosenAttribute::Keyword` via
            // `ContinuousModification::AddChosenKeyword` at layer evaluation.
            // CR 608.2d: a `DistinctFromSourceHistory` number choice
            // ("...that hasn't been chosen") must persist each committed value on
            // the source so successive resolutions exclude prior picks — an
            // already-chosen number is an illegal option (CR 608.2d). The same
            // persisted value lets the guess answer read the last committed
            // number within that resolution (CR 608.2c; The Toymaker's Trap).
            persist: matches!(
                choice_type,
                ChoiceType::CardName
                    | ChoiceType::CreatureType { .. }
                    | ChoiceType::CardType { .. }
                    | ChoiceType::Labeled { .. }
                    | ChoiceType::Keyword { .. }
                    | ChoiceType::NumberRange {
                        distinctness:
                            crate::types::ability::NumberDistinctness::DistinctFromSourceHistory,
                        ..
                    }
            ),
            choice_type,
        },
        ChooseImperativeAst::RevealHandFilter {
            card_filter,
            choice_optional,
        } => Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter,
            count: None,
            selection: crate::types::ability::CardSelectionMode::Chosen,
            choice_optional,
            reveal: true,
        },
        // CR 608.2d: Anaphoric "choose N of them/those" → select from the tracked set
        // populated by the preceding effect (RevealTop, RevealHand, ExileTop, etc.).
        ChooseImperativeAst::FromTrackedSet {
            count,
            chooser,
            selection,
        } => Effect::ChooseFromZone {
            count,
            zone: Zone::Exile,
            additional_zones: Vec::new(),
            zone_owner: ZoneOwner::Controller,
            filter: None,
            chooser,
            up_to: false,
            selection,
            constraint: None,
        },
        ChooseImperativeAst::FromZone {
            count,
            zones,
            zone_owner,
            filter,
            chooser,
            up_to,
            selection,
        } => {
            let mut zones = zones.into_iter();
            let zone = zones.next().unwrap_or(Zone::Hand);
            Effect::ChooseFromZone {
                count,
                zone,
                additional_zones: zones.collect(),
                zone_owner,
                filter: Some(filter),
                chooser,
                up_to,
                selection,
                constraint: None,
            }
        }
        // CR 101.4 + CR 701.21a: Multi-category permanent selection + sacrifice rest.
        ChooseImperativeAst::CategoryAndSacrificeRest {
            categories,
            chooser_scope,
            choose_filter,
            sacrifice_filter,
            total_power_cap,
        } => Effect::ChooseAndSacrificeRest {
            categories,
            chooser_scope,
            choose_filter,
            sacrifice_filter,
            total_power_cap,
            keeper_constraint: None,
        },
        // CR 115.1c + CR 601.2c: Two independent target slots. The bare-Effect
        // lowering surfaces only the first slot — the chained `TargetOnly`
        // sub_ability for the second slot is attached by
        // `lower_imperative_family_ast`, which can express a `sub_ability`
        // chain (a single `Effect` cannot). Direct callers of
        // `lower_choose_ast` are restricted to single-effect contexts and do
        // not exercise this variant.
        ChooseImperativeAst::TwoTargets { target_a, .. } => Effect::TargetOnly { target: target_a },
        // CR 608.2d + CR 122.1: "choose a counter on it" → interactive
        // counter-kind selection on the anaphoric object.
        ChooseImperativeAst::CounterKind { target } => Effect::ChooseCounterKind { target },
    }
}

pub(super) fn parse_utility_imperative_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<UtilityImperativeAst> {
    // Simple verb dispatch: prevent, regenerate, copy
    if let Some((verb, rest)) = nom_on_lower(text, lower, |input| {
        alt((
            value("prevent", tag("prevent ")),
            value("regenerate", tag("regenerate ")),
            value("copy", tag("copy ")),
        ))
        .parse(input)
    }) {
        return match verb {
            "prevent" => Some(UtilityImperativeAst::Prevent {
                text: text.to_string(),
                // CR 608.2c: capture whether an earlier chain clause selected a
                // target for a trailing anaphor ("that creature"/"it") to bind
                // to. This is the one construction site where `ParseContext` is
                // live; downstream lowering has only the captured flag.
                parent_target_available: ctx.parent_target_available,
            }),
            "regenerate" => Some(UtilityImperativeAst::Regenerate {
                text: text.to_string(),
            }),
            "copy" => {
                let rest_lower = &lower[lower.len() - rest.len()..];
                if tag::<_, _, OracleError<'_>>("that spell or ability")
                    .parse(rest_lower)
                    .is_ok()
                {
                    let consumed = "that spell or ability".len();
                    let rem = &rest[consumed..];
                    let retarget = if super::sequence::recognize_copy_retarget_clause(rem.trim()) {
                        CopyRetargetPermission::MayChooseNewTargets
                    } else {
                        #[cfg(debug_assertions)]
                        assert_no_compound_remainder(rem, text);
                        CopyRetargetPermission::KeepOriginalTargets
                    };
                    return Some(UtilityImperativeAst::Copy {
                        target: TargetFilter::TriggeringSource,
                        retarget,
                    });
                }
                // CR 608.2k: an exile-cost anaphor ("exile a card, then copy the
                // exiled card") disambiguates via `CostPaidObject`, not this
                // exile-producer binding — guard so that path (handled
                // elsewhere) is never shadowed here.
                if ctx.current_ability_exile_cost_zone.is_none() {
                    if let Ok((rem_lower, _)) =
                        tag::<_, _, OracleError<'_>>("the exiled card").parse(rest_lower)
                    {
                        let rem = &rest[rest.len() - rem_lower.len()..];
                        let retarget =
                            if super::sequence::recognize_copy_retarget_clause(rem.trim()) {
                                CopyRetargetPermission::MayChooseNewTargets
                            } else {
                                #[cfg(debug_assertions)]
                                assert_no_compound_remainder(rem, text);
                                CopyRetargetPermission::KeepOriginalTargets
                            };
                        // CR 406.6 + CR 607.2a + CR 707.10: "copy the exiled
                        // card" binds durably via `ExiledBySource` in BOTH
                        // exile-source cases. Cross-resolution (ETB Imprint,
                        // e.g. Isochron Scepter) always did. Same-chain (#5576
                        // Saruman of Many Colors: "exile target … card … Copy
                        // the exiled card") must too: a plain-targeted
                        // `ChangeZone{Exile}` never publishes a chain-local
                        // `TrackedSet`, so the old `TrackedSet{0}` sentinel
                        // resolved to nothing and `copy_source_from_tracked_set`
                        // fell back to a global scan, copying the wrong card.
                        // Binding to `ExiledBySource` makes the exile a linked
                        // consumer (`should_track_exiled_by_source`), publishing
                        // the `ExileLink` that `copy_source_from_exiled_by_source`
                        // reads. The distinct CastFromZone "cast the exiled card"
                        // sites keep their own same-chain bindings.
                        return Some(UtilityImperativeAst::Copy {
                            target: resolve_singular_exiled_card_target(
                                ctx.chain_has_prior_exile_producer,
                                TargetFilter::ExiledBySource,
                            ),
                            retarget,
                        });
                    }
                }
                let (target, _rem) = if let Some((target, rem_lower)) =
                    parse_copy_stack_ability_target(rest_lower)
                {
                    let rem = &rest[rest.len() - rem_lower.len()..];
                    (target, rem)
                } else {
                    // CR 707.10 + CR 701.6a: multi-way stack-object disjunctions
                    // ("instant spell, sorcery spell, activated ability, or
                    // triggered ability") must use `parse_stack_object_target`,
                    // same as the Counter path — bare `parse_target` only keeps
                    // the first spell type (Return the Favor, issue #3996).
                    let stack_phrase = opt(tag::<_, _, OracleError<'_>>("target "))
                        .parse(rest_lower)
                        .map(|(after, _)| after)
                        .unwrap_or(rest_lower);
                    if let Ok((rem_lower, stack_target)) =
                        crate::parser::oracle_nom::target::parse_stack_object_target(stack_phrase)
                    {
                        let rem = &rest[rest.len() - rem_lower.len()..];
                        (stack_target, rem)
                    } else {
                        // CR 707.10 + CR 608.2k: thread ctx so "copy it" routes the
                        // "it" pronoun through resolve_it_pronoun → TriggeringSource
                        // (the triggering spell), matching "copy that spell".
                        // Precondition (resolve_it_pronoun, oracle_effect/mod.rs:165):
                        // this yields TriggeringSource ONLY for trigger subjects that
                        // are non-SelfRef / non-Any (Taigam's subject is Controller —
                        // the "you" arm — so it qualifies). For SelfRef/Any subjects
                        // "it" stays SelfRef/ParentTarget and the CopySpell runtime
                        // fallback (triggering_spell_stack_entry, copy_spell.rs)
                        // keeps them working — behavior-neutral-or-better, never a
                        // regression for non-Taigam "copy it" cards.
                        parse_target_with_ctx(rest, ctx)
                    }
                };
                let retarget = if super::sequence::recognize_copy_retarget_clause(_rem.trim()) {
                    // CR 707.10c: "copy that spell and may choose new targets for the
                    // copy" — same-chunk compound when bare-`and` splitting did not run.
                    CopyRetargetPermission::MayChooseNewTargets
                } else {
                    #[cfg(debug_assertions)]
                    assert_no_compound_remainder(_rem, text);
                    CopyRetargetPermission::KeepOriginalTargets
                };
                Some(UtilityImperativeAst::Copy { target, retarget })
            }
            _ => unreachable!(),
        };
    }
    if let Some((attachment_text, target_text)) = nom_on_lower(text, lower, |input| {
        let (input, _) = tag("unattach all ").parse(input)?;
        let (input, attachment) = terminated(take_until(" from "), tag(" from ")).parse(input)?;
        Ok((input, attachment.to_string()))
    }) {
        let (attachment, attachment_rem) = parse_type_phrase(attachment_text.trim());
        let (target, target_rem) = parse_target_with_ctx(target_text, ctx);
        if attachment_rem.trim().is_empty() && target_rem.trim().is_empty() {
            return Some(UtilityImperativeAst::UnattachAll { attachment, target });
        }
    }
    // CR 701.27 + CR 701.28: "transform" and "convert" are equivalent game actions.
    // CR 608.2k: the bare-pronoun and self-deictic arms ("transform it" /
    // "transform itself" / "transform this creature") split into two anaphor
    // classes:
    //   • Self-deictic ("~"/"this <type>") always binds to the source.
    //   • Bare object pronoun ("it"/"itself") binds via `resolve_it_pronoun`,
    //     which returns `TriggeringSource` when the parse context carries a
    //     non-self trigger subject (Serpent's Soul-Jar pattern, issue #319),
    //     and `SelfRef` otherwise (Primal Amulet self-trigger).
    if matches!(
        lower,
        "transform"
            | "transform ~"
            | "transform this"
            | "transform this creature"
            | "transform this permanent"
            | "transform this artifact"
            | "transform this land"
            | "convert"
            | "convert ~"
            | "convert this"
            | "convert this creature"
            | "convert this permanent"
            | "convert this artifact"
            | "convert this land"
    ) {
        return Some(UtilityImperativeAst::Transform {
            target: TargetFilter::SelfRef,
        });
    }
    if matches!(
        lower,
        "transform it" | "transform itself" | "convert it" | "convert itself"
    ) {
        // CR 608.2k: bare object pronoun resolves via the same dispatch as
        // exile/destroy — typed trigger subject → TriggeringSource (Werewolf
        // packs class), self-ref/any/none → ParentTarget (Primal Amulet's
        // self-trigger preserves source via the empty-targets fallback at
        // CR 608.2c). The diagnostic-only pronoun arg is uniform across the
        // bare object pronoun family per `is_bare_object_pronoun`.
        return Some(UtilityImperativeAst::Transform {
            target: resolve_pronoun_target(ctx, "it"),
        });
    }
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("transform "), tag("convert ")))).parse(input)
    }) {
        // CR 608.2k: thread `ctx` so dynamic targets like "transform that
        // creature" / "transform target permanent" resolve anaphors via the
        // same trigger-subject machinery.
        let (target, _) = parse_target_with_ctx(rest, ctx);
        if !matches!(target, TargetFilter::Any) {
            return Some(UtilityImperativeAst::Transform { target });
        }
    }
    // CR 613.4d: switch power and toughness — two surface forms (sibling branches):
    //   - prepositional: "switch the power and toughness of <target>" (Inversion
    //     Behemoth class — supports the "(each of) any number of target X"
    //     distribution, with multi_target recovered by
    //     `extract_switch_pt_multi_target` in the post-parse fixup. Authorizing
    //     rule for variable-count targeting: CR 115.1d.)
    //   - possessive: "switch <target>'s power and toughness" (single-target
    //     class — Inversion of Fortune, Twiddle's siblings).
    // Try prepositional first so the more specific "the power and toughness of"
    // shape is consumed before the bare "switch <target>" form runs.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("switch the power and toughness of ")).parse(input)
    }) {
        // Strip the optional "each of " and "any number of " distribution
        // prefixes so `parse_target` sees a bare target phrase. The quantifier
        // itself is recovered as a `MultiTargetSpec` in mod.rs via
        // `extract_switch_pt_multi_target` (parallel to the DealDamage / Double
        // counter fixups). Walking the lowercased view in lock-step with the
        // original text preserves casing for `parse_target`.
        let rest_lower = rest.to_ascii_lowercase();
        let mut consumed = 0usize;
        if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("each of ").parse(rest_lower.as_str())
        {
            consumed = rest_lower.len() - after.len();
        }
        let after_each_lower = &rest_lower[consumed..];
        if let Ok((after, _)) =
            tag::<_, _, OracleError<'_>>("any number of ").parse(after_each_lower)
        {
            consumed += after_each_lower.len() - after.len();
        }
        let target_text = &rest[consumed..];
        let (target, rem) = parse_target_with_ctx(target_text, ctx);
        let rem_lower = rem.trim_start().to_ascii_lowercase();
        // The trailing duration ("until end of turn") is stripped upstream by
        // `strip_trailing_duration`; in that case `rem` is empty. Accept either
        // form so the branch also matches when this parser is invoked directly
        // on text that retains the duration (e.g. unit tests).
        let rem_after_duration = tag::<_, _, OracleError<'_>>("until end of turn")
            .parse(rem_lower.as_str())
            .map(|(rest, _)| rest)
            .unwrap_or(rem_lower.as_str());
        let mut terminal = alt((
            value((), eof),
            value((), all_consuming(tag::<_, _, OracleError<'_>>("."))),
        ));
        if terminal.parse(rem_after_duration).is_ok() {
            return Some(UtilityImperativeAst::SwitchPT { target });
        }
    }
    // CR 613.4d + CR 608.2c: pronoun form "switch its power and toughness" —
    // "its" is the source or triggering creature (Valakut Fireboar: this
    // creature on attack; Mangled Soulrager's granted boon: the entering
    // creature), resolved via the shared it-pronoun anaphor. `parse_target`
    // does not treat a bare "its" as a target, so the possessive and
    // prepositional branches above miss it.
    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), tag("switch its power and toughness")).parse(input)
    }) {
        let rem_lower = rest.trim_start().to_ascii_lowercase();
        let rem_after_duration = tag::<_, _, OracleError<'_>>("until end of turn")
            .parse(rem_lower.as_str())
            .map(|(rest, _)| rest)
            .unwrap_or(rem_lower.as_str());
        let mut terminal = alt((
            value((), eof),
            value((), all_consuming(tag::<_, _, OracleError<'_>>("."))),
        ));
        if terminal.parse(rem_after_duration).is_ok() {
            return Some(UtilityImperativeAst::SwitchPT {
                target: resolve_it_pronoun(ctx),
            });
        }
    }

    // CR 613.4d: "switch [target]'s power and toughness"
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("switch ")).parse(input))
    {
        let (target, rem) = parse_target(rest);
        // Consume "'s power and toughness" or " power and toughness" suffix
        let rem_lower = rem.to_lowercase();
        if tag::<_, _, OracleError<'_>>("'s power and toughness")
            .parse(rem_lower.as_str())
            .is_ok()
            || tag::<_, _, OracleError<'_>>(" power and toughness")
                .parse(rem_lower.as_str())
                .is_ok()
        {
            return Some(UtilityImperativeAst::SwitchPT { target });
        }
    }
    // CR 400.7j + CR 608.2h: Zack Fair — "attach an Equipment that was attached
    // to ~ to that creature". The attachment is battlefield Equipment whose
    // host was the ability source (including LKI after self-sacrifice).
    if let Some((multi_target, recipient_text)) = nom_on_lower(text, lower, |input| {
        let (input, _) = tag("attach ").parse(input)?;
        let (input, _) = opt(tag("an ")).parse(input)?;
        let (input, multi_target) = opt(value(
            MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }),
            tag("up to one "),
        ))
        .parse(input)?;
        let (input, _) = tag("equipment that was attached to ").parse(input)?;
        let (input, _) = alt((tag("~"), tag("this equipment"))).parse(input)?;
        let (input, _) = tag(" to ").parse(input)?;
        Ok((input, multi_target))
    }) {
        let (target, _target_rem) = parse_attach_recipient(recipient_text, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_target_rem, text);
        if _target_rem.trim().is_empty() {
            return Some(UtilityImperativeAst::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .properties(vec![FilterProp::AttachedToSource]),
                ),
                target,
                multi_target,
            });
        }
    }
    if let Some(((attachment, target), rem)) = nom_on_lower(text, lower, |input| {
        preceded(tag("attach "), parse_attach_anaphor_to_token).parse(input)
    }) {
        if rem.trim().is_empty() {
            return Some(UtilityImperativeAst::Attach {
                attachment,
                target,
                multi_target: None,
            });
        }
    }
    if let Some(((attachment_text, target_text, multi_target), rem)) =
        nom_on_lower(text, lower, parse_explicit_targeted_attach)
    {
        if rem.trim().is_empty() {
            let (attachment, _attachment_rem) = parse_attachment_anaphor(&attachment_text, ctx);
            let (target, _target_rem) = parse_attach_recipient(&target_text, ctx);
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_attachment_rem, text);
            #[cfg(debug_assertions)]
            assert_no_compound_remainder(_target_rem, text);
            return Some(UtilityImperativeAst::Attach {
                attachment,
                target,
                multi_target,
            });
        }
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("attach ")).parse(input))
    {
        let tp = TextPair::new(text, lower);
        let after_to = tp.strip_after(" to ").map(|tp| tp.original).unwrap_or(rest);
        // CR 608.2k: same anaphor dispatch as the explicit-attach arm above.
        let (target, _rem) = parse_target_with_ctx(after_to, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(UtilityImperativeAst::Attach {
            attachment: TargetFilter::SelfRef,
            target,
            multi_target: None,
        });
    }
    None
}

fn parse_copy_stack_ability_target(input: &str) -> Option<(TargetFilter, &str)> {
    let (input, _) = opt(tag::<_, _, OracleError<'_>>("target "))
        .parse(input)
        .ok()?;
    let (input, _) = alt((
        tag::<_, _, OracleError<'_>>("activated or triggered ability"),
        tag("triggered or activated ability"),
        tag("activated ability"),
        tag("triggered ability"),
    ))
    .parse(input)
    .ok()?;
    let (input, _) = nom::character::complete::multispace0::<_, OracleError<'_>>(input).ok()?;
    if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("you control").parse(input) {
        return Some((
            TargetFilter::StackAbility {
                controller: Some(ControllerRef::You),
                tag: None,
                kind: None,
            },
            rem,
        ));
    }
    if input.is_empty()
        || all_consuming(tag::<_, _, OracleError<'_>>("."))
            .parse(input)
            .is_ok()
    {
        return Some((
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            },
            input,
        ));
    }
    None
}

pub(super) fn stack_ability_filter_from_text(input: &str) -> TargetFilter {
    let controller = if nom_primitives::scan_contains(input, "you control") {
        Some(ControllerRef::You)
    } else if nom_primitives::scan_contains(input, "opponents control")
        || nom_primitives::scan_contains(input, "opponent controls")
        || nom_primitives::scan_contains(input, "opponent's control")
    {
        Some(ControllerRef::Opponent)
    } else {
        None
    };
    TargetFilter::StackAbility {
        controller,
        tag: None,
        kind: None,
    }
}

fn parse_explicit_targeted_attach(
    input: &str,
) -> nom::IResult<&str, (String, String, Option<MultiTargetSpec>), OracleError<'_>> {
    let (input, _) = tag("attach ").parse(input)?;
    let (input, multi_target) = parse_attach_target_quantifier(input)?;
    let (input, attachment) = take_until(" to ").parse(input)?;
    let (input, _) = tag(" to ").parse(input)?;
    let (input, target) = rest.parse(input)?;
    Ok((
        input,
        (attachment.to_string(), target.to_string(), multi_target),
    ))
}

fn parse_attach_target_quantifier(
    input: &str,
) -> nom::IResult<&str, Option<MultiTargetSpec>, OracleError<'_>> {
    let any_number = |input| {
        let (input, _) = tag("any number of ").parse(input)?;
        let (_, _) = peek(alt((
            tag("target "),
            tag("other target "),
            tag("another target "),
        )))
        .parse(input)?;
        Ok((input, MultiTargetSpec::unlimited(0)))
    };
    let up_to = |input| {
        let (input, _) = tag("up to ").parse(input)?;
        let (input, max) = parse_multi_target_count_expr(input)?;
        let (input, _) = space1.parse(input)?;
        let (_, _) = peek(alt((
            tag("target "),
            tag("other target "),
            tag("another target "),
        )))
        .parse(input)?;
        Ok((input, MultiTargetSpec::up_to(max)))
    };
    opt(alt((any_number, up_to))).parse(input)
}

fn parse_attach_recipient<'a>(text: &'a str, ctx: &mut ParseContext) -> (TargetFilter, &'a str) {
    // CR 608.2k: thread `ctx` so "attach this Equipment to it" in trigger
    // bodies binds "it" to the triggering subject (Ancestral Katana —
    // "Whenever a Samurai or Warrior you control attacks alone … attach this
    // Equipment to it"). Pre-existing "her" / "him" → SelfRef carve-out is
    // preserved for legacy attach-to-self phrasings (e.g. "attach to her").
    let (target, rest) = parse_target_with_ctx(text, ctx);
    if matches!(target, TargetFilter::ParentTarget) {
        let trimmed = text.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        if parse_gendered_attach_self_recipient(lower.trim()).is_ok() {
            return (TargetFilter::SelfRef, &trimmed[lower.len()..]);
        }
        if parse_neuter_attach_self_recipient(lower.trim()).is_ok()
            && attach_neuter_recipient_resolves_via_subject(ctx)
        {
            return (resolve_it_pronoun(ctx), &trimmed[lower.len()..]);
        }
    }
    (target, rest)
}

fn attach_neuter_recipient_resolves_via_subject(ctx: &ParseContext) -> bool {
    match &ctx.subject {
        Some(subject) if !matches!(subject, TargetFilter::SelfRef | TargetFilter::Any) => true,
        Some(_) => !ctx.parent_target_is_chosen,
        None => !ctx.parent_target_available,
    }
}

fn parse_gendered_attach_self_recipient(input: &str) -> OracleResult<'_, ()> {
    all_consuming(alt((
        |i| parse_word_bounded(i, "her"),
        |i| parse_word_bounded(i, "him"),
    )))
    .parse(input)
}

fn parse_neuter_attach_self_recipient(input: &str) -> OracleResult<'_, ()> {
    all_consuming(|i| parse_word_bounded(i, "it")).parse(input)
}

/// CR 608.2c + CR 301.5: Resolve an Attach effect's `attachment` argument.
///
/// A bare object pronoun ("Attach **it** to the chosen creature") names the
/// object being attached. In a chain that declared multiple target slots via
/// "Choose target X and target Y", that object is the sole attachable
/// (Equipment/Aura) slot among the declared targets — so bind the pronoun to
/// that `ParentTargetSlot`, giving the Attach a distinct `attachment` and
/// `target` (no slot collision). When there is no unique attachable slot (empty
/// or ambiguous registry), fall back to the normal target parse so every
/// existing attach card is byte-identical.
fn parse_attachment_anaphor<'a>(text: &'a str, ctx: &ParseContext) -> (TargetFilter, &'a str) {
    if is_bare_object_pronoun(text.trim().to_ascii_lowercase().as_str()) {
        if let Some(index) = unique_attachable_slot(&ctx.declared_target_slots) {
            return (
                TargetFilter::ParentTargetSlot { index },
                &text[text.len()..],
            );
        }
    }
    parse_target(text)
}

/// CR 301.5 + CR 303.4: The index of the unique Equipment/Aura slot among the
/// declared target slots, or `None` when there is zero or more than one — the
/// only attachable object a bare "it" attachment can name.
fn unique_attachable_slot(slots: &[TargetFilter]) -> Option<usize> {
    let mut found = None;
    for (index, slot) in slots.iter().enumerate() {
        let TargetFilter::Typed(tf) = slot else {
            continue;
        };
        if tf.get_subtype().is_some_and(|sub| {
            sub.eq_ignore_ascii_case("Equipment") || sub.eq_ignore_ascii_case("Aura")
        }) {
            if found.is_some() {
                return None;
            }
            found = Some(index);
        }
    }
    found
}

fn parse_attach_anaphor_to_token(
    input: &str,
) -> nom::IResult<&str, (TargetFilter, TargetFilter), OracleError<'_>> {
    let (input, attachment) = alt((
        value(
            TargetFilter::TriggeringSource,
            alt((
                tag("it"),
                tag("that aura"),
                tag("that enchantment"),
                tag("that equipment"),
                tag("that permanent"),
            )),
        ),
        value(TargetFilter::SelfRef, tag("~")),
    ))
    .parse(input)?;
    let (input, _) = tag(" to ").parse(input)?;
    let (input, target) = value(
        TargetFilter::LastCreated,
        alt((
            tag("the token created this way"),
            tag("that token created this way"),
            tag("the token"),
            tag("that token"),
        )),
    )
    .parse(input)?;
    Ok((input, (attachment, target)))
}

pub(super) fn lower_utility_imperative_ast(ast: UtilityImperativeAst) -> Effect {
    match ast {
        UtilityImperativeAst::Prevent {
            text,
            parent_target_available,
        } => parse_prevent_effect(&text, parent_target_available),
        UtilityImperativeAst::Regenerate { text } => {
            let lower = text.to_lowercase();
            let rest = tag::<_, _, OracleError<'_>>("regenerate ")
                .parse(&*lower)
                .map(|(r, _)| r)
                .unwrap_or(&lower);
            let (target, _) = parse_target(rest);
            Effect::Regenerate { target }
        }
        UtilityImperativeAst::Copy { target, retarget } => Effect::CopySpell {
            target,
            retarget,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        },
        UtilityImperativeAst::Transform { target } => Effect::Transform { target },
        UtilityImperativeAst::Attach {
            attachment, target, ..
        } => Effect::Attach { attachment, target },
        UtilityImperativeAst::UnattachAll { attachment, target } => {
            Effect::UnattachAll { attachment, target }
        }
        // CR 613.4d: Switch power and toughness.
        UtilityImperativeAst::SwitchPT { target } => Effect::SwitchPT { target },
    }
}

/// CR 615.1a: decompose the prevention amount from the LOCAL parser stream
/// (`rest`, positioned just past "prevent ") with nom combinators. "all but "
/// must be tried before the bare "all " arm because it shares that prefix.
/// Shared by `parse_prevent_effect` and `try_parse_bidirectional_prevent`
/// (issue #1094) so amount detection lives in one authority.
pub(super) fn parse_prevention_amount(rest: &str) -> PreventionAmount {
    if let Ok((after_all_but, _)) = tag::<_, _, OracleError<'_>>("all but ").parse(rest) {
        let n = nom_primitives::parse_number
            .parse(after_all_but)
            .map(|(_, n)| n)
            .unwrap_or(1);
        PreventionAmount::AllBut(n)
    } else if tag::<_, _, OracleError<'_>>("all ").parse(rest).is_ok() {
        PreventionAmount::All
    } else if let Ok((after_next, _)) = tag::<_, _, OracleError<'_>>("the next ").parse(rest) {
        let n = nom_primitives::parse_number
            .parse(after_next)
            .map(|(_, n)| n)
            .unwrap_or(1);
        PreventionAmount::Next(n)
    } else {
        // Fallback: try to extract a number.
        let n = nom_primitives::parse_number
            .parse(rest)
            .map(|(_, n)| n)
            .unwrap_or(1);
        PreventionAmount::Next(n)
    }
}

/// CR 615: Parse "prevent" damage effects into `Effect::PreventDamage`.
///
/// Handles patterns like:
/// - "prevent the next N damage that would be dealt to any target this turn"
/// - "prevent all damage that would be dealt this turn"
/// - "prevent all combat damage that would be dealt this turn"
/// - "prevent the next N damage that would be dealt to target creature"
///
/// CR 608.2c: `parent_target_available` reflects whether an earlier clause in
/// the same effect chain selected a target that a trailing anaphor ("that
/// creature" / "the creature" / "it") can bind to via `ParentTarget`. When
/// false the anaphor recognizer is a no-op and the target falls back to `Any`.
fn parse_prevent_effect(text: &str, parent_target_available: bool) -> Effect {
    let lower = text.to_lowercase();
    let rest = tag::<_, _, OracleError<'_>>("prevent ")
        .parse(&*lower)
        .map(|(r, _)| r)
        .unwrap_or(&lower);

    // Determine scope: combat damage only vs all damage
    let scope = if nom_primitives::scan_contains(rest, "combat damage") {
        PreventionScope::CombatDamage
    } else {
        PreventionScope::AllDamage
    };

    // CR 511.2 + CR 615: the trailing duration window ("this combat" ->
    // UntilEndOfCombat, "this turn" -> UntilEndOfTurn) bounds how long the
    // prevention shield persists. `parse_duration` matches the demonstrative
    // phrase at the END of the clause (target/scope are scanned mid-string),
    // so scan word boundaries for it. Absent -> `None` (legacy end-of-turn
    // prune via `is_shield`).
    let prevention_duration =
        nom_primitives::scan_preceded(rest, crate::parser::oracle_nom::duration::parse_duration)
            .map(|(_, d, _)| d);

    // Determine amount: "all damage" vs "all but N" vs "the next N damage".
    let amount = parse_prevention_amount(rest);

    // CR 609.7 + CR 615.2: prevention scoped to a chosen source (a targeted
    // spell). "Prevent all damage target instant or sorcery spell would deal
    // this turn" (Dromoka's Command) restricts the shield to a SINGLE chosen
    // source object, not a blanket recipient-scoped prevent-all. When this
    // matches, the recipient `target` collapses to `Any` and the source scope
    // is carried as the `damage_source_filter`.
    if let Some(source_filter) = parse_prevent_source_scope(text, &lower) {
        // CR 609.7a: the source is chosen when the effect is created and applies
        // even after the spell leaves the stack. The chosen spell lands in
        // `ability.targets[0]` (the recipient `Any` surfaces no slot), so the
        // shield captures it via `ParentTargetSlot { index: 0 }`.
        return Effect::PreventDamage {
            amount,
            amount_dynamic: None,
            target: TargetFilter::Any,
            scope,
            damage_source_filter: Some(TargetFilter::And {
                filters: vec![TargetFilter::ParentTargetSlot { index: 0 }, source_filter],
            }),
            prevention_duration,
        };
    }

    // Determine target
    let target = if nom_primitives::scan_contains(rest, "any target") {
        TargetFilter::Any
    } else if nom_primitives::scan_contains(rest, "target creature")
        || nom_primitives::scan_contains(rest, "target permanent")
    {
        // Extract the target from the text
        let tp = TextPair::new(text, &lower);
        if let Ok((_, before)) = take_until::<_, _, OracleError<'_>>("target ").parse(tp.lower) {
            let (_, from_target) = tp.split_at(before.len());
            let (t, _) = parse_target(from_target.original);
            t
        } else {
            TargetFilter::Any
        }
    } else if let Some(permanent_type) = parse_compound_you_and_permanents(text, &lower) {
        // CR 615 + CR 614.1a: "to you and [<type>] permanents you control" — a
        // compound player+permanent recipient (Comeuppance's "you and
        // planeswalkers you control"; Channel Harm's "you and permanents you
        // control"). Checked BEFORE the bare "to you" scan (which this phrase
        // also contains) so the permanent leg is not dropped. Lowered to the
        // dedicated `DamageTargetFilter::PlayerOrPermanentsControlledBy` at
        // shield creation — never to a bare `Or` that would leak to `valid_card`.
        TargetFilter::ControllerAndControlledPermanents { permanent_type }
    } else if nom_primitives::scan_contains(rest, "to you")
        || nom_primitives::scan_contains(rest, "to its controller")
    {
        TargetFilter::Controller
    } else if let Some(anaphor_filter) = TextPair::new(text, &lower)
        .strip_after("dealt to ")
        .and_then(|tp| parse_anaphoric_target_ref(tp.original, parent_target_available))
        .map(|(filter, _)| filter)
    {
        // CR 608.2c: "prevent [amount] damage that would be dealt to that
        // creature/it/the creature this turn" — a single-direction anaphor
        // bound to a target an earlier clause selected. Strict superset of the
        // prior `Any` fallback (only reached when `parent_target_available`).
        anaphor_filter
    } else {
        // Default: "that would be dealt" with no specific target → Any
        TargetFilter::Any
    };

    let damage_source_filter = parse_prevent_damage_source_filter(text, &lower)
        .or_else(|| parse_prevent_that_would_deal_source_filter(text, &lower));

    // CR 615.11 + CR 107.3i: `amount_dynamic` (the "prevent X … where X is
    // <quantity>" override) is populated at chunk level by
    // `apply_where_x_effect_expression`, not here — the chunk machinery
    // strips the trailing "where x is …" binding before this parser ever
    // sees it. `amount` is the Next(1) fallback used when no dynamic clause
    // is present (or when the binding fails to resolve to a known quantity).
    Effect::PreventDamage {
        amount,
        amount_dynamic: None,
        target,
        scope,
        damage_source_filter,
        prevention_duration,
    }
}

/// CR 615.2 + CR 109.4: Source-scoped prevent phrasing without a "target"
/// keyword — "prevent all combat damage that other creatures would deal this
/// turn" (Haze Frog). The `<source>` descriptor sits between "that " and
/// " would deal"; route through the shared target grammar so "other"/"another"
/// map to `FilterProp::Another`.
fn parse_prevent_that_would_deal_source_filter(text: &str, lower: &str) -> Option<TargetFilter> {
    let (before_would_deal, _) = split_once_on_lower(text, lower, " would deal")?;
    let before_lower = before_would_deal.to_lowercase();
    let (_, source_descriptor) = split_once_on_lower(before_would_deal, &before_lower, "that ")?;
    let mut ctx = ParseContext::default();
    let (filter, rem) = parse_target_with_ctx(source_descriptor.trim(), &mut ctx);
    let rem = rem.trim().trim_end_matches('.');
    if !rem.is_empty() {
        return None;
    }
    match filter {
        TargetFilter::Any | TargetFilter::Controller | TargetFilter::Player => None,
        other => Some(other),
    }
}

/// CR 615 + CR 614.1a: Recognize the compound damage recipient "you and
/// [`<type>`] permanents you control". Returns the permanent-leg type
/// restriction — `Some(Some(Planeswalker))` for Comeuppance's "you and
/// planeswalkers you control", `Some(None)` for Channel Harm's bare "you and
/// permanents you control", and `None` when the phrase is absent. Nom
/// combinators only — the plural type word is a single `alt()` axis.
fn parse_compound_you_and_permanents(text: &str, lower: &str) -> Option<Option<CoreType>> {
    let region = TextPair::new(text, lower).strip_after("to you and ")?;
    let (_, permanent_type) = you_and_controlled_permanent_type(region.lower).ok()?;
    Some(permanent_type)
}

/// CR 614.1a: "`<plural-type>` you control" tail of the compound recipient. Maps
/// the plural permanent-type word to its `CoreType` restriction; bare
/// "permanents" carries no restriction (`None`).
fn you_and_controlled_permanent_type(input: &str) -> OracleResult<'_, Option<CoreType>> {
    let (input, permanent_type) = alt((
        value(Some(CoreType::Planeswalker), tag("planeswalkers")),
        value(Some(CoreType::Creature), tag("creatures")),
        value(Some(CoreType::Artifact), tag("artifacts")),
        value(Some(CoreType::Enchantment), tag("enchantments")),
        value(Some(CoreType::Land), tag("lands")),
        value(None, tag("permanents")),
    ))
    .parse(input)?;
    let (input, _) = tag(" you control").parse(input)?;
    Ok((input, permanent_type))
}

/// CR 615.1 + CR 609.7b: Optional trailing "by [source-filter]" on
/// prevent clauses. Routes the "by " remainder through the shared replacement-
/// side source-noun grammar (`parse_damage_source_subject_filter`) so the
/// prevent path and the "instead"/no-"instead" replacement paths share ONE
/// source grammar. Covers typed sources (Arachnogenesis: "by non-Spider
/// creatures") and controller-axis sources (Comeuppance: "by sources you don't
/// control"; Channel Harm: "by sources you don't control"). The `_text` slice is
/// unused here — the shared grammar consumes lowercase, matching the replacement
/// side — but is retained for signature parity with the sibling extractors.
fn parse_prevent_damage_source_filter(_text: &str, lower: &str) -> Option<TargetFilter> {
    let (_, (_, after_by)) = nom_primitives::split_once_on(lower, " by ").ok()?;
    let subject = after_by.trim().trim_end_matches('.').trim();
    crate::parser::oracle_replacement::parse_damage_source_subject_filter(subject)
}

/// CR 609.7 + CR 615.2: Detect the source-scoped prevent form — "prevent
/// [all/the next N] damage **target `<source>`** would deal this turn"
/// (Dromoka's Command: "Prevent all damage target instant or sorcery spell
/// would deal this turn").
///
/// This is distinct from the recipient form ("...damage **to** target
/// creature"), which the caller already handles. The disambiguator is the
/// trailing `" would deal"` clause: a source-scoped prevent names what the
/// chosen object *deals*, whereas the recipient form names what is dealt *to*
/// the target. We isolate the region before `" would deal"`, take the source
/// descriptor after `"target "` (both via the `split_once_on_lower` combinator
/// bridge — `take_until`+`tag`, never ad-hoc string dispatch), and run
/// `parse_target` on it. We accept ONLY when the parsed filter resolves to a
/// choosable stack source (a spell on the stack) — `parse_target` is the
/// detector.
///
/// Returns the parsed `<source>` filter (e.g. an `Or`/`And` of stack-spell
/// `Typed` legs) on match, or `None` to fall through to the recipient/by-source
/// logic.
fn parse_prevent_source_scope(text: &str, lower: &str) -> Option<TargetFilter> {
    // Isolate the slice that precedes " would deal" (the source phrase region).
    // `split_once_on_lower` composes `take_until`+`tag` internally and returns
    // original-case slices, so the borrowed remainder escapes cleanly.
    let (before_would_deal, _) = split_once_on_lower(text, lower, " would deal")?;
    // Within that region, the source descriptor follows the "target " keyword.
    let region_lower = before_would_deal.to_lowercase();
    let (_, source_descriptor) = split_once_on_lower(before_would_deal, &region_lower, "target ")?;
    let (source_filter, rem) = parse_target(source_descriptor);
    // The candidate must consume the entire source descriptor and resolve to a
    // choosable spell on the stack — otherwise this is not the source-scoped
    // form (e.g. "target creature" recipient).
    if rem.trim().is_empty() && filter_is_choosable_stack_source(&source_filter) {
        Some(source_filter)
    } else {
        None
    }
}

/// CR 609.7a: A source-scope prevent's chosen source must be a choosable object
/// on the stack — a spell (`StackSpell`, or a `Typed` filter scoped `InZone
/// Stack`), possibly wrapped in `And`/`Or`/`Not` (e.g. "instant or sorcery
/// spell" → `Or` of two stack-spell legs). Mirrors `targeting.rs`'s
/// `filter_targets_stack_spells`, kept local so the parser stays dependency-free
/// of the runtime targeting module.
fn filter_is_choosable_stack_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::StackSpell => true,
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Stack)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_is_choosable_stack_source)
        }
        TargetFilter::Not { filter } => filter_is_choosable_stack_source(filter),
        _ => false,
    }
}

/// CR 113.3 + CR 604.1: Parse "gain `<quoted ability>`" / `"gain "<...>" until
/// end of turn"` in the imperative path. Handles inline ability grants like
/// `gain "When this creature dies, draw a card."` (Rabid Attack class) by
/// delegating to the existing `parse_quoted_ability_modifications` helper —
/// which already routes trigger-prefix quoted text to `GrantTrigger`,
/// keyword-form text to `AddKeyword`, and other ability text to
/// `GrantAbility`.
///
/// Returns `None` when the gain clause contains no quoted text — bare keyword
/// grants are handled by `try_parse_gain_keyword`. Designed as a fallback
/// after the bare-keyword path fails.
fn try_parse_gain_quoted_ability(text: &str) -> Option<Effect> {
    // Cheap pre-check: must contain at least one matched quote pair to be a
    // candidate. Avoids invoking the heavier modification parser on bare
    // keyword text.
    if !text.contains('"') {
        return None;
    }
    let (text_without_duration, duration) = super::strip_trailing_duration(text);
    let modifications = parse_quoted_ability_modifications(text_without_duration);
    if modifications.is_empty() {
        return None;
    }
    // CR 113.3a: Granted abilities last as long as the granting effect. For
    // sub_ability inline grants in pump-style spells the parent's UntilEndOfTurn
    // is the typical default; preserve any explicitly stripped duration.
    let duration = duration.or(Some(Duration::UntilEndOfTurn));
    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .description(text.to_string())],
        duration,
        target: None,
    })
}

/// CR 611.2c + CR 702.10: Coalesce a subjectless continuous body that combines a
/// P/T pump with one or more keyword/ability grants ("get +2/+0 and gain haste
/// until end of turn") into a single `Effect::GenericEffect`, mirroring the
/// subject-bound `build_continuous_clause` so both paths emit the same mods.
///
/// Returns `None` for a pure pump body (no non-P/T modification) so the caller
/// falls through to the existing bare-`Effect::Pump` numeric arm unchanged, and
/// `None` for a keyword-only body (handled by `try_parse_gain_keyword`).
fn coalesce_pump_with_modifications(body_text: &str) -> Option<Effect> {
    let (without_duration, duration) = super::strip_trailing_duration(body_text);
    let modifications = parse_continuous_modifications(without_duration);
    let has_pt = modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )
    });
    let has_non_pt = modifications.iter().any(|m| {
        !matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )
    });
    if !(has_pt && has_non_pt) {
        return None;
    }
    let duration = duration.or(Some(Duration::UntilEndOfTurn));
    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .description(body_text.to_string())],
        duration,
        target: None,
    })
}

/// CR 702: Parse bare "gain [keyword]" / "gain [keyword] until end of turn"
/// in the imperative path. Handles "gain haste", "gain trample and haste",
/// "gain flying until end of turn", etc.
///
/// Reuses `parse_continuous_modifications` which already handles
/// "gain/gains [keyword]" via `extract_keyword_clause`.
fn try_parse_gain_keyword(text: &str) -> Option<Effect> {
    let (text_without_duration, duration) = super::strip_trailing_duration(text);
    let modifications = parse_continuous_modifications(text_without_duration);

    // Only accept if we got at least one AddKeyword or RemoveKeyword modification
    let has_keyword = modifications.iter().any(|m| {
        matches!(
            m,
            ContinuousModification::AddKeyword { .. }
                | ContinuousModification::RemoveKeyword { .. }
        )
    });
    if !has_keyword {
        return None;
    }

    // Default duration: UntilEndOfTurn for keyword granting sub-abilities
    let duration = duration.or(Some(Duration::UntilEndOfTurn));

    Some(Effect::GenericEffect {
        static_abilities: vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(modifications)
            .description(text.to_string())],
        duration,
        target: None,
    })
}

/// CR 113.1a + CR 611.2: Parse "gain[s] all activated abilities of <donor>
/// [until end of turn]" (Quicksilver Elemental, Grell Philosopher) into
/// `Effect::GainActivatedAbilitiesOfTarget`. The fixed lead-in phrase is
/// stripped via `TextPair::strip_prefix`, then the remaining donor noun phrase
/// is delegated entirely to the shared `parse_target` combinator (which already
/// handles "target creature", "target artifact an opponent controls", etc.).
///
/// `recipient` is left at its `SelfRef` default here (Quicksilver's implicit
/// self-recipient); the group-recipient form ("each Horror you control gains
/// …") is wired by the subject-application layer, which rebinds `recipient`
/// before this effect is emitted.
///
/// Returns `None` (so the line falls through to the loud `Unimplemented`
/// fallback) when the lead-in phrase is absent, when no donor target parses, or
/// when unrecognized text remains after the target strip — never swallowing
/// unrecognized text into a half-built effect.
fn try_parse_gain_all_activated_abilities_of_target(text: &str) -> Option<Effect> {
    let (text_without_duration, duration) = super::strip_trailing_duration(text);
    let lower = text_without_duration.to_lowercase();
    let tp = TextPair::new(text_without_duration, &lower);

    // CR 113.1a: fixed lead-in. Compose the two verb forms as a single axis.
    let after_phrase = tp
        // allow-noncombinator: TextPair dual-string fixed lead-in strip (oracle_util convention)
        .strip_prefix("gains all activated abilities of ")
        // allow-noncombinator: TextPair dual-string fixed lead-in strip (oracle_util convention)
        .or_else(|| tp.strip_prefix("gain all activated abilities of "))?;

    // CR 611.2: the donor noun phrase is a normal target — delegate to the
    // shared target combinator. No new target grammar here.
    let donor = after_phrase.original.trim();
    if donor.is_empty() {
        return None;
    }
    let (target, rest) = parse_target(donor);
    // `parse_target` returns the full input as `rest` (and a fallback filter)
    // when it cannot classify the phrase. Any unconsumed trailing text means
    // the donor phrase was not fully recognized — fall through to
    // Unimplemented rather than mis-parse a half-built effect.
    if !rest.trim().is_empty() {
        return None;
    }

    Some(Effect::GainActivatedAbilitiesOfTarget {
        target,
        recipient: TargetFilter::SelfRef,
        // CR 602.1: "all ACTIVATED abilities of" — the default scope.
        scope: GrantedAbilityScope::ActivatedOnly,
        duration: duration.or(Some(Duration::UntilEndOfTurn)),
    })
}

/// CR 602.1 + CR 603.1 + CR 611.2a: Parse "gain[s] this card's other abilities"
/// (Symbiote Spider-Man's Find New Host: "Put a +1/+1 counter on target creature
/// you control. It gains this card's other abilities.") into
/// `Effect::GainActivatedAbilitiesOfTarget` with the `AllOther` scope.
///
/// This is the mirror of `try_parse_gain_all_activated_abilities_of_target`:
/// the donor is `SelfRef` ("this card" — the source), the recipient is
/// `ParentTarget` ("It" — the just-counter'd creature), and the scope is
/// `AllOther` (every ability of the donor except the granting ability). Per CR
/// 611.2a the grant states no duration, so it lasts until the end of the game
/// (`Duration::Permanent`) — NOT the imperative default of until-end-of-turn.
fn try_parse_gain_this_cards_other_abilities(text: &str) -> Option<Effect> {
    let lower = text.to_lowercase();
    // nom: the fixed lead-in, composing the two subject-normalized verb forms.
    let matched = nom_parse_lower(&lower, |input| {
        let (input, _) = alt((tag("gains "), tag("gain "))).parse(input)?;
        value((), tag("this card's other abilities")).parse(input)
    })
    .is_some();
    if !matched {
        return None;
    }

    Some(Effect::GainActivatedAbilitiesOfTarget {
        target: TargetFilter::SelfRef, // donor = "this card" (the source)
        recipient: TargetFilter::ParentTarget, // recipient = "It" (the +1/+1 target)
        scope: GrantedAbilityScope::AllOther,
        duration: Some(Duration::Permanent), // CR 611.2a: no duration stated
    })
}

pub(super) fn lower_imperative_ast(ast: ImperativeAst) -> Effect {
    match ast {
        ImperativeAst::Numeric(ast) => lower_numeric_imperative_ast(ast),
        ImperativeAst::Targeted(ast) => lower_targeted_action_ast(ast),
        ImperativeAst::SearchCreation(ast) => lower_search_and_creation_ast(ast),
        ImperativeAst::HandReveal(ast) => lower_hand_reveal_ast(ast),
        ImperativeAst::Choose(ast) => lower_choose_ast(ast),
        ImperativeAst::Utility(ast) => lower_utility_imperative_ast(ast),
    }
}

pub(super) fn parse_put_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<PutImperativeAst> {
    tag::<_, _, OracleError<'_>>("put ").parse(lower).ok()?;

    if nom_on_lower(text, lower, |input| {
        value(
            (),
            (
                tag("put "),
                tag("that many "),
                tag("cards "),
                tag("from the top of your library "),
                tag("into your hand"),
            ),
        )
        .parse(input)
    })
    .is_some()
    {
        return Some(PutImperativeAst::PutTopCardsIntoHandMatchingExileCount);
    }

    if let Ok((after, _)) = tag::<_, _, OracleError<'_>>("put the top ").parse(lower) {
        if nom_primitives::scan_contains(lower, "graveyard") {
            let count = nom_primitives::parse_number
                .parse(after)
                .map(|(_, n)| n)
                .unwrap_or(1);
            return Some(PutImperativeAst::Mill { count });
        }

        // CR 701.40a + CR 708.2a + CR 110.2a: "put the top N cards of [a player]'s
        // library onto the battlefield face down [under your control]" is the
        // put-clause surface form of manifest (Cybership). It carries a count
        // (top-N), a library-owner binding ("that player's library"), an optional
        // face-down profile seed, and an optional controller override — none of
        // which the generic `try_parse_put_zone_change_parts` ChangeZone path
        // preserves. Intercept it here, before that fallback.
        let (rem, count) = match nom_primitives::parse_number.parse(after) {
            Ok((rem, n)) => (rem, QuantityExpr::Fixed { value: n as i32 }),
            Err(_) => match tag::<_, _, OracleError<'_>>("x").parse(after) {
                Ok((rem, _)) => (
                    rem,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                ),
                Err(_) => (after, QuantityExpr::Fixed { value: 1 }),
            },
        };
        // CR 701.40a: This is the manifest surface form, so the dispatch
        // condition is "<library-owner suffix> onto the battlefield face down".
        // The library-owner suffix must match AND the tail must continue with
        // "onto the battlefield face down" — `face down` is part of the dispatch
        // because a put-onto-battlefield that is NOT face down is a regular
        // ChangeZone, not a manifest. Forms that fail any of these fall through
        // to the non-battlefield ("on top/bottom") and generic ChangeZone paths.
        if let Some((tail, target)) = parse_library_player_suffix(rem.trim_start(), ctx) {
            let battlefield_face_down = (
                tag::<_, _, OracleError<'_>>("onto the battlefield"),
                space1,
                tag("face down"),
            )
                .parse(tail.trim_start())
                .is_ok();
            if battlefield_face_down {
                // CR 110.2a: presence-flag detection of "under your control" —
                // mirrors the accepted Dig presence-flag detection; the dispatch
                // decision itself is the combinators above, not this flag.
                let enters_under = nom_primitives::scan_contains(lower, "under your control")
                    .then_some(ControllerRef::You);
                // CR 708.2a: seed the vanilla 2/2 profile (the manifest default)
                // so a trailing "They're 2/2 Cyberman artifact creatures." spec
                // has a profile to refine in the back-walk patcher.
                let profile = Some(FaceDownProfile::vanilla_2_2());
                return Some(PutImperativeAst::Manifest {
                    target,
                    count,
                    profile,
                    enters_under,
                });
            }
        }
    }

    // CR 401.4 + CR 608.2c: "put the cards {in|from} <possessive> hand on the
    // bottom/top [of <possessive> library] [in any order]" — whole-hand library
    // reposition (Teferi's Puzzle Box). The "the cards {in|from} <possessive>
    // hand" guard keeps single-card "put that card on the bottom" and "put the
    // rest on the bottom" out of this branch (they lack the hand-origin phrase),
    // so those still route to the positional `BottomOfLibrary`/`TopOfLibrary`
    // forms below.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("put ").parse(lower) {
        if let Some(position) = parse_hand_to_library_position(rest) {
            return Some(PutImperativeAst::HandToLibraryPosition { position });
        }
    }

    let has_mass_zone_origin = (nom_primitives::scan_contains(lower, "all")
        || nom_primitives::scan_contains(lower, "each"))
        && nom_primitives::scan_contains(lower, "from");

    // CR 608.2c + CR 401.4: a "put all <filter> revealed this way into <z1> and
    // the rest on the bottom/top of <library> …" partition must reach the
    // tracked-set path (`try_parse_put_zone_change_parts`, called below) — NOT
    // the single-card Bottom/TopOfLibrary reposition. The rest clause supplies
    // the "on the bottom/top of … library" phrase; without this guard it
    // hijacks the whole sentence (The Fourteenth Doctor, Garruk Caller of
    // Beasts, Goblin Ringleader, and the rest of the reveal-partition class).
    // Scoped to the REVEAL-origin anaphor ("revealed this way") plus the "the
    // rest" complement subject so ordinary single-card "put it on the bottom"
    // text is unaffected. The anaphor is deliberately the full "revealed this
    // way" phrase, not the bare "this way": an exile-origin partition whose
    // primary subject is itself a negation over the *cast* set — Muse Vortex's
    // "put the exiled instant and sorcery cards that weren't cast this way into
    // your hand and the rest on the bottom of your library in a random order" —
    // matches "the rest" + "this way" but is NOT the reveal-partition shape this
    // guard means (it has no revealed tracked set and no targeting), so it must
    // fall through to its own positional handling rather than be hijacked here.
    // This is a gating pre-filter, not the classifier — the structural
    // classification is delegated to `try_parse_put_zone_change_parts` (which
    // recognizes the reveal tracked anaphor via `tracked_anaphor_cause`),
    // mirroring the `has_mass_zone_origin` idiom above.
    let is_this_way_partition = nom_primitives::scan_contains(lower, "the rest")
        && nom_primitives::scan_contains(lower, "revealed this way");

    // "put X on top of Y's library" — specific position, no auto-shuffle.
    // Must check before try_parse_put_zone_change which would emit ChangeZone (auto-shuffles).
    // Fixed-count forms with an origin zone ("from your graveyard") remain library
    // reposition effects; mass "all"/"each" forms move a whole source zone.
    if nom_primitives::scan_contains(lower, "on top of")
        && nom_primitives::scan_contains(lower, "library")
        && !has_mass_zone_origin
        && !is_this_way_partition
    {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    // "put that card on top" / "put it on top" / "put them on top" —
    // abbreviated form used after "shuffle" in search-and-put-on-top tutors (41 cards).
    if lower.ends_with("on top") {
        return Some(PutImperativeAst::TopOfLibrary);
    }

    // "put X on the bottom of Y's library" — specific position. Fixed-count
    // origin-zone forms remain library reposition effects; mass "all"/"each"
    // forms move a whole source zone.
    if nom_primitives::scan_contains(lower, "on the bottom of")
        && nom_primitives::scan_contains(lower, "library")
        && !has_mass_zone_origin
        && !is_this_way_partition
    {
        return Some(PutImperativeAst::BottomOfLibrary);
    }

    // "put that card on the bottom" / "put it on the bottom" —
    // abbreviated form without "of Y's library".
    if lower.ends_with("on the bottom") {
        return Some(PutImperativeAst::BottomOfLibrary);
    }

    // CR 401.7 (Unexpectedly Absent): "into its owner's library just beneath
    // the top X cards of that library" — the placed permanent ends with exactly
    // the named number of cards above it. The bare "X" is the spell's announced
    // {X}, resolved at resolution time. The "where X is ..." rebinding forms
    // (Quarry Colossus's Plains count, the die-roll cards) carry their own count
    // source and stay out of scope, so a present "where x is" clause is skipped.
    if take_until::<_, _, OracleError<'_>>("where x is")
        .parse(lower)
        .is_err()
    {
        if let Some(depth) = parse_beneath_top_depth(lower) {
            return Some(PutImperativeAst::BeneathTop { depth });
        }
    }

    // "put X into Y's library Nth from the top" —
    // specific positional placement (God-Eternals, Approach, Bury in Books).
    if let Ok((_, before_from)) = take_until::<_, _, OracleError<'_>>("from the top").parse(lower) {
        {
            // Look backwards from "from the top" to find the ordinal
            let before = before_from.trim_end();
            if let Some(last_space) = before.rfind(' ') {
                let ordinal_word = &before[last_space + 1..];
                if let Some((n, _)) = parse_ordinal(ordinal_word) {
                    return Some(PutImperativeAst::NthFromTop { n });
                }
            }
        }
    }

    if let Some((effect, choice_count)) = super::try_parse_put_zone_change_parts(lower, text, ctx) {
        return match effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                library_position,
                random_order,
                ..
            } => {
                // CR 608.2c: "Put all <filter> revealed this way into <z1> and
                // the rest into <z2>" partitions the tracked (revealed) set. The
                // primary move sends the chosen subset to `destination`; capture
                // the rest zone so the lowering emits the complement move for
                // the cards the producer left behind (Winding Way). Only the
                // tracked-set partition form carries a rest clause — a non-
                // tracked mass move keeps `rest_destination: None`.
                let rest_destination = matches!(
                    target,
                    TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
                )
                .then(|| super::parse_put_rest_destination(lower))
                .flatten();
                // CR 401.4: only meaningful when the rest returns to the
                // library — capture whether it lands on the bottom or top so the
                // complement move suppresses the default shuffle and places at
                // that position (The Fourteenth Doctor / Garruk / Goblin
                // Ringleader class).
                let rest_library_position = (rest_destination == Some(Zone::Library))
                    .then(|| super::parse_put_rest_library_position(lower))
                    .flatten();
                Some(PutImperativeAst::ZoneChangeAll {
                    origin,
                    destination,
                    target,
                    enters_under,
                    enter_tapped: enter_tapped.is_tapped(),
                    library_position,
                    random_order,
                    rest_destination,
                    rest_library_position,
                })
            }
            Effect::ChangeZone {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                enter_transformed,
                enters_attacking,
                up_to,
                enter_with_counters,
                ..
            } => {
                // CR 608.2c + CR 400.7: A bare "put those cards / put them onto
                // the battlefield" anaphor names a tracked set whose members
                // never moved as part of THIS clause, so the clause text gives
                // no origin (`infer_origin_zone` → None). When a producer clause
                // earlier in the chain published the set's source zone
                // (`pending_tracked_set_origin`, e.g. Breach the Multiverse's
                // graveyard choose), bind it here. An impulse/cascade producer
                // leaves the context unset, so the exile default in
                // `lower_put_ast` still governs those reanimations.
                let origin = match (&origin, &target) {
                    (
                        None,
                        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. },
                    ) => ctx.pending_tracked_set_origin.or(origin),
                    _ => origin,
                };
                Some(PutImperativeAst::ZoneChange {
                    origin,
                    destination,
                    target,
                    enters_under,
                    enter_tapped: enter_tapped.is_tapped(),
                    enter_transformed,
                    enters_attacking,
                    up_to,
                    choice_count: choice_count.map(Box::new),
                    enter_with_counters,
                })
            }
            _ => None,
        };
    }

    None
}

/// Extract the depth N from a "...beneath the top N cards..." clause (CR
/// 401.7). The bare "X" form resolves to the spell's announced `{X}` via
/// `QuantityRef::Variable { name: "X" }`; an explicit integer yields a literal.
/// Any other phrasing returns `None` so the clause falls through unchanged.
fn parse_beneath_top_depth(lower: &str) -> Option<QuantityExpr> {
    // Combinator scan to the depth token: "...beneath the top <DEPTH> cards...".
    let (rest, _) = take_until::<_, _, OracleError<'_>>("beneath the top ")
        .parse(lower)
        .ok()?;
    let (_, depth_token) = preceded(
        tag::<_, _, OracleError<'_>>("beneath the top "),
        take_until::<_, _, OracleError<'_>>(" cards"),
    )
    .parse(rest)
    .ok()?;
    // `all_consuming` keeps the bare "X" / literal-integer arms from matching a
    // longer token (e.g. "xenagos") — the whole depth token must be the count.
    all_consuming(alt((
        map(tag::<_, _, OracleError<'_>>("x"), |_| QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }),
        map(nom::character::complete::i32, |value| QuantityExpr::Fixed {
            value,
        }),
    )))
    .parse(depth_token)
    .ok()
    .map(|(_, expr)| expr)
}

pub(super) fn lower_put_ast(ast: PutImperativeAst) -> Effect {
    match ast {
        PutImperativeAst::Mill { count } => Effect::Mill {
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
            // CR 701.17a: "Put top N into graveyard" is self-mill.
            target: TargetFilter::Controller,
            destination: Zone::Graveyard,
        },
        PutImperativeAst::ZoneChangeAll {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
            library_position,
            random_order,
            // CR 608.2c: The "and the rest into <zone>" complement is materialized
            // as a sibling sub-ability by `lower_imperative_family_ast`, which
            // intercepts the partition form before this bare-effect lowering.
            // Here it has already been consumed (or was absent).
            rest_destination: _,
            // CR 401.4: The rest pile's library position belongs to the
            // complement move, which `lower_imperative_family_ast` emits; the
            // bare (non-partition) lowering never carries it.
            rest_library_position: _,
        } => Effect::ChangeZoneAll {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position,
            random_order,
        },
        PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
            enter_transformed,
            enters_attacking,
            up_to,
            choice_count: _,
            enter_with_counters,
        } => {
            // CR 610.3: Mass filters (ExiledBySource, TrackedSet) act on all matching
            // objects without individual targeting — use ChangeZoneAll.
            // ExiledBySource always originates from Exile regardless of inferred zone.
            // CR 122.1: ChangeZoneAll has no counter-stamping channel — those
            // patterns are single-target only in current Oracle text, so the
            // mass-filter branch deliberately drops `enter_with_counters`.
            if matches!(
                target,
                TargetFilter::ExiledBySource | TargetFilter::TrackedSet { .. }
            ) && enter_with_counters.is_empty()
            {
                Effect::ChangeZoneAll {
                    // CR 608.2c + CR 400.7: A tracked-set / impulse mass move
                    // defaults to scanning Exile (cascade, impulse-draw, and the
                    // `ExiledBySource` class all leave their members in exile).
                    // When the producer published a non-exile source zone
                    // (Breach the Multiverse's graveyard choose stamps
                    // `origin: Some(Graveyard)` in `parse_put_ast`), honor it so
                    // the chosen cards are read out of the right zone.
                    origin: origin.or(Some(Zone::Exile)),
                    destination,
                    target,
                    // CR 110.2a: Preserve the parsed entering-controller override
                    // ("put those cards onto the battlefield UNDER YOUR CONTROL"
                    // → Some(You) for Breach the Multiverse). Impulse/cascade
                    // text carries no such phrase, so `enters_under` stays None
                    // for them — identical to the prior hardcoded default.
                    enters_under,
                    enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
                    enter_with_counters: vec![],
                    face_down_profile: None,
                    library_position: None,
                    random_order: false,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination,
                    target,
                    owner_library: false,
                    enter_transformed,
                    enters_under,
                    enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
                    // CR 508.4: Propagated from the inline-tail patcher in
                    // `try_parse_put_zone_change` (Kaalia / Ilharg class).
                    enters_attacking,
                    up_to,
                    enter_with_counters,
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                }
            }
        }
        // Place at a specific position — uses move_to_library_position,
        // not ChangeZone which shuffles the destination library. `count` defaults to
        // `Fixed(1)` here; the cardinality patcher in `oracle_effect/mod.rs`
        // upgrades it (and the target filter) by re-inspecting the imperative
        // text once the clause has been lowered.
        PutImperativeAst::TopOfLibrary => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
        },
        PutImperativeAst::BottomOfLibrary => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Bottom,
        },
        PutImperativeAst::NthFromTop { n } => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::NthFromTop { n },
        },
        PutImperativeAst::BeneathTop { depth } => Effect::PutAtLibraryPosition {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::BeneathTop { depth },
        },
        PutImperativeAst::PutTopCardsIntoHandMatchingExileCount => Effect::Mill {
            count: QuantityExpr::Ref {
                qty: QuantityRef::CardsExiledBySource,
            },
            target: TargetFilter::Controller,
            destination: Zone::Hand,
        },
        // CR 701.40a + CR 708.2a + CR 110.2a: "put the top N cards of [a player]'s
        // library onto the battlefield face down [under your control]" lowers 1:1
        // onto `Effect::Manifest`, preserving the count, the library-owner
        // binding, the face-down profile seed, and the controller override.
        PutImperativeAst::Manifest {
            target,
            count,
            profile,
            enters_under,
        } => Effect::Manifest {
            target,
            count,
            profile,
            enters_under,
        },
        // CR 401.4 + CR 608.2c: "put the cards in their hand on the bottom of
        // their library in any order" (Teferi's Puzzle Box) moves the mover's
        // whole hand to the named library position at once. Mirrors the
        // hand→library field set of `change_zone_all_to_library_effect`, but
        // pins `library_position` and OMITS that helper's trailing `Shuffle`
        // sub-ability — a shuffle would scatter the cards the effect just placed
        // on the bottom/top. `target: Controller` is rebound to the acting
        // player ("that player") by `inject_subject_target` when the enclosing
        // subject-predicate clause carries a player subject.
        PutImperativeAst::HandToLibraryPosition { position } => Effect::ChangeZoneAll {
            origin: Some(Zone::Hand),
            destination: Zone::Library,
            target: TargetFilter::Controller,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: Some(position),
            random_order: false,
        },
    }
}

/// Parse "put that many {type} counter(s) on {target}" — dynamic counter count from event context.
/// CR 120.1: "that many" references the amount from the triggering event (e.g., damage dealt).
/// Produces PutCounter with count=0 as a sentinel for event-context resolution.
fn try_parse_that_many_counters(lower: &str, ctx: &mut ParseContext) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("put that many ")
        .parse(lower)
        .ok()?;
    // Counter type (e.g. "+1/+1", "charge", "loyalty", "double strike").
    // CR 122.1 + CR 122.1b: route through the shared `parse_counter_type_typed`
    // combinator so multi-word keyword counter names ("first strike", "double
    // strike") canonicalize to `CounterType::Keyword(...)` instead of being
    // truncated at the first whitespace.
    let (after_type, counter_type) = nom_primitives::parse_counter_type_typed(rest).ok()?;

    // Skip "counter" or "counters" keyword
    let after_type = after_type.trim_start();
    let after_counter = alt((tag::<_, _, OracleError<'_>>("counters"), tag("counter")))
        .parse(after_type)
        .map(|(r, _)| r)
        .unwrap_or(after_type)
        .trim_start();

    // Parse target after "on"
    let target = if let Ok((on_rest, _)) = tag::<_, _, OracleError<'_>>("on ").parse(after_counter)
    {
        if alt((tag::<_, _, OracleError<'_>>("~"), tag("this ")))
            .parse(on_rest)
            .is_ok()
        {
            TargetFilter::SelfRef
        } else if alt((tag::<_, _, OracleError<'_>>("it"), tag("itself")))
            .parse(on_rest)
            .is_ok()
        {
            // CR 608.2k: Bare pronoun — context-dependent
            resolve_it_pronoun(ctx)
        } else {
            let (t, _) = parse_target(on_rest);
            t
        }
    } else {
        TargetFilter::SelfRef
    };

    // "That many" resolves from trigger event context at runtime.
    Some(Effect::PutCounter {
        counter_type,
        count: QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        target,
    })
}

/// CR 701.60a: Parse the "no longer suspected" un-designation transition into an
/// `Effect::Unsuspect { target }`. The subject precedes the copula + phrase and
/// varies by card:
///   - "all suspected creatures are no longer suspected" (Absolving Lammasu)
///   - "it's no longer suspected" / "it is no longer suspected" (Airtight Alibi)
///   - "they're no longer suspected" / "they are no longer suspected"
///     (Eliminate the Impossible)
///   - "~ is no longer suspected" / "this creature is no longer suspected"
///     (Frantic Scapegoat)
///   - "become no longer suspected" (Deadly Complication, after the upstream
///     "you may have it " causative is stripped — the referent is the prior
///     clause's target)
///
/// Subject → filter mapping: anaphoric pronouns ("it"/"they"/"them") and the
/// empty subject left by the "become" causative bind to `ParentTarget`; the
/// printed-name anaphor ("~"/"this creature") binds to `SelfRef`; any other noun
/// phrase ("all suspected creatures") is parsed by `parse_target`. Mirrors
/// `parse_remove_from_combat_ast`.
fn parse_no_longer_suspected_ast(lower: &str) -> Option<Effect> {
    let input = lower.trim();
    // Each alternative is `copula + " no longer suspected"`; `take_until` finds
    // the subject preceding the first matching tail, and the tail must end the
    // clause (optional trailing period). The copula axis is an ordered list so
    // the longer contraction/word forms are tried before the bare phrase.
    // CR 701.60a: "no longer suspected" is the un-designation transition.
    for tail in [
        "'s no longer suspected",
        "'re no longer suspected",
        " is no longer suspected",
        " are no longer suspected",
        "become no longer suspected",
        " no longer suspected",
        "no longer suspected",
    ] {
        let Ok((after, subject)) = take_until::<_, _, OracleError<'_>>(tail).parse(input) else {
            continue;
        };
        // The tail must terminate the clause (optionally with a period) so a
        // mid-sentence "no longer suspected" fragment doesn't false-match.
        if all_consuming(terminated(
            tag::<_, _, OracleError<'_>>(tail),
            opt(tag(".")),
        ))
        .parse(after)
        .is_err()
        {
            continue;
        }

        let subject = subject.trim();
        // CR 701.60a applies the un-designation to *each* matching permanent, so
        // the subject's shape selects the resolution scope:
        //   - anaphors ("it"/"they"/"them"/causative residue) and the
        //     printed-name anaphor ("~"/"this creature") act on the prior
        //     clause's announced target(s) / the source permanent — `Single`,
        //     resolved via `ability.targets`. (The plural "they"/"them" form
        //     reads every announced object target, so a multi-target antecedent
        //     is already covered. Eliminate the Impossible's "they're no longer
        //     suspected" over a non-targeting `PumpAll` *population* — rather
        //     than announced targets — is a separate population-anaphor +
        //     swallowed-conditional gap, not wired here.)
        //   - an explicit noun phrase ("all suspected creatures" — Absolving
        //     Lammasu) is a non-targeting population filter (`All`), enumerated
        //     over the battlefield at resolution with no announced target.
        let (target, scope) = match subject {
            // Anaphoric / causative pronouns → the prior clause's target(s).
            "it" | "they" | "them" | "" => (TargetFilter::ParentTarget, EffectScope::Single),
            // Printed-name anaphor → the source permanent.
            "~" | "this creature" | "this permanent" => {
                (TargetFilter::SelfRef, EffectScope::Single)
            }
            _ => {
                let (tf, _) = parse_target(subject);
                // `parse_target` returns `Any` for unrecognized phrases; bail
                // rather than emit an over-broad Unsuspect against every object.
                if matches!(tf, TargetFilter::Any) {
                    return None;
                }
                // An explicit noun-phrase subject ("all suspected creatures") is
                // a mass population filter, not an announced target — CR 701.60a
                // removes the designation from every matching permanent.
                (tf, EffectScope::All)
            }
        };
        return Some(Effect::Unsuspect { target, scope });
    }
    None
}

/// CR 506.4: Parse "remove [target] from combat" patterns.
/// Matches: "remove it from combat", "remove ~ from combat",
/// "remove target [creature] from combat", "remove that creature from combat".
fn parse_remove_from_combat_ast(lower: &str, ctx: &mut ParseContext) -> Option<TargetFilter> {
    // Strip the "remove " prefix
    let (rest, _) = tag::<_, _, OracleError<'_>>("remove ").parse(lower).ok()?;
    // Check that "from combat" appears in the remainder
    let from_combat_pos = rest.find("from combat")?;
    let subject = rest[..from_combat_pos].trim();
    // Resolve the subject to a target filter
    let target = match subject {
        "it" | "that creature" | "that land" | "that permanent" => TargetFilter::ParentTarget,
        "" => TargetFilter::SelfRef,
        _ => {
            // Try parsing as a target phrase (e.g., "target attacking creature you control")
            let (tf, _rest) = parse_target(subject);
            if matches!(tf, TargetFilter::Any) && !subject.starts_with("target") {
                // parse_target returns Any when it doesn't recognize the phrase —
                // bail out to avoid false matches.
                return None;
            }
            // structural: not dispatch — mirrors guard above for warning diagnostic
            if matches!(tf, TargetFilter::Any) && subject.starts_with("target") {
                ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
                    context: "'target' prefix but unrecognized filter".into(),
                    text: subject.into(),
                    line_index: 0,
                });
            }
            tf
        }
    };
    Some(target)
}

/// Parse a possessive determiner from a fixed set of MTG Oracle variants.
///
/// Accepts: "your", "their", "its owner's", "that player's". These are the possessives
/// that can precede a zone reference in a "shuffle X into Y" phrase.
fn parse_possessive_determiner(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
    value(
        (),
        alt((
            tag("your"),
            tag("their"),
            tag("its owner's"),
            tag("that player's"),
        )),
    )
    .parse(input)
}

fn parse_shuffle_origin_zones(input: &str) -> nom::IResult<&str, Vec<Zone>, OracleError<'_>> {
    alt((
        value(vec![Zone::Hand, Zone::Graveyard], tag("hand and graveyard")),
        value(vec![Zone::Graveyard, Zone::Hand], tag("graveyard and hand")),
        value(vec![Zone::Hand], tag("hand")),
        value(vec![Zone::Graveyard], tag("graveyard")),
        value(vec![Zone::Exile], tag("exile")),
    ))
    .parse(input)
}

/// Parse "shuffle [the cards {from|in}] {possessive} {zone-list} into
/// {possessive} library" and return the origin zones.
///
/// CR 400.6 + CR 701.24c: Recognizes whole-zone bulk moves like Whirlpool Drake's
/// "shuffle the cards from your hand into your library" and Midnight Clock's
/// "shuffle your hand and graveyard into your library". The origin-zone phrase
/// names every card in each listed zone — no targeting or filtering — so the
/// resulting AST lowers to `ChangeZoneAll` (not `ChangeZone`).
///
/// Supports zones: hand, graveyard, exile. Returns None for any other structure.
fn parse_mass_zones_to_library(lower: &str) -> Option<Vec<Zone>> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("shuffle ").parse(lower).ok()?;
    let (rest, _) = opt(preceded(
        tag::<_, _, OracleError<'_>>("the cards "),
        alt((tag("from "), tag("in "))),
    ))
    .parse(rest)
    .ok()?;
    // "{possessive} "
    let (rest, _) = parse_possessive_determiner(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" ").parse(rest).ok()?;
    let (rest, origins) = parse_shuffle_origin_zones(rest).ok()?;
    // " into {possessive} library"
    let (rest, _) = tag::<_, _, OracleError<'_>>(" into ").parse(rest).ok()?;
    let (rest, _) = parse_possessive_determiner(rest).ok()?;
    let (_rest, _) = tag::<_, _, OracleError<'_>>(" library").parse(rest).ok()?;
    Some(origins)
}

/// Parse "the cards {in|from} {possessive} hand on the bottom/top [of
/// {possessive} library] [in any order]" and return the target library position.
///
/// CR 401.4 + CR 608.2c: The whole-hand reposition of Teferi's Puzzle Box —
/// "that player puts the cards in their hand on the bottom of their library in
/// any order". Modeled on `parse_mass_zones_to_library`'s combinator style; the
/// optional " of {possessive} library" and "in any order" tail is matched to be
/// consumed but the trailing remainder is discarded (the position is the whole
/// payload). The leading "the cards {in|from} {possessive} hand" guard is what
/// keeps single-card "put that card on the bottom" and "put the rest on the
/// bottom" from routing here.
fn parse_hand_to_library_position(rest: &str) -> Option<LibraryPosition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("the cards ")
        .parse(rest)
        .ok()?;
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("in "),
        tag::<_, _, OracleError<'_>>("from "),
    ))
    .parse(rest)
    .ok()?;
    let (rest, _) = parse_possessive_determiner(rest).ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" hand").parse(rest).ok()?;
    let (rest, position) = alt((
        value(
            LibraryPosition::Bottom,
            tag::<_, _, OracleError<'_>>(" on the bottom"),
        ),
        value(LibraryPosition::Top, tag(" on top")),
    ))
    .parse(rest)
    .ok()?;
    // Optional " of {possessive} library" and "in any order" tail — matched to
    // consume when present, but not required. Only "in any order" appears in this
    // class (CR 401.4 owner arrangement); bottom-of-library order is gameplay-inert
    // so it is not modeled as an interactive choice.
    let (rest, _) = opt(preceded(
        tag::<_, _, OracleError<'_>>(" of "),
        (parse_possessive_determiner, tag(" library")),
    ))
    .parse(rest)
    .ok()?;
    let (_rest, _) = opt(tag::<_, _, OracleError<'_>>(" in any order"))
        .parse(rest)
        .ok()?;
    Some(position)
}

pub(super) fn parse_shuffle_ast(text: &str, lower: &str) -> Option<ShuffleImperativeAst> {
    if matches!(
        lower,
        "shuffle" | "shuffles" | "then shuffle" | "then shuffles"
    ) {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    // "shuffle the rest into your library" — the "rest" are already in the library
    // from a preceding dig/reveal effect; this is just a shuffle.
    if nom_primitives::scan_contains(lower, "shuffle the rest")
        || nom_primitives::scan_contains(lower, "shuffle them")
    {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Controller,
        });
    }
    if matches!(lower, "that player shuffles" | "target player shuffles") {
        return Some(ShuffleImperativeAst::ShuffleLibrary {
            target: TargetFilter::Player,
        });
    }
    if tag::<_, _, OracleError<'_>>("shuffle")
        .parse(lower)
        .is_err()
        || !nom_primitives::scan_contains(lower, "library")
    {
        return None;
    }

    // CR 608.2c + CR 701.24a: "shuffle {possessive} library" — exact
    // library-shuffle forms only. Compound forms like "shuffle your graveyard
    // into your library" fall through to the zone-change parser below.
    if let Ok((_, target)) = parse_shuffle_library_target(lower) {
        return Some(ShuffleImperativeAst::ShuffleLibrary { target });
    }
    // CR 701.24c + CR 400.3: "shuffle <pronoun> into <possessive> library" —
    // covers "shuffle it into its owner's library" (Cavalier cycle), "shuffle
    // ~ into its owner's library" (Green Sun's Zenith, Beacon cycle, Nexus of
    // Fate — self-referential tutors that shuffle themselves back), "shuffle
    // that card into its owner's library" (search-then-shuffle tutors),
    // "shuffle them into their owners' libraries" (compound subject). Both
    // pronoun (it/them/that card/those cards/~) and possessive (its owner's /
    // their owner's / their owners' / your) are classified via nom combinators
    // so the lowered `ChangeZone` carries the correct `target` (SelfRef vs
    // ParentTarget) and `owner_library` flag. The `~` token is produced by
    // `normalize_card_name_refs` for self-references; it is handled by
    // `contains_self_or_object_pronoun` here (not `contains_object_pronoun`)
    // because the anaphoric/self-reference distinction matters at other call
    // sites (compound action splitting in `try_split_targeted_compound`).
    if contains_self_or_object_pronoun(lower, "shuffle", "into")
        || contains_self_or_object_pronoun(lower, "shuffles", "into")
    {
        // Pronoun classification. Walk word-boundaries, peel "shuffle"/
        // "shuffles" + " ", then alt() over the five recognized references.
        // "it" / "~" → SelfRef (singular, anaphoric to the source object;
        // "~" is the self-reference token from `normalize_card_name_refs`);
        // "them" / "that card" / "those cards" → ParentTarget (refers to a
        // previously-bound target). The fall-through "SelfRef" arm only
        // engages when the outer `contains_self_or_object_pronoun` guard
        // somehow matched a pronoun the inner combinator didn't recognize —
        // defensive and also matches the existing "shuffle this creature
        // into …" form.
        let target = nom_primitives::scan_at_word_boundaries(lower, |input| {
            let (rest, _) =
                alt((tag::<_, _, OracleError<'_>>("shuffle "), tag("shuffles "))).parse(input)?;
            alt((
                value(TargetFilter::ParentTarget, tag("them")),
                value(TargetFilter::ParentTarget, tag("that card")),
                value(TargetFilter::ParentTarget, tag("those cards")),
                value(TargetFilter::SelfRef, tag("it")),
                value(TargetFilter::SelfRef, tag("~")),
            ))
            .parse(rest)
        })
        .unwrap_or(TargetFilter::SelfRef);
        // Library possessor. CR 400.3 routes the move to the card's *owner*
        // when the Oracle names a possessive that resolves to the owner —
        // "its owner's", "their owner's", "their owners'". Bare "their" /
        // "their library" is intentionally NOT treated as owner-routing:
        // "their" is ambiguous (controller vs owner vs plural antecedent)
        // and would mis-classify "each player shuffles their library".
        // "your library" leaves owner_library: false (the default).
        // TODO(CR 400.3): When `owner_library: true`, the `Shuffle` sub_ability
        // produced by `with_shuffle_sub_ability` still targets `Controller`,
        // so a stolen creature shuffles its current controller's library
        // instead of its owner's. Fixing this requires lifting `Effect::Shuffle`
        // to accept an owner-of-target binding (separate commit).
        let owner_library = nom_primitives::scan_at_word_boundaries(lower, |input| {
            alt((
                value((), tag::<_, _, OracleError<'_>>("its owner's library")),
                value((), tag("their owner's library")),
                value((), tag("their owners' libraries")),
            ))
            .parse(input)
        })
        .is_some();
        return Some(ShuffleImperativeAst::ChangeZoneToLibrary {
            target,
            owner_library,
        });
    }
    // CR 701.24c + CR 400.3: "shuffle <descriptive target> into <possessive>
    // library" — covers "shuffle enchanted creature into its owner's library"
    // (Dramatic Accusation, Stay Hidden Stay Silent) and any future card that
    // names a non-pronoun target phrase before "into ... library". The target
    // is parsed by `parse_target` which handles "enchanted creature",
    // "equipped creature", "target creature", etc.
    // Placed after the pronoun path (which handles it/them/~/that card) so
    // pronoun forms are not accidentally consumed by `parse_target`.
    if let Some(((target_phrase, owner_library), _)) = nom_on_lower(lower, lower, |input| {
        let (input, _) = tag::<_, _, OracleError<'_>>("shuffle ").parse(input)?;
        let (input, target_phrase) = take_until(" into ").parse(input)?;
        not(take_until::<_, _, OracleError<'_>>(" from ")).parse(target_phrase)?;
        let (input, _) = tag(" into ").parse(input)?;
        let (input, owner_library) = alt((
            value(true, tag("its owner's library")),
            value(true, tag("their owner's library")),
            value(true, tag("their owners' libraries")),
        ))
        .parse(input)?;
        Ok((input, (target_phrase.to_string(), owner_library)))
    }) {
        let (target, _) = parse_target(&target_phrase);
        if !matches!(target, TargetFilter::Any) {
            return Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            });
        }
    }
    // CR 701.24c + CR 400.6: "shuffle the cards from {possessive} {zone} into
    // {possessive} library" and "shuffle {possessive} hand and graveyard into
    // {possessive} library" — whole-zone mass move(s) + implicit shuffle.
    // Must run before the generic targeted-shuffle path below, which would otherwise
    // consume "the cards" as a `ParentTarget` pronoun.
    if let Some(origins) = parse_mass_zones_to_library(lower) {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins });
    }
    if contains_possessive(lower, "shuffle", "graveyard") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origins: vec![Zone::Graveyard],
        });
    }
    if contains_possessive(lower, "shuffle", "hand") {
        return Some(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origins: vec![Zone::Hand],
        });
    }
    // CR 701.24c + CR 400.3: "shuffle <descriptive target> into their/your
    // library" — the actor-possessive destination form. A card put into a
    // library always returns to its OWNER's library (CR 400.3), and the target
    // phrase carries the actor scope (e.g. "another creature they own" →
    // `Owned { controller: ScopedPlayer }`), which binds the chosen object to
    // the acting player. Powers the villainous-choice branch "they shuffle
    // another creature they own into their library" (This Is How It Ends).
    //
    // Placed AFTER the whole-zone mass-move paths so that bare zone-possessive
    // moves with an actor-possessive destination ("shuffle your graveyard into
    // your library") are still classified as `ChangeZoneAll`, not as a single
    // descriptive-target shuffle.
    if let Some(((target_phrase, ()), _)) = nom_on_lower(lower, lower, |input| {
        let (input, _) = tag::<_, _, OracleError<'_>>("shuffle ").parse(input)?;
        let (input, target_phrase) = take_until(" into ").parse(input)?;
        not(take_until::<_, _, OracleError<'_>>(" from ")).parse(target_phrase)?;
        let (input, _) = tag(" into ").parse(input)?;
        let (input, ()) = alt((
            value((), tag("their library")),
            value((), tag("your library")),
        ))
        .parse(input)?;
        Ok((input, (target_phrase.to_string(), ())))
    }) {
        let (target, _) = parse_target(&target_phrase);
        // Only accept a real typed object target — never a whole-zone phrase
        // (which the mass-move paths above already handled).
        if matches!(target, TargetFilter::Typed(_)) {
            return Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library: true,
            });
        }
    }
    // CR 701.24c: "shuffle target card from your graveyard into your library" —
    // targeted zone change (origin → library) + implicit shuffle.
    // Placed after possessive checks to avoid matching "shuffle your graveyard into library".
    if let Some((_, after_shuffle)) =
        nom_on_lower(text, lower, |input| value((), tag("shuffle ")).parse(input))
    {
        if nom_primitives::scan_contains(lower, "into")
            && nom_primitives::scan_contains(lower, "library")
            && nom_primitives::scan_contains(lower, "from")
        {
            // CR 400.6: a leading "all "/"each " quantifier ("shuffle all
            // nonland cards from your graveyard into your library", Elixir) is a
            // filtered mass move — every eligible object moves with no choice.
            // Distinguish it from the single-target form ("shuffle target card
            // from your graveyard …") so the lowering picks `ChangeZoneAll` over
            // `ChangeZone`. `parse_target` strips the quantifier, so detect it on
            // the lowercase remainder first.
            let all = nom_on_lower(text, lower, |input| {
                let (input, _) = tag::<_, _, OracleError<'_>>("shuffle ").parse(input)?;
                value((), alt((tag("all "), tag("each ")))).parse(input)
            })
            .is_some();
            // CR 115.1d: "shuffle up to N target cards from <zone> into <library>"
            // (Memory's Journey) carries an "up to N" count on the target slot.
            // Recover it as a `MultiTargetSpec` so the lowering surfaces N slots;
            // `parse_target` itself strips the quantifier and only returns the
            // filter. The mass-move (`all`) form ignores it.
            let (_, multi_target) = super::strip_optional_target_prefix(after_shuffle);
            let (target, _) = parse_target(after_shuffle);
            let origin = if nom_primitives::scan_contains(lower, "graveyard") {
                Some(Zone::Graveyard)
            } else if nom_primitives::scan_contains(lower, "from your hand") {
                Some(Zone::Hand)
            } else if nom_primitives::scan_contains(lower, "from exile") {
                Some(Zone::Exile)
            } else {
                None
            };
            return Some(ShuffleImperativeAst::TargetedChangeZoneToLibrary {
                target,
                origin,
                all,
                multi_target: if all { None } else { multi_target },
            });
        }
    }

    Some(ShuffleImperativeAst::Unimplemented {
        text: text.to_string(),
    })
}

fn parse_shuffle_library_target(
    input: &str,
) -> crate::parser::oracle_nom::error::OracleResult<'_, TargetFilter> {
    let (input, _) = tag("shuffle ").parse(input)?;
    let (input, target) = alt((
        value(TargetFilter::Controller, tag("your")),
        value(
            TargetFilter::Owner,
            alt((tag("his or her owner's"), tag("its owner's"))),
        ),
        value(
            TargetFilter::ParentTarget,
            alt((
                tag("that player's"),
                tag("his or her"),
                tag("their"),
                tag("that"),
            )),
        ),
    ))
    .parse(input)?;
    let (input, _) = tag(" library").parse(input)?;
    let (input, _) = eof(input)?;

    Ok((input, target))
}

/// CR 701.24a: Lower a shuffle AST into a `ParsedEffectClause`.
/// Compound forms ("shuffle X into library") produce a `ChangeZone` + `Shuffle` sub_ability
/// chain so the library is actually randomized after the zone move.
pub(super) fn lower_shuffle_ast(ast: ShuffleImperativeAst) -> ParsedEffectClause {
    match ast {
        ShuffleImperativeAst::ShuffleLibrary { target } => {
            parsed_clause(Effect::Shuffle { target })
        }
        ShuffleImperativeAst::ChangeZoneToLibrary {
            target,
            owner_library,
        } => {
            // CR 701.24c + CR 400.3: `target` and `owner_library` are
            // populated by `parse_shuffle_ast`'s combinator-based pronoun /
            // possessive classification. See the construction site for the
            // detection grammar and the TODO on the `Shuffle` sub-target.
            let effect = Effect::ChangeZone {
                origin: None,
                destination: Zone::Library,
                target,
                owner_library,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            };
            with_shuffle_sub_ability(effect)
        }
        ShuffleImperativeAst::ChangeZoneAllToLibrary { origins } => {
            // CR 400.6 + CR 400.3: "shuffle {possessive} {zone} into {possessive}
            // library" moves every card in the origin zone(s) owned by the
            // identified player. The sentinel `TargetFilter::Controller` is
            // later remapped to a concrete player by `inject_subject_target`
            // when a subject like "that player" precedes the shuffle phrase
            // (Jace's ultimate) — otherwise the resolver treats it as "the
            // ability controller's cards".
            lower_change_zone_all_to_library(origins)
        }
        // CR 701.24a: Targeted zone change to library with implicit shuffle sub_ability.
        ShuffleImperativeAst::TargetedChangeZoneToLibrary {
            target,
            origin,
            all,
            multi_target,
        } => {
            // CR 400.6: "shuffle all <filter> from <zone> into your library"
            // (Elixir) is a mandatory mass move of every eligible object — no
            // interactive "choose up to one" choice. `ChangeZoneAll` carries the
            // typed filter, moves all matching cards at once, and stamps
            // `last_effect_count` (which a chained "equal to the number shuffled
            // this way" quantity reads). The single-target form keeps `ChangeZone`.
            if all {
                let effect = Effect::ChangeZoneAll {
                    origin,
                    destination: Zone::Library,
                    target,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                    library_position: None,
                    random_order: false,
                };
                with_shuffle_sub_ability(effect)
            } else {
                let effect = Effect::ChangeZone {
                    origin,
                    destination: Zone::Library,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                };
                // CR 115.1d: propagate the "up to N target" count so the cast
                // surfaces N target slots (Memory's Journey: up to three).
                let mut clause = with_shuffle_sub_ability(effect);
                clause.multi_target = multi_target;
                clause
            }
        }
        ShuffleImperativeAst::Unimplemented { text } => parsed_clause(Effect::Unimplemented {
            name: "shuffle".to_string(),
            description: Some(text),
        }),
    }
}

/// CR 122.1 + CR 608.2c: Lower a multi-typed counter list to a ParsedEffectClause
/// whose primary effect carries the resolved target and whose `sub_ability`
/// chain re-applies each remaining counter via `TargetFilter::ParentTarget`.
/// Because `ParentTarget` is a context-ref filter (see
/// `TargetFilter::is_context_ref`), the sub-ability chain does not surface
/// additional target-selection slots — the player chooses the target once
/// on the primary effect and every chained `PutCounter` inherits it.
pub(super) fn lower_put_counter_list(
    entries: Vec<(crate::types::counter::CounterType, QuantityExpr)>,
    target: TargetFilter,
    multi_target: Option<MultiTargetSpec>,
) -> ParsedEffectClause {
    let mut iter = entries.into_iter();
    let (first_type, first_count) = iter
        .next()
        .expect("PutCounterList must have at least one entry");

    // Build the sub_ability chain right-to-left so each link owns the next.
    let mut sub_ability: Option<Box<AbilityDefinition>> = None;
    for (counter_type, count) in iter.collect::<Vec<_>>().into_iter().rev() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type,
                count,
                target: TargetFilter::ParentTarget,
            },
        );
        def.sub_ability = sub_ability;
        sub_ability = Some(Box::new(def));
    }

    let mut clause = parsed_clause(Effect::PutCounter {
        counter_type: first_type,
        count: first_count,
        target,
    });
    clause.sub_ability = sub_ability;
    clause.multi_target = multi_target;
    clause
}

/// CR 701.23a + CR 701.23h: Lower a multi-filter library search ("a X card
/// and a Y card [and a Z card ...], put them onto the battlefield [tapped],
/// then shuffle") into a single `SearchLibrary` when each printed filter asks
/// for one card. The prompt exposes the union of legal cards while
/// `SearchSelectionConstraint::MatchEachFilter` requires the submitted set to
/// be assignable to each printed description.
///
/// Non-unit counts, `up to`, or additional group constraints keep the older
/// chained lowering below so those semantics remain explicit until they can be
/// represented as a composed selection constraint.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_multi_filter_search_library(
    primary_filter: TargetFilter,
    count: QuantityExpr,
    reveal: bool,
    target_player: Option<TargetFilter>,
    up_to: bool,
    selection_constraint: SearchSelectionConstraint,
    extra_filters: Vec<TargetFilter>,
    destination: Zone,
    enter_tapped: bool,
) -> ParsedEffectClause {
    if !up_to
        && selection_constraint == SearchSelectionConstraint::None
        && matches!(count, QuantityExpr::Fixed { value: 1 })
    {
        let filters = std::iter::once(primary_filter)
            .chain(extra_filters)
            .collect::<Vec<_>>();
        return parsed_clause(Effect::SearchLibrary {
            filter: TargetFilter::Or {
                filters: filters.clone(),
            },
            count: QuantityExpr::Fixed {
                value: filters.len() as i32,
            },
            reveal,
            target_player,
            selection_constraint: SearchSelectionConstraint::MatchEachFilter { filters },
            split: None,
            source_zones: vec![crate::types::zones::Zone::Library],
        });
    }

    // Build the chain right-to-left so each link owns its successor. The chain
    // ends at a `SearchLibrary` (the last extra filter) so the outer intrinsic
    // continuation can append the terminal `ChangeZone` for that last search.
    let change_zone_effect = || Effect::ChangeZone {
        origin: Some(Zone::Library),
        destination,
        target: TargetFilter::Any,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    };

    // CR 107.1c + CR 701.23d: Wrap the count in `UpTo` once at the helper's
    // entrance — every search in the chain shares the same `up_to` semantic,
    // so the per-arm constructions below all read the wrapped form.
    let chain_count = if up_to {
        QuantityExpr::up_to(count)
    } else {
        count
    };

    let mut tail: Option<Box<AbilityDefinition>> = None;
    for extra_filter in extra_filters.into_iter().rev() {
        // Append `Search(extra)` first (it is the successor of the ChangeZone
        // we will prepend in the next step).
        let mut search_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SearchLibrary {
                filter: extra_filter,
                count: chain_count.clone(),
                reveal,
                target_player: target_player.clone(),
                selection_constraint: selection_constraint.clone(),
                split: None,
                source_zones: vec![crate::types::zones::Zone::Library],
            },
        );
        search_def.sub_ability = tail;
        // Prepend the `ChangeZone` that moves the PREVIOUS search's found card
        // to the destination. This sits between the preceding SearchLibrary
        // (either the primary or a prior extra) and this extra's search.
        let mut change_zone_def = AbilityDefinition::new(AbilityKind::Spell, change_zone_effect());
        change_zone_def.sub_ability = Some(Box::new(search_def));
        tail = Some(Box::new(change_zone_def));
    }

    let mut clause = parsed_clause(Effect::SearchLibrary {
        filter: primary_filter,
        count: chain_count,
        reveal,
        target_player,
        selection_constraint,
        split: None,
        source_zones: vec![crate::types::zones::Zone::Library],
    });
    clause.sub_ability = tail;
    clause
}

pub(super) fn lower_multi_filter_seek(
    primary_filter: TargetFilter,
    count: QuantityExpr,
    from_top: Option<usize>,
    destination: Zone,
    enter_tapped: bool,
    extra_filters: Vec<TargetFilter>,
) -> ParsedEffectClause {
    let mut tail: Option<Box<AbilityDefinition>> = None;
    for extra_filter in extra_filters.into_iter().rev() {
        let mut seek_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Seek {
                filter: extra_filter,
                count: count.clone(),
                from_top,
                destination,
                enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
            },
        );
        seek_def.sub_ability = tail;
        tail = Some(Box::new(seek_def));
    }

    let mut clause = parsed_clause(Effect::Seek {
        filter: primary_filter,
        count,
        from_top,
        destination,
        enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
    });
    clause.sub_ability = tail;
    clause
}

#[allow(clippy::too_many_arguments)]
fn lower_target_referenced_search_library(
    reference_target: TargetFilter,
    filter: TargetFilter,
    count: QuantityExpr,
    reveal: bool,
    target_player: Option<TargetFilter>,
    up_to: bool,
    selection_constraint: SearchSelectionConstraint,
    extra_filters: Vec<TargetFilter>,
    destination: Zone,
    enter_tapped: bool,
) -> ParsedEffectClause {
    let search_clause = if extra_filters.is_empty() {
        parsed_clause(Effect::SearchLibrary {
            filter,
            count: if up_to {
                QuantityExpr::up_to(count)
            } else {
                count
            },
            reveal,
            target_player,
            selection_constraint,
            split: None,
            source_zones: vec![crate::types::zones::Zone::Library],
        })
    } else {
        lower_multi_filter_search_library(
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            extra_filters,
            destination,
            enter_tapped,
        )
    };

    let mut search_def = AbilityDefinition::new(AbilityKind::Spell, search_clause.effect);
    search_def.sub_ability = search_clause.sub_ability;

    let mut clause = parsed_clause(Effect::TargetOnly {
        target: reference_target,
    });
    clause.sub_ability = Some(Box::new(search_def));
    clause
}

/// Wrap an effect with a `Shuffle` sub_ability for compound "X into library" operations.
pub(super) fn with_shuffle_sub_ability(effect: Effect) -> ParsedEffectClause {
    // CR 400.3: When the parent `ChangeZone` routes to the target's owner's
    // library (`owner_library: true`), the implicit shuffle must randomize that
    // same library — not the spell controller's (Chaos Warp stolen-permanent case).
    let shuffle_target = match &effect {
        Effect::ChangeZone {
            owner_library: true,
            ..
        } => TargetFilter::ParentTargetOwner,
        _ => TargetFilter::Controller,
    };
    let shuffle = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: shuffle_target,
        },
    );
    ParsedEffectClause {
        effect,
        duration: None,
        sub_ability: Some(Box::new(shuffle)),
        distribute: None,
        multi_target: None,
        condition: None,
        optional: false,
        unless_pay: None,
    }
}

fn change_zone_all_to_library_effect(origin: Zone) -> Effect {
    Effect::ChangeZoneAll {
        origin: Some(origin),
        destination: Zone::Library,
        target: TargetFilter::Controller,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_with_counters: vec![],
        face_down_profile: None,
        library_position: None,
        random_order: false,
    }
}

fn lower_change_zone_all_to_library(origins: Vec<Zone>) -> ParsedEffectClause {
    let (first, rest) = origins
        .split_first()
        .expect("ChangeZoneAllToLibrary must have at least one origin");
    let first = *first;

    let mut tail: Option<Box<AbilityDefinition>> = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Shuffle {
            target: TargetFilter::Controller,
        },
    )));
    for origin in rest.iter().rev().copied() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            change_zone_all_to_library_effect(origin),
        );
        def.sub_ability = tail;
        tail = Some(Box::new(def));
    }

    let mut clause = parsed_clause(change_zone_all_to_library_effect(first));
    clause.sub_ability = tail;
    clause
}

pub(super) fn parse_destroy_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if nom_on_lower(text, lower, |input| {
        value((), alt((tag("destroy all "), tag("destroy each ")))).parse(input)
    })
    .is_some()
    {
        let (_, rest) = nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))?;
        // CR 608.2k: thread `ctx` so bare "it"/"them" anaphors bind to the
        // triggering subject ("Whenever a creature dies, destroy it" class).
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: true });
    }
    if let Some((_, rest)) =
        nom_on_lower(text, lower, |input| value((), tag("destroy ")).parse(input))
    {
        // CR 608.2k: see comment above — anaphor binding via parse_target_with_ctx.
        let (target, _rem) = parse_target_with_ctx(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        return Some(ZoneCounterImperativeAst::Destroy { target, all: false });
    }
    None
}

/// Detect "target {player,opponent}'s {graveyard,library,hand}" prefixes.
///
/// CR 400.12: A zone-targeting effect operates on every card in the named zone.
/// "target player's" and "target opponent's" are not in the shared `POSSESSIVES`
/// list (those reflect possessive pronouns / determiner phrases for objects);
/// they appear only in *zone-as-operand* contexts like Nihil Spellbomb, Bojuka
/// Bog, Tormod's Crypt, Cremate, Faerie Macabre, etc. — so we recognize them
/// here at the dispatch site rather than widening `POSSESSIVES` globally.
fn starts_with_target_possessive_zone(rest_lower: &str) -> bool {
    fn inner(i: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        preceded(
            alt((tag("target player's "), tag("target opponent's "))),
            alt((tag("graveyard"), tag("library"), tag("hand"))),
        )
        .parse(i)
    }
    inner(rest_lower).is_ok()
}

/// CR 400.12 + CR 115.1: Match a "[card|cards] of [a player]'s library" suffix
/// and return the matched-suffix tail plus the resolved library-owner filter.
///
/// This is the player-binding half of the twelve top-of-library suffix patterns
/// shared by the exile and put-onto-battlefield (manifest) paths. It maps each
/// possessive form to its canonical `TargetFilter`:
/// - "your library" → `Controller`
/// - "that player's" / "their" library → the relative player from `ctx`
///   (`TriggeringPlayer` for DamageDone triggers via `that_player_library_filter`)
/// - "target opponent's library" → `Typed{controller: Opponent}`
/// - "target player's library" → `Player`
/// - "each player's library" → `ScopedPlayer`
///
/// The helper performs only the player-suffix match; callers own any trailing
/// face-down / where-X / destination parsing (the exile and manifest epilogues
/// differ).
pub(super) fn parse_library_player_suffix<'a>(
    remainder: &'a str,
    ctx: &ParseContext,
) -> Option<(&'a str, TargetFilter)> {
    let that_player = that_player_library_filter(ctx);
    let target_opponent_filter =
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
    for (pattern, player) in [
        ("card of your library", TargetFilter::Controller),
        ("cards of your library", TargetFilter::Controller),
        ("card of that player's library", that_player.clone()),
        ("cards of that player's library", that_player.clone()),
        ("card of their library", that_player.clone()),
        ("cards of their library", that_player.clone()),
        (
            "card of target opponent's library",
            target_opponent_filter.clone(),
        ),
        (
            "cards of target opponent's library",
            target_opponent_filter.clone(),
        ),
        ("card of target player's library", TargetFilter::Player),
        ("cards of target player's library", TargetFilter::Player),
        ("card of each player's library", TargetFilter::ScopedPlayer),
        ("cards of each player's library", TargetFilter::ScopedPlayer),
    ] {
        if let Ok((tail, _)) = tag::<_, _, OracleError<'_>>(pattern).parse(remainder) {
            return Some((tail, player));
        }
    }
    None
}

/// CR 112.1: A spell is a card on the stack. `scope_target_spell_phrase` lowers
/// a "spell"/"spells" head noun to a `StackSpell` leg, so detecting a `StackSpell`
/// (or an explicit `InZone(Stack)`) in the PARSED filter is how the "exile all
/// spells" wipe knows to constrain its population to the stack. Recurses through
/// `Or`/`And`/`Not`. Checks the parsed structure, never the raw text — a "spell"
/// buried in a relative clause ("a creature that convoked this spell") must not
/// trigger the constraint, and "cards" (also `TypeFilter::Card`, e.g. "exile all
/// cards from target player's library", Jace) must NOT be treated as a spell.
fn filter_targets_stack(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::StackSpell | TargetFilter::StackAbility { .. } => true,
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack })),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_targets_stack)
        }
        TargetFilter::Not { filter } => filter_targets_stack(filter),
        _ => false,
    }
}

/// CR 700.4 (#5649): match "&lt;quantity&gt; cards from your graveyard" and return the
/// exile count. `"that many"` → the dynamic replacement amount
/// (`QuantityRef::EventContextAmount`, Nefarious Lich); a number word → `Fixed`.
/// Anchored on the possessive "your graveyard" so it never overlaps the
/// top-of-library impulse idiom guarded by `parse_dynamic_count_phrase`.
fn parse_exile_count_from_your_graveyard(lower: &str) -> Option<QuantityExpr> {
    let trimmed = lower.trim().trim_end_matches('.').trim();
    let (rest, qty) = alt((
        value(
            QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::EventContextAmount,
            },
            tag::<_, _, OracleError<'_>>("that many"),
        ),
        map(crate::parser::oracle_nom::primitives::parse_number, |n| {
            QuantityExpr::Fixed { value: n as i32 }
        }),
    ))
    .parse(trimmed)
    .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>(" cards from your graveyard")
        .parse(rest)
        .ok()?;
    rest.trim().is_empty().then_some(qty)
}

/// CR 608.2c: Resolve the count expression of a dynamic-count top-of-library
/// exile ("that many" / "equal to <expr>"). Mirrors the
/// `resolve_exile_top_where_x_binding` chain — the event-context quantity grammar
/// first ("its power/toughness/mana value", "the damage prevented/excess damage
/// dealt this way"), then the general quantity-ref grammar (differently-named
/// token counts, etc.). FAIL-CLOSED: an unbindable expression returns `None` so
/// the caller declines the arm and the clause falls through to the generic path
/// unchanged — it NEVER fabricates a `Fixed { 1 }` default.
fn resolve_dynamic_exile_count(qty_text: &str) -> Option<QuantityExpr> {
    let t = qty_text.trim().trim_end_matches(['.', ',']).trim();
    if t.is_empty() {
        return None;
    }
    if let Some(expr) = crate::parser::oracle_quantity::parse_event_context_quantity(t) {
        return Some(expr);
    }
    crate::parser::oracle_quantity::parse_quantity_ref(t).map(|qty| QuantityExpr::Ref { qty })
}

/// CR 401.1 + CR 115.1: Recognize the "from the top of <owner>'s library" source
/// suffix of a dynamic-count top-of-library exile and map the possessive owner to
/// its library-owner `TargetFilter`.
///
/// R7 zero-touch: this is a self-contained 6-owner table deliberately kept
/// separate from `parse_library_player_suffix` (whose surface is the different
/// "card[s] of <owner>'s library" form). Each entry is matched as the full
/// "<owner> library" phrase, so no shorter alternative can prefix-shadow a longer
/// one. Returns the post-library tail plus the resolved owner filter:
/// - "your library" → `Controller`
/// - "that player's" / "their" library → the relative player from `ctx`
/// - "target opponent's library" → `Typed{controller: Opponent}`
/// - "target player's library" → `Player`
/// - "each player's library" → `ScopedPlayer`
fn parse_from_top_of_library_owner<'a>(
    input: &'a str,
    ctx: &ParseContext,
) -> Option<(&'a str, TargetFilter)> {
    // Requires the "of <owner> library" qualifier. The bare "from the top" form
    // (no "of <owner> library" — Vault 112: Sadistic Simulation's "shuffle your
    // library, then exile that many cards from the top") DECLINES here. That is a
    // SEPARATE, outside-`parse_exile_ast` root cause: the exile clause is lost
    // after the preceding Shuffle in the chain (Magus of the Mind shares this), not
    // a missing owner. Pinned by the `bare_from_the_top_*` tripwire test.
    let (after_prefix, _) = tag::<_, _, OracleError<'_>>("from the top of ")
        .parse(input)
        .ok()?;
    let that_player = that_player_library_filter(ctx);
    for (pattern, player) in [
        ("your library", TargetFilter::Controller),
        ("that player's library", that_player.clone()),
        ("their library", that_player.clone()),
        (
            "target opponent's library",
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        ),
        ("target player's library", TargetFilter::Player),
        ("each player's library", TargetFilter::ScopedPlayer),
    ] {
        if let Ok((tail, _)) = tag::<_, _, OracleError<'_>>(pattern).parse(after_prefix) {
            return Some((tail, player));
        }
    }
    // DEFERRED (honest gap): "its owner's library" (Dead Man's Chest — "exile cards
    // equal to its power from the top of its owner's library") needs an
    // anaphoric-object-owner → player `TargetFilter` that does not exist yet
    // (add-engine-variant Stage-2/3). ROOT CAUSE: the closed owner table above has
    // no "its owner" possessive, so this recognizer declines and Dead Man's Chest
    // stays a count-less `ChangeZone`. Pinned by the `dead_mans_chest_*` tripwire;
    // adding the owner variant flips it to `ExileTop`.
    None
}

/// CR 701.13 + CR 401.1 + CR 608.2c: Recognize the dynamic-count top-of-library
/// exile class — "exile [dynamic count] cards from the top of <owner>'s library"
/// — that the generic `ChangeZone(Library→Exile)` fallback below lowers
/// count-less. Composes two axes with nom combinators (never string dispatch):
///   * count-position — Form A "that many cards …" (`EventContextAmount`);
///     Form B' "cards equal to <expr> from the top of …" (count clause BEFORE the
///     library source — Rakdos, the Muscle); Form B "[a number of ]cards from the
///     top of … equal to <expr>" (count clause AFTER the source).
///   * owner — `parse_from_top_of_library_owner` (6 possessive owners).
///
/// Form B' is tried before Form B because Form B's leading `tag("cards ")` would
/// partially consume Rakdos's "cards equal to …" and then fail the owner match,
/// masking it. Form B' bounds the `<expr>` with `take_until("from the top of ")`
/// so it can never swallow the library source phrase.
///
/// FAIL-CLOSED: an unbindable "equal to <expr>" (or an unrecognized owner)
/// returns `None`, so the caller declines and the clause falls through unchanged
/// — never a `Fixed { 1 }` default. Returns `(post_library_tail, owner, count)`;
/// the tail feeds the shared `strip_exile_top_face_down` epilogue.
fn parse_dynamic_exile_from_top<'a>(
    rest_lower: &'a str,
    ctx: &ParseContext,
) -> Option<(&'a str, TargetFilter, QuantityExpr)> {
    // Form A: "that many cards from the top of <owner>'s library"
    if let Ok((after_noun, _)) = tag::<_, _, OracleError<'_>>("that many cards ").parse(rest_lower)
    {
        let (after_lib, player) = parse_from_top_of_library_owner(after_noun, ctx)?;
        return Some((
            after_lib,
            player,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
        ));
    }

    // Form B': "cards equal to <expr> from the top of <owner>'s library"
    if let Ok((after_eq, _)) = tag::<_, _, OracleError<'_>>("cards equal to ").parse(rest_lower) {
        if let Ok((before_source, expr_text)) =
            take_until::<_, _, OracleError<'_>>("from the top of ").parse(after_eq)
        {
            let count = resolve_dynamic_exile_count(expr_text)?;
            let (after_lib, player) = parse_from_top_of_library_owner(before_source, ctx)?;
            return Some((after_lib, player, count));
        }
    }

    // Form B: "[a number of ]cards from the top of <owner>'s library equal to <expr>"
    if let Ok((after_noun, _)) = (
        opt(tag::<_, _, OracleError<'_>>("a number of ")),
        tag::<_, _, OracleError<'_>>("cards "),
    )
        .parse(rest_lower)
    {
        if let Some((after_lib, player)) = parse_from_top_of_library_owner(after_noun, ctx) {
            if let Ok((expr_text, _)) = tag::<_, _, OracleError<'_>>(" equal to ").parse(after_lib)
            {
                let count = resolve_dynamic_exile_count(expr_text)?;
                // The <expr> runs to the clause end; no post-library tail remains
                // for the face-down epilogue (no Form B card is exiled face down).
                return Some(("", player, count));
            }
        }
    }

    None
}

pub(super) fn parse_exile_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("exile the top ").parse(lower) {
        let (initial_count, remainder) =
            if let Ok((rem, n)) = nom_primitives::parse_number.parse(rest) {
                (QuantityExpr::Fixed { value: n as i32 }, rem.trim_start())
            } else if let Ok((rem, _)) = tag::<_, _, OracleError<'_>>("x").parse(rest) {
                (
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    rem.trim_start(),
                )
            } else {
                (QuantityExpr::Fixed { value: 1 }, rest)
            };
        // CR 608.2 + CR 400.12 + CR 115.1: Match the "[card|cards] of [a player]'s
        // library" suffix and resolve the library owner via the shared
        // `parse_library_player_suffix` helper (also used by the manifest
        // put-onto-battlefield path). It maps "your" → Controller, "that
        // player's"/"their" → the relative player (`TriggeringPlayer` for
        // DamageDone), "target opponent's" → Typed{Opponent}, "target player's"
        // → Player, "each player's" → ScopedPlayer.
        if let Some((after_lib, player)) = parse_library_player_suffix(remainder, ctx) {
            // CR 406.3: Detect the "face down" suffix Oracle text uses to
            // mark hidden-information exiles (Necropotence / Bomat Courier
            // / Asmodeus the Archfiend / Knowledge Vault class). The
            // resolver propagates this to the moved object's `face_down`
            // flag so `visibility.rs` redacts the card for non-owners.
            let (after_lib, face_down) = strip_exile_top_face_down(after_lib);
            // CR 107.3i: Optional ", where x is <quantity expr>" suffix
            // overrides the leading `Variable { "X" }` binding with the
            // dynamic quantity expression. Mirrors the
            // try_parse_token_enters_with_counters / put-counters-on-token
            // followup patterns. Without this, the trigger has no chosen X
            // (it's an ETB-triggered ability, not a cast), and the count
            // would default to 0 at resolution time.
            let count = resolve_exile_top_where_x_binding(after_lib, initial_count);
            return Some(ZoneCounterImperativeAst::ExileTop {
                player,
                count,
                face_down,
            });
        }

        // CR 701.13: Exile — "exile the top card[s]" with NO "of <player>'s
        // library" qualifier is an implicit-controller top-of-library exile
        // (Urza, Lord High Artificer's {5}; Bloodsoaked Insight; the
        // "shuffle ..., then exile the top card" class). The twelve qualified
        // suffix patterns above run FIRST so a real "of <player>'s library"
        // continuation is never shadowed by this fallback.
        // CR 701.24: Shuffle — after a shuffle the library order is RANDOM, so
        // the exiled top card is determined by post-shuffle order; the player
        // makes NO selection. This deterministic top-of-library ExileTop is the
        // rules-correct shape — NOT a library-wide ChangeZone (which would offer
        // a tutor prompt). The ABSENCE of a selection prompt is correct.
        let head = remainder.trim_start();
        if let Ok((after_noun, _)) = alt((
            tag::<_, _, OracleError<'_>>("cards"),
            tag::<_, _, OracleError<'_>>("card"),
        ))
        .parse(head)
        {
            // Require this is a clause boundary, NOT a qualified continuation
            // (" of <player>'s library", already handled above). `not(tag(" of "))`
            // rejects the qualified form on the raw tail; the boundary check
            // accepts EOF or a sentence/clause terminator (after an optional
            // single separator space).
            let not_qualified = not(tag::<_, _, OracleError<'_>>(" of "))
                .parse(after_noun)
                .is_ok();
            let boundary = after_noun.strip_prefix(' ').unwrap_or(after_noun); // allow-noncombinator: trim one optional separator before nom boundary peek
            let at_boundary = alt((
                value((), eof::<_, OracleError<'_>>),
                value((), peek(one_of(".,"))),
            ))
            .parse(boundary)
            .is_ok();
            if not_qualified && at_boundary {
                // CR 406.3: honor a trailing "face down" suffix exactly as the
                // twelve qualified patterns do.
                let (after_lib, face_down) = strip_exile_top_face_down(after_noun);
                // CR 107.3c: honor a trailing ", where x is <expr>" binding —
                // the text defines X, so the controller doesn't choose it.
                let count = resolve_exile_top_where_x_binding(after_lib, initial_count);
                return Some(ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count,
                    face_down,
                });
            }
        }
    }

    if let Some((_, rest)) = nom_on_lower(text, lower, |input| {
        value((), alt((tag("exile all "), tag("exile each ")))).parse(input)
    }) {
        let rest_lower = &lower[lower.len() - rest.len()..];
        // CR 400.3 + CR 404.1 + CR 406.2 + CR 108.2: "exile all cards from <possessive> <zone>
        // and <zone>" (Identity Crisis) — a *union* of origin zones. Recognized
        // before the generic single-origin path below, whose `infer_origin_zone`
        // captures only the first zone and (via `try_split_targeted_compound`)
        // orphans the trailing " and <zone>" as an unsupported child clause. The
        // zone union rides on the target filter via `InAnyZone`; `origin: None`
        // defers to it, matching the `MultiZoneSameNameExile` lowering.
        if let Some((owner, zones)) = try_parse_multi_zone_player_exile(rest_lower) {
            return Some(ZoneCounterImperativeAst::Exile {
                origin: None,
                target: TargetFilter::Typed(
                    TypedFilter::default()
                        .controller(owner)
                        .properties(vec![crate::types::ability::FilterProp::InAnyZone { zones }]),
                ),
                all: true,
                enter_with_counters: vec![],
                multi_target: None,
            });
        }
        // CR 205.2a + CR 205.3a + CR 608.2c: parse the full target as a
        // multi-type union so "exile all A except <X>, all B, and all C" lowers to
        // one `ChangeZoneAll { Or[…] }` instead of fragmenting the trailing
        // "all B"/"all C" legs into verb-less sub-abilities (Everything Comes to
        // Dust). Union legs span card types (205.2a) and subtypes (205.3a).
        let (parsed_target, _rem) = parse_mass_type_union(rest, ctx);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        // CR 109.2b + CR 701.13a: "exile all spells" must scope to the stack, but
        // the bare word "spell" can appear deep inside a relative clause ("a
        // creature that convoked this spell", Everything Comes to Dust) where it
        // must NOT force a stack constraint. The parser is the detector: only
        // constrain when the PARSED filter actually references a stack object,
        // never a substring scan over the raw text.
        let target = if filter_targets_stack(&parsed_target) {
            super::constrain_filter_to_stack(parsed_target)
        } else {
            parsed_target
        };
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
            enter_with_counters: vec![],
            multi_target: None,
        });
    }

    let (_, rest_text) = nom_on_lower(text, lower, |input| value((), tag("exile ")).parse(input))?;
    let rest_lower = &lower[lower.len() - rest_text.len()..];

    // CR 701.13a: "exile a card from the top of your library" — synonymous with
    // "exile the top card of your library". Without ExileTop lowering the generic
    // path emits ChangeZone(Library→Exile) with a library-wide EffectZoneChoice
    // prompt (Wall of Mourning, issue #2397).
    if let Ok((after_lib, _)) = (
        opt(nom_primitives::parse_article),
        tag("card from the top of your library"),
    )
        .parse(rest_lower)
    {
        let (after_lib, face_down) = strip_exile_top_face_down(after_lib);
        let count = parse_exile_card_from_top_for_each_suffix(after_lib);
        return Some(ZoneCounterImperativeAst::ExileTop {
            player: TargetFilter::Controller,
            count,
            face_down,
        });
    }

    // CR 701.13 + CR 401.1: "exile [dynamic count] cards from the top of
    // <owner>'s library" — a plural top-of-library exile whose count the generic
    // ChangeZone(Library→Exile) fallback below drops entirely. The count is
    // defined by the ability: "that many" = the triggering-event amount
    // (EventContextAmount); "equal to <expr>" resolves through the shared
    // quantity combinators (its power / mana value, excess or prevented damage,
    // differently-named token count).
    // CR 608.2c: a trailing "equal to <expr>" is later text defining the earlier
    // "exile cards" count (Archaic's Agony / End-Blaze Epiphany / Neriv / Bone
    // Mask); Rakdos, the Muscle places the "equal to <expr>" BEFORE the source.
    // Ordered after the singular path (b) above — those match "a card from the
    // top" (singular); this arm requires the plural "cards" of the dynamic class,
    // so it never shadows (a)/(b) and declines (returns None) on anything that is
    // not exactly "[dynamic count] cards from the top of <owner>'s library".
    //
    // DEFERRED (honest gap): this arm fixes only the exile COUNT and library
    // OWNER. Rakdos, the Muscle's trailing "mana of any type can be spent to cast
    // those spells" rider is a SEPARATE `PlayFromExile.mana_spend_permission` grant
    // concern the exile parser never touches — it remains dropped
    // (`mana_spend_permission: None`), not upgraded to a false-supported `Some(..)`.
    // The `rakdos_*` runtime drive asserts that drop stays fail-closed.
    if let Some((after_lib, player, count)) = parse_dynamic_exile_from_top(rest_lower, ctx) {
        // CR 406.3: honor a trailing "face down" suffix (Flamewar) exactly as the
        // qualified top-of-library patterns above.
        let (_after_fd, face_down) = strip_exile_top_face_down(after_lib);
        return Some(ZoneCounterImperativeAst::ExileTop {
            player,
            count,
            face_down,
        });
    }

    // CR 400.12: "exile their graveyard" / "exile target player's graveyard"
    // act on all cards in that zone. Bare possessive zone references and
    // "target {player,opponent}'s <zone>" share semantics with "exile all/each".
    // CR 404 (graveyard) / CR 406 (exile) — the zone itself is the operand.
    let mass_zone = starts_with_possessive(rest_lower, "", "graveyard")
        || starts_with_possessive(rest_lower, "", "library")
        || starts_with_possessive(rest_lower, "", "hand")
        || starts_with_target_possessive_zone(rest_lower);
    if mass_zone {
        let (target, _rem) = parse_target(rest_text);
        #[cfg(debug_assertions)]
        assert_no_compound_remainder(_rem, text);
        let origin = super::infer_origin_zone(rest_lower);
        return Some(ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all: true,
            enter_with_counters: vec![],
            multi_target: None,
        });
    }

    // "exile <filter> from your hand [with <counter suffix>]"
    // Mirror the original arm's `terminated(take_until, tag)`; the ONLY change
    // vs. the original is dropping the trailing `eof` from the terminator so a
    // post-hand tail is allowed. `terminated` returns the `take_until` slice
    // (the card filter) and consumes " from your hand"; the parser remainder
    // (`after_hand`) is the post-hand tail.
    if let Ok((after_hand, (_, filter_text))) = (
        opt(nom_primitives::parse_article),
        terminated(take_until(" from your hand"), tag(" from your hand")),
    )
        .parse(rest_lower)
    {
        let (mut target, rem) = parse_type_phrase(filter_text.trim());
        // `after_hand` is the parser remainder AFTER "from your hand": empty/"."
        // (no-tail case) or " with a number of … counters on it equal to …".
        let enter_with_counters = super::parse_with_counters_suffix(after_hand);
        let tail_clean = after_hand.trim().trim_start_matches('.').trim();
        // Guard against silently dropping an unrecognized trailing clause: the
        // arm declines unless the tail is empty or fully consumed as a counter
        // suffix. An unconsumed tail falls through to the generic exile path.
        let tail_is_consumed = tail_clean.is_empty() || !enter_with_counters.is_empty();
        if rem.trim().is_empty() && !matches!(target, TargetFilter::Any) && tail_is_consumed {
            attach_controller_if_absent(&mut target, ControllerRef::You);
            if let TargetFilter::Typed(typed) = &mut target {
                typed
                    .properties
                    .push(FilterProp::InZone { zone: Zone::Hand });
            }
            return Some(ZoneCounterImperativeAst::Exile {
                origin: Some(Zone::Hand),
                target,
                all: false,
                enter_with_counters,
                multi_target: None,
            });
        }
    }

    // CR 608.2c: "exile a nonland card from among them" selects from the
    // previous effect's published set, then moves that selected card to exile.
    // The destination is the effect's job; the origin is intentionally open
    // because the tracked set already identifies the exact object(s).
    if let Ok((after_among, (_, filter_text))) = (
        opt(nom_primitives::parse_article),
        terminated(
            take_until(" from among "),
            alt((tag(" from among them"), tag(" from among those cards"))),
        ),
    )
        .parse(rest_lower)
    {
        let (target, rem) = parse_type_phrase(filter_text.trim());
        let tail_clean = after_among.trim().trim_start_matches('.').trim(); // allow-noncombinator: punctuation cleanup after typed terminator
        if rem.trim().is_empty() && !matches!(target, TargetFilter::Any) && tail_clean.is_empty() {
            return Some(ZoneCounterImperativeAst::Exile {
                origin: None,
                target: TargetFilter::TrackedSetFiltered {
                    id: crate::types::identifiers::TrackedSetId(0),
                    filter: Box::new(target),
                    // "from among them" is a selection-set anaphor — its members
                    // were not relocated by the producer, so it is zone-agnostic.
                    caused_by: None,
                },
                all: false,
                enter_with_counters: vec![],
                multi_target: None,
            });
        }
    }

    // CR 700.4 (#5649): "exile <N> cards from your graveyard" is a COUNTED
    // graveyard exile — Nefarious Lich's damage substitution ("exile that many
    // cards from your graveyard instead"). `Effect::ChangeZone` has no count
    // slot, so capture the quantity here and thread it onto the clause's
    // `MultiTargetSpec` at lowering (Forage precedent). The generic tail below
    // would otherwise drop the count and bind a bare `ParentTarget`.
    if let Some(count) = parse_exile_count_from_your_graveyard(&rest_text.to_ascii_lowercase()) {
        let mut filter = crate::types::ability::TypedFilter::default()
            .controller(ControllerRef::You)
            .properties(vec![crate::types::ability::FilterProp::InZone {
                zone: crate::types::zones::Zone::Graveyard,
            }]);
        filter.type_filters = vec![crate::types::ability::TypeFilter::Card];
        return Some(ZoneCounterImperativeAst::Exile {
            origin: Some(crate::types::zones::Zone::Graveyard),
            target: TargetFilter::Typed(filter),
            all: false,
            enter_with_counters: vec![],
            multi_target: Some(crate::types::ability::MultiTargetSpec::exact(count)),
        });
    }

    // CR 608.2k: thread `ctx` through so bare "it"/"them" anaphors in trigger
    // bodies ("Whenever an Elf you control dies, exile it") bind to the
    // triggering subject via `resolve_pronoun_target`, not the ability source.
    // Issue #319: Serpent's Soul-Jar exiled itself instead of the dying Elf.
    let (parsed_target, rem) = parse_target_with_ctx(rest_text, ctx);
    // CR 122.1 + CR 702.62: "exile … with N <type> counter(s) on it" lifts the
    // counter clause onto the exile ChangeZone's `enter_with_counters` so the
    // object enters Exile carrying them (Taigam, Master Opportunist: "exile the
    // spell you cast with four time counters on it" — the count is PARSED from
    // "four" via parse_number inside parse_with_counters_suffix, not hardcoded).
    // The engine applies these at the exile destination (change_zone.rs
    // resolved_counters path, covered by the egg-counter regression test).
    // Excise the consumed clause so the debug-only compound-remainder assert
    // below does not flag it.
    let rem_lower = rem.to_ascii_lowercase();
    let (mut enter_with_counters, counters_offset) =
        super::parse_with_counters_suffix_spanned(&rem_lower);
    // CR 122.1 + CR 702.62b: An anaphoric exile target ("that card" / "it" /
    // "those cards") greedily absorbs the trailing counter instruction —
    // `parse_target` returns `ParentTarget`/`SelfRef` with an EMPTY remainder —
    // so the counters would be silently dropped (Sibylline Soothsayer: "Exile
    // that card with three time counters on it"). When the post-target
    // remainder yielded none AND the target is a bare anaphor (which cannot
    // carry a "with N counters" FILTER — that reading only applies to
    // descriptive targets like "exile each creature with a +1/+1 counter on
    // it"), recover the enter-with-counters suffix from the full clause. The
    // `rem` is already empty in this case, so `counters_offset` stays `None`.
    if enter_with_counters.is_empty()
        && matches!(
            parsed_target,
            TargetFilter::ParentTarget | TargetFilter::SelfRef
        )
    {
        let rest_lower_full = rest_text.to_ascii_lowercase();
        enter_with_counters = super::parse_with_counters_suffix(&rest_lower_full);
    }
    let _rem = match counters_offset {
        Some(off) => &rem[..off],
        None => rem,
    };
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_rem, text);
    // CR 701.5a: "exile target spell" must constrain targeting to the stack,
    // mirroring parse_counter_ast at line 1218-1219.
    let target = if nom_primitives::scan_contains(rest_lower, "spell") {
        super::constrain_filter_to_stack(parsed_target)
    } else {
        parsed_target
    };
    let origin = super::infer_origin_zone(rest_lower);
    Some(ZoneCounterImperativeAst::Exile {
        origin,
        target,
        all: false,
        enter_with_counters,
        multi_target: None,
    })
}

pub(super) fn that_player_library_filter(ctx: &ParseContext) -> TargetFilter {
    if matches!(ctx.relative_player_scope, Some(ControllerRef::ScopedPlayer)) {
        return TargetFilter::ScopedPlayer;
    }
    if matches!(ctx.relative_player_scope, Some(ControllerRef::TargetPlayer)) {
        return TargetFilter::TriggeringPlayer;
    }
    // CR 603.7c: DamageDone triggers use TriggeringPlayer for "that player"
    if matches!(
        ctx.relative_player_scope,
        Some(ControllerRef::TriggeringPlayer)
    ) {
        return TargetFilter::TriggeringPlayer;
    }

    match &ctx.subject {
        Some(TargetFilter::Typed(tf)) if tf.type_filters.is_empty() && tf.controller.is_some() => {
            TargetFilter::TriggeringPlayer
        }
        Some(TargetFilter::Player) => TargetFilter::TriggeringPlayer,
        _ => TargetFilter::ParentTarget,
    }
}

/// CR 406.3: Strip a trailing "face down" suffix from the remainder of an
/// `exile the top ... library` body. Returns `(remaining_text, face_down)`.
/// The Oracle text places "face down" immediately after the library clause
/// and before any subsequent dynamic-count or follow-on phrase (Necropotence,
/// Bomat Courier, Asmodeus the Archfiend, Knowledge Vault). Matching at this
/// boundary keeps the downstream `where x is` and "those cards" lowering
/// paths untouched.
/// Parse an optional trailing "for each opponent you have" (or bare "for each
/// opponent") suffix on an `exile a card from the top of your library` clause.
fn parse_exile_card_from_top_for_each_suffix(after_lib: &str) -> QuantityExpr {
    let trimmed = after_lib.trim_start();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("for each opponent you have").parse(trimmed)
    {
        if rest.trim().trim_start_matches('.').is_empty() {
            return QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: crate::types::ability::PlayerFilter::Opponent,
                },
            };
        }
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("for each opponent").parse(trimmed) {
        if rest.trim().trim_start_matches('.').is_empty() {
            return QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: crate::types::ability::PlayerFilter::Opponent,
                },
            };
        }
    }
    if let Ok((rest, qty)) =
        crate::parser::oracle_nom::quantity::parse_for_each_clause_ref.parse(trimmed)
    {
        let tail = rest.trim().trim_start_matches('.').trim();
        if tail.is_empty() {
            return QuantityExpr::Ref { qty };
        }
    }
    QuantityExpr::Fixed { value: 1 }
}

fn strip_exile_top_face_down(after_lib: &str) -> (&str, bool) {
    let trimmed = after_lib.trim_start();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("face down").parse(trimmed) {
        // Word-boundary check after the nom tag accepted: clause
        // terminators (EOF, '.', ',') or a separator (' ') keep us from
        // bleeding into a larger identifier. Not a parser dispatch — the
        // dispatch was the `tag` above; this is post-tag validation.
        match rest.chars().next() {
            None | Some('.') | Some(',') | Some(' ') => return (rest, true),
            _ => {}
        }
    }
    (after_lib, false)
}

/// CR 107.3i: Resolve a `", where x is <quantity expr>"` suffix that follows an
/// `Effect::ExileTop` body, overriding the leading `Variable { "X" }` binding
/// with the dynamic quantity expression. Mirrors the X-binding suffix logic
/// already used by `try_parse_token_enters_with_counters` for declarative
/// "the token enters with X +1/+1 counters on it, where X is …" cards.
///
/// Trigger contexts have no chosen X (cast cost-X-paid only exists on a spell
/// cast), so the parse-time substitution is the only way the resolver sees the
/// dynamic count. When the suffix is missing or the inner phrase fails to
/// parse to a known quantity, the original count (typically `Variable { "X" }`
/// or `Fixed { value: N }`) is returned unchanged.
/// CR 701.50d + CR 107.3i: Parse connive count from text after "connive"/"connives".
/// Handles literal N, bare `X` (spell-cost path), and ", where X is <quantity>"
/// bindings (Spymaster's Vault class).
fn parse_connive_count_expr<'a>(rest_orig: &'a str, lower_rest: &str) -> (QuantityExpr, &'a str) {
    if let Ok((after_num, n)) = nom_primitives::parse_number.parse(lower_rest) {
        let consumed = lower_rest.len() - after_num.len();
        let after_orig = rest_orig.get(consumed..).unwrap_or(rest_orig).trim_start();
        return (QuantityExpr::Fixed { value: n as i32 }, after_orig);
    }

    if let Ok((after_x, _)) = tag::<_, _, OracleError<'_>>("x").parse(lower_rest) {
        let consumed = lower_rest.len() - after_x.len();
        let after_orig = rest_orig.get(consumed..).unwrap_or(rest_orig).trim_start();
        let initial = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        let count = resolve_exile_top_where_x_binding(after_orig, initial);
        return (count, after_orig);
    }

    (QuantityExpr::Fixed { value: 1 }, rest_orig)
}

fn parse_draw_cards_where_x_tail(remainder: &str) -> Option<QuantityExpr> {
    let remainder_lower = remainder.to_ascii_lowercase();
    let (after_cards_lower, _) = alt((
        tag::<_, _, OracleError<'_>>("cards"),
        tag::<_, _, OracleError<'_>>("card"),
    ))
    .parse(remainder_lower.as_str())
    .ok()?;
    let consumed = remainder_lower.len() - after_cards_lower.len();
    let after_cards = remainder.get(consumed..)?;
    let where_tail_lower = after_cards.trim_start().to_ascii_lowercase();
    let (qty_text_lower, _) = preceded(
        opt(tag::<_, _, OracleError<'_>>(",")),
        preceded(space0, tag("where x is ")),
    )
    .parse(where_tail_lower.as_str())
    .ok()?;
    let consumed_where = where_tail_lower.len() - qty_text_lower.len();
    let trimmed_after_cards = after_cards.trim_start();
    let qty_text = trimmed_after_cards.get(consumed_where..)?.trim();
    let qty_text = terminated(
        take_until::<_, _, OracleError<'_>>("."),
        tag::<_, _, OracleError<'_>>("."),
    )
    .parse(qty_text)
    .map_or(qty_text, |(_, sentence)| sentence.trim());
    parse_where_x_quantity_expression(qty_text)
}

fn draw_count_tail_is_complete(remainder: &str) -> bool {
    let remainder_lower = remainder
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_ascii_lowercase();
    if remainder_lower.is_empty() {
        return true;
    }
    let after_noun = (
        opt(tag::<_, _, OracleError<'_>>("additional ")),
        alt((tag("cards"), tag("card"))),
    )
        .parse(remainder_lower.as_str())
        .ok()
        .map(|(rest, _)| rest.trim_start());
    if let Some(after_noun) = after_noun {
        if tag::<_, _, OracleError<'_>>("and proliferate")
            .parse(after_noun)
            .is_ok()
            || preceded(
                opt(tag::<_, _, OracleError<'_>>(",")),
                preceded(space0, tag("where x is ")),
            )
            .parse(after_noun)
            .is_ok()
        {
            return false;
        }
    }
    let complete = all_consuming((
        opt(tag::<_, _, OracleError<'_>>("additional ")),
        alt((tag("cards"), tag("card"))),
        opt(preceded(
            opt(tag(",")),
            preceded(
                space0,
                alt((
                    tag("rounded down"),
                    tag("rounded up"),
                    tag("rounded down to the nearest whole number"),
                    tag("rounded up to the nearest whole number"),
                )),
            ),
        )),
    ))
    .parse(remainder_lower.as_str())
    .is_ok();
    complete
        || tag::<_, _, OracleError<'_>>("and proliferate")
            .parse(remainder_lower.as_str())
            .is_err()
}

fn resolve_exile_top_where_x_binding(after_lib: &str, initial_count: QuantityExpr) -> QuantityExpr {
    let trimmed = after_lib
        .trim_start()
        .trim_start_matches([',', '.', ' '])
        .trim_start();
    let Ok((rest_where, _)) = alt((
        tag::<_, _, OracleError<'_>>("where x is "),
        tag("where X is "),
    ))
    .parse(trimmed) else {
        return initial_count;
    };
    let qty_text = rest_where
        .trim_end()
        .trim_end_matches(['.', ','])
        .trim_end();
    if let Some(expr) = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text) {
        return expr;
    }
    if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
        return QuantityExpr::Ref { qty };
    }
    initial_count
}

fn parse_counter_unless_pay(
    rest: &str,
) -> Option<Option<crate::types::ability::UnlessPayModifier>> {
    match super::parse_unless_payment(rest) {
        Some(cost) => Some(Some(super::counter_unless_pay_modifier(cost))),
        None if counter_unless_has_partial_where_x_quantity(rest) => None,
        None if super::has_unless_clause(rest) => None,
        None => Some(None),
    }
}

fn counter_unless_has_partial_where_x_quantity(rest: &str) -> bool {
    let Some(qty_text) = counter_unless_where_x_quantity(rest) else {
        return false;
    };
    let Ok((remaining, _)) = nom_quantity::parse_quantity(qty_text) else {
        return false;
    };
    !remaining.trim().trim_end_matches('.').is_empty()
}

fn counter_unless_where_x_quantity(rest: &str) -> Option<&str> {
    let (_, _, after_unless) =
        nom_primitives::scan_preceded(rest, |i| tag::<_, _, OracleError<'_>>("unless ").parse(i))?;
    let (_, _, cost_str) = nom_primitives::scan_preceded(after_unless, |i| {
        tag::<_, _, OracleError<'_>>("pays ").parse(i)
    })?;
    let cost_end = cost_str
        .find(|c: char| c != '{' && c != '}' && !c.is_alphanumeric())
        .unwrap_or(cost_str.len());
    let cost_text = cost_str[..cost_end].trim();
    if cost_text != "{X}" && cost_text != "{x}" {
        return None;
    }
    let after_cost = cost_str[cost_end..].trim().trim_start_matches(',').trim();
    let (qty_text, _) = tag::<_, _, OracleError<'_>>("where x is ")
        .parse(after_cost)
        .ok()?;
    Some(qty_text.trim_end_matches('.').trim())
}

pub(super) fn parse_counter_ast(text: &str, lower: &str) -> Option<ZoneCounterImperativeAst> {
    // CR 701.6 + CR 405.1: "Counter all/each [filter] spells/abilities"
    // mass-counter precheck. Mirrors the `parse_destroy_ast` precheck +
    // `BounceAll` precedent: consume the verb plus an optional `all`/`each`
    // pluralizer, set `all: true`, and let the existing target / spell-stack
    // / activated-or-triggered-ability dispatch run unchanged on the tail.
    let (mass_consumed, rest_orig, rest) = if let Some((_, rest_orig)) =
        nom_on_lower(text, lower, |input| {
            value((), alt((tag("counter all "), tag("counter each ")))).parse(input)
        }) {
        let rest_lower = &lower[lower.len() - rest_orig.len()..];
        (true, rest_orig, rest_lower)
    } else {
        let (_, rest_orig) =
            nom_on_lower(text, lower, |input| value((), tag("counter ")).parse(input))?;
        let rest_lower = &lower[lower.len() - rest_orig.len()..];
        (false, rest_orig, rest_lower)
    };

    // CR 113.3: "abilities" (or "activated or triggered ability") with no
    // intervening type phrase ⇒ stack-ability filter. Mass mode also accepts
    // bare "abilities" (Kadena's Silencer: "counter all abilities your
    // opponents control"). Single-target mode keeps the original strict
    // `activated or triggered ability` requirement to avoid false positives
    // on noun-counter phrases like "page counter on this artifact".
    // Bare "abilities" head: tag-match (skipping leading whitespace) so the
    // dispatch is a real nom combinator, not a string starts_with.
    fn abilities_head(i: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        preceded(nom::character::complete::multispace0, tag("abilities")).parse(i)
    }
    let abilities_match = nom_primitives::scan_contains(rest, "activated or triggered ability")
        || (mass_consumed && abilities_head(rest).is_ok());
    if abilities_match {
        // CR 118.12: Parse "unless pays" even for ability counters.
        let unless_pay = parse_counter_unless_pay(rest)?;
        return Some(ZoneCounterImperativeAst::Counter {
            target: stack_ability_filter_from_text(rest),
            source_rider: None,
            unless_pay,
            all: mass_consumed,
        });
    }

    // CR 701.6a + CR 115.1: "Counter target <stack-object phrase>" where the
    // phrase is a multi-way disjunction of spells and/or activated/triggered
    // abilities — e.g. Louisoix's Sacrifice's "activated ability, triggered
    // ability, or noncreature spell". Bare `parse_target` cannot recognize the
    // "activated ability" disjunct (it is not a card type) and silently drops
    // the noncreature restriction, yielding a degenerate empty-`type_filters`
    // stack filter. Strip the leading "target " token (nom `tag`, not string
    // matching) and try the dedicated stack-object combinator first; it
    // composes the ability-kind axis with an optional restricted-spell tail.
    let stack_phrase = opt(tag::<_, _, OracleError<'_>>("target "))
        .parse(rest)
        .map(|(after, _)| after)
        .unwrap_or(rest);
    if let Ok((_, stack_target)) =
        crate::parser::oracle_nom::target::parse_stack_object_target(stack_phrase)
    {
        let unless_pay = parse_counter_unless_pay(rest)?;
        return Some(ZoneCounterImperativeAst::Counter {
            target: stack_target,
            source_rider: None,
            unless_pay,
            all: mass_consumed,
        });
    }

    // CR 107.3i + CR 202.3 + CR 608.2b: A trailing "where X is <expression>"
    // defining clause (Spellstutter Sprite: "counter target spell with mana
    // value X or less, where X is the number of Faeries you control") binds
    // the literal X in the type filter's `Cmc` bound. Without this
    // substitution the bound stays `QuantityRef::Variable("X")` with no
    // defining expression, collapses to 0 at resolution, and the target
    // legality re-check at CR 608.2b (target legality is verified again as a
    // spell or ability resolves) fails every legal spell. The strip-and-apply
    // pattern mirrors the Birthing Ritual callsite (see
    // `oracle_effect/mod.rs:16859`).
    let (without_where_tp, where_x_expression) =
        super::strip_trailing_where_x(crate::parser::oracle_util::TextPair::new(rest_orig, rest));
    let (target, _rem) = parse_target(without_where_tp.original);
    #[cfg(debug_assertions)]
    assert_no_compound_remainder(_rem, text);
    // `parse_target("... spell with mana value X or less")` already scopes
    // the spell phrase to the stack through the shared target parser. Keep this
    // path on that building block and only apply the trailing X definition.
    // CR 107.3c: fail honestly instead of fabricating a raw-text placeholder.
    let target = super::apply_where_x_to_filter(target, where_x_expression.as_deref())?;
    // CR 118.12: Parse "unless its controller pays {X}" for conditional counters
    let unless_pay = parse_counter_unless_pay(rest)?;
    Some(ZoneCounterImperativeAst::Counter {
        target,
        source_rider: None,
        unless_pay,
        all: mass_consumed,
    })
}

/// CR 118.8 + CR 119.4: Parse the amount portion of a "pay <amount> life" cost.
///
/// `rest` is the lowercase text after the leading `"pay "` token. Returns the
/// resolved `QuantityExpr` on success — literal (`"3 life"`), X variable
/// (`"X life"`), or a dynamic reference (`"life equal to its power"`,
/// `"life equal to <quantity-ref>"`). All dispatch is nom-combinator based,
/// with a shared `life_with_boundary` combinator that guards `"life"` against
/// accidental alpha-suffix matches (e.g., `"x lifelink"`).
fn parse_pay_life_amount(rest: &str) -> Option<QuantityExpr> {
    use crate::parser::oracle_nom::error::OracleResult;
    use nom::combinator::recognize;
    use nom::sequence::terminated;

    // Shared word-boundary guard: the token just consumed must be followed by
    // end-of-input or punctuation/whitespace — not another alpha char. This
    // blocks false matches like "lifelink" when we only want "life".
    fn word_boundary(i: &str) -> OracleResult<'_, ()> {
        peek(alt((value((), eof), value((), recognize(one_of(" .,")))))).parse(i)
    }

    // CR 118.8: "pay life equal to <quantity-ref>" — delegates to the shared
    // event-context / named-quantity resolvers so every dynamic amount pattern
    // already recognized for gain/lose life composes here too. The quantity
    // helpers are not nom-based, so content cleanup (trailing period + space)
    // happens on the already-dispatched remainder — nom owns the dispatch,
    // not the content normalization.
    if let Ok((tail, _)) = tag::<_, _, OracleError<'_>>("life equal to ").parse(rest) {
        let qty_text = tail.trim_end().trim_end_matches('.').trim_end();
        if let Some(expr) = crate::parser::oracle_quantity::parse_event_context_quantity(qty_text) {
            return Some(expr);
        }
        if let Some(qty) = crate::parser::oracle_quantity::parse_quantity_ref(qty_text) {
            return Some(QuantityExpr::Ref { qty });
        }
        return None;
    }

    // CR 107.1b: "pay X life" — variable amount resolved from `chosen_x`.
    if terminated(tag::<_, _, OracleError<'_>>("x life"), word_boundary)
        .parse(rest)
        .is_ok()
    {
        return Some(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        });
    }

    // CR 118.8: "pay half your life[, rounded up]" — delegate the life-fraction
    // phrase to the shared quantity expression parser (`DivideRounded` over the
    // controller's `LifeTotal`), so the rounding mode and life-total wording
    // recognized everywhere else compose here too. Gated on a "half " prefix so
    // only fraction phrases reach the (non-dispatch) all-consuming delegation.
    if tag::<_, _, OracleError<'_>>("half ").parse(rest).is_ok() {
        let qty_text = rest.trim_end().trim_end_matches('.').trim_end();
        if let Ok(("", expr)) = crate::parser::oracle_nom::quantity::parse_quantity(qty_text) {
            return Some(expr);
        }
    }

    // CR 118.8: "pay N life" — literal amount via `parse_number` (digit words
    // or numerals, never "X" — handled above). Same word-boundary guard so
    // hypothetical phrases like "3 lifelink" cannot false-match.
    if let Ok((_, (n, _))) = (
        nom_primitives::parse_number,
        terminated(tag::<_, _, OracleError<'_>>(" life"), word_boundary),
    )
        .parse(rest)
    {
        return Some(QuantityExpr::Fixed { value: n as i32 });
    }

    None
}

fn parse_mana_and_life_payment(rest_orig: &str) -> Option<AbilityCost> {
    let (mana_cost, after_mana) = parse_mana_symbols(rest_orig.trim())?;
    let after_mana_lower = after_mana.to_lowercase();
    let (_, after_and) = nom_on_lower(after_mana, &after_mana_lower, |input| {
        value(
            (),
            preceded(nom::character::complete::multispace0, tag("and ")),
        )
        .parse(input)
    })?;
    let amount = parse_pay_life_amount(after_and.trim_start())?;
    Some(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana_cost },
            AbilityCost::PayLife { amount },
        ],
    })
}

pub(super) fn parse_cost_resource_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<CostResourceImperativeAst> {
    if let Some(Effect::Unimplemented {
        name,
        description: Some(description),
    }) = try_parse_activate_only_condition(text)
    {
        if name == "activate_only_if_controls_land_subtype_any" {
            return Some(
                CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny {
                    subtypes: description.split('|').map(ToString::to_string).collect(),
                },
            );
        }
    }
    if let Some((_, rest_orig)) =
        nom_on_lower(text, lower, |input| value((), tag("pay ")).parse(input))
    {
        let rest = &lower[lower.len() - rest_orig.len()..];
        if let Some(cost) = parse_mana_and_life_payment(rest_orig) {
            return Some(CostResourceImperativeAst::Pay { cost });
        }
        // CR 118.8 + CR 119.4: `pay <amount> life` — literal count, X variable,
        // or dynamic reference (`pay life equal to its power`). Dispatched with
        // nom combinators over the post-"pay " remainder.
        if let Some(amount) = parse_pay_life_amount(rest) {
            return Some(CostResourceImperativeAst::Pay {
                cost: AbilityCost::PayLife { amount },
            });
        }
        // CR 107.14: "pay any amount of {E}" → variable energy payment
        if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("any amount of {e}").parse(rest) {
            return Some(CostResourceImperativeAst::Pay {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
            });
        }
        // CR 119.4 + CR 107.1c: "pay any amount of life" → variable life payment;
        // the amount paid binds X for the downstream "draw/look at that many"
        // step. The word-boundary guard (eof or " .,") blocks "lifelink" /
        // "lifeforce" from false-matching.
        if terminated(
            tag::<_, _, OracleError<'_>>("any amount of life"),
            peek(alt((
                value((), eof),
                value((), nom::combinator::recognize(one_of(" .,"))),
            ))),
        )
        .parse(rest)
        .is_ok()
        {
            return Some(CostResourceImperativeAst::Pay {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
            });
        }
        // CR 118.1 + CR 107.3: "pay any amount of mana" → variable generic
        // mana payment. Join forces uses the total paid this way as X for the
        // following effect chain.
        if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("any amount of mana").parse(rest) {
            return Some(CostResourceImperativeAst::Pay {
                cost: AbilityCost::Mana {
                    cost: crate::types::mana::ManaCost::Cost {
                        shards: vec![crate::types::mana::ManaCostShard::X],
                        generic: 0,
                    },
                },
            });
        }
        // "pay an amount of {e} equal to ..." → dynamic energy payment.
        // Delegates to the single-authority combinator
        // `parse_dynamic_energy_unless_cost`; the `Variable("X")` fallback
        // below covers "equal to" tails the quantity parser cannot resolve.
        if tag::<_, _, OracleError<'_>>("an amount of {e} equal to ")
            .parse(rest)
            .is_ok()
        {
            if let Some(amount) = super::parse_dynamic_energy_unless_cost(rest) {
                return Some(CostResourceImperativeAst::Pay {
                    cost: AbilityCost::PayEnergy { amount },
                });
            }
            // Fallback: variable energy payment
            return Some(CostResourceImperativeAst::Pay {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
            });
        }
        // CR 107.14: "pay {E}", "pay {E}{E}", "pay N {E}" → AbilityCost::PayEnergy
        if rest.contains("{e}") {
            let energy_count = rest.matches("{e}").count() as u32;
            let cleaned = rest.replace("{e}", "").replace(' ', "");
            if cleaned.is_empty() {
                // Pure {E} symbols: "pay {e}{e}"
                return Some(CostResourceImperativeAst::Pay {
                    cost: AbilityCost::PayEnergy {
                        amount: QuantityExpr::Fixed {
                            value: energy_count as i32,
                        },
                    },
                });
            }
            // "pay N {e}" / "pay eight {e}" — number prefix + {e} suffix
            if rest.ends_with("{e}") {
                let prefix = rest.trim_end_matches("{e}").trim();
                if let Ok((_, n)) = nom_primitives::parse_number.parse(prefix) {
                    return Some(CostResourceImperativeAst::Pay {
                        cost: AbilityCost::PayEnergy {
                            amount: QuantityExpr::Fixed { value: n as i32 },
                        },
                    });
                }
            }
        }
        // "pay {2}{B}" → AbilityCost::Mana (CR 117.1)
        if let Some((mana_cost, _)) = parse_mana_symbols(rest_orig.trim()) {
            return Some(CostResourceImperativeAst::Pay {
                cost: AbilityCost::Mana { cost: mana_cost },
            });
        }
    }
    // CR 106.4 + CR 505.1: Dispatch to the mana detector for a bare "add …"
    // clause OR a subject-led mana clause ("the active player adds …", "that
    // player adds …") — `try_parse_add_mana_effect` strips the subject itself.
    if nom_on_lower(text, lower, |input| {
        alt((
            value((), tag("add ")),
            value(
                (),
                (
                    alt((tag("the active player "), tag("that player "))),
                    tag("adds "),
                ),
            ),
        ))
        .parse(input)
    })
    .is_some()
    {
        return match try_parse_add_mana_effect_with_context(text, ctx) {
            Some(Effect::Mana {
                produced,
                restrictions,
                target,
                ..
            }) => Some(CostResourceImperativeAst::Mana {
                produced,
                restrictions,
                target,
            }),
            _ => None,
        };
    }
    if let Some(effect) = super::try_parse_damage(lower, text, ctx) {
        return match effect {
            Effect::DealDamage {
                amount,
                target,
                damage_source: None,
                excess: _,
            } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: false,
            }),
            Effect::DamageAll {
                amount,
                target,
                player_filter: None,
                damage_source: None,
            } => Some(CostResourceImperativeAst::Damage {
                amount,
                target,
                all: true,
            }),
            // DealDamage with damage_source, DamageEachPlayer, DamageAll with
            // a non-default damage_source/player_filter — pass through directly.
            other => Some(CostResourceImperativeAst::DamageEffect(Box::new(other))),
        };
    }
    None
}

pub(super) fn lower_cost_resource_ast(ast: CostResourceImperativeAst) -> Effect {
    match ast {
        CostResourceImperativeAst::ActivateOnlyIfControlsLandSubtypeAny { subtypes } => {
            Effect::Unimplemented {
                name: "activate_only_if_controls_land_subtype_any".to_string(),
                description: Some(subtypes.join("|")),
            }
        }
        CostResourceImperativeAst::Mana {
            produced,
            restrictions,
            target,
        } => Effect::Mana {
            produced,
            restrictions,
            grants: vec![],
            expiry: None,
            target,
        },
        CostResourceImperativeAst::Damage {
            amount,
            target,
            all,
        } => {
            if all {
                Effect::DamageAll {
                    amount,
                    target,
                    player_filter: None,
                    damage_source: None,
                }
            } else {
                Effect::DealDamage {
                    amount,
                    target,
                    damage_source: None,
                    excess: None,
                }
            }
        }
        CostResourceImperativeAst::Pay { cost } => Effect::PayCost {
            cost,
            scale: None,
            payer: TargetFilter::Controller,
        },
        CostResourceImperativeAst::DamageEffect(effect) => *effect,
    }
}

/// CR 500.8 + CR 510.2: Quantity for "<N> additional <step/phase>s". The
/// scanner advances along word boundaries and tries a single composed
/// combinator at each position:
///   `quantifier ~ " additional"` where `quantifier` =
///     `tag("that many")` → event-bound (`QuantityRef::EventContextAmount`)
///   | `parse_number`        → literal N (e.g. "two additional combat phases")
///
/// Anything else — including the article forms "an"/"a"/"the" already parsed
/// elsewhere as 1 — falls through to the singular default
/// `QuantityExpr::Fixed { value: 1 }`. Anchoring on `" additional"` keeps the
/// helper agnostic to surrounding sentence shapes ("you get that many
/// additional upkeep steps after this phase", "after this phase, there is an
/// additional combat phase").
fn parse_additional_phase_count(lower: &str) -> QuantityExpr {
    fn count_combinator(input: &str) -> OracleResult<'_, QuantityExpr> {
        let event_bound = value(
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            tag("that many"),
        );
        let literal = map(nom_primitives::parse_number, |n| QuantityExpr::Fixed {
            value: n as i32,
        });
        terminated(alt((event_bound, literal)), tag(" additional")).parse(input)
    }

    let mut remaining = lower;
    while !remaining.is_empty() {
        if let Ok((_rest, qty)) = count_combinator(remaining) {
            return qty;
        }
        // Advance to the next word boundary so the combinator stays anchored
        // to candidate quantifier positions.
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    QuantityExpr::Fixed { value: 1 }
}

/// CR 701.4a: Recognize a "behold a [quality]" effect leaf. "Behold a [quality]"
/// means "Reveal a [quality] card from your hand or choose a [quality] permanent
/// you control on the battlefield" — the beheld quality is a subtype/type filter
/// shared by both legs. Rejects a leftover tail or a non-type quality
/// (`TargetFilter::Any`), identical to the cost-side `parse_behold_cost` guard,
/// so unrelated clauses cannot masquerade as behold.
fn parse_behold_effect_ast(text: &str, lower: &str) -> Option<ImperativeFamilyAst> {
    type E<'a> = OracleError<'a>;
    let (rest_lower, _) = alt((
        tag::<_, _, E<'_>>("beholds "),
        tag::<_, _, E<'_>>("behold "),
    ))
    .parse(lower)
    .ok()?;
    let (rest_lower, _) = opt(alt((tag::<_, _, E<'_>>("a "), tag::<_, _, E<'_>>("an "))))
        .parse(rest_lower)
        .ok()?;
    // Map the consumed lowercase byte span back onto the original-case text so
    // `parse_type_phrase` sees the properly-cased subtype (e.g. "Dragon").
    let consumed = lower.len() - rest_lower.len();
    let rest_orig = text.get(consumed..)?;
    // CR 701.4a: bound the quality at sentence/reminder-text punctuation.
    let (_, filter_text) = take_till::<_, _, E<'_>>(|c| c == '.' || c == '(')
        .parse(rest_orig)
        .ok()?;
    let (filter, remainder) = parse_type_phrase(filter_text.trim());
    if !remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }
    Some(ImperativeFamilyAst::Behold(filter))
}

pub(super) fn parse_imperative_family_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ImperativeFamilyAst> {
    let text = text.trim_start();
    let lower = lower.trim_start();
    let first_word = lower.split_whitespace().next().unwrap_or("");

    // CR 701.60a: "[subject] no longer suspected" — the un-designation
    // transition. The subject leads the clause (varying anaphor / noun phrase),
    // so `first_word` is "all"/"it's"/"they're"/"~"/"become" rather than a verb
    // keyword. Intercept it as an anchored whole-clause production before the
    // first-word dispatch, alongside the other non-verb-led effects below.
    if let Some(effect) = parse_no_longer_suspected_ast(lower) {
        return Some(ImperativeFamilyAst::GainKeyword(effect));
    }

    if let Some(ast) = parse_assemble_contraption_imperative(lower) {
        return Some(ast);
    }

    if let Some(ast) = parse_reassemble_contraption_imperative(text, lower, ctx) {
        return Some(ast);
    }

    // CR 724.1: "end the turn" (Time Stop, Sundial of the Infinite, Obeka,
    // Glorious End, Discontinuity, Day's Undoing). Whole-phrase imperative
    // with no target; parse it as an anchored nom production rather than a
    // substring scan so unrelated clauses cannot accidentally match it.
    if all_consuming(terminated(
        tag::<_, _, OracleError<'_>>("end the turn"),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::EndTheTurn));
    }

    // CR 724.2: "end the combat phase" (Mandate of Peace). Whole-phrase
    // imperative with no target; anchored nom production mirroring the
    // "end the turn" parse so unrelated clauses cannot accidentally match it.
    if all_consuming(terminated(
        tag::<_, _, OracleError<'_>>("end the combat phase"),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::EndCombatPhase));
    }

    // CR 311.7 / CR 901.9b: "chaos ensues" as a standalone resolving instruction
    // — the active plane's "whenever chaos ensues" ability triggers. Whole-phrase
    // imperative with no target; anchored nom production mirroring the "end the
    // turn" / "end the combat phase" parses so unrelated clauses cannot
    // accidentally match it. Makes `parse_effect("chaos ensues") ==
    // Effect::ChaosEnsues`, which the planar-die planeswalk-replacement parser
    // (Fixed Point in Time) consumes as its substitute.
    if all_consuming(terminated(
        tag::<_, _, OracleError<'_>>("chaos ensues"),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::ChaosEnsues));
    }

    // CR 103.1 + CR 101.4: "reverse the game's turn order" / "reverse the turn
    // order" (Temple of Atropos chaos, Aeon Engine activated ability, Time
    // Distortion phenomenon). Whole-phrase imperative with no target; anchored
    // nom production mirroring the "chaos ensues" parse so unrelated clauses
    // cannot accidentally match it. The "(For example …)" reminder is stripped
    // upstream, and the apostrophe reaching the parser is straight. The two
    // surface forms are a single `alt()` axis (compose, don't enumerate).
    if all_consuming(terminated(
        alt((
            tag::<_, _, OracleError<'_>>("reverse the game's turn order"),
            tag("reverse the turn order"),
        )),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::ReverseTurnOrder));
    }

    // CR 701.12a: "two target players exchange life totals" (Soul Conduit, Axis
    // of Mortality). The subject ("two target players") precedes the verb, so
    // `first_word` is "two"/"target"/"have" rather than a verb keyword —
    // intercept it as an anchored whole-phrase production before the first-word
    // dispatch. Accept three optional leading wrinkles:
    //   - "have " — the causative construction from "you may have <players>
    //     exchange ..." (Axis of Mortality), which keeps its full surface form.
    //   - "two " — the cardinality quantifier, which may already be stripped
    //     upstream by `strip_any_number_quantifier`.
    // Both players are `Player` targets.
    if all_consuming(terminated(
        preceded(
            (opt(tag::<_, _, OracleError<'_>>("have ")), opt(tag("two "))),
            tag("target players exchange life totals"),
        ),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::ExchangeLifeTotals {
            player_a: TargetFilter::Player,
            player_b: TargetFilter::Player,
        });
    }

    // CR 119.7 + CR 119.8: "redistribute any number of players' life totals" (Reverse the
    // Sands, The Doctor's Tomb) and the bare "redistribute any number of life
    // totals" (You Live Only Because I Will It — Archenemy scheme). Verb-initial,
    // but handled here as an anchored whole-phrase production (alongside the
    // ExchangeLifeTotals sibling above) so the generic "any number of" quantifier
    // stripping in later dispatch can't fragment the phrase. The reminder text
    // ("(Each of those players gets one life total back.)" / "(Each affected
    // player or team gets one of those life totals back.)") is stripped upstream.
    // The possessive subject ("players'") is an optional axis so the whole class
    // parses; accept both the ASCII (`players'`) and typographic
    // (`players\u{2019}`) apostrophe as a single `alt()` axis (compose, don't
    // enumerate).
    if all_consuming(terminated(
        (
            tag::<_, _, OracleError<'_>>("redistribute any number of "),
            opt((tag("players"), alt((tag("'"), tag("\u{2019}"))), tag(" "))),
            tag("life totals"),
        ),
        opt(tag(".")),
    ))
    .parse(lower.trim())
    .is_ok()
    {
        return Some(ImperativeFamilyAst::RedistributeLifeTotals);
    }

    // CR 500.8: Additional step/phase effects can appear in various sentence structures
    // ("there is an additional combat phase", "after this phase, there is an additional...").
    // Intercept early regardless of first_word.
    if nom_primitives::scan_contains(lower, "additional combat phase") {
        let with_main =
            nom_primitives::scan_contains(lower, "followed by an additional main phase");
        // CR 500.8 (Full Throttle): "After this main phase, there are N additional
        // combat phases" anchors to whichever main phase the spell resolves in.
        // `PreCombatMain` is a resolution-time sentinel remapped in
        // `effects/additional_phase.rs` when the active phase is a main phase.
        let after = if nom_primitives::scan_contains(lower, "after this main phase") {
            Phase::PreCombatMain
        } else {
            Phase::EndCombat
        };
        return Some(ImperativeFamilyAst::GainKeyword(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::BeginCombat,
            after,
            followed_by: if with_main {
                vec![Phase::PostCombatMain]
            } else {
                vec![]
            },
            count: parse_additional_phase_count(lower),
            attacker_restriction: None,
        }));
    }
    if nom_primitives::scan_contains(lower, "additional upkeep step") {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::Upkeep,
            after: Phase::Upkeep,
            followed_by: vec![],
            count: parse_additional_phase_count(lower),
            attacker_restriction: None,
        }));
    }
    // CR 500.8 + CR 513.1: "there is an additional end step after this step"
    // (Y'shtola Rhul). The extra end step is anchored to `Phase::End` so the
    // LIFO `advance_phase` scan inserts it as the current end step completes.
    if nom_primitives::scan_contains(lower, "additional end step") {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::End,
            after: Phase::End,
            followed_by: vec![],
            count: parse_additional_phase_count(lower),
            attacker_restriction: None,
        }));
    }
    // CR 501.1 + CR 500.8: "there is an additional beginning phase after this
    // phase" (Temple of Atropos, Sphinx/Shadow of the Second Sun, Cyclonus). The
    // beginning phase is untap → upkeep → draw (CR 501.1). `phase: Untap` is the
    // self-documenting marker the resolver keys the beginning-phase schedule on
    // (untap begins only the beginning phase, CR 502) — no other AdditionalPhase
    // producer emits `phase: Untap`. `after`/`followed_by` are unused for this
    // shape: the resolver derives the anchor from the phase this ability resolves
    // in ("this phase", CR 500.8) via `last_step_of_phase`, and resumes the turn
    // at that phase's natural successor. `after` is a don't-care sentinel.
    if nom_primitives::scan_contains(lower, "additional beginning phase") {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::AdditionalPhase {
            target: TargetFilter::Controller,
            phase: Phase::Untap,
            after: Phase::PostCombatMain,
            followed_by: vec![],
            count: parse_additional_phase_count(lower),
            attacker_restriction: None,
        }));
    }

    // CR 606.3: "activate each planeswalker's loyalty ability an additional
    // time this turn" — The Chain Veil class. The outer "you may" is
    // stripped by `strip_optional_effect_prefix`, leaving the imperative
    // "activate ..." form here. The grant raises the per-permanent CR 606.3
    // activation cap by `amount` for the rest of the turn (the resolver lives
    // in `effects::grant_extra_loyalty_activations`).
    //
    // Quantity axes accepted:
    //   - "an additional time"        → +1
    //   - "an additional <number> times" → +N (future-proofs cards that grant
    //     more than one additional activation in a single ability).
    if let Some(amount) = parse_grant_extra_loyalty_activations(lower) {
        return Some(ImperativeFamilyAst::GainKeyword(
            Effect::GrantExtraLoyaltyActivations {
                amount,
                target: TargetFilter::Controller,
            },
        ));
    }

    // CR 723.1: "You control target player during that player's next turn"
    // (Mindslaver / Word of Command class). "You" is the spell/ability controller
    // in a declarative sentence (not an imperative verb), so this bypasses the
    // first_word dispatch below. Delegates to the ControlNextTurn combinator
    // in `parse_targeted_action_ast`.
    if tag::<_, _, OracleError<'_>>("you control ")
        .parse(lower)
        .is_ok()
    {
        if let Some(ast) = parse_targeted_action_ast(text, lower, ctx) {
            return Some(ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
                ast,
            )));
        }
    }

    // CR 614.9 + CR 614.1a + CR 615: One-shot "the next time [source] would deal
    // [combat] damage [to X] this turn, [modify/redirect] instead" damage
    // replacement (Desperate Gambit, Soltari Guerrillas, Beacon of Destiny, Jade
    // Monolith, Goblin Psychopath). The text begins with "the next time" — not a
    // verb — so it is intercepted here, alongside the other non-verb-led effects
    // above, before the first-word verb dispatch. The detector is the prefix
    // combinator inside `parse_oneshot_damage_replacement`; on failure it returns
    // `None` and we fall through.
    if let Some(effect) = crate::parser::oracle_replacement::parse_oneshot_damage_replacement(lower)
    {
        return Some(ImperativeFamilyAst::GainKeyword(effect));
    }

    // CR 614.1a + CR 614.12 + CR 707.2: One-shot "the next time one or more
    // creatures or planeswalkers enter this turn, they enter as copies of the
    // chosen creature" replacement (Mystic Reflection). Like the damage/draw
    // one-shot replacements, this starts with "the next time" rather than an
    // imperative verb, so it must be intercepted before first-word dispatch.
    if let Some(effect) =
        crate::parser::oracle_replacement::parse_oneshot_enter_as_copy_replacement(lower)
    {
        return Some(ImperativeFamilyAst::GainKeyword(effect));
    }

    // CR 708.5: "[Until end of turn,] you may look at face-down [permanents] you
    // don't control any time" (Lumbering Laundry's `{2}:` activated ability).
    // This is the duration-bound form of Found Footage's continuous look
    // permission: the same `StaticMode::MayLookAtFaceDown` carried by a resolving
    // ability as a transient continuous effect. The "you may" here is part of the
    // permission grammar ("you ARE allowed to look"), not an optional one-shot, so
    // intercept the whole phrase BEFORE the `you`/`may` first-word arms strip
    // "you may " and re-dispatch it as a generic optional clause (which would
    // otherwise fail to a `look` Unimplemented). The face-down filter is parsed by
    // the shared `parse_may_look_at_face_down_filter` (the single authority for
    // the phrase grammar, also used by the static-line builder), and the duration
    // rides on the resolving ability via the stripped "Until end of turn," prefix;
    // the `GenericEffect` resolution path defaults to `UntilEndOfTurn` when no
    // duration is supplied.
    if let Some(filter) = parse_may_look_at_face_down_filter(text, lower) {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::MayLookAtFaceDown)
                .affected(filter)
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::MayLookAtFaceDown,
                }])],
            duration: None,
            target: None,
        }));
    }

    if nom_on_lower(first_word, first_word, |i| value((), tag("put")).parse(i)).is_some() {
        return if nom_primitives::scan_contains(lower, "that many")
            && nom_primitives::scan_contains(lower, "counter")
        {
            try_parse_that_many_counters(lower, ctx)
                .map(ImperativeFamilyAst::GainKeyword)
                .or_else(|| {
                    try_parse_put_sticker_effect(lower, ctx).map(ImperativeFamilyAst::GainKeyword)
                })
                .or_else(|| {
                    parse_zone_counter_ast(text, lower, ctx)
                        .map(ImperativeFamilyAst::ZoneCounter)
                        .or_else(|| parse_put_ast(text, lower, ctx).map(ImperativeFamilyAst::Put))
                })
        } else {
            try_parse_put_sticker_effect(lower, ctx)
                .map(ImperativeFamilyAst::GainKeyword)
                .or_else(|| {
                    parse_zone_counter_ast(text, lower, ctx)
                        .map(ImperativeFamilyAst::ZoneCounter)
                        .or_else(|| parse_put_ast(text, lower, ctx).map(ImperativeFamilyAst::Put))
                })
        };
    }

    // CR 614.11 + CR 614.6: One-shot "the next time you would draw a card this
    // turn, [effect] instead" delayed DRAW replacement (Words of Worship/
    // Wilding). Also begins with "the next time", so intercepted here before the
    // first-word verb dispatch. The detector is the prefix combinator inside
    // `parse_oneshot_draw_replacement`; on failure it returns `None`.
    if let Some(effect) = crate::parser::oracle_replacement::parse_oneshot_draw_replacement(lower) {
        return Some(ImperativeFamilyAst::GainKeyword(effect));
    }

    // NOTE: when adding verbs here, also add them to IMPERATIVE_EXTRA_VERBS
    // in game/gap_analysis.rs so the parser gap analyzer can classify them.
    match first_word {
        // ── Unambiguous single-category verbs ──

        // Cost/resource verbs (CR 117-118)
        "pay" | "spend" => {
            parse_cost_resource_ast(text, lower, ctx).map(ImperativeFamilyAst::CostResource)
        }

        // CR 701.10: "double the power/toughness" or "double the number of counters"
        "double" => try_parse_double_effect(lower, ctx).map(ImperativeFamilyAst::GainKeyword),

        // CR 613.4c: "triple target creature's power and toughness" (Tifa's Limit
        // Break — Final Heaven). "Triple" only applies to P/T (no counter/life/mana
        // triple cards exist), so it routes straight to the shared P/T-multiply arm.
        // allow-noncombinator: first-word verb dispatch arm (sibling of "double" above)
        "triple" => try_parse_multiply_pt_effect(lower, ctx).map(ImperativeFamilyAst::GainKeyword),

        // Zone-change/counter verbs (CR 701)
        "destroy" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),
        "exile" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),
        "counter" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),

        // CR 406.3 + CR 701.20a: "turn the exiled card(s) face up" — the Imprint "flip" cards
        // (Clone Shell, Summoner's Egg, Compleated Clone Shell, The Creation of
        // Avacyn). Distinct from the morph/disguise special action ("you may
        // turn this face up"), which is an activated ability parsed elsewhere.
        "turn" | "turns" if nom_primitives::scan_contains(lower, "face up") => {
            // Match ONLY the complete "turn the exiled card(s) face up" clause
            // (anchored, all-consuming). A trailing follow-up sentence — e.g.
            // "... face up. If it's a creature card, put it onto the
            // battlefield under your control." — must be left for the chain's
            // sentence splitter, not swallowed: returning `None` on the
            // combined text lets the splitter break it into separate clauses.
            let matched = all_consuming(terminated(
                preceded(
                    alt((
                        tag::<_, _, OracleError<'_>>("turn the exiled cards"),
                        tag("turn the exiled card"),
                        tag("turn that card"),
                        tag("turns the exiled cards"),
                        tag("turns the exiled card"),
                        tag("turns that card"),
                    )),
                    tag(" face up"),
                ),
                opt(tag(".")),
            ))
            .parse(lower.trim())
            .is_ok();
            if matched {
                return Some(ImperativeFamilyAst::TurnFaceUp {
                    target: TargetFilter::ExiledBySource,
                });
            }

            // CR 708.7 + CR 708.8: General "turn <target> face up" resolving
            // effect — the rules that allow a permanent to be face down may also
            // allow turning it face up (708.7), and as it is turned face up its
            // copiable values revert (708.8). Covers
            // "turn a creature you control face up" (Bustle), "turn target
            // face-down creature face up" (Expose the Culprit), "turn it face
            // up" (Hauntwoods Shrieker's reveal follow-up). Distinct from the
            // morph/disguise/manifest *special action* ("turn this permanent
            // face up"), which is the controller's own special action parsed as
            // an activated ability elsewhere — those self-referential
            // "this"/"~"-subject forms are deliberately NOT matched here. Parse
            // the lowercase form, then slice the original-cased middle for
            // `parse_target`. Anchored/all-consuming on the lowercase clause so
            // a trailing follow-up sentence is left for the chain splitter.
            let text_trim = text.trim();
            let lower_trim = lower.trim();
            // CR 116.2b + CR 708.7: Inside the body of an explicit granted
            // activated ability ("{cost}: Turn this creature face up. ..."),
            // the self-referential turn-up IS the printed resolving effect of
            // that ability — it turns the granted ability's source (the
            // face-down permanent) face up. Etrata, Deadly Fugitive grants
            // face-down creatures a custom-cost version of this. Distinct from
            // the top-level rejection below, which keeps the rule-based
            // morph/disguise special action ("turn this permanent face up")
            // from becoming a spurious resolving effect.
            if ctx.in_granted_activated_ability
                && all_consuming(terminated(
                    preceded(
                        // Self-reference subject — either the `~` normalized
                        // form ("turn ~ face up", dropping the noun) or the
                        // explicit "turn this [creature|permanent] face up".
                        alt((
                            tag::<_, _, OracleError<'_>>("turn ~"),
                            tag("turns ~"),
                            preceded(
                                alt((
                                    tag::<_, _, OracleError<'_>>("turn this "),
                                    tag("turns this "),
                                )),
                                alt((tag("creature"), tag("permanent"))),
                            ),
                        )),
                        tag(" face up"),
                    ),
                    opt(tag(".")),
                ))
                .parse(lower_trim)
                .is_ok()
            {
                return Some(ImperativeFamilyAst::TurnFaceUp {
                    target: TargetFilter::SelfRef,
                });
            }
            // The all-consuming clause extracts the target phrase between the
            // verb prefix and " face up"; any trailing sentence (Hauntwoods's
            // full reveal text) fails it and falls through to the chain splitter.
            let parsed = (|| {
                let (rest, _) = alt((
                    tag::<_, _, OracleError<'_>>("turn "),
                    tag("turns "),
                ))
                .parse(lower_trim)?;
                if alt((
                    tag::<_, _, OracleError<'_>>("this "),
                    tag("~ "),
                ))
                .parse(rest)
                .is_ok()
                {
                    return Err(nom::Err::Error(OracleError::new(
                        rest,
                        nom::error::ErrorKind::Fail,
                    )));
                }
                all_consuming(terminated(
                    take_until::<_, _, OracleError<'_>>(" face up"),
                    preceded(tag(" face up"), opt(tag("."))),
                ))
                .parse(rest)
                .and_then(|(_, mid)| {
                    // Slice the original-cased middle by the byte offsets the
                    // lowercase parse produced. `to_lowercase()` can change byte
                    // length for non-ASCII input (accented card names, smart
                    // quotes), so the offset may not land on a char boundary —
                    // `.get()` returns None instead of panicking, which we map
                    // to a nom error so the clause falls through cleanly.
                    let start = lower_trim.len() - rest.len();
                    text_trim.get(start..start + mid.len()).ok_or_else(|| {
                        nom::Err::Error(OracleError::new(rest, nom::error::ErrorKind::Fail))
                    })
                })
            })();
            match parsed {
                Ok(mid_orig) if !mid_orig.trim().is_empty() => {
                    let (target, _) = parse_target(mid_orig);
                    if matches!(target, TargetFilter::None) {
                        None
                    } else {
                        Some(ImperativeFamilyAst::TurnFaceUp { target })
                    }
                }
                _ => None,
            }
        }

        // CR 708.2a: "Turn <permanent> face down" — a resolving effect that turns
        // the named/targeted face-up permanent(s) face down. This is ALWAYS a
        // resolving spell/ability effect, never a special action: the morph,
        // cloak, and disguise special actions only ever turn a permanent face UP
        // (CR 702.37e / CR 701.58b / CR 702.168d), so "turn ... face down" has no
        // special-action wording to collide with. The subject may therefore be a
        // target (Cyber Conversion — "Turn target creature face down"), a
        // self-reference (Mischievous Quanar / Wall of Deceit — "{cost}: Turn
        // this creature face down"; Obscuring Aether — "Turn this enchantment
        // face down"; Skittish Valesk's coin-flip trigger), or a mass non-target
        // form ("turn all other ... creatures face down"). `parse_target` maps
        // each form, including "~" / "this <type>" → `SelfRef` (resolved at
        // runtime via `resolved_battlefield_object_ids`); self-references are NOT
        // rejected. Mirrors the general face-up extraction: anchored/all-consuming
        // so the combined two-sentence input ("... face down. It's a 2/2 Cyberman
        // artifact creature.") fails here and is left for the chain splitter,
        // which re-attaches the trailing body sentence as a `FaceDownProfileSpec`
        // continuation (CR 205.1a). The seed `Some(vanilla_2_2())` profile
        // (CR 708.2a sentence 1) is overwritten by that continuation when present.
        "turn" | "turns" if nom_primitives::scan_contains(lower, "face down") => {
            let text_trim = text.trim();
            let lower_trim = lower.trim();
            let parsed = (|| {
                let (rest, _) = alt((
                    tag::<_, _, OracleError<'_>>("turn "),
                    tag("turns "),
                ))
                .parse(lower_trim)?;
                all_consuming(terminated(
                    take_until::<_, _, OracleError<'_>>(" face down"),
                    preceded(tag(" face down"), opt(tag("."))),
                ))
                .parse(rest)
                .and_then(|(_, mid)| {
                    // Slice the original-cased middle by the lowercase byte
                    // offsets; `.get()` guards against non-ASCII boundary shifts.
                    let start = lower_trim.len() - rest.len();
                    text_trim.get(start..start + mid.len()).ok_or_else(|| {
                        nom::Err::Error(OracleError::new(rest, nom::error::ErrorKind::Fail))
                    })
                })
            })();
            match parsed {
                Ok(mid_orig) if !mid_orig.trim().is_empty() => {
                    // `parse_target` maps the subject to a `TargetFilter` AND
                    // reports the unconsumed remainder. CR 115.1d: recover the
                    // target-count quantifier ("any number of target", "N target")
                    // separately via the shared subject building block so a plural
                    // subject (Illithid Harvester) surfaces a `MultiTargetSpec`
                    // instead of collapsing to one slot; single subjects (Cyber
                    // Conversion, self-references) yield `None`.
                    let (target, remainder) = parse_target(mid_orig);
                    // The count quantifier is derived purely from the subject's
                    // count phrase, not from enclosing context, so a throwaway
                    // context (mirroring the `parse_target` wrapper above) keeps
                    // the caller's `ctx` diagnostics untouched.
                    let multi_target = super::subject::parse_subject_application(
                        mid_orig,
                        &mut ParseContext::default(),
                    )
                    .and_then(|app| app.multi_target);
                    // CR 608.2b: only emit `TurnFaceDown` when `parse_target`
                    // consumed the ENTIRE subject. A non-empty remainder means an
                    // unmodeled target restriction ("with a morph ability" —
                    // Backslide) was silently dropped, which would illegally widen
                    // the legal-target set (any creature would become a legal
                    // target). Fall through to Unimplemented rather than ship an
                    // over-broad / illegal target.
                    if matches!(target, TargetFilter::None) || !remainder.trim().is_empty() {
                        None
                    } else {
                        Some(ImperativeFamilyAst::TurnFaceDown {
                            target,
                            profile: Some(
                                crate::types::ability::FaceDownProfile::vanilla_2_2(),
                            ),
                            multi_target,
                        })
                    }
                }
                _ => None,
            }
        }

        // Numeric verbs (CR 121)
        "draw" if nom_primitives::scan_contains(lower, "that many") => {
            // "draw that many cards" / "draw that many cards minus one" →
            // EventContextAmount-based quantities, bypassing numeric AST which
            // can only represent fixed u32 counts.
            let count = tag::<_, _, OracleError<'_>>("draw ")
                .parse(lower)
                .ok()
                .and_then(|(tail_lower, _)| {
                    let tail = &text[text.len() - tail_lower.len()..];
                    super::super::oracle_quantity::parse_event_context_quantity(tail)
                })
            .unwrap_or(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            });
            Some(ImperativeFamilyAst::GainKeyword(Effect::Draw {
                count,
                target: TargetFilter::Controller,
            }))
        }
        "draw" => parse_numeric_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast))),
        "scry" | "surveil" | "mill" => parse_numeric_imperative_ast(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast))),

        // Targeted action verbs (CR 701)
        "tap" | "untap" | "sacrifice" | "discard" | "return" | "fight" => {
            parse_targeted_action_ast(text, lower, ctx)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
        }
        // CR 709.5f-g + CR 709.5j: "lock"/"unlock"/"lock or unlock a door of
        // target Room" — the room-door instruction (Ghostly Keybearer, Keys to
        // the House, Marina Vendrell). Routed to `parse_targeted_action_ast`,
        // whose combinator arm also rejects unrelated "lock"/"unlock" lines by
        // returning `None` (it requires the "door of " connective).
        "lock" | "unlock" => parse_targeted_action_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast))),
        "earthbend" | "airbend" => parse_targeted_action_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast))),

        // Search/creation verbs (CR 701.18, CR 111.2)
        "search" | "seek" => parse_search_and_creation_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast))),
        "create" => parse_search_and_creation_ast(text, lower, ctx) // allow-noncombinator: pre-existing match dispatch, only threading ctx through
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast))),

        // Utility verbs (CR 615, CR 701.19, CR 701.6, CR 613.4d)
        "prevent" | "regenerate" | "copy" | "attach" | "unattach" | "switch" => {
            parse_utility_imperative_ast(text, lower, ctx)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)))
        }
        // CR 701.27 + CR 701.28: "transform" and "convert" are equivalent game actions.
        "transform" | "transforms" | "convert" | "converts" => {
            parse_utility_imperative_ast(text, lower, ctx)
                .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Utility(ast)))
        }

        // Shuffle (CR 701.19)
        "shuffle" | "shuffles" => parse_shuffle_ast(text, lower).map(ImperativeFamilyAst::Shuffle),

        // Reveal: "reveal the top N" → Dig (via search path), else hand reveal (CR 701.16, CR 701.20)
        "reveal" | "reveals" => parse_search_and_creation_ast(text, lower, ctx)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast)))
            .or_else(|| {
                parse_hand_reveal_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast)))
            }),

        // Choose (CR 608.2d)
        "choose" | "secretly" => parse_choose_discard_rest_of_hand(text, lower)
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
            .or_else(|| {
                parse_choose_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Choose(ast)))
            }),

        // ── Exact-match keyword actions ──
        "explore" if lower == "explore" || lower == "explore again" => {
            Some(ImperativeFamilyAst::Explore)
        }
        // CR 702.162a + CR 701.50d: "connive" / "connives" — extract optional
        // count ("connive 2", "connives X, where X is …") and target from remainder.
        "connive" | "connives" => {
            let after_verb_lower = &lower[first_word.len()..];
            let rest_lower = after_verb_lower.trim_start();
            let prefix_len = lower.len() - rest_lower.len();
            let rest_orig = text.get(prefix_len..).unwrap_or("");
            let (count, rest_orig) = parse_connive_count_expr(rest_orig, rest_lower);
            if !rest_orig.trim().is_empty() {
                let (target, _) = parse_target(rest_orig.trim());
                Some(ImperativeFamilyAst::GainKeyword(Effect::Connive { target, count }))
            } else if count != (QuantityExpr::Fixed { value: 1 }) {
                Some(ImperativeFamilyAst::GainKeyword(Effect::Connive {
                    target: TargetFilter::Any,
                    count,
                }))
            } else {
                Some(ImperativeFamilyAst::Connive)
            }
        }
        // CR 701.16: "investigate" / third-person "investigates" (e.g. Blink's
        // "Its owner shuffles it into their library, then investigates.").
        "investigate" | "investigates" => Some(ImperativeFamilyAst::Investigate),
        // CR 701.4a: "behold a [quality]" as an EFFECT (Sarkhan, Dragon Ascendant).
        // The "you may" wrapper is peeled by clause_shell into the optional flag and
        // the reminder text is stripped upstream, so the recognizer sees the bare
        // "behold a Dragon". Mirrors `parse_behold_cost`'s quality guard.
        "behold" | "beholds" => parse_behold_effect_ast(text, lower),
        // CR 701.48a: "learn"
        "learn" => Some(ImperativeFamilyAst::Learn),
        // CR 701.62a: "manifest dread" / CR 701.40a: "manifest the top card of your library"
        "manifest" => {
            if tag::<_, _, OracleError<'_>>("manifest dread")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::ManifestDread)
            } else if tag::<_, _, OracleError<'_>>("manifest the top ")
                .parse(lower)
                .is_ok()
            {
                let (_, (target, count, enters_under)) =
                    parse_direct_manifest_clause(lower, ctx).ok()?;
                Some(ImperativeFamilyAst::Manifest {
                    target,
                    count,
                    enters_under,
                })
            } else {
                None
            }
        }
        // CR 701.58a: "cloak the top card of your library" / "cloak the top N
        // cards of [your / that player's] library" (library-top source: Cryptic
        // Coat, Ransom Note) and "cloak a card from your hand" (chosen-object
        // source: Vannifar). The from-hand form lowers to a `ChooseFromZone`
        // parent + `Cloak` sub-chain in `lower_imperative_family_ast`.
        "cloak" | "cloaks" => {
            let trimmed = lower.trim();
            // "cloak a card from your hand" — cloak a card the controller
            // chooses from their hand (the `Some(Zone::Hand)` discriminant).
            let from_hand = all_consuming((
                alt((tag::<_, _, OracleError<'_>>("cloak "), tag("cloaks "))),
                alt((tag("a card"), tag("one card"))),
                tag(" from your hand"),
                opt(tag(".")),
            ))
            .parse(trimmed)
            .is_ok();

            if from_hand {
                Some(ImperativeFamilyAst::Cloak {
                    target: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                    from_zone: Some(Zone::Hand),
                })
            } else {
                let that_player_target = that_player_library_filter(ctx);
                let parsed = all_consuming((
                    alt((
                        tag::<_, _, OracleError<'_>>("cloak the top "),
                        tag("cloaks the top "),
                    )),
                    alt((
                        value(
                            QuantityExpr::Fixed { value: 1 },
                            alt((tag::<_, _, OracleError<'_>>("cards"), tag("card"))),
                        ),
                        terminated(
                            map(nom_primitives::parse_number, |n| QuantityExpr::Fixed {
                                value: n as i32,
                            }),
                            preceded(
                                space1::<_, OracleError<'_>>,
                                alt((tag("cards"), tag("card"))),
                            ),
                        ),
                    )),
                    space1::<_, OracleError<'_>>,
                    alt((
                        value(TargetFilter::Controller, tag("of your library")),
                        value(that_player_target, tag("of that player's library")),
                    )),
                    opt(tag(".")),
                ))
                .parse(trimmed);

                if let Ok((_, (_, count, _, target, _))) = parsed {
                    Some(ImperativeFamilyAst::Cloak {
                        target,
                        count,
                        from_zone: None,
                    })
                } else {
                    None
                }
            }
        }
        "proliferate" => Some(ImperativeFamilyAst::Proliferate),
        // CR 701.56a: "time travel" / "time travel N times"
        "time" => {
            if tag::<_, _, OracleError<'_>>("time travel")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::TimeTravel)
            } else {
                None
            }
        }
        // CR 701.36a: "populate"
        "populate" => Some(ImperativeFamilyAst::Populate),
        // CR 701.30: "clash with an opponent"
        "clash" => {
            if tag::<_, _, OracleError<'_>>("clash with an opponent")
                .parse(lower)
                .is_ok()
            {
                Some(ImperativeFamilyAst::Clash)
            } else {
                None
            }
        }
        // CR 701.60a: "suspect it" / "suspect target creature". Every printed
        // suspect instruction designates a single chosen/anaphoric permanent, so
        // the scope is always `Single`; no card mass-suspects via a population
        // filter today.
        "suspect" | "suspects" => {
            let rest = lower[first_word.len()..].trim();
            let target = if !rest.is_empty() {
                let (t, _) = parse_target(rest);
                t
            } else {
                crate::types::ability::TargetFilter::ParentTarget
            };
            Some(ImperativeFamilyAst::GainKeyword(Effect::Suspect {
                target,
                scope: EffectScope::Single,
            }))
        }
        // CR 701.35a: "detain target creature an opponent controls"
        "detain" | "detains" => {
            let rest = lower[first_word.len()..].trim();
            let target = if !rest.is_empty() {
                let (t, _) = parse_target(rest);
                t
            } else {
                crate::types::ability::TargetFilter::ParentTarget
            };
            Some(ImperativeFamilyAst::GainKeyword(Effect::Detain { target }))
        }
        // Blight N as an effect (e.g. trigger effect "blight 1")
        "blight" => {
            let rest = alt((tag::<_, _, OracleError<'_>>("blight "), tag("blight")))
                .parse(lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            let count = nom_primitives::parse_number
                .parse(rest.trim())
                .map(|(_, n)| n)
                .unwrap_or(1);
            Some(ImperativeFamilyAst::GainKeyword(Effect::BlightEffect {
                count,
                // CR 701.68a: bare "blight N" is the controller blighting.
                player: crate::types::ability::TargetFilter::Controller,
            }))
        }
        // Forage keyword action (CR 701.61a)
        "forage" => Some(ImperativeFamilyAst::GainKeyword(Effect::Forage)),
        // CR 701.64a: "Harness [this permanent]" — always targets the source
        // permanent (normalized to "~"). Guard on the self-reference so the verb
        // does not match unrelated text; harnessing another permanent is not a
        // printed pattern (CR 701.64a only ever harnesses "this permanent").
        "harness" | "harnesses" => {
            let rest = lower[first_word.len()..].trim().trim_end_matches('.').trim();
            (rest.is_empty() || rest == "~")
                .then_some(ImperativeFamilyAst::GainKeyword(Effect::Harness))
        }
        // Collect evidence N keyword action (CR 702.163a)
        "collect" => {
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("collect evidence ").parse(lower)
            {
                let count = nom_primitives::parse_number
                    .parse(rest.trim())
                    .map(|(_, n)| n)
                    .unwrap_or(1);
                Some(ImperativeFamilyAst::GainKeyword(Effect::CollectEvidence {
                    amount: count,
                }))
            } else {
                None
            }
        }
        // Endure N keyword action
        "endure" | "endures" => {
            let (text_pair, where_x) = strip_trailing_where_x(TextPair::new(text, lower));
            let rest = alt((tag::<_, _, OracleError<'_>>("endure "), tag("endures ")))
                .parse(text_pair.lower)
                .map(|(r, _)| r)
                .unwrap_or("");
            let amount = if let Some(expr) = where_x {
                parse_where_x_quantity_expression(&expr)
                    .unwrap_or(QuantityExpr::Fixed { value: 0 })
            } else {
                let count = nom_primitives::parse_number_or_x
                    .parse(rest.trim())
                    .map(|(_, n)| n as i32)
                    .unwrap_or(1);
                QuantityExpr::Fixed { value: count }
            };
            // CR 608.2k: subject-shifted "it endures" in a trigger names the
            // event object (the entering creature), not the ability source.
            let subject = if ctx.subject.is_some() {
                resolve_it_pronoun(ctx)
            } else {
                TargetFilter::SelfRef
            };
            Some(ImperativeFamilyAst::GainKeyword(Effect::Endure { amount, subject }))
        }
        // CR 701.53a: "incubate N"
        "incubate" => try_parse_incubate(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.47a: "amass [Type] N"
        "amass" => try_parse_amass(text, lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.37a: "monstrosity N"
        "monstrosity" => try_parse_monstrosity(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.46a: "adapt N"
        "adapt" => try_parse_adapt(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.39a: "bolster N"
        "bolster" => try_parse_bolster(lower).map(ImperativeFamilyAst::GainKeyword),
        // CR 701.41a: "support N"
        "support" => {
            let (rest, _) = tag::<_, _, OracleError<'_>>("support ")
                .parse(lower)
                .ok()?;
            let rest = rest.trim().trim_end_matches('.');
            let count = nom_primitives::parse_number
                .parse(rest)
                .map(|(_, n)| n)
                .unwrap_or(1);
            // CR 701.41a: On a permanent, Support targets "other" creatures.
            // On an instant/sorcery, it targets any creatures. When parsing within
            // a trigger effect (subject is Some), the card is a permanent.
            let is_other = ctx.subject.is_some();
            Some(ImperativeFamilyAst::Support { count, is_other })
        }
        // CR 508.1d + CR 509.1c: "attacks or blocks this turn/combat if able" —
        // combined forced attack-or-block requirement (Hustle). Tried before the
        // plain attack recognizer since both share the "attacks" first word.
        // CR 508.1d: "attacks/attack this turn/combat if able" — forced attack requirement.
        "attacks" | "attack" => {
            try_parse_attack_or_block_if_able(lower).or_else(|| try_parse_attack_if_able(lower))
        }
        // CR 509.1b / CR 508.1d: "can't be blocked [this turn]", "can't attack", etc.
        // These appear as subjectless clauses in compound effects (e.g., "gets +2/+0 and can't be blocked this turn").
        "can't" | "cannot" => try_parse_subjectless_cant(lower),
        // CR 705: "flip a coin" / "flip N coins" / "flip a coin until you lose a flip"
        "flip" | "flips" => {
            // Longest-match first: "flip a coin until you lose a flip" must
            // precede plain "flip a coin". The N-coin form is tried after
            // "until lose" (since "until lose" always has "a coin"), but
            // before the 1-coin fallback.
            if let Ok((_, ast)) = value::<_, _, OracleError<'_>, _>(
                ImperativeFamilyAst::FlipCoinUntilLose,
                alt((
                    tag::<_, _, OracleError<'_>>("flip a coin until you lose a flip"),
                    tag("flips a coin until they lose a flip"),
                )),
            )
            .parse(lower)
            {
                return Some(ast);
            }
            // CR 705.1 + CR 107.1: "flip N coins" / "flip X coins" — N-coin form.
            if let Some(ast) = try_parse_flip_n_coins(lower) {
                return Some(ast);
            }
            value::<_, _, OracleError<'_>, _>(
                ImperativeFamilyAst::FlipCoin,
                alt((
                    tag::<_, _, OracleError<'_>>("flip a coin"),
                    tag("flips a coin"),
                )),
            )
            .parse(lower)
            .ok()
            .map(|(_, ast)| ast)
        }
        // CR 701.52: "roll to visit your Attractions" (not a generic d20/d6 roll).
        "roll" | "rolls" => {
            if nom_parse_lower(lower, |input| {
                value(
                    ImperativeFamilyAst::RollToVisitAttractions,
                    (
                        alt((tag("roll"), tag("rolls"))),
                        tag(" to visit your attractions"),
                        eof,
                    ),
                )
                .parse(input)
            })
            .is_some()
            {
                Some(ImperativeFamilyAst::RollToVisitAttractions)
            } else if let Some(ast) = try_parse_roll_n_dice(lower) {
                // CR 706.1: "roll two six-sided dice" / "roll X d12" — the
                // multi-dice form. Tried before the single-die path; returns
                // None for count == 1 so "roll a d6" / "roll one d6" falls through.
                Some(ast)
            } else {
                try_parse_roll_die_with_modifier(lower).map(|(sides, modifier)| {
                    ImperativeFamilyAst::RollDie {
                        count: QuantityExpr::Fixed { value: 1 },
                        sides,
                        modifier,
                    }
                })
            }
        }
        // CR 701.51b: "open an Attraction" / "open two Attractions"
        "open" | "opens" => parse_open_attraction_imperative(lower),
        // CR 725.1: "become the monarch"
        "become" | "becomes" => {
            if lower == "become the monarch" || lower == "becomes the monarch" {
                Some(ImperativeFamilyAst::BecomeMonarch)
            } else {
                None
            }
        }
        // CR 701.49: "venture into the dungeon" / "venture into the Undercity"
        "venture" => alt((
            value(
                ImperativeFamilyAst::VentureIntoUndercity,
                tag::<_, _, OracleError<'_>>("venture into the undercity"),
            ),
            value(
                ImperativeFamilyAst::VentureIntoDungeon,
                tag("venture into the dungeon"),
            ),
        ))
        .parse(lower)
        .ok()
        .map(|(_, ast)| ast),
        // CR 701.31c: "planeswalk" — an ability instructs a player to
        // planeswalk (TARDIS, Start the TARDIS, TARDIS Bay). Rules-correct
        // no-op outside a Planechase game (CR 701.31a); handled by
        // effects::planeswalk via game::planechase. The "you may " / "then "
        // prefix and the `optional` flag are already stripped upstream, so the
        // family parser sees only the bare verb body for both the optional
        // (TARDIS, Start the TARDIS) and mandatory (TARDIS Bay) forms. The
        // anchored all-consuming guard prevents matching a longer clause.
        "planeswalk" | "planeswalks" => all_consuming(terminated(
            // Longer alternative first so "planeswalks" matches the full token
            // before the "planeswalk" prefix can short-circuit the `alt`.
            alt((
                tag::<_, _, OracleError<'_>>("planeswalks"),
                tag("planeswalk"),
            )),
            opt(tag(".")),
        ))
        .parse(lower.trim())
        .ok()
        .map(|_| ImperativeFamilyAst::Planeswalk),
        // CR 500.7: "take an extra turn after this one"
        // CR 726.1: "take the initiative"
        "take" | "takes" => {
            if alt((
                value((), tag::<_, _, OracleError<'_>>("take the initiative")),
                value((), tag("takes the initiative")),
            ))
            .parse(lower)
            .is_ok()
            {
                Some(ImperativeFamilyAst::TakeTheInitiative)
            } else if nom_primitives::scan_contains(lower, "extra turn") {
                Some(ImperativeFamilyAst::GainKeyword(Effect::ExtraTurn {
                    target: TargetFilter::Controller,
                }))
            } else {
                None
            }
        }
        // CR 702.26a + CR 702.26c: "phase out" / "phases out" / "phase in" /
        // "phases in" — with optional "target ..." clause. Nom-combinator
        // dispatch on the lowercase input; the target extraction delegates
        // to the shared `parse_target` helper so the full typed filter
        // vocabulary (target creature, each creature you control, etc.) is
        // reused. A leading "~" placeholder (post-subject-strip self-ref)
        // is accepted implicitly: the subject-strip pipeline collapses
        // "~ phases out" to "phases out" before this match runs.
        "phase" | "phases" => {
            // Verb head: "phase out" / "phases out" / "phase in" / "phases in"
            let parsed = alt((
                value(PhaseDir::Out, tag::<_, _, OracleError<'_>>("phase out")),
                value(PhaseDir::Out, tag("phases out")),
                value(PhaseDir::In, tag("phase in")),
                value(PhaseDir::In, tag("phases in")),
            ))
            .parse(lower)
            .ok();

            parsed.map(|(rest, dir)| {
                // Extract optional "target ..." / filter tail. Empty tail =
                // self-reference (the imperative subject handles the
                // attachment); a non-empty tail routes through parse_target
                // for full filter vocabulary.
                let tail = rest.trim_start_matches([' ', ',', '.', ';']).trim();
                let target = if tail.is_empty() {
                    TargetFilter::Any
                } else {
                    let (t, _) = parse_target(tail);
                    t
                };
                match dir {
                    PhaseDir::Out => ImperativeFamilyAst::GainKeyword(Effect::PhaseOut { target }),
                    PhaseDir::In => ImperativeFamilyAst::GainKeyword(Effect::PhaseIn { target }),
                }
            })
        }

        // CR 701.15a: "goad target creature" / "goads target creature" / "goad it"
        "goad" | "goads" => {
            let rest = lower[first_word.len()..].trim();
            if !rest.is_empty() {
                if let Ok((mass_rest, _)) = alt((
                    tag::<_, _, OracleError<'_>>("all "),
                    tag::<_, _, OracleError<'_>>("each "),
                ))
                .parse(rest)
                {
                    let (target, _) = parse_target_with_ctx(mass_rest, ctx);
                    return Some(ImperativeFamilyAst::GainKeyword(Effect::GoadAll {
                        target,
                    }));
                }
                let (target, _) = parse_target_with_ctx(rest, ctx);
                Some(ImperativeFamilyAst::GainKeyword(Effect::Goad { target }))
            } else {
                Some(ImperativeFamilyAst::Goad)
            }
        }

        // CR 701.12a: "exchange control of <two-target-spec>"
        // Two grammatical shapes the parser must extract per-slot filters from:
        //   • Quantified: "two target Xs"        (Switcheroo, Role Reversal)
        //   • Compound:   "target X and target Y" / "target X and another target Y"
        //                                          (Phyrexian Infiltrator, Oko, Trade the Helm)
        //                "this <type> and target Y" / "target X and this <type>"
        //                                          (Avarice Totem, Eyes Everywhere — SelfRef one side)
        // Both shapes lower to ExchangeControl { target_a, target_b }; in the
        // quantified case both filters are identical.
        "exchange" => {
            // CR 701.12a: player-to-player "exchange life totals" (Soul Conduit,
            // Axis of Mortality, Magus of the Mirror, Mirror Universe) — checked
            // before the life-with-stat shape since both begin "exchange life".
            if let Some((player_a, player_b)) = try_parse_exchange_life_totals(lower) {
                return Some(ImperativeFamilyAst::ExchangeLifeTotals { player_a, player_b });
            }
            // CR 701.12a: "exchange <player>'s life total with ~'s power/toughness"
            // (Tree of Perdition, Tree of Redemption, Evra) — checked before
            // "exchange control of" since the two shapes share only the verb.
            if let Some((player, stat)) = try_parse_exchange_life_with_stat(lower) {
                return Some(ImperativeFamilyAst::ExchangeLifeWithStat { player, stat });
            }
            let (rest, _) = tag::<_, _, OracleError<'_>>("exchange control of ")
                .parse(lower)
                .ok()?;
            // Strip trailing terminator from the candidate target span so per-slot
            // parse_target sees clean input (parse_target is whitespace-tolerant
            // but stops on punctuation only via its own grammar).
            let span = rest.trim_end_matches(['.', ';']);
            try_parse_exchange_control_targets(span).map(|(target_a, target_b)| {
                ImperativeFamilyAst::ExchangeControl { target_a, target_b }
            })
        }

        // ── Combat-related ──

        // CR 509.1g: "block [object] this turn/combat if able"
        // Handles: "block this turn if able", "blocks ~ this turn if able",
        // "blocks it this combat if able", "blocks this creature this turn if able"
        "block" | "blocks" => {
            if nom_primitives::scan_contains(lower, "this turn if able")
                || nom_primitives::scan_contains(lower, "this combat if able")
            {
                Some(ImperativeFamilyAst::ForceBlock)
            } else {
                None
            }
        }
        // CR 509.1c: "must be blocked [this turn] [if able]"
        "must" => {
            if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("must be blocked").parse(lower) {
                let rest = rest.trim();
                if rest.is_empty()
                    || rest == "this turn if able"
                    || rest == "if able"
                    || rest == "this turn"
                {
                    return Some(ImperativeFamilyAst::MustBeBlocked);
                }
            }
            None
        }

        // "remove" → "remove from combat" (CR 506.4) → counter removal (step 2)
        "remove" => parse_remove_from_combat_ast(lower, ctx) // allow-noncombinator: pre-existing match dispatch, only threading ctx through
            .map(ImperativeFamilyAst::RemoveFromCombat)
            .or_else(|| {
                parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter)
            }),

        // "move" → counter movement (step 2): "move N counters from X onto Y"
        "move" => parse_zone_counter_ast(text, lower, ctx).map(ImperativeFamilyAst::ZoneCounter),

        // "add" → mana/cost-resource (step 1)
        // allow-noncombinator: pre-existing match dispatch, only threading ctx through
        "add" => parse_cost_resource_ast(text, lower, ctx).map(ImperativeFamilyAst::CostResource),

        // "gain" → "gain control of" (step 4) → "gain life" (step 3) → keyword (step 8)
        // The current if/else chain checks numeric first (step 3), but numeric guards with
        // `contains("gain") && contains("life")`, so "gain control of" never matches numeric.
        // This reordering makes the disambiguation explicit.
        "gain" | "gains" => {
            if nom_primitives::scan_contains(lower, "control of") {
                parse_targeted_action_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Targeted(ast)))
            } else if let Some(effect) = try_parse_gain_all_activated_abilities_of_target(text) {
                // CR 113.1a + CR 611.2: "gains all activated abilities of
                // target <donor> [until end of turn]" (Quicksilver Elemental,
                // Grell Philosopher). Tried BEFORE the bare-keyword/quoted-ability
                // arms so the longer, more specific phrase is not shadowed.
                Some(ImperativeFamilyAst::GainKeyword(effect))
            } else if let Some(effect) = try_parse_gain_this_cards_other_abilities(text) {
                // CR 602.1 + CR 603.1 + CR 611.2a: "gains this card's other
                // abilities" (Symbiote Spider-Man) — the `AllOther`-scope mirror
                // (donor = SelfRef, recipient = ParentTarget). Same precedence
                // rationale as the sibling above.
                Some(ImperativeFamilyAst::GainKeyword(effect))
            } else if let Some(effect) =
                try_parse_gain_keyword(text).or_else(|| try_parse_gain_quoted_ability(text))
            {
                // CR 113.3 + CR 604.1: grant a keyword ability (or, via
                // try_parse_gain_quoted_ability, a quoted ability) to an object.
                // Checked BEFORE the life-gain branch because the bare
                // `scan_contains(lower, "life")` guard below also matches
                // keywords whose name contains "life" — e.g. "gain lifelink",
                // which otherwise misrouted to the numeric life-gain parser and
                // fell through to Unimplemented. `try_parse_gain_keyword`
                // returns `None` unless it actually finds a keyword, so genuine
                // life-gain clauses ("gain 3 life") still fall through below.
                Some(ImperativeFamilyAst::GainKeyword(effect))
            } else if nom_primitives::scan_contains(lower, "life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else {
                None
            }
        }

        // "lose" → "lose the game" (step 6) → "lose all counters" (step 6.5)
        //       → "lose life" (step 3) → keyword (step 7)
        "lose" | "loses" => {
            if nom_primitives::scan_contains(lower, "the game") {
                Some(ImperativeFamilyAst::LoseTheGame)
            } else if let Some(effect) = try_parse_lose_all_player_counters(text, lower) {
                // CR 122.1: Player-scoped "lose all counters" —
                // Suncleanser ("target opponent loses all counters") and
                // Final Act mode 5 ("each opponent loses all counters"). The
                // `each opponent` subject is already stripped upstream via
                // `strip_each_player_subject`, leaving `lose all counters` to
                // dispatch here; `target opponent loses all counters` retains
                // its target for the parse_target call below.
                Some(ImperativeFamilyAst::GainKeyword(effect))
            } else if nom_primitives::scan_contains(lower, "life") {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            } else if !nom_primitives::scan_contains(lower, "mana") {
                try_parse_gain_keyword(text).map(ImperativeFamilyAst::LoseKeyword)
            } else {
                None
            }
        }

        // CR 104.3a: "win the game"
        "win" | "wins" => {
            if nom_primitives::scan_contains(lower, "the game") {
                Some(ImperativeFamilyAst::WinTheGame)
            } else {
                None
            }
        }

        // "look" → exiled/hand targets (step 4) → "look at the top" (step 5)
        "look" => parse_hand_reveal_ast(text, lower, ctx) // allow-noncombinator: pre-existing match dispatch; exiled-card look must precede library-top search
            .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::HandReveal(ast)))
            .or_else(|| {
                parse_search_and_creation_ast(text, lower, ctx)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(ast)))
            }),

        // "gets"/"get" → try player counter first, then the subjectless
        // pump+keyword coalescer (CR 611.2c: keeps "gain haste" attached to the
        // P/T pump as one GenericEffect), then bare numeric pump (step 3). Pure
        // "get +N/+M" bodies coalesce to None and fall through to the numeric arm.
        "gets" | "get" => try_parse_player_counter(lower)
            .or_else(|| coalesce_pump_with_modifications(text).map(ImperativeFamilyAst::GainKeyword))
            .or_else(|| {
                parse_numeric_imperative_ast(text, lower)
                    .map(|ast| ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)))
            }),

        // "deals"/"deal" → damage (via cost_resource, which contains try_parse_damage)
        "deals" | "deal" => {
            parse_cost_resource_ast(text, lower, ctx).map(ImperativeFamilyAst::CostResource)
        }

        // "you may" → optional wrapper
        "you" => nom_on_lower(text, lower, |input| value((), tag("you may ")).parse(input)).map(
            |(_, stripped)| ImperativeFamilyAst::YouMay {
                text: stripped.to_string(),
            },
        ),

        // "may" → optional wrapper (produced after strip_each_player_subject strips "each player ")
        // e.g. "Each player may discard their hand" → subject stripped → "may discard their hand"
        "may" => nom_on_lower(text, lower, |input| value((), tag("may ")).parse(input)).map(
            |(_, stripped)| ImperativeFamilyAst::YouMay {
                text: stripped.to_string(),
            },
        ),

        // Unknown first word — try position-agnostic parsers that use `contains`/`find`
        // rather than `starts_with`. This handles cases where the verb isn't the first
        // word (e.g., "Lightning Bolt deals 3 damage" after failed subject stripping,
        // "each player gains 2 life" where "each" isn't a verb, or
        // "that player shuffles" where "that" precedes the verb).
        _ => {
            // Damage: try_parse_damage uses lower.find("deals ") — matches anywhere
            if let Some(ast) = parse_cost_resource_ast(text, lower, ctx) {
                return Some(ImperativeFamilyAst::CostResource(ast));
            }
            // CR 611.2c: subjectless pump+keyword body ("get +2/+0 and gain
            // haste ...") — coalesce into one GenericEffect so the keyword grant
            // is not dropped by the bare-Pump numeric arm. Pure pump → None.
            if let Some(effect) = coalesce_pump_with_modifications(text) {
                return Some(ImperativeFamilyAst::GainKeyword(effect));
            }
            // Numeric: contains("gain")+contains("life"), contains("gets +"), etc.
            if let Some(ast) = parse_numeric_imperative_ast(text, lower) {
                return Some(ImperativeFamilyAst::Structured(ImperativeAst::Numeric(ast)));
            }
            // Shuffle: "that player shuffles" / "target player shuffles" have
            // non-verb first words but are exact-match shuffle patterns
            if let Some(ast) = parse_shuffle_ast(text, lower) {
                return Some(ImperativeFamilyAst::Shuffle(ast));
            }
            None
        }
    }
}

/// CR 701.12a: Parse "exchange <player>'s life total with ~'s power/toughness"
/// (Tree of Perdition, Tree of Redemption, Evra, Halcyon Witness) into the
/// exchanged player filter and the source stat. Returns `None` for any other
/// "exchange" shape so the caller falls through to "exchange control of".
///
/// The whole clause must be consumed (only a trailing terminator may remain) so
/// unrelated "exchange" phrasings don't match partially.
fn try_parse_exchange_life_with_stat(lower: &str) -> Option<(TargetFilter, PtStat)> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("exchange ")
        .parse(lower)
        .ok()?;
    let (rest, player) = parse_exchange_life_player(rest)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("life total with ")
        .parse(rest)
        .ok()?;
    // Source possessive: "~'s " (self-reference normalization) or the literal
    // "this creature's " form, both naming the ability's source permanent.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("~'s "),
        tag("this creature's "),
    ))
    .parse(rest)
    .ok()?;
    let (rest, stat) = alt((
        value(PtStat::Toughness, tag::<_, _, OracleError<'_>>("toughness")),
        value(PtStat::Power, tag("power")),
    ))
    .parse(rest)
    .ok()?;
    if !rest
        .trim_start()
        .trim_end_matches(['.', ';'])
        .trim()
        .is_empty()
    {
        return None;
    }
    Some((player, stat))
}

/// CR 119: Parse the player whose life total is exchanged. "your" binds to the
/// ability's controller (no target); "target opponent's" / "target player's"
/// declare a player target. Returns the filter and the remaining text after the
/// possessive.
fn parse_exchange_life_player(input: &str) -> Option<(&str, TargetFilter)> {
    alt((
        value(
            TargetFilter::Controller,
            tag::<_, _, OracleError<'_>>("your "),
        ),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
            tag("target opponent's "),
        ),
        value(TargetFilter::Player, tag("target player's ")),
    ))
    .parse(input)
    .ok()
}

/// CR 701.12a: Player-to-player "exchange life totals", verb-initial shape:
/// "exchange life totals with target opponent" / "... target player" — the
/// controller (`Controller`) exchanges with that opponent/player (Magus of the
/// Mirror, Mirror Universe). The subject-initial "two target players exchange
/// life totals" shape (Soul Conduit, Axis of Mortality) is intercepted earlier
/// in `parse_imperative_family_ast` because its first word is not a verb.
/// Returns `(player_a, player_b)` or `None` for unrecognised shapes.
fn try_parse_exchange_life_totals(lower: &str) -> Option<(TargetFilter, TargetFilter)> {
    // "exchange life totals with <target opponent | target player>".
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("exchange life totals with ").parse(lower) {
        let (rest, other) = alt((
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag::<_, _, OracleError<'_>>("target opponent"),
            ),
            value(TargetFilter::Player, tag("target player")),
        ))
        .parse(rest)
        .ok()?;
        if rest.trim().trim_end_matches(['.', ';']).trim().is_empty() {
            return Some((TargetFilter::Controller, other));
        }
    }

    None
}

/// CR 701.12a: Extract the two per-slot target filters from the "<...>" body of
/// "exchange control of <...>". Returns `None` for unrecognised shapes so the
/// caller can fall through (no Effect is emitted) rather than silently dropping
/// targets into a bare ExchangeControl.
///
/// Two grammatical shapes are recognised:
/// 1. Quantified: "two target Xs" — both slot filters are identical. Driven by
///    `parse_target`'s built-in "two " quantifier handling, which consumes the
///    count word and returns a single filter.
/// 2. Compound: "<slot> and <slot>" — each slot is parsed independently. A slot
///    is either a "target …" phrase (with optional "another"/"other", delegated
///    to `parse_target`) or "this <type>" (Avarice Totem, Eyes Everywhere,
///    Phyrexian Infiltrator — lowered to `SelfRef`).
///
/// Each per-slot parse must consume its entire substring (no trailing remainder)
/// so we don't accept malformed inputs like "target creature and dance" as a
/// valid two-target phrase.
fn try_parse_exchange_control_targets(span: &str) -> Option<(TargetFilter, TargetFilter)> {
    // CR 601.2c + CR 115.1: a trailing "controlled by different players"
    // (Kitsune, Dragon's Daughter: "exchange control of two other target
    // creatures controlled by different players") is a target-SET constraint
    // (`TargetSelectionConstraint::DifferentObjectControllers`), extracted
    // separately in the chain lowerer — it is not part of either per-slot filter.
    // Strip it via the shared `strip_controlled_by_different_players` combinator
    // (single source of truth with the detector
    // `parse_controlled_by_different_players_target_constraint`) so the per-slot
    // `parse_target` sees a clean "two other target creatures" span.
    let span = super::lower::strip_controlled_by_different_players(span).unwrap_or(span);
    // Quantified shape: "two target Xs" dispatched via nom. We peek for the
    // `"two target "` prefix with `alt((tag(...), tag(...)))` (plural handled by
    // `parse_target`'s QUANTIFIED_PREFIXES), then re-enter `parse_target` on the
    // full span so its quantifier path runs and returns a single filter that
    // applies to both slots.
    if alt((
        tag::<_, _, OracleError<'_>>("two target "),
        tag("two other target "),
        tag("two another target "),
    ))
    .parse(span)
    .is_ok()
    {
        let (filter, remainder) = parse_target(span);
        if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
            return Some((filter.clone(), filter));
        }
    }

    // Compound shape: locate the top-level " and " connective with nom's
    // `take_until` (a combinator, not string-dispatch), then delegate each side
    // to `parse_exchange_slot`. `take_until` is structural — it splits the
    // span; all dispatch decisions happen inside `parse_exchange_slot` via nom.
    //
    // ASSUMPTION: Exchange-control slots do NOT contain an internal " and "
    // (e.g. no "creature with flying and first strike and target creature" in
    // printed Oracle text for this effect). `take_until` is first-occurrence
    // greedy, so a slot with internal " and " would misfire. No current card
    // triggers this; if such text appears, switch to a right-anchored split
    // or recognise per-slot terminators.
    let (right, (left, _)) =
        nom::sequence::pair(take_until::<_, _, OracleError<'_>>(" and "), tag(" and "))
            .parse(span)
            .ok()?;
    let target_a = parse_exchange_slot(left.trim())?;
    let target_b = parse_exchange_slot(right.trim())?;
    Some((target_a, target_b))
}

/// Parse a single exchange-control slot phrase. Returns the slot filter, or
/// `None` if the phrase isn't a recognised slot. The slot must be fully
/// consumed — a trailing remainder indicates the caller handed us malformed
/// input and we must fall through rather than silently accepting a partial
/// parse.
fn parse_exchange_slot(phrase: &str) -> Option<TargetFilter> {
    // Self-referential slot dispatch via nom: "this <type>" refers to the
    // source permanent and resolves to SelfRef regardless of the type word
    // (artifact, creature, enchantment …).
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("this ").parse(phrase) {
        if !rest.trim().is_empty() {
            return Some(TargetFilter::SelfRef);
        }
    }

    // Standard target slot: "target …" / "another target …" / "other target …".
    // parse_target absorbs all "target"/"another target"/"other target" prefixes.
    let (filter, remainder) = parse_target(phrase);
    if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        return Some(filter);
    }
    None
}

/// CR 122.1: Parse "lose(s) all counters" / "target opponent loses
/// all counters" / "lose all counters" into an `Effect::LoseAllPlayerCounters`.
///
/// Two shapes are handled here:
/// 1. Bare predicate: "lose all counters" / "loses all counters" — the
///    "each opponent" / "each player" subject has already been stripped by
///    `strip_each_player_subject`, and the outer `player_scope` drives
///    per-player iteration. Target defaults to `Controller` so the iterator
///    addresses the iterating player (CR 608.2 player_scope rebinding).
/// 2. Explicit target: "target opponent loses all counters" /
///    "target player loses all counters" — `parse_target` lifts the typed
///    filter out of the subject; the effect resolves against that chosen
///    player.
fn try_parse_lose_all_player_counters(text: &str, lower: &str) -> Option<Effect> {
    // Case 1: bare predicate after subject-strip — "lose all counters" /
    // "loses all counters" (trailing period already stripped by the dispatch).
    let bare = lower.trim().trim_end_matches('.').trim();
    let bare_tail = alt((
        tag::<_, _, OracleError<'_>>("loses all counters"),
        tag("lose all counters"),
    ))
    .parse(bare);
    if let Ok((rest, _)) = bare_tail {
        if rest.trim().is_empty() {
            return Some(Effect::LoseAllPlayerCounters {
                target: TargetFilter::Controller,
            });
        }
    }

    // Case 2: explicit subject — "target opponent loses all counters" /
    // "target player loses all counters". Strip the " loses all counters" /
    // " lose all counters" suffix (structural slice of a known trailing
    // literal, not parsing dispatch), then hand the subject prefix to
    // `parse_target`.
    let trimmed = text.trim_end_matches('.').trim();
    let trimmed_lower = trimmed.to_lowercase();
    let subject_len = trimmed_lower
        .strip_suffix(" loses all counters")
        .or_else(|| trimmed_lower.strip_suffix(" lose all counters"))
        .map(str::len)?;
    let subject = trimmed[..subject_len].trim();
    let (filter, remainder) = parse_target(subject);
    if remainder.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        return Some(Effect::LoseAllPlayerCounters { target: filter });
    }

    None
}

/// CR 107.17: Count a leading run of consecutive `{tk}` ticket symbols, each of
/// which represents one ticket counter. Returns the count when the input begins
/// with at least one `{tk}` glyph and the only remaining text is a sentence
/// terminator (so "{tk}{tk}" and "{tk}{tk}." match, but "{tk} for each ..." or
/// a `{tk}` activation cost does not — those carry trailing clauses this player
/// counter parser must not swallow). Mirrors the `{tk}`-counting idiom in
/// `oracle_keyword::strip_ticket_activation_cost_prefix`.
fn count_ticket_symbols(rest: &str) -> Option<u32> {
    let mut remaining = rest;
    let mut count = 0u32;
    while let Ok((next, _)) = tag::<_, _, OracleError<'_>>("{tk}").parse(remaining) {
        count += 1;
        remaining = next;
    }
    if count == 0 {
        return None;
    }
    // Only a trailing terminator may remain; any other text means this is not a
    // bare "you get {TK}…" instruction (e.g. an activation cost or larger clause).
    let tail = remaining
        .trim_start()
        .trim_end_matches(['.', ';', ','])
        .trim();
    tail.is_empty().then_some(count)
}

/// CR 122.1: Parse "get/gets a/an/N [type] counter(s)" into a GivePlayerCounter AST.
/// Handles patterns like:
/// - "get a poison counter"
/// - "gets two experience counters"
/// - "get ten rad counters"
/// - "get {TK}{TK}" (CR 107.17 ticket symbol form; see `count_ticket_symbols`)
pub(super) fn try_parse_player_counter(lower: &str) -> Option<ImperativeFamilyAst> {
    // Strip "get/gets " prefix
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("gets "), tag("get ")))
        .parse(lower)
        .ok()?;

    // CR 107.17: The ticket symbol is {TK}; it represents one ticket counter.
    // Unfinity cards write "you get N ticket counters" as N repeated `{TK}`
    // glyphs (e.g. "you get {TK}{TK}{TK}"). Each glyph is one ticket counter, so
    // count the run of consecutive `{tk}` symbols and emit a Ticket player
    // counter of that size. This must precede the word-form "counter(s)" suffix
    // check below because the symbol form carries no "counter" noun. The branch
    // returns None on the no-symbol case and falls through to the word form.
    if let Some(count) = count_ticket_symbols(rest) {
        return Some(ImperativeFamilyAst::GivePlayerCounter {
            counter_kind: PlayerCounterKind::Ticket,
            count: QuantityExpr::Fixed {
                value: count as i32,
            },
        });
    }

    // Must end with "counter" or "counters"
    let (before_counter, plural) = if let Some(s) = rest.strip_suffix(" counters") {
        (s, true)
    } else {
        let s = rest.strip_suffix(" counter")?;
        (s, false)
    };

    // Parse quantity + counter kind from the remaining text.
    // Patterns: "that many poison" / "a poison" / "an experience" / "two rad" / "10 poison"
    // CR 608.2h: "that many/much <kind> counters" binds the triggering event's
    // amount (e.g. Etali, Primal Sickness — combat damage → that many poison
    // counters); the amount is determined once, when the effect is applied.
    let (count, counter_kind): (QuantityExpr, &str) =
        if let Ok((rest, qty)) = nom_quantity::parse_that_much_or_many(before_counter) {
            (QuantityExpr::Ref { qty }, rest.trim())
        } else if let Ok((kind, _)) = nom_primitives::parse_article.parse(before_counter) {
            (QuantityExpr::Fixed { value: 1 }, kind.trim())
        } else if let Ok((rest, n)) = nom_primitives::parse_number.parse(before_counter) {
            (QuantityExpr::Fixed { value: n as i32 }, rest.trim())
        } else {
            return None;
        };

    // Validate: counter kind should be a single word (no spaces) to avoid false positives
    // like "gets +1/+1 counter" which is an object counter, not a player counter.
    if counter_kind.is_empty() || counter_kind.contains('+') || counter_kind.contains('-') {
        return None;
    }

    // CR 122.1: Map to typed PlayerCounterKind — reject anything that's an object
    // counter. Energy counters are NOT included (they use the dedicated
    // GainEnergy effect). Single authority: `nom_primitives::parse_player_counter_kind`,
    // shared with the oracle_nom threshold rider. `all_consuming` enforces that
    // the whole kind word is a recognized player counter (so "poisonx" is
    // rejected rather than matching the "poison" prefix).
    let (_, kind) = all_consuming(nom_primitives::parse_player_counter_kind)
        .parse(counter_kind)
        .ok()?;

    let _ = plural; // plural is just grammatical, doesn't affect semantics
    Some(ImperativeFamilyAst::GivePlayerCounter {
        counter_kind: kind,
        count,
    })
}

/// CR 706: Parse die side count from "roll a dN" / "roll a six-sided die" patterns.
/// CR 705.1 + CR 107.1: Parse "flip N coins" / "flip X coins" / "flip two coins" —
/// the N-coin form. Delegates the count to `parse_count_expr`, covering digit,
/// word-number, and `X` forms uniformly. Returns None for "flip a coin" / "flip one coin"
/// so the caller falls back to `FlipCoin` (the existing 1-flip shape).
fn try_parse_flip_n_coins(lower: &str) -> Option<ImperativeFamilyAst> {
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("flip "), tag("flips ")))
        .parse(lower)
        .ok()?;

    let (expr, after) = parse_count_expr(rest)?;

    // `parse_count_expr` returns the remainder with leading whitespace trimmed
    // ("five coins" → rest = "coins"), so we match "coins"/"coin" without the
    // leading space. Word-boundary termination rejects "coinsomething".
    // "flip a coin" is handled by FlipCoin; "flip one coin" would semantically
    // match but is never printed.
    let (after_noun, _) = alt((tag::<_, _, OracleError<'_>>("coins"), tag("coin")))
        .parse(after)
        .ok()?;
    // structural: not dispatch — checks that the next char is a non-alphanumeric boundary.
    if !after_noun.is_empty() && !after_noun.starts_with(|c: char| !c.is_alphanumeric()) {
        return None;
    }

    // Reject count == 1 so "flip 1 coin" (if ever printed) prefers FlipCoin.
    if matches!(expr, QuantityExpr::Fixed { value: 1 }) {
        return None;
    }

    Some(ImperativeFamilyAst::FlipCoins { count: expr })
}

/// CR 706.1: Parse "roll N six-sided dice" / "roll X d12" / "roll two d6" —
/// the multi-dice form. Mirrors `try_parse_flip_n_coins`: strip the
/// "roll "/"rolls " prefix, take the count via `parse_count_expr` (digit,
/// word-number, and `X` forms), then parse the die size from the remainder.
/// Returns None for count == 1 so "roll a d6" / "roll one d6" falls through
/// to the existing single-die path.
fn try_parse_roll_n_dice(lower: &str) -> Option<ImperativeFamilyAst> {
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("roll "), tag("rolls ")))
        .parse(lower)
        .ok()?;

    // `parse_count_expr` returns the remainder with leading whitespace trimmed
    // ("two six-sided dice" → after = "six-sided dice").
    let (expr, after) = parse_count_expr(rest)?;

    // CR 706.1: Reject count == 1 so "roll a d6" / "roll one d6" prefers the
    // single-die `RollDie { count: Fixed(1), .. }` path.
    if matches!(expr, QuantityExpr::Fixed { value: 1 }) {
        return None;
    }

    let (sides, rest_after_sides) = parse_die_sides_with_rest(after)?;
    // CR 706.1: The remainder must be only the plural/singular die noun
    // (possibly with trailing punctuation) — a wider clause means this isn't a
    // bare multi-dice roll, so fall through to higher-level chain parsing.
    let rest_after_sides = rest_after_sides
        .trim_start()
        .trim_end_matches(['.', ',', ';'])
        .trim();
    if !rest_after_sides.is_empty() {
        return None;
    }

    Some(ImperativeFamilyAst::RollDie {
        count: expr,
        sides,
        // CR 706.2: multi-dice forms with a result-shifting modifier are
        // vanishingly rare and parsed via the table path; the bare form has
        // no modifier.
        modifier: None,
    })
}

/// CR 706.1a: Returns `(sides, remainder)`. The remainder is the slice immediately after
/// the consumed die phrase, with whitespace untrimmed. Callers needing to
/// attach trailing modifiers / clauses can branch on the remainder shape.
fn try_parse_roll_die_sides_with_rest(lower: &str) -> Option<(u8, &str)> {
    // Strip the single-die article/count prefix.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("roll a "),
        tag("rolls a "),
        tag("roll one "),
        tag("rolls one "),
    ))
    .parse(lower)
    .ok()?;
    parse_die_sides_with_rest(rest)
}

/// CR 706.1a: Parse a die size (`dN` numeric form or `N-sided`/word form) from
/// the text immediately following the count/article, returning `(sides,
/// remainder)`. Shared by the single-die (`roll a dN`) and multi-dice (`roll N
/// dN`) parsers so both pick up every die-size spelling uniformly. The
/// trailing " die"/" dice" noun is consumed when present.
fn parse_die_sides_with_rest(rest: &str) -> Option<(u8, &str)> {
    // CR 706.1a: Numeric form — consume "d" followed by the longest run of
    // ASCII digits. This permits trailing text like " and add the number of
    // cards in your hand" or terminating punctuation.
    if let Ok((after_d, _)) = tag::<_, _, OracleError<'_>>("d").parse(rest) {
        let digit_end = after_d
            .bytes()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(after_d.len());
        if digit_end > 0 {
            if let Ok(sides) = after_d[..digit_end].parse::<u8>() {
                return Some((sides, &after_d[digit_end..]));
            }
        }
    }
    // CR 706.1a: Word-form — "six-sided die", "four-sided die", etc.
    let (after_word, sides) = alt((
        value(
            4_u8,
            alt((tag::<_, _, OracleError<'_>>("four-sided"), tag("4-sided"))),
        ),
        value(6, alt((tag("six-sided"), tag("6-sided")))),
        value(8, alt((tag("eight-sided"), tag("8-sided")))),
        value(10, alt((tag("ten-sided"), tag("10-sided")))),
        value(12, alt((tag("twelve-sided"), tag("12-sided")))),
        value(20, alt((tag("twenty-sided"), tag("20-sided")))),
    ))
    .parse(rest)
    .ok()?;
    // Consume the optional " dice"/" die" noun so it doesn't leak into the
    // modifier-detection path. " dice" (plural, multi-dice form) is tried
    // before " die" so it isn't mis-consumed as " die" + "ce". Tolerant of
    // absence ("roll a six-sided").
    let after_word = alt((
        value((), tag::<_, _, OracleError<'_>>(" dice")),
        value((), tag(" die")),
        value((), tag("")),
    ))
    .parse(after_word)
    .ok()
    .map(|(rest, _)| rest)
    .unwrap_or(after_word);
    Some((sides, after_word))
}

/// CR 701.51b: "open N attraction(s)" after the open/opens prefix.
fn parse_open_attractions_count_imperative(input: &str) -> OracleResult<'_, ImperativeFamilyAst> {
    let (rest, count) = nom_primitives::parse_number(input)?;
    let (rest, _) = space1(rest)?;
    let (rest, _) = alt((tag("attractions"), tag("attraction"))).parse(rest)?;
    Ok((rest, ImperativeFamilyAst::OpenAttractions { count }))
}

/// CR 701.51b: "open an Attraction" / "open two Attractions".
fn parse_open_attraction_imperative(lower: &str) -> Option<ImperativeFamilyAst> {
    nom_parse_lower(lower, |input| {
        map(
            (
                alt((tag("open "), tag("opens "))),
                alt((
                    value(
                        ImperativeFamilyAst::OpenAttractions { count: 1 },
                        tag("an attraction"),
                    ),
                    value(
                        ImperativeFamilyAst::OpenAttractions { count: 1 },
                        tag("a attraction"),
                    ),
                    parse_open_attractions_count_imperative,
                )),
                opt(nom::bytes::complete::take_while(|c: char| {
                    c == '.' || c == ','
                })),
                eof,
            ),
            |(_, ast, _, _)| ast,
        )
        .parse(input)
    })
}

/// Unstable Contraptions: "assemble a Contraption" / "assembles two
/// Contraptions" / "assemble X plus one Contraptions".
fn parse_assemble_contraption_imperative(lower: &str) -> Option<ImperativeFamilyAst> {
    nom_parse_lower(lower, |input| {
        map(
            all_consuming(terminated(
                preceded(
                    opt(alt((tag::<_, _, OracleError<'_>>("~ "), tag("it ")))),
                    preceded(
                        alt((tag::<_, _, OracleError<'_>>("assemble "), tag("assembles "))),
                        parse_assemble_contraption_count,
                    ),
                ),
                opt(tag(".")),
            )),
            |count| match count {
                ContraptionAssembleCount::Quantity(count) => {
                    ImperativeFamilyAst::AssembleContraptions { count }
                }
                ContraptionAssembleCount::FromRollDifference => {
                    ImperativeFamilyAst::AssembleContraptionsFromRollDifference
                }
            },
        )
        .parse(input)
    })
}

#[derive(Clone)]
enum ContraptionAssembleCount {
    Quantity(QuantityExpr),
    FromRollDifference,
}

fn parse_assemble_contraption_count(input: &str) -> OracleResult<'_, ContraptionAssembleCount> {
    alt((
        value(
            ContraptionAssembleCount::FromRollDifference,
            (
                tag::<_, _, OracleError<'_>>("a number of contraptions equal to the "),
                tag("difference between those results"),
            ),
        ),
        value(
            ContraptionAssembleCount::Quantity(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
            (
                tag::<_, _, OracleError<'_>>("a number of contraptions equal to the "),
                tag("result"),
            ),
        ),
        map(
            preceded(
                tag::<_, _, OracleError<'_>>("a contraption for each "),
                nom_quantity::parse_for_each_clause_ref,
            ),
            |qty| ContraptionAssembleCount::Quantity(QuantityExpr::Ref { qty }),
        ),
        value(
            ContraptionAssembleCount::Quantity(QuantityExpr::Fixed { value: 1 }),
            tag::<_, _, OracleError<'_>>("a contraption"),
        ),
        value(
            ContraptionAssembleCount::Quantity(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                }),
                offset: 1,
            }),
            tag::<_, _, OracleError<'_>>("x plus one contraptions"),
        ),
        value(
            ContraptionAssembleCount::Quantity(QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }),
            tag::<_, _, OracleError<'_>>("x contraptions"),
        ),
        map(
            terminated(nom_primitives::parse_number, tag(" contraptions")),
            |count| {
                ContraptionAssembleCount::Quantity(QuantityExpr::Fixed {
                    value: count as i32,
                })
            },
        ),
    ))
    .parse(input)
}

/// Unstable Contraptions: "reassemble target Contraption you control" / "it
/// reassembles target Contraption that player controls".
fn parse_reassemble_contraption_imperative(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ImperativeFamilyAst> {
    let text = text.trim_start();
    let lower = lower.trim_start();
    let (rest, control_mode) = preceded(
        opt(alt((tag::<_, _, OracleError<'_>>("~ "), tag("it ")))),
        alt((
            value(
                ReassembleControlMode::KeepController,
                tag::<_, _, OracleError<'_>>("reassemble "),
            ),
            value(ReassembleControlMode::GainControl, tag("reassembles ")),
        )),
    )
    .parse(lower)
    .ok()?;
    let consumed = lower.len() - rest.len();
    let after_verb = text.get(consumed..)?;
    let (target, rest, _) = parse_target_with_syntax(after_verb, ctx);
    if matches!(target, TargetFilter::Any)
        || all_consuming(terminated(space0::<_, OracleError<'_>>, opt(tag("."))))
            .parse(rest)
            .is_err()
    {
        return None;
    }
    Some(ImperativeFamilyAst::ReassembleContraption {
        target,
        control_mode,
    })
}

/// CR 706 + CR 706.2: Try to parse a full `"roll a d{N}"` clause, including
/// an optional trailing `" and (add|subtract) {quantity}"` modifier that the
/// resolver applies to the natural roll before result-table lookup.
///
/// Returns `(sides, modifier)` on success. The modifier is `None` when the
/// remainder is empty or only contains trailing punctuation; otherwise the
/// remainder must shape as a recognized add/subtract clause.
fn try_parse_roll_die_with_modifier(
    lower: &str,
) -> Option<(u8, Option<crate::types::ability::DieRollModifier>)> {
    let (sides, rest) = try_parse_roll_die_sides_with_rest(lower)?;
    let rest = rest.trim_end_matches(['.', ',', ';']).trim();
    if rest.is_empty() {
        return Some((sides, None));
    }
    // Modifier shapes: "and add X", "and subtract X". Anything else means the
    // clause is wider than just a roll — let the dispatch fall through to
    // higher-level chain parsing (e.g., "roll a d20 for each player").
    let (after_and, _) = tag::<_, _, OracleError<'_>>("and ").parse(rest).ok()?;
    let (modifier_text, sign) = alt((
        value(true, tag::<_, _, OracleError<'_>>("add ")),
        value(false, tag("subtract ")),
    ))
    .parse(after_and)
    .ok()?;
    let (_, value) = nom_quantity::parse_quantity_ref_complete(modifier_text).ok()?;
    let modifier = if sign {
        crate::types::ability::DieRollModifier::Add {
            value: crate::types::ability::QuantityExpr::Ref { qty: value },
        }
    } else {
        crate::types::ability::DieRollModifier::Subtract {
            value: crate::types::ability::QuantityExpr::Ref { qty: value },
        }
    };
    Some((sides, Some(modifier)))
}

/// CR 706.2: Try to parse a d20 result table line like "1—9 | Draw two cards",
/// "20 | Search your library for a card", or "15+ | Scry X, then draw X cards".
/// Returns `(min, max, effect_text)`. Open-ended upper bounds ("15+") set
/// `max = u8::MAX` so any modifier-boosted roll above the printed lower bound
/// resolves to this branch — see CR 706.2 on modifier-shifted results
/// (Diviner's Portent, Gale's Redirection, etc.).
pub(crate) fn try_parse_die_result_line(text: &str) -> Option<(u8, u8, &str)> {
    let trimmed = text.trim();

    // Find the pipe separator: "N—M | effect", "N+ | effect", or "N | effect"
    let (_, (range_part, effect_text)) = nom_primitives::split_once_on(trimmed, " | ").ok()?;
    let range_part = range_part.trim();
    let effect_text = effect_text.trim();

    // Parse range: "1—9" (em dash U+2014), "10—19", "15+" (open-ended upper),
    // or "20" (single value).
    let (min, max) = if let Some(dash_idx) = range_part.find('\u{2014}') {
        let min_str = &range_part[..dash_idx];
        let max_str = &range_part[dash_idx + '\u{2014}'.len_utf8()..];
        (min_str.parse::<u8>().ok()?, max_str.parse::<u8>().ok()?)
    // allow-noncombinator: CR 706.2 "N+" open-ended upper bound — single-char structural suffix on a pre-tokenized numeric range slice; the surrounding nom split already isolated `range_part` off the pipe delimiter (Pattern 3 in PATTERNS.md).
    } else if let Some(min_str) = range_part.strip_suffix('+') {
        (min_str.trim().parse::<u8>().ok()?, u8::MAX)
    } else {
        // Single value like "20"
        let val = range_part.parse::<u8>().ok()?;
        (val, val)
    };

    Some((min, max, effect_text))
}

/// CR 705: Try to parse "if you win the flip, [effect]" / "if you lose the flip,
/// [effect]" from Oracle text. Returns `(is_win, effect_text)`.
///
/// Only the "if you win/lose the flip" wording is matched here. This phrasing is
/// part of the same one-shot resolution as the flip itself (Krark, the
/// Thumbless), so it folds into the preceding `FlipCoin`'s
/// `win_effect`/`lose_effect` and resolves inline.
///
/// The superficially-similar "WHEN you win/lose the flip, [effect]" wording
/// (Breeches, the Blastmaker) is deliberately NOT matched here: per CR 603.12 it
/// creates a *reflexive triggered ability* that follows delayed-triggered-ability
/// rules (CR 603.3 / CR 603.7), so it must be put on the stack the next time a
/// player would receive priority — giving a priority window before the branch
/// effect resolves — rather than running inline during the flip's resolution.
/// `try_parse_reflexive_coin_flip_branch` owns that "when" wording and lowers it
/// to a `CreateDelayedTrigger`; this function is strictly the inline-fold path.
pub(crate) fn try_parse_coin_flip_branch(text: &str) -> Option<(bool, &str)> {
    const WIN: &str = "if you win the flip, ";
    const LOSE: &str = "if you lose the flip, ";
    if let Some(prefix) = text.get(..WIN.len()) {
        if prefix.eq_ignore_ascii_case(WIN) {
            return Some((true, &text[WIN.len()..]));
        }
    }
    if let Some(prefix) = text.get(..LOSE.len()) {
        if prefix.eq_ignore_ascii_case(LOSE) {
            return Some((false, &text[LOSE.len()..]));
        }
    }
    None
}

/// CR 603.12: Parse the reflexive coin-flip-result trigger wording
/// "when you win the flip, [effect]" / "when you lose the flip, [effect]"
/// (Breeches, the Blastmaker). Returns `(is_win, effect_text)` with `effect_text`
/// in its original case, or `None` when the clause does not open with this
/// wording.
///
/// Unlike `try_parse_coin_flip_branch`'s inline "if you win/lose the flip"
/// (Krark, the Thumbless — CR 705), this "when" wording creates a *reflexive
/// triggered ability* (CR 603.12) that follows delayed-triggered-ability rules
/// (CR 603.3 / CR 603.7): the branch effect must go on the stack and resolve with
/// a priority window, NOT inline during the flip's own resolution. The clause
/// dispatcher lowers this to an `Effect::CreateDelayedTrigger` whose embedded
/// `TriggerMode::FlippedCoin` trigger (filtered by `coin_flip_result`) fires on
/// the `CoinFlipped` event emitted earlier in the flip's resolution — exactly the
/// CR 603.12 "checked immediately after being created, triggering on whether the
/// event occurred during this resolution" model — and goes on the stack with its
/// own priority window via the existing `check_delayed_triggers` path.
pub(crate) fn try_parse_reflexive_coin_flip_branch<'a>(
    text: &'a str,
    lower: &str,
) -> Option<(bool, &'a str)> {
    nom_on_lower(text, lower, |input| {
        let (rest, is_win) = preceded(
            tag("when you "),
            alt((value(true, tag("win")), value(false, tag("lose")))),
        )
        .parse(input)?;
        let (rest, _) = tag(" the flip, ").parse(rest)?;
        Ok((rest, is_win))
    })
}

pub(super) fn lower_imperative_family_ast(ast: ImperativeFamilyAst) -> ParsedEffectClause {
    match ast {
        // CR 118.12: A Counter with an "unless [player] pays [cost]" modifier
        // — intercepted here so the modifier propagates to
        // `ParsedEffectClause.unless_pay`. The Effect itself becomes the
        // modifier-free `Effect::Counter`; runtime resolution flows through
        // the unified `ResolvedAbility.unless_pay` pipeline rather than a
        // counter-specific bespoke branch.
        ImperativeFamilyAst::ZoneCounter(ZoneCounterImperativeAst::Counter {
            target,
            source_rider,
            unless_pay: Some(unless_pay),
            all,
        }) => {
            let effect = if all {
                // CR 701.6 + CR 405.1: Mass counter drops both source_rider
                // and unless_pay (no corpus card combines them with mass
                // counter, and mass counter is non-targeting per CR 115.1).
                Effect::CounterAll { target }
            } else {
                Effect::Counter {
                    target,
                    source_rider,
                    // CR 701.6a + CR 614.1a: the countered-spell redirect is a
                    // continuation absorbed post-hoc (sequence.rs); the base
                    // effect parses with the default graveyard destination.
                    countered_spell_zone: None,
                }
            };
            let mut clause = parsed_clause(effect);
            // For mass counter, unless_pay is meaningless — drop it.
            if !all {
                clause.unless_pay = Some(unless_pay);
            }
            clause
        }
        // CR 608.2c: A tracked-set partition with a rest complement ("Put all
        // <filter> revealed this way into your hand and the rest into your
        // graveyard" — Winding Way). The primary mass move sends the chosen
        // subset (`target`, a `TrackedSetFiltered`) to `destination`; emit a
        // sibling `ChangeZoneAll` for "the rest" — the revealed cards NOT in the
        // chosen subset. Expressing the complement as `TrackedSetFiltered { Not
        // <chosen inner filter> }` (rather than the whole `TrackedSet`) is
        // zone-independent and order-independent: the chosen cards are excluded
        // by predicate even after they have already moved to `destination`, so
        // the complement never re-moves them. Intercepted here because the
        // partition needs a sub_ability linkage that only `ParsedEffectClause`
        // can express.
        ImperativeFamilyAst::Put(PutImperativeAst::ZoneChangeAll {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
            library_position,
            random_order,
            rest_destination: Some(rest_destination),
            rest_library_position,
        }) => {
            // CR 401.4: "in a random order" (and the bottom/top position)
            // describes whichever pile returns to the library. Exactly one pile
            // does in the printed partition class (the other goes to a
            // hidden/unordered zone — hand/graveyard/exile), so route position
            // and randomness to that pile. `random_order` was scanned by
            // `try_parse_put_zone_change_parts` from the post-primary tail
            // (which contains the rest clause), so it belongs to the rest
            // whenever the rest is the library pile. Note "in a random order"
            // (The Fourteenth Doctor) → random_order true; "in any order"
            // (Garruk, Goblin Ringleader) → random_order false (owner arranges),
            // because the scan only matches "in a random order".
            let (primary_random, complement_random) = if rest_destination == Zone::Library {
                (false, random_order)
            } else {
                (random_order, false)
            };
            // "The rest" excludes the chosen subset by predicate. When the
            // primary names a filtered subset, negate its inner filter;
            // otherwise (no inner filter) the complement is the full tracked set.
            let rest_target = match &target {
                TargetFilter::TrackedSetFiltered {
                    id,
                    filter,
                    caused_by,
                } => TargetFilter::TrackedSetFiltered {
                    id: *id,
                    filter: Box::new(TargetFilter::Not {
                        filter: filter.clone(),
                    }),
                    caused_by: *caused_by,
                },
                TargetFilter::TrackedSet { id } => TargetFilter::TrackedSet { id: *id },
                _ => TargetFilter::TrackedSet {
                    id: crate::types::identifiers::TrackedSetId(0),
                },
            };
            let primary = Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(enter_tapped),
                enter_with_counters: vec![],
                face_down_profile: None,
                // CR 401.4: `library_position` is the PRIMARY move's own
                // position (set only when the primary destination is a library
                // pile); randomness for the primary is routed above.
                library_position,
                random_order: primary_random,
            };
            let complement = Effect::ChangeZoneAll {
                origin: None,
                destination: rest_destination,
                target: rest_target,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                // CR 401.4: the "rest" pile's bottom/top position and randomness.
                library_position: rest_library_position,
                random_order: complement_random,
            };
            let mut clause = parsed_clause(primary);
            clause.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                complement,
            )));
            clause
        }
        // CR 122.1 + CR 608.2c: Multi-typed counter list → PutCounter chain.
        // Intercepted here (rather than in lower_zone_counter_ast which returns
        // a bare Effect) because the chain requires a sub_ability linkage that
        // only ParsedEffectClause can express.
        ImperativeFamilyAst::Put(PutImperativeAst::ZoneChange {
            origin,
            destination,
            target,
            enters_under,
            enter_tapped,
            enter_transformed,
            enters_attacking,
            up_to,
            choice_count: Some(choice_count),
            enter_with_counters,
        }) => {
            let effect = lower_put_ast(PutImperativeAst::ZoneChange {
                origin,
                destination,
                target,
                enters_under,
                enter_tapped,
                enter_transformed,
                enters_attacking,
                up_to,
                choice_count: Some(choice_count.clone()),
                enter_with_counters,
            });
            let mut clause = parsed_clause(effect);
            clause.multi_target = Some(*choice_count);
            clause
        }
        ImperativeFamilyAst::ZoneCounter(ZoneCounterImperativeAst::PutCounterList {
            entries,
            target,
            multi_target,
        }) => lower_put_counter_list(entries, target, multi_target),
        // CR 115.1c + CR 601.2c: "Choose target X and target Y" — two
        // independent target slots. Lowered to a primary `Effect::TargetOnly`
        // (slot A) with a chained `TargetOnly` sub_ability (slot B). At
        // resolution time both targets are announced as part of the same cast
        // (CR 601.2c: "if the spell uses the word 'target' in multiple
        // places, the same object or player can be chosen once for each
        // instance"). Intercepted here because a bare `Effect` cannot express
        // the sub_ability chain — only `ParsedEffectClause` can. Sentences
        // following the targeting clause that reference the chosen objects
        // (e.g., Goblin Welder's "If both targets are still legal …") chain
        // further sub_abilities and resolve their target slots via
        // `TargetFilter::ParentTarget` walking the chain (CR 608.2c:
        // instructions are followed in order; later text may modify earlier
        // text).
        ImperativeFamilyAst::Structured(ImperativeAst::Choose(
            ChooseImperativeAst::TwoTargets { target_a, target_b },
        )) => {
            let mut clause = parsed_clause(Effect::TargetOnly { target: target_a });
            clause.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::TargetOnly { target: target_b },
            )));
            clause
        }
        // CR 701.23a + CR 107.1: Dual/N-way search ("a X card and a Y card") lowers
        // to a chain of independent `SearchLibrary` effects linked via sub_ability,
        // mirroring `lower_put_counter_list`. Intercepted here because the bare
        // `Effect` returned by `lower_search_and_creation_ast` cannot express a
        // chain — only `ParsedEffectClause.sub_ability` can.
        ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(
            SearchCreationImperativeAst::SearchLibrary {
                filter,
                count,
                reveal,
                target_player,
                up_to,
                selection_constraint,
                reference_target: Some(reference_target),
                extra_filters,
                multi_destination,
                multi_enter_tapped,
                // Reference-target searches are not cultivate-class splits.
                split: _,
                // Reference-target searches are library-only (default).
                source_zones: _,
            },
        )) => lower_target_referenced_search_library(
            reference_target,
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            extra_filters,
            multi_destination,
            multi_enter_tapped,
        ),
        // CR 701.23a + CR 107.1: Dual/N-way search ("a X card and a Y card") lowers
        // to a chain of independent `SearchLibrary` effects linked via sub_ability,
        // mirroring `lower_put_counter_list`. Intercepted here because the bare
        // `Effect` returned by `lower_search_and_creation_ast` cannot express a
        // chain — only `ParsedEffectClause.sub_ability` can.
        ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(
            SearchCreationImperativeAst::SearchLibrary {
                filter,
                count,
                reveal,
                target_player,
                up_to,
                selection_constraint,
                reference_target: None,
                extra_filters,
                multi_destination,
                multi_enter_tapped,
                // Multi-filter searches handle destinations per-filter, not via split.
                split: _,
                // Multi-filter searches are library-only (default).
                source_zones: _,
            },
        )) if !extra_filters.is_empty() => lower_multi_filter_search_library(
            filter,
            count,
            reveal,
            target_player,
            up_to,
            selection_constraint,
            extra_filters,
            multi_destination,
            multi_enter_tapped,
        ),
        ImperativeFamilyAst::Structured(ImperativeAst::SearchCreation(
            SearchCreationImperativeAst::Seek {
                filter,
                count,
                from_top,
                destination,
                enter_tapped,
                extra_filters,
            },
        )) if !extra_filters.is_empty() => lower_multi_filter_seek(
            filter,
            count,
            from_top,
            destination,
            enter_tapped,
            extra_filters,
        ),
        ImperativeFamilyAst::Shuffle(ast) => lower_shuffle_ast(ast),
        // CR 115.1d + CR 701.26a/b: "tap/untap up to N target creatures" (Nyssa of
        // Traken: "tap up to that many target creatures"). The bare-Effect lowering
        // (`lower_targeted_action_ast`) cannot carry the variable target count, so
        // a Tap/Untap AST that captured a `multi_target` is lowered here and the
        // count is threaded onto the clause — mirroring the `Support` arm below.
        ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
            ast @ (TargetedImperativeAst::Tap {
                multi_target: Some(_),
                ..
            }
            | TargetedImperativeAst::Untap {
                multi_target: Some(_),
                ..
            }),
        )) => {
            let multi_target = match &ast {
                TargetedImperativeAst::Tap { multi_target, .. }
                | TargetedImperativeAst::Untap { multi_target, .. } => multi_target.clone(),
                _ => None,
            };
            let mut clause = parsed_clause(lower_targeted_action_ast(ast));
            clause.multi_target = multi_target;
            clause
        }
        // CR 115.1d + CR 601.2c: "return up to N cards from your graveyard to
        // your hand" (Ill-Gotten Gains) or "return that many cards …"
        // (dynamic count) captured a `multi_target` on a non-targeted
        // (`BounceSelection::AtResolution`) return. The bare-Effect lowering
        // (`lower_targeted_action_ast`) cannot carry it, so intercept here and
        // thread the count onto the clause — mirroring the Tap/Untap arm
        // above. The runtime resolves the card selection via the shared
        // `EffectZoneChoice` multi-target picker (Wrenn and Six precedent).
        ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
            ast @ TargetedImperativeAst::Return {
                multi_target: Some(_),
                ..
            },
        )) => {
            let multi_target = match &ast {
                TargetedImperativeAst::Return { multi_target, .. } => multi_target.clone(),
                _ => None,
            };
            let mut clause = parsed_clause(lower_targeted_action_ast(ast));
            clause.multi_target = multi_target;
            clause
        }
        // CR 115.1d + CR 601.2c + CR 700.4: sibling of the `Return` arm above
        // for the `origin.is_some()` shape ("return up to N cards from your
        // graveyard to your hand" lowers to a `ChangeZone`, not a `Bounce`,
        // when an explicit origin zone is present — Ill-Gotten Gains). Mirrors
        // `ZoneCounterImperativeAst::Exile`'s `multi_target` handling (#5649 /
        // Forage precedent): `Effect::ChangeZone` has no count slot of its
        // own, so the quantity rides the clause's `MultiTargetSpec` instead,
        // resolved by the same shared `EffectZoneChoice` picker.
        ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
            ast @ TargetedImperativeAst::ReturnToZone {
                multi_target: Some(_),
                ..
            },
        )) => {
            let multi_target = match &ast {
                TargetedImperativeAst::ReturnToZone { multi_target, .. } => multi_target.clone(),
                _ => None,
            };
            let mut clause = parsed_clause(lower_targeted_action_ast(ast));
            clause.multi_target = multi_target;
            clause
        }
        // CR 701.41a: Support N → PutCounter with multi-target "up to N".
        // On permanents (is_other=true): "up to N other target creatures"
        // On instants/sorceries (is_other=false): "up to N target creatures"
        ImperativeFamilyAst::Support { count, is_other } => {
            let properties = if is_other {
                vec![crate::types::ability::FilterProp::Another]
            } else {
                vec![]
            };
            let target = TargetFilter::Typed(TypedFilter {
                type_filters: vec![crate::types::ability::TypeFilter::Creature],
                properties,
                ..Default::default()
            });
            let mut clause = parsed_clause(Effect::PutCounter {
                counter_type: crate::types::counter::CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target,
            });
            clause.multi_target = Some(MultiTargetSpec::fixed(0, count as usize));
            clause
        }
        // CR 603.5 / CR 608.2d: "you may <effect>" and subject-stripped "each
        // player may <effect>" both arrive here as `YouMay` (the `you`/`may`
        // first-word arms). The optionality is an ability-level flag, not part
        // of the Effect, so lower the body and mark the CLAUSE optional. The
        // bare `lower_imperative_family_effect` path drops the "may" entirely,
        // making the effect mandatory — e.g. Mog, Moogle Warrior's "each player
        // may discard a card" became a forced discard (issue #2901).
        ImperativeFamilyAst::YouMay { text } => {
            let mut clause = parsed_clause(super::parse_effect(&text));
            clause.optional = true;
            clause
        }
        // CR 115.6: "it fights up to one target creature …" allows zero targets.
        // The "up to N" cardinality is an ability-level field
        // (`ParsedEffectClause.multi_target` → `AbilityDefinition.multi_target`
        // with min=0 via `lower.rs`), not an `Effect::Fight` field. The
        // bare-Effect lowering chain (`lower_targeted_action_ast`) cannot carry
        // it, so intercept here where a `ParsedEffectClause` is in scope and
        // stamp the spec onto the clause — mirroring the `YouMay` /
        // `PutCounterList` clause-field interception arms above and the
        // subject-form "up to" path in `subject.rs`. `None` (mandatory "fights
        // target …") leaves the clause unchanged.
        ImperativeFamilyAst::Structured(ImperativeAst::Targeted(
            TargetedImperativeAst::Fight {
                target,
                multi_target,
            },
        )) => {
            let mut clause =
                parsed_clause(lower_targeted_action_ast(TargetedImperativeAst::Fight {
                    target,
                    multi_target: None,
                }));
            clause.multi_target = multi_target;
            clause
        }
        // CR 115.1d + CR 708.2a: "turn any number of target … face down" /
        // "turn N target … face down" (Illithid Harvester). The target-count
        // quantifier is an ability-level field (`ParsedEffectClause.multi_target`
        // → `AbilityDefinition.multi_target`), not an `Effect::TurnFaceDown`
        // field, so intercept here where a `ParsedEffectClause` is in scope and
        // stamp the spec — mirroring the `Fight` / `PutCounterList` clause-field
        // arms above. `None` (single subject — Cyber Conversion) leaves the
        // clause's default unchanged.
        ImperativeFamilyAst::TurnFaceDown {
            target,
            profile,
            multi_target,
        } => {
            let mut clause = parsed_clause(Effect::TurnFaceDown { target, profile });
            clause.multi_target = multi_target;
            clause
        }
        // CR 601.2d + CR 615.7: "prevent N damage divided/distributed among [targets]"
        // — intercepted here so `distribute` and `multi_target` propagate to
        // `ParsedEffectClause`. The bare Effect returned by `lower_utility_imperative_ast`
        // cannot carry these fields. Mirrors the ZoneCounter { unless_pay } intercept above.
        ImperativeFamilyAst::Structured(ImperativeAst::Utility(UtilityImperativeAst::Attach {
            attachment,
            target,
            multi_target,
        })) => {
            let mut clause = parsed_clause(Effect::Attach { attachment, target });
            clause.multi_target = multi_target;
            clause
        }
        ImperativeFamilyAst::Structured(ImperativeAst::Utility(
            UtilityImperativeAst::Prevent {
                ref text,
                parent_target_available,
            },
        )) => {
            // CR 615 + CR 608.2c (issue #1094): a bidirectional "dealt to and
            // dealt by <anaphor>" shield must split into two independent
            // PreventDamage nodes — tried FIRST, before the distribute path
            // (mutually exclusive markers: no card combines "distributed among"
            // with "dealt to and dealt by").
            if let Some(clause) =
                super::lower::try_parse_bidirectional_prevent(text, parent_target_available)
            {
                return clause;
            }
            if let Some(clause) = super::lower::try_parse_prevent_distribute(text) {
                return clause;
            }
            // Fallback: standard prevent with no distribution.
            // lower_utility_imperative_ast is defined in THIS file (imperative.rs),
            // called unqualified — NOT super::lower::lower_utility_imperative_ast.
            parsed_clause(lower_utility_imperative_ast(
                UtilityImperativeAst::Prevent {
                    text: text.clone(),
                    parent_target_available,
                },
            ))
        }
        // CR 701.58a: "cloak a card from your hand" (Vannifar). The controller
        // chooses a card from their hand — delegated to the `ChooseFromZone`
        // building block — then cloaks it (CR 701.58a). The `Cloak` sub-ability
        // reads the chosen card from `object_source` (`ParentTarget`, resolved
        // against the `ability.targets` the choose forwards — CR 608.2c: later
        // instructions read the earlier selection). Intercepted here because a
        // bare Effect cannot express the parent + sub chain — only
        // `ParsedEffectClause` can (mirrors the SearchLibrary sub_ability chain).
        ImperativeFamilyAst::Cloak {
            target,
            count,
            from_zone: Some(zone),
        } => {
            let mut clause = parsed_clause(Effect::ChooseFromZone {
                count: 1,
                zone,
                additional_zones: Vec::new(),
                zone_owner: ZoneOwner::Controller,
                filter: None,
                chooser: Chooser::Controller,
                up_to: false,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                constraint: None,
            });
            clause.sub_ability = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Cloak {
                    target,
                    count,
                    object_source: Some(TargetFilter::ParentTarget),
                },
            )));
            clause
        }
        // CR 700.4 (#5649): a COUNTED graveyard exile ("exile that many cards
        // from your graveyard", Nefarious Lich) captured a `multi_target`; the
        // bare-Effect lowering below produces a `ChangeZone` that cannot carry
        // the count, so thread it onto the clause here (mirroring the Tap/Untap
        // arm). The runtime resolves the card selection via the shared
        // multi-target picker (Forage precedent).
        ImperativeFamilyAst::ZoneCounter(
            ast @ ZoneCounterImperativeAst::Exile {
                multi_target: Some(_),
                ..
            },
        ) => {
            let multi_target = match &ast {
                ZoneCounterImperativeAst::Exile { multi_target, .. } => multi_target.clone(),
                _ => None,
            };
            let mut clause = parsed_clause(lower_zone_counter_ast(ast));
            clause.multi_target = multi_target;
            clause
        }
        // All other arms produce a bare Effect with no sub_ability chain.
        other => parsed_clause(lower_imperative_family_effect(other)),
    }
}

fn lower_imperative_family_effect(ast: ImperativeFamilyAst) -> Effect {
    match ast {
        ImperativeFamilyAst::Structured(ast) => lower_imperative_ast(ast),
        ImperativeFamilyAst::CostResource(ast) => lower_cost_resource_ast(ast),
        ImperativeFamilyAst::ZoneCounter(ast) => lower_zone_counter_ast(ast),
        ImperativeFamilyAst::Explore => Effect::Explore,
        ImperativeFamilyAst::Connive => Effect::Connive {
            target: TargetFilter::Any,
            count: QuantityExpr::Fixed { value: 1 },
        },
        ImperativeFamilyAst::ForceBlock => Effect::ForceBlock {
            target: TargetFilter::Any,
        },
        ImperativeFamilyAst::ForceAttack {
            duration,
            required_player,
        } => Effect::ForceAttack {
            target: TargetFilter::Any,
            required_player,
            duration,
        },
        // CR 701.15a: Goad target creature. Subject injection fills target from parsed text.
        ImperativeFamilyAst::Goad => Effect::Goad {
            target: TargetFilter::Any,
        },
        // CR 701.12a: Exchange control of two permanents. The two slot filters
        // come from the parser; resolution reads ability.targets for the chosen
        // objects.
        ImperativeFamilyAst::ExchangeControl { target_a, target_b } => {
            Effect::ExchangeControl { target_a, target_b }
        }
        ImperativeFamilyAst::ExchangeLifeWithStat { player, stat } => {
            Effect::ExchangeLifeWithStat { player, stat }
        }
        // CR 701.12a: Two players exchange life totals. The two player filters
        // come from the parser; resolution reads ability.targets in declaration
        // order for any non-context-ref slots.
        ImperativeFamilyAst::ExchangeLifeTotals { player_a, player_b } => {
            Effect::ExchangeLifeTotals { player_a, player_b }
        }
        // CR 119.7 + CR 119.8: field-less interactive life-total redistribution. The
        // resolver self-gathers participants, so no target wiring is needed.
        ImperativeFamilyAst::RedistributeLifeTotals => Effect::RedistributeLifeTotals,
        // CR 509.1c: Must be blocked — grant transient MustBeBlocked static via GenericEffect.
        // Uses AddStaticMode so the mode propagates through the layer system to
        // static_definitions, where combat.rs checks it.
        ImperativeFamilyAst::MustBeBlocked => Effect::GenericEffect {
            static_abilities: vec![
                StaticDefinition::new(StaticMode::MustBeBlocked { by: None }).modifications(vec![
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustBeBlocked { by: None },
                    },
                ]),
            ],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
        },
        ImperativeFamilyAst::Investigate => Effect::Investigate,
        ImperativeFamilyAst::Learn => Effect::Learn,
        // CR 701.40a: Default subject is the controller ("you manifest..."). Subject
        // lowering for "its controller manifests..." routes through the dedicated
        // subject-predicate arm in `lower_subject_predicate_ast` below, which
        // constructs `Effect::Manifest { target: subject.affected, ... }` directly.
        // CR 701.40a + CR 110.2a: The direct "manifest the top N cards" surface
        // carries no effect-specified face-down profile; `enters_under` records
        // the instruction-controller default. The put-form manifest may also
        // seed an effect-specified profile (see `lower_put_ast`).
        ImperativeFamilyAst::Manifest {
            target,
            count,
            enters_under,
        } => Effect::Manifest {
            target,
            count,
            profile: None,
            enters_under,
        },
        ImperativeFamilyAst::ManifestDread => Effect::ManifestDread,
        // CR 701.58a: Cloak the top card(s) of a library (face-down 2/2 + ward
        // {2}). The from-hand form (`from_zone: Some`) is intercepted upstream in
        // `lower_imperative_family_ast`; only the library-top source reaches here.
        ImperativeFamilyAst::Cloak { target, count, .. } => Effect::Cloak {
            target,
            count,
            object_source: None,
        },
        // CR 406.3: Turn the exiled card(s) face up (Imprint flip cards).
        ImperativeFamilyAst::TurnFaceUp { target } => Effect::TurnFaceUp { target },
        // CR 708.2a: Turn target permanent(s) face down (Cyber Conversion).
        // `multi_target` is a clause-level field stamped in
        // `lower_imperative_family_ast`'s intercept arm; the bare-Effect path
        // (used by unit tests that inspect only the `Effect`) cannot carry it.
        ImperativeFamilyAst::TurnFaceDown {
            target,
            profile,
            multi_target: _,
        } => Effect::TurnFaceDown { target, profile },
        ImperativeFamilyAst::BecomeMonarch => Effect::BecomeMonarch,
        ImperativeFamilyAst::VentureIntoDungeon => Effect::VentureIntoDungeon,
        ImperativeFamilyAst::VentureIntoUndercity => Effect::VentureInto {
            dungeon: crate::game::dungeon::DungeonId::Undercity,
        },
        ImperativeFamilyAst::TakeTheInitiative => Effect::TakeTheInitiative,
        // CR 701.31c: An ability instructs a player to planeswalk.
        ImperativeFamilyAst::Planeswalk => Effect::Planeswalk,
        ImperativeFamilyAst::OpenAttractions { count } => Effect::OpenAttractions { count },
        ImperativeFamilyAst::RollToVisitAttractions => Effect::RollToVisitAttractions,
        ImperativeFamilyAst::AssembleContraptions { count } => {
            Effect::AssembleContraptions { count }
        }
        ImperativeFamilyAst::AssembleContraptionsFromRollDifference => {
            Effect::AssembleContraptionsFromRollDifference
        }
        ImperativeFamilyAst::ReassembleContraption {
            target,
            control_mode,
        } => Effect::ReassembleContraption {
            target,
            control_mode,
        },
        ImperativeFamilyAst::Proliferate => Effect::Proliferate,
        // CR 701.56a: Time travel.
        ImperativeFamilyAst::TimeTravel => Effect::TimeTravel,
        // CR 701.36a: Populate.
        ImperativeFamilyAst::Populate => Effect::Populate,
        // CR 701.30: Clash with an opponent.
        ImperativeFamilyAst::Clash => Effect::Clash,
        // CR 701.4a: Behold a [quality] — reveal-or-choose keyword action.
        ImperativeFamilyAst::Behold(filter) => Effect::Behold { filter },
        ImperativeFamilyAst::GainKeyword(effect) => effect,
        ImperativeFamilyAst::LoseKeyword(effect) => effect,
        ImperativeFamilyAst::LoseTheGame => Effect::LoseTheGame { target: None },
        ImperativeFamilyAst::WinTheGame => Effect::WinTheGame { target: None },
        ImperativeFamilyAst::RollDie {
            count,
            sides,
            modifier,
        } => Effect::RollDie {
            count,
            sides,
            results: vec![],
            modifier,
        },
        // CR 705.2: the bare imperative lowers with `flipper = Controller`; a
        // player subject ("that player flips a coin") is stamped onto `flipper`
        // afterward by `inject_subject_target`.
        ImperativeFamilyAst::FlipCoin => Effect::FlipCoin {
            win_effect: None,
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
        ImperativeFamilyAst::FlipCoins { count } => Effect::FlipCoins {
            count,
            win_effect: None,
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
        ImperativeFamilyAst::FlipCoinUntilLose => Effect::FlipCoinUntilLose {
            // Stub — subsequent "For each flip you won, ..." clauses are
            // consolidated into this by consolidate_die_and_coin_defs.
            win_effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "flip_coin_until_lose_stub".to_string(),
                    description: Some("pending consolidation".to_string()),
                },
            )),
        },
        ImperativeFamilyAst::Put(ast) => lower_put_ast(ast),
        ImperativeFamilyAst::YouMay { text } => super::parse_effect(&text),
        // CR 122.1: Player counter manipulation. Target is set by subject injection.
        ImperativeFamilyAst::GivePlayerCounter {
            counter_kind,
            count,
        } => Effect::GivePlayerCounter {
            counter_kind,
            count,
            target: TargetFilter::Controller,
        },
        // CR 506.4: Remove from combat.
        ImperativeFamilyAst::RemoveFromCombat(target) => Effect::RemoveFromCombat { target },
        // Shuffle and Support are handled in `lower_imperative_family_ast` directly.
        ImperativeFamilyAst::Shuffle(_) | ImperativeFamilyAst::Support { .. } => unreachable!(),
    }
}

/// CR 122.1: Detect a *mass* counter placement ("on each" / "on all") within a
/// single counter-placement clause. The caller MUST pass only the primary
/// clause text — never a full compound string — otherwise a trailing "on each"
/// conjunct would wrongly promote a targeted primary clause.
pub(super) fn counter_placement_is_mass(clause_lower: &str) -> bool {
    nom_primitives::scan_contains(clause_lower, "counter on each")
        || nom_primitives::scan_contains(clause_lower, "counters on each")
        || nom_primitives::scan_contains(clause_lower, "counter on all")
        || nom_primitives::scan_contains(clause_lower, "counters on all")
        || nom_primitives::scan_contains(clause_lower, "on each ")
        || nom_primitives::scan_contains(clause_lower, "on all ")
}

/// CR 122.1 + CR 608.2c: In a distributive "put a number of +1/+1 counters on
/// EACH ... equal to THAT CREATURE's <stat>" (Canopy Gargantuan), "that
/// creature" is the per-iteration recipient — each object receives counters
/// equal to its OWN stat. The shared `parse_event_context_refs` lowers "that
/// creature's <stat>" to `ObjectScope::CostPaidObject` (correct in trigger
/// bodies, where it refers to the triggering object), so for a mass placement
/// we rebind it to `ObjectScope::Recipient`; `resolve_add_all` then re-evaluates
/// the count per object. A genuine cost referent ("the SACRIFICED creature's
/// power") carries a cost/zone-change participle and is left as
/// `CostPaidObject` — `resolve_add_all` resolves that once and applies it
/// uniformly per CR 608.2k.
fn rebind_distributive_recipient_count(count: QuantityExpr, lower: &str) -> QuantityExpr {
    let is_cost_referent = nom_primitives::scan_contains(lower, "sacrificed")
        || nom_primitives::scan_contains(lower, "exiled")
        || nom_primitives::scan_contains(lower, "discarded")
        || nom_primitives::scan_contains(lower, "milled");
    if is_cost_referent {
        return count;
    }
    rebind_costpaid_scope_to_recipient(count)
}

/// Walk a `QuantityExpr` and rebind `ObjectScope::CostPaidObject` on the object
/// P/T/mana-value leaves to `ObjectScope::Recipient`. Recurses through the
/// arithmetic wrappers ("that creature's toughness plus 1", "twice that
/// creature's power") so the whole class composes.
fn rebind_costpaid_scope_to_recipient(expr: QuantityExpr) -> QuantityExpr {
    use crate::types::ability::ObjectScope::{CostPaidObject, Recipient};
    match expr {
        QuantityExpr::Ref { qty } => QuantityExpr::Ref {
            qty: match qty {
                QuantityRef::Power {
                    scope: CostPaidObject,
                } => QuantityRef::Power { scope: Recipient },
                QuantityRef::Toughness {
                    scope: CostPaidObject,
                } => QuantityRef::Toughness { scope: Recipient },
                QuantityRef::ObjectManaValue {
                    scope: CostPaidObject,
                } => QuantityRef::ObjectManaValue { scope: Recipient },
                other => other,
            },
        },
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
            offset,
        },
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
            minimum,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor,
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
        },
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(rebind_costpaid_scope_to_recipient(*inner)),
            divisor,
            rounding,
        },
        QuantityExpr::Sum { exprs } => QuantityExpr::Sum {
            exprs: exprs
                .into_iter()
                .map(rebind_costpaid_scope_to_recipient)
                .collect(),
        },
        QuantityExpr::Max { exprs } => QuantityExpr::Max {
            exprs: exprs
                .into_iter()
                .map(rebind_costpaid_scope_to_recipient)
                .collect(),
        },
        QuantityExpr::UpTo { max } => QuantityExpr::UpTo {
            max: Box::new(rebind_costpaid_scope_to_recipient(*max)),
        },
        QuantityExpr::Power { base, exponent } => QuantityExpr::Power {
            base,
            exponent: Box::new(rebind_costpaid_scope_to_recipient(*exponent)),
        },
        QuantityExpr::Difference { left, right } => QuantityExpr::Difference {
            left: Box::new(rebind_costpaid_scope_to_recipient(*left)),
            right: Box::new(rebind_costpaid_scope_to_recipient(*right)),
        },
        QuantityExpr::Fixed { value } => QuantityExpr::Fixed { value },
    }
}

/// CR 122.1 + CR 608.2d (Clockspinning sentence 2): recognize the three forms
/// of the "choose a counter, then adjust it" clause and return the operation set
/// the controller may choose among:
///
/// - `AddOrRemove`: "remove that counter [from <anaphor>] or put another of
///   those counters on <anaphor>" — Clockspinning.
/// - `Remove`: "remove that counter [from <anaphor>]" — remove-only variants.
/// - `Add`: "put another of those counters on <anaphor>" — add-only variants.
///
/// The singular "that counter" / plural "those counters" deictic anaphors are
/// unique to the "choose a counter, then adjust it" class, so this is a
/// self-contained combinator over the full clause — it deliberately does NOT
/// delegate to the shared `parse_counter_anaphor` authority (which gates the
/// remove-all sentinel path and has no singular "that counter" arm; extending
/// it would wrongly widen that dispatch for unrelated cards).
///
/// The object anaphor on either side ("that permanent or card" / "that
/// permanent" / "that card" / "it") is a don't-care: the established target is
/// reused via the parent `TargetOnly` chain. Returns `None` for every other
/// remove/add clause, leaving existing paths unchanged.
fn try_parse_choose_counter_adjustment(lower: &str) -> Option<CounterAdjustment> {
    type E<'a> = OracleError<'a>;

    // The reused object anaphor, longest-match-first so "that permanent or card"
    // is not shadowed by the "that permanent" prefix.
    fn counter_object_anaphor(input: &str) -> OracleResult<'_, ()> {
        value(
            (),
            alt((
                tag::<_, _, E>("that permanent or card"),
                tag("that permanent"),
                tag("that card"),
                tag("it"),
            )),
        )
        .parse(input)
    }

    let clause = lower.trim_end_matches(['.', ' ']);

    // Remove-based forms share the "remove that counter [from <anaphor>]"
    // prefix. Try AddOrRemove (longer form) before Remove-only so the shared
    // prefix does not short-circuit into the wrong variant.
    if let Ok((rest, _)) = tag::<_, _, E>("remove that counter").parse(clause) {
        let (rest, _) = opt(preceded(tag::<_, _, E>(" from "), counter_object_anaphor))
            .parse(rest)
            .ok()?;
        // AddOrRemove: remove leg followed by "or put another of those counters
        // on <anaphor>".
        if let Ok((rest2, _)) = preceded(
            tag::<_, _, E>(" or put another of those counters on "),
            counter_object_anaphor,
        )
        .parse(rest)
        {
            if rest2.is_empty() {
                return Some(CounterAdjustment::AddOrRemove);
            }
        }
        // Remove-only: the remove clause consumed the entire clause.
        if rest.is_empty() {
            return Some(CounterAdjustment::Remove);
        }
        return None;
    }

    // Add-only: "put another of those counters on <anaphor>".
    if let Ok((rest, _)) = preceded(
        tag::<_, _, E>("put another of those counters on "),
        counter_object_anaphor,
    )
    .parse(clause)
    {
        if rest.is_empty() {
            return Some(CounterAdjustment::Add);
        }
    }

    None
}

pub(super) fn parse_zone_counter_ast(
    text: &str,
    lower: &str,
    ctx: &mut ParseContext,
) -> Option<ZoneCounterImperativeAst> {
    if let Some(ast) = parse_destroy_ast(text, lower, ctx) {
        return Some(ast);
    }
    if let Some(ast) = parse_exile_ast(text, lower, ctx) {
        return Some(ast);
    }
    if let Some(ast) = parse_counter_ast(text, lower) {
        return Some(ast);
    }
    if tag::<_, _, OracleError<'_>>("put ").parse(lower).is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        // CR 122.1 + CR 122.6: "put [a[n]] [additional] counter of that kind on
        // <anaphor>" — add one counter of the kind chosen by a preceding
        // `ChooseCounterKind` (The Caves of Androzani). Detected before the
        // generic counter-type paths so "counter of that kind" is never mis-read
        // as a literal counter name. `parse_target`/`try_parse_put_counter`
        // cannot express "of that kind" (no CounterType), so this is a distinct
        // seam.
        if let Some(after_put) =
            nom_on_lower(text, lower, |i| value((), tag("put ")).parse(i)).map(|((), rest)| rest)
        {
            if let Some(Effect::PutChosenCounter { target, count }) =
                super::counter::try_parse_put_chosen_counter(&after_put.to_ascii_lowercase())
            {
                return Some(ZoneCounterImperativeAst::PutChosenCounter { target, count });
            }
        }
        // Try move-counters first ("put its counters on ...")
        if let Some((
            Effect::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection,
                target,
            },
            _rem,
        )) = super::counter::try_parse_move_counters(lower, text, ctx)
        {
            return Some(ZoneCounterImperativeAst::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection,
                target,
            });
        }
        // CR 122.1: Multi-typed counter list ("put a flying counter, a first
        // strike counter, and a lifelink counter on that creature"). Must run
        // before the single-counter path — `try_parse_put_counter_chain` only
        // returns `Some` when it consumed >=2 entries, so single-counter cases
        // fall through untouched.
        if let Some((entries, target, _rem, multi_target)) =
            super::counter::try_parse_put_counter_chain(lower, text, ctx)
        {
            return Some(ZoneCounterImperativeAst::PutCounterList {
                entries,
                target,
                multi_target,
            });
        }
        // Then fixed-count put ("put N counter(s) on ...")
        // Detect "each"/"all" to route to PutCounterAll (mass placement without targeting).
        // CR 122.1: "on each" and "on all" indicate mass application. The "counter(s)"
        // anchor handles the common case; the bare "on each "/"on all " fallbacks
        // cover phrases where a quantity clause ("equal to its power") intervenes
        // between the counter noun and the target — e.g. Gruff Triplets:
        // "put a number of +1/+1 counters equal to its power on each creature you
        // control named ~".
        let is_all = counter_placement_is_mass(lower);
        return match try_parse_put_counter(lower, text, ctx) {
            Some((
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                },
                _remainder,
                multi_target,
            )) => {
                // CR 608.2c: A `ParentTarget` placement subject ("each of them")
                // is an anaphor to a bounded set of chosen objects, not a
                // battlefield-wide type scan. The `PutCounterAll` resolver
                // matches its filter against every battlefield object and cannot
                // resolve a bare `ParentTarget` (it has no target slot to bind),
                // so route the anaphor to `PutCounter`, whose
                // `resolve_defined_or_targets` binds `ability.targets` (the
                // chosen creatures). Typed mass placements ("on each creature you
                // control") and tracked-set demonstratives still take the
                // `PutCounterAll` path.
                if is_all && multi_target.is_none() && !matches!(target, TargetFilter::ParentTarget)
                {
                    Some(ZoneCounterImperativeAst::PutCounterAll {
                        counter_type,
                        count: rebind_distributive_recipient_count(count, lower),
                        target,
                    })
                } else {
                    Some(ZoneCounterImperativeAst::PutCounter {
                        counter_type,
                        count,
                        target,
                    })
                }
            }
            _ => None,
        };
    }
    // CR 122.1 + CR 608.2k: route "remove …" to the counter parser when the
    // clause either names a counter explicitly ("remove a +1/+1 counter from ~")
    // OR refers to the just-established counters anaphorically. The anaphoric
    // forms ("remove all of them" / "remove them" — level-up/incubate cards like
    // Ludevic's Test Subject and Smoldering Egg, where the antecedent is the
    // trigger's intervening-if "if it has N or more <type> counters on it")
    // carry no literal "counter" token, so the `scan_contains` anchor alone
    // would drop them to `Unimplemented`. `parse_counter_anaphor` (the shared
    // anaphor authority in counter.rs) recognizes that surface against the
    // post-"remove " remainder so it reaches `try_parse_remove_counter`.
    // CR 122.1 + CR 608.2d (Clockspinning sentence 2): "remove that counter ...
    // or put another of those counters on it" — the controller chooses, at
    // resolution, a kind of counter present on the established target and adds OR
    // removes one of it. Must run BEFORE the generic `tag("remove ")` gate below
    // so this anaphoric add-or-remove clause is captured here rather than failing
    // through `try_parse_remove_counter`. The combinator returns `None` for every
    // other "remove …" clause, leaving the existing remove path unchanged.
    if let Some(adjustment) = try_parse_choose_counter_adjustment(lower) {
        return Some(ZoneCounterImperativeAst::ChooseCounterAdjustment { adjustment });
    }
    if let Ok((after_remove, _)) = tag::<_, _, OracleError<'_>>("remove ").parse(lower) {
        let is_counter_remove = nom_primitives::scan_contains(lower, "counter")
            || nom_on_lower(after_remove, after_remove, parse_counter_anaphor).is_some();
        if is_counter_remove {
            return match try_parse_remove_counter(lower, ctx) {
                Some(Effect::RemoveCounter {
                    counter_type,
                    count,
                    target,
                }) => Some(ZoneCounterImperativeAst::RemoveCounter {
                    counter_type,
                    count,
                    target,
                }),
                _ => None,
            };
        }
    }
    // CR 122.5: "move [N] [type] counter(s) from [source] onto/to [target]"
    if tag::<_, _, OracleError<'_>>("move ").parse(lower).is_ok()
        && nom_primitives::scan_contains(lower, "counter")
    {
        if let Some(Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        }) = try_parse_move_counters_from(lower, ctx)
        {
            return Some(ZoneCounterImperativeAst::MoveCounters {
                source,
                counter_type,
                count,
                mode,
                selection,
                target,
            });
        }
    }
    None
}

pub(super) fn lower_zone_counter_ast(ast: ZoneCounterImperativeAst) -> Effect {
    match ast {
        ZoneCounterImperativeAst::Destroy { target, all } => {
            if all {
                Effect::DestroyAll {
                    target,
                    cant_regenerate: false,
                }
            } else {
                Effect::Destroy {
                    target,
                    cant_regenerate: false,
                }
            }
        }
        ZoneCounterImperativeAst::Exile {
            origin,
            target,
            all,
            enter_with_counters,
            // The count (for "exile N cards from your graveyard") rides the
            // clause's `MultiTargetSpec`, threaded by `lower_imperative_family_ast`
            // (#5649); the bare-Effect lowering here ignores it.
            multi_target: _,
        } => {
            if all {
                // `ChangeZoneAll` has no counter slot; mass exile never carries
                // a "with counters" clause (all five non-from-hand construction
                // sites pass `vec![]`).
                Effect::ChangeZoneAll {
                    origin,
                    destination: Zone::Exile,
                    target,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_with_counters: vec![],
                    face_down_profile: None,
                    library_position: None,
                    random_order: false,
                }
            } else {
                Effect::ChangeZone {
                    origin,
                    destination: Zone::Exile,
                    target,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters,
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                }
            }
        }
        ZoneCounterImperativeAst::ExileTop {
            player,
            count,
            face_down,
        } => Effect::ExileTop {
            player,
            count,
            face_down,
        },
        ZoneCounterImperativeAst::Counter {
            target,
            source_rider,
            // CR 118.12: An unless-pay-bearing Counter is intercepted in
            // `lower_imperative_family_ast` so the modifier flows into
            // `ParsedEffectClause.unless_pay`. By the time we reach this
            // bare-Effect lowering site, `unless_pay` is always None — the
            // ParsedEffectClause-aware paths consumed it. This is the
            // sub_ability-less fallback for the (rare) case the Counter is
            // routed through `TargetedImperativeAst::ZoneCounterProxy`
            // directly without going through `lower_imperative_family_ast`.
            unless_pay: _,
            all,
        } => {
            if all {
                // CR 701.6 + CR 405.1: Mass counter. Drops `source_rider` —
                // no corpus card combines a source_rider with mass counter,
                // and the runtime resolver does not honor that slot on
                // `Effect::CounterAll`. (The rider is a per-target-permanent
                // silence/destroy follow-up.)
                Effect::CounterAll { target }
            } else {
                Effect::Counter {
                    target,
                    source_rider,
                    // CR 701.6a + CR 614.1a: the countered-spell redirect is a
                    // continuation absorbed post-hoc (sequence.rs); the base
                    // effect parses with the default graveyard destination.
                    countered_spell_zone: None,
                }
            }
        }
        ZoneCounterImperativeAst::PutCounter {
            counter_type,
            count,
            target,
        } => Effect::PutCounter {
            counter_type,
            count,
            target,
        },
        // CR 122.1 + CR 122.6: "put an additional counter of that kind on <anaphor>".
        ZoneCounterImperativeAst::PutChosenCounter { target, count } => {
            Effect::PutChosenCounter { target, count }
        }
        // CR 122.1: PutCounterList is always intercepted upstream in
        // `lower_imperative_family_ast` because it lowers to a sub_ability
        // chain that a bare Effect can't express. If execution reaches here
        // (e.g., via `TargetedImperativeAst::ZoneCounterProxy` in a compound
        // action, which only carries single-counter variants), degrade
        // gracefully to the first entry rather than panicking.
        ZoneCounterImperativeAst::PutCounterList {
            mut entries,
            target,
            ..
        } => {
            if let Some((counter_type, count)) = entries.drain(..).next() {
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                }
            } else {
                Effect::Unimplemented {
                    name: "put_counter_list_empty".to_string(),
                    description: None,
                }
            }
        }
        ZoneCounterImperativeAst::PutCounterAll {
            counter_type,
            count,
            target,
        } => Effect::PutCounterAll {
            counter_type,
            count,
            target,
        },
        ZoneCounterImperativeAst::RemoveCounter {
            counter_type,
            count,
            target,
        } => Effect::RemoveCounter {
            counter_type,
            count,
            target,
        },
        // CR 122.1 + CR 608.2d: the kind/op choice happens at resolution; the
        // target arrives via the parent `TargetOnly` chain, so this effect carries
        // no target slot. `count` is one per the printed "another"/"that counter".
        ZoneCounterImperativeAst::ChooseCounterAdjustment { adjustment } => {
            Effect::ChooseCounterAdjustment {
                adjustment,
                count: QuantityExpr::Fixed { value: 1 },
            }
        }
        ZoneCounterImperativeAst::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        } => Effect::MoveCounters {
            source,
            counter_type,
            count,
            mode,
            selection,
            target,
        },
    }
}

/// CR 701.53a: Parse "incubate {N}" from Oracle text.
///
/// Handles numeric and "X" counts via shared `parse_count_expr`.
fn try_parse_incubate(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("incubate ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // CR 716.1: Incubate's count is part of the keyword action; if it doesn't
    // parse, return None so the line lands in Unimplemented rather than
    // fabricating "incubate 1".
    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Incubate { count })
}

/// CR 701.47a: Parse "amass {Type} {N}" from Oracle text.
///
/// Handles all subtypes generically. The subtype is canonicalized from plural
/// to singular form (e.g., "Zombies" -> "Zombie") via `parse_subtype`.
fn try_parse_amass(text: &str, lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("amass ").parse(lower).ok()?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }

    // Parse subtype from original text (preserving case for parse_subtype)
    let original_rest = text[text.len() - rest.len()..].trim();
    let (subtype, consumed) = crate::parser::oracle_util::parse_subtype(original_rest)?;
    let remainder = rest[consumed..].trim();

    // CR 701.47a: Amass requires an explicit count after the subtype. If it
    // doesn't parse, surface as Unimplemented rather than amassing 1.
    let count = parse_count_expr(remainder).map(|(q, _)| q)?;

    Some(Effect::Amass { subtype, count })
}

/// CR 701.37a: Parse "monstrosity {N}" from Oracle text.
///
/// Used inside activated ability effect text (after the colon).
fn try_parse_monstrosity(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("monstrosity ")
        .parse(lower)
        .ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Monstrosity { count })
}

/// CR 701.46a: Parse "adapt N" from Oracle text.
///
/// Used inside activated ability effect text (after the colon).
fn try_parse_adapt(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("adapt ").parse(lower).ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Adapt { count })
}

/// CR 508.1d: Parse "attacks/attack [player] this turn/combat if able" requirements.
///
/// Bare forms ("attacks this turn if able") emit a temporary `MustAttack`.
/// Player-bound "attacks you ..." forms emit `ForceAttack`, whose resolver binds
/// "you" to the resolving ability controller and grants `MustAttackPlayer`.
pub(super) fn try_parse_attack_if_able(lower: &str) -> Option<ImperativeFamilyAst> {
    let trimmed = lower.trim_end_matches('.');

    // First try: bare forms without a player reference.
    // verb axis × phase axis (PATTERNS.md §8b): factor "attack(s)" out front,
    // then map the phase clause to its duration.
    let result: Result<(&str, Duration), nom::Err<OracleError<'_>>> = (
        alt((tag("attacks"), tag("attack"))),
        preceded(
            tag(" "),
            alt((
                value(Duration::UntilEndOfTurn, tag("this turn if able")),
                value(
                    Duration::UntilEndOfCombat,
                    alt((tag("this combat if able"), tag("that combat if able"))),
                ),
                // CR 508.1d + CR 511.3: "attacks during its controller's next combat
                // phase if able" — Trench Behemoth. The forced-attack requirement
                // persists until the end of the target creature's controller's next
                // combat phase (analogous to "doesn't untap during its controller's
                // next untap step"). `PlayerScope::Controller` resolves to the
                // affected object's controller at enforcement time.
                value(
                    Duration::UntilNextStepOf {
                        step: Phase::EndCombat,
                        player: PlayerScope::Controller,
                    },
                    tag("during its controller's next combat phase if able"),
                ),
            )),
        ),
    )
        .map(|(_, duration)| duration)
        .parse(trimmed);

    if let Ok((_, duration)) = result {
        return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
            static_abilities: vec![must_attack_static_definition()],
            duration: Some(duration),
            target: None,
        }));
    }

    let targeted: Result<(&str, (TargetFilter, Duration)), nom::Err<OracleError<'_>>> = (
        alt((tag("attacks"), tag("attack"))),
        preceded(
            tag(" "),
            // CR 508.1d: the required player. "you" binds the resolving ability
            // controller; "that player" references the opponent chosen earlier
            // in the same resolution — Ruhan of the Fomori, Raving Dead, Knight
            // Rampager ("choose an opponent at random. This creature attacks
            // that player this combat if able."). CR 608.2c: the choose+attack
            // resolve together, so the resolution-scoped ChosenPlayer { index }
            // (read from chosen_players) is the correct reference, not the
            // durable SourceChosenPlayer. The opponent choice is the single
            // preceding choice in every card of this class, so index 0.
            alt((
                value(TargetFilter::Controller, tag("you")),
                value(
                    TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::ChosenPlayer { index: 0 }),
                    ),
                    tag("that player"),
                ),
            )),
        ),
        preceded(
            tag(" "),
            alt((
                value(Duration::UntilEndOfTurn, tag("this turn if able")),
                value(
                    Duration::UntilEndOfCombat,
                    alt((
                        tag("this combat if able"),
                        tag("that combat if able"),
                        tag("each combat if able"),
                    )),
                ),
            )),
        ),
    )
        .map(|(_, required_player, duration)| (required_player, duration))
        .parse(trimmed);

    if let Ok((rest, (required_player, duration))) = targeted {
        if rest.is_empty() {
            return Some(ImperativeFamilyAst::ForceAttack {
                duration,
                required_player,
            });
        }
    }

    None
}

/// CR 701.15a + CR 701.15b: Recognize the verbatim goad *definition* —
/// "attack[s] each combat if able and attack[s] a player other than you if able".
/// This compound is precisely what goad does (CR 701.15a: must attack each combat
/// if able; CR 701.15b: must attack a player other than the goading player),
/// printed in full on cards that grant the effect without the "goad" keyword
/// (Maximum Carnage chapter I). Mapping it to the goad mechanic is the correct
/// class reuse: the goading player is the resolving controller ("you"), so the
/// subject's own creatures are forced onto opponents, and `goaded_by` cleanup at
/// the goading player's next turn matches the card's "Until your next turn"
/// duration (CR 701.15a). Returns the bare marker; `subject.rs` binds the affected
/// set to `Effect::GoadAll`.
pub(super) fn try_parse_goad_equivalent(lower: &str) -> bool {
    let trimmed = lower.trim_end_matches('.').trim();
    let parsed: Result<(&str, ()), nom::Err<OracleError<'_>>> = (
        alt((tag("attacks"), tag("attack"))),
        value(
            (),
            (
                tag(" each combat if able and "),
                alt((tag("attacks"), tag("attack"))),
                tag(" a player other than you if able"),
            ),
        ),
    )
        .map(|(_, marker)| marker)
        .parse(trimmed);
    matches!(parsed, Ok((rest, ())) if rest.is_empty())
}

/// CR 508.1d + CR 509.1c: Parse the combined "attacks or blocks ... if able"
/// requirement (Hustle: "Target creature attacks or blocks this turn if able.").
///
/// This is the imperative one-shot analogue of the continuous "attacks or blocks
/// each combat if able" static. It is the composition of an attack requirement
/// (CR 508.1d) and a block requirement (CR 509.1c): the creature must attack if
/// able during its controller's declare-attackers step, and must block if able
/// during a later declare-blockers step. The combined form emits both
/// `MustAttack` and `MustBlock` transient statics for the requested duration,
/// mirroring the bare-form `try_parse_attack_if_able` `MustAttack` path.
///
/// Verb axis × phase axis: factor the "attack(s) or block(s)" verb pair out front
/// (a single `alt()` over the conjugation variants), then map the phase clause to
/// its duration. Bare/source-granted forms (empty subject) carry `target: None`;
/// the targeted subject form binds the target via `subject.rs`.
pub(super) fn try_parse_attack_or_block_if_able(lower: &str) -> Option<ImperativeFamilyAst> {
    let trimmed = lower.trim_end_matches('.');

    // Each verb's conjugation varies independently: the bare imperative path
    // passes the raw text ("attacks or blocks …") while the subject path passes a
    // predicate whose leading verb was already deconjugated ("attack or blocks
    // …"). Match each verb with its own `alt()` so both arrive here.
    let result: Result<(&str, Duration), nom::Err<OracleError<'_>>> = (
        alt((tag("attacks"), tag("attack"))),
        tag(" or "),
        alt((tag("blocks"), tag("block"))),
        preceded(
            tag(" "),
            alt((
                value(Duration::UntilEndOfTurn, tag("this turn if able")),
                value(
                    Duration::UntilEndOfCombat,
                    alt((tag("this combat if able"), tag("that combat if able"))),
                ),
            )),
        ),
    )
        .map(|(_, _, _, duration)| duration)
        .parse(trimmed);

    if let Ok((rest, duration)) = result {
        if rest.is_empty() {
            return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
                static_abilities: must_attack_or_block_static_definitions(),
                duration: Some(duration),
                target: None,
            }));
        }
    }

    None
}

/// CR 508.1d: Build the `StaticDefinition` for a transient "attacks if able"
/// requirement. `Effect::GenericEffect` resolution snapshots only
/// `static_def.modifications` (the `mode` field is inert for a transient grant —
/// `snapshot_transient_modifications` never reads it), so the `MustAttack` mode
/// must be carried by an explicit `AddStaticMode` modification to actually reach
/// the layer system and `combat.rs` enforcement. Mirrors the block path's
/// `ImperativeFamilyAst::MustBeBlocked` lowering.
pub(super) fn must_attack_static_definition() -> StaticDefinition {
    use crate::types::statics::StaticMode;
    StaticDefinition::new(StaticMode::MustAttack).modifications(vec![
        ContinuousModification::AddStaticMode {
            mode: StaticMode::MustAttack,
        },
    ])
}

/// CR 509.1c: Build the `StaticDefinition` for a transient "blocks if able"
/// requirement. Mirrors [`must_attack_static_definition`]: the `MustBlock` mode
/// must be carried by an explicit `AddStaticMode` modification so the transient
/// `Effect::GenericEffect` grant reaches the layer system and `combat.rs`
/// declare-blockers enforcement (the `mode` field on `StaticDefinition` is inert
/// for transient grants — `snapshot_transient_modifications` reads only
/// `modifications`).
pub(super) fn must_block_static_definition() -> StaticDefinition {
    use crate::types::statics::StaticMode;
    StaticDefinition::new(StaticMode::MustBlock).modifications(vec![
        ContinuousModification::AddStaticMode {
            mode: StaticMode::MustBlock,
        },
    ])
}

/// CR 508.1d + CR 509.1c: Build both static definitions for the combined
/// "attacks or blocks ... if able" requirement (Hustle). The requirement is the
/// composition of an attack requirement (CR 508.1d, obeyed during the
/// controller's declare-attackers step) and a block requirement (CR 509.1c,
/// obeyed during a later declare-blockers step) — each is checked independently
/// at its own step, exactly as the continuous "attacks or blocks each combat if
/// able" static (`try_parse_scoped_must_attack_block`) emits both
/// `MustAttack` and `MustBlock`.
pub(super) fn must_attack_or_block_static_definitions() -> Vec<StaticDefinition> {
    vec![
        must_attack_static_definition(),
        must_block_static_definition(),
    ]
}

/// CR 508.1d / CR 509.1c: True iff `lower` (already lowercased, trimmed) is a
/// recognized *standalone* combat requirement — "attack(s) [player] this
/// turn/combat if able", "attack(s) or block(s) this turn/combat if able", or
/// "must be blocked [this turn] [if able]". Used by `split_clause_sequence` to
/// gate the trailing-conjunct split of
/// "gains <keyword> until end of turn and <combat requirement>" so the
/// requirement reaches its existing standalone parser. Composes the existing
/// recognizers as Some/None classifiers; their produced AST is discarded.
pub(crate) fn is_standalone_combat_requirement(lower: &str) -> bool {
    let trimmed = lower.trim().trim_end_matches('.').trim();
    if try_parse_attack_or_block_if_able(trimmed).is_some() {
        return true;
    }
    if try_parse_attack_if_able(trimmed).is_some() {
        return true;
    }
    // CR 509.1c: "must be blocked [this turn] [if able]" — mirrors the
    // imperative `"must"` verb arm.
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("must be blocked").parse(trimmed) {
        let rest = rest.trim();
        return rest.is_empty()
            || rest == "this turn if able"
            || rest == "if able"
            || rest == "this turn";
    }
    false
}

/// Handles "can't be blocked [this turn]", "can't attack [this turn]", "can't block [this turn]",
/// and compound forms like "can't attack or block". These delegate to the subject.rs
/// static-granting machinery, wrapping the result in a `GenericEffect`.
fn try_parse_subjectless_cant(lower: &str) -> Option<ImperativeFamilyAst> {
    use crate::parser::oracle_effect::subject::{
        parse_restriction_modes, static_mode_needs_grant_propagation,
    };

    let trimmed = lower.trim_end_matches('.');

    // Determine duration from the suffix: "this combat" → UntilEndOfCombat,
    // "this turn" (or bare) → UntilEndOfTurn.
    let (clean, duration) = if let Some(c) = trimmed.strip_suffix(" this combat") {
        (c, Duration::UntilEndOfCombat)
    } else if let Some(c) = trimmed.strip_suffix(" this turn") {
        (c, Duration::UntilEndOfTurn)
    } else {
        (trimmed, Duration::UntilEndOfTurn)
    };

    // CR 702.18a / 702.11a: "can't be the target [of ...]" granted to the target
    // for a duration is a Shroud / Hexproof keyword grant (Vines of Vastwood). Map
    // it to the keyword so the targeting check applies the correct controller scope
    // (Hexproof leaves the controller able to target), reusing the enforced keyword
    // path rather than a scope-less rule static.
    if let Some(scope) = crate::parser::oracle_keyword::classify_cant_be_targeted(clean) {
        let keyword = match scope {
            crate::parser::oracle_keyword::CantBeTargetedScope::AnyPlayer => {
                crate::types::keywords::Keyword::Shroud
            }
            crate::parser::oracle_keyword::CantBeTargetedScope::OpponentsOnly => {
                crate::types::keywords::Keyword::Hexproof
            }
        };
        return Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .modifications(vec![ContinuousModification::AddKeyword { keyword }])],
            duration: Some(duration),
            target: None,
        }));
    }

    let modes = parse_restriction_modes(clean)?;
    let statics: Vec<StaticDefinition> = modes
        .into_iter()
        .map(|mode| {
            // CR 508.1d + CR 509.1a + CR 509.1b (issue #327): Duration-scoped
            // combat restriction modes must carry an `AddStaticMode`
            // modification so the transient continuous effect propagates
            // them onto the recipient's `static_definitions` at layer-apply
            // time. Without this, the runtime block / attack check never
            // sees the rule. Mirrors the injection in
            // `subject::build_restriction_clause`.
            let needs_propagation = static_mode_needs_grant_propagation(&mode);
            let mut def = StaticDefinition::new(mode.clone());
            if needs_propagation {
                def = def.modifications(vec![ContinuousModification::AddStaticMode { mode }]);
            }
            def
        })
        .collect();
    Some(ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
        static_abilities: statics,
        duration: Some(duration),
        target: None,
    }))
}

/// CR 701.39a: Parse "bolster N" from Oracle text.
fn try_parse_bolster(lower: &str) -> Option<Effect> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("bolster ").parse(lower).ok()?;
    let rest = rest.trim().trim_end_matches('.');

    let count = parse_count_expr(rest).map(|(q, _)| q)?;

    Some(Effect::Bolster { count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ParitySource;

    fn typed_leg(filter: &TargetFilter) -> Option<&TypedFilter> {
        match filter {
            TargetFilter::Typed(tf) => Some(tf),
            TargetFilter::And { filters } => filters.iter().find_map(typed_leg),
            _ => None,
        }
    }

    fn is_stack_spell_leg(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::StackSpell => true,
            TargetFilter::And { filters } => filters.iter().any(is_stack_spell_leg),
            _ => false,
        }
    }

    /// CR 122.1 + CR 608.2d (Clockspinning sentence 2): the add-or-remove anaphor
    /// clause is recognized as `AddOrRemove`. The negative case proves the
    /// self-contained combinator returns `None` for an ordinary "remove a <type>
    /// counter" clause, leaving the existing remove path untouched.
    #[test]
    fn try_parse_choose_counter_adjustment_recognizes_add_or_remove() {
        assert_eq!(
            try_parse_choose_counter_adjustment(
                "remove that counter from that permanent or card or put another of those counters on it"
            ),
            Some(CounterAdjustment::AddOrRemove)
        );
        // Trailing punctuation and the bare "it" anaphor are both accepted.
        assert_eq!(
            try_parse_choose_counter_adjustment(
                "remove that counter or put another of those counters on it."
            ),
            Some(CounterAdjustment::AddOrRemove)
        );
        // An ordinary remove clause must NOT be captured here.
        assert_eq!(
            try_parse_choose_counter_adjustment("remove a +1/+1 counter from ~"),
            None
        );
        assert_eq!(
            try_parse_choose_counter_adjustment("remove all +1/+1 counters from it"),
            None
        );
    }

    /// CR 122.1: the remove-only form ("remove that counter [from <anaphor>]"
    /// without an add-back clause) parses as `Remove`, making the `Remove`
    /// variant reachable from the parser.
    #[test]
    fn try_parse_choose_counter_adjustment_recognizes_remove_only() {
        // Bare "it" anaphor.
        assert_eq!(
            try_parse_choose_counter_adjustment("remove that counter from it"),
            Some(CounterAdjustment::Remove)
        );
        // "that permanent" anaphor with trailing punctuation.
        assert_eq!(
            try_parse_choose_counter_adjustment("remove that counter from that permanent."),
            Some(CounterAdjustment::Remove)
        );
        // "that permanent or card" anaphor.
        assert_eq!(
            try_parse_choose_counter_adjustment("remove that counter from that permanent or card"),
            Some(CounterAdjustment::Remove)
        );
        // Clause without an explicit "from <anaphor>" phrase.
        assert_eq!(
            try_parse_choose_counter_adjustment("remove that counter"),
            Some(CounterAdjustment::Remove)
        );
        // Must NOT fire for an ordinary type-specific remove.
        assert_eq!(
            try_parse_choose_counter_adjustment("remove a +1/+1 counter from it"),
            None
        );
    }

    /// CR 122.1: the add-only form ("put another of those counters on <anaphor>")
    /// without a preceding remove clause parses as `Add`, making the `Add`
    /// variant reachable from the parser.
    #[test]
    fn try_parse_choose_counter_adjustment_recognizes_add_only() {
        // Bare "it" anaphor.
        assert_eq!(
            try_parse_choose_counter_adjustment("put another of those counters on it"),
            Some(CounterAdjustment::Add)
        );
        // "that permanent" anaphor with trailing punctuation.
        assert_eq!(
            try_parse_choose_counter_adjustment("put another of those counters on that permanent."),
            Some(CounterAdjustment::Add)
        );
        // "that permanent or card" anaphor.
        assert_eq!(
            try_parse_choose_counter_adjustment(
                "put another of those counters on that permanent or card"
            ),
            Some(CounterAdjustment::Add)
        );
        // Must NOT fire for an ordinary PutCounter clause.
        assert_eq!(
            try_parse_choose_counter_adjustment("put a +1/+1 counter on it"),
            None
        );
    }

    /// CR 122.1 + CR 115.1 + CR 702.62b (Clockspinning sentence 1): "choose a
    /// counter on target permanent or suspended card" designates the target only
    /// and lowers to `TargetOnly` over the battlefield∪exile `Or` pool.
    #[test]
    fn try_parse_choose_counter_on_target_yields_permanent_or_suspended_card() {
        let ast =
            try_parse_choose_counter_on_target("a counter on target permanent or suspended card.")
                .expect("counter-on-target designation must parse");
        let ChooseImperativeAst::TargetOnly { target } = ast else {
            panic!("expected TargetOnly, got {ast:?}");
        };
        let TargetFilter::Or { filters } = target else {
            panic!("expected an Or target pool, got {target:?}");
        };
        assert_eq!(
            filters.len(),
            2,
            "battlefield permanent + suspended card legs"
        );
        // Battlefield-permanent leg carries an explicit InZone{Battlefield}.
        assert!(filters.iter().any(|f| matches!(
            f,
            TargetFilter::Typed(tf)
                if tf.type_filters == vec![TypeFilter::Permanent]
                    && tf.properties.contains(&FilterProp::InZone { zone: Zone::Battlefield })
        )));
        // Suspended-card leg: exile + suspend keyword + time counter.
        assert!(filters.iter().any(|f| matches!(
            f,
            TargetFilter::Typed(tf)
                if tf.properties.contains(&FilterProp::InZone { zone: Zone::Exile })
                    && tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::HasKeywordKind {
                            value: crate::types::keywords::KeywordKind::Suspend
                        }
                    ))
        )));
    }

    fn has_type(tf: &TypedFilter, ty: TypeFilter) -> bool {
        tf.type_filters.iter().any(|candidate| candidate == &ty)
    }

    fn has_prop(tf: &TypedFilter, prop: FilterProp) -> bool {
        tf.properties.iter().any(|candidate| candidate == &prop)
    }

    /// CR 701.60a: the "no longer suspected" un-designation parses to
    /// `Effect::Unsuspect`, mapping each card's subject to the right filter +
    /// scope:
    ///   - anaphoric "it" / "they" / "become" residue → ParentTarget, Single
    ///   - printed-name anaphor "~" / "this creature" → SelfRef, Single
    ///   - "all suspected creatures" → a Suspected-creature filter, All (mass)
    #[test]
    fn parse_no_longer_suspected_subject_mapping() {
        use crate::types::ability::EffectScope;

        // Anaphoric pronouns → ParentTarget, single scope (read announced
        // target(s)).
        for text in [
            "it's no longer suspected",
            "it is no longer suspected",
            "they're no longer suspected",
            "they are no longer suspected",
            // Deadly Complication causative residue after "you may have it ".
            "become no longer suspected",
        ] {
            assert!(
                matches!(
                    parse_no_longer_suspected_ast(text),
                    Some(Effect::Unsuspect {
                        target: TargetFilter::ParentTarget,
                        scope: EffectScope::Single,
                    })
                ),
                "{text:?} should map to Unsuspect(ParentTarget, Single)"
            );
        }

        // Printed-name anaphor → SelfRef, single scope.
        for text in [
            "~ is no longer suspected",
            "this creature is no longer suspected",
        ] {
            assert!(
                matches!(
                    parse_no_longer_suspected_ast(text),
                    Some(Effect::Unsuspect {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                    })
                ),
                "{text:?} should map to Unsuspect(SelfRef, Single)"
            );
        }

        // "all suspected creatures" → a typed Suspected filter under the mass
        // (All) scope — CR 701.60a removes the designation from every match.
        let all = parse_no_longer_suspected_ast("all suspected creatures are no longer suspected");
        match all {
            Some(Effect::Unsuspect { target, scope }) => {
                let tf = typed_leg(&target).expect("typed filter for 'all suspected creatures'");
                assert!(
                    has_prop(tf, FilterProp::Suspected),
                    "filter should carry the Suspected property, got {target:?}"
                );
                assert_eq!(
                    scope,
                    EffectScope::All,
                    "an explicit mass noun phrase must use the All scope"
                );
            }
            other => panic!("expected Unsuspect for 'all suspected creatures', got {other:?}"),
        }

        // A non-clause-final "no longer suspected" fragment must NOT match.
        assert!(
            parse_no_longer_suspected_ast("it's no longer suspected and draws a card").is_none(),
            "trailing clause must prevent a false match"
        );
    }

    /// CR 701.60a: end-to-end through `parse_effect` — the same dispatch the card
    /// loader uses — so the interception is reachable in production, not just via
    /// the helper. Frantic Scapegoat's self-referential form lowers to SelfRef.
    #[test]
    fn parse_effect_self_no_longer_suspected_is_unsuspect_selfref() {
        use crate::parser::oracle_effect::parse_effect;
        let effect = parse_effect("~ is no longer suspected.");
        assert!(
            matches!(
                effect,
                Effect::Unsuspect {
                    target: TargetFilter::SelfRef,
                    scope: crate::types::ability::EffectScope::Single,
                }
            ),
            "expected Unsuspect(SelfRef, Single), got {effect:?}"
        );
    }

    /// CR 107.1a + CR 121.1: Change B — `parse_dynamic_count_phrase` routes a
    /// fraction-led draw count ("cards equal to half the number of cards in
    /// their library") through `parse_fraction_rounded` FIRST, yielding a
    /// `DivideRounded` over the target's library count (Peer into the Abyss).
    #[test]
    fn dynamic_count_phrase_fraction_routes_to_divide_rounded() {
        let qty =
            parse_dynamic_count_phrase("cards equal to half the number of cards in their library")
                .expect("fraction-led draw count must parse");
        match qty {
            QuantityExpr::DivideRounded {
                inner,
                divisor,
                rounding,
            } => {
                assert_eq!(divisor, 2);
                // Rounding defaults to Down here; the trailing "Round up each
                // time." post-pass flips it to Up at the card level.
                assert_eq!(rounding, crate::types::ability::RoundingMode::Down);
                assert!(
                    matches!(
                        *inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::TargetZoneCardCount {
                                zone: crate::types::ability::ZoneRef::Library
                            }
                        }
                    ),
                    "expected TargetZoneCardCount{{Library}} inner, got {inner:?}"
                );
            }
            other => panic!("expected DivideRounded, got {other:?}"),
        }
    }

    /// Change B fallback guard: a NON-fraction "cards equal to <ref>" draw
    /// count still parses to its prior `QuantityExpr::Ref` via the unchanged
    /// semantic `parse_quantity_ref` call — `parse_fraction_rounded` misses on
    /// the non-fraction lead and falls through (no regression).
    #[test]
    fn dynamic_count_phrase_nonfraction_falls_through_to_ref() {
        let qty = parse_dynamic_count_phrase("cards equal to the number of creatures you control")
            .expect("non-fraction draw count must still parse");
        assert!(
            matches!(qty, QuantityExpr::Ref { .. }),
            "non-fraction draw count must remain a plain Ref, got {qty:?}"
        );
    }

    /// CR 122.1: `counter_placement_is_mass` recognizes "on each"/"on all"
    /// mass placements and rejects targeted ("on target") or anaphoric
    /// ("on it") single-object placements.
    #[test]
    fn counter_placement_is_mass_predicate() {
        assert!(counter_placement_is_mass(
            "a +1/+1 counter on each creature"
        ));
        assert!(counter_placement_is_mass(
            "a loyalty counter on all planeswalkers"
        ));
        assert!(!counter_placement_is_mass(
            "a +1/+1 counter on target creature"
        ));
        assert!(!counter_placement_is_mass("a stun counter on it"));
    }

    /// Issue #408 — "Counter target activated ability, triggered ability, or
    /// noncreature spell" (Louisoix's Sacrifice) must parse to the full
    /// three-way disjunction, not a degenerate empty-`type_filters` stack
    /// filter. The noncreature restriction must survive on the spell leg.
    #[test]
    fn parse_counter_louisoix_three_way_disjunction() {
        let text = "Counter target activated ability, triggered ability, or noncreature spell.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("Louisoix's Sacrifice counter clause should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        let TargetFilter::Or { filters } = &target else {
            panic!("expected Or {{ ability, noncreature-spell }}, got {target:?}");
        };
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::StackAbility {
                    controller: None,
                    tag: None,
                    kind: None,
                }
            )),
            "missing the activated/triggered ability disjunct: {target:?}"
        );
        let spell_leg = filters
            .iter()
            .find_map(typed_leg)
            .expect("missing the typed noncreature-spell disjunct");
        assert!(
            has_type(spell_leg, TypeFilter::Non(Box::new(TypeFilter::Creature))),
            "the spell leg must EXCLUDE creature spells (noncreature restriction)"
        );
        assert!(
            has_prop(spell_leg, FilterProp::InZone { zone: Zone::Stack }),
            "the spell leg must be pinned to the stack zone"
        );
    }

    /// Spider-Sense — "Counter target instant spell, sorcery spell, or
    /// triggered ability." must parse to the full three-way disjunction, not
    /// the buggy bare `Typed { [Instant] }` that dropped the sorcery and
    /// triggered-ability legs. CR 701.6a + CR 115.1: every listed leg of the
    /// legal target set must be reproduced.
    #[test]
    fn parse_counter_spider_sense_spell_first_disjunction() {
        let text = "Counter target instant spell, sorcery spell, or triggered ability.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("Spider-Sense counter clause should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        // Regression guard for the exact bug: it must NOT be the bare
        // instant-only `Typed` filter.
        assert!(
            !matches!(&target, TargetFilter::Typed(_)),
            "Spider-Sense must not parse to a bare Typed filter (the instant-only bug): {target:?}"
        );
        let TargetFilter::Or { filters } = &target else {
            panic!("expected Or {{ instant, sorcery, ability }}, got {target:?}");
        };
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::StackAbility {
                    controller: None,
                    tag: None,
                    kind: Some(crate::types::ability::StackAbilityKind::Triggered),
                }
            )),
            "missing the triggered-ability disjunct: {target:?}"
        );
        let instant_leg = filters
            .iter()
            .filter_map(typed_leg)
            .find(|tf| has_type(tf, TypeFilter::Instant))
            .expect("missing the instant-spell disjunct");
        assert!(
            has_prop(instant_leg, FilterProp::InZone { zone: Zone::Stack }),
            "the instant leg must be pinned to the stack zone: {instant_leg:?}"
        );
        let sorcery_leg = filters
            .iter()
            .filter_map(typed_leg)
            .find(|tf| has_type(tf, TypeFilter::Sorcery))
            .expect("missing the sorcery-spell disjunct (the dropped leg)");
        assert!(
            has_prop(sorcery_leg, FilterProp::InZone { zone: Zone::Stack }),
            "the sorcery leg must be pinned to the stack zone: {sorcery_leg:?}"
        );
    }

    /// Disallow / Voidslime / Overcharged Amalgam / Ertai Resurrected —
    /// "Counter target spell, activated ability, or triggered ability." must
    /// parse to `Or { spell, ability }`, not the buggy bare `StackSpell` that
    /// dropped BOTH ability legs (the highest-printing member of the class).
    /// CR 701.6a + CR 113.3b/113.3c.
    #[test]
    fn parse_counter_disallow_three_way_disjunction() {
        let text = "Counter target spell, activated ability, or triggered ability.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("Disallow counter clause should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        // Regression guard for the exact bug: it must NOT be the bare
        // StackSpell that silently dropped both ability legs.
        assert!(
            !matches!(&target, TargetFilter::StackSpell),
            "Disallow must not parse to bare StackSpell (the dropped-abilities bug): {target:?}"
        );
        let TargetFilter::Or { filters } = &target else {
            panic!("expected Or {{ spell, ability }}, got {target:?}");
        };
        assert!(
            filters.iter().any(|f| matches!(
                f,
                TargetFilter::StackAbility {
                    controller: None,
                    tag: None,
                    kind: None,
                }
            )),
            "missing the activated/triggered ability disjunct: {target:?}"
        );
        let spell_leg = filters
            .iter()
            .find_map(typed_leg)
            .expect("missing the bare-spell disjunct");
        assert!(
            has_type(spell_leg, TypeFilter::Card),
            "the bare-spell leg must carry TypeFilter::Card: {spell_leg:?}"
        );
        assert!(
            has_prop(spell_leg, FilterProp::InZone { zone: Zone::Stack }),
            "the bare-spell leg must be pinned to the stack zone: {spell_leg:?}"
        );
    }

    /// Issue #899 regression — "counter target spell with mana value X or
    /// less, where X is the number of Faeries you control" (Spellstutter
    /// Sprite) must resolve the `Cmc` bound to the defining `where X is …`
    /// expression rather than leaving it as the bare `Variable("X")` that
    /// collapses to 0 at resolution. Building-block coverage: this exercises
    /// `strip_trailing_where_x` + `apply_where_x_to_filter` composed under
    /// `parse_counter_ast`, so every counter-target-spell-with-where-X card
    /// (Faerie Trickery, Filigree Sages variants, etc.) is covered by the
    /// same path.
    /// CR 107.3i (shared X on an object) + CR 202.3 (mana value). Target
    /// legality re-check on resolution (CR 608.2b) is exercised by the
    /// integration tests in
    /// `tests/integration/spellstutter_sprite_counter_with_x.rs`.
    #[test]
    fn parse_counter_target_spell_with_where_x_cmc_bound() {
        let text = "counter target spell with mana value X or less, where X is the number of Faeries you control.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("Spellstutter Sprite counter clause should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        let typed = typed_leg(&target).unwrap_or_else(|| {
            panic!("expected a typed leg under the stack constraint, got {target:?}")
        });
        let cmc = typed
            .properties
            .iter()
            .find_map(|p| match p {
                FilterProp::Cmc { comparator, value } => Some((comparator, value)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a Cmc bound in the typed leg, got {typed:?}"));
        assert_eq!(
            *cmc.0,
            crate::types::ability::Comparator::LE,
            "Spellstutter's 'or less' clause must parse as a <= mana-value bound"
        );
        let cmc = cmc.1;
        let QuantityExpr::Ref { qty } = cmc else {
            panic!("expected the Cmc bound to be a dynamic Ref, got {cmc:?}");
        };
        let QuantityRef::ObjectCount { filter } = qty else {
            panic!("expected the Cmc bound to resolve to ObjectCount(Faeries you control), got {qty:?} — the where-X binding was dropped");
        };
        let object_typed = match filter {
            TargetFilter::Typed(tf) => tf,
            other => panic!("expected ObjectCount over a Typed filter, got {other:?}"),
        };
        assert!(
            object_typed
                .type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Faerie"))),
            "ObjectCount filter must include the Faerie subtype: {object_typed:?}"
        );
        assert_eq!(
            object_typed.controller,
            Some(ControllerRef::You),
            "ObjectCount filter must be controller-scoped to You: {object_typed:?}"
        );
    }

    /// Issue #408 regression guard — a plain "Counter target spell"
    /// (Counterspell) must still yield a stack-spell filter; the new
    /// stack-object combinator must not steal the simple case.
    #[test]
    fn parse_counter_plain_spell_unchanged() {
        let text = "Counter target spell.";
        let ast = parse_counter_ast(text, &text.to_lowercase())
            .expect("\"Counter target spell\" should parse");
        let ZoneCounterImperativeAst::Counter { target, .. } = ast else {
            panic!("expected a Counter AST");
        };
        // "Counter target spell" → a stack-pinned spell filter (StackSpell or
        // a stack-constrained Typed Card filter). It must NOT be an Or with an
        // ability disjunct.
        assert!(
            !matches!(&target, TargetFilter::Or { .. }),
            "plain \"Counter target spell\" must not gain an ability disjunct: {target:?}"
        );
        let is_spell_filter = is_stack_spell_leg(&target)
            || typed_leg(&target)
                .is_some_and(|tf| has_prop(tf, FilterProp::InZone { zone: Zone::Stack }));
        assert!(
            is_spell_filter,
            "plain \"Counter target spell\" must yield a stack-spell filter, got {target:?}"
        );
    }

    #[test]
    fn parse_outside_game_wish_reveal_to_hand() {
        let ability = super::super::parse_effect_chain(
            "You may reveal a sorcery card you own from outside the game and put it into your hand. Exile ~.",
            AbilityKind::Spell,
        );
        assert!(
            !ability.optional,
            "the reveal choice is optional; the self-exile sentence is mandatory"
        );
        assert!(matches!(&*ability.effect, Effect::SearchOutsideGame { .. }));
        assert!(matches!(
            ability.sub_ability.as_deref().map(|sub| &*sub.effect),
            Some(Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            })
        ));

        let effect = super::super::parse_effect(
            "reveal a sorcery card you own from outside the game and put it into your hand",
        );
        match effect {
            Effect::SearchOutsideGame {
                filter,
                count,
                reveal,
                destination,
                source_pool,
            } => {
                assert_eq!(count, QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 }));
                assert!(reveal);
                assert_eq!(destination, Zone::Hand);
                assert!(
                    !source_pool.includes_face_up_exile(),
                    "legacy single-branch wording must default to sideboard-only"
                );
                match filter {
                    TargetFilter::Typed(typed) => {
                        assert!(typed.type_filters.contains(&TypeFilter::Sorcery));
                    }
                    other => panic!("expected sorcery filter, got {other:?}"),
                }
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }

        let effect = super::super::parse_effect(
            "reveal a creature card you own from outside the game and put it into your hand",
        );
        match effect {
            Effect::SearchOutsideGame { filter, .. } => match filter {
                TargetFilter::Typed(typed) => {
                    assert!(typed.type_filters.contains(&TypeFilter::Creature));
                }
                other => panic!("expected creature filter, got {other:?}"),
            },
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// CR 406.3 + CR 400.11: Karn, the Great Creator's -2 reveals OR pulls
    /// from face-up exile. Verifies the Karn-class disjunction sets
    /// `source_pool: OutsideGameSourcePool::SideboardAndFaceUpExile`, identifies the artifact filter, and
    /// captures Hand as the destination. The trailing "Put that card into
    /// your hand." sentence (CR 608.2c anaphoric "that card") is absorbed
    /// into the parent effect's destination by the chain splitter, so no
    /// separate ChangeZone sub-ability is built.
    #[test]
    fn parse_outside_game_karn_minus_two() {
        let ability = super::super::parse_effect_chain(
            "You may reveal an artifact card you own from outside the game or choose a face-up artifact card you own in exile. Put that card into your hand.",
            AbilityKind::Activated,
        );
        match &*ability.effect {
            Effect::SearchOutsideGame {
                filter,
                destination,
                source_pool,
                reveal,
                ..
            } => {
                assert!(
                    source_pool.includes_face_up_exile(),
                    "Karn-class disjunction must set source_pool"
                );
                assert!(reveal);
                assert_eq!(*destination, Zone::Hand);
                match filter {
                    TargetFilter::Typed(typed) => {
                        assert!(
                            typed.type_filters.contains(&TypeFilter::Artifact),
                            "artifact filter must be recognized from the outside-game branch"
                        );
                    }
                    other => panic!("expected artifact filter, got {other:?}"),
                }
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// CR 406.3 + CR 400.11: Coax from the Blind Eternities — same Karn-class
    /// disjunction with an Eldrazi subtype filter. Confirms the parser is
    /// filter-agnostic and the disjunction trigger is structural, not
    /// keyword-specific.
    #[test]
    fn parse_outside_game_coax_blind_eternities() {
        let ability = super::super::parse_effect_chain(
            "You may reveal an Eldrazi card you own from outside the game or choose a face-up Eldrazi card you own in exile. Put that card into your hand.",
            AbilityKind::Spell,
        );
        match &*ability.effect {
            Effect::SearchOutsideGame {
                filter,
                source_pool,
                destination,
                ..
            } => {
                assert!(
                    source_pool.includes_face_up_exile(),
                    "Coax disjunction must set source_pool"
                );
                assert_eq!(*destination, Zone::Hand);
                // CR 205.3m: Eldrazi is a creature subtype; the search filter
                // carries it as TypeFilter::Subtype within `type_filters`.
                match filter {
                    TargetFilter::Typed(typed) => {
                        let has_eldrazi = typed.type_filters.iter().any(|tf| {
                            matches!(tf, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("eldrazi"))
                        });
                        assert!(
                            has_eldrazi,
                            "expected Eldrazi subtype filter, got {:?}",
                            typed.type_filters
                        );
                    }
                    other => panic!("expected typed filter, got {other:?}"),
                }
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// Regression: legacy single-branch "reveal a … from outside the game"
    /// wording (Wish, Cunning Wish, Burning Wish) must still parse with
    /// `source_pool: OutsideGameSourcePool::Sideboard` so non-Karn wishboard cards keep their
    /// sideboard-only resolution path.
    #[test]
    fn parse_outside_game_legacy_single_branch_still_works() {
        let effect = super::super::parse_effect(
            "reveal a sorcery card you own from outside the game and put it into your hand",
        );
        match effect {
            Effect::SearchOutsideGame {
                source_pool,
                destination,
                ..
            } => {
                assert!(
                    !source_pool.includes_face_up_exile(),
                    "legacy wishboard text must NOT enable face-up exile"
                );
                assert_eq!(destination, Zone::Hand);
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// Regression (issue #1976): Wish (M19) — "You may play a card you own from
    /// outside the game this turn." must parse to `SearchOutsideGame` (the
    /// sideboard / wishboard pool, CR 400.11 + CR 400.11a + CR 701.23j), NOT a
    /// `CastFromZone` that targets an in-game permanent the controller already
    /// owns. The bare "a card" filter (no type adjective) must widen to
    /// `TargetFilter::Any`, the destination defaults to Hand (CR 400.11b), the
    /// play verb suppresses the reveal, and the choice is optional ("up to").
    #[test]
    fn parse_outside_game_wish_play_from_sideboard() {
        let ability = super::super::parse_effect_chain(
            "You may play a card you own from outside the game this turn.",
            AbilityKind::Spell,
        );
        match &*ability.effect {
            Effect::SearchOutsideGame {
                filter,
                count,
                reveal,
                destination,
                source_pool,
            } => {
                assert_eq!(
                    *filter,
                    TargetFilter::Any,
                    "bare \"a card\" must widen to any card"
                );
                assert!(count.is_up_to(), "Wish's play choice is optional (up to 1)");
                assert!(!*reveal, "the play form does not reveal the chosen card");
                assert_eq!(*destination, Zone::Hand);
                assert!(
                    !source_pool.includes_face_up_exile(),
                    "Wish is sideboard-only, not a Karn-class disjunction"
                );
            }
            other => panic!(
                "Wish must parse to SearchOutsideGame, not a permanent-targeting cast; got {other:?}"
            ),
        }

        // End-to-end through the canonical card pipeline: the full Wish card
        // must yield a single SearchOutsideGame ability, never a CastFromZone.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "You may play a card you own from outside the game this turn.",
            "Wish",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        assert!(
            parsed
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::SearchOutsideGame { .. })),
            "Wish card pipeline must produce SearchOutsideGame, got {:?}",
            parsed
                .abilities
                .iter()
                .map(|ability| &ability.effect)
                .collect::<Vec<_>>()
        );
        assert!(
            !parsed
                .abilities
                .iter()
                .any(|ability| matches!(&*ability.effect, Effect::CastFromZone { .. })),
            "Wish must not parse to a permanent-targeting CastFromZone"
        );
    }

    /// SHAPE (issue #4241): Teferi's Puzzle Box's verbatim Oracle text parses to
    /// a per-player draw-step trigger whose execute effect is a whole-hand
    /// `ChangeZoneAll { origin: Hand, destination: Library, library_position:
    /// Some(Bottom) }` with a `Draw` sub-ability ("then draws that many cards"),
    /// NOT a no-op `PutAtLibraryPosition`/`Unimplemented`. The zero-Unimplemented
    /// sweep is the positive reach-guard that keeps the shape assertion from
    /// passing vacuously (card-test foot-gun #6).
    #[test]
    fn puzzle_box_hand_to_bottom_then_draw_shape() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "At the beginning of each player's draw step, that player puts the cards \
             in their hand on the bottom of their library in any order, then draws \
             that many cards.",
            "Teferi's Puzzle Box",
            &[],
            &["Artifact".to_string()],
            &[],
        );

        // Positive reach-guard: nothing anywhere fell to Unimplemented.
        fn walk<'a>(ability: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
            out.push(&ability.effect);
            if let Some(sub) = &ability.sub_ability {
                walk(sub, out);
            }
        }
        let mut effects: Vec<&Effect> = Vec::new();
        for ability in &parsed.abilities {
            walk(ability, &mut effects);
        }
        for trigger in &parsed.triggers {
            if let Some(exec) = &trigger.execute {
                walk(exec, &mut effects);
            }
        }
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Unimplemented { .. })),
            "no clause may fall to Unimplemented; got {effects:?}"
        );

        let trigger = parsed
            .triggers
            .iter()
            .find(|t| t.execute.is_some())
            .expect("Puzzle Box yields a triggered ability");
        let exec = trigger.execute.as_ref().unwrap();

        match &*exec.effect {
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                library_position: Some(LibraryPosition::Bottom),
                ..
            } => {}
            other => panic!(
                "execute must be a hand->library ChangeZoneAll placed on the bottom, got {other:?}"
            ),
        }

        let draw_sub = exec
            .sub_ability
            .as_ref()
            .expect("the hand-cycle must chain a 'draw that many' sub-ability");
        assert!(
            matches!(&*draw_sub.effect, Effect::Draw { .. }),
            "the sub-ability must be a Draw, got {:?}",
            draw_sub.effect
        );
    }

    /// Regression (issue #1976): the "cast a … card you own from outside the
    /// game" sibling surface form (Cunning Wish class for spells) routes through
    /// the same outside-game pool, carrying its type filter.
    #[test]
    fn parse_outside_game_cast_typed_from_sideboard() {
        let effect = super::super::parse_effect(
            "You may cast an instant card you own from outside the game this turn.",
        );
        match effect {
            Effect::SearchOutsideGame {
                filter,
                source_pool,
                reveal,
                ..
            } => {
                assert!(!reveal, "the cast form does not reveal");
                assert!(!source_pool.includes_face_up_exile());
                let typed = typed_leg(&filter).expect("instant filter must be typed");
                assert!(
                    typed.type_filters.contains(&TypeFilter::Instant),
                    "expected instant filter, got {:?}",
                    typed.type_filters
                );
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
    }

    /// CR 701.27 + CR 608.2c + CR 608.2k: "transform it" / "convert itself" —
    /// bare object pronoun resolves to `ParentTarget` when no trigger subject
    /// is set on the parse context. At resolution time, `ParentTarget` with
    /// empty `ability.targets` falls back to the source per CR 608.2c
    /// (`targeting::resolved_targets`), so DFC self-transform sub-abilities
    /// (Primal Amulet class) still target the source. The trigger-subject
    /// anaphor case (typed subject → `TriggeringSource`) is exercised at the
    /// `parse_target_with_ctx` layer in `oracle_target` tests.
    #[test]
    fn parse_transform_self_pronouns() {
        for input in [
            "transform it",
            "transform itself",
            "convert it",
            "convert itself",
        ] {
            let result = parse_utility_imperative_ast(input, input, &mut ParseContext::default());
            let Some(UtilityImperativeAst::Transform { target }) = result else {
                panic!("{input}: expected Transform, got {result:?}");
            };
            assert!(
                matches!(target, TargetFilter::ParentTarget),
                "{input}: expected ParentTarget, got {target:?}"
            );
        }
    }

    #[test]
    fn parse_attach_triggering_object_to_last_created_token() {
        let input = "attach it to the token";
        let result = parse_utility_imperative_ast(input, input, &mut ParseContext::default());
        let Some(UtilityImperativeAst::Attach {
            attachment,
            target,
            multi_target,
        }) = result
        else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert_eq!(attachment, TargetFilter::TriggeringSource);
        assert_eq!(target, TargetFilter::LastCreated);
        assert_eq!(multi_target, None);
    }

    #[test]
    fn parse_attach_target_equipment_to_self_pronoun() {
        for input in [
            "attach up to one target Equipment you control to her",
            "attach up to one target Equipment you control to it",
        ] {
            let lower = input.to_lowercase();
            let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
            let Some(UtilityImperativeAst::Attach {
                attachment,
                target,
                multi_target,
            }) = result
            else {
                panic!("{input}: expected Attach, got {result:?}");
            };
            assert_eq!(
                attachment,
                TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You)
                ),
                "{input}"
            );
            assert_eq!(target, TargetFilter::SelfRef, "{input}");
            assert_eq!(
                multi_target,
                Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
                "{input}"
            );
        }
    }

    #[test]
    fn parse_attach_recipient_it_preserves_available_parent_target() {
        let input = "attach up to one target Equipment you control to it";
        let lower = input.to_lowercase();
        let mut ctx = ParseContext {
            parent_target_available: true,
            ..Default::default()
        };
        let result = parse_utility_imperative_ast(input, &lower, &mut ctx);
        let Some(UtilityImperativeAst::Attach { target, .. }) = result else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert_eq!(target, TargetFilter::ParentTarget);
    }

    #[test]
    fn parse_attach_recipient_it_preserves_chosen_parent_target_for_self_subject() {
        let input = "attach up to one target Equipment you control to it";
        let lower = input.to_lowercase();
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            parent_target_available: true,
            parent_target_is_chosen: true,
            ..Default::default()
        };
        let result = parse_utility_imperative_ast(input, &lower, &mut ctx);
        let Some(UtilityImperativeAst::Attach { target, .. }) = result else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert_eq!(target, TargetFilter::ParentTarget);
    }

    #[test]
    fn parse_attach_up_to_one_other_target_preserves_other_filter() {
        let input = "attach up to one other target Equipment you control to her";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::Attach {
            attachment,
            target,
            multi_target,
        }) = result
        else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        let TargetFilter::Typed(typed) = attachment else {
            panic!("expected typed Equipment filter, got {attachment:?}");
        };
        assert!(typed.type_filters.iter().any(
            |filter| matches!(filter, TypeFilter::Subtype(subtype) if subtype == "Equipment")
        ));
        assert_eq!(typed.controller, Some(ControllerRef::You));
        assert!(typed.properties.contains(&FilterProp::Another));
        assert_eq!(target, TargetFilter::SelfRef);
        assert_eq!(
            multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }))
        );
    }

    #[test]
    fn parse_attach_equipment_was_attached_to_self_to_parent_target() {
        let input = "attach an Equipment that was attached to ~ to that creature";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::Attach {
            attachment,
            target,
            multi_target,
        }) = result
        else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        match attachment {
            TargetFilter::Typed(tf) => {
                assert!(tf
                    .type_filters
                    .iter()
                    .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Equipment")));
                assert!(tf.properties.contains(&FilterProp::AttachedToSource));
            }
            other => panic!("expected typed Equipment filter, got {other:?}"),
        }
        assert!(matches!(target, TargetFilter::ParentTarget));
        assert_eq!(multi_target, None);
    }

    #[test]
    fn parse_attach_target_equipment_to_target_creature() {
        let input = "attach target Equipment you control to target creature you control";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::Attach {
            attachment,
            target,
            multi_target,
        }) = result
        else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert_eq!(
            attachment,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Equipment".to_string())
                    .controller(ControllerRef::You)
            )
        );
        assert_eq!(
            target,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
        );
        assert_eq!(multi_target, None);
    }

    #[test]
    fn parse_unattach_all_equipment_from_target_creature() {
        let input = "unattach all Equipment from target creature";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::UnattachAll { attachment, target }) = result else {
            panic!("{input}: expected UnattachAll, got {result:?}");
        };
        assert_eq!(
            attachment,
            TargetFilter::Typed(TypedFilter::default().subtype("Equipment".to_string()))
        );
        assert_eq!(target, TargetFilter::Typed(TypedFilter::creature()));
    }

    #[test]
    fn parse_unattach_all_decomposes_attachment_type_and_pronoun_target() {
        let input = "unattach all Equipment from it";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::UnattachAll { attachment, target }) = result else {
            panic!("{input}: expected UnattachAll, got {result:?}");
        };
        assert_eq!(
            attachment,
            TargetFilter::Typed(TypedFilter::default().subtype("Equipment".to_string()))
        );
        assert!(matches!(target, TargetFilter::ParentTarget));
    }

    /// CR 608.2c + CR 301.5b: Gilgamesh attach body — moved Equipment binds on
    /// the attachment side; the Samurai recipient stays explicitly typed.
    #[test]
    fn parse_attach_one_of_them_to_samurai_you_control() {
        use crate::types::ability::{TypeFilter, TypedFilter};

        let input = "attach one of them to a Samurai you control";
        let lower = input.to_lowercase();
        let result = parse_utility_imperative_ast(input, &lower, &mut ParseContext::default());
        let Some(UtilityImperativeAst::Attach {
            attachment,
            target,
            multi_target,
        }) = result
        else {
            panic!("{input}: expected Attach, got {result:?}");
        };
        assert!(
            matches!(attachment, TargetFilter::ParentTarget),
            "attachment should bind to a chosen moved Equipment, got {attachment:?}"
        );
        assert!(
            matches!(
                target,
                TargetFilter::Typed(TypedFilter {
                    controller: Some(ControllerRef::You),
                    ref type_filters,
                    ..
                }) if type_filters.iter().any(
                    |f| matches!(f, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Samurai"))
                )
            ),
            "expected Samurai you control attach target, got {target:?}"
        );
        assert_eq!(multi_target, None);
    }

    /// CR 608.2k regression — issue #319 sibling.
    /// "attach ~ to it" inside a typed-subject trigger ("Whenever a Samurai
    /// or Warrior you control attacks alone … attach this Equipment to it"
    /// — Ancestral Katana) must bind "it" to the triggering creature, not
    /// the parent ability target. Verifies the ctx-threaded attach path.
    #[test]
    fn parse_attach_self_to_it_in_typed_trigger_binds_triggering_source() {
        for input in ["attach ~ to it", "attach this equipment to it"] {
            let mut ctx = ParseContext {
                subject: Some(TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .subtype("Samurai".to_string()),
                        ),
                        TargetFilter::Typed(
                            TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .subtype("Warrior".to_string()),
                        ),
                    ],
                }),
                ..Default::default()
            };
            let result = parse_utility_imperative_ast(input, input, &mut ctx);
            let Some(UtilityImperativeAst::Attach {
                attachment: _,
                target,
                multi_target,
            }) = result
            else {
                panic!("{input}: expected Attach, got {result:?}");
            };
            assert_eq!(target, TargetFilter::TriggeringSource, "{input}");
            assert_eq!(multi_target, None);
        }
    }

    /// CR 122.1 + CR 608.2k: the imperative dispatch gate routes anaphoric
    /// remove-counter clauses ("remove all of them" / "remove them" — the
    /// just-referenced counters established by a trigger's intervening-if) to
    /// the counter parser even though they carry no literal "counter" token.
    /// Building-block coverage for the level-up/incubate transform class
    /// (Ludevic's Test Subject, Smoldering Egg) at the dispatch layer.
    #[test]
    fn remove_counter_anaphor_routes_through_dispatch_gate() {
        for input in ["remove all of them", "remove them", "remove those counters"] {
            let lower = input.to_lowercase();
            let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
            let Some(ZoneCounterImperativeAst::RemoveCounter {
                counter_type: None,
                count: QuantityExpr::Fixed { value: -1 },
                target,
            }) = result
            else {
                panic!("{input}: expected anaphoric RemoveCounter (count -1), got {result:?}");
            };
            assert!(
                matches!(target, TargetFilter::SelfRef),
                "{input}: expected SelfRef target, got {target:?}"
            );
        }
    }

    #[test]
    fn parse_exile_from_your_hand_preserves_type_phrase_filter() {
        let input = "exile a nonartifact, nonland card from your hand";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            origin: Some(Zone::Hand),
            target: TargetFilter::Typed(filter),
            all: false,
            enter_with_counters,
            multi_target: _,
        }) = result
        else {
            panic!("{input}: expected hand-origin typed exile, got {result:?}");
        };

        assert!(enter_with_counters.is_empty());
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter.type_filters.contains(&TypeFilter::Card));
        assert!(filter
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Artifact))));
        assert!(filter
            .type_filters
            .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
        assert!(filter
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }));
    }

    #[test]
    fn parse_exile_all_creatures_and_spacecraft() {
        let input = "exile all creatures and Spacecraft";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            origin: None,
            target: TargetFilter::Or { filters },
            all: true,
            enter_with_counters,
            multi_target: _,
        }) = result
        else {
            panic!("{input}: expected mass exile type union, got {result:?}");
        };

        assert!(enter_with_counters.is_empty());
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0], TargetFilter::Typed(TypedFilter::creature()));
        assert_eq!(
            filters[1],
            TargetFilter::Typed(TypedFilter::default().subtype("Spacecraft".to_string()))
        );
    }

    #[test]
    fn parse_exile_each_creature_with_mana_value_chosen_quality() {
        let input = "exile each creature with mana value of the chosen quality";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            origin: None,
            target: TargetFilter::Typed(filter),
            all: true,
            enter_with_counters,
            multi_target: _,
        }) = result
        else {
            panic!("{input}: expected mass exile parity filter, got {result:?}");
        };

        assert!(enter_with_counters.is_empty());
        assert!(filter.type_filters.contains(&TypeFilter::Creature));
        assert!(filter.properties.contains(&FilterProp::ManaValueParity {
            parity: ParitySource::LastNamedChoice,
        }));
    }

    #[test]
    fn parse_exile_from_your_hand_handles_article_variants() {
        for (input, expected_type) in [
            ("exile an instant card from your hand", TypeFilter::Instant),
            ("exile a creature card from your hand", TypeFilter::Creature),
        ] {
            let lower = input.to_lowercase();
            let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
            let Some(ZoneCounterImperativeAst::Exile {
                origin: Some(Zone::Hand),
                target: TargetFilter::Typed(filter),
                all: false,
                enter_with_counters: _,
                multi_target: _,
            }) = result
            else {
                panic!("{input}: expected hand-origin typed exile, got {result:?}");
            };

            assert_eq!(filter.controller, Some(ControllerRef::You));
            assert!(filter.type_filters.contains(&expected_type));
            assert!(filter
                .properties
                .contains(&FilterProp::InZone { zone: Zone::Hand }));
        }
    }

    #[test]
    fn parse_exile_from_hand_with_dynamic_counter_suffix() {
        use crate::types::ability::{ObjectScope, QuantityExpr, QuantityRef, TypeFilter};
        use crate::types::counter::CounterType;
        // The Eleventh Doctor: "exile a card from your hand with a number of
        // time counters on it equal to its mana value."
        let input =
            "exile a card from your hand with a number of time counters on it equal to its mana value";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            origin: Some(Zone::Hand),
            target: TargetFilter::Typed(filter),
            all: false,
            enter_with_counters,
            multi_target: _,
        }) = result
        else {
            panic!("{input}: expected hand-origin typed exile, got {result:?}");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter.type_filters.contains(&TypeFilter::Card));
        assert!(filter
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }));
        assert_eq!(
            enter_with_counters,
            vec![(
                CounterType::Time,
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Recipient,
                    },
                },
            )],
        );
    }

    #[test]
    fn parse_exile_from_hand_unrecognized_tail_attributes_no_counters() {
        // Negative: an unrecognized `with …` tail that is NOT a counter suffix
        // makes the from-hand arm's `tail_is_consumed` guard fail, so the arm
        // declines and the parse falls through to the generic exile path.
        // The key invariant: an unrecognized tail must NEVER be misinterpreted
        // as a counter suffix — `enter_with_counters` stays empty regardless of
        // which arm produces the AST.
        let input = "exile a card from your hand with flying";
        let lower = input.to_lowercase();
        let result = parse_zone_counter_ast(input, &lower, &mut ParseContext::default());
        let Some(ZoneCounterImperativeAst::Exile {
            enter_with_counters,
            ..
        }) = result
        else {
            panic!("{input}: expected an Exile AST, got {result:?}");
        };
        assert!(
            enter_with_counters.is_empty(),
            "unrecognized `with` tail must not yield counters: {enter_with_counters:?}"
        );
    }

    #[test]
    fn parse_gain_life_equal_to_life_lost() {
        let text = "gain life equal to the life you've lost this turn";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        match result {
            Some(NumericImperativeAst::GainLife { amount }) => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::LifeLostThisTurn { .. }
                        }
                    ),
                    "Expected LifeLostThisTurn, got {amount:?}"
                );
            }
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    /// The Pride of Hull Clade (std long-tail): the granted ability body
    /// "draw cards equal to its toughness," closes at a comma (it is the inner
    /// quotation of a `gains "..."` clause). The trailing comma must be stripped
    /// before the anaphoric-ref match so the dynamic count resolves to
    /// `QuantityRef::Toughness { Anaphoric }` instead of `Effect::Unimplemented`.
    /// Revert-discriminating: with the comma in the qty text, the anaphoric match
    /// fails and `parse_numeric_imperative_ast` returns `None` (-> Unimplemented
    /// upstream). CR 121.1 + CR 113.3c (granted triggered-ability quotation).
    #[test]
    fn parse_draw_cards_equal_to_its_toughness_with_trailing_comma() {
        use crate::types::ability::QuantityRef;
        let text = "draw cards equal to its toughness,";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        match result {
            Some(NumericImperativeAst::Draw { count, up_to }) => {
                assert!(!up_to, "fixed-anaphor draw count is not 'up to'");
                // The "its toughness" anaphor binds to the source/granted creature
                // at parse time (Source) or remaps later (Anaphoric) — the
                // discriminating point is that the trailing comma no longer breaks
                // the dynamic-count parse into `Effect::Unimplemented`.
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Toughness { .. }
                        }
                    ),
                    "expected Draw count = a Toughness ref, got {count:?}"
                );
            }
            other => panic!("expected Draw with a toughness count, got {other:?}"),
        }
    }

    /// CR 121.1: a bare plural "draw cards" / "draw card" with no quantifier —
    /// the residue left after the vote parser strips a dynamic count tail
    /// ("... equal to the number of <X> votes") — yields a Fixed(1) placeholder
    /// the vote aggregate binder then rebinds to `QuantityRef::VoteCount`.
    #[test]
    fn parse_bare_draw_cards_yields_fixed_one() {
        for text in ["draw cards", "draw card", "draw cards."] {
            let lower = text.to_lowercase();
            match parse_numeric_imperative_ast(text, &lower) {
                Some(NumericImperativeAst::Draw { count, up_to }) => {
                    assert!(!up_to, "bare draw is not 'up to' for {text:?}");
                    assert_eq!(count, QuantityExpr::Fixed { value: 1 }, "text={text:?}");
                }
                other => panic!("expected bare Draw, got {other:?} for {text:?}"),
            }
        }
    }

    /// Negative guard: a bare-draw with an unmodeled tail must NOT collapse to
    /// Fixed(1) — it stays `None` (→ `Effect::Unimplemented` upstream) so the
    /// dynamic-quantity coverage gap is not silently hidden.
    #[test]
    fn parse_draw_cards_with_unmodeled_tail_is_none() {
        let text = "draw cards from the top of their library";
        let lower = text.to_lowercase();
        assert!(parse_numeric_imperative_ast(text, &lower).is_none());
    }

    #[test]
    fn parse_earthbend_verb() {
        let text = "Earthbend 3 target land";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse 'earthbend' verb");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power,
                toughness,
                keywords,
                ..
            } => {
                assert_eq!(power, Some(PtValue::Fixed(3)));
                assert_eq!(toughness, Some(PtValue::Fixed(3)));
                assert!(keywords.contains(&crate::types::keywords::Keyword::Haste));
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    // CR 709.5f + CR 709.5j: "unlock a locked door of up to one target Room you
    // control" (Ghostly Keybearer's combat-damage trigger) must lower to
    // `Effect::SetRoomDoorLock { op: Unlock }` targeting a Room you control. The
    // "a locked " narrowing and "up to one " optional-target flag are both
    // consumed by the combinator and don't bleed into the Room target filter.
    #[test]
    fn parse_unlock_locked_door_of_up_to_one_target_room() {
        let text = "unlock a locked door of up to one target Room you control";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse the unlock-door instruction");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::SetRoomDoorLock { op, target } => {
                assert_eq!(op, DoorLockOp::Unlock);
                assert_eq!(
                    target,
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .subtype("Room".to_string())
                            .controller(ControllerRef::You)
                    )
                );
            }
            other => panic!("Expected Effect::SetRoomDoorLock, got {other:?}"),
        }
    }

    // CR 709.5f + CR 709.5g: "lock or unlock a door of target Room you control"
    // (Keys to the House, Marina Vendrell) must lower to
    // `Effect::SetRoomDoorLock { op: LockOrUnlock }` — the longest-first op
    // ordering ensures "lock or unlock " wins over the bare "lock " prefix.
    #[test]
    fn parse_lock_or_unlock_door_of_target_room() {
        let text = "lock or unlock a door of target Room you control";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "Should parse the lock-or-unlock instruction"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::SetRoomDoorLock { op, target } => {
                assert_eq!(op, DoorLockOp::LockOrUnlock);
                assert_eq!(
                    target,
                    TargetFilter::Typed(
                        TypedFilter::default()
                            .subtype("Room".to_string())
                            .controller(ControllerRef::You)
                    )
                );
            }
            other => panic!("Expected Effect::SetRoomDoorLock, got {other:?}"),
        }
    }

    // CR 709.5g: the bare "lock a door of ..." instruction lowers to
    // `op: Lock`. No standard card uses this phrasing yet, but the building
    // block must cover the full lock/unlock axis, not just the shipped cards.
    #[test]
    fn parse_lock_door_of_target_room() {
        let text = "lock a door of target Room you control";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse the lock instruction");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::SetRoomDoorLock { op, .. } => assert_eq!(op, DoorLockOp::Lock),
            other => panic!("Expected Effect::SetRoomDoorLock, got {other:?}"),
        }
    }

    // CR 701.21a + CR 608.2k: When the trigger body is "you (may) sacrifice
    // [filter]", the actor hint piped through ParseContext.actor must default
    // the parsed `TargetFilter::Typed.controller` to ControllerRef::You so the
    // resolver restricts the prompt to the actor's permanents — sacrificing
    // requires controlling the permanent, never an opponent's.
    #[test]
    fn parse_sacrifice_defaults_controller_to_you_actor() {
        let text = "sacrifice a non-Demon creature";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(
                    tf.controller,
                    Some(ControllerRef::You),
                    "Promise of Aclazotz: controller must default to You, got {tf:?}"
                ),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_another_single_type_excludes_source() {
        let text = "sacrifice another creature";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    assert!(
                        tf.properties.contains(&FilterProp::Another),
                        "single-type sacrifice must preserve source exclusion, got {tf:?}"
                    );
                }
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_one_or_more_uses_filtered_up_to_count() {
        let text = "sacrifice one or more Treasures";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice {
                target,
                count,
                min_count,
            } => {
                assert_eq!(min_count, 1);
                match &target {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(tf.type_filters.iter().any(|type_filter| matches!(
                            type_filter,
                            crate::types::ability::TypeFilter::Subtype(subtype)
                                if subtype == "Treasure"
                        )));
                    }
                    other => panic!("expected Typed target, got {other:?}"),
                }
                match count {
                    QuantityExpr::UpTo { max } => match *max {
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        } => assert_eq!(filter, target),
                        other => panic!("expected ObjectCount max, got {other:?}"),
                    },
                    other => panic!("expected UpTo count, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_all_uses_filtered_object_count() {
        let text = "sacrifice all permanents you control";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice {
                target,
                count,
                min_count,
            } => {
                assert_eq!(min_count, 0);
                match &target {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(tf
                            .type_filters
                            .iter()
                            .any(|type_filter| matches!(type_filter, TypeFilter::Permanent)));
                    }
                    other => panic!("expected Typed target, got {other:?}"),
                }
                match count {
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    } => assert_eq!(filter, target),
                    other => panic!("expected ObjectCount count, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_all_applies_actor_default_to_count_filter() {
        let text = "sacrifice all permanents";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::ParentTargetController),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, count, .. } => {
                let TargetFilter::Typed(tf) = &target else {
                    panic!("expected Typed target, got {target:?}");
                };
                assert_eq!(tf.controller, Some(ControllerRef::ParentTargetController));
                match count {
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    } => assert_eq!(filter, target),
                    other => panic!("expected ObjectCount count, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    /// Issue #967: "sacrifice any number of creatures, each with power 1 or
    /// less" — the comma+"each" distributive linker between the collective
    /// type word and the per-object property suffix dropped the power filter
    /// entirely (the parser stopped at the comma, leaving `, each with...`
    /// unconsumed; the type-phrase fallback then produced
    /// `Effect::Sacrifice { target: TargetFilter::Typed(Creature), count: 1 }`
    /// — no power constraint, fixed count). CR 208.1: the per-object power
    /// comparison applies via the existing P/T suffix combinator.
    #[test]
    fn parse_sacrifice_any_number_creatures_comma_each_power_filter_attached() {
        use crate::types::ability::{Comparator, FilterProp, PtStat, PtValueScope};

        for text in [
            "sacrifice any number of creatures, each with power 1 or less",
            "sacrifice any number of creatures each with power 1 or less",
        ] {
            let lower = text.to_lowercase();
            let mut ctx = ParseContext {
                actor: Some(ControllerRef::You),
                ..Default::default()
            };
            let result =
                parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
            let Effect::Sacrifice { target, count, .. } = lower_targeted_action_ast(result) else {
                panic!("expected Effect::Sacrifice for {text:?}");
            };
            let TargetFilter::Typed(ref tf) = target else {
                panic!("expected Typed filter for {text:?}, got {target:?}");
            };
            assert!(
                tf.type_filters.contains(&TypeFilter::Creature),
                "missing Creature type for {text:?}",
            );
            let has_pt = tf.properties.iter().any(|p| {
                matches!(
                    p,
                    FilterProp::PtComparison {
                        stat: PtStat::Power,
                        scope: PtValueScope::Current,
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 1 },
                    }
                )
            });
            assert!(
                has_pt,
                "missing PtComparison(Power, Current, LE, 1) for {text:?}: {:?}",
                tf.properties,
            );
            match count {
                QuantityExpr::UpTo { max } => match *max {
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { filter },
                    } => assert_eq!(filter, target, "ObjectCount filter mismatch for {text:?}"),
                    other => panic!("expected ObjectCount max for {text:?}, got {other:?}"),
                },
                other => panic!("expected UpTo count for {text:?}, got {other:?}"),
            }
        }
    }

    /// Issue #967 follow-up: Angelic Aberration's "each with base power or
    /// toughness 1 or less" disjunctive variant. The same comma-each linker
    /// must allow the `power or toughness` disjunction (CR 208 + CR 208.4b)
    /// to attach correctly with the `Base` scope qualifier.
    #[test]
    fn parse_sacrifice_any_number_creatures_comma_each_base_pt_disjunction() {
        use crate::types::ability::{Comparator, FilterProp, PtStat, PtValueScope};

        let text = "sacrifice any number of creatures, each with base power or toughness 1 or less";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        let Effect::Sacrifice { target, .. } = lower_targeted_action_ast(result) else {
            panic!("expected Effect::Sacrifice");
        };
        let TargetFilter::Typed(ref tf) = target else {
            panic!("expected Typed filter, got {target:?}");
        };
        // Disjunctive `power or toughness ≤ 1` ⇒ `AnyOf {
        //   PtComparison(Power, Base, LE, 1),
        //   PtComparison(Toughness, Base, LE, 1),
        // }`.
        let has_disj = tf.properties.iter().any(|p| {
            let FilterProp::AnyOf { props } = p else {
                return false;
            };
            let want_power = FilterProp::PtComparison {
                stat: PtStat::Power,
                scope: PtValueScope::Base,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 1 },
            };
            let want_tough = FilterProp::PtComparison {
                stat: PtStat::Toughness,
                scope: PtValueScope::Base,
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 1 },
            };
            props.contains(&want_power) && props.contains(&want_tough)
        });
        assert!(
            has_disj,
            "missing AnyOf[Power Base LE 1, Toughness Base LE 1], got {:?}",
            tf.properties,
        );
    }

    // Issue #458: "sacrifice any number of <filter>" — Scapeshift class.
    // CR 107.1c: "any number" includes zero, so `min_count` is 0 (vs. 1 for
    // "one or more"). The dynamic `UpTo(ObjectCount)` ceiling is unchanged.
    #[test]
    fn parse_sacrifice_any_number_of_lands_uses_zero_min_count() {
        let text = "sacrifice any number of lands";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice {
                target,
                count,
                min_count,
            } => {
                assert_eq!(min_count, 0, "\"any number\" includes zero (CR 107.1c)");
                match count {
                    QuantityExpr::UpTo { max } => match *max {
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        } => assert_eq!(filter, target),
                        other => panic!("expected ObjectCount max, got {other:?}"),
                    },
                    other => panic!("expected UpTo count, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    // CR 701.21a: Symmetric handling — "an opponent (may) sacrifices [filter]"
    // routes ControllerRef::Opponent into the parsed Sacrifice target.
    #[test]
    fn parse_sacrifice_defaults_controller_to_opponent_actor() {
        let text = "sacrifice a creature";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::Opponent),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(tf.controller, Some(ControllerRef::Opponent)),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_preserves_choice_relative_clause() {
        let text = "sacrifice a permanent of their choice that shares a card type with it";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::Opponent),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                    assert!(tf.type_filters.iter().any(|type_filter| matches!(
                        type_filter,
                        crate::types::ability::TypeFilter::Permanent
                    )));
                    assert!(tf.properties.iter().any(|prop| matches!(
                        prop,
                        crate::types::ability::FilterProp::SharesQuality {
                            quality: crate::types::ability::SharedQuality::CardType,
                            reference: Some(reference),
                            relation: crate::types::ability::SharedQualityRelation::Shares,
                        } if matches!(reference.as_ref(), TargetFilter::ParentTarget)
                    )));
                }
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_each_opponent_may_sacrifice_preserves_choice_relative_clause() {
        let def = super::super::parse_effect_chain(
            "each opponent may sacrifice a permanent of their choice that shares a card type with it",
            AbilityKind::Spell,
        );

        assert!(def.optional, "expected optional sacrifice");
        assert_eq!(
            def.player_scope,
            Some(crate::types::ability::PlayerFilter::Opponent)
        );
        match &*def.effect {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                    assert!(tf.type_filters.iter().any(|type_filter| matches!(
                        type_filter,
                        crate::types::ability::TypeFilter::Permanent
                    )));
                    assert!(tf.properties.iter().any(|prop| matches!(
                        prop,
                        crate::types::ability::FilterProp::SharesQuality {
                            quality: crate::types::ability::SharedQuality::CardType,
                            reference: Some(reference),
                            relation: crate::types::ability::SharedQualityRelation::Shares,
                        } if matches!(reference.as_ref(), TargetFilter::ParentTarget)
                    )));
                }
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Sacrifice effect, got {other:?}"),
        }
    }

    // CR 701.21a: An explicit controller phrase in the target text must NOT be
    // overwritten by the actor default. "Sacrifice a creature an opponent
    // controls" stays Some(Opponent) even when ctx.actor = Some(You).
    #[test]
    fn parse_sacrifice_preserves_explicit_controller() {
        let text = "sacrifice a creature an opponent controls";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            actor: Some(ControllerRef::You),
            ..Default::default()
        };
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(
                    tf.controller,
                    Some(ControllerRef::Opponent),
                    "explicit controller must be preserved, got {tf:?}"
                ),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    // Regression guard: without an actor hint (ctx.actor = None), the legacy
    // `controller: None` behavior is preserved. Establishes that the default is
    // strictly opt-in — non-trigger contexts (activated abilities) still rely on
    // the existing `ability.controller` resolver path.
    #[test]
    fn parse_sacrifice_without_actor_leaves_controller_unset() {
        let text = "sacrifice a creature";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result =
            parse_targeted_action_ast(text, &lower, &mut ctx).expect("sacrifice should parse");
        match lower_targeted_action_ast(result) {
            Effect::Sacrifice { target, .. } => match target {
                TargetFilter::Typed(tf) => assert!(
                    tf.controller.is_none(),
                    "no actor hint should leave controller unset, got {tf:?}"
                ),
                other => panic!("expected Typed target, got {other:?}"),
            },
            other => panic!("expected Effect::Sacrifice, got {other:?}"),
        }
    }

    // CR 723.1: Mindslaver's declarative "You control target player during that
    // player's next turn" must route through the ControlNextTurn combinator.
    #[test]
    fn parse_mindslaver_control_next_turn() {
        let text = "You control target player during that player's next turn.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "Should parse Mindslaver's 'you control ...' declarative"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::ControlNextTurn {
                target,
                grant_extra_turn_after,
                window,
            } => {
                assert!(matches!(target, TargetFilter::Player));
                assert!(!grant_extra_turn_after);
                // CR 723.1: "next turn" → full-turn window (default).
                assert_eq!(window, ControlWindow::NextTurn);
            }
            other => panic!("Expected Effect::ControlNextTurn, got {other:?}"),
        }
    }

    // CR 723.1: "their"-possessive duration variant with an "opponent" target
    // (Construct a Cosmic Cube: "you control target opponent during their next
    // turn"). Exercises the possessive `alt()` axis and the opponent target
    // filter, both of which differ from the Mindslaver "that player's"/"player"
    // shape — confirms the combinator generalizes the possessive rather than
    // hard-coding "that player's".
    #[test]
    fn parse_control_next_turn_their_possessive_opponent_target() {
        let text = "You control target opponent during their next turn.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "Should parse the 'their next turn' / 'target opponent' variant"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::ControlNextTurn {
                target,
                grant_extra_turn_after,
                window,
            } => {
                assert_eq!(
                    target,
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                    "target opponent → ControllerRef::Opponent filter"
                );
                assert!(!grant_extra_turn_after);
                assert_eq!(window, ControlWindow::NextTurn);
            }
            other => panic!("Expected Effect::ControlNextTurn, got {other:?}"),
        }
    }

    // CR 723.2: Secret of Bloodbending's base leaf — "you control target opponent
    // during their next combat phase" — selects the NextCombatPhase window via the
    // same possessive × window combinator axis. This is the discriminating shape
    // guard for the new window arm (reverting the `alt(( .. NextCombatPhase ..))`
    // arm drops this to `Effect::Unimplemented`).
    #[test]
    fn parse_control_next_combat_phase_window() {
        let text = "You control target opponent during their next combat phase.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "should parse the next-combat-phase window"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::ControlNextTurn {
                target,
                grant_extra_turn_after,
                window,
            } => {
                assert_eq!(
                    target,
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                );
                assert!(!grant_extra_turn_after);
                assert_eq!(window, ControlWindow::NextCombatPhase);
            }
            other => panic!("Expected Effect::ControlNextTurn, got {other:?}"),
        }
    }

    // CR 608.2k + CR 723.1: the paid-override anaphora "you control that player
    // during their next turn" (Secret of Bloodbending's `AdditionalCostPaidInstead`
    // sub) must parse to ControlNextTurn — the "that player" reference resolves via
    // the shared event-context building block rather than failing target parsing.
    #[test]
    fn parse_control_that_player_anaphora() {
        let text = "You control that player during their next turn.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "'that player' anaphora should parse");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::ControlNextTurn { window, .. } => {
                assert_eq!(window, ControlWindow::NextTurn);
            }
            other => panic!("Expected Effect::ControlNextTurn, got {other:?}"),
        }
    }

    // CR 723.1: variant that grants an extra turn afterward (e.g., Emrakul-style).
    #[test]
    fn parse_control_next_turn_with_extra_turn_tail() {
        let text = "You control target player during that player's next turn. \
                    After that turn, that player takes an extra turn.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some());
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::ControlNextTurn {
                grant_extra_turn_after,
                ..
            } => assert!(grant_extra_turn_after),
            other => panic!("Expected Effect::ControlNextTurn, got {other:?}"),
        }
    }

    // Regression guard: Mindslaver Toolkit's "Target opponent gains control of"
    // still parses to GainControl (not ControlNextTurn) after the refactor.
    #[test]
    fn parse_gain_control_of_not_control_next_turn() {
        let text = "Target opponent gains control of Mindslaver Toolkit";
        let lower = text.to_lowercase();
        // Subject-strip happens upstream; imperative dispatcher sees the
        // stripped form "gain control of Mindslaver Toolkit".
        let stripped = "gain control of Mindslaver Toolkit";
        let stripped_lower = stripped.to_lowercase();
        let result =
            parse_targeted_action_ast(stripped, &stripped_lower, &mut ParseContext::default());
        assert!(result.is_some());
        let effect = lower_targeted_action_ast(result.unwrap());
        assert!(
            matches!(effect, Effect::GainControl { .. }),
            "Expected GainControl, got {effect:?}"
        );
        let _ = (text, lower);
    }

    /// CR 613.1b: Hellkite Tyrant — "gain control of all artifacts that player
    /// controls" is the untargeted MASS form, lowered to `GainControlAll`
    /// (mirrors "destroy all" → `DestroyAll`). The "target" single form must
    /// stay `GainControl`. The mass population filter still carries the type +
    /// `controller: TargetPlayer` anaphor.
    #[test]
    fn parse_gain_control_of_all_is_mass_gain_control_all() {
        use crate::types::ability::TypeFilter;
        let text = "gain control of all artifacts that player controls";
        let lower = text.to_lowercase();
        let ast = parse_targeted_action_ast(text, &lower, &mut ParseContext::default())
            .expect("mass gain control should parse");
        match lower_targeted_action_ast(ast) {
            Effect::GainControlAll { target } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.type_filters.contains(&TypeFilter::Artifact),
                        "filter must be artifacts, got {tf:?}"
                    );
                    // A "that player controls" controller anaphor is present; the
                    // concrete `ControllerRef` (TargetPlayer in the Hellkite
                    // trigger) is resolved with effect context — see the
                    // GainControlAll runtime test for the bound-target behavior.
                    assert!(
                        tf.controller.is_some(),
                        "mass filter must carry a controller anaphor, got {tf:?}"
                    );
                }
                other => panic!("expected Typed filter, got {other:?}"),
            },
            other => panic!("expected GainControlAll, got {other:?}"),
        }

        // Regression: the targeted single form stays GainControl.
        let single = "gain control of target artifact";
        let single_lower = single.to_lowercase();
        let ast =
            parse_targeted_action_ast(single, &single_lower, &mut ParseContext::default()).unwrap();
        assert!(
            matches!(lower_targeted_action_ast(ast), Effect::GainControl { .. }),
            "targeted gain control must stay GainControl"
        );
    }

    #[test]
    fn parse_mass_return_to_battlefield_after_leading_destination() {
        let text = "Return to the battlefield all artifact and creature cards in your graveyard that were put there from the battlefield this turn";
        let lower = text.to_lowercase();
        let ast = parse_targeted_action_ast(text, &lower, &mut ParseContext::default())
            .expect("mass return should parse");
        let effect = lower_targeted_action_ast(ast);
        match effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target: TargetFilter::Or { filters },
                enters_under: None,
                enter_tapped,
                enter_with_counters: _,
                face_down_profile: None,
                library_position: None,
                random_order: false,
            } => {
                assert_eq!(origin, None);
                assert_eq!(destination, Zone::Battlefield);
                assert!(!enter_tapped.is_tapped());
                assert_eq!(filters.len(), 2);
                for filter in filters {
                    let TargetFilter::Typed(typed) = filter else {
                        panic!("expected typed OR leg, got {filter:?}");
                    };
                    assert_eq!(typed.controller, Some(ControllerRef::You));
                    assert!(typed.properties.contains(&FilterProp::InZone {
                        zone: Zone::Graveyard
                    }));
                    assert!(typed.properties.iter().any(|prop| matches!(
                        prop,
                        FilterProp::ZoneChangedThisTurn {
                            from: Some(Zone::Battlefield),
                            to: Some(Zone::Graveyard),
                        }
                    )));
                }
            }
            other => panic!("Expected ChangeZoneAll return to battlefield, got {other:?}"),
        }
    }

    #[test]
    fn parse_airbend_verb() {
        let text = "Airbend target creature {2}";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse 'airbend' verb");
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::GrantCastingPermission { permission, .. } => {
                assert!(
                    matches!(
                        permission,
                        crate::types::ability::CastingPermission::ExileWithAltCost { ref cost, .. }
                            if matches!(cost, crate::types::mana::ManaCost::Cost { generic: 2, .. })
                    ),
                    "Expected ExileWithAltCost with {{2}}, got {permission:?}"
                );
            }
            other => panic!("Expected Effect::GrantCastingPermission, got {other:?}"),
        }
    }

    #[test]
    fn parse_airbend_up_to_one_other_target_creature_or_spell() {
        let text = "Airbend up to one other target creature or spell {2}";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default())
            .expect("Should parse airbend");
        let effect = lower_targeted_action_ast(result);
        match effect {
            Effect::GrantCastingPermission {
                permission,
                target: crate::types::ability::TargetFilter::Or { filters },
                ..
            } => {
                assert!(matches!(
                    permission,
                    crate::types::ability::CastingPermission::ExileWithAltCost { ref cost, .. }
                        if matches!(cost, crate::types::mana::ManaCost::Cost { generic: 2, .. })
                ));
                assert!(
                    filters.iter().any(|filter| matches!(
                        filter,
                        crate::types::ability::TargetFilter::Typed(tf)
                            if has_type(tf, crate::types::ability::TypeFilter::Creature)
                                && has_prop(tf, crate::types::ability::FilterProp::Another)
                    )),
                    "expected creature branch with Another, got {filters:?}"
                );
                assert!(
                    filters.iter().any(|filter| {
                        is_stack_spell_leg(filter)
                            && typed_leg(filter).is_some_and(|tf| {
                                has_prop(tf, crate::types::ability::FilterProp::Another)
                            })
                    }),
                    "expected spell branch with Another, got {filters:?}"
                );
            }
            other => panic!(
                "Expected GrantCastingPermission with creature-or-spell target, got {other:?}"
            ),
        }
    }

    #[test]
    fn parse_earthbend_default_pt() {
        let text = "Earthbend target land";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some());
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power, toughness, ..
            } => {
                assert_eq!(power, Some(PtValue::Fixed(0)));
                assert_eq!(toughness, Some(PtValue::Fixed(0)));
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    #[test]
    fn parse_earthbend_no_explicit_target() {
        // After reminder text stripping, "Earthbend 2." has no explicit target.
        // Should default to "target land you control" per keyword definition.
        let text = "Earthbend 2.";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "Should parse 'earthbend' without explicit target"
        );
        let effect = lower_targeted_action_ast(result.unwrap());
        match effect {
            Effect::Animate {
                power,
                toughness,
                target,
                ..
            } => {
                assert_eq!(power, Some(PtValue::Fixed(2)));
                assert_eq!(toughness, Some(PtValue::Fixed(2)));
                assert_eq!(
                    target,
                    default_earthbend_target(),
                    "Should default to land you control"
                );
            }
            other => panic!("Expected Effect::Animate, got {other:?}"),
        }
    }

    /// CR 122.1: literal-N earthbend keeps the `Fixed` count path intact.
    #[test]
    fn earthbend_count_expr_literal_n() {
        let (target, count) =
            parse_earthbend_count_expr("2", "2").expect("literal-N earthbend parses");
        assert_eq!(count, QuantityExpr::Fixed { value: 2 });
        assert_eq!(target, default_earthbend_target());
    }

    /// CR 122.1: Toph's "earthbend X, where X is the number of experience
    /// counters you have" produces a typed PlayerCounter ref, not Fixed 0.
    #[test]
    fn earthbend_count_expr_x_with_player_counter_tail() {
        let tail = "x, where x is the number of experience counters you have";
        let (_, count) =
            parse_earthbend_count_expr(tail, tail).expect("player-counter where-clause parses");
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::PlayerCounter {
                    kind: PlayerCounterKind::Experience,
                    scope: crate::types::ability::CountScope::Controller,
                },
            }
        );
    }

    /// CR 107.3a + CR 601.2b: bare "earthbend X" without a where-clause defers
    /// to the spell-cost X resolution path (Variable("X")), not Fixed 0.
    #[test]
    fn earthbend_count_expr_bare_x_falls_through_to_variable() {
        let (_, count) = parse_earthbend_count_expr("x", "x").expect("bare earthbend X parses");
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }
        );
    }

    /// CR 701.66a + CR 107.3: "earthbend X, where X is the number of creatures
    /// you control with power 4 or greater" (The Boulder, Ready to Rumble —
    /// issue #4729). The object-count where-clause must bind X to a typed
    /// `ObjectCount` (preserving the "power 4 or greater" restriction) and the
    /// earthbend target must stay "target land you control" — before the fix
    /// this class fell through to the bare-X branch, degrading X to
    /// `Variable{X}` (resolves to 0) and the target to `TargetFilter::Any`.
    #[test]
    fn earthbend_count_expr_object_count_with_power_restriction() {
        use crate::types::ability::{
            Comparator, ControllerRef, FilterProp, PtStat, TargetFilter, TypeFilter,
        };
        let tail = "x, where x is the number of creatures you control with power 4 or greater";
        let (target, count) =
            parse_earthbend_count_expr(tail, tail).expect("object-count where-clause parses");
        assert_eq!(
            target,
            default_earthbend_target(),
            "earthbend target must stay 'target land you control', not degrade to Any"
        );
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = count
        else {
            panic!("expected an ObjectCount quantity, got {count:?}");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected a Typed object-count filter, got {filter:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::PtComparison {
                    stat: PtStat::Power,
                    comparator: Comparator::GE,
                    ..
                }
            )),
            "the 'power 4 or greater' restriction must be preserved: {:?}",
            tf.properties
        );
    }

    /// CR 701.66a + CR 107.3: "earthbend X, where X is the number of Forests you
    /// control" (Rockalanche) binds X to a subtype-restricted `ObjectCount`,
    /// exercising the same general where-clause path as The Boulder for a
    /// land-subtype count rather than a P/T-restricted creature count.
    #[test]
    fn earthbend_count_expr_object_count_subtype() {
        use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter};
        let tail = "x, where x is the number of forests you control";
        let (target, count) =
            parse_earthbend_count_expr(tail, tail).expect("subtype-count where-clause parses");
        assert_eq!(target, default_earthbend_target());
        let QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        } = count
        else {
            panic!("expected an ObjectCount quantity, got {count:?}");
        };
        let TargetFilter::Typed(tf) = filter else {
            panic!("expected a Typed object-count filter, got {filter:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert_eq!(
            tf.type_filters,
            vec![TypeFilter::Subtype("Forest".to_string())]
        );
    }

    /// CR 701.66a + CR 107.3: "earthbend X, where X is the amount of mana spent
    /// to cast her" (Toph, Greatest Earthbender) binds X to a
    /// `ManaSpentToCast` ref via the event-context quantity parser — the
    /// non-object-count arm of the general where-clause handler. Before the fix
    /// this fell through to the bare-X branch (Variable{X} → 0) with an Any
    /// target.
    #[test]
    fn earthbend_count_expr_mana_spent_to_cast() {
        let tail = "x, where x is the amount of mana spent to cast her";
        let (target, count) =
            parse_earthbend_count_expr(tail, tail).expect("mana-spent where-clause parses");
        assert_eq!(target, default_earthbend_target());
        assert!(
            matches!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast { .. }
                }
            ),
            "expected a ManaSpentToCast quantity, got {count:?}"
        );
    }

    /// CR 107.3: an "earthbend X, where X is …" clause whose quantity shape is
    /// not yet recognized must be a STRICT FAILURE (`None`), not a silent
    /// acceptance. Binding X to `Variable{X}` (→ 0 for a triggered ability) would
    /// report the card as supported while applying the wrong counter count — a
    /// well-typed lie. The caller turns this `None` into an honest
    /// `Effect::unimplemented` gap so coverage counts the card as unsupported.
    /// (matthewevans review, PR #5881.) The genuine bare-`earthbend X` spell-cost
    /// path is unaffected — see `earthbend_count_expr_bare_x_falls_through_to_variable`.
    #[test]
    fn earthbend_count_expr_unrecognized_where_clause_is_strict_failure() {
        let tail = "x, where x is the number of glorbs you frobnicate";
        assert_eq!(
            parse_earthbend_count_expr(tail, tail),
            None,
            "an unrecognized where-clause quantity must fail strictly, not bind a fake Variable{{X}}"
        );
    }

    #[test]
    fn parse_lose_half_their_life_rounded_up_trace() {
        // Class-level trace: make sure "lose half their life, rounded up"
        // produces a typed DivideRounded amount at the imperative level.
        let text = "lose half their life, rounded up";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse; got {result:?}");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::DivideRounded {
                            rounding: crate::types::ability::RoundingMode::Up,
                            ..
                        }
                    ),
                    "Expected DivideRounded(Up), got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_half_non_demon_permanents_lifts_count_filter() {
        let text = "sacrifice half the non-Demon permanents you control, rounded up";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Sacrifice { target, count, .. }) => {
                assert!(matches!(
                    count,
                    QuantityExpr::DivideRounded {
                        rounding: crate::types::ability::RoundingMode::Up,
                        ..
                    }
                ));
                let TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::You),
                    ..
                }) = target
                else {
                    panic!("expected You-controlled typed target");
                };
                assert_eq!(
                    type_filters,
                    vec![
                        crate::types::ability::TypeFilter::Permanent,
                        crate::types::ability::TypeFilter::Non(Box::new(
                            crate::types::ability::TypeFilter::Subtype("Demon".to_string())
                        )),
                    ]
                );
            }
            other => panic!("Expected Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_half_non_god_creatures_ignores_choice_rounding_suffix() {
        let text =
            "sacrifice half the non-God creatures they control of their choice, rounded down";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result = parse_targeted_action_ast(text, &lower, &mut ctx);
        match result {
            Some(TargetedImperativeAst::Sacrifice { target, count, .. }) => {
                assert!(matches!(
                    count,
                    QuantityExpr::DivideRounded {
                        rounding: crate::types::ability::RoundingMode::Down,
                        ..
                    }
                ));
                assert!(ctx.diagnostics.is_empty(), "{:?}", ctx.diagnostics);
                let TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::ScopedPlayer),
                    ..
                }) = target
                else {
                    panic!("expected ScopedPlayer-controlled typed target");
                };
                assert_eq!(
                    type_filters,
                    vec![
                        crate::types::ability::TypeFilter::Creature,
                        crate::types::ability::TypeFilter::Non(Box::new(
                            crate::types::ability::TypeFilter::Subtype("God".to_string())
                        )),
                    ]
                );
            }
            other => panic!("Expected Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn parse_sacrifice_half_creatures_choice_rounding_up_suffix_is_applied() {
        let text = "sacrifice half the creatures they control of their choice, rounded up";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result = parse_targeted_action_ast(text, &lower, &mut ctx);
        match result {
            Some(TargetedImperativeAst::Sacrifice { target, count, .. }) => {
                assert!(matches!(
                    count,
                    QuantityExpr::DivideRounded {
                        rounding: crate::types::ability::RoundingMode::Up,
                        ..
                    }
                ));
                assert!(ctx.diagnostics.is_empty(), "{:?}", ctx.diagnostics);
                let TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller: Some(ControllerRef::ScopedPlayer),
                    ..
                }) = target
                else {
                    panic!("expected ScopedPlayer-controlled typed target");
                };
                assert_eq!(
                    type_filters,
                    vec![crate::types::ability::TypeFilter::Creature]
                );
            }
            other => panic!("Expected Sacrifice, got {other:?}"),
        }
    }

    /// CR 608.2c — Yuriko, the Tiger's Shadow / Dark Confidant class. The bare
    /// anaphoric prefix "that card" in `"loses life equal to that card's mana
    /// value"` is an instruction-order referent: it points at the object
    /// introduced by an earlier `RevealTop` / `Mill` / `ChangeZone`
    /// instruction in the same ability. `classify_possessive_referent`
    /// therefore emits `ObjectScope::Demonstrative` (the noun-phrase
    /// back-reference, distinct from the pronoun "its"), whose runtime resolver
    /// reads `effect_context_object` first (the revealed card) and only then
    /// falls back to the trigger source and the cost-paid object. The dedicated
    /// variant keeps the subject-injection rewrite from rebinding this fixed
    /// antecedent.
    #[test]
    fn parse_lose_life_equal_to_mana_value() {
        let text = "loses life equal to that card's mana value";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'loses life equal to'");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectManaValue {
                                scope: crate::types::ability::ObjectScope::Demonstrative
                            }
                        }
                    ),
                    "Expected ObjectManaValue {{ Demonstrative }}, got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_lose_life_equal_to_life_gained_this_turn() {
        let text = "loses life equal to the amount of life you gained this turn";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(
            result.is_some(),
            "Should parse third-person life loss whose amount references gained life"
        );
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => {
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::LifeGainedThisTurn {
                                player: PlayerScope::Controller
                            }
                        }
                    ),
                    "Expected LifeGainedThisTurn {{ Controller }}, got {amount:?}"
                );
            }
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    /// CR 115.1 + CR 119.3: Astarion (Feed mode) — "loses life equal to the
    /// amount of life they lost this turn." The third-person "they" anaphor in a
    /// targeted life-change refers to the effect's player target, so it maps to
    /// `LifeLostThisTurn { Target }` (NOT `Controller` like the "you" form). The
    /// maintainer's shared `parse_life_lost_ref` "amount of … they lost" tag is
    /// unreachable behind its own prefix strip, so this targeted-context
    /// recognizer is what resurrects the Feed mode.
    #[test]
    fn parse_lose_life_equal_to_life_they_lost_this_turn_is_target_scoped() {
        for text in [
            "loses life equal to the amount of life they lost this turn",
            "loses life equal to the amount of life that player lost this turn",
        ] {
            let lower = text.to_lowercase();
            let result = parse_numeric_imperative_ast(text, &lower)
                .unwrap_or_else(|| panic!("should parse {text:?}"));
            match result {
                NumericImperativeAst::LoseLife { amount } => assert_eq!(
                    amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeLostThisTurn {
                            player: PlayerScope::Target,
                        },
                    },
                    "{text:?} must be Target-scoped",
                ),
                other => panic!("Expected LoseLife, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_lose_life_two_times_x() {
        let text = "lose two times X life";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse multiplied X life loss");
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => match amount {
                QuantityExpr::Multiply { factor, inner } => {
                    assert_eq!(factor, 2);
                    assert!(matches!(
                        *inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Variable { .. }
                        }
                    ));
                }
                other => panic!("Expected Multiply, got {other:?}"),
            },
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_gain_life_equal_to_power() {
        let text = "gain life equal to its power";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(result.is_some(), "Should parse 'gain life equal to'");
        match result.unwrap() {
            NumericImperativeAst::GainLife { amount } => {
                // CR 608.2k: bare anaphoric "its power" parses to the parse-time
                // marker `Anaphoric`; the parser remaps it to a concrete scope
                // where context allows, else it survives to runtime (resolving
                // identically to `CostPaidObject`).
                assert!(
                    matches!(
                        amount,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Power {
                                scope: crate::types::ability::ObjectScope::Anaphoric
                            }
                        }
                    ),
                    "Expected Power {{ CostPaidObject }}, got {amount:?}"
                );
            }
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_gains_life_equal_to_power() {
        let text = "gains life equal to its power";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(
            result.is_some(),
            "Should parse stripped third-person gain-life predicates"
        );
        match result.unwrap() {
            NumericImperativeAst::GainLife { amount } => assert!(
                matches!(
                    amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: crate::types::ability::ObjectScope::Anaphoric
                        }
                    }
                ),
                "Expected Power {{ Anaphoric }}, got {amount:?}"
            ),
            other => panic!("Expected GainLife, got {other:?}"),
        }
    }

    /// CR 119.3 + CR 208.1: "loses life equal to its power plus its toughness" —
    /// Phthisis class. Both operands use Anaphoric scope so the enclosing
    /// clause's subject-injection applies identically to the individual
    /// "its power" / "its toughness" single-value forms. The destroy effect
    /// that precedes this clause sets `effect_context_object` to the destroyed
    /// creature's LKI, providing the correct runtime referent.
    #[test]
    fn parse_lose_life_equal_to_power_plus_toughness() {
        let text = "lose life equal to its power plus its toughness";
        let lower = text.to_lowercase();
        let result = parse_numeric_imperative_ast(text, &lower);
        assert!(
            result.is_some(),
            "Should parse 'lose life equal to its power plus its toughness'"
        );
        match result.unwrap() {
            NumericImperativeAst::LoseLife { amount } => match amount {
                QuantityExpr::Sum { exprs } => {
                    assert_eq!(exprs.len(), 2, "Sum should have two operands");
                    assert!(
                        matches!(
                            exprs[0],
                            QuantityExpr::Ref {
                                qty: QuantityRef::Power {
                                    scope: crate::types::ability::ObjectScope::Anaphoric
                                }
                            }
                        ),
                        "First operand should be Power(Anaphoric), got {:?}",
                        exprs[0]
                    );
                    assert!(
                        matches!(
                            exprs[1],
                            QuantityExpr::Ref {
                                qty: QuantityRef::Toughness {
                                    scope: crate::types::ability::ObjectScope::Anaphoric
                                }
                            }
                        ),
                        "Second operand should be Toughness(Anaphoric), got {:?}",
                        exprs[1]
                    );
                }
                other => panic!("Expected Sum, got {other:?}"),
            },
            other => panic!("Expected LoseLife, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_zombies_2() {
        let result = try_parse_amass("amass Zombies 2", "amass zombies 2");
        assert!(result.is_some(), "Should parse 'amass Zombies 2'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Zombie");
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_orcs_3() {
        let result = try_parse_amass("amass Orcs 3", "amass orcs 3");
        assert!(result.is_some(), "Should parse 'amass Orcs 3'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Orc");
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_amass_zombies_x() {
        let result = try_parse_amass("amass Zombies X", "amass zombies x");
        assert!(result.is_some(), "Should parse 'amass Zombies X'");
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Zombie");
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    /// Issue #720: "amass Orcs X, where X is that spell's mana value" (Saruman,
    /// the White Hand) must bind X to the triggering spell's mana value, not
    /// fall through to a bare `Variable` ref — which always resolves to 0
    /// outside an actually-paid-X cost, silently amassing nothing.
    #[test]
    fn parse_amass_orcs_x_where_x_is_spell_mana_value() {
        let result = try_parse_amass(
            "amass Orcs X, where X is that spell's mana value",
            "amass orcs x, where x is that spell's mana value",
        );
        assert!(
            result.is_some(),
            "Should parse 'amass Orcs X, where X is ...'"
        );
        match result.unwrap() {
            Effect::Amass { subtype, count } => {
                assert_eq!(subtype, "Orc");
                assert_eq!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: crate::types::ability::ObjectScope::EventSource,
                        }
                    }
                );
            }
            other => panic!("Expected Amass, got {other:?}"),
        }
    }

    #[test]
    fn parse_monstrosity_4() {
        let result = try_parse_monstrosity("monstrosity 4.");
        assert!(result.is_some(), "Should parse 'monstrosity 4.'");
        match result.unwrap() {
            Effect::Monstrosity { count } => {
                assert!(matches!(count, QuantityExpr::Fixed { value: 4 }));
            }
            other => panic!("Expected Monstrosity, got {other:?}"),
        }
    }

    #[test]
    fn parse_monstrosity_x() {
        let result = try_parse_monstrosity("monstrosity x.");
        assert!(result.is_some(), "Should parse 'monstrosity X.'");
        match result.unwrap() {
            Effect::Monstrosity { count } => {
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("Expected Monstrosity, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_a_poison_counter() {
        let result = try_parse_player_counter("get a poison counter");
        assert!(result.is_some(), "Should parse 'get a poison counter'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Poison);
                assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_gets_two_experience_counters() {
        let result = try_parse_player_counter("gets two experience counters");
        assert!(
            result.is_some(),
            "Should parse 'gets two experience counters'"
        );
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Experience);
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_three_rad_counters() {
        let result = try_parse_player_counter("get three rad counters");
        assert!(result.is_some(), "Should parse 'get three rad counters'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Rad);
                assert!(matches!(count, QuantityExpr::Fixed { value: 3 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_get_an_experience_counter() {
        let result = try_parse_player_counter("get an experience counter");
        assert!(result.is_some(), "Should parse 'get an experience counter'");
        match result.unwrap() {
            ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            } => {
                assert_eq!(counter_kind, PlayerCounterKind::Experience);
                assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    #[test]
    fn parse_player_counter_rejects_plus1_counter() {
        // "+1/+1 counter" is an object counter, not a player counter
        let result = try_parse_player_counter("gets a +1/+1 counter");
        assert!(
            result.is_none(),
            "Should NOT parse '+1/+1 counter' as player counter"
        );
    }

    #[test]
    fn parse_player_counter_rejects_unknown_type() {
        // "charge counter" is an object counter, not a known player counter
        let result = try_parse_player_counter("get a charge counter");
        assert!(
            result.is_none(),
            "Should NOT parse unknown counter type as player counter"
        );
    }

    /// CR 608.2h: "get that many <kind> counters" binds the triggering event's
    /// amount (Etali, Primal Sickness: combat damage → that many poison
    /// counters). Revert-failing: without the `parse_that_much_or_many` arm the
    /// count matches neither `parse_article` nor `parse_number`, so
    /// `try_parse_player_counter` returns None and the effect lowers to
    /// `Effect::Unimplemented`.
    #[test]
    fn parse_player_counter_that_many_binds_event_amount() {
        match try_parse_player_counter("get that many poison counters") {
            Some(ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            }) => {
                assert_eq!(counter_kind, PlayerCounterKind::Poison);
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    }
                ));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    /// Class coverage: the "that many" amount arm generalizes across the whole
    /// player-counter kind set (experience, not just poison).
    #[test]
    fn parse_player_counter_that_many_experience() {
        match try_parse_player_counter("get that many experience counters") {
            Some(ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            }) => {
                assert_eq!(counter_kind, PlayerCounterKind::Experience);
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    }
                ));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    /// Sibling reach-guard: the new that-many arm must not swallow the fixed
    /// article/number forms — "get a poison counter" still binds Fixed 1 and
    /// "gets two rad counters" still binds Fixed 2 (proves the fixed arms are
    /// reached, not short-circuited).
    #[test]
    fn parse_player_counter_fixed_arms_unaffected() {
        match try_parse_player_counter("get a poison counter") {
            Some(ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            }) => {
                assert_eq!(counter_kind, PlayerCounterKind::Poison);
                assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
        match try_parse_player_counter("gets two rad counters") {
            Some(ImperativeFamilyAst::GivePlayerCounter {
                counter_kind,
                count,
            }) => {
                assert_eq!(counter_kind, PlayerCounterKind::Rad);
                assert!(matches!(count, QuantityExpr::Fixed { value: 2 }));
            }
            other => panic!("Expected GivePlayerCounter, got {other:?}"),
        }
    }

    /// Hostile: "+1/+1 counter" is an object counter, not a player counter —
    /// the that-many arm must not misroute it (the `+`/`-` guard still rejects).
    #[test]
    fn parse_player_counter_that_many_rejects_object_counter() {
        assert!(
            try_parse_player_counter("gets a +1/+1 counter").is_none(),
            "Object counter must not parse as a player counter"
        );
    }

    /// CR 107.17: Each `{TK}` glyph is one ticket counter. "get {tk}{tk}" → 2.
    /// Building-block test: exercises the symbol-counting branch across counts.
    #[test]
    fn parse_player_counter_ticket_symbols() {
        for (text, expected) in [
            ("get {tk}", 1),
            ("get {tk}{tk}", 2),
            ("get {tk}{tk}{tk}", 3),
            ("gets {tk}{tk}.", 2),
        ] {
            match try_parse_player_counter(text) {
                Some(ImperativeFamilyAst::GivePlayerCounter {
                    counter_kind,
                    count,
                }) => {
                    assert_eq!(
                        counter_kind,
                        PlayerCounterKind::Ticket,
                        "{text:?} should be a Ticket counter"
                    );
                    assert!(
                        matches!(count, QuantityExpr::Fixed { value } if value == expected),
                        "{text:?} should give {expected} ticket counters, got {count:?}"
                    );
                }
                other => panic!("Expected GivePlayerCounter for {text:?}, got {other:?}"),
            }
        }
    }

    /// The symbol branch must not swallow a `{TK}` activation cost or a larger
    /// clause: only a bare "get {TK}…" with an optional terminator may match.
    #[test]
    fn parse_player_counter_ticket_symbols_reject_trailing_clause() {
        assert!(
            try_parse_player_counter("get {tk} for each creature you control").is_none(),
            "Trailing 'for each ...' clause must not parse as a bare ticket grant"
        );
        // No leading ticket symbol — falls through to the word form (and fails
        // there, since there is no counter noun), so the symbol branch is inert.
        assert!(
            count_ticket_symbols("a poison counter").is_none(),
            "Non-symbol input must not be counted as ticket symbols"
        );
    }

    /// End-to-end (CR 107.17): real Unfinity oracle text that was previously
    /// Unimplemented now parses to a Ticket player counter. Representative cards:
    /// Blorbian Buddy ("you get {TK}"), Stiltstrider/Prize Wall ("you get
    /// {TK}{TK}"), Ticketomaton ("you get {TK}{TK}{TK}").
    #[test]
    fn parse_effect_you_get_ticket_symbols_end_to_end() {
        for (oracle, expected) in [
            ("You get {TK}", 1),
            ("You get {TK}{TK}", 2),
            ("You get {TK}{TK}{TK}", 3),
        ] {
            match super::super::parse_effect(oracle) {
                Effect::GivePlayerCounter {
                    counter_kind,
                    count,
                    target,
                } => {
                    assert_eq!(counter_kind, PlayerCounterKind::Ticket);
                    assert_eq!(target, TargetFilter::Controller);
                    assert!(
                        matches!(count, QuantityExpr::Fixed { value } if value == expected),
                        "{oracle:?} should give {expected} tickets, got {count:?}"
                    );
                }
                other => panic!(
                    "{oracle:?} should parse to GivePlayerCounter, not {other:?} (regression: was Unimplemented)"
                ),
            }
        }
    }

    #[test]
    fn parse_turn_target_face_up_resolving_effect() {
        // CR 708.7 + CR 708.8: "turn <target> face up" resolving effect (not the
        // morph special action). Three target axes the cluster needs: a controlled
        // descriptor (Bustle), a targeted face-down creature (Expose the
        // Culprit), and an anaphoric "it" (Hauntwoods Shrieker reveal follow-up).
        for (text, expect_face_down) in [
            ("turn a creature you control face up", false),
            ("turn target face-down creature face up", true),
            ("turn it face up", false),
        ] {
            let lower = text.to_lowercase();
            let ast = parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
                .unwrap_or_else(|| panic!("{text:?} should parse to a turn-face-up effect"));
            match lower_imperative_family_effect(ast) {
                Effect::TurnFaceUp { target } => {
                    if expect_face_down {
                        // The face-down property must ride the parsed target so
                        // only face-down creatures are legal targets.
                        let has_face_down = matches!(
                            &target,
                            TargetFilter::Typed(t)
                                if t.properties
                                    .iter()
                                    .any(|p| matches!(p, FilterProp::FaceDown))
                        );
                        assert!(
                            has_face_down,
                            "{text:?} target must carry FaceDown, got {target:?}"
                        );
                    }
                }
                other => panic!("{text:?} expected TurnFaceUp, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_turn_this_permanent_face_up_is_not_a_resolving_effect() {
        // CR 116.2b / CR 702.37e: "turn this permanent face up" is the morph
        // special action (controller's own action), not a resolving TurnFaceUp
        // effect. The general resolving-effect arm must NOT swallow it.
        for text in ["turn this permanent face up", "turn this creature face up"] {
            let lower = text.to_lowercase();
            let ast = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
            let is_turn_face_up = ast
                .map(lower_imperative_family_effect)
                .is_some_and(|e| matches!(e, Effect::TurnFaceUp { .. }));
            assert!(
                !is_turn_face_up,
                "{text:?} (special action) must not parse as a TurnFaceUp resolving effect"
            );
        }
    }

    #[test]
    fn parse_reveal_target_object_emits_reveal_effect() {
        // CR 701.20: "reveal target face-down permanent" (Hauntwoods Shrieker)
        // reveals a targeted object, distinct from hand reveals and back-refs.
        let text = "reveal target face-down permanent";
        let lower = text.to_lowercase();
        let ast = parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
            .expect("reveal target object should parse");
        match lower_imperative_family_effect(ast) {
            Effect::Reveal { target } => {
                assert!(
                    matches!(
                        &target,
                        TargetFilter::Typed(t)
                            if t.properties.iter().any(|p| matches!(p, FilterProp::FaceDown))
                    ),
                    "reveal target must carry FaceDown, got {target:?}"
                );
            }
            other => panic!("expected Reveal, got {other:?}"),
        }
    }

    #[test]
    fn parse_additional_phase_after_this_main_phase_anchors_to_main() {
        let text = "After this main phase, there are two additional combat phases.";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(
            result.is_some(),
            "Should parse main-phase-anchored additional combats"
        );
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalPhase {
                    phase: Phase::BeginCombat,
                    after: Phase::PreCombatMain,
                    ref followed_by,
                    count: QuantityExpr::Fixed { value: 2 },
                    ..
                } if followed_by.is_empty()
            ),
            "Expected AdditionalPhase anchored to main phase with count 2, got {effect:?}"
        );
    }

    #[test]
    fn parse_additional_phase_phase() {
        let text = "there is an additional combat phase after this phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse additional combat phase");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalPhase {
                    phase: Phase::BeginCombat,
                    after: Phase::EndCombat,
                    ref followed_by,
                    ..
                } if followed_by.is_empty()
            ),
            "Expected AdditionalPhase without main phase, got {effect:?}"
        );
    }

    #[test]
    fn parse_pay_any_amount_of_mana_as_variable_mana_cost() {
        let text = "pay any amount of mana";
        let lower = text.to_lowercase();
        let Some(CostResourceImperativeAst::Pay {
            cost: AbilityCost::Mana { cost },
        }) = parse_cost_resource_ast(text, &lower, &mut ParseContext::default())
        else {
            panic!("expected variable mana PayCost");
        };
        let crate::types::mana::ManaCost::Cost { shards, generic } = cost else {
            panic!("expected concrete mana cost");
        };
        assert_eq!(generic, 0);
        assert!(matches!(
            shards.as_slice(),
            [crate::types::mana::ManaCostShard::X]
        ));
    }

    #[test]
    fn parse_pay_half_your_life_rounded_up() {
        // CR 118.8: delegate the life-fraction phrase to the shared quantity
        // parser (DivideRounded over the controller's life total).
        let text = "pay half your life, rounded up";
        let lower = text.to_lowercase();
        let Some(CostResourceImperativeAst::Pay {
            cost: AbilityCost::PayLife { amount },
        }) = parse_cost_resource_ast(text, &lower, &mut ParseContext::default())
        else {
            panic!("expected PayLife cost for {text:?}");
        };
        assert!(
            matches!(
                amount,
                QuantityExpr::DivideRounded {
                    divisor: 2,
                    rounding: crate::types::ability::RoundingMode::Up,
                    ..
                }
            ),
            "expected half-life DivideRounded, got {amount:?}"
        );
    }

    #[test]
    fn parse_pay_any_amount_of_life_as_variable_life_cost() {
        // CR 119.4 + CR 107.1c: "pay any amount of life" → PayLife with a
        // Variable("X") amount; the chosen amount binds X for the downstream
        // "draw/look at that many" step (Necrodominance, Plunge into Darkness).
        for text in [
            "pay any amount of life",
            "pay any amount of life, then look at that many cards",
        ] {
            let lower = text.to_lowercase();
            let Some(CostResourceImperativeAst::Pay {
                cost: AbilityCost::PayLife { amount },
            }) = parse_cost_resource_ast(text, &lower, &mut ParseContext::default())
            else {
                panic!("expected PayLife variable cost for {text:?}");
            };
            assert!(
                matches!(
                    amount,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { ref name },
                    } if name == "X"
                ),
                "expected Variable(X) life amount for {text:?}, got {amount:?}"
            );
        }
    }

    #[test]
    fn parse_pay_does_not_false_match_lifelink_or_lifeless() {
        // Word-boundary guard: "any amount of life" must be a complete token.
        for text in ["pay any amount of lifelink", "pay any amount of lifeforce"] {
            let lower = text.to_lowercase();
            let res = parse_cost_resource_ast(text, &lower, &mut ParseContext::default());
            assert!(
                !matches!(
                    res,
                    Some(CostResourceImperativeAst::Pay {
                        cost: AbilityCost::PayLife { .. }
                    })
                ),
                "{text:?} must not parse as a life payment"
            );
        }
    }

    #[test]
    fn parse_additional_phase_with_main_phase() {
        let text = "there is an additional combat phase followed by an additional main phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse additional combat + main");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalPhase {
                    phase: Phase::BeginCombat,
                    after: Phase::EndCombat,
                    ref followed_by,
                    ..
                } if followed_by == &vec![Phase::PostCombatMain]
            ),
            "Expected AdditionalPhase with main phase, got {effect:?}"
        );
    }

    #[test]
    fn parse_additional_upkeep_step() {
        let text = "get an additional upkeep step after this step";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse additional upkeep step");
        let effect = lower_imperative_family_effect(result.unwrap());
        assert!(
            matches!(
                effect,
                Effect::AdditionalPhase {
                    phase: Phase::Upkeep,
                    after: Phase::Upkeep,
                    ref followed_by,
                    ..
                } if followed_by.is_empty()
            ),
            "Expected AdditionalPhase for upkeep step, got {effect:?}"
        );
    }

    #[test]
    fn parse_after_this_phase_additional_phase() {
        let text = "after this phase, there is an additional combat phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(result.is_some(), "Should parse 'after this phase' variant");
    }

    /// CR 500.8 + CR 510.2: Obeka, Splitter of Seconds — "you get that many
    /// additional upkeep steps after this phase" must thread the triggering
    /// combat damage amount through `EventContextAmount`, not collapse it to
    /// the singular default.
    #[test]
    fn parse_obeka_that_many_additional_upkeep_steps_binds_event_amount() {
        let text = "you get that many additional upkeep steps after this phase";
        let lower = text.to_lowercase();
        let result = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        let effect = lower_imperative_family_effect(
            result.expect("Obeka consequent should parse as AdditionalPhase"),
        );
        match effect {
            Effect::AdditionalPhase {
                phase: Phase::Upkeep,
                after: Phase::Upkeep,
                count:
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                ..
            } => {}
            other => {
                panic!("expected AdditionalPhase with EventContextAmount count, got {other:?}")
            }
        }
    }

    #[test]
    fn parse_fixed_count_additional_upkeep_steps_binds_literal_count() {
        let text = "you get two additional upkeep steps after this phase";
        let lower = text.to_lowercase();
        let effect = lower_imperative_family_effect(
            parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
                .expect("fixed-count additional upkeep should parse"),
        );
        match effect {
            Effect::AdditionalPhase {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            } => {}
            other => panic!("expected count=Fixed(2) for fixed-count form, got {other:?}"),
        }
    }

    /// Singular "an additional upkeep step" must keep the legacy Fixed(1)
    /// semantics so Paradox Haze and similar cards still push exactly one
    /// extra step.
    #[test]
    fn parse_additional_upkeep_step_defaults_to_count_one() {
        let text = "get an additional upkeep step after this step";
        let lower = text.to_lowercase();
        let effect = lower_imperative_family_effect(
            parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
                .expect("should parse singular additional upkeep"),
        );
        match effect {
            Effect::AdditionalPhase {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } => {}
            other => panic!("expected count=Fixed(1) for singular form, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_your_hand() {
        let text = "discard your hand";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::Controller
                            }
                        }
                    ),
                    "Expected HandSize ref, got {count:?}"
                );
            }
            other => panic!("Expected Discard with HandSize, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_their_hand() {
        // CR 109.5 + CR 115.10: "their" in a discard imperative refers to
        // the subject, not the printed ability controller. The local
        // imperative parser represents that as `Target`; an outer
        // each-player scope rewrites it to `ScopedPlayer`.
        let text = "discard their hand";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::Target
                            }
                        }
                    ),
                    "Expected HandSize ref scoped to Target, got {count:?}"
                );
            }
            other => panic!("Expected Discard with HandSize, got {other:?}"),
        }
    }

    /// CR 701.9a + CR 107.1b: Monomania -- "chooses a card in their hand and
    /// discards the rest" routes through the "choose" verb to the existing
    /// `Effect::Discard` with a `hand size - 1` (floored at 0) count. The
    /// possessive drives the `HandSize` scope; "their" -> `Target`.
    #[test]
    fn parse_choose_discard_rest_of_hand_their() {
        let text = "choose a card in their hand and discards the rest";
        match parse_choose_discard_rest_of_hand(text, text) {
            Some(TargetedImperativeAst::Discard {
                count,
                random,
                up_to,
                unless_filter,
                filter,
            }) => {
                assert!(!random && !up_to);
                assert!(unless_filter.is_none() && filter.is_none());
                match count {
                    QuantityExpr::ClampMin { inner, minimum } => {
                        assert_eq!(minimum, 0);
                        match *inner {
                            QuantityExpr::Offset { inner, offset } => {
                                assert_eq!(offset, -1);
                                assert!(
                                    matches!(
                                        *inner,
                                        QuantityExpr::Ref {
                                            qty: QuantityRef::HandSize {
                                                player: PlayerScope::Target
                                            }
                                        }
                                    ),
                                    "expected HandSize(Target), got {inner:?}"
                                );
                            }
                            other => panic!("expected Offset(-1), got {other:?}"),
                        }
                    }
                    other => panic!("expected ClampMin, got {other:?}"),
                }
            }
            other => panic!("expected Discard, got {other:?}"),
        }
    }

    /// The de-inflected leading "choose" verb and the "your hand" possessive
    /// (Controller scope) are both accepted.
    #[test]
    fn parse_choose_discard_rest_of_hand_your() {
        let text = "choose a card in your hand and discard the rest";
        match parse_choose_discard_rest_of_hand(text, text) {
            Some(TargetedImperativeAst::Discard { count, .. }) => match count {
                QuantityExpr::ClampMin { inner, .. } => match *inner {
                    QuantityExpr::Offset { inner, .. } => assert!(matches!(
                        *inner,
                        QuantityExpr::Ref {
                            qty: QuantityRef::HandSize {
                                player: PlayerScope::Controller
                            }
                        }
                    )),
                    other => panic!("expected Offset, got {other:?}"),
                },
                other => panic!("expected ClampMin, got {other:?}"),
            },
            other => panic!("expected Discard, got {other:?}"),
        }
    }

    /// Whole-clause anchoring guard: near-miss clauses that share the "choose"
    /// / "the rest" tokens must NOT be captured by the Monomania recognizer.
    /// A bare "choose a card in their hand" (no disposal), a "discard it"
    /// back-reference, a plain targeting clause, a "two cards" cardinality
    /// mismatch, and the Balance-family "a number of ... they control ...
    /// discards the rest" equalization clause all return `None`.
    #[test]
    fn parse_choose_discard_rest_of_hand_rejects_near_misses() {
        for s in [
            "choose a card in their hand",
            "choose a card in their hand and discard it",
            "choose a creature",
            "choose two cards in their hand and discard the rest",
            "choose a number of lands they control equal to the number of lands controlled by the player who controls the fewest, then discards the rest",
        ] {
            assert!(
                parse_choose_discard_rest_of_hand(s, s).is_none(),
                "near-miss must not match: {s:?}"
            );
        }
    }

    /// Binding Negotiation (std long-tail): "If you do, they discard it" — the
    /// "discard it" back-reference targets the card chosen by the parent
    /// RevealHand effect. It must lower to `DiscardCard { target: ParentTarget }`
    /// alongside the existing "that card" / "those cards" anaphors, not
    /// `Effect::Unimplemented`. Revert-discriminating: removing the "it" arm
    /// makes `parse_targeted_action_ast` miss the anaphor.
    /// CR 701.9a (discard); CR 608.2c (anaphoric back-reference).
    #[test]
    fn parse_discard_it_anaphor() {
        let text = "discard it";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        assert!(
            matches!(
                result,
                Some(TargetedImperativeAst::DiscardCard {
                    target: TargetFilter::ParentTarget
                })
            ),
            "expected DiscardCard(ParentTarget) for 'discard it', got {result:?}"
        );
    }

    #[test]
    fn parse_discard_a_card_regression() {
        let text = "discard a card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 1 }),
                    "Expected Fixed(1), got {count:?}"
                );
            }
            other => panic!("Expected Discard with Fixed(1), got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_two_cards_regression() {
        let text = "discard two cards";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard { count, .. }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
            }
            other => panic!("Expected Discard with Fixed(2), got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_unless_creature() {
        let text = "discard two cards unless you discard a creature card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard {
                count,
                unless_filter,
                ..
            }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
                assert!(
                    unless_filter.is_some(),
                    "Expected unless_filter for creature"
                );
            }
            other => panic!("Expected Discard with unless_filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_discard_unless_pirate() {
        let text = "discard two cards unless you discard a Pirate card";
        let lower = text.to_lowercase();
        let result = parse_targeted_action_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(TargetedImperativeAst::Discard {
                count,
                unless_filter,
                ..
            }) => {
                assert!(
                    matches!(count, QuantityExpr::Fixed { value: 2 }),
                    "Expected Fixed(2), got {count:?}"
                );
                assert!(
                    unless_filter.is_some(),
                    "Expected unless_filter for Pirate subtype"
                );
            }
            other => panic!("Expected Discard with unless_filter, got {other:?}"),
        }
    }

    #[test]
    fn parse_support_on_spell() {
        // CR 701.41a: Support N on an instant/sorcery — "up to N target creatures"
        let text = "support 2";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default(); // No subject = spell context
        let ast = parse_imperative_family_ast(text, &lower, &mut ctx);
        assert!(
            matches!(
                &ast,
                Some(ImperativeFamilyAst::Support {
                    count: 2,
                    is_other: false
                })
            ),
            "Expected Support {{ count: 2, is_other: false }}, got {ast:?}"
        );
        let clause = lower_imperative_family_ast(ast.unwrap());
        assert!(
            matches!(
                &clause.effect,
                Effect::PutCounter { counter_type, count: QuantityExpr::Fixed { value: 1 }, .. }
                if *counter_type == crate::types::counter::CounterType::Plus1Plus1
            ),
            "Expected PutCounter P1P1, got {:?}",
            clause.effect
        );
        assert_eq!(clause.multi_target, Some(MultiTargetSpec::fixed(0, 2)));
        // Spell support should NOT have Another property
        if let Effect::PutCounter {
            target: TargetFilter::Typed(tf),
            ..
        } = &clause.effect
        {
            assert!(
                !tf.properties
                    .contains(&crate::types::ability::FilterProp::Another),
                "Spell support should not use 'other'"
            );
        }
    }

    #[test]
    fn parse_support_on_permanent() {
        // CR 701.41a: Support N on a permanent — "up to N other target creatures"
        let text = "support 3";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            ..Default::default()
        };
        let ast = parse_imperative_family_ast(text, &lower, &mut ctx);
        assert!(
            matches!(
                &ast,
                Some(ImperativeFamilyAst::Support {
                    count: 3,
                    is_other: true
                })
            ),
            "Expected Support {{ count: 3, is_other: true }}, got {ast:?}"
        );
        let clause = lower_imperative_family_ast(ast.unwrap());
        // Permanent support should have Another property
        if let Effect::PutCounter {
            target: TargetFilter::Typed(tf),
            ..
        } = &clause.effect
        {
            assert!(
                tf.properties
                    .contains(&crate::types::ability::FilterProp::Another),
                "Permanent support should use 'other'"
            );
        }
        assert_eq!(clause.multi_target, Some(MultiTargetSpec::fixed(0, 3)));
    }

    /// CR 115.1d + CR 708.2a: "turn any number of target … face down" (Illithid
    /// Harvester) is a plural-subject turn-face-down. The target-count quantifier
    /// must survive lowering as `ParsedEffectClause.multi_target` (unlimited,
    /// min 0) so the cast surfaces every chosen target rather than collapsing to
    /// one slot — the bare `Effect::TurnFaceDown` cannot carry the cardinality.
    /// The single-subject form (Cyber Conversion) carries `None`, pinning the
    /// other end of the axis.
    #[test]
    fn turn_face_down_any_number_of_target_carries_multi_target() {
        let text = "turn any number of target tapped nontoken creatures face down";
        let lower = text.to_lowercase();
        let ast = parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
            .expect("plural turn-face-down should parse");
        assert!(
            matches!(
                &ast,
                ImperativeFamilyAst::TurnFaceDown {
                    multi_target: Some(_),
                    ..
                }
            ),
            "expected TurnFaceDown with a multi_target, got {ast:?}"
        );
        let clause = lower_imperative_family_ast(ast);
        assert!(
            matches!(clause.effect, Effect::TurnFaceDown { .. }),
            "expected Effect::TurnFaceDown, got {:?}",
            clause.effect
        );
        assert_eq!(
            clause.multi_target,
            Some(MultiTargetSpec::unlimited(0)),
            "any number of target → unlimited (min 0) multi-target"
        );

        // Single-subject form carries no multi-target.
        let text = "turn target creature face down";
        let lower = text.to_lowercase();
        let ast = parse_imperative_family_ast(text, &lower, &mut ParseContext::default())
            .expect("single turn-face-down should parse");
        let clause = lower_imperative_family_ast(ast);
        assert_eq!(
            clause.multi_target, None,
            "single 'target creature' must not carry a multi-target spec"
        );
    }

    /// CR 608.2b: "turn target creature with a morph ability face down" (Backslide)
    /// carries a target restriction ("with a morph ability") the parser cannot
    /// model. Rather than silently drop it — which would illegally widen the
    /// legal-target set to any creature — the verb arm must reject the clause
    /// (returning `None`) so it falls through to `Unimplemented`. The unrestricted
    /// "turn target creature face down" still parses, pinning the guard's other
    /// end.
    #[test]
    fn turn_face_down_with_unmodeled_restriction_falls_through() {
        let text = "turn target creature with a morph ability face down";
        let lower = text.to_lowercase();
        let ast = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(
            ast.is_none(),
            "an unmodeled target restriction must not silently widen the target; \
             expected fall-through to Unimplemented, got {ast:?}"
        );

        // The unrestricted subject still parses to TurnFaceDown.
        let text = "turn target creature face down";
        let lower = text.to_lowercase();
        let ast = parse_imperative_family_ast(text, &lower, &mut ParseContext::default());
        assert!(
            matches!(ast, Some(ImperativeFamilyAst::TurnFaceDown { .. })),
            "unrestricted 'target creature' must still parse to TurnFaceDown, got {ast:?}"
        );
    }

    /// CR 115.6: "it fights up to one target creature …" allows zero targets.
    /// The optional-target cardinality must survive the full effect → clause →
    /// `AbilityDefinition` lowering as `AbilityDefinition.multi_target` with
    /// min=0 (`up_to(1)`), since the bare-`Effect::Fight` lowering cannot carry
    /// it. Building-block test across the optionality axis: "up to one" →
    /// `up_to(1)`, mandatory → `None`.
    #[test]
    fn lower_fight_up_to_one_target_carries_multi_target() {
        let def = super::super::parse_effect_chain(
            "it fights up to one target creature defending player controls",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::Fight { .. }),
            "Expected Effect::Fight, got {:?}",
            def.effect
        );
        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 })),
            "fight 'up to one target' must carry up_to(1) (min=0)"
        );
    }

    /// CR 701.14a: the mandatory "fights target creature" form (no "up to")
    /// carries NO multi_target — the target is required. Pins the other end of
    /// the optionality axis so the recovery does not over-apply.
    #[test]
    fn lower_fight_mandatory_target_no_multi_target() {
        let def = super::super::parse_effect_chain("it fights target creature", AbilityKind::Spell);
        assert!(
            matches!(*def.effect, Effect::Fight { .. }),
            "Expected Effect::Fight, got {:?}",
            def.effect
        );
        assert_eq!(
            def.multi_target, None,
            "mandatory fight target must not be optional"
        );
    }

    #[test]
    fn parse_choose_one_of_them() {
        let text = "choose one of them";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser, .. }) => {
                assert_eq!(count, 1);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet, got {other:?}"),
        }
    }

    // CR 701.13a + CR 109.5: Sothera, the Supervoid's per-actor edict. The
    // post-strip body ("choose a creature they control and exiles it") must
    // reparse to the controller-scoped edict "exile a creature you control".
    #[test]
    fn parse_choose_creature_they_control_and_exiles_it() {
        let text = "choose a creature they control and exiles it";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::Reparse { text }) => {
                assert_eq!(text, "exile a creature you control");
            }
            other => panic!("Expected Reparse(\"exile a creature you control\"), got {other:?}"),
        }
    }

    // Anti-Unimplemented reach-guard: the reparsed edict text must itself parse
    // to a real exile effect (not Unimplemented), proving the Reparse produces a
    // resolvable body rather than silently degrading.
    #[test]
    fn choose_and_exiles_it_reparse_body_is_not_unimplemented() {
        let effect = super::super::parse_effect("exile a creature you control");
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "reparsed edict body must resolve to a real exile effect, got {effect:?}"
        );
    }

    #[test]
    fn parse_choose_two_of_those_cards() {
        let text = "choose two of those cards";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser, .. }) => {
                assert_eq!(count, 2);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet with count=2, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_anaphoric_opponent() {
        let text = "an opponent chooses one of them";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser, .. }) => {
                assert_eq!(count, 1);
                assert_eq!(chooser, Chooser::Opponent);
            }
            other => panic!("Expected FromTrackedSet with Opponent chooser, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_anaphoric_you_choose() {
        let text = "you choose one of those cards";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser, .. }) => {
                assert_eq!(count, 1);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_up_to_two_of_them() {
        let text = "choose up to two of them";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromTrackedSet { count, chooser, .. }) => {
                assert_eq!(count, 2);
                assert_eq!(chooser, Chooser::Controller);
            }
            other => panic!("Expected FromTrackedSet with count=2, got {other:?}"),
        }
    }

    /// CR 702.62b + CR 608.2c: "choose a suspended card you own" (Amy Pond) routes
    /// to a `ChooseFromZone { Exile, Controller }` with the canonical suspended-card
    /// filter, even though it has no explicit "in/from <zone>" connector.
    #[test]
    fn parse_choose_suspended_card_you_own() {
        use crate::types::counter::{CounterMatch, CounterType};
        let text = "choose a suspended card you own";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone {
                count,
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(count, 1);
                assert_eq!(zones, vec![Zone::Exile]);
                assert_eq!(zone_owner, ZoneOwner::Controller);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed suspended-card filter, got {filter:?}");
                };
                assert!(tf.properties.contains(&FilterProp::HasKeywordKind {
                    value: crate::types::keywords::KeywordKind::Suspend,
                }));
                assert!(tf
                    .properties
                    .contains(&FilterProp::InZone { zone: Zone::Exile }));
                assert!(tf.properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::You,
                }));
                assert!(tf.properties.contains(&FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Time),
                    comparator: crate::types::ability::Comparator::GE,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                }));
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
    }

    /// Anti-misparse guard: an unsplit chain ("… and remove that many time
    /// counters from it" still attached) must NOT be claimed as a bare suspended
    /// selection — bail so the line falls to a strict failure, never a silent
    /// drop of the counter clause.
    #[test]
    fn choose_suspended_card_requires_clause_boundary() {
        let text = "choose a suspended card you own and remove that many time counters from it";
        let lower = text.to_lowercase();
        assert!(super::try_parse_choose_suspended_card(&lower).is_none());
    }

    #[test]
    fn parse_choose_creature_card_in_your_hand() {
        let text = "choose a creature card in your hand";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Hand]);
                assert_eq!(zone_owner, ZoneOwner::Controller);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed creature filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
    }

    /// D-09 regression: Riveteers Provocateur's "choose a creature card in your
    /// hand without blitz" has an unmodeled post-zone restriction ("without
    /// blitz"). The bare type filter (`Creature`) must still parse, but the
    /// unparseable suffix must NOT be force-fed to the strict search-filter
    /// suffix dispatch — doing so emits a `search-filter-suffix unmatched`
    /// TargetFallback diagnostic, which trips the parser-diagnostic ratchet.
    #[test]
    fn parse_choose_creature_card_with_unmodeled_post_zone_suffix_emits_no_diagnostic() {
        let text = "choose a creature card in your hand without blitz";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        let result = parse_choose_ast(text, &lower, &mut ctx);
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Hand]);
                assert_eq!(zone_owner, ZoneOwner::Controller);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed creature filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
        assert!(
            !ctx.diagnostics.iter().any(|d| matches!(
                d,
                OracleDiagnostic::TargetFallback { context, .. }
                    if context == "search-filter-suffix unmatched"
            )),
            "unmodeled post-zone suffix must not emit a search-filter-suffix \
             unmatched diagnostic, got {:?}",
            ctx.diagnostics
        );
    }

    #[test]
    fn parse_choose_instant_or_sorcery_card_in_hand_or_graveyard() {
        let text = "choose an instant or sorcery card in your hand or graveyard";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Hand, Zone::Graveyard]);
                assert_eq!(zone_owner, ZoneOwner::Controller);
                let TargetFilter::Or { filters } = filter else {
                    panic!("expected Or instant/sorcery filter, got {filter:?}");
                };
                assert_eq!(filters.len(), 2);
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_targeted_players_graveyard_or_hand() {
        let text = "you choose a nonland card from that player's graveyard or hand";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone {
                zones,
                zone_owner,
                filter,
                ..
            }) => {
                assert_eq!(zones, vec![Zone::Graveyard, Zone::Hand]);
                assert_eq!(zone_owner, ZoneOwner::TargetedPlayer);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("expected Typed nonland filter, got {filter:?}");
                };
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Land))));
            }
            other => panic!("Expected FromZone, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_random_card_in_graveyard_records_random_selection() {
        // CR 608.2d (override): "choose a card at random in your graveyard" is a
        // ChooseFromZone, but the "at random" qualifier is now captured as a
        // typed `CardSelectionMode::Random` (previously the parser bailed out and
        // dropped the random axis, treating it as a deliberate player choice).
        let text = "choose a card at random in your graveyard";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::FromZone { selection, .. }) => {
                assert_eq!(selection, CardSelectionMode::Random);
            }
            other => panic!("expected random FromZone, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_one_of_them_at_random_records_random_selection() {
        // CR 608.2d (override): "choose one of them at random" (River Song's
        // Diary) → anaphoric FromTrackedSet with CardSelectionMode::Random.
        let text = "choose one of them at random";
        let lower = text.to_lowercase();
        match parse_choose_ast(text, &lower, &mut ParseContext::default()) {
            Some(ChooseImperativeAst::FromTrackedSet {
                count, selection, ..
            }) => {
                assert_eq!(count, 1);
                assert_eq!(selection, CardSelectionMode::Random);
            }
            other => panic!("expected random FromTrackedSet, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_a_player_at_random_records_random_selection() {
        // CR 608.2d (override): "choose a player at random" (Strax) → NamedChoice
        // with TargetSelectionMode::Random.
        let text = "choose a player at random";
        let lower = text.to_lowercase();
        match parse_choose_ast(text, &lower, &mut ParseContext::default()) {
            Some(ChooseImperativeAst::NamedChoice { selection, .. }) => {
                assert_eq!(selection, TargetSelectionMode::Random);
            }
            other => panic!("expected random NamedChoice, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_from_zone_at_random_strips_qualifier_no_target_fallback() {
        // CR 608.2d (override): "choose a creature card at random from target
        // opponent's graveyard" (Tariel, Reckoner of Souls) → FromZone with
        // selection=Random and zone_owner=TargetedPlayer. The captured "at
        // random" must be stripped before the filter parse so it does NOT leak
        // into the search-filter prefix and emit a spurious
        // "search-filter-suffix unmatched" TargetFallback (pre-fix regression).
        let text = "choose a creature card at random from target opponent's graveyard";
        let lower = text.to_lowercase();
        let mut ctx = ParseContext::default();
        match parse_choose_ast(text, &lower, &mut ctx) {
            Some(ChooseImperativeAst::FromZone {
                selection,
                zone_owner,
                ..
            }) => {
                assert_eq!(selection, CardSelectionMode::Random);
                assert_eq!(zone_owner, ZoneOwner::TargetedPlayer);
            }
            other => panic!("expected random FromZone, got {other:?}"),
        }
        // The clean parse emits no target-fallback at all; pre-fix the leaked
        // "at random" produced a "search-filter-suffix unmatched" TargetFallback.
        assert!(
            !ctx.diagnostics
                .iter()
                .any(|d| matches!(d, OracleDiagnostic::TargetFallback { .. })),
            "the stripped 'at random' qualifier must not leak a target-fallback: {:?}",
            ctx.diagnostics
        );
    }

    #[test]
    fn parse_choose_anaphoric_non_random_defaults_to_chosen() {
        // Building-block regression: the ordinary anaphoric path stays Chosen.
        let text = "choose one of them";
        match parse_choose_anaphoric(&text.to_lowercase()) {
            Some((count, _chooser, selection)) => {
                assert_eq!(count, 1);
                assert_eq!(selection, CardSelectionMode::Chosen);
            }
            other => panic!("expected anaphoric tuple, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_creature_they_control() {
        // Imperial Edict pattern: "choose a creature they control"
        let text = "choose a creature they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::TargetOnly { target }) => {
                // Should extract creature filter with controller
                assert!(
                    matches!(target, TargetFilter::Typed { .. }),
                    "Expected Typed filter, got {target:?}"
                );
            }
            Some(ChooseImperativeAst::Reparse { .. }) => {
                // Also acceptable — reparse path handles "they control"
            }
            other => panic!("Expected TargetOnly or Reparse for 'they control', got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_from_among_cataclysm_pattern() {
        // Cataclysm: "choose from among the permanents they control an artifact, ..."
        let text =
            "choose from among the permanents they control an artifact, a creature, an enchantment, and a land";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope,
                ..
            }) => {
                assert_eq!(
                    categories,
                    vec![
                        CoreType::Artifact,
                        CoreType::Creature,
                        CoreType::Enchantment,
                        CoreType::Land
                    ]
                );
                assert_eq!(
                    chooser_scope,
                    crate::types::ability::CategoryChooserScope::EachPlayerSelf
                );
            }
            other => panic!("Expected CategoryAndSacrificeRest, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_permanent_of_each_type_liliana_dreadhorde() {
        // Liliana, Dreadhorde General −9: "Each opponent chooses a permanent they
        // control of each permanent type and sacrifices the rest."
        // The "Each opponent" actor prefix is stripped upstream; this exercises the
        // post-strip body reaching parse_choose_ast.
        let text = "choose a permanent they control of each permanent type";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope,
                ..
            }) => {
                assert_eq!(
                    categories,
                    vec![
                        CoreType::Artifact,
                        CoreType::Battle,
                        CoreType::Creature,
                        CoreType::Enchantment,
                        CoreType::Land,
                        CoreType::Planeswalker,
                    ]
                );
                assert_eq!(
                    chooser_scope,
                    crate::types::ability::CategoryChooserScope::EachPlayerSelf
                );
            }
            other => panic!("Expected CategoryAndSacrificeRest, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_from_among_gearhulk_pattern() {
        // Cataclysmic Gearhulk: "choose an artifact, a creature, ... from among ..."
        let text = "choose an artifact, a creature, an enchantment, and a planeswalker from among the nonland permanents they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                chooser_scope,
                choose_filter,
                sacrifice_filter,
                ..
            }) => {
                assert_eq!(
                    categories,
                    vec![
                        CoreType::Artifact,
                        CoreType::Creature,
                        CoreType::Enchantment,
                        CoreType::Planeswalker
                    ]
                );
                assert_eq!(
                    chooser_scope,
                    crate::types::ability::CategoryChooserScope::EachPlayerSelf
                );
                assert!(
                    matches!(
                        &choose_filter,
                        TargetFilter::Typed(TypedFilter {
                            type_filters,
                            ..
                        }) if type_filters.contains(&TypeFilter::Non(Box::new(TypeFilter::Land)))
                    ),
                    "Gearhulk choose_filter should be nonland permanent, got {choose_filter:?}"
                );
                assert_eq!(choose_filter, sacrifice_filter);
            }
            other => panic!("Expected CategoryAndSacrificeRest, got {other:?}"),
        }
    }

    #[test]
    fn parse_choose_from_among_gearhulk_pattern_with_comma() {
        let text = "choose an artifact, a creature, an enchantment, and a planeswalker, from among the nonland permanents they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::CategoryAndSacrificeRest {
                categories,
                choose_filter,
                sacrifice_filter,
                ..
            }) => {
                assert_eq!(
                    categories,
                    vec![
                        CoreType::Artifact,
                        CoreType::Creature,
                        CoreType::Enchantment,
                        CoreType::Planeswalker
                    ]
                );
                assert_eq!(choose_filter, sacrifice_filter);
            }
            other => panic!("Expected CategoryAndSacrificeRest, got {other:?}"),
        }
    }

    /// CR 115.1c + CR 601.2c: Goblin Welder activated ability.
    /// "Choose target artifact a player controls and target artifact card in
    /// that player's graveyard." must yield two distinct target slots.
    #[test]
    fn parse_choose_two_targets_goblin_welder() {
        use crate::types::ability::{FilterProp, TypeFilter};
        let text = "choose target artifact a player controls and target artifact card in that player's graveyard";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::TwoTargets { target_a, target_b }) => {
                let tf_a = match &target_a {
                    TargetFilter::Typed(tf) => tf,
                    other => panic!("target_a should be Typed, got {other:?}"),
                };
                assert_eq!(
                    tf_a.type_filters,
                    vec![TypeFilter::Artifact],
                    "target_a should be Artifact"
                );
                let tf_b = match &target_b {
                    TargetFilter::Typed(tf) => tf,
                    other => panic!("target_b should be Typed, got {other:?}"),
                };
                assert!(
                    tf_b.type_filters.contains(&TypeFilter::Artifact)
                        || tf_b.type_filters.contains(&TypeFilter::Card),
                    "target_b should reference an artifact card, got type_filters={:?}",
                    tf_b.type_filters
                );
                // CR 400.1: The second slot must anchor to the graveyard zone
                // via `FilterProp::InZone(Graveyard)` — the canonical zone
                // marker on a `TargetFilter::Typed`. The `parse_zone_qual`
                // combinator emits this for "in/from <player>'s graveyard"
                // suffixes (verified for Goblin Engineer's correct AST in
                // the L9-12b brief).
                assert!(
                    tf_b.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::InZone {
                            zone: Zone::Graveyard
                        }
                    )),
                    "target_b should carry FilterProp::InZone(Graveyard); got properties={:?}",
                    tf_b.properties
                );
            }
            other => panic!("Expected TwoTargets, got {other:?}"),
        }
    }

    /// CR 115.1c + CR 601.2c: TwoTargets lowering must emit a primary
    /// `TargetOnly` for slot A with a chained `TargetOnly` sub_ability for
    /// slot B so both targets are announced at activation.
    #[test]
    fn lower_choose_two_targets_emits_chained_target_only() {
        use crate::types::ability::TypeFilter;
        let ast = ImperativeFamilyAst::Structured(ImperativeAst::Choose(
            ChooseImperativeAst::TwoTargets {
                target_a: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Artifact],
                    ..Default::default()
                }),
                target_b: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    ..Default::default()
                }),
            },
        ));
        let clause = lower_imperative_family_ast(ast);
        match &clause.effect {
            Effect::TargetOnly {
                target: TargetFilter::Typed(tf),
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Artifact]);
            }
            other => panic!("primary effect should be TargetOnly(Artifact), got {other:?}"),
        }
        let sub = clause
            .sub_ability
            .as_ref()
            .expect("TwoTargets must produce a chained sub_ability for slot B");
        match &*sub.effect {
            Effect::TargetOnly {
                target: TargetFilter::Typed(tf),
            } => {
                assert_eq!(tf.type_filters, vec![TypeFilter::Card]);
            }
            other => panic!("sub_ability effect should be TargetOnly(Card), got {other:?}"),
        }
        assert!(
            sub.sub_ability.is_none(),
            "TwoTargets sub_ability should not chain further; resolution-time semantics live in higher-level continuation parsing"
        );
    }

    /// CR 115.1c regression: single-target "choose target X" must keep
    /// emitting the existing `TargetOnly` AST — the two-target detector must
    /// not steal single-slot wordings.
    #[test]
    fn parse_choose_single_target_unchanged() {
        let text = "choose a creature they control";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        match result {
            Some(ChooseImperativeAst::TargetOnly { .. })
            | Some(ChooseImperativeAst::Reparse { .. }) => {
                // Either path is acceptable — what matters is that we did NOT
                // accidentally promote this to TwoTargets.
            }
            Some(ChooseImperativeAst::TwoTargets { .. }) => {
                panic!("single-target wording must not be promoted to TwoTargets")
            }
            other => {
                panic!("Expected TargetOnly or Reparse for single-target wording, got {other:?}")
            }
        }
    }

    /// CR 609.7a: Desperate Gambit's "Choose a source you control" must lower to
    /// `ChooseDamageSource` with a controller-scoped candidate filter, not
    /// `TargetOnly { Any }`.
    #[test]
    fn desperate_gambit_choose_source_you_control_lowers_to_choose_damage_source() {
        use crate::types::ability::ControllerRef;

        let text = "choose a source you control";
        let lower = text.to_lowercase();
        let ast = parse_choose_ast(text, &lower, &mut ParseContext::default())
            .expect("must parse choose-a-source-you-control");
        match ast {
            ChooseImperativeAst::DamageSource { ref source_filter } => {
                let tf = typed_leg(source_filter).expect("expected typed source filter");
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.type_filters.is_empty(),
                    "article 'a' must not parse as subtype A: {tf:?}"
                );
            }
            other => panic!("expected DamageSource AST, got {other:?}"),
        }

        let effect = lower_choose_ast(ast);
        match effect {
            Effect::ChooseDamageSource { source_filter } => {
                let tf = typed_leg(&source_filter).expect("expected typed source filter");
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(
                    tf.type_filters.is_empty(),
                    "article 'a' must not parse as subtype A: {tf:?}"
                );
            }
            other => panic!("expected ChooseDamageSource effect, got {other:?}"),
        }
    }

    /// Depth-first search for the first effect matching `pred`, descending
    /// through `sub_ability`, `else_ability`, AND `FlipCoin` win/lose branches
    /// (each branch is a boxed `AbilityDefinition`). Needed because the shipped
    /// Desperate Gambit text produces TWO sibling `FlipCoin` nodes — one per
    /// "If you win / If you lose" sentence — each with only one populated branch.
    #[cfg(test)]
    fn find_effect_in_def<'a>(
        def: &'a AbilityDefinition,
        pred: &dyn Fn(&Effect) -> bool,
    ) -> Option<&'a Effect> {
        if pred(&def.effect) {
            return Some(&def.effect);
        }
        if let Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        } = &*def.effect
        {
            for branch in [win_effect, lose_effect] {
                if let Some(found) = branch.as_deref().and_then(|b| find_effect_in_def(b, pred)) {
                    return Some(found);
                }
            }
        }
        def.sub_ability
            .as_deref()
            .and_then(|s| find_effect_in_def(s, pred))
            .or_else(|| {
                def.else_ability
                    .as_deref()
                    .and_then(|e| find_effect_in_def(e, pred))
            })
    }

    /// CR 609.7a + CR 608.2c + CR 615.1 (#5601): Desperate Gambit's win and lose
    /// branches share one anaphor — "a source you control" — so both the win
    /// branch's damage-doubling replacement and the lose branch's prevention must
    /// bind `ChosenDamageSource`, never `SelfRef` (the Instant on the stack, which
    /// deals no damage, so the prevention would shield nothing).
    ///
    /// This uses the VERBATIM shipped oracle text ("Choose a source you control
    /// AND flip a coin" — one sentence). That form lowers the head choice to a
    /// bare target selection rather than `Effect::ChooseDamageSource`, so the
    /// repair pass's original `ChooseDamageSource`-keyed gate no longer fired and
    /// the lose branch silently reverted to `SelfRef` (the pass mutated
    /// 0/35,396 pool-wide). Reverting the flattened-head gate signal flips the
    /// lose-branch assertion below back to `SelfRef` — this test can fail if the
    /// fix is removed (AI-CONTRIBUTOR.md §5(i)).
    ///
    /// It drives the FULL card pipeline via `parse_oracle_text` — the exact entry
    /// `database::synthesis` uses to generate `card-data.json` — NOT the
    /// standalone `parse_effect_chain`, so it exercises the path the shipped card
    /// actually takes and cannot go green while the card is broken.
    #[test]
    fn desperate_gambit_shipped_text_threads_chosen_damage_source_both_branches() {
        use crate::parser::oracle::parse_oracle_text;

        let text = "Choose a source you control and flip a coin. If you win the flip, \
            the next time that source would deal damage this turn, it deals double that damage instead. \
            If you lose the flip, the next time it would deal damage this turn, prevent that damage.";
        let parsed =
            parse_oracle_text(text, "Desperate Gambit", &[], &["Instant".to_string()], &[]);

        let win = parsed
            .abilities
            .iter()
            .find_map(|d| {
                find_effect_in_def(d, &|e| matches!(e, Effect::CreateDamageReplacement { .. }))
            })
            .expect("win branch must emit a CreateDamageReplacement");
        assert!(
            matches!(
                win,
                Effect::CreateDamageReplacement {
                    source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
                    ..
                }
            ),
            "win branch must bind ChosenDamageSource, got {win:?}"
        );

        let lose = parsed
            .abilities
            .iter()
            .find_map(|d| find_effect_in_def(d, &|e| matches!(e, Effect::PreventDamage { .. })))
            .expect("lose branch must emit a PreventDamage");
        assert!(
            matches!(
                lose,
                Effect::PreventDamage {
                    damage_source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
                    ..
                }
            ),
            "lose branch prevention must thread ChosenDamageSource (not SelfRef), got {lose:?}"
        );
    }

    /// CR 615.1 no-regression guard for #5601: a prevention whose damage source
    /// is the source itself ("this creature", Mercenaries) has NO chosen-source
    /// context, so the #5601 gate must NOT fire — `SelfRef` must be preserved.
    /// Mercenaries is the only other card in the pool with a `SelfRef`
    /// damage-source prevention filter, so this pins the gate's blast radius.
    #[test]
    fn mercenaries_selfref_prevention_is_not_rewritten() {
        use crate::parser::oracle::parse_oracle_text;

        // Full card pipeline, activated ability form (Mercenaries' real card).
        let text = "{3}: The next time this creature would deal damage to you this turn, \
            prevent that damage. Any player may activate this ability.";
        let parsed = parse_oracle_text(
            text,
            "Mercenaries",
            &[],
            &["Creature".to_string()],
            &["Human".to_string(), "Mercenary".to_string()],
        );
        let prevent = parsed
            .abilities
            .iter()
            .find_map(|d| find_effect_in_def(d, &|e| matches!(e, Effect::PreventDamage { .. })))
            .expect("must emit a PreventDamage");
        assert!(
            matches!(
                prevent,
                Effect::PreventDamage {
                    damage_source_filter: Some(TargetFilter::SelfRef),
                    ..
                }
            ),
            "no chosen-source context: SelfRef must be preserved, got {prevent:?}"
        );
    }

    /// CR 115.1c + CR 608.2c regression: "target X and put a counter on it"
    /// must NOT be split into two target slots. The "and ..." continuation
    /// is a compound action handled by `try_split_targeted_compound`, not a
    /// second target slot. (This wording is not actually a "choose" form,
    /// but the unit-level guard ensures the two-target detector requires
    /// "and target Y" specifically — not just any "and" continuation.)
    #[test]
    fn parse_choose_target_and_non_target_continuation() {
        // "Choose target creature and put a +1/+1 counter on it" — the
        // second clause is NOT a target slot. The two-target detector must
        // refuse this and let single-target dispatch claim it.
        let text = "choose target creature and put a +1/+1 counter on it";
        let lower = text.to_lowercase();
        let result = parse_choose_ast(text, &lower, &mut ParseContext::default());
        // Any non-TwoTargets variant (TargetOnly with the creature filter, or
        // Reparse routing into the compound-action splitter) is fine — the
        // contract is just "no false positive on TwoTargets".
        if let Some(ChooseImperativeAst::TwoTargets { .. }) = result {
            panic!("non-target 'and ...' continuation must not be promoted to TwoTargets");
        }
    }

    #[test]
    fn lower_choose_anaphoric_to_choose_from_zone() {
        let ast = ChooseImperativeAst::FromTrackedSet {
            count: 3,
            chooser: Chooser::Opponent,
            selection: crate::types::ability::CardSelectionMode::Chosen,
        };
        let effect = lower_choose_ast(ast);
        match effect {
            Effect::ChooseFromZone {
                count,
                zone,
                additional_zones,
                zone_owner,
                filter,
                chooser,
                up_to,
                constraint,
                ..
            } => {
                assert_eq!(count, 3);
                assert_eq!(zone, Zone::Exile);
                assert!(additional_zones.is_empty());
                assert_eq!(zone_owner, ZoneOwner::Controller);
                assert!(filter.is_none());
                assert_eq!(chooser, Chooser::Opponent);
                assert!(!up_to);
                assert!(constraint.is_none());
            }
            other => panic!("Expected ChooseFromZone, got {other:?}"),
        }
    }

    #[test]
    fn parse_incubate_fixed() {
        let result = try_parse_incubate("incubate 3");
        assert!(result.is_some(), "Should parse 'incubate 3'");
        match result.unwrap() {
            Effect::Incubate { count } => {
                assert_eq!(count, QuantityExpr::Fixed { value: 3 });
            }
            other => panic!("Expected Incubate, got {other:?}"),
        }
    }

    #[test]
    fn parse_incubate_x() {
        let result = try_parse_incubate("incubate x");
        assert!(result.is_some(), "Should parse 'incubate x'");
        match result.unwrap() {
            Effect::Incubate { count } => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Variable { .. }
                        }
                    ),
                    "Expected Ref(Variable), got {count:?}"
                );
            }
            other => panic!("Expected Incubate, got {other:?}"),
        }
    }

    #[test]
    fn parse_attack_this_turn_if_able() {
        let result = try_parse_attack_if_able("attacks this turn if able");
        assert!(result.is_some(), "Should parse 'attacks this turn if able'");
        match result.unwrap() {
            ImperativeFamilyAst::GainKeyword(Effect::GenericEffect {
                static_abilities,
                duration,
                ..
            }) => {
                assert_eq!(
                    static_abilities[0].mode,
                    crate::types::statics::StaticMode::MustAttack
                );
                assert_eq!(duration, Some(Duration::UntilEndOfTurn));
            }
            other => panic!("Expected GenericEffect with MustAttack, got {other:?}"),
        }
    }

    #[test]
    fn parse_attack_this_combat_if_able() {
        let result = try_parse_attack_if_able("attack this combat if able");
        assert!(
            result.is_some(),
            "Should parse 'attack this combat if able'"
        );
        match result.unwrap() {
            ImperativeFamilyAst::GainKeyword(Effect::GenericEffect { duration, .. }) => {
                assert_eq!(duration, Some(Duration::UntilEndOfCombat));
            }
            other => panic!("Expected GenericEffect, got {other:?}"),
        }
    }

    #[test]
    fn parse_attack_you_this_combat_if_able() {
        let result = try_parse_attack_if_able("attacks you this combat if able");
        assert!(
            result.is_some(),
            "Should parse 'attacks you this combat if able'"
        );
        match result.unwrap() {
            ImperativeFamilyAst::ForceAttack {
                duration,
                required_player,
            } => {
                assert_eq!(duration, Duration::UntilEndOfCombat);
                assert_eq!(required_player, TargetFilter::Controller);
            }
            other => panic!("Expected ForceAttack, got {other:?}"),
        }
    }

    #[test]
    fn parse_attack_that_player_this_combat_if_able() {
        // CR 508.1d: Ruhan of the Fomori / Raving Dead / Knight Rampager —
        // "attacks that player" references the opponent chosen earlier in the
        // same resolution, lowered to ControllerRef::ChosenPlayer { index: 0 }.
        let result = try_parse_attack_if_able("attacks that player this combat if able")
            .expect("should parse 'attacks that player this combat if able'");
        match result {
            ImperativeFamilyAst::ForceAttack {
                duration,
                required_player,
            } => {
                assert_eq!(duration, Duration::UntilEndOfCombat);
                assert_eq!(
                    required_player.chosen_player_index(),
                    Some(0),
                    "that player must reference the chosen player at index 0, got {required_player:?}"
                );
            }
            other => panic!("Expected ForceAttack, got {other:?}"),
        }
    }

    #[test]
    fn ruhan_choose_opponent_then_force_attack_composes() {
        use crate::types::ability::{ChoiceType, Effect};

        // CR 608.2c: The full Ruhan-class trigger composes the opponent choice
        // with a forced attack at that opponent. Both resolve together, so the
        // sub-ability references the resolution-scoped ChosenPlayer { index }.
        let parsed = crate::parser::oracle::parse_oracle_text(
            "At the beginning of combat on your turn, choose an opponent at random. ~ attacks that player this combat if able.",
            "Ruhan of the Fomori",
            &[],
            &["Creature".to_string()],
            &[],
        );
        let trigger = parsed
            .triggers
            .first()
            .expect("should produce one triggered ability");
        let execute = trigger
            .execute
            .as_ref()
            .expect("trigger has an execute chain");
        assert!(
            matches!(
                &*execute.effect,
                Effect::Choose {
                    choice_type: ChoiceType::Opponent { .. },
                    ..
                }
            ),
            "parent must be an opponent choice, got {:?}",
            execute.effect
        );
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("the force-attack must chain as the sub-ability");
        // The forced attacker is the source (~ → SelfRef), forced to attack the
        // resolution-scoped chosen opponent (chosen_players[0]).
        let Effect::ForceAttack {
            target,
            required_player,
            duration,
        } = &*sub.effect
        else {
            panic!("sub-ability must be a ForceAttack, got {:?}", sub.effect);
        };
        assert_eq!(*target, TargetFilter::SelfRef);
        assert_eq!(*duration, Duration::UntilEndOfCombat);
        assert_eq!(
            required_player.chosen_player_index(),
            Some(0),
            "must force attacking the chosen player, got {required_player:?}"
        );
    }

    /// CR 508.1d / CR 509.1c: the standalone-combat-requirement recognizer
    /// used to gate the conjunction split. Recognizes both attack and
    /// must-be-blocked forms, and rejects non-requirements.
    #[test]
    fn standalone_combat_requirement_recognizer() {
        assert!(is_standalone_combat_requirement(
            "attack this combat if able"
        ));
        assert!(is_standalone_combat_requirement(
            "attacks this turn if able"
        ));
        assert!(is_standalone_combat_requirement(
            "attacks or blocks this turn if able"
        ));
        assert!(is_standalone_combat_requirement(
            "attack or block this combat if able"
        ));
        assert!(is_standalone_combat_requirement(
            "must be blocked this turn if able"
        ));
        assert!(is_standalone_combat_requirement("must be blocked if able"));
        // Not combat requirements.
        assert!(!is_standalone_combat_requirement("haste"));
        assert!(!is_standalone_combat_requirement("gains flying"));
        assert!(!is_standalone_combat_requirement("draw a card"));
    }

    /// CR 400.6 + CR 701.24c: "shuffle the cards from your hand into your
    /// library" — Whirlpool Drake class. The phrase names every card in the
    /// hand, so the lowered AST must be ChangeZoneAllToLibrary (mass move),
    /// not a TargetedChangeZoneToLibrary where "the cards" would be read as
    /// a pronoun target.
    #[test]
    fn parse_shuffle_cards_from_your_hand_into_your_library() {
        let text = "shuffle the cards from your hand into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Hand, got {other:?}"),
        }
    }

    /// Sibling coverage: the same structural phrase with a different zone
    /// ("from your graveyard into your library") must also route to the mass
    /// path — confirms the combinator generalizes across zones.
    #[test]
    fn parse_shuffle_cards_from_your_graveyard_into_your_library() {
        let text = "shuffle the cards from your graveyard into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Graveyard]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Graveyard, got {other:?}"),
        }
    }

    /// Possessive variance: "shuffle the cards from their hand into their
    /// library" (opponent-facing phrasing) — same structure, different
    /// possessive.
    #[test]
    fn parse_shuffle_cards_from_their_hand_into_their_library() {
        let text = "shuffle the cards from their hand into their library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Hand, got {other:?}"),
        }
    }

    /// CR 608.2c: "shuffle that library" — anaphoric reference to the
    /// player target bound earlier in the same instruction (Visions).
    #[test]
    fn parse_shuffle_that_library_resolves_to_parent_target() {
        let text = "shuffle that library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ShuffleLibrary { target }) => {
                assert_eq!(target, TargetFilter::ParentTarget);
            }
            other => panic!("Expected ShuffleLibrary {{ ParentTarget }}, got {other:?}"),
        }
    }

    #[test]
    fn parse_shuffle_possessive_library_siblings() {
        for text in [
            "shuffle that player's library",
            "shuffle their library",
            "shuffle his or her library",
        ] {
            let result = parse_shuffle_ast(text, text);
            match result {
                Some(ShuffleImperativeAst::ShuffleLibrary { target }) => {
                    assert_eq!(target, TargetFilter::ParentTarget);
                }
                other => {
                    panic!("Expected ShuffleLibrary {{ ParentTarget }} for {text}, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn parse_shuffle_hand_and_graveyard_into_your_library() {
        let text = "shuffle your hand and graveyard into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand, Zone::Graveyard]);
            }
            other => panic!("Expected ChangeZoneAllToLibrary Hand+Graveyard, got {other:?}"),
        }
    }

    #[test]
    fn lower_shuffle_hand_and_graveyard_into_change_zone_all_chain() {
        let clause = lower_shuffle_ast(ShuffleImperativeAst::ChangeZoneAllToLibrary {
            origins: vec![Zone::Hand, Zone::Graveyard],
        });
        assert!(matches!(
            &clause.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                target: TargetFilter::Controller,
                ..
            }
        ));

        let graveyard = clause
            .sub_ability
            .as_deref()
            .expect("hand move should chain graveyard move");
        assert!(matches!(
            &*graveyard.effect,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                target: TargetFilter::Controller,
                ..
            }
        ));

        let shuffle = graveyard
            .sub_ability
            .as_deref()
            .expect("graveyard move should chain shuffle");
        assert!(matches!(
            &*shuffle.effect,
            Effect::Shuffle {
                target: TargetFilter::Controller,
            }
        ));
        assert!(shuffle.sub_ability.is_none());
    }

    /// CR 701.24c + CR 400.3: "shuffle ~ into its owner's library" — the
    /// self-referential tail of Green Sun's Zenith, the Beacon cycle, Nexus
    /// of Fate, etc. The `~` token is produced by `normalize_card_name_refs`
    /// for the source card's own name; the AST must classify it as
    /// `TargetFilter::SelfRef` with `owner_library: true`, NOT
    /// `Unimplemented`. This is the building-block assertion that unlocks
    /// the entire self-shuffle class.
    #[test]
    fn parse_shuffle_self_ref_into_owners_library() {
        let text = "shuffle ~ into its owner's library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            }) => {
                assert_eq!(target, TargetFilter::SelfRef);
                assert!(owner_library);
            }
            other => panic!("Expected ChangeZoneToLibrary SelfRef+owner_library, got {other:?}"),
        }
    }

    /// Sibling coverage: "shuffle ~ into your library" — same self-reference
    /// shape but possessive resolves to controller (`owner_library: false`),
    /// matching the variant cycle that names "your library" explicitly.
    #[test]
    fn parse_shuffle_self_ref_into_your_library() {
        let text = "shuffle ~ into your library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            }) => {
                assert_eq!(target, TargetFilter::SelfRef);
                assert!(!owner_library);
            }
            other => panic!("Expected ChangeZoneToLibrary SelfRef+!owner_library, got {other:?}"),
        }
    }

    /// Regression: "shuffle their hand and graveyard into their library"
    /// (Echo of Eons after subject-stripping + deconjugation) must still route
    /// to `ChangeZoneAllToLibrary`, NOT the descriptive-target path.
    #[test]
    fn parse_shuffle_their_hand_and_graveyard_into_their_library() {
        let text = "shuffle their hand and graveyard into their library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneAllToLibrary { origins }) => {
                assert_eq!(origins, vec![Zone::Hand, Zone::Graveyard]);
            }
            other => {
                panic!("Expected ChangeZoneAllToLibrary Hand+Graveyard, got {other:?}")
            }
        }
    }

    /// CR 701.24c + CR 400.3: "shuffle enchanted creature into its owner's
    /// library" — the descriptive-target path for Aura-based shuffle effects
    /// (Dramatic Accusation, Stay Hidden Stay Silent). The target is parsed by
    /// `parse_target` which resolves "enchanted creature" to a `Typed` filter
    /// with `EnchantedBy` property. `owner_library` is `true` because the
    /// possessive resolves to the card's owner.
    #[test]
    fn parse_shuffle_enchanted_creature_into_owners_library() {
        use crate::types::ability::FilterProp;
        let text = "shuffle enchanted creature into its owner's library";
        let result = parse_shuffle_ast(text, text);
        match result {
            Some(ShuffleImperativeAst::ChangeZoneToLibrary {
                target,
                owner_library,
            }) => {
                // "enchanted creature" → Typed filter with EnchantedBy property
                match &target {
                    TargetFilter::Typed(tf) => {
                        assert!(
                            tf.properties.contains(&FilterProp::EnchantedBy),
                            "expected EnchantedBy property, got {tf:?}"
                        );
                    }
                    other => panic!("Expected Typed filter with EnchantedBy, got {other:?}"),
                }
                assert!(owner_library, "expected owner_library = true");
            }
            other => {
                panic!("Expected ChangeZoneToLibrary with EnchantedBy target, got {other:?}")
            }
        }
    }

    /// Issue #4255 — Memory's Journey: "Target player shuffles up to three
    /// target cards from their graveyard into their library." The "up to three
    /// target cards" count must survive onto the `ChangeZone` ability as a
    /// `MultiTargetSpec` so the cast surfaces three target slots rather than
    /// one. CR 115.1d. Drives the full `parse_effect_chain` pipeline (subject
    /// shift → shuffle imperative → lowering) and fails if the count is dropped.
    #[test]
    fn memorys_journey_up_to_three_targets_survives_to_change_zone() {
        let def = super::super::parse_effect_chain(
            "Target player shuffles up to three target cards from their graveyard into their library.",
            AbilityKind::Spell,
        );

        // Walk the sub_ability chain to the ChangeZone(Library) link.
        fn find_change_zone_multi_target(
            def: &AbilityDefinition,
        ) -> Option<Option<MultiTargetSpec>> {
            if matches!(
                &*def.effect,
                Effect::ChangeZone {
                    destination: Zone::Library,
                    ..
                }
            ) {
                return Some(def.multi_target.clone());
            }
            def.sub_ability
                .as_deref()
                .and_then(find_change_zone_multi_target)
        }

        let multi_target = find_change_zone_multi_target(&def).unwrap_or_else(|| {
            panic!("expected a ChangeZone(Library) link in the chain, got {def:?}")
        });
        assert_eq!(
            multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 3 })),
            "the 'up to three target cards' count must surface as a MultiTargetSpec"
        );
    }

    /// Issue #5898 — Settle the Wreckage: "That player may search their library
    /// for that many basic land cards …". The "for that many" anaphoric count
    /// (CR 608.2c, = the number of creatures exiled by the prior instruction)
    /// without an "up to" prefix previously fell through to the `for N` number
    /// parser, which rejected "that many" and defaulted the search to
    /// `count: Fixed 1` with a dropped type filter (`Any`) — so it searched for a
    /// single card of any type ("functioning as Chaos Warp"). The count must
    /// surface as `EventContextAmount` and the "basic land cards" type phrase must
    /// still reach the filter.
    #[test]
    fn settle_the_wreckage_for_that_many_basic_lands_binds_event_count() {
        fn find_search(def: &AbilityDefinition) -> Option<(QuantityExpr, TargetFilter)> {
            if let Effect::SearchLibrary { filter, count, .. } = &*def.effect {
                return Some((count.clone(), filter.clone()));
            }
            def.sub_ability.as_deref().and_then(find_search)
        }

        let def = super::super::parse_effect_chain(
            "That player may search their library for that many basic land cards, put those cards onto the battlefield tapped, then shuffle.",
            AbilityKind::Spell,
        );
        let (count, filter) = find_search(&def)
            .unwrap_or_else(|| panic!("expected a SearchLibrary link in the chain, got {def:?}"));
        assert_eq!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            },
            "'for that many' must bind the anaphoric event count, not default to Fixed 1"
        );
        let TargetFilter::Typed(basic_land) = filter else {
            panic!("expected typed basic-land filter, got {filter:?}");
        };
        assert!(
            basic_land.type_filters.contains(&TypeFilter::Land),
            "the search filter must preserve the Land type, got {basic_land:?}"
        );
        assert!(
            basic_land.properties.iter().any(|property| matches!(
                property,
                FilterProp::HasSupertype {
                    value: crate::types::card_type::Supertype::Basic
                }
            )),
            "the search filter must preserve the Basic supertype, got {basic_land:?}"
        );
    }

    /// CR 400.7 + CR 701.23 + CR 107.1c: Multi-zone same-name exile combinator.
    /// The `All` ("all cards") quantifier accepts every possessive (mandatory
    /// mass-exile class: Eradicate, Quash, Counterbore, Lost Legacy). The
    /// interactive `AnyNumber` / `UpTo` quantifiers accept ONLY object-relative
    /// possessives (`its owner's` / `its controller's`) — the §9 R1 guard —
    /// because this recognizer binds `SameNameAsParentTarget`; player-possessive
    /// interactive forms name their card via a "Choose a card name" clause and
    /// must route through the general `HasChosenName` path. Both "with that name"
    /// and "with the same name as that <noun>" forms are accepted.
    #[test]
    fn parse_multi_zone_same_name_exile_pattern() {
        use MultiZoneExileQuantifier::{All, AnyNumber, UpTo};
        let positives = [
            // `All` — every possessive accepted.
            (
                "search target player's graveyard, hand, and library for all cards with that name and exile them",
                ControllerRef::TargetPlayer,
                All,
            ),
            (
                "search its controller's graveyard, hand, and library for all cards with the same name as that creature and exile them",
                ControllerRef::ParentTargetController,
                All,
            ),
            (
                "search its controller's graveyard, hand, and library for all cards with the same name as that spell and exile them",
                ControllerRef::ParentTargetController,
                All,
            ),
            (
                "search its owner's graveyard, hand, and library for all cards with the same name as that card and exile them",
                ControllerRef::ParentTargetOwner,
                All,
            ),
            // Interactive — object-relative possessives only (R1). Deadly Cover-Up,
            // Surgical Extraction, Crumble to Dust, The End class.
            (
                "search its owner's graveyard, hand, and library for any number of cards with that name and exile them",
                ControllerRef::ParentTargetOwner,
                AnyNumber,
            ),
            (
                "search its owner's graveyard, hand, and library for any number of cards with the same name as that card and exile them",
                ControllerRef::ParentTargetOwner,
                AnyNumber,
            ),
            (
                "search its controller's graveyard, hand, and library for any number of cards with the same name as that land and exile them",
                ControllerRef::ParentTargetController,
                AnyNumber,
            ),
            // "up to N cards" interactive form, object-relative.
            (
                "search its controller's graveyard, hand, and library for up to four cards with the same name as that spell and exile them",
                ControllerRef::ParentTargetController,
                UpTo(4),
            ),
        ];
        for (text, owner, quantifier) in positives {
            assert_eq!(
                try_parse_multi_zone_same_name_exile(text),
                Some((owner.clone(), quantifier)),
                "expected match ({owner:?}, {quantifier:?}) for: {text}"
            );
        }

        let negatives = [
            // R1: player-possessive interactive forms decline here (→ HasChosenName path).
            "search target player's graveyard, hand, and library for any number of cards with that name and exile them",
            "search their graveyard, hand, and library for up to four cards with that name and exile them",
            "search target opponent's graveyard, hand, and library for up to four cards with that name and exile them",
            // "a card" quantifier is not in the axis.
            "search their graveyard, hand, and library for a card with that name and exile them",
            // Library-only — handled by the regular SearchLibrary branch.
            "search your library for a card",
            // Two-zone permutation we don't recognize (deliberate scope cut).
            "search target player's graveyard and library for any number of cards with that name and exile them",
            // Different action verb after — single-zone search-and-put-into-hand.
            "search your library for a basic land card and put it into your hand",
        ];
        for text in negatives {
            assert!(
                try_parse_multi_zone_same_name_exile(text).is_none(),
                "expected no match for: {text}"
            );
        }
    }

    #[test]
    fn parse_copy_stack_ability_target_preserves_unknown_qualifier_remainder() {
        let controlled =
            parse_copy_stack_ability_target("target activated or triggered ability you control")
                .expect("controlled stack ability target should parse");
        assert_eq!(controlled.1, "");
        assert!(matches!(
            controlled.0,
            TargetFilter::StackAbility {
                controller: Some(ControllerRef::You),
                tag: None,
                kind: None,
            }
        ));

        let unscoped = parse_copy_stack_ability_target("target triggered ability")
            .expect("unqualified stack ability target should parse");
        assert_eq!(unscoped.1, "");
        assert!(matches!(
            unscoped.0,
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            }
        ));

        assert!(
            parse_copy_stack_ability_target(
                "target activated or triggered ability you don't control"
            )
            .is_none(),
            "unknown qualifier must not widen to an unscoped StackAbility target"
        );
    }

    /// Issue #3996 — Return the Favor copy mode must accept instants, sorceries,
    /// and stack abilities, not only the first spell type.
    #[test]
    fn copy_four_way_stack_object_disjunction_parses_or_filter() {
        use crate::types::ability::TypeFilter;
        use crate::types::ability::{CopyRetargetPermission, Effect, TargetFilter};

        let def = super::super::parse_effect_chain(
            "Copy target instant spell, sorcery spell, activated ability, or triggered ability. \
             You may choose new targets for the copy.",
            AbilityKind::Spell,
        );
        let Effect::CopySpell {
            target, retarget, ..
        } = &*def.effect
        else {
            panic!("expected CopySpell, got {:?}", def.effect);
        };
        let TargetFilter::Or { filters } = target else {
            panic!("expected Or stack-object filter, got {target:?}");
        };
        assert!(
            filters.iter().any(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(tf)
                        if tf.type_filters == [TypeFilter::Instant]
                )
            }),
            "missing instant spell leg: {filters:?}"
        );
        assert!(
            filters.iter().any(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(tf)
                        if tf.type_filters == [TypeFilter::Sorcery]
                )
            }),
            "missing sorcery spell leg: {filters:?}"
        );
        assert!(
            filters
                .iter()
                .any(|f| matches!(f, TargetFilter::StackAbility { .. })),
            "missing stack ability leg: {filters:?}"
        );
        assert_eq!(
            *retarget,
            CopyRetargetPermission::MayChooseNewTargets,
            "retarget clause must parse"
        );
    }

    #[test]
    fn parse_search_creation_lowering_emits_change_zone_all_with_same_name_as_parent_target() {
        use crate::types::ability::FilterProp;
        let text = "search its owner's graveyard, hand, and library for all cards with that name and exile them";
        let ast = parse_search_and_creation_ast(text, text, &mut ParseContext::default())
            .expect("multi-zone same-name exile must parse");
        let effect = lower_search_and_creation_ast(ast);
        match effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                ..
            } => {
                assert!(
                    origin.is_none(),
                    "origin must be None — zones come from filter"
                );
                assert_eq!(destination, Zone::Exile);
                let TargetFilter::Typed(tf) = target else {
                    panic!("Expected Typed target, got {target:?}");
                };
                assert_eq!(
                    tf.controller,
                    Some(ControllerRef::ParentTargetOwner),
                    "possessive owner must scope searched zones"
                );
                let zones_ok = tf.properties.iter().any(|p| {
                    matches!(p, FilterProp::InAnyZone { zones }
                        if zones == &vec![Zone::Graveyard, Zone::Hand, Zone::Library])
                });
                let same_name_ok = tf
                    .properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::SameNameAsParentTarget));
                assert!(zones_ok, "InAnyZone[GY,Hand,Lib] missing");
                assert!(same_name_ok, "SameNameAsParentTarget missing");
            }
            other => panic!("Expected ChangeZoneAll, got {other:?}"),
        }

        // S25-P2a (W1'): the object-relative "any number" quantifier now lowers
        // to an interactive `Effect::SearchLibrary` (CR 701.23b — the searcher may
        // fail to find), NOT the mandatory `ChangeZoneAll` mass exile.
        let any_number = "search its owner's graveyard, hand, and library for any number of cards with that name and exile them";
        let any_number_ast =
            parse_search_and_creation_ast(any_number, any_number, &mut ParseContext::default())
                .expect("object-relative any-number multi-zone same-name exile must parse (W1')");
        assert!(
            matches!(
                lower_search_and_creation_ast(any_number_ast),
                Effect::SearchLibrary { .. }
            ),
            "any-number object-relative multi-zone same-name exile must lower to SearchLibrary"
        );
    }

    /// CR 113.3 + CR 604.1: `gain "<quoted ability>"` in a sub_ability context
    /// produces a `GenericEffect` wrapping a `GrantTrigger` modification when
    /// the quoted text starts with `When`/`Whenever`/`At …`. Used by Rabid
    /// Attack: `+1/+0 and gain "When this creature dies, draw a card."` until
    /// end of turn.
    #[test]
    fn gain_quoted_trigger_ability_until_end_of_turn() {
        let effect =
            try_parse_gain_quoted_ability("gain \"When this creature dies, draw a card.\"")
                .expect("expected gain-quoted-ability to parse");
        let Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } = effect
        else {
            panic!("expected GenericEffect, got something else");
        };
        assert_eq!(duration.as_ref(), Some(&Duration::UntilEndOfTurn));
        let static_def = static_abilities
            .first()
            .expect("static_abilities must contain the granted modification");
        let grant_trigger = static_def
            .modifications
            .iter()
            .find(|m| matches!(m, ContinuousModification::GrantTrigger { .. }));
        assert!(
            grant_trigger.is_some(),
            "expected a GrantTrigger modification, got modifications: {:?}",
            static_def.modifications
        );
    }

    /// `try_parse_gain_quoted_ability` must NOT swallow bare keyword grants —
    /// those belong to `try_parse_gain_keyword`. Returning `None` here lets
    /// the dispatcher's `or_else` try the bare-keyword path first.
    #[test]
    fn gain_quoted_ability_returns_none_for_bare_keyword() {
        assert!(
            try_parse_gain_quoted_ability("gain flying until end of turn").is_none(),
            "no quote marks → not a quoted-ability candidate"
        );
    }

    // ── CR 113.1a + CR 611.2: "gain[s] all activated abilities of target X" ──

    /// Parser test 1: "gains all activated abilities of target creature until
    /// end of turn" → `GainActivatedAbilitiesOfTarget` with a creature target,
    /// `SelfRef` recipient, and `UntilEndOfTurn` duration (Quicksilver Elemental).
    #[test]
    fn gain_all_activated_abilities_of_target_creature() {
        let effect = try_parse_gain_all_activated_abilities_of_target(
            "gains all activated abilities of target creature until end of turn",
        )
        .expect("expected GainActivatedAbilitiesOfTarget");
        let Effect::GainActivatedAbilitiesOfTarget {
            target,
            recipient,
            scope,
            duration,
        } = effect
        else {
            panic!("expected GainActivatedAbilitiesOfTarget, got something else");
        };
        assert_eq!(
            scope,
            GrantedAbilityScope::ActivatedOnly,
            "Quicksilver donor scope is activated-only"
        );
        assert_eq!(recipient, TargetFilter::SelfRef, "implicit self-recipient");
        assert_eq!(duration, Some(Duration::UntilEndOfTurn));
        let TargetFilter::Typed(tf) = target else {
            panic!("expected a typed creature target, got {target:?}");
        };
        assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, crate::types::ability::TypeFilter::Creature)),
            "donor target must be a creature; got {tf:?}"
        );
    }

    /// Parser test 2: "gains all activated abilities of target artifact an
    /// opponent controls until end of turn" → artifact + opponent-controls
    /// donor filter (Grell Philosopher's donor phrase).
    #[test]
    fn gain_all_activated_abilities_of_opponent_artifact() {
        let effect = try_parse_gain_all_activated_abilities_of_target(
            "gains all activated abilities of target artifact an opponent controls until end of turn",
        )
        .expect("expected GainActivatedAbilitiesOfTarget");
        let Effect::GainActivatedAbilitiesOfTarget { target, .. } = effect else {
            panic!("expected GainActivatedAbilitiesOfTarget");
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected a typed artifact target, got {target:?}");
        };
        assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, crate::types::ability::TypeFilter::Artifact)),
            "donor target must be an artifact; got {tf:?}"
        );
        assert_eq!(
            tf.controller,
            Some(crate::types::ability::ControllerRef::Opponent),
            "donor must be controlled by an opponent; got {tf:?}"
        );
    }

    /// Parser test 3: malformed "gains all activated abilities" (no "of X") must
    /// return `None` rather than produce a half-built effect.
    #[test]
    fn gain_all_activated_abilities_without_donor_returns_none() {
        assert!(
            try_parse_gain_all_activated_abilities_of_target("gains all activated abilities")
                .is_none(),
            "no donor phrase → must not produce a half-built effect"
        );
    }

    /// Parser test 4 (dispatch-ordering guard): "gains flying until end of
    /// turn" must route to the bare-keyword grant path, NOT the new combinator.
    #[test]
    fn gain_flying_is_not_captured_by_all_activated_abilities_combinator() {
        assert!(
            try_parse_gain_all_activated_abilities_of_target("gains flying until end of turn")
                .is_none(),
            "bare keyword grant must not be captured by the new combinator"
        );
        // And the keyword path still produces a keyword grant.
        assert!(
            try_parse_gain_keyword("gains flying until end of turn").is_some(),
            "the keyword grant path must still handle bare 'gains flying'"
        );
    }

    /// Parser test 5: full real-Oracle-text parse-through of Quicksilver
    /// Elemental. The `{U}:` activated ability's effect must lower to
    /// `GainActivatedAbilitiesOfTarget` (no longer `Unimplemented`), and the
    /// untouched "spend blue mana as any color" sentence must still produce its
    /// own ability — proving the new effect slots in without disturbing the
    /// rest of the card.
    #[test]
    fn quicksilver_elemental_full_oracle_parses_gain_effect() {
        const TEXT: &str = "{U}: This creature gains all activated abilities of target creature until end of turn. (If any of the abilities use that creature's name, use this creature's name instead.)\nYou may spend blue mana as though it were mana of any color to pay the activation costs of this creature's abilities.";
        let parsed = crate::parser::oracle::parse_oracle_text(
            TEXT,
            "Quicksilver Elemental",
            &[],
            &["Creature".to_string()],
            &["Elemental".to_string()],
        );

        let gain = parsed
            .abilities
            .iter()
            .find(|a| {
                matches!(
                    a.effect.as_ref(),
                    Effect::GainActivatedAbilitiesOfTarget { .. }
                )
            })
            .expect("the {U} ability must lower to GainActivatedAbilitiesOfTarget");
        let Effect::GainActivatedAbilitiesOfTarget {
            recipient,
            duration,
            ..
        } = gain.effect.as_ref()
        else {
            unreachable!()
        };
        assert_eq!(*recipient, TargetFilter::SelfRef, "implicit self-recipient");
        assert_eq!(*duration, Some(Duration::UntilEndOfTurn));

        assert!(
            !parsed.abilities.iter().any(|a| matches!(
                a.effect.as_ref(),
                Effect::Unimplemented { name, .. } if name == "gain"
            )),
            "the 'gain' clause must no longer be Unimplemented"
        );
    }

    /// Parser test 6: full real-Oracle-text parse-through of Grell Philosopher,
    /// proving the GROUP-recipient rebind in `lower_subject_predicate_ast`'s
    /// `ImperativeFallback` arm (oracle_effect/mod.rs) is reachable from the real
    /// production parsing path. Grell's clause lives inside the "Aberrant
    /// Tinkering — When this creature enters and at the beginning of your upkeep,
    /// each Horror you control gains all activated abilities of target artifact
    /// an opponent controls until end of turn." trigger sentence (the
    /// byte-for-byte `oracle_text` from `data/card-data.json`). Test 5 uses the
    /// full card text via `parse_oracle_text`, so this follows the same unit.
    ///
    /// The resolver-level `group_recipient_grants_to_each_matching_object_only`
    /// test hand-constructs the effect and so never exercises this rebind; this
    /// is the missing proof that production parsing threads "each Horror you
    /// control" into `recipient` (NOT defaulting to `SelfRef`) while keeping the
    /// opponent-controlled-artifact donor as `target`.
    #[test]
    fn grell_philosopher_full_oracle_rebinds_group_recipient() {
        const TEXT: &str = "Aberrant Tinkering — When this creature enters and at the beginning of your upkeep, each Horror you control gains all activated abilities of target artifact an opponent controls until end of turn. You may spend blue mana as though it were mana of any color to activate those abilities.";
        let parsed = crate::parser::oracle::parse_oracle_text(
            TEXT,
            "Grell Philosopher",
            &[],
            &["Creature".to_string()],
            &["Horror".to_string()],
        );

        // Grell's grant is the body of the "Aberrant Tinkering" triggered
        // ability, so it lands in `parsed.triggers[*].execute` (NOT
        // `parsed.abilities`). Walk each trigger's `execute` effect plus its
        // `sub_ability` chain so we find it whether it is the top-level effect or
        // a chained clause.
        fn find_gain(def: &AbilityDefinition) -> Option<&Effect> {
            let mut cur = Some(def);
            while let Some(d) = cur {
                if matches!(
                    d.effect.as_ref(),
                    Effect::GainActivatedAbilitiesOfTarget { .. }
                ) {
                    return Some(d.effect.as_ref());
                }
                cur = d.sub_ability.as_deref();
            }
            None
        }
        let gain = parsed
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .find_map(find_gain)
            .expect("Grell's trigger must lower to GainActivatedAbilitiesOfTarget");
        let Effect::GainActivatedAbilitiesOfTarget {
            target,
            recipient,
            scope: _,
            duration,
        } = gain
        else {
            unreachable!()
        };

        // Recipient must be the typed "Horror you control" group filter, NOT the
        // default `SelfRef` — this is the rebind under test.
        let TargetFilter::Typed(rf) = recipient else {
            panic!("recipient must be a typed group filter, got {recipient:?}");
        };
        assert!(
            rf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Horror")),
            "recipient must be the Horror subtype filter; got {rf:?}"
        );
        assert_eq!(
            rf.controller,
            Some(ControllerRef::You),
            "recipient must be controlled by you; got {rf:?}"
        );

        // Donor target must remain the opponent-controlled artifact filter.
        let TargetFilter::Typed(tf) = target else {
            panic!("donor target must be a typed artifact filter, got {target:?}");
        };
        assert!(
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Artifact)),
            "donor target must be an artifact; got {tf:?}"
        );
        assert_eq!(
            tf.controller,
            Some(ControllerRef::Opponent),
            "donor target must be controlled by an opponent; got {tf:?}"
        );

        assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
    }

    /// CR 602.1 + CR 603.1 + CR 611.2a: full real-Oracle-text parse-through of
    /// Symbiote Spider-Man's Find New Host. Its "It gains this card's other
    /// abilities" sub-clause must lower to `GainActivatedAbilitiesOfTarget` with
    /// `scope = AllOther`, `target = SelfRef` (donor = "this card"), `recipient =
    /// ParentTarget` ("It" = the +1/+1 target), and `duration = Permanent` (no
    /// duration stated) — and no residual `Unimplemented` "gain" clause.
    #[test]
    fn symbiote_find_new_host_lowers_all_other_grant() {
        const TEXT: &str = "Find New Host — {2}{U/B}, Exile this card from your graveyard: Put a +1/+1 counter on target creature you control. It gains this card's other abilities. Activate only as a sorcery.";
        let parsed = crate::parser::oracle::parse_oracle_text(
            TEXT,
            "Symbiote Spider-Man",
            &[],
            &["Creature".to_string()],
            &["Alien".to_string()],
        );

        fn find_gain(def: &AbilityDefinition) -> Option<&Effect> {
            let mut cur = Some(def);
            while let Some(d) = cur {
                if matches!(
                    d.effect.as_ref(),
                    Effect::GainActivatedAbilitiesOfTarget { .. }
                ) {
                    return Some(d.effect.as_ref());
                }
                cur = d.sub_ability.as_deref();
            }
            None
        }
        let gain = parsed
            .abilities
            .iter()
            .find_map(find_gain)
            .expect("Find New Host's grant must lower to GainActivatedAbilitiesOfTarget");
        let Effect::GainActivatedAbilitiesOfTarget {
            target,
            recipient,
            scope,
            duration,
        } = gain
        else {
            unreachable!()
        };
        assert_eq!(*target, TargetFilter::SelfRef, "donor = 'this card'");
        assert_eq!(*recipient, TargetFilter::ParentTarget, "recipient = 'It'");
        assert_eq!(*scope, GrantedAbilityScope::AllOther, "'OTHER abilities'");
        assert_eq!(
            *duration,
            Some(Duration::Permanent),
            "CR 611.2a: no duration stated → until end of game"
        );

        assert!(
            !parsed.abilities.iter().any(|a| {
                let mut cur = Some(a);
                while let Some(d) = cur {
                    if matches!(d.effect.as_ref(), Effect::Unimplemented { name, .. } if name == "gain")
                    {
                        return true;
                    }
                    cur = d.sub_ability.as_deref();
                }
                false
            }),
            "the 'gain' clause must no longer be Unimplemented"
        );
    }

    /// CR 122.1b: "put that many <keyword> counter(s) on <target>" must
    /// canonicalize multi-word keyword counter names ("double strike", "first
    /// strike") to `CounterType::Keyword(...)`. The previous
    /// `.find(whitespace)` slicing truncated at the first space, mapping
    /// "double strike" → `CounterType::Generic("double")`.
    #[test]
    fn that_many_counters_multi_word_keyword() {
        use crate::types::counter::CounterType;
        use crate::types::keywords::KeywordKind;
        let mut ctx = ParseContext::default();
        let effect =
            try_parse_that_many_counters("put that many double strike counters on it", &mut ctx)
                .expect("clause should parse");
        match effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(
                    counter_type,
                    CounterType::Keyword(KeywordKind::DoubleStrike),
                    "multi-word keyword counter must canonicalize to Keyword(DoubleStrike)"
                );
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount
                        }
                    ),
                    "'that many' must resolve to event-context amount, got {count:?}"
                );
            }
            other => panic!("expected Effect::PutCounter, got {other:?}"),
        }
    }

    // --- coalesce_pump_with_modifications (CR 611.2c / 702.10) ---

    /// Returns true if any static ability in a GenericEffect carries `keyword`.
    fn generic_has_keyword(effect: &Effect, keyword: crate::types::keywords::Keyword) -> bool {
        match effect {
            Effect::GenericEffect {
                static_abilities, ..
            } => static_abilities.iter().any(|s| {
                s.modifications.iter().any(|m| {
                    matches!(m, ContinuousModification::AddKeyword { keyword: k } if *k == keyword)
                })
            }),
            _ => false,
        }
    }

    /// pump + haste body retains all three mods (AddPower, AddToughness,
    /// AddKeyword(Haste)) as one GenericEffect with `target: None` and
    /// UntilEndOfTurn.
    #[test]
    fn coalesce_pump_haste_retains_all_mods() {
        let effect = coalesce_pump_with_modifications("get +2/+0 and gain haste until end of turn")
            .expect("pump+keyword body should coalesce");
        match &effect {
            Effect::GenericEffect {
                static_abilities,
                duration,
                target,
            } => {
                assert_eq!(*target, None, "non-distributed body must not broadcast");
                assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
                let mods = &static_abilities[0].modifications;
                assert!(mods
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::AddPower { value: 2 })));
                assert!(mods
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::AddToughness { value: 0 })));
                assert!(generic_has_keyword(
                    &effect,
                    crate::types::keywords::Keyword::Haste
                ));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// "lifelink" contains the substring "life", so the `gain` dispatch must
    /// route "gain lifelink" to the keyword-grant parser, not the numeric
    /// life-gain branch (which would fail and fall through to Unimplemented).
    /// Reaches cards like Blessing of Belzenlok ("…it also gains lifelink…").
    #[test]
    fn gain_lifelink_dispatches_to_keyword_grant_not_life() {
        let ast = parse_imperative_family_ast(
            "gain lifelink",
            "gain lifelink",
            &mut ParseContext::default(),
        )
        .expect("'gain lifelink' should parse");
        match ast {
            ImperativeFamilyAst::GainKeyword(effect) => assert!(
                generic_has_keyword(&effect, crate::types::keywords::Keyword::Lifelink),
                "expected a Lifelink keyword grant"
            ),
            _ => panic!("expected GainKeyword(Lifelink)"),
        }
    }

    /// CR 724.1: "end the turn" parses to the no-target `Effect::EndTheTurn`
    /// (Time Stop, Sundial of the Infinite, Obeka, Glorious End).
    #[test]
    fn end_the_turn_parses_to_end_the_turn_effect() {
        let ast = parse_imperative_family_ast(
            "end the turn",
            "end the turn",
            &mut ParseContext::default(),
        )
        .expect("'end the turn' should parse");
        assert!(
            matches!(ast, ImperativeFamilyAst::GainKeyword(Effect::EndTheTurn)),
            "expected Effect::EndTheTurn"
        );
    }

    /// CR 724.2: "end the combat phase" parses to the no-target
    /// `Effect::EndCombatPhase` (Mandate of Peace).
    #[test]
    fn end_the_combat_phase_parses_to_end_combat_phase_effect() {
        let ast = parse_imperative_family_ast(
            "end the combat phase",
            "end the combat phase",
            &mut ParseContext::default(),
        )
        .expect("'end the combat phase' should parse");
        assert!(
            matches!(
                ast,
                ImperativeFamilyAst::GainKeyword(Effect::EndCombatPhase)
            ),
            "expected Effect::EndCombatPhase"
        );
    }

    /// Regression: a genuine life-gain clause must still reach the numeric
    /// life-gain parser, not be captured by the keyword-grant branch.
    #[test]
    fn gain_life_still_dispatches_to_numeric_life_gain() {
        let ast =
            parse_imperative_family_ast("gain 3 life", "gain 3 life", &mut ParseContext::default())
                .expect("'gain 3 life' should parse");
        assert!(
            matches!(
                ast,
                ImperativeFamilyAst::Structured(ImperativeAst::Numeric(_))
            ),
            "expected numeric life-gain dispatch"
        );
    }

    /// Production-path regression for the same lifelink/life substring
    /// ambiguity, including default duration and SelfRef scope.
    #[test]
    fn effect_gain_lifelink_dispatches_to_keyword_grant() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "gain lifelink until end of turn",
            AbilityKind::Spell,
        );
        let Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } = &*def.effect
        else {
            panic!(
                "expected GenericEffect for lifelink grant, got {:?}",
                def.effect
            );
        };
        assert_eq!(duration.as_ref(), Some(&Duration::UntilEndOfTurn));
        assert_eq!(
            static_abilities[0].affected,
            Some(TargetFilter::SelfRef),
            "bare creature-scoped keyword grant must remain SelfRef"
        );
        assert!(
            static_abilities[0].modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: crate::types::keywords::Keyword::Lifelink
                }
            )),
            "expected AddKeyword(Lifelink), got {:?}",
            static_abilities[0].modifications
        );
    }

    /// Pure pump body → None (falls through to the bare numeric Pump arm).
    #[test]
    fn coalesce_pure_pump_is_none() {
        assert!(coalesce_pump_with_modifications("get +2/+0").is_none());
    }

    /// Keyword-only body → None (handled by `try_parse_gain_keyword`).
    #[test]
    fn coalesce_keyword_only_is_none() {
        assert!(coalesce_pump_with_modifications("gain haste until end of turn").is_none());
    }

    /// No explicit duration on a pump+keyword body defaults to UntilEndOfTurn.
    #[test]
    fn coalesce_pump_keyword_defaults_until_end_of_turn() {
        let effect = coalesce_pump_with_modifications("get +1/+0 and gain flying")
            .expect("pump+keyword body should coalesce");
        match effect {
            Effect::GenericEffect { duration, .. } => {
                assert_eq!(duration, Some(Duration::UntilEndOfTurn));
            }
            other => panic!("expected GenericEffect, got {other:?}"),
        }
    }

    /// Multi-keyword body keeps both granted keywords.
    #[test]
    fn coalesce_pump_multi_keyword() {
        let effect = coalesce_pump_with_modifications("get +1/+0 and gain flying and haste")
            .expect("pump+multi-keyword body should coalesce");
        assert!(generic_has_keyword(
            &effect,
            crate::types::keywords::Keyword::Haste
        ));
        assert!(generic_has_keyword(
            &effect,
            crate::types::keywords::Keyword::Flying
        ));
    }

    /// Integration: a subjectless pump+keyword body routed through
    /// `parse_imperative_family_ast` yields `GainKeyword(GenericEffect)`.
    #[test]
    fn imperative_family_pump_keyword_is_gain_keyword() {
        let text = "get +2/+0 and gain haste until end of turn";
        let mut ctx = ParseContext::default();
        let ast = parse_imperative_family_ast(text, &text.to_lowercase(), &mut ctx)
            .expect("pump+keyword body should parse");
        match ast {
            ImperativeFamilyAst::GainKeyword(effect) => {
                assert!(matches!(effect, Effect::GenericEffect { .. }));
            }
            other => panic!("expected GainKeyword(GenericEffect), got {other:?}"),
        }
    }

    /// Regression: a pure pump body still routes through the numeric Pump arm.
    #[test]
    fn imperative_family_pure_pump_still_numeric() {
        let text = "get +2/+0";
        let mut ctx = ParseContext::default();
        let ast = parse_imperative_family_ast(text, &text.to_lowercase(), &mut ctx)
            .expect("pump body should parse");
        assert!(
            matches!(
                ast,
                ImperativeFamilyAst::Structured(ImperativeAst::Numeric(_))
            ),
            "pure pump must remain a numeric imperative, got {ast:?}"
        );
    }

    /// CR 706 + CR 706.2: "Roll a d20" with no modifier parses to a bare RollDie.
    /// Issue #1675 — Canopy Gargantuan. "Put a number of +1/+1 counters on each
    /// other creature you control equal to that creature's toughness." rebinds
    /// the per-recipient "that creature's toughness" count to `Recipient` so the
    /// resolver re-evaluates it per object.
    #[test]
    fn put_counter_all_each_equal_to_that_creatures_toughness_rebinds_to_recipient() {
        use crate::types::ability::ObjectScope;
        let def = super::super::parse_effect_chain(
            "Put a number of +1/+1 counters on each other creature you control equal to that creature's toughness.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::PutCounterAll { count, .. } => assert!(
                matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Toughness {
                            scope: ObjectScope::Recipient
                        }
                    }
                ),
                "count must rebind to Recipient, got {count:?}"
            ),
            other => panic!("expected PutCounterAll, got {other:?}"),
        }
    }

    /// The rebind is gated: a genuine cost referent ("the sacrificed creature's
    /// power") stays `CostPaidObject` (resolved once, applied uniformly), so the
    /// rebind does not hijack cost-paid `*All` counts.
    #[test]
    fn rebind_distributive_recipient_count_leaves_cost_referent_alone() {
        use crate::types::ability::ObjectScope;
        let cost_paid = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
        };
        let out = super::rebind_distributive_recipient_count(
            cost_paid.clone(),
            "put a +1/+1 counter on each creature you control equal to the sacrificed creature's power",
        );
        assert_eq!(
            out, cost_paid,
            "cost-referent count must NOT rebind to Recipient"
        );

        // And "that creature's power" (no cost participle) DOES rebind.
        let anaphor = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: ObjectScope::CostPaidObject,
            },
        };
        let rebound = super::rebind_distributive_recipient_count(
            anaphor,
            "put a +1/+1 counter on each creature you control equal to that creature's power",
        );
        assert!(matches!(
            rebound,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient
                }
            }
        ));
    }

    #[test]
    fn rebind_distributive_recipient_count_walks_all_quantity_wrappers() {
        use crate::types::ability::ObjectScope;

        let expr = QuantityExpr::Difference {
            left: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
            right: Box::new(QuantityExpr::Power {
                base: 2,
                exponent: Box::new(QuantityExpr::UpTo {
                    max: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::ObjectManaValue {
                            scope: ObjectScope::CostPaidObject,
                        },
                    }),
                }),
            }),
        };

        let rebound = super::rebind_distributive_recipient_count(
            expr,
            "put a number of +1/+1 counters on each creature you control equal to that creature's power",
        );

        let QuantityExpr::Difference { left, right } = rebound else {
            panic!("expected Difference, got {rebound:?}");
        };
        assert!(matches!(
            *left,
            QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Recipient
                }
            }
        ));

        let QuantityExpr::Power { exponent, .. } = *right else {
            panic!("expected Power, got {right:?}");
        };
        let QuantityExpr::UpTo { max } = *exponent else {
            panic!("expected UpTo, got {exponent:?}");
        };
        assert!(matches!(
            *max,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Recipient
                }
            }
        ));
    }

    #[test]
    fn roll_a_d20_no_modifier_parses_to_roll_die() {
        let def = super::super::parse_effect_chain("Roll a d20.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie {
                count,
                sides,
                modifier,
                results,
            } => {
                assert_eq!(*sides, 20);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 1 }
                );
                assert!(modifier.is_none());
                assert!(results.is_empty());
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: "Roll a d20 and add the number of cards in your hand" parses
    /// to RollDie with an Add modifier referencing controller hand-zone count.
    #[test]
    fn roll_a_d20_with_add_modifier_parses_to_roll_die_with_modifier() {
        let def = super::super::parse_effect_chain(
            "Roll a d20 and add the number of cards in your hand.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::RollDie {
                sides, modifier, ..
            } => {
                assert_eq!(*sides, 20);
                let m = modifier.as_ref().expect("expected Add modifier");
                match m {
                    crate::types::ability::DieRollModifier::Add { value } => match value {
                        QuantityExpr::Ref {
                            qty:
                                QuantityRef::ZoneCardCount {
                                    zone,
                                    card_types,
                                    filter,
                                    scope,
                                },
                        } => {
                            assert!(matches!(zone, crate::types::ability::ZoneRef::Hand));
                            assert!(card_types.is_empty());
                            assert!(filter.is_none());
                            assert!(matches!(
                                scope,
                                crate::types::ability::CountScope::Controller
                            ));
                        }
                        other => {
                            panic!("expected controller hand ZoneCardCount ref, got {other:?}")
                        }
                    },
                    other => panic!("expected Add modifier, got {other:?}"),
                }
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    #[test]
    fn roll_a_d20_with_add_modifier_rejects_partial_quantity_tail() {
        let def = super::super::parse_effect_chain(
            "Roll a d20 and add the number of cards in your hand plus one.",
            AbilityKind::Spell,
        );
        assert!(
            !matches!(&*def.effect, Effect::RollDie { .. }),
            "partial quantity tail must not silently parse as RollDie, got {:?}",
            def.effect
        );
        assert!(
            matches!(&*def.effect, Effect::Unimplemented { .. }),
            "partial quantity tail should stay visible as Unimplemented, got {:?}",
            def.effect
        );
    }

    /// CR 706.1a + CR 706.2: The single-die parser accepts word-count "one"
    /// as the same semantic count as article "a", including modifier clauses.
    #[test]
    fn roll_one_d20_with_add_modifier_parses_to_roll_die_with_modifier() {
        let def = super::super::parse_effect_chain(
            "Roll one d20 and add the number of cards in your hand.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::RollDie {
                count,
                sides,
                modifier,
                ..
            } => {
                assert_eq!(*sides, 20);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 1 }
                );
                assert!(matches!(
                    modifier,
                    Some(crate::types::ability::DieRollModifier::Add { .. })
                ));
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: "Roll a d20 and subtract the number of cards in your hand"
    /// parses to RollDie with a Subtract modifier. Mirrors the Add case to
    /// guarantee the sign path is wired.
    #[test]
    fn roll_a_d20_with_subtract_modifier_parses_to_roll_die_with_modifier() {
        let def = super::super::parse_effect_chain(
            "Roll a d20 and subtract the number of cards in your hand.",
            AbilityKind::Activated,
        );
        match &*def.effect {
            Effect::RollDie {
                sides, modifier, ..
            } => {
                assert_eq!(*sides, 20);
                let m = modifier.as_ref().expect("expected Subtract modifier");
                assert!(matches!(
                    m,
                    crate::types::ability::DieRollModifier::Subtract { .. }
                ));
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: "Roll a six-sided die" is the word-form variant — parses to
    /// RollDie { sides: 6 } so cards printed in the older "N-sided die"
    /// phrasing continue to work alongside the modern "dN" shorthand.
    #[test]
    fn roll_a_six_sided_die_parses_to_roll_die_six() {
        let def = super::super::parse_effect_chain("Roll a six-sided die.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie {
                sides, modifier, ..
            } => {
                assert_eq!(*sides, 6);
                assert!(modifier.is_none());
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1: "Roll two six-sided dice" is the multi-dice word form —
    /// parses to `RollDie { count: Fixed(2), sides: 6 }`. Mirrors the
    /// `FlipCoins { count }` precedent for the dice axis.
    #[test]
    fn roll_two_six_sided_dice_parses_to_roll_die_count_two() {
        let def = super::super::parse_effect_chain("Roll two six-sided dice.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie {
                count,
                sides,
                modifier,
                ..
            } => {
                assert_eq!(*sides, 6);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 2 }
                );
                assert!(modifier.is_none());
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1: The single-die path is unchanged — "Roll a six-sided die"
    /// still lowers to `count: Fixed(1)` so back-compat with existing
    /// single-die cards holds.
    #[test]
    fn roll_a_six_sided_die_lowers_to_count_one() {
        let def = super::super::parse_effect_chain("Roll a six-sided die.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie { count, sides, .. } => {
                assert_eq!(*sides, 6);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 1 }
                );
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1: The `dN` shorthand also supports a leading count: "Roll two
    /// d6" → `count: Fixed(2), sides: 6`.
    #[test]
    fn roll_two_d6_parses_to_roll_die_count_two() {
        let def = super::super::parse_effect_chain("Roll two d6.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie { count, sides, .. } => {
                assert_eq!(*sides, 6);
                assert_eq!(
                    *count,
                    crate::types::ability::QuantityExpr::Fixed { value: 2 }
                );
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.1 + CR 107.3a: "Roll X twelve-sided dice" binds the count to the
    /// announced `X`, lowering to `count: Variable("X"), sides: 12`.
    #[test]
    fn roll_x_twelve_sided_dice_parses_to_variable_count() {
        let def = super::super::parse_effect_chain("Roll X twelve-sided dice.", AbilityKind::Spell);
        match &*def.effect {
            Effect::RollDie { count, sides, .. } => {
                assert_eq!(*sides, 12);
                assert!(matches!(
                    count,
                    crate::types::ability::QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Variable { .. }
                    }
                ));
            }
            other => panic!("expected RollDie, got {other:?}"),
        }
    }

    /// CR 706.2: open-ended upper bound "N+" parses to (min=N, max=u8::MAX) so
    /// modifier-boosted rolls beyond the printed face count still resolve to
    /// the intended branch.
    #[test]
    fn die_result_line_open_ended_upper_bound() {
        assert_eq!(
            super::try_parse_die_result_line("15+ | Draw two cards."),
            Some((15, u8::MAX, "Draw two cards."))
        );
        assert_eq!(
            super::try_parse_die_result_line("1\u{2014}9 | Draw a card."),
            Some((1, 9, "Draw a card."))
        );
        assert_eq!(
            super::try_parse_die_result_line("20 | Win the game."),
            Some((20, 20, "Win the game."))
        );
    }

    /// CR 701.13 + CR 701.24: A suffix-less "exile the top card[s]" (no "of
    /// <player>'s library" qualifier) is an implicit-controller, deterministic
    /// top-of-library exile — the "shuffle ..., then exile the top card" class
    /// (Urza, Lord High Artificer's {5}). It must NOT fall through to the
    /// generic `tag("exile ")` path (which produces a library-wide tutor).
    /// Covers period-terminated, plural-count, and EOF-terminated forms.
    #[test]
    fn exile_the_top_card_no_qualifier_parses_controller_exile_top() {
        let mut ctx = ParseContext::default();

        let singular = parse_exile_ast("exile the top card.", "exile the top card.", &mut ctx)
            .expect("'exile the top card.' should parse as ExileTop");
        assert!(
            matches!(
                singular,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                    face_down: false,
                }
            ),
            "expected ExileTop(Controller, 1), got {singular:?}"
        );

        let plural = parse_exile_ast(
            "exile the top two cards.",
            "exile the top two cards.",
            &mut ctx,
        )
        .expect("'exile the top two cards.' should parse as ExileTop");
        assert!(
            matches!(
                plural,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 2 },
                    face_down: false,
                }
            ),
            "expected ExileTop(Controller, 2), got {plural:?}"
        );

        let eof = parse_exile_ast("exile the top card", "exile the top card", &mut ctx)
            .expect("'exile the top card' (no period) should parse as ExileTop");
        assert!(
            matches!(
                eof,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Controller,
                    count: QuantityExpr::Fixed { value: 1 },
                    face_down: false,
                }
            ),
            "expected ExileTop(Controller, 1) at EOF, got {eof:?}"
        );
    }

    /// Regression: the suffix-less branch must NOT shadow the qualified "of
    /// <player>'s library" patterns, which run first and take precedence.
    /// "of target opponent's library" → opponent filter; "of each player's
    /// library" → ScopedPlayer. Both must survive the new fallback.
    #[test]
    fn exile_the_top_qualified_library_patterns_take_precedence() {
        let mut ctx = ParseContext::default();

        let opponent = parse_exile_ast(
            "exile the top card of target opponent's library.",
            "exile the top card of target opponent's library.",
            &mut ctx,
        )
        .expect("qualified opponent-library phrase should parse");
        assert!(
            matches!(
                opponent,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::Typed(_),
                    ..
                }
            ),
            "expected opponent-typed ExileTop, got {opponent:?}"
        );

        let each = parse_exile_ast(
            "exile the top card of each player's library.",
            "exile the top card of each player's library.",
            &mut ctx,
        )
        .expect("qualified each-player-library phrase should parse");
        assert!(
            matches!(
                each,
                ZoneCounterImperativeAst::ExileTop {
                    player: TargetFilter::ScopedPlayer,
                    ..
                }
            ),
            "expected ScopedPlayer ExileTop, got {each:?}"
        );
    }

    /// CR 609.7 + CR 615.2: A source-scoped prevent ("prevent all damage target
    /// instant or sorcery spell would deal this turn" — Dromoka's Command)
    /// collapses the recipient `target` to `Any` and carries the chosen source
    /// as `damage_source_filter = And[ParentTargetSlot{0}, <stack-spell leaf>]`.
    #[test]
    fn prevent_source_scoped_spell_yields_parent_target_slot_filter() {
        let effect = parse_prevent_effect(
            "Prevent all damage target instant or sorcery spell would deal this turn.",
            false,
        );
        let Effect::PreventDamage {
            amount,
            target,
            scope,
            damage_source_filter,
            ..
        } = effect
        else {
            panic!("expected PreventDamage, got {effect:?}");
        };
        assert_eq!(amount, PreventionAmount::All);
        assert_eq!(target, TargetFilter::Any);
        assert_eq!(scope, PreventionScope::AllDamage);
        let Some(TargetFilter::And { filters }) = damage_source_filter else {
            panic!("expected And source filter, got {damage_source_filter:?}");
        };
        // First leg: the cast-time-chosen-source sentinel at slot 0.
        assert!(
            filters
                .iter()
                .any(|f| matches!(f, TargetFilter::ParentTargetSlot { index: 0 })),
            "expected ParentTargetSlot {{ index: 0 }} leg, got {filters:?}"
        );
        // Sibling leg: the choosable instant-or-sorcery stack-spell filter.
        let source_leaf = filters
            .iter()
            .find(|f| !matches!(f, TargetFilter::ParentTargetSlot { .. }))
            .expect("source leaf present");
        assert!(
            is_stack_spell_leg(source_leaf)
                || matches!(source_leaf, TargetFilter::Or { filters } if filters.iter().any(is_stack_spell_leg)),
            "source leaf must scope to a stack spell, got {source_leaf:?}"
        );
    }

    /// NEGATIVE: a recipient-scoped prevent ("prevent the next 3 damage to
    /// target creature") must stay recipient-targeted — `target: Typed(creature)`
    /// and `damage_source_filter: None` — proving the source-scope branch does
    /// not divert the recipient form.
    #[test]
    fn prevent_recipient_scoped_creature_not_diverted_to_source() {
        let effect = parse_prevent_effect(
            "Prevent the next 3 damage that would be dealt to target creature this turn.",
            false,
        );
        let Effect::PreventDamage {
            amount,
            target,
            damage_source_filter,
            ..
        } = effect
        else {
            panic!("expected PreventDamage, got {effect:?}");
        };
        assert_eq!(amount, PreventionAmount::Next(3));
        assert!(
            typed_leg(&target).is_some_and(|tf| has_type(tf, TypeFilter::Creature)),
            "recipient target must stay Typed(creature), got {target:?}"
        );
        assert_eq!(
            damage_source_filter, None,
            "recipient prevent must not carry a source filter"
        );
    }

    /// CR 615.2 + CR 109.4: Haze Frog — "prevent all combat damage that other
    /// creatures would deal this turn" must scope the shield to non-self creature
    /// sources via `FilterProp::Another`, not a blanket prevent-all.
    #[test]
    fn prevent_that_other_creatures_would_deal_scopes_source_filter() {
        let effect = parse_prevent_effect(
            "Prevent all combat damage that other creatures would deal this turn.",
            false,
        );
        let Effect::PreventDamage {
            amount,
            target,
            scope,
            damage_source_filter,
            prevention_duration,
            ..
        } = effect
        else {
            panic!("expected PreventDamage, got {effect:?}");
        };
        assert_eq!(amount, PreventionAmount::All);
        assert_eq!(target, TargetFilter::Any);
        assert_eq!(scope, PreventionScope::CombatDamage);
        assert_eq!(prevention_duration, Some(Duration::UntilEndOfTurn));
        let damage_source_filter =
            damage_source_filter.expect("expected Typed creature source filter");
        let TargetFilter::Typed(tf) = damage_source_filter else {
            panic!("expected Typed source filter, got {damage_source_filter:?}");
        };
        assert!(has_type(&tf, TypeFilter::Creature));
        assert!(
            tf.properties.contains(&FilterProp::Another),
            "other creatures must carry FilterProp::Another, got {:?}",
            tf.properties
        );
    }

    /// CR 511.2 + CR 615 (issue #2924, Bug B): the trailing duration window on a
    /// prevent clause is captured into `prevention_duration`. "this combat" ->
    /// `UntilEndOfCombat` (Suppressor Skyguard — must NOT bleed into a later
    /// combat the same turn), "this turn" -> `UntilEndOfTurn`, and no stated
    /// window -> `None` (legacy end-of-turn `is_shield` prune).
    #[test]
    fn prevent_clause_captures_trailing_duration_window() {
        let cases = [
            (
                "Prevent all combat damage that would be dealt to you this combat.",
                Some(Duration::UntilEndOfCombat),
            ),
            (
                "Prevent all combat damage that would be dealt to you this turn.",
                Some(Duration::UntilEndOfTurn),
            ),
            (
                "Prevent all combat damage that would be dealt to you.",
                None,
            ),
        ];
        for (text, expected) in cases {
            let effect = parse_prevent_effect(text, false);
            let Effect::PreventDamage {
                prevention_duration,
                ..
            } = effect
            else {
                panic!("expected PreventDamage, got {effect:?}");
            };
            assert_eq!(
                prevention_duration, expected,
                "wrong prevention_duration for {text:?}"
            );
        }
    }

    /// CR 615 + CR 614.1a: "to you and planeswalkers you control" lowers to the
    /// compound `ControllerAndControlledPermanents { Planeswalker }` recipient,
    /// NOT a bare `Controller` that would silently drop the planeswalker leg
    /// (Comeuppance). "you and permanents you control" carries no type
    /// restriction (Channel Harm).
    #[test]
    fn prevent_compound_recipient_you_and_controlled_permanents() {
        use crate::types::card_type::CoreType;

        let planeswalker_text =
            "Prevent all damage that would be dealt to you and planeswalkers you control this turn.";
        let Effect::PreventDamage { target, .. } = parse_prevent_effect(planeswalker_text, false)
        else {
            panic!("expected PreventDamage");
        };
        assert_eq!(
            target,
            TargetFilter::ControllerAndControlledPermanents {
                permanent_type: Some(CoreType::Planeswalker)
            }
        );

        let permanents_text =
            "Prevent all damage that would be dealt to you and permanents you control this turn.";
        let Effect::PreventDamage { target, .. } = parse_prevent_effect(permanents_text, false)
        else {
            panic!("expected PreventDamage");
        };
        assert_eq!(
            target,
            TargetFilter::ControllerAndControlledPermanents {
                permanent_type: None
            }
        );
    }

    /// CR 615: the plain "to you" recipient must remain `Controller` — the
    /// compound-recipient recognizer must not over-trigger on the bare form.
    #[test]
    fn prevent_plain_to_you_recipient_stays_controller() {
        let text = "Prevent all damage that would be dealt to you this turn.";
        let Effect::PreventDamage { target, .. } = parse_prevent_effect(text, false) else {
            panic!("expected PreventDamage");
        };
        assert_eq!(target, TargetFilter::Controller);
    }

    /// CR 609.7b: "by sources you don't control" lowers to a
    /// controller-axis source filter (`ControllerRef::Opponent`) via the shared
    /// replacement-side source grammar — a self source (you control) is NOT
    /// matched (Comeuppance / Channel Harm).
    #[test]
    fn prevent_by_sources_you_dont_control_source_filter() {
        use crate::types::ability::ControllerRef;

        let text = "Prevent all damage that would be dealt to you and planeswalkers you control \
                    this turn by sources you don't control.";
        let Effect::PreventDamage {
            damage_source_filter,
            ..
        } = parse_prevent_effect(text, false)
        else {
            panic!("expected PreventDamage");
        };
        let Some(TargetFilter::Typed(tf)) = damage_source_filter else {
            panic!("expected Typed source filter, got {damage_source_filter:?}");
        };
        assert_eq!(tf.controller, Some(ControllerRef::Opponent));
        assert!(
            tf.type_filters.is_empty(),
            "\"sources\" carries no type restriction"
        );
    }

    /// CR 119.3 + CR 608.2c: Kaya's Wrath lifegain (issue #2943) must parse
    /// through the imperative GainLife path with a FilteredTrackedSetSize
    /// amount, not fall through to Unimplemented.
    #[test]
    fn gain_life_equal_to_destroyed_creatures_you_controlled_this_way() {
        use crate::types::ability::{ControllerRef, QuantityExpr, QuantityRef};

        let text = "You gain life equal to the number of creatures you controlled that were \
                    destroyed this way.";
        let lower = text.to_ascii_lowercase();
        let ast = parse_numeric_imperative_ast(text, &lower)
            .expect("Kaya's Wrath lifegain clause must parse");
        match ast {
            NumericImperativeAst::GainLife { amount } => match amount {
                QuantityExpr::Ref {
                    qty: QuantityRef::FilteredTrackedSetSize { filter, .. },
                } => {
                    let tf = typed_leg(&filter).expect("filter must be Typed");
                    assert!(has_type(tf, TypeFilter::Creature));
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
                other => panic!("expected FilteredTrackedSetSize, got {other:?}"),
            },
            other => panic!("expected GainLife imperative, got {other:?}"),
        }
    }

    // CR 121.1 + CR 107.1: a trailing "for each …" multiplier must scale the draw
    // count off Fixed(1). Cluster-01 (Surge of Brilliance class).
    #[test]
    fn draw_for_each_attaches_multiplier() {
        let text = "draw a card for each creature you control";
        match parse_numeric_imperative_ast(text, text) {
            Some(NumericImperativeAst::Draw { count, .. }) => {
                assert_ne!(
                    count,
                    QuantityExpr::Fixed { value: 1 },
                    "the for-each multiplier must replace the Fixed(1) base count"
                );
            }
            other => panic!("expected Draw imperative, got {other:?}"),
        }
    }

    #[test]
    fn draw_for_each_spell_cast_origin() {
        let text =
            "draw a card for each spell you've cast this turn from anywhere other than your hand";
        match parse_numeric_imperative_ast(text, text) {
            Some(NumericImperativeAst::Draw { count, .. }) => {
                // Must resolve to the SpellsCastThisTurn ref carrying the cast-origin
                // filter, not a Fixed(1) draw.
                match count {
                    QuantityExpr::Ref {
                        qty: QuantityRef::SpellsCastThisTurn { filter, .. },
                    } => {
                        let filter = filter.expect("cast-origin filter must be present");
                        assert!(
                            matches!(&filter, TargetFilter::Typed(t)
                                if t.properties.iter().any(|p| matches!(p, FilterProp::InAnyZone { .. }))),
                            "draw count must carry the cast-origin InAnyZone filter, got {filter:?}"
                        );
                    }
                    other => panic!("expected SpellsCastThisTurn ref, got {other:?}"),
                }
            }
            other => panic!("expected Draw imperative, got {other:?}"),
        }
    }

    #[test]
    fn draw_rejects_unsplit_proliferate_tail() {
        let text = "draw a card and proliferate";
        assert!(
            parse_numeric_imperative_ast(text, text).is_none(),
            "draw parser must not swallow an unrelated proliferate tail"
        );
    }

    #[test]
    fn draw_accepts_supported_followup_tails() {
        for text in [
            "draw a card and target opponent may draw a card",
            "draw a card and that opponent discards a card",
            "draw a card and blight 1",
            "draw a card and repeat this process",
            "draw two cards and target opponent discards two cards",
            "draw x cards and the chosen creatures get +X/+X and gain trample until end of turn, where X is the difference between the chosen creatures' powers",
            "draw x cards, where X is the number of permanent types among cards in your graveyard",
        ] {
            assert!(
                parse_numeric_imperative_ast(text, &text.to_ascii_lowercase()).is_some(),
                "draw parser must keep the supported draw head for: {text}"
            );
        }
    }

    #[test]
    fn draw_rejects_unsupported_where_x_tail() {
        let text = "draw x cards, where x is the mystery total";
        assert!(
            parse_numeric_imperative_ast(text, text).is_none(),
            "draw parser must not accept unsupported where-X quantity tails"
        );
    }

    #[test]
    fn draw_plain_card_stays_fixed_one() {
        let text = "draw a card";
        match parse_numeric_imperative_ast(text, text) {
            Some(NumericImperativeAst::Draw { count, .. }) => {
                assert_eq!(count, QuantityExpr::Fixed { value: 1 });
            }
            other => panic!("expected Draw imperative, got {other:?}"),
        }
    }

    // CR 107.1: `replace_fixed_quantity` must never silently discard a non-Fixed
    // base when a for-each multiplier attaches. There is no product-of-two-dynamic
    // `QuantityExpr` variant, so a dynamic base is preserved unchanged rather than
    // replaced by the bare for-each (which would lose the parsed base count).
    #[test]
    fn replace_fixed_quantity_preserves_dynamic_base() {
        let for_each = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Any,
            },
        };
        // Fixed(1) → for_each (direct substitution).
        assert_eq!(
            replace_fixed_quantity(QuantityExpr::Fixed { value: 1 }, for_each.clone()),
            for_each,
        );
        // Fixed(0) → Fixed(0) (zero effect regardless of multiplier).
        assert_eq!(
            replace_fixed_quantity(QuantityExpr::Fixed { value: 0 }, for_each.clone()),
            QuantityExpr::Fixed { value: 0 },
        );
        // Fixed(N>1) → Multiply { factor: N, inner: for_each }.
        assert_eq!(
            replace_fixed_quantity(QuantityExpr::Fixed { value: 3 }, for_each.clone()),
            QuantityExpr::Multiply {
                factor: 3,
                inner: Box::new(for_each.clone()),
            },
        );
        // Dynamic base (e.g. "that many" → EventContextAmount): kept unchanged,
        // NOT dropped in favor of the bare for-each.
        let dynamic_base = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(
            replace_fixed_quantity(dynamic_base.clone(), for_each),
            dynamic_base,
        );
    }

    /// CR 705.1 + CR 705.2: Subject-prefixed coin flips lower to `FlipCoin` AND
    /// carry the subject as the typed `flipper` so the right player flips and
    /// wins/loses (CR 705.2: "only the player who flips wins or loses the flip").
    ///   - "you flip a coin" → `flipper = Controller` (the default).
    ///   - "that player flips a coin" → the anaphoric parent-target controller
    ///     (a SpellCast trigger resolves this to `TriggeringPlayer`; see
    ///     `flip_coin.rs` runtime tests).
    ///   - "its controller flips a coin" → likewise the parent-target controller.
    ///   - "each player flips a coin" → `flipper = Controller` but the whole
    ///     ability is tagged `player_scope = All`, so the resolver flips once for
    ///     EACH player (CR 101.4 APNAP) rather than collapsing to one flip.
    #[test]
    fn subject_prefixed_flip_a_coin_binds_flipper() {
        // "you flip a coin" — controller flipper, no player_scope.
        let mut ctx = ParseContext::default();
        let you = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "you flip a coin",
            AbilityKind::Spell,
            &mut ctx,
        );
        assert!(
            matches!(
                &*you.effect,
                Effect::FlipCoin {
                    flipper: TargetFilter::Controller,
                    ..
                }
            ),
            "expected FlipCoin {{ flipper: Controller }}, got {:?}",
            you.effect
        );
        assert_eq!(you.player_scope, None);

        // "that player" / "its controller" → parent-target controller anaphor.
        for text in ["that player flips a coin", "its controller flips a coin"] {
            let mut ctx = ParseContext::default();
            let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
                text,
                AbilityKind::Spell,
                &mut ctx,
            );
            assert!(
                matches!(
                    &*ability.effect,
                    Effect::FlipCoin {
                        flipper: TargetFilter::ParentTargetController,
                        ..
                    }
                ),
                "expected FlipCoin {{ flipper: ParentTargetController }} for {text:?}, got {:?}",
                ability.effect
            );
        }

        // "each player flips a coin" → controller flipper iterated over ALL
        // players via player_scope, NOT a single controller flip.
        let mut ctx = ParseContext::default();
        let each = crate::parser::oracle_effect::parse_effect_chain_with_context(
            "each player flips a coin",
            AbilityKind::Spell,
            &mut ctx,
        );
        assert!(
            matches!(
                &*each.effect,
                Effect::FlipCoin {
                    flipper: TargetFilter::Controller,
                    ..
                }
            ),
            "expected FlipCoin {{ flipper: Controller }} for each-player, got {:?}",
            each.effect
        );
        assert_eq!(
            each.player_scope,
            Some(crate::types::ability::PlayerFilter::All),
            "each player must iterate ALL players, not collapse to one flip"
        );
    }

    /// CR 705.1 + CR 705.2: End-to-end — a trigger whose effect is "that player
    /// flips a coin" (Mirrored Depths, Planar Chaos) must emit a `FlipCoin` whose
    /// `flipper` is the TRIGGERING player, not the source's controller. This is
    /// the heart of the maintainer's CHANGES_REQUESTED: a non-controller who casts
    /// must be the one who flips and wins/loses (the runtime consequence is
    /// asserted in `flip_coin.rs`).
    #[test]
    fn trigger_subject_flip_a_coin_binds_triggering_player_flipper() {
        fn flip_coin_flipper(def: &AbilityDefinition) -> Option<&TargetFilter> {
            match &*def.effect {
                Effect::FlipCoin { flipper, .. } | Effect::FlipCoins { flipper, .. } => {
                    Some(flipper)
                }
                _ => def.sub_ability.as_ref().and_then(|s| flip_coin_flipper(s)),
            }
        }
        for (text, name) in [
            (
                "Whenever a player casts a spell, that player flips a coin. If the player loses the flip, counter that spell.",
                "Mirrored Depths",
            ),
            (
                "Whenever a player casts a spell, that player flips a coin. If the flip comes up tails, counter that spell.",
                "Planar Chaos",
            ),
        ] {
            let parsed = crate::parser::oracle::parse_oracle_text(text, name, &[], &[], &[]);
            let flipper = parsed
                .triggers
                .iter()
                .find_map(|t| t.execute.as_ref().and_then(|e| flip_coin_flipper(e)))
                .unwrap_or_else(|| panic!("{name}: trigger must lower to FlipCoin, got:\n{parsed:#?}"));
            assert_eq!(
                *flipper,
                TargetFilter::TriggeringPlayer,
                "{name}: the casting (triggering) player must be the flipper, not the controller"
            );
        }
    }

    /// CR 710.4 vs CR 705.1: The Kamigawa "flip <permanent>" flip-card mechanic
    /// ("flip ~" / "flip it") must NOT be mis-routed to the coin-flip `FlipCoin`
    /// effect — the flip arm only matches when "a coin" is present. Adding "flip"
    /// to `PREDICATE_VERBS` (so subject-prefixed coin flips strip) must not
    /// regress this: a bare object-form "flip" never produces FlipCoin.
    #[test]
    fn flip_permanent_transform_is_not_coin_flip() {
        for text in ["flip ~", "flip it"] {
            let mut ctx = ParseContext::default();
            let ability = crate::parser::oracle_effect::parse_effect_chain_with_context(
                text,
                AbilityKind::Spell,
                &mut ctx,
            );
            assert!(
                !matches!(&*ability.effect, Effect::FlipCoin { .. }),
                "object-form flip {text:?} must not route to FlipCoin, got {:?}",
                ability.effect
            );
        }
    }

    /// CR 701.31c: the bare "planeswalk" verb dispatches to the
    /// `Planeswalk` imperative-family leaf (the "you may " / "then " prefix and
    /// the optional flag are stripped upstream, so the family parser only ever
    /// sees the bare verb body).
    #[test]
    fn planeswalk_verb_dispatches_to_planeswalk_leaf() {
        // The family parser sees the bare verb body (subject + optional-prefix +
        // trailing-period normalization happen upstream — see the end-to-end
        // `parse_effect_chain` tests below for the "you may planeswalk." /
        // "then planeswalk." forms). Both singular and plural verb tokens map to
        // the same leaf.
        for input in ["planeswalk", "planeswalks"] {
            let ast = parse_imperative_family_ast(input, input, &mut ParseContext::default())
                .unwrap_or_else(|| panic!("'{input}' should parse to a Planeswalk leaf"));
            assert!(
                matches!(ast, ImperativeFamilyAst::Planeswalk),
                "expected Planeswalk leaf for '{input}', got {ast:?}"
            );
        }
    }

    /// CR 701.31c: "you may planeswalk" → optional `Effect::Planeswalk`
    /// (TARDIS, Start the TARDIS rider). The optional shell is produced by the
    /// upstream optional-prefix strip.
    #[test]
    fn effect_you_may_planeswalk_is_optional_planeswalk() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "You may planeswalk.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::Planeswalk),
            "expected Effect::Planeswalk, got {:?}",
            def.effect
        );
        assert!(
            def.optional,
            "expected optional: true for 'you may planeswalk'"
        );
    }

    /// CR 701.31c: "Then planeswalk." → mandatory `Effect::Planeswalk`
    /// (TARDIS Bay). The mandatory form must parse to the same effect with
    /// `optional: false`.
    #[test]
    fn effect_then_planeswalk_is_mandatory_planeswalk() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "Then planeswalk.",
            AbilityKind::Spell,
        );
        assert!(
            matches!(*def.effect, Effect::Planeswalk),
            "expected Effect::Planeswalk, got {:?}",
            def.effect
        );
        assert!(
            !def.optional,
            "expected optional: false for 'then planeswalk'"
        );
    }

    /// CR 701.12a: Soul Conduit / Axis of Mortality body — "two target players
    /// exchange life totals" lowers to ExchangeLifeTotals{Player, Player}.
    #[test]
    fn two_target_players_exchange_life_totals_parses() {
        let def = super::super::parse_effect_chain(
            "Two target players exchange life totals.",
            AbilityKind::Activated,
        );
        match &*def.effect {
            Effect::ExchangeLifeTotals { player_a, player_b } => {
                assert_eq!(*player_a, TargetFilter::Player);
                assert_eq!(*player_b, TargetFilter::Player);
            }
            other => panic!("expected ExchangeLifeTotals, got {other:?}"),
        }
    }

    /// CR 701.12a: Axis of Mortality causative body — "have two target players
    /// exchange life totals" (from "you may have …") lowers to
    /// ExchangeLifeTotals{Player, Player}.
    #[test]
    fn have_two_target_players_exchange_life_totals_parses() {
        let def = super::super::parse_effect_chain(
            "Have two target players exchange life totals.",
            AbilityKind::Spell,
        );
        match &*def.effect {
            Effect::ExchangeLifeTotals { player_a, player_b } => {
                assert_eq!(*player_a, TargetFilter::Player);
                assert_eq!(*player_b, TargetFilter::Player);
            }
            other => panic!("expected ExchangeLifeTotals, got {other:?}"),
        }
    }

    /// CR 701.12a: Magus of the Mirror / Mirror Universe body — "exchange life
    /// totals with target opponent" lowers to ExchangeLifeTotals{Controller,
    /// Typed(Opponent)}.
    #[test]
    fn exchange_life_totals_with_target_opponent_parses() {
        let def = super::super::parse_effect_chain(
            "Exchange life totals with target opponent.",
            AbilityKind::Activated,
        );
        match &*def.effect {
            Effect::ExchangeLifeTotals { player_a, player_b } => {
                assert_eq!(*player_a, TargetFilter::Controller);
                assert_eq!(
                    *player_b,
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
                );
            }
            other => panic!("expected ExchangeLifeTotals, got {other:?}"),
        }
    }

    /// CR 701.12a: "exchange life totals with target player" form lowers to
    /// ExchangeLifeTotals{Controller, Player}.
    #[test]
    fn exchange_life_totals_with_target_player_parses() {
        let def = super::super::parse_effect_chain(
            "Exchange life totals with target player.",
            AbilityKind::Activated,
        );
        match &*def.effect {
            Effect::ExchangeLifeTotals { player_a, player_b } => {
                assert_eq!(*player_a, TargetFilter::Controller);
                assert_eq!(*player_b, TargetFilter::Player);
            }
            other => panic!("expected ExchangeLifeTotals, got {other:?}"),
        }
    }

    /// Walk an `AbilityDefinition`'s effect + sub_ability chain (descending into
    /// `CreateDelayedTrigger`'s wrapped effect) and return the first
    /// `Effect::RevealHand`'s `count` (cloned). Returns `None` if no RevealHand
    /// is present.
    fn find_reveal_hand_count(def: &AbilityDefinition) -> Option<Option<QuantityExpr>> {
        let mut cur = Some(def);
        while let Some(d) = cur {
            match &*d.effect {
                Effect::RevealHand { count, .. } => return Some(count.clone()),
                // Klaw's ETB ability synthesizes as a CreateDelayedTrigger whose
                // wrapped effect carries the RevealHand chain.
                Effect::CreateDelayedTrigger { effect, .. } => {
                    if let Some(found) = find_reveal_hand_count(effect) {
                        return Some(found);
                    }
                }
                _ => {}
            }
            cur = d.sub_ability.as_deref();
        }
        None
    }

    /// CR 701.20a + CR 107.1: Klaw's full ETB ability — "When ~ enters, target
    /// player reveals a number of cards from their hand equal to one plus the
    /// number of creature cards in your graveyard. You choose one of them. That
    /// player discards that card." — must synthesize Effect::RevealHand with a
    /// full `Offset` count. Before the fix this delegated to `parse_quantity_ref`,
    /// which returns `None` for the compound "N plus ..." form, so the count was
    /// dropped (count == None) — measured baseline.
    #[test]
    fn parse_hand_reveal_compound_offset_count() {
        // Production entry: the full Klaw oracle flows through `parse_effect_chain`,
        // strips the "target player" subject, and lowers to Effect::RevealHand
        // nested in the CreateDelayedTrigger. Before the fix count was None;
        // after, it is the full Offset.
        let def = super::super::parse_effect_chain(
            "When ~ enters, target player reveals a number of cards from their hand equal to one plus the number of creature cards in your graveyard. You choose one of them. That player discards that card.",
            AbilityKind::Activated,
        );
        let count = find_reveal_hand_count(&def).unwrap_or_else(|| {
            panic!("expected Effect::RevealHand in chain, got {:?}", def.effect)
        });
        let Some(QuantityExpr::Offset { inner, offset }) = count else {
            panic!("expected Some(Offset) count, got {count:?}");
        };
        assert_eq!(offset, 1);
        let QuantityExpr::Ref { qty } = *inner else {
            panic!("expected Ref inner, got {inner:?}");
        };
        match qty {
            QuantityRef::ZoneCardCount {
                zone,
                card_types,
                scope,
                ..
            } => {
                assert_eq!(zone, crate::types::ability::ZoneRef::Graveyard);
                assert!(card_types.contains(&TypeFilter::Creature));
                assert_eq!(scope, crate::types::ability::CountScope::Controller);
            }
            other => panic!("expected ZoneCardCount, got {other:?}"),
        }
    }

    /// CR 107.1: Bare-ref subsumption regression — "equal to the number of
    /// creature cards in your graveyard" (no offset) must still lower to
    /// RevealPartial with a plain `Ref(ZoneCardCount)`, NOT an `Offset`. Proves
    /// `parse_cda_quantity` still handles the single-ref case the old
    /// `parse_quantity_ref` covered (would regress if the delegate dropped bare
    /// refs).
    #[test]
    fn parse_hand_reveal_bare_ref_count_subsumed() {
        // `parse_hand_reveal_ast` receives the verb-led clause (the "target
        // player" subject is stripped upstream), so call it with the production
        // input shape directly.
        let input =
            "reveals a number of cards from their hand equal to the number of creature cards in your graveyard";
        let lower = input.to_lowercase();
        let result = parse_hand_reveal_ast(input, &lower, &mut ParseContext::default());
        let Some(HandRevealImperativeAst::RevealPartial { count }) = result else {
            panic!("{input}: expected RevealPartial, got {result:?}");
        };
        let QuantityExpr::Ref { qty } = count else {
            panic!("expected bare Ref count (not Offset), got {count:?}");
        };
        assert!(matches!(
            qty,
            QuantityRef::ZoneCardCount {
                zone: crate::types::ability::ZoneRef::Graveyard,
                scope: crate::types::ability::CountScope::Controller,
                ..
            }
        ));
    }

    #[test]
    fn parse_oracle_text_aerial_toastmaster_activation_assembles_a_contraption() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "{3}{W}, Sacrifice another artifact: This creature assembles a Contraption.",
            "Aerial Toastmaster",
            &[],
            &["Artifact".to_string(), "Creature".to_string()],
            &["Cyborg".to_string(), "Rigger".to_string()],
        );
        assert!(
            parsed.abilities.iter().any(|ability| matches!(
                *ability.effect,
                Effect::AssembleContraptions {
                    count: QuantityExpr::Fixed { value: 1 }
                }
            )),
            "abilities: {:?}",
            parsed
                .abilities
                .iter()
                .map(|ability| &ability.effect)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn parse_effect_chain_then_assemble_adds_follow_up_clause() {
        let def = super::super::parse_effect_chain(
            "Counter target spell, then assemble a Contraption.",
            AbilityKind::Spell,
        );
        assert!(matches!(*def.effect, Effect::Counter { .. }));
        let sub = def
            .sub_ability
            .as_deref()
            .unwrap_or_else(|| panic!("expected assemble follow-up on {def:?}"));
        assert!(matches!(
            *sub.effect,
            Effect::AssembleContraptions {
                count: QuantityExpr::Fixed { value: 1 }
            }
        ));
    }

    #[test]
    fn parse_effect_chain_assemble_x_plus_one_contraptions() {
        let def = super::super::parse_effect_chain(
            "Assemble X plus one Contraptions.",
            AbilityKind::Spell,
        );
        assert!(matches!(
            *def.effect,
            Effect::AssembleContraptions {
                count: QuantityExpr::Offset { .. }
            }
        ));
    }

    #[test]
    fn parse_effect_chain_reassemble_target_contraption_you_control() {
        let def = super::super::parse_effect_chain(
            "Reassemble target Contraption you control.",
            AbilityKind::Activated,
        );
        match &*def.effect {
            Effect::ReassembleContraption {
                target,
                control_mode,
            } => {
                assert_eq!(*control_mode, ReassembleControlMode::KeepController);
                assert!(matches!(target, TargetFilter::Typed(_)));
            }
            other => panic!("expected ReassembleContraption, got {other:?}"),
        }
    }

    #[test]
    fn parse_oracle_text_triggered_reassembles_that_player_controls() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Whenever this creature deals combat damage to a player, it reassembles target Contraption that player controls.",
            "Suspicious Nanny",
            &[],
            &["Creature".to_string()],
            &["Human".to_string(), "Spy".to_string(), "Rigger".to_string()],
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|trigger| trigger.execute.is_some())
            .unwrap_or_else(|| panic!("expected triggered reassemble in {:?}", parsed.triggers));
        let execute = trigger.execute.as_ref().expect("trigger execute");
        match &*execute.effect {
            Effect::ReassembleContraption {
                target,
                control_mode,
            } => {
                assert_eq!(*control_mode, ReassembleControlMode::GainControl);
                match target {
                    TargetFilter::Typed(filter) => {
                        assert_eq!(filter.controller, Some(ControllerRef::TargetPlayer));
                    }
                    other => panic!("expected typed target, got {other:?}"),
                }
            }
            other => panic!("expected ReassembleContraption, got {other:?}"),
        }
    }

    #[test]
    fn parse_oracle_text_triggered_it_assembles_for_each_contraption() {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "When this creature enters, it assembles a Contraption for each Contraption you control.",
            "Steamflogger of the Month",
            &[],
            &["Creature".to_string()],
            &["Goblin".to_string(), "Rigger".to_string()],
        );
        let trigger = parsed
            .triggers
            .iter()
            .find(|trigger| trigger.execute.is_some())
            .unwrap_or_else(|| panic!("expected triggered assemble in {:?}", parsed.triggers));
        let execute = trigger.execute.as_ref().expect("trigger execute");
        match &*execute.effect {
            Effect::AssembleContraptions { count } => match count {
                QuantityExpr::Ref { qty } => match qty {
                    QuantityRef::ObjectCount { filter } => match filter {
                        TargetFilter::Typed(filter) => {
                            assert!(filter
                                .type_filters
                                .contains(&TypeFilter::Subtype("Contraption".to_string())));
                            assert_eq!(filter.controller, Some(ControllerRef::You));
                        }
                        other => panic!("expected typed for-each filter, got {other:?}"),
                    },
                    other => panic!("expected object-count quantity, got {other:?}"),
                },
                other => panic!("expected for-each quantity ref, got {other:?}"),
            },
            other => panic!("expected AssembleContraptions, got {other:?}"),
        }
    }

    /// #5649 review (matthewevans): the `Fixed`-count arm of
    /// `parse_exile_count_from_your_graveyard` fixes 8 of the 9 affected cards
    /// (Aegis Sculptor / Bloodcurdler / Egon / Insatiable Frugivore / Kroxa /
    /// Emeritus / Ultimecia — "exile <N> cards from your graveyard"), while the
    /// "that many" arm covers Nefarious Lich. Pin both arms AND the negative that
    /// the possessive "your graveyard" anchor keeps the top-of-library impulse
    /// idiom (owned by `parse_dynamic_count_phrase`) untouched.
    #[test]
    fn exile_count_from_your_graveyard_binds_fixed_and_dynamic_counts() {
        // Fixed-count arm — the eight numeric cards.
        assert_eq!(
            parse_exile_count_from_your_graveyard("two cards from your graveyard"),
            Some(QuantityExpr::Fixed { value: 2 })
        );
        assert_eq!(
            parse_exile_count_from_your_graveyard("eight cards from your graveyard"),
            Some(QuantityExpr::Fixed { value: 8 })
        );
        // Dynamic arm — Nefarious Lich.
        assert_eq!(
            parse_exile_count_from_your_graveyard("that many cards from your graveyard"),
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            })
        );
        // Negatives: the possessive "your graveyard" anchor must never capture the
        // top-of-library impulse idiom.
        assert_eq!(
            parse_exile_count_from_your_graveyard("the top two cards of your library"),
            None
        );
        assert_eq!(
            parse_exile_count_from_your_graveyard("two cards from the top of your library"),
            None
        );

        // End-to-end: a fixed graveyard exile rides the clause's `MultiTargetSpec`.
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "exile two cards from your graveyard",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::ChangeZone {
                    origin: Some(crate::types::zones::Zone::Graveyard),
                    destination: crate::types::zones::Zone::Exile,
                    ..
                }
            ),
            "expected a graveyard->exile ChangeZone, got {:?}",
            def.effect
        );
        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec::exact(QuantityExpr::Fixed { value: 2 })),
            "the count must ride the clause MultiTargetSpec as exact(Fixed {{ 2 }})"
        );
    }

    /// Ill-Gotten Gains class (issue filed alongside this fix): "return up to
    /// N cards from your graveyard to your hand" with no "target" keyword
    /// lowers to `Effect::ChangeZone` (via `ReturnToZone` — an explicit origin
    /// zone routes here rather than the target-less `Return`/`Bounce` arm,
    /// confirmed against the actual parser output) — the shape the *targeted*
    /// sibling "return up to N target cards …" already handles correctly via
    /// `strip_optional_target_prefix`. Before this fix the bare "up to N"
    /// prefix was silently absorbed by the destination/target parser: the
    /// count never reached `ParsedEffectClause.multi_target`, so the effect
    /// resolved as an unbounded "return every matching graveyard card"
    /// instead of "up to N". Pins the `Fixed` arm (this test) and the dynamic
    /// arm (below), plus a negative proving the singular case is untouched.
    #[test]
    fn return_up_to_n_cards_from_graveyard_binds_bounded_multi_target() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "return up to three cards from your graveyard to your hand",
            AbilityKind::Spell,
        );
        let Effect::ChangeZone {
            origin: Some(crate::types::zones::Zone::Graveyard),
            destination: crate::types::zones::Zone::Hand,
            target,
            ..
        } = &*def.effect
        else {
            panic!(
                "expected a graveyard-to-hand ChangeZone, got {:?}",
                def.effect
            );
        };
        let TargetFilter::Typed(tf) = target else {
            panic!("expected a Typed graveyard-card filter, got {target:?}");
        };
        assert!(
            tf.properties
                .contains(&crate::types::ability::FilterProp::InZone {
                    zone: crate::types::zones::Zone::Graveyard
                }),
            "target must be constrained to the graveyard, got {target:?}"
        );
        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 3 })),
            "the count must ride the clause MultiTargetSpec as up_to(Fixed {{ 3 }}), got {:?}",
            def.multi_target
        );
    }

    /// Dynamic-count sibling of the test above: "return up to that many
    /// cards …" (the Nefarious-Lich-shaped anaphoric back-reference, CR
    /// 608.2c, composed with the "up to" quantifier this fix handles) must
    /// bind `QuantityRef::EventContextAmount` through the same seam rather
    /// than silently dropping to a fixed/absent count. `strip_leading_quantifier`
    /// only recognizes "any number of "/"up to " as top-level prefixes (a bare
    /// "that many" with no "up to" is a distinct, not-yet-handled shape — see
    /// `parse_exile_count_from_your_graveyard`'s bare "that many" arm for
    /// `exile`, #5649), so this exercises the dynamic count *inside* the "up
    /// to" branch via `parse_multi_target_count_expr`; not tied to a specific
    /// named card.
    #[test]
    fn return_up_to_that_many_cards_from_graveyard_binds_dynamic_multi_target() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "return up to that many cards from your graveyard to your hand",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::ChangeZone {
                    origin: Some(crate::types::zones::Zone::Graveyard),
                    destination: crate::types::zones::Zone::Hand,
                    ..
                }
            ),
            "expected a graveyard-to-hand ChangeZone, got {:?}",
            def.effect
        );
        assert_eq!(
            def.multi_target,
            Some(MultiTargetSpec::up_to(QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::EventContextAmount
            })),
            "the dynamic count must ride the clause MultiTargetSpec, got {:?}",
            def.multi_target
        );
    }

    /// Negative: the ordinary singular "return a card from your graveyard to
    /// your hand" (Forbidden Crypt / Recall / Skullwinder class) carries no
    /// quantifier at all and must stay `multi_target: None` — the resolver's
    /// existing single-object default is unaffected by this fix.
    #[test]
    fn return_a_card_from_graveyard_has_no_multi_target() {
        let def = crate::parser::oracle_effect::parse_effect_chain(
            "return a card from your graveyard to your hand",
            AbilityKind::Spell,
        );
        assert!(
            matches!(
                &*def.effect,
                Effect::ChangeZone {
                    origin: Some(crate::types::zones::Zone::Graveyard),
                    destination: crate::types::zones::Zone::Hand,
                    ..
                }
            ),
            "expected a graveyard-to-hand ChangeZone, got {:?}",
            def.effect
        );
        assert_eq!(
            def.multi_target, None,
            "a bare singular return must not gain a multi_target, got {:?}",
            def.multi_target
        );
    }
}
