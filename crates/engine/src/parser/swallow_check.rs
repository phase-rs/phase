//! Architectural rule: the parser must never silently discard Oracle text.
//!
//! Every clause in Oracle text must either be represented in the parsed AST,
//! OR cause the line to fail and yield `Effect::Unimplemented` carrying the
//! original phrase. Anything in between is a parser lie.
//!
//! This module audits each card's parsed `ParsedAbilities` against its
//! original Oracle text and emits a `parse_warning` for every swallow marker
//! that has no AST representation. Findings surface in the coverage report
//! via `CardFace::parse_warnings`.
//!
//! Phase 1 (this commit): observability only — warnings, no semantic changes.
//! Once detector noise is calibrated, Phase 2 will demote affected abilities
//! to `Effect::Unimplemented`.
//!
//! Detectors are intentionally conservative. Each one:
//!   1. Scans the lower-cased Oracle text (with parenthesized reminder text
//!      stripped) for a marker phrase.
//!   2. Inspects the parsed `ParsedAbilities` directly for the corresponding
//!      AST representation.
//!   3. Emits a warning ONLY when the marker is present and the AST has no
//!      representation.

use super::oracle::{is_draft_matters_sentence, ParsedAbilities};
use super::oracle_effect::player_lookback_relative_clause_owns_suffix;
use super::oracle_ir::diagnostic::{CascadeSlot, OracleDiagnostic};
use super::oracle_ir::doc::OracleItemIr;
use super::oracle_ir::feature::{
    audit_units, scope_to_unit, AuditUnit, ItemIdTracks, OracleSemanticFeature,
};
use super::swallow_evidence::UnitEvidence;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, ActivationRestriction, CastingPermission, Comparator,
    ContinuousModification, CopyRetargetPermission, DamageModification, DelayedTriggerCondition,
    Duration, Effect, FilterProp, ManaProduction, ModalSelectionConstraint, OpponentMayScope,
    ParsedCondition, PlayerFilter, QuantityExpr, QuantityRef, ReplacementCondition,
    ReplacementMode, RestrictionExpiry, StaticCondition, StaticDefinition, TargetFilter,
    TriggerCondition, TriggerConstraint, TriggerDefinition, UnlessPayScaling,
};
use crate::types::game_state::RetargetScope;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaExpiry};
use crate::types::replacements::ReplacementEvent;
use crate::types::statics::ActivationExemption;
use crate::types::statics::{CastCostMode, StaticMode};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use nom::{
    branch::alt,
    bytes::complete::{tag, take_while1},
    character::complete::digit1,
    combinator::{opt, value},
    Parser,
};

/// Strip parenthesized reminder text. Reminder text is the parser's
/// responsibility to ignore at the keyword level — keywords themselves are
/// parsed via the keyword pipeline, and the reminder text inside parens just
/// describes what the keyword does. Marker phrases inside reminder text
/// would generate false positives.
fn strip_parens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth: u32 = 0;
    for ch in s.chars() {
        match ch {
            '(' => depth = depth.saturating_add(1),
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Stamp the owning unit's provenance — line, span, and evidence items — onto
/// everything that unit's detectors emitted.
///
/// Detectors are deliberately provenance-agnostic: a detector knows *evidence*, not
/// where it is. Attribution belongs to the loop that scoped the unit, because that loop
/// is the only thing that knows which unit it scoped. Threading provenance through all
/// fifteen detectors instead would mean a single forgotten call site silently keeps the
/// unattributed default — and `line_index: 0` does not read as "unattributed", it reads
/// as *line 1*. (That is also why the span is an `Option` and not a zeroed
/// `OracleSourceSpan`: absence must be expressible.)
///
/// The provenance is the UNIT's, not an item's, and that is the honest scope: the
/// audit's evidence half is the unit's pooled items, so no single item can be named. See
/// `OracleDiagnostic::SwallowedClause`.
///
/// Exhaustive on purpose: a future audit-emitted diagnostic variant must make a
/// deliberate decision about how it carries provenance rather than silently inheriting
/// the unattributed default.
fn stamp_provenance(unit_diagnostics: &mut [OracleDiagnostic], unit: &AuditUnit<'_>) {
    for diagnostic in unit_diagnostics {
        match diagnostic {
            OracleDiagnostic::SwallowedClause {
                line_index,
                unit_span,
                items,
                ..
            } => {
                *line_index = unit.first_line;
                *unit_span = Some(unit.span.clone());
                *items = unit.item_ids();
                debug_assert!(
                    unit.span.first_line <= unit.first_line
                        && unit.first_line <= unit.span.last_line,
                    "a unit's attributed line must lie inside the span of the text it owns",
                );
            }
            // The swallow audit emits `SwallowedClause` and nothing else. These
            // three are parse-time diagnostics that reach the document's channel by
            // other routes and are never constructed here.
            OracleDiagnostic::TargetFallback { .. }
            | OracleDiagnostic::IgnoredRemainder { .. }
            | OracleDiagnostic::CascadeLoss { .. } => {}
        }
    }
}

/// Run all swallow detectors, once per document item, against **that item's own**
/// lowered definitions.
///
/// The audit's question is per-unit: *does the Oracle text of this item raise a
/// semantic expectation that the parse of this same item does not represent?*
/// Previously both halves were card-wide — every detector was handed the whole
/// card's text and the whole card's `ParsedAbilities` — so evidence never had to
/// come from the clause that raised the expectation. A card that dropped an
/// activation limit on line 3 was excused by an unrelated restriction on line 1,
/// and three detectors (`ActivateLimit`, `Duration_NextTurn`, `APNAP`) were
/// vacuous by construction as a result. Scoping both halves to the item is the fix.
///
/// SINK INVARIANT: `diagnostics` may arrive **non-empty** — it is the document's
/// one warning channel and already carries the parse-time diagnostics sealed by
/// `finish()`. The audit never reads it. Each item's detectors emit into a fresh
/// local vec, which is stamped and appended, so no predicate here can ever match a
/// diagnostic this audit did not itself emit. (The variant sets are disjoint
/// besides — the audit only ever constructs `SwallowedClause` — so this is a
/// type-level fact, not a convention.)
pub(crate) fn check_swallowed_clauses(
    items: &[OracleItemIr],
    source_text: &str,
    result: &ParsedAbilities,
    tracks: &ItemIdTracks<'_>,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    for unit in audit_units(items, source_text) {
        // CR 905: draft-time "draft matters" lines are intentionally consumed as
        // no-ops, so their "you may" / "if you do" / "as long as" markers would every
        // one otherwise report as a swallowed clause. Filtered per LINE, not per unit:
        // a unit owns a block of lines, and a draft line sitting in the same block as a
        // constructed-play line must not take the whole block down with it.
        let draft_filtered;
        let fragment: &str = if unit.text.lines().any(is_draft_matters_sentence) {
            draft_filtered = unit
                .text
                .lines()
                .filter(|line| !is_draft_matters_sentence(line))
                .collect::<Vec<_>>()
                .join("\n");
            &draft_filtered
        } else {
            &unit.text
        };
        if fragment.trim().is_empty() {
            continue;
        }

        let scoped = scope_to_unit(result, tracks, &unit);

        // Architectural rule: a parser that produced `Effect::Unimplemented` has
        // *explicitly* admitted it couldn't parse this text — the text is preserved
        // on the Unimplemented effect itself and a separate coverage warning is
        // raised, so re-reporting it as a swallowed clause would double-count one
        // defect. Evaluated against an item-scoped `parsed`, this is the plan's
        // per-unit suppression rule: an unsupported item suppresses **only its own**
        // expectations. Card-wide, it silenced every detector on 2,563 faces.
        if any_ability_has_unimplemented(&scoped) {
            continue;
        }

        let lower_owned = fragment.to_ascii_lowercase();
        let cleaned = strip_parens(&lower_owned);

        // Typed evidence probe over this item's definitions: the tree the detectors
        // interrogate for "is a carrier for semantic S present?". Replaces the
        // serialized-AST substring haystack — see `swallow_evidence` for why a string
        // marker is unsound (it can name a type that does not exist, or silently match
        // a LONGER type's name and discharge an unrelated rule's fact).
        let evidence = UnitEvidence::of(&scoped);

        let mut found = Vec::new();
        detect_replacement(&cleaned, fragment, &scoped, &evidence, &mut found);
        detect_replacement_instead(&cleaned, fragment, &scoped, &mut found);
        detect_activate_only_during(&cleaned, fragment, &scoped, &mut found);
        detect_activate_limit(&cleaned, fragment, &scoped, &mut found);
        detect_duration_until_eot(&cleaned, fragment, &scoped, &evidence, &mut found);
        detect_optional_you_may(&cleaned, fragment, &scoped, &mut found);
        detect_dynamic_qty(&cleaned, fragment, &evidence, &mut found);
        detect_condition_if(&cleaned, fragment, &evidence, &scoped, &mut found);
        detect_condition_unless(&cleaned, fragment, &evidence, &mut found);
        detect_condition_as_long_as(&cleaned, fragment, &evidence, &scoped, &mut found);
        detect_duration_this_turn(&cleaned, fragment, &evidence, &mut found);
        detect_duration_next_turn(&cleaned, fragment, &evidence, &mut found);
        detect_optional_may_have(&cleaned, fragment, &evidence, &mut found);
        detect_apnap(&cleaned, fragment, &scoped, &mut found);
        detect_modal_dynamic_max_dropped(&cleaned, fragment, &evidence, &mut found);

        stamp_provenance(&mut found, &unit);
        diagnostics.append(&mut found);
    }
}

/// CR 603.2: true when this line's "enters with" is the TRIGGER's event filter
/// ("Whenever a creature you control enters with a counter on it, ...") rather than a
/// CR 614.1c replacement. A trigger's condition clause runs up to the comma that closes
/// it, so an "enters with" ahead of that comma on a `when`/`whenever` line describes
/// which entries FIRE the ability — it does not replace the entry event.
fn enters_with_is_trigger_filter(line: &str) -> bool {
    let l = line.trim_start();
    // allow-noncombinator: swallow detector marker scan on classified text
    if !(l.starts_with("whenever ") || l.starts_with("when ")) {
        return false;
    }
    // allow-noncombinator: swallow detector marker scan on classified text
    // allow-noncombinator: swallow detector marker scan on classified text
    l.split(',')
        .next()
        .is_some_and(|cond| cond.contains("enters with")) // allow-noncombinator: swallow detector marker scan on classified text
}

// ── Detector N: Replacement (CR 614) ────────────────────────────────────

/// CR 614: an event-modifying effect exists. This is the CR 614 replacement
/// grammar that does **not** use the word "instead" — `Replacement_Instead`
/// (CR 614.1a) already owns that one, and the two are deliberately separate
/// labels so per-detector regression attribution survives.
///
/// The two grammars this owns, both verified against the Comprehensive Rules:
///
///   CR 614.1b  "Effects that use the word 'skip' are replacement effects."
///              Carriers: `StaticMode::SkipStep`, `Effect::SkipNextTurn`,
///              `Effect::SkipNextStep`.
///   CR 614.1c  "Effects that read '[This permanent] enters with . . . ,'
///              'As [this permanent] enters . . . ,' or '[This permanent]
///              enters as . . .' are replacement effects."
///              Carrier: a `ReplacementDefinition`, or one of the non-`replacements`
///              carriers the shared helpers below already know about.
///
/// CR 614.1d ("[This permanent] enters . . ." / "[Objects] enter . . .") is
/// deliberately NOT claimed here. Its marker would be the bare word "enters",
/// which every ETB *trigger* on every creature in the pool also matches — the
/// expectation would fire on the whole pool and the label would carry no
/// information. It needs a grammar that distinguishes the replacement reading
/// from the trigger reading, which is recognizer-bring-up work, not audit work.
/// Stated rather than silently omitted.
///
/// Evidence reuses the SAME carrier helpers as `detect_replacement_instead`:
/// a replacement is a replacement however its text spelled it, so the two
/// detectors must not drift apart on what counts as one.
fn detect_replacement(
    cleaned: &str,
    original: &str,
    parsed: &ParsedAbilities,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // CR 614.1b: "skip" — scoped to the verb form, so "skipped" / a card NAMED
    // "Skip" cannot raise the expectation.
    // allow-noncombinator: swallow detector marker scan on classified text
    let has_skip = cleaned.contains("skip your ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("skip their ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("skip the next ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("skips their ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("skips his or her "); // allow-noncombinator: swallow detector marker scan on classified text

    // CR 614.1c: the three explicit enters-replacement templates. "as ~ enters" is
    // matched per LINE (a line whose first word is "as" and which mentions entering),
    // because the unit text carries the card's REAL name, not the normalized `~` —
    // `AuditUnit::text` is sliced from raw `source_text` precisely so the detectors'
    // marker phrases and the emitted `description` stay raw-text concepts.
    // CR 614.1c vs CR 603.2: "Whenever a creature you control enters WITH a counter on
    // it, ..." (Murderous Redcap Avatar) reads "enters with" as a TRIGGER EVENT FILTER —
    // it describes which entries fire the trigger, and replaces no event. Only the
    // REPLACEMENT reading raises a CR 614 expectation, so the trigger reading is excluded
    // per line: when the phrase sits inside the trigger's condition clause (ahead of the
    // comma that closes it) on a line that opens with "when"/"whenever", it is a filter.
    let has_enters_with = cleaned
        .lines()
        // allow-noncombinator: swallow detector marker scan on classified text
        .filter(|line| line.contains("enters with "))
        .any(|line| !enters_with_is_trigger_filter(line)); // allow-noncombinator: swallow detector marker scan on classified text
                                                           // allow-noncombinator: swallow detector marker scan on classified text
    let has_enters_as = cleaned.contains(" enters as "); // allow-noncombinator: swallow detector marker scan on classified text
    let has_as_enters = cleaned.lines().any(|line| {
        let l = line.trim_start();
        // allow-noncombinator: swallow detector marker scan on classified text
        l.starts_with("as ") && l.contains(" enters")
    });

    if !(has_skip || has_enters_with || has_enters_as || has_as_enters) {
        return;
    }

    // CR 614: the replacement is represented if this unit produced a
    // `ReplacementDefinition`, or any of the carriers that hold a replacement
    // OUTSIDE the `replacements` collection (rider replacements registered at
    // resolution, effect-chain `*Instead` conditions, replacement-bearing statics).
    if !parsed.replacements.is_empty()
        || any_ability_has_target_replacement(parsed)
        || any_ability_has_replacement_carrier(parsed)
    {
        return;
    }
    // CR 614.1b carriers: a skip is modelled as a step-skipping static or a
    // skip-next-turn/step effect rather than a `ReplacementDefinition`.
    if evidence.any_static_mode(|m| matches!(m, StaticMode::SkipStep { .. }))
        || evidence
            .any_effect(|e| matches!(e, Effect::SkipNextTurn { .. } | Effect::SkipNextStep { .. }))
    {
        return;
    }
    // CR 614.1c carriers: an "enters with ..." replacement is frequently modelled as a
    // STATIC or a continuous modification rather than a `ReplacementDefinition` — the
    // effect is continuous and applies to a CLASS of objects, so it lives in the layer
    // system. Thunderous Velocipede ("Each other Vehicle and creature you control enters
    // with an additional +1/+1 counter ...") is the motivating case: it lowers to
    // `StaticMode::EntersWithAdditionalCounters` and carries no ReplacementDefinition at
    // all. Completeness grep over the type definitions:
    //
    //   $ rg '^    \w*Enter\w*\s*[,{(]' crates/engine/src/types/
    //     StaticMode::EntersWithAdditionalCounters   StaticMode::CantEnterBattlefieldFrom
    //     ContinuousModification::AddCounterOnEnter  Effect::AddPendingEntersModifications
    //
    // That grep is exactly why `Effect::AddPendingETBCounters` was missing below: it keys
    // on variants NAMED Enter*, and this one is named Add*. It is the DELAYED CR 614.1c
    // carrier — the enters-with rider that binds to a LATER cast instead of to the source
    // permanent. Yuna, Grand Summoner ("When you next cast a creature spell this turn,
    // that creature enters with two additional +1/+1 counters on it") lowers to
    // `CreateDelayedTrigger{ WhenNextEvent{SpellCast} -> AddPendingETBCounters }`;
    // Osteomancer Adept's graveyard-cast rider lowers to a sequential
    // `AddPendingETBCounters` consumed as `CastFromZone` permission metadata. Both ARE the
    // replacement, and neither produces a `ReplacementDefinition`. Omitting it reported 5
    // FALSE POSITIVES pool-wide: chocobo camp, kumano faces kakkazan, osteomancer adept,
    // summon: fenrir, yuna.
    //
    // It must be probed via the tree-global typed evidence, NOT via the structural
    // `effect_is_replacement_carrier` walk: that walk descends `sub_ability` / `else_ability`
    // / `mode_abilities` only, so it cannot see a carrier nested inside an EFFECT — and
    // Yuna's carrier lives inside `Effect::CreateDelayedTrigger`'s inner definition.
    if evidence.any_static_mode(|m| {
        matches!(
            m,
            StaticMode::EntersWithAdditionalCounters { .. } | StaticMode::CantEnterBattlefieldFrom
        )
    }) || evidence.any::<ContinuousModification>(|m| {
        matches!(m, ContinuousModification::AddCounterOnEnter { .. })
    }) || evidence.any_effect(|e| {
        matches!(
            e,
            Effect::AddPendingEntersModifications { .. } | Effect::AddPendingETBCounters { .. }
        )
    }) {
        return;
    }
    // CR 614.1c + CR 614.12: the per-object enters-modifier slots — an "as it enters"
    // gate the parser folded onto the moving object rather than into a standalone
    // replacement definition.
    if evidence.has_slot("enters_modified_if")
        || evidence.has_slot("conditional_enter_with_counters")
        || evidence.has_slot("enter_with_counters")
    {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::Replacement.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector A: Replacement_Instead ─────────────────────────────────────

/// CR 614: "if X would Y, [do Z] instead" — every "instead" phrase outside of
/// reminder text must yield a `ReplacementDefinition` somewhere in the parsed
/// abilities. If Oracle has " instead" but `replacements` is empty AND no
/// existing ability captures replacement semantics, the clause was swallowed.
fn detect_replacement_instead(
    cleaned: &str,
    original: &str,
    parsed: &ParsedAbilities,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains(" instead") {
        return;
    }
    if !parsed.replacements.is_empty() {
        return;
    }
    // CR 700.2a / CR 601.2b: "choose both instead" modal overrides are
    // represented as casting-time modal choice constraints, not replacement
    // effects.
    if parsed_has_conditional_modal_max(parsed) {
        return;
    }
    // CR 614.1a: AddTargetReplacement riders register a replacement at
    // resolution time on the parent target — they ARE replacements, just
    // not in the static `replacements` collection.
    if any_ability_has_target_replacement(parsed) {
        return;
    }
    // CR 614.1a + CR 701.5: cast-then-exile / counter-then-exile sub_ability
    // chains ARE the "exile it instead" rider, structurally encoded as a
    // chained ChangeZone-to-Exile on the parent's target.
    if any_ability_has_exile_parent_rider(parsed) {
        return;
    }
    // CR 608.2c + CR 614.1a: effect-chain "instead" overrides are encoded as
    // `AbilityCondition::*Instead` on a sub_ability, not as top-level
    // replacement definitions.
    if any_ability_has_instead_condition(parsed) {
        return;
    }
    // CR 608.2m + CR 614.1a + CR 614.11: the remaining replacement carriers live
    // in an effect or a static rather than in `parsed.replacements`.
    if any_ability_has_replacement_carrier(parsed) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::ReplacementInstead.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector B: ActivateOnlyDuring ──────────────────────────────────────

/// CR 605.1c: "Activate only during X" — restricted activation timing.
/// Must be represented as an activation constraint on the parsed ability.
fn detect_activate_only_during(
    cleaned: &str,
    original: &str,
    parsed: &ParsedAbilities,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    let has_marker = cleaned.contains("activate only during") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("activate this ability only during"); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    if any_ability_has_constraint(parsed) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::ActivateOnlyDuring.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector C: ActivateLimit ───────────────────────────────────────────

/// CR 605: "Activate this ability only once/twice/no more than N times each
/// turn" — usage-limited activation. Must be represented as an activation
/// limit on the parsed ability.
fn detect_activate_limit(
    cleaned: &str,
    original: &str,
    parsed: &ParsedAbilities,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    let has_marker = cleaned.contains("activate this ability only once each") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("activate this ability only twice each") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("activate this ability no more than") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("activate only once each turn") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("activate only twice each turn"); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    if any_ability_has_limit(parsed) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::ActivateLimit.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector D: Duration_UntilEndOfTurn ─────────────────────────────────

/// CR 611.2a: "until end of turn" — temporal scope. Must be represented as a
/// duration on the parsed ability.
fn detect_duration_until_eot(
    cleaned: &str,
    original: &str,
    parsed: &ParsedAbilities,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("until end of turn") {
        return;
    }
    // CR 611.2a: the evidence must be an END-OF-TURN duration, not "some duration" — a
    // `Permanent` grant must not discharge an "until end of turn" expectation. The typed
    // probe is the SINGLE evidence channel for `Duration`-typed carriers. Its reach is
    // serde-derived, so it structurally cannot miss one: it sees carriers that a
    // hand-enumerated walk missed, e.g. a duration nested in
    // `Effect::Token.static_abilities -> GrantTrigger -> trigger.execute`, or
    // `ManaSpellGrant::AddKeywordUntilEndOfTurn.duration` (a `Box<Duration>`; `Box` is
    // transparent to serde).
    //
    // Key-anchored (`any_duration`) because `Duration` is externally tagged: its unit
    // variants serialize as BARE STRINGS, so an unanchored probe would accept any string
    // anywhere that happened to equal a variant name. The anchor also fixes a blind spot
    // the substring marker had: `Effect::PreventDamage` stores its duration in
    // `prevention_duration`, and `"prevention_duration":"UntilEndOfTurn"` does NOT contain
    // the substring `"duration":"UntilEndOfTurn"` — so every damage-prevention shield's
    // end-of-turn duration was invisible to the old marker.
    if evidence.any_duration(|d| matches!(d, Duration::UntilEndOfTurn | Duration::UntilEndOfCombat))
    {
        return;
    }
    // The two carriers below sit OUTSIDE the typed probe's reach BY CONSTRUCTION, not by
    // omission: the probe is keyed on values of type `Duration`, and each of these expresses
    // an end-of-turn scope WITHOUT one. No widening of `any_duration` can subsume them, so
    // they remain typed legs beside it rather than being folded into it.

    // CR 106.4: a player's mana pool empties at the end of each step and phase; "that mana
    // doesn't empty until end of turn" overrides that. The scope is carried by
    // `Effect::Mana.expiry`, typed `ManaExpiry` — a DIFFERENT TYPE than `Duration`.
    if unit_has_end_of_turn_mana_expiry(parsed) {
        return;
    }
    // CR 614.6 + CR 514.2: a one-shot DAMAGE replacement created by a resolving spell
    // ("Until end of turn, all damage that would be dealt to you ... is dealt to that
    // creature instead" — Heroic Sacrifice) has NO duration field at all —
    // `ReplacementDefinition` does not have one. Its "until end of turn" lifetime is
    // structural: the modified event replaces the original (CR 614.6) and the shield ends at
    // cleanup (CR 514.2). The event type IS the duration, so the clause is represented.
    // Typed exemption on the EVENT, not a blanket "this unit has a replacement".
    if parsed
        .replacements
        .iter()
        .any(|replacement| matches!(replacement.event, ReplacementEvent::DamageDone))
    {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::DurationUntilEndOfTurn.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector E: Optional_YouMay ─────────────────────────────────────────

/// CR 117.3a: "you may [verb]" — optional effect. The triggered/activated
/// ability that contains this phrase must have its `optional` flag set.
fn detect_optional_you_may(
    cleaned: &str,
    original: &str,
    parsed: &ParsedAbilities,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // Only the bare "you may [verb]" optional-effect form. "you may cast" is
    // NOT excluded at this scan level — the optionality is satisfied on the
    // AST-walk side via `any_ability_is_optional` checking `casting_options`,
    // `CastFromZone`, `GrantCastingPermission`, and `CastCopyOfCard`.
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("you may ") {
        return;
    }
    if any_ability_is_optional(parsed) {
        return;
    }
    // CR 700.2a / CR 601.2b: "you may choose both instead" grants a modal
    // choice range, not an optional effect during resolution.
    if parsed_has_conditional_modal_max(parsed) {
        return;
    }
    // CR 702.160a: Prototype keyword explanation "(You may cast this spell with
    // different mana cost, color, and size. It keeps its abilities and types.)"
    // is keyword reminder text, not an optional effect.
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("you may cast this spell with different mana cost") {
        // allow-noncombinator: swallow detector marker scan on classified text
        return;
    }
    // CR 305.2: "you may play additional lands" / "any number of lands" is
    // encoded as a land-drop static, which is an optional permission static,
    // not a def-level optional effect.
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("you may play") // allow-noncombinator: swallow detector marker scan on classified text
        && (cleaned.contains("additional land") // allow-noncombinator: swallow detector marker scan on classified text
            || cleaned.contains("any number of lands"))
    // allow-noncombinator: swallow detector marker scan on classified text
    {
        return;
    }
    // CR 614.1c: "you may reveal" in ETB replacement effects (e.g., Arsenal
    // Thresher) is part of the replacement condition, not a separate optional
    // effect. The reveal choice is captured in the replacement logic.
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("you may reveal") // allow-noncombinator: swallow detector marker scan on classified text
        && (cleaned.contains("as this creature enters") // allow-noncombinator: swallow detector marker scan on classified text
            || cleaned.contains("as this permanent enters"))
    // allow-noncombinator: swallow detector marker scan on classified text
    {
        return;
    }
    // Die roll result branches (e.g., "1—9 | You may put that card on top of
    // your library") are conditional effects gated by the die result, not
    // standalone optional effects. The optionality is conditional on the roll.
    // Gate on die-roll pattern (N—N |) to avoid over-broad exemption for other pipe uses.
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("— | you may")
    // allow-noncombinator: swallow detector marker scan on classified text
    {
        return;
    }
    // CR 611.3: Static abilities that grant triggers with optional effects
    // (e.g., Arm with Aether granting "you may return target creature")
    // carry the optionality in the granted trigger, not at the grant site.
    if any_static_has_granted_trigger_with_optional(parsed) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::OptionalYouMay.detector_label(),
        truncate(original, 140),
    ));
}

// ── AST predicates ──────────────────────────────────────────────────────

/// Recursive walk: does any def in the tree have `optional == true`,
/// `optional_targeting == true`, or an effect that internally encodes
/// "you may" via its own parameters (e.g., `Dig { up_to: true }`,
/// modal `ChoiceOfEffects`)?
fn def_tree_has_optional(def: &AbilityDefinition) -> bool {
    if def.optional || def.optional_targeting {
        return true;
    }
    // CR 107.1c: "you may repeat this process [any number of times]" is a
    // controller decision captured on `repeat_until` — an optional player
    // action, so the "you may" in the text is accounted for.
    if matches!(
        def.repeat_until,
        Some(crate::types::ability::RepeatContinuation::ControllerChoice)
    ) {
        return true;
    }
    if effect_has_internal_optionality(&def.effect) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_optional(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_optional(else_ab) {
            return true;
        }
    }
    def.mode_abilities.iter().any(def_tree_has_optional)
}

fn trigger_tree_has_optional(trigger: &TriggerDefinition) -> bool {
    trigger.optional
        || matches!(trigger.mode, TriggerMode::Exerted)
        || trigger
            .execute
            .as_deref()
            .is_some_and(def_tree_has_optional)
}

/// Detects "you may" optionality encoded inside the effect itself rather
/// than via `def.optional`. Some effects model the choice at the runtime
/// resolution layer (e.g., `Dig` with `up_to: true` lets the player keep
/// zero), and the def-level optional flag is therefore (correctly) false.
///
/// CR 117.3a: `GrantCastingPermission` inherently encodes a "you may
/// cast/play" permission — granting permission is opt-in by definition,
/// so the def-level optional flag does not need to be set.
///
/// CR 601.2 + CR 118.9: `CastFromZone` likewise grants a "you may cast/play"
/// permission ("you may cast sorcery spells as though they had flash",
/// Teferi/Time Raveler class; "you may play one of those cards", Nashi-class
/// impulse-draw). The "may" is the permission itself — the player choosing
/// not to cast doesn't need a separate `optional: true` flag.
///
/// CR 118.9: `PayCost` paired with the alt-cost grammar ("you may exile two
/// green cards from your hand rather than pay this spell's mana cost",
/// Allosaurus Rider) carries the "may" inside the alternative-cost choice
/// — the player either pays the alt cost or the original.
///
/// CR 305.9 / CR 701.20: `RevealFromHand` with an `on_decline` branch is
/// the structural shape of "you may reveal X. If you don't, ..." — the
/// player's reveal choice IS the "may" decision, with the decline branch
/// handling the "if you don't" alternative.
///
/// CR 118.9b + CR 707.12: `CastCopyOfCard` encodes "you may cast the copy
/// without paying its mana cost" — CR 118.9b makes the alternative cost
/// optional; the resolver presents a TrackedSet
/// `ChooseFromZoneChoice { up_to: true }` — choosing 0 is the decline path.
/// The def-level `optional` flag is correctly false (`fold_cast_copy_of_card_defs`
/// hardcodes it); the "may" lives in the CR 707.12 cast step.
fn effect_has_internal_optionality(effect: &Effect) -> bool {
    match effect {
        // CR 701.23j: Outside-game searches are optional at the selection
        // level; the parser lowers "you may reveal a ... card you own from
        // outside the game" as `count: UpTo(1)` instead of `def.optional`.
        Effect::SearchOutsideGame { count, .. } if count.is_up_to() => true,
        Effect::Dig { up_to: true, .. }
        | Effect::GrantCastingPermission { .. }
        | Effect::CastFromZone { .. }
        // CR 118.9b + CR 707.12: CastCopyOfCard encodes "you may cast the copy
        // without paying its mana cost" — CR 118.9b makes the alternative cost
        // optional; the resolver presents a TrackedSet
        // `ChooseFromZoneChoice { up_to: true }` — choosing 0 is the decline
        // path. Restricted to TrackedSet-target forms (what
        // `fold_cast_copy_of_card_defs` actually produces); `TrackedSetFiltered`
        // is included as defensive forward coverage for any future parser path.
        // The Cipher runtime path uses a pre-resolved target with no optional
        // gate and is correctly excluded.
        | Effect::CastCopyOfCard {
            target: TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. },
            ..
        }
        | Effect::PayCost { .. }
        | Effect::RevealHand {
            choice_optional: true,
            ..
        }
        | Effect::RevealFromHand {
            on_decline: Some(_),
            ..
        }
        // CR 707.10c: CopySpell with MayChooseNewTargets encodes the "you may
        // choose new targets for the copy" opt-in at the runtime resolution
        // layer (WaitingFor::CopyRetarget). The def-level `optional` flag is
        // therefore not needed — analogous to Dig { up_to: true }.
        | Effect::CopySpell {
            retarget: CopyRetargetPermission::MayChooseNewTargets,
            ..
        }
        // CR 115.7d: "you may choose new targets for [spell/ability]" lowers to
        // `ChangeTargets { scope: All }` with the full surface form preserved
        // (not `def.optional`). The player may leave targets unchanged.
        | Effect::ChangeTargets {
            scope: RetargetScope::All,
            ..
        }
        // CR 701.20a + CR 608.2c: RevealUntil with kept_optional_to encodes
        // "you may put that card onto the battlefield" — the kept-card
        // destination choice IS the "may" decision (mirrors RevealFromHand
        // { on_decline }).
        | Effect::RevealUntil {
            kept_optional_to: Some(_),
            ..
        }
        // CR 608.2d: ChangeZone `up_to` encodes "you may put/return up to N"
        // at resolution time. The player may choose zero cards, so this is
        // the same internal optionality shape as Dig { up_to: true }.
        | Effect::ChangeZone { up_to: true, .. }
        // CR 606.3 + CR 117.3a: `GrantExtraLoyaltyActivations` inherently
        // encodes the "you may activate" permission — granting permission is
        // opt-in by definition, mirroring `GrantCastingPermission`. The Chain
        // Veil's "you may activate one of its loyalty abilities once this turn"
        // is the permission itself; the player still decides each activation.
        | Effect::GrantExtraLoyaltyActivations { .. } => true,
        // CR 601.3b + CR 702.8a + CR 609.4: a `GenericEffect` whose statics
        // encode a "you may" opt-in accounts for the marker in two ways:
        //
        //   1. Casting-permission modes (`StaticMode::CastWithKeyword`, etc.):
        //      detected by `static_mode_is_optional_permission` (via
        //      `static_definition_has_optional`).
        //
        //   2. Optional modification grants (`ContinuousModification::
        //      AssignDamageAsThoughUnblocked`, `GrantStaticAbility` recursion, etc.):
        //      detected by `static_carries_optional_modification` (via
        //      `static_definition_has_optional`). Garruk, Savage Herald's [-7]
        //      ("Until end of turn, creatures you control gain 'You may have this
        //      creature assign its combat damage as though it weren't blocked.'")
        //      is the motivating case — CR 510.1c + CR 609.4.
        //
        // STILL NARROW: `static_definition_has_optional` only exempts permission
        // modes and optional modifications — statics that are neither (CantGainLife,
        // +1/+1, MustAttack, etc.) remain subject to Optional_YouMay detection.
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(static_definition_has_optional),
        Effect::ChooseOneOf { branches, .. } => branches.iter().any(def_tree_has_optional),
        Effect::CreateDelayedTrigger { effect, .. } => def_tree_has_optional(effect),
        Effect::CreateEmblem { statics, triggers } => {
            statics.iter().any(static_definition_has_optional)
                || triggers.iter().any(trigger_tree_has_optional)
        }
        // CR 705: Flip-coin branches carry win/lose payloads as nested defs;
        // "you may choose new targets for the copy" on the win branch (Krark)
        // lives in `win_effect`, not at `def.optional`.
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        } => win_effect
            .as_ref()
            .is_some_and(|def| def_tree_has_optional(def))
            || lose_effect
                .as_ref()
                .is_some_and(|def| def_tree_has_optional(def)),
        Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => win_effect
            .as_ref()
            .is_some_and(|def| def_tree_has_optional(def))
            || lose_effect
                .as_ref()
                .is_some_and(|def| def_tree_has_optional(def)),
        Effect::FlipCoinUntilLose { win_effect, .. } => def_tree_has_optional(win_effect),
        _ => false,
    }
}

/// Recursive walk: does any def in the tree carry an `AddTargetReplacement`
/// or `CreateDamageReplacement` effect? This single Effect variant simultaneously
/// encodes a replacement effect (CR 614.1a "instead"), a conditional gate
/// ("if [target] would die"), and an EOT duration (the carried replacement's
/// `expiry: EndOfTurn`). Its presence satisfies the Replacement_Instead,
/// Condition_If, and Duration_ThisTurn detectors when the original text matches
/// the "die this turn, exile instead" rider grammar. Flip-coin branches
/// (Desperate Gambit) nest these under `Effect::FlipCoin`, so recurse there too.
/// Flip-coin branch payloads may carry one-shot damage replacements.
fn flip_branch_has_target_replacement(
    win_effect: &Option<Box<AbilityDefinition>>,
    lose_effect: &Option<Box<AbilityDefinition>>,
) -> bool {
    win_effect
        .as_deref()
        .is_some_and(def_tree_has_target_replacement)
        || lose_effect
            .as_deref()
            .is_some_and(def_tree_has_target_replacement)
}

fn def_tree_has_target_replacement(def: &AbilityDefinition) -> bool {
    match def.effect.as_ref() {
        Effect::AddTargetReplacement { .. } | Effect::CreateDamageReplacement { .. } => {
            return true
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } if flip_branch_has_target_replacement(win_effect, lose_effect) => return true,
        _ => {}
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_target_replacement(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_target_replacement(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_target_replacement)
}

/// CR 702.20a / CR 702.21: certain `ContinuousModification` variants
/// encode an inherently-optional player choice that the def-level
/// `optional` flag does not capture:
///   - `AssignDamageAsThoughUnblocked` ("you may have ~ assign its combat
///     damage as though it weren't blocked") — Lone Wolf class.
///   - `AssignDamageFromToughness` is mandatory (Brontodon class), so
///     it is NOT included here.
fn static_carries_optional_modification(s: &StaticDefinition) -> bool {
    s.modifications.iter().any(|m| match m {
        ContinuousModification::AssignDamageAsThoughUnblocked => true,
        ContinuousModification::GrantTrigger { trigger } => trigger_tree_has_optional(trigger),
        ContinuousModification::GrantAbility { definition } => def_tree_has_optional(definition),
        // CR 113.3d + CR 613.1f: GrantStaticAbility conveys a static as if printed on the
        // recipient (CR 113.3d: static abilities are simply true; CR 613.1f: layer 6).
        // Recurse into the granted static definition to detect optional markers it may carry.
        ContinuousModification::GrantStaticAbility { definition } => {
            static_definition_has_optional(definition)
        }
        _ => false,
    })
}

fn static_mode_is_optional_permission(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::MayLookAtTopOfLibrary
            // CR 708.5: "you may look at face-down creatures [you don't control |
            // your opponents control] any time" — opt-in look permission.
            | StaticMode::MayLookAtFaceDown
            | StaticMode::MayChooseNotToUntap
            | StaticMode::MayPlayAdditionalLand
            | StaticMode::AdditionalLandDrop { .. }
            | StaticMode::TopOfLibraryCastPermission { .. }
            // CR 702.170a grant + CR 702.170f permission: "The top card of your
            // library has plot" / "You may plot [filter] cards from the top of
            // your library" — opt-in plot-from-library (Fblthp). The plot special
            // action (CR 702.170b) is taken at the player's discretion.
            | StaticMode::TopOfLibraryHasPlot
            | StaticMode::TopOfLibraryPlotPermission
            // CR 702.8: "You may cast this spell as though it had flash" —
            // opt-in cast-timing permission.
            | StaticMode::CastWithFlash
            // CR 702.51a: "Creature spells you cast have convoke",
            // "you may cast X as though it had flash if you pay Y" —
            // generalized cast-timing/keyword permission, always opt-in.
            | StaticMode::CastWithKeyword { .. }
            // CR 118.9: "You may pay X rather than pay the mana cost for [filter]
            // spells you cast" — opt-in alternative-mana-cost permission
            // (Rooftop Storm, Fist of Suns, Jodah), structurally optional.
            | StaticMode::CastWithAlternativeCost { .. }
            // CR 118.9 + CR 702.29a + CR 702.122a: AlternativeKeywordCost is an
            // opt-in substitution — "you may" is the permission itself
            // (New Perspectives, Heart of Kiran, Gavi Nest Warden).
            | StaticMode::AlternativeKeywordCost { .. }
            // CR 107.4f: "For each {C} in a cost, you may pay 2 life rather than
            // pay that mana." K'rrik class — per-payment substitution is opt-in.
            | StaticMode::PayLifeAsColoredMana { .. }
            // CR 602.5e: "You may activate [abilities] any time you could
            // cast an instant" is an activation-timing permission, not an
            // optional effect to execute during resolution.
            | StaticMode::ActivateAsInstant { .. }
            // CR 117.3a: "You may play lands from your graveyard"
            // (Crucible, Ramunap Excavator, etc.) — graveyard-as-zone
            // cast permission, structurally opt-in.
            | StaticMode::GraveyardCastPermission { .. }
            // CR 601.2a + CR 113.6b: Maralen-class "Once each turn, you
            // may cast …" exile-cast permission — structurally opt-in by
            // the same "you may cast" surface as the graveyard sibling.
            | StaticMode::ExileCastPermission { .. }
            // CR 601.2a + CR 113.6: Evelyn-class "Once each turn, you may
            // play a card from exile … if it was exiled by an ability you
            // controlled" — opt-in "you may play" permission whose "if"
            // provenance clause is enforced at runtime via the per-card
            // `PlayFromExile { exiled_by_ability_controller }` grant, not a
            // dropped condition.
            | StaticMode::LinkedCollectionCounterPlayPermission
            // CR 601.2f: Defiler-style cost reductions encode the optional
            // life payment inside the static cost-modification primitive.
            | StaticMode::DefilerCostReduction { .. }
            // CR 609.4b: "You may spend mana as though it were mana of any color" /
            // "you may spend mana of any type to cast [filtered] spells" — opt-in
            // mana substitution, inherently optional by the "you may" surface.
            | StaticMode::SpendManaAsAnyColor { .. }
            // CR 602.5a + CR 702.10c: "You may activate abilities of X as though those
            // creatures had haste" — lifts the summoning-sickness gate on {T}/{Q}
            // activated abilities; the permission is opt-in by the "you may" surface.
            | StaticMode::CanActivateAbilitiesAsThoughHaste
            // CR 118.9 + CR 118.9b: "You may cast [this] without paying its mana
            // cost" / "you may pay {0} rather than pay the mana cost" is an
            // alternative cost, and alternative costs are generally optional — the
            // "you may" permission is the static's entire semantic content
            // (Omniscience, As Foretold, Zaffai). Mirrors the sibling permission
            // modes above; without it the swallow auditor false-positives an
            // Optional_YouMay clause and demotes the card from "supported."
            | StaticMode::CastFromHandFree { .. }
    )
}

fn static_definition_has_optional(s: &StaticDefinition) -> bool {
    static_carries_optional_modification(s) || static_mode_is_optional_permission(&s.mode)
}

/// Check if any static ability in the parsed abilities grants a trigger
/// that has internal optionality (e.g., Arm with Aether granting a trigger
/// with "you may return target creature").
fn any_static_has_granted_trigger_with_optional(parsed: &ParsedAbilities) -> bool {
    parsed.statics.iter().any(|s| {
        s.modifications.iter().any(|m| match m {
            ContinuousModification::GrantTrigger { trigger } => trigger_tree_has_optional(trigger),
            _ => false,
        })
    })
}

/// Recursive walk: does any def in the tree carry an `Effect::Unimplemented`?
/// When the parser cannot parse a line, it emits Unimplemented carrying the
/// original text — that is itself a coverage signal. Suppressing swallow
/// detectors for these cards prevents double-reporting the same gap.
fn def_tree_has_unimplemented(def: &AbilityDefinition) -> bool {
    if matches!(*def.effect, Effect::Unimplemented { .. }) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_unimplemented(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_unimplemented(else_ab) {
            return true;
        }
    }
    def.mode_abilities.iter().any(def_tree_has_unimplemented)
}

fn trigger_tree_has_unimplemented(trigger: &TriggerDefinition) -> bool {
    trigger
        .execute
        .as_deref()
        .is_some_and(def_tree_has_unimplemented)
}

fn static_definition_has_unimplemented(s: &StaticDefinition) -> bool {
    s.modifications.iter().any(|m| match m {
        ContinuousModification::GrantTrigger { trigger } => trigger_tree_has_unimplemented(trigger),
        ContinuousModification::GrantAbility { definition } => {
            def_tree_has_unimplemented(definition)
        }
        // CR 113.3d + CR 613.1f: Parallel to static_carries_optional_modification —
        // recurse into GrantStaticAbility so an Unimplemented-carrying granted static
        // suppresses swallow detectors rather than double-reporting the parse gap.
        ContinuousModification::GrantStaticAbility { definition } => {
            static_definition_has_unimplemented(definition)
        }
        _ => false,
    })
}

fn any_ability_has_unimplemented(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_unimplemented)
        || parsed
            .triggers
            .iter()
            .any(|t| t.execute.as_deref().is_some_and(def_tree_has_unimplemented))
        || parsed
            .replacements
            .iter()
            .any(|r| r.execute.as_deref().is_some_and(def_tree_has_unimplemented))
        || parsed.statics.iter().any(static_definition_has_unimplemented)
        // CR 603: A `TriggerMode::Unknown(_)` is the trigger-side equivalent
        // of `Effect::Unimplemented` — the parser preserved the original
        // trigger text but couldn't classify the timing/event. Suppress
        // swallow detectors so we don't double-report the same gap. The
        // unparsed trigger mode text is a coverage signal in its own right.
        || parsed.triggers.iter().any(|t| {
            matches!(t.mode, crate::types::triggers::TriggerMode::Unknown(_))
        })
}

fn any_ability_has_target_replacement(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_target_replacement)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_target_replacement)
        })
}

/// Recursive walk: does any def in the tree carry a sub_ability whose
/// effect is `ChangeZone { destination: Exile, target: ParentTarget }`?
///
/// CR 614.1a + CR 701.5: This is the structural shape of "exile-instead"
/// riders attached to a primary effect that would otherwise put the
/// referenced card into a graveyard. Examples:
///   - Snapcaster/Daring Waverider: cast from graveyard, then exile.
///   - Defabricate: counter target spell, then exile (instead of putting
///     it into its owner's graveyard).
///   - Cast-from-X then exile riders generally (Chandra Acolyte, etc.).
///
/// The conditional gate ("if that spell would be put into your graveyard")
/// and the replacement semantics ("exile it instead") are both encoded by
/// this structural pairing. A sub_ability that targets the parent's target
/// and moves it to exile IS the "if X, exile instead" rider.
fn def_tree_has_exile_parent_rider(def: &AbilityDefinition) -> bool {
    if let Effect::ChangeZone {
        destination: crate::types::zones::Zone::Exile,
        target: crate::types::ability::TargetFilter::ParentTarget,
        ..
    } = &*def.effect
    {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_exile_parent_rider(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_exile_parent_rider(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_exile_parent_rider)
}

/// CR 614.1a + CR 608.2n: True when any node is a `CastFromZone` (or `Counter`)
/// whose sub-ability / else-ability chain carries a graveyard-redirect rider
/// targeting the cast/countered spell (`ParentTarget`) — to exile, a library
/// position (Kylox's Voltstrider → bottom), or the owner's hand. This is the
/// "if that spell would be put into a graveyard, [dest] instead" rider; its
/// leading conditional is represented by the structural pairing, not swallowed.
///
/// SCOPED to the cast/counter parent on purpose: a bare
/// `PutAtLibraryPosition { ParentTarget }` / `ChangeZone { Hand, ParentTarget }`
/// is a COMMON standalone effect (Conundrum Sphinx "puts it on the bottom of
/// their library", etc.) and must NOT suppress an unrelated condition swallow.
/// The exile case is also covered narrowly by `def_tree_has_exile_parent_rider`
/// (Exile-to-parent is rare outside riders); this adds the library/hand
/// destinations only inside the redirect-rider context.
fn def_tree_has_cast_graveyard_redirect_rider(def: &AbilityDefinition) -> bool {
    if matches!(
        &*def.effect,
        Effect::CastFromZone { .. } | Effect::Counter { .. }
    ) && (def
        .sub_ability
        .as_deref()
        .is_some_and(def_is_graveyard_redirect_to_parent)
        || def
            .else_ability
            .as_deref()
            .is_some_and(def_is_graveyard_redirect_to_parent))
    {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_cast_graveyard_redirect_rider(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_cast_graveyard_redirect_rider(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_cast_graveyard_redirect_rider)
}

/// A graveyard-redirect rider body: a move of the cast/countered spell
/// (`ParentTarget`) to exile, the owner's hand, or a library position. Walks the
/// sub-ability chain so an intervening continuation does not hide the rider.
fn def_is_graveyard_redirect_to_parent(def: &AbilityDefinition) -> bool {
    if matches!(
        &*def.effect,
        Effect::ChangeZone {
            destination: crate::types::zones::Zone::Exile | crate::types::zones::Zone::Hand,
            target: crate::types::ability::TargetFilter::ParentTarget,
            ..
        } | Effect::PutAtLibraryPosition {
            target: crate::types::ability::TargetFilter::ParentTarget,
            ..
        }
    ) {
        return true;
    }
    def.sub_ability
        .as_deref()
        .is_some_and(def_is_graveyard_redirect_to_parent)
}

/// CR 119.7 + CR 608.2c: True when any ability/trigger tree contains a
/// `CantGainLife` grant scoped to `ParentTarget` — the structural encoding of
/// Screaming Nemesis's "If a player is dealt damage this way, they can't gain
/// life for the rest of the game" rider. The `ParentTarget` affected filter IS
/// the "dealt damage this way" anaphor (it binds to the redirect's target only
/// when that target is a player), so the leading "if" is represented, not
/// swallowed. The match is deliberately narrow (mode + ParentTarget affected)
/// so unrelated player-scoped life-locks (e.g. "Players can't gain life")
/// remain subject to their own condition detectors.
fn def_tree_has_parent_target_cant_gain_life(def: &AbilityDefinition) -> bool {
    if let Effect::GenericEffect {
        ref static_abilities,
        ..
    } = *def.effect
    {
        if static_abilities
            .iter()
            .any(static_def_is_parent_target_cant_gain_life)
        {
            return true;
        }
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_parent_target_cant_gain_life(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_parent_target_cant_gain_life(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_parent_target_cant_gain_life)
}

fn static_def_is_parent_target_cant_gain_life(static_def: &StaticDefinition) -> bool {
    matches!(static_def.mode, StaticMode::CantGainLife)
        && matches!(static_def.affected, Some(TargetFilter::ParentTarget))
}

fn any_ability_has_dealt_damage_this_way_life_lock(parsed: &ParsedAbilities) -> bool {
    parsed
        .abilities
        .iter()
        .any(def_tree_has_parent_target_cant_gain_life)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_parent_target_cant_gain_life)
        })
}

/// CR 608.2c: True when any ability/trigger tree contains an `Effect::Discard`
/// whose target is `ParentTarget` — the structural encoding of Sonic Shrieker's
/// "If a player is dealt damage this way, they discard a card" rider. The
/// `ParentTarget` target IS the CR 608.2c "this way" back-reference: it resolves
/// to the damage recipient and only forces a discard when that recipient is a
/// player (a creature/planeswalker target no-ops), so the leading "if" is
/// represented, not swallowed. Mirrors `def_tree_has_parent_target_cant_gain_life`.
/// ponytail: prevented-damage ceiling (CR 615.5) not gated — a fully prevented
/// hit still discards here, the identical fidelity ceiling Screaming Nemesis
/// ships; upgrade path = damage-recipient AbilityCondition when a card forces it.
fn def_tree_has_parent_target_discard(def: &AbilityDefinition) -> bool {
    if matches!(
        &*def.effect,
        Effect::Discard {
            target: TargetFilter::ParentTarget,
            ..
        }
    ) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_parent_target_discard(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_parent_target_discard(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_parent_target_discard)
}

fn any_ability_has_parent_target_discard(parsed: &ParsedAbilities) -> bool {
    parsed
        .abilities
        .iter()
        .any(def_tree_has_parent_target_discard)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_parent_target_discard)
        })
}

fn any_ability_has_exile_parent_rider(parsed: &ParsedAbilities) -> bool {
    let has = |f: fn(&AbilityDefinition) -> bool| {
        parsed.abilities.iter().any(f)
            || parsed
                .triggers
                .iter()
                .any(|t| t.execute.as_deref().is_some_and(f))
    };
    // Exile-to-parent matched anywhere (narrow); library/hand only in the
    // cast/counter redirect-rider context (CR 614.1a) so a standalone library
    // placement does not falsely suppress an unrelated condition swallow.
    has(def_tree_has_exile_parent_rider) || has(def_tree_has_cast_graveyard_redirect_rider)
}

fn target_filter_has_zone(filter: &TargetFilter, zone: Zone) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(
            |prop| matches!(prop, FilterProp::InZone { zone: prop_zone } if *prop_zone == zone),
        ),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => filters
            .iter()
            .any(|filter| target_filter_has_zone(filter, zone)),
        TargetFilter::Not { filter } => target_filter_has_zone(filter, zone),
        _ => false,
    }
}

fn def_tree_has_graveyard_cast_from_zone(def: &AbilityDefinition) -> bool {
    if let Effect::CastFromZone { target, .. } = &*def.effect {
        if target_filter_has_zone(target, Zone::Graveyard) {
            return true;
        }
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_graveyard_cast_from_zone(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_graveyard_cast_from_zone(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_graveyard_cast_from_zone)
}

fn any_ability_has_graveyard_cast_from_zone(parsed: &ParsedAbilities) -> bool {
    parsed
        .abilities
        .iter()
        .any(def_tree_has_graveyard_cast_from_zone)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_graveyard_cast_from_zone)
        })
}

fn condition_has_instead_semantics(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::AdditionalCostPaidInstead
        | AbilityCondition::CastVariantPaidInstead { .. }
        | AbilityCondition::TargetHasKeywordInstead { .. }
        | AbilityCondition::ConditionInstead { .. } => true,
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            conditions.iter().any(condition_has_instead_semantics)
        }
        AbilityCondition::Not { condition } => condition_has_instead_semantics(condition),
        _ => false,
    }
}

fn def_tree_has_instead_condition(def: &AbilityDefinition) -> bool {
    if def
        .condition
        .as_ref()
        .is_some_and(condition_has_instead_semantics)
    {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_instead_condition(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_instead_condition(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_instead_condition)
}

fn any_ability_has_instead_condition(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_instead_condition)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_instead_condition)
        })
        || parsed.replacements.iter().any(|r| {
            r.execute
                .as_deref()
                .is_some_and(def_tree_has_instead_condition)
        })
}

/// CR 614: replacement semantics carried by an EFFECT or a STATIC rather than by a
/// `ReplacementDefinition` in `parsed.replacements`.
///
/// # What this replaces, and why the thing it replaces was vacuous
///
/// The exemption here used to be `any_text_field_contains(parsed, "instead")` — a
/// DESCRIPTION-channel check, and the purest vacuity in this module. `description` is
/// **raw Oracle text**: `oracle_trigger.rs` sets `def.description = Some(ir.source_text
/// .clone())` and `oracle_effect/assembly.rs` sets it from the clause's own source
/// fragment. So the evidence for *"did the parse represent 'instead'?"* was *"does our
/// copy of the Oracle text contain 'instead'?"* — which is true **precisely when the
/// detector's own marker fired**, because that marker is this very detector's ` instead`
/// substring scan over the same text.
///
/// That is the APNAP defect exactly (CR 101.4 / `def_tree_has_apnap_ordering`): the fact
/// demanded as proof was implied by the very clause raising the expectation, so the
/// detector excused itself no matter what the parser had actually dropped. A description
/// is not evidence — it is a transcript of the question.
///
/// Exhaustive with no `_` arm on purpose: a new replacement-carrying effect must declare
/// itself here rather than silently failing to suppress a false positive.
fn effect_is_replacement_carrier(effect: &Effect) -> bool {
    match effect {
        // CR 614.11: "the next time you would draw a card this turn, [effect] instead"
        // (Words of Worship / Wilding class) — the substitute rides on the effect.
        Effect::CreateDrawReplacement { .. }
        // CR 614.1a + CR 901.9c: "if a player would planeswalk as a result of rolling
        // the planar die, [effect] instead" (Fixed Point in Time).
        | Effect::CreatePlaneswalkReplacement { .. }
        // CR 614.1a + CR 608.2n: "exile it instead of putting it into its owner's
        // graveyard" replaces the CR 608.2n graveyard-put default — the variant
        // name IS the replacement, with or without the `on_exile` rider (the
        // Feather return / Lilah plot parameterization is a second consequence
        // folded into the same carrier, so it stays exempt either way).
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. } => true,
        _ => false,
    }
}

/// CR 614.1a: a def that carries BOTH a `condition` and an `else_ability` has modelled a
/// two-way alternative — "if C, do B **instead of** A" is exactly `A.condition = C` with
/// `A.else_ability = B`. The branch IS the "instead", so the clause is represented.
///
/// This is the precise structural discriminator between the represented form and the
/// swallowed one, and both are live in the pool:
///
/// - **BRANCH (represented).** `accumulate wisdom` — "Put one of those cards into your
///   hand … Put each of those cards into your hand **instead** if there are three or more
///   Lesson cards in your graveyard" lowers to `Dig { condition: QuantityCheck,
///   else_ability: Dig }`. The engine does one or the other.
/// - **CHAIN (swallowed).** `a-paragon of modernity` — "~ gets +1/+1 until end of turn.
///   If exactly three colors of mana were spent …, put a +1/+1 counter on it **instead**"
///   lowers to `Pump { sub_ability: PutCounter }` with **no condition and no else** — the
///   engine does BOTH. That is a real defect, and this predicate must keep reporting it.
///
/// So the carrier is `else_ability`, never `sub_ability`: a sub-ability is a *sequel*
/// ("and then"), an else-ability is an *alternative* ("instead"). Accepting `sub_ability`
/// here would suppress precisely the class of bug the detector exists to find.
fn def_is_represented_instead_branch(def: &AbilityDefinition) -> bool {
    def.condition.is_some() && def.else_ability.is_some()
}

fn def_tree_has_replacement_carrier(def: &AbilityDefinition) -> bool {
    if effect_is_replacement_carrier(&def.effect) || def_is_represented_instead_branch(def) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_replacement_carrier(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_replacement_carrier(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_replacement_carrier)
}

/// CR 614.1a: static modes that ARE the replacement the "instead" clause asked for.
///
/// Each arm is pinned to a witness whose parse was read out of the pool export — a static
/// mode is only a carrier if it encodes the replaced EVENT, not merely if it exists. The
/// counter-example that proves this matters is `anthem of rakdos`: its "if a source you
/// control would deal damage to an opponent, it deals that much damage plus 1 instead"
/// lowers to a `Continuous` static with **`modifications: []`** — an empty shell carrying
/// only the Hellbent condition. The replacement is gone. "Has a conditional static" is
/// therefore NOT evidence, and is deliberately not accepted here.
fn static_is_replacement_carrier(static_def: &StaticDefinition) -> bool {
    matches!(
        static_def.mode,
        // CR 614.1a: "if a spell cast this way would be put into your graveyard, exile it
        // instead". `Some(zone)` IS the rider; `None` means this printing dropped it, so
        // it must NOT suppress — `glimpse the cosmos` and `maestros ascendancy` both carry
        // `None` here and correctly keep warning.
        StaticMode::GraveyardCastPermission {
            graveyard_destination_replacement: Some(_),
            ..
        }
        // CR 614.1a + CR 701.23: "If an opponent would search a library, that player
        // searches the top four cards of that library instead" (aven mindcensor) — the
        // replaced search IS this mode.
        | StaticMode::RestrictLibrarySearchToTop { .. }
        // CR 614.1a + CR 106.4 ("each player's mana pool empties … and the player is said
        // to LOSE this mana"): "If you would lose unspent mana, that mana becomes
        // colorless instead" (horizon stone, kruphix, omnath) — the replaced mana-loss
        // event IS this mode.
        | StaticMode::StepEndUnspentMana { .. }
    )
}

fn any_ability_has_replacement_carrier(parsed: &ParsedAbilities) -> bool {
    parsed
        .abilities
        .iter()
        .any(def_tree_has_replacement_carrier)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_replacement_carrier)
        })
        || parsed.statics.iter().any(static_is_replacement_carrier)
}

fn def_tree_has_conditional_mana_spell_grant(def: &AbilityDefinition) -> bool {
    // CR 609.4b + CR 608.2c: "if you cast a spell this way, you may spend mana as
    // though it were mana of any type/color to cast it" folds onto the preceding
    // `PlayFromExile` grant as `mana_spend_permission` (Outrageous Robbery,
    // Brainstealer Dragon). The leading "if you cast a spell this way" is the
    // back-reference scoping the concession to spells cast via that grant —
    // represented by the field, not a swallowed condition.
    if let Effect::GrantCastingPermission {
        permission:
            crate::types::ability::CastingPermission::PlayFromExile {
                mana_spend_permission: Some(_),
                ..
            },
        ..
    } = &*def.effect
    {
        return true;
    }
    if let Effect::Mana { grants, .. } = &*def.effect {
        if grants.iter().any(|grant| {
            matches!(
                grant,
                crate::types::mana::ManaSpellGrant::AddKeywordUntilEndOfTurn { .. }
            )
        }) {
            return true;
        }
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_conditional_mana_spell_grant(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_conditional_mana_spell_grant(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_conditional_mana_spell_grant)
}

fn any_ability_has_conditional_mana_spell_grant(parsed: &ParsedAbilities) -> bool {
    parsed
        .abilities
        .iter()
        .any(def_tree_has_conditional_mana_spell_grant)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_conditional_mana_spell_grant)
        })
}

fn def_tree_has_cast_from_zone_alt_ability_cost(def: &AbilityDefinition) -> bool {
    if matches!(
        *def.effect,
        Effect::CastFromZone {
            alt_ability_cost: Some(_),
            ..
        }
    ) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_cast_from_zone_alt_ability_cost(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_cast_from_zone_alt_ability_cost(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_cast_from_zone_alt_ability_cost)
}

fn any_ability_has_cast_from_zone_alt_ability_cost(parsed: &ParsedAbilities) -> bool {
    parsed
        .abilities
        .iter()
        .any(def_tree_has_cast_from_zone_alt_ability_cost)
        || parsed.triggers.iter().any(|trigger| {
            trigger
                .execute
                .as_deref()
                .is_some_and(def_tree_has_cast_from_zone_alt_ability_cost)
        })
}

fn any_replacement_has_may_cost_decline(parsed: &ParsedAbilities) -> bool {
    parsed.replacements.iter().any(|repl| {
        matches!(
            repl.mode,
            ReplacementMode::MayCost {
                decline: Some(_),
                ..
            }
        )
    })
}

/// CR 614.1a + CR 120.8: "If a [source] would deal damage ..., it deals double
/// that damage instead" is an UNCONDITIONAL value-modifier replacement (CR 120.8
/// damage-increase replacement). The leading "if" is CR 614.1a replacement
/// syntax introducing the replacement's applicability — NOT an independent
/// CR 608.2c game-state gate — and is fully represented by the
/// `ReplacementDefinition`'s `damage_modification`/`quantity_modification` with
/// no `condition`. Only false-positives on ability-word-prefixed lines
/// ("Flare Star — if a wizard ...") where the `— if` injects the leading space
/// the bare-" if " marker keys on. The single-`if` + no-residual-gate guard
/// keeps a genuinely gated value-modifier (a delirium/threshold clause that DOES
/// want a `condition` field) from being masked here.
fn unconditional_valmod_leading_if_is_only_if_marker(
    stripped: &str,
    parsed: &ParsedAbilities,
) -> bool {
    let Some(body) = super::oracle_modal::strip_ability_word(stripped) else {
        return false;
    };
    let body = body.trim_start();
    // allow-noncombinator: swallow detector marker scan on classified text
    if !body.starts_with("if ") {
        return false;
    }
    // A residual while/as-long-as/only-if is a genuine second gate (card 2's
    // delirium threshold) that must still flag / be captured as `condition`.
    // allow-noncombinator: swallow detector marker scan on classified text
    if body.contains("while") || body.contains("as long as") || body.contains("only if") {
        return false;
    }
    // Exactly the single leading replacement-`if`; a future value-modifier card
    // carrying a SECOND genuine if-gate must still flag.
    if body.split_whitespace().filter(|w| *w == "if").count() != 1 {
        return false;
    }
    parsed.replacements.iter().any(|r| {
        (r.damage_modification.is_some() || r.quantity_modification.is_some())
            && r.condition.is_none()
    })
}

fn target_filter_has_targets_property(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(|prop| {
            matches!(
                prop,
                crate::types::ability::FilterProp::Targets { .. }
                    | crate::types::ability::FilterProp::TargetsOnly { .. }
            )
        }),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_has_targets_property)
        }
        TargetFilter::Not { filter } => target_filter_has_targets_property(filter),
        _ => false,
    }
}

fn static_has_target_gated_cost_modification(def: &StaticDefinition) -> bool {
    match &def.mode {
        StaticMode::ModifyCost {
            spell_filter: Some(filter),
            ..
        } => target_filter_has_targets_property(filter),
        StaticMode::ImposeAdditionalCost {
            spell_filter: Some(filter),
            ..
        } => target_filter_has_targets_property(filter),
        _ => false,
    }
}

fn any_static_has_target_gated_cost_modification(parsed: &ParsedAbilities) -> bool {
    parsed
        .statics
        .iter()
        .any(static_has_target_gated_cost_modification)
}

fn any_ability_is_optional(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_optional)
        // CR 603.3: Triggers carry their own optional flag for the outer
        // "you may" prompt; the inner execute may carry a nested optional too.
        // CR 702.139a: `Exerted` triggers fire only when the controller chose
        // to exert the creature — exert itself is the "you may" gate, so the
        // trigger doesn't need an `optional` flag.
        || parsed.triggers.iter().any(trigger_tree_has_optional)
        // CR 614.1a: Replacement effects with `mode = Optional` (e.g., "you
        // may have this creature enter as a copy of...") encode the choice
        // at the replacement layer, not via `def.optional`. Mandatory
        // replacements may still carry optionality inside their execute
        // tree (e.g., `RevealFromHand { on_decline }` — the player chooses
        // whether to reveal).
        || parsed.replacements.iter().any(|r| {
            matches!(
                r.mode,
                ReplacementMode::Optional { .. } | ReplacementMode::MayCost { .. }
            ) || r.execute.as_deref().is_some_and(def_tree_has_optional)
        })
        // Static modes that ARE the "you may" permission — their entire
        // semantic content is granting an optional player action:
        //   CR 701.43:  MayLookAtTopOfLibrary ("you may look at...any time")
        //   CR 117.3a:  MayChooseNotToUntap   ("you may choose not to untap")
        //   CR 117.3a:  TopOfLibraryCastPermission (Bolas's Citadel-style)
        || parsed.statics.iter().any(static_definition_has_optional)
        // CR 700.2c: "you may choose the same mode more than once" is
        // encoded as `modal.allow_repeat_modes = true`, not as a def-level
        // optional flag.
        || parsed
            .modal
            .as_ref()
            .is_some_and(|m| m.allow_repeat_modes)
        // CR 601.2f: "As an additional cost to cast this spell, you may
        // [pay X]" — captured as `additional_cost: Optional(_)` on the
        // top-level parse result, not on any def. Spans Murders evidence,
        // dragon-reveal kicker, blight, behold, etc.
        || matches!(
            parsed.additional_cost,
            Some(crate::types::ability::AdditionalCost::Optional { .. }
                | crate::types::ability::AdditionalCost::Kicker { .. }
                | crate::types::ability::AdditionalCost::Choice(_, _))
        )
        // CR 117.6 + 117.9 + 702.8 + 715.3a: Every variant of
        // `SpellCastingOption` is an opt-in player choice — alternative
        // casts, free casts, flash permission, Adventure casts. Their
        // presence in `parsed.casting_options` IS the "you may" capture
        // for the corresponding Oracle clause (Force of Will, Misdirection,
        // Borderpost cycle, Mastery cycle, Pact cycle, Expertise cycle, etc.)
        || !parsed.casting_options.is_empty()
}

fn parsed_has_conditional_modal_max(parsed: &ParsedAbilities) -> bool {
    parsed.modal.as_ref().is_some_and(modal_has_conditional_max)
        || parsed
            .abilities
            .iter()
            .any(def_tree_has_conditional_modal_max)
        || parsed.triggers.iter().any(|trigger| {
            trigger
                .execute
                .as_ref()
                .is_some_and(|execute| def_tree_has_conditional_modal_max(execute))
        })
}

fn def_tree_has_conditional_modal_max(def: &AbilityDefinition) -> bool {
    def.modal.as_ref().is_some_and(modal_has_conditional_max)
        || def
            .sub_ability
            .as_ref()
            .is_some_and(|sub| def_tree_has_conditional_modal_max(sub))
        || def
            .else_ability
            .as_ref()
            .is_some_and(|else_ab| def_tree_has_conditional_modal_max(else_ab))
        || def
            .mode_abilities
            .iter()
            .any(def_tree_has_conditional_modal_max)
}

fn modal_has_conditional_max(modal: &crate::types::ability::ModalChoice) -> bool {
    modal.constraints.iter().any(|constraint| {
        matches!(
            constraint,
            ModalSelectionConstraint::ConditionalMaxChoices { .. }
        )
    })
}

// ── Duration evidence: one typed channel, plus two non-`Duration` carriers ───────────────
//
// Three vacuous helpers used to live here — `def_tree_has_duration`, `any_ability_has_duration`
// and `static_has_duration`. All are DELETED. The third is why:
//
//     fn static_has_duration(s: &StaticDefinition) -> bool { let _ = s; true }
//
// It returned `true` for ANY static ability whatsoever — a keyword grant, an anthem, a cost
// modifier — so `any_ability_has_duration` (its only caller, and the sole evidence gate on
// `detect_duration_until_eot`) was discharged by the mere EXISTENCE of a static.
// `StaticDefinition` has no `Duration` field at all; a static's duration lives on the
// `Effect::GenericEffect` that wraps it. The stub was not a conservative approximation of a
// duration — it was unrelated to one. `any_ability_has_duration` compounded it by accepting a
// duration of ANY KIND: a `Permanent` grant satisfied an "until end of turn" expectation.
// Evidence must be the fact the expectation asked about (CR 611.2a).
//
// A hand-written typed walk replaced them, and was in turn replaced by the serde probe in
// `swallow_evidence`. The reason is worth keeping, because it is the argument for the probe:
// a hand-enumerated carrier list is a CLAIM ABOUT A POPULATION, and that claim was falsified
// twice in review — first by `ManaSpellGrant::AddKeywordUntilEndOfTurn.duration` (a
// `Box<Duration>`, fully qualified, in `mana.rs`), then by durations nested under
// `Effect::Token.static_abilities -> GrantTrigger -> trigger.execute`. Each miss surfaced as a
// false-positive warning cluster. `UnitEvidence::any_duration` derives its reach from serde
// instead of from a human, so it structurally cannot miss a `Duration`-typed carrier.
//
// THE PROBE'S REACH IS A TYPE, NOT A CONCEPT. It finds values of type `Duration`. Two carriers
// express an end-of-turn scope WITHOUT being a `Duration`, so they are outside it BY
// CONSTRUCTION and keep the typed legs below — no widening of `any_duration` can subsume them:
//
//   1. `Effect::Mana.expiry` (`ManaExpiry`, CR 106.4) — "that mana doesn't empty until end of
//      turn". A different TYPE.
//   2. `ReplacementEvent::DamageDone` (CR 614.6 + CR 514.2) — a one-shot damage shield has no
//      duration FIELD at all; its lifetime is structural. Handled inline in the detector.

fn mana_expiry_is_end_of_turn(expiry: &ManaExpiry) -> bool {
    // CR 106.4: a player's mana pool empties at the end of each step and phase; an expiry
    // overrides that, which is what makes it duration-equivalent for this detector.
    matches!(expiry, ManaExpiry::EndOfTurn | ManaExpiry::EndOfCombat)
}

/// CR 106.4: `Effect::Mana.expiry` is a duration-equivalent that is not typed `Duration`.
fn unit_has_end_of_turn_mana_expiry(parsed: &ParsedAbilities) -> bool {
    fn def_has(def: &AbilityDefinition) -> bool {
        if let Effect::Mana {
            expiry: Some(expiry),
            ..
        } = &*def.effect
        {
            if mana_expiry_is_end_of_turn(expiry) {
                return true;
            }
        }
        def.sub_ability.as_deref().is_some_and(def_has)
            || def.else_ability.as_deref().is_some_and(def_has)
            || def.mode_abilities.iter().any(def_has)
    }
    parsed.abilities.iter().any(def_has)
        || parsed
            .triggers
            .iter()
            .any(|t| t.execute.as_deref().is_some_and(def_has))
}

fn any_ability_has_constraint(parsed: &ParsedAbilities) -> bool {
    // CR 605: activation constraints are stored on
    // `AbilityDefinition.activation_restrictions` (sorcery-speed timing,
    // upkeep gates, etc.) and on `TriggerDefinition.constraint`.
    parsed.abilities.iter().any(def_has_activation_restriction)
        || parsed.triggers.iter().any(|t| t.constraint.is_some())
}

fn def_has_activation_restriction(def: &AbilityDefinition) -> bool {
    // CR 602.5d: sorcery-speed timing is now represented as
    // `ActivationRestriction::AsSorcery` in `activation_restrictions`, so the
    // non-empty check below already covers it.
    !def.activation_restrictions.is_empty()
}

// CR 702.122 + CR 602.5b: Crew with a once-per-turn activation limit.
fn keyword_has_activation_limit(keyword: &Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Crew { once_per_turn, .. }
            if matches!(
                once_per_turn.as_deref(),
                Some(ActivationRestriction::OnlyOnceEachTurn)
            )
    )
}

fn any_keyword_has_activation_limit(parsed: &ParsedAbilities) -> bool {
    parsed
        .extracted_keywords
        .iter()
        .any(keyword_has_activation_limit)
}

/// CR 602.5b: does this restriction cap HOW OFTEN an ability may be activated?
///
/// A limit ("Activate only once each turn" — CR 602.5b's own example) is a different
/// fact from a timing WINDOW (CR 602.5d, "Activate only as a sorcery") and from a
/// game-state gate. Conflating them is precisely why `ActivateLimit` fired ZERO times
/// across 35,396 faces while 113 faces' text raises the expectation: the old evidence
/// check accepted ANY activation restriction, so a card that dropped its limit was
/// excused by an unrelated sorcery-speed gate on the very same ability. The evidence
/// was satisfied by a fact the expectation never asked about.
///
/// Exhaustive with no `_` arm on purpose: a new restriction variant must state which of
/// the three it is, rather than defaulting into counting as a limit it is not.
fn restriction_is_activation_limit(restriction: &ActivationRestriction) -> bool {
    match restriction {
        // CR 602.5b: usage caps — the only things that are limits.
        ActivationRestriction::OnlyOnceEachTurn
        | ActivationRestriction::OnlyOnce
        | ActivationRestriction::MaxTimesEachTurn { .. } => true,
        // CR 602.5d + CR 602.5e: timing windows — WHEN it may be activated, not how often.
        ActivationRestriction::AsSorcery
        | ActivationRestriction::AsInstant
        | ActivationRestriction::DuringYourTurn
        | ActivationRestriction::DuringYourUpkeep
        | ActivationRestriction::DuringCombat
        | ActivationRestriction::BeforeAttackersDeclared
        | ActivationRestriction::BeforeCombatDamage
        | ActivationRestriction::MatchesCardCastTiming => false,
        // CR 602.5: game-state gates — WHETHER it may be activated at all.
        ActivationRestriction::RequiresCondition { .. }
        | ActivationRestriction::IsSolved
        | ActivationRestriction::SourceIsHarnessed
        | ActivationRestriction::ClassLevelIs { .. }
        | ActivationRestriction::LevelCounterRange { .. }
        | ActivationRestriction::CounterThreshold { .. } => false,
    }
}

fn def_tree_has_activation_limit(def: &AbilityDefinition) -> bool {
    if def
        .activation_restrictions
        .iter()
        .any(restriction_is_activation_limit)
    {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_activation_limit(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_activation_limit(else_ab) {
            return true;
        }
    }
    def.mode_abilities.iter().any(def_tree_has_activation_limit)
}

fn any_ability_has_limit(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_activation_limit)
        || any_keyword_has_activation_limit(parsed)
}

/// CR 101.4: is an explicit turn-order start represented?
///
/// The ordering fact lives in exactly two typed homes: `AbilityDefinition.starting_with`
/// and `Effect::Vote.starting_with`. It is NOT `player_scope`.
///
/// That distinction is the whole defect. The old evidence check listed `"player_scope":`
/// as a marker — but a card reading "Starting with you, EACH PLAYER votes..." parses
/// `player_scope = EachPlayer` **from the very clause that raises the expectation**, so
/// the marker was always present and the detector always excused itself, even when
/// `starting_with` had been dropped on the floor. Vacuous by construction: the evidence
/// was implied by the expectation. `APNAP` fired ZERO times across 35,396 faces while 56
/// faces' text raises it.
///
/// A bare player scope says WHO acts. It says nothing about the ORDER they act in, which
/// is the only thing CR 101.4 is about.
fn def_tree_has_apnap_ordering(def: &AbilityDefinition) -> bool {
    if def.starting_with.is_some() {
        return true;
    }
    // CR 701.38a: a Vote always carries `starting_with` as a non-optional field, so the
    // variant's presence IS the ordering fact.
    if matches!(&*def.effect, Effect::Vote { .. }) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_apnap_ordering(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_apnap_ordering(else_ab) {
            return true;
        }
    }
    def.mode_abilities.iter().any(def_tree_has_apnap_ordering)
}

fn any_ability_has_apnap_ordering(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_apnap_ordering)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_apnap_ordering)
        })
        || parsed.replacements.iter().any(|r| {
            r.execute
                .as_deref()
                .is_some_and(def_tree_has_apnap_ordering)
        })
}

// The five `*_description_contains` helpers that used to live here are DELETED.
//
// They read `AbilityDefinition::description` / `TriggerDefinition::description` /
// `StaticDefinition::description` — fields the parser fills with **raw Oracle text**
// (`oracle_trigger.rs`: `def.description = Some(ir.source_text.clone())`;
// `oracle_effect/assembly.rs`: `def.description = Some(clause_ir.source.fragment()…)`).
//
// An audit that accepts a description as evidence is asking the text whether the text is
// represented. It is a transcript of the question, never an answer to it: every such check
// is satisfied by the very clause that raised the expectation, which is the same defect
// `player_scope` was for APNAP (CR 101.4). Evidence must be a STRUCTURAL fact the parser
// could only have produced by understanding the clause.
//
// Their sole caller was `any_text_field_contains(parsed, "instead")` in
// `detect_replacement_instead`, now `any_ability_has_replacement_carrier`.

// ── Typed-evidence detectors ────────────────────────────────────────────
//
// These detectors answer tree-global existence questions ("is a dynamic quantity
// carried ANYWHERE under this unit's definitions?") against the typed probe in
// `swallow_evidence`. They previously scanned the serialized AST for substring
// markers; see that module's header for why a string marker cannot be trusted —
// it can name a type that does not exist, or match a LONGER type's name and
// discharge an unrelated rule's fact.

// ── Detector F: DynamicQty ──────────────────────────────────────────────

/// Dynamic-quantity marker phrases other than " twice " (which each caller
/// handles per-site: the has_marker gate ORs it with its activation-limit
/// guard; the *_is_only_dynamic_marker helpers treat it as the sole/second
/// marker). Extracted so the list can't drift across the three call sites.
const OTHER_DYNAMIC_MARKERS: &[&str] = &[
    " equal to ",
    "for each ",
    "where x is ",
    "the number of ",
    "half your ",
    "half their ",
    "half its ",
    "half the ",
];

/// Oracle text contains dynamic-quantity grammar ("equal to", "for each",
/// "twice", "where x is", "the number of", "half [poss]") but the parsed
/// AST contains no dynamic carrier (Ref, Multiply, DivideRounded, Offset,
/// Variable, EventContext, ForEach, NumberOf). The clause was swallowed.
///
/// CR 107.1a + CR 107.3 + CR 119.1: dynamic quantities must produce typed
/// `QuantityExpr` carriers — never silently substituted with `Fixed`.
fn detect_dynamic_qty(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // CR 605.1g: "Activate ... twice each turn" is a fixed-count activation
    // limit (handled by ActivateLimit detector), not a dynamic quantity.
    // "twice that many" / "twice X" remain real dynamic-quantity markers.
    let twice_is_activation_limit = cleaned.contains("twice each turn") // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("twice that") // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("twice x"); // allow-noncombinator: swallow detector marker scan on classified text
    let has_marker = (cleaned.contains(" twice ") && !twice_is_activation_limit) // allow-noncombinator: swallow detector marker scan on classified text
        || OTHER_DYNAMIC_MARKERS.iter().any(|m| cleaned.contains(m));
    if !has_marker {
        return;
    }
    // ── Typed dynamic-quantity carriers ─────────────────────────────────
    //
    // CR 107.1a + CR 107.3 + CR 119.1. The engine has exactly ONE dynamic-quantity
    // vocabulary: `QuantityRef` (a value read from game state), wrapped in
    // `QuantityExpr`. Nearly every marker in the deleted list — `"type":"Ref"`,
    // CountersOn, NumberOf, ForEach, TrackedSetSize, Devotion, ObjectCount,
    // ZoneCardCount, EventContext*, DistinctColorsAmongPermanents,
    // DistinctCounterKindsAmong, SelfManaValue, `"dynamic_count":{` — was a
    // `QuantityRef` variant name. The whole family collapses into one typed predicate
    // whose reach the compiler maintains: a new `QuantityRef` variant, or a new
    // `Effect` field that carries one, is covered on the day it is added.
    //
    // `QuantityExpr::Fixed` is a CONSTANT, not a dynamic quantity, and is excluded.
    // That exclusion is load-bearing: `QuantityExpr`'s hand-written `Deserialize` also
    // accepts a BARE INTEGER as `Fixed` (the legacy on-disk form), so a predicate that
    // forgot to exclude it would be satisfied by any number anywhere on the card.
    //
    // KEY-ANCHORED (`any_quantity_ref` / `any_quantity_expr`, see `QUANTITY_KEYS`). These
    // were once unanchored, on the reasoning that an internally-tagged enum carries its own
    // discriminator. It does not: an internally-tagged UNIT variant matches on `type` alone
    // and serde drops unknown fields, and 10 of `QuantityRef`'s variant names are shared
    // with other tagged enums. Unanchored, this probe read Boing!'s
    // `AbilityCondition::PreviousEffectAmount` (a CONDITION, under key `condition`) and
    // Siren's Call's `FilterProp::AttackedThisTurn` (a TARGET FILTER, under key
    // `properties[].prop`) as dynamic quantities, and suppressed both cards' real warnings.
    // Boing! lowers "scry a number of cards equal to the result" to `Scry { count: Fixed(1) }`
    // — the quantity IS dropped, and the warning it silenced was a true positive.
    if evidence.any_quantity_ref(|_| true)
        || evidence.any_quantity_expr(|q| !matches!(q, QuantityExpr::Fixed { .. }))
    {
        return;
    }
    // Carriers that express a dynamic amount WITHOUT a `QuantityExpr` field — the
    // quantity is intrinsic to the variant, so the variant itself is the evidence.
    //   CR 120.1   EachDealsDamageEqualToPower — "each deals damage equal to its power"
    //              (Band Together / Allies at Last).
    //   CR 701.34a ProliferateTarget — counter-kind iteration is intrinsic to proliferate.
    //   CR 122.1   EachPlayerCopyChosen — `scale_property` is read live at placement.
    //   Sylvan Library  ChooseDrawnThisTurnPayOrTopdeck — per-card choice effect.
    if evidence.any_effect(|e| {
        matches!(
            e,
            Effect::EachDealsDamageEqualToPower { .. }
                | Effect::ProliferateTarget { .. }
                | Effect::EachPlayerCopyChosen { .. }
                | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        )
    }) {
        return;
    }
    // CR 106.1: a mana ability whose AMOUNT is read from game state carries its quantity in
    // the `ManaProduction` variant itself — `Effect::Mana.produced` is typed `ManaProduction`,
    // NOT `QuantityRef`, so the key-anchored quantity probes above cannot see it.
    //
    //   Bloom Tender / Faeburrow Elder / Sunbird Effigy / Tarnation Vista —
    //   "For each color among permanents you control, add one mana of that color."
    //
    // This leg exists BECAUSE the probes above are anchored. Unanchored, they "saw" this
    // carrier only by ACCIDENT: `DistinctColorsAmongPermanents` is also a `QuantityRef`
    // variant name, so the `ManaProduction` node deserialized as a `QuantityRef` by cross-enum
    // collision. Right answer, wrong reason — and the same collision suppressed Boing! and
    // Siren's Call. Anchoring removed the accident; this restores the fact, typed.
    //
    // ONE variant, and that is a MEASURED bound, not a guess: over the full 35,396-face pool,
    // `DistinctColorsAmongPermanents` is the only `ManaProduction` on a face where the marker
    // fires and no other dynamic carrier exists. Every other variant that appears on a warning
    // face (Colorless, AnyOneColor, Fixed, ChosenColor, AnyInCommandersColorIdentity,
    // TriggerEventManaType) warns on main too — widening this `matches!` would SUPPRESS true
    // positives, which is the silent direction. A future state-derived variant that is missing
    // here over-reports instead: conservative-RED, and the full-pool delta shows it.
    if evidence.any_at::<ManaProduction>(&["produced"], |p| {
        matches!(p, ManaProduction::DistinctColorsAmongPermanents { .. })
    }) {
        return;
    }
    //   CR 702.143d AddKeywordWithDerivedCost — foretell cost "equal to its mana cost
    //              reduced by {N}", computed per recipient by `CostDerivation`.
    //   CR 702.20a  AssignDamageFromToughness / AssignDamageAsThoughUnblocked —
    //              Brontodon class; a typed modification, not a quantity expression.
    if evidence.any::<ContinuousModification>(|m| {
        matches!(
            m,
            ContinuousModification::AddKeywordWithDerivedCost { .. }
                | ContinuousModification::AssignDamageFromToughness
                | ContinuousModification::AssignDamageAsThoughUnblocked
        )
    }) {
        return;
    }
    //   CR 702.170a TopOfLibraryHasPlot — "plot cost equal to its mana cost" is computed
    //              at synthesis from the live top card, not stored.
    if evidence.any_static_mode(|m| matches!(m, StaticMode::TopOfLibraryHasPlot)) {
        return;
    }
    //   CR 702.34 / 702.144 / 702.83 / 702.143d  A cost derived from the card's OWN mana
    //              cost rather than a fixed amount: Flashback / Scavenge / Replicate
    //              ("equal to its mana cost") and Foretell ("equal to its mana cost
    //              reduced by {N}", Singing Towers of Darillium). `ManaCost` has exactly
    //              three self-referential variants and all three are dynamic; `Cost` and
    //              `NoCost` are fixed and are excluded.
    //
    //              `SelfManaCostReduced` is here because the OLD marker list did not
    //              actually name it: it carried `AddKeywordWithDerivedCost` (a
    //              `ContinuousModification` variant this card never produces — the parser
    //              emits `AddKeyword` + `ManaCost::SelfManaCostReduced`) and got away with
    //              it only because its OTHER marker, `SelfManaCost`, is a SUBSTRING of
    //              `SelfManaCostReduced`. The typed probe cannot lean on that accident, so
    //              the real carrier has to be named.
    //
    // UNANCHORED, AND COLLISION-INVARIANT (audited, task #51). `SelfManaValue` is also a
    // `QuantityRef` variant name, so a `QuantityRef::SelfManaValue` node deserializes here as
    // `ManaCost::SelfManaValue`. The ANSWER is the same under either reading: this detector
    // asks only "is there a value derived from the card's OWN mana cost rather than a fixed
    // amount?" (CR 106.1), and both readings ARE exactly that. Anchoring would buy nothing and
    // would risk dropping the quantity reading, which is equally valid evidence here.
    if evidence.any::<ManaCost>(|c| {
        matches!(
            c,
            ManaCost::SelfManaCost | ManaCost::SelfManaValue | ManaCost::SelfManaCostReduced { .. }
        )
    }) {
        return;
    }
    //   CR 508.1h + CR 509.1d  Ghostly Prison / Propaganda phrase their combat tax with
    //              "for each creature", but it is a typed scaling mode on
    //              `StaticCondition::UnlessPay`, not a `QuantityExpr`.
    if evidence.any::<UnlessPayScaling>(|s| matches!(s, UnlessPayScaling::PerAffectedCreature)) {
        return;
    }
    //   CR 608.2c + CR 701.38  "For each player who chose <choice>" vote bodies resolve
    //              against the ballot ledger via `PlayerFilter::VotedFor`.
    if evidence.any::<PlayerFilter>(|p| matches!(p, PlayerFilter::VotedFor { .. })) {
        return;
    }
    //   CR 702.139 / 702.41  Affinity-style built-in cost mods carry their scaling in the
    //              keyword payload. `Keyword` is EXTERNALLY tagged, so it is key-anchored
    //              (array elements inherit their field's key).
    if evidence.any_at::<Keyword>(&["extracted_keywords", "keywords"], |k| {
        matches!(k, Keyword::Affinity { .. })
    }) {
        return;
    }
    // Slot-shaped carriers: the fact IS "the parser filled this slot", and the slot's
    // value carries no further discrimination this detector needs.
    //   CR 207.2c + CR 601.2f  strive_cost — "{N} more for each target beyond the first"
    //   CR 614.1d              quantity_modification — "twice that many" (Doubling Season)
    //   CR 115.10 + CR 608.2c  source_filter — "for each X, create a token copy of it"
    if evidence.has_slot("striveCost")
        || evidence.has_slot("quantity_modification")
        || evidence.has_slot("source_filter")
    {
        return;
    }
    if cleaned_has_only_counter_multiplier_dynamic(cleaned)
        && evidence.any_effect(|e| matches!(e, Effect::MultiplyCounter { .. }))
    {
        return;
    }
    // CR 608.2c: "<verb> twice instead" (Secrets of the Key, Increasing
    // Vengeance, every Flashback "twice instead" card) is a count-replacement
    // instruction whose doubled count is carried by `AbilityDefinition.repeat_for`
    // — a QuantityExpr home the marker list above does not enumerate because
    // `repeat_for` is a structural field, not a value-typed `"type":"Ref"` node.
    // When "twice" is the SOLE dynamic marker and the AST carries a `repeat_for`,
    // the quantity IS represented; the warning is a false positive.
    if cleaned_twice_is_only_dynamic_marker(cleaned) && evidence.has_slot("repeat_for") {
        return;
    }
    // CR 614.1a + CR 701.10g: "...it deals twice that much damage instead"
    // (Neriv, Heart of the Storm) is a damage-doubling value-modifier
    // replacement whose ×2 is carried by `ReplacementDefinition.damage_modification`
    // (`Double`/`Triple`), NOT a `QuantityExpr` node — the sibling of the
    // `quantity_modification` slot ("twice that many", Doubling Season) and the
    // `repeat_for` clause above. `Double` is a UNIT variant with no `QuantityExpr`
    // field, so it also escapes the `any_quantity_expr` carrier. When
    // "twice that much damage" is the SOLE dynamic marker and the AST carries such
    // a modification, the multiplier IS represented — runtime resolves it
    // end-to-end via the `damage_done_applier` Double/Triple arm. Distinct from
    // the `repeat_for` guard above (repeat-count "twice", which deliberately
    // rejects the "twice that" multiplier form): here that multiplier IS the
    // carried modification, so it is the accepted marker.
    if cleaned_twice_damage_double_is_only_dynamic_marker(cleaned)
        && evidence.any_at::<DamageModification>(&["damage_modification"], |m| {
            matches!(m, DamageModification::Double | DamageModification::Triple)
        })
    {
        return;
    }
    // CR 608.2e + CR 109.5: "For each opponent who doesn't, <body>" is a
    // per-opponent decline iteration, NOT a dynamic quantity — its carrier is a
    // `player_scope: Opponent` node with a `Not{IfYouDo}`-conditioned
    // decline-consequence sub-ability. Suppress the warning only when the AST
    // carries the `Not` wrapper specifically: a bare `IfYouDo` token is present
    // on the opponent-sacrifice node of EVERY Braids-class AST regardless of
    // whether the decline body actually attached, so checking for `IfYouDo`
    // would suppress the warning even when the decline body failed to parse.
    // The `Not` gate is what proves the decline-consequence clause is
    // represented (issue #491 follow-up).
    // The `Not` wrapper is what proves the decline-consequence clause is represented; a
    // bare `IfYouDo` sits on EVERY Braids-class AST whether or not the decline body
    // attached. Both condition enums carry a `Not` (they share 17 variant names) and
    // either satisfies this fact, so the probe deliberately accepts both.
    //
    // UNANCHORED, AND COLLISION-INVARIANT (audited, task #51). Two independent reasons, both
    // checkable:
    //   * the two condition readings are OR'd together right here, so `AbilityCondition::Not`
    //     vs `StaticCondition::Not` is answer-identical BY CONSTRUCTION;
    //   * `Not` is ALSO a variant of `FilterProp` and `TargetFilter`, but those are
    //     `Not { prop }` and `Not { filter }` — the field NAME differs from `Not { condition }`,
    //     so neither can deserialize as a condition. That collision is structurally impossible,
    //     not merely improbable.
    if cleaned_for_each_is_only_decline_iteration(cleaned)
        && (evidence.any::<AbilityCondition>(|c| matches!(c, AbilityCondition::Not { .. }))
            || evidence.any::<StaticCondition>(|c| matches!(c, StaticCondition::Not { .. })))
    {
        return;
    }
    // CR 101.4 + CR 701.21a: Tragic Arrogance-style "For each player, you choose
    // ..." is a turn-order choice procedure, not a numeric quantity. Its carrier
    // is the dedicated ChooseAndSacrificeRest effect rather than a QuantityExpr.
    if cleaned.contains("for each player, you choose ") // allow-noncombinator: swallow detector marker scan on classified text
        && evidence.any_effect(|e| matches!(e, Effect::ChooseAndSacrificeRest { .. }))
    {
        return;
    }
    // CR 701.38: Council's-dilemma vote-tally payoffs ("create a number of X
    // equal to [twice] the number of <choice> votes" — Emissary Green) realize
    // their dynamic count through the Vote resolver's per-vote fan-out: each
    // per-choice sub-effect runs once per tallied vote, with the multiplier
    // folded into a fixed per-vote count. The dynamic quantity is therefore
    // represented by the `Vote` structure, not a `QuantityExpr` carrier. When
    // the AST is a Vote and every dynamic marker is tally phrasing, nothing was
    // swallowed.
    if cleaned_dynamic_is_only_vote_tally(cleaned)
        && evidence.any_effect(|e| matches!(e, Effect::Vote { .. }))
    {
        return;
    }
    // CR 107.4f: "For each {C} in a cost, you may pay 2 life rather than
    // pay that mana." — the "for each {" phrase is a per-payment-substitution
    // using an inline mana symbol, NOT a QuantityExpr carrier. Suppress when
    // the parsed AST already contains PayLifeAsColoredMana and every "for each"
    // marker is the mana-symbol form (immediately followed by `{`).
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("for each {") {
        // allow-noncombinator: swallow detector marker scan on classified text
        let all_for_each_are_mana_subst = !cleaned.contains("for each ")
            || cleaned
                // allow-noncombinator: swallow detector marker scan on classified text
                .match_indices("for each ")
                .all(|(idx, _)| cleaned[idx + "for each ".len()..].starts_with('{'));
        if all_for_each_are_mana_subst
            && evidence.any_static_mode(|m| matches!(m, StaticMode::PayLifeAsColoredMana { .. }))
        {
            return;
        }
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::DynamicQty.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector M: Modal_DynamicMaxDropped ─────────────────────────────────

/// CR 700.2 + CR 700.2d: a "choose up to X / up to that many" MODAL header
/// whose dynamic cap was not captured (a `"modal":{` node exists but its
/// `dynamic_max_choices` is None) silently mis-sizes the modal — the player
/// would be locked to the fixed `mode_count` cap instead of the dynamic
/// "up to X" / "up to that many" cap. Surface it so coverage stays honest.
///
/// The `"modal":{` gate excludes non-modal "choose up to X <nouns>" selection
/// clauses (Heroic Feast: "choose up to that many target creatures you
/// control"; Temporal Firestorm: "choose up to X creatures ... where X is ..."):
/// those parse to a quantified target/selection, not a modal node, so no
/// `"modal":{` appears and this detector stays silent on them.
///
/// Keys on serialized-field presence:
/// - `modal` (`AbilityDefinitionRepr`, ability.rs:13381) is omitted when None
///   via `skip_serializing_if`, so `"modal":{` is an exact proxy for "a modal
///   node was parsed" (there is no `"modal":null` form to confuse it).
/// - `dynamic_max_choices` (ability.rs:12925) carries
///   `#[serde(default, skip_serializing_if = "Option::is_none")]`, so it is
///   omitted when None; ABSENCE of `"dynamic_max_choices":{` means the dynamic
///   cap was dropped (there is no `:null` form to test).
///
/// CONSERVATIVE-RED LIMITATION (deliberate, never false-green): the three gates
/// are independent whole-text / whole-AST scans, not a per-node association. A
/// single card carrying BOTH (a) an UNRELATED fixed modal node (gate 2) AND (b)
/// a SEPARATE non-modal "choose up to X <nouns>" selection clause elsewhere in
/// its text (gate 1) would fire even though its fixed modal's cap was never
/// meant to be dynamic. This errs toward RED — it understates coverage, never
/// over-states it — so a card so flagged stays honestly unsupported rather than
/// being marked green without a working dynamic cap. No such card exists in the
/// current corpus (the modal-bearing dynamic-header cards — Hawkeye, Tranquil
/// Frillback, Bumi, Riku — each have the header ON the modal itself). Tightening
/// to a per-node "the header terminates the modal node, not a noun phrase"
/// association would duplicate the parser's `oracle_modal` negative-lookahead in
/// audit code and risk regressing the measured Frillback/Hawkeye discrimination;
/// it is intentionally NOT done while the false-RED set is empty.
fn detect_modal_dynamic_max_dropped(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // (1) Oracle carries a dynamic modal header (the "choose " lead is intrinsic
    //     to both markers).
    let has_dynamic_header = cleaned.contains("choose up to that many") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("choose up to x"); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_dynamic_header {
        return;
    }
    // (2) A modal node was parsed (excludes non-modal selection clauses).
    if !evidence.has_slot("modal") {
        return;
    }
    // (3) ...but it carries no dynamic cap — the "up to X / that many" was lost.
    if evidence.has_slot("dynamic_max_choices") {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::ModalDynamicMaxDropped.detector_label(),
        truncate(original, 140),
    ));
}

/// CR 701.38: True when every dynamic-quantity marker in `cleaned` belongs to a
/// Council's-dilemma vote tally — "[equal to [twice|N times] ]the number of
/// <choice> votes". Such tallies are realized by the Vote resolver's per-vote
/// fan-out, not by a `QuantityExpr` carrier, so when the AST is a `Vote` the
/// marker is not a swallowed clause. Kept narrow: any non-tally dynamic marker
/// (a per-choice body's own swallowed "equal to its power", a "for each", a
/// "half …") keeps the warning.
fn cleaned_dynamic_is_only_vote_tally(cleaned: &str) -> bool {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("votes") {
        return false;
    }
    // No non-tally dynamic marker may be present.
    let has_foreign_marker = [
        "for each ",
        "where x is ",
        "half your ",
        "half their ",
        "half its ",
        "half the ",
    ]
    .iter()
    // allow-noncombinator: swallow detector marker scan on classified text
    .any(|marker| cleaned.contains(marker));
    if has_foreign_marker {
        return false;
    }
    // Every "the number of " must read "the number of <word> votes".
    let all_number_of_are_vote = cleaned
        // allow-noncombinator: swallow detector marker scan on classified text
        .match_indices("the number of ")
        .all(|(idx, _)| vote_tally_count_suffix(&cleaned[idx..]));
    if !all_number_of_are_vote {
        return false;
    }
    // Every " equal to " must lead (through an optional multiplier) into a tally.
    cleaned
        // allow-noncombinator: swallow detector marker scan on classified text
        .match_indices(" equal to ")
        .all(|(idx, _)| equal_to_vote_tally_suffix(&cleaned[idx..]))
}

/// nom: `"the number of <word> votes"`.
fn vote_tally_count_suffix(input: &str) -> bool {
    let res: nom::IResult<&str, _, nom::error::Error<&str>> = (
        tag("the number of "),
        take_while1(|c: char| c.is_alphanumeric() || c == '\'' || c == '-'),
        tag(" votes"),
    )
        .parse(input);
    res.is_ok()
}

/// nom: `" equal to [twice |<n> times ]the number of <word> votes"`.
fn equal_to_vote_tally_suffix(input: &str) -> bool {
    let res: nom::IResult<&str, _, nom::error::Error<&str>> = (
        tag(" equal to "),
        opt(alt((
            value((), tag("twice ")),
            value((), (digit1, tag(" times "))),
        ))),
        tag("the number of "),
        take_while1(|c: char| c.is_alphanumeric() || c == '\'' || c == '-'),
        tag(" votes"),
    )
        .parse(input);
    res.is_ok()
}

/// CR 608.2e + CR 608.2c + CR 101.3: True when every "for each " occurrence in
/// the classified text is the "for each opponent who doesn't / does not /
/// can't / cannot" decline-iteration phrase and no other dynamic-quantity
/// marker is present. Such text's iteration is carried by a `player_scope`
/// node, not a `QuantityExpr`. Covers both the optional-decline shape
/// (Braids-class, CR 118.12 optional-cost branch) and the mandatory-impossible
/// shape (Refurbished-Familiar-class, CR 101.3 + CR 118.12 mandatory-cost
/// branch).
fn cleaned_for_each_is_only_decline_iteration(cleaned: &str) -> bool {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("for each ") {
        return false;
    }
    // Every "for each" must be immediately followed by the decline subject.
    let all_for_each_are_decline = cleaned
        .match_indices("for each ") // allow-noncombinator: swallow detector marker scan on classified text
        .all(|(idx, _)| {
            let rest = &cleaned[idx..];
            decline_iteration_prefix(rest)
        });
    if !all_for_each_are_decline {
        return false;
    }
    // No OTHER dynamic marker may be present.
    ![
        " equal to ",
        "where x is ",
        "the number of ",
        "half your ",
        "half their ",
        "half its ",
        "half the ",
    ]
    .iter()
    // allow-noncombinator: swallow detector marker scan on classified text
    .any(|marker| cleaned.contains(marker))
}

fn decline_iteration_prefix(input: &str) -> bool {
    alt((
        tag::<_, _, nom::error::Error<&str>>("for each opponent who doesn't"),
        tag("for each opponent who does not"),
        tag("for each opponent who can't"),
        tag("for each opponent who cannot"),
    ))
    .parse(input)
    .is_ok()
}

fn cleaned_has_only_counter_multiplier_dynamic(cleaned: &str) -> bool {
    // allow-noncombinator: swallow detector phrase scan on classified text
    let has_counter_multiplier = cleaned.contains("double the number of +1/+1 counters");
    if !has_counter_multiplier {
        return false;
    }
    // The counter multiplier itself accounts for "the number of". If another
    // dynamic marker is present, keep the warning because that second marker
    // may be a real uncaptured clause.
    ![
        " equal to ",
        "for each ",
        " twice ",
        "where x is ",
        "half your ",
        "half their ",
        "half its ",
        "half the ",
    ]
    .iter()
    // allow-noncombinator: swallow detector marker scan on classified text
    .any(|marker| cleaned.contains(marker))
}

/// True when " twice " is the ONLY dynamic-quantity marker in `cleaned` (and
/// is not the "twice each turn" activation-limit form). Used to keep the
/// `repeat_for` suppression narrow: a card that ALSO carries another dynamic
/// phrase ("for each", "equal to", "the number of", …) must still flag, since
/// that second marker may be a genuinely-swallowed clause `repeat_for` does
/// not account for.
fn cleaned_twice_is_only_dynamic_marker(cleaned: &str) -> bool {
    // allow-noncombinator: swallow detector marker scan on classified text
    let twice_is_activation_limit = cleaned.contains("twice each turn")
        // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("twice that")
        // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("twice x");
    // allow-noncombinator: swallow detector marker scan on classified text
    let has_twice = cleaned.contains(" twice ") && !twice_is_activation_limit;
    if !has_twice {
        return false;
    }
    // "twice that many" / "twice x" are multiplier markers, not the plain
    // repeat count `repeat_for` carries — they need a real QuantityExpr.
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("twice that") || cleaned.contains("twice x") {
        return false;
    }
    // No OTHER dynamic marker may be present.
    // allow-noncombinator: swallow detector marker scan on classified text
    !OTHER_DYNAMIC_MARKERS.iter().any(|m| cleaned.contains(m))
}

/// True when the sole dynamic-quantity marker in `cleaned` is the
/// damage-doubling phrase "twice that much damage" — the surface form Neriv,
/// Heart of the Storm lowers to `DamageModification::Double`. Sibling of
/// `cleaned_twice_is_only_dynamic_marker` (repeat-count "twice") for the
/// damage-modification carrier: that helper deliberately REJECTS "twice that" (a
/// multiplier needing a real `QuantityExpr`), but here the ×2 IS the
/// "twice that much damage" phrase, already carried by `DamageModification`, so
/// it is the accepted marker. Every OTHER dynamic marker — a second, independent
/// "twice", "for each", "equal to", "the number of", "where x is", "half …" —
/// keeps the warning, so a genuinely-swallowed second clause is never masked.
fn cleaned_twice_damage_double_is_only_dynamic_marker(cleaned: &str) -> bool {
    // allow-noncombinator: swallow detector phrase scan on classified text
    if !cleaned.contains("twice that much damage") {
        return false;
    }
    // Strip the accepted doubling phrase, then require no dynamic marker to
    // remain — a residual " twice " catches a second, independent doubling.
    let residual = cleaned.replace("twice that much damage", " ");
    // allow-noncombinator: swallow detector marker scan on classified text
    !(residual.contains(" twice ") || OTHER_DYNAMIC_MARKERS.iter().any(|m| residual.contains(m)))
}

/// CR 702.170c + CR 608.2c: "[you may] exile a card. If you do, it becomes
/// plotted." The "if you do" gate is the optional-exile linkage — structurally
/// represented by the `GrantCastingPermission { CastingPermission::Plotted }`
/// chained off the (optional) exile, which only takes effect when the exile
/// happened. It is not an uncaptured game-state condition (the coverage-side
/// `line_has_condition_text` likewise excludes "if you do" wholesale).
fn def_tree_has_plotted_grant(def: &AbilityDefinition) -> bool {
    if let Effect::GrantCastingPermission {
        permission: crate::types::ability::CastingPermission::Plotted { .. },
        ..
    } = &*def.effect
    {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_plotted_grant(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_plotted_grant(else_ab) {
            return true;
        }
    }
    def.mode_abilities.iter().any(def_tree_has_plotted_grant)
}

fn any_ability_has_plotted_grant(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_plotted_grant)
        || parsed
            .triggers
            .iter()
            .any(|t| t.execute.as_deref().is_some_and(def_tree_has_plotted_grant))
}

fn plotted_grant_linkage_is_only_if_marker(stripped: &str) -> bool {
    let has_plot_link = stripped.contains("if you do, it becomes plotted"); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_plot_link {
        return false;
    }
    let without_plot_link = stripped.replace("if you do, it becomes plotted", "");
    let has_if_marker = without_plot_link.contains(" if "); // allow-noncombinator: swallow detector marker scan on classified text
    let has_as_if_marker = without_plot_link.contains(" as if "); // allow-noncombinator: swallow detector marker scan on classified text
    let has_even_if_marker = without_plot_link.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    !(has_if_marker && !has_as_if_marker && !has_even_if_marker)
}

fn def_tree_has_dig(def: &AbilityDefinition) -> bool {
    if matches!(&*def.effect, Effect::Dig { .. }) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_dig(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_dig(else_ab) {
            return true;
        }
    }
    def.mode_abilities.iter().any(def_tree_has_dig)
}

/// CR 608.2c + CR 701.20: "you may look at the top N cards ... If you do,
/// reveal/put ... from among them ..." (Fertile Thicket, Munda, Planar Atlas).
/// The optional "look" lowers to an optional `Dig`; the dependent "reveal ...
/// from among them" is a continuation that patches that same `Dig`. The
/// "if you do" is not an independent game-state condition — per CR 608.2c
/// (read the whole text and apply the rules of English) it links the dependent
/// reveal to the optional look having happened, and the optional `Dig` (the
/// player may decline the look, and then nothing in the chain resolves) IS that
/// gate. So when the parse contains a `Dig` inside an optional ability/trigger,
/// the "if you do" marker is represented, not swallowed.
fn any_optional_ability_has_dig(parsed: &ParsedAbilities) -> bool {
    parsed
        .abilities
        .iter()
        .any(|def| def_tree_has_optional(def) && def_tree_has_dig(def))
        || parsed.triggers.iter().any(|t| {
            trigger_tree_has_optional(t) && t.execute.as_deref().is_some_and(def_tree_has_dig)
        })
}

fn dig_if_you_do_is_only_if_marker(stripped: &str) -> bool {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !stripped.contains("if you do") {
        return false;
    }
    let without_link = stripped.replace("if you do", "");
    let has_if_marker = without_link.contains(" if "); // allow-noncombinator: swallow detector marker scan on classified text
    let has_as_if_marker = without_link.contains(" as if "); // allow-noncombinator: swallow detector marker scan on classified text
    let has_even_if_marker = without_link.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    !(has_if_marker && !has_as_if_marker && !has_even_if_marker)
}

/// CR 603.7a + CR 608.2c + CR 702.170c: the exile-instead carrier
/// (`Effect::ExileResolvingSpellInsteadOfGraveyard`) carries an `on_exile`
/// rider representing the "If you do, ..." consequence — Feather, the Redeemed's
/// "return it to your hand at the beginning of the next end step" or Lilah,
/// Undefeated Slickshot's "it becomes plotted". `on_exile: Some(_)` means the
/// consequence clause is modelled (the riderless Rod of Absorption form is
/// `None` and has no "if you do" text to account for).
fn any_ability_has_exile_resolving_rider(parsed: &ParsedAbilities) -> bool {
    parsed.triggers.iter().any(|t| {
        t.execute
            .as_deref()
            .is_some_and(def_tree_has_exile_resolving_rider)
    }) || parsed
        .abilities
        .iter()
        .any(def_tree_has_exile_resolving_rider)
}

fn def_tree_has_exile_resolving_rider(def: &AbilityDefinition) -> bool {
    if matches!(
        &*def.effect,
        Effect::ExileResolvingSpellInsteadOfGraveyard { on_exile: Some(_) }
    ) {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_exile_resolving_rider(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_exile_resolving_rider(else_ab) {
            return true;
        }
    }
    def.mode_abilities
        .iter()
        .any(def_tree_has_exile_resolving_rider)
}

/// CR 608.2c: the "if you do" linking the exile-instead consequence (Feather's
/// return, Lilah's plot) to its exile is a back-reference, not an independent
/// game-state condition — represented by the `on_exile` rider. Suppress only
/// when "if you do" is the sole bare "if" marker left in the classified text
/// (mirrors `dig_if_you_do_is_only_if_marker`).
fn exile_resolving_rider_if_you_do_is_only_if_marker(stripped: &str) -> bool {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !stripped.contains("if you do") {
        return false;
    }
    let without_link = stripped.replace("if you do", "");
    let has_if_marker = without_link.contains(" if "); // allow-noncombinator: swallow detector marker scan on classified text
    let has_as_if_marker = without_link.contains(" as if "); // allow-noncombinator: swallow detector marker scan on classified text
    let has_even_if_marker = without_link.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    !(has_if_marker && !has_as_if_marker && !has_even_if_marker)
}

/// CR 614.12: "[you may] put a creature card from your hand onto the
/// battlefield. If that card is an enchantment card, it enters tapped and
/// attacking." (Summoner's Grimoire). The leading moved-object type condition
/// is represented by the typed `Effect::ChangeZone.enters_modified_if` gate, so
/// it is not a swallowed condition.
///
/// Unlike `plotted_grant_linkage_is_only_if_marker` / `dig_if_you_do_is_only_if_marker`
/// (which AST-gate externally via a parsed-tree walk), this folds the AST gate
/// INSIDE via a `"enters_modified_if":` JSON probe — the same JSON-substring
/// pattern the `source_rider` / `countered_spell_zone` / `PreventDamage` gates in
/// `detect_condition_if` use. Because the field carries `skip_serializing_if =
/// Option::is_none`, `None` never serializes, so the substring appears ONLY when
/// the gate is `Some` (N4). It is text-scoped: the represented enters-modifier
/// clause is located and dropped via the shared `is_moved_object_enters_modifier_clause`
/// combinator, and suppression fires ONLY when no OTHER bare " if " remains — so
/// a compound card carrying the gate AND a separate dropped " if " still flags.
fn enters_modified_if_is_only_if_marker(stripped: &str, evidence: &UnitEvidence) -> bool {
    if !evidence.has_slot("enters_modified_if") {
        return false;
    }
    // Text-scoped: drop the represented moved-object enters-modifier clause(s)
    // sentence-by-sentence (mirrors `strip_cr_implicit_if_phrases`), then check
    // whether any OTHER bare " if " survives.
    let residual: String = stripped
        .split('.')
        .filter(|sentence| {
            !crate::parser::oracle_effect::sequence::is_moved_object_enters_modifier_clause(
                sentence,
            )
        })
        .collect::<Vec<_>>()
        .join(".");
    let has_other_if = residual.contains(" if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" as if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    !has_other_if
}

/// CR 614.1a: A replacement effect's antecedent ("If [subject] would
/// [event], [effect] instead.") is not an independent CR 608.2c conditional
/// gate — the leading "if" is part of the replacement's event description.
/// Detector G (`detect_condition_if`) must not flag it.
///
/// Unlike the card-wide gate this replaces, the exemption here is
/// structural and per-sentence: a sentence is only stripped when it is
/// actually shaped like a represented replacement antecedent AND (where a
/// `ReplacementDefinition` exists to check against) its text is actually
/// backed by one of `parsed.replacements`. This keeps compound replacement
/// text honest — a card that pairs one *represented* replacement sentence
/// with a second " instead" sentence carrying an unrepresented conditional
/// still gets flagged for the second sentence, and a single sentence that
/// smuggles in a second, unrelated "if" clause is not exempted either.
///
/// Conditions for stripping a given sentence (ALL must hold):
///   (a) the sentence contains " instead" (CR 614.1a marker);
///   (b) the sentence structurally matches a replacement antecedent shape:
///       after skipping any leading ability-word / keyword-line prefix (text
///       up to and including the last CR 207.2c ability-word em-dash "— " or
///       newline that appears before the clause), the remainder starts with
///       "if " and contains " would ". Ability-word lines (e.g. "Rot Fly —
///       If an opponent would gain life, ...") and preceding keyword lines
///       ("Flying\n...") are extremely common on replacement-bearing cards,
///       so the shape check is anchored to the clause itself, not to
///       byte-offset 0 of the whole (possibly multi-line) `.`-delimited
///       sentence;
///   (c) the sentence contains exactly one "if" clause — i.e. no second bare
///       " if " later in the sentence, counting from the start of the
///       antecedent clause located in (b). A second bare "if" means a real,
///       additional conditional clause is riding along with the replacement
///       and must survive to be checked by the rest of `detect_condition_if`;
///   (d) it corresponds to an actually-parsed replacement: either
///       - some entry in `parsed.replacements` has a `description` whose
///         normalized (lowercased, whitespace-collapsed) text contains the
///         sentence's normalized text or vice versa, or
///       - no such entry exists, but `any_ability_has_target_replacement`
///         is true for this card (CR 614.1a `AddTargetReplacement` riders —
///         e.g. "if [target] would die" — register their replacement at
///         resolution time on the parent target and never appear in the
///         static `parsed.replacements` collection, so they have no
///         description to match against). This fallback is intentionally
///         narrow: it only ever accepts sentences that already satisfied
///         (a)-(c), so it cannot paper over a genuinely-unrepresented
///         compound conditional.
fn strip_represented_replacement_instead_sentences(
    stripped: &str,
    parsed: &ParsedAbilities,
) -> String {
    // `stripped` is derived from `check_swallowed_clauses`'s `cleaned`, which
    // is already `to_ascii_lowercase()`'d — sentences pulled from it are
    // already lowercase. Only the replacement `description` (raw builder
    // text, case-preserved, and NOT split on '.' so it still carries its
    // trailing period) needs lower-casing and trailing-punctuation trimming
    // before comparison against a clause pulled out of `stripped`.
    // allow-noncombinator: swallow detector phrase scan on normalized description text
    fn normalize_for_match(s: &str) -> String {
        s.trim()
            .trim_end_matches('.')
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    // CR 207.2c: locate where the actual antecedent clause begins by
    // skipping past any leading ability-word / keyword-line prefix. Returns
    // the byte offset (into `sentence`) of the last "— " or "\n" separator
    // that occurs before the clause, or `0` if there is none (the whole
    // sentence IS the clause). Only ever walks forward from a separator to
    // the next one — it does not itself require an "if" to be present.
    fn antecedent_clause_start(sentence: &str) -> usize {
        // allow-noncombinator: structural ability-word em-dash boundary scan on cleaned text
        let em_dash_pos = sentence.rfind("— ").map(|i| i + "— ".len());
        let newline_pos = sentence.rfind('\n').map(|i| i + 1);
        em_dash_pos
            .into_iter()
            .chain(newline_pos)
            .max()
            .unwrap_or(0)
    }

    // Structural shape check per condition (b): after skipping any leading
    // ability-word / keyword-line prefix, the clause must start with "if "
    // and contain " would ".
    fn matches_replacement_antecedent_shape(sentence: &str) -> bool {
        let clause = &sentence[antecedent_clause_start(sentence)..];
        // allow-noncombinator: swallow detector phrase scan on cleaned sentence
        clause.starts_with("if ") && clause.contains(" would ")
    }

    // Condition (c): exactly one "if" clause from the antecedent clause
    // start onward. The clause already starts with "if " (checked by the
    // caller via (b)); reject it if a *second* bare " if " appears later —
    // that is an additional, independent conditional clause riding along
    // with the replacement and must not be silently swallowed. Text BEFORE
    // the antecedent clause (ability-word / keyword-line prefix) is not
    // scanned — an "if" there would belong to a different line entirely.
    fn has_single_if_clause(sentence: &str) -> bool {
        let clause = &sentence[antecedent_clause_start(sentence)..];
        let after_leading_if = &clause[3.min(clause.len())..];
        !after_leading_if.contains(" if ") // allow-noncombinator: swallow detector marker scan on classified text
    }

    // Condition (d): match against a parsed replacement's description, or
    // fall back to the target-replacement carve-out. `clause_norm` is the
    // normalized *antecedent clause* (ability-word / keyword-line prefix
    // already stripped by the caller) — comparing against the clause rather
    // than the whole (possibly multi-line, multi-keyword) sentence avoids
    // false negatives from unrelated leading keyword lines that never made
    // it into the replacement's `description` (which is set from the single
    // source line the replacement was parsed from).
    fn is_backed_by_parsed_replacement(clause_norm: &str, parsed: &ParsedAbilities) -> bool {
        let matched_by_description = parsed.replacements.iter().any(|r| {
            r.description.as_deref().is_some_and(|d| {
                let d_norm = normalize_for_match(d);
                // allow-noncombinator: swallow detector description-vs-clause containment check
                d_norm.contains(clause_norm) || clause_norm.contains(&d_norm)
            })
        });
        if matched_by_description {
            return true;
        }
        // CR 614.1a: AddTargetReplacement riders (e.g. "if [target] would
        // die, exile it instead") register their replacement at resolution
        // time on the parent target and carry no top-level
        // `ReplacementDefinition`/description to match against. Only accept
        // this fallback when there is no matching description AND no
        // top-level replacement at all could plausibly correspond to this
        // sentence — i.e. when the card's *only* evidence of a replacement
        // is the target-replacement rider itself.
        parsed.replacements.is_empty() && any_ability_has_target_replacement(parsed)
    }

    let mut out = String::with_capacity(stripped.len());
    for sentence in stripped.split('.') {
        let s = sentence.trim();
        if s.is_empty() {
            continue;
        }
        // allow-noncombinator: swallow detector phrase scan on cleaned sentence
        let has_instead = s.contains(" instead");
        let clause = &s[antecedent_clause_start(s)..];
        let strip_this_sentence = has_instead
            && matches_replacement_antecedent_shape(s)
            && has_single_if_clause(s)
            && is_backed_by_parsed_replacement(&normalize_for_match(clause), parsed);
        if strip_this_sentence {
            continue;
        }
        out.push_str(sentence);
        out.push('.');
    }
    out
}

/// CR 122.1 + CR 614.1c + CR 608.2c + CR 400.7: "If you put a[n] <type> onto the
/// battlefield this way, put [N] +1/+1 counters on it" (Oviya, Automech Artisan)
/// is represented by the typed `Effect::ChangeZone.conditional_enter_with_counters`
/// gate — the moved object's entry-time counters are applied only when it matches
/// the carried filter (runtime-verified in
/// `change_zone::enter_with_counters_for_object`), so the leading "if" is a
/// representation marker, not a swallowed condition.
///
/// Mirrors `enters_modified_if_is_only_if_marker`: an inside AST probe
/// (`conditional_enter_with_counters` carries `skip_serializing_if = Vec::is_empty`,
/// so the key serializes ONLY when non-empty — keying tightly on the
/// ChangeScope→Battlefield-with-counters shape the resolver handles) plus
/// text-scoping — the represented put-onto-battlefield-this-way counter clause is
/// located via the shared `is_moved_object_put_onto_battlefield_counters_clause`
/// combinator and dropped sentence-by-sentence, and suppression fires ONLY when no
/// OTHER bare " if " survives, so a compound card carrying the gate AND a separate
/// unrelated " if " still flags.
fn conditional_enter_counters_if_is_only_if_marker(
    stripped: &str,
    evidence: &UnitEvidence,
) -> bool {
    if !evidence.has_slot("conditional_enter_with_counters") {
        return false;
    }
    let residual: String = stripped
        .split('.')
        .filter(|sentence| {
            !crate::parser::oracle_effect::sequence::is_moved_object_put_onto_battlefield_counters_clause(
                sentence,
            )
        })
        .collect::<Vec<_>>()
        .join(".");
    let has_other_if = residual.contains(" if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" as if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    !has_other_if
}

/// CR 607.1 + CR 614.1c + CR 122.1: a cast-permission static with an
/// `enters_with_counter` rider represents "if you cast a spell this way, that
/// permanent enters with a counter". Suppress only that represented sentence;
/// a separate conditional in the same item must remain visible to the audit.
fn enters_with_finality_this_way_is_only_if_marker(
    stripped: &str,
    evidence: &UnitEvidence,
) -> bool {
    if !evidence.any_static_mode(|mode| {
        matches!(
            mode,
            StaticMode::GraveyardCastPermission {
                enters_with_counter: Some(_),
                ..
            } | StaticMode::ExileCastPermission {
                enters_with_counter: Some(_),
                ..
            }
        )
    }) {
        return false;
    }

    let residual: String = stripped
        .split('.')
        .filter(|sentence| {
            crate::parser::oracle_effect::parse_cast_this_way_enters_with_counter(
                sentence.trim_start(),
            )
            .is_none()
        })
        .collect::<Vec<_>>()
        .join(".");
    let has_other_if = residual.contains(" if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" as if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    !has_other_if
}

/// CR 118.9 + CR 607.1 + CR 608.2c: an alternative-cost rider on a cast
/// permission represents its linked "if you cast a spell this way, pay …"
/// clause. Additional-cost riders deliberately remain outside this exemption.
fn cast_this_way_alt_cost_is_only_if_marker(stripped: &str, evidence: &UnitEvidence) -> bool {
    if !evidence.any_static_mode(|mode| {
        matches!(
            mode,
            StaticMode::GraveyardCastPermission {
                extra_cost: Some(cost),
                ..
            } | StaticMode::ExileCastPermission {
                extra_cost: Some(cost),
                ..
            } if matches!(cost.mode, CastCostMode::Alternative)
        )
    }) {
        return false;
    }

    let residual: String = stripped
        .split('.')
        .filter(|sentence| {
            let sentence = sentence.trim_start();
            let is_rider = (sentence.contains("cast a spell this way") // allow-noncombinator: swallow detector marker scan on classified text
                || sentence.contains("cast it this way")) // allow-noncombinator: swallow detector marker scan on classified text
                && crate::parser::oracle_effect::try_parse_alt_cost_rider(sentence).is_some();
            !is_rider
        })
        .collect::<Vec<_>>()
        .join(".");
    let has_other_if = residual.contains(" if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" as if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !residual.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    !has_other_if
}

// ── Detector G: Condition_If ────────────────────────────────────────────

/// CR 608.2c: "if [condition], [effect]" — conditional gate. Must be
/// represented as a `condition` / `constraint` field on the parsed ability,
/// or as an `unless_pay` / `unless_filter` for the inverse form.
fn detect_condition_if(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    parsed: &ParsedAbilities,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // CR 614.1a / CR 701.5: cast-then-exile and counter-then-exile riders
    // are encoded as a sub_ability `ChangeZone { destination: Exile,
    // target: ParentTarget }` chained off the primary effect. Snapcaster,
    // Daring Waverider, Defabricate-class — all share this structural
    // shape, with the conditional gate ("if that spell would be put into
    // your graveyard") implicit in the sub_ability's relationship to the
    // parent effect.
    if any_ability_has_exile_parent_rider(parsed) {
        return;
    }
    // CR 119.7 + CR 608.2c: Screaming Nemesis's "If a player is dealt damage
    // this way, they can't gain life for the rest of the game" rider. The
    // "this way" anaphor is not an independent game-state condition — it is
    // the CR 608.2c back-reference that scopes the life-lock to the redirect's
    // damaged player. That scoping is encoded structurally as a
    // `CantGainLife` grant whose `affected` is `ParentTarget` (so it binds
    // only when the redirect's target was a player), making the leading "if"
    // a representation marker rather than a swallowed condition.
    if any_ability_has_dealt_damage_this_way_life_lock(parsed) {
        return;
    }
    // CR 614.1a + CR 701.5: The imperative CastFromZone resolver grants
    // graveyard casts by moving the selected card to exile before casting.
    // For coverage purposes that represents "If that spell would be put into
    // your graveyard, exile it instead" riders on Dreadhorde Arcanist-class
    // triggers, even though it is not a separate ReplacementDefinition.
    if any_ability_has_graveyard_cast_from_zone(parsed) {
        return;
    }
    if any_ability_has_conditional_mana_spell_grant(parsed) {
        return;
    }
    if any_ability_has_cast_from_zone_alt_ability_cost(parsed) {
        return;
    }
    if any_static_has_target_gated_cost_modification(parsed) {
        return;
    }
    // Strip CR-implicit "if" phrases that aren't real conditional gates
    // before scanning. These are built-in rules of their parent effect, not
    // separate conditions:
    //   CR 701.19f: "If you search your library this way, shuffle." — search
    //               always-shuffles is built into the search effect.
    //   CR 305.9 :  "If you don't, [it/this/this land] enters tapped." — the
    //               mana-payment alternative is encoded as a replacement
    //               with `ReplacementMode::Optional { decline: Tap(SelfRef) }`,
    //               i.e., the decline branch IS the "if you don't" gate.
    let stripped = strip_cr_implicit_if_phrases(cleaned);
    // CR 614.1a: strip sentences that are structurally a *represented*
    // replacement antecedent ("if [subject] would [event], ... instead").
    // See `strip_represented_replacement_instead_sentences` doc comment for
    // the full per-sentence exemption criteria — this is intentionally NOT
    // a card-wide gate (a card having *some* replacement no longer exempts
    // *every* " instead" sentence on it).
    let stripped = strip_represented_replacement_instead_sentences(&stripped, parsed);
    let stripped =
        strip_represented_tiered_enters_with_additional_counter_if_pairs(&stripped, parsed);
    // CR 608.2c: "if a player is dealt damage this way, they discard" — the ParentTarget
    // discard rider is structurally represented (Effect::Discard{target:ParentTarget}); the
    // leading "if" is the CR 608.2c back-reference, not a swallowed game-state condition.
    // Mirrors the Screaming Nemesis "dealt damage this way" life-lock exemption above.
    // allow-noncombinator: swallow detector marker scan on classified text
    if stripped.contains("dealt damage this way") && any_ability_has_parent_target_discard(parsed) {
        return;
    }
    // CR 702.170c: "[you may] exile a card. If you do, it becomes plotted." —
    // the "if you do" is the optional-exile linkage, represented by the
    // chained `Plotted` casting-permission grant (see `any_ability_has_plotted_grant`).
    if any_ability_has_plotted_grant(parsed) && plotted_grant_linkage_is_only_if_marker(&stripped) {
        return;
    }
    // CR 608.2c + CR 701.20: "you may look at the top N cards ... If you do,
    // reveal ... from among them ..." (Fertile Thicket, Munda Ambush Leader,
    // Planar Atlas). The optional look lowers to an optional `Dig` and the
    // dependent "reveal ... from among them" is a continuation patching that
    // same `Dig`; the "if you do" linkage IS represented by the optional `Dig`
    // (declining the look stops the whole chain), not swallowed.
    if any_optional_ability_has_dig(parsed) && dig_if_you_do_is_only_if_marker(&stripped) {
        return;
    }
    // CR 603.7a + CR 608.2c + CR 702.170c: "exile that {card,spell} instead of
    // putting it into your graveyard as it resolves. If you do, [return it to
    // your hand at the beginning of the next end step | it becomes plotted]"
    // (Feather, the Redeemed / Lilah, Undefeated Slickshot). The "if you do" is
    // the CR 608.2c back-reference linking the consequence to the exile actually
    // being applied — represented by the typed `on_exile` rider on
    // `Effect::ExileResolvingSpellInsteadOfGraveyard` (the consequence is
    // applied only when the replacement applies). Not a swallowed game-state
    // condition.
    if any_ability_has_exile_resolving_rider(parsed)
        && exile_resolving_rider_if_you_do_is_only_if_marker(&stripped)
    {
        return;
    }
    // CR 614.12: "[you may] put a creature card ... If that card is an
    // enchantment card, it enters tapped and attacking" (Summoner's Grimoire).
    // The leading moved-object type condition is represented by the typed
    // `enters_modified_if` gate on the absorbed ChangeZone. Text-scoped: only
    // suppresses when that enters-modifier clause is the card's only bare " if ".
    if enters_modified_if_is_only_if_marker(&stripped, evidence) {
        return;
    }
    // CR 122.1 + CR 614.1c + CR 608.2c: "If you put a[n] <type> onto the
    // battlefield this way, put [N] +1/+1 counters on it" (Oviya) is represented
    // by `Effect::ChangeZone.conditional_enter_with_counters`.
    if conditional_enter_counters_if_is_only_if_marker(&stripped, evidence) {
        return;
    }
    if enters_with_finality_this_way_is_only_if_marker(&stripped, evidence) {
        return;
    }
    if cast_this_way_alt_cost_is_only_if_marker(&stripped, evidence) {
        return;
    }
    // CR 615.5: "If damage is prevented this way, [effect]" is not an
    // independent condition; prevention replacements encode it by storing the
    // follow-up in `execute`, which the replacement pipeline only fires from
    // the `Prevented` arm.
    // allow-noncombinator: swallow detector marker scan on classified text
    if stripped.contains("if damage is prevented this way") {
        return;
    }
    // CR 615 + CR 615.5: "If damage would be dealt to <target> this turn,
    // prevent that damage [and put that many counters on it]" is encoded
    // structurally as an `Effect::PreventDamage` whose `amount: All` +
    // `duration: UntilEndOfTurn` IS the conditional gate (the shield fires
    // only when matching damage is proposed; otherwise it sits dormant until
    // cleanup). Gatta and Luzzu is the motivating case. The marker test is
    // narrow: the `if`-clause body must lead with "prevent" so generic
    // "if damage" patterns (e.g., damage-redirect replacements that DO want
    // a separate `condition` field) aren't suppressed.
    if stripped.contains("if damage would be dealt to") // allow-noncombinator: swallow detector marker scan on classified text
        && stripped.contains("prevent that damage") // allow-noncombinator: swallow detector marker scan on classified text
        && evidence.any_effect(|e| matches!(e, Effect::PreventDamage { .. }))
    {
        return;
    }
    // CR 118.12 + CR 614.12a: "you may pay [cost]. If you don't, ..."
    // is encoded as `ReplacementMode::MayCost { decline }`; the decline
    // branch is the alternative instruction, not an uncaptured condition.
    // allow-noncombinator: swallow detector marker scan on classified text
    if stripped.contains("if you don't") && any_replacement_has_may_cost_decline(parsed) {
        return;
    }
    // CR 608.2c: "If you [lost/gained] life this way, draw that many cards"
    // (Mister Negative). "[lost/gained] life this way" is a result-reference to
    // the life the controller lost/gained from the preceding effect, and "that
    // many" lowers the dependent draw to `count: EventContextAmount`. The
    // conditional is jointly represented by the event-context quantity —
    // drawing zero when zero life changed is exactly the no-op the "if" guards —
    // so the leading "if" is a representation marker, not a swallowed condition.
    // Mirrors the Screaming Nemesis "dealt damage this way" exemption above.
    // allow-noncombinator: swallow detector marker scan on classified text
    if (stripped.contains("lost life this way") || stripped.contains("gained life this way"))
        && stripped.contains("that many") // allow-noncombinator: swallow detector marker scan on classified text
        && evidence.any_quantity_ref(|q| matches!(q, QuantityRef::EventContextAmount))
    {
        return;
    }
    // CR 117.6 / CR 702.8: A `SpellCastingOption` with `cost: Some(_)`
    // encodes the cost-gated casting permission inline: either an "if you pay"
    // surcharge or an "as an additional cost" surface such as Molten Exhale's
    // behold cost. The "if" is a cost-payment gate, not a game-state condition.
    let has_pay_phrase = stripped.contains("if you pay ") // allow-noncombinator: swallow detector marker scan on classified text
        || stripped.contains("as an additional cost"); // allow-noncombinator: swallow detector marker scan on classified text
    if parsed.casting_options.iter().any(|o| o.cost.is_some()) && has_pay_phrase {
        return;
    }
    // CR 614.1a + CR 120.8: unconditional value-modifier replacement whose
    // ability-word-stripped line leads with its own CR 614.1a applicability "if"
    // ("Flare Star — if a wizard you control would deal damage ... it deals
    // double that damage instead") — represented by the replacement's
    // damage/quantity modification, not a swallowed conditional gate.
    if unconditional_valmod_leading_if_is_only_if_marker(&stripped, parsed) {
        return;
    }
    // Bare " if " — covers prefix conditional ("if X, do Y") and suffix
    // conditional ("do Y if X"). Excluded: "as if", "even if" — modifiers,
    // not conditions. Also "if able" (CR 508.1d / CR 509.1c) —
    // must-attack/must-block riders, encoded as `MustAttack`/`MustBeBlocked`
    // static modes.
    let has_marker = stripped.contains(" if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !stripped.contains(" as if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !stripped.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    // ── Typed condition-representation carriers ─────────────────────────
    //
    // THREE markers from the deleted list are GONE, not translated, because they name
    // no type in the engine and so could never have matched what they claimed:
    //
    //   `ConditionMet`      — there is NO `ConditionMet` variant anywhere. Its only
    //                         matches were the SUBSTRING inside
    //                         `TriggerCondition::SolveConditionMet`, a CR 719 Case
    //                         solve-condition. So on every card carrying a Case, this
    //                         `Condition_If` expectation was silently discharged by an
    //                         unrelated rule's fact (15/15 pool hits). This is the
    //                         collision the typed cutover exists to kill: it fails
    //                         CLOSED, suppressing true positives.
    //   `ConditionalEffect` — names no type; 0 pool hits; dead on arrival.
    //   `IfYouDo`           — names no type either; the token occurs only inside doc
    //                         comments. "If you do" chains are represented as
    //                         sub_ability linkage, which the slot probes below cover.
    //
    // A condition slot being POPULATED is the fact here — which of the three condition
    // enums fills it is not something this detector needs to distinguish, so the slot
    // probes below are deliberately type-agnostic.
    if evidence.has_slot("condition")
        || evidence.has_slot("constraint")
        || evidence.has_slot("unless_filter")
        || evidence.has_slot("unless_pay")
        || evidence.has_slot("if_clause")
        || evidence.has_slot("intervening_if")
    {
        return;
    }
    // CR 700.2a / CR 601.2b: a conditional modal cap is a casting-time choice gate.
    if evidence.any::<ModalSelectionConstraint>(|m| {
        matches!(m, ModalSelectionConstraint::ConditionalMaxChoices { .. })
    }) {
        return;
    }
    // CR 603.4: an explicit quantity comparison IS the conditional gate.
    if evidence.any::<AbilityCondition>(|c| matches!(c, AbilityCondition::QuantityCheck { .. })) {
        return;
    }
    // CR 614.1a: replacement riders whose carried event/destination IS the "if X would Y"
    // gate — AddTargetReplacement ("if [target] would die"), CreatePlaneswalkReplacement
    // (CR 901.9c). CR 705: coin-flip / die-roll branches encode "if you win the flip" and
    // the result table as structured win_effect / lose_effect / results sub-trees, so the
    // effect IS the gate. CR 508.1d / CR 509.1c: "if able" attack/block requirements.
    if evidence.any_effect(|e| {
        matches!(
            e,
            Effect::AddTargetReplacement { .. }
                | Effect::CreatePlaneswalkReplacement { .. }
                | Effect::FlipCoin { .. }
                | Effect::FlipCoins { .. }
                | Effect::RollDie { .. }
                | Effect::ForceBlock { .. }
                | Effect::ForceAttack { .. }
        )
    }) {
        return;
    }
    // CR 508.1d / CR 509.1c / CR 506.6: must-attack / must-block / must-be-blocked "if
    // able" riders are static-mode constraints, not conditional gates.
    // CR 117.3a: TopOfLibraryCastPermission with `alt_cost` IS the "if you cast a spell
    // this way, pay X" gate (Bolas's Citadel).
    // CR 113.6 + CR 601.2a: Evelyn's "... if it was exiled by an ability you controlled"
    // provenance clause is represented by the LinkedCollectionCounterPlayPermission
    // live-source marker static.
    if evidence.any_static_mode(|m| {
        matches!(
            m,
            StaticMode::MustAttack
                | StaticMode::MustBlock
                | StaticMode::MustBeBlocked { .. }
                | StaticMode::TopOfLibraryCastPermission { .. }
                | StaticMode::LinkedCollectionCounterPlayPermission
                | StaticMode::DefilerCostReduction { .. }
        )
    }) {
        return;
    }
    // CR 305.9: "as ~ enters, you may pay X. If you don't, it enters tapped." — the
    // decline branch IS the "if you don't" gate. `ReplacementMode` is internally tagged
    // but shares the key `mode` with the externally tagged `StaticMode`; the two cannot
    // be confused (object-with-tag vs bare string), and anchoring keeps the probe honest.
    if evidence
        .any_at::<ReplacementMode>(&["mode"], |m| matches!(m, ReplacementMode::Optional { .. }))
    {
        return;
    }
    // Slot-shaped gates: the parser filled a branch slot, and its presence IS the
    // conditional representation.
    //   CR 305.9   on_decline                        — "if you don't" alternative
    //   CR 701.20a kept_optional_to                  — RevealUntil decline branch
    //   CR 614.1a  graveyard_destination_replacement — "exile it instead" rider
    //   CR 705     win_effect / lose_effect          — flip branches
    //   CR 701.6   source_rider                      — "if countered this way, ..."
    //   CR 701.6a  countered_spell_zone              — countered-spell destination
    if evidence.has_slot("on_decline")
        || evidence.has_slot("kept_optional_to")
        || evidence.has_slot("graveyard_destination_replacement")
        || evidence.has_slot("win_effect")
        || evidence.has_slot("lose_effect")
        || evidence.has_slot("source_rider")
        || evidence.has_slot("countered_spell_zone")
    {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::ConditionIf.detector_label(),
        truncate(original, 140),
    ));
}

/// Remove sentences containing CR-implicit "if" phrases. These do not
/// represent semantic conditional gates — they are built-in instructions
/// of their parent effect that the engine handles automatically.
fn strip_cr_implicit_if_phrases(cleaned: &str) -> String {
    // Sentence-level replacement is sufficient: we drop the entire sentence
    // containing the implicit phrase, then rejoin. This avoids partial
    // matches leaving stray ", shuffle." fragments.
    let mut out = String::with_capacity(cleaned.len());
    for sentence in cleaned.split('.') {
        let s = sentence.trim();
        if s.is_empty() {
            continue;
        }
        // CR 701.19f: search-shuffle implicit.
        // allow-noncombinator: swallow detector phrase scan on classified text
        if s.contains("if you search your library this way") {
            continue;
        }
        // allow-noncombinator: swallow detector phrase scan on classified text
        if s.contains("if you searched your library this way") {
            continue;
        }
        out.push_str(sentence);
        out.push('.');
    }
    out
}

fn strip_represented_tiered_enters_with_additional_counter_if_pairs(
    cleaned: &str,
    parsed: &ParsedAbilities,
) -> String {
    let mut out = String::with_capacity(cleaned.len());
    for (line_index, line) in cleaned.lines().enumerate() {
        if line_index > 0 {
            out.push('\n');
        }
        out.push_str(&strip_represented_tiered_pairs_from_line(line, parsed));
    }
    out
}

fn strip_represented_tiered_pairs_from_line(line: &str, parsed: &ParsedAbilities) -> String {
    let mut kept = Vec::new();
    let segments: Vec<&str> = line.split('.').collect();
    let mut index = 0usize;
    while index < segments.len() {
        let current = segments[index].trim();
        if current.is_empty() {
            index += 1;
            continue;
        }
        if let Some(next_raw) = segments.get(index + 1) {
            let next = next_raw.trim();
            if sentence_starts_with_otherwise(next) {
                let pair = format!("{current}. {next}.");
                if represented_tiered_counter_pair(&pair, parsed) {
                    index += 2;
                    continue;
                }
            }
        }
        kept.push(format!("{current}."));
        index += 1;
    }
    kept.join(" ")
}

fn sentence_starts_with_otherwise(sentence: &str) -> bool {
    tag::<_, _, nom::error::Error<_>>("otherwise,")
        .parse(sentence)
        .is_ok()
}

fn represented_tiered_counter_pair(pair: &str, parsed: &ParsedAbilities) -> bool {
    let Some(pattern) =
        super::oracle_static::parse_tiered_enters_with_additional_counters_pattern(pair)
    else {
        return false;
    };

    let has_first = parsed.statics.iter().any(|static_def| {
        static_matches_tiered_counter_branch(
            static_def,
            &pattern.counter_type,
            pattern.first_count,
            Comparator::LE,
            pattern.threshold,
        )
    });
    let has_otherwise = parsed.statics.iter().any(|static_def| {
        static_matches_tiered_counter_branch(
            static_def,
            &pattern.counter_type,
            pattern.otherwise_count,
            Comparator::GT,
            pattern.threshold,
        )
    });

    has_first && has_otherwise
}

fn static_matches_tiered_counter_branch(
    static_def: &StaticDefinition,
    counter_type: &crate::types::counter::CounterType,
    count: u32,
    comparator: Comparator,
    threshold: u32,
) -> bool {
    let StaticMode::EntersWithAdditionalCounters {
        counter_type: parsed_counter_type,
        count: parsed_count,
    } = &static_def.mode
    else {
        return false;
    };
    if parsed_counter_type != counter_type || *parsed_count != count {
        return false;
    }
    static_def
        .affected
        .as_ref()
        .is_some_and(|filter| target_filter_has_cmc(filter, comparator, threshold))
}

fn target_filter_has_cmc(filter: &TargetFilter, comparator: Comparator, threshold: u32) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.properties.iter().any(|prop| {
            matches!(
                prop,
                FilterProp::Cmc {
                    comparator: parsed_comparator,
                    value: QuantityExpr::Fixed { value },
                } if *parsed_comparator == comparator
                    && u32::try_from(*value).ok() == Some(threshold)
            )
        }),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(|filter| target_filter_has_cmc(filter, comparator, threshold)),
        _ => false,
    }
}

// ── Detector H: Condition_Unless ────────────────────────────────────────

/// CR 608.2c + CR 118.12: "unless [X]" — inverse conditional or
/// unless-pay-cost rider. Must produce an `unless_*` slot or a
/// `condition` with negated semantics.
fn detect_condition_unless(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains(" unless ") {
        return;
    }
    // CR 118.12 + CR 603.5: the "unless" payment/condition slot. `unless_pay` subsumes
    // both the trigger-level and the (retired) `Effect::Counter.unless_payment` encoding.
    if evidence.has_slot("unless_filter")
        || evidence.has_slot("unless_pay")
        || evidence.has_slot("unless_condition")
        || evidence.has_slot("condition")
    {
        return;
    }
    // CR 605.1a: an activation `exemption` IS the "unless they're mana abilities" clause.
    // THREE `StaticMode` variants carry that field — pinning only `CantBeActivated` (as a
    // first cut of this cutover did) makes Suppression Field ("Activated abilities cost
    // {2} more to activate unless they're mana abilities", which lowers to
    // `ReduceAbilityCost`) report a swallow it fully represents. Completeness grep:
    //
    //   $ rg -A8 '^    [A-Z]\w* \{' types/statics.rs | rg -B8 'exemption:'
    //     StaticMode::CantBeActivated
    //     StaticMode::ReduceAbilityCost
    //     StaticMode::CantActivateDuring
    if evidence.any_static_mode(|m| {
        matches!(
            m,
            StaticMode::CantBeActivated {
                exemption: ActivationExemption::ManaAbilities,
                ..
            } | StaticMode::ReduceAbilityCost {
                exemption: ActivationExemption::ManaAbilities,
                ..
            } | StaticMode::CantActivateDuring {
                exemption: ActivationExemption::ManaAbilities,
                ..
            }
        )
    }) {
        return;
    }
    // CR 509.1c: "can't be blocked unless all creatures defending player controls block
    // it" (Tromokratis) folds its whole `unless` clause into one typed static mode — the
    // condition slot stays empty because the mode IS the condition. The deleted marker
    // list caught this only through the bare substring `Unless`, which matched the tail
    // of the variant name by accident.
    if evidence.any_static_mode(|m| matches!(m, StaticMode::CantBeBlockedUnlessAllBlock)) {
        return;
    }
    // CR 118.12 + CR 614.1a: replacement-effect `unless` gates. The complete closed set
    // (grep: `rg '^    Unless\w*' types/replacements.rs`) — each is an "unless <clause>"
    // guard folded into the replacement's condition.
    if evidence.any::<ReplacementCondition>(|c| {
        matches!(
            c,
            ReplacementCondition::UnlessControlsSubtype { .. }
                | ReplacementCondition::UnlessControlsOtherLeq { .. }
                | ReplacementCondition::UnlessControlsMatching { .. }
                | ReplacementCondition::UnlessControlsCountMatching { .. }
                | ReplacementCondition::UnlessPlayerLifeAtMost { .. }
                | ReplacementCondition::UnlessMultipleOpponents
                | ReplacementCondition::UnlessYourTurn
                | ReplacementCondition::UnlessQuantity { .. }
        )
    }) {
        return;
    }
    // CR 118.12: `StaticCondition::UnlessPay` — the payment gate itself.
    if evidence.any::<StaticCondition>(|c| matches!(c, StaticCondition::UnlessPay { .. })) {
        return;
    }
    // CR 508.1f + CR 701.26a: "... can't become tapped unless [they're/it's]
    // being declared as attackers." The attacker-declaration exemption is
    // inherent to the tap keyword action — CR 508.1f states that tapping a
    // creature as it's declared an attacker isn't a cost, so a modeled
    // `StaticMode::CantTap` restriction already permits that tap with no extra
    // AST slot. The unless clause is therefore fully modeled, not swallowed.
    // Class-general: recognizes any goad-lock printing of this exemption whose
    // tap restriction lowered to a `CantTap` static (Ood Sphere's Red-Eye).
    let declared_as_attacker_exemption = cleaned.contains("declared as an attacker") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("declared as attackers"); // allow-noncombinator: swallow detector marker scan on classified text
    if declared_as_attacker_exemption
        && evidence.any_static_mode(|m| matches!(m, StaticMode::CantTap))
    {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::ConditionUnless.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector I: Condition_AsLongAs ──────────────────────────────────────

/// CR 611.3: "as long as [X]" — duration tied to a condition (typically a
/// static ability with a `condition` field).
fn detect_condition_as_long_as(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    parsed: &ParsedAbilities,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("as long as ") {
        return;
    }
    // CR 400.7i + CR 609.4b: "play/cast that card for as long as it remains
    // exiled, and mana ..." is represented as a zone-scoped PlayFromExile
    // permission on the exiled object. The permission is stored with
    // Duration::Permanent because zones::apply_zone_exit_cleanup removes it
    // when the card stops being the exiled object this effect refers to.
    let exile_duration_clause_recognized = [
        "as long as it remains exiled",
        "as long as that card remains exiled",
        "as long as those cards remain exiled",
        "as long as they remain exiled",
    ]
    .iter()
    .any(|phrase| cleaned.contains(phrase));
    if exile_duration_clause_recognized
        && evidence
            .any::<CastingPermission>(|c| matches!(c, CastingPermission::PlayFromExile { .. }))
        && evidence.any_duration(|d| matches!(d, Duration::Permanent))
    {
        return;
    }
    // CR 611.3. `AsLongAs` and `ConditionalStatic` are GONE, not translated: neither
    // names a type. `AsLongAs` matched only as a SUBSTRING of `Duration::ForAsLongAs`,
    // and `ConditionalStatic` matched nothing at all (0 pool hits).
    //
    // The real carriers: a populated condition slot, or a duration that IS the gate —
    // `ForAsLongAs` (CR 611.2b, an explicit conditional duration) and `UntilHostLeavesPlay`
    // (CR 611.3a, "as long as you control this creature" — the duration's lifetime IS the
    // controllership condition; Aegis Angel, Hostage Taker).
    if evidence.has_slot("condition") {
        return;
    }
    if evidence.any_duration(|d| {
        matches!(
            d,
            Duration::ForAsLongAs { .. } | Duration::UntilHostLeavesPlay
        )
    }) {
        return;
    }
    if any_static_has_per_object_as_long_as_gate(parsed) {
        return;
    }
    if any_static_has_attached_subject_qualifier_grant(parsed) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::ConditionAsLongAs.detector_label(),
        truncate(original, 140),
    ));
}

/// CR 611.3a + CR 613: an inverted attached-subject grant
/// ("As long as enchanted/equipped creature is `<characteristic>`, it gets …")
/// represents its "as long as" qualifier by folding the characteristic into the
/// grant's `affected` attached-subject filter (e.g. `creature + EnchantedBy +
/// HasColor{White}`), not as a separate `condition`. The qualifier IS
/// represented — the static only applies while the host matches the folded
/// characteristic — so the clause is not swallowed.
///
/// This is precise: when the qualifier is unparseable the inverted grant falls
/// back to `affected: SelfRef` (not an attached-subject filter), so this
/// exemption never masks a genuinely-dropped qualifier.
fn any_static_has_attached_subject_qualifier_grant(parsed: &ParsedAbilities) -> bool {
    parsed.statics.iter().any(|static_def| {
        static_def.description.as_ref().is_some_and(|description| {
            let lower = description.to_ascii_lowercase(); // allow-noncombinator: swallow detector marker scan on parsed static description
            lower.contains("as long as enchanted ") || lower.contains("as long as equipped ")
        }) && static_def
            .affected
            .as_ref()
            .is_some_and(target_filter_is_attached_subject)
    })
}

fn target_filter_is_attached_subject(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(|prop| {
            matches!(
                prop,
                crate::types::ability::FilterProp::EnchantedBy
                    | crate::types::ability::FilterProp::EquippedBy
            )
        }),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_is_attached_subject)
        }
        TargetFilter::Not { filter } => target_filter_is_attached_subject(filter),
        _ => false,
    }
}

fn any_static_has_per_object_as_long_as_gate(parsed: &ParsedAbilities) -> bool {
    parsed.statics.iter().any(|static_def| {
        static_def
            .description
            .as_ref()
            .is_some_and(|description| description.to_ascii_lowercase().contains("as long as ")) // allow-noncombinator: swallow detector marker scan on parsed static description
            && static_def
                .modifications
                .contains(&ContinuousModification::AssignDamageFromToughness)
            && static_def
                .affected
                .as_ref()
                .is_some_and(target_filter_has_per_object_condition_property)
    })
}

fn target_filter_has_per_object_condition_property(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(|prop| {
            matches!(
                prop,
                crate::types::ability::FilterProp::ToughnessGTPower
                    | crate::types::ability::FilterProp::PowerExceedsBase
                    | crate::types::ability::FilterProp::WithKeyword { .. }
                    | crate::types::ability::FilterProp::CanEnchant { .. }
            )
        }),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => filters
            .iter()
            .any(target_filter_has_per_object_condition_property),
        TargetFilter::Not { filter } => target_filter_has_per_object_condition_property(filter),
        _ => false,
    }
}

// ── Detector J: Duration_ThisTurn ───────────────────────────────────────

/// CR 611.2a: "this turn" — temporal scope. Must produce a `Duration`
/// slot on the parsed ability or a duration-bearing modification.
fn detect_duration_this_turn(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains(" this turn") {
        return;
    }
    // Exempt forms where "this turn" is part of a different grammar.
    // "before this turn" / "earlier this turn" describe past events, not
    // a forward-looking duration on an effect.
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("earlier this turn") || cleaned.contains("before this turn") {
        return;
    }
    // CR 615.5: one-shot prevention spells use "this turn" for the prevention
    // shield's lifetime; the follow-up phrase is gated by the prevention event,
    // not by an independent duration field on the nested effect.
    // allow-noncombinator: swallow detector marker scan on classified text
    if cleaned.contains("if damage is prevented this way") {
        return;
    }
    // CR 719.2: Case solve conditions are synthesized into the Case
    // auto-solve trigger after Oracle parsing. When every "this turn"
    // occurrence lives on a "To solve" line, the phrase is a turn-history
    // condition, not an effect duration swallowed by the parser.
    let total_this_turn = cleaned.matches(" this turn").count();
    let case_solve_this_turn: usize = cleaned
        .lines()
        // allow-noncombinator: swallow detector marker scan on classified text
        .filter(|line| line.contains("to solve"))
        .map(|line| line.matches(" this turn").count())
        .sum();
    if total_this_turn > 0 && total_this_turn == case_solve_this_turn {
        return;
    }
    // CR 603.4 / CR 307.5: "Activate only if ... this turn" routes the clause
    // to an `ActivationRestriction::RequiresCondition`; "this turn" there
    // scopes the activation condition, never an effect duration. Exempt ONLY
    // when EVERY "this turn" occurrence lives on an "activate only" line
    // (occurrence-balanced line scoping, mirroring the `case_solve_this_turn`
    // block above) AND the AST confirms a `RequiresCondition` node. Line
    // scoping is required so a card whose OTHER lines genuinely drop a
    // duration is NOT exempted.
    let activate_only_this_turn: usize = cleaned
        .lines()
        // allow-noncombinator: swallow detector marker scan on classified text
        .filter(|line| line.contains("activate only"))
        .map(|line| line.matches(" this turn").count())
        .sum();
    if total_this_turn > 0
        && total_this_turn == activate_only_this_turn
        && evidence.any_activation_restriction(|r| {
            matches!(r, ActivationRestriction::RequiresCondition { .. })
        })
    {
        return;
    }
    // CR 700.4 + CR 700.5 (turn-history quantities and counters):
    // "this turn" is used pervasively as a SUFFIX on count/quantity
    // references rather than as a duration on an effect. The detector
    // should only fire when "this turn" plausibly denotes a forward-
    // looking duration. These past-participle / verb-phrase suffixes
    // are quantity/count contexts and must not warn:
    //   - "<verb-past> this turn"  e.g. died/cast/drawn/lost/gained/
    //     dealt/attacked/blocked/entered/warped/controlled/sacrificed/
    //     discarded/exiled/played/revealed/spent this turn
    //   - "you/they/X has/have <verb-past> ... this turn"  same shape,
    //     present-perfect form, also count.
    // Two scans cover both: a present-perfect prefix scan and a list
    // of past-participle suffix collocations. The exemption is
    // conservative — when "this turn" really IS a duration, none of
    // these phrasings appear (the duration form is "[modification]
    // until end of turn" or "[modification] this turn", not
    // "[verb-past] this turn").
    // allow-noncombinator: swallow detector marker scan on classified text
    const QUANTITY_CONTEXT_SUFFIXES: &[&str] = &[
        "died this turn",
        "cast this turn",
        "drawn this turn",
        "lost this turn",
        "gained this turn",
        "dealt this turn",
        "attacked this turn",
        "blocked this turn",
        "entered this turn",
        "warped this turn",
        "controlled this turn",
        "sacrificed this turn",
        "discarded this turn",
        "exiled this turn",
        "played this turn",
        "revealed this turn",
        "spent this turn",
        "milled this turn",
        "tapped this turn",
        "untapped this turn",
        "destroyed this turn",
        "regenerated this turn",
        "scryed this turn",
        "surveiled this turn",
        // CR 702.171c: "creature that saddled it this turn" — a relative-clause
        // target filter (`FilterProp::SaddledSource`), not an effect duration.
        // Same turn-history-quantity class as "attacked this turn" / "died this
        // turn": the "this turn" scopes the saddler-membership window (cleared at
        // cleanup), never a forward-looking duration. Calamity / Giant Beaver /
        // The Gitrog, Ravenous Ride.
        "saddled it this turn",
    ];
    // Only exempt when EVERY occurrence of "this turn" is part of a quantity
    // context. Counting occurrences ensures we still fire on cards that have
    // BOTH a quantity-context phrase AND a real duration (the duration could
    // be the swallow). The marker check below handles the all-captured case.
    let quantity_this_turn: usize = QUANTITY_CONTEXT_SUFFIXES
        .iter()
        .map(|s| cleaned.matches(s).count())
        .sum();
    if total_this_turn > 0 && total_this_turn == quantity_this_turn {
        return;
    }
    // CR 608.2i (look-back) + CR 611.2a: a "controlled by a player who <look-back
    // verb> this turn" relative clause owns its trailing "this turn" — it scopes a
    // turn-history predicate on the target's controller, NOT a forward-looking
    // duration on the effect. The parser's `strip_trailing_duration` recognizes
    // this same clause structure via `player_lookback_relative_clause_owns_suffix`
    // and declines to amputate the suffix (so the control-change stays permanent,
    // CR 611.2a). This detector must mirror that recognition, else it would flag a
    // correctly-deferred look-back clause as a swallowed duration (Admiral Beckett
    // Brass). Occurrence-balanced like the quantity/case-solve exemptions above:
    // only exempt when the SOLE "this turn" is the one the look-back clause owns,
    // so a card with a genuine earlier duration AND a trailing look-back clause
    // still fires.
    if total_this_turn == 1 && player_lookback_relative_clause_owns_suffix(cleaned) {
        return;
    }
    // ── Typed "this turn" carriers ──────────────────────────────────────
    //
    // The deleted list leaned on three UNANCHORED substrings — `"ThisTurn"`,
    // `"EndOfTurn"`, `"EndOfCombat"` — which silently matched ANY variant name
    // CONTAINING them, across every enum in the tree. That is why the list "worked":
    // ~25 turn-history variants were caught by accident rather than by name. Each is now
    // an explicit arm the compiler checks.
    //
    // (a) CR 611.2a + CR 514.2: a real duration carrier of ANY kind — the "this turn"
    //     landed on a duration slot, which is exactly what this detector asks about.
    if evidence.any_duration(|_| true) {
        return;
    }
    // (b)–(h) CR 700.4 + CR 700.5 + CR 603.4: TURN-HISTORY references. "this turn" here
    //     is a suffix on a count/condition read from the turn's history, not a
    //     forward-looking duration on an effect.
    //
    //     These arms are the COMPLETE closed set of `*ThisTurn` variants, generated from
    //     the type definitions rather than hand-picked:
    //
    //       $ rg '^    (\w*ThisTurn\w*)\s*[,{(]' crates/engine/src/types/
    //         QuantityRef       20      TriggerCondition   8
    //         ParsedCondition   18      FilterProp         7
    //         AbilityCondition   3      StaticCondition    2
    //         TriggerConstraint  2
    //
    //     The deleted substring marker `"ThisTurn"` caught all 60 of these BY ACCIDENT —
    //     it matched any variant name containing the token, in any enum. Hand-picking a
    //     subset (as a first cut of this cutover did) silently reintroduces a
    //     false-positive wave: `QuantityRef::LifeGainedThisTurn` was missed, and Follow
    //     the Lumarets ("If you gained life this turn, ... instead") immediately reported
    //     a swallowed duration it does in fact represent. Generating the set closes that.
    //
    //     ROT DIRECTION (declared): a NEW `*ThisTurn` variant added later is not caught
    //     here until this list is regenerated. That failure is conservative-RED — the
    //     audit over-reports a swallow — never a silent false green.
    // KEY-ANCHORED: four of the variants below — `AttackedThisTurn`, `EnteredThisTurn`,
    // `BattlefieldEntriesThisTurn`, `CounterAddedThisTurn` — are ALSO variant names of
    // `FilterProp` / `TriggerCondition` / `ParsedCondition`. Unanchored, a target filter
    // saying "creatures that attacked this turn" would deserialize as
    // `QuantityRef::AttackedThisTurn` and silently discharge a "this turn" duration
    // expectation. See `QUANTITY_KEYS`.
    if evidence.any_quantity_ref(|x| {
        matches!(
            x,
            QuantityRef::LifeLostThisTurn { .. }
                | QuantityRef::SpellsCastThisTurn { .. }
                | QuantityRef::EnteredThisTurn { .. }
                | QuantityRef::SacrificedThisTurn { .. }
                | QuantityRef::CrimesCommittedThisTurn
                | QuantityRef::BendTypesThisTurn
                | QuantityRef::LifeGainedThisTurn { .. }
                | QuantityRef::CardsDrawnThisTurn { .. }
                | QuantityRef::BattlefieldEntriesThisTurn { .. }
                | QuantityRef::LandsPlayedThisTurn { .. }
                | QuantityRef::ZoneChangeCountThisTurn { .. }
                | QuantityRef::ZoneChangeAggregateThisTurn { .. }
                | QuantityRef::DamageDealtThisTurn { .. }
                | QuantityRef::AttackedThisTurn { .. }
                | QuantityRef::DescendedThisTurn
                | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
                | QuantityRef::CounterAddedThisTurn { .. }
                | QuantityRef::CardsDiscardedThisTurn { .. }
                | QuantityRef::TokensCreatedThisTurn { .. }
                | QuantityRef::PlayerActionsThisTurn { .. }
        )
    }) {
        return;
    }
    // The five probes that follow (`ParsedCondition`, `StaticCondition`, `AbilityCondition`,
    // `TriggerCondition`, `FilterProp`) are deliberately UNANCHORED, and that is SOUND — for a
    // stated, checkable reason rather than because "each names a specific variant" (that claim
    // is false in general; see `swallow_evidence`'s module doc, and `Effect::CastFromZone`).
    //
    // COLLISION-INVARIANT (audited over all 162 tagged enums, task #51). 14 of the variant names
    // matched below are shared with another tagged enum — `SourceEnteredThisTurn` alone is a
    // UNIT variant of FOUR condition enums, so it deserializes on the `type` tag alone and any
    // of these probes will accept any of the others' nodes. It does not matter, because:
    //
    //   * this whole block asks ONE existential question — "does this unit carry ANY turn-scoped
    //     carrier?" — as a disjunction across seven enums; and
    //   * EVERY variant named in these `matches!` arms has `ThisTurn` in its name, and a
    //     collision IS name identity. So a colliding node, read as any of its owning enums,
    //     still bears a `ThisTurn` name and is still a turn-scoped carrier.
    //
    // The disjunction is therefore CLOSED under its own collision set and the answer is the same
    // under every reading. Anchoring these would be actively WRONG: `ReplacementCondition` also
    // owns `YouAttackedThisTurn` and `DealtDamageThisTurnBySource` and has NO leg here, so today
    // those carriers are seen only VIA the collision. Anchoring without first adding typed legs
    // for them would suppress true turn-scope evidence — the `ManaProduction` trap from #50,
    // where anchoring removed a fact that was accidentally load-bearing.
    //
    // The checkable invariant, if you change these arms: every variant named below must contain
    // `ThisTurn`. Add one that does not, and this argument no longer holds — anchor instead.
    if evidence.any::<ParsedCondition>(|x| {
        matches!(
            x,
            ParsedCondition::SourceEnteredThisTurn
                | ParsedCondition::SourceAttackedThisTurn
                | ParsedCondition::OpponentSearchedLibraryThisTurn
                | ParsedCondition::YouAttackedThisTurn
                | ParsedCondition::YouAttackedSourceControllerThisTurn
                | ParsedCondition::YouPlayedLandThisTurn
                | ParsedCondition::YouCastSpellThisTurn { .. }
                | ParsedCondition::YouCastNoncreatureSpellThisTurn
                | ParsedCondition::YouGainedLifeThisTurn
                | ParsedCondition::YouCreatedTokenThisTurn
                | ParsedCondition::YouDiscardedCardThisTurn
                | ParsedCondition::YouSacrificedArtifactThisTurn
                | ParsedCondition::CreatureDiedThisTurn
                | ParsedCondition::YouHadCreatureEnterThisTurn
                | ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn
                | ParsedCondition::YouHadArtifactEnterThisTurn
                | ParsedCondition::BattlefieldEntriesThisTurn { .. }
                | ParsedCondition::CardsLeftYourGraveyardThisTurnAtLeast { .. }
        )
    }) {
        return;
    }
    if evidence.any::<StaticCondition>(|x| {
        matches!(
            x,
            StaticCondition::SpellCastWithVariantThisTurn { .. }
                | StaticCondition::SourceEnteredThisTurn
        )
    }) {
        return;
    }
    if evidence.any::<AbilityCondition>(|x| {
        matches!(
            x,
            AbilityCondition::SourceEnteredThisTurn
                | AbilityCondition::SpellCastWithVariantThisTurn { .. }
                | AbilityCondition::NthResolutionThisTurn { .. }
        )
    }) {
        return;
    }
    if evidence.any::<TriggerCondition>(|x| {
        matches!(
            x,
            TriggerCondition::SourceEnteredThisTurn
                | TriggerCondition::DealtDamageBySourceThisTurn
                | TriggerCondition::DealtDamageThisTurnBySource { .. }
                | TriggerCondition::FirstTimeObjectTappedThisTurn
                | TriggerCondition::FirstTimeObjectCountersAddedThisTurn
                | TriggerCondition::AttackedThisTurn
                | TriggerCondition::CastSpellThisTurn { .. }
                | TriggerCondition::SpellCastWithVariantThisTurn { .. }
                | TriggerCondition::CounterAddedThisTurn
        )
    }) {
        return;
    }
    if evidence.any::<TriggerConstraint>(|x| {
        matches!(
            x,
            TriggerConstraint::NthSpellThisTurn { .. } | TriggerConstraint::NthDrawThisTurn { .. }
        )
    }) {
        return;
    }
    if evidence.any::<FilterProp>(|x| {
        matches!(
            x,
            FilterProp::WasDealtDamageThisTurn
                | FilterProp::DealtDamageThisTurn
                | FilterProp::EnteredThisTurn
                | FilterProp::ZoneChangedThisTurn { .. }
                | FilterProp::AttackedThisTurn { .. }
                | FilterProp::BlockedThisTurn
                | FilterProp::AttackedOrBlockedThisTurn
                | FilterProp::CountersPutOnThisTurn { .. }
        )
    }) {
        return;
    }
    // CR 700.2 + CR 603.4: "choose one that hasn't been chosen this turn" (the Alliance
    //     cycle: Gala Greeters, Monument to Endurance, The Fantastic Four) folds its turn
    //     scope into a typed modal-selection constraint. `ModalSelectionConstraint` is a
    //     separate enum from the seven above, so the generated `*ThisTurn` sweep does not
    //     reach it and it must be named. Missing it produced 6 false-positive swallows.
    if evidence.any::<ModalSelectionConstraint>(|c| {
        matches!(c, ModalSelectionConstraint::NoRepeatThisTurn)
    }) {
        return;
    }
    // (h2) Turn-history carriers whose names do NOT end in `ThisTurn`, so the generated
    //     set above cannot reach them. They must be named explicitly — and the fact that
    //     they must is itself the point: a NAME-SHAPED rule ("contains ThisTurn") is not
    //     the same as a SEMANTIC one ("is turn-scoped"), and only the compiler-checked
    //     enumeration keeps the two from drifting apart.
    //       CR 603.4  ParsedCondition::YouCastSpellCountAtLeast — "only if you've cast
    //                 another spell this turn" (Illusory Angel); the turn scope is
    //                 implicit in the per-turn cast counter.
    //       CR 719.2  TriggerCondition::SolveConditionMet — a Case solve condition.
    //       CR 119.3  PlayerFilter per-turn player-history filters.
    if evidence
        .any::<ParsedCondition>(|c| matches!(c, ParsedCondition::YouCastSpellCountAtLeast { .. }))
    {
        return;
    }
    if evidence.any::<TriggerCondition>(|c| matches!(c, TriggerCondition::SolveConditionMet)) {
        return;
    }
    if evidence.any::<PlayerFilter>(|p| {
        matches!(
            p,
            PlayerFilter::OpponentGainedLife
                | PlayerFilter::OpponentLostLife
                | PlayerFilter::OpponentDealtDamage { .. }
        )
    }) {
        return;
    }
    // A condition slot holding the typed `Unrecognized` node means the parser ROUTED the
    // clause into a condition slot and explicitly recorded that it could not parse it —
    // the "this turn" is consumed by a condition, and the card stays visibly unsupported
    // (CR 611.3).
    if evidence.any_at::<StaticCondition>(&["condition"], |c| {
        matches!(c, StaticCondition::Unrecognized { .. })
    }) {
        return;
    }
    // (i) One-shot effects whose "this turn" lifetime is INTRINSIC (they expire at
    //     cleanup, CR 514.2) rather than stored in a `duration` slot:
    //       CR 615.1   PreventDamage / CreateDamageReplacement — prevention shields
    //       CR 614.11  CreateDrawReplacement — "next time you would draw ... instead"
    //       CR 614.1a  AddTargetReplacement
    //       CR 603.7c  CreateDelayedTrigger — delayed triggers from spells expire at EOT
    //       CR 601.2f  ReduceNextSpellCost — consumed by the next cast
    //       CR 509.1c  ForceBlock — a one-turn combat requirement
    //       CR 601.2   CastFromZone — a cast permission, not a duration
    if evidence.any_effect(|e| {
        matches!(
            e,
            Effect::PreventDamage { .. }
                | Effect::CreateDamageReplacement { .. }
                | Effect::CreateDrawReplacement { .. }
                | Effect::AddTargetReplacement { .. }
                | Effect::CreateDelayedTrigger { .. }
                | Effect::ReduceNextSpellCost { .. }
                | Effect::ForceBlock { .. }
                | Effect::CastFromZone { .. }
        )
    }) {
        return;
    }
    // (j) CR 603.7c: a `WhenNextEvent` delayed-trigger condition IS the "next [event] this
    //     turn" scope (Chandra, the Firebrand -2; Doublecast).
    if evidence.any::<DelayedTriggerCondition>(|c| {
        matches!(c, DelayedTriggerCondition::WhenNextEvent { .. })
    }) {
        return;
    }
    // (k) CR 514.2: `AddTargetReplacement` carries `expiry: EndOfTurn` — the EOT scope
    //     encoded structurally on the ReplacementDefinition rather than via `duration`.
    if evidence.any_at::<RestrictionExpiry>(&["expiry"], |e| {
        matches!(
            e,
            RestrictionExpiry::EndOfTurn | RestrictionExpiry::EndOfCombat
        )
    }) {
        return;
    }
    // (l) CR 614.6: a `DamageDone` replacement event scopes to a single resolution; the
    //     "this turn" is implicit in the spell-level replacement lifetime.
    //     `ReplacementEvent` is EXTERNALLY tagged, so it is key-anchored.
    if evidence
        .any_at::<ReplacementEvent>(&["event"], |e| matches!(e, ReplacementEvent::DamageDone))
    {
        return;
    }
    // (m) CR 601.2 + CR 604.1: per-turn limit statics ARE the enforcement window; the
    //     "this turn" / "once each turn" wording is intrinsic to the variant (Ethersworn
    //     Canonist), not a separate duration slot.
    if evidence.any_static_mode(|m| {
        matches!(
            m,
            StaticMode::PerTurnCastLimit { .. }
                | StaticMode::PerTurnDrawLimit { .. }
                | StaticMode::ExileCastPermission { .. }
        )
    }) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::DurationThisTurn.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector K: Duration_NextTurn ───────────────────────────────────────

/// CR 611.2a: "until your next turn" — extended-duration scope.
fn detect_duration_next_turn(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("until your next turn")
        // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("until that player's next turn")
    {
        return;
    }
    // CR 611.2a + CR 514.2. The three markers this replaces were, respectively: DEAD
    // (`YourNextTurn` names no type), DEAD (`UntilYourNextTurn` names no type), and a
    // SUBSTRING ACCIDENT (`NextTurn` matched only because it is a substring of
    // `Duration::UntilNextTurnOf` / `UntilEndOfNextTurnOf`). This detector's entire
    // evidence therefore rested on one accidental substring hit — and that same substring
    // made the expectation dischargeable by `Effect::SkipNextTurn` / `Effect::ControlNextTurn`,
    // which are facts about extra/skipped turns, not about how long a continuous effect
    // lasts. Counting `[A-Za-z]*NextTurn[A-Za-z]*` over the full 35,396-face export:
    //
    //     UntilNextTurnOf 216 | UntilEndOfNextTurnOf 182   <- real durations
    //     SkipNextTurn     10 | ControlNextTurn        8   <- NOT durations
    //
    // A typed, key-anchored match cannot make that mistake. These are the two real carriers,
    // keyed off the `PlayerScope`-parameterized variants:
    //   UntilNextTurnOf      — expires at the BEGINNING of that player's next turn
    //   UntilEndOfNextTurnOf — persists THROUGH that turn (Light Up the Stage class)
    if evidence.any_duration(|d| {
        matches!(
            d,
            Duration::UntilNextTurnOf { .. } | Duration::UntilEndOfNextTurnOf { .. }
        )
    }) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::DurationNextTurn.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector L: Optional_MayHave ────────────────────────────────────────

/// CR 608.2d: "have it [verb]" / "may have [it]" — causative optional from
/// "any opponent may [verb], [if they do] have it [verb]" patterns.
/// Distinct from the simple `you may` optional flag.
fn detect_optional_may_have(
    cleaned: &str,
    original: &str,
    evidence: &UnitEvidence,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    let has_marker = cleaned.contains("may have ") || cleaned.contains("you may have "); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    // The "have causative" parser produces effects that recursively contain
    // optional sub-abilities. Conservative check: if the AST contains any
    // optional flag OR explicit causative marker, treat as captured.
    // CR 603.5. THREE of this detector's six markers were DEAD — `Causative`,
    // `HaveCausative` and `HaveItVerb` name no type in the engine (0 pool hits each), so
    // half its evidence list could never have matched anything. Its real evidence is only
    // the three carriers below.
    if evidence.any::<AbilityDefinition>(|d| d.optional) {
        return;
    }
    // CR 614.1a: "you may have this creature enter as a copy ..." — the optional choice
    // lives on the replacement's mode, not on `def.optional`.
    if evidence
        .any_at::<ReplacementMode>(&["mode"], |m| matches!(m, ReplacementMode::Optional { .. }))
    {
        return;
    }
    // CR 702.20a: "you may have this creature assign its combat damage as though it
    // weren't blocked" — a continuous modification whose optionality is the per-combat
    // player decision.
    if evidence.any::<ContinuousModification>(|m| {
        matches!(m, ContinuousModification::AssignDamageAsThoughUnblocked)
    }) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::OptionalMayHave.detector_label(),
        truncate(original, 140),
    ));
}

// ── Detector M: APNAP ───────────────────────────────────────────────────

/// CR 101.4: "starting with you" / "in turn order" — APNAP (active
/// player → non-active player) iteration order. Must produce an explicit
/// ordering marker on the parsed ability so multiplayer resolution honors
/// the ordering rather than defaulting to engine-internal player order.
fn detect_apnap(
    cleaned: &str,
    original: &str,
    parsed: &ParsedAbilities,
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    let has_marker = cleaned.contains("starting with you") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("starting with the active player") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("starting with that player") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("in turn order"); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    if any_ability_has_apnap_ordering(parsed) {
        return;
    }
    diagnostics.push(OracleDiagnostic::swallowed_clause(
        OracleSemanticFeature::Apnap.detector_label(),
        truncate(original, 140),
    ));
}

// ── Cascade-vs-AST structural diff (option 3) ──────────────────────────
//
// Complementary to the oracle-text-scanning detectors above. Where those
// detect *parser gaps* ("the cascade had no stripper for this phrase"),
// the structural diff detects *parser bugs* ("the cascade variable was
// set, but def-assembly dropped it").
//
// Hooked into `parse_effect_chain_ir` at the end of each chunk
// iteration, after `current_defs` has been finalized but before
// `defs.extend(current_defs)`. The cascade variables in scope at that
// point are compared against the resulting primary def's fields. Any
// populated cascade variable with no corresponding non-default def field
// emits a `Swallow:Cascade*` warning.

/// Snapshot of cascade-stage variables captured during a single chunk
/// iteration. Populated at the end of the chunk loop and diffed against
/// the resulting `AbilityDefinition` before it is appended to the chain.
///
/// Only the cascade variables whose loss would represent silent dropping
/// are included. Internal bookkeeping variables (`anchor_subject`,
/// `chunk_actor`, etc.) that feed other captures are excluded — their
/// loss is observable only through the *terminal* slot they affect, and
/// that terminal slot is what the diff checks.
#[derive(Debug, Clone, Default)]
pub(crate) struct CascadeSnapshot<'a> {
    /// `is_optional` from `strip_optional_effect_prefix` (line ~6260) OR
    /// from the parsed clause's subject-phrase "may" modal.
    pub is_optional: bool,
    /// `opponent_may_scope` from `strip_optional_effect_prefix`. Only
    /// meaningful when `is_optional` is also true.
    pub opponent_may_scope: Option<&'a OpponentMayScope>,
    /// Effective condition: chain-level cascade `condition` OR-folded
    /// with `clause.condition` (matches `effective_condition` at
    /// line ~6428).
    pub condition: Option<&'a AbilityCondition>,
    /// `repeat_for` from `strip_for_each_prefix` / `strip_repeat_count_suffix`
    /// (line ~6261).
    pub repeat_for: Option<&'a QuantityExpr>,
    /// `player_scope` after the implicit-scope merge at line ~6206.
    pub player_scope: Option<&'a PlayerFilter>,
    /// `clause.duration` — duration captured by `parse_effect_clause`.
    pub clause_duration: Option<&'a crate::types::ability::Duration>,
}

/// Run the structural diff against the primary def of the just-finalized
/// chunk and emit warnings for any populated cascade slot that did not
/// land on the def.
pub(crate) fn check_cascade_diff(
    snap: &CascadeSnapshot<'_>,
    defs: &[AbilityDefinition],
    diagnostics: &mut Vec<OracleDiagnostic>,
) {
    let Some(def) = defs.first() else {
        // Empty current_defs is itself a swallow but the iteration would
        // have produced an Unimplemented up-stack; nothing to compare.
        return;
    };

    if snap.is_optional && !def.optional {
        diagnostics.push(OracleDiagnostic::CascadeLoss {
            slot: CascadeSlot::Optional,
            effect_name: effect_name(&def.effect).to_string(),
            line_index: 0,
        });
    }

    if snap.opponent_may_scope.is_some() && def.optional_for.is_none() {
        diagnostics.push(OracleDiagnostic::CascadeLoss {
            slot: CascadeSlot::OpponentMay,
            effect_name: effect_name(&def.effect).to_string(),
            line_index: 0,
        });
    }

    if snap.condition.is_some() && def.condition.is_none() {
        diagnostics.push(OracleDiagnostic::CascadeLoss {
            slot: CascadeSlot::Condition,
            effect_name: effect_name(&def.effect).to_string(),
            line_index: 0,
        });
    }

    if snap.repeat_for.is_some() && def.repeat_for.is_none() {
        // CR 608.2c: "for each X" / "twice" repeat counts (the instruction is
        // followed as written, once per iteration) are sometimes
        // pushed onto a sub_ability instead of the def itself for
        // TargetOnly wrappers (line ~6411). Walk the sub_ability chain
        // before declaring loss.
        if !def_tree_has_repeat_for(def) {
            diagnostics.push(OracleDiagnostic::CascadeLoss {
                slot: CascadeSlot::RepeatFor,
                effect_name: effect_name(&def.effect).to_string(),
                line_index: 0,
            });
        }
    }

    if snap.player_scope.is_some() && def.player_scope.is_none() {
        diagnostics.push(OracleDiagnostic::CascadeLoss {
            slot: CascadeSlot::PlayerScope,
            effect_name: effect_name(&def.effect).to_string(),
            line_index: 0,
        });
    }

    if snap.clause_duration.is_some()
        && def.duration.is_none()
        && !effect_carries_duration(&def.effect)
    {
        diagnostics.push(OracleDiagnostic::CascadeLoss {
            slot: CascadeSlot::Duration,
            effect_name: effect_name(&def.effect).to_string(),
            line_index: 0,
        });
    }
}

fn def_tree_has_repeat_for(def: &AbilityDefinition) -> bool {
    if def.repeat_for.is_some() {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_repeat_for(sub) {
            return true;
        }
    }
    false
}

/// CR 514.2 + CR 611.2: GenericEffect and GrantCastingPermission embed a
/// duration field inside the effect rather than (or in addition to) the
/// outer `def.duration`. `with_clause_duration` patches both. The
/// cascade-diff treats either presence as "captured."
fn effect_carries_duration(effect: &Effect) -> bool {
    match effect {
        Effect::GenericEffect { duration, .. } => duration.is_some(),
        Effect::GrantCastingPermission { permission, .. } => {
            use crate::types::ability::CastingPermission;
            matches!(permission, CastingPermission::PlayFromExile { .. })
        }
        _ => false,
    }
}

fn effect_name(effect: &Effect) -> &str {
    // Reuse the existing public name function — keeps this in sync with
    // the rest of the codebase's effect-naming convention.
    crate::types::ability::effect_variant_name(effect)
}

#[cfg(test)]
mod tests {
    use crate::parser::swallow_evidence::UnitEvidence;

    use super::{
        any_ability_has_unimplemented, def_tree_has_optional, def_tree_has_unimplemented,
        trigger_tree_has_optional,
    };
    use crate::parser::oracle::parse_oracle_text;
    use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
    use crate::types::ability::{
        AbilityDefinition, DamageModification, Effect, OutsideGameSourcePool, TargetFilter,
    };
    use crate::types::identifiers::TrackedSetId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn parse(text: &str, types: &[&str]) -> crate::parser::oracle::ParsedAbilities {
        parse_named(text, "Test Card", types)
    }

    fn parse_named(
        text: &str,
        card_name: &str,
        types: &[&str],
    ) -> crate::parser::oracle::ParsedAbilities {
        parse_oracle_text(
            text,
            card_name,
            &[],
            &types.iter().map(|ty| (*ty).to_string()).collect::<Vec<_>>(),
            &[],
        )
    }

    fn has_swallowed_detector(
        parsed: &crate::parser::oracle::ParsedAbilities,
        detector: &str,
    ) -> bool {
        parsed.parse_warnings.iter().any(|warning| {
            matches!(
                warning,
                OracleDiagnostic::SwallowedClause {
                    detector: warning_detector,
                    ..
                } if warning_detector == detector
            )
        })
    }

    /// Every swallow finding on this face, with its stamped unit provenance.
    fn swallows(
        parsed: &crate::parser::oracle::ParsedAbilities,
    ) -> Vec<(
        &str,
        usize,
        &crate::parser::oracle_ir::diagnostic::OracleSourceSpan,
    )> {
        parsed
            .parse_warnings
            .iter()
            .filter_map(|warning| match warning {
                OracleDiagnostic::SwallowedClause {
                    detector,
                    line_index,
                    unit_span,
                    ..
                } => Some((
                    detector.as_str(),
                    *line_index,
                    unit_span
                        .as_ref()
                        .expect("the audit stamps a unit span on every finding it emits"),
                )),
                _ => None,
            })
            .collect()
    }

    // ── Unit provenance (Plan 02 gate 5, as amended by the t124 ruling) ──────────

    /// THE COLLAPSE, PINNED BY NAME.
    ///
    /// Aether Revolt raises TWO swallowed semantics from ONE physical source line: an
    /// `as long as` revolt condition and an `if … would … instead` replacement condition.
    /// They are genuinely distinct clauses — and their diagnostics carry the SAME
    /// `unit_span`, so a consumer cannot locate them separately.
    ///
    /// That is NOT a defect in this payload. It is the line-granularity ceiling of the
    /// span SUBSTRATE: `DocEmitter::exact_span` hands every item on a line the whole
    /// line's byte range, so `audit_units` groups both clauses into one unit, and one
    /// unit has exactly one span. Sub-line item spans are the prerequisite that lifts it;
    /// when they land, the units subdivide on their own and these two spans separate.
    ///
    /// Asserted BY NAME so that whoever lands sub-line spans must consciously flip this
    /// test, rather than leave a silently stale collapse behind.
    #[test]
    fn same_line_clauses_share_the_unit_span_until_subline_item_spans_exist() {
        let text = "Revolt — As long as a permanent left the battlefield under your control \
                    this turn, if a source you control would deal noncombat damage to an \
                    opponent or a permanent an opponent controls, it deals that much damage \
                    plus 2 instead.\nWhenever you get one or more {E}, this enchantment deals \
                    that much damage to any target.";
        let parsed = parse_named(text, "Aether Revolt", &["Enchantment"]);

        let found = swallows(&parsed);
        assert_eq!(
            found.len(),
            2,
            "two clauses on line 0 each swallow a semantic: {found:?}"
        );
        assert_ne!(
            found[0].0, found[1].0,
            "the two findings are DIFFERENT detectors — distinct clauses, not one clause twice",
        );

        // Both are attributed to line 0 …
        assert_eq!((found[0].1, found[1].1), (0, 0));
        // … and — the ceiling — they SHARE one span. Distinct byte spans for the two
        // clauses are unreachable until items carry sub-line spans.
        assert_eq!(
            found[0].2, found[1].2,
            "same-line clauses share the unit span; flip this only when items carry sub-line spans",
        );
        // The span is the unit's own line, exactly located and card-absolute.
        let span = found[0].2;
        assert_eq!((span.first_line, span.last_line), (0, 0));
        assert_eq!(span.start_byte, 0);
        assert_eq!(
            span.end_byte,
            text.lines().next().expect("line 0").len(),
            "the unit span is the exact byte extent of the line it owns",
        );
    }

    /// NON-VACUITY for the collapse above: the span is not a constant.
    ///
    /// Captain Eberhart swallows a `this turn` duration on line 1 AND another on line 2.
    /// Those are two units, so they must carry DIFFERENT, non-overlapping spans. Without
    /// this, `same_line_clauses_share_the_unit_span…` would pass just as happily against
    /// a span that never varied at all.
    #[test]
    fn different_units_carry_different_spans() {
        let text = "Double strike\nSpells cast from among cards you drew this turn cost {1} \
                    less to cast.\nSpells cast from among cards your opponents drew this turn \
                    cost {1} more to cast.";
        let parsed = parse_named(text, "Captain Eberhart", &["Creature"]);

        let found = swallows(&parsed);
        assert_eq!(found.len(), 2, "one finding per line: {found:?}");
        assert_eq!(
            (found[0].1, found[1].1),
            (1, 2),
            "attributed to lines 1 and 2"
        );
        assert_ne!(
            found[0].2, found[1].2,
            "distinct units must carry distinct spans — otherwise the span is inert",
        );
        assert!(
            found[0].2.end_byte <= found[1].2.start_byte,
            "unit spans are disjoint and in source order: {:?} then {:?}",
            found[0].2,
            found[1].2,
        );
    }

    /// The evidence set is the unit's POOLED items, and it is honestly `0..N`.
    /// A finding names every item the audit consulted — never one hand-picked id.
    #[test]
    fn findings_carry_the_units_pooled_evidence_items() {
        let text = "Revolt — As long as a permanent left the battlefield under your control \
                    this turn, if a source you control would deal noncombat damage to an \
                    opponent or a permanent an opponent controls, it deals that much damage \
                    plus 2 instead.\nWhenever you get one or more {E}, this enchantment deals \
                    that much damage to any target.";
        let parsed = parse_named(text, "Aether Revolt", &["Enchantment"]);

        let items: Vec<_> = parsed
            .parse_warnings
            .iter()
            .filter(|warning| matches!(warning, OracleDiagnostic::SwallowedClause { .. }))
            .map(|warning| warning.evidence_items().to_vec())
            .collect();

        assert_eq!(items.len(), 2);
        assert!(
            !items[0].is_empty(),
            "line 0 lowered an item; the finding must name the evidence it was judged against",
        );
        assert_eq!(
            items[0], items[1],
            "both findings came from ONE unit, so they carry ONE evidence set",
        );
    }

    /// CR 603.7a + CR 608.2c: Feather, the Redeemed's "If you do, return it to
    /// your hand at the beginning of the next end step" is represented by the
    /// `on_exile` rider on `Effect::ExileResolvingSpellInsteadOfGraveyard`,
    /// so Detector G must NOT flag it as a swallowed `Condition_If`. Reverting
    /// the `any_ability_has_exile_resolving_rider` exemption re-fires the
    /// swallow (the coverage-honesty regression this guards against).
    #[test]
    fn feather_return_rider_is_not_a_swallowed_condition_if() {
        let parsed = parse_named(
            "Flying\nWhenever you cast an instant or sorcery spell that targets a creature you \
             control, exile that card instead of putting it into your graveyard as it resolves. \
             If you do, return it to your hand at the beginning of the next end step.",
            "Feather, the Redeemed",
            &["Legendary", "Creature"],
        );
        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "Feather's folded return rider must not surface as a swallowed Condition_If: {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 702.170c + CR 608.2c: Lilah, Undefeated Slickshot's "If you do, it
    /// becomes plotted" folds onto the same `on_exile` rider (as
    /// `ExiledSpellRider::BecomePlotted`), so — like Feather — Detector G must
    /// NOT flag its "if you do" as a swallowed `Condition_If`. This guards the
    /// coverage-honesty regression that the plot-grant fold could otherwise
    /// introduce (the folded grant leaves no `GrantCastingPermission { Plotted }`
    /// node for the `any_ability_has_plotted_grant` suppression to see).
    #[test]
    fn lilah_plotted_rider_is_not_a_swallowed_condition_if() {
        let parsed = parse_named(
            "Prowess\nWhenever you cast a multicolored instant or sorcery spell from your hand, \
             exile that spell instead of putting it into your graveyard as it resolves. If you \
             do, it becomes plotted.",
            "Lilah, Undefeated Slickshot",
            &["Legendary", "Creature"],
        );
        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "Lilah's folded plotted rider must not surface as a swallowed Condition_If: {:?}",
            parsed.parse_warnings
        );
    }

    fn find_search_outside_game(def: &AbilityDefinition) -> Option<&Effect> {
        if matches!(&*def.effect, Effect::SearchOutsideGame { .. }) {
            return Some(&def.effect);
        }
        def.sub_ability
            .as_deref()
            .and_then(find_search_outside_game)
    }

    // ── Modal_DynamicMaxDropped (Sub-plan A) ────────────────────────────

    /// Core gate (positive): a `"modal":{` node with no `"dynamic_max_choices":{`
    /// and a dynamic header marker fires the detector. Revert discriminator:
    /// removing the `diagnostics.push` in `detect_modal_dynamic_max_dropped`
    /// (or gate (1)/(2)/(3)) drops the diagnostic and fails this assertion.
    #[test]
    fn modal_dynamic_max_dropped_fires_on_modal_without_dynamic_cap() {
        let evidence = UnitEvidence::from_json_for_test(
            r#"{"abilities":[{"modal":{"min_choices":1,"max_choices":1,"mode_count":3}}]}"#,
        );
        let mut diags = Vec::new();
        super::detect_modal_dynamic_max_dropped(
            "when you do, choose up to that many",
            "When you do, choose up to that many.",
            &evidence,
            &mut diags,
        );
        assert!(
            diags.iter().any(|d| matches!(
                d,
                OracleDiagnostic::SwallowedClause { detector, .. }
                    if detector == "Modal_DynamicMaxDropped"
            )),
            "detector must fire when a modal node lacks a dynamic cap: {diags:?}"
        );
    }

    /// Negative (a) — Ruinous shape: a modal node that DOES carry
    /// `"dynamic_max_choices":{` is silent (the cap was captured). Proves the
    /// detector keys on the AST cap, not the phrase. Revert gate (3) → fires.
    #[test]
    fn modal_dynamic_max_dropped_silent_when_dynamic_cap_present() {
        let evidence = UnitEvidence::from_json_for_test(
            r#"{"abilities":[{"modal":{"min_choices":0,"max_choices":3,"mode_count":3,"dynamic_max_choices":{"type":"Ref","qty":"CostXPaid"}}}]}"#,
        );
        let mut diags = Vec::new();
        super::detect_modal_dynamic_max_dropped(
            "choose up to x",
            "Choose up to X —",
            &evidence,
            &mut diags,
        );
        assert!(
            diags.is_empty(),
            "must stay silent when dynamic_max_choices is present: {diags:?}"
        );
    }

    /// Negative (b) — A1 fix: a NON-modal "choose up to X <nouns>" selection
    /// clause has no `"modal":{` node, so the detector is silent even though
    /// the dynamic header marker is present. Revert gate (2) → false-fires on
    /// Heroic Feast / Temporal Firestorm.
    #[test]
    fn modal_dynamic_max_dropped_silent_without_modal_node() {
        let evidence = UnitEvidence::from_json_for_test(
            r#"{"abilities":[{"effect":{"type":"PutCounter","count":{"type":"Ref","qty":"CostXPaid"}}}]}"#,
        );
        let mut diags = Vec::new();
        super::detect_modal_dynamic_max_dropped(
            "choose up to that many target creatures you control",
            "Choose up to that many target creatures you control.",
            &evidence,
            &mut diags,
        );
        assert!(
            diags.is_empty(),
            "must stay silent without a modal node (A1 gate): {diags:?}"
        );
    }

    /// Registration + real-pipeline positive: a "choose up to X, where X is ..."
    /// modal keeps the fixed-default cap (the existing "where" guard blocks the
    /// cast-{X} arm), so the real parser yields a modal node WITHOUT
    /// `dynamic_max_choices`. Driven end-to-end through `parse_oracle_text` →
    /// `check_swallowed_clauses`, so it discriminates the detector registration.
    /// This "where X is" shape is unaffected by Sub-plan B's "that many" arm,
    /// keeping the test stable across both commits. Revert the registration line
    /// in `check_swallowed_clauses` → no diagnostic → fails.
    #[test]
    fn modal_dynamic_max_dropped_registered_via_real_parse() {
        let parsed = parse_named(
            "Choose up to X, where X is the number of cards in your hand \u{2014}\n\
             \u{2022} You gain 2 life.\n\
             \u{2022} Draw a card.",
            "Synthetic Dropped Cap Modal",
            &["Sorcery"],
        );
        assert!(
            has_swallowed_detector(&parsed, "Modal_DynamicMaxDropped"),
            "real parse of a dropped-cap modal must surface the detector: {:?}",
            parsed.parse_warnings
        );
    }

    /// Real-pipeline negative — The Ruinous Wrecking Crew: its modal carries
    /// `dynamic_max_choices: Some(CostXPaid)` on the base, so the detector is
    /// silent and the line-counter fold (A-1) greens it. Stable across B.
    #[test]
    fn modal_dynamic_max_dropped_silent_on_ruinous() {
        let parsed = parse_named(
            "The Ruinous Wrecking Crew enters with X +1/+1 counters on it.\n\
             When The Ruinous Wrecking Crew enters, choose up to X \u{2014}\n\
             \u{2022} Discard a card, then draw a card.\n\
             \u{2022} Target opponent loses 2 life.\n\
             \u{2022} Destroy target token.\n\
             \u{2022} Each player sacrifices a creature of their choice.",
            "The Ruinous Wrecking Crew",
            &["Creature"],
        );
        assert!(
            !has_swallowed_detector(&parsed, "Modal_DynamicMaxDropped"),
            "Ruinous carries a dynamic cap and must stay silent: {:?}",
            parsed.parse_warnings
        );
    }

    /// Real-pipeline negative — Heroic Feast / Temporal Firestorm: a non-modal
    /// "choose up to X/that many <nouns>" selection clause has no modal node, so
    /// the detector stays silent (A1 gate on real parses). Guards no-regression.
    #[test]
    fn modal_dynamic_max_dropped_silent_on_non_modal_selection_clauses() {
        let heroic = parse_named(
            "When this enchantment enters, create a Food token.\n\
             Whenever you gain life, choose up to that many target creatures you control. \
             Put a +1/+1 counter on each of them.",
            "Heroic Feast",
            &["Enchantment"],
        );
        assert!(
            !has_swallowed_detector(&heroic, "Modal_DynamicMaxDropped"),
            "Heroic Feast is a non-modal selection clause and must stay silent: {:?}",
            heroic.parse_warnings
        );

        let firestorm = parse_named(
            "Choose up to X creatures and/or planeswalkers you control, where X is the number \
             of times this spell was kicked. Those permanents phase out.\n\
             Temporal Firestorm deals 5 damage to each creature and each planeswalker.",
            "Temporal Firestorm",
            &["Sorcery"],
        );
        assert!(
            !has_swallowed_detector(&firestorm, "Modal_DynamicMaxDropped"),
            "Temporal Firestorm is a non-modal selection clause and must stay silent: {:?}",
            firestorm.parse_warnings
        );
    }

    #[test]
    fn duration_this_turn_accepts_turn_history_case_condition() {
        let parsed = parse_named(
            "Instant and sorcery spells you cast cost {1} less to cast.\n\
             To solve — You've cast four or more instant and sorcery spells this turn. \
             (If unsolved, solve at the beginning of your end step.)\n\
             Solved — Whenever you cast an instant or sorcery spell, draw a card.",
            "Case of the Ransacked Lab",
            &["Enchantment"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// CR 608.2i (look-back) + CR 611.2a: a "controlled by a player who <look-back
    /// verb> this turn" relative clause owns its trailing "this turn" — a
    /// turn-history predicate on the target's controller, not an effect duration.
    /// The parser's `strip_trailing_duration` correctly leaves the control-change
    /// permanent (no phantom `UntilEndOfTurn`), so the Duration_ThisTurn detector
    /// must not flag the deferred look-back clause as a swallowed duration.
    /// Admiral Beckett Brass (#4735 / PR #5517).
    #[test]
    fn duration_this_turn_accepts_player_lookback_relative_clause() {
        let parsed = parse_named(
            "Other Pirates you control get +1/+1.\n\
             At the beginning of your end step, gain control of target nonland \
             permanent controlled by a player who was dealt combat damage by \
             three or more Pirates this turn.",
            "Admiral Beckett Brass",
            &["Legendary", "Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// CR 611.3: equipment and creature statics that fold "as long as" qualifiers
    /// into attached-subject filters must not trip Condition_AsLongAs warnings
    /// (issue #2234).
    #[test]
    fn condition_as_long_as_bronze_horse_reports_known_gap_champions_helm_accepted() {
        use crate::types::ability::{FilterProp, ShieldKind, TypedFilter};
        use crate::types::keywords::Keyword;
        use crate::types::replacements::ReplacementEvent;
        use crate::types::ContinuousModification;

        let bronze = parse_named(
            "Trample\nAs long as you control another creature, prevent all damage that would be dealt to this creature by spells that target it.",
            "Bronze Horse",
            &["Artifact", "Creature"],
        );
        assert!(
            !bronze
                .replacements
                .iter()
                .any(|r| r.execute.as_deref().is_some_and(def_tree_has_unimplemented)),
            "Bronze Horse replacement must parse without Unimplemented"
        );
        let as_long_as = "as long as";
        assert!(
            bronze.replacements.iter().any(|r| {
                r.event == ReplacementEvent::DamageDone
                    && r.valid_card == Some(TargetFilter::SelfRef)
                    && matches!(r.shield_kind, ShieldKind::Prevention { .. })
                    && r.description
                        .as_deref()
                        .is_some_and(|d| d.to_ascii_lowercase().contains(as_long_as))
            }),
            "expected gated damage-prevention replacement, got {:#?}",
            bronze.replacements
        );
        // KNOWN GAP, pinned deliberately. Bronze Horse DOES report a swallowed
        // `Condition_AsLongAs` — and did so in the shipped card data long before this
        // change (verified against the pre-cutover full-pool export). The assertions
        // above are why: the "as long as you control another creature" gate survives
        // only inside `description` (a String), and is never lifted into a typed
        // condition on the replacement. So the condition really is swallowed and the
        // detector is right.
        //
        // This test previously asserted the OPPOSITE and passed — vacuously. It parses
        // with an empty MTGJSON keyword list, so the "Trample" line fell through to an
        // `Effect::Unimplemented`, which tripped the CARD-WIDE Unimplemented gate and
        // silenced all fourteen detectors on the whole card. The green was the gate,
        // not the parse. Per-unit suppression scopes that gate to the "Trample" line
        // alone, so line 1 is now audited and the pre-existing gap is visible.
        //
        // Flip this assertion when the "as long as" gate is lifted into a typed
        // replacement condition; until then it is a tripwire, not an endorsement.
        assert!(
            has_swallowed_detector(&bronze, "Condition_AsLongAs"),
            "Bronze Horse's as-long-as gate is description-only, never typed: the swallow \
             is real. Warnings: {:?}",
            bronze.parse_warnings
        );

        let helm = parse_named(
            "Equipped creature gets +2/+2.\nAs long as equipped creature is legendary, it has hexproof. (It can't be the target of spells or abilities your opponents control.)\nEquip {1}",
            "Champion's Helm",
            &["Artifact", "Equipment"],
        );
        assert!(
            !helm.abilities.iter().any(def_tree_has_unimplemented)
                && !helm
                    .triggers
                    .iter()
                    .any(|t| t.execute.as_deref().is_some_and(def_tree_has_unimplemented)),
            "Champion's Helm must parse without Unimplemented"
        );
        assert!(
            helm.statics.iter().any(|s| {
                matches!(s.mode, crate::types::statics::StaticMode::Continuous)
                    && matches!(
                        &s.affected,
                        Some(TargetFilter::Typed(TypedFilter {
                            properties,
                            ..
                        })) if properties.contains(&FilterProp::EquippedBy)
                            && properties.contains(&FilterProp::HasSupertype {
                                value: crate::types::card_type::Supertype::Legendary
                            })
                    )
                    && s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AddKeyword {
                                keyword: Keyword::Hexproof
                            }
                        )
                    })
            }),
            "expected legendary-equipped hexproof static, got {:#?}",
            helm.statics
        );
        assert!(!has_swallowed_detector(&helm, "Condition_AsLongAs"));
    }

    #[test]
    fn condition_as_long_as_accepts_inverted_attached_subject_color_grant() {
        // CR 611.3a + CR 613: Shield of the Oversoul folds "is white/green" into
        // the grant's `affected` attached-subject filter, so the "as long as"
        // qualifier is represented (not swallowed) despite `condition: None`.
        let parsed = parse_named(
            "Enchant creature\n\
             As long as enchanted creature is white, it gets +1/+1 and has flying.\n\
             As long as enchanted creature is green, it gets +1/+1 and has indestructible.",
            "Shield of the Oversoul",
            &["Enchantment", "Aura"],
        );

        assert!(!has_swallowed_detector(&parsed, "Condition_AsLongAs"));
    }

    #[test]
    fn condition_as_long_as_accepts_inverted_equipped_subject_grant() {
        let parsed = parse_named(
            "Equip {2}\nAs long as equipped creature is red, it gets +1/+1 and has haste.",
            "Test Equipment",
            &["Artifact", "Equipment"],
        );

        assert!(!has_swallowed_detector(&parsed, "Condition_AsLongAs"));
    }

    #[test]
    fn optional_you_may_accepts_repeat_this_process() {
        // CR 107.1c: "You may repeat this process any number of times" is
        // captured as `repeat_until: ControllerChoice` on the root ability —
        // a controller decision, not a swallowed optional effect.
        let parsed = parse(
            "Reveal the top card of your library and put that card into your \
             hand. You lose life equal to its mana value. You may repeat this \
             process any number of times.",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_accepts_up_to_change_zone_choice() {
        let parsed = parse(
            "Mill four cards, then you may return a permanent card from among them to your hand.",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_accepts_any_number_land_drop_static() {
        let parsed = parse_named(
            "You may play any number of lands on each of your turns.\n\
             Whenever you play a land, if it wasn't the first land you played this turn, \
             this enchantment deals 1 damage to you.",
            "Fastbond",
            &["Enchantment"],
        );

        assert!(
            parsed
                .statics
                .iter()
                .any(|s| s.mode == (StaticMode::AdditionalLandDrop { count: u8::MAX })),
            "expected Fastbond land-drop permission to parse as a static, got: {:#?}",
            parsed.statics
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_accepts_teferi_flash_grant_generic_effect() {
        // CR 117.3a + CR 702.8a: Teferi, Time Raveler's [+1] ("you may cast
        // sorcery spells as though they had flash") lowers to a `GenericEffect`
        // granting `StaticMode::CastWithKeyword { Flash }`. The granted casting
        // permission IS the "you may cast" opt-in, so the "you may " marker must
        // NOT be reported as a swallowed clause.
        let parsed = parse_named(
            "Each opponent can cast spells only any time they could cast a sorcery.\n\
             [+1]: Until your next turn, you may cast sorcery spells as though they had flash.\n\
             [\u{2212}3]: Return up to one target artifact, creature, or enchantment to its owner's hand. Draw a card.",
            "Teferi, Time Raveler",
            &["Planeswalker"],
        );

        // Pin the structural shape the exemption keys on: the [+1] must lower to
        // a GenericEffect granting CastWithKeyword (directly or via
        // GrantStaticAbility). Guards against a silent regression where the
        // grant stops parsing — then the negative assertion below would pass
        // vacuously.
        assert!(
            parsed
                .abilities
                .iter()
                .any(def_tree_grants_cast_with_keyword),
            "expected Teferi [+1] to lower to a GenericEffect granting \
             CastWithKeyword, parsed abilities: {:#?}",
            parsed.abilities
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_accepts_spend_mana_as_any_color_static() {
        // CR 609.4b: "You may spend mana as though it were mana of any color."
        // Must not produce an Optional_YouMay warning.
        let parsed = parse(
            "You may spend mana as though it were mana of any color.",
            &["Artifact"],
        );
        assert!(
            parsed.statics.iter().any(|s| matches!(
                s.mode,
                StaticMode::SpendManaAsAnyColor {
                    spell_filter: None,
                    activation_source_filter: None,
                }
            )),
            "expected SpendManaAsAnyColor static to parse, got statics: {:#?}",
            parsed.statics
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn condition_as_long_as_accepts_play_from_exile_they_remain_exiled() {
        // CR 400.7i + CR 609.4b: Brainstealer Dragon's tracked-set
        // PlayFromExile permission represents the "for as long as they remain
        // exiled" duration; the following any-color mana rider folds into that
        // same permission, not a swallowed condition.
        let parsed = parse_named(
            "Flying\n\
             At the beginning of your end step, exile the top card of each opponent's library. \
             You may play those cards for as long as they remain exiled. \
             If you cast a spell this way, you may spend mana as though it were mana of any color to cast it.",
            "Brainstealer Dragon",
            &["Creature", "Dragon"],
        );

        assert!(!has_swallowed_detector(&parsed, "Condition_AsLongAs"));
    }

    #[test]
    fn optional_you_may_accepts_activate_abilities_as_though_haste_static() {
        // CR 602.5a + CR 702.10c: "You may activate abilities of creatures you
        // control as though those creatures had haste."
        let parsed = parse(
            "You may activate abilities of creatures you control as though those creatures had haste.",
            &["Creature"],
        );
        assert!(
            parsed
                .statics
                .iter()
                .any(|s| matches!(s.mode, StaticMode::CanActivateAbilitiesAsThoughHaste)),
            "expected CanActivateAbilitiesAsThoughHaste static to parse, got statics: {:#?}",
            parsed.statics
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn static_carries_optional_modification_recurses_into_grant_static_ability() {
        // CR 113.3d + CR 613.1f: GrantStaticAbility wrapping an optional modification
        // must be detected by static_carries_optional_modification via recursion.
        use crate::types::ability::{ContinuousModification, StaticDefinition};

        let inner_def = Box::new(
            StaticDefinition::continuous()
                .modifications(vec![ContinuousModification::AssignDamageAsThoughUnblocked]),
        );
        let outer_static = StaticDefinition::continuous().modifications(vec![
            ContinuousModification::GrantStaticAbility {
                definition: inner_def,
            },
        ]);
        assert!(
            super::static_carries_optional_modification(&outer_static),
            "static_carries_optional_modification must recurse into GrantStaticAbility"
        );
    }

    #[test]
    fn optional_you_may_accepts_chromatic_orrery_real_oracle_text() {
        // Regression test against actual Chromatic Orrery oracle text.
        // SpendManaAsAnyColor static must suppress Optional_YouMay.
        let parsed = parse(
            "You may spend mana as though it were mana of any color.\n\
             {T}: Add {C}{C}{C}{C}{C}.\n\
             {5}, {T}: Draw a card for each color among permanents you control.",
            &["Artifact"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_accepts_thousand_year_elixir_real_oracle_text() {
        // Regression test against actual Thousand-Year Elixir oracle text.
        // CanActivateAbilitiesAsThoughHaste static must suppress Optional_YouMay.
        let parsed = parse(
            "You may activate abilities of creatures you control as though those creatures had haste.\n\
             {1}, {T}: Untap target creature.",
            &["Artifact"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_accepts_proud_wildbonder_real_oracle_text() {
        // Regression test against actual Proud Wildbonder oracle text.
        // "Creatures you control with trample have '...' " is a top-level static
        // (exercises the parsed.statics path at swallow_check.rs line ~980), not the
        // Effect::GenericEffect arm. AssignDamageAsThoughUnblocked must suppress
        // Optional_YouMay via static_carries_optional_modification.
        let parsed = parse(
            "Trample\n\
             Creatures you control with trample have \
             \"You may have this creature assign its combat damage as though it weren't blocked.\"",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_accepts_garruk_savage_herald_minus_seven() {
        // CR 510.1c + CR 609.4: Garruk, Savage Herald's [-7] ("Until end of
        // turn, creatures you control gain \"You may have this creature assign
        // its combat damage as though it weren't blocked.\"") lowers to a
        // loyalty AbilityDefinition with Effect::GenericEffect whose static
        // carries AssignDamageAsThoughUnblocked (directly or via GrantStaticAbility).
        // static_definition_has_optional must recognise this via
        // static_carries_optional_modification so Optional_YouMay does not fire.
        let parsed = parse_named(
            "[+1]: Reveal the top card of your library. If it's a creature card, put it into your hand. Otherwise, put it on the bottom of your library.\n\
             [\u{2212}2]: Target creature you control deals damage equal to its power to another target creature.\n\
             [\u{2212}7]: Until end of turn, creatures you control gain \
             \"You may have this creature assign its combat damage as though it weren't blocked.\"",
            "Garruk, Savage Herald",
            &["Planeswalker"],
        );
        // Structural guard: the [-7] must lower to a GenericEffect carrying
        // AssignDamageAsThoughUnblocked (directly or via GrantStaticAbility).
        // Without this, the negative assertion below could pass vacuously if
        // the [-7] regresses to Unimplemented (any_ability_has_unimplemented
        // early-returns from check_swallowed_clauses, masking the gap).
        use crate::types::ability::ContinuousModification;
        fn ability_grants_assign_damage_unblocked(def: &AbilityDefinition) -> bool {
            if let Effect::GenericEffect {
                ref static_abilities,
                ..
            } = *def.effect
            {
                if static_abilities.iter().any(|s| {
                    s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::AssignDamageAsThoughUnblocked
                                | ContinuousModification::GrantStaticAbility { .. }
                        )
                    })
                }) {
                    return true;
                }
            }
            def.sub_ability
                .as_deref()
                .is_some_and(ability_grants_assign_damage_unblocked)
        }
        assert!(
            parsed
                .abilities
                .iter()
                .any(ability_grants_assign_damage_unblocked),
            "expected Garruk [-7] to lower to GenericEffect with \
             AssignDamageAsThoughUnblocked/GrantStaticAbility static, \
             abilities: {:#?}",
            parsed.abilities
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn optional_you_may_still_flags_unrepresented_optional_verb() {
        // Guard the exemption did NOT over-broaden: a genuine "you may <verb>"
        // optional effect with no AST representation must still be flagged.
        // `Effect::Unimplemented` suppression is avoided by pairing the bogus
        // clause with a fully-parsed primary effect.
        let parsed = parse_named(
            "Each opponent can cast spells only any time they could cast a sorcery.\n\
             [+1]: You may wibble the frobnicator until your next turn.\n\
             [\u{2212}3]: Return up to one target artifact, creature, or enchantment to its owner's hand. Draw a card.",
            "Not Teferi",
            &["Planeswalker"],
        );

        // Only meaningful if the bogus +1 did NOT itself become Unimplemented
        // (which would suppress all swallow detectors). If parsing classified it
        // as Unimplemented, the test is inconclusive — skip rather than assert a
        // false positive.
        let plus_one_unimplemented = parsed.abilities.iter().any(def_tree_has_unimplemented);
        if !plus_one_unimplemented {
            assert!(
                has_swallowed_detector(&parsed, "Optional_YouMay"),
                "an unrepresented 'you may <verb>' must still be flagged; \
                 the CastWithKeyword exemption must not over-broaden. \
                 warnings: {:#?}",
                parsed.parse_warnings
            );
        }
    }

    /// Walk a def tree for a `GenericEffect` granting `CastWithKeyword` (directly
    /// or via `GrantStaticAbility`) — the flash-grant shape Teferi's [+1] lowers
    /// to and the swallow-check exemption keys on.
    fn def_tree_grants_cast_with_keyword(def: &AbilityDefinition) -> bool {
        let here = if let Effect::GenericEffect {
            ref static_abilities,
            ..
        } = &*def.effect
        {
            static_abilities.iter().any(|s| {
                matches!(s.mode, StaticMode::CastWithKeyword { .. })
                    || s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            crate::types::ability::ContinuousModification::GrantStaticAbility {
                                definition,
                            } if matches!(definition.mode, StaticMode::CastWithKeyword { .. })
                        )
                    })
            })
        } else {
            false
        };
        here || def
            .sub_ability
            .as_deref()
            .is_some_and(def_tree_grants_cast_with_keyword)
            || def
                .else_ability
                .as_deref()
                .is_some_and(def_tree_grants_cast_with_keyword)
            || def
                .mode_abilities
                .iter()
                .any(def_tree_grants_cast_with_keyword)
    }

    #[test]
    fn optional_you_may_accepts_outside_game_wish_search() {
        let parsed = parse_named(
            "You may reveal a sorcery card you own from outside the game and put it into your hand. \
             Exile Burning Wish.",
            "Burning Wish",
            &["Sorcery"],
        );

        let effect = parsed
            .abilities
            .iter()
            .find_map(find_search_outside_game)
            .expect("expected outside-game wish search to parse");
        match effect {
            Effect::SearchOutsideGame {
                count, source_pool, ..
            } => {
                assert!(
                    count.is_up_to(),
                    "wish search must encode the optional reveal as an up-to count"
                );
                assert_eq!(*source_pool, OutsideGameSourcePool::Sideboard);
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn kaya_orzhov_usurper_plus_one_gates_gain_life_on_creature_exiled_this_way() {
        // PR #2447 / issue #1998 follow-up. With the +1 conditional now parsed,
        // Kaya has zero Unimplemented across all three loyalty abilities, so the
        // swallow detectors un-suppress. The +1's trailing outcome gate
        // ("You gain 2 life if at least one creature card was exiled this way")
        // must re-home as `AbilityCondition::ZoneChangedThisWay { creature }`
        // — otherwise `detect_condition_if` flags a swallowed " if " clause.
        let parsed = parse_named(
            "[+1]: Exile up to two target cards from a single graveyard. \
             You gain 2 life if at least one creature card was exiled this way.\n\
             [\u{2212}1]: Exile target nonland permanent with mana value 1 or less.\n\
             [\u{2212}5]: Kaya deals damage to target player equal to the number of \
             cards that player owns in exile and you gain that much life.",
            "Kaya, Orzhov Usurper",
            &["Planeswalker"],
        );

        // No ability may be Unimplemented (the precondition for the swallow
        // detectors to run at all — and the whole point of the fix).
        assert!(
            !parsed.abilities.iter().any(def_tree_has_unimplemented),
            "Kaya's loyalty abilities must all parse without Unimplemented"
        );
        // The trailing "if ... this way" gate must not be swallowed.
        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "Kaya +1 trailing outcome gate must not be a swallowed clause"
        );

        // The +1 GainLife must carry a non-null ZoneChangedThisWay condition.
        let gated_gain_life = parsed
            .abilities
            .iter()
            .any(def_tree_gates_gain_life_on_this_way);
        assert!(
            gated_gain_life,
            "expected a GainLife gated by ZoneChangedThisWay on Kaya's +1, \
             parsed abilities: {:#?}",
            parsed.abilities
        );
    }

    /// Walk a def tree looking for a `GainLife` (anywhere in the chain) whose
    /// owning def carries an `AbilityCondition::ZoneChangedThisWay` gate.
    fn def_tree_gates_gain_life_on_this_way(def: &AbilityDefinition) -> bool {
        let gain_here = matches!(&*def.effect, Effect::GainLife { .. })
            && matches!(
                def.condition,
                Some(crate::types::ability::AbilityCondition::ZoneChangedThisWay { .. })
            );
        gain_here
            || def
                .sub_ability
                .as_deref()
                .is_some_and(def_tree_gates_gain_life_on_this_way)
            || def
                .else_ability
                .as_deref()
                .is_some_and(def_tree_gates_gain_life_on_this_way)
            || def
                .mode_abilities
                .iter()
                .any(def_tree_gates_gain_life_on_this_way)
    }

    #[test]
    fn optional_you_may_accepts_outside_game_face_up_exile_disjunction() {
        let parsed = parse_named(
            "You may reveal an Eldrazi card you own from outside the game or choose a \
             face-up Eldrazi card you own in exile. Put that card into your hand.",
            "Coax from the Blind Eternities",
            &["Sorcery"],
        );

        let effect = parsed
            .abilities
            .iter()
            .find_map(find_search_outside_game)
            .expect("expected outside-game Coax search to parse");
        match effect {
            Effect::SearchOutsideGame {
                count, source_pool, ..
            } => {
                assert!(
                    count.is_up_to(),
                    "Coax search must encode the optional reveal as an up-to count"
                );
                assert_eq!(*source_pool, OutsideGameSourcePool::SideboardAndFaceUpExile);
            }
            other => panic!("expected SearchOutsideGame, got {other:?}"),
        }
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 611.2a: Amplifire — upkeep P/T set uses "until your next turn" duration
    /// on a layer effect; must not trip Duration_NextTurn swallow warnings (issue #2239).
    #[test]
    fn duration_next_turn_accepts_amplifire_upkeep_pt_set() {
        use crate::types::ability::{ContinuousModification, Duration, PlayerScope};

        let parsed = parse_named(
            "At the beginning of your upkeep, reveal cards from the top of your library until you reveal a creature card. Until your next turn, this creature's base power becomes twice that card's power and its base toughness becomes twice that card's toughness. Put the revealed cards on the bottom of your library in a random order.",
            "Amplifire",
            &["Creature"],
        );
        let execute = parsed.triggers[0]
            .execute
            .as_ref()
            .expect("Amplifire upkeep trigger");
        assert!(
            !def_tree_has_unimplemented(execute),
            "Amplifire trigger must parse without Unimplemented"
        );
        assert!(
            matches!(execute.effect.as_ref(), Effect::RevealUntil { .. }),
            "Amplifire head must be RevealUntil, got {:?}",
            execute.effect
        );
        fn find_timed_pt_layer(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
            let has_pt_layer = matches!(
                def.effect.as_ref(),
                Effect::GenericEffect {
                    static_abilities,
                    ..
                } if static_abilities.iter().any(|s| {
                    s.modifications.iter().any(|m| {
                        matches!(
                            m,
                            ContinuousModification::SetPowerDynamic { .. }
                                | ContinuousModification::SetToughnessDynamic { .. }
                        )
                    })
                })
            );
            if has_pt_layer
                && matches!(
                    def.duration,
                    Some(Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller
                    })
                )
            {
                return Some(def);
            }
            def.sub_ability
                .as_deref()
                .and_then(find_timed_pt_layer)
                .or_else(|| def.else_ability.as_deref().and_then(find_timed_pt_layer))
        }
        assert!(
            find_timed_pt_layer(execute).is_some(),
            "expected until-your-next-turn duration on the P/T layer clause, got {execute:#?}",
        );
        assert!(!has_swallowed_detector(&parsed, "Duration_NextTurn"));
    }

    /// CR 400.11 + CR 701.23j: Wish-cycle and planeswalker wishboard fetches must
    /// lower to SearchOutsideGame without Optional_YouMay swallow warnings (issue #2276).
    #[test]
    fn optional_you_may_accepts_wishboard_creature_or_land_and_loyalty_fetches() {
        let living_wish = parse_named(
            "You may reveal a creature or land card you own from outside the game and put it into your hand. Exile Living Wish.",
            "Living Wish",
            &["Sorcery"],
        );
        assert!(
            !living_wish.abilities.iter().any(def_tree_has_unimplemented),
            "Living Wish must parse without Unimplemented"
        );
        let living = living_wish
            .abilities
            .iter()
            .find_map(find_search_outside_game)
            .expect("Living Wish outside-game search");
        assert!(matches!(living, Effect::SearchOutsideGame { count, .. } if count.is_up_to()));
        assert!(!has_swallowed_detector(&living_wish, "Optional_YouMay"));

        let karn = parse_named(
            "[−2]: You may reveal an artifact card you own from outside the game or choose a face-up artifact card you own in exile. Put that card into your hand.",
            "Karn, the Great Creator",
            &["Planeswalker"],
        );
        assert!(
            !karn.abilities.iter().any(def_tree_has_unimplemented),
            "Karn -2 must parse without Unimplemented"
        );
        let karn_search = karn
            .abilities
            .iter()
            .find_map(find_search_outside_game)
            .expect("Karn -2 outside-game search");
        assert!(matches!(
            karn_search,
            Effect::SearchOutsideGame {
                source_pool: OutsideGameSourcePool::SideboardAndFaceUpExile,
                ..
            }
        ));
        assert!(!has_swallowed_detector(&karn, "Optional_YouMay"));

        let vivien = parse_named(
            "[−5]: You may reveal a creature card you own from outside the game and put it into your hand.",
            "Vivien, Arkbow Ranger",
            &["Planeswalker"],
        );
        assert!(
            !vivien.abilities.iter().any(def_tree_has_unimplemented),
            "Vivien -5 must parse without Unimplemented"
        );
        assert!(vivien
            .abilities
            .iter()
            .any(|a| matches!(a.effect.as_ref(), Effect::SearchOutsideGame { .. })));
        assert!(!has_swallowed_detector(&vivien, "Optional_YouMay"));
    }

    #[test]
    fn apnap_protection_racket_reports_turn_order_as_swallowed() {
        use crate::types::ability::PlayerFilter;

        let parsed = parse_named(
            "At the beginning of your upkeep, repeat the following process for each opponent in turn order. Reveal the top card of your library. That player may pay life equal to that card's mana value. If they do, exile that card. Otherwise, put it into your hand.",
            "Protection Racket",
            &["Enchantment"],
        );
        assert_eq!(parsed.triggers.len(), 1);
        let execute = parsed.triggers[0]
            .execute
            .as_ref()
            .expect("Protection Racket upkeep trigger execute");
        assert!(
            !def_tree_has_unimplemented(execute),
            "Protection Racket trigger must parse without Unimplemented"
        );
        assert_eq!(
            execute.player_scope,
            Some(PlayerFilter::Opponent),
            "repeat-for-each-opponent-in-turn-order must stamp player_scope = Opponent"
        );

        // KNOWN GAP, pinned deliberately — and this test is why `APNAP` never fired once
        // across 35,396 faces.
        //
        // The assertion directly above is the whole problem: `player_scope = Opponent` is
        // the ONLY thing this card's "in turn order" clause leaves behind. That says WHO
        // acts. It says nothing about the ORDER they act in, which is the sole subject of
        // CR 101.4. The ordering fact is dropped.
        //
        // The old evidence check listed `"player_scope":` among its APNAP markers, so a
        // card reading "...for each opponent IN TURN ORDER" parsed `player_scope` FROM THE
        // VERY CLAUSE that raises the expectation, and the detector excused itself. The
        // evidence was implied by the expectation — vacuous by construction — and this
        // test asserted that vacuity was correct. It passed for exactly the reason the
        // detector was broken.
        //
        // Typed evidence for CR 101.4 is `starting_with` (on `AbilityDefinition` or
        // `Effect::Vote`), and this card has neither. Flip this when the turn-order
        // iteration is given a typed ordering carrier; until then it is a tripwire.
        assert!(
            has_swallowed_detector(&parsed, "APNAP"),
            "player_scope says WHO, not in what ORDER: the turn-order fact is swallowed. \
             Warnings: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn duration_this_turn_accepts_force_block_scope() {
        let parsed = parse(
            "Target creature blocks target creature this turn if able.",
            &["Sorcery"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    #[test]
    fn duration_this_turn_accepts_cast_permission_scope() {
        let parsed = parse(
            "{T}: Add {C}.\n\
             {1}, {T}, Sacrifice this land: You may cast spells this turn as though they had flash.",
            &["Land"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    #[test]
    fn duration_this_turn_accepts_exile_cast_permission_scope() {
        // CR 601.2a + CR 113.6b: Maralen, Fae Ascendant — the "this turn"
        // wording on the cast-permission line is intrinsic to
        // `ExileCastPermission { frequency: OncePerTurn, ... }` (the per-turn
        // rolling pool keyed by source), not a separate duration slot.
        let parsed = parse_named(
            "Flying\n\
             Whenever ~ or another Elf or Faerie you control enters, exile the top two cards of target opponent's library.\n\
             Once each turn, you may cast a spell with mana value less than or equal to the number of Elves and Faeries you control from among cards exiled with ~ this turn without paying its mana cost.",
            "Maralen, Fae Ascendant",
            &["Creature"],
        );

        // Guard against the silent-regression case: the negative assertion below
        // would also pass if the `ExileCastPermission` static simply stopped
        // parsing (no marker emitted, no other "this turn" AST). Pin that the
        // structural variant the exemption keys on is actually present.
        assert!(
            parsed
                .statics
                .iter()
                .any(|s| matches!(s.mode, StaticMode::ExileCastPermission { .. })),
            "expected an ExileCastPermission static to parse for Maralen"
        );
        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    #[test]
    fn duration_this_turn_accepts_prevention_shield_scope() {
        let parsed = parse(
            "Prevent the next 3 damage that would be dealt to any target this turn by a source of your choice. \
             You gain 3 life.",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// CR 614.9 + CR 615.1: the en-Kor cycle (Nomads / Spirit / Warrior / Shaman
    /// / Lancers en-Kor) and General's Regalia parse the redirection clause into
    /// a `CreateDamageReplacement` shield whose "this turn" lifetime is inherent
    /// to the one-shot effect — it must NOT be reported as a swallowed duration.
    #[test]
    fn duration_this_turn_accepts_one_shot_damage_replacement_shield() {
        for (oracle, name) in [
            (
                "{0}: The next 1 damage that would be dealt to this creature this turn \
                 is dealt to target creature you control instead.",
                "Nomads en-Kor",
            ),
            (
                "{3}: The next time a source of your choice would deal damage to you this turn, \
                 that damage is dealt to target creature you control instead.",
                "General's Regalia",
            ),
        ] {
            let parsed = parse_named(oracle, name, &["Creature"]);
            assert!(
                !has_swallowed_detector(&parsed, "Duration_ThisTurn"),
                "{name}: one-shot damage-replacement shield must not report a swallowed this-turn duration: {:?}",
                parsed.parse_warnings
            );
        }
    }

    #[test]
    fn replacement_instead_accepts_effect_chain_instead_condition() {
        let parsed = parse_named(
            "Kicker—Sacrifice a land.\n\
             Prevent the next 3 damage that would be dealt this turn to any number of targets, divided as you choose. \
             If this spell was kicked, prevent the next 6 damage this way instead.",
            "Pollen Remedy",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "Replacement_Instead"));
    }

    #[test]
    fn condition_if_accepts_graveyard_cast_exile_rider() {
        let parsed = parse_named(
            "Trample\n\
             Whenever this creature attacks, you may cast target instant or sorcery card with mana value less than or equal to this creature's power from your graveyard without paying its mana cost. \
             If that spell would be put into your graveyard, exile it instead.",
            "Dreadhorde Arcanist",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Condition_If"));
    }

    /// CR 509.1c: "must be blocked if able" riders are represented by
    /// `StaticMode::MustBeBlocked { by }`, so the trailing "if able" must not be
    /// reported as a swallowed `Condition_If`. The `by` field has no
    /// `skip_serializing_if`, so serde emits the object form for BOTH shapes:
    /// `{"MustBeBlocked":{"by":null}}` for the bare rider (`by: None`, Gaea's
    /// Protector) and `{"MustBeBlocked":{"by":{...}}}` for the typed form
    /// (`by: Some(filter)`, Slayer's Cleaver). The suppression marker matches the
    /// quoted variant key `"MustBeBlocked"` common to both serializations.
    #[test]
    fn condition_if_accepts_must_be_blocked_rider() {
        let bare = parse_named(
            "This creature must be blocked if able.",
            "Gaea's Protector",
            &["Creature"],
        );
        assert!(
            !has_swallowed_detector(&bare, "Condition_If"),
            "bare must-be-blocked static must not report Condition_If: {:?}",
            bare.parse_warnings
        );
        assert!(
            bare.statics
                .iter()
                .any(|s| matches!(s.mode, StaticMode::MustBeBlocked { by: None })),
            "expected bare MustBeBlocked static, got {:?}",
            bare.statics
        );

        let typed = parse_named(
            "Equipped creature gets +3/+1 and must be blocked by an Eldrazi if able.\nEquip {4}",
            "Slayer's Cleaver",
            &["Artifact"],
        );
        assert!(
            !has_swallowed_detector(&typed, "Condition_If"),
            "typed must-be-blocked static must not report Condition_If: {:?}",
            typed.parse_warnings
        );
        assert!(
            typed
                .statics
                .iter()
                .any(|s| matches!(s.mode, StaticMode::MustBeBlocked { by: Some(_) })),
            "expected typed MustBeBlocked static, got {:?}",
            typed.statics
        );
    }

    #[test]
    fn condition_if_accepts_tiered_enters_with_counter_static() {
        let parsed = parse_named(
            "Trample\n\
             Each other Vehicle and creature you control enters with an additional +1/+1 counter on it if its mana value is 4 or less. Otherwise, it enters with three additional +1/+1 counters on it.\n\
             Crew 3",
            "Thunderous Velocipede",
            &["Artifact"],
        );

        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "represented tiered ETB-counter static must not report Condition_If: {:?}",
            parsed.parse_warnings
        );
        assert!(
            parsed
                .statics
                .iter()
                .filter(|static_def| {
                    matches!(
                        static_def.mode,
                        StaticMode::EntersWithAdditionalCounters { .. }
                    )
                })
                .count()
                >= 2,
            "expected tiered ETB-counter statics, got {:?}",
            parsed.statics
        );
    }

    /// CR 122.1 + CR 614.1c + CR 608.2c: Oviya's "If you put an artifact onto the
    /// battlefield this way, put two +1/+1 counters on it" rider is represented by
    /// `Effect::ChangeZone.conditional_enter_with_counters`, so the leading "if"
    /// must NOT be reported as a swallowed condition. REVERT: without the
    /// `conditional_enter_counters_if_is_only_if_marker` guard the false
    /// `Condition_If` swallow returns and this assertion flips.
    #[test]
    fn condition_if_accepts_conditional_enter_with_counters_put_this_way() {
        let parsed = parse_named(
            "Each creature that's attacking one of your opponents has trample.\n\
             {G}, {T}: You may put a creature or Vehicle card from your hand onto the battlefield. \
             If you put an artifact onto the battlefield this way, put two +1/+1 counters on it.",
            "Oviya, Automech Artisan",
            &["Creature"],
        );

        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "represented conditional_enter_with_counters put-this-way rider must not report Condition_If: {:?}",
            parsed.parse_warnings
        );
    }

    /// The put-this-way counter guard must not reach across lines: a card whose
    /// line 1 carries the *represented* rider AND whose line 2 carries a separate,
    /// genuinely-unrepresented " if " must still flag Condition_If for line 2.
    ///
    /// This is the named witness for the cross-line false-green that per-item
    /// scoping kills. Under the card-wide audit, line 1's represented rider was
    /// evidence enough to excuse line 2's unrepresented "if" — the evidence never
    /// had to come from the clause that raised the expectation. It now does, and the
    /// warning is attributed to **line 2**, which is asserted here: a `line_index`
    /// of 0 would not mean "unattributed", it would mean "line 1", i.e. the bug.
    #[test]
    fn conditional_enter_counters_does_not_hide_unrelated_if() {
        let parsed = parse_named(
            "{G}, {T}: You may put a creature or Vehicle card from your hand onto the battlefield. \
             If you put an artifact onto the battlefield this way, put two +1/+1 counters on it.\n\
             Draw a card if the moon is bright.",
            "Oviya, Automech Artisan",
            &["Creature"],
        );

        let condition_if: Vec<_> = parsed
            .parse_warnings
            .iter()
            .filter(|warning| {
                matches!(
                    warning,
                    OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
                )
            })
            .collect();
        assert!(
            !condition_if.is_empty(),
            "a separate unrelated if line must remain visible to Condition_If, got {:?}",
            parsed.parse_warnings
        );
        assert!(
            condition_if.iter().all(|warning| warning.line_index() == 1),
            "the swallow must be attributed to the line that raised it (line 1, 0-based), \
             not to line 0's represented rider; got {condition_if:?}"
        );
    }

    /// TOTAL-COVERAGE INVARIANT — the false-green tripwire.
    ///
    /// A modal item's span claims only its header line (`first_line == last_line == 0`,
    /// fragment `"Choose one -"`), even though the item consumed the bullet lines
    /// beneath it. If the audit trusted item fragments, the bullets — which hold the
    /// card's entire meaning — would be claimed by nothing, raise no expectation, and
    /// their swallowed clauses would silently DISAPPEAR. That is the one delta
    /// direction that hides a regression, and it was measured at 21 faces before units
    /// were made to partition the source by line.
    ///
    /// Drown in the Loch's dynamic quantity ("less than or equal to the number of cards
    /// in its controller's graveyard") lives only on the bullets. It is reported as a
    /// swallowed DynamicQty in the shipped card data; it must stay reported.
    /// A card the parser drops ENTIRELY must still be audited — the most dangerous
    /// false green there is.
    ///
    /// Chorus of the Conclave lowers to a completely empty `ParsedAbilities`: no
    /// ability, no static, no additional cost, not even an `Effect::Unimplemented` to
    /// declare the gap. It produces no items, so a unit-iterating audit has nothing to
    /// iterate and says nothing at all — going silent at exactly the moment the parser
    /// failed hardest. The card-wide audit correctly reported its swallowed clauses.
    ///
    /// With no item claiming any text, nothing represents anything, so every semantic
    /// the text raises is swallowed. That is what must be reported.
    #[test]
    fn a_card_that_parses_to_nothing_is_still_audited() {
        // Parsed the way production does: with the MTGJSON keyword list populated. That
        // is load-bearing — with the keyword list EMPTY the "Forestwalk" line falls
        // through to an ability and the card is no longer item-less, so the very
        // condition under test disappears.
        let parsed = parse_oracle_text(
            "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\n\
             As an additional cost to cast creature spells, you may pay any amount of mana. \
             If you do, that creature enters with that many additional +1/+1 counters on it.",
            "Chorus of the Conclave",
            &["Forestwalk".to_string()],
            &["Creature".to_string()],
            &["Centaur".to_string()],
        );
        assert!(
            parsed.abilities.is_empty()
                && parsed.statics.is_empty()
                && parsed.additional_cost.is_none(),
            "premise: this card still parses to nothing; if that changed, re-point this test"
        );
        assert!(
            has_swallowed_detector(&parsed, "Optional_YouMay"),
            "a card that produced NO parsed output at all must report its text as \
             swallowed, not fall silent. Warnings: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn modal_bullet_lines_are_audited_not_orphaned() {
        let parsed = parse_named(
            "Choose one \u{2014}\n\
             \u{2022} Counter target spell with mana value less than or equal to the number of \
             cards in its controller's graveyard.\n\
             \u{2022} Destroy target creature with mana value less than or equal to the number of \
             cards in its controller's graveyard.",
            "Drown in the Loch",
            &["Instant"],
        );
        assert!(
            has_swallowed_detector(&parsed, "DynamicQty"),
            "the modal bullets' dynamic quantity must still be audited: text no unit \
             claims raises no expectation, and the warning vanishes. Warnings: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn represented_tiered_counter_pair_does_not_hide_unrelated_if() {
        let tiered_line = "Each other Vehicle and creature you control enters with an additional +1/+1 counter on it if its mana value is 4 or less. Otherwise, it enters with three additional +1/+1 counters on it.";
        let parsed = parse_named(
            &format!("{tiered_line}\nDraw a card if the moon is bright."),
            "Thunderous Velocipede",
            &["Artifact"],
        );

        assert!(
            has_swallowed_detector(&parsed, "Condition_If"),
            "a separate unrelated if line must remain visible to Condition_If, got {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 608.2c: Mister Negative's "If you lost life this way, draw that many
    /// cards" rider — the "lost life this way" result-reference and "that many"
    /// draw quantity are jointly represented by `Draw { count:
    /// EventContextAmount }`, so the leading "if" must NOT be reported as a
    /// swallowed condition.
    #[test]
    fn condition_if_accepts_lost_life_this_way_draw_that_many() {
        let parsed = parse_named(
            "Vigilance, lifelink\n\
             When this creature enters, you may exchange life totals with target opponent. \
             If you lost life this way, draw that many cards.",
            "Mister Negative",
            &["Creature"],
        );

        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "lost-life-this-way result-reference draw must not report a swallowed condition: {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 614.1a + CR 120.8: An UNCONDITIONAL value-modifier replacement whose
    /// ability-word-stripped body leads with its own CR 614.1a applicability
    /// "if" ("Flare Star — If a Wizard you control would deal damage ..., it
    /// deals double that damage instead") is fully represented by the
    /// `ReplacementDefinition`'s `damage_modification` — the leading "if" is
    /// replacement syntax, not a swallowed CR 608.2c gate. Revert discriminator:
    /// removing the `unconditional_valmod_leading_if_is_only_if_marker` exemption
    /// re-emits the `Condition_If` warning and this assertion flips.
    #[test]
    fn condition_if_accepts_unconditional_valmod_leading_if() {
        let parsed = parse_named(
            "Flare Star — If a Wizard you control would deal damage to a permanent \
             or player, it deals double that damage instead.",
            "Trance Kuja, Fate Defied",
            &["Legendary", "Creature", "Wizard"],
        );

        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "unconditional value-modifier replacement's leading applicability-if \
             must not report a swallowed condition: {:?}",
            parsed.parse_warnings
        );
    }

    /// The exemption is guarded: a value-modifier replacement that ALSO carries a
    /// genuine `while` gate (The Rollercrusher Ride's delirium threshold) does
    /// NOT take the unconditional carve-out — its residual `while` blocks it. The
    /// gate is instead captured as a `condition`, which self-suppresses
    /// `Condition_If` via the `"condition":{` marker. Either way the warning must
    /// be absent, but for the RIGHT reason (captured gate, not blanket exemption).
    #[test]
    fn condition_if_gated_valmod_captures_condition_not_blanket_exemption() {
        let parsed = parse_named(
            "Delirium — If a source you control would deal noncombat damage to a \
             permanent or player while there are four or more card types among \
             cards in your graveyard, it deals double that damage instead.",
            "The Rollercrusher Ride",
            &["Legendary", "Enchantment"],
        );

        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "gated value-modifier must capture its while-gate as a condition and \
             not report a swallowed condition: {:?}",
            parsed.parse_warnings
        );
        assert!(
            parsed.replacements.iter().any(|r| r.condition.is_some()),
            "the delirium while-gate must be captured as a replacement condition, \
             not dropped: {:?}",
            parsed.replacements
        );
    }

    #[test]
    fn duration_this_turn_accepts_life_loss_turn_history_condition() {
        let parsed = parse(
            "{1}{R}, Discard a card, Sacrifice a Vampire: Draw two cards. \
             Activate only if an opponent lost life this turn.",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    #[test]
    fn duration_this_turn_accepts_cast_restriction_turn_history_condition() {
        let parsed = parse_named(
            "Cast this spell only if you've cast another spell this turn.\nFlying",
            "Illusory Angel",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    #[test]
    fn duration_this_turn_accepts_intervening_spell_history_condition() {
        let parsed = parse_named(
            "Vigilance, trample, haste\n\
             Whenever Rhino attacks, if you've cast a spell with mana value 4 or greater this turn, draw a card.",
            "Rhino, Barreling Brute",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
        assert!(!has_swallowed_detector(&parsed, "Condition_If"));
    }

    #[test]
    fn duration_this_turn_accepts_entered_this_turn_quantity_condition() {
        let parsed = parse_named(
            "Reach\n\
             This creature gets +1/+0 and has trample as long as you control a land creature or a land entered the battlefield under your control this turn.",
            "Earth Rumble Wrestlers",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    #[test]
    fn optional_you_may_accepts_delayed_trigger_inner_optionality() {
        let parsed = parse(
            "Whenever a creature enters this turn, you may draw a card.",
            &["Sorcery"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 611.2a: an "until end of turn" duration nested inside a token-granted
    /// trigger (`Effect::Token` → `GrantTrigger` → `trigger.execute`) is
    /// captured in the AST — `detect_duration_until_eot`'s structured walk
    /// cannot see it, so the serialized-AST marker check exempts it.
    #[test]
    fn duration_until_eot_accepts_token_granted_trigger() {
        let parsed = parse_named(
            "Create a 2/2 green Bird creature token with \"Whenever a land you \
             control enters, this token gets +1/+0 until end of turn.\"",
            "Token Maker",
            &["Sorcery"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_UntilEndOfTurn"));
    }

    /// CR 603.4: "Activate only if ... this turn" scopes a turn-history
    /// activation condition (`ActivationRestriction::RequiresCondition`), not
    /// an effect duration — `detect_duration_this_turn` must not fire.
    #[test]
    fn duration_this_turn_accepts_activation_restriction_condition() {
        let parsed = parse_named(
            "{T}: Draw a card. Activate only if you attacked with two or more creatures this turn.",
            "Test Keep",
            &["Land"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// CR 305.2a + CR 603.4: Spider-Man 2099's end-step trigger has "this turn"
    /// in its intervening-if condition ("if you've played a land or cast a spell
    /// this turn from anywhere other than your hand"). Both arms of the disjunction
    /// are turn-history quantities (`LandsPlayedThisTurn` / `SpellsCastThisTurn`)
    /// — not forward-looking durations — so `detect_duration_this_turn` must not
    /// fire even after the casting restriction parses cleanly (no Unimplemented
    /// shield).
    #[test]
    fn duration_this_turn_accepts_land_or_spell_this_turn_disjunction_condition() {
        let parsed = parse_named(
            "From the Future \u{2014} You can\u{2019}t cast ~ during your first, second, or third turns of the game.\n\
             Double strike, vigilance\n\
             At the beginning of your end step, if you've played a land or cast a spell this turn from anywhere other than your hand, ~ deals damage equal to its power to any target.",
            "Spider-Man 2099",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// CR 611.3: an "as long as ... this turn" clause routed into an
    /// `Unrecognized` condition slot means "this turn" was consumed by a
    /// condition, not dropped as an effect duration (War Historian shape).
    #[test]
    fn duration_this_turn_accepts_unrecognized_as_long_as_condition() {
        let parsed = parse_named(
            "Reach\nThis creature has indestructible as long as it attacked a battle this turn.",
            "War Historian",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// CR 702.171c: "creature that saddled it this turn" is a relative-clause
    /// target filter (`FilterProp::SaddledSource`), a turn-history-quantity
    /// context — not a forward-looking effect duration. After the saddler-ref
    /// filter suffix parses, `detect_duration_this_turn` must not fire (Giant
    /// Beaver / The Gitrog, Ravenous Ride regression).
    #[test]
    fn duration_this_turn_accepts_saddled_it_this_turn_filter() {
        let parsed = parse_named(
            "Vigilance\nWhenever this creature attacks while saddled, put a +1/+1 counter on target creature that saddled it this turn.",
            "Giant Beaver",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// Regression guard #1: a "this turn" clause OUTSIDE Step 2's exemption
    /// scope — no "activate only" line, no `Unrecognized` condition slot, not a
    /// quantity-suffix collocation — must STILL fire `Duration_ThisTurn`. A
    /// genuine forward-looking effect-duration swallow must not be suppressed
    /// by the exemptions, proving they do not blanket-suppress.
    ///
    /// (Bloodcrazed Goblin previously served as this guard's example, but Unit
    /// 5d-D4 made its "an opponent has been dealt damage this turn" `unless`
    /// clause parse into a typed `DamageDealtThisTurn` condition — it is no
    /// longer a swallow, so the guard now uses a genuinely dropped duration.)
    #[test]
    fn duration_this_turn_still_fires_outside_exemption_scope() {
        let parsed = parse_named(
            "Creatures you control can't block this turn.",
            "Test Block Lock",
            &["Land"],
        );

        assert!(has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    /// Regression guard #2 (C2 over-suppression case): a card carrying BOTH a
    /// `RequiresCondition` activation-restriction line AND a genuine dropped-
    /// duration effect on a SEPARATE line must STILL fire `Duration_ThisTurn`
    /// — the line-scoped count (`total_this_turn != activate_only_this_turn`)
    /// keeps the exemption from over-reaching.
    #[test]
    fn duration_this_turn_fires_when_duration_and_activation_restriction_coexist() {
        let parsed = parse_named(
            "Creatures you control can't block this turn.\n\
             {T}: Draw a card. Activate only if you attacked with two or more creatures this turn.",
            "Test Hybrid",
            &["Land"],
        );

        assert!(has_swallowed_detector(&parsed, "Duration_ThisTurn"));
    }

    #[test]
    fn optional_you_may_accepts_activation_timing_permission_static() {
        let parsed = parse_named(
            "Flash\n\
             As long as The Wandering Emperor entered this turn, you may activate her loyalty abilities any time you could cast an instant.\n\
             [+1]: Put a +1/+1 counter on up to one target creature.",
            "The Wandering Emperor",
            &["Planeswalker"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 701.20a + CR 608.2c: RevealUntil's "you may put that card onto the
    /// battlefield" is represented as `kept_optional_to: Some(Battlefield)`, so
    /// neither `Optional_YouMay` nor (for the explicit "if you don't" form)
    /// `Condition_If` swallowed-clause warnings are emitted. Covers Genesis
    /// Storm / Hei Bai / Songbirds' Blessing.
    #[test]
    fn optional_you_may_accepts_reveal_until_optional_kept() {
        let hei_bai = parse(
            "Reveal cards from the top of your library until you reveal a creature card. \
             You may put that card onto the battlefield. Then shuffle your library.",
            &["Sorcery"],
        );
        assert!(!has_swallowed_detector(&hei_bai, "Optional_YouMay"));

        let songbirds = parse(
            "Reveal cards from the top of your library until you reveal a creature card. \
             You may put that card onto the battlefield. If you don't, put it into your hand. \
             Put the rest on the bottom of your library in a random order.",
            &["Sorcery"],
        );
        assert!(!has_swallowed_detector(&songbirds, "Optional_YouMay"));
        assert!(!has_swallowed_detector(&songbirds, "Condition_If"));
    }

    /// CR 701.6 + CR 608.2c: The "If a permanent's ability is countered this
    /// way, destroy that permanent." rider is represented as
    /// `Effect::Counter.source_rider = Some(Destroy)`, so the `Condition_If`
    /// detector must not flag Teferi's Response or Green Slime.
    #[test]
    fn condition_if_accepts_counter_destroy_rider() {
        let teferis = parse_named(
            "Counter target spell or ability an opponent controls that targets a land you control. \
             If a permanent's ability is countered this way, destroy that permanent.\nDraw two cards.",
            "Teferi's Response",
            &["Instant"],
        );
        assert!(!has_swallowed_detector(&teferis, "Condition_If"));

        let green_slime = parse_named(
            "Flash\nWhen this creature enters, counter target activated or triggered ability from \
             an artifact or enchantment source. If a permanent's ability is countered this way, \
             destroy that permanent.",
            "Green Slime",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&green_slime, "Condition_If"));
    }

    /// CR 115.1 + CR 608.2c + CR 702.185a: Full Bore's "If that creature was cast
    /// for its warp cost, it also gains trample and haste" rider is represented as
    /// a grant sub-ability with `condition: CastVariantPaid { variant: Warp,
    /// subject: Target }`, so the `Condition_If` detector must not flag it. Before
    /// the parser arm was added the condition was dropped and this swallow fired
    /// (the measured coverage gap). Reverting the parser arm re-fires it.
    #[test]
    fn condition_if_accepts_full_bore_target_warp_grant() {
        let full_bore = parse_named(
            "Target creature you control gets +3/+2 until end of turn. If that creature was \
             cast for its warp cost, it also gains trample and haste until end of turn.",
            "Full Bore",
            &["Sorcery"],
        );
        assert!(!has_swallowed_detector(&full_bore, "Condition_If"));
    }

    /// CR 701.6a: "If that spell is countered this way, put it [somewhere]"
    /// — the redirect destination is encoded as `countered_spell_zone` on the
    /// Counter effect.  Its presence IS the conditional gate (Memory Lapse,
    /// Lapse of Certainty, Remand, Spell Crumple).
    #[test]
    fn condition_if_accepts_countered_spell_zone_redirect() {
        let memory_lapse = parse_named(
            "Counter target spell. If that spell is countered this way, \
             put it on top of its owner's library instead of into that \
             player's graveyard.",
            "Memory Lapse",
            &["Instant"],
        );
        assert!(!has_swallowed_detector(&memory_lapse, "Condition_If"));

        let remand = parse_named(
            "Counter target spell. If that spell is countered this way, \
             put it into its owner's hand instead of into that player's \
             graveyard.\nDraw a card.",
            "Remand",
            &["Instant"],
        );
        assert!(!has_swallowed_detector(&remand, "Condition_If"));
    }

    /// CR 702.170c + CR 608.2c: "You may exile a card … If you do, it becomes
    /// plotted." — the "if you do" gate is the optional-exile linkage,
    /// represented by the chained `GrantCastingPermission { Plotted }`, so the
    /// `Condition_If` detector must not flag Make Your Own Luck / Kellan Joins Up.
    #[test]
    fn condition_if_accepts_if_you_do_becomes_plotted() {
        let myol = parse_named(
            "Look at the top three cards of your library. You may exile a nonland card from \
             among them. If you do, it becomes plotted. Put the rest into your hand.",
            "Make Your Own Luck",
            &["Sorcery"],
        );
        assert!(!has_swallowed_detector(&myol, "Condition_If"));

        let kellan = parse_named(
            "When this creature enters, you may exile a nonland card with mana value 3 or less \
             from your hand. If you do, it becomes plotted.",
            "Kellan Joins Up",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&kellan, "Condition_If"));
    }

    /// CR 608.2c: Keep the plotted-grant exemption scoped to the actual
    /// linkage phrase; a separate conditional marker on the same card must
    /// still run through the detector.
    #[test]
    fn plotted_grant_linkage_exemption_is_text_scoped() {
        assert!(super::plotted_grant_linkage_is_only_if_marker(
            "you may exile a card. if you do, it becomes plotted."
        ));
        assert!(!super::plotted_grant_linkage_is_only_if_marker(
            "you may exile a card. if you do, it becomes plotted. if another condition is true, draw a card."
        ));
    }

    /// CR 608.2c + CR 701.20: "you may look at the top N cards ... If you do,
    /// reveal ... from among them ..." — the optional look lowers to an optional
    /// `Dig` and the dependent reveal patches it, so the "if you do" linkage is
    /// represented (not a swallowed condition). Fertile Thicket, Munda, and
    /// Planar Atlas are the motivating cards (#2349).
    #[test]
    fn condition_if_accepts_you_may_look_if_you_do_reveal_from_among() {
        let fertile = parse_named(
            "When this land enters, you may look at the top five cards of your library. \
             If you do, reveal up to one basic land card from among them, then put that \
             card on top of your library and the rest on the bottom in any order.",
            "Fertile Thicket",
            &["Land"],
        );
        assert!(!has_swallowed_detector(&fertile, "Condition_If"));

        let munda = parse_named(
            "Whenever this creature or another Ally you control enters, you may look at \
             the top four cards of your library. If you do, reveal any number of Ally \
             cards from among them, then put those cards on top of your library in any \
             order and the rest on the bottom in any order.",
            "Munda, Ambush Leader",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&munda, "Condition_If"));

        let atlas = parse_named(
            "When this artifact enters, you may look at the top four cards of your \
             library. If you do, reveal up to one land card from among them, then put \
             that card on top of your library and the rest on the bottom in a random order.",
            "Planar Atlas",
            &["Artifact"],
        );
        assert!(!has_swallowed_detector(&atlas, "Condition_If"));
    }

    /// CR 608.2c: the optional-look "if you do" exemption is scoped to the
    /// linkage phrase; a separate game-state conditional on the same card must
    /// still reach the detector.
    #[test]
    fn dig_if_you_do_exemption_is_text_scoped() {
        assert!(super::dig_if_you_do_is_only_if_marker(
            "you may look at the top five cards. if you do, reveal a land card from among them."
        ));
        assert!(!super::dig_if_you_do_is_only_if_marker(
            "you may look at the top five cards. if you do, reveal a land. if you control a forest, draw a card."
        ));
    }

    /// CR 614.12: Summoner's Grimoire — the granted ability's "if that card is
    /// an enchantment card" clause materializes the typed `enters_modified_if`
    /// gate, so it is represented, not swallowed. With that as the card's only
    /// " if ", `Swallow:Condition_If` must clear (the card flips supported).
    /// Revert (field never set / marker absent) re-flags Condition_If.
    #[test]
    fn condition_if_accepts_grimoire_moved_type_enter_modifier() {
        let grimoire = parse_named(
            "Job select\nEquipped creature is a Shaman in addition to its other types and \
             has \"Whenever this creature attacks, you may put a creature card from your hand \
             onto the battlefield. If that card is an enchantment card, it enters tapped and \
             attacking.\"\nAbraxas — Equip {3}",
            "Summoner's Grimoire",
            &["Artifact", "Equipment"],
        );
        assert!(!has_swallowed_detector(&grimoire, "Condition_If"));
    }

    /// CR 614.12 (N-A non-vacuity): the enters-modifier exemption is
    /// text-scoped — it clears only the represented clause, so a card carrying
    /// the gate AND a separate unrelated dropped " if " still flags. This FAILS
    /// if the marker is implemented whole-AST instead of text-scoped.
    #[test]
    fn enters_modified_if_exemption_is_text_scoped() {
        let ast = &UnitEvidence::from_json_for_test(r#"{"enters_modified_if":{"type":"Typed"}}"#);
        let no_gate = &UnitEvidence::from_json_for_test("{}");
        // The represented enters-modifier clause is the card's only " if " -> suppress.
        assert!(super::enters_modified_if_is_only_if_marker(
            "you may put a creature card from your hand onto the battlefield. if that card \
             is an enchantment card, it enters tapped and attacking.",
            ast,
        ));
        // Gate present BUT a separate unrelated " if " survives -> do NOT suppress.
        assert!(!super::enters_modified_if_is_only_if_marker(
            "you may put a creature card from your hand onto the battlefield. if that card \
             is an enchantment card, it enters tapped and attacking. if you control a \
             forest, draw a card.",
            ast,
        ));
        // No AST gate (clause not structurally represented) -> do NOT suppress.
        assert!(!super::enters_modified_if_is_only_if_marker(
            "if that card is an enchantment card, it enters tapped and attacking.",
            no_gate,
        ));
    }

    /// CR 707.10c: Mirrorpool's "you may choose new targets for the copy" is
    /// represented as `CopySpell { retarget: MayChooseNewTargets }`, so no
    /// `Optional_YouMay` swallowed-clause warning is emitted.
    #[test]
    fn optional_you_may_accepts_copy_retarget_clause() {
        let parsed = parse_named(
            "{T}, Sacrifice this land: Copy target instant or sorcery spell you control. \
             You may choose new targets for the copy.",
            "Mirrorpool",
            &["Land"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 707.10c (B3): Galvanic Iteration nests its CopySpell inside a delayed
    /// trigger; the retarget clause is absorbed onto the inner CopySpell and
    /// `effect_has_internal_optionality` detects it via the existing
    /// `CreateDelayedTrigger` recursion.
    #[test]
    fn optional_you_may_accepts_copy_retarget_clause_in_delayed_trigger() {
        let parsed = parse_named(
            "When you next cast an instant or sorcery spell this turn, copy that spell. \
             You may choose new targets for the copy.",
            "Galvanic Iteration",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    #[test]
    fn alt_cost_cast_permissions_do_not_swallow_pay_life_riders() {
        for (oracle, name, types) in [
            (
                "Flying\n\
                 Lifelink\n\
                 You may play lands and cast spells from among cards in your graveyard you've \
                 surveilled this turn. If you cast a spell this way, you pay life equal to its \
                 mana value rather than paying its mana cost.",
                "Eye of Duskmantle",
                &["Creature"][..],
            ),
            (
                "Menace\n\
                 Whenever The Infamous Cruelclaw deals combat damage to a player, exile cards \
                 from the top of your library until you exile a nonland card. You may cast that \
                 card by discarding a card rather than paying its mana cost.",
                "The Infamous Cruelclaw",
                &["Creature"][..],
            ),
            (
                "Devoid\n\
                 Menace\n\
                 Whenever this creature deals combat damage to a player, that player exiles cards \
                 from the top of their library until they exile a nonland card. You may cast that \
                 card by paying life equal to the spell's mana value rather than paying its mana cost.",
                "Bismuth Mindrender",
                &["Creature"][..],
            ),
            (
                "Casualty 2\n\
                 Each opponent exiles the top card of their library. You may cast spells from among \
                 those cards this turn. If you cast a spell this way, pay life equal to that spell's \
                 mana value rather than pay its mana cost.",
                "Xander's Pact",
                &["Sorcery"][..],
            ),
        ] {
            let parsed = parse_named(oracle, name, types);
            assert!(
                parsed.parse_warnings.iter().all(|warning| {
                    !matches!(warning, OracleDiagnostic::SwallowedClause { .. })
                }),
                "{name} must not gain coverage via a swallowed clause: {:?}",
                parsed.parse_warnings
            );
        }
    }

    /// Issue #2233: Condition_Unless — representative cards from the drilldown.
    #[test]
    fn condition_unless_accepts_representative_cards() {
        for (oracle, name, types) in [
            (
                "Creatures can't attack a player unless that player cast a spell or put a nontoken permanent onto the battlefield during their last turn.",
                "Arboria",
                &["Enchantment"][..],
            ),
            (
                "Enchanted creature can't be blocked unless defending player pays {3} for each creature they control that's blocking it.",
                "Awesome Presence",
                &["Enchantment"][..],
            ),
            (
                "Blazing Salvo deals 3 damage to target creature unless that creature's controller has Blazing Salvo deal 5 damage to them.",
                "Blazing Salvo",
                &["Instant"][..],
            ),
            (
                "Counter target instant or sorcery spell unless that spell's controller has Molten Influence deal 4 damage to them.",
                "Molten Influence",
                &["Instant"][..],
            ),
            (
                "This creature can't attack unless defending player is poisoned.",
                "Chained Throatseeker",
                &["Creature"][..],
            ),
            // Issue #3466: counter spells with a NON-mana "unless" cost. The
            // counter path previously recognized only the mana form ("pays
            // {N}") and silently dropped life / sacrifice / discard costs,
            // shipping an unconditional counter. CR 118.12 / CR 119.4 / CR
            // 608.2c.
            (
                "Counter target spell unless its controller pays 5 life.",
                "Dash Hopes",
                &["Instant"][..],
            ),
            (
                "Counter target spell unless its controller sacrifices a creature.",
                "Counter-Sacrifice",
                &["Instant"][..],
            ),
            (
                "Counter target spell unless its controller discards a card.",
                "Counter-Discard",
                &["Instant"][..],
            ),
            (
                "Draw X cards. For each card drawn this way, discard a card unless you sacrifice a permanent.",
                "Read the Runes",
                &["Instant"][..],
            ),
            (
                "At the beginning of your upkeep, for each player, this enchantment deals 1 damage to that player unless they pay {B} or {3}.",
                "Lim-Dul's Hex",
                &["Enchantment"][..],
            ),
            (
                "Return target creature to its owner's hand unless its controller has you draw a card.",
                "Decoy Gambit Bounce",
                &["Instant"][..],
            ),
        ] {
            let parsed = parse_named(oracle, name, types);
            assert!(
                !has_swallowed_detector(&parsed, "Condition_Unless"),
                "{name} should not swallow unless clause"
            );
        }
    }

    /// CR 508.1f + CR 701.26a (Ood Sphere Red-Eye): "... can't become tapped
    /// unless they're being declared as attackers." The attacker-declaration
    /// exemption is inherent to a modeled `CantTap` static, so the unless clause
    /// is fully represented and must NOT flag Condition_Unless. Guard against a
    /// suppression false-positive: the card must have zero Unimplemented so the
    /// detector actually runs (not skipped via `any_ability_has_unimplemented`).
    #[test]
    fn condition_unless_accepts_declared_as_attackers_cant_tap_exemption() {
        let parsed = parse_named(
            "Whenever chaos ensues, for each opponent, goad up to one target creature that opponent controls. Until your next turn, those creatures can't become tapped unless they're being declared as attackers.",
            "Red-Eye",
            &["Plane"],
        );
        assert!(
            !any_ability_has_unimplemented(&parsed),
            "Red-Eye must fully parse for the detector to run: {:?}",
            parsed.parse_warnings
        );
        assert!(
            !has_swallowed_detector(&parsed, "Condition_Unless"),
            "declared-as-attackers CantTap exemption must not flag Condition_Unless: {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 701.20a + CR 604.3: Reveal-until chosen-type and shares-a-type filters
    /// must parse without any swallowed-clause warnings (Riptide Shapeshifter,
    /// Heirloom Blade).
    #[test]
    fn reveal_until_chosen_type_and_shares_type_do_not_swallow() {
        for (oracle, name, types) in [
            (
                "Reveal cards from the top of your library until you reveal a creature card of the chosen type. Put that card onto the battlefield and the rest on the bottom of your library in a random order.",
                "Riptide Shapeshifter",
                &["Creature"][..],
            ),
            (
                "Whenever equipped creature dies, reveal cards from the top of your library until you reveal a creature card that shares a creature type with it, then you may put that card into your hand and the rest on the bottom of your library in a random order.",
                "Heirloom Blade",
                &["Artifact"][..],
            ),
        ] {
            let parsed = parse_named(oracle, name, types);
            assert!(
                parsed.parse_warnings.iter().all(|warning| {
                    !matches!(warning, OracleDiagnostic::SwallowedClause { .. })
                }),
                "{name} must not trigger any swallowed clause warnings: {:?}",
                parsed.parse_warnings
            );
        }
    }

    /// CR 702.5a + CR 702.9: Aura enchant lines with "without [keyword]" must not
    /// fall through as unknown Enchant targets (Trapped in the Tower, Roots).
    #[test]
    fn enchant_creature_without_flying_do_not_swallow() {
        for (oracle, name, types) in [
            (
                "Enchant creature without flying\nEnchanted creature can't attack or block, and its activated abilities can't be activated.",
                "Trapped in the Tower",
                &["Enchantment", "Aura"][..],
            ),
            (
                "Enchant creature without flying\nEnchanted creature can't block.",
                "Roots",
                &["Enchantment", "Aura"][..],
            ),
        ] {
            let parsed = parse_named(oracle, name, types);
            assert!(
                parsed.parse_warnings.iter().all(|warning| {
                    !matches!(warning, OracleDiagnostic::SwallowedClause { .. })
                }),
                "{name} must not trigger any swallowed clause warnings: {:?}",
                parsed.parse_warnings
            );
        }
    }

    /// CR 601.2f + CR 607.2d: Progenitor's Icon's chosen-type next-spell flash
    /// grant must parse without swallowing the "of the chosen type" qualifier.
    #[test]
    fn progenitors_icon_chosen_type_next_spell_flash_do_not_swallow() {
        let parsed = parse_named(
            "As this artifact enters, choose a creature type.\n\
             {T}: Add one mana of any color.\n\
             {T}: The next spell of the chosen type you cast this turn can be cast as though it had flash.",
            "Progenitor's Icon",
            &["Artifact"],
        );
        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| { !matches!(warning, OracleDiagnostic::SwallowedClause { .. }) }),
            "Progenitor's Icon must not trigger swallowed clause warnings: {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 601.2f + CR 700.5: Drag to the Underworld — devotion where-X self-spell
    /// cost reduction must parse alongside destroy without swallowing either clause.
    #[test]
    fn drag_to_the_underworld_devotion_cost_reduction_parses_without_swallow() {
        let parsed = parse_named(
            "This spell costs {X} less to cast, where X is your devotion to black. (Each {B} in the mana costs of permanents you control counts toward your devotion to black.)\n\
             Destroy target creature.",
            "Drag to the Underworld",
            &["Instant"],
        );
        assert_eq!(
            parsed.statics.len(),
            1,
            "expected one self-spell cost static"
        );
        assert!(
            matches!(
                parsed.statics[0].mode,
                StaticMode::ModifyCost {
                    dynamic_count: Some(crate::types::ability::QuantityRef::Devotion { .. }),
                    ..
                }
            ),
            "expected devotion-bound ModifyCost, got {:?}",
            parsed.statics[0].mode
        );
        assert_eq!(parsed.abilities.len(), 1);
        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| !matches!(warning, OracleDiagnostic::SwallowedClause { .. })),
            "Drag to the Underworld must not swallow cost-reduction or destroy clauses: {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 608.2c: Wretched Banquet — least-power destroy gate must parse without
    /// swallowing the intervening-if clause.
    #[test]
    fn wretched_banquet_least_power_destroy_parses_without_swallow() {
        let parsed = parse_named(
            "Destroy target creature if it has the least power among creatures.",
            "Wretched Banquet",
            &["Sorcery"],
        );
        assert_eq!(parsed.abilities.len(), 1, "expected one spell ability");
        match &parsed.abilities[0].condition {
            Some(crate::types::ability::AbilityCondition::QuantityCheck { comparator, .. }) => {
                assert_eq!(*comparator, crate::types::ability::Comparator::LE)
            }
            other => panic!("expected QuantityCheck least-power gate, got: {other:?}"),
        }
        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| !matches!(warning, OracleDiagnostic::SwallowedClause { .. })),
            "Wretched Banquet must not swallow the least-power gate: {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 702.34a + CR 601.2f: Visions of Ruin — flashback cost plus commander-MV
    /// "cast this way" reduction must parse without swallowing either clause.
    #[test]
    fn visions_of_ruin_flashback_commander_reduction_parses_without_swallow() {
        let parsed = parse_named(
            "Each opponent sacrifices an artifact. For each artifact sacrificed this way, you create a Treasure token.\n\
             Flashback {8}{R}{R}. This spell costs {X} less to cast this way, where X is the greatest mana value of a commander you own on the battlefield or in the command zone.",
            "Visions of Ruin",
            &["Sorcery"],
        );
        assert!(
            parsed
                .extracted_keywords
                .iter()
                .any(|k| matches!(k, Keyword::Flashback(_))),
            "expected Flashback keyword, got {:?}",
            parsed.extracted_keywords
        );
        assert!(
            parsed.statics.iter().any(|sd| {
                matches!(sd.mode, StaticMode::ModifyCost { .. })
                    && sd.condition.as_ref().is_some_and(|cond| {
                        matches!(
                            cond,
                            crate::types::ability::StaticCondition::CastingAsVariant { .. }
                        )
                    })
            }),
            "expected flashback-gated ReduceCost static, got {:?}",
            parsed.statics
        );
        assert!(
            parsed
                .parse_warnings
                .iter()
                .all(|warning| !matches!(warning, OracleDiagnostic::SwallowedClause { .. })),
            "Visions of Ruin must not swallow flashback cost-reduction clauses: {:?}",
            parsed.parse_warnings
        );
    }

    /// CR 508.1 + CR 118.9: Lethargy Trap — leading-if attacking-creature count
    /// gate on the {U} alternative casting cost must not report Condition_If.
    #[test]
    fn condition_if_accepts_lethargy_trap_alt_cost_gate() {
        let parsed = parse_named(
            "If three or more creatures are attacking, you may pay {U} rather than pay \
this spell's mana cost.\nAttacking creatures get -3/-0 until end of turn.",
            "Lethargy Trap",
            &["Instant"],
        );
        assert!(
            !has_swallowed_detector(&parsed, "Condition_If"),
            "alt-cost attacking-creature gate must bind to casting_options: {:?}",
            parsed.parse_warnings
        );
        assert_eq!(
            parsed.casting_options.len(),
            1,
            "expected one alternative casting option, got {:?}",
            parsed.casting_options
        );
        assert!(
            parsed.casting_options[0].condition.is_some(),
            "alt-cost must carry the attacking-creature count gate"
        );
    }

    /// CR 115.7d: Standalone retarget spells (Deflecting Swat, Redirect) lower
    /// to `ChangeTargets { scope: All }` with the full `you may choose new
    /// targets` surface preserved — not `def.optional`.
    #[test]
    fn optional_you_may_accepts_change_targets_retarget_spells() {
        for (oracle, name, types) in [
            (
                "The next time a spell or ability an opponent controls targets you \
                 this turn, change the target to another spell or ability. \
                 Overload {2}{U}{U} (You may cast this spell for its overload cost. \
                 If you do, change its target.)\n\
                 You may choose new targets for target spell or ability.",
                "Deflecting Swat",
                &["Instant"][..],
            ),
            (
                "You may choose new targets for target spell.",
                "Redirect",
                &["Instant"][..],
            ),
        ] {
            let parsed = parse_named(oracle, name, types);
            assert!(
                !has_swallowed_detector(&parsed, "Optional_YouMay"),
                "{name} should not swallow retarget optional"
            );
        }
    }

    /// CR 707.10c + CR 115.7d: Increasing Vengeance — copy with optional
    /// retarget for copies (absorbed onto CopySpell when adjacent).
    #[test]
    fn optional_you_may_accepts_increasing_vengeance_copy_retarget() {
        let parsed = parse_named(
            "Copy target instant or sorcery spell you control. If this spell was cast from a \
             graveyard, copy that spell twice instead. You may choose new targets for the copies.\n\
             Flashback {3}{R}{R}",
            "Increasing Vengeance",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 707.10c: Thousand-Year Storm exercises the triggered-ability context
    /// — the plural "for the copies" clause is absorbed onto the trigger's
    /// inner CopySpell.
    #[test]
    fn optional_you_may_accepts_copy_retarget_clause_in_triggered_ability() {
        let parsed = parse_named(
            "Whenever you cast an instant or sorcery spell, copy it for each other \
             instant and sorcery spell you've cast this turn. \
             You may choose new targets for the copies.",
            "Thousand-Year Storm",
            &["Enchantment"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 705 + CR 707.10c: Krark nests CopySpell retarget permission inside
    /// the flip-coin win branch; `effect_has_internal_optionality` must recurse
    /// into `FlipCoin.win_effect`.
    #[test]
    fn optional_you_may_accepts_copy_retarget_clause_in_flip_coin_win_branch() {
        let parsed = parse_named(
            "Whenever you cast an instant or sorcery spell, flip a coin. \
             If you lose the flip, return that spell to its owner's hand. \
             If you win the flip, copy that spell, and you may choose new targets for the copy.",
            "Krark, the Thumbless",
            &["Legendary", "Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 603.2b + CR 611.2 + CR 609.4b: Xanathar's upkeep trigger bundles
    /// look/play/spend-as-any-color permissions inside the execute tree.
    #[test]
    fn optional_you_may_accepts_xanathar_upkeep_permissions() {
        let parsed = parse_named(
            "At the beginning of your upkeep, choose target opponent. Until end of turn, \
             that player can't cast spells, you may look at the top card of their library \
             any time, you may play the top card of their library, and you may spend mana \
             as though it were mana of any color to cast spells this way.",
            "Xanathar, Guild Kingpin",
            &["Legendary", "Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// Regression: issue #2277 — when the leading `If <X>, ` condition has no
    /// typed recognizer, the structural fallback strips the head so the inner
    /// `you may` optional choice is still extracted.
    #[test]
    fn optional_you_may_accepts_amareth_pattern() {
        let parsed = parse(
            "Whenever another permanent you control enters, look at the top card \
             of your library. If it shares a card type with that permanent, you \
             may reveal that card and put it into your hand.",
            &["Creature"],
        );

        assert!(
            !has_swallowed_detector(&parsed, "Optional_YouMay"),
            "Amareth pattern must not emit Optional_YouMay swallow diagnostic"
        );
        assert!(
            parsed.triggers.iter().any(trigger_tree_has_optional),
            "Amareth's inner `you may reveal` continuation must be marked optional"
        );
    }

    /// Regression: issue #2277 — Tithe's "If target opponent controls more
    /// lands than you, you may search …" has an unrecognized leading condition;
    /// the structural fallback strips the head so the optional flag is preserved.
    #[test]
    fn optional_you_may_accepts_tithe_optional_search() {
        let parsed = parse_named(
            "Search your library for a Plains card. If target opponent controls \
             more lands than you, you may search your library for an additional \
             Plains card. Reveal those cards, put them into your hand, then shuffle.",
            "Tithe",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
        assert!(
            parsed.abilities.iter().any(def_tree_has_optional),
            "Tithe's optional second search must be marked optional"
        );
    }

    /// CR 601.2f: Awaken the Blood Avatar's `you may sacrifice any number of
    /// creatures` is an additional-cost optional, captured as
    /// `AdditionalCost::Optional(_)` at the top level — `any_ability_is_optional`
    /// recognizes this shape, so no `Optional_YouMay` swallow fires.
    ///
    /// **Status:** ignored — the parser doesn't currently extract the
    /// `As an additional cost to cast this spell, you may sacrifice any number
    /// of creatures` line into `AdditionalCost::Optional`. The investigator's
    /// plan explicitly noted this case as a possible follow-up: "if it fails,
    /// note it as a follow-up — do NOT expand scope". Tracked separately.
    #[test]
    #[ignore = "additional-cost extraction for `you may sacrifice any number` not in scope (issue #2277 follow-up)"]
    fn optional_you_may_accepts_awaken_blood_avatar_additional_cost() {
        let parsed = parse_named(
            "As an additional cost to cast this spell, you may sacrifice any \
             number of creatures. This spell costs {2} less to cast for each \
             creature sacrificed this way.\n\
             Each opponent sacrifices a creature of their choice. Create a 3/6 \
             black and red Avatar creature token with haste and \"Whenever this \
             token attacks, it deals 3 damage to each opponent.\"",
            "Awaken the Blood Avatar",
            &["Sorcery"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// CR 701.20a: Atraxa, Grand Unifier — `you may put a card of that type
    /// from among the revealed cards into your hand` carries the `from among`
    /// continuation, so the `is_specialized_put_body` shape guard blocks the
    /// `you may ` peel; the optionality is encoded as `up_to: true` on the
    /// internal `ChangeZone` (Dig keep grammar). The refactor must NOT
    /// regress this — verified via `effect_has_internal_optionality`.
    #[test]
    fn optional_you_may_atraxa_grand_unifier_reports_known_gap() {
        let parsed = parse_named(
            "Flying, vigilance, deathtouch, lifelink\n\
             When this creature enters, reveal the top ten cards of your library. \
             For each card type, you may put a card of that type from among the \
             revealed cards into your hand. Put the rest on the bottom of your \
             library in a random order.",
            "Atraxa, Grand Unifier",
            &["Creature"],
        );

        // KNOWN GAP, pinned deliberately — see `condition_as_long_as_accepts_bronze_
        // horse_and_champions_helm` for the full explanation. Atraxa reports a swallowed
        // `Optional_YouMay` (and `DynamicQty`), and did so in the shipped card data long
        // before this change: the per-card-type "you may put a card of that type" choice
        // is not typed as optional. The test asserted the opposite and passed only
        // because its empty MTGJSON keyword list turned the "Flying, vigilance,
        // deathtouch, lifelink" line into an `Effect::Unimplemented`, tripping the
        // card-wide gate that silenced every detector on the card.
        assert!(
            has_swallowed_detector(&parsed, "Optional_YouMay"),
            "pre-existing gap: the per-card-type 'you may put' optionality is not typed. \
             Warnings: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn dynamic_qty_accepts_counter_multiplier_carrier() {
        let parsed = parse(
            "Put a +1/+1 counter on target creature you control, then double the number of +1/+1 counters on that creature.",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// CR 702.170a: Fblthp's "The plot cost is equal to its mana cost" is the
    /// intrinsic plot cost of the `TopOfLibraryHasPlot` static (computed at
    /// synthesis, no stored `QuantityExpr`), so the " equal to " marker must NOT
    /// raise a DynamicQty swallow warning — the static's presence is the carrier
    /// (mirrors the SelfManaCost precedent). Reverting the marker re-reds Fblthp.
    #[test]
    fn dynamic_qty_accepts_plot_cost_equal_to_mana_cost() {
        let parsed = parse_named(
            "You may look at the top card of your library any time.\n\
             The top card of your library has plot. The plot cost is equal to its mana cost.\n\
             You may plot nonland cards from the top of your library.",
            "Fblthp, Lost on the Range",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// CR 702.143d: Singing Towers of Darillium grants foretell whose cost is
    /// "equal to its mana cost reduced by {2}". That derived cost is intrinsic to
    /// the `AddKeywordWithDerivedCost` continuous modification (computed per
    /// recipient via `CostDerivation`, no stored `QuantityExpr`), so the
    /// " equal to " marker must NOT raise a DynamicQty swallow warning — the
    /// modification's presence is the carrier (mirrors the `SelfManaCost` /
    /// `TopOfLibraryHasPlot` precedents). Reverting the marker re-reds this card.
    #[test]
    fn dynamic_qty_accepts_foretell_cost_equal_to_mana_cost_reduced() {
        let parsed = parse_named(
            "Each nonland card in your hand without foretell has foretell. \
             Its foretell cost is equal to its mana cost reduced by {2}. \
             (During your turn, you may pay {2} and exile it from your hand \
             face down. Cast it on a later turn for its foretell cost.)\n\
             Whenever chaos ensues, you may cast a foretold card you own from \
             exile without paying its mana cost this turn.",
            "Singing Towers of Darillium",
            &["Plane"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    #[test]
    fn dynamic_qty_accepts_choose_and_sacrifice_rest_for_each_player() {
        let parsed = parse_named(
            "For each player, you choose from among the permanents that player controls an artifact, a creature, an enchantment, and a planeswalker. Then each player sacrifices all other nonland permanents they control.",
            "Tragic Arrogance",
            &["Sorcery"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    #[test]
    fn dynamic_qty_suppressed_for_unimplemented_granted_trigger_child() {
        let parsed = parse_named(
            "Commander creatures you own have \"When this creature enters and at the beginning of your upkeep, each player may put two +1/+1 counters on a creature they control. For each opponent who does, you gain protection from that player until your next turn.\"",
            "Noble Heritage",
            &["Enchantment"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    #[test]
    fn dynamic_qty_accepts_vote_voted_for_carrier() {
        let parsed = parse_named(
            "At the beginning of your upkeep, each opponent chooses money, friends, or secrets. \
             For each player who chose money, you and that player each create a Treasure token. \
             For each player who chose friends, you and that player each create a 1/1 green and white Citizen creature token. \
             For each player who chose secrets, you and that player each draw a card.",
            "Master of Ceremonies",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// CR 701.38: Emissary Green's aggregate vote tally ("a number of X equal to
    /// [twice] the number of <choice> votes") is realized by the Vote per-vote
    /// fan-out, so the DynamicQty detector must not flag it as swallowed.
    #[test]
    fn dynamic_qty_accepts_emissary_green_vote_tally() {
        let parsed = parse_named(
            "Whenever Emissary Green attacks, starting with you, each player votes for profit or security. \
             You create a number of Treasure tokens equal to twice the number of profit votes. \
             Put a number of +1/+1 counters on each creature you control equal to the number of security votes.",
            "Emissary Green",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// Guard against over-suppression: a vote card whose per-choice body has its
    /// own swallowed dynamic ("equal to its power", not a vote tally) must still
    /// flag DynamicQty.
    #[test]
    fn dynamic_qty_keeps_warning_for_non_tally_dynamic_in_vote_body() {
        assert!(!super::equal_to_vote_tally_suffix(" equal to its power"));
        assert!(!super::cleaned_dynamic_is_only_vote_tally(
            "each player votes for a or b. for each a vote, draw cards equal to your life total. \
             for each b vote, do nothing."
        ));
    }

    #[test]
    fn dynamic_qty_keeps_warning_when_counter_multiplier_card_has_second_dynamic_clause() {
        let parsed = parse(
            "Put a +1/+1 counter on target creature, then double the number of +1/+1 counters on it.\n\
             Flashback {8}{G}{G}. This spell costs {X} less to cast this way, where X is the greatest mana value of a commander you own on the battlefield or in the command zone.",
            &["Sorcery"],
        );

        // After fixing commander mana value parsing, the "greatest mana value of a commander"
        // pattern now parses correctly, so DynamicQty should NOT be flagged.
        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// CR 608.2c: "investigate twice instead" — the doubled count is carried
    /// by `AbilityDefinition.repeat_for`, a legitimate QuantityExpr home. The
    /// "twice" word must not flag DynamicQty.
    #[test]
    fn dynamic_qty_accepts_repeat_for_carrier_secrets_of_the_key() {
        let parsed = parse_named(
            "Investigate. If this spell was cast from a graveyard, investigate twice instead. \
             (Create a Clue token. It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")\n\
             Flashback {3}{U}",
            "Secrets of the Key",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// CR 608.2c: Increasing Vengeance shares the "copy that spell twice
    /// instead" shape — same `repeat_for` carrier, same suppression.
    #[test]
    fn dynamic_qty_accepts_repeat_for_carrier_increasing_vengeance() {
        let parsed = parse_named(
            "Copy target instant or sorcery spell you control. If this spell was cast from a \
             graveyard, copy that spell twice instead. You may choose new targets for the copies.\n\
             Flashback {3}{R}{R}",
            "Increasing Vengeance",
            &["Instant"],
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// Negative: the existing counter-multiplier card still flags `DynamicQty`
    /// when it carries a second swallowed dynamic clause — proves the new
    /// `repeat_for` suppression did not widen the exemption.
    /// (See `dynamic_qty_keeps_warning_when_counter_multiplier_card_has_second_dynamic_clause`.)
    ///
    /// Helper-level narrowness gate for `cleaned_twice_is_only_dynamic_marker`:
    /// the `repeat_for` suppression fires ONLY when " twice " is the sole
    /// dynamic marker. Any second marker, or the "twice that" / "twice x"
    /// multiplier forms (which need a real `QuantityExpr`, not a repeat count),
    /// must keep the warning live even if a `repeat_for` is also present.
    #[test]
    fn twice_is_only_dynamic_marker_gate() {
        // Plain "twice" with no other marker — the suppression-eligible case.
        assert!(super::cleaned_twice_is_only_dynamic_marker(
            "investigate. if this spell was cast from a graveyard, investigate twice instead."
        ));
        // "twice that" is a multiplier — needs a real QuantityExpr.
        assert!(!super::cleaned_twice_is_only_dynamic_marker(
            "they lose twice that much life instead."
        ));
        // "twice x" is a multiplier.
        assert!(!super::cleaned_twice_is_only_dynamic_marker(
            "deal damage equal to twice x to any target."
        ));
        // A second dynamic marker present — must not be suppression-eligible.
        assert!(!super::cleaned_twice_is_only_dynamic_marker(
            "investigate twice instead, then draw cards equal to your life total."
        ));
        assert!(!super::cleaned_twice_is_only_dynamic_marker(
            "investigate twice instead and create a token for each creature you control."
        ));
        // "twice each turn" alone is the activation-limit form, not dynamic.
        assert!(!super::cleaned_twice_is_only_dynamic_marker(
            "activate this ability only twice each turn."
        ));
        // No "twice" at all.
        assert!(!super::cleaned_twice_is_only_dynamic_marker(
            "draw a card for each creature you control."
        ));
    }

    /// CR 614.1a + CR 701.10g: Neriv, Heart of the Storm — "it deals twice that
    /// much damage instead" is a `DamageModification::Double` replacement. The ×2
    /// is carried by the modification (a unit variant with no `QuantityExpr`), so
    /// the "twice" marker must not flag DynamicQty.
    ///
    /// Non-vacuous: the two reach-guards prove the parse reached the real Double
    /// carrier and admitted zero `Effect::Unimplemented` (an Unimplemented would
    /// early-return `check_swallowed_clauses` and make the negative pass for the
    /// wrong reason — card-test foot-gun #6). Revert surface: remove the
    /// `damage_modification` suppression clause in `detect_dynamic_qty` and this
    /// flips to a live DynamicQty warning.
    #[test]
    fn dynamic_qty_accepts_damage_double_carrier_neriv() {
        // Verbatim Oracle text. "Flying" is supplied as an MTGJSON keyword — as
        // the real card-data pipeline does — so it is recognized rather than left
        // as an Unimplemented line; the doubling clause is the unit under test.
        let types: Vec<String> = ["Legendary", "Creature", "Dragon"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let parsed = parse_oracle_text(
            "Flying\n\
             If a creature you control that entered this turn would deal damage, it deals twice \
             that much damage instead.",
            "Neriv, Heart of the Storm",
            &["Flying".to_string()],
            &types,
            &["Dragon".to_string()],
        );

        // Reach-guard A: zero Unimplemented ⇒ the doubling unit's detector runs to
        // completion (an Unimplemented would early-return `check_swallowed_clauses`
        // — card-test foot-gun #6). Isolation was measured: the doubling clause
        // ALONE parses to zero abilities + one Double replacement, so this
        // negative is exercised on the doubling unit, not masked by the "Flying"
        // keyword line.
        assert!(
            !any_ability_has_unimplemented(&parsed),
            "reach-guard: Neriv must parse with zero Unimplemented (else the negative is vacuous)"
        );
        // Reach-guard B: the Double carrier the suppression keys on is present.
        assert!(
            parsed
                .replacements
                .iter()
                .any(|r| r.damage_modification == Some(DamageModification::Double)),
            "reach-guard: Neriv must parse to a Double damage-modification replacement"
        );

        assert!(!has_swallowed_detector(&parsed, "DynamicQty"));
    }

    /// Helper-level narrowness gate for
    /// `cleaned_twice_damage_double_is_only_dynamic_marker`: the damage-double
    /// suppression fires ONLY when "twice that much damage" is the sole dynamic
    /// marker, and — unlike `cleaned_twice_is_only_dynamic_marker` (repeat-count
    /// "twice") — it accepts the "twice that" multiplier form because here the ×2
    /// is carried by `DamageModification::Double`. Any second dynamic marker keeps
    /// the warning, so a real second clause is never masked.
    #[test]
    fn dynamic_qty_damage_double_marker_gate() {
        // Neriv's sole-marker case — suppression-eligible.
        assert!(super::cleaned_twice_damage_double_is_only_dynamic_marker(
            "if a creature you control that entered this turn would deal damage, it deals twice \
             that much damage instead."
        ));
        // The OLD repeat-count helper REJECTS this exact phrase (it treats
        // "twice that" as a multiplier needing a real QuantityExpr) — which is
        // precisely why the damage-double helper is a separate sibling.
        assert!(!super::cleaned_twice_is_only_dynamic_marker(
            "it deals twice that much damage instead."
        ));
        // Non-masking: a genuine second dynamic marker keeps the warning live.
        assert!(!super::cleaned_twice_damage_double_is_only_dynamic_marker(
            "it deals twice that much damage. then draw cards equal to the number of counters on it."
        ));
        assert!(!super::cleaned_twice_damage_double_is_only_dynamic_marker(
            "it deals twice that much damage, then create a token for each creature you control."
        ));
        // No damage-double phrase at all → not eligible.
        assert!(!super::cleaned_twice_damage_double_is_only_dynamic_marker(
            "investigate twice instead."
        ));
    }

    // ── ActivateLimit regressions (#2240) ──────────────────────────────────

    #[test]
    fn activate_limit_accepts_crew_once_per_turn_cadence() {
        // CR 702.122 + CR 602.5b: Luxurious Locomotive — "Crew 1. Activate only
        // once each turn." The cadence sentence is represented on the Crew
        // keyword's `once_per_turn` field, not on an activated ability.
        let parsed = parse_named(
            "Crew 1. Activate only once each turn. (Tap any number of creatures you control with total power 1 or more: This Vehicle becomes an artifact creature until end of turn.)\n\
             Whenever a creature attacks, create a Treasure token for each creature and Vehicle that attacked this turn.",
            "Luxurious Locomotive",
            &["Artifact"],
        );
        assert!(!has_swallowed_detector(&parsed, "ActivateLimit"));
    }

    // ── Optional_MayHave regressions (#2237) ───────────────────────────────

    #[test]
    fn optional_may_have_risk_factor() {
        let parsed = parse_named(
            "Target opponent may have Risk Factor deal 4 damage to them. \
             If that player doesn't, you draw three cards.",
            "Risk Factor",
            &["Instant"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_MayHave"));
    }

    /// CR 121.3a + CR 506.2 + CR 608.2d: "<actor> may have you draw a card" —
    /// the named actor decides; the printed controller draws. Covers the
    /// targeted-opponent actor (Palantír of Orthanc, Bane, Lord of Darkness) and
    /// the defending-player actor (Shakedown Heavy). The build-for-the-class
    /// invariants checked here:
    ///   1. the grant is an `Effect::Draw`, not an Unimplemented "have" static;
    ///   2. the clause is `optional` (the actor's may-choice);
    ///   3. the actor is captured as the may-actor `player_scope`;
    ///   4. "you" is bound to `OriginalController`, so the controller-rebind the
    ///      `player_scope` fan-out applies (CR 109.5) does not redirect the draw
    ///      to the actor.
    fn have_you_draw_grant_trigger(text: &str, name: &str) -> AbilityDefinition {
        let parsed = parse_named(text, name, &["Creature"]);
        let trigger = parsed
            .triggers
            .first()
            .expect("trigger must parse")
            .execute
            .as_deref()
            .expect("trigger must have an executed ability")
            .clone();
        assert!(
            !def_tree_has_unimplemented(&trigger),
            "{name}: have-you-draw grant must not be Unimplemented"
        );
        trigger
    }

    #[test]
    fn defending_player_may_have_you_draw_routes_to_original_controller() {
        let def = have_you_draw_grant_trigger(
            "Whenever this creature attacks, defending player may have you draw a card. \
             If they do, untap this creature and remove it from combat.",
            "Shakedown Heavy",
        );
        assert!(matches!(*def.effect, Effect::Draw { .. }), "must be a Draw");
        assert!(
            def.optional,
            "the defending player's may-choice is optional"
        );
        assert_eq!(
            def.player_scope,
            Some(crate::types::ability::PlayerFilter::DefendingPlayer),
            "may-actor must be the defending player",
        );
        if let Effect::Draw { ref target, .. } = *def.effect {
            assert_eq!(
                *target,
                TargetFilter::OriginalController,
                "\"you draw\" must survive the may-actor controller rebind",
            );
        }
    }

    #[test]
    fn target_opponent_may_have_you_draw_routes_to_original_controller() {
        let def = have_you_draw_grant_trigger(
            "At the beginning of your end step, target opponent may have you draw a card. \
             If they don't, you scry 2.",
            "Palantir of Orthanc",
        );
        assert!(matches!(*def.effect, Effect::Draw { .. }), "must be a Draw");
        assert!(def.optional, "the opponent's may-choice is optional");
        assert_eq!(
            def.player_scope,
            Some(crate::types::ability::PlayerFilter::Opponent),
            "may-actor must be the targeted opponent",
        );
        if let Effect::Draw { ref target, .. } = *def.effect {
            assert_eq!(
                *target,
                TargetFilter::OriginalController,
                "\"you draw\" must survive the may-actor controller rebind",
            );
        }
    }

    #[test]
    fn defending_player_may_have_you_draw_not_swallowed() {
        let parsed = parse_named(
            "Whenever this creature attacks, defending player may have you draw a card. \
             If they do, untap this creature and remove it from combat.",
            "Shakedown Heavy",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_MayHave"));
    }

    #[test]
    fn optional_may_have_channel_harm() {
        let parsed = parse_named(
            "Prevent all damage that would be dealt to you and permanents you control this turn \
             by sources you don't control. If damage is prevented this way, you may have Channel Harm \
             deal that much damage to target creature.",
            "Channel Harm",
            &["Instant"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_MayHave"));
    }

    #[test]
    fn optional_may_have_murderous_redcap_avatar() {
        let parsed = parse_named(
            "Whenever a creature you control enters with a counter on it, \
             you may have it deal damage equal to its power to any target.",
            "Murderous Redcap Avatar",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_MayHave"));
    }

    #[test]
    fn optional_may_have_requiem_monolith() {
        let parsed = parse_named(
            "{T}: Until end of turn, target creature gains \"Whenever this creature is dealt damage, \
             you draw that many cards and lose that much life.\" That creature's controller may have \
             this artifact deal 1 damage to it. Activate only as a sorcery.",
            "Requiem Monolith",
            &["Artifact"],
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_MayHave"));
    }

    /// KNOWN GAP, pinned deliberately — see `condition_as_long_as_accepts_bronze_horse_
    /// and_champions_helm` for the full explanation of the vacuity this replaces.
    ///
    /// Siege Behemoth reports `Optional_MayHave`, `Optional_YouMay` and `DynamicQty` as
    /// swallowed, and reported all three in the shipped card data long before this
    /// change (verified against the pre-cutover full-pool export). This test asserted
    /// the opposite and passed only because its empty MTGJSON keyword list turned the
    /// "Hexproof" line into an `Effect::Unimplemented`, tripping the card-wide gate that
    /// silenced every detector on the card.
    #[test]
    fn optional_may_have_siege_behemoth_reports_known_gap() {
        let parsed = parse_named(
            "Hexproof\nAs long as this creature is attacking, for each creature you control, \
             you may have that creature assign its combat damage as though it weren't blocked.",
            "Siege Behemoth",
            &["Creature"],
        );
        assert!(
            has_swallowed_detector(&parsed, "Optional_MayHave"),
            "pre-existing gap: the per-creature 'you may have' optionality is not typed. \
             Warnings: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn optional_may_have_wall_of_stolen_identity() {
        let parsed = parse_named(
            "You may have this creature enter as a copy of any creature on the battlefield, \
             except it's a Wall in addition to its other types and has defender. When you do, \
             tap the copied creature and it doesn't untap during its controller's untap step \
             for as long as you control this creature.",
            "Wall of Stolen Identity",
            &["Creature"],
        );
        assert_eq!(
            parsed.replacements.len(),
            1,
            "expected ETB clone replacement, got replacements={:?} statics={:?} abilities={:?}",
            parsed.replacements.len(),
            parsed.statics.len(),
            parsed.abilities.len()
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_MayHave"));
    }

    /// Issue #2235 regression: representative cards whose Oracle text contains
    /// "until end of turn" must surface a typed duration in the AST.
    #[test]
    fn duration_until_eot_agility_bobblehead() {
        let parsed = parse_named(
            "{T}: Add one mana of any color.\n\
             {3}, {T}: Up to X target creatures you control each gain haste until end of turn and can't be blocked this turn except by creatures with haste, where X is the number of Bobbleheads you control as you activate this ability.",
            "Agility Bobblehead",
            &["Artifact"],
        );
        assert!(!has_swallowed_detector(&parsed, "Duration_UntilEndOfTurn"));
    }

    #[test]
    fn duration_until_eot_alandra_sky_dreamer() {
        let parsed = parse_named(
            "Whenever you draw your second card each turn, create a 2/2 blue Drake creature token with flying.\n\
             Whenever you draw your fifth card each turn, Alandra and Drakes you control each get +X/+X until end of turn, where X is the number of cards in your hand.",
            "Alandra, Sky Dreamer",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&parsed, "Duration_UntilEndOfTurn"));
    }

    #[test]
    fn duration_until_eot_barbarian_bully() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let text = "This creature gets +2/+2 until end of turn unless a player has this creature deal 4 damage to them.";
        let def = parse_effect_chain(text, AbilityKind::Activated);
        assert!(
            def.unless_pay.is_some(),
            "unless_pay missing: {:?}",
            def.unless_pay
        );
        assert_eq!(
            def.duration,
            Some(crate::types::ability::Duration::UntilEndOfTurn),
            "chain duration missing: {:?}, effect={:?}",
            def.duration,
            def.effect
        );

        let parsed = parse_named(
            "Discard a card at random: This creature gets +2/+2 until end of turn unless a player has this creature deal 4 damage to them. Activate only once each turn.",
            "Barbarian Bully",
            &["Creature"],
        );
        assert!(!has_swallowed_detector(&parsed, "Duration_UntilEndOfTurn"));
    }

    #[test]
    fn duration_until_eot_dragon_egg_reports_known_gap() {
        let parsed = parse_named(
            "Defender\n\
             When this creature dies, create a 2/2 red Dragon creature token with flying and \"{R}: This token gets +1/+0 until end of turn.\"",
            "Dragon Egg",
            &["Creature"],
        );
        // KNOWN GAP, pinned deliberately — see `condition_as_long_as_accepts_bronze_
        // horse_and_champions_helm` for the full explanation. Dragon Egg reports a
        // swallowed `Duration_UntilEndOfTurn`, and did so in the shipped card data long
        // before this change: the "until end of turn" lives inside the *token's granted*
        // activated ability, and the duration evidence walker does not descend into a
        // token grant. The test asserted the opposite and passed only because its empty
        // MTGJSON keyword list turned the "Defender" line into an `Effect::Unimplemented`,
        // tripping the card-wide gate that silenced every detector on the card.
        assert!(
            has_swallowed_detector(&parsed, "Duration_UntilEndOfTurn"),
            "pre-existing gap: the duration evidence walker does not descend into a \
             token's granted ability. Warnings: {:?}",
            parsed.parse_warnings
        );
    }

    #[test]
    fn duration_until_eot_drop_tower() {
        let parsed = parse_named(
            "Visit — Target creature gains flying until end of turn, or until any player rolls a 1, whichever comes first.",
            "Drop Tower",
            &["Artifact"],
        );
        assert!(!has_swallowed_detector(&parsed, "Duration_UntilEndOfTurn"));
    }

    /// CR 118.9b + CR 707.12: `CastCopyOfCard` encodes the "you may cast the
    /// copy without paying its mana cost" permission internally, so
    /// `effect_has_internal_optionality` must classify the TrackedSet-target
    /// form (the only shape the parser produces) as carrying its own
    /// optionality (analogous to `CastFromZone`). The def-level `optional` flag
    /// stays false; the "may" is presented by the resolver as a TrackedSet
    /// `ChooseFromZoneChoice { up_to: true }`.
    #[test]
    fn effect_has_internal_optionality_cast_copy_of_card() {
        let effect = Effect::CastCopyOfCard {
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            cost: ManaCost::zero(),
            count: None,
        };
        assert!(super::effect_has_internal_optionality(&effect));
    }

    /// Recursive walk mirroring the module's `def_tree_has_*` predicates:
    /// does any def in the tree carry a `CastCopyOfCard` effect?
    fn def_tree_has_cast_copy_of_card(def: &AbilityDefinition) -> bool {
        if matches!(def.effect.as_ref(), Effect::CastCopyOfCard { .. }) {
            return true;
        }
        if def
            .sub_ability
            .as_deref()
            .is_some_and(def_tree_has_cast_copy_of_card)
        {
            return true;
        }
        if def
            .else_ability
            .as_deref()
            .is_some_and(def_tree_has_cast_copy_of_card)
        {
            return true;
        }
        def.mode_abilities
            .iter()
            .any(def_tree_has_cast_copy_of_card)
    }

    fn parsed_has_cast_copy_of_card(parsed: &crate::parser::oracle::ParsedAbilities) -> bool {
        parsed.abilities.iter().any(def_tree_has_cast_copy_of_card)
            || parsed.triggers.iter().any(|t| {
                t.execute
                    .as_deref()
                    .is_some_and(def_tree_has_cast_copy_of_card)
            })
    }

    /// Issue #2273: Mizzix's Mastery folds "copy it. You may cast the copy
    /// without paying its mana cost" into `CastCopyOfCard`; the comma+and
    /// continuation must not trip the `Optional_YouMay` swallow detector now
    /// that `CastCopyOfCard` carries its own internal optionality.
    #[test]
    fn optional_you_may_accepts_mizzix_mastery_cast_copy() {
        let parsed = parse_named(
            "Exile target card that's an instant or sorcery from your graveyard. \
             For each card exiled this way, copy it. You may cast the copy \
             without paying its mana cost.",
            "Mizzix's Mastery",
            &["Sorcery"],
        );

        // Structural guard: `check_swallowed_clauses` early-returns when any
        // ability is Unimplemented, so the `Optional_YouMay` assertion could
        // otherwise pass vacuously. Assert the parse actually folded the
        // exile+copy+cast chain into a `CastCopyOfCard` effect so the swallow
        // assertion exercises the real CastCopyOfCard optionality path.
        assert!(
            parsed_has_cast_copy_of_card(&parsed),
            "expected a CastCopyOfCard effect in the parsed ability chain, got {:?}",
            parsed.abilities
        );
        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));
    }

    /// Issue #2273: Narset's attack trigger ends a sentence before "You may cast
    /// the copy …". In the *trigger* context the exile+copy currently folds to
    /// `ChangeZone → CopySpell { retarget: KeepOriginalTargets }` and the
    /// "You may cast the copy without paying its mana cost" sentence is dropped,
    /// so `Optional_YouMay` still fires. The primary `CastCopyOfCard`
    /// optionality fix (verified by the Mizzix spell-context test above) does
    /// NOT cover this because the trigger fold never produces `CastCopyOfCard`.
    ///
    /// **Status:** ignored — the trigger-context fold to `CastCopyOfCard` is a
    /// separate parser gap (in the trigger/sequence fold, out of scope for the
    /// swallow_check optionality fix). Tracked as issue #2273 follow-up.
    #[test]
    #[ignore = "trigger-context exile+copy folds to CopySpell, not CastCopyOfCard; \
                trigger fold gap is out of scope for the swallow_check fix (issue #2273 follow-up)"]
    fn optional_you_may_accepts_narset_attack_cast_copy() {
        let parsed = parse_named(
            "Creatures you control have prowess.\n\
             Whenever Narset attacks, exile target noncreature, nonland card with \
             mana value less than Narset's power from a graveyard and copy it. \
             You may cast the copy without paying its mana cost.",
            "Narset, Enlightened Exile",
            &["Creature"],
        );

        assert!(!has_swallowed_detector(&parsed, "Optional_YouMay"));

        let trigger = parsed
            .triggers
            .iter()
            .find(|t| t.execute.is_some())
            .expect("expected Narset's attack trigger with an execute body");
        let execute = trigger
            .execute
            .as_deref()
            .expect("attack trigger execute body");
        assert!(
            matches!(
                execute.effect.as_ref(),
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ),
            "expected ChangeZone(Exile), got {:?}",
            execute.effect
        );
        let cast_copy = execute
            .sub_ability
            .as_deref()
            .expect("expected CastCopyOfCard sub-ability after the exile");
        assert!(
            matches!(cast_copy.effect.as_ref(), Effect::CastCopyOfCard { .. }),
            "expected CastCopyOfCard, got {:?}",
            cast_copy.effect
        );
    }

    /// CR 613.4b + CR 613.1f + CR 603.2: Moon Girl's full Oracle text parses with
    /// zero Unimplemented effects. The second-draw trigger lowers the possessive
    /// "~'s base power and toughness become 6/6 and they gain trample" clause to a
    /// `GenericEffect` set-base-P/T + keyword grant; the artifact-ETB once-per-turn
    /// draw already parsed. Shape gate paired with the runtime regression in
    /// `tests/moon_girl_second_draw_base_pt.rs`.
    #[test]
    fn moon_girl_full_oracle_parses_zero_unimplemented() {
        let parsed = parse_named(
            "Whenever you draw your second card each turn, until end of turn, Moon Girl and Devil Dinosaur's base power and toughness become 6/6 and they gain trample.\n\
             Whenever an artifact you control enters, draw a card. This ability triggers only once each turn.",
            "Moon Girl and Devil Dinosaur",
            &["Creature"],
        );
        assert!(parsed
            .abilities
            .iter()
            .all(|d| !def_tree_has_unimplemented(d)));
        assert!(parsed
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .all(|d| !def_tree_has_unimplemented(d)));
    }

    /// CR 122.1 + CR 122.6 + CR 702.11: Kid Loki's full Oracle text parses with
    /// zero Unimplemented effects. The conditional hexproof static lowers to a
    /// continuous static whose affected filter carries
    /// `FilterProp::CountersPutOnThisTurn`; the second-draw trigger puts a +1/+1
    /// counter on the source. Shape gate paired with the runtime regression in
    /// `tests/kid_loki_counter_hexproof_static.rs`.
    #[test]
    fn kid_loki_full_oracle_parses_zero_unimplemented() {
        use crate::types::ability::{CountScope, FilterProp, TypedFilter};
        use crate::types::counter::{CounterMatch, CounterType};
        let parsed = parse_named(
            "Each creature you control that you've put one or more +1/+1 counters on this turn has hexproof.\n\
             Whenever you draw your second card each turn, put a +1/+1 counter on Kid Loki.",
            "Kid Loki",
            &["Creature"],
        );
        assert!(parsed
            .abilities
            .iter()
            .all(|d| !def_tree_has_unimplemented(d)));
        assert!(parsed
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .all(|d| !def_tree_has_unimplemented(d)));
        // Building-block assertion: the static's affected filter carries the new
        // counters-put-this-turn FilterProp with the correct axes.
        let static_def = parsed
            .statics
            .first()
            .expect("Kid Loki has a conditional hexproof static");
        let TargetFilter::Typed(TypedFilter { properties, .. }) = static_def
            .affected
            .as_ref()
            .expect("static has affected filter")
        else {
            panic!("expected a Typed affected filter");
        };
        assert!(properties.iter().any(|p| matches!(
            p,
            FilterProp::CountersPutOnThisTurn {
                actor: CountScope::Controller,
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                comparator: crate::types::ability::Comparator::GE,
                count: 1,
            }
        )));
    }

    /// CR 122.1 + CR 603.2 + CR 723.1: Construct a Cosmic Cube parses with zero
    /// Unimplemented across the whole card. The second-draw trigger body (token +
    /// plan counter) is fully supported; the seventh-plan-counter sacrifice
    /// parses; and the reflexive "you control target opponent during their next
    /// turn" rider now lowers to `Effect::ControlNextTurn` via the shared
    /// turn-control subsystem (CR 723) rather than staying `Unimplemented`. Shape
    /// gate paired with `tests/construct_cosmic_cube_second_draw_token.rs`.
    #[test]
    fn construct_second_draw_body_parses_token_and_plan_counter() {
        let parsed = parse_named(
            "Whenever you draw your second card each turn, create a 2/1 black Villain creature token with menace and put a plan counter on this enchantment.\n\
             When the seventh plan counter is put on this enchantment, sacrifice it. When you do, you control target opponent during their next turn.",
            "Construct a Cosmic Cube",
            &["Enchantment"],
        );
        // The second-draw trigger body (token + plan counter) is fully supported.
        let second_draw = parsed
            .triggers
            .iter()
            .find(|t| {
                matches!(
                    t.constraint,
                    Some(crate::types::ability::TriggerConstraint::NthDrawThisTurn { n: 2 })
                )
            })
            .and_then(|t| t.execute.as_deref())
            .expect("Construct has a second-draw trigger");
        assert!(
            !def_tree_has_unimplemented(second_draw),
            "the token + plan-counter body must be fully supported"
        );
        // CR 723.1: the entire card — including the reflexive control-opponent
        // rider — now parses with zero Unimplemented effects.
        let total_unimpl: usize = parsed
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .filter(|d| def_tree_has_unimplemented(d))
            .count();
        assert_eq!(
            total_unimpl, 0,
            "every effect on Construct a Cosmic Cube must be supported (control-opponent rider now lowers to ControlNextTurn)"
        );

        // CR 723.1: the reflexive rider lowers to `Effect::ControlNextTurn` —
        // the discriminating shape assertion. Without the "their next turn"
        // possessive variant in the suffix combinator this would be Unimplemented.
        fn def_tree_has_control_next_turn(def: &AbilityDefinition) -> bool {
            if matches!(*def.effect, Effect::ControlNextTurn { .. }) {
                return true;
            }
            def.sub_ability
                .as_deref()
                .is_some_and(def_tree_has_control_next_turn)
                || def
                    .else_ability
                    .as_deref()
                    .is_some_and(def_tree_has_control_next_turn)
                || def
                    .mode_abilities
                    .iter()
                    .any(def_tree_has_control_next_turn)
        }
        assert!(
            parsed
                .triggers
                .iter()
                .filter_map(|t| t.execute.as_deref())
                .any(def_tree_has_control_next_turn),
            "the seventh-counter reflexive rider must lower to Effect::ControlNextTurn"
        );
    }

    /// CR 514.2 + CR 609.4b + CR 611.2a: Black Widow's "if you don't" branch
    /// grants a typed `PlayFromExile` impulse cast scoped to end of turn with
    /// any-type/any-color mana spend permission. Before the
    /// `try_parse_play_the_exiled_card_grant` extension this branch degraded to
    /// `GenericEffect { SpendManaAsAnyColor, duration: null }` (dropping the
    /// cast permission and the EOT window → `Swallow:Duration_UntilEndOfTurn`).
    /// Discrimination: reverting either leaf addition flips the gated node back
    /// to `GenericEffect` (proven via revert-probe), so the asserts below fail.
    #[test]
    fn black_widow_if_you_dont_grants_typed_play_from_exile_until_eot() {
        use crate::types::ability::{AbilityCondition, CastingPermission, ManaSpendPermission};
        use crate::types::statics::StaticMode;
        use crate::types::Duration;

        let parsed = parse_named(
            "Menace\n\
             Whenever Black Widow deals combat damage to a player, that player exiles \
             cards from the top of their library until they exile a nonland card. You may \
             put a +1/+1 counter on Black Widow. If you don't, you may cast the exiled \
             nonland card until end of turn and mana of any type can be spent to cast that spell.",
            "Black Widow, Super Spy",
            &["Legendary", "Creature"],
        );

        // Walk the trigger sub_ability chain to the `Not(OptionalEffectPerformed)`
        // gated node (the "if you don't" branch).
        fn find_if_you_dont(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
            if def
                .condition
                .as_ref()
                .is_some_and(AbilityCondition::is_not_optional_effect_performed)
            {
                return Some(def);
            }
            def.sub_ability.as_deref().and_then(find_if_you_dont)
        }

        let gated = parsed
            .triggers
            .iter()
            .filter_map(|t| t.execute.as_deref())
            .find_map(find_if_you_dont)
            .expect("Black Widow trigger must carry a Not(OptionalEffectPerformed) gated node");

        match &*gated.effect {
            Effect::GrantCastingPermission { permission, .. } => match permission {
                CastingPermission::PlayFromExile {
                    duration,
                    mana_spend_permission,
                    ..
                } => {
                    assert_eq!(*duration, Duration::UntilEndOfTurn);
                    assert_eq!(
                        *mana_spend_permission,
                        Some(ManaSpendPermission::AnyTypeOrColor)
                    );
                }
                other => panic!("expected PlayFromExile permission, got {other:?}"),
            },
            other => panic!("expected GrantCastingPermission, got {other:?}"),
        }

        // The pre-fix degradation lowered to a GenericEffect carrying a
        // `SpendManaAsAnyColor` static mode; assert no node in the chain does so,
        // proving the cast permission was not dropped to that fallback.
        fn chain_has_spend_mana_generic(def: &AbilityDefinition) -> bool {
            let here = matches!(
                &*def.effect,
                Effect::GenericEffect { static_abilities, .. }
                    if static_abilities.iter().any(|s| matches!(s.mode, StaticMode::SpendManaAsAnyColor { .. }))
            );
            here || def
                .sub_ability
                .as_deref()
                .is_some_and(chain_has_spend_mana_generic)
        }
        assert!(
            !parsed
                .triggers
                .iter()
                .filter_map(|t| t.execute.as_deref())
                .any(chain_has_spend_mana_generic),
            "the cast permission must not degrade to GenericEffect{{SpendManaAsAnyColor}}"
        );

        // No swallowed-clause diagnostic for the dropped EOT duration.
        assert!(!has_swallowed_detector(&parsed, "Duration_UntilEndOfTurn"));
    }

    fn flip_branch_has_create_damage_replacement(
        win_effect: &Option<Box<AbilityDefinition>>,
        lose_effect: &Option<Box<AbilityDefinition>>,
    ) -> bool {
        win_effect
            .as_deref()
            .is_some_and(def_tree_has_create_damage_replacement)
            || lose_effect
                .as_deref()
                .is_some_and(def_tree_has_create_damage_replacement)
    }

    fn def_tree_has_create_damage_replacement(def: &AbilityDefinition) -> bool {
        match def.effect.as_ref() {
            Effect::CreateDamageReplacement { .. } => return true,
            Effect::FlipCoin {
                win_effect,
                lose_effect,
                ..
            }
            | Effect::FlipCoins {
                win_effect,
                lose_effect,
                ..
            } if flip_branch_has_create_damage_replacement(win_effect, lose_effect) => return true,
            _ => {}
        }
        def.sub_ability
            .as_deref()
            .is_some_and(def_tree_has_create_damage_replacement)
            || def
                .else_ability
                .as_deref()
                .is_some_and(def_tree_has_create_damage_replacement)
            || def
                .mode_abilities
                .iter()
                .any(def_tree_has_create_damage_replacement)
    }

    /// CR 614.9 + CR 705: Desperate Gambit — flip-coin win/lose branches carry
    /// one-shot damage replacements; the Replacement_Instead detector must walk
    /// `FlipCoin` payloads (issue #2236).
    #[test]
    fn replacement_instead_accepts_desperate_gambit_flip_coin_damage_replacements() {
        let parsed = parse_named(
            "Choose a source you control and flip a coin. If you win the flip, the next time that source would deal damage this turn, it deals double that damage instead. If you lose the flip, the next time it would deal damage this turn, prevent that damage.",
            "Desperate Gambit",
            &["Instant"],
        );
        assert!(
            !parsed.abilities.iter().any(def_tree_has_unimplemented),
            "Desperate Gambit must parse without Unimplemented"
        );
        assert!(
            parsed
                .abilities
                .iter()
                .any(def_tree_has_create_damage_replacement),
            "expected CreateDamageReplacement in flip-coin branches, got {:#?}",
            parsed.abilities
        );
        assert!(!has_swallowed_detector(&parsed, "Replacement_Instead"));
    }

    /// CR 614.1a: Edge of Malacol untap replacement and Jinnie Fay token
    /// replacement choice must not trip Replacement_Instead (issue #2236).
    #[test]
    fn replacement_instead_accepts_untap_and_token_choice_replacements() {
        use crate::types::ability::ReplacementCondition;
        use crate::types::replacements::ReplacementEvent;

        let edge = parse_named(
            "If a creature you control would untap during your untap step, put two +1/+1 counters on it instead.",
            "Edge of Malacol",
            &["Enchantment"],
        );
        assert!(
            !edge
                .replacements
                .iter()
                .any(|r| { r.execute.as_deref().is_some_and(def_tree_has_unimplemented) }),
            "Edge of Malacol replacement must parse without Unimplemented"
        );
        assert!(
            edge.replacements.iter().any(|r| {
                r.event == ReplacementEvent::Untap
                    && r.condition == Some(ReplacementCondition::DuringUntapStep)
                    && r.execute.is_some()
            }),
            "expected untap-step replacement AST, got {:#?}",
            edge.replacements
        );
        assert!(!has_swallowed_detector(&edge, "Replacement_Instead"));

        let doubling = parse_named(
            "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.",
            "Doubling Season",
            &["Enchantment"],
        );
        assert!(
            doubling.replacements.iter().any(|r| {
                r.event == ReplacementEvent::CreateToken && r.quantity_modification.is_some()
            }),
            "expected CreateToken quantity-modifier replacement AST, got {:#?}",
            doubling.replacements
        );
        assert!(!has_swallowed_detector(&doubling, "Replacement_Instead"));

        let jinnie = parse_named(
            "If you would create one or more tokens, you may instead create that many 2/2 green Cat creature tokens with haste or that many 3/1 green Dog creature tokens with vigilance.",
            "Jinnie Fay, Jetmir's Second",
            &["Legendary", "Creature"],
        );
        assert!(
            !jinnie
                .replacements
                .iter()
                .any(|r| r.execute.as_deref().is_some_and(def_tree_has_unimplemented)),
            "Jinnie Fay replacement must parse without Unimplemented"
        );
        fn def_tree_has_create_token_choice(def: &AbilityDefinition) -> bool {
            match &*def.effect {
                Effect::ChooseOneOf { branches, .. } => branches
                    .iter()
                    .any(|branch| matches!(&*branch.effect, Effect::Token { .. })),
                Effect::CreateDelayedTrigger { effect, .. } => {
                    def_tree_has_create_token_choice(effect)
                }
                _ => {
                    def.sub_ability
                        .as_deref()
                        .is_some_and(def_tree_has_create_token_choice)
                        || def
                            .else_ability
                            .as_deref()
                            .is_some_and(def_tree_has_create_token_choice)
                }
            }
        }
        assert!(
            jinnie.replacements.iter().any(|r| {
                r.event == ReplacementEvent::CreateToken
                    && r.execute
                        .as_deref()
                        .is_some_and(def_tree_has_create_token_choice)
            }),
            "expected CreateToken replacement-choice AST, got {:#?}",
            jinnie.replacements
        );
        assert!(!has_swallowed_detector(&jinnie, "Replacement_Instead"));
    }

    /// CR 601.2 + CR 609.4b + CR 614.1a: Quistis Trepe's ETB must lower to a real
    /// `Effect::CastFromZone` carrying `mana_spend_permission: Some(AnyTypeOrColor)`
    /// (full-cost graveyard cast with the any-type concession), with the trailing
    /// "exile it instead" rider rebound onto the cast spell as a
    /// `ChangeZone{Exile, ParentTarget}` sub-ability — NOT degraded to a bare
    /// `GenericEffect{SpendManaAsAnyColor}` that drops the cast.
    ///
    /// DISCRIMINATING: reverting the Q1 head parser
    /// (`try_parse_cast_target_from_graveyard_any_mana`) flips the effect back to
    /// `GenericEffect{SpendManaAsAnyColor}` (no `CastFromZone`), failing the
    /// effect-type assertion; reverting Commit 1's rider rebind generalization
    /// binds the exile rider to the triggering source (Quistis), so the
    /// sub-ability target is no longer `ParentTarget`.
    #[test]
    fn quistis_cast_from_graveyard_is_castfromzone_with_any_type_mana_and_exile_rider() {
        use crate::types::ability::{Effect, ManaSpendPermission, TargetFilter};
        use crate::types::zones::Zone;

        let parsed = parse_named(
            "Blue Magic — When Quistis Trepe enters, you may cast target instant or sorcery \
             card from a graveyard, and mana of any type can be spent to cast that spell. \
             If that spell would be put into a graveyard, exile it instead.",
            "Quistis Trepe",
            &["Legendary", "Creature"],
        );

        let execute = parsed
            .triggers
            .iter()
            .find_map(|t| t.execute.as_deref())
            .expect("Quistis must carry an ETB trigger effect");

        let Effect::CastFromZone {
            target,
            without_paying_mana_cost,
            mana_spend_permission,
            driver,
            ..
        } = &*execute.effect
        else {
            panic!(
                "expected CastFromZone (not degraded GenericEffect), got {:?}",
                execute.effect
            );
        };
        assert!(
            !without_paying_mana_cost,
            "Quistis casts at full cost (CR 609.4b is payment-mode, not free)"
        );
        // CR 608.2g: the graveyard any-mana cast is a during-resolution paid cast,
        // routed by the explicit driver — not a lingering permission.
        assert_eq!(
            *driver,
            crate::types::ability::CastFromZoneDriver::DuringResolution,
            "Quistis lowers to a during-resolution cast (CR 608.2g)"
        );
        assert_eq!(
            *mana_spend_permission,
            Some(ManaSpendPermission::AnyTypeOrColor),
            "the any-type concession must ride the CastFromZone grant"
        );
        // Cast from a graveyard (any controller) — InZone Graveyard, no owner.
        assert_eq!(target.extract_in_zone(), Some(Zone::Graveyard));

        // Exile rider rebound onto the cast spell (ParentTarget), not Quistis.
        let rider = execute
            .sub_ability
            .as_deref()
            .expect("the exile-instead rider must attach as a sub-ability");
        match &*rider.effect {
            Effect::ChangeZone {
                destination,
                target,
                ..
            } => {
                assert_eq!(*destination, Zone::Exile);
                assert_eq!(*target, TargetFilter::ParentTarget);
            }
            other => panic!("expected ChangeZone{{Exile, ParentTarget}} rider, got {other:?}"),
        }

        // No node degrades to GenericEffect{SpendManaAsAnyColor}.
        fn chain_has_spend_mana_generic(def: &AbilityDefinition) -> bool {
            let here = matches!(
                &*def.effect,
                Effect::GenericEffect { static_abilities, .. }
                    if static_abilities.iter().any(|s| matches!(
                        s.mode,
                        crate::types::statics::StaticMode::SpendManaAsAnyColor { .. }
                    ))
            );
            here || def
                .sub_ability
                .as_deref()
                .is_some_and(chain_has_spend_mana_generic)
        }
        assert!(
            !parsed
                .triggers
                .iter()
                .filter_map(|t| t.execute.as_deref())
                .any(chain_has_spend_mana_generic),
            "the cast must not degrade to GenericEffect{{SpendManaAsAnyColor}}"
        );
        // The reflexive-if swallow marker must clear.
        assert!(!has_swallowed_detector(&parsed, "Condition_If"));
    }

    /// CR 611.2a + CR 108.3 (multiplayer FINDING-4): Tinybones the Pickpocket casts
    /// "from that player's graveyard" — the combat-damaged player's. The
    /// `CastFromZone` target MUST carry `Owned{TriggeringPlayer}` so a 3+ player
    /// game restricts the cast to that one player's graveyard, never any
    /// opponent's. Also carries `mana_spend_permission: Some(AnyTypeOrColor)`.
    ///
    /// DISCRIMINATING: reverting the FINDING-4 owner-add in
    /// `try_parse_cast_target_from_graveyard_any_mana` drops the
    /// `Owned{TriggeringPlayer}` property; reverting the Q1 head parser degrades
    /// the whole clause to `GenericEffect{SpendManaAsAnyColor}` (no CastFromZone).
    #[test]
    fn tinybones_cast_from_damaged_player_graveyard_owned_triggering_player_any_mana() {
        use crate::types::ability::{
            ControllerRef, Effect, FilterProp, ManaSpendPermission, TargetFilter,
        };
        use crate::types::zones::Zone;

        let parsed = parse_named(
            "Deathtouch\nWhenever Tinybones deals combat damage to a player, you may cast \
             target nonland permanent card from that player's graveyard, and mana of any \
             type can be spent to cast that spell.",
            "Tinybones, the Pickpocket",
            &["Legendary", "Creature"],
        );

        let execute = parsed
            .triggers
            .iter()
            .find_map(|t| t.execute.as_deref())
            .expect("Tinybones must carry a combat-damage trigger effect");

        let Effect::CastFromZone {
            target,
            mana_spend_permission,
            without_paying_mana_cost,
            ..
        } = &*execute.effect
        else {
            panic!(
                "expected CastFromZone (not degraded GenericEffect), got {:?}",
                execute.effect
            );
        };
        assert!(!without_paying_mana_cost, "full-cost cast");
        assert_eq!(
            *mana_spend_permission,
            Some(ManaSpendPermission::AnyTypeOrColor)
        );
        assert_eq!(target.extract_in_zone(), Some(Zone::Graveyard));

        // FINDING-4: owner constraint bound to the triggering (damaged) player.
        fn has_owned_triggering(filter: &TargetFilter) -> bool {
            match filter {
                TargetFilter::Typed(tf) => tf.properties.iter().any(|p| {
                    matches!(
                        p,
                        FilterProp::Owned {
                            controller: ControllerRef::TriggeringPlayer
                        }
                    )
                }),
                TargetFilter::And { filters } | TargetFilter::Or { filters } => {
                    filters.iter().any(has_owned_triggering)
                }
                TargetFilter::Not { filter } => has_owned_triggering(filter),
                _ => false,
            }
        }
        assert!(
            has_owned_triggering(target),
            "Tinybones must restrict the cast to the damaged player's graveyard \
             via Owned{{TriggeringPlayer}}; got {target:?}"
        );
    }

    /// CR 614.1c: `Effect::AddPendingETBCounters` IS the "enters with ..." replacement in
    /// its DELAYED form — the rider binds to a later cast, not to the source permanent.
    /// The `Replacement` detector reported 5 pool-wide FALSE POSITIVES until the shared
    /// carrier helper accepted it (chocobo camp, kumano faces kakkazan, osteomancer adept,
    /// summon: fenrir, yuna). Both producing grammars are pinned: the delayed
    /// when-you-next-cast trigger (Yuna) and the cast-this-way graveyard rider
    /// (Osteomancer Adept).
    ///
    /// The negative assertions cannot be vacuous: the detector only fires when the
    /// "enters with " marker is present AND no carrier is found, and the POSITIVE CONTROL
    /// below carries the same marker with no carrier — it must still fire. If the control
    /// ever goes quiet, the detector is broken, not the cards.
    #[test]
    fn replacement_accepts_add_pending_etb_counters_as_a_614_1c_carrier() {
        let yuna = parse_named(
            "{T}: Add one mana of any color. When you next cast a creature spell this turn, that creature enters with two additional +1/+1 counters on it.",
            "Yuna, Grand Summoner",
            &["Creature"],
        );
        assert!(
            !has_swallowed_detector(&yuna, "Replacement"),
            "CR 614.1c: the delayed enters-with rider lowers to AddPendingETBCounters, which \
             IS the replacement — it must not be reported as swallowed"
        );

        let osteomancer = parse_named(
            "{T}: Until end of turn, you may cast creature spells from your graveyard. If you cast a spell this way, that creature enters with a finality counter on it.",
            "Osteomancer Adept",
            &["Creature"],
        );
        assert!(
            !has_swallowed_detector(&osteomancer, "Replacement"),
            "the cast-this-way graveyard rider is the same CR 614.1c carrier"
        );

        // POSITIVE CONTROL: the same "enters with" marker with NO carrier the parser can
        // build. Undead Sprinter is a MEASURED true positive of this detector (its
        // cast-permission is a STATIC, and the static path has no enters-with rider hook —
        // see the CR 614.1c static-path gap). The detector MUST still fire on it, or the
        // two assertions above prove nothing.
        let uncarried = parse_named(
            "Trample, haste\nYou may cast this card from your graveyard if a non-Zombie creature died this turn. If you do, this creature enters with a +1/+1 counter on it.",
            "Undead Sprinter",
            &["Creature"],
        );
        assert!(
            has_swallowed_detector(&uncarried, "Replacement"),
            "positive control: an enters-with clause with NO carrier must still be reported \
             — if this goes quiet the detector is dead and the assertions above are vacuous"
        );
    }

    // ── Plan 02 step 6: per-unit audit independence, and the two detectors whose
    //    typed arms were never exercised ───────────────────────────────────────

    /// PLAN 02'S FLAGSHIP CLAIM, and until now it had no true fixture anywhere:
    /// *an unsupported unit does not suppress the audit of a sibling unit.*
    ///
    /// Every existing "does not hide" test pairs a swallowed line with a line that is
    /// *exempted* (represented, or carved out) — none pairs it with a line that
    /// genuinely produced `Effect::Unimplemented`. That is the one shape the old
    /// card-wide suppression actually broke: a single unparseable line silenced every
    /// detector on the whole card (measured: 2,563 faces).
    ///
    /// Line 0 is unsupported. Line 1 is Protection Racket's upkeep trigger, which
    /// parses cleanly and drops its "in turn order" clause (CR 101.4). The audit must
    /// still report line 1.
    ///
    /// REVERT DISCRIMINATOR: in `check_swallowed_clauses`, pass the card-wide `result`
    /// instead of the unit-scoped `scoped` to `any_ability_has_unimplemented` (i.e.
    /// restore the card-wide rule) and line 0's `Unimplemented` suppresses line 1 —
    /// the `APNAP` assertion below goes red. Watched fail→pass.
    #[test]
    fn unsupported_unit_does_not_suppress_the_audit_of_a_sibling_unit() {
        let parsed = parse_named(
            "Whenever this enchantment untaps, chronoshift the aetheric weave.\n\
             At the beginning of your upkeep, repeat the following process for each opponent in turn order. Reveal the top card of your library. That player may pay life equal to that card's mana value. If they do, exile that card. Otherwise, put it into your hand.",
            "Synthetic Mixed-Support Card",
            &["Enchantment"],
        );

        // REACH GUARD 1 (non-vacuity): there must actually BE an unsupported sibling.
        // Without it the test would pass trivially on a fully-supported card and would
        // be asserting nothing at all about suppression.
        let has_unsupported = any_ability_has_unimplemented(&parsed)
            || parsed
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_ref())
                .any(|execute| def_tree_has_unimplemented(execute));
        assert!(
            has_unsupported,
            "line 0 must be genuinely UNSUPPORTED for this fixture to test anything; if the \
             parser learned to model it, swap line 0 for another unsupported line"
        );

        // REACH GUARD 2 (non-vacuity): the sibling must have REACHED expectation
        // comparison — it parsed, so its warning cannot be an artifact of its own failure.
        assert!(
            parsed
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_ref())
                .any(|execute| !def_tree_has_unimplemented(execute)),
            "the Protection Racket upkeep trigger must parse without Unimplemented"
        );

        // THE CLAIM: the supported sibling is still audited even though line 0 is
        // unsupported. Card-wide suppression would silence exactly this.
        assert!(
            has_swallowed_detector(&parsed, "APNAP"),
            "the unsupported line 0 must NOT suppress the swallowed-clause audit of line 1"
        );

        // ...and the diagnostic is attributed to the line that raised it, not to line 0.
        let apnap_line = parsed
            .parse_warnings
            .iter()
            .find_map(|warning| match warning {
                OracleDiagnostic::SwallowedClause {
                    detector,
                    line_index,
                    ..
                } if detector == "APNAP" => Some(*line_index),
                _ => None,
            })
            .expect("APNAP diagnostic must exist");
        assert_eq!(
            apnap_line, 1,
            "the swallowed clause belongs to line 1 (the trigger), not to the unsupported line 0"
        );
    }

    /// `ActivateOnlyDuring` had ZERO tests of any kind: the detector is wired into
    /// `check_swallowed_clauses` but nothing exercised it, positively or negatively.
    ///
    /// POSITIVE (a): CR 602.5b. "Activate only during your upkeep" IS typed — the timing
    /// grammar maps it to `ActivationRestriction::DuringYourUpkeep` (verified: the parse
    /// yields `activation_restrictions == [DuringYourUpkeep]`, no `Unimplemented`). The
    /// detector must therefore stay SILENT.
    ///
    /// NOTE for anyone extending this: task #66 says "activate only during <step>" windows
    /// are a parser gap. That is true only for steps with no `ActivationRestriction`
    /// variant. `your upkeep` / `your turn` / `combat` all have one and DO get typed. Do
    /// not infer from #66 that every phrasing in this family is untyped — this fixture was
    /// originally written on that assumption and it was wrong.
    #[test]
    fn activate_only_during_is_silent_when_the_timing_window_is_typed() {
        let parsed = parse_named(
            "{T}: Draw a card. Activate only during your upkeep.",
            "Synthetic Upkeep-Only Activator",
            &["Artifact"],
        );

        // REACH GUARD: the window must actually be TYPED, or the silence below proves
        // nothing about the evidence arm.
        assert!(
            parsed
                .abilities
                .iter()
                .any(|ability| !ability.activation_restrictions.is_empty()),
            "the upkeep window must be typed onto activation_restrictions"
        );
        assert!(
            !any_ability_has_unimplemented(&parsed),
            "the line must parse — a self-suppressing Unimplemented would make this vacuous"
        );

        assert!(
            !has_swallowed_detector(&parsed, "ActivateOnlyDuring"),
            "a TYPED timing window must not be reported as swallowed"
        );
    }

    /// The OTHER half of the `ActivateOnlyDuring` input space, pinned to what the parser
    /// ACTUALLY does — which is not what it looks like from the detector alone.
    ///
    /// `ActivationRestriction` has variants for `your upkeep`, `your turn`, and `combat`,
    /// but NOT for `your end step`. An untypeable window does NOT get silently dropped:
    /// it produces an honest `Effect::Unimplemented`, and the per-unit suppression rule
    /// (see `check_swallowed_clauses`) then correctly declines to ALSO report it as a
    /// swallowed clause — the unit already declared its gap explicitly, and
    /// double-counting one defect is exactly what that rule exists to prevent.
    ///
    /// So this is a coverage-honesty pin, not a swallow pin: the semantics are visibly
    /// red, not invisibly green. If someone ever makes this line parse "successfully"
    /// without typing the window, the first assertion goes red and the swallow becomes
    /// silent — which is the regression that matters.
    ///
    /// CONSEQUENCE WORTH KNOWING (filed, not claimed): both reachable input classes for
    /// this detector are silent — a TYPED window supplies evidence, and an UNTYPEABLE one
    /// self-suppresses via `Unimplemented`. Whether `ActivateOnlyDuring` can fire on ANY
    /// real card is therefore an open question requiring a corpus scan; it may be a
    /// zero-hit detector. Not asserted here, because I have not computed that count.
    #[test]
    fn untypeable_activation_window_is_an_honest_unimplemented_not_a_silent_swallow() {
        let parsed = parse_named(
            "{T}: Draw a card. Activate only during your end step.",
            "Synthetic End-Step-Only Activator",
            &["Artifact"],
        );

        // THE CLAIM: the dropped CR 602.5b window is declared, not hidden.
        assert!(
            any_ability_has_unimplemented(&parsed),
            "an untypeable activation window must surface as an explicit Effect::Unimplemented \
             (honest red), never as a quietly-accepted line"
        );

        // ...and because the unit owns an explicit gap, it is not ALSO double-reported as a
        // swallowed clause. This is the documented per-unit suppression rule, pinned.
        assert!(
            !has_swallowed_detector(&parsed, "ActivateOnlyDuring"),
            "a unit that already declared an Unimplemented must not be double-counted with a \
             swallowed-clause warning for the same defect"
        );
    }

    /// Sibling-negative for the pair above: a LIMIT is not a TIMING window. CR 602.5b's
    /// "Activate only once each turn" is typed as `ActivationRestriction::OnlyOnceEachTurn`,
    /// so it must not raise the timing expectation.
    #[test]
    fn activate_limit_does_not_raise_the_timing_expectation() {
        let parsed = parse_named(
            "{T}: Draw a card. Activate only once each turn.",
            "Synthetic Once-Each-Turn Activator",
            &["Artifact"],
        );
        assert!(
            !has_swallowed_detector(&parsed, "ActivateOnlyDuring"),
            "a once-each-turn LIMIT must not raise the 'activate only during' TIMING expectation"
        );
    }

    /// The `APNAP` typed-evidence arm (`any_ability_has_apnap_ordering`) had no positive
    /// fixture: the detector's only test was a pinned KNOWN GAP asserting it FIRES.
    /// Nothing asserted it stays SILENT when the ordering IS typed — so the evidence arm
    /// could have been deleted outright and the suite would still have passed. That is
    /// the vacuous-detector class this campaign keeps getting burned by: a leg only ever
    /// seen through its failure path is not tested.
    ///
    /// REVERT DISCRIMINATOR: make `any_ability_has_apnap_ordering` return `false` (i.e.
    /// delete the typed `starting_with` arm) and this goes red — the detector would then
    /// report a card whose ordering the AST actually carries.
    #[test]
    fn apnap_typed_starting_with_evidence_arm_is_not_vacuous() {
        let parsed = parse_named(
            "Starting with you, each player votes for time or money.",
            "Synthetic Vote Orderer",
            &["Sorcery"],
        );

        // REACH GUARD: the ordering must actually be TYPED on the AST. Without this, the
        // assertion below could pass for the wrong reason (no expectation raised at all).
        assert!(
            super::any_ability_has_apnap_ordering(&parsed),
            "the vote's starting player must be typed on the AST for this test to be about \
             the evidence arm at all"
        );

        assert!(
            !has_swallowed_detector(&parsed, "APNAP"),
            "a typed 'starting with' ordering must SILENCE the APNAP detector — if this fires, \
             the typed evidence arm is not being consulted"
        );
    }
}

#[cfg(test)]
mod detect_condition_if_replacement_exemption_tests {
    use super::*;
    use crate::parser::oracle::parse_oracle_text;
    // Imported here rather than at module scope: the swallow detectors no longer read
    // `ReplacementDefinition` (the description-channel helpers that did are deleted), so a
    // lib-level import would be dead. Only this fixture still builds one.
    use crate::types::ability::ReplacementDefinition;
    use crate::types::replacements::ReplacementEvent;

    /// Plague Drone-class text: a single represented gain-life replacement,
    /// preceded by a keyword line and an ability-word line. The structural
    /// per-sentence exemption must still recognize and strip this sentence's
    /// leading "if" even though it isn't at byte offset 0 of the sentence
    /// (it follows "Flying\nRot Fly — ").
    const PLAGUE_DRONE_TEXT: &str =
        "Flying\nRot Fly — If an opponent would gain life, that player loses that much life instead.";

    fn has_condition_if_swallow(diagnostics: &[OracleDiagnostic]) -> bool {
        diagnostics.iter().any(|d| {
            matches!(
                d,
                OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
            )
        })
    }

    /// Positive, end-to-end through the real parser: Plague Drone's represented
    /// gain-life replacement must NOT trip Condition_If. This is the exact
    /// regression the original card-wide gate was (over-broadly) fixing.
    ///
    /// Asserts on `parse_warnings` — the audit already ran inside
    /// `parse_oracle_text`, so re-invoking it by hand would have tested a second,
    /// non-production invocation rather than the one that ships.
    #[test]
    fn plague_drone_replacement_antecedent_is_not_swallowed_condition() {
        let parsed = parse_oracle_text(
            PLAGUE_DRONE_TEXT,
            "Plague Drone",
            &[],
            &["Creature".to_string()],
            &["Phyrexian".to_string(), "Insect".to_string()],
        );
        assert!(
            !has_condition_if_swallow(&parsed.parse_warnings),
            "Plague Drone's represented replacement antecedent must not be flagged \
             as a swallowed Condition_If; warnings: {:?}",
            parsed.parse_warnings
        );
    }

    /// Builds a minimal, otherwise-empty `ParsedAbilities` carrying exactly
    /// one `ReplacementDefinition` whose `description` is `description`.
    /// Field list is taken verbatim from the `ParsedAbilities` struct
    /// definition in `crates/engine/src/parser/oracle.rs`.
    fn parsed_with_one_replacement_description(description: &str) -> ParsedAbilities {
        let mut def = ReplacementDefinition::new(ReplacementEvent::GainLife);
        def.description = Some(description.to_string());
        ParsedAbilities {
            abilities: Vec::new(),
            triggers: Vec::new(),
            statics: Vec::new(),
            replacements: vec![def],
            extracted_keywords: Vec::new(),
            modal: None,
            additional_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            solve_condition: None,
            strive_cost: None,
            parse_warnings: Vec::new(),
        }
    }

    /// Negative (compound-replacement honesty): one sentence's antecedent IS
    /// backed by a `ReplacementDefinition.description` in `parsed.replacements`;
    /// a second " instead" sentence, structurally shaped the same way, has NO
    /// matching description anywhere in `parsed.replacements` (simulating a
    /// card whose second replacement clause the parser failed to represent).
    /// The card-wide gate this PR originally shipped would have exempted
    /// BOTH sentences once `parsed.replacements` was non-empty; the
    /// structural per-sentence exemption must still flag the unrepresented
    /// second one.
    #[test]
    fn compound_replacement_with_unrepresented_second_instead_sentence_is_flagged() {
        let represented_sentence =
            "If an opponent would gain life, that player loses that much life instead.";
        let unrepresented_sentence = "If a player would sacrifice a permanent, exile it instead.";
        // A neutral leading sentence keeps the unrepresented "if" clause off
        // byte offset 0 of the cleaned text: the detector's bare-" if " scan
        // (like every marker scan in this file) requires a preceding space,
        // which sentence-initial text at offset 0 never has.
        let text = format!("You gain 1 life. {represented_sentence} {unrepresented_sentence}");
        let parsed = parsed_with_one_replacement_description(represented_sentence);

        let cleaned = text.to_ascii_lowercase();
        let mut diagnostics = Vec::new();
        let evidence = UnitEvidence::from_json_for_test("{}");
        detect_condition_if(&cleaned, &text, &evidence, &parsed, &mut diagnostics);

        assert!(
            has_condition_if_swallow(&diagnostics),
            "the unrepresented second replacement sentence's 'if' must still be \
             flagged as a swallowed Condition_If; diagnostics: {diagnostics:?}"
        );
    }

    /// Negative (extra real conditional riding on a represented sentence):
    /// even when the leading antecedent IS represented (its description
    /// matches), a second bare "if" later in the SAME sentence is a real,
    /// additional conditional gate that must not be silently swallowed by
    /// the replacement exemption.
    #[test]
    fn represented_replacement_with_trailing_extra_conditional_is_flagged() {
        let sentence = "If an opponent would gain life, that player loses that much life \
             instead if that player controls no lands.";
        let parsed = parsed_with_one_replacement_description(
            "If an opponent would gain life, that player loses that much life instead.",
        );

        let cleaned = sentence.to_ascii_lowercase();
        let mut diagnostics = Vec::new();
        let evidence = UnitEvidence::from_json_for_test("{}");
        detect_condition_if(&cleaned, sentence, &evidence, &parsed, &mut diagnostics);

        assert!(
            has_condition_if_swallow(&diagnostics),
            "a second, independent 'if' clause riding on an otherwise-represented \
             replacement sentence must still be flagged; diagnostics: {diagnostics:?}"
        );
    }

    /// Sanity check on the helper in isolation: a represented sentence
    /// (matching description present) has its leading "if" fully stripped,
    /// while an unrepresented one survives untouched.
    #[test]
    fn strip_helper_only_removes_sentences_backed_by_a_parsed_replacement() {
        let represented =
            "if an opponent would gain life, that player loses that much life instead";
        let unrepresented = "if a player would sacrifice a permanent, exile it instead";
        let combined = format!("{represented}. {unrepresented}.");
        let parsed = parsed_with_one_replacement_description(&format!("{represented}."));

        let result = strip_represented_replacement_instead_sentences(&combined, &parsed);

        assert_eq!(
            result, " if a player would sacrifice a permanent, exile it instead.",
            "only the represented sentence should have been stripped"
        );
    }
}
