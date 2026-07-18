// CR 604 / CR 613 — shared static parser infrastructure.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;
use nom::character::complete::anychar;
use nom::combinator::not;
use nom::multi::many1;

/// CR 702.11e + CR 609.4 + CR 702.21a: Parse the "[subject] can be the targets
/// of spells and abilities[ you control] as though they didn't have hexproof[.
/// Ward abilities of those creatures don't trigger]" static pair (Nowhere to
/// Run, Glaring Spotlight).
///
/// Sentence 1 → `StaticMode::IgnoreHexproof` scoped to `<subject>` via the
/// definition's `affected` filter (CR 702.11e — the bypass lets the matched
/// permanents be targeted as though they had no hexproof). The optional "you
/// control" qualifier restricts the beneficiary (`bypass_beneficiary`). Optional
/// sentence 2
/// → `StaticMode::SuppressTriggers { source_filter: <same subject>, events:
/// [BecomesTargeted] }` (CR 702.21a — "those creatures" anaphors sentence 1's
/// subject, so the parsed filter is reused rather than re-derived).
///
/// Parsed as one unit (before generic sentence splitting) so the anaphoric
/// "those creatures" keeps its antecedent. When the ward sentence is present but
/// unrecognized trailing prose follows, the whole line is deferred (`None`)
/// rather than silently dropping a clause.
pub(crate) fn parse_ignore_hexproof_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    // Sentence 1: subject up to the hexproof-bypass clause.
    let (after_subject, subject) = take_until::<_, _, OracleError<'_>>(" can be the target")
        .parse(tp.lower)
        .ok()?;
    let bypass: OracleResult<'_, bool> = (|| {
        let (i, _) = tag::<_, _, OracleError<'_>>(" can be the target").parse(after_subject)?;
        let (i, _) = opt(tag::<_, _, OracleError<'_>>("s")).parse(i)?;
        let (i, _) = tag::<_, _, OracleError<'_>>(" of spells and abilities").parse(i)?;
        // CR 702.11e + CR 609.4: an optional "you control" qualifier restricts
        // which spells and abilities bypass hexproof to the static controller's
        // (Glaring Spotlight — "spells and abilities you control"). Its presence
        // is semantically load-bearing in multiplayer: without it (Nowhere to
        // Run) every player's spells and abilities gain the bypass; with it, only
        // the controller's do. The flag drives `bypass_beneficiary` below.
        let (i, you_control) = opt(tag::<_, _, OracleError<'_>>(" you control")).parse(i)?;
        let (i, _) = tag::<_, _, OracleError<'_>>(" as though ").parse(i)?;
        // CR 702.11e: plural ("they") or singular ("it") subject pronoun.
        let (i, _) = alt((
            tag::<_, _, OracleError<'_>>("they didn't"),
            tag::<_, _, OracleError<'_>>("it didn't"),
        ))
        .parse(i)?;
        let (i, _) = tag::<_, _, OracleError<'_>>(" have hexproof").parse(i)?;
        Ok((i, you_control.is_some()))
    })();
    let (rest, you_control) = bypass.ok()?;
    // CR 109.5: "you control" resolves relative to the static's source
    // controller, so the beneficiary is `ControllerRef::You`; absent, the bypass
    // benefits every player (`None`).
    let beneficiary = you_control.then_some(ControllerRef::You);

    // Map the subject phrase to a typed filter; require it to fully consume so a
    // partial parse never silently scopes the bypass wider than written.
    let (filter, filter_remainder) = parse_type_phrase(subject.trim());
    if !filter_remainder.trim().is_empty() || matches!(filter, TargetFilter::Any) {
        return None;
    }

    let mut defs = vec![StaticDefinition::new(StaticMode::IgnoreHexproof)
        .affected(filter.clone())
        .bypass_beneficiary(beneficiary)
        .description(text.to_string())];

    // Optional sentence 2: ward suppression for the same subject.
    let after_bypass = rest.trim_start_matches('.').trim_start();
    if !after_bypass.is_empty() {
        let ward: OracleResult<'_, ()> = (|| {
            let (i, _) =
                tag::<_, _, OracleError<'_>>("ward abilities of those creatures don't trigger")
                    .parse(after_bypass)?;
            let (i, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(i.trim())?;
            Ok((i, ()))
        })();
        let (ward_rest, ()) = ward.ok()?;
        // Any unconsumed prose means this isn't a clean hexproof+ward line.
        if !ward_rest.trim().is_empty() {
            return None;
        }
        defs.push(
            StaticDefinition::new(StaticMode::SuppressTriggers {
                source_filter: filter,
                events: vec![SuppressedTriggerEvent::BecomesTargeted],
            })
            .description(text.to_string()),
        );
    }

    Some(defs)
}

/// CR 109.5 vs CR 102.1 + structural distributive: the pronoun-binding axis
/// of an "only during X turn(s)" prohibition.
///
/// - `SourceRelative` ≡ "your turn" — CR 109.5 binds to the static's source
///   controller (Fires of Invention).
/// - `PerAffected` ≡ "their own turn(s)" — distributive per-affected-player
///   binding (Dosan, City of Solitude). The CompRules don't carve out a
///   specific pronoun rule for "their"; the distributive reading follows from
///   CR 102.1 + the template structure of "[every player] can [action] only
///   during their own [time]".
///
/// This enum is parser-internal — it never appears on `StaticMode`. The
/// resulting `CastingProhibitionCondition` (`NotDuringYourTurn` vs
/// `NotDuringAffectedPlayersTurn`) carries the binding axis into the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WhenKind {
    SourceRelative,
    PerAffected,
}

/// Parse the trailing `"only during {your | their own} turn(s?)"` clause and
/// return the typed binding axis.
///
/// Composed from nested `alt()` calls — one axis per choice — not enumerated
/// as 4 full-string permutations. Adding "his or her" or "each player's own"
/// is a single new `value(WhenKind::_, tag("..."))` arm.
///
/// Grammar:
///   "only during " (`"your"` | `"their own"`) " turn" `"s"?` `"."?`
///
/// Returns `(remaining_input, WhenKind)` on success.
pub(crate) fn parse_when_clause(input: &str) -> OracleResult<'_, WhenKind> {
    let (input, _) = tag::<_, _, OracleError<'_>>("only during ").parse(input)?;
    let (input, kind) = alt((
        value(WhenKind::SourceRelative, tag("your")),
        value(WhenKind::PerAffected, tag("their own")),
    ))
    .parse(input)?;
    let (input, _) = tag(" turn").parse(input)?;
    let (input, _) = opt(tag("s")).parse(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;
    Ok((input, kind))
}

/// Map a `WhenKind` to its `CastingProhibitionCondition`. Single-authority
/// mapper so the binding axis lives in exactly one place.
pub(crate) fn when_kind_to_condition(kind: WhenKind) -> CastingProhibitionCondition {
    match kind {
        WhenKind::SourceRelative => CastingProhibitionCondition::NotDuringYourTurn,
        WhenKind::PerAffected => CastingProhibitionCondition::NotDuringAffectedPlayersTurn,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AloneCombatRestriction {
    Attack,
    Block,
    AttackOrBlock,
}

pub(crate) fn parse_alone_combat_restriction(
    input: &str,
) -> OracleResult<'_, AloneCombatRestriction> {
    terminated(
        alt((
            value(
                AloneCombatRestriction::AttackOrBlock,
                tag("can't attack or block alone"),
            ),
            value(AloneCombatRestriction::Attack, tag("can't attack alone")),
            value(AloneCombatRestriction::Block, tag("can't block alone")),
        )),
        opt(tag(".")),
    )
    .parse(input)
}

/// Try matching a nom `tag()` against the lowercase text, returning the remaining original-case
/// text on success. This bridges nom's exact-match combinators with the TextPair dual-string
/// pattern used throughout the parser.
pub(crate) fn nom_tag_lower<'a>(text: &'a str, lower: &str, prefix: &str) -> Option<&'a str> {
    tag::<_, _, OracleError<'_>>(prefix)
        .parse(lower)
        .ok()
        .map(|(_, matched)| &text[matched.len()..])
}

/// Like `nom_tag_lower`, but operates on a `TextPair` and returns a new `TextPair`
/// with both original and lowercase remainders advanced past the matched prefix.
pub(crate) fn nom_tag_tp<'a>(tp: &TextPair<'a>, prefix: &str) -> Option<TextPair<'a>> {
    tag::<_, _, OracleError<'_>>(prefix)
        .parse(tp.lower)
        .ok()
        .map(|(rest_lower, matched)| {
            let rest_original = &tp.original[matched.len()..];
            TextPair::new(rest_original, rest_lower)
        })
}

/// Recognizes the first token/phrase of an effect clause that follows the
/// condition-vs-effect comma in an inverted `"As long as <cond>, <effect>"` line.
///
/// Every alternative ends on a word boundary (trailing space or apostrophe) so
/// `tag("it ")` does not accept `"its "`. The set is derived from the 134-row
/// corpus of currently-affected cards in `client/public/card-data.json` and is
/// intentionally conservative: bare nouns/verbs that commonly appear inside
/// condition clauses (e.g. `"creatures "`, `"lands "`, `"a "`) are omitted.
pub(crate) fn parse_effect_subject_prefix(input: &str) -> OracleResult<'_, ()> {
    alt((
        // Self-reference pronouns ("it …", "it's …").
        value(
            (),
            alt((
                tag("it "),
                tag("it's "),
                tag("it has "),
                tag("it gets "),
                tag("it can "),
                tag("it assigns "),
                tag("it deals "),
                tag("it doesn't "),
            )),
        ),
        // Self-reference tilde token.
        value(
            (),
            alt((
                tag("~ "),
                tag("~'s "),
                tag("~ is "),
                tag("~ has "),
                tag("~ gets "),
                tag("~ can "),
                tag("~ and "),
            )),
        ),
        // Anaphoric subjects for paired/attached/enchanted interactions.
        value(
            (),
            alt((
                tag("that creature "),
                tag("those creatures "),
                tag("both creatures "),
                tag("each of those "),
                tag("that permanent "),
                tag("that card "),
            )),
        ),
        // Typed bulk subjects.
        value(
            (),
            alt((
                tag("each "),
                tag("all "),
                tag("other "),
                tag("enchanted "),
                tag("equipped "),
                tag("creatures you control "),
                tag("lands you control "),
                tag("permanents you control "),
                tag("cards in your hand "),
                tag("cards in your graveyard "),
                tag("the top card "),
                tag("the turn order "),
                tag("the first time "),
            )),
        ),
        // Player-directed and global subjects.
        value(
            (),
            alt((
                tag("you may "),
                tag("you can't "),
                tag("you control "),
                tag("you "),
                tag("players "),
                tag("no more than "),
                tag("defending player "),
                tag("each opponent "),
                tag("each player "),
            )),
        ),
        // Effect-starter verbs/nouns (when no explicit subject).
        value(
            (),
            alt((
                tag("if "),
                tag("prevent "),
                tag("damage "),
                tag("untap all "),
                tag("they "),
            )),
        ),
    ))
    .parse(input)
    .map(|(rest, _)| (rest, ()))
}

/// Scan `tp.lower` for the first `", "` whose tail begins with a recognized
/// effect-subject prefix (see `parse_effect_subject_prefix`). Returns the
/// `(condition, effect)` halves, each as a `TextPair` aligned with the source.
///
/// Uses `match_indices(", ")` for structural iteration over candidate split
/// points (not for parsing dispatch); the dispatch itself is a nom combinator.
/// This mirrors the word-boundary-scan pattern used by `scan_timing_restrictions`
/// in `oracle_casting.rs`.
pub(crate) fn split_on_effect_subject_comma<'a>(
    tp: &TextPair<'a>,
) -> Option<(TextPair<'a>, TextPair<'a>)> {
    for (pos, sep) in tp.lower.match_indices(", ") {
        let after = pos + sep.len();
        let tail_lower = &tp.lower[after..];
        if parse_effect_subject_prefix(tail_lower).is_ok() {
            let (condition, _) = tp.split_at(pos);
            let (_, effect) = tp.split_at(after);
            return Some((condition, effect));
        }
    }
    None
}

/// Result of splitting an inverted `"As long as <cond>, <effect>"` line.
pub(crate) struct InvertedSplit {
    /// Canonical-form rewrite `"<effect> as long as <condition>"` ready for
    /// re-dispatch through `parse_static_line_inner`.
    pub(super) canonical: String,
    /// The effect clause in original case.
    pub(super) effect_text: String,
    /// The condition clause in original case, suitable for
    /// `StaticCondition::Unrecognized { text }` when the recursed parse fails.
    pub(super) condition_text: String,
}

/// Detect inverted static form `"As long as <condition>, <effect>"` and split
/// it into a canonical rewrite plus the isolated condition text. Returns
/// `None` when the line does not start with `"as long as "` or when no comma
/// boundary has a recognized effect-subject tail (in which case the caller
/// falls through to the existing generic fallback, preserving today's
/// behavior).
///
/// CR 611.3a: Continuous effects from static abilities apply when their stated
/// condition is true; orientation of the condition clause in the printed text
/// is irrelevant to rules semantics.
pub(crate) fn try_split_inverted_as_long_as(tp: &TextPair<'_>) -> Option<InvertedSplit> {
    let rest = nom_tag_tp(tp, "as long as ")?;
    // Trim a trailing period from both sides before splitting so the canonical
    // form does not carry a stray `.` at the condition boundary.
    let trimmed_original = rest.original.trim_end_matches('.');
    let trimmed_lower = rest.lower.trim_end_matches('.');
    let body = TextPair::new(trimmed_original, trimmed_lower);
    let (condition, effect) = split_on_effect_subject_comma(&body)?;
    let condition_text = condition.original.trim().to_string();
    let effect_text = effect.original.trim();
    let canonical = format!("{effect_text} as long as {condition_text}");
    Some(InvertedSplit {
        canonical,
        effect_text: effect_text.to_string(),
        condition_text,
    })
}

pub(crate) fn try_parse_inverted_attached_subject_grant(
    split: &InvertedSplit,
    description: &str,
) -> Option<StaticDefinition> {
    let condition_lower = split.condition_text.to_lowercase();
    let affected = parse_attached_subject_qualifier(&condition_lower)?;

    let effect_lower = split.effect_text.to_lowercase();
    let effect_tp = TextPair::new(&split.effect_text, &effect_lower);
    let predicate = nom_tag_tp(&effect_tp, "it ").or_else(|| nom_tag_tp(&effect_tp, "they "))?;

    parse_continuous_gets_has(predicate.original, affected, description)
}

/// CR 508.1a + CR 611.3a + CR 613.1f: Inverted attached-subject grant gated on
/// the host creature's COMBAT STATE — "As long as equipped/enchanted creature is
/// attacking|blocking, it has/gets <X> [and <unmodeled conjunct>]" (Ace's
/// Baseball Bat, Slayer's-Cleaver-style lure compounds).
///
/// Distinct from `try_parse_inverted_attached_subject_grant` (which keys on a
/// STATIC characteristic and folds it into `affected`): combat state is
/// re-evaluated each layer cycle (CR 611.3a), so it is bound as a
/// `RecipientMatchesFilter` GATE on the recipient (the host creature) instead of
/// folded into the filter. Gating on the source (the Equipment/Aura) — as the
/// generic inverted fallback did via `SourceIsAttacking` — is wrong: an
/// Equipment is never an attacker, so the static never fires, and the keyword
/// would land on the Equipment rather than the host.
///
/// Returns a `Vec` so that each conjunct of the effect predicate is modeled
/// independently: the P/T + keyword grants merge into one gated `Continuous`
/// static, recognized combat requirements ("must be blocked if able", "is
/// goaded") become gated rule-statics, and the FILTERED "must be blocked by a
/// Dalek if able" conjunct lowers to the typed `MustBeBlocked { by: Some(filter)
/// }` requirement gated on the same combat condition (CR 509.1c). Only a
/// conjunct that none of these recognize is surfaced as a sibling
/// `Effect::Unimplemented` residual rather than being silently dropped — an
/// honest coverage signal (`is_static_supported` / `any_ability_has_unimplemented`)
/// independent of the whole-card `"condition":{` suppression that the supported
/// static's gate would otherwise trip in `detect_condition_if`. An
/// `Unrecognized`-condition companion is NOT used for residuals: it would
/// suppress `detect_condition_if` (cond_markers include `"condition":{`) AND be
/// runtime-active (`layers.rs` evaluates `Unrecognized => true`). CR 509.1c.
pub(crate) fn try_parse_inverted_attached_combat_grant(
    split: &InvertedSplit,
    description: &str,
) -> Vec<StaticDefinition> {
    let condition_lower = split.condition_text.to_lowercase();
    // CR 611.3a: bind the combat state to the recipient, not the source.
    let Ok((cond_rest, (affected, combat_prop))) =
        nom_condition::parse_attached_subject_combat_state(&condition_lower)
    else {
        return Vec::new();
    };
    if !cond_rest.trim().is_empty() {
        return Vec::new();
    }

    let effect_lower = split.effect_text.to_lowercase();
    let effect_tp = TextPair::new(&split.effect_text, &effect_lower);
    // Strip the anaphoric subject ("it "/"they ") to reach the bare predicate.
    let Some(predicate) = nom_tag_tp(&effect_tp, "it ").or_else(|| nom_tag_tp(&effect_tp, "they "))
    else {
        return Vec::new();
    };
    let predicate_body = predicate.original.trim();

    // CR 613.1f: parse the whole predicate ("has first strike and must be
    // blocked by a Dalek if able") into its modeled continuous modifications
    // (P/T + keyword grants). `parse_continuous_modifications` is the single
    // authority for the grant portion and merges everything it can model; it does
    // not consume combat-requirement / lure conjuncts ("must be blocked …").
    let modifications = parse_continuous_modifications(predicate_body);

    // CR 611.3a + CR 508.1a: gate the supported grant on the recipient (the
    // equipped/enchanted creature) being in the stated combat state.
    let gate = StaticCondition::RecipientMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::creature().properties(vec![combat_prop])),
    };
    let mut defs = Vec::new();
    if !modifications.is_empty() {
        defs.push(
            StaticDefinition::continuous()
                .affected(affected.clone())
                .modifications(modifications)
                .condition(gate.clone())
                .description(description.to_string()),
        );
    }

    // CR 613.1f: identify conjuncts NOT covered by the grant above — the
    // combat-requirement / lure conjuncts. Strip a leading verb ("has "/"have ")
    // and split the conjunction; a conjunct that `parse_continuous_modifications`
    // (with the verb re-attached) cannot model is a residual to classify below.
    let body_lower = predicate_body.to_lowercase();
    let list_input = nom_tag_lower(predicate_body, &body_lower, "has ")
        .or_else(|| nom_tag_lower(predicate_body, &body_lower, "have "))
        .unwrap_or(predicate_body);
    let mut residual_conjuncts: Vec<String> = Vec::new();
    for part in split_keyword_list(list_input.trim().trim_end_matches('.')) {
        let conjunct = part.trim();
        if conjunct.is_empty() {
            continue;
        }
        let conjunct_lower = conjunct.to_lowercase();
        // A conjunct that carries its own grant verb keeps that verb; a bare
        // keyword conjunct is re-parsed as a grant only when re-prefixed with
        // "has ". If neither form yields a continuous modification, it is a
        // requirement/lure residual.
        let grant_probe = if parse_grant_conjunct_verb(&conjunct_lower).is_ok() {
            conjunct.to_string()
        } else {
            format!("has {conjunct}")
        };
        if parse_continuous_modifications(&grant_probe).is_empty() {
            residual_conjuncts.push(conjunct.to_string());
        }
    }

    for residual_text in residual_conjuncts {
        // CR 508.1d / CR 509.1c / CR 701.15b: A conjunct `push_grant_clause_modifications`
        // can't model may still be a recognized combat REQUIREMENT ("must be blocked
        // if able", "attacks each combat if able", "is goaded"). Recover it via the
        // rule-static predicate combinator and emit a sibling rule-static gated on the
        // same combat condition — modeled, not an `Unimplemented` residual. (The
        // FILTERED "must be blocked by a Dalek if able" form is handled by the typed
        // `MustBeBlocked { by }` branch below, not this bare-form combinator.)
        let residual_lower = residual_text.to_lowercase();
        if let Ok((rest, predicate)) =
            all_consuming(parse_rule_static_predicate_nom).parse(residual_lower.trim())
        {
            let _ = rest;
            let mut companion = lower_rule_static(predicate, affected.clone(), &residual_text);
            companion.condition = Some(gate.clone());
            defs.push(companion);
            continue;
        }

        // CR 509.1c: the FILTERED "must be blocked by <quality> if able" conjunct
        // (Ace's Baseball Bat: "must be blocked by a Dalek if able") lowers to the
        // typed `MustBeBlocked { by: Some(filter) }` requirement, gated on the same
        // combat condition as the grant (so it inherits the "as long as ~ is
        // attacking" gate). Modeled, not an `Unimplemented` residual.
        if let Some(filter) = parse_must_be_blocked_by_filter(&residual_lower) {
            defs.push(
                StaticDefinition::new(StaticMode::MustBeBlocked { by: Some(filter) })
                    .affected(affected.clone())
                    .condition(gate.clone())
                    .description(residual_text.clone()),
            );
            continue;
        }

        // CR 509.1c: surface the still-unmodeled conjunct as an `Effect::Unimplemented`
        // residual carried in a `GrantAbility` modification so coverage flags it and
        // the swallow check defers (see fn-level note). The stable category key
        // groups the gap in coverage; the raw conjunct text is the diagnostic.
        defs.push(attached_grant_unmodeled_conjunct_residual(
            affected.clone(),
            &residual_text,
        ));
    }

    defs
}

fn parse_grant_conjunct_verb(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag("gets "),
            tag("get "),
            tag("has "),
            tag("have "),
            tag("gains "),
            tag("gain "),
        )),
    )
    .parse(input)
}

/// Parse the attached-subject qualifier of an inverted grant
/// ("enchanted/equipped creature is `<characteristic>`") into the `affected`
/// `TargetFilter` for the enchanted/equipped permanent.
///
/// Delegates to the canonical attached-subject predicate machinery
/// (`oracle_nom::condition::parse_attached_subject_is_filter`) so the full
/// characteristic class is covered — color (`HasColor`), type/subtype, and the
/// `legendary`/`basic` supertypes — not just `legendary`. Previously only
/// `"creature is legendary"` was recognized; every other characteristic fell
/// through to the generic inverted rewrite, which left `affected = SelfRef`
/// (the Aura/Equipment itself), so the grant never reached the host (#2818).
pub(crate) fn parse_attached_subject_qualifier(condition_lower: &str) -> Option<TargetFilter> {
    let (rest, filter) =
        crate::parser::oracle_nom::condition::parse_attached_subject_is_filter(condition_lower)
            .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(filter)
}

/// CR 113.6b: Whether `filter` scopes to cards you own/control in `zone` — the
/// zone a granted cast keyword functions from. Generalized from the
/// graveyard-only predicate so the same shape validates hand grants (foretell,
/// miracle) against `Zone::Hand`.
pub(crate) fn target_filter_is_your_zone(filter: &TargetFilter, zone: Zone) -> bool {
    match filter {
        TargetFilter::Typed(tf) => {
            tf.controller == Some(ControllerRef::You)
                && tf
                    .properties
                    .iter()
                    .any(|prop| matches!(prop, FilterProp::InZone { zone: z } if *z == zone))
        }
        TargetFilter::Or { filters } => filters.iter().all(|f| target_filter_is_your_zone(f, zone)),
        _ => false,
    }
}

/// Thin wrapper preserving the graveyard-specific call sites (no churn) —
/// delegates to the generalized `target_filter_is_your_zone`.
pub(crate) fn target_filter_is_your_graveyard(filter: &TargetFilter) -> bool {
    target_filter_is_your_zone(filter, Zone::Graveyard)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantedCastKeywordKind {
    Flashback,
    Escape,
    Mayhem,
    Scavenge,
    Encore,
    /// CR 702.143a: Foretell functions from hand (Dream Devourer grant).
    Foretell,
    /// CR 702.94a: Miracle functions from hand (Aminatou, Veil Piercer grant).
    Miracle,
    /// CR 702.128a: Naktamun ("Each creature card in your graveyard has
    /// embalm. Its embalm cost is equal to its mana cost.") — the runtime
    /// resolver (`resolve_self_cost_graveyard_activated_keyword`) already
    /// concretizes `Keyword::Embalm(EmbalmCost::Mana(SelfManaCost))`; this was
    /// a pure parser-recognition gap.
    Embalm,
}

impl GrantedCastKeywordKind {
    pub(crate) fn matches_keyword(self, keyword: &Keyword) -> bool {
        match self {
            GrantedCastKeywordKind::Flashback => {
                keyword.kind() == crate::types::keywords::KeywordKind::Flashback
            }
            GrantedCastKeywordKind::Escape => {
                keyword.kind() == crate::types::keywords::KeywordKind::Escape
            }
            // CR 702.187b: Green Goblin grants Mayhem to graveyard cards.
            GrantedCastKeywordKind::Mayhem => {
                keyword.kind() == crate::types::keywords::KeywordKind::Mayhem
            }
            // CR 702.97 (Scavenge) / CR 702.141 (Encore) / CR 702.128 (Embalm):
            // activated graveyard keywords share `KeywordKind::Unknown`, so
            // match the variant directly.
            GrantedCastKeywordKind::Scavenge => matches!(keyword, Keyword::Scavenge(_)),
            GrantedCastKeywordKind::Encore => matches!(keyword, Keyword::Encore(_)),
            GrantedCastKeywordKind::Embalm => matches!(keyword, Keyword::Embalm(_)),
            // CR 702.143a / CR 702.94a: hand-zone cast keywords.
            GrantedCastKeywordKind::Foretell => {
                keyword.kind() == crate::types::keywords::KeywordKind::Foretell
            }
            GrantedCastKeywordKind::Miracle => {
                keyword.kind() == crate::types::keywords::KeywordKind::Miracle
            }
        }
    }

    /// CR 113.6b: The zone this granted cast keyword functions from. The gate in
    /// `keyword_grant.rs` uses it to decline zone mismatches (foretell-in-graveyard,
    /// flashback-in-hand).
    pub(crate) fn grant_zone(self) -> Zone {
        match self {
            GrantedCastKeywordKind::Flashback
            | GrantedCastKeywordKind::Escape
            | GrantedCastKeywordKind::Mayhem
            | GrantedCastKeywordKind::Scavenge
            | GrantedCastKeywordKind::Encore
            | GrantedCastKeywordKind::Embalm => Zone::Graveyard,
            GrantedCastKeywordKind::Foretell | GrantedCastKeywordKind::Miracle => Zone::Hand,
        }
    }
}

/// CR 113.6 + CR 113.6b: When a static ability's condition asserts the source
/// is in a non-battlefield zone (e.g., "as long as this card is in your
/// graveyard"), that zone is an opt-in functional zone for the static. This
/// mirrors `self_recursion_trigger_zone` for `TriggerDefinition.trigger_zones`.
///
/// Walks the `StaticCondition` tree and collects every `SourceInZone { zone }`
/// it can reach. For a single non-battlefield reference (Anger-class), the
/// resulting `active_zones` is `[Zone]` — `Battlefield` is the CR 113.6 default
/// and only needs to be listed when the condition is a disjunction that names
/// multiple zones (Eminence: "in the command zone or on the battlefield").
/// When ALL collected zones happen to be `Battlefield`, `active_zones` is left
/// empty so the standard battlefield-default applies.
pub(crate) fn populate_active_zones_from_condition(def: &mut StaticDefinition) {
    use crate::types::zones::Zone;
    let mut zones: Vec<Zone> = Vec::new();
    if let Some(cond) = def.condition.as_ref() {
        collect_source_in_zones(cond, &mut zones);
    }
    // Deduplicate while preserving order.
    zones.dedup();
    // If the only reference was Battlefield, fall back to the empty/default
    // representation (CR 113.6) — adding `[Battlefield]` explicitly is
    // semantically identical but would diverge from existing tests that
    // assume `active_zones.is_empty()` for pure-battlefield statics.
    if zones.len() == 1 && zones[0] == Zone::Battlefield {
        zones.clear();
    }
    // Don't clobber an explicitly-set active_zones: upstream callers may pin
    // non-battlefield zones directly on the StaticDefinition (e.g. hand-zone
    // statics) and the condition-derived inference should only fill in zones
    // when nothing has been specified.
    if !zones.is_empty() && def.active_zones.is_empty() {
        def.active_zones = zones;
    }
}

pub(crate) fn collect_source_in_zones(
    cond: &StaticCondition,
    out: &mut Vec<crate::types::zones::Zone>,
) {
    match cond {
        StaticCondition::SourceInZone { zone } if !out.contains(zone) => {
            out.push(*zone);
        }
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for c in conditions {
                collect_source_in_zones(c, out);
            }
        }
        StaticCondition::Not { condition } => collect_source_in_zones(condition, out),
        _ => {}
    }
}

/// CR 702.5 + CR 702.6 + CR 613.4c: Shared subject dispatch for attached-subject
/// grant lines ("enchanted creature ...", "equipped creature ...", etc.).
///
/// Returns the `EnchantedBy`/`EquippedBy` `TargetFilter` plus the remaining
/// predicate (the original-case slice after the subject prefix), or `None` when
/// the line has no recognized attached-subject prefix. Longest-prefix-first so
/// "enchanted permanent " is tried before "enchanted creature " cannot win
/// erroneously — each prefix is distinct, but ordering keeps intent explicit.
///
/// "enchanted land is a " is intentionally NOT handled here; that type-changing
/// branch has its own dedicated dispatch in `parse_static_line_inner`.
pub(crate) fn attached_subject_filter<'a>(tp: &TextPair<'a>) -> Option<(TargetFilter, &'a str)> {
    if let Some(rest) = nom_tag_tp(tp, "enchanted creature ") {
        return Some((
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
            rest.original,
        ));
    }
    if let Some(rest) = nom_tag_tp(tp, "enchanted permanent ") {
        return Some((
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy])),
            rest.original,
        ));
    }
    if let Some(rest) = nom_tag_tp(tp, "enchanted land ") {
        return Some((
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::EnchantedBy])),
            rest.original,
        ));
    }
    if let Some(rest) = nom_tag_tp(tp, "equipped creature ") {
        return Some((
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
            rest.original,
        ));
    }
    // An Equipment that can attach to a non-creature permanent (e.g. Luxior,
    // Giada's Gift equips a planeswalker) addresses the "equipped permanent" —
    // the widest attached-Equipment subject. Mirrors the "enchanted permanent"
    // arm above with `EquippedBy`.
    if let Some(rest) = nom_tag_tp(tp, "equipped permanent ") {
        return Some((
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EquippedBy])),
            rest.original,
        ));
    }
    None
}

/// CR 605.1a: Match the mana-ability exemption suffix " unless they're mana
/// abilities" with either the ASCII (`'`) or typographic (U+2019) apostrophe.
///
/// MTGJSON oracle text carries the U+2019 form, and there is no global apostrophe
/// normalization in the parser pipeline — which is exactly why the `can't be
/// activated` predicate combinators already dual-branch (see
/// `parse_activation_compound_tail` and `evasion::try_split_and_cant_activate_abilities`).
/// The exemption suffix must accept both glyphs too, or a U+2019 printing silently
/// loses the carve-out and the runtime wrongly blocks mana abilities that CR 605.1a
/// requires to stay activatable. Single authority shared by every "can't be
/// activated" / "cost {N} more to activate" exemption site.
pub(crate) fn parse_mana_ability_exemption_suffix(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            alt((tag(" unless they're "), tag(" unless they\u{2019}re "))),
            tag("mana abilities"),
        ),
    )
    .parse(input)
}

/// CR 602.5: The bare `can't be activated` predicate, tolerant of both the ASCII
/// (`'`) and typographic (U+2019) apostrophe. Companion of
/// `parse_mana_ability_exemption_suffix` — the single authority every activation
/// prohibition predicate routes through, since there is no global apostrophe
/// normalization in the parser pipeline.
pub(crate) fn parse_cant_be_activated_predicate(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((tag("can't be activated"), tag("can\u{2019}t be activated"))),
    )
    .parse(input)
}

/// CR 602.5: The `activated abilities can't be activated` predicate phrase,
/// dual-apostrophe. Composes the fixed `"activated abilities "` lead with the
/// shared `parse_cant_be_activated_predicate`.
pub(crate) fn parse_activated_abilities_cant_be_activated(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            tag("activated abilities "),
            parse_cant_be_activated_predicate,
        ),
    )
    .parse(input)
}

/// CR 602.5: True if `text` contains the `activated abilities can't be activated`
/// predicate with either apostrophe glyph — the scan form used by the
/// self-reference and compound-Aura activation-prohibition gates.
pub(crate) fn contains_activated_abilities_cant_be_activated(text: &str) -> bool {
    nom_primitives::scan_contains(text, "activated abilities can't be activated")
        || nom_primitives::scan_contains(text, "activated abilities can\u{2019}t be activated")
}

/// CR 602.5: Parses the activation-prohibition tail of compound static text.
fn parse_activation_compound_tail(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            tag(", and "),
            opt(alt((tag("its "), tag("their ")))),
            parse_activated_abilities_cant_be_activated,
            opt(parse_mana_ability_exemption_suffix),
            opt(tag(".")),
        ),
    )
    .parse(input)
}

fn rule_static_predicate_to_activation_compound_mode(
    predicate: RuleStaticPredicate,
) -> Option<StaticMode> {
    match predicate {
        RuleStaticPredicate::CantAttack => Some(StaticMode::CantAttack),
        RuleStaticPredicate::CantBlock => Some(StaticMode::CantBlock),
        RuleStaticPredicate::CantAttackOrBlock => Some(StaticMode::CantAttackOrBlock),
        RuleStaticPredicate::CantCrew => Some(StaticMode::CantCrew),
        RuleStaticPredicate::CantUntap
        | RuleStaticPredicate::CantBeActivated
        | RuleStaticPredicate::CantBeSacrificed
        | RuleStaticPredicate::MustAttack
        | RuleStaticPredicate::MustBlock
        | RuleStaticPredicate::MustBeBlocked
        | RuleStaticPredicate::Goaded
        | RuleStaticPredicate::BlockOnlyCreaturesWithFlying
        | RuleStaticPredicate::Shroud
        | RuleStaticPredicate::Hexproof
        | RuleStaticPredicate::MayLookAtTopOfLibrary
        | RuleStaticPredicate::LoseAllAbilities
        | RuleStaticPredicate::NoMaximumHandSize
        | RuleStaticPredicate::MayPlayAdditionalLand => None,
    }
}

fn parse_activation_compound_restriction_modes(predicate_lower: &str) -> Option<Vec<StaticMode>> {
    let (rest, restriction_text) = terminated(take_until(", and "), parse_activation_compound_tail)
        .parse(predicate_lower)
        .ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    let restriction_text = restriction_text.trim();
    if let Ok((_, (predicate, None))) =
        all_consuming(parse_combat_rule_static_predicate_with_defended_nom).parse(restriction_text)
    {
        return rule_static_predicate_to_activation_compound_mode(predicate).map(|mode| vec![mode]);
    }

    parse_restriction_modes(restriction_text)
}

/// Like `parse_static_line`, but returns all `StaticDefinition`s produced by a line.
///
/// Most lines produce zero or one static. Compound forms like
/// "All creatures attack or block each combat if able" produce two
/// (one `MustAttack`, one `MustBlock`). Callers that push into a `Vec`
/// should prefer this over `parse_static_line` to avoid silently dropping modes.
pub fn parse_static_line_multi(text: &str) -> Vec<StaticDefinition> {
    parse_static_line_multi_ir(text)
        .into_iter()
        .map(|ir| lower_static_ir(&ir))
        .collect()
}

/// IR production: like `parse_static_line_ir` but returns all `StaticIr`s
/// produced by a compound line.
pub(crate) fn parse_static_line_multi_ir(text: &str) -> Vec<StaticIr> {
    let defs = parse_static_line_multi_inner(text);
    defs.into_iter()
        .map(|definition| StaticIr {
            definition,
            source_text: text.to_string(),
            body_ir: None,
        })
        .collect()
}

/// CR 702.85c + CR 105.2: Parse a spell-cast keyword grant whose granted
/// keyword(s) are QUOTED — "<subject> spells you cast [from <zone>] [with mana
/// value N or greater] have \"<K0>[, <K1>...]\"" — into one `CastWithKeyword`
/// static per listed keyword. Repeats are preserved (Zhulodok, Void Gorger's
/// "Cascade, cascade" yields two `Cascade` grants; the runtime fires one Cascade
/// trigger per granted keyword instance, CR 702.85a). The subject filter is
/// parsed by the single authority [`super::keyword_grant::parse_spells_have_keyword`],
/// re-invoked per keyword with a reconstructed unquoted grant so every subject
/// qualifier (type, from-zone, mana-value) stays consistent with the
/// single-keyword path. A leading color-quality prefix — which that handler
/// cannot see once "spells" is consumed — is peeled here and folded into each
/// grant's affected filter, keeping a "colorless spells" grant colorless-scoped.
///
/// Declines unless the grant is quoted AND every listed token is a keyword the
/// single handler accepts, so quoted non-keyword grants (a granted triggered
/// ability) and ordinary unquoted single-keyword grants fall through to their
/// own handlers.
fn parse_spells_have_quoted_keyword_list(text: &str) -> Option<Vec<StaticDefinition>> {
    let lower = text.to_lowercase();

    // Split "<subject>" from the quoted keyword list at the grant verb + opening
    // quote. Requiring the opening quote scopes this handler to the quoted-grant
    // class and leaves unquoted single-keyword grants to `parse_spells_have_keyword`.
    // `scan_preceded` yields the post-match remainder (the keyword list), unlike
    // `scan_split_at_phrase` which returns the slice still starting at the match.
    // It scans at word boundaries and trims leading whitespace, so the grant-verb
    // tags carry no leading space; `subject` is trimmed to drop the trailing one.
    let (subject, _grant_verb, after_quote) = nom_primitives::scan_preceded(&lower, |i| {
        alt((
            tag::<_, _, OracleError<'_>>("have \""),
            tag("has \""),
            tag("gain \""),
            tag("gains \""),
        ))
        .parse(i)
    })?;
    let subject = subject.trim();

    // The keyword list runs up to the closing quote; consume the quote with a
    // combinator, after which only an optional trailing period may follow.
    let (after_close, inner) = take_until::<_, _, OracleError<'_>>("\"")
        .parse(after_quote)
        .ok()?;
    let (residue, _) = tag::<_, _, OracleError<'_>>("\"").parse(after_close).ok()?;
    if !residue.trim().trim_end_matches('.').trim().is_empty() {
        return None;
    }

    // Split the quoted list into keyword names, validating each is a keyword
    // (`parse_keyword_name`) so a quoted *ability* grant declines here.
    let inner = inner.trim().trim_end_matches('.').trim();
    let (list_rest, keyword_names) = separated_list1(
        alt((
            tag::<_, _, OracleError<'_>>(", and "),
            tag(", "),
            tag(" and "),
        )),
        nom_primitives::parse_keyword_name,
    )
    .parse(inner)
    .ok()?;
    if !list_rest.trim().is_empty() || keyword_names.is_empty() {
        return None;
    }

    // Peel an optional leading color-quality qualifier (CR 105.2). The delegated
    // subject parser only sees the text before "spells you cast", so a bare
    // "colorless"/"monocolored"/"multicolored" prefix would otherwise be dropped.
    let (subject_no_color, color_prop) = peel_color_quality_prefix(subject);

    // Delegate the subject-filter parse per keyword by re-forming an unquoted
    // single-keyword grant. Declines the whole line if any token isn't accepted.
    let mut defs = Vec::with_capacity(keyword_names.len());
    for name in keyword_names {
        let reconstructed = format!("{subject_no_color} have {name}");
        let tp = TextPair::new(&reconstructed, &reconstructed);
        let mut def = super::keyword_grant::parse_spells_have_keyword(&tp, &reconstructed)?;
        if let Some(prop) = color_prop.clone() {
            if let Some(affected) = def.affected.take() {
                def = def.affected(add_property(affected, prop));
            }
        }
        // Preserve the full printed line as the static's description.
        def = def.description(text.to_string());
        defs.push(def);
    }

    // CR 113.2c: A quoted list may REPEAT a keyword ("Cascade, cascade" —
    // CR 702.85c), but only for keywords whose duplicate cast-time instances the
    // runtime actually preserves and consumes (`Keyword::cast_merge_preserves_
    // instances` — the same authority `casting.rs::requires_per_instance_keyword`
    // gates the merge on). A duplicate of any other keyword that reaches here (e.g.
    // "Exalted, exalted" — Exalted is in KEYWORDS and functions separately by rule
    // but its cast-grant count is unconsumed) would emit two grants that
    // `merge_spell_keyword` coalesces by kind, silently under-counting. Decline the
    // whole line rather than over-claim a grammar the runtime cannot realize.
    let mut seen: Vec<&Keyword> = Vec::new();
    for def in &defs {
        let StaticMode::CastWithKeyword { keyword } = &def.mode else {
            continue;
        };
        if seen.contains(&keyword) && !keyword.cast_merge_preserves_instances() {
            return None;
        }
        seen.push(keyword);
    }

    Some(defs)
}

/// Peel a leading color-quality qualifier ("colorless"/"monocolored"/
/// "multicolored") from a lowercase subject, returning the remainder and the
/// matching `FilterProp::ColorCount` (CR 105.2). Mirrors the color-quality
/// prefixes `oracle_target` recognizes before a type word.
fn peel_color_quality_prefix(subject: &str) -> (&str, Option<FilterProp>) {
    alt((
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 0,
            },
            tag::<_, _, OracleError<'_>>("colorless "),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::EQ,
                count: 1,
            },
            tag("monocolored "),
        ),
        value(
            FilterProp::ColorCount {
                comparator: Comparator::GE,
                count: 2,
            },
            tag("multicolored "),
        ),
    ))
    .parse(subject)
    .map(|(rest, prop)| (rest, Some(prop)))
    .unwrap_or((subject, None))
}

/// CR 611.3 + CR 613.1: Split a static line into its sentence segments, then
/// parse each as an independent continuous static. Returns `Some(defs)` only
/// when the line splits into 2+ segments and EVERY segment yields at least one
/// `StaticDefinition` — i.e. the line is genuinely a sequence of sibling
/// statics (dual-subject anthems and their relatives). When any segment is
/// non-static prose (or there is only one sentence) this returns `None`, so the
/// single-sentence pipeline keeps ownership of the line.
///
/// Each segment is re-entered through `parse_static_line_multi_inner` so that a
/// sentence which itself decomposes (e.g. "<grant> and can't block") still
/// emits all of its own statics. Recursion terminates because a single-sentence
/// segment produces only one `split_static_sentences` segment, which fails the
/// 2+ guard.
fn parse_multi_sentence_statics(text: &str) -> Option<Vec<StaticDefinition>> {
    let segments = split_static_sentences(text);
    if segments.len() < 2 {
        return None;
    }
    // CR 611.3a: A sentence that opens with a back-referential connector
    // ("Otherwise", "Then", "Instead") is a continuation whose meaning depends
    // on the prior clause's condition (Hunter's Blowgun's ". Otherwise, it has
    // reach." gates on `Not(<head condition>)`; the same holds for "as long
    // as"-gated alternatives). Splitting these into independent statics would
    // drop the complement condition, so defer the whole line to the dedicated
    // attached-subject / otherwise handlers downstream.
    if segments
        .iter()
        .skip(1)
        .any(|segment| segment_is_back_referential_continuation(segment))
    {
        return None;
    }
    let mut defs = Vec::new();
    let mut attached_scope: Option<TargetFilter> = None;
    for segment in &segments {
        let mut segment_defs = parse_static_line_multi_inner(segment);
        // CR 608.2c: In an Aura/Equipment, a continuation sentence whose subject is
        // the pronoun "It" refers to the enchanted/equipped creature, not the
        // Aura/Equipment object itself. Its static parses with `SelfRef` (the
        // pronoun resolves to self at the line level, with no attachment context);
        // rebind it to the attached scope the first sentence established (Spider-Man
        // No More: "Enchanted creature is a Citizen ... It has defender and loses all
        // other abilities." — the second sentence applies to the enchanted creature).
        if let Some(scope) = &attached_scope {
            if segment_subject_is_pronoun_it(segment) {
                for def in &mut segment_defs {
                    if def.affected.as_ref() == Some(&TargetFilter::SelfRef) {
                        def.affected = Some(scope.clone());
                    }
                }
            }
        } else {
            attached_scope = segment_defs
                .iter()
                .find_map(|def| {
                    def.affected
                        .as_ref()
                        .filter(|f| affected_is_attached_scope(f))
                })
                .cloned();
        }
        if segment_defs.is_empty() {
            // CR 602.5b + CR 602.5c: An "activate ... only once each turn" rider
            // carries no standalone static — it folds a once-per-turn use-restriction
            // cap into the immediately-preceding `GrantAllActivatedAbilitiesOf`
            // (Locus of Enlightenment, and any future "<grant abilities>. activate
            // those only once each turn." card). This is the shared grant-rider
            // primitive, composed with the standard grant parse — not a card hook.
            if fold_grant_cap_rider(segment, &mut defs) {
                continue;
            }
            // A non-static sentence (or one the static pipeline can't classify)
            // means this isn't a pure sibling-static line — defer the whole
            // line to the single-sentence fallback rather than emitting a
            // partial result that silently drops the unparsed sentence.
            return None;
        }
        defs.extend(segment_defs);
    }
    Some(defs)
}

/// CR 608.2c: True iff the sentence's subject is the bare pronoun "It" — an
/// Aura/Equipment continuation referring to the enchanted/equipped creature,
/// distinct from a self-name (`~`) or a typed subject.
fn segment_subject_is_pronoun_it(segment: &str) -> bool {
    // `trim_start` normalizes leading whitespace on the pre-split sentence chunk
    // and `to_lowercase` builds the TextPair lower half — both structural, not
    // dispatch. The "it " subject test itself runs through nom's `tag()` via the
    // `nom_tag_tp` bridge so the pronoun match stays on the combinator path.
    let trimmed = segment.trim_start();
    let lower = trimmed.to_lowercase();
    nom_tag_tp(&TextPair::new(trimmed, &lower), "it ").is_some()
}

/// True iff a filter is scoped to an attached object — the enchanted (Aura,
/// `EnchantedBy`) or equipped (Equipment, `EquippedBy`) creature.
fn affected_is_attached_scope(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.properties.iter().any(|p| matches!(p, FilterProp::EnchantedBy | FilterProp::EquippedBy))
    )
}

/// CR 611.3a: Recognize a sentence whose leading connector binds it to the
/// preceding clause's condition rather than standing on its own. Such a sentence
/// must not be split off as an independent static.
fn segment_is_back_referential_continuation(segment: &str) -> bool {
    let lower = segment.to_lowercase();
    let result: OracleResult<'_, &str> =
        alt((tag("otherwise"), tag("instead"), tag("then "))).parse(lower.as_str());
    result.is_ok()
}

/// Split a static line on sentence boundaries (`.` followed by whitespace or
/// end-of-input), tracking `{…}` mana-symbol and quote nesting so a period
/// inside a quoted granted ability or a mana symbol never ends a sentence. Each
/// returned segment keeps its terminating period and is trimmed; empty segments
/// are dropped.
fn split_static_sentences(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut brace_depth = 0usize;
    let mut in_double_quote = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        current.push(ch);
        match ch {
            '{' if !in_double_quote => brace_depth += 1,
            '}' if !in_double_quote => brace_depth = brace_depth.saturating_sub(1),
            '"' => in_double_quote = !in_double_quote,
            // A sentence ends at a period that is followed by whitespace or
            // end-of-input. A period directly followed by a non-space (e.g. an
            // ellipsis or a decimal that MTG static text never uses) is kept
            // inside the current segment.
            '.' if brace_depth == 0
                && !in_double_quote
                && chars.peek().is_none_or(|next| next.is_whitespace()) =>
            {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    segments.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => {}
        }
    }
    let trailing = current.trim();
    if !trailing.is_empty() {
        segments.push(trailing.to_string());
    }
    segments
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TieredEntersWithAdditionalCountersPattern {
    pub counter_type: crate::types::counter::CounterType,
    pub threshold: u32,
    pub first_count: u32,
    pub otherwise_count: u32,
}

fn parse_enter_with_an_additional_counter_prefix(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>("enter with an additional "),
            tag("enters with an additional "),
        )),
    )
    .parse(input)
}

fn parse_counter_carrier_pronoun(input: &str) -> OracleResult<'_, ()> {
    value((), alt((tag("it"), tag("them")))).parse(input)
}

fn parse_counter_on_phrase(input: &str) -> OracleResult<'_, ()> {
    value((), alt((tag(" counter on "), tag(" counters on ")))).parse(input)
}

fn parse_tiered_mana_value_clause(input: &str) -> OracleResult<'_, ()> {
    let (input, _) = tag("if ").parse(input)?;
    let (input, _) = alt((tag("its"), tag("their"))).parse(input)?;
    let (input, _) = tag(" mana value is ").parse(input)?;
    Ok((input, ()))
}

fn parse_tiered_enters_with_additional_counters_predicate(
    input: &str,
) -> OracleResult<'_, TieredEntersWithAdditionalCountersPattern> {
    let (input, _) = parse_enter_with_an_additional_counter_prefix(input)?;
    let (input, counter_type) = nom_primitives::parse_strict_counter_type(input)?;
    let (input, _) = parse_counter_on_phrase(input)?;
    let (input, _) = parse_counter_carrier_pronoun(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, _) = parse_tiered_mana_value_clause(input)?;
    let (input, threshold) = nom_primitives::parse_number(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, _) = tag("or less.").parse(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, _) = tag("otherwise,").parse(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, _) = alt((tag("it enters with "), tag("they enter with "))).parse(input)?;
    let (input, otherwise_count) = nom_primitives::parse_number(input)?;
    let (input, _) = space1.parse(input)?;
    let (input, _) = tag("additional ").parse(input)?;
    let (input, otherwise_counter_type) = nom_primitives::parse_strict_counter_type(input)?;
    let (input, _) = parse_counter_on_phrase(input)?;
    let (input, _) = parse_counter_carrier_pronoun(input)?;
    let (input, _) = opt(tag(".")).parse(input)?;

    if otherwise_counter_type != counter_type {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }

    Ok((
        input,
        TieredEntersWithAdditionalCountersPattern {
            counter_type,
            threshold,
            first_count: 1,
            otherwise_count,
        },
    ))
}

fn parse_tiered_enters_with_additional_counters_parts(
    tp: &TextPair<'_>,
) -> Option<(TargetFilter, TieredEntersWithAdditionalCountersPattern)> {
    let (subject_lower, predicate_lower) = nom_primitives::scan_split_at_phrase(tp.lower, |i| {
        parse_enter_with_an_additional_counter_prefix(i)
    })?;
    let (_, pattern) = all_consuming(terminated(
        parse_tiered_enters_with_additional_counters_predicate,
        space0,
    ))
    .parse(predicate_lower)
    .ok()?;

    let subject_original = tp.original[..subject_lower.len()].trim();
    let affected = parse_continuous_subject_filter(subject_original)?;
    if !filter_is_controller_you(&affected) {
        return None;
    }

    Some((affected, pattern))
}

fn cmc_filter_prop(comparator: Comparator, threshold: u32) -> Option<FilterProp> {
    Some(FilterProp::Cmc {
        comparator,
        value: QuantityExpr::Fixed {
            value: i32::try_from(threshold).ok()?,
        },
    })
}

fn parse_tiered_enters_with_additional_counters_static(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    let (base, pattern) = parse_tiered_enters_with_additional_counters_parts(tp)?;
    let le_filter = add_property(
        base.clone(),
        cmc_filter_prop(Comparator::LE, pattern.threshold)?,
    )
    .normalized();
    let gt_filter =
        add_property(base, cmc_filter_prop(Comparator::GT, pattern.threshold)?).normalized();

    Some(vec![
        StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
            counter_type: pattern.counter_type.clone(),
            count: pattern.first_count,
        })
        .affected(le_filter)
        .description(text.to_string()),
        StaticDefinition::new(StaticMode::EntersWithAdditionalCounters {
            counter_type: pattern.counter_type,
            count: pattern.otherwise_count,
        })
        .affected(gt_filter)
        .description(text.to_string()),
    ])
}

pub(crate) fn is_tiered_enters_with_additional_counters_static(lower: &str) -> bool {
    let tp = TextPair::new(lower, lower);
    parse_tiered_enters_with_additional_counters_parts(&tp).is_some()
}

pub(crate) fn parse_tiered_enters_with_additional_counters_pattern(
    lower: &str,
) -> Option<TieredEntersWithAdditionalCountersPattern> {
    let tp = TextPair::new(lower, lower);
    parse_tiered_enters_with_additional_counters_parts(&tp).map(|(_, pattern)| pattern)
}

/// CR 207.2c: An ability word is italicized flavor text with no rules meaning
/// (e.g. `Chroma`, `Metalcraft`, `Fateful hour`, and the set-specific `Protector`
/// / `Proclamator Hailer`). The subject-anchored static parsers match their
/// subject at the *start* of the line, so a leading ability-word label like
/// `"Chroma — Each creature you control gets ..."` prevents them from firing and
/// the whole static silently drops. When the ordinary dispatch classifies
/// nothing, strip a *recognized* ability-word label — whitelist-gated through the
/// shared `is_known_ability_word` authority, exactly as the token-grant path in
/// `keyword_grant.rs` does — and re-enter the dispatch once on the body.
///
/// This is a strict fallback: any line the dispatch already parses is returned
/// untouched, so no existing coverage can regress. The stripped body carries no
/// further label, so `strip_ability_word_with_name` yields `None` on the retry
/// and the recursion terminates after a single hop.
pub(crate) fn parse_static_line_multi_inner(text: &str) -> Vec<StaticDefinition> {
    let defs = parse_static_line_multi_dispatch(text);
    if !defs.is_empty() {
        return defs;
    }
    if let Some((ability_word, body)) = super::oracle_modal::strip_ability_word_with_name(text) {
        if super::oracle_modal::is_known_ability_word(&ability_word) {
            return parse_static_line_multi_dispatch(&body);
        }
    }
    defs
}

/// CR 611.3a + CR 702 (#5257 Rayami, First of the Fallen): "As long as an exiled
/// <type> card [with a <counter> counter on it] has <K0>, ~ has <K0>. The same is
/// true for <K1>, …, and <Kn>." Each listed keyword is an INDEPENDENT conditional
/// grant — the source has keyword K as long as an exiled matching card that HAS K
/// is present — so this emits one Continuous SelfRef static per keyword, gated on
/// `IsPresent { filter + WithKeyword(K) }`. The shared runtime already evaluates
/// this: `IsPresent` scans every object and `WithKeyword` reads the exiled card's
/// keywords. Modeling it as one static with a shared condition (the prior fallback
/// left the condition `Unrecognized`) made every keyword apply unconditionally.
fn parse_keyword_grant_from_exiled_object_static(text: &str) -> Option<Vec<StaticDefinition>> {
    // "As long as a[n] exiled " → the object phrase (original case for parse_type_phrase).
    let lower = text.to_lowercase();
    let (_, obj) = nom_on_lower(text, &lower, |i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("as long as ").parse(i)?;
        let (i, _) = alt((tag("an "), tag("a "))).parse(i)?;
        let (i, _) = tag("exiled ").parse(i)?;
        Ok((i, ()))
    })?;

    // The object type phrase; the remainder begins at " has <keyword>".
    let (base_filter, remainder) = parse_type_phrase(obj);
    let TargetFilter::Typed(mut typed) = base_filter else {
        return None;
    };
    // CR 400.1: "exiled" scopes the presence check to the exile zone.
    typed
        .properties
        .push(FilterProp::InZone { zone: Zone::Exile });
    let base = typed;

    // remainder (lowercased for keyword matching): "has <K0>, ~ has <K0>. The same
    // is true for <list>." The condition keyword and the granted keyword must match.
    let rem = remainder.trim_start().to_lowercase();
    let (i, _) = tag::<_, _, OracleError<'_>>("has ")
        .parse(rem.as_str())
        .ok()?;
    let (i, k0_name) = crate::parser::oracle_nom::primitives::parse_keyword_name(i).ok()?;
    let (i, _) = tag::<_, _, OracleError<'_>>(", ").parse(i).ok()?;
    let (i, _) = alt((tag::<_, _, OracleError<'_>>("~ has "), tag("it has ")))
        .parse(i)
        .ok()?;
    let (tail, k0b_name) = crate::parser::oracle_nom::primitives::parse_keyword_name(i).ok()?;
    if k0_name != k0b_name {
        return None;
    }
    let k0: Keyword = k0_name.parse().ok()?;

    // tail: ". the same is true for <list>." (or "." / "" for a single keyword).
    let tail = tail.trim_start_matches('.').trim_start();
    let mut keywords = vec![k0];
    if !tail.is_empty() {
        keywords.extend(
            super::super::oracle_effect::sequence::try_parse_same_is_true_continuation(tail)?,
        );
    }

    let defs = keywords
        .into_iter()
        .map(|ki| {
            let mut tf = base.clone();
            tf.properties
                .push(FilterProp::WithKeyword { value: ki.clone() });
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword { keyword: ki }])
                .condition(StaticCondition::IsPresent {
                    filter: Some(TargetFilter::Typed(tf)),
                })
                .description(text.to_string())
        })
        .collect();
    Some(defs)
}

/// CR 508.1c + CR 509.1b: predicate combinator for the defensive-flyer compound
/// "can't attack you or block creatures you control" (Storm, Windrider). Returns
/// the attack-defender scope (the `you`/`you or …` filter) and the block-target
/// filter (the creatures the subject may not block). Requires a defender scope —
/// a bare "can't attack" (no `you`) is a blanket restriction, not this template.
fn parse_cant_attack_you_or_block_predicate(
    input: &str,
) -> OracleResult<
    '_,
    (
        Option<crate::types::triggers::AttackTargetFilter>,
        TargetFilter,
    ),
> {
    let (input, _) = tag("can't attack").parse(input)?;
    let (input, defended) = parse_cant_attack_defended_scope_nom(input)?;
    // CR 508.1c: the defended scope ("you") is what keeps this from being a
    // blanket "can't attack" — bail out to the existing single-clause parsers
    // when the attack half has no defender.
    if defended.is_none() {
        return Err(super::oracle_nom::error::oracle_err(input));
    }
    let (input, _) = tag(" or block ").parse(input)?;
    // CR 509.1b: the block target ("creatures you control") is parsed by the
    // full type-phrase grammar so the class covers any "block <filter>" object.
    let (block_filter, rest) = parse_type_phrase(input);
    if rest.len() >= input.len() || matches!(block_filter, TargetFilter::Any) {
        return Err(super::oracle_nom::error::oracle_err(input));
    }
    Ok((rest, (defended, block_filter)))
}

/// CR 508.1c + CR 509.1b: "<subject> can't attack you or block creatures you
/// control" (Storm, Windrider). The single-clause "<subject> can't attack you"
/// already parses (`parse_subject_combat_rule_static`); the trailing "or block
/// …" made that parser reject, and the line then collapsed to a self-scoped
/// blanket `CantAttack` in the generic dispatch arm — so the source creature
/// itself could not attack. Emit the two correctly-scoped statics instead:
///
///  1. a defender-scoped `CantAttack` (the subject can't attack the source's
///     controller, per `attack_defended`, but may still attack anyone else); and
///  2. a `BlockRestriction` whose filter is the negation of the block target —
///     "can block only things that are NOT creatures you control" is exactly
///     "can't block creatures you control" (CR 509.1b enforcement in
///     `combat.rs` allows a block only when the attacker matches the filter).
///
/// Both are scoped to the subject filter, so this is a building block for the
/// defensive-flyer class, not a single card.
fn parse_subject_cant_attack_you_or_block_static(
    text: &str,
    lower: &str,
) -> Option<Vec<StaticDefinition>> {
    let (subject_lower, (defended, block_filter), rest) =
        nom_primitives::scan_preceded(lower, parse_cant_attack_you_or_block_predicate)?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    let subject = text[..subject_lower.len()].trim();
    let affected = parse_rule_static_subject_filter(subject)?;

    let attack = StaticDefinition::new(StaticMode::CantAttack)
        .affected(affected.clone())
        .attack_defended(defended)
        .description(text.to_string());
    let block = StaticDefinition::new(StaticMode::BlockRestriction {
        filter: TargetFilter::Not {
            filter: Box::new(block_filter),
        },
    })
    .affected(affected)
    .description(text.to_string());
    Some(vec![attack, block])
}

/// CR 613.1f (Layer 6) + CR 105.2: a per-recipient COLOR-qualified keyword grant —
/// "[subject] has <K0> if it's <C0>, <K1> if it's <C1>, …, and <Kn> if it's <Cn>."
/// (Scion of Draco: "Each creature you control has vigilance if it's white, hexproof
/// if it's blue, lifelink if it's black, first strike if it's red, and trample if
/// it's green.").
///
/// Each `if it's <color>` qualifies ONLY the creature its own keyword lands on, so the
/// color folds into that branch's AFFECTED FILTER as `FilterProp::HasColor` — the same
/// fold [`parse_continuous_subject_filter`] already applies to a color-named subject
/// ("White creatures you control") — and never becomes a `StaticCondition`, which
/// would gate every listed keyword on one shared board check. One `StaticDefinition`
/// per listed pair, mirroring the per-keyword expansion
/// [`parse_keyword_grant_from_exiled_object_static`] emits for the Rayami class.
///
/// Declines (returns `None`) unless the predicate is ENTIRELY such a list, so a plain
/// keyword grant ("Creatures you control have flying") is left to its own path.
fn parse_color_conditional_keyword_grants(text: &str) -> Option<Vec<StaticDefinition>> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);
    let (subject_tp, predicate_tp) = tp
        .split_around(" has ")
        .or_else(|| tp.split_around(" have "))?;
    let base = parse_continuous_subject_filter(subject_tp.original.trim())?;
    let predicate = predicate_tp.lower.trim().trim_end_matches('.').trim();
    let (rest, grants) = parse_color_conditional_keyword_list(predicate).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(
        grants
            .into_iter()
            .map(|(keyword, color)| {
                StaticDefinition::continuous()
                    .affected(add_property(base.clone(), FilterProp::HasColor { color }))
                    .modifications(vec![ContinuousModification::AddKeyword { keyword }])
                    .description(text.to_string())
            })
            .collect(),
    )
}

/// The `"<keyword> if it's <color>"` enumeration, one or more items joined by the
/// ordinary list connectors. Ordered longest-first so an Oxford `", and "` wins over a
/// bare `", "` and never leaves a dangling comma on an item.
fn parse_color_conditional_keyword_list(
    input: &str,
) -> OracleResult<'_, Vec<(Keyword, ManaColor)>> {
    separated_list1(
        color_conditional_keyword_separator,
        parse_color_conditional_keyword_grant,
    )
    .parse(input)
}

fn color_conditional_keyword_separator(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>(", and "),
            tag(", "),
            tag(" and "),
        )),
    )
    .parse(input)
}

/// One `<keyword> if it's <color>` pair. The keyword is read by the shared
/// `parse_keyword_name` atom (so every keyword the engine knows is accepted) and the
/// color by the shared `parse_color` atom — no bespoke vocabulary here.
fn parse_color_conditional_keyword_grant(input: &str) -> OracleResult<'_, (Keyword, ManaColor)> {
    let (i, name) = nom_primitives::parse_keyword_name(input)?;
    let keyword: Keyword = name
        .parse()
        .map_err(|_| nom::Err::Error(OracleError::new(input, nom::error::ErrorKind::Tag)))?;
    let (i, _) = alt((tag(" if it's "), tag(" if it\u{2019}s "))).parse(i)?;
    let (i, color) = nom_primitives::parse_color(i)?;
    Ok((i, (keyword, color)))
}

/// CR 104.2b + CR 104.3e + CR 810.8a: Compound player-scope game-outcome lock —
/// "<player-scope> can't lose|win the game and <player-scope> can't win|lose
/// the game" — lowered to one `CantLoseTheGame`/`CantWinTheGame` static per
/// conjunct, each carrying its own subject's affected filter (Platinum Angel:
/// "You can't lose the game and your opponents can't win the game."; Abyssal
/// Persecutor reverses the modes; Gideon of the Trials' emblem wraps the same
/// sentence in an "as long as" gate handled by the inverted-split arm in
/// `parse_static_line_multi_dispatch`). Requires ≥ 2 conjuncts and consumes
/// the whole line, so single-mode lines keep their existing
/// `parse_static_line_inner` arms and rider-bearing one-shot effect sentences
/// (Angel's Grace: "You can't lose the game this turn and …") fall through
/// untouched.
pub(crate) fn parse_cant_win_lose_compound_statics(
    text: &str,
    lower: &str,
) -> Option<Vec<StaticDefinition>> {
    // Subject axis: the player scope each conjunct names. Filter shapes match
    // the single-mode arms' `parse_player_scope_filter` output so both runtime
    // readers (`static_affects_player`, `static_filter_matches`) see the same
    // vocabulary. "your opponents " precedes "you " in source order for
    // clarity only — `tag("you ")` requires a trailing space, so it cannot
    // claim the "your…" prefix.
    fn parse_subject(i: &str) -> OracleResult<'_, TargetFilter> {
        alt((
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
                tag("your opponents "),
            ),
            value(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
                tag("you "),
            ),
            value(
                TargetFilter::Typed(TypedFilter::default()),
                alt((tag("each player "), tag("players "))),
            ),
        ))
        .parse(i)
    }
    // Predicate axis: which game outcome is locked (CR 104.2b win effects /
    // CR 104.3e loss effects; CR 810.8a is the "can't win"/"can't lose"
    // effect language).
    fn parse_predicate(i: &str) -> OracleResult<'_, StaticMode> {
        preceded(
            alt((tag("can't "), tag("cannot "))),
            alt((
                value(StaticMode::CantLoseTheGame, tag("lose the game")),
                value(StaticMode::CantWinTheGame, tag("win the game")),
            )),
        )
        .parse(i)
    }

    let (rest, first) = (parse_subject, parse_predicate).parse(lower).ok()?;
    let (rest, tail) = many1(preceded(tag(" and "), (parse_subject, parse_predicate)))
        .parse(rest)
        .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(".")).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(
        std::iter::once(first)
            .chain(tail)
            .map(|(affected, mode)| {
                StaticDefinition::new(mode)
                    .affected(affected)
                    .description(text.to_string())
            })
            .collect(),
    )
}

fn parse_static_line_multi_dispatch(text: &str) -> Vec<StaticDefinition> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();
    let tp = TextPair::new(&stripped, &lower);

    // CR 508.1c + CR 509.1b: "<subject> can't attack you or block creatures you
    // control" — two scoped statics (defender-scoped CantAttack + BlockRestriction).
    // Must precede generic combat-rule dispatch, which would collapse the whole
    // line to a self-scoped blanket CantAttack (Storm, Windrider).
    if let Some(defs) = parse_subject_cant_attack_you_or_block_static(&stripped, &lower) {
        return defs;
    }

    // CR 604.1 + CR 614.1c + CR 122.1 + CR 202.3: Tiered ETB-counter
    // replacement static. The otherwise sentence is a semantic companion to
    // the first sentence, so it must bind before generic multi-sentence
    // splitting can treat "Otherwise" as independent prose.
    if let Some(defs) = parse_tiered_enters_with_additional_counters_static(&tp, &stripped) {
        return defs;
    }

    // CR 702.11b + CR 702.21a: "Creatures your opponents control can be the
    // targets of spells and abilities as though they didn't have hexproof. Ward
    // abilities of those creatures don't trigger." (Nowhere to Run). The ward
    // sentence's "those creatures" anaphors the first sentence's subject, so the
    // pair is parsed as one unit before generic sentence splitting would treat
    // them as independent statics (which would strand the anaphor).
    if let Some(defs) = parse_ignore_hexproof_static(&tp, &stripped) {
        return defs;
    }

    // CR 611.3 + CR 613.1: A static ability whose Oracle text is several
    // independent sentences (each a self-contained continuous effect) defines
    // each sentence as its own continuous effect with its own affected set.
    // Dual-subject anthems are the canonical class — e.g. Flowering of the
    // White Tree ("Legendary creatures you control get +2/+1 and have ward {1}.
    // Nonlegendary creatures you control get +1/+1."), Intangible Virtue
    // siblings, Glorious Anthem variants, the *-Tribute supertype pairs. Without
    // this split the single-sentence pipeline parses the first sentence,
    // swallows the period, and drops every following sentence. Split into
    // sentence segments and parse each independently; only adopt the split when
    // there are 2+ segments and EVERY segment yields at least one static, which
    // restricts the path to genuine sibling-static lines and leaves trailing
    // non-static prose to the single-sentence fallback below.
    // CR 611.3a + CR 702 (#5257 Rayami): "As long as an exiled <type> card
    // [with a <counter> counter on it] has <K0>, ~ has <K0>. The same is true
    // for <K1>, …" — one independent conditional keyword grant per listed
    // keyword. Must precede generic multi-sentence splitting, which would strand
    // the shared condition on the first keyword only (the observed bug: the grant
    // applies unconditionally to every keyword).
    if let Some(defs) = parse_keyword_grant_from_exiled_object_static(&stripped) {
        return defs;
    }

    // CR 613.1f + CR 105.2 (Scion of Draco): "<subject> has <K0> if it's <C0>, …, and
    // <Kn> if it's <Cn>." — the COLOR-qualified sibling of the exiled-object grant
    // above: one independent grant per listed pair, each color folded into its own
    // branch's affected filter. Must precede the single-sentence pipeline, which reads
    // only the first keyword and drops the "if it's <color>" qualifier — the observed
    // bug (the whole static vanished, so the card did nothing).
    if let Some(defs) = parse_color_conditional_keyword_grants(&stripped) {
        return defs;
    }

    // CR 702.85a + CR 702.85c + CR 613.1: "<subject> spells you cast [...] have
    // "<K0>[, <K1>...]"" — a spell-cast keyword grant whose granted keyword(s)
    // are QUOTED and may repeat (Zhulodok, Void Gorger: "... have 'Cascade,
    // cascade.'"). The single unquoted keyword grant is owned by
    // `parse_spells_have_keyword`; this sibling expands the quoted list into one
    // `CastWithKeyword` static per listed keyword — repeats included, since
    // granted-keyword multiplicity is meaningful (the runtime fires one Cascade
    // trigger per granted instance). Mirrors the exiled-object / color-conditional
    // grant handlers above (one static per listed keyword).
    if let Some(defs) = parse_spells_have_quoted_keyword_list(&stripped) {
        return defs;
    }

    if let Some(defs) = parse_multi_sentence_statics(&stripped) {
        return defs;
    }

    // CR 116.2d: "ignore this effect" actions from static abilities are special
    // actions. Until the engine models that priority-time action, the static
    // parser must fail closed instead of exporting the lock while dropping the
    // opt-out sentence.
    if nom_primitives::scan_contains(&lower, "ignore this effect until end of turn") {
        return Vec::new();
    }

    // CR 508.1a + CR 611.3a + CR 613.1f: Inverted attached-subject grant gated on
    // the host creature's COMBAT STATE — "As long as equipped/enchanted creature
    // is attacking|blocking, it has/gets <X> [and <unmodeled conjunct>]" (Ace's
    // Baseball Bat). This must run on the multi-static path (and before the
    // single-return fallback) for two reasons: (1) the generic inverted rewrite
    // would gate the grant on `SourceIsAttacking` (the Equipment, never an
    // attacker) with `affected = SelfRef` — both wrong; (2) the compound effect
    // may carry an unmodeled conjunct (the "must be blocked by a Dalek if able"
    // lure) that must surface as a sibling `Effect::Unimplemented` residual
    // rather than being silently dropped. The single-return path can carry only
    // one def, so the residual would have nowhere to live there.
    if let Some(split) = try_split_inverted_as_long_as(&tp) {
        let defs = try_parse_inverted_attached_combat_grant(&split, &stripped);
        if !defs.is_empty() {
            return defs;
        }
        // CR 702.11 + CR 702.18 + CR 611.3a: Inverted player+object compound
        // keyword grants ("As long as <cond>, you and <objects> have hexproof")
        // must decompose into TWO defs (object Continuous + player Hexproof/
        // Shroud/PlayerProtection). The single-return inverted rewrite can only
        // keep one def, so rewrite to the canonical trailing-gate form and
        // re-enter through the multi compound-keyword splitter here.
        let canon_lower = split.canonical.to_lowercase();
        if let Some(mut defs) =
            parse_compound_subject_keyword_static(&split.canonical, &canon_lower)
        {
            let condition = parse_static_condition(&split.condition_text).unwrap_or(
                StaticCondition::Unrecognized {
                    text: split.condition_text.clone(),
                },
            );
            for def in &mut defs {
                if def.condition.is_none() {
                    def.condition = Some(condition.clone());
                }
                def.description = Some(stripped.to_string());
            }
            return defs;
        }
        // CR 104.2b + CR 104.3e + CR 611.3a: conditional game-outcome lock —
        // "As long as <condition>, you can't lose the game and your opponents
        // can't win the game" (Gideon of the Trials' emblem). Each conjunct
        // becomes its own condition-gated static. CR 611.3a: fail CLOSED — an
        // unrecognized condition falls through to the existing fallback (the
        // line keeps its honest `Condition_AsLongAs` swallow flag) rather
        // than emitting an unconditional outcome lock:
        // `StaticCondition::Unrecognized` evaluates as always-true in the
        // layer system, which for "you can't lose the game" would be
        // game-breaking. Mirrors the CantPlayLand trailing-gate precedent in
        // `dispatch.rs`.
        let effect_lower = split.effect_text.to_lowercase();
        if let Some(mut defs) =
            parse_cant_win_lose_compound_statics(&split.effect_text, &effect_lower)
        {
            if let Some(condition) = parse_static_condition(&split.condition_text) {
                for def in &mut defs {
                    def.condition = Some(condition.clone());
                    def.description = Some(stripped.to_string());
                }
                return defs;
            }
        }
    }

    // CR 601.2 + CR 602.5: City of Solitude class — "can cast spells and
    // activate abilities only during {your | their own} turn(s)". Emits both
    // halves of the prohibition independently. Must run first so the cast-only
    // branch (which matches "can cast spells only during") does not consume
    // the line before the activate-half is emitted.
    if let Some(defs) = parse_cast_and_activate_only_during(&tp, &stripped) {
        return defs;
    }

    if let Some(defs) = parse_cost_payment_prohibition_statics(&tp, &stripped) {
        return defs;
    }

    // CR 508.1c + CR 201.2a: "Except for <A> and <B>, <rule-static>" (Akron
    // Legionnaire) — a leading exempt-list clause. Must precede
    // parse_compound_subject_rule_static: that sibling's subject grammar has
    // no leading-clause syntax and would otherwise strict-fail on the
    // "except for" prefix.
    if let Some(defs) = parse_leading_except_for_rule_static(&stripped, &lower) {
        return defs;
    }

    if let Some(defs) = parse_compound_subject_rule_static(&stripped, &lower) {
        return defs;
    }

    // CR 702.11 + CR 702.16 + CR 702.18 + CR 611.3a: "You and <objects> have
    // <player-applicable keyword>" (Sigarda / Serra's Emissary / Gruul
    // Spellbreaker), or the Oxford-comma N-item form "You, <objects>, …, and
    // <objects> have <keyword>" (Shalai, Voice of Plenty). Must claim before
    // the single-return fallback, which otherwise emits one bogus Continuous
    // Or{empty-typed You, objects} that grants the keyword to every permanent
    // you control.
    if let Some(defs) = parse_compound_subject_keyword_static(&stripped, &lower) {
        return defs;
    }

    // CR 508.1c + CR 509.1b + CR 611.3a: "~ can't attack if <cond> and can't block
    // if <cond>" (The Fallen Apart) — each restriction carries its own trailing
    // gate; must split before the single-gate `can't block` dispatch arm.
    // Attached-subject forms scope `affected` to the enchanted/equipped host.
    if let Some(defs) = try_parse_dual_gated_cant_attack_and_cant_block(&tp, &stripped) {
        return defs;
    }

    // CR 508.1c + CR 509.1b + CR 611.3a: "<grant> and can't attack if <A> and
    // can't block if <B>" (Cagemail-class pump plus gated combat drawbacks).
    // The bare dual-gate splitter declines when the subject carries a leading
    // grant; peel the conjunct and append both gated combat statics.
    if let Some(defs) = try_split_grant_and_dual_gated_combat(&tp, &stripped) {
        return defs;
    }

    // Check compound must-attack/block first — may return multiple.
    if let Some(defs) = try_parse_scoped_must_attack_block(&lower, &stripped) {
        return defs;
    }

    // CR 701.3 + CR 702.5 + CR 702.6: Compound "can't be equipped or enchanted"
    // produces two static definitions (CantBeEquipped + CantBeEnchanted). Fortifications
    // are intentionally excluded by the Oracle wording, so CantBeAttached is NOT emitted.
    if nom_primitives::scan_contains(&lower, "can't be equipped or enchanted") {
        return vec![
            StaticDefinition::new(StaticMode::Other("CantBeEquipped".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
            StaticDefinition::new(StaticMode::Other("CantBeEnchanted".to_string()))
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
        ];
    }

    // CR 506.5 + CR 508.1c + CR 509.1b: "can't attack or block alone" (Mogg
    // Flunkies) imposes both CombatAlone(Attack,NeedsCompanion) and
    // CombatAlone(Block,NeedsCompanion).
    if let Some((_, AloneCombatRestriction::AttackOrBlock, rest)) =
        nom_primitives::scan_preceded(&lower, parse_alone_combat_restriction)
    {
        if rest.trim().is_empty() {
            return vec![
                StaticDefinition::new(StaticMode::CombatAlone {
                    action: CombatAloneAction::Attack,
                    requirement: CombatAloneRequirement::NeedsCompanion,
                })
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
                StaticDefinition::new(StaticMode::CombatAlone {
                    action: CombatAloneAction::Block,
                    requirement: CombatAloneRequirement::NeedsCompanion,
                })
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
            ];
        }
    }

    // CR 119.7 + CR 119.8: "[scope] life total can't change" — bidirectional
    // life-lock. Emits both CantGainLife and CantLoseLife with the same
    // player-scope filter (Platinum Emperion: "Your life total can't change.";
    // also covers "Players' life totals can't change", "Your opponents' life
    // totals can't change", etc.).
    if nom_primitives::scan_contains(&lower, "life total can't change")
        || nom_primitives::scan_contains(&lower, "life totals can't change")
        || nom_primitives::scan_contains(&lower, "life total cannot change")
        || nom_primitives::scan_contains(&lower, "life totals cannot change")
    {
        let affected = parse_life_total_scope_filter(&lower);
        return vec![
            StaticDefinition::new(StaticMode::CantGainLife)
                .affected(affected.clone())
                .description(stripped.to_string()),
            StaticDefinition::new(StaticMode::CantLoseLife)
                .affected(affected)
                .description(stripped.to_string()),
        ];
    }

    // CR 104.2b + CR 104.3e + CR 810.8a: "<player-scope> can't lose/win the
    // game and <player-scope> can't win/lose the game" — one game-outcome-lock
    // static per conjunct, each with its own player scope (Platinum Angel
    // wording class, both mode orders; emblem bodies such as Gideon of the
    // Trials' reach this via `try_parse_emblem_creation` →
    // `parse_static_line_multi`). Sibling of the life-lock compound above.
    if let Some(defs) = parse_cant_win_lose_compound_statics(&stripped, &lower) {
        return defs;
    }

    let tp = TextPair::new(&stripped, &lower);
    let attached_activation_compound_modes =
        attached_subject_filter(&tp).and_then(|(_, predicate)| {
            let predicate_lower = predicate.to_lowercase();
            parse_activation_compound_restriction_modes(&predicate_lower)
        });

    // CR 508.1c / CR 509.1b / CR 702.122c + CR 602.5: compound attack,
    // block, crew, and activation prohibitions produce parallel static definitions.
    if contains_activated_abilities_cant_be_activated(&lower)
        && (attached_activation_compound_modes.is_some()
            || nom_primitives::scan_contains(&lower, "can't attack")
            || nom_primitives::scan_contains(&lower, "can't block"))
    {
        // Faith's Fetters / Arrest-class Aura lines lead with "enchanted
        // permanent/creature …"; the combat lock and activation prohibition apply
        // to the host, not the Aura source.
        let affected = attached_subject_filter(&tp)
            .map(|(filter, _)| filter)
            .unwrap_or(TargetFilter::SelfRef);
        let source_filter = affected.clone();
        let mut defs = Vec::new();
        let combat_modes = attached_activation_compound_modes.unwrap_or_else(|| {
            vec![
                if nom_primitives::scan_contains(&lower, "can't attack or block") {
                    StaticMode::CantAttackOrBlock
                } else if nom_primitives::scan_contains(&lower, "can't attack") {
                    StaticMode::CantAttack
                } else {
                    StaticMode::CantBlock
                },
            ]
        });
        for combat_mode in combat_modes {
            defs.push(
                StaticDefinition::new(combat_mode)
                    .affected(affected.clone())
                    .description(stripped.to_string()),
            );
        }
        defs.push(
            StaticDefinition::new(StaticMode::CantBeActivated {
                who: ProhibitionScope::AllPlayers,
                source_filter,
                exemption: parse_cant_be_activated_exemption_in_text(&lower),
            })
            .affected(affected)
            .description(stripped.to_string()),
        );
        return defs;
    }

    // CR 702.3b + CR 611.3a + CR 613: Cross-mode conjunctions of the form
    // "<predicate_1> and can attack as though <pronoun> didn't have defender
    // [as long as <cond>]" combine a Continuous modification (keyword grant,
    // +N/+M, assigns-damage-from-toughness) with a `CanAttackWithDefender`
    // permission. A single `StaticDefinition` cannot carry both static modes,
    // so decompose: strip the conjunction phrase, re-parse the remainder, then
    // emit a companion `CanAttackWithDefender` inheriting `affected` + `condition`.
    // Corpus: Arcades, the Strategist; Colossus of Akros; Spire Serpent.
    if let Some(defs) = try_split_and_can_attack_despite_defender(&stripped) {
        return defs;
    }

    // CR 508.1d / CR 509.1c / CR 701.15b: Cross-mode conjunctions of the form
    // "<predicate_1> and attack/block each combat if able/is goaded" combine a
    // continuous static (usually a keyword grant) with a combat requirement.
    // A single `StaticDefinition` cannot carry both modes, so decompose them.
    if let Some(defs) = try_split_and_must_attack_block(&stripped) {
        return defs;
    }

    // CR 509.1b: "<predicate> and can block an additional creature [each combat]"
    // pairs a keyword/continuous grant with an extra-block grant under one
    // subject (Brave the Sands). Split so the extra-block clause is not dropped.
    if let Some(defs) = try_split_and_can_block_additional(&stripped) {
        return defs;
    }

    // CR 509.1b: "<predicate> and can't be blocked[ by/except by … | by more
    // than N creatures]" pairs a keyword/continuous grant with an evasion grant
    // under one subject (Madcap Skills). Split so the evasion clause is not
    // dropped.
    if let Some(defs) = try_split_and_cant_be_blocked(&stripped) {
        return defs;
    }

    // CR 509.1b: "<grant> and can't block" pairs a P/T (or keyword) grant with a
    // blocking restriction under one subject (Copper Carapace, Maniacal Rage,
    // Threshold downside creatures). Split so the CantBlock clause is not dropped.
    if let Some(defs) = try_split_and_cant_block(&stripped) {
        return defs;
    }

    // CR 508.1c / CR 509.1b: "<grant or restriction> and can't attack or block"
    // pairs a first clause with a full combat lockout under one subject (Immovable
    // Rod, Fog on the Barrow-Downs). Split so the CantAttackOrBlock clause is not
    // dropped. Registered before the bare-attack splitter so the combined phrase is
    // consumed first.
    if let Some(defs) = try_split_and_cant_attack_or_block(&stripped) {
        return defs;
    }

    // CR 508.1d: "<grant or restriction> and can't attack you [or planeswalkers
    // you control]" — the Vow cycle (Vow of Lightning / Duty / Flight / Torment
    // / Wildness). Registered before the bare-attack splitter so the more
    // specific scoped phrase is consumed first.
    if let Some(defs) = try_split_and_cant_attack_scoped(&stripped) {
        return defs;
    }

    // CR 508.1c: "<grant> and can't attack" pairs a P/T (or keyword) grant with an
    // attacking restriction under one subject (Cagemail). Split so the CantAttack
    // clause is not dropped. The terminal-phrase guard keeps the scoped
    // "can't attack alone / you / planeswalkers / its owner …" forms with their
    // own handlers.
    if let Some(defs) = try_split_and_cant_attack(&stripped) {
        return defs;
    }

    // CR 502.3: "<grant> and doesn't untap during its controller's untap step"
    // pairs a continuous grant with an untap restriction under one subject (Flood
    // the Engine). Split so the CantUntap clause is not dropped. (The "enters
    // tapped and doesn't untap" replacement+static compound is carved out earlier.)
    if let Some(defs) = try_split_and_doesnt_untap(&stripped) {
        return defs;
    }

    // CR 702.5 / CR 702.6: "<grant or restriction> and can't be enchanted [or
    // equipped] [by other Auras]" pairs a first clause with an attach prohibition
    // under one subject (Anti-Magic Aura, Consecrate Land). Split so the
    // CantBeEnchanted/CantBeEquipped clause is not dropped.
    if let Some(defs) = try_split_and_cant_be_attached(&stripped) {
        return defs;
    }

    // CR 702.18a / CR 702.11a: "<grant or restriction> and can't be the target of
    // …" pairs a first clause with a targeting restriction under one subject
    // (Spectral Shield). Split so the CantBeTargeted/Hexproof clause is not
    // dropped.
    if let Some(defs) = try_split_and_cant_be_targeted(&stripped) {
        return defs;
    }

    // CR 602.5: "<grant or restriction> and its activated abilities can't be
    // activated" pairs a first clause with an activation prohibition under one
    // subject (Viper's Kiss). Split so the CantBeActivated clause is not dropped.
    // (The "can't attack/block, and activated abilities …" compound — Arrest,
    // Faith's Fetters — is handled by its own earlier branch above.)
    if let Some(defs) = try_split_and_cant_activate_abilities(&stripped) {
        return defs;
    }

    // CR 701.21: "<grant or restriction> and can't be sacrificed" pairs a first
    // clause with a sacrifice prohibition under one subject (Assault Suit). Split
    // so the CantBeSacrificed clause is not dropped.
    if let Some(defs) = try_split_and_cant_be_sacrificed(&stripped) {
        return defs;
    }

    // CR 611.3a + CR 613.1f: "PRIMARY and FOREIGN_SUBJECT have/has/gains/gain
    // KEYWORD [as long as COND]" — compound static where the second conjunct has
    // a different subject (e.g., Angelic Field Marshal: "~ gets +2/+2 and
    // creatures you control have vigilance as long as you control your commander").
    // Must run before the single-return fallback that can only produce one def.
    if let Some(defs) = try_split_and_foreign_keyword_grant(&stripped) {
        return defs;
    }

    // CR 509.1b + CR 604.1 + CR 611.3a + CR 613.1f: Attached-subject grant lines
    // ("enchanted creature ...", "equipped creature ...") may decompose into more
    // than one StaticDefinition (e.g. CantBeBlocked + Continuous{AddKeyword}).
    // `parse_enchanted_equipped_predicate` is the single mechanism for all such
    // compound forms; simple lines flow back as a length-1 Vec. The single-return
    // `parse_static_line` path keeps only the first def, so the multi path must
    // dispatch here before the fallback.
    //
    // CR 205.1a + CR 613.1d: "enchanted creature is a [type] ..." type-change
    // lines (Darksteel Mutation) are owned by `parse_enchanted_is_type`, which
    // the single-return fallback dispatches BEFORE the attached-subject grant
    // branch. Defer those to the fallback so the type-line decomposition is not
    // pre-empted by the continuous-grant parser.
    if parse_enchanted_is_type(&tp, &stripped).is_none() {
        if let Some((filter, rest)) = attached_subject_filter(&tp) {
            let defs = parse_enchanted_equipped_predicate(rest, filter, &stripped);
            if !defs.is_empty() {
                return defs;
            }
        }
    }

    // Fall back to the single-return parser.
    let mut defs: Vec<StaticDefinition> = parse_static_line(text).into_iter().collect();
    append_cant_have_keyword_denials(text, &mut defs);
    defs
}

/// CR 613.1f / CR 702: "... can't have or gain [keyword]" (Theros Archetype cycle,
/// Arcane Lighthouse) both strips the keyword now — a `RemoveKeyword` continuous
/// modification on the base `Continuous` static — AND denies it going forward, so a
/// concurrent anthem can't grant it back. The forward denial is a Layer 6
/// `StaticMode::CantHaveKeyword` static (enforced by `apply_cant_have_keyword_denials`
/// in `layers.rs`). Emit it as a sibling of the continuous static, reusing that
/// static's `affected`/`condition` so it covers exactly the same objects.
fn append_cant_have_keyword_denials(text: &str, defs: &mut Vec<StaticDefinition>) {
    // Identify the SPECIFIC keyword the line denies, parsed from the clause
    // "... can't have or gain [keyword]" / "... can't have [keyword]". Keying the
    // emission off any `RemoveKeyword` alone would mis-target a line that removes
    // one keyword but denies a different one ("lose flying ... can't have or gain
    // trample"); the denied keyword must come from the can't-have clause itself.
    let Some(denied) = parse_cant_have_or_gain_keyword(&text.to_lowercase()) else {
        return;
    };
    let mut siblings: Vec<StaticDefinition> = Vec::new();
    for def in defs.iter() {
        if !matches!(def.mode, StaticMode::Continuous) {
            continue;
        }
        // Reuse the affected/condition scope of the continuous static that strips
        // the denied keyword now, so the forward denial covers identical objects.
        let strips_denied = def.modifications.iter().any(|m| {
            matches!(m, ContinuousModification::RemoveKeyword { keyword } if *keyword == denied)
        });
        if strips_denied {
            siblings.push(StaticDefinition {
                mode: StaticMode::CantHaveKeyword {
                    keyword: denied.clone(),
                },
                modifications: Vec::new(),
                ..def.clone()
            });
        }
    }
    defs.extend(siblings);
}

/// Extract the keyword denied by a "... can't have or gain [keyword]" /
/// "... can't have [keyword]" clause from the already-lowercased line, using the
/// canonical keyword combinator rather than coincidentally matching a removal.
fn parse_cant_have_or_gain_keyword(lower: &str) -> Option<Keyword> {
    let tail =
        if let Ok((_, (_, after))) = nom_primitives::split_once_on(lower, "can't have or gain ") {
            after
        } else if let Ok((_, (_, after))) = nom_primitives::split_once_on(lower, "can't have ") {
            after
        } else {
            return None;
        };
    crate::parser::oracle_keyword::parse_granted_keyword_fragment(tail.trim().trim_end_matches('.'))
}

pub(crate) fn push_or_filter_branch(filters: &mut Vec<TargetFilter>, filter: TargetFilter) {
    match filter {
        TargetFilter::Or { filters: inner } => filters.extend(inner),
        other => filters.push(other),
    }
}

pub(crate) fn filter_has_source_or_controller_anchor(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::SelfRef | TargetFilter::Controller => true,
        TargetFilter::Typed(typed) => matches!(
            typed.controller,
            Some(ControllerRef::You | ControllerRef::Opponent)
        ),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_has_source_or_controller_anchor)
        }
        _ => false,
    }
}

pub(crate) fn exactly_one_creature_you_control_filter(
    condition: &StaticCondition,
) -> Option<&TargetFilter> {
    match condition {
        StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 1 },
        } if is_creature_you_control_filter(filter) => Some(filter),
        _ => None,
    }
}

pub(crate) fn is_creature_you_control_filter(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter {
            type_filters,
            controller: Some(ControllerRef::You),
            ..
        }) => type_filters
            .iter()
            .any(|type_filter| type_filter == &TypeFilter::Creature),
        TargetFilter::And { filters } => filters.iter().any(is_creature_you_control_filter),
        TargetFilter::Or { filters } => filters.iter().all(is_creature_you_control_filter),
        _ => false,
    }
}

pub(crate) fn matches_soulbond_paired_condition(condition_text: &str) -> bool {
    all_consuming(parse_soulbond_paired_condition_nom)
        .parse(condition_text)
        .is_ok()
}

pub(crate) fn parse_soulbond_paired_condition_nom(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag("~ is paired with another creature"),
            tag("this creature is paired with another creature"),
            tag("it is paired with another creature"),
        )),
    )
    .parse(input)
}

/// Parse a condition clause (the text between "As long as" and the comma).
///
/// Returns a typed `StaticCondition` for known patterns, or `None` if the
/// condition text is not recognized. Callers may fall back to `Unrecognized`.
///
/// Try splitting a condition on " and " into compound `StaticCondition::And`.
/// Only succeeds when BOTH halves parse as valid conditions — prevents false splits
/// on noun phrases like "artifacts and creatures".
pub(crate) fn try_split_compound_and(text: &str) -> Option<StaticCondition> {
    let lower = text.to_lowercase();
    // Find " and " boundaries — try each occurrence in case the first is a noun conjunction.
    let mut search_from = 0;
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    while let Some(pos) = lower[search_from..].find(" and ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let abs_pos = search_from + pos;
        let left = &text[..abs_pos];
        let right = &text[abs_pos + 5..]; // " and " is 5 bytes
        if let (Some(lhs), Some(rhs)) =
            (parse_static_condition(left), parse_static_condition(right))
        {
            return Some(StaticCondition::And {
                conditions: vec![lhs, rhs],
            });
        }
        search_from = abs_pos + 5;
    }
    None
}

/// Supported patterns:
/// - "you have at least N life more than your starting life total" → LifeMoreThanStartingBy
/// - "your devotion to [colors] is less than N" → DevotionGE (with inverted threshold)
/// - "it's your turn" → DuringYourTurn
/// - "you control a/an [type]" → IsPresent with filter
pub(crate) fn parse_static_condition(text: &str) -> Option<StaticCondition> {
    let text = text.trim().trim_end_matches('.');
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    // Delegate to shared nom condition combinator (prefix already stripped by callers).
    // Callers like parse_conditional_static strip "As long as " before calling us,
    // so we use parse_inner_condition (no prefix required), not parse_condition.
    if let Ok((rest, condition)) = nom_condition::parse_inner_condition(&lower) {
        if rest.trim().is_empty() {
            return Some(condition);
        }
    }

    // CR 601.2 + CR 400.7: "<source> was cast this turn" gates on the source
    // having been cast (WasCast) AND having entered this turn
    // (SourceEnteredThisTurn) — a permanent that was cast and entered this turn
    // was necessarily cast this turn, while one put onto the battlefield (not
    // cast) or cast on an earlier turn fails one conjunct. Composed from the two
    // existing leaf primitives rather than a new `SourceWasCastThisTurn` variant
    // (compose-don't-proliferate). `parse_inner_condition` above recognizes the
    // bare "<source> was cast" (→ `WasCast`) but not the "this turn" tightening,
    // so the compound is handled here. Rock Jockey: "You can't play lands if this
    // creature was cast this turn."
    for self_ref in ["it ", "this creature ", "this permanent ", "~ "] {
        let Some(after_ref) = nom_tag_lower(tp.lower, tp.lower, self_ref) else {
            continue;
        };
        if nom_tag_lower(after_ref, after_ref, "was cast this turn")
            .is_some_and(|remainder| remainder.trim().is_empty())
        {
            return Some(StaticCondition::And {
                conditions: vec![
                    StaticCondition::WasCast { zone: None },
                    StaticCondition::SourceEnteredThisTurn,
                ],
            });
        }
    }

    // Compound " and " splitting: try splitting on " and ", parse both halves recursively.
    // Only succeeds if BOTH halves parse independently — avoids false splits on
    // noun phrases like "artifacts and creatures".
    if let Some(condition) = try_split_compound_and(text) {
        return Some(condition);
    }

    if matches_soulbond_paired_condition(tp.lower) {
        return Some(StaticCondition::SourceIsPaired);
    }

    // Note: "you have at least N life more than your starting life total"
    // (LifeAboveStarting ≥ N) is now owned by `parse_inner_condition` above
    // (see `parse_you_have_conditions`), so both the static "as long as" gate
    // and the trigger intervening-if share one parse path. No separate arm here.

    if tp.lower == "you have max speed" || tp.lower == "have max speed" {
        return Some(StaticCondition::HasMaxSpeed);
    }
    if tp.lower == "you don't have max speed" || tp.lower == "don't have max speed" {
        return Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::HasMaxSpeed),
        });
    }
    if let Some(speed_text) = nom_tag_lower(tp.lower, tp.lower, "your speed is ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(number_text) = speed_text.strip_suffix(" or higher") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            if let Some((threshold, remainder)) = parse_number(number_text) {
                if remainder.trim().is_empty() {
                    return Some(StaticCondition::SpeedGE {
                        threshold: u8::try_from(threshold).ok()?,
                    });
                }
            }
        }
    }

    // "your devotion to [color(s)] is less than N" (Theros gods)
    if let Some(condition) = parse_devotion_condition(tp.lower) {
        return Some(condition);
    }

    // "the number of [quantity] is [comparator] [quantity]"
    if let Some(condition) = parse_quantity_comparison(tp.lower) {
        return Some(condition);
    }

    // "[N] or more [type] are on the battlefield" (Limited Resources)
    if let Some(condition) = parse_count_on_battlefield_condition(tp.lower) {
        return Some(condition);
    }

    // "a[n] [type] is on the battlefield" (Wirecat: "... if an enchantment is on
    // the battlefield") — singular existence gate = ObjectCount(type) >= 1.
    if let Some(condition) = parse_exists_on_battlefield_condition(tp.lower) {
        return Some(condition);
    }

    // "there are [N] or more [type] on the battlefield" (Hour of Revelation:
    // "... if there are ten or more nonland permanents on the battlefield") —
    // the existential-phrasing counterpart of the "[N] or more [type] are on the
    // battlefield" count form. Same ObjectCount(type) >= N shape.
    if let Some(condition) = parse_there_are_count_on_battlefield_condition(tp.lower) {
        return Some(condition);
    }

    // "there's a[n]/another [type] on the battlefield" (Shauku, Endbringer:
    // "... can't attack if there's another creature on the battlefield.") — the
    // singular existential of the "there are [N] or more" count form. Existence
    // gate = ObjectCount(type) >= 1; the "another " article carries source
    // exclusion through into the filter (Another prop).
    if let Some(condition) = parse_there_is_exists_on_battlefield_condition(tp.lower) {
        return Some(condition);
    }

    // "it shares a color with the most common color among all permanents
    // [or a color tied for most common]" (Heroic Defiance)
    if let Some(condition) = parse_shares_most_common_color_condition(tp.lower) {
        return Some(condition);
    }

    // "the chosen color is [color]"
    if let Some(color_name) = nom_tag_lower(tp.lower, tp.lower, "the chosen color is ") {
        let trimmed = color_name.trim().trim_end_matches('.');
        if let Ok((rest, color)) = nom_primitives::parse_color.parse(trimmed) {
            if rest.is_empty() {
                return Some(StaticCondition::ChosenColorIs { color });
            }
        }
    }

    None
}

pub(crate) fn parse_attached_static_condition(text: &str) -> Option<StaticCondition> {
    parse_static_condition(text).map(rebind_source_object_quantities_to_recipient)
}

/// CR 611.3a + CR 702.16: Parse a multi-clause conditional protection grant —
/// "protection from `<quality>` if `<condition>`, from `<quality>` if
/// `<condition>`, ..., and from `<quality>` if `<condition>`" (Dominaria's
/// Judgment: "gain protection from white if you control a Plains, from blue if
/// you control an Island, ..., and from green if you control a Forest").
///
/// Returns one `(ProtectionTarget, StaticCondition)` per clause so each color's
/// protection is gated on its OWN condition. The prior generic grant path
/// emitted a single static carrying every protection modification but only the
/// FINAL clause's condition — silently dropping the gating for every other
/// color (and leaving their qualities as raw `ProtectionTarget::CardType`
/// strings like `"white if you control a plains"`).
///
/// The trailing-condition stripper one layer up peels the FINAL clause's "if
/// `<condition>`" and re-applies it afterward, so the last clause may arrive as a
/// bare quality (`None` condition); only that final clause may omit its `if`.
/// Returns `None` for a single-clause grant or any unrecognized shape, so those
/// fall through to the existing path untouched.
pub(crate) fn parse_conditional_protection_grant_list(
    predicate: &str,
) -> Option<
    Vec<(
        crate::types::keywords::ProtectionTarget,
        Option<StaticCondition>,
    )>,
> {
    let (rest, grants) = conditional_protection_grant_list(predicate.trim()).ok()?;
    // Require full consumption and a genuine multi-clause list; a single clause
    // is parsed correctly by the generic suffix-condition path.
    (rest.trim().is_empty() && grants.len() >= 2).then_some(grants)
}

/// nom body for [`parse_conditional_protection_grant_list`]: lead-in followed by
/// one or more "from `<quality>` if `<condition>`" clauses (the leading clause's
/// "from" is consumed by the lead-in; the final one is Oxford-prefixed "and").
type ConditionalProtectionGrant = (
    crate::types::keywords::ProtectionTarget,
    Option<StaticCondition>,
);

fn conditional_protection_grant_list(
    input: &str,
) -> OracleResult<'_, Vec<ConditionalProtectionGrant>> {
    let (input, _) = alt((
        tag("gain protection from "),
        tag("gains protection from "),
        tag("have protection from "),
        tag("has protection from "),
    ))
    .parse(input)?;
    let (input, first) = conditional_protection_clause(input)?;
    let (input, rest) = many0(preceded(
        // Oxford-comma tolerant: longest separator first.
        alt((tag(", and from "), tag(", from "), tag(" and from "))),
        conditional_protection_clause,
    ))
    .parse(input)?;
    let grants = std::iter::once(first).chain(rest).collect();
    Ok((input, grants))
}

/// Parse one "`<protection quality>` if `<condition>`" clause, delegating the
/// quality to [`parse_protection_target`](crate::types::keywords::parse_protection_target)
/// and the condition run to [`parse_attached_condition_run`]. A bare trailing
/// quality (its `if <condition>` already peeled upstream) yields a `None`
/// condition for the caller to fill.
fn conditional_protection_clause(input: &str) -> OracleResult<'_, ConditionalProtectionGrant> {
    let (input, qualified) = opt((take_until(" if "), tag(" if "))).parse(input)?;
    match qualified {
        Some((quality, _)) => {
            let (input, condition) = parse_attached_condition_run(input)?;
            let target = crate::types::keywords::parse_protection_target(quality.trim());
            Ok((input, (target, Some(condition))))
        }
        None => {
            let (input, quality) = rest.parse(input)?;
            let target = crate::types::keywords::parse_protection_target(quality.trim());
            Ok((input, (target, None)))
        }
    }
}

pub(crate) fn rebind_source_object_quantities_to_recipient(
    condition: StaticCondition,
) -> StaticCondition {
    match condition {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => StaticCondition::QuantityComparison {
            lhs: rebind_source_object_quantity_expr_to_recipient(lhs),
            comparator,
            rhs: rebind_source_object_quantity_expr_to_recipient(rhs),
        },
        StaticCondition::And { conditions } => StaticCondition::And {
            conditions: conditions
                .into_iter()
                .map(rebind_source_object_quantities_to_recipient)
                .collect(),
        },
        StaticCondition::Or { conditions } => StaticCondition::Or {
            conditions: conditions
                .into_iter()
                .map(rebind_source_object_quantities_to_recipient)
                .collect(),
        },
        StaticCondition::Not { condition } => StaticCondition::Not {
            condition: Box::new(rebind_source_object_quantities_to_recipient(*condition)),
        },
        StaticCondition::HasCounters {
            counters,
            minimum,
            maximum,
        } => StaticCondition::RecipientHasCounters {
            counters,
            minimum,
            maximum,
        },
        other => other,
    }
}

pub(crate) fn rebind_source_object_quantity_expr_to_recipient(expr: QuantityExpr) -> QuantityExpr {
    match expr {
        QuantityExpr::Ref { qty } => QuantityExpr::Ref {
            qty: rebind_source_object_quantity_ref_to_recipient(qty),
        },
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(rebind_source_object_quantity_expr_to_recipient(*inner)),
            divisor,
            rounding,
        },
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(rebind_source_object_quantity_expr_to_recipient(*inner)),
            offset,
        },
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(rebind_source_object_quantity_expr_to_recipient(*inner)),
            minimum,
        },
        QuantityExpr::Multiply { inner, factor } => QuantityExpr::Multiply {
            inner: Box::new(rebind_source_object_quantity_expr_to_recipient(*inner)),
            factor,
        },
        QuantityExpr::Sum { exprs } => QuantityExpr::Sum {
            exprs: exprs
                .into_iter()
                .map(rebind_source_object_quantity_expr_to_recipient)
                .collect(),
        },
        QuantityExpr::UpTo { max } => QuantityExpr::UpTo {
            max: Box::new(rebind_source_object_quantity_expr_to_recipient(*max)),
        },
        QuantityExpr::Power { base, exponent } => QuantityExpr::Power {
            base,
            exponent: Box::new(rebind_source_object_quantity_expr_to_recipient(*exponent)),
        },
        QuantityExpr::Difference { left, right } => QuantityExpr::Difference {
            left: Box::new(rebind_source_object_quantity_expr_to_recipient(*left)),
            right: Box::new(rebind_source_object_quantity_expr_to_recipient(*right)),
        },
        QuantityExpr::Max { exprs } => QuantityExpr::Max {
            exprs: exprs
                .into_iter()
                .map(rebind_source_object_quantity_expr_to_recipient)
                .collect(),
        },
        other => other,
    }
}

pub(crate) fn rebind_source_object_quantity_ref_to_recipient(qty: QuantityRef) -> QuantityRef {
    match qty {
        QuantityRef::Power {
            scope: ObjectScope::Source,
        } => QuantityRef::Power {
            scope: ObjectScope::Recipient,
        },
        QuantityRef::Toughness {
            scope: ObjectScope::Source,
        } => QuantityRef::Toughness {
            scope: ObjectScope::Recipient,
        },
        QuantityRef::ObjectManaValue {
            scope: ObjectScope::Source,
        } => QuantityRef::ObjectManaValue {
            scope: ObjectScope::Recipient,
        },
        other => other,
    }
}

/// Parse the trailing " unless [condition]" clause of a combat-restriction
/// static. Delegates `Not`-wrapping (with the `UnlessPay` raw-passthrough
/// exception) to the shared `parse_unless_condition` combinator so the static
/// layer and the `parse_condition` "unless " dispatch share one polarity rule.
pub(crate) fn parse_unless_static_condition(tp: &TextPair<'_>) -> Option<StaticCondition> {
    let (_, unless_text) = tp.split_around(" unless ")?;
    let original = unless_text.original.trim().trim_end_matches('.');
    let lower = original.to_lowercase();
    if let Ok((_, condition)) = nom_condition::parse_unless_condition(&lower) {
        return Some(condition);
    }
    // CR 611.3a: "gets +X/+X unless <condition>" applies the grant precisely when
    // <condition> is false — fall back to the shared static-condition parser and
    // negate, so a recognized inner condition (e.g. Heroic Defiance's most-common-
    // color check) gates the grant instead of being swallowed as Unrecognized.
    if let Some(condition) = parse_static_condition(original) {
        return Some(StaticCondition::Not {
            condition: Box::new(condition),
        });
    }
    // Preserve the Oracle unless rider in the AST so swallow/coverage see a
    // `condition` slot even when the inner clause is not yet decomposed.
    Some(StaticCondition::Not {
        condition: Box::new(StaticCondition::Unrecognized {
            text: format!("unless {original}"),
        }),
    })
}

/// True when `if_offset` points at an `if …` gate immediately preceded by `as `
/// (the `as if` phrase).
fn is_as_if_gate_marker(input: &str, if_offset: usize) -> bool {
    let Some(start) = if_offset.checked_sub(3) else {
        return false;
    };
    if !input.is_char_boundary(start) {
        return false;
    }
    tag::<_, _, OracleError<'_>>("as ")
        .parse(&input[start..if_offset])
        .is_ok()
}

/// Split a trailing `" as long as <condition>"` rider, anchored on the last
/// occurrence (restriction gates are terminal).
fn split_trailing_as_long_as(lower: &str) -> Option<&str> {
    let (_, _, tail) = nom_primitives::scan_last_at_word_boundaries_with_offset(lower, |i| {
        tag::<_, _, OracleError<'_>>("as long as ").parse(i)
    })?;
    Some(tail.trim_start())
}

/// Split a trailing `" if <condition>"` rider, skipping `as if` false positives
/// via word-boundary scanning with a nom `as ` prefix guard.
fn split_trailing_if_condition(lower: &str) -> Option<&str> {
    let (_, _, tail) = nom_primitives::scan_last_valid_at_word_boundaries_with_offset(
        lower,
        |i| tag::<_, _, OracleError<'_>>("if ").parse(i),
        |if_offset| !is_as_if_gate_marker(lower, if_offset),
    )?;
    Some(tail.trim_start())
}

fn split_trailing_if_condition_tp<'a>(tp: &'a TextPair<'a>) -> Option<&'a str> {
    let (_, _, tail_lower) = nom_primitives::scan_last_valid_at_word_boundaries_with_offset(
        tp.lower,
        |i| tag::<_, _, OracleError<'_>>("if ").parse(i),
        |if_offset| !is_as_if_gate_marker(tp.lower, if_offset),
    )?;
    let start = tp.lower.len().checked_sub(tail_lower.len())?;
    Some(tp.original.get(start..)?.trim_start())
}

/// CR 508.1c + CR 509.1b: Split the gated combat tail `<A> and can't block if <B>`
/// after the leading `"can't attack if "` marker has been consumed.
fn parse_dual_gated_combat_condition_tails(input: &str) -> OracleResult<'_, (&str, &str)> {
    let (input, attack_cond) = take_until(" and can't block if ").parse(input)?;
    let (input, _) = tag(" and can't block if ").parse(input)?;
    let (input, block_cond) = terminated(rest, opt(tag("."))).parse(input)?;
    Ok((input, (attack_cond, block_cond)))
}

/// CR 508.1c + CR 509.1b: Split a compound "~ can't attack if <A> and can't block
/// if <B>" static into two gated restrictions (The Fallen Apart).
fn parse_dual_gated_cant_attack_block(input: &str) -> OracleResult<'_, (&str, &str, &str)> {
    let (input, subject) = take_until("can't attack if ").parse(input)?;
    let (input, _) = tag("can't attack if ").parse(input)?;
    let (input, (attack_cond, block_cond)) = parse_dual_gated_combat_condition_tails(input)?;
    Ok((input, (subject, attack_cond, block_cond)))
}

fn is_self_ref_combat_subject(subject: &str) -> bool {
    let subject = subject.trim();
    subject == "~"
        || subject == "it"
        || SELF_REF_TYPE_PHRASES.contains(&subject)
        || SELF_REF_PARSE_ONLY_PHRASES.contains(&subject)
}

fn lower_subslice_to_original<'a>(tp: &'a TextPair<'a>, lower_sub: &str) -> Option<&'a str> {
    let start = lower_sub.as_ptr() as usize - tp.lower.as_ptr() as usize;
    tp.original.get(start..start + lower_sub.len())
}

fn parse_attached_combat_subject_nom(input: &str) -> OracleResult<'_, TargetFilter> {
    all_consuming(alt((
        value(
            TargetFilter::Typed(TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy])),
            tag("enchanted permanent"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])),
            tag("enchanted creature"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::land().properties(vec![FilterProp::EnchantedBy])),
            tag("enchanted land"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EquippedBy])),
            tag("equipped creature"),
        ),
    )))
    .parse(input)
}

fn is_attached_combat_subject(subject: &str) -> Option<TargetFilter> {
    parse_attached_combat_subject_nom(subject.trim())
        .ok()
        .map(|(_, filter)| filter)
}

fn dual_gated_combat_affected(subject_lower: &str) -> Option<TargetFilter> {
    is_attached_combat_subject(subject_lower)
        .or_else(|| is_self_ref_combat_subject(subject_lower).then_some(TargetFilter::SelfRef))
}

fn try_parse_dual_gated_cant_attack_and_cant_block(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    let (remainder, (subject_lower, attack_cond_lower, block_cond_lower)) =
        parse_dual_gated_cant_attack_block(tp.lower).ok()?;
    let affected = dual_gated_combat_affected(subject_lower)?;
    if !remainder.trim().is_empty() || attack_cond_lower.is_empty() || block_cond_lower.is_empty() {
        return None;
    }
    let attack_cond = lower_subslice_to_original(tp, attack_cond_lower)?;
    let block_cond = lower_subslice_to_original(tp, block_cond_lower)?;
    let (Some(attack_condition), Some(block_condition)) = (
        parse_static_condition(attack_cond.trim()),
        parse_static_condition(block_cond.trim()),
    ) else {
        // CR 508.1c + CR 509.1b: both gates must decompose — an unrecognized
        // rider must not collapse to unconditional CantAttack / CantBlock.
        return Some(vec![]);
    };
    Some(vec![
        StaticDefinition::new(StaticMode::CantAttack)
            .affected(affected.clone())
            .condition(attack_condition)
            .description(text.to_string()),
        StaticDefinition::new(StaticMode::CantBlock)
            .affected(affected)
            .condition(block_condition)
            .description(text.to_string()),
    ])
}

/// CR 508.1c + CR 509.1b + CR 611.3a: Decompose `"<grant> and can't attack if
/// <A> and can't block if <B>"` into the leading grant static(s) plus gated
/// `CantAttack` and `CantBlock` companions sharing the grant's `affected`.
///
/// Without this split the dual-gate arm declines (the subject prefix carries the
/// grant) and the bare `try_split_and_cant_attack` / `try_split_and_cant_block`
/// arms decline (non-terminal gated tails), so only the pump grant is emitted.
fn try_split_grant_and_dual_gated_combat(
    tp: &TextPair<'_>,
    text: &str,
) -> Option<Vec<StaticDefinition>> {
    type VE<'a> = OracleError<'a>;

    // `scan_preceded` resumes at word boundaries without the preceding space, so
    // match `and can't attack if ` (not ` and …`) — same as `try_split_and_cant_attack`.
    let (grant_lower, _matched, gates_lower) =
        nom_primitives::scan_preceded(tp.lower, |i: &str| {
            let (i, _) = alt((
                tag::<_, _, VE>("and can't attack if "),
                tag::<_, _, VE>("and can\u{2019}t attack if "),
            ))
            .parse(i)?;
            Ok((i, ()))
        })?;

    let (remainder, (attack_cond_lower, block_cond_lower)) =
        parse_dual_gated_combat_condition_tails(gates_lower).ok()?;
    if !remainder.trim().is_empty() || attack_cond_lower.is_empty() || block_cond_lower.is_empty() {
        return None;
    }

    let grant_text = lower_subslice_to_original(tp, grant_lower.trim())?;
    let grant_line = format!("{}.", grant_text.trim_end_matches('.'));
    let mut defs = parse_static_line_multi(&grant_line);
    if defs.is_empty() {
        return None;
    }

    let affected = defs.iter().find_map(|def| def.affected.clone())?;

    let attack_cond = lower_subslice_to_original(tp, attack_cond_lower)?;
    let block_cond = lower_subslice_to_original(tp, block_cond_lower)?;
    let (Some(attack_condition), Some(block_condition)) = (
        parse_static_condition(attack_cond.trim()),
        parse_static_condition(block_cond.trim()),
    ) else {
        return None;
    };

    for def in &mut defs {
        def.description = Some(text.to_string());
    }
    defs.push(
        StaticDefinition::new(StaticMode::CantAttack)
            .affected(affected.clone())
            .condition(attack_condition)
            .description(text.to_string()),
    );
    defs.push(
        StaticDefinition::new(StaticMode::CantBlock)
            .affected(affected)
            .condition(block_condition)
            .description(text.to_string()),
    );
    Some(defs)
}

/// CR 611.3a: A static restriction may carry a trailing gate introduced by
/// either `" as long as <condition>"` (continuous) or `" if <condition>"` (state
/// gate) — e.g. Rock Jockey: "You can't play lands if this creature was cast
/// this turn." Returns the condition text for `parse_static_condition`. The
/// `as long as` form is tried first so a card carrying both keywords anchors on
/// the continuous form; a bare `if` gate uses the last valid trailing "if"
/// (not an "as if" substring). As with the `as long as` peel, an unrecognized
/// condition downstream leaves the line unsupported rather than enforcing the
/// restriction unconditionally.
pub(crate) fn split_trailing_gate_condition(lower: &str) -> Option<&str> {
    split_trailing_as_long_as(lower).or_else(|| split_trailing_if_condition(lower))
}

/// Body-preserving sibling of [`split_trailing_gate_condition`] for callers that
/// must re-parse the pre-gate body as its own static (e.g. an extra-blocker grant
/// gated on "… as long as you're the monarch"). Returns `(body, condition)` in
/// ORIGINAL case from the SAME authority — `as long as` is tried first, then the
/// last valid `if` marker (excluding `as if`) — so trailing-gate splitting is not
/// re-implemented per call site. `body` is the line with the trailing gate
/// removed (trailing separator whitespace trimmed); `condition` is the gate's
/// condition text for [`parse_static_condition`]. The word-boundary scan yields
/// the marker's byte offset, so the original-case body/condition are recovered by
/// slicing `tp.original` (mirroring `split_trailing_if_condition_tp`).
pub(crate) fn split_trailing_gate_condition_with_body<'a>(
    tp: &'a TextPair<'a>,
) -> Option<(&'a str, &'a str)> {
    let (marker_offset, _, tail_lower) =
        nom_primitives::scan_last_at_word_boundaries_with_offset(tp.lower, |i| {
            tag::<_, _, OracleError<'_>>("as long as ").parse(i)
        })
        .or_else(|| {
            nom_primitives::scan_last_valid_at_word_boundaries_with_offset(
                tp.lower,
                |i| tag::<_, _, OracleError<'_>>("if ").parse(i),
                |if_offset| !is_as_if_gate_marker(tp.lower, if_offset),
            )
        })?;
    let condition_start = tp.lower.len().checked_sub(tail_lower.len())?;
    let condition = tp.original.get(condition_start..)?.trim_start();
    let body = tp.original.get(..marker_offset)?.trim_end();
    Some((body, condition))
}

/// CR 508.1c / CR 509.1b: Parse the trailing " if [condition]" clause of a
/// combat-restriction static ("~ can't attack if defending player controls an
/// untapped land"; "~ can't block if you control an untapped land"). Mirrors
/// `parse_unless_static_condition`; delegates the condition body to
/// `parse_static_condition` → `parse_inner_condition` (the single authority
/// for game-state conditions).
pub(crate) fn parse_if_static_condition(tp: &TextPair<'_>) -> Option<StaticCondition> {
    let condition_text = split_trailing_if_condition_tp(tp)?;
    parse_static_condition(condition_text.trim_end_matches('.'))
}

/// CR 611.3a: Parse the trailing " as long as [condition]" clause of a
/// combat-restriction static ("~ can't attack or block as long as it has a stun
/// counter on it" — Seer of the Bright Side). "As long as" and "if" both express
/// a continuous game-state gate on a static ability (CR 611.3a), so this mirrors
/// [`parse_if_static_condition`] exactly, delegating the condition body to
/// `parse_static_condition` → `parse_inner_condition` (the single authority for
/// game-state conditions). Restriction arms peel "unless"/"if" but historically
/// dropped the "as long as" rider on their SelfRef restriction, enforcing it
/// unconditionally; this closes that keyword gap without touching the shared
/// condition grammar.
pub(crate) fn parse_as_long_as_static_condition(tp: &TextPair<'_>) -> Option<StaticCondition> {
    // CR 611.3a vs duration seam: "for as long as" is effect-duration/provenance
    // text (`Duration::ForAsLongAs` — Promise of Loyalty: "... can't attack you
    // or planeswalkers you control for as long as it has a vow counter on it"),
    // NOT a trailing static-restriction gate. Only a bare "as long as" introduces
    // a continuous game-state gate here; reject the "for as long as" form so it
    // stays with the duration/effect pipeline rather than being mis-attached as a
    // static condition.
    if tp.split_around(" for as long as ").is_some() {
        return None;
    }
    let (_, as_long_as_text) = tp.split_around(" as long as ")?;
    parse_static_condition(as_long_as_text.original)
}

/// Result of the combat-tax nom parse.
pub(crate) struct CombatTaxParse {
    pub(super) mode: StaticMode,
    pub(super) affected: TargetFilter,
    pub(super) base_cost: ManaCost,
    pub(super) scaling: crate::types::ability::UnlessPayScaling,
    /// CR 506.3 + CR 508.1d: Which declared attacks this tax applies to. `None`
    /// for the block side and for tax-attack lines with no explicit defender
    /// scope. `Some(AttackTargetFilter::Player)` for "...attack you...";
    /// `Some(AttackTargetFilter::PlayerOrPlaneswalker)` for "...attack you or
    /// planeswalkers you control...".
    pub(super) defended: Option<crate::types::triggers::AttackTargetFilter>,
}

/// Subject axis of the combat-tax grammar.
#[derive(Debug, Clone)]
pub(crate) enum CombatTaxSubject {
    /// "[Color] creatures [can't attack you]" — applies to opponents' creatures.
    /// CR 105.2: the optional `FilterProp` carries a color predicate
    /// (`HasColor` for "Red creatures", `NotColor` for "Nonblack creatures" —
    /// Elephant Grass). `None` is the bare "Creatures" form (Ghostly Prison).
    Creatures(Option<FilterProp>),
    /// "Enchanted creature [can't attack]" — aura attached-to creature form (Brainwash).
    EnchantedCreature,
    /// CR 122.1: "Each creature with one or more counters on it [can't attack you]"
    /// — counter-gated subject form (Nils, Discipline Enforcer). Applies to every
    /// creature on the battlefield carrying at least one counter; pairs naturally
    /// with per-affected cost scaling driven by the attacker's counter count.
    EachCreatureWithCounters,
    /// CR 508.1d / CR 509.1c: "~ can't attack [or block] unless you pay {N} ..."
    /// — self-referential combat tax on the source permanent itself (Myr
    /// Prototype, Phyrexian Marauder). The affected filter is `SelfRef`.
    SourcePermanent,
}

pub(crate) fn parse_for_each_cost_quantity(input: &str) -> OracleResult<'_, QuantityRef> {
    let (input, _) = tag_no_case::<_, _, OracleError<'_>>(" for each ").parse(input)?;
    let lowered = input.trim_end_matches('.').to_lowercase();
    let (_, quantity) = super::oracle_nom::quantity::parse_for_each_clause_ref_complete(&lowered)
        .map_err(|_| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
    })?;
    Ok(("", quantity))
}

/// Parse ", where X is the number of <filter>" → `QuantityRef::ObjectCount {...}`.
/// Used by Sphere of Safety. Delegates to the shared `parse_quantity_ref`
/// which handles "the number of <filter>" as a single alternative.
///
/// CR 122.1: Also recognizes the untyped-counter anaphoric phrasing ", where X
/// is the number of counters on that creature" → `QuantityRef::AnyCountersOnTarget`.
/// The shared `parse_quantity_ref` rejects this because it requires a non-empty
/// counter-type prefix; Nils, Discipline Enforcer's text omits the counter type,
/// so the dedicated branch is tried first.
pub(crate) fn parse_dynamic_x_clause(input: &str) -> OracleResult<'_, QuantityRef> {
    use crate::parser::oracle_nom::error::OracleError;

    let (input, _) = tag_no_case::<_, _, OracleError<'_>>(", where x is ").parse(input)?;

    // CR 122.1: Untyped counter anaphor — consume the rest of the clause and
    // emit `AnyCountersOnTarget`. Accepted variants mirror the counter-on-target
    // anaphor family (no type prefix).
    if let Ok((_, _)) = alt((
        tag_no_case::<_, _, OracleError<'_>>("the number of counters on that creature"),
        tag_no_case::<_, _, OracleError<'_>>("the number of counters on that permanent"),
    ))
    .parse(input)
    {
        return Ok((
            "",
            QuantityRef::CountersOn {
                scope: ObjectScope::Target,
                counter_type: None,
            },
        ));
    }

    // Delegate to the shared quantity-ref combinator which is case-sensitive on
    // lowercase patterns ("the number of"). Normalize to lowercase for the
    // remaining phrase so the upstream combinators match.
    let lowered = input.to_lowercase();
    let (_, quantity) =
        super::oracle_nom::quantity::parse_quantity_ref(&lowered).map_err(|e| match e {
            nom::Err::Error(_) | nom::Err::Failure(_) => {
                nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Fail))
            }
            nom::Err::Incomplete(n) => nom::Err::Incomplete(n),
        })?;
    // Don't try to keep a &str reference into the lowered string — accept that the
    // dynamic-X clause consumes the rest of the phrase and return empty remainder.
    Ok(("", quantity))
}

/// Parse "your devotion to [color(s)] is less than N" or "is N or greater".
pub(crate) fn parse_devotion_condition(lower: &str) -> Option<StaticCondition> {
    let rest = nom_tag_lower(lower, lower, "your devotion to ")?;

    // Split at " is " to get colors and comparison
    let (color_text, comparison) = rest.split_once(" is ")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.

    // Parse colors: "white", "blue and red", "white and black"
    let colors = parse_color_list(color_text)?;

    // Parse comparison: "less than N" or "N or greater"
    // CR 110.4b: "less than N" means NOT (devotion >= N), "N or greater" means devotion >= N.
    if let Some(n_text) = nom_tag_lower(comparison, comparison, "less than ") {
        let threshold = parse_number(n_text.trim())?.0;
        return Some(StaticCondition::Not {
            condition: Box::new(StaticCondition::DevotionGE { colors, threshold }),
        });
    }

    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some(n_rest) = comparison.strip_suffix(" or greater") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let threshold = parse_number(n_rest.trim())?.0;
        return Some(StaticCondition::DevotionGE { colors, threshold });
    }

    None
}

/// Parse a color list like "white", "blue and red", "white, blue, and black".
/// Parse a list of color names: "red", "white and blue", "red, white, and blue".
///
/// Delegates individual color word recognition to the shared nom color combinator.
pub(crate) fn parse_color_list(text: &str) -> Option<Vec<crate::types::mana::ManaColor>> {
    /// Parse a single color name using the nom combinator with case normalization.
    fn color_from_name(s: &str) -> Option<crate::types::mana::ManaColor> {
        let lower = s.trim().to_ascii_lowercase();
        let (rest, color) = nom_primitives::parse_color.parse(&lower).ok()?;
        if rest.is_empty() {
            Some(color)
        } else {
            None
        }
    }

    // Try single color first
    if let Some(c) = color_from_name(text) {
        return Some(vec![c]);
    }

    // "X and Y"
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    if let Some((a, b)) = text.split_once(" and ") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let mut colors = Vec::new();
        // Handle "X, Y, and Z" — a would be "X, Y" and b would be "Z"
        for part in a.split(", ") {
            colors.push(color_from_name(part)?);
        }
        colors.push(color_from_name(b)?);
        return Some(colors);
    }

    None
}

/// CR 105.2 + CR 611.3a: "it shares a color with the most common color among all
/// permanents[ or a color tied for most common]" (Heroic Defiance) →
/// `SharesColorWithMostCommonColorAmongPermanents`. The optional "or a color tied
/// for most common" tail is redundant — the runtime predicate already treats
/// every color at the maximum count as most-common — so both phrasings map to the
/// same condition.
pub(crate) fn parse_shares_most_common_color_condition(lower: &str) -> Option<StaticCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>(
        "it shares a color with the most common color among all permanents",
    )
    .parse(lower)
    .ok()?;
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>(
        " or a color tied for most common",
    ))
    .parse(rest)
    .ok()?;
    rest.trim()
        .is_empty()
        .then_some(StaticCondition::SharesColorWithMostCommonColorAmongPermanents)
}

/// CR 611.3a: "[N] or more [type] are on the battlefield" → a count
/// `QuantityComparison` (Limited Resources: "ten or more lands are on the
/// battlefield"). Modeled as `ObjectCount(type) >= N`; the gate is then attached
/// by the shared "as long as <condition>" machinery to the host static.
pub(crate) fn parse_count_on_battlefield_condition(lower: &str) -> Option<StaticCondition> {
    count_on_battlefield_condition(lower)
        .ok()
        .and_then(|(rest, cond)| rest.trim().is_empty().then_some(cond))
}

/// CR 611.3a: "there are [N] or more [type] on the battlefield" → the same count
/// gate as `count_on_battlefield_condition` (`ObjectCount(type) >= N`) but in the
/// existential "there are …" phrasing (Hour of Revelation: "This spell costs {3}
/// less to cast if there are ten or more nonland permanents on the
/// battlefield."). The count form anchors "are on the battlefield" after the
/// type; this form fronts the "there are" existential and closes with a bare
/// "on the battlefield".
pub(crate) fn parse_there_are_count_on_battlefield_condition(
    lower: &str,
) -> Option<StaticCondition> {
    there_are_count_on_battlefield_condition(lower)
        .ok()
        .and_then(|(rest, cond)| rest.trim().is_empty().then_some(cond))
}

/// CR 611.3a: "there's a[n]/another [type] on the battlefield" → an existence
/// gate `ObjectCount(type) >= 1` (Shauku, Endbringer: "Shauku can't attack if
/// there's another creature on the battlefield."). The singular existential
/// counterpart of `there_are_count_on_battlefield_condition` ("there are [N] or
/// more [type] …"): it fronts the "there's"/"there is" existential and closes
/// with a bare "on the battlefield" (no trailing "is"/"are", unlike
/// `exists_on_battlefield_condition`, which anchors "is on the battlefield").
///
/// The indefinite article "a "/"an " is stripped, but "another " is preserved so
/// `parse_type_phrase` attaches the source-exclusion `Another` prop — "another
/// creature" must count creatures OTHER than the source (else the source itself
/// would satisfy its own gate and the restriction would never lift).
pub(crate) fn parse_there_is_exists_on_battlefield_condition(
    lower: &str,
) -> Option<StaticCondition> {
    there_is_exists_on_battlefield_condition(lower)
        .ok()
        .and_then(|(rest, cond)| rest.trim().is_empty().then_some(cond))
}

fn there_is_exists_on_battlefield_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (input, _) = alt((tag("there's "), tag("there is "))).parse(input)?;
    let (input, subject) = take_until(" on the battlefield").parse(input)?;
    let (input, _) = tag(" on the battlefield").parse(input)?;
    let subject = subject.trim();
    // Strip the indefinite article ("a"/"an") but keep "another " — parse_article's
    // trailing-space word boundary leaves "another <type>" (source exclusion) intact.
    let (type_text, _) = opt(nom_primitives::parse_article).parse(subject)?;
    let (filter, remainder) = parse_type_phrase(type_text.trim());
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        input,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
    ))
}

fn there_are_count_on_battlefield_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (input, _) = tag("there are ").parse(input)?;
    let (input, n) = nom_primitives::parse_number(input)?;
    let (input, _) = tag(" or more ").parse(input)?;
    let (input, type_text) = take_until(" on the battlefield").parse(input)?;
    let (input, _) = tag(" on the battlefield").parse(input)?;
    let (filter, remainder) = parse_type_phrase(type_text.trim());
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        input,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

fn count_on_battlefield_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (input, n) = nom_primitives::parse_number(input)?;
    let (input, _) = tag(" or more ").parse(input)?;
    let (input, type_text) = take_until(" are on the battlefield").parse(input)?;
    let (input, _) = tag(" are on the battlefield").parse(input)?;
    let (filter, remainder) = parse_type_phrase(type_text.trim());
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        input,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: n as i32 },
        },
    ))
}

/// CR 611.3a: "a[n] [type] is on the battlefield" → an existence gate, i.e.
/// `ObjectCount(type) >= 1` (Wirecat: "This creature can't attack or block if an
/// enchantment is on the battlefield."). Singular counterpart of
/// `count_on_battlefield_condition` ("[N] or more [type] are on the
/// battlefield"); it reuses the same `ObjectCount >= n` shape with `n = 1`. The
/// type phrase must consume the whole subject, mirroring the count form's guard.
pub(crate) fn parse_exists_on_battlefield_condition(lower: &str) -> Option<StaticCondition> {
    exists_on_battlefield_condition(lower)
        .ok()
        .and_then(|(rest, cond)| rest.trim().is_empty().then_some(cond))
}

fn exists_on_battlefield_condition(input: &str) -> OracleResult<'_, StaticCondition> {
    let (input, _) = alt((tag("an "), tag("a "))).parse(input)?;
    let (input, type_text) = take_until(" is on the battlefield").parse(input)?;
    let (input, _) = tag(" is on the battlefield").parse(input)?;
    let (filter, remainder) = parse_type_phrase(type_text.trim());
    if matches!(filter, TargetFilter::Any) || !remainder.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((
        input,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        },
    ))
}

/// Parse "the number of [quantity] is [comparator] [quantity]" into a QuantityComparison.
pub(crate) fn parse_quantity_comparison(lower: &str) -> Option<StaticCondition> {
    let rest = nom_tag_lower(lower, lower, "the number of ")?;
    let (lhs_text, comparison) = rest.split_once(" is ")?; // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let lhs = parse_quantity_ref(lhs_text)?;
    let (comparator, rhs_text) = parse_comparator_prefix(comparison)?;
    let rhs = parse_quantity_ref(rhs_text.trim())?;
    Some(StaticCondition::QuantityComparison {
        lhs: QuantityExpr::Ref { qty: lhs },
        comparator,
        rhs: QuantityExpr::Ref { qty: rhs },
    })
}

pub(crate) fn find_continuous_predicate_start(lower: &str) -> Option<usize> {
    [
        " gets ", " get ", " gains ", " gain ", " has ", " have ", " loses ", " lose ",
    ]
    .into_iter()
    .filter_map(|marker| lower.find(marker))
    .min()
}

/// CR 108.3 + CR 109.4: Strip a leading negated-ownership qualifier ("but don't
/// own", "but do not own") from a "<subject> you control" predicate tail.
///
/// The "<X> you control" dispatch arms (`creatures you control `, `other
/// creatures you control `) consume the `you control` controller anchor before
/// the predicate, so a trailing "but don't own" qualifier would otherwise be
/// silently dropped from the affected filter. Returns the
/// `FilterProp::Owned { Opponent }` property ("controller doesn't own it") and
/// the remaining predicate text when the qualifier is present. The companion
/// "but don't own" handling in `parse_type_phrase` covers the full-subject path
/// (Laughing Jasper Flint's "Creatures you control but don't own are
/// Mercenaries …"); this is the controller-prefix-consumed sibling.
pub(crate) fn strip_negated_ownership_qualifier(after_prefix: &str) -> Option<(FilterProp, &str)> {
    type VE<'a> = OracleError<'a>;
    for qualifier in [
        "but don't own ",
        "but do not own ",
        "but doesn't own ",
        "but does not own ",
    ] {
        if let Ok((rest, _)) = tag::<_, _, VE>(qualifier).parse(after_prefix) {
            return Some((
                FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                },
                rest,
            ));
        }
    }
    None
}

pub(crate) fn parse_qualified_creatures_you_control_suffix<'a>(
    subject_prefix: &str,
    after_prefix: &'a str,
    after_prefix_lower: &str,
) -> Option<(TargetFilter, &'a str)> {
    let subject_end = find_continuous_predicate_start(after_prefix_lower)?;
    let qualifier = after_prefix[..subject_end].trim();
    if qualifier.is_empty() {
        return None;
    }

    let subject = format!("{subject_prefix} {qualifier}");
    let filter = parse_continuous_subject_filter(&subject)?;
    let predicate_text = after_prefix[subject_end + 1..].trim_start();
    Some((filter, predicate_text))
}

/// CR 611.3a: Split an Oxford-comma / "and" / "or" / "and/or" subject list into
/// its item text slices — "Skeletons, Vampires, and Zombies" →
/// `["Skeletons", "Vampires", "Zombies"]`; "Plants and Treefolk" →
/// `["Plants", "Treefolk"]`; a single subject → one item.
///
/// Uses `separated_list1(separator, item)` — the same idiom as
/// [`parse_subtype_or_list_prefix_with_word_parser`] — where each item is the
/// maximal run of characters that does not begin a list separator. Unlike a
/// word-boundary scan (which only ever tries a match at the start of a word and
/// so can never match a separator that begins with a comma or a leading space),
/// this consumes a separator wherever it actually starts, so a shared-suffix
/// Oxford list splits into *every* item rather than collapsing to one.
///
/// A bare comma is only a list separator inside a *coordinated* enumeration:
/// English writes a genuine subject union as "A, B, and C" / "A and B" /
/// "A or B" — always with a coordinating conjunction before the final item. A
/// bare-comma sequence with no coordinator is instead a stack of pre-nominal
/// adjectives modifying one shared head noun ("Noncreature, non-Equipment
/// artifacts" — Dan Lewis: artifacts that are noncreature AND non-Equipment),
/// which is a *single* subject, not a union. A pure `", "` split cannot tell the
/// two apart — both "Noncreature" and "non-Equipment artifacts" anchor as valid
/// standalone subjects — so this gates bare-comma splitting on the presence of a
/// coordinator ([`list_has_coordinator`]). Without one, the whole descriptor is
/// returned as a single item and the caller defers to the single-subject path.
///
/// Bare " or " is also excluded from the separator grammar (it would split an
/// intra-subject qualifier like "with power 3 or greater"); only the
/// comma-anchored ", or " is a separator. As a final backstop, the caller
/// re-parses every item as a controller-anchored subject and rejects the whole
/// compound (`None`) unless each one anchors, so an over-split can never emit a
/// wrong `Or`.
fn split_subject_list(text: &str) -> Vec<&str> {
    // Not a coordinated enumeration → one subject (do not split bare commas).
    if !list_has_coordinator(text) {
        return vec![text.trim()];
    }

    // One list item: one or more characters, none of which begins a separator.
    fn subject_list_item(input: &str) -> OracleResult<'_, &str> {
        recognize(many1(preceded(not(subject_list_separator), anychar))).parse(input)
    }

    match separated_list1(subject_list_separator, subject_list_item).parse(text) {
        // Require the split to consume the whole list (a trailing/dangling
        // separator leaves `rest` non-empty) — otherwise treat it as one item.
        Ok((rest, items)) if rest.trim().is_empty() => items.into_iter().map(str::trim).collect(),
        _ => vec![text.trim()],
    }
}

/// True when `text` carries a coordinating conjunction ("and" / "or" / "and/or")
/// at a word boundary — the syntactic marker of a genuine enumeration. Matched
/// without the leading space because [`nom_primitives::scan_at_word_boundaries`]
/// lands on each word start; the trailing space keeps "or"/"and" from matching
/// inside a word ("Warriors", "Orcs"). Oracle text writes these coordinators
/// lowercase mid-subject, so the original-case `text` matches directly.
fn list_has_coordinator(text: &str) -> bool {
    nom_primitives::scan_at_word_boundaries(text, |i: &str| {
        alt((
            tag::<_, _, OracleError<'_>>("and/or "),
            tag("and "),
            tag("or "),
        ))
        .parse(i)
    })
    .is_some()
}

/// One subject-list connector token. Ordered longest/most-specific first so that
/// at a comma position ", and " wins over ", " (no dangling comma on the item).
fn subject_list_separator(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        alt((
            tag::<_, _, OracleError<'_>>(", and/or "),
            tag(", and "),
            tag(", or "),
            tag(" and/or "),
            tag(" and "),
            tag(", "),
        )),
    )
    .parse(input)
}

fn parse_shared_controller_compound_subject_filter(subject: &TextPair<'_>) -> Option<TargetFilter> {
    let (descriptor, suffix) = parse_subject_suffix(subject, " you control")
        .map(|descriptor| (descriptor, " you control"))
        .or_else(|| {
            parse_subject_suffix(subject, " your opponents control")
                .map(|descriptor| (descriptor, " your opponents control"))
        })?;

    // A leading distribution word ("Other <list>", "Each other <list>", "Each
    // <list>") applies across EVERY list item, so strip it here and re-attach
    // "other " to each item's subject. A mid-list "other" (Grimlock: "…, and
    // other Transformers creatures") stays on its own item and is handled by
    // `parse_continuous_subject_filter` when that item is parsed.
    let descriptor_original = descriptor.original.trim();
    let descriptor_lower_owned = descriptor_original.to_lowercase();
    let descriptor_tp = TextPair::new(descriptor_original, &descriptor_lower_owned);
    let (list_text, distribute_other) =
        if let Some(rest) = nom_tag_tp(&descriptor_tp, "each other ") {
            (rest.original.trim(), true)
        } else if let Some(rest) = nom_tag_tp(&descriptor_tp, "other ") {
            (rest.original.trim(), true)
        } else if let Some(rest) = nom_tag_tp(&descriptor_tp, "each ") {
            (rest.original.trim(), false)
        } else {
            (descriptor_original, false)
        };
    if list_text.is_empty() {
        return None;
    }

    // Split the full Oxford-comma / and / or subject list into its items. A
    // single item is not a compound subject — defer to the single-subject path.
    let items = split_subject_list(list_text);
    if items.len() < 2 {
        return None;
    }

    let mut filters = Vec::new();
    for item in items {
        if item.is_empty() {
            return None;
        }
        let item_subject = if distribute_other {
            format!("other {item}{suffix}")
        } else {
            format!("{item}{suffix}")
        };
        let item_filter = parse_continuous_subject_filter(&item_subject)?;
        // Fail-safe: an intra-subject mis-split (e.g. a qualifier containing
        // "and") yields an item that isn't a controller-anchored subject, so we
        // bail to `None` and let the caller's other handlers try — never emit a
        // wrong Or.
        if !filter_has_source_or_controller_anchor(&item_filter) {
            return None;
        }
        push_or_filter_branch(&mut filters, item_filter);
    }
    Some(TargetFilter::Or { filters })
}

/// CR 702.143d (and the CR 702 alternative-cost cast-from-off-zone family):
/// parse "<type> cards in your hand [without <kw>] have <kw>. Its <kw> cost is
/// equal to its mana cost reduced by {N}." into a continuous
/// `AddKeywordWithDerivedCost` static (Singing Towers of Darillium). The granted
/// keyword name selects the `CostBearingKeywordKind`, so a future
/// "... have madness. Its madness cost is …" card reuses this branch with a
/// different kind. Combinator dispatch throughout — the per-recipient "without
/// foretell" dedup is enforced by the off-zone applier, so the leading "without
/// <kw>" qualifier is consumed but not re-encoded in the affected filter.
pub(crate) fn parse_hand_cards_have_derived_cost_keyword(text: &str) -> Option<StaticDefinition> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();
    let tp = TextPair::new(&stripped, &lower);
    let tp = nom_tag_tp(&tp, "each ").unwrap_or(tp);
    let (type_tp, after_hand) = tp.split_around(" in your hand ")?;

    fn kw_word(i: &str) -> OracleResult<'_, &str> {
        take_while1(|c: char| c.is_ascii_alphabetic()).parse(i)
    }
    fn body(i: &str) -> OracleResult<'_, (&str, ManaCost)> {
        // Optional "without <kw> " qualifier before "has/have <kw>".
        let (i, _) = opt((tag("without "), kw_word, tag(" ")).map(|_| ())).parse(i)?;
        let (i, _) = alt((tag("has "), tag("have "))).parse(i)?;
        let (i, kw1) = kw_word(i)?;
        let (i, _) = tag(". its ").parse(i)?;
        let (i, kw2) = kw_word(i)?;
        let (i, _) = tag(" cost is equal to its mana cost reduced by ").parse(i)?;
        let (i, reduction) = nom_primitives::parse_mana_cost(i)?;
        let (i, _) = opt(tag(".")).parse(i)?;
        if !kw1.eq_ignore_ascii_case(kw2) {
            return Err(nom::Err::Error(OracleError::new(
                i,
                nom::error::ErrorKind::Verify,
            )));
        }
        Ok((i, (kw1, reduction)))
    }
    let (_, (kw_name, reduction)) = body(after_hand.lower).ok()?;

    let kind = crate::types::keywords::CostBearingKeywordKind::from_name(kw_name)?;

    // Affected: the parsed type phrase (e.g. "nonland card"), owned by "you",
    // restricted to your hand. The off-zone applier reads each recipient's mana
    // cost to derive the granted cost.
    let (base_filter, rest) = parse_type_phrase(type_tp.original.trim());
    if !rest.trim().is_empty() {
        return None;
    }
    let TargetFilter::Typed(mut typed) = base_filter else {
        return None;
    };
    typed = typed.controller(ControllerRef::You);
    typed.properties.push(FilterProp::InAnyZone {
        zones: vec![Zone::Hand],
    });

    Some(
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(typed))
            .modifications(vec![ContinuousModification::AddKeywordWithDerivedCost {
                kind,
                derivation: crate::types::ability::CostDerivation::ManaCostReducedBy(reduction),
            }])
            .description(text.to_string()),
    )
}

/// CR 607.2d / CR 607.2m (by analogy): parse "<type> controlled by players who
/// last chose <label>" into the base type filter carrying
/// `FilterProp::ControllerChoseLabel`. Splits on the "controlled by player[s]
/// who last chose " head, parses the leading type phrase (must fully consume),
/// and canonicalizes the trailing anchor label. Returns `None` for any other
/// shape so it never shadows the generic subject parser.
fn parse_controlled_by_anchor_subject_filter(subject: &TextPair<'_>) -> Option<TargetFilter> {
    let (type_tp, label_tp) = subject
        .split_around(" controlled by players who last chose ")
        .or_else(|| subject.split_around(" controlled by player who last chose "))?;
    let (type_filter, rest) = parse_type_phrase(type_tp.original.trim());
    if !rest.trim().is_empty() || matches!(type_filter, TargetFilter::Any) {
        return None;
    }
    let label = canonicalize_anchor_label(label_tp.original.trim());
    if label.is_empty() {
        return None;
    }
    Some(merge_filter_prop(
        type_filter,
        FilterProp::ControllerChoseLabel { label },
    ))
}

/// True when `filter` is a typed filter carrying a creature subtype constraint.
/// The gate for the bare tribal compound below, so a generic
/// "creatures and <X>" compound is left to the type-phrase fallback.
fn filter_carries_subtype(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.type_filters.iter().any(|t| matches!(t, TypeFilter::Subtype(_)))
    )
}

/// CR 205.3m: True when the branch text explicitly names creatures. This gate
/// keeps the bare tribal compound to CREATURE anthems (Verdeloth's "Saproling
/// creatures" / "Treefolk creatures") and off subjects whose subtype belongs to
/// a different set — Life and Limb's "All Forests and all Saprolings", where
/// "Forests" is a LAND subtype that this creature-tribal helper must not
/// reinterpret as a creature (#5147). `filter_carries_subtype` alone accepts any
/// subtype (including land/artifact), so the explicit head noun is required.
fn branch_names_creatures(original: &str) -> bool {
    original
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w.eq_ignore_ascii_case("creature") || w.eq_ignore_ascii_case("creatures"))
}

/// CR 611.3a: A bare (battlefield-wide, no-controller) compound tribal anthem
/// subject where each branch carries its own creature subtype and the second may
/// take a per-branch "other" source exclusion — "<subtype> creatures and [other]
/// <subtype> creatures" (Verdeloth the Ancient: "Saproling creatures and other
/// Treefolk creatures get +1/+1"). The controller-scoped compound is handled by
/// [`parse_shared_controller_compound_subject_filter`]; this is the tribal
/// battlefield form. Each branch delegates to [`parse_continuous_subject_filter`]
/// (so "other Treefolk creatures" picks up the `Another` source exclusion via the
/// existing "other " arm), and the branches are OR'd. Both branches MUST resolve
/// to subtype-scoped typed filters, so a generic "creatures and <X>" compound is
/// left for the fallback rather than over-claimed.
fn parse_bare_compound_subtype_subject_filter(subject: &TextPair<'_>) -> Option<TargetFilter> {
    // Controller-scoped compounds belong to the sibling handler above.
    if parse_subject_suffix(subject, " you control").is_some()
        || parse_subject_suffix(subject, " your opponents control").is_some()
    {
        return None;
    }
    let (left_lower, _, right_lower) = nom_primitives::scan_preceded(subject.lower, |input| {
        value((), tag::<_, _, OracleError<'_>>("and ")).parse(input)
    })?;
    let right_start = subject.lower.len() - right_lower.len();
    let left_original = subject.original[..left_lower.len()].trim();
    let right_original = subject.original[right_start..].trim();
    if left_original.is_empty() || right_original.is_empty() {
        return None;
    }
    // Both branches must explicitly name creatures (CR 205.3m) AND resolve to a
    // subtype-scoped typed filter. The creature-term gate keeps this off subjects
    // whose subtype belongs to another set — e.g. Life and Limb's "All Forests
    // and all Saprolings", whose "Forests" is a LAND subtype that must remain a
    // land subject, not be reinterpreted here as a creature (#5147).
    if !branch_names_creatures(left_original) || !branch_names_creatures(right_original) {
        return None;
    }
    let left_filter = parse_continuous_subject_filter(left_original)?;
    let right_filter = parse_continuous_subject_filter(right_original)?;
    if !filter_carries_subtype(&left_filter) || !filter_carries_subtype(&right_filter) {
        return None;
    }
    let mut filters = Vec::new();
    push_or_filter_branch(&mut filters, left_filter);
    push_or_filter_branch(&mut filters, right_filter);
    Some(TargetFilter::Or { filters })
}

pub(crate) fn parse_continuous_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    let tp = TextPair::new(trimmed, &lower);

    // Strip "Each " / "All " quantifier prefixes — "Each creature you control" and
    // "All Sliver creatures" are semantically identical to the bare type phrase for
    // filter purposes (CR 205.3 / CR 700.1). Without this, "All Sliver creatures"
    // flows into parse_type_phrase which treats "All Sliver" as a verbatim subtype
    // string and matches zero real creatures.
    if let Some(rest_tp) = nom_tag_tp(&tp, "each ").or_else(|| nom_tag_tp(&tp, "all ")) {
        return parse_continuous_subject_filter(rest_tp.original.trim());
    }

    // CR 605.1 / CR 113.1: strip a trailing "with a mana ability" / "with no
    // abilities" object qualifier, parse the base subject recursively, and
    // attach the runtime-evaluated `FilterProp`. Covers Raggadragga, Goregutter
    // ("Each creature you control with a mana ability gets +2/+2"), Muraganda
    // Petroglyphs ("Creatures with no abilities get +2/+2"), and Ruxa, Patient
    // Professor ("Creatures you control with no abilities get +1/+1"). Both
    // props are matched authoritatively by `game::filter`
    // (`HasManaAbility` via the mana-ability classifier, `HasNoAbilities` via
    // `object_has_no_abilities`), so this is a grammar-only seam. The qualifier
    // must sit at the very end of the subject phrase (`after` empty) so a
    // mid-phrase "with ..." clause is not misclaimed.
    for (needle, prop) in [
        (" with a mana ability", FilterProp::HasManaAbility),
        (" with no abilities", FilterProp::HasNoAbilities),
    ] {
        let mut parse_trailing_qualifier = all_consuming(terminated(
            take_until::<_, _, OracleError<'_>>(needle),
            tag::<_, _, OracleError<'_>>(needle),
        ));
        if let Ok((_, base_lower)) = parse_trailing_qualifier.parse(tp.lower) {
            if !base_lower.trim().is_empty() {
                let base = lower_subslice_to_original(&tp, base_lower)?.trim();
                return parse_continuous_subject_filter(base).map(|f| add_property(f, prop));
            }
        }
    }

    // CR 509.1g / CR 509.1h: strip a trailing "blocking or blocked by ~" /
    // "blocking or blocked by this creature" combat-relationship qualifier and
    // attach a source-anchored `FilterProp::CombatRelation` (Alms Beast,
    // "Creatures blocking or blocked by ~ have lifelink"). The self-reference is
    // normalized to "~"; "this creature" is accepted for the literal phrasing.
    // Mirrors the already-parsed target-relative form in `oracle_target`;
    // `game::filter` evaluates `CombatRelationSubject::Source` authoritatively,
    // so this is a grammar-only seam.
    for needle in [
        " blocking or blocked by ~",
        " blocking or blocked by this creature",
    ] {
        let mut parse_trailing_qualifier = all_consuming(terminated(
            take_until::<_, _, OracleError<'_>>(needle),
            tag::<_, _, OracleError<'_>>(needle),
        ));
        if let Ok((_, base_lower)) = parse_trailing_qualifier.parse(tp.lower) {
            if !base_lower.trim().is_empty() {
                let base = lower_subslice_to_original(&tp, base_lower)?.trim();
                return parse_continuous_subject_filter(base).map(|f| {
                    add_property(
                        f,
                        FilterProp::CombatRelation {
                            relation: CombatRelation::BlockingOrBlockedBy,
                            subject: CombatRelationSubject::Source,
                        },
                    )
                });
            }
        }
    }

    if let Some(filter) = parse_shared_controller_compound_subject_filter(&tp) {
        return Some(filter);
    }

    // CR 607.2d / CR 607.2m (by analogy): "<type> controlled by players who last
    // chose <label>" — the object anthem subject keyed on the controller's
    // durable anchor (Two Streams Facility's "Creatures controlled by players
    // who last chose red waterfall get +2/+0 and have haste"). Runs before the
    // "X and Y" compound split so the "who last chose ..." tail is not misread.
    if let Some(filter) = parse_controlled_by_anchor_subject_filter(&tp) {
        return Some(filter);
    }

    if let Some(filter) = parse_controlled_compound_continuous_subject_filter(&tp) {
        return Some(filter);
    }

    // CR 611.3a: bare tribal compound "<subtype> creatures and [other] <subtype>
    // creatures" (Verdeloth the Ancient) — no controller suffix.
    if let Some(filter) = parse_bare_compound_subtype_subject_filter(&tp) {
        return Some(filter);
    }

    if let Some(rest_tp) = nom_tag_tp(&tp, "other ") {
        return parse_continuous_subject_filter(rest_tp.original.trim()).map(add_another_filter);
    }

    // CR 105.4 / CR 205.3m: "Creatures [you control] of the chosen color/type [opponent control]"
    // Handle "of the chosen color/type" qualifiers that appear in creature subject phrases.
    if let Some(filter) = parse_chosen_qualifier_subject(&tp) {
        return Some(filter);
    }

    // CR 201.3 / CR 113.6: "<type-phrase> with the chosen name" — the chosen-name
    // name-picker class (Petrified Hamlet, Cheering Fanatic, Disruptor Flute, ...).
    // The type prefix selects the object class; `HasChosenName` restricts it to
    // objects whose name matches the source's `ChosenAttribute::CardName` (bound
    // by a preceding `Effect::Choose { CardName, persist: true }`).
    if let Ok((_, (type_part, _))) =
        nom_primitives::split_once_on(tp.lower, " with the chosen name")
    {
        let type_part_original = tp.original[..type_part.len()].trim();
        let (type_filter, type_rest) = parse_type_phrase(type_part_original);
        if type_rest.trim().is_empty() && !matches!(type_filter, TargetFilter::Any) {
            return Some(TargetFilter::And {
                filters: vec![type_filter, TargetFilter::HasChosenName],
            });
        }
    }

    // CR 205.3m: "creature [you control] that's a Wolf or a Werewolf" — relative
    // clause restricting a base creature/permanent phrase to a subtype disjunction.
    // Split on " that's a " / " that is a ", parse the base phrase (with controller
    // suffix) via recursive call, then compose with the subtype filter.
    if let Some(filter) = parse_thats_a_subject_filter(trimmed, &lower) {
        return Some(filter);
    }

    if let Some(filter) = parse_modified_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    if let Some(filter) = parse_typed_you_control_subject_filter(&tp) {
        return Some(filter);
    }

    // CR 903.3d: "commander(s) you control" / "commander(s)" subject phrase.
    // Must run before parse_creature_subject_filter because the bare token
    // "Commanders" otherwise falls into the capitalized-subtype fallback and
    // emits a bogus `Subtype: "Commander"` (Commander is not an MTG subtype).
    if let Some(filter) = parse_commander_subject_filter(trimmed) {
        return Some(filter);
    }

    if let Some(filter) = parse_creature_subject_filter(trimmed) {
        return Some(filter);
    }

    // NOTE: deliberately NOT wiring `parse_owned_off_battlefield_subject_filter`
    // in here as a general fallback. It's safe under
    // `parse_spells_have_keyword`'s `CastWithKeyword` mode (casting is checked
    // directly against the zone a card sits in, so an off-battlefield filter is
    // meaningful there), but every OTHER caller of this function feeds a
    // `Continuous` static whose modifications apply through the Layer system —
    // which only iterates battlefield objects. `game/off_zone_characteristics.rs`
    // is the sole off-battlefield continuous-effect path, and its
    // `supports_off_zone_keyword_query` allowlist is keyword-only (`AddKeyword`
    // and its siblings) — it has no notion of `AddSubtype`/`AddType`/etc. Folding
    // an off-battlefield conjunct into a general filter here would let a
    // type-changing static (e.g. Dune Chanter's "land cards you own that aren't
    // on the battlefield are Deserts...") claim an `affected` scope the engine
    // cannot actually realize, which is worse than leaving the line
    // `Unimplemented` — it would silently under-deliver while looking parsed.
    // Revisit once an off-zone characteristics path exists for non-keyword
    // modifications.

    let (filter, rest) = parse_type_phrase(trimmed);
    if rest.trim().is_empty() {
        // CR 109.2: a bare "spell(s)" head noun in a static-ability subject
        // ("permanent spells you control", Secret Arcade) means the affected
        // objects sit on the stack, not the battlefield — the same rule
        // `parse_target_with_ctx` already applies to targeting noun phrases.
        // `parse_type_phrase` has no notion of this (it's a bare type-phrase
        // grammar shared by many non-targeting callers), so without this the
        // "spell(s)" word is silently swallowed and the filter collapses to a
        // battlefield-permanent filter that never reaches the stack.
        return Some(scope_target_spell_phrase(filter, &lower));
    }

    parse_rule_static_subject_filter(trimmed)
}

/// CR 109.4 + CR 109.5: "`<type>` cards you own that aren't on the battlefield"
/// — an off-battlefield-scoped subject naming cards by ownership rather than
/// control, since CR 109.4 objects that are neither on the stack nor the
/// battlefield have no controller, so CR 109.5 falls back to reading "you"/
/// "your" as the object's owner instead. Resolves the leading type phrase and
/// attaches `ControllerRef::You` (read as "owned by you" off the battlefield)
/// plus `FilterProp::InAnyZone` over every non-battlefield, non-stack zone this
/// class of card names (hand/graveyard/exile/command — no printed card in this
/// shape also reaches into the hidden library zone).
///
/// Extracted from [`super::keyword_grant::parse_spells_have_keyword`]'s Pattern
/// 2 (Leyline of Anticipation: "Creature cards you own that aren't on the
/// battlefield have flash.") as a standalone, fully-anchored subject parser —
/// still used only by that caller today. `CastWithKeyword` statics are checked
/// directly against a card's actual zone (casting inherently happens from
/// off-battlefield zones), so this filter's off-battlefield scope is realized
/// there; `Continuous`-mode statics are not (see the caller-side note in
/// [`parse_continuous_subject_filter`]), so do NOT wire this into that
/// function's general dispatch chain until an off-zone characteristics path
/// exists for non-keyword modifications.
///
/// All-consuming: the ENTIRE (trimmed, period-stripped) subject must be exactly
/// `<type phrase>` + `"cards you own that aren't on the battlefield"`, with
/// nothing before the type phrase and nothing after the fixed suffix. A
/// partial/substring match (trailing qualifier, leading noise) declines rather
/// than silently truncating.
pub(crate) fn parse_owned_off_battlefield_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim().trim_end_matches('.');
    let lower = trimmed.to_lowercase();
    let (prefix, remainder) = nom_primitives::scan_split_at_phrase(&lower, |i| {
        tag::<_, _, OracleError<'_>>("cards you own that aren't on the battlefield").parse(i)
    })?;
    all_consuming(tag::<_, _, OracleError<'_>>(
        "cards you own that aren't on the battlefield",
    ))
    .parse(remainder)
    .ok()?;
    let type_part = &trimmed[..prefix.len()];
    let (base_filter, rest) = parse_type_phrase(type_part);
    if !rest.trim().is_empty() {
        return None;
    }
    // A bare, untyped "cards you own that aren't on the battlefield" (empty
    // type_part) doesn't resolve to a `Typed` filter — pass it through unscoped
    // rather than force a controller/zone property onto a non-Typed variant.
    // No printed card in this class omits the type qualifier, so this arm is
    // unreached in practice; kept to preserve the pre-extraction behavior of
    // `parse_spells_have_keyword`'s Pattern 2 exactly.
    match base_filter {
        TargetFilter::Typed(mut typed) => {
            typed = typed.controller(ControllerRef::You);
            typed.properties.push(FilterProp::InAnyZone {
                zones: vec![Zone::Hand, Zone::Graveyard, Zone::Exile, Zone::Command],
            });
            Some(TargetFilter::Typed(typed))
        }
        other => Some(other),
    }
}

/// CR 109.5: Keep the subject descriptor paired with its "you control" suffix
/// so controller-scoped subjects can lower to the source controller.
pub(crate) fn parse_subject_suffix<'a>(
    subject: &TextPair<'a>,
    suffix: &str,
) -> Option<TextPair<'a>> {
    let (_, descriptor_lower) = all_consuming(terminated(
        take_until::<_, _, OracleError<'_>>(suffix),
        tag::<_, _, OracleError<'_>>(suffix),
    ))
    .parse(subject.lower)
    .ok()?;
    Some(TextPair::new(
        &subject.original[..descriptor_lower.len()],
        descriptor_lower,
    ))
}

/// CR 111.1 + CR 111.6 + CR 109.5: "[creature ]token(s) you control" — token-ness is an
/// object property (CR 111.1), never a card type/subtype, and a token can be any card type
/// (CR 111.6), so "tokens you control" spans Treasure/Clue/Food/creature tokens alike.
pub(crate) fn parse_token_you_control_descriptor(
    descriptor: &TextPair<'_>,
) -> Option<TargetFilter> {
    // Token-ness derived from an optional "creature " prefix (CR 111.6: a token may be any
    // card type); the bare form matches any token permanent. The passed creature_subject
    // flag is intentionally not consulted — the prefix is the sole creature discriminator here.
    let creature_prefixed = nom_tag_tp(descriptor, "creature ");
    let core = creature_prefixed.as_ref().unwrap_or(descriptor);
    if !matches!(core.lower, "token" | "tokens") {
        return None;
    }
    let base = if creature_prefixed.is_some() {
        TypedFilter::creature()
    } else {
        TypedFilter::permanent()
    };
    Some(TargetFilter::Typed(
        base.properties(vec![FilterProp::Token])
            .controller(ControllerRef::You),
    ))
}

/// CR 109.5 + CR 205.3 + CR 205.4a: Controller-scoped subject descriptors
/// may name object types, colors, subtypes, or supertypes controlled by the
/// source's controller.
pub(crate) fn typed_you_control_descriptor_filter(
    descriptor: TextPair<'_>,
    creature_subject: bool,
) -> Option<TargetFilter> {
    if descriptor_is_negation(descriptor.original) || descriptor_is_supertype(descriptor.original) {
        return None;
    }

    if let Some(filter) = parse_token_you_control_descriptor(&descriptor) {
        return Some(filter);
    }

    if matches!(descriptor.lower, "creature" | "creatures") {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ));
    }

    if let Some(color) = parse_named_color(descriptor.original) {
        return Some(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::HasColor { color }]),
        ));
    }

    if let Some(filter) = try_parse_compound_subtypes(descriptor.original, &[], false) {
        return Some(filter);
    }

    let singular_core_descriptor = strip_one_trailing_ascii_s(descriptor.lower);
    if let Some(core_type) = try_parse_core_type_descriptor(descriptor.lower)
        .or_else(|| try_parse_core_type_descriptor(singular_core_descriptor))
    {
        let typed = if creature_subject {
            TypedFilter::creature().with_type(core_type)
        } else {
            TypedFilter::new(core_type)
        };
        return Some(TargetFilter::Typed(typed.controller(ControllerRef::You)));
    }

    if is_capitalized_words(descriptor.original) {
        let subtype_name = parse_subtype(descriptor.original)
            .map(|(canonical, _)| canonical)
            .unwrap_or_else(|| descriptor.original.to_string());
        return Some(TargetFilter::Typed(
            typed_filter_for_subtype(&subtype_name).controller(ControllerRef::You),
        ));
    }

    None
}

/// CR 205.2a: Core card type descriptors may appear in singular or regular
/// plural form in Oracle subject phrases; remove at most one ASCII plural `s`
/// for core-type lookup only.
pub(crate) fn strip_one_trailing_ascii_s(text: &str) -> &str {
    if text.as_bytes().last() == Some(&b's') {
        &text[..text.len() - 1]
    } else {
        text
    }
}

/// CR 205.3m: Parse "creature [you control] that's a Wolf or a Werewolf" subjects.
/// Splits on "that's a " / "that is a ", parses the base phrase (with controller/zone
/// suffix) via `parse_type_phrase`, then parses a comma/or/and-separated subtype list
/// and composes with `TargetFilter::And`.
pub(crate) fn parse_thats_a_subject_filter(text: &str, lower: &str) -> Option<TargetFilter> {
    type VE<'a> = OracleError<'a>;

    let (before, subtype_lower, _) = nom_primitives::scan_preceded(lower, |i| {
        preceded(
            alt((tag::<_, _, VE>("that's a "), tag::<_, _, VE>("that is a "))),
            nom::combinator::rest,
        )
        .parse(i)
    })?;
    let base_text = text[..before.len()].trim();
    let subtype_text = text[text.len() - subtype_lower.len()..].trim();

    let (base_filter, base_rest) = parse_type_phrase(base_text);
    if !base_rest.trim().is_empty() || matches!(base_filter, TargetFilter::Any) {
        return None;
    }

    let subtype_filter = parse_subtype_or_list(subtype_text)?;

    Some(TargetFilter::And {
        filters: vec![base_filter, subtype_filter],
    })
}

/// CR 205.3m: Parse a comma/or/and/and-or-separated list of capitalized subtypes.
/// Handles: "Wolf or a Werewolf", "Barbarian, a Warrior, or a Berserker",
/// "Cleric, Rogue, Warrior, and/or Wizard", "Cat, Elemental, Nightmare, Dinosaur, or Beast".
/// Returns `TargetFilter::Or` for multiple subtypes, single `TargetFilter::Typed` for one.
pub(crate) fn parse_subtype_or_list(input: &str) -> Option<TargetFilter> {
    parse_subtype_or_list_with_word_parser(input, parse_subtype_word_capitalized)
}

/// CR 205.3m: Lowercase subtype list prefix plus the unconsumed suffix.
pub(crate) fn parse_subtype_or_list_insensitive_prefix(
    input: &str,
) -> Option<(TargetFilter, &str)> {
    parse_subtype_or_list_prefix_with_word_parser(input, parse_subtype_word_any_case)
}

fn parse_subtype_word_capitalized(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
    use nom::bytes::complete::take_while1;
    let (rest, word) = take_while1(|c: char| c.is_alphabetic() || c == '-').parse(input)?;
    if !word.chars().next().is_some_and(|c| c.is_uppercase()) {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Fail,
        )));
    }
    Ok((rest, word))
}

fn parse_subtype_word_any_case(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
    use nom::bytes::complete::take_while1;
    take_while1(|c: char| c.is_alphabetic() || c == '-').parse(input)
}

fn parse_subtype_or_list_with_word_parser(
    input: &str,
    parse_subtype_word: fn(&str) -> nom::IResult<&str, &str, OracleError<'_>>,
) -> Option<TargetFilter> {
    let (filter, rest) = parse_subtype_or_list_prefix_with_word_parser(input, parse_subtype_word)?;
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('.') {
        return None;
    }
    Some(filter)
}

fn parse_subtype_or_list_prefix_with_word_parser(
    input: &str,
    parse_subtype_word: fn(&str) -> nom::IResult<&str, &str, OracleError<'_>>,
) -> Option<(TargetFilter, &str)> {
    fn parse_list_separator(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
        alt((
            tag(", and/or a "),
            tag(", and/or "),
            tag(", or a "),
            tag(", and a "),
            tag(", or "),
            tag(", and "),
            tag(", a "),
            tag(", "),
            tag(" and/or a "),
            tag(" and/or "),
            tag(" or a "),
            tag(" and a "),
            tag(" or "),
            tag(" and "),
        ))
        .parse(input)
    }

    let (rest, words): (&str, Vec<&str>) =
        separated_list1(parse_list_separator, parse_subtype_word)
            .parse(input)
            .ok()?;
    let filters: Vec<TargetFilter> = words
        .iter()
        .map(|w| {
            let canonical = parse_subtype(w)
                .map(|(c, _)| c)
                .unwrap_or_else(|| w.to_string());
            TargetFilter::Typed(typed_filter_for_subtype(&canonical))
        })
        .collect();
    if filters.len() == 1 {
        filters.into_iter().next().map(|filter| (filter, rest))
    } else {
        Some((TargetFilter::Or { filters }, rest))
    }
}

/// Try to strip a leading "with [counter] counter(s) on it/them" clause from `text`,
/// returning the `FilterProp` and the remaining text after the clause.
/// CR 613.1 + CR 613.7: Used to parse conditional static keyword grants in layer 6.
pub(crate) fn strip_counter_condition_prefix(text: &str) -> Option<(FilterProp, &str)> {
    let lower = text.to_lowercase();
    nom_tag_lower(&lower, &lower, "with ")?;
    // parse_counter_suffix expects optional leading whitespace before "with"
    let (prop, consumed) = parse_counter_suffix(&lower)?;
    Some((prop, text[consumed..].trim_start()))
}

pub(crate) fn parse_modified_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let lower = subject.to_lowercase();
    let tp = TextPair::new(subject, &lower);
    if tp.lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }
    if tp.lower == "equipped creatures you control" {
        return Some(attachment_creatures_you_control_filter(
            AttachmentKind::Equipment,
        ));
    }

    let controlled_patterns = [
        ("tapped creatures you control", FilterProp::Tapped),
        (
            "attacking creatures you control",
            FilterProp::Attacking { defender: None },
        ),
        // CR 700.9: "modified creatures you control" — permanents with
        // counters, equipped, or enchanted by own-controlled Aura.
        ("modified creatures you control", FilterProp::Modified),
        ("modified creature you control", FilterProp::Modified),
    ];

    for (pattern, property) in controlled_patterns {
        if tp.lower == pattern {
            return Some(TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![property]),
            ));
        }
    }

    if tp.lower == "attacking creatures" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::Attacking { defender: None }]),
        ));
    }

    // CR 700.9 + CR 700.4: "modified creature(s)" and "other modified
    // creature(s) [you control]" — includes "Another" variant for triggers
    // that exclude the source (Ondu Knotmaster, Golden-Tail Trainer).
    let controller_suffix_patterns: [(&str, Option<ControllerRef>); 3] = [
        (" you control", Some(ControllerRef::You)),
        (" your opponents control", Some(ControllerRef::Opponent)),
        ("", None),
    ];
    for (suffix, controller) in controller_suffix_patterns {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        let Some(core) = tp.lower.strip_suffix(suffix) else {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            continue;
        };
        for (phrase, has_other) in [
            ("other modified creatures", true),
            ("other modified creature", true),
            ("modified creatures", false),
            ("modified creature", false),
        ] {
            if core == phrase {
                let mut props = vec![FilterProp::Modified];
                if has_other {
                    props.push(FilterProp::Another);
                }
                let mut typed = TypedFilter::creature().properties(props);
                if let Some(c) = controller {
                    typed = typed.controller(c);
                }
                return Some(TargetFilter::Typed(typed));
            }
        }
    }

    None
}

pub(crate) fn parse_creatures_you_control_that_clause<'a>(
    original: &'a str,
    lower: &str,
    is_other: bool,
) -> Option<(TargetFilter, &'a str)> {
    let (mut properties, consumed) = parse_that_clause_suffix(lower, None)?;
    if is_other {
        properties.push(FilterProp::Another);
    }
    Some((
        TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(properties),
        ),
        original[consumed..].trim_start(),
    ))
}

pub(crate) fn parse_attachment_creatures_you_control_descriptor(
    descriptor: &str,
) -> Option<TargetFilter> {
    // CR 303.4b + CR 301.5a: plural/global "enchanted/equipped creatures you
    // control" is not source-relative. It means creatures with a qualifying
    // Aura/Equipment attached, unlike Aura/Equipment text such as "Enchanted
    // creature gets ..." where `EnchantedBy`/`EquippedBy` intentionally points
    // at the static ability's source.
    let kind = if descriptor.eq_ignore_ascii_case("enchanted") {
        AttachmentKind::Aura
    } else if descriptor.eq_ignore_ascii_case("equipped") {
        AttachmentKind::Equipment
    } else {
        return None;
    };

    Some(attachment_creatures_you_control_filter(kind))
}

pub(crate) fn attachment_creatures_you_control_filter(kind: AttachmentKind) -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::HasAttachment {
                kind,
                controller: None,
                exclude_source: crate::types::ability::SourceExclusion::Include,
            }]),
    )
}

/// CR 903.3d: Parse "commander(s) [you control | your opponents control]"
/// subject phrases into a `TargetFilter` carrying `FilterProp::IsCommander`.
/// "Commander" is the deck-construction designation (CR 903.3) — it is NOT
/// an MTG subtype, so it must not be routed through `parse_subtype` or the
/// capitalized-subtype fallback (which would synthesize `Subtype("Commander")`
/// and match zero objects at runtime).
///
/// Covers Codsworth, Falthis, Anara, Champions of Archery, Vexilus Praetor,
/// Guardian Augmenter, The Dilu Horse, Dancer's Chakrams ("other commanders
/// you control"), and analogous "[other] commander(s) [you control | your
/// opponents control]" subject phrases.
pub(crate) fn parse_commander_subject_filter(subject: &str) -> Option<TargetFilter> {
    let (filter, rest) = parse_commander_subject_filter_prefix(subject.trim())?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(filter)
}

/// CR 903.3 + CR 903.3d: Parse a commander subject prefix, returning the
/// unconsumed text for trigger/event parsers that need to continue at the verb.
pub(crate) fn parse_commander_subject_filter_prefix(subject: &str) -> Option<(TargetFilter, &str)> {
    type VE<'a> = OracleError<'a>;
    let lower = subject.to_lowercase();
    let i = lower.as_str();

    // Possessive "your commander(s)" is owner-scoped: it refers to the
    // commander's designation for the evaluating player, not just any
    // commander currently controlled by that player.
    let (i, possessive_your) = opt(tag::<_, _, VE>("your ")).parse(i).ok()?;

    // Optional leading "other " — emits FilterProp::Another.
    let (i, other) = opt(tag::<_, _, VE>("other ")).parse(i).ok()?;
    let has_other = other.is_some();

    // The bare commander token (singular or plural), optionally as an adjective
    // on a creature subject ("commander creatures").
    let (i, _) = alt((tag::<_, _, VE>("commanders"), tag::<_, _, VE>("commander")))
        .parse(i)
        .ok()?;
    let (i, is_creature_subject) = alt((
        value(true, tag::<_, _, VE>(" creatures")),
        value(true, tag::<_, _, VE>(" creature")),
        value(false, tag::<_, _, VE>("")),
    ))
    .parse(i)
    .ok()?;

    // Optional ownership/controller suffix. Ownership composes as a property
    // because CR 108.3 ownership and CR 108.4 control are distinct axes.
    let (i, (controller, owned)) = alt((
        value(
            (
                Some(ControllerRef::You),
                Some(FilterProp::Owned {
                    controller: ControllerRef::You,
                }),
            ),
            tag::<_, _, VE>(" you own and control"),
        ),
        value((Some(ControllerRef::You), None), tag(" you control")),
        value(
            (Some(ControllerRef::Opponent), None),
            tag(" your opponents control"),
        ),
        value(
            (
                None,
                Some(FilterProp::Owned {
                    controller: ControllerRef::You,
                }),
            ),
            tag(" you own"),
        ),
        value((None, None), tag("")),
    ))
    .parse(i)
    .ok()?;

    let mut props = Vec::new();
    if possessive_your.is_some() {
        props.push(FilterProp::Owned {
            controller: ControllerRef::You,
        });
    }
    props.push(FilterProp::IsCommander);
    if has_other {
        props.push(FilterProp::Another);
    }
    if let Some(owned) = owned {
        props.push(owned);
    }
    let mut typed = if is_creature_subject {
        TypedFilter::creature().properties(props)
    } else if possessive_your.is_some() {
        TypedFilter::default().properties(props)
    } else {
        TypedFilter::permanent().properties(props)
    };
    if let Some(c) = controller {
        typed = typed.controller(c);
    }

    let consumed = lower.len() - i.len();
    Some((TargetFilter::Typed(typed), &subject[consumed..]))
}

/// CR 205.1a / CR 205.3 / CR 111.1: Returns true when `descriptor` is a
/// `non`/`non-` negation adjective (e.g. "Nontoken", "Nonland", "noncreature").
/// The negation targets a card type (CR 205.1a), a subtype (CR 205.3), or
/// token object identity (CR 111.1) — never a supertype.
///
/// Subject-filter parsers strip the trailing `" creatures"` to obtain a bare
/// descriptor and then route capitalized descriptors through a
/// `subtype`-fabricating fallback. A sentence-leading "Nontoken" is
/// capitalized but is NOT a subtype — it is a type/token-identity negation.
/// This guard lets such descriptors fall through to `parse_type_phrase`, whose
/// negation loop maps the negated word to `FilterProp`/`TypeFilter::Non` via
/// `classify_negation` (the single authority).
///
/// The detection is made by *trying the nom negation tag* — never `==` /
/// `contains` — and is word-boundary-anchored: the guard fires only when
/// `non`/`non-` is the genuine head of a complete negation descriptor token
/// (a non-empty negated word follows the prefix), so it cannot match the
/// prefix of an unrelated subtype word.
pub(crate) fn descriptor_is_negation(descriptor: &str) -> bool {
    let lower = descriptor.to_lowercase();
    let Ok((after_non, _)) =
        alt((tag::<_, _, OracleError<'_>>("non-"), tag("non"))).parse(lower.as_str())
    else {
        return false;
    };
    after_non.chars().next().is_some_and(|c| !c.is_whitespace())
}

/// CR 205.4a: Supertype descriptors include legendary, basic, snow, and world;
/// parse supported supertype words through the shared target combinator so they
/// fall through to `parse_type_phrase` instead of becoming fabricated subtypes.
pub(crate) fn descriptor_is_supertype(descriptor: &str) -> bool {
    let lower = descriptor.to_lowercase();
    let is_supertype = all_consuming(nom_target::parse_supertype_word)
        .parse(lower.as_str())
        .is_ok();
    is_supertype
}

/// Nom-backed helper: split a subject string into (descriptor_core, controller)
/// by trying to parse a trailing controller suffix. Only accepts the controller
/// scopes that this static subject seam can actually resolve:
///
/// - `ControllerRef::You` — "you control"
/// - `ControllerRef::Opponent` — "your opponents control" / "you don't control"
/// - `ControllerRef::EnchantedPlayer` — "enchanted player controls" (CR 303.4b)
///
/// `TargetPlayer` and `DefendingPlayer` are deliberately excluded because this
/// call site builds a continuous static `TargetFilter` with no companion
/// target-player authority or combat context.
///
/// Uses nom `alt`/`tag`/`value` combinators so the phrase set is maintained
/// alongside the shared grammar rather than as raw suffix literals.
fn parse_static_controller_suffix(input: &str) -> OracleResult<'_, ControllerRef> {
    alt((
        value(ControllerRef::You, tag("you control")),
        value(ControllerRef::Opponent, tag("your opponents control")),
        value(ControllerRef::Opponent, tag("you don't control")),
        // CR 303.4b + CR 702.5a: "enchanted player controls" — the controller
        // scope is the player the source Aura is attached to.
        value(
            ControllerRef::EnchantedPlayer,
            tag("enchanted player controls"),
        ),
    ))
    .parse(input)
}

/// Strip a trailing controller suffix from a subject string using the
/// restricted nom grammar above. Returns (descriptor_core, Some(controller))
/// on match, or (original, None) if no valid suffix is found.
fn strip_subject_controller_suffix<'a>(
    original: &'a str,
    lower: &str,
) -> (&'a str, Option<ControllerRef>) {
    // Try each space-delimited split point (left to right) and check if the
    // remainder is a complete controller suffix.
    let mut start = 0;
    while let Some(pos) = lower[start..].find(' ') {
        let abs_pos = start + pos;
        let suffix_lower = &lower[abs_pos + 1..];
        if let Ok((rest, ctrl)) = parse_static_controller_suffix(suffix_lower) {
            if rest.is_empty() {
                return (original[..abs_pos].trim(), Some(ctrl));
            }
        }
        start = abs_pos + 1;
    }
    (original, None)
}

/// CR 205.2a + CR 110.1: A bulk card-type / permanent noun — "creature(s)" (the
/// creature card type) or "permanent(s)" (any permanent on the battlefield) —
/// names a type, NOT a creature subtype. Returns the base `TypedFilter` for the
/// noun so the subject parser never fabricates a `Subtype("Permanent")` (which
/// matches no real card) or `Subtype("Creature")`.
pub(crate) fn bulk_type_subject_base(word: &str) -> Option<TypedFilter> {
    if word.eq_ignore_ascii_case("creature") || word.eq_ignore_ascii_case("creatures") {
        Some(TypedFilter::creature())
    } else if word.eq_ignore_ascii_case("permanent") || word.eq_ignore_ascii_case("permanents") {
        Some(TypedFilter::permanent())
    } else {
        None
    }
}

/// Apply the shared subject scope to a base subject filter: the controller
/// suffix ("you control" → `ControllerRef`, CR 109.5) and the leading "Other "
/// exclusion (`FilterProp::Another`).
fn scoped_subject_filter(
    mut typed: TypedFilter,
    controller: Option<ControllerRef>,
    has_other: bool,
) -> TargetFilter {
    if let Some(controller) = controller {
        typed = typed.controller(controller);
    }
    if has_other {
        typed = typed.properties(vec![FilterProp::Another]);
    }
    TargetFilter::Typed(typed)
}

/// Parse one capitalized subtype word (alphabetic characters and hyphens,
/// starting uppercase) — the atom of an Oxford-comma subtype list.
fn parse_capitalized_subtype_word(input: &str) -> OracleResult<'_, &str> {
    use nom::bytes::complete::take_while1;
    use nom::combinator::verify;
    verify(
        take_while1(|c: char| c.is_alphabetic() || c == '-'),
        |w: &str| w.chars().next().is_some_and(|c| c.is_uppercase()),
    )
    .parse(input)
}

/// CR 205.3m + CR 611.3a: Parse an Oxford-comma / conjunction subtype LIST
/// subject — "<Subtype>, <Subtype>, ..., [and|or] <Subtype>" (Raphael, Fiendish
/// Savior "Other Demons, Devils, Imps, and Tieflings you control"; Tiefling
/// Outcasts) — into an `Or` of per-subtype creature filters. Generalizes the
/// two-member compound to any arity: a `split_once(" and ")` split captured only
/// the first and last member, silently dropping every middle comma-separated
/// subtype. Requires two or more members and full consumption, so a non-list
/// subject declines and falls through to the other subject parsers.
pub(crate) fn parse_subtype_list_filter(
    descriptor: &str,
    extra_props: &[FilterProp],
    is_other: bool,
) -> Option<TargetFilter> {
    use nom::multi::separated_list1;
    // Separators longest-first so ", and "/", or " win over ", " and " and ".
    let separator = alt((
        tag::<_, _, OracleError<'_>>(", and "),
        tag(", or "),
        tag(", "),
        tag(" and "),
        tag(" or "),
    ));
    let (rest, words) = separated_list1(separator, parse_capitalized_subtype_word)
        .parse(descriptor.trim())
        .ok()?;
    if !rest.trim().is_empty() || words.len() < 2 {
        return None;
    }
    let mut all_props = extra_props.to_vec();
    if is_other {
        all_props.push(FilterProp::Another);
    }
    // CR 205.3m: normalize each plural member to its canonical singular subtype
    // (Demons→Demon); an unrecognized capitalized word passes through unchanged,
    // matching the prior two-member behavior.
    let filters = words
        .iter()
        .map(|word| {
            let subtype = parse_subtype(word)
                .map(|(canonical, _)| canonical)
                .unwrap_or_else(|| word.to_string());
            TargetFilter::Typed(
                typed_filter_for_subtype(&subtype)
                    .controller(ControllerRef::You)
                    .properties(all_props.clone()),
            )
        })
        .collect();
    Some(TargetFilter::Or { filters })
}

pub(crate) fn parse_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    let tp = TextPair::new(trimmed, &lower);

    // CR 109.5 + CR 303.4b: Split the subject into a descriptor core and an
    // optional controller suffix. Uses `parse_static_controller_suffix`, a
    // restricted nom grammar that only accepts the controller scopes this
    // static seam can resolve (You, Opponent, EnchantedPlayer).
    let (subject_core, controller) = strip_subject_controller_suffix(tp.original, &lower);

    let subject_core_lower = subject_core.to_lowercase();
    let subject_core_tp = TextPair::new(subject_core, &subject_core_lower);
    let (descriptor_text, has_other) =
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        if let Some(rest) = subject_core_tp.original.strip_prefix("Other ") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            (rest.trim(), true)
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        } else if let Some(rest) = subject_core_tp.original.strip_prefix("other ") {
            // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
            (rest.trim(), true)
        } else {
            (subject_core_tp.original.trim(), false)
        };

    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let descriptor = if let Some(prefix) = descriptor_text.strip_suffix(" creatures") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        prefix.trim()
    } else if !descriptor_text.contains(' ') && descriptor_text.to_lowercase().ends_with('s') {
        // CR 205.2a + CR 110.1: a bulk card-type / permanent noun ("creatures",
        // "permanents") names a type, not a creature subtype — checked BEFORE the
        // subtype fallback so "Permanents you control" spans every permanent
        // rather than fabricating a zero-match Subtype("Permanent").
        if let Some(base) = bulk_type_subject_base(descriptor_text) {
            return Some(scoped_subject_filter(base, controller, has_other));
        }
        // CR 205.3m: Use parse_subtype for irregular plurals (Elves→Elf, Dwarves→Dwarf)
        if let Some((canonical, _)) = parse_subtype(descriptor_text) {
            return Some(scoped_subject_filter(
                TypedFilter::creature().subtype(canonical),
                controller,
                has_other,
            ));
        }
        descriptor_text.trim_end_matches('s').trim()
    } else {
        return None;
    };

    // CR 205.2a + CR 110.1: bare "creature" / "permanent" name a type, not a subtype.
    if let Some(base) = bulk_type_subject_base(descriptor) {
        return Some(scoped_subject_filter(base, controller, has_other));
    }

    if descriptor.is_empty() {
        return None;
    }

    if let Some(color) = parse_named_color(descriptor) {
        let mut typed = TypedFilter::creature().properties(vec![FilterProp::HasColor { color }]);
        if let Some(controller) = controller {
            typed = typed.controller(controller);
        }
        if has_other {
            typed.properties.push(FilterProp::Another);
        }
        return Some(TargetFilter::Typed(typed));
    }

    // CR 111.1 / CR 205.3 / CR 205.4a: A `non`/`non-` negation descriptor
    // (e.g. "Nontoken creatures") or a supertype descriptor (e.g. "Legendary
    // creatures") is NOT a subtype. `is_capitalized_words` below would
    // otherwise fabricate a bogus subtype. Bail so `parse_continuous_subject_filter`
    // falls through to its own `parse_type_phrase` call, whose typed grammar
    // maps these descriptors onto properties.
    if descriptor_is_negation(descriptor) || descriptor_is_supertype(descriptor) {
        return None;
    }

    if is_capitalized_words(descriptor) {
        let subtype = descriptor.to_string();
        let mut typed = TypedFilter::creature().subtype(subtype);
        if let Some(controller) = controller {
            typed = typed.controller(controller);
        }
        if has_other {
            typed.properties.push(FilterProp::Another);
        }
        return Some(TargetFilter::Typed(typed));
    }

    None
}

pub(crate) fn add_another_filter(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(FilterProp::Another);
            TargetFilter::Typed(typed)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(add_another_filter).collect(),
        },
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another])),
            ],
        },
    }
}

/// Add a single `FilterProp` to an existing `TargetFilter`.
pub(crate) fn add_property(filter: TargetFilter, prop: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut typed) => {
            typed.properties.push(prop);
            TargetFilter::Typed(typed)
        }
        other => TargetFilter::And {
            filters: vec![
                other,
                TargetFilter::Typed(TypedFilter::default().properties(vec![prop])),
            ],
        },
    }
}

/// CR 109.5: True when `filter` is anchored to the source's controller via a
/// `ControllerRef::You` constraint (directly or within an Or/And composition).
/// Stricter than `filter_has_source_or_controller_anchor`, which also accepts
/// `Opponent` — "enters with an additional counter" statics are always
/// "you control" scoped, so an opponent anchor must NOT match.
pub(crate) fn filter_is_controller_you(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.controller == Some(ControllerRef::You),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().all(filter_is_controller_you)
        }
        _ => false,
    }
}

pub(crate) fn strip_rule_static_subject<'a>(
    text: &'a str,
    lower: &str,
) -> Option<(TargetFilter, &'a str)> {
    for marker in [
        " doesn't untap during ",
        " doesn't untap during ",
        " don't untap during ",
        " don't untap during ",
        " must attack each combat if able",
        " must attack if able",
        " attacks each combat if able",
        " attack each combat if able",
        " attacks each turn if able",
        " attack each turn if able",
        " must block each combat if able",
        " must block if able",
        " blocks each combat if able",
        " block each combat if able",
        " blocks each turn if able",
        " block each turn if able",
        " can block only creatures with flying",
        // CR 509.1b: Evasion — "<subject> can't be blocked except by <filter>".
        " can't be blocked except by ",
        " can\u{2019}t be blocked except by ",
        // CR 509.1b: Evasion — "<subject> can't be blocked" (Tetsuko Umezawa).
        // Must follow the "except by" needles so the longer form wins.
        " can't be blocked",
        " can\u{2019}t be blocked",
        " has shroud",
        " have shroud",
        " has hexproof",
        " have hexproof",
        " has no maximum hand size",
        " have no maximum hand size",
        " may play an additional land",
        " may play up to ",
        " may look at the top card of your library",
        " loses all abilities",
        " lose all abilities",
    ] {
        let Some(subject_end) = lower.find(marker) else {
            continue;
        };
        let subject = text[..subject_end].trim();
        let predicate = text[subject_end + 1..].trim();
        let affected = parse_rule_static_subject_filter(subject)?;
        return Some((affected, predicate));
    }

    None
}

/// CR 303.4 + CR 301.5: Strip "that is/are/'s enchanted/equipped by <kind> you control"
/// from a subject phrase and return the corresponding `FilterProp`.
fn parse_attachment_relative_clause_nom(input: &str) -> OracleResult<'_, (&str, AttachmentKind)> {
    let (input, before) = take_until(" that").parse(input)?;
    let (input, _) = tag(" that").parse(input)?;
    let (input, _) = opt(alt((tag("'s"), tag(" is"), tag(" are")))).parse(input)?;
    let (input, kind) = alt((
        value(AttachmentKind::Aura, tag(" enchanted by an aura")),
        value(AttachmentKind::Equipment, tag(" equipped by an equipment")),
    ))
    .parse(input)?;
    let (input, _) = tag(" you control").parse(input)?;
    if !input.is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }
    Ok((input, (before.trim_end(), kind)))
}

pub(crate) fn strip_attachment_relative_clause(subject: &str) -> (&str, Option<FilterProp>) {
    let lower = subject.to_lowercase();
    let Ok((rest, (before, kind))) = parse_attachment_relative_clause_nom(&lower) else {
        return (subject, None);
    };
    if !rest.is_empty() {
        return (subject, None);
    }
    let prop = FilterProp::HasAttachment {
        kind,
        controller: Some(ControllerRef::You),
        exclude_source: crate::types::ability::SourceExclusion::Include,
    };
    (&subject[..before.len()], Some(prop))
}

pub(crate) fn merge_filter_prop(filter: TargetFilter, prop: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties.push(prop);
            TargetFilter::Typed(tf)
        }
        other => other,
    }
}

/// CR 607.2d / CR 607.2m (by analogy): canonicalize an anchor label ("green anchor") to the
/// capitalized casing used by `ChoiceType::Labeled`'s option list ("Green
/// anchor"), so the parsed static/filter/effect labels read identically to the
/// choice options. Runtime matching (`player_last_chose_label`) is
/// case-insensitive, so this is a readability/consistency canonicalization, not
/// a correctness dependency. Capitalizes only the first character (anchor labels
/// are "<color> <noun>", matching the printed "Green anchor" / "Red waterfall").
pub(crate) fn canonicalize_anchor_label(label: &str) -> String {
    let trimmed = label.trim().trim_end_matches('.').trim();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub(crate) fn parse_rule_static_subject_filter(subject: &str) -> Option<TargetFilter> {
    let (subject, attachment_prop) = strip_attachment_relative_clause(subject);
    let lower = subject.to_lowercase();
    let tp = TextPair::new(subject, &lower);

    if matches!(tp.lower, "~" | "this" | "it")
        || SELF_REF_PARSE_ONLY_PHRASES.contains(&tp.lower)
        || SELF_REF_TYPE_PHRASES.contains(&tp.lower)
    {
        return Some(TargetFilter::SelfRef);
    }

    if tp.lower == "you" {
        return Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ));
    }

    if matches!(tp.lower, "players" | "each player") {
        return Some(TargetFilter::Player);
    }

    // CR 607.2d / CR 607.2m (by analogy): "[each ]player[s] who last chose <label>"
    // player-scope subject — the durable per-player anchor gate (Two Streams
    // Facility's "Each player who last chose green anchor …"). Combinator strips
    // the optional "each " prefix, then the "player[s] who last chose " head, and
    // canonicalizes the trailing anchor label to match `ChoiceType::Labeled`'s
    // capitalized option casing. Runs AFTER the plain "players"/"each player"
    // arm so it never shadows the un-anchored player scope.
    {
        let cursor = nom_tag_tp(&tp, "each ").unwrap_or(tp);
        if let Some(rest) = nom_tag_tp(&cursor, "players who last chose ")
            .or_else(|| nom_tag_tp(&cursor, "player who last chose "))
        {
            let label = canonicalize_anchor_label(rest.original.trim());
            if !label.is_empty() {
                return Some(TargetFilter::PlayerWhoChoseLabel { label });
            }
        }
    }

    // CR 205.3 + CR 604.1: "All/Each <subtype>" universal-quantifier subject for a
    // rule-static grant (e.g. "All Slivers have shroud"). Strip the quantifier and
    // delegate to parse_type_phrase (mirroring parse_target), so the subtype filter
    // is recognized and the line lands as a top-level continuous static (CR 604.1)
    // instead of a spell-resolution GenericEffect. Runs AFTER the player-scope match
    // above so it never shadows "all players"/"each player".
    if let Some(rest_tp) = nom_tag_tp(&tp, "all ").or_else(|| nom_tag_tp(&tp, "each ")) {
        let (filter, rest) = parse_type_phrase(rest_tp.original);
        if rest.trim().is_empty() {
            return Some(match attachment_prop {
                Some(prop) => merge_filter_prop(filter, prop),
                None => filter,
            });
        }
    }

    if tp.lower == "enchanted creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if tp.lower == "enchanted permanent" {
        return Some(TargetFilter::Typed(
            TypedFilter::permanent().properties(vec![FilterProp::EnchantedBy]),
        ));
    }

    if tp.lower == "equipped creature" {
        return Some(TargetFilter::Typed(
            TypedFilter::creature().properties(vec![FilterProp::EquippedBy]),
        ));
    }

    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() {
        return Some(match attachment_prop {
            Some(prop) => merge_filter_prop(filter, prop),
            None => filter,
        });
    }

    None
}

pub(crate) fn parse_rule_static_predicate(text: &str) -> Option<RuleStaticPredicate> {
    let lower = text.to_lowercase();
    let tp = TextPair::new(text, &lower);

    if let Ok((rest, predicate)) = parse_rule_static_predicate_nom(tp.lower) {
        if rest.trim().is_empty() {
            return Some(predicate);
        }
    }

    if nom_tag_tp(&tp, "doesn't untap during").is_some()
        || nom_tag_tp(&tp, "doesn\u{2019}t untap during").is_some()
        || nom_tag_tp(&tp, "don't untap during").is_some()
        || nom_tag_tp(&tp, "don\u{2019}t untap during").is_some()
    {
        return Some(RuleStaticPredicate::CantUntap);
    }

    // CR 508.1d: A creature that "attacks if able" is a requirement on the declare attackers step.
    if matches!(
        tp.lower,
        "attack each combat if able"
            | "attack each combat if able."
            | "attacks each combat if able"
            | "attacks each combat if able."
            | "attack each turn if able"
            | "attack each turn if able."
            | "attacks each turn if able"
            | "attacks each turn if able."
            | "must attack each combat if able"
            | "must attack each combat if able."
            | "must attack if able"
            | "must attack if able."
    ) {
        return Some(RuleStaticPredicate::MustAttack);
    }

    // CR 509.1c: A creature that "blocks if able" is a requirement on the declare blockers step.
    if matches!(
        tp.lower,
        "block each combat if able"
            | "block each combat if able."
            | "blocks each combat if able"
            | "blocks each combat if able."
            | "block each turn if able"
            | "block each turn if able."
            | "blocks each turn if able"
            | "blocks each turn if able."
            | "must block each combat if able"
            | "must block each combat if able."
            | "must block if able"
            | "must block if able."
    ) {
        return Some(RuleStaticPredicate::MustBlock);
    }

    if matches!(
        tp.lower,
        "can block only creatures with flying" | "can block only creatures with flying."
    ) {
        return Some(RuleStaticPredicate::BlockOnlyCreaturesWithFlying);
    }

    if matches!(
        tp.lower,
        "has shroud" | "has shroud." | "have shroud" | "have shroud."
    ) {
        return Some(RuleStaticPredicate::Shroud);
    }

    // CR 702.11: Hexproof — player-scope hexproof ("You have hexproof.") mirrors
    // the shroud predicate wiring so the static is represented as a player-level
    // rule modification rather than a bogus AddKeyword on empty-typed objects.
    if matches!(
        tp.lower,
        "has hexproof" | "has hexproof." | "have hexproof" | "have hexproof."
    ) {
        return Some(RuleStaticPredicate::Hexproof);
    }

    if nom_tag_tp(&tp, "may look at the top card of your library").is_some() {
        return Some(RuleStaticPredicate::MayLookAtTopOfLibrary);
    }

    if matches!(
        tp.lower,
        "lose all abilities"
            | "lose all abilities."
            | "loses all abilities"
            | "loses all abilities."
    ) {
        return Some(RuleStaticPredicate::LoseAllAbilities);
    }

    if matches!(
        tp.lower,
        "has no maximum hand size"
            | "has no maximum hand size."
            | "have no maximum hand size"
            | "have no maximum hand size."
    ) {
        return Some(RuleStaticPredicate::NoMaximumHandSize);
    }

    if nom_tag_tp(&tp, "may play an additional land").is_some()
        || (nom_tag_tp(&tp, "may play up to ").is_some()
            && nom_primitives::scan_contains(tp.lower, "additional land"))
    {
        return Some(RuleStaticPredicate::MayPlayAdditionalLand);
    }

    None
}

pub(crate) fn parse_rule_static_predicate_nom(
    input: &str,
) -> OracleResult<'_, RuleStaticPredicate> {
    let (rest, predicate) = alt((
        map(
            parse_combat_rule_static_predicate_with_defended_nom,
            |(predicate, _)| predicate,
        ),
        value(
            RuleStaticPredicate::CantBeSacrificed,
            tag("can't be sacrificed"),
        ),
        // NOTE: "can't become untapped" / "can't be untapped" (CR 701.26b) is the
        // BROAD untap prohibition and is NOT a rule-static predicate. It would
        // conflate with `StaticMode::CantUntap`, which is the untap-step-only
        // class (CR 502.3, "doesn't untap during its untap step") enforced only by
        // the untap-step turn-based-action loop — a spell/ability untap would
        // bypass it. The broad form is parsed as an unconditional
        // `ProposedEvent::Untap` prevention by
        // `oracle_replacement::parse_cant_become_untapped_replacement` (mirroring
        // CR 122.1d stun counters), so every untap path consults it.
        value(
            RuleStaticPredicate::LoseAllAbilities,
            alt((tag("loses all abilities"), tag("lose all abilities"))),
        ),
    ))
    .parse(input)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    Ok((rest, predicate))
}

/// Combat-rule predicate plus optional CR 508.1b + CR 508.1c defended scope
/// (`CantAttack` only).
pub(crate) fn parse_combat_rule_static_predicate_with_defended_nom(
    input: &str,
) -> OracleResult<
    '_,
    (
        RuleStaticPredicate,
        Option<crate::types::triggers::AttackTargetFilter>,
    ),
> {
    alt((
        value(
            (RuleStaticPredicate::CantAttackOrBlock, None),
            tag("can't attack or block"),
        ),
        map(parse_cant_attack_rule_static_predicate_nom, |defended| {
            (RuleStaticPredicate::CantAttack, defended)
        }),
        value((RuleStaticPredicate::CantBlock, None), tag("can't block")),
        value(
            (RuleStaticPredicate::CantCrew, None),
            (tag("can't crew"), opt(preceded(space1, tag("vehicles")))),
        ),
        value(
            (RuleStaticPredicate::MustAttack, None),
            alt((
                tag("attacks each combat if able"),
                tag("attack each combat if able"),
                tag("attacks each turn if able"),
                tag("attack each turn if able"),
                tag("must attack each combat if able"),
                tag("must attack if able"),
            )),
        ),
        value(
            (RuleStaticPredicate::MustBlock, None),
            alt((
                tag("blocks each combat if able"),
                tag("block each combat if able"),
                tag("blocks each turn if able"),
                tag("block each turn if able"),
                tag("must block each combat if able"),
                tag("must block if able"),
            )),
        ),
        value(
            (RuleStaticPredicate::MustBeBlocked, None),
            alt((
                tag("must be blocked each combat if able"),
                tag("must be blocked if able"),
            )),
        ),
        value(
            (RuleStaticPredicate::Goaded, None),
            alt((tag("is goaded"), tag("are goaded"))),
        ),
    ))
    .parse(input)
}

pub(crate) fn parse_rule_static_tail_predicate_nom(
    input: &str,
) -> OracleResult<
    '_,
    (
        RuleStaticPredicate,
        Option<crate::types::triggers::AttackTargetFilter>,
    ),
> {
    alt((
        map(
            parse_combat_rule_static_predicate_with_defended_nom,
            |(predicate, defended)| (predicate, defended),
        ),
        map(parse_rule_static_predicate_nom, |predicate| {
            (predicate, None)
        }),
        map(value(RuleStaticPredicate::CantBlock, tag("block")), |p| {
            (p, None)
        }),
        map(
            value(
                RuleStaticPredicate::CantCrew,
                (tag("crew"), opt(preceded(space1, tag("vehicles")))),
            ),
            |p| (p, None),
        ),
        map(
            value(
                RuleStaticPredicate::CantBeActivated,
                alt((
                    tag("have its activated abilities activated"),
                    tag("have their activated abilities activated"),
                )),
            ),
            |p| (p, None),
        ),
    ))
    .parse(input)
}

pub(crate) fn parse_rule_static_tail_predicates(
    rest: &str,
) -> Option<
    Vec<(
        RuleStaticPredicate,
        Option<crate::types::triggers::AttackTargetFilter>,
    )>,
> {
    let mut remaining = rest;
    let mut predicates = Vec::new();

    loop {
        let trimmed = remaining.trim();
        if trimmed.is_empty() || trimmed == "." {
            return Some(predicates);
        }
        let (after_separator, _) = parse_rule_static_separator_nom(trimmed).ok()?;
        let (after_predicate, (predicate, defended)) =
            parse_rule_static_tail_predicate_nom(after_separator).ok()?;
        predicates.push((predicate, defended));
        remaining = after_predicate;
    }
}

/// Optional attack-target scope after "can't attack" (CR 508.1b + CR 508.1c).
pub(crate) fn parse_cant_attack_defended_scope_nom(
    input: &str,
) -> OracleResult<'_, Option<crate::types::triggers::AttackTargetFilter>> {
    use crate::types::triggers::AttackTargetFilter;
    // CR 508.1c + CR 310.5: " you or permanents you control" defends battles too,
    // so it is a distinct filter from " you or planeswalkers you control". Both
    // longer phrases precede the bare " you" (nom `alt` is leftmost-match).
    opt(alt((
        value(
            AttackTargetFilter::PlayerOrPermanents,
            tag(" you or permanents you control"),
        ),
        value(
            AttackTargetFilter::PlayerOrPlaneswalker,
            tag(" you or planeswalkers you control"),
        ),
        value(AttackTargetFilter::Player, tag(" you")),
    )))
    .parse(input)
}

pub(crate) fn parse_cant_attack_rule_static_predicate_nom(
    input: &str,
) -> OracleResult<'_, Option<crate::types::triggers::AttackTargetFilter>> {
    use crate::types::triggers::AttackTargetFilter;

    let (rest, _) = tag("can't attack").parse(input)?;
    let (rest, owner_restriction) = opt(preceded(
        space1,
        alt((
            value(
                AttackTargetFilter::OwnerOrPlaneswalker,
                tag("its owner or planeswalkers its owner controls"),
            ),
            value(AttackTargetFilter::Owner, tag("its owner")),
        )),
    ))
    .parse(rest)?;
    let (rest, a_player) = opt(preceded(space1, tag("a player"))).parse(rest)?;
    let (rest, defended) = parse_cant_attack_defended_scope_nom(rest)?;
    let defended = if let Some(owner_restriction) = owner_restriction {
        Some(owner_restriction)
    } else if a_player.is_some() {
        Some(AttackTargetFilter::Player)
    } else {
        defended
    };
    Ok((rest, defended))
}
