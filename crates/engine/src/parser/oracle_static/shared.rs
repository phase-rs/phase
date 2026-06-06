// CR 604 / CR 613 — shared static parser infrastructure.

#[allow(unused_imports)]
use super::prelude::*;
#[allow(unused_imports)]
use super::support::*;

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
    let condition_tp = TextPair::new(&split.condition_text, &condition_lower);
    let affected = parse_attached_subject_is_legendary(&condition_tp)?;

    let effect_lower = split.effect_text.to_lowercase();
    let effect_tp = TextPair::new(&split.effect_text, &effect_lower);
    let predicate = nom_tag_tp(&effect_tp, "it ").or_else(|| nom_tag_tp(&effect_tp, "they "))?;

    parse_continuous_gets_has(predicate.original, affected, description)
}

pub(crate) fn parse_attached_subject_is_legendary(
    condition: &TextPair<'_>,
) -> Option<TargetFilter> {
    let (rest, attachment_prop) = if let Some(rest) = nom_tag_tp(condition, "equipped ") {
        (rest, FilterProp::EquippedBy)
    } else {
        (
            nom_tag_tp(condition, "enchanted ")?,
            FilterProp::EnchantedBy,
        )
    };
    let rest = nom_tag_tp(&rest, "creature is legendary")?;
    if !rest.original.trim().is_empty() {
        return None;
    }

    Some(TargetFilter::Typed(TypedFilter::creature().properties(
        vec![
            attachment_prop,
            FilterProp::HasSupertype {
                value: Supertype::Legendary,
            },
        ],
    )))
}

pub(crate) fn target_filter_is_your_graveyard(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => {
            tf.controller == Some(ControllerRef::You)
                && tf.properties.iter().any(|prop| {
                    matches!(
                        prop,
                        FilterProp::InZone {
                            zone: Zone::Graveyard
                        }
                    )
                })
        }
        TargetFilter::Or { filters } => filters.iter().all(target_filter_is_your_graveyard),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraveyardGrantedKeywordKind {
    Flashback,
    Escape,
}

impl GraveyardGrantedKeywordKind {
    pub(crate) fn matches_keyword(self, keyword: &Keyword) -> bool {
        match self {
            GraveyardGrantedKeywordKind::Flashback => {
                keyword.kind() == crate::types::keywords::KeywordKind::Flashback
            }
            GraveyardGrantedKeywordKind::Escape => {
                keyword.kind() == crate::types::keywords::KeywordKind::Escape
            }
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
    None
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

pub(crate) fn parse_static_line_multi_inner(text: &str) -> Vec<StaticDefinition> {
    let stripped = strip_reminder_text(text);
    let lower = stripped.to_lowercase();
    let tp = TextPair::new(&stripped, &lower);

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

    if let Some(defs) = parse_compound_subject_rule_static(&stripped, &lower) {
        return defs;
    }

    if let Some(defs) = parse_compound_subject_keyword_static(&stripped, &lower) {
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

    // CR 506.5 + CR 508.1a + CR 509.1b: "can't attack or block alone" (Mogg
    // Flunkies) imposes both the attack-alone and block-alone restrictions.
    if let Some((_, AloneCombatRestriction::AttackOrBlock, rest)) =
        nom_primitives::scan_preceded(&lower, parse_alone_combat_restriction)
    {
        if rest.trim().is_empty() {
            return vec![
                StaticDefinition::new(StaticMode::CantAttackAlone)
                    .affected(TargetFilter::SelfRef)
                    .description(stripped.to_string()),
                StaticDefinition::new(StaticMode::CantBlockAlone)
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

    // CR 602.5: Compound "can't attack/block" + "activated abilities can't be activated"
    // produces two static definitions (e.g., CantAttackOrBlock + CantBeActivated).
    if nom_primitives::scan_contains(&lower, "activated abilities can't be activated")
        && (nom_primitives::scan_contains(&lower, "can't attack")
            || nom_primitives::scan_contains(&lower, "can't block"))
    {
        let mut defs = Vec::new();
        let combat_mode = if nom_primitives::scan_contains(&lower, "can't attack or block") {
            StaticMode::CantAttackOrBlock
        } else if nom_primitives::scan_contains(&lower, "can't attack") {
            StaticMode::CantAttack
        } else {
            StaticMode::CantBlock
        };
        defs.push(
            StaticDefinition::new(combat_mode)
                .affected(TargetFilter::SelfRef)
                .description(stripped.to_string()),
        );
        defs.push(
            // CR 602.5 + CR 603.2a: Self-reference case — the affected permanent's
            // own activated abilities can't be activated by anyone.
            StaticDefinition::new(StaticMode::CantBeActivated {
                who: ProhibitionScope::AllPlayers,
                source_filter: TargetFilter::SelfRef,
                exemption: parse_cant_be_activated_exemption_in_text(&lower),
            })
            .affected(TargetFilter::SelfRef)
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

    // CR 602.5: "<grant or restriction> and its activated abilities can't be
    // activated" pairs a first clause with an activation prohibition under one
    // subject (Viper's Kiss). Split so the CantBeActivated clause is not dropped.
    // (The "can't attack/block, and activated abilities …" compound — Arrest,
    // Faith's Fetters — is handled by its own earlier branch above.)
    if let Some(defs) = try_split_and_cant_activate_abilities(&stripped) {
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
    crate::parser::oracle_keyword::parse_keyword_from_oracle(tail.trim().trim_end_matches('.'))
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
    // Preserve the Oracle unless rider in the AST so swallow/coverage see a
    // `condition` slot even when the inner clause is not yet decomposed.
    Some(StaticCondition::Not {
        condition: Box::new(StaticCondition::Unrecognized {
            text: format!("unless {original}"),
        }),
    })
}

/// CR 508.1 / CR 509.1c: Parse the trailing " if [condition]" clause of a
/// combat-restriction static ("~ can't attack if defending player controls an
/// untapped land"). Mirrors `parse_unless_static_condition`; delegates the
/// condition body to `parse_static_condition` → `parse_inner_condition` (the
/// single authority for game-state conditions).
pub(crate) fn parse_if_static_condition(tp: &TextPair<'_>) -> Option<StaticCondition> {
    let (_, if_text) = tp.split_around(" if ")?;
    parse_static_condition(if_text.original)
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
    let quantity = parse_for_each_clause(&lowered).ok_or_else(|| {
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

    if let Some(filter) = parse_controlled_compound_continuous_subject_filter(&tp) {
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

    let (filter, rest) = parse_type_phrase(trimmed);
    if rest.trim().is_empty() {
        return Some(filter);
    }

    parse_rule_static_subject_filter(trimmed)
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
    fn parse_subtype_word(input: &str) -> nom::IResult<&str, &str, OracleError<'_>> {
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
    if !rest.is_empty() && !rest.starts_with(' ') && !rest.starts_with('.') {
        return None;
    }
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
        filters.into_iter().next()
    } else {
        Some(TargetFilter::Or { filters })
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
        ("attacking creatures you control", FilterProp::Attacking),
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
            TypedFilter::creature().properties(vec![FilterProp::Attacking]),
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
                exclude_source: false,
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

pub(crate) fn parse_creature_subject_filter(subject: &str) -> Option<TargetFilter> {
    let trimmed = subject.trim();
    let lower = trimmed.to_lowercase();
    let tp = TextPair::new(trimmed, &lower);

    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    let (subject_core, controller) = if let Some(prefix) = tp.original.strip_suffix(" you control")
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    {
        (prefix.trim(), Some(ControllerRef::You))
    // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
    } else if let Some(prefix) = tp.original.strip_suffix(" your opponents control") {
        // allow-noncombinator: moved legacy static parser code; refactor-only split preserves behavior.
        (prefix.trim(), Some(ControllerRef::Opponent))
    } else {
        (tp.original, None)
    };

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
        if descriptor_text.eq_ignore_ascii_case("creatures") {
            // CR 205.2a: "creatures" names the creature card type, not a creature subtype.
            let mut typed = TypedFilter::creature();
            if let Some(controller) = controller {
                typed = typed.controller(controller);
            }
            if has_other {
                typed = typed.properties(vec![FilterProp::Another]);
            }
            return Some(TargetFilter::Typed(typed));
        }
        // CR 205.3m: Use parse_subtype for irregular plurals (Elves→Elf, Dwarves→Dwarf)
        if let Some((canonical, _)) = parse_subtype(descriptor_text) {
            let mut typed = TypedFilter::creature().subtype(canonical);
            if let Some(controller) = controller {
                typed = typed.controller(controller);
            }
            if has_other {
                typed = typed.properties(vec![FilterProp::Another]);
            }
            return Some(TargetFilter::Typed(typed));
        }
        descriptor_text.trim_end_matches('s').trim()
    } else {
        return None;
    };

    if descriptor.eq_ignore_ascii_case("creature") {
        // CR 205.2a: "creature" names the creature card type, not a creature subtype.
        let mut typed = TypedFilter::creature();
        if let Some(controller) = controller {
            typed = typed.controller(controller);
        }
        if has_other {
            typed = typed.properties(vec![FilterProp::Another]);
        }
        return Some(TargetFilter::Typed(typed));
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
        exclude_source: false,
    };
    (&subject[..before.len()], Some(prop))
}

fn merge_filter_prop(filter: TargetFilter, prop: FilterProp) -> TargetFilter {
    match filter {
        TargetFilter::Typed(mut tf) => {
            tf.properties.push(prop);
            TargetFilter::Typed(tf)
        }
        other => other,
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
        value(
            RuleStaticPredicate::LoseAllAbilities,
            alt((tag("loses all abilities"), tag("lose all abilities"))),
        ),
    ))
    .parse(input)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    Ok((rest, predicate))
}

/// Combat-rule predicate plus optional CR 508.1d defended scope (`CantAttack` only).
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
) -> OracleResult<'_, RuleStaticPredicate> {
    alt((
        parse_rule_static_predicate_nom,
        value(RuleStaticPredicate::CantBlock, tag("block")),
        value(
            RuleStaticPredicate::CantCrew,
            (tag("crew"), opt(preceded(space1, tag("vehicles")))),
        ),
        value(
            RuleStaticPredicate::CantBeActivated,
            alt((
                tag("have its activated abilities activated"),
                tag("have their activated abilities activated"),
            )),
        ),
    ))
    .parse(input)
}

pub(crate) fn parse_rule_static_tail_predicates(rest: &str) -> Option<Vec<RuleStaticPredicate>> {
    let mut remaining = rest;
    let mut predicates = Vec::new();

    loop {
        let trimmed = remaining.trim();
        if trimmed.is_empty() || trimmed == "." {
            return Some(predicates);
        }
        let (after_separator, _) = parse_rule_static_separator_nom(trimmed).ok()?;
        let (after_predicate, predicate) =
            parse_rule_static_tail_predicate_nom(after_separator).ok()?;
        predicates.push(predicate);
        remaining = after_predicate;
    }
}

/// Optional attack-target scope after "can't attack" (CR 508.1d).
pub(crate) fn parse_cant_attack_defended_scope_nom(
    input: &str,
) -> OracleResult<'_, Option<crate::types::triggers::AttackTargetFilter>> {
    use crate::types::triggers::AttackTargetFilter;
    opt(alt((
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
    let (rest, _) = tag("can't attack").parse(input)?;
    let (rest, _) = opt(preceded(space1, tag("its owner"))).parse(rest)?;
    let (rest, a_player) = opt(preceded(space1, tag("a player"))).parse(rest)?;
    let (rest, defended) = parse_cant_attack_defended_scope_nom(rest)?;
    use crate::types::triggers::AttackTargetFilter;
    let defended = if a_player.is_some() {
        Some(AttackTargetFilter::Player)
    } else {
        defended
    };
    Ok((rest, defended))
}
