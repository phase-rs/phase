//! CR 701.38 + CR 207.2c: Council's-dilemma / Will-of-the-Council voting parser.
//!
//! This module owns recognition of the full vote effect block:
//!
//! ```text
//! starting with you, each player votes for <choice-a> or <choice-b>.
//! For each <choice-a> vote, <effect-a>.
//! For each <choice-b> vote, <effect-b>.
//! ```
//!
//! Output: a synthesized `Effect::Vote` whose `per_choice_effect` slots carry
//! the parsed sub-effects in `choices` declaration order.
//!
//! Architectural rules:
//! * Nom combinators for ALL dispatch — never `find` / `contains` / `split_once`.
//! * Builds for the *class* of cards (every Will-of-the-Council / Council's-
//!   dilemma vote with two-or-more named choices), not just Tivit.
//! * The detector is pure: given vote text, it returns the synthesized
//!   `AbilityDefinition`. Failure to match returns `None`, leaving the caller
//!   free to fall back to the standard chain parser.

use crate::parser::oracle_nom::error::OracleError;
use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::Parser;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, PlayerFilter, VoterScope,
};

use super::oracle_effect::parse_effect_chain_with_context;
use super::oracle_ir::context::ParseContext;

/// Detect and parse the entire Council's-dilemma vote block. Returns a single
/// `AbilityDefinition` whose `effect` is `Effect::Vote` populated with the
/// per-choice sub-effects, or `None` if the input doesn't match the pattern.
///
/// The input is the trigger/effect *body* text — i.e., what comes after
/// "Whenever ~ enters or deals combat damage to a player, ". The "starting
/// with you, " prefix is consumed here (kept inside this module so chain-level
/// stripping in `parse_effect_chain_ir` doesn't interfere).
pub(crate) fn parse_vote_block(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    let lower = text.to_lowercase();
    // Phase 1: optional "starting with you," prefix.
    let (i, starting_with) =
        parse_starting_with(&lower).unwrap_or((lower.as_str(), ControllerRef::You));
    // Phase 2: opener clause. Two shapes covered:
    //   * "each player votes for <a> or <b>."         → VoterScope::AllPlayers
    //   * "each player may vote for <a> or <b>."      → VoterScope::AllPlayers
    //   * "each player chooses <a> or <b>."           → VoterScope::AllPlayers
    //   * "each opponent chooses <a> or <b>."         → VoterScope::EachOpponent
    //   * "each opponent may choose <a> or <b>."      → VoterScope::EachOpponent
    // CR 701.38c: "chooses" patterns aren't strict votes per the rules but
    // are mechanically identical for the engine's purposes — the resolver
    // tallies and fans out per-choice effects the same way.
    let (i, choices, voter_scope) = parse_each_player_votes_clause(i)?;
    if choices.len() < 2 {
        return None;
    }
    // Phase 3: per-choice clauses. Two shapes covered:
    //   * "For each <choice> vote, <effect>."                     (Tivit / classic)
    //   * "For each player who chose <choice>, <effect>."          (Master of Ceremonies)
    // Walk the text exactly once and key the parsed sub-effects by their
    // canonical `choices` index so the output array always matches
    // declaration order.
    let mut slots: Vec<Option<Box<AbilityDefinition>>> = (0..choices.len()).map(|_| None).collect();
    let mut walk = i.trim_start();
    while !walk.is_empty() {
        let (rest, (choice, effect_text, who_chose)) = parse_for_each_vote_clause(walk, &choices)?;
        let idx = choices.iter().position(|c| c == &choice)?;
        if slots[idx].is_some() {
            // Same choice referenced twice — shape we don't yet model.
            return None;
        }
        let mut parsed =
            parse_effect_chain_with_context(effect_text, kind, &mut ParseContext::default());
        if who_chose {
            // CR 608.2c + CR 701.38: "for each player who chose <choice>,
            // <effect>" routes the per-vote sub-effect to the controller plus
            // each voter who picked that option. The runtime player-scope
            // expansion (controller + matching voters) is encoded by
            // `PlayerFilter::VotedFor`.
            //
            // u8 fits trivially: vote-choice cardinality is bounded by Magic
            // card design (no card has ever exceeded ~5 choices).
            parsed.player_scope = Some(PlayerFilter::VotedFor {
                choice_index: idx as u8,
            });
        }
        slots[idx] = Some(Box::new(parsed));
        walk = rest.trim_start();
    }
    let per_choice_effect: Vec<Box<AbilityDefinition>> =
        slots.into_iter().collect::<Option<Vec<_>>>()?;

    Some(AbilityDefinition::new(
        kind,
        Effect::Vote {
            choices,
            per_choice_effect,
            starting_with,
            voter_scope,
        },
    ))
}

/// Parse the optional "starting with you, " prefix. Returns the unconsumed
/// remainder plus the resolved `ControllerRef`. Other phrasings ("starting
/// with the player to your left") map to `ControllerRef::You` until we model
/// player-position refs.
fn parse_starting_with(input: &str) -> Option<(&str, ControllerRef)> {
    let res: nom::IResult<&str, (), OracleError<'_>> = value(
        (),
        alt((tag("starting with you, "), tag("starting with you "))),
    )
    .parse(input);
    match res {
        Ok((rest, ())) => Some((rest, ControllerRef::You)),
        Err(_) => None,
    }
}

/// Parse the opener that precedes the vote choice list. Five shapes:
///
/// | Pattern                                      | `VoterScope`            |
/// |----------------------------------------------|-------------------------|
/// | `"each player votes for "`                   | `AllPlayers`            |
/// | `"each player may vote for "`                | `AllPlayers`            |
/// | `"each player chooses "`                     | `AllPlayers`            |
/// | `"each opponent chooses "`                   | `EachOpponent`          |
/// | `"each opponent may choose "`                | `EachOpponent`          |
///
/// Returns the unconsumed remainder, the lowercase choice list, and the
/// resolved voter scope.
///
/// Generalized to N>=2 choices via repeated " or " / ", " separators —
/// covers cards like Capital Punishment that vote on three options.
fn parse_each_player_votes_clause(input: &str) -> Option<(&str, Vec<String>, VoterScope)> {
    let res: nom::IResult<&str, VoterScope, OracleError<'_>> = alt((
        value(VoterScope::AllPlayers, tag("each player votes for ")),
        value(VoterScope::AllPlayers, tag("each player may vote for ")),
        value(VoterScope::EachOpponent, tag("each opponent chooses ")),
        value(VoterScope::EachOpponent, tag("each opponent may choose ")),
        value(VoterScope::AllPlayers, tag("each player chooses ")),
    ))
    .parse(input);
    let (rest, voter_scope) = res.ok()?;

    // Read the choice list: "<a>[, <b>][, <c>] or <last>." — allow "or"
    // separator for the last item, comma between earlier items.
    let (after, choice_list_text) = read_until_period(rest)?;
    let choices = split_choices(choice_list_text)?;
    Some((after, choices, voter_scope))
}

/// Parse a single "For each ..." clause. Two shapes are accepted:
///
/// 1. `"for each <choice> vote, <effect>."`            (Tivit / classic council's-dilemma)
/// 2. `"for each <player-noun> who chose <choice>, <effect>."` (Master of Ceremonies)
///
/// Returns the unconsumed remainder, the matched choice (lowercase), the
/// inner effect text, and a flag indicating whether the clause was the
/// "who chose" shape (which triggers `PlayerFilter::VotedFor` wiring on
/// the parsed sub-effect).
///
/// Whitespace handling:
/// * Accepts both upper- and lowercase "For"/"for".
/// * Consumes a trailing period if present.
fn parse_for_each_vote_clause<'a>(
    input: &'a str,
    choices: &[String],
) -> Option<(&'a str, (String, &'a str, bool))> {
    let lower = input.to_lowercase();
    let res: nom::IResult<&str, (), OracleError<'_>> =
        value((), tag("for each ")).parse(lower.as_str());
    let (lower_rest, ()) = res.ok()?;
    // Slice the original input at the same offset.
    let consumed = input.len() - lower_rest.len();
    let original_rest = &input[consumed..];

    // Try the "<player-noun> who chose <choice>, " shape first — its prefix
    // is alphabetic-leading just like the simple "<choice> vote, " shape, so
    // a successful match here unambiguously routes to the VotedFor wiring.
    if let Some((after_clause, choice_lower)) =
        parse_who_chose_player_clause(original_rest, choices)
    {
        let (effect_text, rest) = read_effect_until_next_clause(after_clause);
        return Some((rest, (choice_lower, effect_text, true)));
    }

    // Fallback: classic "<choice> vote, <effect>" shape.
    // Read the choice token (case-insensitive); choices are whitespace-free
    // single words in canonical Council's-dilemma cards.
    let (choice, after_choice) = read_word(original_rest)?;
    let choice_lower = choice.to_lowercase();
    if !choices.iter().any(|c| c == &choice_lower) {
        return None;
    }
    // Consume " vote, " (singular) — plural "votes" would imply the resolver
    // re-tally pattern that Council's dilemma never uses; reject to keep the
    // detector tight.
    let (after_vote, _): (&str, &str) = tag::<_, _, OracleError<'_>>(" vote, ")
        .parse(after_choice)
        .ok()?;
    // Read up to terminator: either next "For each " OR end-of-string,
    // stripping trailing period.
    let (effect_text, rest) = read_effect_until_next_clause(after_vote);
    Some((rest, (choice_lower, effect_text, false)))
}

/// Parse the "who chose" sub-shape of a `for each ...` clause:
///
///   `"<player-noun> who chose <choice>, "`
///
/// where `<player-noun>` is `"player"` or `"opponent"` and `<choice>` must
/// be a member of the parent vote's `choices` list. Returns the remainder
/// after the trailing `", "` and the matched choice (lowercase).
fn parse_who_chose_player_clause<'a>(
    input: &'a str,
    choices: &[String],
) -> Option<(&'a str, String)> {
    let res: nom::IResult<&str, (), OracleError<'_>> =
        value((), alt((tag("player"), tag("opponent")))).parse(input);
    let (after_noun, ()) = res.ok()?;
    let (after_who, _): (&str, &str) = tag::<_, _, OracleError<'_>>(" who chose ")
        .parse(after_noun)
        .ok()?;
    let (choice_word, after_choice) = read_word(after_who)?;
    let choice_lower = choice_word.to_lowercase();
    if !choices.iter().any(|c| c == &choice_lower) {
        return None;
    }
    let (after_comma, _): (&str, &str) = tag::<_, _, OracleError<'_>>(", ")
        .parse(after_choice)
        .ok()?;
    Some((after_comma, choice_lower))
}

/// Read a maximal prefix up to (but not including) the next "For each "
/// clause or end of input. Strips a trailing period from the consumed slice.
fn read_effect_until_next_clause(input: &str) -> (&str, &str) {
    let lower = input.to_lowercase();
    // Find the next "for each " case-insensitively. structural: not dispatch
    // — this is a local sentence-boundary scanner, not a parser dispatch
    // decision. We use lowercase for the search but slice the original input
    // so casing is preserved in the returned effect text.
    let cut = lower
        .match_indices("for each ")
        .find(|(idx, _)| {
            *idx == 0
                || matches!(
                    lower.as_bytes().get(*idx - 1),
                    Some(b' ') | Some(b'.') | Some(b',')
                )
        })
        .map(|(idx, _)| idx)
        .unwrap_or(input.len());
    let head = &input[..cut];
    let tail = &input[cut..];
    let head_trimmed = head.trim_end();
    // allow-noncombinator: structural period strip on pre-extracted sentence clause
    let head_no_period = head_trimmed.strip_suffix('.').unwrap_or(head_trimmed);
    (head_no_period.trim(), tail.trim_start())
}

/// Read a word (alphanumeric + apostrophes). Returns (word, remainder).
fn read_word(input: &str) -> Option<(&str, &str)> {
    let end = input
        .char_indices()
        .find(|(_, c)| !c.is_alphanumeric() && *c != '\'' && *c != '-')
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    if end == 0 {
        return None;
    }
    Some((&input[..end], &input[end..]))
}

/// Read characters up to and including a period; return the substring before
/// the period and the remainder after it.
fn read_until_period(input: &str) -> Option<(&str, &str)> {
    let idx = input.find('.')?;
    Some((&input[idx + 1..], &input[..idx]))
}

/// Split a list like "evidence or bribery" or "guards, hounds, or dragons"
/// into individual lowercase choices. Returns `None` if fewer than two
/// choices were found.
///
/// Uses nom to consume word tokens separated by `", or "`, `" or "`, or `", "` —
/// handling the standard MTG list formats without string-splitting on raw bytes.
fn split_choices(input: &str) -> Option<Vec<String>> {
    let lower = input.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let word_chars = |c: char| c.is_alphanumeric() || c == '\'' || c == '-';
    let mut choices: Vec<String> = Vec::new();
    let mut rest: &str = lower.as_str();
    loop {
        let (after_word, word) =
            nom::bytes::complete::take_while1::<_, &str, OracleError<'_>>(word_chars)
                .parse(rest)
                .ok()?;
        choices.push(word.to_string());
        rest = after_word;
        if rest.is_empty() {
            break;
        }
        // Consume separator; try longest match first to avoid partial matches.
        let sep_res: nom::IResult<&str, (), OracleError<'_>> =
            value((), alt((tag(", or "), tag(" or "), tag(", ")))).parse(rest);
        let (after_sep, ()) = sep_res.ok()?;
        rest = after_sep;
    }
    if choices.len() < 2 {
        return None;
    }
    Some(choices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TargetFilter;

    #[test]
    fn parses_tivit_vote_block() {
        let text = "starting with you, each player votes for evidence or bribery. For each evidence vote, investigate. For each bribery vote, create a Treasure token.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                starting_with,
                voter_scope,
            } => {
                assert_eq!(
                    choices,
                    &vec!["evidence".to_string(), "bribery".to_string()]
                );
                assert_eq!(per_choice_effect.len(), 2);
                assert_eq!(starting_with, ControllerRef::You);
                assert_eq!(voter_scope, VoterScope::AllPlayers);
                // First per-choice = Investigate
                assert!(matches!(*per_choice_effect[0].effect, Effect::Investigate));
                // Second per-choice = Token (Treasure)
                assert!(matches!(*per_choice_effect[1].effect, Effect::Token { .. }));
                // Classic Tivit shape: per-choice sub-effects do not carry a
                // VotedFor scope (they fan out per-vote, not per-voter).
                assert!(per_choice_effect[0].player_scope.is_none());
                assert!(per_choice_effect[1].player_scope.is_none());
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// CR 800.4g: Master of Ceremonies's full upkeep-trigger body — three
    /// choices, `EachOpponent` voter scope, and "for each player who chose X,
    /// you and that player each Y" per-choice clauses. This is the canonical
    /// regression test for the bug fix this module was generalized to support.
    #[test]
    fn parses_master_of_ceremonies_vote_block() {
        let text = "each opponent chooses money, friends, or secrets. For each player who chose money, you and that player each create a Treasure token. For each player who chose friends, you and that player each create a 1/1 green and white Citizen creature token. For each player who chose secrets, you and that player each draw a card.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                ref per_choice_effect,
                voter_scope,
                ..
            } => {
                assert_eq!(
                    choices,
                    &vec![
                        "money".to_string(),
                        "friends".to_string(),
                        "secrets".to_string()
                    ]
                );
                assert_eq!(voter_scope, VoterScope::EachOpponent);
                assert_eq!(per_choice_effect.len(), 3);
                // Each per-choice sub-effect is wired to PlayerFilter::VotedFor
                // with its own choice index.
                assert_eq!(
                    per_choice_effect[0].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 0 })
                );
                assert_eq!(
                    per_choice_effect[1].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 1 })
                );
                assert_eq!(
                    per_choice_effect[2].player_scope,
                    Some(PlayerFilter::VotedFor { choice_index: 2 })
                );

                // CR 109.5: Each per-choice body has been distributed by the
                // compound-subject combinator. The top-level effect's recipient
                // is `OriginalController`; the second half is in `sub_ability`
                // with `ScopedPlayer`.
                let assert_distributed = |idx: usize, label: &str| {
                    let body = &per_choice_effect[idx];
                    let top_target = match &*body.effect {
                        Effect::Token { owner, .. } => owner.clone(),
                        Effect::Draw { target, .. } => target.clone(),
                        other => panic!("[{}] unexpected per_choice top effect {:?}", label, other),
                    };
                    assert_eq!(
                        top_target,
                        TargetFilter::OriginalController,
                        "[{}] top half must target OriginalController",
                        label
                    );
                    let sub = body
                        .sub_ability
                        .as_ref()
                        .unwrap_or_else(|| panic!("[{}] expected per_choice sub_ability", label));
                    let sub_target = match &*sub.effect {
                        Effect::Token { owner, .. } => owner.clone(),
                        Effect::Draw { target, .. } => target.clone(),
                        other => panic!("[{}] unexpected per_choice sub effect {:?}", label, other),
                    };
                    assert_eq!(
                        sub_target,
                        TargetFilter::ScopedPlayer,
                        "[{}] sub half must target ScopedPlayer",
                        label
                    );
                };
                assert_distributed(0, "money");
                assert_distributed(1, "friends");
                assert_distributed(2, "secrets");
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Two-choice variant of the "each opponent chooses ..." pattern.
    #[test]
    fn parses_each_opponent_chooses_two_options() {
        let text = "each opponent chooses left or right. For each player who chose left, you and that player each draw a card. For each player who chose right, you and that player each draw a card.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                voter_scope,
                ..
            } => {
                assert_eq!(choices, &vec!["left".to_string(), "right".to_string()]);
                assert_eq!(voter_scope, VoterScope::EachOpponent);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Three-choice variant of the "each opponent chooses ..." pattern.
    #[test]
    fn parses_each_opponent_chooses_three_options() {
        let text = "each opponent chooses one, two, or three. For each player who chose one, you and that player each draw a card. For each player who chose two, you and that player each draw a card. For each player who chose three, you and that player each draw a card.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                voter_scope,
                ref per_choice_effect,
                ..
            } => {
                assert_eq!(choices.len(), 3);
                assert_eq!(per_choice_effect.len(), 3);
                assert_eq!(voter_scope, VoterScope::EachOpponent);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    /// Single-choice opener must be rejected — `parse_vote_block` requires
    /// at least two choices to avoid false-positives on unrelated text.
    #[test]
    fn rejects_each_opponent_with_only_one_choice() {
        let text = "each opponent chooses money. For each player who chose money, you and that player each draw a card.";
        // `split_choices` requires N>=2 — single-choice input fails the
        // detector outright.
        assert!(parse_vote_block(text, AbilityKind::Spell).is_none());
    }

    /// Regression: serialized vote effects from the previous schema
    /// (without `voter_scope`) deserialize as `VoterScope::AllPlayers`.
    /// We don't have direct access to a stale JSON blob here; instead,
    /// confirm the classic "starting with you, each player votes for ..."
    /// path produces `AllPlayers`, which is what the serde default emits.
    #[test]
    fn tivit_test_still_passes_with_default_voter_scope() {
        let text = "starting with you, each player votes for evidence or bribery. For each evidence vote, investigate. For each bribery vote, create a Treasure token.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        if let Effect::Vote { voter_scope, .. } = *def.effect {
            assert_eq!(voter_scope, VoterScope::AllPlayers);
        } else {
            panic!("expected Vote effect");
        }
    }

    /// Direct unit test for the "<player-noun> who chose <choice>, " sub-clause.
    #[test]
    fn parses_for_each_player_who_chose_money_clause() {
        let choices = vec!["money".to_string(), "friends".to_string()];
        let (rest, choice) =
            parse_who_chose_player_clause("player who chose money, do stuff", &choices)
                .expect("clause parses");
        assert_eq!(choice, "money");
        assert_eq!(rest, "do stuff");
        // Same with "opponent".
        let (rest2, choice2) =
            parse_who_chose_player_clause("opponent who chose friends, draw a card", &choices)
                .expect("clause parses");
        assert_eq!(choice2, "friends");
        assert_eq!(rest2, "draw a card");
    }

    /// Regression: existing N=3 voting card (Capital Punishment is the public
    /// reference; here we use its grammatical shape with stand-in choices).
    #[test]
    fn parses_capital_punishment_three_choice_vote() {
        let text = "starting with you, each player votes for first, second, or third. For each first vote, draw a card. For each second vote, investigate. For each third vote, create a Treasure token.";
        let def = parse_vote_block(text, AbilityKind::Spell).expect("vote block parses");
        match *def.effect {
            Effect::Vote {
                ref choices,
                voter_scope,
                ref per_choice_effect,
                ..
            } => {
                assert_eq!(choices.len(), 3);
                assert_eq!(per_choice_effect.len(), 3);
                assert_eq!(voter_scope, VoterScope::AllPlayers);
            }
            other => panic!("expected Vote, got {:?}", other),
        }
    }

    #[test]
    fn rejects_non_vote_text() {
        assert!(parse_vote_block("Draw a card.", AbilityKind::Spell).is_none());
    }

    /// CR 608.2c + CR 701.38: Documented parser gap (R5 in the
    /// implementation plan). The Master of Ceremonies vote skeleton parses
    /// correctly (see `parses_master_of_ceremonies_vote_block`), but the
    /// per-choice effect text "you and that player each create a Treasure
    /// token" is NOT yet distributed into a 2-element chain by
    /// `parse_effect_chain_with_context`.
    ///
    /// The current parser produces:
    ///   * top effect: `Effect::Unimplemented { name: "you", description: "you" }`
    ///   * sub_ability: `Effect::Draw { count: 1, target: Any }` (subject lost)
    ///
    /// The architecturally correct fix is to teach `oracle_effect` to
    /// recognize "<player-noun-A> and <player-noun-B> each Y" and emit a
    /// chain of two parallel sub-effects (one targeting `Controller`, one
    /// targeting `ScopedPlayer`/the recorded voter). That work is non-trivial
    /// new parser infrastructure (a new combinator + scoped-player wiring)
    /// and is therefore out of scope for this PR per the plan's R5 risk
    /// gate. Tracked as a follow-up.
    ///
    /// This test pins the current behavior so the gap is visible in the
    /// test suite and so any future fix updates this assertion in lockstep.
    /// CR 109.5 + CR 608.2c + CR 800.4g: "you and that player each Y" must
    /// distribute the body across two recipients. The first half is targeted
    /// at `OriginalController` (the printed ability controller); the second
    /// half is targeted at `ScopedPlayer` (the iterated voter from
    /// `PlayerFilter::VotedFor`). Halves chain via `sub_ability`.
    ///
    /// This was originally a documented gap test that pinned `Unimplemented`;
    /// it is now the positive regression for the R5 distribution combinator.
    #[test]
    fn parser_distributes_you_and_that_player_each_draw() {
        let parsed = parse_effect_chain_with_context(
            "you and that player each draw a card",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        match *parsed.effect {
            Effect::Draw { ref target, .. } => {
                assert_eq!(*target, TargetFilter::OriginalController);
            }
            other => panic!(
                "expected Draw {{ target: OriginalController }} for first half, got {:?}",
                other
            ),
        }
        let sub = parsed
            .sub_ability
            .expect("expected second-half sub_ability");
        match *sub.effect {
            Effect::Draw { ref target, .. } => {
                assert_eq!(*target, TargetFilter::ScopedPlayer);
            }
            other => panic!(
                "expected Draw {{ target: ScopedPlayer }} for second half, got {:?}",
                other
            ),
        }
    }

    /// "you and that player each create a Treasure token" — the canonical
    /// Master of Ceremonies "money" reward. Each half is `Effect::Token`
    /// with its `owner` field rewritten.
    #[test]
    fn parser_distributes_you_and_that_player_each_create_token() {
        let parsed = parse_effect_chain_with_context(
            "you and that player each create a Treasure token",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        match *parsed.effect {
            Effect::Token { ref owner, .. } => {
                assert_eq!(*owner, TargetFilter::OriginalController);
            }
            other => panic!("expected Token for first half, got {:?}", other),
        }
        let sub = parsed
            .sub_ability
            .expect("expected second-half sub_ability");
        match *sub.effect {
            Effect::Token { ref owner, .. } => {
                assert_eq!(*owner, TargetFilter::ScopedPlayer);
            }
            other => panic!("expected Token for second half, got {:?}", other),
        }
    }

    /// Full-line typed-token body (Citizen reward path): "1/1 green and white
    /// Citizen creature token" must round-trip through the body parser and
    /// retain its full type description on both halves.
    #[test]
    fn parser_distributes_you_and_that_player_each_chain_with_typed_token() {
        let parsed = parse_effect_chain_with_context(
            "you and that player each create a 1/1 green and white Citizen creature token",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        match *parsed.effect {
            Effect::Token {
                ref owner,
                ref types,
                ..
            } => {
                assert_eq!(*owner, TargetFilter::OriginalController);
                assert!(
                    types.iter().any(|t| t.eq_ignore_ascii_case("citizen")),
                    "expected types to include Citizen, got {:?}",
                    types
                );
            }
            other => panic!("expected Token for first half, got {:?}", other),
        }
        let sub = parsed
            .sub_ability
            .expect("expected second-half sub_ability");
        match *sub.effect {
            Effect::Token {
                ref owner,
                ref types,
                ..
            } => {
                assert_eq!(*owner, TargetFilter::ScopedPlayer);
                assert!(
                    types.iter().any(|t| t.eq_ignore_ascii_case("citizen")),
                    "expected sub types to include Citizen, got {:?}",
                    types
                );
            }
            other => panic!("expected Token for second half, got {:?}", other),
        }
    }
}
