//! CR 700.3 + CR 608: Pile-separation parser.
//!
//! Recognises the three-sentence Make-an-Example shape:
//!
//! ```text
//! Each opponent separates the creatures they control into two piles.
//! For each opponent, you choose one of their piles.
//! Each opponent sacrifices the creatures in their chosen pile.
//! ```
//!
//! Output: a single `Effect::SeparateIntoPiles` whose `chosen_pile_effect`
//! is the trailing-sentence sub-effect (a `Sacrifice` for Make an Example).
//! Replaces the prior `Unimplemented{name:"separate"} → Unimplemented{name:"choose"}
//! → Sacrifice` chain plus the spurious `repeat_for` sub-ability.
//!
//! Architectural rules:
//! * Nom combinators for ALL dispatch — never `find` / `contains` /
//!   `split_once` / `starts_with` for parsing.
//! * Builds for the *class* of cards (any "each opponent separates ... into
//!   two piles. For each opponent, you choose ... Each opponent <effect> ...
//!   their chosen pile" card), not just Make an Example. The trailing
//!   sub-effect is parsed by the existing imperative chain parser so future
//!   variants (mill, exile, return-to-hand) come for free.

use nom::branch::alt;
use nom::bytes::complete::tag_no_case;
use nom::combinator::value;
use nom::Parser;

use crate::parser::oracle_nom::error::OracleError;
use crate::parser::oracle_nom::primitives::parse_number;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PileSource, PlayerScope, QuantityExpr, TargetFilter,
    TypeFilter, TypedFilter, VoterScope,
};
use crate::types::zones::{EtbTapState, Zone};

use super::oracle_effect::parse_effect_chain_with_context;
use super::oracle_ir::context::ParseContext;

/// CR 700.3: Detect and parse the full pile-separation block. Returns a
/// synthesized `AbilityDefinition` wrapping the new `Effect::SeparateIntoPiles`,
/// or `None` if the input doesn't match.
///
/// The input is the joined effect-body text (multi-sentence). The dispatcher
/// in `parser/oracle.rs` calls this BEFORE generic chain parsing so the
/// three-sentence chain is consumed as a single unit rather than parsed into
/// three Unimplemented chunks.
///
/// Supports two shape families:
/// 1. **Battlefield partition** (Make an Example): "Each opponent separates the
///    creatures they control into two piles. For each opponent, you choose one
///    of their piles. Each opponent sacrifices the creatures in their chosen pile."
/// 2. **Reveal-from-library** (Fact or Fiction): "Reveal the top N cards of your
///    library. An opponent separates those cards into two piles. Put one pile
///    into your hand and the rest into your graveyard."
pub(crate) fn parse_separate_into_piles(
    text: &str,
    kind: AbilityKind,
) -> Option<AbilityDefinition> {
    // Try the reveal-from-library shape first (Fact or Fiction family).
    if let Some(def) = try_parse_reveal_separate(text, kind) {
        return Some(def);
    }
    // Fall through to the battlefield partition shape (Make an Example family).
    let (rest, partition_subject) = parse_separates_line(text)?;
    let (rest, chooser) = parse_choose_line(rest)?;
    let trailing = rest.trim_start();
    if trailing.is_empty() {
        return None;
    }
    // Parse the trailing sentence (the per-pile sub-effect) through the
    // standard imperative chain parser. For Make an Example this yields a
    // `Sacrifice { target: ParentTarget }` chain — the runtime resolver
    // re-binds `controller` to each subject before applying it.
    //
    // CR 700.3b: the pile is not an object — the sub-effect's target is
    // wired by the resolver per-object, not via the parsed `target_filter`.
    let parsed = parse_effect_chain_with_context(trailing, kind, &mut ParseContext::default());
    // Reject if the trailing sentence didn't yield a real effect (the parser
    // returns an Unimplemented stub on failure).
    if matches!(*parsed.effect, Effect::Unimplemented { .. }) {
        return None;
    }
    // CR 700.3 + CR 608.2c: Build a sub-effect with a generic ParentTarget
    // filter so the runtime's per-object loop in `apply_pile_effect` sets
    // the target via `TargetRef::Object`. Force-rewrite the sub-effect's
    // target filter to `ParentTarget` so the per-object pipeline routes
    // through the standard sacrifice handler unambiguously.
    let mut sub_def = parsed;
    rewrite_sub_effect_target_to_parent(&mut sub_def.effect);

    Some(AbilityDefinition::new(
        kind,
        Effect::SeparateIntoPiles {
            partition_subject,
            // CR 700.3: Make an Example partitions creatures specifically;
            // the Liliana −6 follow-up will pass a wider filter. Defaulting
            // to the parsed subject filter is a future extension — for now
            // we hardcode Creature, which is the only printed shape.
            object_filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            chooser,
            chosen_pile_effect: Box::new(sub_def),
            pile_source: crate::types::ability::PileSource::Battlefield,
            unchosen_pile_effect: None,
        },
    ))
}

/// CR 700.3 + CR 700.3a: Consume the "Each opponent separates the creatures
/// they control into two piles." opener. Returns the remainder and the
/// `VoterScope` for the partitioning subject. Currently supports the
/// "each opponent" shape; "target player separates ..." (Liliana −6) is a
/// leaf extension on `VoterScope` and slots in here as another `alt()`
/// branch.
fn parse_separates_line(input: &str) -> Option<(&str, VoterScope)> {
    let res: nom::IResult<&str, VoterScope, OracleError<'_>> = value(
        VoterScope::EachOpponent,
        tag_no_case("each opponent separates "),
    )
    .parse(input);
    let (rest, scope) = res.ok()?;
    // Consume "the creatures they control " — the subject filter is fixed
    // for the current shape (see comment in caller about Creature default).
    let rest = consume_creatures_they_control(rest)?;
    let res: nom::IResult<&str, (), OracleError<'_>> =
        value((), tag_no_case("into two piles")).parse(rest);
    let (rest, ()) = res.ok()?;
    // Optional trailing period and whitespace.
    let rest = rest.trim_start_matches('.').trim_start();
    Some((rest, scope))
}

fn consume_creatures_they_control(input: &str) -> Option<&str> {
    // Two variants: "the creatures they control " and the rarer "creatures
    // they control " (no article). Both are nom alternatives.
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((
            tag_no_case("the creatures they control "),
            tag_no_case("creatures they control "),
        )),
    )
    .parse(input);
    res.ok().map(|(rest, ())| rest)
}

/// CR 700.3 + CR 608.2c: Consume "For each opponent, you choose one of their
/// piles." (or the bare "You choose one of their piles."). Returns the
/// remainder and the chooser scope.
fn parse_choose_line(input: &str) -> Option<(&str, PlayerScope)> {
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((
            tag_no_case("for each opponent, you choose "),
            tag_no_case("you choose "),
        )),
    )
    .parse(input);
    let (rest, ()) = res.ok()?;
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((tag_no_case("one of their piles"), tag_no_case("one pile"))),
    )
    .parse(rest);
    let (rest, ()) = res.ok()?;
    let rest = rest.trim_start_matches('.').trim_start();
    Some((rest, PlayerScope::Controller))
}

/// Rewrite the sub-effect's primary target filter to `TargetFilter::ParentTarget`
/// so the runtime per-object loop in `effects/separate_piles::apply_pile_effect`
/// can pass each pile object through `targets[0]` and have the standard
/// sacrifice/exile/bounce handler pick it up. Currently only `Effect::Sacrifice`
/// is exercised; extend with new effect arms as new pile-effect shapes ship.
fn rewrite_sub_effect_target_to_parent(effect: &mut Effect) {
    if let Effect::Sacrifice { target, count, .. } = effect {
        // `SeparateIntoPiles` applies this sub-effect once per object in the
        // chosen pile. Keep the sub-effect canonical as "sacrifice this one
        // parent target"; the parsed "all" cardinality belongs to the pile loop.
        *target = TargetFilter::ParentTarget;
        *count = QuantityExpr::Fixed { value: 1 };
    }
}

/// CR 700.3 + CR 701.20a: Parse the "Reveal the top N cards ... An opponent
/// separates ... Put one pile into [zone] and the rest into [zone]" shape.
/// Builds for the class: any reveal-top-N → opponent-separates → zone-routing
/// card (Fact or Fiction, Steam Augury, Epiphany at the Drownyard, etc.).
fn try_parse_reveal_separate(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    // Sentence 1: "Reveal the top N cards of your library."
    let (rest, count) = parse_reveal_top_sentence(text)?;
    // Sentence 2: "An opponent separates those cards into two piles."
    let (rest, partition_subject) = parse_opponent_separates_sentence(rest)?;
    // Sentence 3: "Put one pile into your hand and the rest into your graveyard."
    let rest = rest.trim_start();
    let (chosen_zone, unchosen_zone) = parse_pile_disposition_sentence(rest)?;

    // CR 700.3b: Build sub-effects for chosen and unchosen piles.
    // The resolver applies these per-object via `TargetFilter::ParentTarget`.
    let chosen_pile_effect = Box::new(AbilityDefinition::new(
        kind,
        make_change_zone_effect(chosen_zone),
    ));
    let unchosen_pile_effect = Some(Box::new(AbilityDefinition::new(
        kind,
        make_change_zone_effect(unchosen_zone),
    )));

    Some(AbilityDefinition::new(
        kind,
        Effect::SeparateIntoPiles {
            partition_subject,
            // CR 700.3: revealed cards are the objects being separated —
            // no battlefield filter applies.
            object_filter: TargetFilter::Any,
            chooser: PlayerScope::Controller,
            chosen_pile_effect,
            pile_source: PileSource::RevealedFromLibraryTop { count },
            unchosen_pile_effect,
        },
    ))
}

/// Parse "Reveal the top N cards of your library." — returns remainder and count.
fn parse_reveal_top_sentence(input: &str) -> Option<(&str, u32)> {
    // CR 701.20a: "Reveal" is a keyword action.
    let res: nom::IResult<&str, &str, OracleError<'_>> =
        tag_no_case("reveal the top ").parse(input);
    let (rest, _) = res.ok()?;
    // Parse the count ("five", "7", etc.) using the shared number combinator.
    let (rest, count) = parse_number(rest).ok()?;
    // Consume " cards of your library" (with optional plural).
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((
            tag_no_case(" cards of your library"),
            tag_no_case(" card of your library"),
        )),
    )
    .parse(rest);
    let (rest, ()) = res.ok()?;
    let rest = rest.trim_start_matches('.').trim_start();
    Some((rest, count))
}

/// Parse "An opponent separates those cards into two piles." — returns remainder
/// and the `VoterScope` for the partitioner.
fn parse_opponent_separates_sentence(input: &str) -> Option<(&str, VoterScope)> {
    // CR 700.3a: "An opponent" or "target opponent" separates.
    let res: nom::IResult<&str, VoterScope, OracleError<'_>> = alt((
        value(
            VoterScope::AnOpponent,
            tag_no_case("an opponent separates "),
        ),
        value(
            VoterScope::AnOpponent,
            tag_no_case("target opponent separates "),
        ),
    ))
    .parse(input);
    let (rest, scope) = res.ok()?;
    // Consume "those cards into two piles" or "them into two piles".
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((
            tag_no_case("those cards into two piles"),
            tag_no_case("them into two piles"),
        )),
    )
    .parse(rest);
    let (rest, ()) = res.ok()?;
    let rest = rest.trim_start_matches('.').trim_start();
    Some((rest, scope))
}

/// Parse "Put one pile into your hand and the rest into your graveyard." —
/// returns (chosen_zone, unchosen_zone). Handles both orderings.
fn parse_pile_disposition_sentence(input: &str) -> Option<(Zone, Zone)> {
    // CR 700.3c: The controller chooses which pile to put where.
    let res: nom::IResult<&str, (), OracleError<'_>> =
        value((), tag_no_case("put one pile into your ")).parse(input);
    let (rest, ()) = res.ok()?;
    let (rest, chosen_zone) = parse_zone_name(rest)?;
    // Consume " and the rest into your ".
    let res: nom::IResult<&str, (), OracleError<'_>> =
        value((), tag_no_case(" and the rest into your ")).parse(rest);
    let (rest, ()) = res.ok()?;
    let (rest, unchosen_zone) = parse_zone_name(rest)?;
    // Only allow optional trailing period/whitespace then EOF; reject any
    // remaining rules text so cards with riders are not marked supported.
    let rest = rest.trim_start_matches('.');
    let rest = rest.trim();
    if !rest.is_empty() {
        return None;
    }
    Some((chosen_zone, unchosen_zone))
}

/// Parse a zone name from Oracle text ("hand", "graveyard", "library").
fn parse_zone_name(input: &str) -> Option<(&str, Zone)> {
    let res: nom::IResult<&str, Zone, OracleError<'_>> = alt((
        value(Zone::Hand, tag_no_case("hand")),
        value(Zone::Graveyard, tag_no_case("graveyard")),
        value(Zone::Library, tag_no_case("library")),
        value(Zone::Exile, tag_no_case("exile")),
    ))
    .parse(input);
    res.ok()
}

/// Build a minimal `Effect::ChangeZone` for pile sub-effects. The target is
/// `ParentTarget` because the runtime resolver applies this per-object.
fn make_change_zone_effect(destination: Zone) -> Effect {
    Effect::ChangeZone {
        origin: None,
        destination,
        target: TargetFilter::ParentTarget,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR 700.3: Make-an-Example body parses to a single
    /// `Effect::SeparateIntoPiles` with `EachOpponent` partition,
    /// `Controller` chooser, and a Sacrifice sub-effect.
    #[test]
    fn parses_make_an_example_body() {
        let text = "Each opponent separates the creatures they control into two piles. \
                    For each opponent, you choose one of their piles. \
                    Each opponent sacrifices the creatures in their chosen pile.";
        let def = parse_separate_into_piles(text, AbilityKind::Spell)
            .expect("Make an Example body parses");
        match &*def.effect {
            Effect::SeparateIntoPiles {
                partition_subject,
                chooser,
                chosen_pile_effect,
                ..
            } => {
                assert!(matches!(partition_subject, VoterScope::EachOpponent));
                assert!(matches!(chooser, PlayerScope::Controller));
                assert!(
                    matches!(*chosen_pile_effect.effect, Effect::Sacrifice { .. }),
                    "expected Sacrifice sub-effect, got {:?}",
                    chosen_pile_effect.effect
                );
            }
            other => panic!("expected SeparateIntoPiles, got {other:?}"),
        }
    }

    /// CR 700.3 + CR 701.20a: Fact or Fiction Oracle text parses to a
    /// `SeparateIntoPiles` with `RevealedFromLibraryTop { count: 5 }`,
    /// `AnOpponent` partition, `Controller` chooser, ChangeZone(Hand)
    /// chosen sub-effect, and ChangeZone(Graveyard) unchosen sub-effect.
    #[test]
    fn parses_fact_or_fiction_body() {
        let text = "Reveal the top five cards of your library. \
                    An opponent separates those cards into two piles. \
                    Put one pile into your hand and the rest into your graveyard.";
        let def = parse_separate_into_piles(text, AbilityKind::Spell)
            .expect("Fact or Fiction body parses");
        match &*def.effect {
            Effect::SeparateIntoPiles {
                partition_subject,
                chooser,
                chosen_pile_effect,
                pile_source,
                unchosen_pile_effect,
                ..
            } => {
                assert!(
                    matches!(partition_subject, VoterScope::AnOpponent),
                    "expected AnOpponent, got {partition_subject:?}"
                );
                assert!(matches!(chooser, PlayerScope::Controller));
                assert!(
                    matches!(pile_source, PileSource::RevealedFromLibraryTop { count: 5 }),
                    "expected RevealedFromLibraryTop {{ count: 5 }}, got {pile_source:?}"
                );
                assert!(
                    matches!(
                        &*chosen_pile_effect.effect,
                        Effect::ChangeZone {
                            destination: Zone::Hand,
                            target: TargetFilter::ParentTarget,
                            ..
                        }
                    ),
                    "expected ChangeZone to Hand, got {:?}",
                    chosen_pile_effect.effect
                );
                let unchosen = unchosen_pile_effect
                    .as_ref()
                    .expect("unchosen_pile_effect should be Some");
                assert!(
                    matches!(
                        &*unchosen.effect,
                        Effect::ChangeZone {
                            destination: Zone::Graveyard,
                            target: TargetFilter::ParentTarget,
                            ..
                        }
                    ),
                    "expected ChangeZone to Graveyard, got {:?}",
                    unchosen.effect
                );
            }
            other => panic!("expected SeparateIntoPiles, got {other:?}"),
        }
    }

    /// Non-matching body returns None — the dispatcher must fall back to
    /// generic chain parsing.
    #[test]
    fn rejects_non_pile_body() {
        let text = "Destroy target creature. Draw a card.";
        assert!(parse_separate_into_piles(text, AbilityKind::Spell).is_none());
    }
}
