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

use super::oracle::ParsedAbilities;
use super::oracle_warnings::push_warning;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, ContinuousModification, Effect, ModalSelectionConstraint,
    OpponentMayScope, PlayerFilter, QuantityExpr, ReplacementDefinition, ReplacementMode,
    StaticDefinition, TargetFilter, TriggerDefinition,
};
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;

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

/// Run all swallow detectors against the parsed result. Each finding is
/// pushed to the thread-local parse_warnings buffer.
pub fn check_swallowed_clauses(oracle_text: &str, parsed: &ParsedAbilities) {
    if oracle_text.is_empty() {
        return;
    }
    // Architectural rule: a parser that produced `Effect::Unimplemented` for
    // any ability has *explicitly* admitted it couldn't parse a line — the
    // text is preserved on the Unimplemented effect itself and a separate
    // coverage warning is raised. Suppress all swallow detectors in that
    // case to avoid double-reporting the same gap. Cards with partial
    // parses (some abilities ok, some Unimplemented) still get checked
    // for their parsed portions via the per-detector marker logic below.
    if any_ability_has_unimplemented(parsed) {
        return;
    }
    let lower_owned = oracle_text.to_ascii_lowercase();
    let cleaned = strip_parens(&lower_owned);

    // Pre-compute JSON haystack for detectors that introspect AST shape via
    // serialized field presence. One serialization per card amortizes across
    // detectors. JSON serialization can fail on pathological data; on
    // failure we skip those detectors rather than panicking.
    let ast_json = serde_json::to_string(parsed).unwrap_or_default();

    detect_replacement_instead(&cleaned, oracle_text, parsed);
    detect_activate_only_during(&cleaned, oracle_text, parsed);
    detect_activate_limit(&cleaned, oracle_text, parsed);
    detect_duration_until_eot(&cleaned, oracle_text, parsed);
    detect_optional_you_may(&cleaned, oracle_text, parsed);
    detect_dynamic_qty(&cleaned, oracle_text, &ast_json);
    detect_condition_if(&cleaned, oracle_text, &ast_json, parsed);
    detect_condition_unless(&cleaned, oracle_text, &ast_json);
    detect_condition_as_long_as(&cleaned, oracle_text, &ast_json, parsed);
    detect_duration_this_turn(&cleaned, oracle_text, &ast_json);
    detect_duration_next_turn(&cleaned, oracle_text, &ast_json);
    detect_optional_may_have(&cleaned, oracle_text, &ast_json);
    detect_apnap(&cleaned, oracle_text, &ast_json);
}

// ── Detector A: Replacement_Instead ─────────────────────────────────────

/// CR 614: "if X would Y, [do Z] instead" — every "instead" phrase outside of
/// reminder text must yield a `ReplacementDefinition` somewhere in the parsed
/// abilities. If Oracle has " instead" but `replacements` is empty AND no
/// existing ability captures replacement semantics, the clause was swallowed.
fn detect_replacement_instead(cleaned: &str, original: &str, parsed: &ParsedAbilities) {
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
    // Some cards model "instead" inside a static or ability rather than as
    // a top-level replacement (e.g., conditional alternatives). Conservative
    // exemption: if any static/ability/trigger description mentions "instead",
    // assume the parser captured it.
    if any_text_field_contains(parsed, "instead") {
        return;
    }
    push_warning(format!(
        "Swallow:Replacement_Instead — \"instead\" in Oracle text not captured as a replacement: {}",
        truncate(original, 140)
    ));
}

// ── Detector B: ActivateOnlyDuring ──────────────────────────────────────

/// CR 605.1c: "Activate only during X" — restricted activation timing.
/// Must be represented as an activation constraint on the parsed ability.
fn detect_activate_only_during(cleaned: &str, original: &str, parsed: &ParsedAbilities) {
    let has_marker = cleaned.contains("activate only during") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("activate this ability only during"); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    if any_ability_has_constraint(parsed) {
        return;
    }
    push_warning(format!(
        "Swallow:ActivateOnlyDuring — \"activate only during\" not captured as constraint: {}",
        truncate(original, 140)
    ));
}

// ── Detector C: ActivateLimit ───────────────────────────────────────────

/// CR 605: "Activate this ability only once/twice/no more than N times each
/// turn" — usage-limited activation. Must be represented as an activation
/// limit on the parsed ability.
fn detect_activate_limit(cleaned: &str, original: &str, parsed: &ParsedAbilities) {
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
    push_warning(format!(
        "Swallow:ActivateLimit — activation limit phrase not captured: {}",
        truncate(original, 140)
    ));
}

// ── Detector D: Duration_UntilEndOfTurn ─────────────────────────────────

/// CR 611.2a: "until end of turn" — temporal scope. Must be represented as a
/// duration on the parsed ability.
fn detect_duration_until_eot(cleaned: &str, original: &str, parsed: &ParsedAbilities) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("until end of turn") {
        return;
    }
    if any_ability_has_duration(parsed) {
        return;
    }
    push_warning(format!(
        "Swallow:Duration_UntilEndOfTurn — \"until end of turn\" not captured as duration: {}",
        truncate(original, 140)
    ));
}

// ── Detector E: Optional_YouMay ─────────────────────────────────────────

/// CR 117.3a: "you may [verb]" — optional effect. The triggered/activated
/// ability that contains this phrase must have its `optional` flag set.
fn detect_optional_you_may(cleaned: &str, original: &str, parsed: &ParsedAbilities) {
    // Only the bare "you may [verb]" optional-effect form. We exclude
    // "if you may" / "you may have" / "you may cast" patterns where the "may"
    // belongs to a different grammatical construction.
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
    push_warning(format!(
        "Swallow:Optional_YouMay — \"you may\" optional effect not captured as optional: {}",
        truncate(original, 140)
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
fn effect_has_internal_optionality(effect: &Effect) -> bool {
    match effect {
        Effect::Dig { up_to: true, .. }
        | Effect::GrantCastingPermission { .. }
        | Effect::CastFromZone { .. }
        | Effect::PayCost { .. }
        | Effect::RevealHand {
            choice_optional: true,
            ..
        }
        | Effect::RevealFromHand {
            on_decline: Some(_),
            ..
        } => true,
        Effect::CreateEmblem { statics, triggers } => {
            statics.iter().any(static_definition_has_optional)
                || triggers.iter().any(trigger_tree_has_optional)
        }
        _ => false,
    }
}

/// Recursive walk: does any def in the tree carry an `AddTargetReplacement`
/// effect? This single Effect variant simultaneously encodes a replacement
/// effect (CR 614.1a "instead"), a conditional gate ("if [target] would die"),
/// and an EOT duration (the carried replacement's `expires_at_eot`). Its
/// presence satisfies the Replacement_Instead, Condition_If, and
/// Duration_ThisTurn detectors when the original text matches the
/// "die this turn, exile instead" rider grammar.
fn def_tree_has_target_replacement(def: &AbilityDefinition) -> bool {
    if matches!(*def.effect, Effect::AddTargetReplacement { .. }) {
        return true;
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
        _ => false,
    })
}

fn static_mode_is_optional_permission(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::MayLookAtTopOfLibrary
            | StaticMode::MayChooseNotToUntap
            | StaticMode::MayPlayAdditionalLand
            | StaticMode::TopOfLibraryCastPermission { .. }
            // CR 702.8: "You may cast this spell as though it had flash" —
            // opt-in cast-timing permission.
            | StaticMode::CastWithFlash
            // CR 702.51a: "Creature spells you cast have convoke",
            // "you may cast X as though it had flash if you pay Y" —
            // generalized cast-timing/keyword permission, always opt-in.
            | StaticMode::CastWithKeyword { .. }
            // CR 117.3a: "You may play lands from your graveyard"
            // (Crucible, Ramunap Excavator, etc.) — graveyard-as-zone
            // cast permission, structurally opt-in.
            | StaticMode::GraveyardCastPermission { .. }
            // CR 601.2f: Defiler-style cost reductions encode the optional
            // life payment inside the static cost-modification primitive.
            | StaticMode::DefilerCostReduction { .. }
    )
}

fn static_definition_has_optional(s: &StaticDefinition) -> bool {
    static_carries_optional_modification(s) || static_mode_is_optional_permission(&s.mode)
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

fn any_ability_has_exile_parent_rider(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_exile_parent_rider)
        || parsed.triggers.iter().any(|t| {
            t.execute
                .as_deref()
                .is_some_and(def_tree_has_exile_parent_rider)
        })
}

fn def_tree_has_conditional_mana_spell_grant(def: &AbilityDefinition) -> bool {
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
        StaticMode::ReduceCost {
            spell_filter: Some(filter),
            ..
        }
        | StaticMode::RaiseCost {
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
            Some(crate::types::ability::AdditionalCost::Optional(_)
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

/// Recursive walk: does any def in the tree have a non-None duration?
fn def_tree_has_duration(def: &AbilityDefinition) -> bool {
    if def.duration.is_some() {
        return true;
    }
    if let Some(ref sub) = def.sub_ability {
        if def_tree_has_duration(sub) {
            return true;
        }
    }
    if let Some(ref else_ab) = def.else_ability {
        if def_tree_has_duration(else_ab) {
            return true;
        }
    }
    def.mode_abilities.iter().any(def_tree_has_duration)
}

fn any_ability_has_duration(parsed: &ParsedAbilities) -> bool {
    parsed.abilities.iter().any(def_tree_has_duration)
        || parsed
            .triggers
            .iter()
            .any(|t| t.execute.as_deref().is_some_and(def_tree_has_duration))
        // CR 614.1a: AddTargetReplacement carries an implicit EOT duration
        // for die-exile riders ("if it would die this turn, exile it instead").
        // Its presence in the AST satisfies the Duration_ThisTurn detector.
        || any_ability_has_target_replacement(parsed)
        // Replacements that target a creature with EOT-bounded "die-exile"
        // riders, prevent-damage with this-turn scope, etc. — durations
        // are inside the execute tree or implicit in the replacement event
        // filter for one-shots like "prevent all combat damage this turn".
        // CR 614.6 / CR 614.13: `Mandatory` prevention/exile riders bounded
        // to this turn are inherent to the spell's resolution (one-shot),
        // not a separate `duration` slot.
        || parsed.replacements.iter().any(|r| {
            r.execute
                .as_deref()
                .is_some_and(def_tree_has_duration)
                || matches!(
                    r.event,
                    crate::types::replacements::ReplacementEvent::DamageDone
                )
        })
        || parsed.statics.iter().any(static_has_duration)
        || any_ability_has_conditional_mana_spell_grant(parsed)
}

fn static_has_duration(s: &StaticDefinition) -> bool {
    // StaticDefinition's effect contains the modification scope; durations
    // on continuous effects show up as `Duration` slots inside Effect::Pump,
    // Effect::Animate, etc. Conservative check: serialize-like field probing
    // would be cleaner but for Phase 1 we accept any static abilities as
    // "captured the line" — durations inside statics are off-scope here.
    let _ = s;
    true
}

fn any_ability_has_constraint(parsed: &ParsedAbilities) -> bool {
    // CR 605: activation constraints are stored on
    // `AbilityDefinition.activation_restrictions` (sorcery-speed timing,
    // upkeep gates, etc.) and on `TriggerDefinition.constraint`.
    parsed.abilities.iter().any(def_has_activation_restriction)
        || parsed.triggers.iter().any(|t| t.constraint.is_some())
}

fn def_has_activation_restriction(def: &AbilityDefinition) -> bool {
    !def.activation_restrictions.is_empty() || def.sorcery_speed
}

fn any_ability_has_limit(parsed: &ParsedAbilities) -> bool {
    // For Phase 1, treat presence of any non-trivial `constraint` as
    // covering activation limits too. Phase 2 will split these.
    any_ability_has_constraint(parsed)
}

fn any_text_field_contains(parsed: &ParsedAbilities, needle: &str) -> bool {
    parsed
        .abilities
        .iter()
        .any(|d| def_description_contains(d, needle))
        || parsed
            .triggers
            .iter()
            .any(|t| trigger_description_contains(t, needle))
        || parsed
            .statics
            .iter()
            .any(|s| static_description_contains(s, needle))
}

fn def_description_contains(def: &AbilityDefinition, needle: &str) -> bool {
    if let Some(ref desc) = def.description {
        if desc.to_ascii_lowercase().contains(needle) {
            return true;
        }
    }
    if let Effect::Unimplemented {
        description: Some(d),
        ..
    } = &*def.effect
    {
        if d.to_ascii_lowercase().contains(needle) {
            return true;
        }
    }
    if let Some(ref sub) = def.sub_ability {
        if def_description_contains(sub, needle) {
            return true;
        }
    }
    false
}

fn trigger_description_contains(trig: &TriggerDefinition, needle: &str) -> bool {
    if let Some(ref desc) = trig.description {
        if desc.to_ascii_lowercase().contains(needle) {
            return true;
        }
    }
    trig.execute
        .as_deref()
        .is_some_and(|d| def_description_contains(d, needle))
}

fn static_description_contains(s: &StaticDefinition, needle: &str) -> bool {
    if let Some(ref desc) = s.description {
        return desc.to_ascii_lowercase().contains(needle);
    }
    false
}

// Tag unused for the Phase 1 minimum implementation — left in scope
// for the predicates above.
#[allow(dead_code)]
fn replacement_description_contains(r: &ReplacementDefinition, needle: &str) -> bool {
    if let Some(ref desc) = r.description {
        return desc.to_ascii_lowercase().contains(needle);
    }
    false
}

// ── JSON-haystack detectors ─────────────────────────────────────────────
//
// These detectors operate by checking the serialized AST for representation
// markers. They share a single `ast_json` haystack pre-computed once per
// card. JSON-string scanning is less precise than struct walking but
// dramatically simpler for detectors that touch many AST shapes (e.g.,
// dynamic-quantity is carried by `QuantityExpr` which lives inside dozens
// of effect variants).

/// Word-bounded contains check on the JSON haystack. Looks for any of the
/// given representation markers; returns true if at least one is present.
fn json_has_any(ast_json: &str, markers: &[&str]) -> bool {
    markers.iter().any(|m| ast_json.contains(m))
}

// ── Detector F: DynamicQty ──────────────────────────────────────────────

/// Oracle text contains dynamic-quantity grammar ("equal to", "for each",
/// "twice", "where x is", "the number of", "half [poss]") but the parsed
/// AST contains no dynamic carrier (Ref, Multiply, HalfRounded, Offset,
/// Variable, EventContext, ForEach, NumberOf). The clause was swallowed.
///
/// CR 107.1a + CR 107.3 + CR 119.1: dynamic quantities must produce typed
/// `QuantityExpr` carriers — never silently substituted with `Fixed`.
fn detect_dynamic_qty(cleaned: &str, original: &str, ast_json: &str) {
    // CR 605.1g: "Activate ... twice each turn" is a fixed-count activation
    // limit (handled by ActivateLimit detector), not a dynamic quantity.
    // "twice that many" / "twice X" remain real dynamic-quantity markers.
    let twice_is_activation_limit = cleaned.contains("twice each turn") // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("twice that") // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("twice x"); // allow-noncombinator: swallow detector marker scan on classified text
    let has_marker = cleaned.contains(" equal to ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("for each ") // allow-noncombinator: swallow detector marker scan on classified text
        || (cleaned.contains(" twice ") && !twice_is_activation_limit) // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("where x is ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("the number of ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("half your ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("half their ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("half its ") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("half the "); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    // Any of these AST markers is sufficient evidence the dynamic clause
    // was captured somewhere. The list mirrors the QuantityExpr / QuantityRef
    // variant names plus the few specialty refs that don't tag-serialize as
    // `"Variable"`/`"Multiply"`/etc.
    let dynamic_markers: &[&str] = &[
        "\"type\":\"Ref\"",
        "\"type\":\"Multiply\"",
        "\"type\":\"HalfRounded\"",
        "\"type\":\"Offset\"",
        "\"type\":\"Sum\"",
        "\"Variable\"",
        "EventContext",
        "CountersOn",
        "NumberOf",
        "ForEach",
        "TrackedSetSize",
        "LifeLost",
        "LifeGained",
        "Devotion",
        "ManaValue",
        // CR 601.2f / CR 117.7: spell- and ability-cost reductions whose
        // {N} amount is multiplied by a dynamic count of objects, zone
        // contents, mana value, etc. The carrier is the `dynamic_count`
        // field on `StaticMode::ReduceCost` and `StaticMode::RaiseCost`,
        // populated with `ObjectCount` / `ZoneCardCount` / `Devotion` /
        // `ManaValue` typed quantity refs.
        "\"dynamic_count\":{",
        "ObjectCount",
        "ZoneCardCount",
        // Bloom Tender / Faeburrow Elder class: "For each color among
        // permanents you control, add one mana of that color" is captured as
        // a dynamic mana-production carrier, not a QuantityExpr count.
        "DistinctColorsAmongPermanents",
        // CR 702.122: Strive — "this spell costs {N} more for each target
        // beyond the first" is captured on the top-level `Card` as
        // `strive_cost: Some(ManaCost)`, not inside an ability tree.
        "\"strive_cost\":{",
        // CR 702.139 / CR 702.41: Affinity / Improvise / Convoke style
        // built-in cost mods — captured as `keywords` entries with cost
        // payload, not as in-AST quantity expressions.
        "\"Affinity\":",
        // CR 702.34 / CR 702.144 / CR 702.83: Flashback / Scavenge /
        // Replicate "cost equal to its mana cost" — encoded as a dynamic
        // mana-cost reference rather than a fixed cost.
        "SelfManaCost",
        "TargetManaCost",
        // CR 702.20a: "assigns combat damage equal to its toughness
        // rather than its power" — Brontodon class. Encoded as a typed
        // continuous-modification variant, not a quantity expression.
        "AssignDamageFromToughness",
        "AssignDamageAsThoughUnblocked",
        // CR 508.1h + CR 509.1d: Ghostly Prison / Propaganda combat-tax
        // phrasing uses "for each creature" but is encoded as a typed
        // scaling mode on `StaticCondition::UnlessPay`, not as a
        // `QuantityExpr` carrier.
        "PerAffectedCreature",
        // CR 614.1d: "twice that many" / "thrice that many" replacement
        // multipliers (Doubling Season, Parallel Lives, Anointed
        // Procession, Branching Evolution, Hardened Scales class) are
        // encoded as `quantity_modification: { type: Double }` on the
        // ReplacementDefinition, not as a QuantityExpr in the effect.
        "\"quantity_modification\":{",
    ];
    if json_has_any(ast_json, dynamic_markers) {
        return;
    }
    push_warning(format!(
        "Swallow:DynamicQty — dynamic-quantity grammar present but AST has only Fixed values: {}",
        truncate(original, 140)
    ));
}

// ── Detector G: Condition_If ────────────────────────────────────────────

/// CR 608.2c: "if [condition], [effect]" — conditional gate. Must be
/// represented as a `condition` / `constraint` field on the parsed ability,
/// or as an `unless_pay` / `unless_filter` for the inverse form.
fn detect_condition_if(cleaned: &str, original: &str, ast_json: &str, parsed: &ParsedAbilities) {
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
    // CR 615.5: "If damage is prevented this way, [effect]" is not an
    // independent condition; prevention replacements encode it by storing the
    // follow-up in `execute`, which the replacement pipeline only fires from
    // the `Prevented` arm.
    // allow-noncombinator: swallow detector marker scan on classified text
    if stripped.contains("if damage is prevented this way") {
        return;
    }
    // CR 118.12 + CR 614.12a: "you may pay [cost]. If you don't, ..."
    // is encoded as `ReplacementMode::MayCost { decline }`; the decline
    // branch is the alternative instruction, not an uncaptured condition.
    // allow-noncombinator: swallow detector marker scan on classified text
    if stripped.contains("if you don't") && any_replacement_has_may_cost_decline(parsed) {
        return;
    }
    // CR 117.6 / 702.8: A `SpellCastingOption` with `cost: Some(_)` encodes
    // the "if you pay [cost]" surcharge gate inline (Ghitu Fire, Rout-class
    // "as though it had flash if you pay X" cycle). The "if" is a cost
    // payment trigger, not a conditional check on game state.
    let has_pay_phrase = stripped.contains("if you pay "); // allow-noncombinator: swallow detector marker scan on classified text
    if parsed.casting_options.iter().any(|o| o.cost.is_some()) && has_pay_phrase {
        return;
    }
    // Bare " if " — covers prefix conditional ("if X, do Y") and suffix
    // conditional ("do Y if X"). Excluded: "as if", "even if" — modifiers,
    // not conditions. Also "if able" (CR 701.27) — must-attack/must-block
    // riders, encoded as `MustAttack`/`MustBeBlocked` static modes.
    let has_marker = stripped.contains(" if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !stripped.contains(" as if ") // allow-noncombinator: swallow detector marker scan on classified text
        && !stripped.contains(" even if "); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    let cond_markers: &[&str] = &[
        "\"condition\":{",
        "\"constraint\":{",
        "\"unless_filter\":{",
        "\"unless_pay\":{",
        "\"if_clause\"",
        "\"intervening_if\"",
        "Conditional",
        "QuantityCheck",
        "ConditionMet",
        // "if you do" pattern produces sub_ability chains; this is a
        // representation marker.
        "IfYouDo",
        "ConditionalEffect",
        // CR 614.1a: AddTargetReplacement encodes the "if [target] would die"
        // gate via the carried ReplacementDefinition's event/destination_zone.
        "AddTargetReplacement",
        // CR 701.27 / CR 506.6: must-attack and must-block "if able" riders
        // are encoded as static-mode constraints or as `ForceBlock`/`ForceAttack`
        // effects, not conditional gates.
        "\"mode\":\"MustAttack\"",
        "\"mode\":\"MustBlock\"",
        "\"mode\":\"MustBeBlocked\"",
        "\"type\":\"ForceBlock\"",
        "\"type\":\"ForceAttack\"",
        // CR 305.9: "as ~ enters, you may pay X. If you don't, it enters
        // tapped." — encoded as ReplacementMode::Optional with a `decline`
        // branch that performs the alternative, OR (for cards like Ancient
        // Amphitheater) as an effect with an `on_decline` branch.
        "\"mode\":{\"type\":\"Optional\"",
        "\"on_decline\":{",
        // CR 117.3a: TopOfLibraryCastPermission with `alt_cost` IS the "if
        // you cast a spell this way, pay X" gate (Bolas's Citadel etc.).
        "TopOfLibraryCastPermission",
        // CR 705: FlipCoin / FlipCoins / RollDie variants encode the
        // "if you win the flip" / "if you lose" / die-result branches as
        // structured win_effect/lose_effect/results sub-trees. Their
        // presence IS the conditional gate (Aleatory, Chaotic Strike,
        // Boompile, Bottle of Suleiman, etc.).
        "\"win_effect\":{",
        "\"lose_effect\":{",
        "\"type\":\"FlipCoin\"",
        "\"type\":\"FlipCoins\"",
        "\"type\":\"RollDie\"",
        "DefilerCostReduction",
    ];
    if json_has_any(ast_json, cond_markers) {
        return;
    }
    push_warning(format!(
        "Swallow:Condition_If — \"if <condition>\" not captured as condition/constraint: {}",
        truncate(original, 140)
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

// ── Detector H: Condition_Unless ────────────────────────────────────────

/// CR 608.2c + CR 118.12: "unless [X]" — inverse conditional or
/// unless-pay-cost rider. Must produce an `unless_*` slot or a
/// `condition` with negated semantics.
fn detect_condition_unless(cleaned: &str, original: &str, ast_json: &str) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains(" unless ") {
        return;
    }
    let markers: &[&str] = &[
        "\"unless_filter\":{",
        "\"unless_pay\":{",
        "\"unless_condition\":{",
        "\"unless_payment\":{",
        "\"condition\":{",
        "Unless",
        // CR 605.1a: `CantBeActivated { exemption: ManaAbilities }` is the
        // structural encoding of "can't be activated unless they're mana abilities."
        "\"exemption\":\"ManaAbilities\"",
        // CR 118.12: "Counter target spell unless its controller pays X" —
        // captured as `Effect::Counter { unless_payment: Some(_) }` (Censor,
        // Mana Leak, Disrupt, Spell Shrivel, etc.).
        "\"unless_payment\":",
    ];
    if json_has_any(ast_json, markers) {
        return;
    }
    push_warning(format!(
        "Swallow:Condition_Unless — \"unless\" not captured as unless_*: {}",
        truncate(original, 140)
    ));
}

// ── Detector I: Condition_AsLongAs ──────────────────────────────────────

/// CR 611.3: "as long as [X]" — duration tied to a condition (typically a
/// static ability with a `condition` field).
fn detect_condition_as_long_as(
    cleaned: &str,
    original: &str,
    ast_json: &str,
    parsed: &ParsedAbilities,
) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("as long as ") {
        return;
    }
    let markers: &[&str] = &[
        "\"condition\":{",
        "\"AsLongAs\"",
        "AsLongAs",
        "ConditionalStatic",
        // CR 611.3a: A `Duration::UntilHostLeavesPlay` IS the "as long as
        // you control this creature" / "as long as ~ remains on the
        // battlefield" gate (Aegis Angel, Hostage Taker, Gonti, etc.).
        // The duration's lifetime equates to a perpetual conditional
        // static on the host's controllership.
        "UntilHostLeavesPlay",
    ];
    if json_has_any(ast_json, markers) {
        return;
    }
    if any_static_has_per_object_as_long_as_gate(parsed) {
        return;
    }
    push_warning(format!(
        "Swallow:Condition_AsLongAs — \"as long as\" not captured as conditional static: {}",
        truncate(original, 140)
    ));
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
                    | crate::types::ability::FilterProp::WithKeyword { .. }
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
fn detect_duration_this_turn(cleaned: &str, original: &str, ast_json: &str) {
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
    ];
    // Only exempt when EVERY occurrence of "this turn" is part of a quantity
    // context. Counting occurrences ensures we still fire on cards that have
    // BOTH a quantity-context phrase AND a real duration (the duration could
    // be the swallow). The marker check below handles the all-captured case.
    let total_this_turn = cleaned.matches(" this turn").count();
    let quantity_this_turn: usize = QUANTITY_CONTEXT_SUFFIXES
        .iter()
        .map(|s| cleaned.matches(s).count())
        .sum();
    if total_this_turn > 0 && total_this_turn == quantity_this_turn {
        return;
    }
    let markers: &[&str] = &[
        "\"duration\":\"",
        "UntilEndOfTurn",
        "ThisTurn",
        "EndOfTurn",
        "EndOfCombat",
        // CR 514.2: AddTargetReplacement carries `expires_at_eot: true`,
        // which IS the EOT duration encoded structurally on the
        // ReplacementDefinition rather than via `def.duration`.
        "\"expires_at_eot\":true",
        // CR 614.6: `DamageDone` replacement events scope to a single
        // resolution (one-shot prevention/redirection); the "this turn"
        // wording is implicit in the spell-level replacement lifetime,
        // not a separate `duration` slot.
        "\"event\":\"DamageDone\"",
        "AddTargetReplacement",
        // CR 603.7c: A `CreateDelayedTrigger` with `WhenNextEvent` condition
        // IS the "next [event] this turn" delayed-trigger scope (Chandra,
        // the Firebrand's [-2], Doublecast-class copy-on-next-cast). The
        // "this turn" scope is implicit in the delayed-trigger semantics —
        // delayed triggers created by spells expire at end of turn per CR.
        "CreateDelayedTrigger",
        "WhenNextEvent",
        // CR 514.2 + CR 601.2f: `ReduceNextSpellCost` is a one-shot cost
        // reduction consumed by the next-cast spell — its "this turn"
        // scope is structural, not a `duration` slot.
        "ReduceNextSpellCost",
    ];
    if json_has_any(ast_json, markers) {
        return;
    }
    push_warning(format!(
        "Swallow:Duration_ThisTurn — \"this turn\" not captured as duration: {}",
        truncate(original, 140)
    ));
}

// ── Detector K: Duration_NextTurn ───────────────────────────────────────

/// CR 611.2a: "until your next turn" — extended-duration scope.
fn detect_duration_next_turn(cleaned: &str, original: &str, ast_json: &str) {
    // allow-noncombinator: swallow detector marker scan on classified text
    if !cleaned.contains("until your next turn")
        // allow-noncombinator: swallow detector marker scan on classified text
        && !cleaned.contains("until that player's next turn")
    {
        return;
    }
    let markers: &[&str] = &["YourNextTurn", "NextTurn", "UntilYourNextTurn"];
    if json_has_any(ast_json, markers) {
        return;
    }
    push_warning(format!(
        "Swallow:Duration_NextTurn — \"until your next turn\" not captured as duration: {}",
        truncate(original, 140)
    ));
}

// ── Detector L: Optional_MayHave ────────────────────────────────────────

/// CR 608.2d: "have it [verb]" / "may have [it]" — causative optional from
/// "any opponent may [verb], [if they do] have it [verb]" patterns.
/// Distinct from the simple `you may` optional flag.
fn detect_optional_may_have(cleaned: &str, original: &str, ast_json: &str) {
    let has_marker = cleaned.contains("may have ") || cleaned.contains("you may have "); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    // The "have causative" parser produces effects that recursively contain
    // optional sub-abilities. Conservative check: if the AST contains any
    // optional flag OR explicit causative marker, treat as captured.
    let markers: &[&str] = &[
        "\"optional\":true",
        "Causative",
        "HaveCausative",
        "HaveItVerb",
        // CR 614.1a: "you may have this creature enter as a copy ..." — the
        // optional choice is captured on the replacement's `mode` field
        // (ReplacementMode::Optional), not via `def.optional`.
        "\"mode\":{\"type\":\"Optional\"",
        // CR 702.20a: "you may have this creature assign its combat damage
        // as though it weren't blocked" — captured as a continuous
        // modification on a static, with the optionality implicit in the
        // modification's per-combat-step player decision.
        "AssignDamageAsThoughUnblocked",
    ];
    if json_has_any(ast_json, markers) {
        return;
    }
    push_warning(format!(
        "Swallow:Optional_MayHave — \"may have\" causative not captured: {}",
        truncate(original, 140)
    ));
}

// ── Detector M: APNAP ───────────────────────────────────────────────────

/// CR 101.4: "starting with you" / "in turn order" — APNAP (active
/// player → non-active player) iteration order. Must produce an explicit
/// ordering marker on the parsed ability so multiplayer resolution honors
/// the ordering rather than defaulting to engine-internal player order.
fn detect_apnap(cleaned: &str, original: &str, ast_json: &str) {
    let has_marker = cleaned.contains("starting with you") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("starting with the active player") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("starting with that player") // allow-noncombinator: swallow detector marker scan on classified text
        || cleaned.contains("in turn order"); // allow-noncombinator: swallow detector marker scan on classified text
    if !has_marker {
        return;
    }
    let markers: &[&str] = &[
        "StartingWith",
        "TurnOrder",
        "Apnap",
        "APNAP",
        "starting_with",
        "in_turn_order",
        "\"player_scope\":",
    ];
    if json_has_any(ast_json, markers) {
        return;
    }
    push_warning(format!(
        "Swallow:APNAP — turn-order iteration not captured as ordering metadata: {}",
        truncate(original, 140)
    ));
}

// ── Cascade-vs-AST structural diff (option 3) ──────────────────────────
//
// Complementary to the oracle-text-scanning detectors above. Where those
// detect *parser gaps* ("the cascade had no stripper for this phrase"),
// the structural diff detects *parser bugs* ("the cascade variable was
// set, but def-assembly dropped it").
//
// Hooked into `parse_effect_chain_impl` at the end of each chunk
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
pub(crate) fn check_cascade_diff(snap: &CascadeSnapshot<'_>, defs: &[AbilityDefinition]) {
    let Some(def) = defs.first() else {
        // Empty current_defs is itself a swallow but the iteration would
        // have produced an Unimplemented up-stack; nothing to compare.
        return;
    };

    if snap.is_optional && !def.optional {
        push_warning(format!(
            "Swallow:CascadeOptional — cascade is_optional=true, def.optional=false (effect={})",
            effect_name(&def.effect)
        ));
    }

    if snap.opponent_may_scope.is_some() && def.optional_for.is_none() {
        push_warning(format!(
            "Swallow:CascadeOpponentMay — cascade captured opponent_may_scope, def.optional_for=None (effect={})",
            effect_name(&def.effect)
        ));
    }

    if snap.condition.is_some() && def.condition.is_none() {
        push_warning(format!(
            "Swallow:CascadeCondition — cascade captured condition, def.condition=None (effect={})",
            effect_name(&def.effect)
        ));
    }

    if snap.repeat_for.is_some() && def.repeat_for.is_none() {
        // CR 609.3: "for each X" / "twice" repeat counts are sometimes
        // pushed onto a sub_ability instead of the def itself for
        // TargetOnly wrappers (line ~6411). Walk the sub_ability chain
        // before declaring loss.
        if !def_tree_has_repeat_for(def) {
            push_warning(format!(
                "Swallow:CascadeRepeat — cascade captured repeat_for, no def in tree carries it (effect={})",
                effect_name(&def.effect)
            ));
        }
    }

    if snap.player_scope.is_some() && def.player_scope.is_none() {
        push_warning(format!(
            "Swallow:CascadePlayerScope — cascade captured player_scope, def.player_scope=None (effect={})",
            effect_name(&def.effect)
        ));
    }

    if snap.clause_duration.is_some()
        && def.duration.is_none()
        && !effect_carries_duration(&def.effect)
    {
        push_warning(format!(
            "Swallow:CascadeDuration — clause.duration was Some, def.duration=None and effect carries no embedded duration (effect={})",
            effect_name(&def.effect)
        ));
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
