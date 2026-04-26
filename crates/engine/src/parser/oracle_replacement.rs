use std::str::FromStr;

use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::char;
use nom::combinator::{opt, peek, value};
use nom::sequence::preceded;
use nom::Parser;
use nom_language::error::VerboseError;

use super::oracle_effect::become_copy_except::{parse_except_clause, ExceptClauseContext};
use super::oracle_effect::{parse_effect_chain, try_parse_named_choice};
use super::oracle_nom::bridge::{nom_on_lower, split_once_on_lower};
use super::oracle_nom::condition::parse_inner_condition;
use super::oracle_nom::duration::parse_duration;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_quantity::capitalize_first;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{
    normalize_card_name_refs, parse_count_expr, parse_number, parse_ordinal, strip_after,
    strip_reminder_text, TextPair,
};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ChoiceType, CombatDamageScope, Comparator,
    ContinuousModification, ControllerRef, CopyManaValueLimit, DamageModification,
    DamageTargetFilter, Duration, Effect, FilterProp, ManaModification, PreventionAmount,
    QuantityExpr, QuantityRef, ReplacementCondition, ReplacementDefinition, ReplacementMode,
    StaticCondition, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::mana::{ManaColor, ManaType};
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// Parse a replacement effect line into a ReplacementDefinition.
/// Handles: "If ~ would die", "Prevent all combat damage",
/// "~ enters the battlefield tapped", etc.
///
/// Accepts raw card Oracle text; internally normalizes self-references via
/// `normalize_card_name_refs`. When invoked via [`parse_oracle_text`] the
/// text is already normalized and the internal call is an idempotent no-op.
#[tracing::instrument(level = "debug", skip(card_name))]
pub fn parse_replacement_line(text: &str, card_name: &str) -> Option<ReplacementDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let normalized = replace_self_refs(&text, card_name);
    let norm_lower = normalized.to_lowercase();

    // --- "As ~ enters, choose a [type]" → Moved replacement with persisted Choose ---
    // Must be checked BEFORE shock lands, which may contain this as a sub-pattern.
    if let Some(def) = parse_as_enters_choose(&norm_lower, &text) {
        return Some(def);
    }

    // --- Reveal-lands: "As ~ enters, you may reveal a [FILTER] card from your hand.
    //     If you don't, ~ enters tapped." (Port Town, Gilt-Leaf Palace, Temple cycle) ---
    // Structurally parallel to shock lands: Mandatory replacement whose execute is
    // `RevealFromHand { filter, on_decline: Tap SelfRef }`. The `on_decline` branch
    // mirrors shock lands' decline handler. Must be checked BEFORE shock lands so
    // the "pay N life" pattern isn't fooled by a shared "you may" framing.
    if let Some(def) = parse_reveal_land(&norm_lower, &normalized, &text) {
        return Some(def);
    }

    // --- Shock lands: "As ~ enters, you may pay N life. If you don't, it enters tapped." ---
    // Must be checked BEFORE the generic "enters tapped" pattern.
    if let Some(def) = parse_shock_land(&norm_lower, &text) {
        return Some(def);
    }

    // --- All conditional "enters tapped unless X" patterns (CR 614.1d) ---
    // Dispatches to typed condition extractors in priority order, with generic fallback.
    // Shock lands are handled above (structurally different: Optional mode with decline path).
    if let Some(def) = parse_enters_tapped_unless(&norm_lower, &text) {
        return Some(def);
    }

    // --- "You may have ~ enter as a copy of [filter]" (clone replacement) ---
    // CR 707.9: "Enter as a copy" is a replacement effect modifying the ETB event.
    if let Some(def) = parse_clone_replacement(&norm_lower, &text, card_name) {
        return Some(def);
    }

    // --- "As long as ~ is tapped/untapped, [subject] enter tapped/untapped" ---
    if let Some(def) = parse_source_state_external_entry(&norm_lower, &text) {
        return Some(def);
    }

    // --- "[Type] you control enter untapped" (external replacement) ---
    if let Some(def) = parse_external_enters_untapped(&norm_lower, &text) {
        return Some(def);
    }

    // --- "[Type] your opponents control enter tapped" (external replacement) ---
    if let Some(def) = parse_external_enters_tapped(&norm_lower, &text) {
        return Some(def);
    }

    // --- "~ enters the battlefield tapped" (unconditional) ---
    // Guard: reject text with " unless " — all conditional patterns must be handled above.
    if (nom_primitives::scan_contains(&norm_lower, "enters the battlefield tapped")
        || nom_primitives::scan_contains(&norm_lower, "enters tapped"))
        && !nom_primitives::scan_contains(&norm_lower, "unless")
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Tap {
                        target: TargetFilter::SelfRef,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                .description(text.to_string()),
        );
    }

    // --- "If a card/token would be put into a graveyard, exile it instead" ---
    if let Some(def) = parse_graveyard_exile_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- "If ~ would die, {effect}" ---
    if nom_primitives::scan_contains(&norm_lower, "~ would die")
        || nom_primitives::scan_contains(&norm_lower, "~ would be destroyed")
    {
        let mut def = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description(text.to_string());
        // CR 614.1a + CR 122.1: Try the shared exile-anaphor recognizer first
        // so the self-die branch sees the same prefix/suffix word-order
        // handling and `with N <type> counter(s) on it` lift as the non-self
        // `parse_creature_die_exile_replacement` branch. Darigaaz Reincarnated's
        // "instead exile it with three egg counters on it" routes through
        // here (self-die `~ would die`), not through the non-self path.
        if let Some(execute) = self_die_exile_anaphor_execute(&normalized, &text) {
            def = def.execute(execute);
            return Some(def);
        }
        // Fall through: anaphor didn't match — keep prior coverage for compound
        // tails like "return it to its owner's hand instead" via the generic
        // chain parser.
        let effect_text = extract_replacement_effect(&normalized);
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
        }
        return Some(def);
    }

    // --- "If [filter] would die, exile it instead" (non-self replacement) ---
    // CR 614.1a: Replacement effects that exile dying creatures instead of putting
    // them into the graveyard. Subject is a creature filter, not self-reference.
    // E.g., "If another creature would die, exile it instead." (Void Maw)
    //       "If a nontoken creature an opponent controls would die, exile it instead." (Valentin)
    //       "If a creature an opponent controls would die, exile it instead." (Vren)
    if let Some(def) = parse_creature_die_exile_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- "Prevent all/the next N damage" patterns (CR 615) ---
    if let Some(def) = parse_damage_prevention_replacement(&norm_lower, &text) {
        return Some(def);
    }
    // "damage can't be prevented" is handled by effect parsing (Effect::AddRestriction),
    // not replacement parsing. See oracle_effect.rs damage prevention disabled handler.

    if let Some(def) = parse_conditional_draw_replacement(&text, &lower) {
        return Some(def);
    }

    // --- "If you would draw a card, {effect}" ---
    if nom_primitives::scan_contains(&lower, "you would draw") {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::Draw).description(text.to_string());
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
        }
        return Some(def);
    }

    // --- "If [player] would gain life, {effect}" ---
    // CR 614.1a: Widened from "you would gain life" to handle opponent/player scope.
    if nom_primitives::scan_contains(&lower, "would gain life") {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::GainLife).description(text.to_string());
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
        }
        // Parse the subject to determine player scope
        if nom_primitives::scan_contains(&lower, "an opponent would gain life")
            || nom_primitives::scan_contains(&lower, "opponent would gain life")
        {
            def.valid_player = Some(ControllerRef::Opponent);
        } else if nom_primitives::scan_contains(&lower, "a player would gain life") {
            // "a player" applies to all players — None means controller-only,
            // so we need a way to express "all". Use a sentinel: leave valid_player
            // as None and let the matcher check. Actually, for "a player", the
            // replacement applies regardless of who gains life. The matcher needs
            // to be updated to not filter on controller when valid_player is None
            // and the subject was "a player". For now, set valid_player to None
            // and document that the matcher should not restrict player scope.
            // NOTE: The existing matcher restricts to controller only. For Tainted Remedy
            // ("an opponent"), we set Opponent. For "you", we leave None (controller-only).
        }
        return Some(def);
    }

    // --- "If [someone] would lose life, they lose twice that much life instead" ---
    if nom_primitives::scan_contains(&lower, "would lose life") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::LoseLife).description(text.to_string()),
        );
    }

    // --- "If [source] would deal [noncombat] damage ... it deals that much damage plus N instead" ---
    // CR 614.1a: Damage boost/reduction replacement effects.
    if nom_primitives::scan_contains(&lower, "would deal")
        && nom_primitives::scan_contains(&lower, "damage")
        && nom_primitives::scan_contains(&lower, "instead")
    {
        if let Some(def) = parse_damage_modification_replacement(&norm_lower, &text) {
            return Some(def);
        }
        // Exotic pattern (coin-flip, redirection, etc.) — keep as no-op stub
        return Some(
            ReplacementDefinition::new(ReplacementEvent::DamageDone).description(text.to_string()),
        );
    }

    // --- "Whenever you cast [spell], that [subject] enters with ... counter(s) on it" ---
    // CR 614.1c: Despite the "whenever you cast" framing, "enters with" is a
    // replacement effect (not a triggered ability), so Wildgrowth Archaic and
    // its cousin family (Runadi, Boreal Outrider, Torgal, …) are modeled as
    // static replacements on the *cast spell itself*, not delayed triggers.
    // This branch must run before `parse_enters_with_counters` so the
    // "whenever you cast …" prefix is recognized first.
    if let Some(def) = parse_whenever_you_cast_enters_with(&norm_lower, &text) {
        return Some(def);
    }

    // --- "[Subject] enters/escapes with N [type] counter(s)" ---
    // CR 614.1c: Handles "enters with", "escapes with" (CR 702.138), and
    // kicker-conditional "if was kicked, it enters with" (CR 702.33d).
    if (nom_primitives::scan_contains(&lower, "enters")
        || nom_primitives::scan_contains(&lower, "escapes"))
        && nom_primitives::scan_contains(&lower, "counter")
    {
        if let Some(def) = parse_enters_with_counters(&norm_lower, &text) {
            return Some(def);
        }
    }

    // --- Token creation replacement: "if one or more tokens would be created..." ---
    if nom_primitives::scan_contains(&lower, "tokens would be created")
        || nom_primitives::scan_contains(&lower, "token would be created")
        || nom_primitives::scan_contains(&lower, "would create one or more tokens")
        || nom_primitives::scan_contains(&lower, "would create a token")
    {
        if let Some(def) = parse_token_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // CR 614.1a + CR 111.1: Subtype-gated token creation replacement —
    // "if you would create one or more <subtype> tokens, instead create
    // those tokens plus an additional <subtype> token" (Xorn class).
    // Distinguished from the Chatterfang/Doubling-Season class above by its
    // subtype condition AND inverted "instead create" word order.
    if nom_primitives::scan_contains(&lower, "would create one or more")
        && nom_primitives::scan_contains(&lower, "instead create those tokens plus")
    {
        if let Some(def) = parse_xorn_subtype_token_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // CR 614.1a + CR 111.1: Manufactor-class ensure-all token replacement —
    // "if you would create a <subtype>, <subtype>, or <subtype> token, instead
    // create one of each." Gated by the comma-separated subtype list AND the
    // "instead create one of each" tail; mutually exclusive with the Xorn
    // shape above (which uses "those tokens plus").
    if nom_primitives::scan_contains(&lower, "would create a ")
        && nom_primitives::scan_contains(&lower, "instead create one of each")
    {
        if let Some(def) = parse_manufactor_ensure_all_token_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // --- Counter addition replacement: "if one or more ... counters would be put on..." ---
    if nom_primitives::scan_contains(&lower, "counters would be put on")
        || nom_primitives::scan_contains(&lower, "counter would be put on")
        || nom_primitives::scan_contains(&lower, "would put one or more counters")
        || nom_primitives::scan_contains(&lower, "would put a counter")
    {
        if let Some(def) = parse_counter_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // --- Damage redirection: "all damage that would be dealt to [target] is dealt to ~ instead" ---
    // CR 614.1a: Replacement effects that redirect damage to a different recipient.
    if let Some(def) = parse_damage_redirection_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- Event substitution: "if [player] would [event], [skip/prevent] instead" ---
    // CR 614.1a: Replacement effects that nullify or substitute an event entirely.
    if let Some(def) = parse_event_substitution_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- Mana type replacement: "if a land would produce mana, it produces [X] instead" ---
    // CR 614.1a: Replacement effects that change the type of mana produced.
    if let Some(def) = parse_mana_replacement(&norm_lower, &text) {
        return Some(def);
    }

    None
}

/// Case-insensitive replacement of card name and self-referencing phrases with "~".
fn replace_self_refs(text: &str, card_name: &str) -> String {
    normalize_card_name_refs(text, card_name)
}

/// CR 603.6b + CR 701.20a: Parse the reveal-land pattern.
///
/// Matches "As ~ enters, you may reveal a [FILTER] card from your hand.
/// If you don't, ~ enters tapped." — covering Port Town, Gilt-Leaf Palace, and
/// the full 10-Temple reveal-land cycle (Temple of Abandon, Temple of Enlightenment,
/// etc.). Also symmetric "if you do, [effect]" variants reuse the same primitive.
///
/// Returns a `Mandatory` Moved replacement whose `execute` is a
/// `RevealFromHand { filter, on_decline: Tap SelfRef }` effect. The engine-side
/// resolver sets `WaitingFor::RevealChoice { optional: true, ... }` on the
/// controller's eligible hand cards and routes an empty pick (decline) or an
/// empty eligible set through the `on_decline` chain.
fn parse_reveal_land(
    norm_lower: &str,
    normalized: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Nom combinator: recognize the leading "as ~ enters, you may reveal " framing.
    // `nom_on_lower` bridges the already-lowercase matcher into the normalized
    // (case-preserving, self-refs replaced with `~`) source; indexing is consistent
    // because `normalized.to_lowercase()` equals `norm_lower` bijectively on ASCII.
    let ((), after_reveal) = nom_on_lower(normalized, norm_lower, |i| {
        value(
            (),
            (
                alt((
                    tag("as ~ enters, you may reveal "),
                    tag("as ~ enters the battlefield, you may reveal "),
                )),
                // Leading article on the filter: "a Plains or Island card", "an Elf card".
                alt((tag("a "), tag("an "))),
            ),
        )
        .parse(i)
    })?;

    // Split the filter phrase from the remaining decline sentence at
    // " card from your hand". Nom's `take_until` advances past the prefix;
    // consumed byte count maps back into the original-case slice.
    let after_reveal_lower = after_reveal.to_lowercase();
    let ((), after_filter) = nom_on_lower(after_reveal, &after_reveal_lower, |i| {
        value(
            (),
            take_until::<_, _, VerboseError<&str>>(" card from your hand"),
        )
        .parse(i)
    })?;
    let consumed = after_reveal.len() - after_filter.len();
    let filter_phrase = &after_reveal[..consumed];
    let remainder = after_filter;
    let remainder_lower = remainder.to_lowercase();

    // Parse the filter phrase (e.g., "Plains or Island", "Elf") into a TargetFilter.
    // `parse_type_phrase` handles union types via `TargetFilter::Or` and single
    // subtypes via `TargetFilter::Typed`. Reject phrases we cannot classify —
    // better to fall through to a generic enter-tapped parse than to synthesize
    // a misbehaving filter.
    let (filter, filter_remainder) = parse_type_phrase(filter_phrase.trim());
    if !filter_remainder.trim().is_empty() {
        return None;
    }
    if matches!(filter, TargetFilter::Any) {
        return None;
    }

    // The tail dispatches between two grammatical variants:
    //   (A) Port Town / Gilt-Leaf Palace: "if you don't, ~ enters tapped"
    //   (B) Tarkir reveal-tribal cycle (Fortified Beachhead, Temple of the Dragon
    //       Queen): "~ enters tapped unless you revealed a [filter] card this way
    //       or you control a [filter]"
    // Variant (B) is rules-correct as a single replacement: the on_decline Tap
    // is gated by `AbilityCondition::ControllerControlsMatching { negated: true }`,
    // so the Tap fires only when the controller doesn't already control a
    // [filter] permanent. The accept-reveal path naturally short-circuits the
    // on_decline branch (the optional reveal was satisfied), giving the Or
    // semantics required by CR 614.1d.
    let tail_variant = parse_reveal_land_tail(remainder, &remainder_lower, &filter)?;

    // The accept branch: a RevealFromHand effect that, when resolved, prompts
    // the controller to pick a matching card or decline. on_decline runs the
    // tail-specific decline ability (unconditional Tap for variant A, conditional
    // Tap for variant B).
    let on_decline = match tail_variant {
        RevealLandTail::IfYouDontTap => unconditional_tap_self_ability(),
        RevealLandTail::TappedUnlessRevealedOrControl => {
            tap_self_unless_controls_matching_ability(&filter)
        }
    };

    let reveal = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RevealFromHand {
            filter,
            on_decline: Some(Box::new(on_decline)),
        },
    );

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(reveal)
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string()),
    )
}

/// CR 614.1d: Distinguishes the two grammatical tails of the reveal-land cycle.
/// The filter-bearing variant carries the disjunction structure into the resolver
/// via the on_decline ability's condition, not via a new ReplacementCondition.
enum RevealLandTail {
    /// "if you don't, ~ enters tapped" — Port Town / Gilt-Leaf Palace cycle.
    IfYouDontTap,
    /// "~ enters tapped unless you revealed a [filter] card this way or you
    /// control a [filter]" — Tarkir Dragonstorm reveal-tribal cycle (Fortified
    /// Beachhead, Temple of the Dragon Queen).
    TappedUnlessRevealedOrControl,
}

/// Parse the tail of a reveal-land Oracle text starting at `" card from your
/// hand"`. Both grammatical variants share that prefix, so we dispatch on the
/// remainder via a single `alt()` of nested combinators.
///
/// `expected_filter` is the filter parsed from the lead sentence. For the
/// Tarkir variant we require the post-"or you control" filter phrase to match
/// the same type — a coherence check that mirrors CR 614.1d (the disjunction
/// gates the same permanent class).
fn parse_reveal_land_tail(
    remainder: &str,
    remainder_lower: &str,
    expected_filter: &TargetFilter,
) -> Option<RevealLandTail> {
    // Variant (A): "if you don't, [~|it] enters [the battlefield] tapped".
    // Trailing punctuation (period) is tolerated by `trim_end_matches`.
    let variant_a = nom_on_lower(remainder, remainder_lower, |i| {
        value(
            (),
            (
                tag(" card from your hand. if you don't, "),
                alt((tag("~ "), tag("it "))),
                alt((tag("enters tapped"), tag("enters the battlefield tapped"))),
            ),
        )
        .parse(i)
    });
    if let Some(((), tail)) = variant_a {
        if tail.trim_end_matches('.').trim().is_empty() {
            return Some(RevealLandTail::IfYouDontTap);
        }
    }

    // Variant (B): "[~|it] enters [the battlefield] tapped unless you revealed
    // [a|an] " — match through the unless-you-revealed lead, then check the
    // post-"this way or you control [a|an] " filter against the expected filter.
    let variant_b = nom_on_lower(remainder, remainder_lower, |i| {
        value(
            (),
            (
                tag(" card from your hand. "),
                alt((tag("~ "), tag("it "))),
                alt((tag("enters tapped"), tag("enters the battlefield tapped"))),
                tag(" unless you revealed "),
                alt((tag("a "), tag("an "))),
            ),
        )
        .parse(i)
    });
    let ((), after_unless) = variant_b?;
    let after_unless_lower = after_unless.to_lowercase();

    // Take until " card this way or you control " — between is the first
    // disjunction filter phrase; it must match `expected_filter` for coherence.
    let ((), after_first_filter) = nom_on_lower(after_unless, &after_unless_lower, |i| {
        value(
            (),
            take_until::<_, _, VerboseError<&str>>(" card this way or you control "),
        )
        .parse(i)
    })?;
    let first_filter_consumed = after_unless.len() - after_first_filter.len();
    let first_filter_phrase = &after_unless[..first_filter_consumed];
    let (first_filter, first_remainder) = parse_type_phrase(first_filter_phrase.trim());
    if !first_remainder.trim().is_empty() || first_filter != *expected_filter {
        return None;
    }

    // Step past " card this way or you control " then "a "/"an ", and parse
    // the second disjunction filter phrase up to end-of-string. Both filter
    // phrases must canonicalize identically.
    let after_first_filter_lower = after_first_filter.to_lowercase();
    let ((), after_or) = nom_on_lower(after_first_filter, &after_first_filter_lower, |i| {
        value(
            (),
            (
                tag(" card this way or you control "),
                alt((tag("a "), tag("an "))),
            ),
        )
        .parse(i)
    })?;
    let second_filter_phrase = after_or.trim().trim_end_matches('.').trim();
    let (second_filter, second_remainder) = parse_type_phrase(second_filter_phrase);
    if !second_remainder.trim().is_empty() || second_filter != *expected_filter {
        return None;
    }

    Some(RevealLandTail::TappedUnlessRevealedOrControl)
}

/// Build the unconditional `Tap SelfRef` on_decline used by Port Town / Gilt-Leaf
/// Palace and the rest of the if-you-don't reveal-land cycle.
fn unconditional_tap_self_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Tap {
            target: TargetFilter::SelfRef,
        },
    )
}

/// CR 608.2c + CR 614.1d: Build the conditional `Tap SelfRef` on_decline used by
/// the Tarkir reveal-tribal cycle. The Tap fires only when the controller
/// doesn't already control a [filter] permanent, encoding the "or you control a
/// [filter]" disjunction as an AbilityCondition gate on the decline branch.
/// `filter` is cloned and bound to `ControllerRef::You` so the runtime evaluates
/// it against the ability controller's permanents.
fn tap_self_unless_controls_matching_ability(filter: &TargetFilter) -> AbilityDefinition {
    let bound_filter = inject_controller(filter.clone(), ControllerRef::You);
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Tap {
            target: TargetFilter::SelfRef,
        },
    )
    .condition(
        crate::types::ability::AbilityCondition::ControllerControlsMatching {
            filter: bound_filter,
            negated: true,
        },
    )
}

/// Parse shock land pattern: "As ~ enters, you may pay N life. If you don't, it enters tapped."
/// Returns Optional ReplacementDefinition with execute=LoseLife (accept) and decline=Tap (decline).
fn parse_shock_land(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    // Match: "you may pay N life" + "enters tapped" (in either sentence order)
    if !nom_primitives::scan_contains(norm_lower, "you may pay")
        || !nom_primitives::scan_contains(norm_lower, "life")
    {
        return None;
    }
    if !nom_primitives::scan_contains(norm_lower, "enters tapped")
        && !nom_primitives::scan_contains(norm_lower, "enters the battlefield tapped")
    {
        return None;
    }

    // Extract life amount: "pay 2 life", "pay 3 life", etc.
    let amount = extract_life_payment(norm_lower)?;

    let lose_life = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: amount },
            target: None,
        },
    );

    let tap_self = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Tap {
            target: TargetFilter::SelfRef,
        },
    );

    let has_basic_land_type_choice =
        nom_primitives::scan_contains(norm_lower, "choose a basic land type");
    let execute = if has_basic_land_type_choice {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: true,
            },
        )
        .sub_ability(lose_life)
    } else {
        lose_life
    };

    let decline = if has_basic_land_type_choice {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: true,
            },
        )
        .sub_ability(tap_self)
    } else {
        tap_self
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(execute)
            .mode(ReplacementMode::Optional {
                decline: Some(Box::new(decline)),
            })
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string()),
    )
}

/// Parse "As ~ enters, choose a [type]" into a Moved replacement with persisted Choose.
/// Skips lines that also contain shock land markers (handled by parse_shock_land).
fn parse_as_enters_choose(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    // Must have "as" + "enters" framing
    if !nom_primitives::scan_contains(norm_lower, "as")
        || !nom_primitives::scan_contains(norm_lower, "enters")
    {
        return None;
    }

    // Don't match shock lands — they have their own handler
    if nom_primitives::scan_contains(norm_lower, "you may pay")
        && nom_primitives::scan_contains(norm_lower, "life")
    {
        return None;
    }

    // Extract the "choose a ..." clause — scan_split_at_phrase returns (prefix, rest_starting_at_match)
    let (_, choose_text) = nom_primitives::scan_split_at_phrase(norm_lower, |i| {
        tag::<_, _, VerboseError<&str>>("choose ").parse(i)
    })?;
    let choice_type = try_parse_named_choice(choose_text)?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Choose {
                    choice_type,
                    persist: true,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string()),
    )
}

/// CR 707.9 / CR 614.1c: Parse clone replacement effect.
/// "You may have ~ enter as a copy of [any] [type] on the battlefield"
/// "You may have ~ enter as a copy of any creature card in a graveyard, ..."
/// Emits an Optional Moved replacement with BecomeCopy as the execute effect.
/// The player chooses a valid card to copy as part of the replacement.
///
/// The source zone is carried on the returned filter via `FilterProp::InZone`
/// (battlefield is the default when no zone qualifier is present).
/// `card_name` threads through so `"his/her/its name is <card name>"` exception
/// clauses can emit a `SetName` override keyed to the original card name.
fn parse_clone_replacement(
    norm_lower: &str,
    original_text: &str,
    card_name: &str,
) -> Option<ReplacementDefinition> {
    // CR 614.1c: Two grammatical framings of the same ETB-copy replacement class:
    //   (a) "you may have ~ enter as a copy of ..."     (Phantasmal Image class)
    //   (b) "as ~ enters, you may have it become a copy of ..." (Cursed Mirror class)
    // Both converge on "… a copy of <filter> on the battlefield [<suffix>]". The
    // verb phrase is the only grammatical difference, so we split on it via alt()
    // and share every downstream step (filter, zone, duration, except-clause).
    let (before_copy, after_copy) = find_copy_verb(norm_lower)?;

    // Must be preceded by "you may have" for the optional framing (CR 614.1c).
    // Both framings share this prefix — Phantasmal Image: "You may have ~ enter…",
    // Cursed Mirror: "As ~ enters, you may have it become…". The guard prevents
    // accidental matches on ability text containing "become a copy of" outside
    // an ETB framing (none known today but defensive against future prints).
    if !nom_primitives::scan_contains(before_copy, "you may have") {
        return None;
    }

    // CR 400.1: Match any supported source zone. Battlefield is the existing
    // Clone/Phantasmal Image class; graveyard (Superior Spider-Man) extends the
    // same building block. The zone flows onto the filter's `FilterProp::InZone`
    // below so `find_copy_targets` can scan the correct zone without branching.
    let (type_text, suffix, source_zone) = split_on_clone_source_zone(after_copy)?;
    // Strip "any " / "a " / "an " article before the type phrase
    let type_text = alt((
        tag::<_, _, VerboseError<&str>>("any "),
        tag("a "),
        tag("an "),
    ))
    .parse(type_text)
    .map_or(type_text, |(rest, _)| rest)
    .trim();

    let (mut filter, leftover) = parse_type_phrase(type_text);
    if !leftover.trim().is_empty() {
        return None;
    }

    // CR 400.1: Thread the source zone onto the filter when it isn't the default
    // battlefield. `parse_type_phrase` does not emit `InZone` from a bare type
    // word like "creature", so the zone must be attached here. Skip for
    // battlefield to preserve existing Clone/Phantasmal Image filter shape.
    if source_zone != Zone::Battlefield {
        filter = attach_zone_to_filter(filter, source_zone);
    }

    // CR 707.9 / CR 614.1c: The suffix carries any "except it's a {type}" and
    // "it has {keyword}" modifications plus the optional mana-value ceiling.
    // Also handles "except its/his/her name is X" (SetName override) and
    // "except he's/she's/it's N/M {type list} in addition to its other types"
    // (P/T override + type additions; CR 707.9b).
    //
    // Unrecognized fragments degrade gracefully to `(None, vec![])` so the plain
    // BecomeCopy replacement still registers — dropping the entire replacement
    // for an unparsed suffix would lose the clone behaviour entirely.
    //
    // The suffix may also carry a trailing "When you do, ..." reflexive trigger
    // clause past the sentence boundary — parsed separately into a sub_ability.
    let (mana_value_limit, duration, additional_modifications, post_period) =
        parse_clone_suffix(suffix.trim(), card_name);

    // CR 707.9a: The copy effect uses the chosen object's copiable values.
    // This is NOT targeting (hexproof/shroud don't apply).
    // CR 611.3 + CR 613.1a: When the suffix carries a duration phrase
    // ("until end of turn"), the copy effect is a continuous effect that ends
    // when the duration expires (Cursed Mirror class). Permanent otherwise.
    let mut copy_effect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::BecomeCopy {
            target: filter,
            duration,
            mana_value_limit,
            additional_modifications,
        },
    )
    .description(original_text.to_string());

    // CR 603.12: "When you do, ..." — reflexive trigger that fires when the
    // clone replacement's choose-and-copy action was performed. Parsed as a
    // sub_ability with condition `WhenYouDo`; the parent's targets (the copied
    // source card) are forwarded so "that card" (`TargetFilter::TriggeringSource`)
    // resolves to the chosen card for e.g. "exile that card".
    if let Some(reflexive) = parse_when_you_do_reflexive(post_period) {
        copy_effect = copy_effect.sub_ability(reflexive);
    }

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(copy_effect)
            .mode(ReplacementMode::Optional { decline: None })
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string()),
    )
}

/// Locate the clone-verb phrase in a normalised Oracle line and return
/// `(before_verb, after_verb)` around it.
///
/// Recognises both grammatical framings of the ETB-copy replacement class:
/// - `"enter as a copy of "` (Phantasmal Image / Phyrexian Metamorph / …)
/// - `"become a copy of "` (Cursed Mirror / future ETB-copy prints using
///   the "as this enters, …, become a copy of" shape)
///
/// The verbs are leaf alternatives with no shared prefix, so each is scanned
/// independently and the earliest match wins — this mirrors the earliest-match
/// discipline used by `split_on_clone_source_zone` / `split_on_first_of`.
fn find_copy_verb(norm_lower: &str) -> Option<(&str, &str)> {
    let candidates: &[&str] = &["enter as a copy of ", "become a copy of "];
    let mut best: Option<(usize, usize)> = None;
    for phrase in candidates {
        if let Some((before, _)) = nom_primitives::scan_split_at_phrase(norm_lower, |i| {
            tag::<_, _, VerboseError<&str>>(*phrase).parse(i)
        }) {
            let pos = before.len();
            if best.is_none_or(|(bp, _)| pos < bp) {
                best = Some((pos, phrase.len()));
            }
        }
    }
    let (pos, len) = best?;
    Some((&norm_lower[..pos], &norm_lower[pos + len..]))
}

/// Split the post-"enter as a copy of " remainder into (type_text, suffix, source_zone).
/// Recognises both the battlefield form ("... on the battlefield, ...") and the
/// graveyard forms ("... in a graveyard, ...", "... in any graveyard, ..."). The
/// returned `type_text` is the span between "enter as a copy of " and the zone
/// clause; `suffix` is everything after the zone clause (including the leading
/// `,` / `.` boundary).
fn split_on_clone_source_zone(after_copy: &str) -> Option<(&str, &str, Zone)> {
    let candidates: &[(&str, Zone)] = &[
        (" on the battlefield", Zone::Battlefield),
        (" in any graveyard", Zone::Graveyard),
        (" in a graveyard", Zone::Graveyard),
    ];
    // Earliest-matching phrase wins — "in a graveyard" before "in any graveyard"
    // when both appear; structurally equivalent to `split_on_first_of` but also
    // returns the zone selector.
    let mut best: Option<(usize, usize, Zone)> = None;
    for &(phrase, zone) in candidates {
        if let Ok((_, (before, _))) = nom_primitives::split_once_on(after_copy, phrase) {
            let pos = before.len();
            if best.is_none_or(|(best_pos, _, _)| pos < best_pos) {
                best = Some((pos, phrase.len(), zone));
            }
        }
    }
    if let Some((pos, len, zone)) = best {
        let type_text = &after_copy[..pos];
        let suffix = &after_copy[pos + len..];
        return Some((type_text, suffix, zone));
    }
    // CR 614.1c fallback: no explicit zone qualifier means battlefield
    // (Spark Double's "you may have ~ enter as a copy of a creature or
    // planeswalker you control, except <body>"; Deceptive Frostkite's
    // "a creature you control with power 4 or greater, except <body>").
    // The except clause itself becomes the type/suffix boundary so the
    // type phrase doesn't absorb the modification text. When no except
    // clause is present either, treat the entire post-`copy of` text as
    // the type phrase with an empty suffix.
    if let Ok((_, (before, _))) = nom_primitives::split_once_on(after_copy, ", except") {
        let pos = before.len();
        let type_text = &after_copy[..pos];
        // Suffix INCLUDES the leading `, except <body>` so `parse_clone_suffix`
        // → `parse_except_clause` sees the expected `, except ` start.
        let suffix = &after_copy[pos..];
        return Some((type_text, suffix, Zone::Battlefield));
    }
    Some((after_copy, "", Zone::Battlefield))
}

/// Attach `FilterProp::InZone { zone }` to a filter produced by `parse_type_phrase`.
/// `parse_type_phrase` handles its own "in a graveyard" suffix when present in
/// the type text, but clone-replacement text carries the zone *outside* the type
/// phrase ("any creature card in a graveyard"), so the zone must be merged in.
fn attach_zone_to_filter(filter: TargetFilter, zone: Zone) -> TargetFilter {
    use crate::types::ability::FilterProp;
    match filter {
        TargetFilter::Typed(mut tf) => {
            if !tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { .. }))
            {
                tf.properties.push(FilterProp::InZone { zone });
            }
            TargetFilter::Typed(tf)
        }
        other => other,
    }
}

/// Parse a trailing "When you do, ..." reflexive trigger clause.
///
/// Delegates to the existing effect-chain parser, which routes
/// `strip_if_you_do_conditional` to set `condition = AbilityCondition::WhenYouDo`
/// on the resulting AbilityDefinition (CR 603.12 reflexive trigger semantics).
/// Returns None when the text doesn't start with a "when you do" phrase or the
/// chain parser produces an unimplemented effect (so the caller can fall back
/// to the plain BecomeCopy replacement without a reflexive trigger).
fn parse_when_you_do_reflexive(post_period: &str) -> Option<AbilityDefinition> {
    // Strip the sentence terminator / separator space preceding the reflexive
    // clause. These are structural punctuation, not parsing dispatch.
    let trimmed = post_period.trim_start_matches(['.', ' ']);
    if trimmed.is_empty() {
        return None;
    }
    // Compose the prefix guard as a nom leaf via `nom_on_lower` — matches the
    // rest of this file's cost/prefix stripping pattern and leaves an `alt()`
    // seam for future reflexive-clause variants ("when that happens", etc.)
    // without reshaping the guard.
    let lower = trimmed.to_lowercase();
    nom_on_lower(trimmed, &lower, |i| {
        value((), tag::<_, _, VerboseError<&str>>("when you do")).parse(i)
    })?;
    let def = super::oracle_effect::parse_effect_chain(trimmed, AbilityKind::Spell);
    // Reject unimplemented fallbacks — the chain parser returns
    // `Effect::Unimplemented` when no pattern matches, which would attach a
    // dead sub_ability to the clone replacement.
    if matches!(*def.effect, Effect::Unimplemented { .. }) {
        return None;
    }
    Some(def)
}

/// Parse the suffix of a clone replacement, which carries the optional
/// "with mana value ≤ cost" ceiling (CR 614.1c), any "except it's a(n) {type}"
/// type/subtype additions, any "and it has {keyword[,...]}" keyword grants
/// (CR 707.9a), and — for gender-preserving copies (Superior Spider-Man) —
/// `"except <possessive> name is <card name>"` and
/// `"<subject pronoun>'s N/M {type list} in addition to its other types"`.
///
/// The input is the already-lowercased, trimmed portion of the Oracle line
/// after the source-zone clause (`"on the battlefield"` / `"in a graveyard"`).
///
/// Returns `(mana_value_limit, modifications, post_period)` where `post_period`
/// is the text remaining after the optional sentence-terminating `.` — used by
/// the caller to parse a trailing "When you do, ..." reflexive clause.
///
/// Fail-soft: the parser is **total** over the input. Any unrecognized leading
/// fragment yields defaults (`None`, `vec![]`) so the caller can still register
/// the plain `BecomeCopy` replacement. This preserves correctness for cards
/// whose `except` clause is not yet understood (e.g. Vesuvan Doppelganger's
/// "doesn't copy that creature's color") rather than dropping their clone
/// behaviour entirely.
fn parse_clone_suffix<'a>(
    suffix: &'a str,
    card_name: &str,
) -> (
    Option<CopyManaValueLimit>,
    Option<Duration>,
    Vec<ContinuousModification>,
    &'a str,
) {
    let (remaining, mana_value_limit) =
        parse_mana_value_limit_clause(suffix).unwrap_or((suffix, None));
    // CR 611.3 + CR 613.1a: "until end of turn" (and other duration phrases from
    // `oracle_nom::duration::parse_duration`) qualify the copy effect to expire
    // at cleanup. Appears between the zone clause and the except clause on
    // Cursed Mirror; absent on Phantasmal Image / Clever Impersonator (permanent).
    let (remaining, duration) = parse_leading_duration(remaining);
    // Replacement-form clones don't have a "current trigger" — `has this
    // ability` arms inside an except clause decline gracefully when the
    // context's `current_trigger_index` is `None`.
    let (post_except, modifications) =
        parse_except_clause(remaining, card_name, ExceptClauseContext::default())
            .unwrap_or((remaining, Vec::new()));

    (mana_value_limit, duration, modifications, post_except)
}

/// Parse an optional leading duration phrase off the clone-replacement suffix.
/// The caller may have already trimmed leading whitespace, so this consumes an
/// optional leading space before delegating to the shared `parse_duration` nom
/// combinator. Fail-soft: returns `(input, None)` when no duration is present.
fn parse_leading_duration(suffix: &str) -> (&str, Option<Duration>) {
    let body = suffix.strip_prefix(' ').unwrap_or(suffix);
    match parse_duration(body) {
        Ok((rest, d)) => (rest, Some(d)),
        Err(_) => (suffix, None),
    }
}

/// CR 614.1c: " with mana value less than or equal to the amount of mana spent to cast {self_ref}".
/// Matches at the start of `suffix`; returns the remainder (still lowercase) and the typed limit.
fn parse_mana_value_limit_clause(suffix: &str) -> Option<(&str, Option<CopyManaValueLimit>)> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>(
        "with mana value less than or equal to the amount of mana spent to cast ",
    )
    .parse(suffix)
    .ok()?;
    // Self-reference: the normalizer rewrites the card name to "~" but
    // Oracle text commonly also uses "this creature" verbatim.
    let (rest, _) = alt((tag::<_, _, VerboseError<&str>>("this creature"), tag("~")))
        .parse(rest)
        .ok()?;
    Some((rest, Some(CopyManaValueLimit::AmountSpentToCastSource)))
}

// CR 707.9 + CR 707.9b + CR 707.9a: The `, except <body>` clause grammar lives
// in `oracle_effect/become_copy_except.rs` so both the replacement-form clones
// (`parse_clone_replacement` above) and the triggered-form copies (Irma's
// "becomes a copy of … except her name is ~ and she has this ability") share
// one parser. See that module for the recognised body shapes.

/// Parse check land pattern: "enters tapped unless you control a [LandType] or a [LandType]"
/// Returns Mandatory ReplacementDefinition with an UnlessControlsSubtype condition.
/// Shared dispatcher for all conditional "enters tapped unless X" patterns (CR 614.1d).
/// Tries typed condition extractors in priority order, falling back to generic Unrecognized.
/// Shock lands are excluded — they use ReplacementMode::Optional with a decline path.
fn parse_enters_tapped_unless(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !nom_primitives::scan_contains(norm_lower, "enters tapped")
        && !nom_primitives::scan_contains(norm_lower, "enters the battlefield tapped")
    {
        return None;
    }

    // Try typed condition extractors in priority order:
    // Fast lands BEFORE check lands (both match "unless you control").
    // Check lands BEFORE controls_typed (more specific subtype match).
    let condition = parse_fast_condition(norm_lower)
        .or_else(|| parse_check_condition(norm_lower))
        .or_else(|| parse_controls_typed_condition(norm_lower))
        .or_else(|| parse_opponents_control_condition(norm_lower))
        .or_else(|| parse_player_life_condition(norm_lower))
        .or_else(|| parse_multiple_opponents_condition(norm_lower))
        .or_else(|| parse_your_turn_condition(norm_lower))
        .or_else(|| parse_turn_of_game_condition(norm_lower))
        .or_else(|| parse_generic_unless_condition(norm_lower, original_text))?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Tap {
                    target: TargetFilter::SelfRef,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .description(original_text.to_string())
            .condition(condition),
    )
}

/// Extract check land condition: "unless you control a [LandType] or a [LandType]"
fn parse_check_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless you control ")?;
    let rest = rest.trim_end_matches('.');

    let mut subtypes = Vec::new();
    for part in rest.split(" or ") {
        let trimmed = part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ");
        let canonical = canonical_land_subtype(trimmed)?;
        if !subtypes.contains(&canonical) {
            subtypes.push(canonical);
        }
    }

    if subtypes.is_empty() {
        return None;
    }

    Some(ReplacementCondition::UnlessControlsSubtype { subtypes })
}

/// Extract fast land condition: "unless you control N or fewer other [type]"
/// CR 305.7 + CR 614.1c — fast lands (Spirebluff Canal, Blackcleave Cliffs, etc.).
/// Delegates to `nom_primitives::parse_number` for the count (input already lowercase).
fn parse_fast_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless you control ")?;

    // Parse "two or fewer other lands." → count=2, remainder="or fewer other lands."
    let (nom_rest, count) = nom_primitives::parse_number.parse(rest).ok()?;
    let after_number = nom_rest.trim_start();
    let (after_or_fewer, _) = tag::<_, _, VerboseError<&str>>("or fewer ")
        .parse(after_number.trim_start())
        .ok()?;
    let type_text = after_or_fewer.trim_end_matches('.');

    // parse_type_phrase handles "other lands" → TypedFilter { Land, [Another] }
    let (filter, leftover) = parse_type_phrase(type_text);
    if !leftover.trim().is_empty() {
        return None;
    }

    // Extract TypedFilter and inject ControllerRef::You (not visible in the parsed fragment)
    let typed_filter = match filter {
        TargetFilter::Typed(tf) => tf.controller(ControllerRef::You),
        _ => return None,
    };

    Some(ReplacementCondition::UnlessControlsOtherLeq {
        count,
        filter: typed_filter,
    })
}

/// Map lowercase land subtype name to canonical (title-cased) form.
fn canonical_land_subtype(raw: &str) -> Option<String> {
    match raw {
        "plains" => Some("Plains".to_string()),
        "island" => Some("Island".to_string()),
        "swamp" => Some("Swamp".to_string()),
        "mountain" => Some("Mountain".to_string()),
        "forest" => Some("Forest".to_string()),
        _ => None,
    }
}

/// Extract general "unless you control a [type phrase]" condition (CR 614.1d).
/// Handles basic lands, legendary creatures, Mount/Vehicle, etc.
/// Also handles "unless you control N or more [type]" quantity prefix patterns.
fn parse_controls_typed_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless you control ")?;

    // Try "N or more [type]" pattern first (e.g., "two or more other lands")
    if let Some((minimum, type_text)) = try_parse_quantity_prefix(rest) {
        let (filter, leftover) = parse_type_phrase(type_text);
        if !leftover.trim().trim_end_matches('.').is_empty() || filter == TargetFilter::Any {
            return None;
        }
        let filter = inject_controller(filter, ControllerRef::You);
        return Some(ReplacementCondition::UnlessControlsCountMatching { minimum, filter });
    }

    // Strip leading article — parse_type_phrase does NOT handle "a "/"an "
    let rest = rest.trim_start_matches("a ").trim_start_matches("an ");

    let (filter, leftover) = parse_type_phrase(rest);
    // Reject partial parse — all text must be consumed (modulo trailing period)
    if !leftover.trim().trim_end_matches('.').is_empty() {
        return None;
    }

    // Reject if parse_type_phrase returned Any (nothing meaningful parsed)
    if filter == TargetFilter::Any {
        return None;
    }

    // Inject ControllerRef::You — "you control" is implicit in the Oracle text
    // CR 614.1d — consistent with fast land controller injection pattern
    let filter = inject_controller(filter, ControllerRef::You);

    Some(ReplacementCondition::UnlessControlsMatching { filter })
}

/// Extract "unless your opponents control N or more [type]" condition.
/// CR 614.1d — sibling of `parse_controls_typed_condition` keyed on the
/// "your opponents control" prefix. Only the quantity-prefixed form is accepted
/// (this phrasing always appears with a threshold in printed MTG text).
/// Used by the Turbulent land cycle (SOC): "unless your opponents control eight or more lands".
fn parse_opponents_control_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless your opponents control ")?;
    let (minimum, type_text) = try_parse_quantity_prefix(rest)?;
    let (filter, leftover) = parse_type_phrase(type_text);
    if !leftover.trim().trim_end_matches('.').is_empty() || filter == TargetFilter::Any {
        return None;
    }
    // CR 109.5: stamp ControllerRef::Opponent so the runtime filter counts
    // only permanents controlled by opponents of the entering permanent's controller.
    let filter = inject_controller(filter, ControllerRef::Opponent);
    Some(ReplacementCondition::UnlessControlsCountMatching { minimum, filter })
}

/// Try to parse "N or more " quantity prefix before a type phrase.
/// Returns (minimum, remainder) if matched.
/// Delegates to `nom_primitives::parse_number` for the count (input already lowercase).
fn try_parse_quantity_prefix(text: &str) -> Option<(u32, &str)> {
    let (nom_rest, n) = nom_primitives::parse_number.parse(text).ok()?;
    let (type_text, _) = tag::<_, _, VerboseError<&str>>("or more ")
        .parse(nom_rest.trim_start())
        .ok()?;
    Some((n, type_text))
}

/// Inject a `ControllerRef` into every `Typed` leaf of a `TargetFilter`.
/// CR 109.5 — ownership/control reference is attached to each leaf typed filter,
/// recursing through compound `Or` / `And` / `Not` wrappers so any leaf under a
/// compound filter is stamped. Non-typed leaves (context refs, specific objects,
/// etc.) are preserved untouched.
fn inject_controller(filter: TargetFilter, controller: ControllerRef) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(tf.controller(controller)),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .into_iter()
                .map(|f| inject_controller(f, controller.clone()))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .into_iter()
                .map(|f| inject_controller(f, controller.clone()))
                .collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(inject_controller(*filter, controller)),
        },
        other => other,
    }
}

/// Extract life payment amount from "pay N life" pattern.
fn extract_life_payment(text: &str) -> Option<i32> {
    let after_pay = strip_after(text, "pay ")?;
    let (_rem, value) = nom_primitives::parse_number.parse(after_pay).ok()?;
    Some(value as i32)
}

/// CR 107.3m: In the ETB-enters-with-counters context, bare "X" refers to the
/// mana value paid for `{X}` on the cast. `parse_count_expr` emits
/// `QuantityRef::Variable{name:"X"}` for bare X, which at runtime resolves via
/// the current trigger event's source — a channel that is empty during ETB
/// replacement application. Rewriting to `QuantityRef::CostXPaid` reads the
/// entering object's own `cost_x_paid` field, which is populated by
/// `finalize_cast` and survives the stack → battlefield move. Walks the
/// expression tree so `Multiply { factor: 2, inner: Variable("X") }` (Primo)
/// and `HalfRounded { inner: Variable("X"), .. }` also get the rewrite.
pub(crate) fn rewrite_variable_x_to_cost_x_paid(expr: &mut QuantityExpr) {
    match expr {
        QuantityExpr::Ref { qty } => {
            if matches!(qty, QuantityRef::Variable { name } if name == "X") {
                *qty = QuantityRef::CostXPaid;
            }
        }
        QuantityExpr::Fixed { .. } => {}
        QuantityExpr::HalfRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => rewrite_variable_x_to_cost_x_paid(inner),
    }
}

/// Parse "enters/escapes with N [type] counter(s)" patterns into a Moved replacement.
/// Handles self ("~ enters with"), other ("each other creature ... enters with"),
/// escape ("~ escapes with", CR 702.138c), and kicker-conditional
/// ("if ~ was kicked, it enters with", CR 702.33d).
fn parse_enters_with_counters(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Detect kicker-conditional prefix: "if ~ was kicked [with its {cost} kicker], it enters with"
    // CR 702.33d: kicker condition gates the replacement effect.
    let (kicker_condition, work_text) = extract_kicker_enters_condition(norm_lower);

    // CR 702.138c: "escapes with" is semantically "enters with" gated on escape.
    // Use nom take_until to scan for the "escapes with" phrase at word boundaries.
    let is_escape = take_until::<_, _, VerboseError<&str>>("escapes with")
        .parse(work_text)
        .is_ok();

    // Find "with [N] [type] counter" to extract count and counter type.
    // For escape, the "with" follows "escapes"; for enters, it follows "enters".
    let after_with = strip_after(work_text, "with ")?;
    // Skip "an additional" if present
    let after_additional = alt((
        tag::<_, _, VerboseError<&str>>("an additional "),
        tag("additional "),
    ))
    .parse(after_with)
    .map_or(after_with, |(rest, _)| rest);
    // Detect dynamic count: "a number of [type] counters ... equal to [qty]"
    let (dynamic_remainder, after_prefix) =
        match tag::<_, _, VerboseError<&str>>("a number of ").parse(after_additional) {
            Ok((rest, _)) => (Some(after_additional), rest),
            Err(_) => (None, after_additional),
        };
    // CR 107.3 + CR 107.3m + CR 107.1a: Parse the counter count as a full
    // `QuantityExpr`, so "N", "X", "twice X", "three times X", and
    // "half X, rounded up/down" all compose through the same typed arithmetic
    // wrappers (`Multiply`, `HalfRounded`). `parse_count_expr` returns
    // `Variable("X")` for bare X; the ETB-enters context requires the entering
    // object's `cost_x_paid` (runtime `Variable("X")` only reads trigger-event
    // sources, not the entering permanent), so rewrite X → `CostXPaid`
    // recursively inside the expression.
    let (mut count_expr, rest) =
        parse_count_expr(after_prefix).unwrap_or((QuantityExpr::Fixed { value: 1 }, after_prefix));
    rewrite_variable_x_to_cost_x_paid(&mut count_expr);
    // Next word(s) before "counter" are the counter type
    let (_, (counter_type_raw, _)) = nom_primitives::split_once_on(rest, "counter").ok()?;
    let counter_type_raw = counter_type_raw.trim();
    let counter_type =
        crate::parser::oracle_effect::counter::normalize_counter_type(counter_type_raw);
    // CR 122.6: For "a number of counters equal to [quantity]", parse the dynamic expression
    if dynamic_remainder.is_some() {
        if let Ok((_, (_, qty_text))) = nom_primitives::split_once_on(work_text, "equal to ") {
            let trimmed = qty_text.trim().trim_end_matches('.');
            if let Some(qty_ref) = crate::parser::oracle_quantity::parse_quantity_ref(trimmed) {
                count_expr = QuantityExpr::Ref { qty: qty_ref };
            } else if let Some(qty) = crate::parser::oracle_quantity::parse_cda_quantity(trimmed) {
                count_expr = qty;
            }
        }
    }

    let put_counter = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type,
            count: count_expr,
            target: TargetFilter::SelfRef,
        },
    );

    // Determine valid_card filter: self vs other creatures
    // Strip "each other " or "other " prefix, then delegate to parse_type_phrase
    // which handles non-X, controller, "of the chosen type", etc.
    let subject = alt((
        tag::<_, _, VerboseError<&str>>("each other "),
        tag("other "),
    ))
    .parse(work_text)
    .ok()
    .map(|(rest, _)| rest)
    .filter(|s| {
        nom_primitives::scan_contains(s, "creature")
            || nom_primitives::scan_contains(s, "permanent")
    });
    let valid_card = if let Some(subject_text) = subject {
        let (filter, _) = parse_type_phrase(subject_text);
        // Inject Another since we stripped "other" above
        let filter = match filter {
            TargetFilter::Typed(TypedFilter {
                type_filters,
                controller,
                mut properties,
            }) => {
                properties.insert(0, FilterProp::Another);
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                })
            }
            other => other,
        };
        Some(filter)
    } else {
        Some(TargetFilter::SelfRef)
    };

    // CR 614.12: External ETB counter placements (non-SelfRef) use ChangeZone
    // so tokens also receive counters (e.g., Grumgully + creature tokens).
    // Self-ETB (SelfRef) stays on Moved — tokens don't carry parser-generated
    // replacement definitions, so ChangeZone matching would be wasted work.
    let is_external = !matches!(valid_card, Some(TargetFilter::SelfRef) | None);
    let event = if is_external {
        ReplacementEvent::ChangeZone
    } else {
        ReplacementEvent::Moved
    };
    let mut def = ReplacementDefinition::new(event)
        .execute(put_counter)
        .description(original_text.to_string());
    if let Some(filter) = valid_card {
        def = def.valid_card(filter);
    }
    if is_external {
        def = def.destination_zone(Zone::Battlefield);
    }

    // Apply condition: escape or kicker
    if is_escape {
        def = def.condition(ReplacementCondition::CastViaEscape);
    } else if let Some(cond) = kicker_condition {
        def = def.condition(cond);
    }

    Some(def)
}

/// CR 614.1c + CR 601.2: Parse "Whenever you cast a [spell], that [subject]
/// enters with [an additional] [count] [type] counter(s) on it[, where X is
/// [quantity]]" as a replacement effect on the *cast spell itself*.
///
/// Despite the "whenever you cast" framing, CR 614.1c classifies "enters with"
/// as a replacement effect, not a triggered ability. Wildgrowth Archaic and its
/// cousin family (Runadi, Boreal Outrider, Torgal, …) all share this shape.
///
/// Composition:
///   "whenever you cast " → spell filter → ", that " → subject →
///   " enters with " → count-prefix → counter-type → " counter(s) on it"
///   [", where x is " → quantity ref] [trailing punctuation]
fn parse_whenever_you_cast_enters_with(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Prefix.
    let (rest, _) = tag::<_, _, VerboseError<&str>>("whenever you cast ")
        .parse(norm_lower)
        .ok()?;

    // Drop the article before the spell filter.
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>("a "),
        tag("an "),
        tag("another "),
    ))
    .parse(rest)
    .ok()?;

    // Spell filter — split on ", that " to isolate the filter text from the subject.
    // `split_once_on` returns `Ok(("", (prefix, suffix)))`.
    let (_, (spell_filter_text, after_that_text)) =
        nom_primitives::split_once_on(rest, ", that ").ok()?;
    let (spell_filter, filter_rest) = parse_type_phrase(spell_filter_text);
    // Require that the spell filter cleanly consumed its text (modulo trailing
    // "spell" token which parse_type_phrase leaves in the remainder on some paths).
    let filter_rest = filter_rest.trim();
    if !filter_rest.is_empty() && filter_rest != "spell" && filter_rest != "spells" {
        return None;
    }
    let TargetFilter::Typed(mut spell_typed) = spell_filter else {
        return None;
    };
    // The Oracle text says "you cast" — constrain to the controller.
    spell_typed.controller = Some(ControllerRef::You);

    // Subject — "creature", "permanent", or "spell" — and " enters with ".
    let (rest, _subject) = alt((
        tag::<_, _, VerboseError<&str>>("creature "),
        tag("permanent "),
        tag("spell "),
    ))
    .parse(after_that_text)
    .ok()?;
    let (rest, _) = tag::<_, _, VerboseError<&str>>("enters with ")
        .parse(rest)
        .ok()?;

    // Count prefix: "an additional" | "N additional" | plain "N" | "x additional" | "x".
    // Mirrors `try_parse_enters_with_additional_counters` — the Wildgrowth
    // family always uses "additional" but the underlying shape matches.
    let (rest, fixed_count) =
        if let Ok((r, _)) = tag::<_, _, VerboseError<&str>>("an additional ").parse(rest) {
            (r, Some(1u32))
        } else if let Ok((r, _)) = alt((
            tag::<_, _, VerboseError<&str>>("x additional "),
            tag("X additional "),
        ))
        .parse(rest)
        {
            // X is dynamic — actual value comes from the trailing "where X is …" clause.
            (r, None)
        } else if let Ok((r, n)) = nom_primitives::parse_number(rest) {
            let (r, _) = tag::<_, _, VerboseError<&str>>(" additional ")
                .parse(r)
                .or_else(|_| tag::<_, _, VerboseError<&str>>(" ").parse(r))
                .ok()?;
            (r, Some(n))
        } else {
            return None;
        };

    // Counter type.
    let (rest, counter_type) = alt((
        value("P1P1".to_string(), tag::<_, _, VerboseError<&str>>("+1/+1")),
        value("M1M1".to_string(), tag("-1/-1")),
    ))
    .parse(rest)
    .ok()?;

    // " counter on it" / " counters on it" with optional trailing punctuation.
    let (rest, _) = alt((
        tag::<_, _, VerboseError<&str>>(" counter on it"),
        tag(" counters on it"),
    ))
    .parse(rest)
    .ok()?;

    // Optional trailing "where X is [quantity]" clause.
    let count_expr = match fixed_count {
        Some(n) => QuantityExpr::Fixed { value: n as i32 },
        None => {
            // Expect ", where x is " then a quantity ref.
            let (rest, _) = alt((
                tag::<_, _, VerboseError<&str>>(", where x is "),
                tag(", where X is "),
            ))
            .parse(rest)
            .ok()?;
            let qty_text = rest.trim_end_matches('.').trim();
            let qty = crate::parser::oracle_quantity::parse_quantity_ref(qty_text)?;
            QuantityExpr::Ref { qty }
        }
    };

    let put_counter = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type,
            count: count_expr,
            target: TargetFilter::SelfRef,
        },
    );

    // CR 614.12: External ETB counter placement — use ChangeZone so tokens
    // entering the battlefield also receive counters (Metallic Mimic + creature tokens).
    Some(
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(put_counter)
            .valid_card(TargetFilter::Typed(spell_typed))
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

/// Extract kicker-conditional prefix from "if ~ was kicked [with its {cost} kicker], it enters with..."
/// Returns `(Option<ReplacementCondition>, remaining_text)` where remaining_text has the
/// conditional prefix stripped (just "it enters with..." or the original text if no prefix).
/// CR 702.33d
fn extract_kicker_enters_condition(norm_lower: &str) -> (Option<ReplacementCondition>, &str) {
    // CR 702.33d: Parse "if ~ was kicked [with its {cost} kicker], it enters with..."
    // using nom combinators for structured dispatch.
    let after_if = match tag::<_, _, VerboseError<&str>>("if ").parse(norm_lower) {
        Ok((rest, _)) => rest,
        Err(_) => return (None, norm_lower),
    };

    // Subject can be "~", "it", "this creature", etc. — scan to "was kicked".
    let after_kicked = match take_until::<_, _, VerboseError<&str>>("was kicked")
        .parse(after_if)
        .and_then(|(rest, _)| tag::<_, _, VerboseError<&str>>("was kicked").parse(rest))
    {
        Ok((rest, _)) => rest,
        Err(_) => return (None, norm_lower),
    };

    // Optional "with its {cost} kicker" variant specification
    let (cost_text, after_kicker_clause) =
        match tag::<_, _, VerboseError<&str>>(" with its ").parse(after_kicked) {
            Ok((rest, _)) => {
                match take_until::<_, _, VerboseError<&str>>(" kicker").parse(rest) {
                    Ok((rest2, cost_str)) => {
                        // Consume " kicker" tag
                        match tag::<_, _, VerboseError<&str>>(" kicker").parse(rest2) {
                            Ok((rest3, _)) => (Some(cost_str.trim().to_string()), rest3),
                            Err(_) => (None, after_kicked),
                        }
                    }
                    Err(_) => (None, after_kicked),
                }
            }
            Err(_) => (None, after_kicked),
        };

    // Expect ", it enters with" or ", it enters the battlefield with"
    let enters_result = alt((
        tag::<_, _, VerboseError<&str>>(", it enters with"),
        tag(", it enters the battlefield with"),
    ))
    .parse(after_kicker_clause);

    match enters_result {
        Ok(_) => {
            // Reconstruct the enters-with text for downstream parsing.
            let enters_start = norm_lower.len() - after_kicker_clause.len() + 2; // skip ", "
            let condition = ReplacementCondition::CastViaKicker { cost_text };
            (Some(condition), &norm_lower[enters_start..])
        }
        Err(_) => (None, norm_lower),
    }
}

fn replacement_condition_from_static(condition: StaticCondition) -> Option<ReplacementCondition> {
    match condition {
        StaticCondition::SourceIsTapped => {
            Some(ReplacementCondition::SourceTappedState { tapped: true })
        }
        StaticCondition::Not { condition } if *condition == StaticCondition::SourceIsTapped => {
            Some(ReplacementCondition::SourceTappedState { tapped: false })
        }
        _ => None,
    }
}

fn parse_external_entry_suffix(stripped: &str) -> Option<(&str, bool)> {
    stripped
        .strip_suffix(" enter tapped")
        .map(|subject| (subject, true))
        .or_else(|| {
            stripped
                .strip_suffix(" enters tapped")
                .map(|subject| (subject, true))
        })
        .or_else(|| {
            stripped
                .strip_suffix(" enter untapped")
                .map(|subject| (subject, false))
        })
        .or_else(|| {
            stripped
                .strip_suffix(" enters untapped")
                .map(|subject| (subject, false))
        })
}

fn build_external_entry_replacement(
    subject: &str,
    original_text: &str,
    enters_tapped: bool,
) -> Option<ReplacementDefinition> {
    if subject.contains('~') {
        return None;
    }

    let (filter, rest) = parse_type_phrase(subject);
    if !rest.trim().is_empty() {
        return None;
    }

    let effect = if enters_tapped {
        Effect::Tap {
            target: TargetFilter::SelfRef,
        }
    } else {
        Effect::Untap {
            target: TargetFilter::SelfRef,
        }
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(AbilityDefinition::new(AbilityKind::Spell, effect))
            .valid_card(filter)
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

fn parse_source_state_external_entry(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let (condition, rest) = nom_on_lower(original_text, norm_lower, |i| {
        let (i, _) = tag::<_, _, VerboseError<&str>>("as long as ").parse(i)?;
        let (i, condition) = parse_inner_condition(i)?;
        let (i, _) = tag(", ").parse(i)?;
        Ok((i, condition))
    })?;
    let condition = replacement_condition_from_static(condition)?;
    let rest_lower = rest.to_lowercase();
    let stripped = rest_lower.trim_end_matches('.');
    let (entry_subject, enters_tapped) = parse_external_entry_suffix(stripped)?;
    let mut def = build_external_entry_replacement(entry_subject, original_text, enters_tapped)?;
    def.condition = Some(condition);
    Some(def)
}

/// Parse "[Type] enter untapped" / "[Type] enters untapped" — external replacement effects.
fn parse_external_enters_untapped(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let stripped = norm_lower.trim_end_matches('.');
    let (subject, enters_tapped) = parse_external_entry_suffix(stripped)?;
    if enters_tapped {
        return None;
    }
    build_external_entry_replacement(subject, original_text, false)
}

/// Parse "[Type] enter tapped" / "[Type] enters tapped" — external replacement effects.
/// E.g., "Creatures your opponents control enter tapped." (Authority of the Consuls)
/// E.g., "Artifacts and creatures your opponents control enter tapped." (Blind Obedience)
fn parse_external_enters_tapped(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let stripped = norm_lower.trim_end_matches('.');
    let (subject, enters_tapped) = parse_external_entry_suffix(stripped)?;
    if !enters_tapped {
        return None;
    }
    build_external_entry_replacement(subject, original_text, true)
}

/// CR 614.1a: Parse "If [filter] would die, …instead…" replacement effects.
/// Handles non-self creature filters like "another creature", "a nontoken
/// creature an opponent controls", "a creature an opponent controls", and
/// recognizes the exile-anaphor in either word order via
/// [`parse_exile_anaphor_clause`] (see that function for the prefix vs.
/// suffix grammar). Compound effects whose verb isn't a bare exile-anaphor
/// (e.g., "exile that card with an ice counter on it instead", "return it
/// to its owner's hand instead") fall through to the generic chain parser.
fn parse_creature_die_exile_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Must contain "would die" and "instead" (exile-instead pattern).
    let (before_die, _) = nom_primitives::scan_split_at_phrase(norm_lower, |i| {
        tag::<_, _, VerboseError<&str>>("would die").parse(i)
    })?;
    let would_die_pos = before_die.len();
    if !nom_primitives::scan_contains(norm_lower, "instead") {
        return None;
    }

    // Extract the subject between "if " and " would die".
    let subject_start = {
        let prefix = norm_lower.strip_prefix("if ")?;
        // Subject is everything from after "if " to before " would die"
        let subject_end_in_prefix = would_die_pos - "if ".len();
        prefix[..subject_end_in_prefix].trim()
    };

    // Skip self-reference patterns — handled by the earlier "~ would die" check.
    if subject_start.contains('~') {
        return None;
    }

    // Parse the subject filter (e.g., "another creature", "a nontoken creature an opponent controls")
    // Also detect inline conditions like "dealt damage this turn by a source you controlled"
    let (filter, subject_rest) = parse_type_phrase(subject_start);
    if matches!(&filter, TargetFilter::Any) {
        return None;
    }

    // CR 120.1: Check for "dealt damage this turn by a source you controlled" condition.
    let replacement_condition = if let Ok((_, _)) =
        tag::<_, _, VerboseError<&str>>("dealt damage this turn by a source you controlled")
            .parse(subject_rest.trim())
    {
        Some(
            crate::types::ability::ReplacementCondition::DealtDamageThisTurnBySourceControlledBy {
                controller: crate::types::ability::ControllerRef::You,
            },
        )
    } else {
        None
    };

    // Extract the replacement effect after "would die, " via a nom combinator.
    // CR 614.1a: Replacement effects use "instead" — both word orders are equivalent:
    //   suffix form: "exile it instead [.]"  (Void Maw, Valentin, Vren)
    //   prefix form: "instead exile it [and <continuation>] [.]"  (Darkness Crystal,
    //                Kalitas, Ravenloft Adventurer, Ravenous Slime, Doctor's Tomb)
    let after_would_die = &norm_lower[would_die_pos + "would die".len()..];
    let (effect_lower, _) = preceded(nom_primitives::ws, tag::<_, _, VerboseError<&str>>(", "))
        .parse(after_would_die)
        .ok()?;

    // Original-case slice covering the same bytes as effect_lower for chain parsing.
    let effect_offset = norm_lower.len() - effect_lower.len();
    let effect_orig = &original_text[effect_offset..];
    let effect_pair = TextPair::new(effect_orig, effect_lower)
        .trim_end()
        .trim_end_matches('.')
        .trim_end();

    // Match the exile-anaphor in either word order via nom alt(). The match
    // also lifts an inline `with N <type> counter(s) on it` modifier into
    // `enter_with_counters` so callers see counters on the resulting
    // ChangeZone (Draugr Necromancer's "with an ice counter", Rayami's "with
    // a blood counter", Darigaaz's "with three egg counters" via the self-die
    // branch). Compound suffix tails ("and you gain 2 life") are routed
    // through `parse_effect_chain` as sub-abilities.
    let anaphor = parse_exile_anaphor_clause(effect_pair);

    let execute = if anaphor.matched {
        // CR 614.1a: The anaphoric "it" / "that card" / "that creature" refers
        // to the object whose event is being replaced. In the replacement
        // pipeline, the execute effect's ChangeZone is used only for zone
        // redirection (destination extraction) — the affected object is already
        // known from the ProposedEvent. SelfRef is semantically correct:
        // "exile the same object this replacement is modifying," consistent
        // with how ETB-tapped replacements use SelfRef.
        let mut exile_self = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                // CR 122.1 + CR 614.1c: enter_with_counters is populated when
                // the anaphor clause carried a "with N <type> counter(s) on it"
                // modifier. Empty otherwise.
                enter_with_counters: anaphor.enter_with_counters,
            },
        );
        // CR 614.6: Trailing clauses (e.g., "and you gain 2 life", "and put a
        // hit counter on it") are additional effects that resolve as part of
        // the modified event. Attach them as sub_abilities — the chain parser
        // strips a leading "and " automatically.
        let continuation = anaphor.continuation.original.trim();
        if !continuation.is_empty() {
            let chain = parse_effect_chain(continuation, AbilityKind::Spell);
            exile_self = exile_self.sub_ability(chain);
        }
        exile_self
    } else {
        // Fall through: the effect text isn't a bare exile-anaphor clause —
        // hand the whole tail (with `instead` intact) to the chain parser.
        // This preserves prior coverage for compound effects like
        // "return it to its owner's hand instead" (Necromancer's Magemark).
        let orig_effect =
            if let Ok((_, (_, after))) = nom_primitives::split_once_on(original_text, ", ") {
                after.trim()
            } else {
                anaphor.continuation.original.trim()
            };
        parse_effect_chain(orig_effect, AbilityKind::Spell)
    };

    let mut def = ReplacementDefinition::new(ReplacementEvent::Destroy)
        .execute(execute)
        .valid_card(filter)
        .description(original_text.to_string());
    if let Some(cond) = replacement_condition {
        def = def.condition(cond);
    }
    Some(def)
}

/// CR 614.1a: Match the exile-anaphor clause in either word order, returning
/// the continuation text after the anaphor and whether a match occurred.
///
/// Recognizes both equivalent phrasings:
///   * **suffix form** — `"exile <anaphor> instead"` (Void Maw, Valentin, Vren)
///   * **prefix form** — `"instead exile <anaphor>"` (Darkness Crystal, Kalitas,
///     Ravenloft Adventurer, Ravenous Slime, Doctor's Tomb)
///
/// The anaphor is one of `"exile it"`, `"exile that card"`, `"exile that
/// creature"`. Any text remaining after the matched clause (e.g.,
/// `" and you gain 2 life"`) is returned as the continuation `TextPair` for
/// downstream chain parsing.
///
/// Returns `(continuation, true)` when a clause matched (continuation = post-
/// anaphor remainder). Returns `(input, false)` when the leading content does
/// not match — the caller falls through to a generic `parse_effect_chain` on
/// the unmodified text, preserving coverage for compound effects like
/// `"exile that card with an ice counter on it instead"` (Draugr, Rayami) or
/// `"return it to its owner's hand instead"` (Necromancer's Magemark).
/// Outcome of `parse_exile_anaphor_clause`: continuation slice for any
/// trailing `and <effect>` clause, plus whether the anaphor matched and
/// (optionally) `enter_with_counters` lifted from a `with N <type> counter(s)
/// on it` modifier sandwiched between the anaphor and `instead` / end-of-input.
struct ExileAnaphorMatch<'a> {
    continuation: TextPair<'a>,
    matched: bool,
    enter_with_counters: Vec<(String, QuantityExpr)>,
}

fn parse_exile_anaphor_clause<'a>(input: TextPair<'a>) -> ExileAnaphorMatch<'a> {
    use nom::sequence::terminated;

    let lower = input.lower;
    let exile_anaphor = || {
        alt((
            tag::<_, _, VerboseError<&str>>("exile it"),
            tag("exile that card"),
            tag("exile that creature"),
        ))
    };

    // Optional `with N <type> counter(s) on it` modifier between the anaphor
    // and the `instead` / end-of-input. Mirrors `Token.enter_with_counters`
    // — see `parse_counter_suffix_body_combinator` in `oracle_effect/mod.rs`.
    // The leading space is consumed here so the body combinator sees a clean
    // input starting with the count.
    let with_counters = || {
        preceded(
            tag::<_, _, VerboseError<&str>>(" with "),
            crate::parser::oracle_effect::parse_counter_suffix_body_combinator,
        )
    };

    // Try prefix form first: "instead exile <anaphor> [with N counters on it]".
    // Then suffix form:    "exile <anaphor> [with N counters on it] instead".
    // The body shape is unified: the `with-counters` slot is optional in both
    // word orders.
    let parsed: nom::IResult<&str, Option<(String, QuantityExpr)>, VerboseError<&str>> = alt((
        // Prefix: "instead exile <anaphor> [with N counter(s) on it]"
        preceded(
            tag("instead "),
            preceded(exile_anaphor(), nom::combinator::opt(with_counters())),
        ),
        // Suffix: "exile <anaphor> [with N counter(s) on it] instead"
        terminated(
            preceded(exile_anaphor(), nom::combinator::opt(with_counters())),
            tag(" instead"),
        ),
    ))
    .parse(lower);

    match parsed {
        Ok((rest, counters_opt)) => {
            // Compute the byte offset where the continuation starts.
            let consumed = lower.len() - rest.len();
            let (_, continuation) = input.split_at(consumed);
            ExileAnaphorMatch {
                continuation,
                matched: true,
                enter_with_counters: counters_opt.into_iter().collect(),
            }
        }
        Err(_) => ExileAnaphorMatch {
            continuation: input,
            matched: false,
            enter_with_counters: Vec::new(),
        },
    }
}

/// CR 614.1a + CR 122.1: For the self-die `~ would die` branch, try to
/// recognize the exile-anaphor clause (with optional `with N <type> counter(s)
/// on it` modifier) on the post-`, ` slice and build a `ChangeZone`-to-Exile
/// execute ability with the counters lifted onto `enter_with_counters`.
///
/// Compound trailing clauses ("and you gain 2 life") are routed through
/// `parse_effect_chain` as sub-abilities, mirroring
/// `parse_creature_die_exile_replacement` for the non-self path.
fn self_die_exile_anaphor_execute(
    normalized: &str,
    original_text: &str,
) -> Option<AbilityDefinition> {
    // Find the boundary `, ` that separates "If ~ would die" from the
    // replacement effect text.
    let (_, (_before, after_norm)) = nom_primitives::split_once_on(normalized, ", ").ok()?;
    let after_norm_lower = after_norm.to_lowercase();

    // Compute the matching slice on the original (un-normalized) text so the
    // continuation TextPair preserves original case for downstream chain
    // parsing. The original may differ from `normalized` in case but lengths
    // match for the suffix portion.
    let after_orig =
        if let Ok((_, (_, after_orig))) = nom_primitives::split_once_on(original_text, ", ") {
            after_orig
        } else {
            return None;
        };

    let effect_pair = TextPair::new(after_orig, &after_norm_lower)
        .trim_end()
        .trim_end_matches('.')
        .trim_end();

    let anaphor = parse_exile_anaphor_clause(effect_pair);
    if !anaphor.matched {
        return None;
    }

    let mut exile_self = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            destination: Zone::Exile,
            origin: None,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: anaphor.enter_with_counters,
        },
    );
    let continuation = anaphor.continuation.original.trim();
    if !continuation.is_empty() {
        let chain = parse_effect_chain(continuation, AbilityKind::Spell);
        exile_self = exile_self.sub_ability(chain);
    }
    Some(exile_self)
}

/// Parse graveyard-destination zone-change replacements (CR 614.6).
///
/// Shared prefix: `"if <subject> would be put into <scope> graveyard[ from anywhere],"`.
/// Dispatches via `alt()` between two outcome branches:
///   * **exile**: "exile it instead." — Rest in Peace, Leyline of the Void.
///   * **shuffle-back**: "[reveal ~ and ]shuffle it into its owner's library instead." —
///     Nexus of Fate, Progenitus, Blightsteel/Darksteel Colossus, Legacy Weapon.
///
/// The affected object is not known until replacement resolution time, so the
/// anaphoric "it" is encoded as `TargetFilter::SelfRef` on a top-level
/// `Effect::ChangeZone` — `event_modifiers_for_ability` absorbs this as a
/// destination redirect (CR 614.1). For shuffle-back, the follow-up
/// Reveal(CR 701.20) + Shuffle(CR 701.24) actions hang off the `sub_ability`
/// chain and run via the mandatory post-replacement-effect hook after the
/// redirected ZoneChange physically resolves. Owner-routing (CR 400.3) is
/// enforced at the zone layer, which reads `obj.owner` when writing to a library.
fn parse_graveyard_exile_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    use nom::sequence::preceded;

    // Scope of the subject's destination graveyard. Valid-card filter is keyed
    // off this: "opponent's graveyard" ⇒ `Owned { controller: Opponent }`.
    #[derive(Clone)]
    enum Scope {
        Any,
        Opponent,
    }

    // The outcome clause ("exile it instead" or the shuffle-back phrasing)
    // determines what ChangeZone + sub_ability chain we emit.
    #[derive(Clone)]
    enum Outcome {
        Exile,
        ShuffleBack { reveal: bool },
    }

    let ((scope, outcome), _rest) = nom_on_lower(original_text, norm_lower, |i| {
        // Prefix: "if <subject> would be put into <scope> graveyard[ from anywhere], "
        let (i, _) = tag::<_, _, VerboseError<&str>>("if ").parse(i)?;
        // Subject: accept any phrase up to " would be put into " — covers
        // "a card", "a nontoken creature", "~", "a creature an opponent controls", …
        let (i, _) = take_until::<_, _, VerboseError<&str>>(" would be put into ").parse(i)?;
        let (i, _) = tag::<_, _, VerboseError<&str>>(" would be put into ").parse(i)?;
        let (i, scope) = alt((
            value(Scope::Opponent, tag("an opponent's graveyard")),
            value(Scope::Opponent, tag("an opponents graveyard")),
            value(Scope::Opponent, tag("opponent's graveyard")),
            value(
                Scope::Any,
                preceded(take_until(" graveyard"), tag(" graveyard")),
            ),
        ))
        .parse(i)?;
        let (i, _) = opt(tag(" from anywhere")).parse(i)?;
        let (i, _) = tag(", ").parse(i)?;

        // Outcome dispatch. The shuffle-back variant optionally prefixes
        // "reveal ~ and " (CR 701.20); the exile variant has no such prefix.
        let (i, outcome) = alt((
            value(Outcome::Exile, tag("exile it instead")),
            value(
                Outcome::ShuffleBack { reveal: true },
                tag("reveal ~ and shuffle it into its owner's library instead"),
            ),
            value(
                Outcome::ShuffleBack { reveal: false },
                tag("shuffle it into its owner's library instead"),
            ),
        ))
        .parse(i)?;

        Ok((i, (scope, outcome)))
    })?;

    // Destination routing is determined by the outcome branch.
    let destination = match &outcome {
        Outcome::Exile => Zone::Exile,
        Outcome::ShuffleBack { .. } => Zone::Library,
    };

    // CR 400.3 + CR 108.3: "opponent's graveyard" means cards owned by an opponent
    // (cards go to owner's graveyard, so ownership is the stable discriminant).
    let valid_card = match scope {
        Scope::Opponent => Some(TargetFilter::Typed(TypedFilter::default().properties(
            vec![FilterProp::Owned {
                controller: ControllerRef::Opponent,
            }],
        ))),
        Scope::Any => None,
    };

    // Build the ChangeZone redirect. `event_modifiers_for_ability` extracts only
    // the `destination` field from this top-level ChangeZone — other fields here
    // (owner_library, etc.) are inert metadata along the redirect path.
    let redirect = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            destination,
            origin: None,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
    );

    // For shuffle-back, attach the Reveal → Shuffle(Owner) chain as sub_ability.
    // The mandatory post-effect extractor at `replacement.rs` sees a top-level
    // ChangeZone and stashes `sub_ability` to run after the redirected move lands.
    let execute = match outcome {
        Outcome::Exile => redirect,
        Outcome::ShuffleBack { reveal } => {
            // CR 701.24: shuffle into owner's library. CR 400.3 is the owner-routing
            // authority — TargetFilter::Owner resolves to state.objects[source_id].owner,
            // correct under Mind Control / Threads of Disloyalty when control ≠ ownership.
            let shuffle = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Shuffle {
                    target: TargetFilter::Owner,
                },
            );
            let post = if reveal {
                // CR 701.20: reveal the affected object before shuffling.
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Reveal {
                        target: TargetFilter::SelfRef,
                    },
                )
                .sub_ability(shuffle)
            } else {
                shuffle
            };
            redirect.sub_ability(post)
        }
    };

    let mut def = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(execute)
        .destination_zone(Zone::Graveyard)
        .description(original_text.to_string());
    if let Some(filter) = valid_card {
        def = def.valid_card(filter);
    }
    Some(def)
}

/// CR 614.1a: Parse damage boost/reduction replacement effects.
/// Extracts modification formula, source filter, target filter, and combat scope.
fn parse_damage_modification_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // --- 1. Extract modification formula from the result clause ---
    // Scan for the modification formula at word boundaries using nom combinators.
    let modification = scan_damage_modification(norm_lower)?;

    // --- 2. Extract source filter from the subject clause (before "would deal") ---
    let source_filter = parse_damage_source_filter(norm_lower);

    // --- 3. Extract combat scope ---
    // Scan for "noncombat damage" / "combat damage" at word boundaries.
    // "noncombat" is tried first since "combat damage" is a substring of "noncombat damage".
    let combat_scope = scan_combat_scope(norm_lower);

    // --- 4. Extract target filter ---
    let target_filter = parse_damage_target_filter(norm_lower);

    let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .damage_modification(modification)
        .description(original_text.to_string());
    if let Some(sf) = source_filter {
        def = def.damage_source_filter(sf);
    }
    if let Some(tf) = target_filter {
        def = def.damage_target_filter(tf);
    }
    if let Some(cs) = combat_scope {
        def = def.combat_scope(cs);
    }
    Some(def)
}

/// Parse the damage source filter from the subject clause before "would deal".
fn parse_damage_source_filter(norm_lower: &str) -> Option<TargetFilter> {
    let (_, (subject, _)) = nom_primitives::split_once_on(norm_lower, "would deal").ok()?;
    let subject = subject.trim();

    // Handle ability word prefixes ("Revolt — ..., if a source you control")
    // by finding the last "if " clause, which contains the actual replacement condition.
    // Use split_once_on to extract the last "if " clause (for ability word prefixes).
    // rsplit equivalent: take everything after the last "if " occurrence.
    let subject = {
        let mut last = subject;
        let mut remaining = subject;
        while let Ok((_, (_, after))) = nom_primitives::split_once_on(remaining, "if ") {
            last = after;
            remaining = after;
        }
        last.trim()
    };

    // Self-reference: "~" after stripping "if"
    if subject == "~" {
        return Some(TargetFilter::SelfRef);
    }

    // Strip leading "a " or "an "
    let subject = nom_primitives::parse_article
        .parse(subject)
        .map_or(subject, |(rest, _)| rest)
        .trim();

    // "source you control" with optional qualifiers
    if let Some(prefix) = subject.strip_suffix("source you control") {
        let prefix = prefix.trim();
        let mut filter = TypedFilter::default().controller(ControllerRef::You);
        let mut props = Vec::new();

        if !prefix.is_empty() {
            // Check for "another" prefix — may appear alone or before a qualifier
            let qualifier = if prefix == "another" {
                props.push(FilterProp::Another);
                ""
            } else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("another ").parse(prefix)
            {
                props.push(FilterProp::Another);
                rest.trim()
            } else {
                prefix
            };

            // Check for color qualifier (e.g. "red")
            if let Some(color) = parse_color_word(qualifier) {
                props.push(FilterProp::HasColor { color });
            }
            // CR 205.4b: "noncreature" qualifier — negation via TypeFilter::Non
            else if let Ok((rest, _)) = tag::<_, _, VerboseError<&str>>("non").parse(qualifier) {
                let inner = match rest {
                    "creature" => TypeFilter::Creature,
                    "land" => TypeFilter::Land,
                    "artifact" => TypeFilter::Artifact,
                    "enchantment" => TypeFilter::Enchantment,
                    "planeswalker" => TypeFilter::Planeswalker,
                    other => TypeFilter::Subtype(capitalize_first(other)),
                };
                filter = filter.with_type(TypeFilter::Non(Box::new(inner)));
            }
            // Check for creature type qualifier (e.g. "giant")
            else if !qualifier.is_empty() {
                filter = filter.subtype(capitalize_first(qualifier));
            }
        }

        if !props.is_empty() {
            filter.properties = props;
        }
        return Some(TargetFilter::Typed(filter));
    }

    // "source you control" without explicit "source" word
    if subject.ends_with("you control") {
        return Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
    }

    // "a source" with no qualifier — no filter needed (matches any source)
    if subject == "source" {
        return None;
    }

    // "a spell" — no source filter (handled as general case for now)
    None
}

/// Parse the damage target filter from the clause after "damage".
/// Uses word-boundary scanning with nom combinators for target phrase matching.
fn parse_damage_target_filter(norm_lower: &str) -> Option<DamageTargetFilter> {
    // Most specific first: "to an opponent or a permanent an opponent controls"
    // must precede bare "to an opponent".
    let mut remaining = norm_lower;
    while !remaining.is_empty() {
        if let Ok((_, filter)) = parse_damage_target_phrase(remaining) {
            // Guard: opponent-only and player-only exclude "permanent" from the full text
            match filter {
                DamageTargetFilter::OpponentOnly | DamageTargetFilter::PlayerOnly
                    if nom_primitives::scan_contains(norm_lower, "permanent") =>
                {
                    // Skip — "permanent" present means this is OpponentOrTheirPermanents (already tried)
                }
                _ => return Some(filter),
            }
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// Nom combinator for damage target phrases. Most specific tags first.
fn parse_damage_target_phrase(
    input: &str,
) -> nom::IResult<&str, DamageTargetFilter, VerboseError<&str>> {
    alt((
        value(
            DamageTargetFilter::OpponentOrTheirPermanents,
            tag("to an opponent or a permanent an opponent controls"),
        ),
        value(
            DamageTargetFilter::CreatureOnly,
            alt((tag("to a creature"), tag("to that creature"))),
        ),
        value(DamageTargetFilter::OpponentOnly, tag("to an opponent")),
        value(
            DamageTargetFilter::PlayerOnly,
            alt((tag("to a player"), tag("to that player"))),
        ),
    ))
    .parse(input)
}

// ---------------------------------------------------------------------------
// Damage replacement combinators
// ---------------------------------------------------------------------------

/// Scan for damage modification formula at word boundaries using nom combinators.
fn scan_damage_modification(text: &str) -> Option<DamageModification> {
    if let Some(modification) =
        nom_primitives::scan_at_word_boundaries(text, parse_damage_modification_phrase)
    {
        return Some(modification);
    }
    // Fallback: "that much damage plus/minus N" uses strip_after for the number
    if let Some(rest) = strip_after(text, "that much damage plus ") {
        let (_rem, val) = nom_primitives::parse_number.parse(rest).ok()?;
        return Some(DamageModification::Plus { value: val });
    }
    if let Some(rest) = strip_after(text, "that much damage minus ") {
        let (_rem, val) = nom_primitives::parse_number.parse(rest).ok()?;
        return Some(DamageModification::Minus { value: val });
    }
    None
}

/// Nom combinator for damage modification phrases.
fn parse_damage_modification_phrase(
    input: &str,
) -> nom::IResult<&str, DamageModification, VerboseError<&str>> {
    alt((
        value(
            DamageModification::Double,
            alt((tag("double that damage"), tag("deals double that damage"))),
        ),
        value(
            DamageModification::Triple,
            alt((tag("triple that damage"), tag("deals triple that damage"))),
        ),
        value(
            DamageModification::SetToSourcePower,
            alt((
                tag("damage equal to ~'s power instead"),
                tag("deals damage equal to ~'s power"),
            )),
        ),
    ))
    .parse(input)
}

/// Scan for combat damage scope at word boundaries.
/// "noncombat" tried first since "combat damage" is a substring.
fn scan_combat_scope(text: &str) -> Option<CombatDamageScope> {
    nom_primitives::scan_at_word_boundaries(text, |input| {
        alt((
            value(
                CombatDamageScope::NoncombatOnly,
                tag::<_, _, VerboseError<&str>>("noncombat damage"),
            ),
            value(CombatDamageScope::CombatOnly, tag("combat damage")),
        ))
        .parse(input)
    })
}

fn parse_color_word(word: &str) -> Option<ManaColor> {
    match word {
        "white" => Some(ManaColor::White),
        "blue" => Some(ManaColor::Blue),
        "black" => Some(ManaColor::Black),
        "red" => Some(ManaColor::Red),
        "green" => Some(ManaColor::Green),
        _ => None,
    }
}

fn extract_replacement_effect(text: &str) -> Option<String> {
    // Find ", " after "would" or "instead" clause
    if let Some(effect) = strip_after(text, ", ").map(str::trim) {
        let lower = effect.to_lowercase();
        let effect = TextPair::new(effect, &lower)
            .trim_end()
            .trim_end_matches('.');
        let effect = effect
            .strip_suffix(" instead")
            .map_or(effect, |trimmed| trimmed.trim_end());
        if !effect.original.is_empty() {
            return Some(effect.original.to_string());
        }
    }
    None
}

fn parse_conditional_draw_replacement(text: &str, lower: &str) -> Option<ReplacementDefinition> {
    let ((condition_len, bonus), rest) = nom_on_lower(text, lower, |input| {
        let (input, _) = tag("as long as ").parse(input)?;
        let (input, condition_text) = take_until(", if you would draw ").parse(input)?;
        let (input, _) = tag(", if you would draw ").parse(input)?;
        let (input, _) = alt((tag("a card"), tag("one or more cards"))).parse(input)?;
        let (input, _) = tag(", you draw that many cards plus ").parse(input)?;
        let (input, bonus) = nom_primitives::parse_number.parse(input)?;
        let (input, _) = tag(" instead").parse(input)?;
        let (input, _) = opt(char('.')).parse(input)?;
        Ok((input, (condition_text.len(), bonus)))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let condition_start = "as long as ".len();
    let condition_end = condition_start + condition_len;
    let condition_text = &lower[condition_start..condition_end];
    let (condition_rest, condition) = parse_inner_condition(condition_text).ok()?;
    if !condition_rest.trim().is_empty() {
        return None;
    }
    let offset = i32::try_from(bonus).ok()?;

    let crate::types::ability::StaticCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    } = condition
    else {
        return None;
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::Draw)
            .condition(ReplacementCondition::OnlyIfQuantity {
                lhs,
                comparator,
                rhs,
                active_player_req: None,
            })
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount,
                        }),
                        offset,
                    },
                    target: TargetFilter::Controller,
                },
            ))
            .description(text.to_string()),
    )
}

/// CR 614.1a: Parse token creation replacement effects.
/// Handles "twice that many tokens" (Primal Vigor, Doubling Season, Parallel Lives)
/// and "those tokens plus [spec]" (Chatterfang — "that many 1/1 green Squirrel
/// creature tokens"; Donatello — "a Mutagen token").
fn parse_token_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    let modification_mode = parse_token_replacement_shape(lower)?;

    let mut def = ReplacementDefinition::new(ReplacementEvent::CreateToken)
        .description(original_text.to_string());

    match modification_mode {
        TokenReplacementShape::Double => {
            def = def.quantity_modification(QuantityModification::Double);
        }
        TokenReplacementShape::PlusSpec { spec } => {
            def = def.additional_token_spec(*spec);
        }
    }

    // Scope: "under your control" → restrict to controller's tokens
    if nom_primitives::scan_contains(lower, "under your control") {
        def = def.token_owner_scope(ControllerRef::You);
    }

    Some(def)
}

enum TokenReplacementShape {
    /// "twice that many tokens … are created instead" (Doubling Season).
    Double,
    /// "those tokens plus [spec] are created instead" (Chatterfang, Donatello).
    PlusSpec {
        spec: Box<crate::types::proposed_event::TokenSpec>,
    },
}

/// CR 614.1a: Nom dispatch on the two token-replacement shapes. Uses
/// `nom_on_lower` for case-preserving parsing and delegates token-spec
/// extraction to the existing `parse_token_description` building block.
fn parse_token_replacement_shape(lower: &str) -> Option<TokenReplacementShape> {
    // "twice that many" → Doubling Season pattern.
    if nom_on_lower(lower, lower, |i| {
        let (i, _) = take_until::<_, _, VerboseError<&str>>("twice that many").parse(i)?;
        let (i, _) = tag("twice that many").parse(i)?;
        Ok((i, ()))
    })
    .is_some()
    {
        return Some(TokenReplacementShape::Double);
    }

    // "those tokens plus <spec> (is|are) created instead" → Chatterfang / Donatello.
    // Extract the spec descriptor between "those tokens plus " and the trailing
    // "are/is created instead" clause using nom combinators.
    let ((descriptor_start, descriptor_len), _rest) = nom_on_lower(lower, lower, |i| {
        let (i, pre) = take_until::<_, _, VerboseError<&str>>("those tokens plus ").parse(i)?;
        let start_offset = pre.len() + "those tokens plus ".len();
        let (i, _) = tag("those tokens plus ").parse(i)?;
        let (_, descriptor) = alt((
            take_until::<_, _, VerboseError<&str>>(" are created instead"),
            take_until::<_, _, VerboseError<&str>>(" is created instead"),
        ))
        .parse(i)?;
        Ok((i, (start_offset, descriptor.len())))
    })?;

    let descriptor = lower
        .get(descriptor_start..descriptor_start + descriptor_len)?
        .trim();
    let token = super::oracle_effect::parse_token_description(descriptor)?;
    let spec = token_description_to_spec(&token)?;
    Some(TokenReplacementShape::PlusSpec {
        spec: Box::new(spec),
    })
}

/// CR 614.1a + CR 111.1: Parse Xorn-class subtype-gated additional-token
/// replacements. Matches the shape:
///
/// ```text
/// "If you would create one or more <subtype> tokens, instead create
///  those tokens plus an additional <subtype> token."
/// ```
///
/// Differs from `parse_token_replacement` (Chatterfang) in two ways:
/// (1) the original event already creates tokens of the listed subtype, so a
/// `ReplacementCondition::TokenSubtypeMatches` gate is emitted; (2) the
/// "instead create those tokens plus X" word order is inverted from
/// Chatterfang's "those tokens plus X are created instead." Manufactor
/// ("instead create one of each") shares the same prefix and is parsed
/// separately in Item 5b.
fn parse_xorn_subtype_token_replacement(
    lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Extract the subtype after "would create one or more ".
    // Stops at " tokens," — the comma separator before "instead create".
    let ((subtype_start, subtype_len), _) = nom_on_lower(lower, lower, |i| {
        let (i, pre) =
            take_until::<_, _, VerboseError<&str>>("would create one or more ").parse(i)?;
        let start_offset = pre.len() + "would create one or more ".len();
        let (i, _) = tag("would create one or more ").parse(i)?;
        let (_, subtype) = take_until::<_, _, VerboseError<&str>>(" tokens,").parse(i)?;
        Ok((i, (start_offset, subtype.len())))
    })?;

    let subtype_phrase = lower
        .get(subtype_start..subtype_start + subtype_len)?
        .trim();
    if subtype_phrase.is_empty() || subtype_phrase.contains(' ') {
        // Multi-word subtypes (e.g., "or more Treasure") indicate the prefix
        // didn't isolate a single canonical subtype — bail and let a future
        // multi-subtype branch (Manufactor) handle it.
        return None;
    }

    // Extract the additional-token descriptor after
    // "instead create those tokens plus [an additional ]?", up to a
    // terminating comma or "." Track the post-strip position via input length.
    let total_len = lower.len();
    let ((desc_start, desc_len, needs_article), _) = nom_on_lower(lower, lower, |i| {
        let (i, _) =
            take_until::<_, _, VerboseError<&str>>("instead create those tokens plus ").parse(i)?;
        let (i, _) = tag("instead create those tokens plus ").parse(i)?;
        // Strip the "additional " modifier (with its optional leading article)
        // so parse_token_description sees the canonical token tail. Factor as
        // (opt article) + required "additional " to avoid the cartesian-product
        // expansion of {a_, an_, ε} × additional_.
        let (i, _) = opt(value(
            (),
            preceded(opt(alt((tag("a "), tag("an ")))), tag("additional ")),
        ))
        .parse(i)?;
        let start_offset = total_len - i.len();
        let (i, article) = peek(opt(alt((
            tag::<_, _, VerboseError<&str>>("a "),
            tag("an "),
        ))))
        .parse(i)?;
        let needs_article = article.is_none();
        let (i, descriptor) = alt((
            take_until::<_, _, VerboseError<&str>>("."),
            nom::combinator::rest,
        ))
        .parse(i)?;
        Ok((i, (start_offset, descriptor.len(), needs_article)))
    })?;

    let descriptor_raw = lower.get(desc_start..desc_start + desc_len)?.trim();
    // CR 111.1: parse_token_description's count-prefix requirement
    // (parser/oracle_effect/token.rs:169-175) needs an article or numeric
    // count. Re-add "a " when the modifier strip above consumed the article.
    let descriptor_owned;
    let descriptor: &str = if needs_article {
        descriptor_owned = format!("a {descriptor_raw}");
        &descriptor_owned
    } else {
        descriptor_raw
    };
    let token = super::oracle_effect::parse_token_description(descriptor)?;
    let spec = token_description_to_spec(&token)?;

    // Capitalize the subtype to match the parser's existing convention
    // (TokenSpec.subtypes uses title-case: "Treasure", not "treasure").
    let canonical_subtype = canonicalize_subtype(subtype_phrase);

    Some(
        ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .condition(ReplacementCondition::TokenSubtypeMatches {
                subtypes: vec![canonical_subtype],
            })
            .additional_token_spec(spec)
            .description(original_text.to_string()),
    )
}

/// Title-case a single-word subtype string for canonical TokenSpec storage.
/// "treasure" → "Treasure". Mirrors the existing parser convention; if a
/// shared subtype-canonicalization helper lands later this function should
/// delegate to it.
fn canonicalize_subtype(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// CR 614.1a + CR 111.1: Parse Manufactor-class ensure-all token replacements.
/// Matches the shape:
///
/// ```text
/// "If you would create a <S1>, <S2>, or <S3> token, instead create
///  one of each."
/// ```
///
/// (or any 2+ subtype list with `, or ` before the final entry). Returns a
/// `ReplacementDefinition` whose:
///
/// - `condition` is `TokenSubtypeMatches { subtypes: [S1, S2, S3] }` so the
///   replacement only fires for events whose proposed token spec carries one
///   of the listed subtypes;
/// - `ensure_token_specs` is the parallel list of full `TokenSpec`s, one per
///   subtype, synthesized via `parse_token_description("a <subtype> token")`.
///
/// CR 616.1 idempotence is enforced by the applier's `applied: HashSet` write
/// on each spawned `CreateToken` event, not here.
fn parse_manufactor_ensure_all_token_replacement(
    lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Extract the comma-separated subtype list between "would create a " and
    // " token,". Single combinator: locate the prefix, capture up to the
    // " token," terminator that precedes "instead create one of each".
    let total_len = lower.len();
    let ((list_start, list_len), _) = nom_on_lower(lower, lower, |i| {
        let (i, _) = take_until::<_, _, VerboseError<&str>>("would create a ").parse(i)?;
        let (i, _) = tag("would create a ").parse(i)?;
        let start_offset = total_len - i.len();
        let (i, list) = take_until::<_, _, VerboseError<&str>>(" token,").parse(i)?;
        Ok((i, (start_offset, list.len())))
    })?;

    let list_text = lower.get(list_start..list_start + list_len)?.trim();
    // `split_subtype_list` returns one entry for a single-subtype phrase; the
    // Xorn (single-subtype) shape is dispatched separately upstream, so a
    // <2-entry list at this site means the Manufactor shape didn't match.
    let subtypes = split_subtype_list(list_text);
    if subtypes.len() < 2 {
        return None;
    }

    let condition_subtypes: Vec<String> =
        subtypes.iter().map(|s| canonicalize_subtype(s)).collect();
    let mut specs: Vec<crate::types::proposed_event::TokenSpec> =
        Vec::with_capacity(subtypes.len());
    for sub in &subtypes {
        let descriptor = format!("a {sub} token");
        let token = super::oracle_effect::parse_token_description(&descriptor)?;
        specs.push(token_description_to_spec(&token)?);
    }

    Some(
        ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .condition(ReplacementCondition::TokenSubtypeMatches {
                subtypes: condition_subtypes,
            })
            .ensure_token_specs(specs)
            .description(original_text.to_string()),
    )
}

/// Split a Manufactor-style subtype list ("clue, food, or treasure") into
/// individual entries via nom combinators. Grammar:
///
/// ```text
/// list  := entry ( ", " ( "or " )? entry )+
/// entry := word
/// ```
///
/// The entry parser optionally consumes a leading "or " so the Oxford form
/// ("a, b, or c") and the simple form ("a, b") share one rule. Single-word
/// entries only; multi-word subtypes are not a known printed pattern for
/// this replacement class.
fn split_subtype_list(s: &str) -> Vec<String> {
    use nom::bytes::complete::take_while1;
    use nom::multi::separated_list1;
    use nom::IResult;

    fn entry(i: &str) -> IResult<&str, &str, VerboseError<&str>> {
        let (i, _) = opt(tag("or ")).parse(i)?;
        take_while1(|c: char| c.is_alphanumeric() || c == '-' || c == '\'').parse(i)
    }
    let mut list = separated_list1(tag(", "), entry);
    match list.parse(s) {
        Ok((_, parts)) => parts.into_iter().map(|p| p.to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

/// CR 111.1 + CR 111.4: Convert a parser-extracted `TokenDescription` into a
/// static `TokenSpec`. Source/controller are placeholder zeros — the applier
/// fills them with the replacement source's runtime identity. `sacrifice_at`
/// is `None` because the appended-token class (Chatterfang, Donatello) never
/// composes with duration-bound token keywords. Power/toughness resolution
/// uses the parser's `PtValue::Fixed` directly; variable P/T in an appended
/// spec is not a pattern any known card uses.
fn token_description_to_spec(
    token: &super::oracle_effect::TokenDescription,
) -> Option<crate::types::proposed_event::TokenSpec> {
    use crate::types::ability::PtValue;
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::proposed_event::TokenSpec;

    // Split parsed `types` into core_types vs subtypes by checking CoreType::from_str.
    let mut core_types: Vec<CoreType> = Vec::new();
    let mut subtypes: Vec<String> = Vec::new();
    for ty in &token.types {
        let trimmed = ty.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(core) = CoreType::from_str(trimmed) {
            if !core_types.contains(&core) {
                core_types.push(core);
            }
        } else {
            subtypes.push(trimmed.to_string());
        }
    }

    let fixed_or = |pt: Option<&PtValue>| -> Option<i32> {
        match pt? {
            PtValue::Fixed(v) => Some(*v),
            // Dynamic P/T in an appended spec is not supported by the current
            // pattern class — fall through to `None` (no P/T on the token).
            _ => None,
        }
    };
    let power = fixed_or(token.power.as_ref());
    let toughness = fixed_or(token.toughness.as_ref());
    let has_pt = power.is_some() || toughness.is_some();
    if has_pt && core_types.is_empty() {
        core_types.push(CoreType::Creature);
    }

    Some(TokenSpec {
        display_name: token.name.clone(),
        script_name: token.name.clone(),
        power,
        toughness,
        core_types,
        subtypes,
        supertypes: Vec::<Supertype>::new(),
        colors: token.colors.clone(),
        keywords: token.keywords.clone(),
        static_abilities: token.static_abilities.clone(),
        enter_with_counters: Vec::new(),
        tapped: token.tapped,
        enters_attacking: false,
        sacrifice_at: None,
        // Placeholder: overwritten at apply time with the replacement source's identity.
        source_id: crate::types::identifiers::ObjectId(0),
        controller: crate::types::player::PlayerId(0),
    })
}

/// CR 614.1a: Parse counter addition replacement effects.
/// Handles "twice that many ... counters" (Primal Vigor, Doubling Season)
/// and "that many plus N ... counters" (Hardened Scales, Branching Evolution).
fn parse_counter_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    let modification = if nom_primitives::scan_contains(lower, "twice that many") {
        QuantityModification::Double
    } else if let Some(rest) = strip_after(lower, "that many plus ") {
        // "that many plus one ... counters are put on it instead"
        // Delegate to nom_primitives::parse_number (input already lowercase)
        let (_rem, value) = nom_primitives::parse_number.parse(rest).ok()?;
        QuantityModification::Plus { value }
    } else {
        let rest = strip_after(lower, "that many minus ")?;
        let (_rem, value) = nom_primitives::parse_number.parse(rest).ok()?;
        QuantityModification::Minus { value }
    };

    let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .quantity_modification(modification)
        .description(original_text.to_string());
    if nom_primitives::scan_contains(lower, "permanent you control") {
        def = def.valid_card(TargetFilter::Typed(
            TypedFilter::permanent().controller(ControllerRef::You),
        ));
    } else if nom_primitives::scan_contains(lower, "creature you control") {
        def = def.valid_card(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ));
    }

    Some(def)
}

/// CR 614.1a: Parse damage redirection replacement effects.
/// Handles "all damage that would be dealt to [target] is dealt to ~ instead" (Pariah, Palisade Giant)
/// and "if a source would deal damage to you, prevent that damage. ~ deals that much damage to
/// any target" (Pariah's Shield).
fn parse_damage_redirection_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Pattern 1: "all damage that would be dealt to [X] is dealt to ~ instead" (Pariah)
    // Pattern 2: "damage that would be dealt to [X] is dealt to ~ instead" (Palisade Giant)
    // CR 615.1a: Redirect = prevent original + deal to new target
    if nom_primitives::scan_contains(norm_lower, "would be dealt to")
        && nom_primitives::scan_contains(norm_lower, "is dealt to")
    {
        let target_filter = if nom_primitives::scan_contains(norm_lower, "would be dealt to you") {
            Some(DamageTargetFilter::PlayerOnly)
        } else {
            // "would be dealt to ~" or other targets — no specific filter
            None
        };

        // Determine redirect destination
        let redirect = if nom_primitives::scan_contains(norm_lower, "is dealt to ~ instead") {
            // Redirect to self (the permanent with this ability)
            Some(TargetFilter::SelfRef)
        } else {
            None
        };

        let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::All)
            .description(original_text.to_string());
        if let Some(tf) = target_filter {
            def = def.damage_target_filter(tf);
        }
        if let Some(rt) = redirect {
            def = def.redirect_target(rt);
        }
        return Some(def);
    }

    // Pattern 3: "if a source would deal damage to you, prevent that damage"
    // followed by "~ deals that much damage to any target" (Pariah's Shield)
    // CR 615.1a: Prevention + redirect combination
    if nom_primitives::scan_contains(norm_lower, "would deal damage to you")
        && nom_primitives::scan_contains(norm_lower, "prevent that damage")
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .prevention_shield(PreventionAmount::All)
                .damage_target_filter(DamageTargetFilter::PlayerOnly)
                .redirect_target(TargetFilter::SelfRef)
                .description(original_text.to_string()),
        );
    }

    None
}

/// CR 615: Parse damage prevention replacement effects.
/// Handles:
/// - "prevent all combat damage that would be dealt [this turn]" (Fog, Moments Peace)
/// - "prevent all damage that would be dealt to you [this turn]" (Hallow)
/// - "prevent the next N damage that would be dealt to [target] this turn" (Mending Hands)
/// - "prevent all damage that would be dealt to and dealt by [creature]" (Stonehorn Dignitary)
fn parse_damage_prevention_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Must contain "prevent" and "damage" to be a prevention pattern
    if !nom_primitives::scan_contains(norm_lower, "prevent")
        || !nom_primitives::scan_contains(norm_lower, "damage")
    {
        return None;
    }

    // "damage can't be prevented" is NOT a prevention replacement -- it's a restriction.
    if nom_primitives::scan_contains(norm_lower, "can't be prevented") {
        return None;
    }

    // CR 615: "sources of the color of your choice" requires interactive color choice —
    // handled as a Choose → PreventDamage spell effect chain, not a passive replacement.
    if nom_primitives::scan_contains(norm_lower, "color of your choice") {
        return None;
    }

    // Redirection patterns ("prevent that damage. ~ deals that much damage to") are handled
    // by parse_damage_redirection_replacement — don't intercept them here.
    if nom_primitives::scan_contains(norm_lower, "prevent that damage")
        && nom_primitives::scan_contains(norm_lower, "deals that much damage")
    {
        return None;
    }
    // "is dealt to ~ instead" patterns are also redirections, not pure prevention
    if nom_primitives::scan_contains(norm_lower, "is dealt to")
        && nom_primitives::scan_contains(norm_lower, "instead")
    {
        return None;
    }

    // --- 1. Extract prevention amount ---
    // CR 615.7: "prevent the next N damage" → specific shield amount
    // CR 615.1a: "prevent all damage" → prevent everything
    let amount = if nom_primitives::scan_contains(norm_lower, "prevent all") {
        PreventionAmount::All
    } else if let Some(rest) = strip_after(norm_lower, "prevent the next ") {
        // Uses oracle_util::parse_number (not nom directly) because it handles "X" → 0
        // for cards like Temper, Acolyte's Reward, etc.
        let (n, _) = parse_number(rest)?;
        PreventionAmount::Next(n)
    } else if nom_primitives::scan_contains(norm_lower, "prevent that damage") {
        // "prevent that damage" in redirection context — redirect handled separately
        PreventionAmount::All
    } else {
        return None;
    };

    // --- 2. Extract combat scope ---
    // CR 615: "combat damage" restricts to combat damage only.
    // Longest-match-first: "noncombat damage" before "combat damage" because
    // "noncombat" contains the substring "combat".
    let combat_scope = if nom_primitives::scan_contains(norm_lower, "noncombat damage") {
        Some(CombatDamageScope::NoncombatOnly)
    } else if nom_primitives::scan_contains(norm_lower, "combat damage") {
        Some(CombatDamageScope::CombatOnly)
    } else {
        None
    };

    // --- 3. Extract damage target filter ---
    // "to you" → player only, "to target creature" → creature only
    let damage_target_filter = if nom_primitives::scan_contains(norm_lower, "dealt to you")
        || nom_primitives::scan_contains(norm_lower, "deal to you")
    {
        Some(DamageTargetFilter::PlayerOnly)
    } else if nom_primitives::scan_contains(norm_lower, "dealt to target creature")
        || nom_primitives::scan_contains(norm_lower, "dealt to ~")
        || nom_primitives::scan_contains(norm_lower, "dealt to and dealt by ~")
    {
        Some(DamageTargetFilter::CreatureOnly)
    } else {
        // "prevent all combat damage" with no target → any target
        None
    };

    // --- 4. Extract damage source filter ---
    let damage_source_filter = parse_damage_source_filter(norm_lower);

    // --- 5. Build the replacement definition ---
    let mut def = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .prevention_shield(amount)
        .description(original_text.to_string());

    if let Some(cs) = combat_scope {
        def = def.combat_scope(cs);
    }
    if let Some(tf) = damage_target_filter {
        def = def.damage_target_filter(tf);
    }
    if let Some(sf) = damage_source_filter {
        def = def.damage_source_filter(sf);
    }

    // CR 615.5: A prevention effect may include an additional effect referring to
    // the prevented amount ("Put a -1/-1 counter on ~ for each 1 damage prevented
    // this way", "Create N tokens for each 1 damage prevented this way"). Parse
    // the trailing sentence and attach it as the replacement's `execute` ability,
    // which the runtime fires as a post-replacement follow-up after the shield
    // consumes the damage. Class members: Phyrexian Hydra, Vigor, Stormwild
    // Capridor, Hostility.
    if let Some(followup) = extract_prevention_followup(original_text) {
        let mut followup_def = parse_effect_chain(&followup, AbilityKind::Spell);
        // CR 615.5 + CR 609.7: `parse_target` maps "the source's controller" /
        // "that source's controller" to `ParentTargetController` (correct for
        // anaphoric "its controller" in non-prevention contexts). Inside a
        // prevention follow-up the same surface phrase refers instead to the
        // controller of the *prevented event's* damage source (Swans of Bryn
        // Argoll, Deflecting Palm class). Rewrite the filter at the call site
        // — keeps `parse_target` consolidated for non-prevention callers and
        // avoids parser-context plumbing. Single building-block walker
        // (`each_target_filter_mut`) handles every target-bearing effect arm.
        rewrite_parent_target_controller_to_post_replacement_source(&mut followup_def);
        def = def.execute(followup_def);
    }

    Some(def)
}

/// CR 615.5 + CR 609.7: Walk an `AbilityDefinition` tree and rewrite every
/// `TargetFilter::ParentTargetController` slot to
/// `TargetFilter::PostReplacementSourceController`. Invoked at the prevention
/// follow-up call site only — see the parent comment for rationale.
fn rewrite_parent_target_controller_to_post_replacement_source(def: &mut AbilityDefinition) {
    super::oracle_effect::each_target_filter_mut(&mut def.effect, &mut |f| {
        if matches!(f, TargetFilter::ParentTargetController) {
            *f = TargetFilter::PostReplacementSourceController;
        }
    });
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_parent_target_controller_to_post_replacement_source(sub);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        rewrite_parent_target_controller_to_post_replacement_source(else_branch);
    }
}

/// CR 615.5: Extract the trailing additional-effect sentence from a prevention
/// replacement's Oracle text. Returns the slice after `"prevent that damage. "`,
/// trimmed and ready for `parse_effect_chain`. Returns `None` when there is no
/// follow-up (the common case: pure prevention).
///
/// CR 615.5: Strips an optional `"(when|if) damage is prevented this way, "`
/// prelude before returning the body. The prelude restates the firing condition
/// the replacement's `execute` hook already encodes — `Prevented` arm at
/// `replacement.rs:2207` only stashes `post_replacement_effect` when prevention
/// actually occurred — so the prelude is semantically a no-op and normalizes
/// to a bare effect chain. Documenting this here preempts a future contributor
/// adding a redundant "when damage is prevented" trigger arm in
/// `oracle_trigger.rs`.
///
/// Out of scope: one-shot prevention spells (Acolyte's Reward, Channel Harm,
/// Comeuppance, Bandage-style "Prevent the next N damage. Draw a card.") use a
/// different parser branch (spell-side `parse_effect_chain`) that does not
/// route through this helper.
fn extract_prevention_followup(original_text: &str) -> Option<String> {
    let lower = original_text.to_lowercase();
    let (_, after) = split_once_on_lower(original_text, &lower, "prevent that damage. ")?;
    let trimmed = after.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_lower = trimmed.to_lowercase();
    let body = match nom_on_lower(trimmed, &after_lower, |i| {
        value(
            (),
            preceded(
                alt((
                    tag::<_, _, VerboseError<&str>>("when "),
                    tag::<_, _, VerboseError<&str>>("if "),
                )),
                tag::<_, _, VerboseError<&str>>("damage is prevented this way, "),
            ),
        )
        .parse(i)
    }) {
        Some((_, rest)) => rest.trim(),
        None => trimmed,
    };
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}

/// CR 614.1a: Parse event substitution replacement effects.
/// Handles patterns where an event is completely skipped or replaced with a different outcome:
/// - "if [player] would begin an extra turn, that player skips that turn instead"
/// - "if you would lose the game, instead..."
/// - "if [player] would draw a card except the first one ... each turn, that player discards..."
fn parse_event_substitution_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // CR 500.7 + CR 614.10: "would begin an extra turn" / "would take an extra turn"
    // — Stranglehold ("that player skips that turn instead") and similar.
    // `OnlyExtraTurn` gates the replacement to fire only for extra turns.
    if nom_primitives::scan_contains(norm_lower, "would begin an extra turn")
        || nom_primitives::scan_contains(norm_lower, "would take an extra turn")
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::BeginTurn)
                .condition(ReplacementCondition::OnlyExtraTurn)
                .description(original_text.to_string()),
        );
    }

    // "would lose the game" — Platinum Angel, Lich's Mastery
    if nom_primitives::scan_contains(norm_lower, "would lose the game") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::GameLoss)
                .description(original_text.to_string()),
        );
    }

    // "would win the game" — Angel's Grace interaction
    if nom_primitives::scan_contains(norm_lower, "would win the game") {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::GameWin)
                .description(original_text.to_string()),
        );
    }

    None
}

/// CR 106.3 + CR 614.1a: Parse mana replacement effects.
/// Handles "if a land [you control] would produce mana, it produces [X] instead"
/// (Contamination, Infernal Darkness, Deep Water, Pale Moon, Ritual of Subdual,
/// Chromatic Lantern, Dryad of the Ilysian Grove, Blood Moon color override).
///
/// When the target mana type is extractable (e.g., "{B}" or "colorless mana"),
/// the definition carries a typed `ManaModification::ReplaceWith { ... }` payload
/// so the runtime applier can substitute the produced mana type. When the target
/// type is more exotic ("mana of any color", "mana of a color of your choice"),
/// the bare definition is returned and the static effect is recorded without
/// functional replacement (pending follow-up work for color-choice cards).
fn parse_mana_replacement(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    if !nom_primitives::scan_contains(norm_lower, "would produce mana")
        && !nom_primitives::scan_contains(norm_lower, "tapped for mana")
    {
        return None;
    }

    let def = ReplacementDefinition::new(ReplacementEvent::ProduceMana)
        .description(original_text.to_string());

    match scan_produces_replacement(norm_lower) {
        // CR 106.3: The mana source must be a land — scope the replacement so it
        // only fires on mana produced by lands (Contamination et al.). Applied
        // only when the payload is concretely known so pre-existing
        // color-choice / any-color replacements (not yet wired) retain their
        // parse-only behavior.
        Some(mana_type) => Some(
            def.mana_modification(ManaModification::ReplaceWith { mana_type })
                .valid_card(TargetFilter::Typed(TypedFilter::land())),
        ),
        None => Some(def),
    }
}

/// Walk `text` forward, trying `parse_produces_replacement` at each word boundary.
/// Returns the first extracted `ManaType` from a "produces {X} instead" /
/// "produces colorless mana instead" clause, or `None` if no such clause is found.
fn scan_produces_replacement(text: &str) -> Option<ManaType> {
    let mut remaining = text;
    while !remaining.is_empty() {
        if let Ok((_rest, mana_type)) = parse_produces_replacement(remaining) {
            return Some(mana_type);
        }
        // structural: not dispatch — advance to the next word boundary so the
        // combinator is retried at each word start (mirror of
        // `scan_timing_restrictions` in oracle_casting.rs).
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    None
}

/// CR 106.3 + CR 614.1a: Parse the "produces X instead" clause after "produces ",
/// returning the target `ManaType`. Handles `{W}`/`{U}`/`{B}`/`{R}`/`{G}` for
/// colored replacements and `colorless mana` for colorless replacements.
fn parse_produces_replacement(input: &str) -> super::oracle_nom::error::OracleResult<'_, ManaType> {
    let (rest, _) = tag::<_, _, VerboseError<&str>>("produces ").parse(input)?;
    alt((parse_braced_mana_type, parse_colorless_mana)).parse(rest)
}

/// Parse a single colored-mana brace symbol into `ManaType`: `{W}`/`{U}`/`{B}`/`{R}`/`{G}`.
fn parse_braced_mana_type(input: &str) -> super::oracle_nom::error::OracleResult<'_, ManaType> {
    use nom::sequence::delimited;
    delimited(
        char::<_, VerboseError<&str>>('{'),
        alt((
            value(ManaType::White, tag("w")),
            value(ManaType::Blue, tag("u")),
            value(ManaType::Black, tag("b")),
            value(ManaType::Red, tag("r")),
            value(ManaType::Green, tag("g")),
            value(ManaType::Colorless, tag("c")),
        )),
        char('}'),
    )
    .parse(input)
}

/// Parse "colorless mana" into `ManaType::Colorless`.
fn parse_colorless_mana(input: &str) -> super::oracle_nom::error::OracleResult<'_, ManaType> {
    value(
        ManaType::Colorless,
        tag::<_, _, VerboseError<&str>>("colorless mana"),
    )
    .parse(input)
}

/// CR 614.1d: Parse "enters tapped unless a player has N or less life" (bond lands).
/// Extract "unless a player has N or less life" condition (bond lands).
/// CR 614.1d
fn parse_player_life_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless a player has ")?;
    // "13 or less life" → extract amount
    // Delegate to nom_primitives::parse_number (input already lowercase)
    let (nom_rest, amount) = nom_primitives::parse_number.parse(rest).ok()?;
    let remainder = nom_rest.trim_start();
    if alt((
        tag::<_, _, VerboseError<&str>>("or less life"),
        tag("or fewer life"),
    ))
    .parse(remainder.trim())
    .is_err()
    {
        return None;
    }
    Some(ReplacementCondition::UnlessPlayerLifeAtMost { amount })
}

/// Extract "unless you have two or more opponents" condition (battlebond lands).
/// CR 614.1d
fn parse_multiple_opponents_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    if !nom_primitives::scan_contains(norm_lower, "unless you have two or more opponents") {
        return None;
    }
    Some(ReplacementCondition::UnlessMultipleOpponents)
}

/// Extract "unless it's your turn" / "if it's not your turn" condition.
/// Both phrasings are semantically identical: the permanent enters tapped on the opponent's turn.
/// CR 614.1d + CR 500
fn parse_your_turn_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    if nom_primitives::scan_contains(norm_lower, "unless it's your turn")
        || nom_primitives::scan_contains(norm_lower, "if it's not your turn")
    {
        Some(ReplacementCondition::UnlessYourTurn)
    } else {
        None
    }
}

/// Extract "unless it's your <ordinal-list> turn of the game" condition.
/// CR 614.1d + CR 500
/// Handles variable-length ordinal lists ("first", "first or second", "first, second, or third").
/// Takes the maximum ordinal as the threshold.
fn parse_turn_of_game_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    let rest = strip_after(norm_lower, "unless it's your ")?;
    // Parse comma/or-separated ordinal list: "first, second, or third turn"
    let mut max_ordinal: u32 = 0;
    let mut remaining = rest;
    loop {
        // Strip optional separator: ", or ", ", ", " or ", "or "
        // parse_ordinal trims leading space, so after parsing "first" from
        // "first or second", remaining is "or second" (no leading space).
        remaining = alt((
            tag::<_, _, VerboseError<&str>>(", or "),
            tag(", "),
            tag(" or "),
            tag("or "),
        ))
        .parse(remaining)
        .map_or(remaining, |(rest, _)| rest);
        if let Some((val, rest)) = parse_ordinal(remaining) {
            max_ordinal = max_ordinal.max(val);
            remaining = rest;
        } else {
            break;
        }
    }
    if max_ordinal == 0 {
        return None;
    }
    // Expect "turn" (optionally followed by "of the game")
    tag::<_, _, VerboseError<&str>>("turn")
        .parse(remaining)
        .ok()?;
    Some(ReplacementCondition::UnlessQuantity {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::TurnsTaken,
        },
        comparator: Comparator::LE,
        rhs: QuantityExpr::Fixed {
            value: max_ordinal as i32,
        },
        active_player_req: Some(ControllerRef::You),
    })
}

/// Catch-all: extract the text after "unless" as an Unrecognized condition.
/// CR 614.1d — Ensures the card is counted as having a parsed replacement for coverage.
fn parse_generic_unless_condition(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementCondition> {
    // Only match if there's actually an "unless" clause
    let _ = strip_after(norm_lower, " unless ")?;
    let original_lower = original_text.to_lowercase();
    let tp = TextPair::new(original_text, &original_lower);
    let unless_part = tp.strip_after(" unless ")?;
    let condition_text = unless_part.original;
    Some(ReplacementCondition::Unrecognized {
        text: condition_text.trim_end_matches('.').to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        Comparator, ControllerRef, QuantityExpr, QuantityModification, QuantityRef,
        ReplacementCondition, ShieldKind,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::keywords::Keyword;

    #[test]
    fn rewrite_parent_target_controller_flips_top_level_draw_target() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::ParentTargetController,
            },
        );
        rewrite_parent_target_controller_to_post_replacement_source(&mut def);
        assert!(matches!(
            *def.effect,
            Effect::Draw {
                target: TargetFilter::PostReplacementSourceController,
                ..
            }
        ));
    }

    #[test]
    fn rewrite_parent_target_controller_recurses_into_sub_ability() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::ParentTargetController,
                damage_source: None,
            },
        );
        def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::ParentTargetController,
            },
        )));
        rewrite_parent_target_controller_to_post_replacement_source(&mut def);
        assert!(matches!(
            *def.effect,
            Effect::DealDamage {
                target: TargetFilter::PostReplacementSourceController,
                ..
            }
        ));
        assert!(matches!(
            *def.sub_ability.as_ref().unwrap().effect,
            Effect::Draw {
                target: TargetFilter::PostReplacementSourceController,
                ..
            }
        ));
    }

    #[test]
    fn rewrite_parent_target_controller_leaves_other_filters_untouched() {
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        rewrite_parent_target_controller_to_post_replacement_source(&mut def);
        assert!(matches!(
            *def.effect,
            Effect::DealDamage {
                target: TargetFilter::Any,
                ..
            }
        ));
    }

    #[test]
    fn extract_prevention_followup_returns_none_when_no_followup() {
        assert_eq!(
            extract_prevention_followup("If damage would be dealt to ~, prevent that damage."),
            None
        );
    }

    #[test]
    fn extract_prevention_followup_returns_bare_effect() {
        assert_eq!(
            extract_prevention_followup(
                "If damage would be dealt to ~, prevent that damage. \
                 Put a -1/-1 counter on ~ for each 1 damage prevented this way."
            )
            .as_deref(),
            Some("Put a -1/-1 counter on ~ for each 1 damage prevented this way.")
        );
    }

    #[test]
    fn extract_prevention_followup_strips_when_prelude() {
        assert_eq!(
            extract_prevention_followup(
                "If damage would be dealt to ~, prevent that damage. \
                 When damage is prevented this way, sacrifice an Equipment attached to ~."
            )
            .as_deref(),
            Some("sacrifice an Equipment attached to ~.")
        );
    }

    #[test]
    fn extract_prevention_followup_strips_if_prelude() {
        assert_eq!(
            extract_prevention_followup(
                "If a source would deal damage to ~, prevent that damage. \
                 If damage is prevented this way, you draw a card."
            )
            .as_deref(),
            Some("you draw a card.")
        );
    }

    #[test]
    fn extract_prevention_followup_preserves_original_case_in_body() {
        // Prelude is matched case-insensitively, but the returned body keeps
        // the original casing so downstream parsers see e.g. card-name capitals.
        let result = extract_prevention_followup(
            "If damage would be dealt to ~, prevent that damage. \
             When damage is prevented this way, ~ deals 2 damage to any target.",
        );
        assert_eq!(result.as_deref(), Some("~ deals 2 damage to any target."));
    }

    #[test]
    fn replacement_enters_tapped() {
        let def =
            parse_replacement_line("Gutterbones enters the battlefield tapped.", "Gutterbones")
                .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn replacement_prevent_all_combat_damage_to_you() {
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt to you.",
            "Some Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.combat_scope, Some(CombatDamageScope::CombatOnly));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
    }

    #[test]
    fn replacement_prevent_all_combat_damage_fog() {
        // Fog: "Prevent all combat damage that would be dealt this turn."
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt this turn.",
            "Fog",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.combat_scope, Some(CombatDamageScope::CombatOnly));
        assert!(def.damage_target_filter.is_none()); // any target
    }

    #[test]
    fn replacement_prevent_next_n_damage() {
        let def = parse_replacement_line(
            "Prevent the next 3 damage that would be dealt to target creature this turn.",
            "Mending Hands",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::Next(3)
            }
        ));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::CreatureOnly)
        );
    }

    #[test]
    fn replacement_prevent_all_damage_to_you() {
        let def = parse_replacement_line(
            "Prevent all damage that would be dealt to you this turn.",
            "Safe Passage",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert!(def.combat_scope.is_none()); // all damage, not just combat
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
    }

    #[test]
    fn damage_cant_be_prevented_no_longer_parses_as_replacement() {
        // "can't be prevented" is now routed to effect parsing (Effect::AddRestriction),
        // not replacement parsing. This line should return None from the replacement parser.
        let def = parse_replacement_line(
            "Combat damage that would be dealt by creatures you control can't be prevented.",
            "Questing Beast",
        );
        // Note: This still matches because the line contains "would" which triggers
        // is_replacement_pattern. But parse_replacement_line doesn't have a handler
        // for "can't be prevented" anymore, so it falls through.
        // The line contains "would" so is_replacement_pattern returns true,
        // but the "would die/destroyed" check doesn't match. Result is None.
        assert!(def.is_none());
    }

    #[test]
    fn replacement_lose_life_doubled() {
        let def = parse_replacement_line(
            "If an opponent would lose life during your turn, they lose twice that much life instead.",
            "Bloodletter of Aclazotz",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::LoseLife);
        assert!(def.description.is_some());
    }

    #[test]
    fn replacement_non_match_returns_none() {
        assert!(parse_replacement_line("Destroy target creature.", "Some Card").is_none());
    }

    /// CR 614.6 + 701.20 + 701.24 + 400.3: Nexus of Fate-family shuffle-back replacement.
    /// Verifies the full chain ChangeZone(Library) → Reveal(SelfRef) → Shuffle(Owner).
    /// Parametric across Nexus of Fate / Progenitus / Blightsteel / Darksteel / Legacy Weapon
    /// because all five share structurally identical wording.
    #[test]
    fn replacement_shuffle_back_with_reveal_full_chain() {
        for card in [
            "Nexus of Fate",
            "Progenitus",
            "Blightsteel Colossus",
            "Darksteel Colossus",
            "Legacy Weapon",
        ] {
            let text = format!(
                "If {card} would be put into a graveyard from anywhere, reveal {card} and \
                 shuffle it into its owner's library instead."
            );
            let def = parse_replacement_line(&text, card)
                .unwrap_or_else(|| panic!("failed to parse shuffle-back line for {card}"));

            assert_eq!(def.event, ReplacementEvent::Moved);
            assert_eq!(def.destination_zone, Some(Zone::Graveyard));
            assert!(matches!(def.mode, ReplacementMode::Mandatory));

            // Execute: ChangeZone { destination: Library, target: SelfRef }
            let execute = def.execute.as_ref().unwrap();
            assert!(matches!(
                *execute.effect,
                Effect::ChangeZone {
                    destination: Zone::Library,
                    target: TargetFilter::SelfRef,
                    ..
                }
            ));
            // First sub_ability: Reveal { target: SelfRef }
            let reveal = execute
                .sub_ability
                .as_ref()
                .unwrap_or_else(|| panic!("{card}: missing reveal sub_ability"));
            assert!(matches!(
                *reveal.effect,
                Effect::Reveal {
                    target: TargetFilter::SelfRef
                }
            ));
            // Second sub_ability: Shuffle { target: Owner }
            let shuffle = reveal
                .sub_ability
                .as_ref()
                .unwrap_or_else(|| panic!("{card}: missing shuffle sub_ability"));
            assert!(matches!(
                *shuffle.effect,
                Effect::Shuffle {
                    target: TargetFilter::Owner
                }
            ));
        }
    }

    /// Building-block: the `opt(tag("reveal ~ and "))` combinator must independently
    /// accept the no-reveal variant. Exercises the shuffle-back branch without the
    /// CR 701.20 prefix.
    #[test]
    fn replacement_shuffle_back_without_reveal() {
        let def = parse_replacement_line(
            "If ~ would be put into a graveyard from anywhere, shuffle it into its owner's \
             library instead.",
            "Synthetic",
        )
        .expect("no-reveal shuffle-back must parse");

        let execute = def.execute.as_ref().unwrap();
        // No Reveal step — Shuffle hangs directly off the redirect ChangeZone.
        let shuffle = execute.sub_ability.as_ref().expect("shuffle sub_ability");
        assert!(matches!(
            *shuffle.effect,
            Effect::Shuffle {
                target: TargetFilter::Owner
            }
        ));
        // Ensure the single sub_ability is shuffle — not a reveal with nested shuffle.
        assert!(
            shuffle.sub_ability.is_none(),
            "no-reveal branch must not stash a trailing sub_ability"
        );
    }

    /// Regression: exile-branch must remain fully backward-compatible after the
    /// dispatcher refactor. Rest in Peace / Leyline-style wording.
    #[test]
    fn replacement_graveyard_exile_branch_still_parses() {
        let def = parse_replacement_line(
            "If a card would be put into a graveyard from anywhere, exile it instead.",
            "Rest in Peace",
        )
        .expect("exile branch must parse");
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
        assert!(
            execute.sub_ability.is_none(),
            "exile branch has no post-redirect sub_ability"
        );
    }

    #[test]
    fn shock_land_watery_grave() {
        let def = parse_replacement_line(
            "As this land enters, you may pay 2 life. If you don't, it enters tapped.",
            "Watery Grave",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
        // Accept branch: LoseLife { amount: 2 }
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));
        // Decline branch: Tap { target: SelfRef }
        if let ReplacementMode::Optional { decline } = &def.mode {
            let decline = decline.as_ref().unwrap();
            assert!(matches!(
                *decline.effect,
                Effect::Tap {
                    target: TargetFilter::SelfRef
                }
            ));
        } else {
            panic!("Expected Optional mode");
        }
    }

    #[test]
    fn shock_land_3_life() {
        let def = parse_replacement_line(
            "As this land enters, you may pay 3 life. If you don't, it enters tapped.",
            "Some Shock Land",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ));
    }

    #[test]
    fn shock_land_with_basic_land_type_choice_adds_choose_chain() {
        let def = parse_replacement_line(
            "As this land enters, choose a basic land type. Then you may pay 2 life. If you don't, it enters tapped.",
            "Multiversal Passage",
        )
        .unwrap();

        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                ..
            }
        ));
        assert!(matches!(
            *execute.sub_ability.as_ref().unwrap().effect,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));

        if let ReplacementMode::Optional { decline } = &def.mode {
            let decline = decline.as_ref().unwrap();
            assert!(matches!(
                *decline.effect,
                Effect::Choose {
                    choice_type: ChoiceType::BasicLandType,
                    ..
                }
            ));
            assert!(matches!(
                *decline.sub_ability.as_ref().unwrap().effect,
                Effect::Tap {
                    target: TargetFilter::SelfRef
                }
            ));
        }
    }

    #[test]
    fn reveal_land_port_town_emits_reveal_from_hand_with_or_filter() {
        let def = parse_replacement_line(
            "As Port Town enters, you may reveal a Plains or Island card from your hand. If you don't, Port Town enters tapped.",
            "Port Town",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        // Mandatory + single execute step: the "may reveal / else tap" is encoded inside
        // the RevealFromHand effect's on_decline, not via ReplacementMode::Optional.
        assert!(matches!(def.mode, ReplacementMode::Mandatory));

        let execute = def.execute.as_ref().unwrap();
        let (filter, on_decline) = match &*execute.effect {
            Effect::RevealFromHand { filter, on_decline } => (filter, on_decline),
            other => panic!("expected RevealFromHand, got {other:?}"),
        };
        // Union of Plains and Island — the reveal-land class uses TargetFilter::Or.
        assert!(matches!(filter, TargetFilter::Or { .. }));
        // Decline = Tap SelfRef (the "if you don't, ~ enters tapped" branch).
        let decline = on_decline.as_ref().unwrap();
        assert!(matches!(
            *decline.effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn reveal_land_gilt_leaf_palace_emits_single_subtype_filter() {
        let def = parse_replacement_line(
            "As Gilt-Leaf Palace enters, you may reveal an Elf card from your hand. If you don't, Gilt-Leaf Palace enters tapped.",
            "Gilt-Leaf Palace",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        let filter = match &*execute.effect {
            Effect::RevealFromHand { filter, .. } => filter,
            other => panic!("expected RevealFromHand, got {other:?}"),
        };
        // Single-subtype filter: tribal reveal-lands use TargetFilter::Typed, not Or.
        assert!(matches!(filter, TargetFilter::Typed(_)));
    }

    /// CR 614.1d + CR 701.20a: Tarkir Dragonstorm reveal-tribal land cycle —
    /// Fortified Beachhead. The disjunction "tapped unless revealed [filter]
    /// this way OR you control [filter]" is encoded as a single replacement:
    /// the on_decline Tap is gated by ControllerControlsMatching{filter,
    /// negated:true}, so the decline branch only taps when the controller
    /// doesn't already control a matching permanent. The accept-reveal path
    /// short-circuits the on_decline entirely (via reveal_from_hand's pending
    /// continuation drop on pick), giving the second OR arm semantically.
    #[test]
    fn reveal_land_fortified_beachhead_tarkir_disjunction() {
        let def = parse_replacement_line(
            "As Fortified Beachhead enters, you may reveal a Soldier card from your hand. Fortified Beachhead enters tapped unless you revealed a Soldier card this way or you control a Soldier.",
            "Fortified Beachhead",
        )
        .expect("Tarkir reveal-tribal land must parse");

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));

        let execute = def.execute.as_ref().unwrap();
        let (filter, on_decline) = match &*execute.effect {
            Effect::RevealFromHand { filter, on_decline } => (filter, on_decline),
            other => panic!("expected RevealFromHand, got {other:?}"),
        };
        // Sentence-1 reveal filter: Soldier (single-subtype Typed).
        match filter {
            TargetFilter::Typed(tf) => assert!(tf
                .type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Soldier"))),
            other => panic!("expected Typed Soldier filter, got {other:?}"),
        }

        // Sentence-2 conditional Tap: gated by ControllerControlsMatching{Soldier,
        // negated:true} — Tap fires only when controller controls no Soldier.
        let decline = on_decline.as_ref().expect("on_decline must be present");
        assert!(matches!(
            *decline.effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        let cond = decline
            .condition
            .as_ref()
            .expect("Tarkir variant on_decline must carry a condition");
        match cond {
            crate::types::ability::AbilityCondition::ControllerControlsMatching {
                filter: cond_filter,
                negated,
            } => {
                assert!(*negated, "Tap is suppressed when controller has matching");
                // Coherence check: condition filter has You-bound and Soldier subtype.
                match cond_filter {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(tf
                            .type_filters
                            .iter()
                            .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Soldier")));
                    }
                    other => panic!("expected Typed Soldier condition filter, got {other:?}"),
                }
            }
            other => panic!("expected ControllerControlsMatching, got {other:?}"),
        }
    }

    /// CR 614.1d + CR 701.20a: Tarkir Dragonstorm — Temple of the Dragon Queen
    /// covers the Dragon-tribal printing of the cycle. Verifies the pattern
    /// scales across subtypes by parsing a different filter than Beachhead.
    #[test]
    fn reveal_land_temple_dragon_queen_tarkir_disjunction() {
        let def = parse_replacement_line(
            "As Temple of the Dragon Queen enters, you may reveal a Dragon card from your hand. Temple of the Dragon Queen enters tapped unless you revealed a Dragon card this way or you control a Dragon.",
            "Temple of the Dragon Queen",
        )
        .expect("Temple of the Dragon Queen must parse");

        let execute = def.execute.as_ref().unwrap();
        let on_decline = match &*execute.effect {
            Effect::RevealFromHand { on_decline, .. } => on_decline,
            other => panic!("expected RevealFromHand, got {other:?}"),
        };
        let decline = on_decline.as_ref().unwrap();
        match decline.condition.as_ref() {
            Some(crate::types::ability::AbilityCondition::ControllerControlsMatching {
                filter,
                negated,
            }) => {
                assert!(*negated);
                match filter {
                    TargetFilter::Typed(tf) => {
                        assert_eq!(tf.controller, Some(ControllerRef::You));
                        assert!(tf
                            .type_filters
                            .iter()
                            .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dragon")));
                    }
                    other => panic!("expected Typed Dragon filter, got {other:?}"),
                }
            }
            other => panic!("expected ControllerControlsMatching, got {other:?}"),
        }
    }

    /// Regression: Port Town (and the rest of the if-you-don't reveal-land
    /// cycle) must continue to emit an unconditional Tap on_decline. The
    /// Tarkir-variant tail recognizer must not fire on the older grammar.
    #[test]
    fn reveal_land_port_town_unchanged_after_tarkir_extension() {
        let def = parse_replacement_line(
            "As Port Town enters, you may reveal a Plains or Island card from your hand. If you don't, Port Town enters tapped.",
            "Port Town",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        let on_decline = match &*execute.effect {
            Effect::RevealFromHand { on_decline, .. } => on_decline,
            other => panic!("expected RevealFromHand, got {other:?}"),
        };
        let decline = on_decline.as_ref().unwrap();
        // No condition gates the Tap — Port Town's on_decline runs unconditionally.
        assert!(
            decline.condition.is_none(),
            "Port Town on_decline must remain unconditional, got {:?}",
            decline.condition
        );
    }

    /// Negative: a mismatched filter pair ("reveal a Soldier ... or you control
    /// a Dragon") must NOT be accepted as a Tarkir variant — the parser bails
    /// rather than synthesize a coherently-typed disjunction from incoherent
    /// text, preserving the existing fallback path for unrecognized tails.
    #[test]
    fn reveal_land_tarkir_rejects_mismatched_filter_pair() {
        let def = parse_replacement_line(
            "As Test Land enters, you may reveal a Soldier card from your hand. Test Land enters tapped unless you revealed a Soldier card this way or you control a Dragon.",
            "Test Land",
        );
        // Falls through to the generic enters-tapped-unless fallback (Unrecognized
        // condition) rather than emitting a malformed RevealFromHand.
        let def = def.expect("must still parse via fallback");
        assert!(
            !matches!(
                def.execute.as_ref().unwrap().effect.as_ref(),
                Effect::RevealFromHand { .. }
            ),
            "mismatched filter pair must not be accepted as Tarkir variant",
        );
    }

    #[test]
    fn as_enters_choose_a_color() {
        let def = parse_replacement_line(
            "As Captivating Crossroads enters, choose a color.",
            "Captivating Crossroads",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::Color,
                persist: true,
            }
        ));
    }

    #[test]
    fn as_enters_choose_a_creature_type() {
        let def = parse_replacement_line(
            "As Door of Destinies enters, choose a creature type.",
            "Door of Destinies",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::CreatureType,
                persist: true,
            }
        ));
    }

    #[test]
    fn as_enters_choose_does_not_match_shock_land() {
        // Shock lands with "choose a basic land type" should be handled by parse_shock_land,
        // not parse_as_enters_choose
        let def = parse_replacement_line(
            "As this land enters, choose a basic land type. Then you may pay 2 life. If you don't, it enters tapped.",
            "Multiversal Passage",
        )
        .unwrap();
        // Should be Optional (shock land), not Mandatory (simple choose)
        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
    }

    #[test]
    fn check_land_clifftop_retreat() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mountain or a Plains.",
            "Clifftop Retreat",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsSubtype { subtypes }) => {
                assert_eq!(subtypes, &["Mountain", "Plains"]);
            }
            other => panic!("Expected UnlessControlsSubtype, got {other:?}"),
        }
    }

    #[test]
    fn check_land_drowned_catacomb() {
        let def = parse_replacement_line(
            "Drowned Catacomb enters the battlefield tapped unless you control an Island or a Swamp.",
            "Drowned Catacomb",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsSubtype { subtypes }) => {
                assert_eq!(subtypes, &["Island", "Swamp"]);
            }
            other => panic!("Expected UnlessControlsSubtype, got {other:?}"),
        }
    }

    #[test]
    fn unconditional_enters_tapped_still_works() {
        let def = parse_replacement_line(
            "Submerged Boneyard enters the battlefield tapped.",
            "Submerged Boneyard",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        // execute must be Some(Tap) so the mandatory pipeline can apply it
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn self_enters_with_counters() {
        let def = parse_replacement_line(
            "Polukranos enters the battlefield with twelve +1/+1 counters on it.",
            "Polukranos",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 12 },
                ..
            } if counter_type == "P1P1"
        ));
    }

    #[test]
    fn enters_with_x_counters_uses_cost_x_paid() {
        // CR 107.3m: "This artifact enters with X charge counters on it" — X is the
        // paid value for the {X} cost. Must emit QuantityRef::CostXPaid (not Fixed 0).
        let def = parse_replacement_line(
            "This artifact enters with X charge counters on it.",
            "Astral Cornucopia",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, "charge");
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::CostXPaid,
                        }
                    ),
                    "count should be CostXPaid, got {count:?}"
                );
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn enters_with_x_plus1_plus1_counters_uses_cost_x_paid() {
        // CR 107.3m: Walking Ballista / Endless One / Hangarback Walker class —
        // "enters with X +1/+1 counters on it".
        let def = parse_replacement_line(
            "Walking Ballista enters with X +1/+1 counters on it.",
            "Walking Ballista",
        )
        .unwrap();
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, "P1P1");
                assert!(matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::CostXPaid
                    }
                ));
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn enters_with_twice_x_plus1_plus1_counters() {
        // CR 107.3 + CR 107.3m: Primo, the Unbounded — "twice X" composes
        // `Multiply { factor: 2, inner: CostXPaid }`.
        let def = parse_replacement_line(
            "Primo enters with twice X +1/+1 counters on it.",
            "Primo, the Unbounded",
        )
        .unwrap();
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, "P1P1");
                match count {
                    QuantityExpr::Multiply { factor, inner } => {
                        assert_eq!(*factor, 2);
                        assert!(matches!(
                            inner.as_ref(),
                            QuantityExpr::Ref {
                                qty: QuantityRef::CostXPaid
                            }
                        ));
                    }
                    other => panic!("expected Multiply, got {other:?}"),
                }
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn enters_with_half_x_rounded_up_counters() {
        // CR 107.1a + CR 107.3m: Hypothetical half-X fixture — "half X, rounded up"
        // composes `HalfRounded { inner: CostXPaid, rounding: Up }`.
        let def = parse_replacement_line(
            "~ enters with half X, rounded up +1/+1 counters on it.",
            "Hypothetical Half-X Creature",
        )
        .unwrap();
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, "P1P1");
                match count {
                    QuantityExpr::HalfRounded { inner, rounding } => {
                        assert!(matches!(
                            inner.as_ref(),
                            QuantityExpr::Ref {
                                qty: QuantityRef::CostXPaid
                            }
                        ));
                        assert!(matches!(rounding, crate::types::ability::RoundingMode::Up));
                    }
                    other => panic!("expected HalfRounded, got {other:?}"),
                }
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn enters_with_dynamic_counters_equal_to_quantity() {
        let def = parse_replacement_line(
            "Ulamog enters with a number of +1/+1 counters on it equal to the greatest mana value among cards in exile.",
            "Ulamog",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, "P1P1", "counter type should be P1P1");
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::Aggregate { .. }
                        }
                    ),
                    "count should be Aggregate quantity, got {count:?}"
                );
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn other_creature_enters_with_counter_chosen_type() {
        let def = parse_replacement_line(
            "Each other creature you control of the chosen type enters with an additional +1/+1 counter on it.",
            "Metallic Mimic",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        // valid_card should filter for other creatures you control of chosen type
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(tf.properties.contains(&FilterProp::IsChosenCreatureType));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn other_non_subtype_creature_enters_with_counter() {
        // Grumgully, the Generous
        let def = parse_replacement_line(
            "Each other non-Human creature you control enters with an additional +1/+1 counter on it.",
            "Grumgully, the Generous",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::Another));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Non(Box::new(TypeFilter::Subtype(
                        "Human".to_string()
                    )))));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    // ── Escape-with-counters ──

    #[test]
    fn escape_with_three_counters() {
        // CR 702.138c: "This creature escapes with three +1/+1 counters on it."
        let def = parse_replacement_line(
            "This creature escapes with three +1/+1 counters on it.",
            "Voracious Typhon",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 3 },
                ..
            } if counter_type == "P1P1"
        ));
        assert_eq!(def.condition, Some(ReplacementCondition::CastViaEscape));
    }

    #[test]
    fn escape_with_one_counter() {
        let def = parse_replacement_line(
            "This creature escapes with a +1/+1 counter on it.",
            "Underworld Rage-Hound",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        assert_eq!(def.condition, Some(ReplacementCondition::CastViaEscape));
    }

    // ── Kicker-conditional enters-with-counters ──

    #[test]
    fn kicked_enters_with_counter() {
        // CR 702.33d: "If this creature was kicked, it enters with a +1/+1 counter on it."
        let def = parse_replacement_line(
            "If this creature was kicked, it enters with a +1/+1 counter on it and with flying.",
            "Ana Battlemage",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if counter_type == "P1P1"
        ));
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::CastViaKicker { cost_text: None })
        ));
    }

    #[test]
    fn kicked_with_specific_cost_enters_with_counters() {
        // CR 702.33d: "If this creature was kicked with its {1}{R} kicker, it enters with
        // two +1/+1 counters on it and with first strike."
        let def = parse_replacement_line(
            "If this creature was kicked with its {1}{R} kicker, it enters with two +1/+1 counters on it and with first strike.",
            "Necravolver",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 2 },
                ..
            } if counter_type == "P1P1"
        ));
        match &def.condition {
            Some(ReplacementCondition::CastViaKicker { cost_text }) => {
                assert_eq!(cost_text.as_deref(), Some("{1}{r}"));
            }
            other => panic!("Expected CastViaKicker, got {other:?}"),
        }
    }

    // ── External replacement effects ──

    #[test]
    fn rest_in_peace_graveyard_exile() {
        let def = parse_replacement_line(
            "If a card or token would be put into a graveyard from anywhere, exile it instead.",
            "Rest in Peace",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Graveyard));
        assert!(def.valid_card.is_none()); // matches all objects
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn leyline_of_the_void_opponent_scoped() {
        let def = parse_replacement_line(
            "If a card would be put into an opponent's graveyard from anywhere, exile it instead.",
            "Leyline of the Void",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Graveyard));
        // valid_card should scope to opponent-owned cards
        match &def.valid_card {
            Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
                assert!(properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                }));
            }
            other => panic!("Expected Typed filter with Owned, got {other:?}"),
        }
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn creature_die_exile_anaphoric_target() {
        // "exile it instead" should resolve the anaphoric "it" to SelfRef (the replaced object)
        let def = parse_replacement_line(
            "If a nontoken creature would die, exile it instead.",
            "Kalitas, Traitor of Ghet",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Destroy);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
        // valid_card should be a nontoken creature filter
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    /// CR 614.1a — prefix-form `instead exile it` mirrors the suffix-form
    /// `exile it instead`. The Darkness Crystal is the canonical print and
    /// chains `you gain 2 life` after `and` as a sub-ability.
    #[test]
    fn the_darkness_crystal_prefix_instead_exile_it() {
        let def = parse_replacement_line(
            "If a nontoken creature an opponent controls would die, instead exile it and you gain 2 life.",
            "The Darkness Crystal",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Destroy);
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
        // The "and you gain 2 life" continuation must be attached as a sub_ability.
        let sub = execute.sub_ability.as_ref().expect("expected sub_ability");
        assert!(matches!(
            *sub.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));
        // valid_card: nontoken creature, opponent-controlled.
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    /// CR 614.1a — prefix-form with `exile that card` anaphor variant. Kalitas
    /// chains a Token follow-up after `and`.
    #[test]
    fn kalitas_prefix_instead_exile_that_card() {
        let def = parse_replacement_line(
            "If a nontoken creature an opponent controls would die, instead exile that card and create a 2/2 black Zombie creature token.",
            "Kalitas, Traitor of Ghet",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Destroy);
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
        let sub = execute.sub_ability.as_ref().expect("expected sub_ability");
        assert!(
            matches!(*sub.effect, Effect::Token { .. }),
            "expected Token, got {:?}",
            sub.effect
        );
    }

    /// CR 614.1a — bare prefix-form (no `and` continuation). Confirms the
    /// continuation slot remains empty when there is no trailing clause.
    #[test]
    fn prefix_instead_exile_it_no_continuation() {
        let def = parse_replacement_line(
            "If another creature would die, instead exile it.",
            "Hypothetical Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Destroy);
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
        assert!(
            execute.sub_ability.is_none(),
            "expected no sub_ability for bare anaphor"
        );
    }

    /// CR 614.1a — prefix-form with `exile that creature` anaphor variant.
    #[test]
    fn prefix_instead_exile_that_creature() {
        let def = parse_replacement_line(
            "If a creature would die, instead exile that creature.",
            "Hypothetical Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Destroy);
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    /// CR 614.1a + CR 122.1 — Draugr Necromancer / Rayami class: the
    /// suffix-form exile-anaphor with an inline `with N <type> counter(s) on
    /// it` modifier lifts to `Effect::ChangeZone.enter_with_counters` so the
    /// resolver stamps an "ice"/"blood" counter on the exiled card.
    #[test]
    fn parse_enter_with_counters_on_change_zone_destroy_to_exile() {
        let def = parse_replacement_line(
            "If a nontoken creature an opponent controls would die, exile that card with an ice counter on it instead.",
            "Draugr Necromancer",
        )
        .expect("expected non-empty ReplacementDefinition for Draugr-shape die-replacement");
        assert_eq!(def.event, ReplacementEvent::Destroy);
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
        let execute = def.execute.as_ref().expect("expected execute populated");
        match &*execute.effect {
            Effect::ChangeZone {
                destination,
                target,
                enter_with_counters,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile);
                assert!(matches!(target, TargetFilter::SelfRef));
                assert_eq!(
                    enter_with_counters,
                    &vec![("ice".to_string(), QuantityExpr::Fixed { value: 1 })]
                );
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    /// CR 614.1a + CR 122.1 — Darigaaz Reincarnated: the self-die `~ would
    /// die` branch with prefix-form `instead exile it with three egg counters
    /// on it` lifts to `Effect::ChangeZone.enter_with_counters` (egg, 3) so
    /// the recurring upkeep loop can find Darigaaz with its egg counters.
    #[test]
    fn parse_enter_with_counters_on_self_die_replacement() {
        let def = parse_replacement_line(
            "If Darigaaz Reincarnated would die, instead exile it with three egg counters on it.",
            "Darigaaz Reincarnated",
        )
        .expect("expected non-empty ReplacementDefinition for Darigaaz self-die");
        assert_eq!(def.event, ReplacementEvent::Destroy);
        assert!(
            matches!(def.valid_card, Some(TargetFilter::SelfRef)),
            "self-die replacement must target the source via SelfRef"
        );
        let execute = def.execute.as_ref().expect("expected execute populated");
        match &*execute.effect {
            Effect::ChangeZone {
                destination,
                target,
                enter_with_counters,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile);
                assert!(matches!(target, TargetFilter::SelfRef));
                assert_eq!(
                    enter_with_counters,
                    &vec![("egg".to_string(), QuantityExpr::Fixed { value: 3 })]
                );
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
        // The bare prefix-form has no `and <continuation>` — sub_ability empty.
        assert!(execute.sub_ability.is_none());
    }

    #[test]
    fn authority_of_the_consuls_enters_tapped() {
        let def = parse_replacement_line(
            "Creatures your opponents control enter tapped.",
            "Authority of the Consuls",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn blind_obedience_compound_or_filter() {
        let def = parse_replacement_line(
            "Artifacts and creatures your opponents control enter tapped.",
            "Blind Obedience",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter, got {other:?}"),
        }
    }

    #[test]
    fn frozen_aether_comma_list() {
        let def = parse_replacement_line(
            "Artifacts, creatures, and lands your opponents control enter tapped.",
            "Frozen Aether",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Or { filters }) => {
                assert_eq!(filters.len(), 3);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::Opponent)
                    )
                );
                assert_eq!(
                    filters[2],
                    TargetFilter::Typed(
                        TypedFilter::new(TypeFilter::Land).controller(ControllerRef::Opponent)
                    )
                );
            }
            other => panic!("Expected Or filter with 3 elements, got {other:?}"),
        }
    }

    #[test]
    fn spelunking_lands_you_control_enter_untapped() {
        let def =
            parse_replacement_line("Lands you control enter untapped.", "Spelunking").unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Untap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn archelos_untapped_other_permanents_enter_untapped() {
        let def = parse_replacement_line(
            "As long as ~ is untapped, other permanents enter untapped.",
            "Archelos, Lagoon Mystic",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::SourceTappedState { tapped: false })
        );
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Untap {
                target: TargetFilter::SelfRef
            }
        ));
        assert!(def.valid_card.is_some(), "expected other-permanents filter");
    }

    #[test]
    fn archelos_tapped_other_permanents_enter_tapped() {
        let def = parse_replacement_line(
            "As long as ~ is tapped, other permanents enter tapped.",
            "Archelos, Lagoon Mystic",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::SourceTappedState { tapped: true })
        );
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        assert!(def.valid_card.is_some(), "expected other-permanents filter");
    }

    // ── Fast land tests ──

    #[test]
    fn fast_land_spirebluff_canal() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control two or fewer other lands.",
            "Spirebluff Canal",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Tap {
                target: TargetFilter::SelfRef
            }
        ));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsOtherLeq { count, filter }) => {
                assert_eq!(*count, 2);
                assert!(filter.type_filters.contains(&TypeFilter::Land));
                assert_eq!(filter.controller, Some(ControllerRef::You));
                assert!(filter.properties.contains(&FilterProp::Another));
            }
            other => panic!("Expected UnlessControlsOtherLeq, got {other:?}"),
        }
    }

    #[test]
    fn fast_land_generality_three_or_fewer() {
        // Hypothetical: "three or fewer" should parse count=3
        let def = parse_replacement_line(
            "This land enters tapped unless you control three or fewer other lands.",
            "Hypothetical Land",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsOtherLeq { count, .. }) => {
                assert_eq!(*count, 3);
            }
            other => panic!("Expected UnlessControlsOtherLeq, got {other:?}"),
        }
    }

    #[test]
    fn fast_land_does_not_capture_check_land() {
        // Check lands must still parse as UnlessControlsSubtype, not UnlessControlsOtherLeq
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mountain or a Plains.",
            "Clifftop Retreat",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessControlsSubtype { .. })
        ));
    }

    #[test]
    fn unconditional_enters_tapped_unaffected_by_fast_land() {
        // Plain "enters tapped" must still work (no condition)
        let def = parse_replacement_line("This land enters tapped.", "Some Tapland").unwrap();
        assert!(def.condition.is_none());
    }

    // ── General "unless you control a [type phrase]" tests ──

    #[test]
    fn unless_controls_basic_land() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a basic land.",
            "Ba Sing Se",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Basic,
                }));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_legendary_creature() {
        let def = parse_replacement_line(
            "Minas Tirith enters tapped unless you control a legendary creature.",
            "Minas Tirith",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                }));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_legendary_green_creature() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a legendary green creature.",
            "Argoth, Sanctum of Nature",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.contains(&FilterProp::HasSupertype {
                    value: Supertype::Legendary,
                }));
                assert!(tf.properties.contains(&FilterProp::HasColor {
                    color: ManaColor::Green,
                }));
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_mount_or_vehicle() {
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mount or Vehicle.",
            "Country Roads",
        )
        .unwrap();
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsMatching { filter }) => {
                // "Mount or Vehicle" → Or filter with two branches, each with ControllerRef::You
                let TargetFilter::Or { filters } = filter else {
                    panic!("Expected Or filter, got {filter:?}");
                };
                assert_eq!(filters.len(), 2);
                for f in filters {
                    let TargetFilter::Typed(tf) = f else {
                        panic!("Expected Typed branch, got {f:?}");
                    };
                    assert_eq!(tf.controller, Some(ControllerRef::You));
                }
            }
            other => panic!("Expected UnlessControlsMatching, got {other:?}"),
        }
    }

    #[test]
    fn unless_controls_does_not_steal_check_land() {
        // Check lands must still produce UnlessControlsSubtype, not UnlessControlsMatching
        let def = parse_replacement_line(
            "This land enters tapped unless you control a Mountain or a Plains.",
            "Clifftop Retreat",
        )
        .unwrap();
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessControlsSubtype { .. })
        ));
    }

    /// CR 614.1d: "unless your opponents control N or more [type]" — Turbulent land cycle (SOC).
    /// One parser test covers the class; all five Turbulent lands share this clause verbatim.
    #[test]
    fn unless_opponents_control_n_or_more_lands_turbulent_cycle() {
        let def = parse_replacement_line(
            "This land enters tapped unless your opponents control eight or more lands.",
            "Turbulent Fen",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        match &def.condition {
            Some(ReplacementCondition::UnlessControlsCountMatching { minimum, filter }) => {
                assert_eq!(*minimum, 8);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(tf.type_filters.contains(&TypeFilter::Land));
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
            }
            other => panic!("Expected UnlessControlsCountMatching, got {other:?}"),
        }
    }

    #[test]
    fn unconditional_catchall_rejects_unless() {
        // "enters tapped unless..." must NOT match the unconditional catch-all.
        // If the specific parsers all return None, the result should be None (not unconditional).
        // This is a regression guard for the catch-all safety check.
        let result = parse_replacement_line(
            "This land enters tapped unless some future condition we haven't implemented.",
            "Hypothetical Card",
        );
        assert!(
            result.is_none() || result.as_ref().unwrap().condition.is_some(),
            "Catch-all must not silently drop 'unless' clause"
        );
    }

    // ── Damage modification replacement tests ──

    #[test]
    fn damage_furnace_of_rath_double() {
        let def = parse_replacement_line(
            "If a source would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.",
            "Furnace of Rath",
        ).unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        assert_eq!(def.damage_source_filter, None); // any source
        assert_eq!(def.damage_target_filter, None); // any target
        assert_eq!(def.combat_scope, None); // all damage
    }

    #[test]
    fn damage_torbran_plus_2_red_source() {
        let def = parse_replacement_line(
            "If a red source you control would deal damage to an opponent or a permanent an opponent controls, it deals that much damage plus 2 instead.",
            "Torbran, Thane of Red Fell",
        ).unwrap();
        assert_eq!(
            def.damage_modification,
            Some(DamageModification::Plus { value: 2 })
        );
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::OpponentOrTheirPermanents)
        );
        // Source filter: red source you control
        let sf = def.damage_source_filter.unwrap();
        match sf {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::HasColor {
                    color: ManaColor::Red,
                }));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn damage_artists_talent_noncombat_plus_2() {
        let def = parse_replacement_line(
            "If a source you control would deal noncombat damage to an opponent or a permanent an opponent controls, it deals that much damage plus 2 instead.",
            "Artist's Talent",
        ).unwrap();
        assert_eq!(
            def.damage_modification,
            Some(DamageModification::Plus { value: 2 })
        );
        assert_eq!(def.combat_scope, Some(CombatDamageScope::NoncombatOnly));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::OpponentOrTheirPermanents)
        );
        // Source filter: source you control (no color qualifier)
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.is_empty());
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn damage_fiery_emancipation_triple() {
        let def = parse_replacement_line(
            "If a source you control would deal damage to a permanent or player, it deals triple that damage to that permanent or player instead.",
            "Fiery Emancipation",
        ).unwrap();
        assert_eq!(def.damage_modification, Some(DamageModification::Triple));
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
        assert_eq!(def.damage_target_filter, None); // "permanent or player" = any
    }

    #[test]
    fn damage_benevolent_unicorn_minus_1() {
        let def = parse_replacement_line(
            "If a spell would deal damage to a permanent or player, it deals that much damage minus 1 to that permanent or player instead.",
            "Benevolent Unicorn",
        ).unwrap();
        assert_eq!(
            def.damage_modification,
            Some(DamageModification::Minus { value: 1 })
        );
        assert_eq!(def.damage_source_filter, None); // "a spell" → no source filter
        assert_eq!(def.damage_target_filter, None); // "permanent or player" = any
    }

    #[test]
    fn damage_calamity_bearer_giant_double() {
        let def = parse_replacement_line(
            "If a Giant source you control would deal damage to a permanent or player, it deals double that damage to that permanent or player instead.",
            "Calamity Bearer",
        ).unwrap();
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert_eq!(tf.get_subtype(), Some("Giant"));
            }
            other => panic!("Expected Typed filter, got {other:?}"),
        }
    }

    #[test]
    fn damage_charging_tuskodon_self_combat_player() {
        let def = parse_replacement_line(
            "If this creature would deal combat damage to a player, it deals double that damage to that player instead.",
            "Charging Tuskodon",
        ).unwrap();
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        assert_eq!(def.damage_source_filter, Some(TargetFilter::SelfRef));
        assert_eq!(def.combat_scope, Some(CombatDamageScope::CombatOnly));
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
    }

    // ── Clone replacement tests ──

    #[test]
    fn clone_creature_basic() {
        // CR 707.9: "You may have ~ enter as a copy of any creature on the battlefield"
        let def = parse_replacement_line(
            "You may have Clone enter as a copy of any creature on the battlefield.",
            "Clone",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy {
                target,
                duration,
                mana_value_limit,
                additional_modifications,
            } => {
                assert!(duration.is_none());
                assert!(mana_value_limit.is_none());
                assert!(additional_modifications.is_empty());
                match target {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    }
                    other => panic!("Expected Typed creature filter, got {other:?}"),
                }
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_enchantment() {
        // Estrid's Invocation, Copy Enchantment
        let def = parse_replacement_line(
            "You may have this enchantment enter as a copy of an enchantment on the battlefield.",
            "Copy Enchantment",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Enchantment));
                }
                other => panic!("Expected Typed enchantment filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_artifact() {
        // Sculpting Steel, Phyrexian Metamorph
        let def = parse_replacement_line(
            "You may have this artifact enter as a copy of any artifact on the battlefield.",
            "Sculpting Steel",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Artifact));
                }
                other => panic!("Expected Typed artifact filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_vehicle() {
        let def = parse_replacement_line(
            "You may have this vehicle enter as a copy of any vehicle on the battlefield.",
            "Mirror Vehicle",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert_eq!(tf.get_subtype(), Some("Vehicle"));
                }
                other => panic!("Expected Typed vehicle filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_uses_self_ref_normalization() {
        // "this creature" should be normalized to "~" by replace_self_refs
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield.",
            "Some Clone",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(def.mode, ReplacementMode::Optional { .. }));
    }

    #[test]
    fn mockingbird_clone_replacement_uses_typed_copy_metadata() {
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield with mana value less than or equal to the amount of mana spent to cast this creature, except it's a Bird in addition to its other types and it has flying.",
            "Mockingbird",
        )
        .unwrap();

        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy {
                mana_value_limit,
                additional_modifications,
                ..
            } => {
                assert_eq!(
                    *mana_value_limit,
                    Some(CopyManaValueLimit::AmountSpentToCastSource)
                );
                assert!(
                    additional_modifications.contains(&ContinuousModification::AddSubtype {
                        subtype: "Bird".to_string(),
                    })
                );
                assert!(
                    additional_modifications.contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Flying,
                    })
                );
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn plain_clone_replacement_has_no_modifications() {
        // CR 707.9: Clone's suffix is the empty/period case — no mana-value
        // ceiling and no typed modifications, but the BecomeCopy replacement
        // must still register.
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield.",
            "Clone",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy {
                mana_value_limit,
                additional_modifications,
                ..
            } => {
                assert_eq!(*mana_value_limit, None);
                assert!(additional_modifications.is_empty());
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn phyrexian_metamorph_clone_replacement_adds_artifact_type() {
        // CR 707.9a + CR 205.2a: "except it's an artifact" adds the Artifact
        // core type (not a subtype) via `ContinuousModification::AddType`.
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any artifact or creature on the battlefield, except it's an artifact in addition to its other types.",
            "Phyrexian Metamorph",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy {
                mana_value_limit,
                additional_modifications,
                ..
            } => {
                assert_eq!(*mana_value_limit, None);
                assert!(
                    additional_modifications.contains(&ContinuousModification::AddType {
                        core_type: CoreType::Artifact,
                    }),
                    "expected AddType(Artifact), got {additional_modifications:?}"
                );
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn phantasmal_image_clone_replacement_preserves_subtype_addition() {
        // CR 707.9: Phantasmal Image's inline gained ability is not yet
        // parsed, but the subtype addition must still be captured and the
        // BecomeCopy replacement must still register.
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield, except it's an Illusion in addition to its other types and it has \"When this creature becomes the target of a spell or ability, sacrifice it.\"",
            "Phantasmal Image",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy {
                additional_modifications,
                ..
            } => {
                assert!(
                    additional_modifications.contains(&ContinuousModification::AddSubtype {
                        subtype: "Illusion".to_string(),
                    }),
                    "expected AddSubtype(Illusion), got {additional_modifications:?}"
                );
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn cursed_mirror_as_enters_become_copy_until_end_of_turn_with_haste() {
        // CR 614.1c + CR 707.9a + CR 611.3:
        // "As this artifact enters, you may have it become a copy of any
        // creature on the battlefield until end of turn, except it has haste."
        // Must produce an Optional Moved replacement with:
        //   - target: any creature on the battlefield
        //   - duration: Some(UntilEndOfTurn)
        //   - additional_modifications: [AddKeyword { Haste }]
        let def = parse_replacement_line(
            "As this artifact enters, you may have it become a copy of any creature on the battlefield until end of turn, except it has haste.",
            "Cursed Mirror",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy {
                target,
                duration,
                mana_value_limit,
                additional_modifications,
            } => {
                // Creature filter on the battlefield (default zone — no InZone).
                match target {
                    TargetFilter::Typed(tf) => {
                        assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    }
                    other => panic!("Expected Typed creature filter, got {other:?}"),
                }
                // CR 611.3 + CR 613.1a: until-EOT duration.
                assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
                assert_eq!(*mana_value_limit, None);
                // CR 707.9a: "except it has haste" → AddKeyword(Haste).
                assert!(
                    additional_modifications.contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste,
                    }),
                    "expected AddKeyword(Haste), got {additional_modifications:?}"
                );
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn phantasmal_image_clone_has_no_duration() {
        // Regression: the Phantasmal Image class uses "enter as a copy of" and
        // must continue producing a permanent copy (duration: None) after the
        // verb split was generalised to also accept "become a copy of".
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield.",
            "Clone",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy { duration, .. } => {
                assert_eq!(*duration, None, "Clone must produce a permanent copy");
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_suffix_multiple_keywords_produce_multiple_add_keyword() {
        // Hypothetical clone: "except it's a Spirit in addition to its other
        // types and it has flying, trample, and lifelink." Each keyword must
        // become an `AddKeyword` modification.
        let (mana_value_limit, _duration, modifications, _post) = parse_clone_suffix(
            "with mana value less than or equal to the amount of mana spent to cast ~, except it's a spirit in addition to its other types and it has flying, trample, and lifelink.",
            "Hypothetical Clone",
        );
        assert_eq!(
            mana_value_limit,
            Some(CopyManaValueLimit::AmountSpentToCastSource)
        );
        assert!(modifications.contains(&ContinuousModification::AddSubtype {
            subtype: "Spirit".to_string(),
        }));
        for keyword in [Keyword::Flying, Keyword::Trample, Keyword::Lifelink] {
            assert!(
                modifications.contains(&ContinuousModification::AddKeyword {
                    keyword: keyword.clone(),
                }),
                "expected AddKeyword({keyword:?}) in {modifications:?}"
            );
        }
    }

    #[test]
    fn clone_replacement_unrecognized_suffix_still_registers() {
        // CR 707.9: Quicksilver Gargantuan's "except it's 7/7." suffix is not
        // yet understood, but the parser must still emit the plain
        // BecomeCopy replacement rather than dropping the clone entirely.
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield, except it's 7/7.",
            "Quicksilver Gargantuan",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(&*execute.effect, Effect::BecomeCopy { .. }));
    }

    // --- "Instead" clause pattern tests ---

    #[test]
    fn token_doubling_replacement() {
        let def = parse_replacement_line(
            "If one or more tokens would be created under your control, twice that many tokens are created instead.",
            "Parallel Lives",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert!(def.quantity_modification.is_some());
        assert!(def.token_owner_scope.is_some());
    }

    #[test]
    fn token_doubling_replacement_current_oracle_wording() {
        let def = parse_replacement_line(
            "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.",
            "Doubling Season",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Double)
        );
        assert_eq!(def.token_owner_scope, Some(ControllerRef::You));
    }

    #[test]
    fn counter_doubling_replacement() {
        let def = parse_replacement_line(
            "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on it instead.",
            "Doubling Season",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert!(def.quantity_modification.is_some());
        assert!(matches!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: Some(ControllerRef::You),
                ..
            })) if type_filters == vec![TypeFilter::Creature]
        ));
    }

    #[test]
    fn counter_doubling_replacement_current_oracle_wording() {
        let def = parse_replacement_line(
            "If an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.",
            "Doubling Season",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Double)
        );
        assert!(matches!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: Some(ControllerRef::You),
                ..
            })) if type_filters == vec![TypeFilter::Permanent]
        ));
    }

    #[test]
    fn damage_redirection_to_self_instead() {
        // CR 614.1a: "All damage that would be dealt to you is dealt to ~ instead"
        let def = parse_replacement_line(
            "All damage that would be dealt to you is dealt to Pariah instead.",
            "Pariah",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
        // CR 615.1a: Redirect populates prevention shield + redirect target
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.redirect_target, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn damage_redirection_prevent_and_redirect() {
        // CR 614.1a: "If a source would deal damage to you, prevent that damage.
        // ~ deals that much damage to any target."
        let def = parse_replacement_line(
            "If a source would deal damage to you, prevent that damage. Pariah's Shield deals that much damage to any target.",
            "Pariah's Shield",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(
            def.damage_target_filter,
            Some(DamageTargetFilter::PlayerOnly)
        );
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(def.redirect_target, Some(TargetFilter::SelfRef));
    }

    #[test]
    fn event_substitution_extra_turn_skip() {
        // CR 614.1a: "If a player would begin an extra turn, that player skips that turn instead."
        let def = parse_replacement_line(
            "If a player would begin an extra turn, that player skips that turn instead.",
            "Stranglehold",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::BeginTurn);
    }

    #[test]
    fn conditional_draw_replacement_parses_quantity_gate_and_offset_draw() {
        let def = parse_replacement_line(
            "As long as you have one or fewer cards in hand, if you would draw one or more cards, you draw that many cards plus one instead.",
            "Quantum Riddler",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Draw);
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize,
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 1 },
                active_player_req: None,
            })
        );
        assert!(matches!(
            def.execute.as_deref().map(|ability| &*ability.effect),
            Some(Effect::Draw {
                count: QuantityExpr::Offset { inner, offset },
                ..
            }) if matches!(
                &**inner,
                QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                }
            ) && *offset == 1
        ));
    }

    #[test]
    fn event_substitution_lose_game() {
        // CR 614.1a: "If you would lose the game, instead..."
        let def = parse_replacement_line(
            "If you would lose the game, instead draw seven cards and your life total becomes 20.",
            "Lich's Mastery",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::GameLoss);
    }

    #[test]
    fn event_substitution_win_game() {
        let def = parse_replacement_line(
            "If a player would win the game, instead that player's opponents each draw a card.",
            "Some Card",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::GameWin);
    }

    #[test]
    fn mana_replacement_produce_any_color() {
        // CR 614.1a: "If a land you control would produce mana, it produces mana of any color instead."
        let def = parse_replacement_line(
            "If a land you control would produce mana, it produces mana of any color instead.",
            "Chromatic Lantern",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ProduceMana);
    }

    #[test]
    fn mana_replacement_tapped_for_mana() {
        // CR 614.1a: "If a land is tapped for mana, it produces mana of a color of your choice instead."
        let def = parse_replacement_line(
            "If a land is tapped for mana, it produces mana of a color of your choice instead of any other type.",
            "Celestial Dawn",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ProduceMana);
    }

    #[test]
    fn replacement_bond_land_enters_tapped_unless_player_life() {
        let def = parse_replacement_line(
            "This land enters tapped unless a player has 13 or less life.",
            "Abandoned Campground",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessPlayerLifeAtMost { amount: 13 })
        ));
    }

    #[test]
    fn replacement_battlebond_land_enters_tapped_unless_opponents() {
        let def = parse_replacement_line(
            "This land enters tapped unless you have two or more opponents.",
            "Luxury Suite",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::UnlessMultipleOpponents)
        ));
    }

    #[test]
    fn replacement_enters_tapped_unless_generic_fallback() {
        let def = parse_replacement_line(
            "This land enters tapped unless you revealed a Soldier card from your hand.",
            "Fortified Beachhead",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::Unrecognized { .. })
        ));
    }

    #[test]
    fn enters_tapped_unless_long_card_name() {
        // Verify condition_text is extracted from original_text, not norm_lower offset.
        // norm_lower has the card name replaced with `~` (1 char), so using its byte
        // offset against original_text would point to the wrong position.
        let norm = "~ enters the battlefield tapped unless you pay {2}.";
        let original = "Some Very Long Card Name enters the battlefield tapped unless you pay {2}.";
        let result = parse_enters_tapped_unless(norm, original);
        assert!(result.is_some(), "Should parse enters-tapped-unless");
    }

    #[test]
    fn enters_tapped_unless_your_turn() {
        let text = "~ enters tapped unless it's your turn.";
        let result = parse_replacement_line(text, "Test Card");
        let def = result.expect("Should parse unless-your-turn");
        assert_eq!(def.condition, Some(ReplacementCondition::UnlessYourTurn));
    }

    #[test]
    fn enters_tapped_if_not_your_turn() {
        // "if it's not your turn" is semantically equivalent to "unless it's your turn" (CR 614.1d).
        // Eddymurk Crab uses this positive-conditional phrasing.
        let text = "~ enters tapped if it's not your turn.";
        let result = parse_replacement_line(text, "Eddymurk Crab");
        let def = result.expect("Should parse if-not-your-turn as UnlessYourTurn");
        assert_eq!(def.condition, Some(ReplacementCondition::UnlessYourTurn));
    }

    #[test]
    fn enters_tapped_unless_first_second_third_turn() {
        let text = "~ enters tapped unless it's your first, second, or third turn of the game.";
        let result = parse_replacement_line(text, "Starting Town");
        let def = result.expect("Should parse unless-turn-of-game");
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::UnlessQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::TurnsTaken
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 3 },
                active_player_req: Some(ControllerRef::You),
            })
        );
    }

    #[test]
    fn enters_tapped_unless_first_or_second_turn() {
        let text = "~ enters tapped unless it's your first or second turn of the game.";
        let result = parse_replacement_line(text, "Test Card");
        assert!(
            result.is_some(),
            "Should parse unless-turn-of-game with 2 ordinals"
        );
    }

    #[test]
    fn enters_tapped_unless_sixth_turn() {
        let text = "~ enters tapped unless it's your sixth turn of the game.";
        let result = parse_replacement_line(text, "Test Card");
        let def = result.expect("Should parse single ordinal");
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::UnlessQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::TurnsTaken
                },
                comparator: Comparator::LE,
                rhs: QuantityExpr::Fixed { value: 6 },
                active_player_req: Some(ControllerRef::You),
            })
        );
    }

    #[test]
    fn mana_replacement_produces_black_instead() {
        // CR 106.3 + CR 614.1a: Contamination ("If a land is tapped for mana, it
        // produces {B} instead of any other type.") must carry a typed
        // ManaModification::ReplaceWith { Black } payload.
        let def = parse_replacement_line(
            "If a land is tapped for mana, it produces {B} instead of any other type.",
            "Contamination",
        )
        .expect("Should parse Contamination as ProduceMana replacement");
        assert_eq!(def.event, ReplacementEvent::ProduceMana);
        assert_eq!(
            def.mana_modification,
            Some(ManaModification::ReplaceWith {
                mana_type: ManaType::Black
            })
        );
        // Mana source must be a land for the replacement to fire.
        assert!(matches!(def.valid_card, Some(TargetFilter::Typed(_))));
    }

    #[test]
    fn mana_replacement_produces_colorless_instead() {
        // CR 106.3 + CR 614.1a: Pale Moon ("If a nonbasic land is tapped for mana,
        // it produces colorless mana instead of any other type of mana.") extracts
        // ManaType::Colorless.
        let def = parse_replacement_line(
            "If a land would produce mana, it produces colorless mana instead.",
            "Ritual of Subdual",
        )
        .expect("Should parse colorless mana replacement");
        assert_eq!(def.event, ReplacementEvent::ProduceMana);
        assert_eq!(
            def.mana_modification,
            Some(ManaModification::ReplaceWith {
                mana_type: ManaType::Colorless
            })
        );
    }

    // ── Superior Spider-Man (Mind Swap) ──
    // CR 707.9 + CR 707.2 + CR 613.1d: zone-qualified clone replacement with
    // copiable-value name override, P/T override, and additive subtype list,
    // plus a trailing reflexive "When you do, exile that card" sub-ability
    // (CR 603.12).

    #[test]
    fn superior_spider_man_parses_graveyard_clone_with_all_exceptions() {
        let def = parse_replacement_line(
            "Mind Swap — You may have Superior Spider-Man enter as a copy of any creature card in a graveyard, except his name is Superior Spider-Man and he's a 4/4 Spider Human Hero in addition to his other types. When you do, exile that card.",
            "Superior Spider-Man",
        )
        .expect("should parse clone replacement");

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));

        let execute = def.execute.as_ref().expect("execute present");
        let Effect::BecomeCopy {
            target,
            additional_modifications,
            ..
        } = &*execute.effect
        else {
            panic!("expected BecomeCopy, got {:?}", execute.effect);
        };

        // Filter scopes the copy source to a creature card in a graveyard.
        match target {
            TargetFilter::Typed(tf) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert!(tf.properties.iter().any(|p| matches!(
                    p,
                    FilterProp::InZone {
                        zone: Zone::Graveyard
                    }
                )));
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }

        // additional_modifications must contain SetName + SetPower + SetToughness +
        // one AddSubtype per type word.
        assert!(
            additional_modifications.contains(&ContinuousModification::SetName {
                name: "Superior Spider-Man".to_string()
            })
        );
        assert!(additional_modifications.contains(&ContinuousModification::SetPower { value: 4 }));
        assert!(
            additional_modifications.contains(&ContinuousModification::SetToughness { value: 4 })
        );
        for subtype in ["Spider", "Human", "Hero"] {
            assert!(
                additional_modifications.contains(&ContinuousModification::AddSubtype {
                    subtype: subtype.to_string()
                }),
                "missing AddSubtype({subtype}) in {additional_modifications:?}"
            );
        }

        // Reflexive "When you do, exile that card." attaches as a sub_ability
        // with condition WhenYouDo. The child effect must be an exile ChangeZone
        // to the (forwarded) parent target via ParentTarget.
        let sub = execute.sub_ability.as_ref().expect("reflexive sub_ability");
        assert_eq!(
            sub.condition,
            Some(crate::types::ability::AbilityCondition::WhenYouDo)
        );
        match &*sub.effect {
            Effect::ChangeZone {
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile);
                assert_eq!(*target, TargetFilter::ParentTarget);
            }
            other => panic!("expected ChangeZone(Exile), got {other:?}"),
        }
    }

    #[test]
    fn zone_qualifier_defaults_to_battlefield_for_classic_clones() {
        // Clone's filter must not gain a spurious InZone { Battlefield } — the
        // engine-side `find_copy_targets` defaults to the battlefield when the
        // filter has no InZone property. Preserving the empty properties list
        // keeps the filter shape identical to pre-change Clone behaviour.
        let def = parse_replacement_line(
            "You may have Clone enter as a copy of any creature on the battlefield.",
            "Clone",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        let Effect::BecomeCopy { target, .. } = &*execute.effect else {
            panic!("expected BecomeCopy");
        };
        match target {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.properties.is_empty(),
                    "Clone's filter must not carry InZone; got {:?}",
                    tf.properties
                );
            }
            other => panic!("expected Typed filter, got {other:?}"),
        }
    }

    // `parse_pt_pair` building-block tests moved to
    // `oracle_effect::become_copy_except` along with the helper itself.

    #[test]
    fn split_on_clone_source_zone_prefers_battlefield_when_present() {
        // Phantasmal Image-style text should still resolve to battlefield.
        let (type_text, _suffix, zone) =
            split_on_clone_source_zone("any creature on the battlefield, except...").unwrap();
        assert_eq!(type_text, "any creature");
        assert_eq!(zone, Zone::Battlefield);
    }

    #[test]
    fn split_on_clone_source_zone_accepts_graveyard_variants() {
        let (type_text, _, zone) =
            split_on_clone_source_zone("any creature card in a graveyard, except...").unwrap();
        assert_eq!(type_text, "any creature card");
        assert_eq!(zone, Zone::Graveyard);

        let (type_text, _, zone) =
            split_on_clone_source_zone("any creature card in any graveyard, except...").unwrap();
        assert_eq!(type_text, "any creature card");
        assert_eq!(zone, Zone::Graveyard);
    }

    /// CR 614.1c + CR 601.2h + CR 202.2: Wildgrowth Archaic's replacement line
    /// ("Whenever you cast a creature spell, that creature enters with X
    /// additional +1/+1 counters on it, where X is the number of colors of
    /// mana spent to cast it.") parses into a `ChangeZone` replacement on the
    /// entering creature with `PutCounter { count: Ref(ColorsSpentOnSelf) }`.
    #[test]
    fn parses_wildgrowth_archaic_replacement() {
        let text = "Whenever you cast a creature spell, that creature enters with X additional +1/+1 counters on it, where X is the number of colors of mana spent to cast it.";
        let def = parse_replacement_line(text, "Wildgrowth Archaic")
            .expect("Wildgrowth line should parse as a replacement");
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));

        // valid_card: creature controlled by the Archaic's controller.
        let TargetFilter::Typed(ref tf) = def.valid_card.as_ref().expect("valid_card set") else {
            panic!("expected Typed filter, got {:?}", def.valid_card);
        };
        assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
        assert_eq!(tf.controller, Some(ControllerRef::You));

        // execute: PutCounter { target: SelfRef, count: Ref(ColorsSpentOnSelf) }.
        let exec = def.execute.as_ref().expect("execute set");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*exec.effect
        else {
            panic!("expected PutCounter, got {:?}", exec.effect);
        };
        assert_eq!(counter_type, "P1P1");
        assert_eq!(target, &TargetFilter::SelfRef);
        assert_eq!(
            count,
            &QuantityExpr::Ref {
                qty: QuantityRef::ColorsSpentOnSelf
            }
        );
    }

    /// Regression: a plain "Whenever you cast" trigger without an "enters with"
    /// body must NOT be misrouted to the replacement path.
    #[test]
    fn plain_whenever_you_cast_is_not_replacement() {
        let text = "Whenever you cast a creature spell, draw a card.";
        assert!(parse_replacement_line(text, "Filler").is_none());
    }

    /// Regression: "Whenever you cast" with a fixed additional counter amount
    /// (no "where X is …" tail) also parses cleanly. Covers the cousin shape
    /// where the count is a literal number.
    #[test]
    fn parses_fixed_count_variant() {
        let text = "Whenever you cast a creature spell, that creature enters with an additional +1/+1 counter on it.";
        let def = parse_replacement_line(text, "Filler").expect("should parse");
        let exec = def.execute.as_ref().expect("execute set");
        let Effect::PutCounter { count, .. } = &*exec.effect else {
            panic!("expected PutCounter");
        };
        assert_eq!(count, &QuantityExpr::Fixed { value: 1 });
    }

    /// CR 614.1a + CR 111.1: Chatterfang's "those tokens plus that many 1/1
    /// green Squirrel creature tokens" replacement parses into a CreateToken
    /// replacement whose `additional_token_spec` carries a 1/1 green Squirrel
    /// creature spec, scoped to the controller's tokens.
    #[test]
    fn parses_chatterfang_plus_squirrel_tokens() {
        let text = "If one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.";
        let def = parse_replacement_line(text, "Chatterfang, Squirrel General")
            .expect("should parse Chatterfang replacement");
        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert_eq!(def.token_owner_scope, Some(ControllerRef::You));
        assert!(
            def.quantity_modification.is_none(),
            "Chatterfang adds tokens, not a count modifier"
        );
        let spec = def
            .additional_token_spec
            .as_ref()
            .expect("additional_token_spec set");
        assert_eq!(spec.power, Some(1));
        assert_eq!(spec.toughness, Some(1));
        assert_eq!(spec.core_types, vec![CoreType::Creature]);
        assert_eq!(spec.subtypes, vec!["Squirrel".to_string()]);
        assert_eq!(spec.colors, vec![ManaColor::Green]);
    }

    /// CR 614.1a: The "twice that many" shape and the "those tokens plus"
    /// shape are mutually exclusive in `parse_token_replacement_shape`. The
    /// Double branch must not leak an `additional_token_spec`.
    #[test]
    fn token_replacement_double_shape_has_no_additional_spec() {
        let lower = "it creates twice that many of those tokens instead";
        let def = parse_token_replacement(lower, lower).expect("double shape parses");
        assert!(matches!(
            def.quantity_modification,
            Some(crate::types::ability::QuantityModification::Double)
        ));
        assert!(def.additional_token_spec.is_none());
    }

    /// CR 614.1a + CR 111.1 + CR 111.10a: Xorn's full Oracle text parses to a
    /// CreateToken replacement with a `TokenSubtypeMatches { ["Treasure"] }`
    /// gate and an `additional_token_spec` carrying the Treasure spec.
    /// (CR 111.10a defines the Treasure token, verified via
    /// `grep '^111.10a' docs/MagicCompRules.txt` — earlier "111.10p" was wrong;
    /// 111.10p is the Virtuous Role token.)
    #[test]
    fn parses_xorn_additional_treasure_token_replacement_cr_614_1a() {
        let text = "If you would create one or more Treasure tokens, instead create those tokens plus an additional Treasure token.";
        let def =
            parse_replacement_line(text, "Xorn").expect("should parse Xorn token replacement");

        assert_eq!(def.event, ReplacementEvent::CreateToken);
        match &def.condition {
            Some(ReplacementCondition::TokenSubtypeMatches { subtypes }) => {
                assert_eq!(
                    subtypes,
                    &vec!["Treasure".to_string()],
                    "Xorn gates on Treasure subtype"
                );
            }
            other => panic!("Expected TokenSubtypeMatches, got {other:?}"),
        }
        let spec = def
            .additional_token_spec
            .as_ref()
            .expect("Xorn must populate additional_token_spec");
        assert!(
            spec.subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Treasure")),
            "appended spec must be a Treasure token, got {:?}",
            spec.subtypes
        );
    }

    /// CR 614.1a + CR 111.1: Academy Manufactor's "instead create one of each"
    /// parses to a CreateToken replacement whose `condition` lists all three
    /// gated subtypes and whose `ensure_token_specs` carries a TokenSpec for
    /// each. The applier (covered by replacement.rs tests) emits the missing
    /// subtypes only.
    #[test]
    fn parses_manufactor_ensure_all_token_replacement_cr_614_1a() {
        let text =
            "If you would create a Clue, Food, or Treasure token, instead create one of each.";
        let def = parse_replacement_line(text, "Academy Manufactor")
            .expect("Manufactor replacement must parse");

        assert_eq!(def.event, ReplacementEvent::CreateToken);
        match &def.condition {
            Some(ReplacementCondition::TokenSubtypeMatches { subtypes }) => {
                assert_eq!(
                    subtypes,
                    &vec![
                        "Clue".to_string(),
                        "Food".to_string(),
                        "Treasure".to_string()
                    ],
                    "condition must gate on all three subtypes"
                );
            }
            other => panic!("Expected TokenSubtypeMatches, got {other:?}"),
        }

        let specs = def
            .ensure_token_specs
            .as_ref()
            .expect("Manufactor must populate ensure_token_specs");
        assert_eq!(specs.len(), 3);
        let subtypes_present: Vec<String> = specs.iter().flat_map(|s| s.subtypes.clone()).collect();
        for expected in &["Clue", "Food", "Treasure"] {
            assert!(
                subtypes_present
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(expected)),
                "ensure_token_specs missing {expected}, got {subtypes_present:?}"
            );
        }
        assert!(
            def.additional_token_spec.is_none(),
            "Manufactor uses ensure_token_specs, not additional_token_spec"
        );
    }
}
