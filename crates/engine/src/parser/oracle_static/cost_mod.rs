// CR 601.2e — cost modification static abilities.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use crate::types::ability::CastTimingPermission;

pub(crate) fn parse_activated_cost_reduction_minimum_mana(lower: &str) -> Option<u32> {
    preceded(
        take_until::<_, _, OracleError<'_>>(
            "this effect can't reduce the mana in that cost to less than ",
        ),
        preceded(
            tag("this effect can't reduce the mana in that cost to less than "),
            alt((value(1, tag("one mana")), nom_primitives::parse_number)),
        ),
    )
    .parse(lower)
    .ok()
    .map(|(_, minimum)| minimum)
}

pub(crate) fn parse_cost_payment_prohibition_statics(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    let (who, predicate) = strip_casting_prohibition_subject(tp.lower)?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("can't pay life or sacrifice ")
        .parse(predicate)
        .ok()?;
    let (after_suffix, filter_text) = terminated(
        take_until::<_, _, OracleError<'_>>(" to cast spells or activate abilities"),
        tag::<_, _, OracleError<'_>>(" to cast spells or activate abilities"),
    )
    .parse(rest)
    .ok()?;
    let (_, _) = (opt(tag::<_, _, OracleError<'_>>(".")), eof)
        .parse(after_suffix)
        .ok()?;
    let (filter, filter_remainder) = parse_type_phrase(filter_text.trim());
    if !filter_remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }

    Some(vec![
        StaticDefinition::new(StaticMode::CantPayCost {
            who: who.clone(),
            cost: CostPaymentProhibition::PayLife,
        })
        .description(text.to_string()),
        StaticDefinition::new(StaticMode::CantPayCost {
            who,
            cost: CostPaymentProhibition::Sacrifice { filter },
        })
        .description(text.to_string()),
    ])
}

/// CR 107.4f: Parse the K'rrik-class payment-substitution static:
/// "For each {C} in a cost, you may pay 2 life rather than pay that mana."
///
/// The mana symbol `{C}` is a single colored mana symbol (W/U/B/R/G). The
/// life amount must be exactly 2 — no printed exemplar uses any other value,
/// and the Phyrexian-shape infrastructure assumes 2.
///
/// Composed from nom combinators end-to-end; no string matching for dispatch.
pub(crate) fn parse_pay_life_as_colored_mana(text: &str) -> Option<StaticDefinition> {
    let trimmed = text.trim().trim_end_matches('.');
    // Mana symbols are case-preserved in Oracle text — parse against original
    // case, not lowercase. The phrase tail is normalized so case-insensitive
    // matching there is safe; we apply a lowercase shadow only for tail tags.
    let lower_trimmed = trimmed.to_lowercase();

    // Combinator: "for each " + parse_colored_mana_symbol + " in a cost, you may pay " + parse_number(=2) + " life rather than pay that mana"
    // Run nom on a lowercase-prefix view to handle "For each"/"for each" uniformly,
    // but the brace section is case-stable.
    let parser_result: OracleResult<'_, crate::types::mana::ManaColor> = (|| {
        let i = lower_trimmed.as_str();
        let (i, _) = tag::<_, _, OracleError<'_>>("for each ").parse(i)?;
        // The next chars (`{B}`, etc.) are also `{b}` in the lowercased form —
        // accept the lowercase form by mapping each tag.
        let (i, color) = alt((
            value(
                crate::types::mana::ManaColor::White,
                tag::<_, _, OracleError<'_>>("{w}"),
            ),
            value(
                crate::types::mana::ManaColor::Blue,
                tag::<_, _, OracleError<'_>>("{u}"),
            ),
            value(
                crate::types::mana::ManaColor::Black,
                tag::<_, _, OracleError<'_>>("{b}"),
            ),
            value(
                crate::types::mana::ManaColor::Red,
                tag::<_, _, OracleError<'_>>("{r}"),
            ),
            value(
                crate::types::mana::ManaColor::Green,
                tag::<_, _, OracleError<'_>>("{g}"),
            ),
        ))
        .parse(i)?;
        let (i, _) = tag::<_, _, OracleError<'_>>(" in a cost, you may pay ").parse(i)?;
        let (i, n) = nom_primitives::parse_number(i)?;
        if n != 2 {
            // CR 107.4f: only the 2-life Phyrexian shape exists today; any other
            // life value falls through to Unimplemented for hand verification.
            return Err(super::oracle_nom::error::oracle_err(i));
        }
        let (i, _) = tag::<_, _, OracleError<'_>>(" life rather than pay that mana").parse(i)?;
        let (i, _) = all_consuming(opt(tag::<_, _, OracleError<'_>>("."))).parse(i)?;
        Ok((i, color))
    })();

    let (_, color) = parser_result.ok()?;
    Some(
        StaticDefinition::new(StaticMode::PayLifeAsColoredMana { color })
            .affected(TargetFilter::Controller)
            .description(text.to_string()),
    )
}

/// CR 118.9 + CR 601.2f: Parse alternative-cost grant statics that may also
/// carry a flash rider — "You may cast [filter] by paying {X} rather than
/// paying their mana costs. If you cast a spell this way, you may cast it as
/// though it had flash." (Primal Prayers).
pub(crate) fn parse_cast_spells_alternative_cost_multi(text: &str) -> Vec<StaticDefinition> {
    let Some(alt_cost_def) = parse_cast_spells_alternative_cost(text) else {
        return Vec::new();
    };
    vec![alt_cost_def]
}

/// CR 118.9 + CR 601.2f: "You may cast [filter] by paying {cost} rather than
/// paying [their mana costs | its mana cost]." Primal Prayers ({E}, creature
/// MV ≤ 3). The trailing flash rider is carried by the alternative-cost static,
/// not emitted as an unconditional keyword grant.
fn parse_cast_spells_alternative_cost(text: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let tp = nom_tag_tp(&tp, "you may cast ")?.trim_start();

    let (after_filter_lower, filter_lower) = take_until::<_, _, VE<'_>>(" by paying ")
        .parse(tp.lower)
        .ok()?;
    let filter_len = filter_lower.len();
    let filter_original = tp.original[..filter_len].trim();
    let after_filter = TextPair::new(&tp.original[filter_len..], after_filter_lower);
    let after_filter = nom_tag_tp(&after_filter, " by paying ")?;

    let (after_cost_lower, cost_lower) = take_until::<_, _, VE<'_>>(" rather than paying ")
        .parse(after_filter.lower)
        .ok()?;
    let cost_len = cost_lower.len();
    let cost_slice = after_filter.original[..cost_len].trim();
    let after_cost = TextPair::new(&after_filter.original[cost_len..], after_cost_lower);
    let after_cost = nom_tag_tp(&after_cost, " rather than paying ")?;

    let (remainder_lower, _) = alt((
        tag::<_, _, VE<'_>>("their mana costs"),
        tag("its mana cost"),
    ))
    .parse(after_cost.lower)
    .ok()?;
    let consumed = after_cost.lower.len() - remainder_lower.len();
    let remainder = after_cost.original[consumed..]
        .trim()
        .trim_start_matches('.')
        .trim();

    let remainder_lower = remainder.to_lowercase();
    let flash_suffix = tag::<_, _, VE<'_>>("if you cast a spell this way")
        .parse(remainder_lower.as_str())
        .is_ok();

    let base_filter = parse_type_phrase(filter_original).0;
    let affected = apply_spell_keyword_subject_constraints(base_filter, None, None, Vec::new());

    let cost = parse_oracle_cost(cost_slice);
    if !supported_alternative_cast_cost(&cost) {
        return None;
    }

    let timing_permission = flash_suffix.then_some(CastTimingPermission::AsThoughHadFlash);

    let def = StaticDefinition::new(StaticMode::CastWithAlternativeCost {
        cost,
        timing_permission,
    })
    .affected(affected)
    .description(text.to_string())
    .active_zones(vec![Zone::Battlefield]);
    Some(def)
}

/// CR 118.9: Alternative costs the `CastWithAlternativeCost` static supports
/// today. Mana ({0}, {WUBRG}) and energy ({E}) are in; life/discard/free shapes
/// that belong to other cast-permission classes stay deferred.
fn supported_alternative_cast_cost(cost: &AbilityCost) -> bool {
    matches!(
        cost,
        AbilityCost::Mana { .. } | AbilityCost::PayEnergy { .. }
    )
}

/// CR 118.9 + CR 601.2f: Parse a mana-cost-alternative-grant static —
/// "You may [pay] X rather than pay [the/its/this object's] mana cost for
/// [filter] spells you cast." The permanent's controller may pay the
/// alternative cost `X` instead of a matching spell's printed mana cost.
///
/// Class members: Rooftop Storm ({0}, Zombie creature spells), Fist of Suns
/// ({WUBRG}, any spell), Jodah ({WUBRG}, MV 5+ when the qualifier parses).
///
/// Strict-fails to `None` (never misparses) when the payment cannot be parsed
/// as an `AbilityCost` (Dream Halls discard, Bolas's Citadel life-as-MV).
pub(crate) fn parse_spells_alternative_cost(text: &str) -> Option<StaticDefinition> {
    type VE<'a> = OracleError<'a>;

    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Prefix: "you may pay " (Rooftop Storm / Fist of Suns / Jodah). The shorter
    // "you may " is accepted as a fallback so a payment verb other than "pay"
    // (e.g. "you may exert ...") still routes here and strict-fails at the cost
    // gate below rather than misparsing.
    let tp = nom_tag_tp(&tp, "you may pay ")
        .or_else(|| nom_tag_tp(&tp, "you may "))?
        .trim_start();

    // Cost slice: everything up to " rather than pay ", preserving original case
    // (mana symbols are case-sensitive).
    let (after_cost_lower, cost_lower) = take_until::<_, _, VE<'_>>(" rather than pay ")
        .parse(tp.lower)
        .ok()?;
    let cost_len = cost_lower.len();
    let cost_slice = tp.original[..cost_len].trim();
    let after_cost = TextPair::new(&tp.original[cost_len..], after_cost_lower);
    let after_cost = nom_tag_tp(&after_cost, " rather than pay ")?;

    // Article/possessive axis as ONE alt — "[the|its|this permanent's|this
    // object's] mana cost for ". CR 118.9: the alternative-cost phrasing names
    // the spell's own mana cost being replaced.
    let (subject_lower, _) = alt((
        tag::<_, _, VE<'_>>("the mana cost for "),
        tag("its mana cost for "),
        tag("this permanent's mana cost for "),
        tag("this object's mana cost for "),
    ))
    .parse(after_cost.lower)
    .ok()?;
    let consumed = after_cost.lower.len() - subject_lower.len();
    let subject = TextPair::new(&after_cost.original[consumed..], subject_lower);

    // Remainder: "<filter> spell[s] you cast[.]". Locate the marker with nom
    // combinators (take_until + tag), not manual string scanning: `terminated`
    // yields the type-prefix slice preceding the marker while consuming the
    // marker itself, leaving the optional mana-value tail as the remainder.
    let subject = subject.trim_end_matches('.').trim_end();
    let (after_spells_lower, type_prefix_lower) = alt((
        terminated(
            take_until::<_, _, VE<'_>>("spells you cast"),
            tag("spells you cast"),
        ),
        terminated(
            take_until::<_, _, VE<'_>>("spell you cast"),
            tag("spell you cast"),
        ),
    ))
    .parse(subject.lower)
    .ok()?;

    let type_prefix_original = subject.original[..type_prefix_lower.len()].trim();
    let after_spells = after_spells_lower.trim();

    // Optional "with mana value N or greater" qualifier (Jodah MV-5+ class). If
    // an MV qualifier is present but does not parse cleanly into FilterProp::Cmc,
    // strict-fail (None) rather than over-broadening to any spell.
    let mv_filter = if after_spells.is_empty() {
        None
    } else {
        let (prop, _consumed) =
            parse_mana_value_suffix(after_spells, &mut ParseContext::default())?;
        let FilterProp::Cmc { .. } = prop else {
            return None;
        };
        Some(prop)
    };

    let base_filter = if type_prefix_original.is_empty() {
        // "spells you cast" (no type prefix) — any spell (Fist of Suns).
        TargetFilter::Typed(TypedFilter::card())
    } else {
        parse_type_phrase(type_prefix_original).0
    };
    let affected =
        apply_spell_keyword_subject_constraints(base_filter, None, mv_filter, Vec::new());

    let cost = parse_oracle_cost(cost_slice);
    if !supported_alternative_cast_cost(&cost) {
        return None;
    }

    Some(
        StaticDefinition::new(StaticMode::CastWithAlternativeCost {
            cost,
            timing_permission: None,
        })
        .affected(affected)
        .description(text.to_string())
        .active_zones(vec![Zone::Battlefield]),
    )
}
