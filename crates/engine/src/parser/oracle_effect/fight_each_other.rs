//! CR 701.14a: The "choose two creatures, then those creatures fight each other"
//! spell class — Joust, Blizzard Brawl, Tail Swipe.
//!
//! These cards read:
//!
//! ```text
//! Choose target creature you control and target creature you don't control.
//! [<rider>.] Then those creatures fight each other.
//! ```
//!
//! CR 701.14a covers BOTH "a creature fights another creature" AND "two creatures
//! fight each other". The two chosen creatures are the fighters (CR 601.2c: each
//! instance of the word "target" opens its own slot); the spell itself is not a
//! participant. The optional `<rider>` sentence buffs / protects the creature you
//! control ("the creature you control gets +N/+M [and gains …] until end of turn
//! [if …]") before the fight.
//!
//! The generic clause splitter mis-parses this frame: its bare-`and` continuation
//! probe (`starts_target_continuous_clause_lower`, `take_until(" gets ")`) crosses
//! the sentence boundary and false-matches the rider's verb, stranding fighter B
//! as a disconnected `Unimplemented` and leaving the `Fight` node with an empty
//! target slot (the fight never resolves and a spurious third target is demanded).
//!
//! This module recognizes the whole frame BEFORE chunk splitting and lowers it to
//! the proven dual-target `Fight` shape used by Prey Upon / Epic Confrontation /
//! Ulvenwald Tracker: a single `Fight` node whose OWN two target slots carry both
//! fighters (`subject` = creature you control, `target` = creature you don't
//! control), read by `resolve_fight_fighters`. When a rider is present it binds to
//! fighter A exactly as Epic Confrontation does — the rider becomes the primary
//! effect (its target slot IS fighter A) and the `Fight` becomes a
//! `SequentialSibling` sub whose `subject` is `ParentTarget` (= fighter A) and
//! whose `target` slot is fighter B. Because the sub is an unconditional
//! `SequentialSibling`, the fight resolves even when the rider's condition (Knight
//! / snow permanents / cast during main phase) is false.

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::combinator::map;
use nom::Parser;

use crate::parser::oracle_ir::ast::{ClauseBoundary, ParsedEffectClause};
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_ir::effect_chain::{ClauseIr, EffectChainIr};
use crate::parser::oracle_nom::error::OracleError;
use crate::parser::oracle_target::parse_target;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, SubAbilityLink, TargetFilter, TargetSelectionMode,
    TypeFilter, TypedFilter,
};

/// Cheap byte-substring fast-reject: only cards carrying this closing phrase pay
/// for the full nom recognition. A positive hit still routes through the
/// combinator recognizer, which remains the sole authority on whether the text
/// forms this frame.
pub(crate) const FIGHT_EACH_OTHER_MARKER: &str = "those creatures fight each other";

/// True when `filter` is a `Typed` creature filter — the fighter-slot shape both
/// chosen creatures must have for this class.
fn is_creature_filter(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(TypedFilter { type_filters, .. })
            if type_filters.contains(&TypeFilter::Creature)
    )
}

/// CR 701.14a: Recognize the "choose target A and target B. [rider.] then those
/// creatures fight each other" frame, returning the two fighter filters (A = the
/// creature you control, B = the creature you don't control) and the optional
/// original-case rider sentence.
fn recognize_frame<'a>(
    text: &'a str,
    lower: &str,
) -> Option<(TargetFilter, TargetFilter, Option<&'a str>)> {
    // Opener.
    let (after_choose, _) = tag::<_, _, OracleError<'_>>("choose ").parse(lower).ok()?;
    let choose_len = lower.len() - after_choose.len();

    // Fighter A phrase, bounded by the "and target" conjunction (CR 601.2c: the
    // second "target" opens fighter B's slot).
    let (after_a, a_phrase) = take_until::<_, _, OracleError<'_>>(" and target ")
        .parse(after_choose)
        .ok()?;
    let a_orig = &text[choose_len..choose_len + a_phrase.len()];
    let (after_and, _) = tag::<_, _, OracleError<'_>>(" and ").parse(after_a).ok()?;

    // Fighter B phrase, bounded by the sentence break after the choose clause.
    let b_start = lower.len() - after_and.len();
    let (after_b, b_phrase) = take_until::<_, _, OracleError<'_>>(". ")
        .parse(after_and)
        .ok()?;
    let b_orig = &text[b_start..b_start + b_phrase.len()];
    let (after_sep, _) = tag::<_, _, OracleError<'_>>(". ").parse(after_b).ok()?;

    // Optional rider sentence, then the fight closer. With a rider the closer is
    // ". then those creatures fight each other"; without one it is the bare
    // "then those creatures fight each other" immediately after the sentence break.
    let rider_start = lower.len() - after_sep.len();
    let (fight_tail, rider_lower) = alt((
        map(
            (
                take_until::<_, _, OracleError<'_>>(". then those creatures fight each other"),
                tag::<_, _, OracleError<'_>>(". then those creatures fight each other"),
            ),
            |(rider, _)| Some(rider),
        ),
        map(
            tag::<_, _, OracleError<'_>>("then those creatures fight each other"),
            |_| None,
        ),
    ))
    .parse(after_sep)
    .ok()?;

    // Nothing but an optional trailing period may follow the fight closer.
    if !fight_tail.chars().all(|c| c == '.' || c.is_whitespace()) {
        return None;
    }

    let (filter_a, _) = parse_target(a_orig);
    let (filter_b, _) = parse_target(b_orig);
    if !is_creature_filter(&filter_a) || !is_creature_filter(&filter_b) {
        return None;
    }

    let rider_orig = rider_lower.map(|rider| &text[rider_start..rider_start + rider.len()]);
    Some((filter_a, filter_b, rider_orig))
}

/// CR 611.2c: Rebind an unbound "the creature you control" rider (whose anaphora
/// parsed to `SelfRef`/`Any`) onto fighter A — the same bound shape "target
/// creature you control gets …" produces. Returns `None` for any rider shape this
/// module can't bind cleanly, so the caller keeps the fight resolving and leaves
/// the rider as an honest gap rather than a silently-wrong buff.
fn bind_rider_to_fighter_a(effect: Effect, filter_a: &TargetFilter) -> Option<Effect> {
    match effect {
        // "the creature you control gets +N/+M" → Pump targeting fighter A.
        Effect::Pump {
            power,
            toughness,
            target: TargetFilter::Any,
        } => Some(Effect::Pump {
            power,
            toughness,
            target: filter_a.clone(),
        }),
        // "the creature you control gets +N/+M and gains <keyword>" → a targeted
        // continuous grant. The unbound anaphora parsed each static's `affected`
        // as `SelfRef` with no effect-level target; rebind to `ParentTarget` +
        // fighter A, exactly as "target creature you control gets … and gains …"
        // lowers.
        Effect::GenericEffect {
            mut static_abilities,
            duration,
            target: None,
        } if !static_abilities.is_empty()
            && static_abilities
                .iter()
                .all(|s| matches!(s.affected, Some(TargetFilter::SelfRef))) =>
        {
            for static_def in static_abilities.iter_mut() {
                static_def.affected = Some(TargetFilter::ParentTarget);
            }
            Some(Effect::GenericEffect {
                static_abilities,
                duration,
                target: Some(filter_a.clone()),
            })
        }
        // CR 122.1 + CR 608.2c: "put a +1/+1 counter on the creature you control
        // [if <condition>]" → PutCounter targeting fighter A. The unbound "the
        // creature you control" anaphora parsed the target as `ParentTarget`
        // (or `Any`); rebind it to fighter A so the counter lands on the right
        // creature — exactly as the primary Pump binds. Any gating condition
        // ("if the gift was promised") rides on the rider def's `condition`
        // (lifted by `parse_effect_chain`) and is preserved by the caller, so
        // Longstalk Brawl / Hog-Monkey Rampage / Malamet Battle Glyph resolve the
        // fight AND the gated counter — main's
        // `s07_longstalk_brawl_counter_gated_on_gift_promised` requires the
        // recognizer not strand that counter as an Unimplemented gap.
        Effect::PutCounter {
            counter_type,
            count,
            target: TargetFilter::Any | TargetFilter::ParentTarget,
        } => Some(Effect::PutCounter {
            counter_type,
            count,
            target: filter_a.clone(),
        }),
        _ => None,
    }
}

/// Wrap a fully-assembled `ParsedEffectClause` as the sole `ClauseIr` of a
/// single-sentence chain.
fn single_clause_ir(parsed: ParsedEffectClause, source_text: &str) -> ClauseIr {
    ClauseIr {
        parsed,
        boundary: Some(ClauseBoundary::Sentence),
        condition: None,
        is_optional: false,
        opponent_may_scope: None,
        repeat_for: None,
        player_scope: None,
        starting_with: None,
        delayed_condition: None,
        prefix_delayed_condition: None,
        intrinsic_continuation: None,
        followup_continuation: None,
        absorbed_by_followup: false,
        multi_target: None,
        where_x_expression: None,
        is_otherwise: false,
        unless_pay: None,
        special: None,
        source_text: source_text.to_string(),
        target_selection_mode: TargetSelectionMode::Chosen,
        target_chooser: None,
    }
}

/// CR 701.14a: Recognize and lower the whole "choose two creatures … those
/// creatures fight each other" frame. Returns `None` when the text is not this
/// class (the caller falls through to baseline chunk splitting).
pub(crate) fn parse_choose_two_creatures_fight(
    full_text: &str,
    kind: AbilityKind,
    ctx: &ParseContext,
) -> Option<EffectChainIr> {
    let lower = full_text.to_ascii_lowercase();
    let (filter_a, filter_b, rider_orig) = recognize_frame(full_text, &lower)?;

    // CR 701.14a: The bare dual-target fight (both fighters in their own slots).
    let bare_fight = |subject: TargetFilter| Effect::Fight {
        subject,
        target: filter_b.clone(),
    };

    let parsed = match rider_orig {
        // No rider — the Prey Upon shape: one Fight node, both fighters typed.
        None => ParsedEffectClause {
            effect: bare_fight(filter_a.clone()),
            duration: None,
            sub_ability: None,
            distribute: None,
            multi_target: None,
            condition: None,
            optional: false,
            unless_pay: None,
        },
        Some(rider) => {
            // Parse the rider through the full chain so its leading/trailing
            // condition ("if it's a Knight" / "if you cast this spell during your
            // main phase") and duration ("until end of turn") are lifted onto the
            // def — `parse_effect_clause` alone leaves them in the text. The rider
            // never contains the fight closer, so it can't re-enter this recognizer.
            let rider_def = super::parse_effect_chain(rider.trim(), kind);
            match bind_rider_to_fighter_a(*rider_def.effect, &filter_a) {
                // Epic Confrontation shape: rider buffs fighter A (its target slot
                // IS fighter A), then the Fight sub (subject = ParentTarget =
                // fighter A, target = fighter B) resolves as an unconditional
                // SequentialSibling so it fires even when the rider is skipped.
                Some(bound) => {
                    let mut fight =
                        AbilityDefinition::new(kind, bare_fight(TargetFilter::ParentTarget));
                    fight.sub_link = SubAbilityLink::SequentialSibling;
                    ParsedEffectClause {
                        effect: bound,
                        duration: rider_def.duration,
                        sub_ability: Some(Box::new(fight)),
                        distribute: None,
                        multi_target: None,
                        condition: rider_def.condition,
                        optional: false,
                        unless_pay: None,
                    }
                }
                // Rider can't bind cleanly — keep the fight resolving (Prey Upon
                // shape, primary) and surface the rider as an honest gap so the
                // buff is never silently dropped.
                None => {
                    let mut gap = AbilityDefinition::new(
                        kind,
                        Effect::unimplemented("fight rider", rider.trim()),
                    );
                    gap.sub_link = SubAbilityLink::SequentialSibling;
                    ParsedEffectClause {
                        effect: bare_fight(filter_a.clone()),
                        duration: None,
                        sub_ability: Some(Box::new(gap)),
                        distribute: None,
                        multi_target: None,
                        condition: None,
                        optional: false,
                        unless_pay: None,
                    }
                }
            }
        }
    };

    Some(EffectChainIr {
        clauses: vec![single_clause_ir(parsed, full_text)],
        kind,
        chain_rounding: None,
        actor: ctx.actor.clone(),
        repeat_until: None,
    })
}

#[cfg(test)]
mod tests {
    use crate::parser::parse_oracle_text;
    use crate::types::ability::{AbilityCondition, ControllerRef, Effect, TargetFilter};
    use crate::types::keywords::Keyword;

    fn has_unimplemented(effect: &Effect) -> bool {
        matches!(effect, Effect::Unimplemented { .. })
    }

    fn ability_tree_has_unimplemented(ability: &crate::types::ability::AbilityDefinition) -> bool {
        has_unimplemented(&ability.effect)
            || ability
                .sub_ability
                .as_deref()
                .is_some_and(ability_tree_has_unimplemented)
            || ability
                .else_ability
                .as_deref()
                .is_some_and(ability_tree_has_unimplemented)
    }

    /// Assert the shared invariant for the whole class: one ability whose Fight
    /// (found on the primary or a sub) carries BOTH fighters in its own slots —
    /// `subject` = creature you control (or `ParentTarget` bound to fighter A),
    /// `target` = creature you don't control — and ZERO `Unimplemented` residual.
    fn assert_connected_fight(
        oracle: &str,
        types: &[&str],
    ) -> crate::types::ability::AbilityDefinition {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let parsed = parse_oracle_text(oracle, "Test Card", &[], &types, &[]);
        assert_eq!(parsed.abilities.len(), 1, "expected a single spell ability");
        let ability = parsed.abilities[0].clone();
        assert!(
            !ability_tree_has_unimplemented(&ability),
            "no Unimplemented node may remain: {ability:#?}"
        );

        // Locate the Fight node anywhere in the primary→sub chain.
        let mut node = Some(&ability);
        let mut fight_target = None;
        let mut fight_subject = None;
        while let Some(def) = node {
            if let Effect::Fight { subject, target } = def.effect.as_ref() {
                fight_subject = Some(subject.clone());
                fight_target = Some(target.clone());
                break;
            }
            node = def.sub_ability.as_deref();
        }
        let subject = fight_subject.expect("a connected Fight node must exist");
        let target = fight_target.expect("a connected Fight node must exist");

        // Fighter B (creature you don't control) is the Fight's OWN target slot.
        match target {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!(
                "fight target (fighter B) must be a typed you-don't-control filter, got {other:?}"
            ),
        }
        // Fighter A is either the Fight's own subject slot (no rider) or bound via
        // ParentTarget to the rider's target slot (rider present).
        match subject {
            TargetFilter::ParentTarget => {}
            TargetFilter::Typed(tf) => assert_eq!(tf.controller, Some(ControllerRef::You)),
            other => panic!("fight subject (fighter A) must bind you-control, got {other:?}"),
        }
        ability
    }

    #[test]
    fn joust_binds_pump_to_fighter_a_and_connects_fight() {
        let ability = assert_connected_fight(
            "Choose target creature you control and target creature you don't control. \
             The creature you control gets +2/+1 until end of turn if it's a Knight. \
             Then those creatures fight each other. (Each deals damage equal to its power to the other.)",
            &["Instant"],
        );
        // Rider is a Pump bound to fighter A (creature you control), gated on Knight.
        match ability.effect.as_ref() {
            Effect::Pump { target, .. } => match target {
                TargetFilter::Typed(tf) => assert_eq!(tf.controller, Some(ControllerRef::You)),
                other => panic!("pump must target fighter A, got {other:?}"),
            },
            other => panic!("Joust primary must be the pump rider, got {other:?}"),
        }
        assert!(
            matches!(
                ability.condition,
                Some(AbilityCondition::TargetMatchesFilter { .. })
            ),
            "Joust rider must stay gated on the Knight condition"
        );
        let fight = ability.sub_ability.as_deref().expect("fight sub");
        assert_eq!(
            fight.sub_link,
            crate::types::ability::SubAbilityLink::SequentialSibling,
            "fight must be a SequentialSibling so it resolves even when the rider is skipped"
        );
    }

    #[test]
    fn blizzard_brawl_binds_grant_to_fighter_a_and_connects_fight() {
        let ability = assert_connected_fight(
            "Choose target creature you control and target creature you don't control. \
             If you control three or more snow permanents, the creature you control gets +1/+0 \
             and gains indestructible until end of turn. Then those creatures fight each other. \
             (Each deals damage equal to its power to the other.)",
            &["Instant"],
        );
        // Rider is a targeted continuous grant bound to fighter A (ParentTarget +
        // effect-level you-control target slot), carrying the indestructible grant.
        match ability.effect.as_ref() {
            Effect::GenericEffect {
                static_abilities,
                target,
                ..
            } => {
                assert_eq!(
                    *target,
                    Some(TargetFilter::Typed(crate::types::ability::TypedFilter {
                        type_filters: vec![crate::types::ability::TypeFilter::Creature],
                        controller: Some(ControllerRef::You),
                        properties: vec![],
                    }))
                );
                assert!(static_abilities
                    .iter()
                    .all(|s| s.affected == Some(TargetFilter::ParentTarget)));
                assert!(static_abilities
                    .iter()
                    .any(|s| s.modifications.iter().any(|m| matches!(
                        m,
                        crate::types::ability::ContinuousModification::AddKeyword {
                            keyword: Keyword::Indestructible
                        }
                    ))));
            }
            other => {
                panic!("Blizzard Brawl primary must be the continuous grant rider, got {other:?}")
            }
        }
    }

    #[test]
    fn tail_swipe_binds_pump_to_fighter_a_and_connects_fight() {
        let ability = assert_connected_fight(
            "Choose target creature you control and target creature you don't control. \
             If you cast this spell during your main phase, the creature you control gets +1/+1 \
             until end of turn. Then those creatures fight each other. \
             (Each deals damage equal to its power to the other.)",
            &["Sorcery"],
        );
        assert!(
            matches!(ability.effect.as_ref(), Effect::Pump { .. }),
            "Tail Swipe primary must be the pump rider"
        );
        assert!(
            matches!(
                ability.condition,
                Some(AbilityCondition::CastDuringPhase { .. })
            ),
            "Tail Swipe rider must stay gated on the main-phase condition"
        );
    }

    #[test]
    fn no_rider_lowers_to_bare_dual_target_fight() {
        // The class general form with no buff rider is the Prey Upon shape: a
        // single Fight node whose two slots are the fighters.
        let ability = assert_connected_fight(
            "Choose target creature you control and target creature you don't control. \
             Then those creatures fight each other.",
            &["Sorcery"],
        );
        match ability.effect.as_ref() {
            Effect::Fight { subject, .. } => match subject {
                TargetFilter::Typed(tf) => assert_eq!(tf.controller, Some(ControllerRef::You)),
                other => panic!("no-rider fight subject must be you-control, got {other:?}"),
            },
            other => panic!("no-rider frame must lower to a bare Fight, got {other:?}"),
        }
        assert!(ability.sub_ability.is_none(), "no rider → no sub");
    }
}
