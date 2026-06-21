use std::str::FromStr;

use crate::parser::oracle_nom::error::{oracle_err, OracleError, OracleResult};
use nom::branch::alt;
use nom::bytes::complete::{tag, take_until};
use nom::character::complete::{char, multispace1};
use nom::combinator::{all_consuming, eof, opt, peek, rest, value};
use nom::multi::separated_list1;
use nom::sequence::{pair, preceded, terminated};
use nom::Parser;

use super::oracle_effect::become_copy_except::parse_except_clause;
use super::oracle_effect::{
    parse_effect_chain, parse_effect_chain_with_context, try_parse_named_choice,
};
use super::oracle_ir::context::ParseContext;
use super::oracle_ir::replacement::ReplacementIr;
use super::oracle_nom::bridge::{nom_on_lower, split_once_on_lower};
use super::oracle_nom::condition::{parse_attached_subject_target_filter, parse_inner_condition};
use super::oracle_nom::duration::parse_duration;
use super::oracle_nom::primitives as nom_primitives;
use super::oracle_nom::quantity as nom_quantity;
use super::oracle_nom::target::parse_type_filter_word;
use super::oracle_quantity::capitalize_first;
use super::oracle_target::parse_type_phrase;
use super::oracle_util::{
    normalize_card_name_refs, parse_count_expr, parse_number, parse_ordinal, strip_after,
    strip_reminder_text, TextPair,
};
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CastVariantPaid, ChoiceType, CombatDamageScope,
    Comparator, ContinuousModification, ControllerRef, CopyManaValueLimit, DamageModification,
    DamageRedirectTarget, DamageTargetFilter, DamageTargetPlayerScope, Duration, Effect,
    EffectScope, FilterProp, LibraryPosition, ManaModification, ManaReplacementScope, PlayerFilter,
    PreventionAmount, QuantityExpr, QuantityModification, QuantityRef, ReplacementCondition,
    ReplacementDefinition, ReplacementMode, ReplacementPlayerScope, StaticCondition,
    TapStateChange, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::card_type::Supertype;
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::mana::{ManaColor, ManaCost, ManaType};
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
    let ir = parse_replacement_line_ir(text, card_name)?;
    Some(lower_replacement_ir(&ir))
}

/// IR production: parse a replacement line into `ReplacementIr` (pre-lowering).
pub(crate) fn parse_replacement_line_ir(text: &str, card_name: &str) -> Option<ReplacementIr> {
    let mut definition = parse_replacement_line_inner(text, card_name)?;
    if definition.condition.is_none() {
        if let Some(condition) = parse_replacement_ability_word_condition(text) {
            definition = definition.condition(condition);
        }
    }
    Some(ReplacementIr {
        definition,
        source_text: text.to_string(),
        execute_ir: None,
    })
}

/// Lowering: produce the final `ReplacementDefinition` from IR.
///
/// Currently identity — replacement definitions are fully assembled during parsing.
pub(crate) fn lower_replacement_ir(ir: &ReplacementIr) -> ReplacementDefinition {
    ir.definition.clone()
}

/// Internal dispatch body for replacement line parsing.
fn parse_replacement_line_inner(text: &str, card_name: &str) -> Option<ReplacementDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();
    let normalized = replace_self_refs(&text, card_name);
    let norm_lower = normalized.to_lowercase();

    // --- Krark's Thumb: "If you would flip a coin, instead flip two coins and
    //     ignore one." (CR 705.1 + CR 614.1a) ---
    // Checked early so the generic "instead" / event-substitution handlers below
    // don't mis-claim the line.
    if let Some(def) = parse_krark_coin_flip_replacement(&text, &lower) {
        return Some(def);
    }

    // --- "As ~ enters, choose a [type]" → Moved replacement with persisted Choose ---
    // Must be checked BEFORE shock lands, which may contain this as a sub-pattern.
    if let Some(def) = parse_as_enters_choose(&norm_lower, &text) {
        return Some(def);
    }

    // --- "As ~ is turned face up, [effect]" → TurnFaceUp replacement (megamorph/
    //     disguise). CR 614.1e + CR 708.11: the effect applies as the permanent is
    //     turned face up, so it is a replacement, not a stack triggered ability. ---
    if let Some(def) = parse_turned_face_up_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- The Mimeoplasm: "As ~ enters, you may exile N cards from graveyards. If you do, ..." ---
    // Check before other "as enters" patterns to ensure it matches correctly
    if let Some(def) = parse_as_enters_exile_from_graveyards(&norm_lower, &normalized, &text) {
        return Some(def);
    }

    // --- "~ enters prepared." ---
    // CR 722.3a: "enters prepared" gives the entering permanent the prepared
    // designation as part of the entry event, not through a triggered ability.
    if let Some(def) = parse_enters_prepared(&norm_lower, &text) {
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

    // --- "If you control N or more [type], ~ enters tapped" (positive-count conditional) ---
    // CR 614.1d: Creature lands (Lair of the Hydra, Hall of Storm Giants) and similar.
    // Must be checked BEFORE the unconditional "enters tapped" guard below.
    if let Some(def) = parse_enters_tapped_if_controls(&norm_lower, &text) {
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

    // --- "[Type] enter tapped" / "[Type] played by your opponents enter tapped" ---
    if let Some(def) = parse_external_enters_tapped(&norm_lower, &text) {
        return Some(def);
    }

    // --- "~ enters under the control of an opponent of your choice." ---
    // CR 110.2a: A self-ETB controller-override replacement — the permanent
    // enters the battlefield directly under an opponent's control (Xantcha,
    // Sleeper Agent; Captive Audience; Pendant of Prosperity; Abby, Merciless
    // Soldier). Checked before the generic enters-tapped guard so the "enters"
    // verb isn't claimed by another arm.
    if let Some(def) = parse_self_enters_under_opponent(&norm_lower, &text) {
        return Some(def);
    }

    // --- "~ enters the battlefield tapped" (unconditional) ---
    // Guard: reject text with " unless " or "if you control" — all conditional
    // patterns must be handled above. Counter-bearing variants fall through to
    // `parse_enters_with_counters`, which composes the tap and counter modifiers.
    if (nom_primitives::scan_contains(&norm_lower, "enters the battlefield tapped")
        || nom_primitives::scan_contains(&norm_lower, "enters tapped"))
        && !nom_primitives::scan_contains(&norm_lower, "unless")
        && !nom_primitives::scan_contains(&norm_lower, "if you control")
        && !has_enters_tapped_with_counter(&norm_lower)
    {
        return Some(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                        state: TapStateChange::Tap,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                // CR 614.1c: as-enters defs are battlefield-ENTRY-scoped — the
                // destination gate stops them matching this permanent's own
                // battlefield DEPARTURE (SBA death / bounce / destroy).
                .destination_zone(Zone::Battlefield)
                .description(text.to_string()),
        );
    }

    // --- "If a card/token would be put into a graveyard, exile it instead" ---
    if let Some(def) = parse_graveyard_exile_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- Library of Leng: "If an effect causes you to discard a card, discard it,
    // but you may put it on top of your library instead of into your graveyard." ---
    if let Some(def) = parse_discard_to_library_top_replacement(&norm_lower, &normalized, &text) {
        return Some(def);
    }

    // --- "If an opponent causes you to discard this card, put it onto the battlefield instead" ---
    if let Some(def) =
        parse_discard_self_to_battlefield_replacement(&norm_lower, &normalized, &text)
    {
        return Some(def);
    }

    // --- Karoo self-ETB cost lands: "If this land would enter, sacrifice ...
    //     instead. If you do, ... If you don't, put it into its owner's graveyard." ---
    if let Some(def) = parse_self_enters_pay_cost_replacement(&norm_lower, &normalized, &text) {
        return Some(def);
    }

    // --- "If enchanted land would be destroyed, instead {effect}" ---
    if let Some(def) =
        parse_enchanted_land_destroy_sacrifice_replacement(&norm_lower, &normalized, &text)
    {
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
    if let Some(def) = parse_creature_die_exile_replacement(&norm_lower, &normalized) {
        return Some(def);
    }

    // CR 614.1a + CR 120.6: Wolverine, Fierce Fighter — heal-prior-damage
    // replacement ("instead that damage is dealt, but all other damage already
    // dealt to him is healed"). The new damage IS dealt (no prevention); only
    // prior marked damage clears. Tried before the generic self-instead
    // followup, whose "<effect> instead" shape this mid-clause "instead" does
    // not fit.
    if let Some(def) = parse_damage_heal_self_replacement(&norm_lower, &text) {
        return Some(def);
    }

    // --- "Prevent all/the next N damage" patterns (CR 615) ---
    if let Some(def) = parse_damage_to_player_instead_followup(&norm_lower, &text) {
        return Some(def);
    }
    if let Some(def) = parse_damage_to_self_instead_followup(&norm_lower, &normalized, &text) {
        return Some(def);
    }
    if let Some(def) = parse_damage_prevention_replacement(&norm_lower, &text) {
        return Some(def);
    }
    // "damage can't be prevented" is handled by effect parsing (Effect::AddRestriction),
    // not replacement parsing. See oracle_effect.rs damage prevention disabled handler.

    if let Some(def) = parse_conditional_draw_replacement(&text, &lower) {
        return Some(def);
    }

    if let Some(def) = parse_scry_count_replacement(&lower, &text) {
        return Some(def);
    }

    if let Some(def) = parse_mill_count_replacement(&norm_lower, &text) {
        return Some(def);
    }

    if let Some(def) = parse_proliferate_count_replacement(&lower, &text) {
        return Some(def);
    }

    // --- "If [player] would proliferate, {effect}" ---
    // CR 701.34a + CR 614.1a: Generic proliferate replacement (Tekuthal class).
    if nom_primitives::scan_contains(&lower, "would proliferate") {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::Proliferate).description(text.to_string());
        {
            let e = effect_text?;
            let (optional_modal_present, effect_after_modal) = strip_optional_instead_lead_in(&e);
            if optional_modal_present {
                def = def.mode(ReplacementMode::Optional { decline: None });
            }
            def = def.execute(parse_effect_chain(effect_after_modal, AbilityKind::Spell));
        }
        apply_proliferate_player_scope(&lower, &mut def);
        return Some(def);
    }

    // --- Explore replacement: "If a creature you control would explore, instead …"
    // (Twists and Turns / Topography Tracker class).
    if nom_primitives::scan_contains(&lower, "would explore") {
        if let Some(def) = parse_explore_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // --- Untap-step replacement: "If [filter] would untap during [its
    // controller's | your] untap step, [effect] instead" (Freyalise's Winds,
    // Edge of Malacol). CR 502.3 + CR 502.4 + CR 614.1a.
    if nom_primitives::scan_contains(&lower, "would untap during") {
        if let Some(def) = parse_untap_step_replacement(&text, &lower) {
            return Some(def);
        }
    }

    // --- "If [player] would draw [a card | one or more cards], {effect}" ---
    // CR 614.1a: Widened from "you would draw" to handle opponent/player
    // scope (Notion Thief, Hullbreacher, Chains of Mephistopheles) mirroring
    // the gain-life widening below.
    let mentions_draw = nom_primitives::scan_at_word_boundaries(&lower, |i| {
        value(
            (),
            alt((
                tag::<_, _, OracleError<'_>>("would draw a card"),
                tag("would draw one or more cards"),
            )),
        )
        .parse(i)
    })
    .is_some();
    if mentions_draw {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::Draw).description(text.to_string());
        if let Some(e) = effect_text {
            // CR 614.1a + CR 614.6 + CR 121.6: "you may instead {effect}" makes
            // the draw replacement optional. The player is offered an
            // accept/decline prompt; on decline, the original draw event
            // proceeds unmodified (CR 614.6: only the accept branch replaces
            // the event), so `decline: None` is correct — no synthetic
            // draw-on-decline ability (which would double-draw on accept and
            // shadow the engine's native draw on decline). Strip the lead-in
            // before handing the remainder to `parse_effect_chain`.
            let (optional_modal_present, effect_after_modal) = strip_optional_instead_lead_in(&e);
            if optional_modal_present {
                def = def.mode(ReplacementMode::Optional { decline: None });
            }
            def = def.execute(parse_effect_chain(effect_after_modal, AbilityKind::Spell));
        }
        // CR 614.1a: Player scope for draw replacements.
        apply_draw_player_scope(&lower, &mut def);
        // CR 121.1 + CR 504.1 + CR 614.6: Detect Alhammarret's Archive's
        // "except the first one [you|they] draw in each of [your|their] draw
        // steps" exception clause and gate the replacement so it does NOT
        // apply to the draw step's mandatory first draw.
        if has_except_first_draw_in_draw_step_clause(&lower) {
            def = def.condition(ReplacementCondition::ExceptFirstDrawInDrawStep);
        } else {
            // CR 614.11 + CR 614.1a: "...while your library has no cards in
            // it..." antecedent — gate the replacement so a win-on-draw
            // (Laboratory Maniac, Jace, Wielder of Mysteries) fires only on an
            // empty-library draw. CR 614.11: draw replacements apply even when
            // the library is empty, which is precisely the case this gate
            // selects. Without the gate the WinTheGame post-effect replaces
            // *every* draw, which both wins spuriously and leaks an un-drained
            // post-replacement continuation into later turns.
            match parse_while_antecedent(&lower, "would draw a card") {
                WhileAntecedent::Parsed(condition) => def = def.condition(condition),
                // Guard present but unparseable: fail closed. Emitting an
                // unconditional Draw replacement would fire the (often
                // game-ending) effect on every draw — the exact regression
                // this discipline exists to prevent.
                WhileAntecedent::Unparsed => return None,
                WhileAntecedent::Absent => {}
            }
        }
        return Some(def);
    }

    // --- "If [player] would gain life, {effect}" ---
    // CR 614.1a: Widened from "you would gain life" to handle opponent/player
    // scope. The entry gate is a nom `alt` over the two life-gain phrasings:
    // the direct "would gain life" and the periphrastic "would cause its
    // controller to gain life" (Rain of Gore), which has no contiguous "would
    // gain life" substring and would otherwise skip this branch entirely.
    let mentions_gain_life = nom_primitives::scan_at_word_boundaries(&lower, |i| {
        value(
            (),
            alt((
                tag::<_, _, OracleError<'_>>("would gain life"),
                tag("would cause its controller to gain life"),
            )),
        )
        .parse(i)
    })
    .is_some();
    if mentions_gain_life {
        let effect_text = extract_replacement_effect(&normalized);
        let mut def =
            ReplacementDefinition::new(ReplacementEvent::GainLife).description(text.to_string());
        // CR 119.10 + CR 614.6: "If [a player] would gain life, [that player]
        // gains no life instead." — the lifegain-negation replacement. The
        // body lowers to a bare "gain no life" which `parse_effect_chain` would
        // turn into an `Unimplemented` no-op effect (a silent runtime
        // passthrough). Instead, emit the structured `Prevent` quantity
        // modification, which `gain_life_applier` Branch 1 reads to fully
        // suppress the gain (CR 614.6: a replaced event never happens). Mirrors
        // `parse_global_player_counter_prohibition`: a `Prevent` replacement
        // carries no `execute` effect, so no stray `Unimplemented` pollutes the
        // AST. Scoped to the un-durationed form — the "...would gain life THIS
        // TURN..." durational replacement (Flames of the Blood Hand, CR 611.2a:
        // a resolving spell's continuous effect lasts only as long as stated) is
        // deferred, since a flat `Prevent` would wrongly become permanent.
        let body_negates_lifegain = effect_text
            .as_deref()
            .is_some_and(|e| body_is_lifegain_negation(&e.to_lowercase()));
        if body_negates_lifegain
            && !nom_primitives::scan_contains(&lower, "would gain life this turn")
        {
            def = def.quantity_modification(QuantityModification::Prevent);
            // Apply player scope before short-circuiting (shared with the
            // execute path below): "a player" / opponent / controller scoping.
            apply_gain_life_player_scope(&lower, &mut def);
            return Some(def);
        }
        if let Some(e) = effect_text {
            def = def.execute(parse_effect_chain(&e, AbilityKind::Spell));
        }
        // CR 614.1a: Parse the subject to determine player scope.
        apply_gain_life_player_scope(&lower, &mut def);
        // CR 614.1a: A "while [condition]" gate in the antecedent suppresses the
        // replacement when the condition is false. Phial of Galadriel ("If you
        // would gain life while you have 5 or less life, you gain twice that
        // much life instead") uses this shape — without the gate, the doubler
        // fires unconditionally. Reuses the `parse_inner_condition` building
        // block and the `ReplacementCondition::OnlyIfQuantity` typed surface.
        match parse_while_antecedent(&lower, "would gain life") {
            WhileAntecedent::Parsed(condition) => def = def.condition(condition),
            // Guard present but unparseable: fail closed rather than emit an
            // unconditional life-gain doubler.
            WhileAntecedent::Unparsed => return None,
            WhileAntecedent::Absent => {}
        }
        return Some(def);
    }

    // --- "If [someone] would lose life, they lose twice that much life instead" ---
    if let Some(def) = parse_lose_life_replacement(&text, &lower) {
        return Some(def);
    }

    // --- "Double all damage that [subject] would deal" (without "instead") ---
    // CR 614.1: Static damage modification abilities like Collective Inferno
    // are continuous replacement effects even though they do not use "instead".
    // Must be checked BEFORE the "instead" guard to avoid falling through to stub.
    if nom_primitives::scan_contains(&lower, "would deal")
        && nom_primitives::scan_contains(&lower, "damage")
        && !nom_primitives::scan_contains(&lower, "instead")
        && nom_primitives::scan_contains(&lower, "double")
    {
        if let Some(def) = parse_damage_modification_static(&norm_lower, &text) {
            return Some(def);
        }
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
    // kicker-conditional "if was kicked, it enters with" (CR 702.33d). The
    // bare-verb plural-subject forms ("Other creatures you control enter with …"
    // — Gev, Scaled Scorch) use "enter"/"escape" rather than the singular
    // "enters"/"escapes", so accept both at word boundaries.
    if (nom_primitives::scan_contains(&lower, "enters")
        || nom_primitives::scan_contains(&lower, "escapes")
        || nom_primitives::scan_contains(&lower, "enter with")
        || nom_primitives::scan_contains(&lower, "escape with"))
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
        if let Some(def) = parse_generic_additional_token_replacement(&lower, &text) {
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

    // --- Copy-count replacement: "If you would copy a spell one or more times,
    //     instead copy it that many times plus an additional time." (Twinning
    //     Staff) ---
    // CR 707.10 + CR 614.1a: A replacement effect that increases the number of
    // copies a copy-a-spell effect produces, modeled as a `CopySpell`
    // replacement carrying a `QuantityModification` — the same shape as the
    // token / counter doubling family above.
    if let Some(def) = parse_copy_count_replacement(&lower, &text) {
        return Some(def);
    }

    // --- Counter addition replacement: "if one or more ... counters would be put on..." ---

    if let Some(def) = parse_energy_get_replacement(&lower, &text) {
        return Some(def);
    }

    if nom_primitives::scan_contains(&lower, "counters would be put on")
        || nom_primitives::scan_contains(&lower, "counter would be put on")
        || nom_primitives::scan_contains(&lower, "would put one or more counters")
        || nom_primitives::scan_contains(&lower, "would put a counter")
    {
        if let Some(def) = parse_counter_replacement(&lower, &text) {
            return Some(def);
        }
    }

    // --- Global counter-prohibition replacements: Solemnity class ---
    if let Some(def) = parse_global_player_counter_prohibition(&lower, &text) {
        return Some(def);
    }
    if let Some(def) = parse_global_object_counter_prohibition(&lower, &text) {
        return Some(def);
    }
    if let Some(def) = parse_inverted_typed_counter_prohibition(&lower, &text) {
        return Some(def);
    }

    // --- Counter-prohibition replacement: "~ can't have counters put on it." ---
    // CR 614.6 + CR 614.7 + CR 122.1: A self-targeted counter-placement
    // prohibition. The proposed `AddCounter` event never happens
    // (CR 614.6 — "if an event is replaced, it never happens"). Melira's
    // Keepers class.
    if let Some(def) = parse_no_counters_replacement(&norm_lower, &text) {
        return Some(def);
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

    // --- Life-floor damage replacement: "if you control a [filter], damage that would
    // reduce your life total to less than N reduces it to N instead" ---
    // CR 614.1a: Worship-class replacement effect.
    if let Some(def) = parse_life_floor_damage_replacement(&norm_lower) {
        return Some(def);
    }

    // --- Unconditional life-floor: "damage that would reduce your life total to
    // less than N reduces it to N instead" (Ali from Cairo, Fortune Thief,
    // Sustaining Spirit). CR 614.1a. Tried after the conditional Worship arm,
    // which claims the "if you control …" prefix. ---
    if let Some(def) = parse_unconditional_life_floor_damage_replacement(&norm_lower) {
        return Some(def);
    }

    None
}

/// CR 614.1a + CR 614.6: Library of Leng — when an effect causes the controller
/// to discard, they may put the discarded card on top of their library instead
/// of into their graveyard.
fn parse_discard_to_library_top_replacement(
    norm_lower: &str,
    normalized: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let ((), after_prefix) = nom_on_lower(normalized, norm_lower, |i| {
        value(
            (),
            tag("if an effect causes you to discard a card, discard it, but you may "),
        )
        .parse(i)
    })?;
    let after_lower = after_prefix.to_lowercase();
    if all_consuming(terminated(
        pair(
            tag::<_, _, OracleError<'_>>("put it on top of your "),
            tag::<_, _, OracleError<'_>>("library"),
        ),
        pair(
            tag::<_, _, OracleError<'_>>(" instead of into your graveyard"),
            opt(tag::<_, _, OracleError<'_>>(".")),
        ),
    ))
    .parse(after_lower.as_str())
    .is_err()
    {
        return None;
    }
    let execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutAtLibraryPosition {
            target: TargetFilter::ParentTarget,
            count: QuantityExpr::Fixed { value: 1 },
            position: LibraryPosition::Top,
        },
    );
    Some(
        ReplacementDefinition::new(ReplacementEvent::Discard)
            .mode(ReplacementMode::Optional { decline: None })
            .execute(execute)
            .valid_card(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ))
            .condition(ReplacementCondition::EffectCausedDiscard)
            .description(original_text.to_string()),
    )
}

fn parse_discard_self_to_battlefield_replacement(
    norm_lower: &str,
    normalized: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let ((), after_prefix) = nom_on_lower(normalized, norm_lower, |i| {
        value(
            (),
            tag("if a spell or ability an opponent controls causes you to discard this card, "),
        )
        .parse(i)
    })?;
    let after_prefix_lower = after_prefix.to_lowercase();
    let (effect_text, tail) = split_once_on_lower(
        after_prefix,
        &after_prefix_lower,
        " instead of putting it into your graveyard",
    )?;
    if !tail.trim_end_matches('.').trim().is_empty() {
        return None;
    }
    let execute = parse_effect_chain(effect_text, AbilityKind::Spell);
    Some(
        ReplacementDefinition::new(ReplacementEvent::Discard)
            .execute(execute)
            .valid_card(TargetFilter::SelfRef)
            .condition(ReplacementCondition::EventSourceControlledBy {
                controller: ControllerRef::Opponent,
            })
            .description(original_text.to_string()),
    )
}

/// CR 614.1a + CR 614.12 + CR 614.12a: Karoo-style self-ETB cost replacement.
///
/// Recognizes "If this {land|artifact} would enter, {sacrifice <filter> | you
/// may discard <filter>} instead. If you do, put this {land|artifact} onto the
/// battlefield. If you don't, put it into its owner's graveyard." — the 8-card
/// Karoo class (Lotus Vale, Scorched Ruins, Mox Diamond, etc.).
///
/// Emits a `ReplacementMode::MayCost` on the `Moved` event: the accept-cost is
/// the parsed `AbilityCost::Sacrifice`/`Discard`; the decline branch redirects
/// the ETB destination to the owner's graveyard via `Effect::ChangeZone` so the
/// permanent never appears on the battlefield (CR 614 — no ETB/LTB triggers).
fn parse_self_enters_pay_cost_replacement(
    norm_lower: &str,
    normalized: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Prefix: "if {this land|this artifact|~} would enter, ".
    let ((), after_prefix) = nom_on_lower(normalized, norm_lower, |i| {
        value(
            (),
            preceded(
                tag("if "),
                alt((
                    tag("this land would enter, "),
                    tag("this artifact would enter, "),
                    tag("~ would enter, "),
                )),
            ),
        )
        .parse(i)
    })?;

    // Isolate the cost body from the boilerplate tail at " instead. ".
    let after_prefix_lower = after_prefix.to_lowercase();
    let (cost_body, tail) = split_once_on_lower(after_prefix, &after_prefix_lower, " instead. ")?;

    // Cost: strip an optional non-cost "you may " lead-in (Mox Diamond), then
    // delegate the verb-inclusive residue to `parse_single_cost` — it consumes
    // the "sacrifice "/"discard " verb itself.
    let cost_body = cost_body.trim();
    let cost_body_lower = cost_body.to_lowercase();
    let cost_text = nom_on_lower(cost_body, &cost_body_lower, |i| {
        value((), tag("you may ")).parse(i)
    })
    .map_or(cost_body, |((), rest)| rest);
    let cost = crate::parser::oracle_cost::parse_single_cost(cost_text);
    // Guard: only Sacrifice / Discard are valid Karoo accept-costs.
    if !matches!(
        cost,
        AbilityCost::Sacrifice(_) | AbilityCost::Discard { .. }
    ) {
        return None;
    }

    // Tail boilerplate must match fully (guards against false positives).
    let tail_lower = tail.to_lowercase();
    let ((), tail_rest) = nom_on_lower(tail, &tail_lower, |i| {
        value(
            (),
            (
                tag("if you do, put "),
                alt((tag("this land"), tag("this artifact"), tag("~"), tag("it"))),
                tag(" onto the battlefield. if you don't, put it into its owner's graveyard"),
                opt(char('.')),
            ),
        )
        .parse(i)
    })?;
    if !tail_rest.trim().is_empty() {
        return None;
    }

    // CR 614.1 + CR 614.12: the decline branch redirects the ETB destination to
    // the owner's graveyard so the permanent never enters the battlefield (no
    // ETB/LTB triggers fire). Routed through the engine's existing zone-redirect
    // path via `Effect::ChangeZone`.
    let decline = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Graveyard,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            face_down_profile: None,
        },
    );

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .mode(ReplacementMode::MayCost {
                cost,
                decline: Some(Box::new(decline)),
            })
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

/// CR 614.1a + CR 614.12: The Mimeoplasm — "As ~ enters, you may exile N cards
/// from graveyards. If you do, it enters as a copy of one of those cards with a
/// number of additional +1/+1 counters on it equal to the power of the other card."
///
/// Emits a `ReplacementMode::MayCost` on the `Moved` event: the accept-cost is
/// the parsed `AbilityCost::Exile` from graveyards; the "If you do" continuation
/// is the copy + counter placement effect chain. No decline branch — the permanent
/// enters normally (no exile, no copy, no counters) if declined.
fn parse_as_enters_exile_from_graveyards(
    norm_lower: &str,
    normalized: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Prefix: "as ~ enters, you may exile "
    let ((), after_prefix) = nom_on_lower(normalized, norm_lower, |i| {
        value(
            (),
            preceded(
                tag("as "),
                alt((
                    tag("~ enters, you may exile "),
                    tag("this creature enters, you may exile "),
                )),
            ),
        )
        .parse(i)
    })?;

    // Isolate the cost body from the "If you do" continuation
    let after_prefix_lower = after_prefix.to_lowercase();
    let (cost_body, _tail) =
        split_once_on_lower(after_prefix, &after_prefix_lower, ". if you do, ")?;

    // Parse the exile cost manually to handle "from graveyards" (plural)
    // Pattern: "[count] [type] card(s) from graveyards"
    let cost_body_lower = cost_body.trim().to_lowercase();
    let (count, filter_text) =
        parse_number(&cost_body_lower).unwrap_or((1, cost_body_lower.trim()));

    // Strip the "from graveyards" suffix to extract the type filter.
    // filter_text is already lowercase (slice of cost_body_lower).
    // Use take_until + alt to consume up to and including the zone suffix.
    let parsed: nom::IResult<&str, (&str, &str)> = pair(
        take_until(" from graveyard"),
        alt((tag(" from graveyards"), tag(" from graveyard"))),
    )
    .parse(filter_text);
    let Ok(("", (filter_text, _))) = parsed else {
        return None;
    };

    // Parse the type filter (e.g., "creature")
    let (filter, remainder) = parse_type_phrase(filter_text.trim());
    if !remainder.trim().is_empty() {
        return None;
    }

    let cost = AbilityCost::Exile {
        count,
        zone: Some(Zone::Graveyard),
        filter: Some(filter),
    };

    // CR 607.2a: Manually construct the continuation for Mimeoplasm-style effects.
    // The continuation text "it enters as a copy of one of those cards, except it has
    // the other card's power and toughness as +1/+1 counters" must be lowered to:
    // - BecomeCopy targeting the first exiled card (ExiledCardByIndex { index: 0 })
    // - PutCounter with count = second exiled card's power (ExiledCardPower { index: 1 })
    // This cannot use parse_effect_chain because the generic parser lowers this
    // pattern to CopySpell (which copies spells on the stack, not exiled cards).
    let continuation = crate::types::ability::AbilityDefinition::new(
        crate::types::ability::AbilityKind::Spell,
        crate::types::ability::Effect::BecomeCopy {
            target: crate::types::ability::TargetFilter::ExiledCardByIndex { index: 0 },
            duration: None,
            mana_value_limit: None,
            additional_modifications: vec![],
        },
    )
    .sub_ability(crate::types::ability::AbilityDefinition::new(
        crate::types::ability::AbilityKind::Spell,
        crate::types::ability::Effect::PutCounter {
            counter_type: crate::types::counter::CounterType::Plus1Plus1,
            count: crate::types::ability::QuantityExpr::Ref {
                qty: crate::types::ability::QuantityRef::ExiledCardPower { index: 1 },
            },
            target: crate::types::ability::TargetFilter::SelfRef,
        },
    ));

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .mode(ReplacementMode::MayCost {
                cost,
                decline: None, // No decline branch — enters normally if declined
            })
            .execute(continuation)
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

/// Case-insensitive replacement of card name and self-referencing phrases with "~".
fn replace_self_refs(text: &str, card_name: &str) -> String {
    normalize_card_name_refs(text, card_name)
}

/// CR 614.1a: "instead" marks the enchanted-land destruction event as replaced
/// by the parsed sacrifice/grant effect chain.
fn parse_enchanted_land_destroy_sacrifice_replacement(
    norm_lower: &str,
    normalized: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let ((), rest) = nom_on_lower(normalized, norm_lower, |i| {
        let (i, _) = tag("if ").parse(i)?;
        let (i, _) = tag("enchanted land").parse(i)?;
        let (i, _) = tag(" would be destroyed, ").parse(i)?;
        let (i, _) = tag("instead ").parse(i)?;
        Ok((i, ()))
    })?;
    let effect_text = rest.trim_end_matches('.');
    if effect_text.is_empty() {
        return None;
    }
    let mut execute = parse_effect_chain(effect_text, AbilityKind::Spell);
    bind_enchanted_land_grant_to_replaced_object(&mut execute);

    Some(
        ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::AttachedTo)
            .execute(execute)
            .description(original_text.to_string()),
    )
}

fn bind_enchanted_land_grant_to_replaced_object(def: &mut AbilityDefinition) {
    // CR 614.1a + CR 608.2c: in "If enchanted land would be destroyed, instead
    // sacrifice ~ and that land gains ...", "that land" refers to the object
    // whose destruction is being replaced, not to every land.
    if let Effect::GenericEffect {
        static_abilities,
        target,
        ..
    } = &mut *def.effect
    {
        let mut binds_replaced_land = false;
        for static_ability in static_abilities {
            if matches!(
                static_ability.affected.as_ref(),
                Some(TargetFilter::Typed(filter))
                    if filter.type_filters == [TypeFilter::Land]
            ) {
                static_ability.affected = Some(TargetFilter::ParentTarget);
                binds_replaced_land = true;
            }
        }
        if binds_replaced_land {
            *target = None;
        }
    }

    if let Some(sub_ability) = def.sub_ability.as_mut() {
        bind_enchanted_land_grant_to_replaced_object(sub_ability);
    }
}

/// CR 705.1 + CR 614.1a: Krark's Thumb — "If you would flip a coin, instead flip
/// two coins and ignore one."
///
/// Emits a controller-scoped `CoinFlip` replacement whose `execute` doubles the
/// flip count (`Multiply { factor: 2, EventContextAmount }`). The runtime applier
/// reads this to set the doubled count; the resolver then performs the keep-1
/// choice. No `valid_card` filter — the replacement is objectless (it watches the
/// controller's flips, not a permanent moving), so it must not be skipped by an
/// object-filter mismatch.
fn parse_krark_coin_flip_replacement(text: &str, lower: &str) -> Option<ReplacementDefinition> {
    let ((), rest) = nom_on_lower(text, lower, |i| {
        let (i, _) = tag("if you would flip a coin, instead flip ").parse(i)?;
        let (i, _) = alt((tag("two coins"), tag("2 coins"))).parse(i)?;
        let (i, _) = tag(" and ignore ").parse(i)?;
        let (i, _) = alt((tag("one"), tag("1"))).parse(i)?;
        let (i, _) = opt(char('.')).parse(i)?;
        Ok((i, ()))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let mut def = ReplacementDefinition::new(ReplacementEvent::CoinFlip)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            // CR 614.1a: "instead flip two coins" — double the count the
            // replacement applier sees, then ignore all but one (CR 705.1).
            Effect::FlipCoins {
                count: QuantityExpr::Multiply {
                    factor: 2,
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    }),
                },
                win_effect: None,
                lose_effect: None,
                // CR 614.1a + CR 705.2: the replacement re-flips for the same
                // flipper the original event named (the replacement applier rebinds
                // the acting controller), so `Controller` reads that flipper.
                flipper: crate::types::ability::TargetFilter::Controller,
            },
        ))
        .description(text.to_string());
    // CR 614.1a: "If you would flip a coin" — controller-scoped.
    def.valid_player = Some(ReplacementPlayerScope::You);
    Some(def)
}

/// CR 614.1a + CR 119.3: Lose-life replacement effects.
///
/// Handles Bloodletter-style doublers and preserves generic "If you would lose
/// life, instead ..." replacement recognition without substring dispatch.
fn parse_lose_life_replacement(text: &str, lower: &str) -> Option<ReplacementDefinition> {
    let ((scope, quantity_modification), rest) = nom_on_lower(text, lower, |i| {
        let (i, _) = tag("if ").parse(i)?;
        let (i, scope) = parse_lose_life_subject(i)?;
        let (i, _) = tag(" would lose life").parse(i)?;
        let (i, _) = opt(preceded(tag(" "), tag("during your turn"))).parse(i)?;
        let (i, _) = tag(", ").parse(i)?;
        let (i, quantity_modification) = alt((
            value(
                Some(QuantityModification::Double),
                terminated(parse_double_lose_life_consequence, opt(char('.'))),
            ),
            value(None, parse_lose_life_instead_consequence),
        ))
        .parse(i)?;
        Ok((i, (scope, quantity_modification)))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let mut def =
        ReplacementDefinition::new(ReplacementEvent::LoseLife).description(text.to_string());
    if let Some(scope) = scope {
        def.valid_player = Some(scope);
    }
    if let Some(quantity_modification) = quantity_modification {
        def = def.quantity_modification(quantity_modification);
    }
    Some(def)
}

fn parse_lose_life_subject(input: &str) -> OracleResult<'_, Option<ReplacementPlayerScope>> {
    alt((
        value(
            Some(ReplacementPlayerScope::Opponent),
            alt((tag("an opponent"), tag("opponent"))),
        ),
        value(None, tag("you")),
    ))
    .parse(input)
}

fn parse_double_lose_life_consequence(input: &str) -> OracleResult<'_, ()> {
    value(
        (),
        (
            alt((tag("they "), tag("that opponent "), tag("you "))),
            alt((tag("lose "), tag("loses "))),
            tag("twice that much life instead"),
        ),
    )
    .parse(input)
}

fn parse_lose_life_instead_consequence(input: &str) -> OracleResult<'_, ()> {
    let (remaining, body) = preceded(tag("instead "), rest).parse(input)?;
    if body.trim().is_empty() {
        return Err(oracle_err(body));
    }
    Ok((remaining, ()))
}

fn parse_enters_prepared(norm_lower: &str, text: &str) -> Option<ReplacementDefinition> {
    let mut parser = value(
        (),
        all_consuming(preceded(
            alt((
                tag::<_, _, OracleError<'_>>("~"),
                tag("this creature"),
                tag("this permanent"),
                tag("it"),
            )),
            (tag(" enters prepared"), opt(tag("."))),
        )),
    );
    parser.parse(norm_lower.trim()).ok()?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomePrepared {
                    target: TargetFilter::SelfRef,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
            .description(text.to_string()),
    )
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
            take_until::<_, _, OracleError<'_>>(" card from your hand"),
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
            // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
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
            take_until::<_, _, OracleError<'_>>(" card this way or you control "),
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
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
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
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    )
    .condition(crate::types::ability::AbilityCondition::Not {
        condition: Box::new(
            crate::types::ability::AbilityCondition::ControllerControlsMatching {
                filter: bound_filter,
            },
        ),
    })
}

/// Parse shock land pattern: "As ~ enters, you may pay N life. If you don't, it enters tapped."
/// Returns a cost-bearing replacement choice: paying life accepts; declining taps.
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

    let tap_self = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
    );

    let has_basic_land_type_choice =
        nom_primitives::scan_contains(norm_lower, "choose a basic land type");
    let execute = has_basic_land_type_choice.then(|| {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: true,
                selection: crate::types::ability::TargetSelectionMode::Chosen,
            },
        )
    });

    let decline = if has_basic_land_type_choice {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: true,
                selection: crate::types::ability::TargetSelectionMode::Chosen,
            },
        )
        .sub_ability(tap_self)
    } else {
        tap_self
    };

    Some(
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::Moved);
            if let Some(execute) = execute {
                def = def.execute(execute);
            }
            def
        }
        .mode(ReplacementMode::MayCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: amount },
            },
            decline: Some(Box::new(decline)),
        })
        .valid_card(TargetFilter::SelfRef)
        // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
        .destination_zone(Zone::Battlefield)
        .description(original_text.to_string()),
    )
}

/// Parse "As ~ enters, choose a [type]" into a Moved replacement with persisted Choose.
/// Skips lines that also contain shock land markers (handled by parse_shock_land).
fn parse_as_enters_choose(norm_lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    let has_phrase = |phrase: &'static str| {
        nom_primitives::scan_at_word_boundaries(norm_lower, |input| {
            tag::<_, _, OracleError<'_>>(phrase).parse(input)
        })
        .is_some()
    };

    // Must have "as" + "enters" framing
    if !has_phrase("as ") || !has_phrase("enters") {
        return None;
    }

    // Don't match shock lands — they have their own handler
    if has_phrase("you may pay") && has_phrase("life") {
        return None;
    }

    // Extract the "choose a ..." clause — scan_split_at_phrase returns (prefix, rest_starting_at_match)
    let (_, choose_text) = nom_primitives::scan_split_at_phrase(norm_lower, |i| {
        tag::<_, _, OracleError<'_>>("choose ").parse(i)
    })?;
    let choice_type = try_parse_named_choice(choose_text)?;

    let choose = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Choose {
            choice_type,
            persist: true,
            selection: crate::types::ability::TargetSelectionMode::Chosen,
        },
    );

    // CR 614.1c + CR 614.1d: The Thriving land cycle ("This land enters tapped.
    // As it enters, choose a color other than <C>.") layers TWO replacement
    // effects on the same entry event — the enters-tapped modifier AND the
    // choice. This handler is dispatched BEFORE the unconditional enters-tapped
    // guard and returns early, so without composing here the tap is silently
    // dropped (issue #1581). Compose them into one Moved replacement:
    // `Tap { SelfRef }` (the enter_tapped event-modifier) followed by the
    // `Choose` as post-replacement "real work" — exactly the shape the engine
    // already resolves for Vesuva's "enter tapped as a copy"
    // (`Tap { SelfRef }` -> `sub_ability(BecomeCopy)`). The modifier must come
    // first so `EventModifiers` accumulates the tap before reaching the choice.
    let enters_tapped = (has_phrase("enters tapped")
        || has_phrase("enters the battlefield tapped"))
        && !has_phrase("unless")
        && !has_phrase("if you control");

    let execute = if enters_tapped {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .sub_ability(choose)
    } else {
        choose
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(execute)
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

/// CR 110.2a + CR 614.1d: "`<this permanent>` enters under the control of an
/// opponent of your choice." — a self-ETB controller-override replacement.
///
/// The permanent enters the battlefield directly under an opponent's control;
/// it never enters under its owner's control first (CR 110.2a). Cards: Xantcha,
/// Sleeper Agent; Captive Audience; Pendant of Prosperity; Abby, Merciless
/// Soldier. Emitted as a `Moved` self-replacement (`valid_card = SelfRef`,
/// `destination_zone = Battlefield`) carrying `enters_under = Opponent`; the
/// engine resolves the opponent and stamps the entering `ZoneChange`'s
/// `controller_override` before ETB triggers fire (see
/// `resolve_self_enters_under_controller`).
///
/// "Of your choice" is the controller's choice of opponent — deterministic in a
/// two-player game (the sole opponent); a full multiplayer choice is a follow-up.
fn parse_self_enters_under_opponent(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // The full, highly specific control clause (with or without "the battlefield").
    let has_clause = nom_primitives::scan_contains(
        norm_lower,
        "enters under the control of an opponent of your choice",
    ) || nom_primitives::scan_contains(
        norm_lower,
        "enters the battlefield under the control of an opponent of your choice",
    );
    if !has_clause {
        return None;
    }

    // Self-subject gate: the subject of "enters" must be this permanent — the
    // normalized self-name "~" (legendary short names included), or a "this
    // <card-type>" / bare "this" demonstrative — never an external filter
    // ("creatures you control enter ...").
    let is_self_subject = nom_primitives::scan_contains(norm_lower, "~ enters")
        || nom_primitives::scan_contains(norm_lower, "this artifact enters")
        || nom_primitives::scan_contains(norm_lower, "this creature enters")
        || nom_primitives::scan_contains(norm_lower, "this enchantment enters")
        || nom_primitives::scan_contains(norm_lower, "this planeswalker enters")
        || nom_primitives::scan_contains(norm_lower, "this land enters")
        || nom_primitives::scan_contains(norm_lower, "this battle enters")
        || nom_primitives::scan_contains(norm_lower, "this permanent enters")
        || nom_primitives::scan_contains(norm_lower, "this enters");
    if !is_self_subject {
        return None;
    }

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1d: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
            // CR 110.2a: enters under an opponent's control (resolved at apply time).
            .enters_under(ControllerRef::Opponent)
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
    let (before_copy, after_copy, enter_tapped) = find_copy_verb(norm_lower)?;

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
    let type_text = alt((tag::<_, _, OracleError<'_>>("any "), tag("a "), tag("an ")))
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

    // CR 614.1c: When the verb phrase includes "tapped" ("enter tapped as a copy
    // of"), compose a Tap modifier as the top-level execute with BecomeCopy as its
    // sub_ability. The replacement pipeline walks the chain: event_modifiers_for_ability
    // extracts EtbTapState::Tapped from Tap, then first_non_modifier_ability finds
    // BecomeCopy for the post-replacement CopyTargetChoice dispatch.
    let execute_effect = if enter_tapped {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .sub_ability(copy_effect)
        .description(original_text.to_string())
    } else {
        copy_effect
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(execute_effect)
            .mode(ReplacementMode::Optional { decline: None })
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1c: battlefield-entry-scoped — without the gate this
            // Optional clone def would force an "enter as a copy?" prompt on the
            // permanent's own DEATH.
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

/// Locate the clone-verb phrase in a normalised Oracle line and return
/// `(before_verb, after_verb, enter_tapped)` around it.
///
/// Recognises both grammatical framings of the ETB-copy replacement class:
/// - `"enter as a copy of "` (Phantasmal Image / Phyrexian Metamorph / …)
/// - `"enter tapped as a copy of "` (Vesuva / Callidus Assassin / Echoing Deeps)
/// - `"become a copy of "` (Cursed Mirror / future ETB-copy prints using
///   the "as this enters, …, become a copy of" shape)
///
/// The verbs are leaf alternatives with no shared prefix, so each is scanned
/// independently and the earliest match wins — this mirrors the earliest-match
/// discipline used by `split_on_clone_source_zone` / `split_on_first_of`.
fn find_copy_verb(norm_lower: &str) -> Option<(&str, &str, bool)> {
    let candidates: &[(&str, bool)] = &[
        ("enter tapped as a copy of ", true),
        ("enter as a copy of ", false),
        ("become a copy of ", false),
    ];
    let mut best: Option<(usize, usize, bool)> = None;
    for &(phrase, tapped) in candidates {
        if let Some((before, _)) = nom_primitives::scan_split_at_phrase(norm_lower, |i| {
            tag::<_, _, OracleError<'_>>(phrase).parse(i)
        }) {
            let pos = before.len();
            if best.is_none_or(|(bp, _, _)| pos < bp) {
                best = Some((pos, phrase.len(), tapped));
            }
        }
    }
    let (pos, len, tapped) = best?;
    Some((&norm_lower[..pos], &norm_lower[pos + len..], tapped))
}

/// CR 707.9 / CR 614.1c: whether `lower` contains a copy replacement verb
/// ("enter as a copy of", "become a copy of", "enter tapped as a copy of").
/// Used by the Priority 7 dispatcher to gate the copy-replacement first-pass so
/// static / prevent lines never mis-route into the replacement parsers.
///
/// Intentionally takes UN-normalized lowercase: the copy verbs never contain the
/// card name, so `~`-normalization is irrelevant to this check.
pub(crate) fn find_copy_verb_present(lower: &str) -> bool {
    find_copy_verb(lower).is_some()
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
    // CR 614.1c: no zone phrase and no "except" clause — the whole post-`copy
    // of` remainder is the type phrase. Drop the sentence-final period so the
    // downstream `parse_type_phrase` leftover guard accepts plain
    // controller-scoped filters like "a creature you control" (Mirror Image)
    // or "an artifact or creature you control" (Waxen Shapethief), which carry
    // no zone/except boundary to absorb the trailing punctuation.
    Some((after_copy.trim_end_matches('.'), "", Zone::Battlefield))
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
        value((), tag::<_, _, OracleError<'_>>("when you do")).parse(i)
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
        parse_except_clause(remaining, card_name, &ParseContext::default())
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
    let (rest, _) = tag::<_, _, OracleError<'_>>(
        "with mana value less than or equal to the amount of mana spent to cast ",
    )
    .parse(suffix)
    .ok()?;
    // Self-reference: the normalizer rewrites the card name to "~" but
    // Oracle text commonly also uses "this creature" verbatim.
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("this creature"), tag("~")))
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
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string())
            .condition(condition),
    )
}

/// Parse conditional "enters tapped if you control N or more [type]" patterns (CR 614.1d).
///
/// Covers creature-land "enters tapped" ETBs that are gated on controlling a minimum
/// number of matching permanents. The positive "if you control" form is semantically
/// distinct from the "unless" form: the replacement APPLIES when the condition is met
/// (controller has enough lands), rather than being SUPPRESSED.
///
/// Recognized patterns:
/// - "If you control two or more other lands, this land enters tapped."
///   (Lair of the Hydra, Hall of Storm Giants, Celestial Colonnade, etc.)
/// - "If you control N or more [type phrase], ~ enters tapped."
///   (General class: any "if you control N or more … enters tapped" form.)
fn parse_enters_tapped_if_controls(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !nom_primitives::scan_contains(norm_lower, "enters tapped")
        && !nom_primitives::scan_contains(norm_lower, "enters the battlefield tapped")
    {
        return None;
    }

    let condition = parse_if_controls_count_condition(norm_lower)?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            // CR 614.1c: battlefield-entry-scoped (see destination-gate note above).
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string())
            .condition(condition),
    )
}

/// Extract "if you control N or more [type phrase]" condition (CR 614.1d).
///
/// The "if you control" prefix is the positive form: the replacement APPLIES
/// when the controller has at least `minimum` matching permanents. Source
/// exclusion is filter-driven: "other" injects `FilterProp::Another`, while
/// forms without "other" count the source if it matches.
fn parse_if_controls_count_condition(norm_lower: &str) -> Option<ReplacementCondition> {
    // CR 614.1d: "if you control N or more [type]" — extract the minimum count
    // and the type phrase that follows.
    let (rest, _) = tag::<_, _, OracleError<'_>>("if you control ")
        .parse(norm_lower)
        .ok()?;
    let (minimum, type_text) = try_parse_quantity_prefix(rest)?;

    let (filter, leftover) = parse_type_phrase(type_text);
    // Allow trailing clause like ", this land enters tapped." — strip up to the comma.
    let leftover = leftover.trim().trim_start_matches(',').trim();
    if !leftover.trim_end_matches('.').is_empty()
        && !nom_primitives::scan_contains(leftover, "enters tapped")
        && !nom_primitives::scan_contains(leftover, "enters the battlefield tapped")
    {
        return None;
    }
    if filter == TargetFilter::Any {
        return None;
    }

    // Inject ControllerRef::You — "you control" is implicit in the Oracle text.
    let filter = inject_controller(filter, ControllerRef::You);

    Some(ReplacementCondition::IfControlsMatching { minimum, filter })
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
    let (after_or_fewer, _) = tag::<_, _, OracleError<'_>>("or fewer ")
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
    let (type_text, _) = tag::<_, _, OracleError<'_>>("or more ")
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

/// Scope of a distributive ETB-with-counters subject (CR 614.12). `Other`
/// excludes the source (`FilterProp::Another`); `Distributive` is a general
/// subset that includes the source if it matches the type filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubjectScope {
    /// "each other [type] ..." / "other [type] ..." — excludes the source.
    Other,
    /// "each [type] ..." — general subset including the source per CR 614.12.
    Distributive,
}

/// Strip a distributive subject prefix from an ETB-with-counters line, reporting
/// whether the source is excluded (`Other`) or included (`Distributive`).
///
/// CR 614.12: a replacement that modifies how a permanent enters may affect
/// "only that permanent" or "a general subset of permanents that includes it".
/// The "each other "/"other " forms exclude the source; the bare "each " form
/// is a general subset that includes it. Returns `None` for self-ETB lines
/// ("~ enters with ..."), which fall through to `SelfRef`.
///
/// The `"each other "` alternative must precede `"each "` so the longer match
/// wins; `alt` is order-sensitive and `"each "` would otherwise shadow it.
fn parse_distributive_subject(work_text: &str) -> Option<(&str, SubjectScope)> {
    alt((
        value(
            SubjectScope::Other,
            alt((tag::<_, _, OracleError<'_>>("each other "), tag("other "))),
        ),
        value(SubjectScope::Distributive, tag("each ")),
    ))
    .parse(work_text)
    .ok()
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
/// and `DivideRounded { inner: Variable("X"), .. }` also get the rewrite.
pub(crate) fn rewrite_variable_x_to_cost_x_paid(expr: &mut QuantityExpr) {
    match expr {
        QuantityExpr::Ref { qty } => {
            if matches!(qty, QuantityRef::Variable { name } if name == "X") {
                *qty = QuantityRef::CostXPaid;
            }
        }
        QuantityExpr::Fixed { .. } => {}
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => rewrite_variable_x_to_cost_x_paid(inner),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for inner in exprs {
                rewrite_variable_x_to_cost_x_paid(inner);
            }
        }
        QuantityExpr::UpTo { max } => rewrite_variable_x_to_cost_x_paid(max),
        QuantityExpr::Power { exponent, .. } => rewrite_variable_x_to_cost_x_paid(exponent),
        QuantityExpr::Difference { left, right } => {
            rewrite_variable_x_to_cost_x_paid(left);
            rewrite_variable_x_to_cost_x_paid(right);
        }
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

    // CR 702.138c: "escapes with" / plural-subject "escape with" is
    // semantically "enters with" gated on escape.
    let is_escape = nom_primitives::scan_contains(work_text, "escapes with")
        || nom_primitives::scan_contains(work_text, "escape with");

    // Find "with [N] [type] counter" to extract count and counter type.
    // For escape, the "with" follows "escapes"; for enters, it follows "enters".
    let after_with = strip_after(work_text, "with ")?;
    // Skip "an additional" if present
    let after_additional = alt((
        tag::<_, _, OracleError<'_>>("an additional "),
        tag("additional "),
    ))
    .parse(after_with)
    .map_or(after_with, |(rest, _)| rest);

    // CR 614.12a + CR 608.2d: "~ enters with your choice of <counter-choice-list>
    // on it" — the controller chooses WHICH counter as the permanent enters, and
    // that choice is made before the permanent enters (CR 614.12a). Detect the
    // "your choice of " marker, split off the trailing self-referential target
    // ("on it" / "on ~"), classify the disjunctive list into typed counter
    // entries, and build a `ChooseOneOf` of `PutCounter { target: SelfRef }`
    // branches directly (no parent-target lift — the entering permanent is
    // always the recipient). Runtime folds the chosen counter pre-entry via the
    // deferred-entry-events capture in `engine_replacement.rs` /
    // `engine_resolution_choices.rs`.
    if let Some((choices, _on)) = strip_enters_with_choice_target(after_additional) {
        if let Some(entries) =
            crate::parser::oracle_effect::classify_and_parse_counter_choice_list(choices)
        {
            // `classify_and_parse_counter_choice_list` already requires len >= 2.
            let branches: Vec<AbilityDefinition> = entries
                .into_iter()
                .map(|(counter_type, count)| {
                    let mut def = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutCounter {
                            counter_type: counter_type.clone(),
                            count,
                            // CR 614.12a: the entering permanent is the recipient.
                            target: TargetFilter::SelfRef,
                        },
                    );
                    def.description = Some(format!("a {} counter", counter_type.display_phrase()));
                    def
                })
                .collect();

            let choice = AbilityDefinition::new(
                AbilityKind::Spell,
                // CR 608.2d: resolution choice — controller picks the branch.
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches,
                },
            );
            let mut choice = choice;
            choice.description = Some("your choice of counter".to_string());

            // Compose with "enters tapped" if present (mirrors the single-counter
            // tail below).
            let execute = if has_enters_tapped_phrase(work_text) {
                AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                        state: TapStateChange::Tap,
                    },
                )
                .sub_ability(choice)
            } else {
                choice
            };

            // CR 614.1c: "enters with" is a replacement effect on the Moved event,
            // battlefield-entry-scoped (see destination-gate note above).
            let mut def = ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(execute)
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .description(original_text.to_string());

            // Reuse the existing condition tail (escape / kicker / cast-from-zone
            // / raid / web-slinging / generic only-if).
            if is_escape {
                def = def.condition(ReplacementCondition::CastViaEscape);
            } else if let Some(cond) = kicker_condition {
                def = def.condition(cond);
            } else if let Some(zone) = extract_cast_from_zone_suffix(work_text) {
                def = def.condition(ReplacementCondition::CastFromZone { zone });
            } else if extract_you_attacked_this_turn_suffix(work_text) {
                def = def.condition(ReplacementCondition::YouAttackedThisTurn);
            } else if extract_cast_using_web_slinging_suffix(work_text) {
                def = def.condition(ReplacementCondition::CastVariantPaid {
                    variant: CastVariantPaid::WebSlinging,
                });
            } else if let Some(condition) = extract_enters_with_only_if_suffix(work_text) {
                def = def.condition(condition);
            }

            return Some(def);
        }
    }

    let counter_entries = parse_enters_counter_entries(after_additional);
    // Detect dynamic count: "a number of [type] counters ... equal to [qty]"
    let after_prefix = tag::<_, _, OracleError<'_>>("a number of ")
        .parse(after_additional)
        .map_or(after_additional, |(rest, _)| rest);
    // CR 107.3 + CR 107.3m + CR 107.1a: Parse the counter count as a full
    // `QuantityExpr`, so "N", "X", "twice X", "three times X", and
    // "half X, rounded up/down" all compose through the same typed arithmetic
    // wrappers (`Multiply`, `DivideRounded`). `parse_count_expr` returns
    // `Variable("X")` for bare X; the ETB-enters context requires the entering
    // object's `cost_x_paid` (runtime `Variable("X")` only reads trigger-event
    // sources, not the entering permanent), so rewrite X → `CostXPaid`
    // recursively inside the expression.
    let (mut count_expr, rest) =
        parse_count_expr(after_prefix).unwrap_or((QuantityExpr::Fixed { value: 1 }, after_prefix));
    rewrite_variable_x_to_cost_x_paid(&mut count_expr);
    // Next word(s) before "counter" are the counter type
    let (_, (counter_type_raw, after_counter)) =
        nom_primitives::split_once_on(rest, "counter").ok()?;
    let counter_type_raw = counter_type_raw.trim();
    let counter_type =
        crate::parser::oracle_effect::counter::normalize_counter_type(counter_type_raw);
    if let Some(for_each_count) = parse_enters_counter_for_each_suffix(after_counter) {
        count_expr = multiply_counter_count_by_for_each(count_expr, for_each_count);
    }
    // CR 122.6: For "a number of counters equal to [quantity]" and the
    // sibling shorthand "counters on it equal to [quantity]", parse the
    // dynamic expression.
    if let Ok((_, (_, qty_text))) = nom_primitives::split_once_on(work_text, "equal to ") {
        // The quantity phrase never spans a sentence boundary, so isolate the
        // first sentence before parsing — Slumbering Trudge trails a separate
        // "If X is 2 or less, it enters tapped." clause after "equal to three
        // minus X.", which would otherwise leave the quantity parsers a dangling
        // tail and force the `Fixed { 1 }` fallback (only 1 stun counter).
        let trimmed = qty_text.split('.').next().unwrap_or(qty_text).trim();
        if let Some(qty_ref) = crate::parser::oracle_quantity::parse_quantity_ref(trimmed) {
            count_expr = QuantityExpr::Ref { qty: qty_ref };
        } else if let Some(qty) = crate::parser::oracle_quantity::parse_cda_quantity(trimmed) {
            count_expr = qty;
        } else if let Some(qty) =
            crate::parser::oracle_quantity::parse_event_context_quantity(trimmed)
        {
            count_expr = qty;
        } else if let Some((qty, rest_q)) = crate::parser::oracle_util::parse_count_expr(trimmed) {
            // CR 107.3a: arithmetic over the cost variable ("three minus X").
            // `parse_count_expr` emits `Variable("X")`; the rewrite below maps it
            // to the entering object's `CostXPaid`. Require full consumption so a
            // partial parse never silently truncates the quantity.
            if rest_q.trim().is_empty() {
                count_expr = qty;
            }
        }
    }
    if let Some(qty) = parse_enters_with_where_x_suffix(work_text) {
        count_expr = qty;
    } else if nom_primitives::split_once_on(work_text, ", where x is ").is_ok() {
        return None;
    }
    // CR 614.12: Any `Variable("X")` that survived the dynamic-quantity
    // overrides above refers to the X paid on the *entering* object's cost, not
    // a trigger-event source, so rewrite it to `CostXPaid` (idempotent —
    // already-rewritten `CostXPaid` leaves are untouched).
    rewrite_variable_x_to_cost_x_paid(&mut count_expr);

    let put_counter = build_enters_counter_ability(
        counter_entries.unwrap_or_else(|| vec![(counter_type, count_expr)]),
    );
    let execute = if has_enters_tapped_phrase(work_text) {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            },
        )
        .sub_ability(put_counter)
    } else {
        put_counter
    };

    // Determine valid_card filter: self vs a general subset of permanents.
    // CR 614.1c: Effects that read "[permanent] enters with ..." are
    // replacement effects. CR 614.12 distinguishes effects that affect "only
    // that permanent" (self-ETB → SelfRef) from those affecting "a general
    // subset of permanents that includes it" (distributive → typed filter).
    //
    // Two distributive shapes exist:
    //   - "each other [type] you control enters with ..." (Giada) — explicitly
    //     EXCLUDES the source, so `FilterProp::Another` must be injected.
    //   - "each [type] you control enters with ..." (Dragonstorm Globe) — the
    //     general subset INCLUDES the source if it matches the type; per
    //     CR 614.12 the subset "includes it", so NO `Another` is injected. (The
    //     artifact source simply doesn't match a Dragon type filter, so no
    //     self-application occurs here — but the class must not exclude itself.)
    //
    // `parse_distributive_subject` strips the prefix and reports the scope, then
    // `parse_type_phrase` acts as the type detector: accept the subject iff the
    // parse yields a typed filter with a concrete type/subtype (not the `Any`
    // fallback). A non-type subject parses to the `[Any]` fallback and is
    // rejected, falling through to the `SelfRef` self-ETB branch.
    let subject = parse_distributive_subject(work_text).and_then(|(subject_text, scope)| {
        let (filter, _) = parse_type_phrase(subject_text);
        let is_valid = matches!(
            &filter,
            TargetFilter::Typed(TypedFilter { type_filters, .. })
                if !type_filters.is_empty()
                    && type_filters.as_slice() != [TypeFilter::Any]
        );
        is_valid.then_some((filter, scope))
    });
    let valid_card = if let Some((filter, scope)) = subject {
        // CR 614.12: only the "other" scope excludes the source from the subset.
        let filter = match (filter, scope) {
            (
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    mut properties,
                }),
                SubjectScope::Other,
            ) => {
                properties.insert(0, FilterProp::Another);
                TargetFilter::Typed(TypedFilter {
                    type_filters,
                    controller,
                    properties,
                })
            }
            (other, _) => other,
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
        .execute(execute)
        .description(original_text.to_string())
        // CR 614.1c: "enters with" defs are battlefield-entry-scoped for BOTH
        // branches — the external ChangeZone variant always needed the gate, and
        // the self-ETB Moved variant needs it so the def does not match this
        // permanent's own battlefield DEPARTURE (SBA death / bounce / destroy).
        .destination_zone(Zone::Battlefield);
    if let Some(filter) = valid_card {
        def = def.valid_card(filter);
    }

    // Apply condition: escape, kicker, or cast-from-zone suffix.
    // CR 603.4: Myojin-class "enters with [counter] on it if you cast it
    // from your hand" — trailing zone gate on a self-ETB replacement.
    if is_escape {
        def = def.condition(ReplacementCondition::CastViaEscape);
    } else if let Some(cond) = kicker_condition {
        def = def.condition(cond);
    } else if let Some(zone) = extract_cast_from_zone_suffix(work_text) {
        def = def.condition(ReplacementCondition::CastFromZone { zone });
    } else if extract_you_attacked_this_turn_suffix(work_text) {
        // CR 207.2c (Raid): "Raid — ~ enters with [counter] on it if you
        // attacked this turn." (Cruel Administrator, Goblin Boarders, etc.)
        def = def.condition(ReplacementCondition::YouAttackedThisTurn);
    } else if extract_cast_using_web_slinging_suffix(work_text) {
        // CR 702.188a: "If ~ was cast using web-slinging, ..." (Scarlet Spider).
        def = def.condition(ReplacementCondition::CastVariantPaid {
            variant: CastVariantPaid::WebSlinging,
        });
    } else if let Some(condition) = extract_enters_with_only_if_suffix(work_text) {
        // CR 614.1c + CR 700.4: Generic suffix gates for ETB-counter
        // replacements, e.g. Morbid's "if a creature died this turn".
        def = def.condition(condition);
    }

    Some(def)
}

fn has_enters_tapped_with_counter(text: &str) -> bool {
    has_enters_tapped_phrase(text)
        && preceded(
            take_until::<_, _, OracleError<'_>>("counter"),
            tag::<_, _, OracleError<'_>>("counter"),
        )
        .parse(text)
        .is_ok()
}

fn has_enters_tapped_phrase(text: &str) -> bool {
    alt((
        preceded(
            take_until::<_, _, OracleError<'_>>("enters the battlefield tapped"),
            tag::<_, _, OracleError<'_>>("enters the battlefield tapped"),
        ),
        preceded(
            take_until::<_, _, OracleError<'_>>("enters tapped"),
            tag::<_, _, OracleError<'_>>("enters tapped"),
        ),
    ))
    .parse(text)
    .is_ok()
}

fn parse_enters_with_where_x_suffix(text: &str) -> Option<QuantityExpr> {
    let (_, (_, qty_text)) = nom_primitives::split_once_on(text, ", where x is ").ok()?;
    let trimmed = qty_text.trim().trim_end_matches('.');
    if let Ok((_, qty_ref)) = nom_quantity::parse_quantity_ref_complete(trimmed) {
        return Some(QuantityExpr::Ref { qty: qty_ref });
    }
    if let Some(qty) = crate::parser::oracle_quantity::parse_cda_quantity(trimmed) {
        return Some(qty);
    }
    crate::parser::oracle_quantity::parse_event_context_quantity(trimmed)
}

fn multiply_counter_count_by_for_each(
    count_expr: QuantityExpr,
    for_each_count: QuantityExpr,
) -> QuantityExpr {
    match count_expr {
        QuantityExpr::Fixed { value: 1 } => for_each_count,
        QuantityExpr::Fixed { value } => QuantityExpr::Multiply {
            factor: value,
            inner: Box::new(for_each_count),
        },
        _ => for_each_count,
    }
}

fn extract_enters_with_only_if_suffix(text: &str) -> Option<ReplacementCondition> {
    let (_, (_, condition_text)) = nom_primitives::split_once_on(text, " if ").ok()?;
    let condition_text = condition_text.trim().trim_end_matches('.');
    let (rest, condition) = parse_inner_condition(condition_text).ok()?;
    rest.trim().is_empty().then_some(())?;
    replacement_condition_from_static(condition)
}

fn parse_enters_counter_for_each_suffix(after_counter: &str) -> Option<QuantityExpr> {
    let (rest, _) = opt(tag::<_, _, OracleError<'_>>("s"))
        .parse(after_counter)
        .ok()?;
    // CR 614.12: the self-referential recipient is "it" for a single permanent
    // and "them" for a distributive subject (e.g. Gev, Scaled Scorch's "Other
    // creatures you control enter with … counter on them for each …"). Both
    // forms precede the per-each scaling clause identically.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>(" on it for each "),
        tag(" on them for each "),
    ))
    .parse(rest)
    .ok()?;
    if let Ok((rest, qty)) = parse_for_each_convoked_creature_clause(rest) {
        if rest.trim().is_empty() {
            return Some(qty);
        }
    }
    let clause = match nom_primitives::split_once_on(rest, ".") {
        Ok((_, (before_period, after_period))) if after_period.trim().is_empty() => {
            before_period.trim()
        }
        _ => rest.trim(),
    };
    super::oracle_quantity::parse_for_each_clause_expr(clause)
}

fn parse_for_each_convoked_creature_clause(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, QuantityExpr> {
    let (rest, _) = pair(tag::<_, _, OracleError<'_>>("creature"), opt(tag("s"))).parse(input)?;
    let (rest, _) = tag(" ").parse(rest)?;
    let (rest, _) = tag("that convoked ").parse(rest)?;
    let (rest, _) = alt((
        tag("it"),
        tag("this spell"),
        tag("this permanent"),
        tag("~"),
    ))
    .parse(rest)?;
    let (rest, _) = opt(tag(".")).parse(rest)?;
    Ok((
        rest,
        QuantityExpr::Ref {
            qty: QuantityRef::ConvokedCreatureCount,
        },
    ))
}

fn parse_enters_counter_entries(after_with: &str) -> Option<Vec<(CounterType, QuantityExpr)>> {
    let mut remaining = after_with;
    let mut entries = Vec::new();

    loop {
        let (mut count_expr, rest) = parse_count_expr(remaining)?;
        rewrite_variable_x_to_cost_x_paid(&mut count_expr);

        let (at_counter, counter_type_raw) = take_until::<_, _, OracleError<'_>>(" counter")
            .parse(rest)
            .ok()?;
        if counter_type_raw.trim().is_empty() {
            return None;
        }
        let counter_type =
            crate::parser::oracle_effect::counter::normalize_counter_type(counter_type_raw);
        let (after_space, _) = tag::<_, _, OracleError<'_>>(" ").parse(at_counter).ok()?;
        let (after_counter_word, _) =
            alt((tag::<_, _, OracleError<'_>>("counters"), tag("counter")))
                .parse(after_space)
                .ok()?;

        entries.push((counter_type, count_expr));

        if let Some(next) = parse_enters_counter_separator(after_counter_word) {
            remaining = next;
            continue;
        }

        tag::<_, _, OracleError<'_>>(" on it")
            .parse(after_counter_word)
            .ok()?;
        break;
    }

    (entries.len() >= 2).then_some(entries)
}

fn parse_enters_counter_separator(input: &str) -> Option<&str> {
    let (after_sep, _) = alt((
        tag::<_, _, OracleError<'_>>(", and "),
        tag(" and "),
        tag(", "),
    ))
    .parse(input)
    .ok()?;

    let (_, rest) = parse_count_expr(after_sep)?;
    let (at_counter, counter_type_raw) = take_until::<_, _, OracleError<'_>>(" counter")
        .parse(rest)
        .ok()?;
    if counter_type_raw.trim().is_empty() {
        return None;
    }
    let (after_space, _) = tag::<_, _, OracleError<'_>>(" ").parse(at_counter).ok()?;
    alt((tag::<_, _, OracleError<'_>>("counters"), tag("counter")))
        .parse(after_space)
        .ok()?;

    Some(after_sep)
}

/// CR 614.12a: For "your choice of <list> on it", split off the trailing
/// self-referential target. Given the text AFTER "your choice of " (e.g.
/// "a +1/+1, first strike, or vigilance counter on it."), return
/// `Some((choices, target))` where `choices` is the disjunctive counter list
/// ("a +1/+1, first strike, or vigilance counter") and `target` is the
/// self-reference ("it"). Returns `None` when the target is NOT a self-reference
/// (so external-recipient phrasings fall through to other parsers).
///
/// Nom-only: `take_until(" on ")` splits the list from the trailing " on
/// <target>", then the target (with trailing punctuation stripped) is validated
/// against the self/object pronoun set (`it` / `~`).
fn strip_enters_with_choice_target(after_choice: &str) -> Option<(&str, &str)> {
    // Detect the "your choice of " marker via nom (no string dispatch).
    let (after_marker, _) = tag::<_, _, OracleError<'_>>("your choice of ")
        .parse(after_choice)
        .ok()?;
    // Split list from trailing " on <target>".
    let (after_on, choices) = take_until::<_, _, OracleError<'_>>(" on ")
        .parse(after_marker)
        .ok()?;
    let (target, _) = tag::<_, _, OracleError<'_>>(" on ").parse(after_on).ok()?;
    let target_clean = target.trim().trim_end_matches('.').trim();
    // CR 614.12a: the recipient must be the entering permanent itself.
    if super::oracle_util::SELF_AND_OBJECT_PRONOUNS.contains(&target_clean) {
        Some((choices, target_clean))
    } else {
        None
    }
}

fn build_enters_counter_ability(entries: Vec<(CounterType, QuantityExpr)>) -> AbilityDefinition {
    let mut chain = entries
        .into_iter()
        .rev()
        .fold(None, |tail, (counter_type, count)| {
            let mut ability = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type,
                    count,
                    target: TargetFilter::SelfRef,
                },
            );
            ability.sub_ability = tail;
            Some(Box::new(ability))
        });

    *chain
        .take()
        .expect("enters counter ability requires at least one counter entry")
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("whenever you cast ")
        .parse(norm_lower)
        .ok()?;

    // Drop the article before the spell filter.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>("a "),
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
        tag::<_, _, OracleError<'_>>("creature "),
        tag("permanent "),
        tag("spell "),
    ))
    .parse(after_that_text)
    .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("enters with ")
        .parse(rest)
        .ok()?;

    // Count prefix: "an additional" | "N additional" | plain "N" | "x additional" | "x".
    // Mirrors `try_parse_enters_with_additional_counters` — the Wildgrowth
    // family always uses "additional" but the underlying shape matches.
    let (rest, fixed_count) =
        if let Ok((r, _)) = tag::<_, _, OracleError<'_>>("an additional ").parse(rest) {
            (r, Some(1u32))
        } else if let Ok((r, _)) = alt((
            tag::<_, _, OracleError<'_>>("x additional "),
            tag("X additional "),
        ))
        .parse(rest)
        {
            // X is dynamic — actual value comes from the trailing "where X is …" clause.
            (r, None)
        } else if let Ok((r, n)) = nom_primitives::parse_number(rest) {
            let (r, _) = tag::<_, _, OracleError<'_>>(" additional ")
                .parse(r)
                .or_else(|_| tag::<_, _, OracleError<'_>>(" ").parse(r))
                .ok()?;
            (r, Some(n))
        } else {
            return None;
        };

    // Counter type.
    let (rest, counter_type) = alt((
        value(
            CounterType::Plus1Plus1,
            tag::<_, _, OracleError<'_>>("+1/+1"),
        ),
        value(CounterType::Minus1Minus1, tag("-1/-1")),
    ))
    .parse(rest)
    .ok()?;

    // " counter on it" / " counters on it" with optional trailing punctuation.
    let (rest, _) = alt((
        tag::<_, _, OracleError<'_>>(" counter on it"),
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
                tag::<_, _, OracleError<'_>>(", where x is "),
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
    let after_if = match tag::<_, _, OracleError<'_>>("if ").parse(norm_lower) {
        Ok((rest, _)) => rest,
        Err(_) => return (None, norm_lower),
    };

    // Subject can be "~", "it", "this creature", etc. — scan to "was kicked".
    let after_kicked = match take_until::<_, _, OracleError<'_>>("was kicked")
        .parse(after_if)
        .and_then(|(rest, _)| tag::<_, _, OracleError<'_>>("was kicked").parse(rest))
    {
        Ok((rest, _)) => rest,
        Err(_) => return (None, norm_lower),
    };

    // Optional "with its {cost} kicker" variant specification
    let (cost_text, after_kicker_clause) =
        match tag::<_, _, OracleError<'_>>(" with its ").parse(after_kicked) {
            Ok((rest, _)) => {
                match take_until::<_, _, OracleError<'_>>(" kicker").parse(rest) {
                    Ok((rest2, cost_str)) => {
                        // Consume " kicker" tag
                        match tag::<_, _, OracleError<'_>>(" kicker").parse(rest2) {
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
        tag::<_, _, OracleError<'_>>(", it enters with"),
        tag(", it enters the battlefield with"),
    ))
    .parse(after_kicker_clause);

    match enters_result {
        Ok(_) => {
            // Reconstruct the enters-with text for downstream parsing.
            let enters_start = norm_lower.len() - after_kicker_clause.len() + 2; // skip ", "
            let condition = ReplacementCondition::CastViaKicker {
                variant: None,
                kicker_cost: cost_text.as_deref().and_then(parse_lower_mana_cost),
            };
            (Some(condition), &norm_lower[enters_start..])
        }
        Err(_) => (None, norm_lower),
    }
}

fn parse_lower_mana_cost(cost_text: &str) -> Option<ManaCost> {
    let upper = cost_text.to_ascii_uppercase();
    nom_primitives::parse_mana_cost
        .parse(upper.as_str())
        .ok()
        .map(|(_, cost)| cost)
}

/// CR 603.4: Detect a trailing "if you cast it from [zone]" gate on a
/// self-ETB replacement. Used by Myojin of Blooming Dawn / of Cryptic
/// Dreams / of Grim Betrayal / of Towering Might / of Roaring Blades —
/// "~ enters with an indestructible counter on it if you cast it from
/// your hand."
///
/// Composable: any zone that the runtime tracks via `cast_from_zone` can
/// be matched here. We currently parse `your hand`, `your graveyard`,
/// and `exile` since those are the textually attested forms.
fn extract_cast_from_zone_suffix(work_text: &str) -> Option<Zone> {
    use crate::parser::oracle_nom::error::OracleError;
    use nom::bytes::complete::tag;
    // Locate the suffix.
    let (rest, _) = take_until::<_, _, OracleError<'_>>("if you cast it from ")
        .parse(work_text)
        .ok()?;
    let (rest, _) = tag::<_, _, OracleError<'_>>("if you cast it from ")
        .parse(rest)
        .ok()?;
    // Match the zone tail.
    let zone = if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("your hand").parse(rest) {
        Zone::Hand
    } else if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("your graveyard").parse(rest) {
        Zone::Graveyard
    } else if let Ok((_, _)) = tag::<_, _, OracleError<'_>>("exile").parse(rest) {
        Zone::Exile
    } else {
        return None;
    };
    Some(zone)
}

/// CR 207.2c (Raid): Detect a trailing "if you attacked this turn" gate
/// on a self-ETB replacement. Used by Raid-flavor cards (Cruel
/// Administrator, Goblin Boarders, Mardu Heart-Piercer, Swaggering
/// Corsair, etc.) — "~ enters with a +1/+1 counter on it if you
/// attacked this turn."
fn extract_you_attacked_this_turn_suffix(work_text: &str) -> bool {
    use crate::parser::oracle_nom::error::OracleError;
    use nom::bytes::complete::tag;
    let Ok((rest, _)) =
        take_until::<_, _, OracleError<'_>>("if you attacked this turn").parse(work_text)
    else {
        return false;
    };
    tag::<_, _, OracleError<'_>>("if you attacked this turn")
        .parse(rest)
        .is_ok()
}

/// CR 702.188a: Scan `work_text` for "was cast using web-slinging" — the
/// intervening-if gate on Scarlet Spider's "Sensational Save" ETB replacement.
fn extract_cast_using_web_slinging_suffix(work_text: &str) -> bool {
    use crate::parser::oracle_nom::error::OracleError;
    use nom::bytes::complete::tag;
    let Ok((rest, _)) =
        take_until::<_, _, OracleError<'_>>("was cast using web-slinging").parse(work_text)
    else {
        return false;
    };
    tag::<_, _, OracleError<'_>>("was cast using web-slinging")
        .parse(rest)
        .is_ok()
}

fn replacement_condition_from_static(condition: StaticCondition) -> Option<ReplacementCondition> {
    match condition {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => Some(ReplacementCondition::OnlyIfQuantity {
            lhs,
            comparator,
            rhs,
            active_player_req: None,
        }),
        StaticCondition::SourceIsTapped => {
            Some(ReplacementCondition::SourceTappedState { tapped: true })
        }
        StaticCondition::Not { condition } if *condition == StaticCondition::SourceIsTapped => {
            Some(ReplacementCondition::SourceTappedState { tapped: false })
        }
        StaticCondition::HasMaxSpeed => Some(ReplacementCondition::HasMaxSpeed),
        _ => None,
    }
}

fn parse_replacement_ability_word_condition(text: &str) -> Option<ReplacementCondition> {
    let lower = text.to_lowercase();
    nom_on_lower(text, &lower, |input| {
        value(
            ReplacementCondition::HasMaxSpeed,
            alt((
                tag("max speed \u{2014} "),
                tag("max speed -- "),
                tag("max speed - "),
            )),
        )
        .parse(input)
    })
    .map(|(condition, _)| condition)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalEntryKind {
    Plain {
        enters_tapped: bool,
    },
    /// CR 614.1d: Uphill Battle class — cast/played entry only, not tokens.
    PlayedByOpponents {
        enters_tapped: bool,
    },
}

/// CR 614.1d: Peel external entry-tapped suffixes from a normalized clause.
/// Played-by-opponents variants are checked before plain enter-tapped suffixes
/// so "creatures played by your opponents enter tapped" does not fall through
/// to the Authority-of-the-Consuls control-based shape.
fn parse_external_entry_suffix(stripped: &str) -> Option<(&str, ExternalEntryKind)> {
    stripped
        .strip_suffix(" played by your opponents enter the battlefield tapped") // allow-noncombinator: fixed external-entry suffix peel after type-phrase subject
        .map(|subject| {
            (
                subject,
                ExternalEntryKind::PlayedByOpponents {
                    enters_tapped: true,
                },
            )
        })
        .or_else(|| {
            stripped
                .strip_suffix(" played by your opponents enter tapped") // allow-noncombinator: fixed external-entry suffix peel after type-phrase subject
                .map(|subject| {
                    (
                        subject,
                        ExternalEntryKind::PlayedByOpponents {
                            enters_tapped: true,
                        },
                    )
                })
        })
        .or_else(|| {
            // allow-noncombinator: fixed external-entry suffix peel after type-phrase subject
            stripped.strip_suffix(" enter tapped").map(|subject| {
                (
                    subject,
                    ExternalEntryKind::Plain {
                        enters_tapped: true,
                    },
                )
            })
        })
        .or_else(|| {
            // allow-noncombinator: fixed external-entry suffix peel after type-phrase subject
            stripped.strip_suffix(" enters tapped").map(|subject| {
                (
                    subject,
                    ExternalEntryKind::Plain {
                        enters_tapped: true,
                    },
                )
            })
        })
        .or_else(|| {
            // allow-noncombinator: fixed external-entry suffix peel after type-phrase subject
            stripped.strip_suffix(" enter untapped").map(|subject| {
                (
                    subject,
                    ExternalEntryKind::Plain {
                        enters_tapped: false,
                    },
                )
            })
        })
        .or_else(|| {
            // allow-noncombinator: fixed external-entry suffix peel after type-phrase subject
            stripped.strip_suffix(" enters untapped").map(|subject| {
                (
                    subject,
                    ExternalEntryKind::Plain {
                        enters_tapped: false,
                    },
                )
            })
        })
}

fn build_external_entry_replacement(
    subject: &str,
    original_text: &str,
    kind: ExternalEntryKind,
) -> Option<ReplacementDefinition> {
    if subject.contains('~') {
        return None;
    }

    let enters_tapped = match kind {
        ExternalEntryKind::Plain { enters_tapped }
        | ExternalEntryKind::PlayedByOpponents { enters_tapped } => enters_tapped,
    };

    let (filter, rest) = parse_type_phrase(subject);
    if !rest.trim().is_empty() {
        return None;
    }

    let valid_card = match kind {
        ExternalEntryKind::PlayedByOpponents { .. } => match filter {
            TargetFilter::Typed(mut tf) => {
                tf.controller = Some(ControllerRef::Opponent);
                tf.properties.push(FilterProp::WasPlayed);
                TargetFilter::Typed(tf)
            }
            TargetFilter::Or { filters } if filters.len() == 1 => {
                match filters.into_iter().next()? {
                    TargetFilter::Typed(mut tf) => {
                        tf.controller = Some(ControllerRef::Opponent);
                        tf.properties.push(FilterProp::WasPlayed);
                        TargetFilter::Typed(tf)
                    }
                    _ => return None,
                }
            }
            _ => return None,
        },
        ExternalEntryKind::Plain { .. } => filter,
    };

    let effect = if enters_tapped {
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        }
    } else {
        Effect::SetTapState {
            target: TargetFilter::SelfRef,
            scope: EffectScope::Single,
            state: TapStateChange::Untap,
        }
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::ChangeZone)
            .execute(AbilityDefinition::new(AbilityKind::Spell, effect))
            .valid_card(valid_card)
            .destination_zone(Zone::Battlefield)
            .description(original_text.to_string()),
    )
}

fn parse_source_state_external_entry(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let (condition, rest) = nom_on_lower(original_text, norm_lower, |i| {
        let (i, _) = tag::<_, _, OracleError<'_>>("as long as ").parse(i)?;
        let (i, condition) = parse_inner_condition(i)?;
        let (i, _) = tag(", ").parse(i)?;
        Ok((i, condition))
    })?;
    let condition = replacement_condition_from_static(condition)?;
    let rest_lower = rest.to_lowercase();
    let stripped = rest_lower.trim_end_matches('.');
    let (entry_subject, kind) = parse_external_entry_suffix(stripped)?;
    let mut def = build_external_entry_replacement(entry_subject, original_text, kind)?;
    def.condition = Some(condition);
    Some(def)
}

/// Parse "[Type] enter untapped" / "[Type] enters untapped" — external replacement effects.
fn parse_external_enters_untapped(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let stripped = norm_lower.trim_end_matches('.');
    let (subject, kind) = parse_external_entry_suffix(stripped)?;
    let ExternalEntryKind::Plain {
        enters_tapped: false,
    } = kind
    else {
        return None;
    };
    build_external_entry_replacement(subject, original_text, kind)
}

/// Parse "[Type] enter tapped" / "[Type] enters tapped" — external replacement effects.
/// E.g., "Creatures your opponents control enter tapped." (Authority of the Consuls)
/// E.g., "Artifacts and creatures your opponents control enter tapped." (Blind Obedience)
/// E.g., "Creatures played by your opponents enter tapped." (Uphill Battle)
fn parse_external_enters_tapped(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let stripped = norm_lower.trim_end_matches('.');
    let (subject, kind) = parse_external_entry_suffix(stripped)?;
    match kind {
        ExternalEntryKind::Plain {
            enters_tapped: true,
        }
        | ExternalEntryKind::PlayedByOpponents {
            enters_tapped: true,
        } => build_external_entry_replacement(subject, original_text, kind),
        _ => None,
    }
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
        tag::<_, _, OracleError<'_>>("would die").parse(i)
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

    let (subject_filter_text, replacement_condition) =
        split_dealt_damage_subject_condition(subject_start).unwrap_or((subject_start, None));
    let subject_filter_text = nom_primitives::parse_article
        .parse(subject_filter_text)
        .map_or(subject_filter_text, |(rest, _)| rest)
        .trim();

    // Skip self-reference subjects — handled by the earlier "~ would die" check.
    if subject_filter_text.contains('~') {
        return None;
    }

    // Parse the subject filter (e.g., "another creature", "a nontoken creature an opponent controls")
    let (filter, subject_rest) = parse_type_phrase(subject_filter_text);
    if matches!(&filter, TargetFilter::Any) || !subject_rest.trim().is_empty() {
        return None;
    }

    // Extract the replacement effect after "would die, " via a nom combinator.
    // CR 614.1a: Replacement effects use "instead" — both word orders are equivalent:
    //   suffix form: "exile it instead [.]"  (Void Maw, Valentin, Vren)
    //   prefix form: "instead exile it [and <continuation>] [.]"  (Darkness Crystal,
    //                Kalitas, Ravenloft Adventurer, Ravenous Slime, Doctor's Tomb)
    let after_would_die = &norm_lower[would_die_pos + "would die".len()..];
    let (effect_lower, _) = preceded(nom_primitives::ws, tag::<_, _, OracleError<'_>>(", "))
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
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                // CR 122.1 + CR 614.1c: enter_with_counters is populated when
                // the anaphor clause carried a "with N <type> counter(s) on it"
                // modifier. Empty otherwise.
                enter_with_counters: anaphor.enter_with_counters,
                face_down_profile: None,
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

fn split_dealt_damage_subject_condition(
    input: &str,
) -> Option<(&str, Option<ReplacementCondition>)> {
    let (condition_text, subject) = take_until::<_, _, OracleError<'_>>(" dealt damage")
        .parse(input)
        .ok()?;
    let condition = parse_dealt_damage_this_turn_source_condition(condition_text.trim())?;
    Some((subject.trim(), Some(condition)))
}

fn parse_dealt_damage_this_turn_source_condition(input: &str) -> Option<ReplacementCondition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("dealt damage ")
        .parse(input)
        .ok()?;
    let (rest, source) = if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("by ").parse(rest) {
        let (rest, source) = parse_damage_history_source(rest)?;
        let (rest, _) = tag::<_, _, OracleError<'_>>(" this turn")
            .parse(rest)
            .ok()?;
        (rest, source)
    } else {
        let (rest, _) = tag::<_, _, OracleError<'_>>("this turn by ")
            .parse(rest)
            .ok()?;
        parse_damage_history_source(rest)?
    };

    rest.trim()
        .is_empty()
        .then_some(ReplacementCondition::DealtDamageThisTurnBySource { source })
}

pub(crate) fn parse_damage_history_source(input: &str) -> Option<(&str, TargetFilter)> {
    if let Ok(result) = parse_typed_permanent_you_controlled_damage_source(input) {
        return Some(result);
    }
    alt((
        value(
            TargetFilter::SelfRef,
            tag::<_, _, OracleError<'_>>("this creature"),
        ),
        value(TargetFilter::SelfRef, tag("~")),
        value(TargetFilter::AttachedTo, tag("enchanted creature")),
        value(
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
            alt((
                tag::<_, _, OracleError<'_>>("a source you controlled"),
                tag("source you controlled"),
                tag("a source you control"),
                tag("source you control"),
            )),
        ),
    ))
    .parse(input)
    .ok()
}

/// CR 608.2i: "a [type] you controlled" damage-source look-back (Shelob's Spider gate).
fn parse_typed_permanent_you_controlled_damage_source(
    input: &str,
) -> OracleResult<'_, TargetFilter> {
    let (rest, _) = tag("a ").parse(input)?;
    let (after_type, type_text) =
        take_until::<_, _, OracleError<'_>>(" you controlled").parse(rest)?;
    let (after, _) = tag::<_, _, OracleError<'_>>(" you controlled").parse(after_type)?;
    let (filter, leftover) = parse_type_phrase(type_text);
    if !leftover.trim().is_empty() {
        return Err(nom::Err::Error(OracleError::new(
            leftover,
            nom::error::ErrorKind::Eof,
        )));
    }
    let filter = match filter {
        TargetFilter::Typed(mut tf) => {
            if tf.controller.is_none() {
                tf.controller = Some(ControllerRef::You);
            }
            TargetFilter::Typed(tf)
        }
        TargetFilter::Or { mut filters } => {
            for branch in &mut filters {
                if let TargetFilter::Typed(tf) = branch {
                    if tf.controller.is_none() {
                        tf.controller = Some(ControllerRef::You);
                    }
                }
            }
            TargetFilter::Or { filters }
        }
        other => other,
    };
    Ok((after, filter))
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
    enter_with_counters: Vec<(CounterType, QuantityExpr)>,
}

fn parse_exile_anaphor_clause<'a>(input: TextPair<'a>) -> ExileAnaphorMatch<'a> {
    use nom::sequence::terminated;

    let lower = input.lower;
    let exile_anaphor = || {
        alt((
            tag::<_, _, OracleError<'_>>("exile it"),
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
            tag::<_, _, OracleError<'_>>(" with "),
            crate::parser::oracle_effect::parse_counter_suffix_body_combinator,
        )
    };

    // Try prefix form first: "instead exile <anaphor> [with N counters on it]".
    // Then suffix form:    "exile <anaphor> [with N counters on it] instead".
    // The body shape is unified: the `with-counters` slot is optional in both
    // word orders.
    let parsed: nom::IResult<&str, Option<(CounterType, QuantityExpr)>, OracleError<'_>> = alt((
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
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: anaphor.enter_with_counters,
            face_down_profile: None,
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
///
/// CR 614.1a + CR 608.2n: Self-referential subjects (`~`, "this spell", …) must
/// carry `valid_card: SelfRef` so `find_applicable_replacements` discovers the
/// def while the spell is still on the stack (Nexus of Fate / Progenitus class).
fn graveyard_replacement_subject_is_self_referential(subject: &str) -> bool {
    let subject = subject.trim();
    subject == "~"
        || matches!(subject, "this spell" | "this card")
        || crate::parser::oracle_util::SELF_REF_TYPE_PHRASES.contains(&subject)
}

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

    // CR 730.3e + CR 111.1: the subject's token axis. "a card or token" is
    // token-INCLUSIVE (Rest in Peace) and adds no constraint; "a card" is
    // token-EXCLUDING (Leyline of the Void) and adds a `NonToken` filter so
    // a dying token reaches the graveyard (and dies-triggers fire) instead of
    // being wrongly redirected. Any other subject (`~`, "that spell", "a
    // permanent", a counter condition) leaves the axis `Unscoped` — the
    // pre-existing token-inclusive behavior, preserved.
    #[derive(Clone, Copy)]
    enum TokenScope {
        Unscoped,
        NonToken,
    }

    let ((scope, token_scope, outcome, subject), _rest) =
        nom_on_lower(original_text, norm_lower, |i| {
            // Prefix: "if <subject> would be put into <scope> graveyard[ from anywhere], "
            let (i, _) = tag::<_, _, OracleError<'_>>("if ").parse(i)?;
            // Subject: accept any phrase up to " would be put into " — covers
            // "a card", "a nontoken creature", "~", "a creature an opponent controls", …
            // — and classify its token axis (CR 730.3e) from the captured slice.
            let (i, subject) =
                take_until::<_, _, OracleError<'_>>(" would be put into ").parse(i)?;
            // CR 730.3e + CR 111.1: a card-noun subject WITHOUT an "or token" rider
            // is token-excluding (Leyline of the Void: "a card"). The inclusive RIP
            // phrasing ("a card or token") names tokens explicitly and stays
            // unscoped. The token-rider check wins over the bare-card check, so
            // "a card or token" is never misread as token-excluding.
            //
            // The token axis is a terminal-noun classification of the noun phrase
            // `take_until` already tokenized off. "Ends with <noun>" is expressed as
            // a forward combinator — `take_until(noun) + tag(noun) + eof` — so the
            // classification stays combinator-pure (no raw tail string-ops) and is
            // correct for arbitrarily long subjects ("a nontoken creature card",
            // "a creature an opponent controls") where a first-word split would not
            // be.
            fn subject_ends_with<'a>(subject: &'a str, noun: &'static str) -> bool {
                terminated(
                    (take_until(noun), tag(noun)),
                    eof::<&'a str, OracleError<'a>>,
                )
                .parse(subject)
                .is_ok()
            }
            let names_token =
                subject_ends_with(subject, " or token") || subject_ends_with(subject, " or tokens");
            let names_card =
                subject_ends_with(subject, " card") || subject_ends_with(subject, " cards");
            let token_scope = if names_card && !names_token {
                TokenScope::NonToken
            } else {
                TokenScope::Unscoped
            };
            let (i, _) = tag::<_, _, OracleError<'_>>(" would be put into ").parse(i)?;
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

            Ok((i, (scope, token_scope, outcome, subject.to_string())))
        })?;

    let subject = subject.trim();

    // Destination routing is determined by the outcome branch.
    let destination = match &outcome {
        Outcome::Exile => Zone::Exile,
        Outcome::ShuffleBack { .. } => Zone::Library,
    };

    // CR 400.3 + CR 108.3: "opponent's graveyard" means cards owned by an opponent
    // (cards go to owner's graveyard, so ownership is the stable discriminant).
    // CR 730.3e + CR 111.1: a token-excluding subject ("a card") adds `NonToken`
    // so a dying token is NOT redirected (Leyline of the Void must let an
    // opponent's token reach the graveyard so dies-triggers fire — Blood Artist
    // class). Both axes are leaf `FilterProp`s on one `TypedFilter`.
    let mut props = Vec::new();
    if let Scope::Opponent = scope {
        props.push(FilterProp::Owned {
            controller: ControllerRef::Opponent,
        });
    }
    if let TokenScope::NonToken = token_scope {
        props.push(FilterProp::NonToken);
    }
    let valid_card = if graveyard_replacement_subject_is_self_referential(subject) {
        Some(TargetFilter::SelfRef)
    } else if !props.is_empty() {
        Some(TargetFilter::Typed(
            TypedFilter::default().properties(props),
        ))
    } else {
        None
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
            enters_under: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            face_down_profile: None,
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

/// CR 614.1: Parse static damage modification abilities without "instead" keyword.
/// Handles patterns like "Double all damage that [subject] would deal" (Collective Inferno).
/// Uses quantifier parser ("double all damage") instead of anaphor parser ("double that damage").
/// The subject is between "that" and "would deal", not before "would deal" like in anaphor patterns.
fn parse_damage_modification_static(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // --- 1. Extract modification formula using quantifier parser ---
    let modification =
        nom_primitives::scan_at_word_boundaries(norm_lower, parse_damage_modification_quantifier)?;

    // --- 2. Extract source filter from the subject clause ---
    // Pattern: "Double all damage[ that] [subject] would deal" — the "that"
    // relative-clause marker is OPTIONAL. "Double all damage that creature
    // would deal" carries it; Mjölnir ("Double all damage equipped creature
    // would deal") omits it. Consume it with opt(tag) rather than requiring a
    // split on "that ", and extract the subject as the span between the
    // "double all damage" quantifier and " would deal".
    let (_, (_, after_quantifier)) =
        nom_primitives::split_once_on(norm_lower, "double all damage").ok()?;
    let after_quantifier = after_quantifier.trim_start();
    let (after_that, _) = opt(tag::<_, _, OracleError<'_>>("that "))
        .parse(after_quantifier)
        .ok()?;
    let (_, (subject, _)) = nom_primitives::split_once_on(after_that, " would deal").ok()?;

    let source_filter = parse_damage_source_subject_filter(subject.trim());

    // --- 3. Extract combat scope ---
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

/// CR 614.9 + CR 614.1a + CR 615: Parse a one-shot "the next time [source]
/// would deal [combat] damage [to X] this turn, [modify/redirect] instead"
/// damage-replacement effect into `Effect::CreateDamageReplacement`.
///
/// This is effect-creating text living in an activated/triggered ability body
/// (after `{T}:`, `{0}:`, in a flip-coin branch — Desperate Gambit, Soltari
/// Guerrillas, Beacon of Destiny, Jade Monolith, Goblin Psychopath), NOT a
/// permanent static replacement (those route through `parse_replacement_line`).
///
/// The detector IS the parser: the one-shot branch is gated by the
/// `tag("the next time ")` prefix combinator succeeding, never a string
/// heuristic. Returns `None` (fall-through) when the prefix or grammar fails.
pub(crate) fn parse_oneshot_damage_replacement(norm_lower: &str) -> Option<Effect> {
    // CR 614.9: passive-voice one-shot redirection — "the next N damage that
    // would be dealt to ~ this turn is dealt to <recipient> instead" (the en-Kor
    // cycle). This "would be dealt to" (passive, recipient-first) spine is not
    // covered by the active "the next time [source] would deal" grammar below,
    // so try it first and fall through on mismatch.
    if let Some(effect) = parse_oneshot_next_n_damage_to_self_redirect(norm_lower) {
        return Some(effect);
    }

    // CR 614.9: the mirror shape — the ORIGINAL recipient is a chosen TARGET and
    // the damage is redirected to the source (`~`) or its controller (`you`)
    // (Daughter of Autumn, Vassal's Duty). Tried after the self-recipient form so
    // the en-Kor "...dealt to ~ this turn..." case is claimed there first.
    if let Some(effect) = parse_oneshot_next_n_damage_to_target_redirect(norm_lower) {
        return Some(effect);
    }

    // CR 614.1a + CR 514.2: "the next time ... this turn" — a replacement effect
    // ("instead", CR 614.1a) with a "this turn" duration that ends at cleanup
    // (CR 514.2). The one-opportunity consumption is CR 614.5 (see resolver).
    let (after_prefix, _) = preceded(
        tag::<_, _, OracleError<'_>>("the next time "),
        peek(take_until::<_, _, OracleError<'_>>("would deal")),
    )
    .parse(norm_lower)
    .ok()?;
    // Require a "would deal ... this turn" spine; bail early otherwise so this
    // never shadows other "the next time" effects (e.g. card-draw replacements).
    if !nom_primitives::scan_contains(after_prefix, "would deal")
        || !nom_primitives::scan_contains(after_prefix, "this turn")
    {
        return None;
    }

    // Strip the prefix for sub-parser reuse — `parse_damage_source_filter`
    // splits on "would deal" itself, so feed it the post-prefix slice.
    let body = after_prefix.trim();

    // The "would deal ... this turn" clause carries the source + original
    // recipient; the result clause (after the comma) carries the redirect /
    // amount. Split there so recipient parsing never sees the redirect's "to ..."
    // and vice-versa.
    let (would_clause, result_clause) = split_would_deal_clause(body);

    // Source spec (subject before "would deal"). Reuse the shared source-filter
    // parser, then layer the one-shot-specific anaphors it doesn't cover.
    let source_filter = parse_oneshot_source_filter(body);

    // Combat scope: "combat damage" vs "damage".
    let combat_scope = scan_combat_scope(would_clause);

    // Original-recipient scope from the would-deal clause: a typed scope ("to an
    // opponent" / "to a creature") OR a chosen target ("to target creature" —
    // Jade Monolith). The latter becomes a hosted object slot, not a scope.
    let recipient_object_filter = parse_damage_to_target_filter(would_clause);
    let target_filter = if recipient_object_filter.is_some() {
        None
    } else {
        parse_damage_target_filter(would_clause)
    };

    // Result clause: amount-modifying form ("double that damage") first; else
    // redirection form ("it deals that damage to <recipient> instead").
    if let Some(modification) = scan_damage_modification(result_clause) {
        // CR 614.1a: amount-modifying one-shot (Desperate Gambit).
        return Some(Effect::CreateDamageReplacement {
            source_filter,
            combat_scope,
            target_filter,
            modification: Some(modification),
            redirect_to: None,
            redirect_amount: None,
            redirect_object_filter: None,
            recipient_object_filter,
        });
    }

    // CR 614.9: redirection one-shot.
    if let Some(redirect_to) = parse_redirect_recipient(result_clause) {
        let redirect_object_filter = match redirect_to {
            DamageRedirectTarget::ChosenObjectTarget => {
                parse_damage_to_target_filter(result_clause)
            }
            DamageRedirectTarget::Controller | DamageRedirectTarget::SourceObject => None,
        };
        return Some(Effect::CreateDamageReplacement {
            source_filter,
            combat_scope,
            target_filter,
            modification: None,
            redirect_to: Some(redirect_to),
            redirect_amount: None,
            redirect_object_filter,
            recipient_object_filter,
        });
    }

    // CR 615: prevention sibling ("the next time [source] would deal damage this
    // turn, prevent that damage" — Desperate Gambit lose-branch). The existing
    // `PreventDamage` resolver builds a one-shot `ShieldKind::Prevention` shield;
    // route the source-scoped one-shot prevention through it rather than
    // duplicating the shield-creation flow.
    if nom_primitives::scan_contains(result_clause, "prevent that damage")
        || nom_primitives::scan_contains(result_clause, "prevent the damage")
    {
        return Some(Effect::PreventDamage {
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::Any,
            scope: combat_scope
                .map(|_| crate::types::ability::PreventionScope::CombatDamage)
                .unwrap_or(crate::types::ability::PreventionScope::AllDamage),
            damage_source_filter: source_filter,
            prevention_duration: None,
        });
    }

    None
}

/// CR 614.9 + CR 614.5: Parse the en-Kor cycle's one-shot redirection —
/// "the next N damage that would be dealt to ~ this turn is dealt to target
/// creature you control instead" (Nomads / Lancers / Outrider / Shaman / Spirit
/// / Warrior en-Kor). The original recipient is the source itself (`~`), encoded
/// as `recipient_object_filter: SelfRef`: the resolver hosts the shield on the
/// source with `valid_card: SelfRef` so it fires only on damage to it, and the
/// targeting layer surfaces no slot for the self recipient. The redirect
/// recipient is a chosen object target ("target creature you control"). The
/// amount N is retained as a depletion-style redirection cap so only that much
/// damage is moved to the chosen recipient.
fn parse_oneshot_next_n_damage_to_self_redirect(norm_lower: &str) -> Option<Effect> {
    let (rest, (_, amount, _)) = (
        tag::<_, _, OracleError<'_>>("the next "),
        nom_primitives::parse_number,
        tag::<_, _, OracleError<'_>>(" damage that would be dealt to ~ this turn is dealt to "),
    )
        .parse(norm_lower)
        .ok()?;

    // CR 115.1: redirect recipient — "target creature you control" (every en-Kor
    // card) or the looser "target creature"; both become a chosen object target.
    // (An "any target" redirect is intentionally NOT accepted here: it can be a
    // player, but the CreateDamageReplacement resolver stores only object redirect
    // targets, so a player choice would silently drop the redirect — fail closed.)
    let (rest, redirect_object_filter) = alt((
        value(
            inject_controller(
                TargetFilter::Typed(TypedFilter::creature()),
                ControllerRef::You,
            ),
            tag::<_, _, OracleError<'_>>("target creature you control"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::creature()),
            tag("target creature"),
        ),
    ))
    .parse(rest)
    .ok()?;

    let (rest, _) = tag::<_, _, OracleError<'_>>(" instead").parse(rest).ok()?;
    let (rest, _) = opt(char::<_, OracleError<'_>>('.')).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    Some(Effect::CreateDamageReplacement {
        source_filter: None,
        combat_scope: None,
        target_filter: None,
        modification: None,
        redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
        redirect_amount: Some(PreventionAmount::Next(amount)),
        redirect_object_filter: Some(redirect_object_filter),
        recipient_object_filter: Some(TargetFilter::SelfRef),
    })
}

/// CR 614.9: one-shot redirection whose ORIGINAL recipient is a CHOSEN TARGET.
/// "the next N damage that would be dealt to <target> this turn is dealt to
/// <destination> instead", where the destination is the source itself
/// (`~` → `SourceObject`), its controller (`you` → `Controller`), or a SECOND
/// chosen *object* target (`another target creature` → `ChosenObjectTarget` with
/// its own redirect slot). Covers Daughter of Autumn / Vassal's Duty (→ ~ / you)
/// and Razia, Boros Archangel (→ another target creature). The mirror of
/// `parse_oneshot_next_n_damage_to_self_redirect` (recipient `~`); `Controller`/
/// `SourceObject` need no chosen redirect slot, the chosen-object destination
/// surfaces one (CR 115.1) alongside the recipient slot. An `any target` redirect
/// is rejected (it can be a player, which the object-only redirect resolver can't
/// store) — fail closed.
fn parse_oneshot_next_n_damage_to_target_redirect(norm_lower: &str) -> Option<Effect> {
    let (rest, (_, amount, _)) = (
        tag::<_, _, OracleError<'_>>("the next "),
        nom_primitives::parse_number,
        tag::<_, _, OracleError<'_>>(" damage that would be dealt to "),
    )
        .parse(norm_lower)
        .ok()?;

    // CR 115.1: the original recipient is a chosen object target ("target white
    // creature", "target legendary creature you control", …). Capture the target
    // phrase up to the " this turn is dealt to " spine.
    let (rest, recipient_text) = take_until::<_, _, OracleError<'_>>(" this turn is dealt to ")
        .parse(rest)
        .ok()?;
    // CR 115.1: the original recipient must be an explicit chosen target
    // ("target white creature", "target legendary creature you control"). Gate on
    // the `target` keyword via a combinator — the en-Kor `~` recipient (owned by
    // the sibling function above) and bare scopes ("a creature") fail closed here.
    if tag::<_, _, OracleError<'_>>("target ")
        .parse(recipient_text)
        .is_err()
    {
        return None;
    }
    let (recipient_filter, leftover) = crate::parser::oracle_target::parse_target(recipient_text);
    if !leftover.trim().is_empty() {
        return None;
    }

    let (rest, _) = tag::<_, _, OracleError<'_>>(" this turn is dealt to ")
        .parse(rest)
        .ok()?;

    // CR 614.9: redirect destination. `~` (the source object) and `you` (its
    // controller) need no chosen redirect slot; a second chosen target ("another
    // target creature", "any target", "target creature ...") is a
    // `ChosenObjectTarget` that surfaces its own redirect slot (Razia, Boros
    // Archangel). Capture the recipient phrase up to the trailing " instead".
    let (after_redirect, redirect_text) = take_until::<_, _, OracleError<'_>>(" instead")
        .parse(rest)
        .ok()?;
    let (redirect_to, redirect_object_filter) = if all_consuming(tag::<_, _, OracleError<'_>>("~"))
        .parse(redirect_text)
        .is_ok()
    {
        (DamageRedirectTarget::SourceObject, None)
    } else if all_consuming(tag::<_, _, OracleError<'_>>("you"))
        .parse(redirect_text)
        .is_ok()
    {
        (DamageRedirectTarget::Controller, None)
    } else {
        let (filter, leftover) = crate::parser::oracle_target::parse_target(redirect_text);
        // CR 115.1: require a fully-consumed chosen *object* target. `TargetFilter::Any`
        // is intentionally rejected: it can resolve to a player, but the
        // CreateDamageReplacement resolver stores only object redirect targets
        // (`chosen_redirect_object`), so a player choice would silently drop the
        // redirect. Fail closed on `Any`, scopes, and unparsed remainders.
        if !leftover.trim().is_empty() || !matches!(filter, TargetFilter::Typed(_)) {
            return None;
        }
        (DamageRedirectTarget::ChosenObjectTarget, Some(filter))
    };

    let (rest, _) = tag::<_, _, OracleError<'_>>(" instead")
        .parse(after_redirect)
        .ok()?;
    let (rest, _) = opt(char::<_, OracleError<'_>>('.')).parse(rest).ok()?;
    if !rest.trim().is_empty() {
        return None;
    }

    Some(Effect::CreateDamageReplacement {
        source_filter: None,
        combat_scope: None,
        target_filter: None,
        modification: None,
        redirect_to: Some(redirect_to),
        redirect_amount: Some(PreventionAmount::Next(amount)),
        redirect_object_filter,
        recipient_object_filter: Some(recipient_filter),
    })
}

/// Split the one-shot body at the "this turn[,]" boundary into the would-deal
/// clause (source + original recipient) and the result clause (redirect /
/// amount / prevention). The result clause is what follows "this turn".
fn split_would_deal_clause(body: &str) -> (&str, &str) {
    match nom_primitives::split_once_on(body, "this turn") {
        Ok((_, (before, after))) => {
            // `after` begins after "this turn"; trim a leading comma/space.
            let after = after.trim_start_matches([',', ' ']);
            (before, after)
        }
        Err(_) => (body, body),
    }
}

/// CR 115.1: Detect a chosen-target recipient ("to target creature" / "to
/// target permanent") and return its `TargetFilter`. Distinct from
/// `parse_damage_target_filter`, which handles typed *scopes* ("to a creature",
/// "to an opponent"). Returns `None` when the recipient is a scope or implicit.
fn parse_damage_to_target_filter(clause: &str) -> Option<TargetFilter> {
    nom_primitives::scan_at_word_boundaries(clause, |input| {
        let (input, _) = tag("to ").parse(input)?;
        let (input, filter) = alt((
            value(
                TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                tag("target creature"),
            ),
            value(
                TargetFilter::Typed(TypedFilter::default()),
                tag("target permanent"),
            ),
        ))
        .parse(input)?;
        Ok((input, filter))
    })
}

/// CR 614.1a: Resolve the one-shot replacement's damage *source* spec. Delegates
/// to the shared `parse_damage_source_filter` for the "source you control" /
/// color / type forms, then layers the one-shot anaphors:
/// - "it" / "~" / "this creature" → `SelfRef` (Goblin Psychopath, Soltari).
/// - "that source" / "a source of your choice" → `ChosenDamageSource` (Desperate
///   Gambit, Beacon of Destiny, Jade Monolith) — bound to the source chosen by
///   the preceding "choose a source" step at resolution time.
fn parse_oneshot_source_filter(body: &str) -> Option<TargetFilter> {
    let (_, (subject, _)) = nom_primitives::split_once_on(body, "would deal").ok()?;
    let subject = subject.trim();
    // Bare-anaphor source references (handled by combinator dispatch, not the
    // generic source-filter parser).
    //
    // TODO (known limitation, deferred): cross-sentence anaphora. In the
    // Desperate Gambit lose-branch ("...the next time it would deal damage this
    // turn, prevent that damage"), the bare "it" co-refers with the prior
    // sentence's "that source" (the chosen source), so it should resolve to
    // `ChosenDamageSource`, not `SelfRef`. Resolving that requires sentence-level
    // anaphora tracking across the FlipCoin branches, which is a separate parser
    // problem out of scope here. The win-branch (amount) and the activated-ability
    // cards (Soltari/Beacon/Jade/Goblin Psychopath) are unaffected.
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("it"),
        tag("~"),
        tag("this creature"),
    ))
    .parse(subject)
    {
        if rest.trim().is_empty() {
            return Some(TargetFilter::SelfRef);
        }
    }
    if let Ok((rest, _)) = alt((
        tag::<_, _, OracleError<'_>>("that source"),
        tag("a source of your choice"),
    ))
    .parse(subject)
    {
        if rest.trim().is_empty() {
            // TODO (known limitation, deferred): the candidate constraint on the
            // chosen source is dropped. Desperate Gambit's separate "Choose a
            // source you control" sentence parses as TargetOnly{Any}, and the
            // inline source prompt enumerates with TargetFilter::Any — so a
            // "you control" restriction (and any similar qualifier) is not yet
            // enforced when the player picks the source. Closing this needs the
            // pre-choice candidate filter threaded into ChosenDamageSource;
            // out of scope for this change.
            return Some(TargetFilter::ChosenDamageSource);
        }
    }
    parse_damage_source_filter(body)
}

/// CR 614.9: Parse the redirection recipient from the result clause by scanning
/// word boundaries for one redirection lead-in followed by a recipient. The
/// three lead-ins ("it deals that damage to ", "that damage is dealt to ",
/// "that source deals that damage to ") collapse to two `to`-anchors; the
/// recipient is "you" (Controller), "~" (the source object), or "target
/// creature"/"target permanent" (a chosen object target).
fn parse_redirect_recipient(body: &str) -> Option<DamageRedirectTarget> {
    nom_primitives::scan_at_word_boundaries(body, parse_redirect_recipient_phrase)
}

/// Nom combinator for a redirection lead-in + recipient phrase.
fn parse_redirect_recipient_phrase(
    input: &str,
) -> nom::IResult<&str, DamageRedirectTarget, OracleError<'_>> {
    // Lead-in: "(deals|is dealt) that damage to " — the active form ("it/that
    // source deals that damage to") and passive form ("that damage is dealt
    // to") share the trailing "that damage ... to ".
    let (input, _) = alt((
        tag("deals that damage to "),
        tag("that damage is dealt to "),
    ))
    .parse(input)?;
    alt((
        value(DamageRedirectTarget::Controller, tag("you")),
        value(DamageRedirectTarget::SourceObject, tag("~")),
        value(
            DamageRedirectTarget::ChosenObjectTarget,
            alt((tag("target creature"), tag("target permanent"))),
        ),
    ))
    .parse(input)
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

    // "a spell" — any spell is the source; no typed filter (Benevolent Unicorn).
    // Must precede `parse_type_phrase`, which maps bare "spell" to Card.
    if subject == "spell" {
        return None;
    }

    // "a source" / "sources" with no qualifier — no filter needed (matches any source).
    if matches!(subject, "source" | "sources") {
        return None;
    }

    if let Some(filter) = parse_damage_source_subject_filter(subject) {
        return Some(filter);
    }

    None
}

/// CR 301.5 + CR 702.6: "equipped creature" (Equipment) and "enchanted
/// creature" (Aura) both name the host object the permanent is attached to,
/// which the runtime resolves via `TargetFilter::AttachedTo`. Mirrors the
/// `parse_damage_history_source` "enchanted creature" mapping so both the
/// "instead" and no-"instead" damage-replacement surfaces agree.
fn parse_attached_host_subject(input: &str) -> OracleResult<'_, TargetFilter> {
    alt((
        value(
            TargetFilter::AttachedTo,
            tag::<_, _, OracleError<'_>>("equipped creature"),
        ),
        value(TargetFilter::AttachedTo, tag("enchanted creature")),
    ))
    .parse(input)
}

fn parse_damage_source_subject_filter(subject: &str) -> Option<TargetFilter> {
    // CR 301.5 + CR 702.6: host-relative subjects ("equipped creature",
    // "enchanted creature") resolve to the attached object before the generic
    // typed-source grammar, which would otherwise treat "creature" as a type.
    if let Ok((rest, filter)) = parse_attached_host_subject(subject) {
        if rest.trim().is_empty() {
            return Some(filter);
        }
    }
    if let Some(filter) = parse_damage_source_subject(subject) {
        return Some(filter);
    }

    // Typed damage sources ("creature you control with a +1/+1 counter on it",
    // "creatures you control with counters on them", ...) share the normal
    // target grammar; damage replacement parsing should not maintain a parallel
    // counter/property grammar.
    let (filter, rest) = parse_type_phrase(subject);
    if rest.trim().is_empty() && !matches!(filter, TargetFilter::Any) {
        return Some(filter);
    }

    None
}

/// Parse source-noun subjects shared by "instead" and no-"instead" damage
/// replacement text:
/// - "Giant source you control"
/// - "Goblin sources you control"
/// - "sources you control of the chosen type"
fn parse_damage_source_subject(subject: &str) -> Option<TargetFilter> {
    let (qualifier, tail) = split_damage_source_noun(subject)?;
    if qualifier.trim().is_empty() && tail.trim().is_empty() {
        return None;
    }

    let mut filter = TypedFilter::default();
    let mut props = Vec::new();

    let mut tail = tail.trim();
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("you control").parse(tail) {
        filter = filter.controller(ControllerRef::You);
        tail = rest.trim();
    }

    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("of the chosen type").parse(tail) {
        if !rest.trim().is_empty() {
            return None;
        }
        props.push(FilterProp::IsChosenCreatureType);
    } else if !tail.is_empty() {
        return None;
    }

    apply_damage_source_qualifier(&mut filter, &mut props, qualifier.trim());

    if !props.is_empty() {
        filter.properties = props;
    }

    Some(TargetFilter::Typed(filter))
}

fn split_damage_source_noun(subject: &str) -> Option<(&str, &str)> {
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("sources").parse(subject) {
        return Some(("", rest));
    }
    if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("source").parse(subject) {
        return Some(("", rest));
    }
    if let Ok((_, (qualifier, rest))) = nom_primitives::split_once_on(subject, " sources") {
        return Some((qualifier, rest));
    }
    if let Ok((_, (qualifier, rest))) = nom_primitives::split_once_on(subject, " source") {
        return Some((qualifier, rest));
    }
    None
}

fn apply_damage_source_qualifier(
    filter: &mut TypedFilter,
    props: &mut Vec<FilterProp>,
    qualifier: &str,
) {
    if qualifier.is_empty() {
        return;
    }

    let qualifier = if qualifier == "another" {
        props.push(FilterProp::Another);
        ""
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("another ").parse(qualifier) {
        props.push(FilterProp::Another);
        rest.trim()
    } else {
        qualifier
    };

    if let Some(color) = parse_color_word(qualifier) {
        props.push(FilterProp::HasColor { color });
    } else if let Ok((rest, _)) = tag::<_, _, OracleError<'_>>("non").parse(qualifier) {
        // CR 205.4b: "noncreature" qualifier — negation via TypeFilter::Non.
        if tag::<_, _, OracleError<'_>>("token")
            .parse(rest)
            .is_ok_and(|(after, _)| after.is_empty())
        {
            props.push(FilterProp::NonToken);
        } else {
            let inner = alt((
                value(
                    TypeFilter::Creature,
                    tag::<_, _, OracleError<'_>>("creature"),
                ),
                value(TypeFilter::Land, tag::<_, _, OracleError<'_>>("land")),
                value(
                    TypeFilter::Artifact,
                    tag::<_, _, OracleError<'_>>("artifact"),
                ),
                value(
                    TypeFilter::Enchantment,
                    tag::<_, _, OracleError<'_>>("enchantment"),
                ),
                value(
                    TypeFilter::Planeswalker,
                    tag::<_, _, OracleError<'_>>("planeswalker"),
                ),
            ))
            .parse(rest)
            .ok()
            .filter(|(after, _)| after.is_empty())
            .map_or_else(
                || TypeFilter::Subtype(capitalize_first(rest)),
                |(_, filter)| filter,
            );
            *filter = filter.clone().with_type(TypeFilter::Non(Box::new(inner)));
        }
    } else if !qualifier.is_empty() {
        *filter = filter.clone().subtype(capitalize_first(qualifier));
    }
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
                DamageTargetFilter::Player { .. }
                    if nom_primitives::scan_contains(norm_lower, "permanent") =>
                {
                    // Skip — "permanent" present means the broader player/permanent
                    // scope already matched.
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

fn damage_target_any_player() -> DamageTargetFilter {
    DamageTargetFilter::Player {
        player: DamageTargetPlayerScope::Any,
    }
}

fn damage_target_controller() -> DamageTargetFilter {
    DamageTargetFilter::Player {
        player: DamageTargetPlayerScope::Controller,
    }
}

fn damage_target_opponent() -> DamageTargetFilter {
    DamageTargetFilter::Player {
        player: DamageTargetPlayerScope::Opponent,
    }
}

fn damage_target_opponent_or_permanents() -> DamageTargetFilter {
    DamageTargetFilter::PlayerOrPermanentsControlledBy {
        player: DamageTargetPlayerScope::Opponent,
    }
}

fn damage_target_source_chosen_player_or_permanents() -> DamageTargetFilter {
    DamageTargetFilter::PlayerOrPermanentsControlledBy {
        player: DamageTargetPlayerScope::SourceChosenPlayer,
    }
}

/// Nom combinator for damage target phrases. Most specific tags first.
fn parse_damage_target_phrase(
    input: &str,
) -> nom::IResult<&str, DamageTargetFilter, OracleError<'_>> {
    alt((
        value(
            damage_target_source_chosen_player_or_permanents(),
            alt((
                tag("to the chosen player or a permanent they control"),
                tag("to the chosen player or a permanent the chosen player controls"),
            )),
        ),
        value(
            damage_target_opponent_or_permanents(),
            tag("to an opponent or a permanent an opponent controls"),
        ),
        value(
            DamageTargetFilter::Player {
                player: DamageTargetPlayerScope::SourceChosenPlayer,
            },
            tag("to the chosen player"),
        ),
        value(
            DamageTargetFilter::CreatureOnly,
            alt((tag("to a creature"), tag("to that creature"))),
        ),
        value(damage_target_opponent(), tag("to an opponent")),
        value(
            damage_target_any_player(),
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
    // Fallback: "that much damage plus/minus N" (fixed) or "that much damage
    // plus X" (variable). The X case yields a `Plus { value: 0 }` placeholder —
    // `DamageModification::Plus` carries a `u32`, so X is frozen at activation in
    // `add_target_replacement::resolve` (CR 107.3a). Composed from nom
    // combinators rather than `strip_after` so dispatch stays structural.
    nom_primitives::scan_at_word_boundaries(text, parse_that_much_damage_offset)
}

/// CR 614.1a + CR 107.3a: "that much damage plus N" / "plus X" / "minus N".
/// The "plus X" arm emits `Plus { value: 0 }` as a placeholder frozen at
/// activation (X cannot live in the `u32`-typed `DamageModification`).
fn parse_that_much_damage_offset(
    input: &str,
) -> nom::IResult<&str, DamageModification, OracleError<'_>> {
    let (rest, _) = tag("that much damage ").parse(input)?;
    alt((
        // "plus X" — variable offset, frozen at install. Tried before the
        // numeric arm so the literal "x" token is not consumed by parse_number.
        value(DamageModification::Plus { value: 0 }, tag("plus x")),
        nom::combinator::map(preceded(tag("plus "), nom_primitives::parse_number), |n| {
            DamageModification::Plus { value: n }
        }),
        nom::combinator::map(preceded(tag("minus "), nom_primitives::parse_number), |n| {
            DamageModification::Minus { value: n }
        }),
    ))
    .parse(rest)
}

/// Nom combinator for damage modification phrases.
fn parse_damage_modification_phrase(
    input: &str,
) -> nom::IResult<&str, DamageModification, OracleError<'_>> {
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

/// Nom combinator for quantifier damage modification phrases ("double all damage").
/// Used for static abilities like Collective Inferno that lack the "instead" keyword.
fn parse_damage_modification_quantifier(
    input: &str,
) -> nom::IResult<&str, DamageModification, OracleError<'_>> {
    value(DamageModification::Double, tag("double all damage")).parse(input)
}

/// Scan for combat damage scope at word boundaries.
/// "noncombat" tried first since "combat damage" is a substring.
fn scan_combat_scope(text: &str) -> Option<CombatDamageScope> {
    nom_primitives::scan_at_word_boundaries(text, |input| {
        alt((
            value(
                CombatDamageScope::NoncombatOnly,
                tag::<_, _, OracleError<'_>>("noncombat damage"),
            ),
            value(CombatDamageScope::CombatOnly, tag("combat damage")),
        ))
        .parse(input)
    })
}

/// CR 119.10 + CR 614.6: True iff the replacement body negates the life gain
/// ("[that player] gains no life" / "[you] gain no life"). The optional
/// player-subject anaphor (`that player` / `the player` / `you` / `they`) is
/// consumed by a nom `alt` before the negation `tag`, so the negation phrase is
/// matched structurally rather than by verbatim full-sentence comparison. This
/// covers the "gains no life instead" *replacement* class (Sulfuric Vortex);
/// the separate `StaticAbilityMode::CantGainLife` hate-permanent class
/// (Erebos, Leyline of Punishment, …) uses no replacement framing and is
/// matched elsewhere.
fn body_is_lifegain_negation(lower_body: &str) -> bool {
    let subject = opt(alt((
        tag::<_, _, OracleError<'_>>("that player "),
        tag("the player "),
        tag("you "),
        tag("they "),
    )));
    let mut combinator = preceded(
        subject,
        value((), alt((tag("gains no life"), tag("gain no life")))),
    );
    combinator.parse(lower_body.trim()).is_ok()
}

/// CR 614.1a: Assign the replacement's player scope from the antecedent subject
/// ("an opponent" → Opponent, "a player" / "its controller" → AnyPlayer,
/// "you" → controller-only/None). Shared by the `Prevent` short-circuit and the
/// generic execute path so both surfaces compute scope identically.
fn apply_gain_life_player_scope(lower: &str, def: &mut ReplacementDefinition) {
    if nom_primitives::scan_contains(lower, "an opponent would gain life")
        || nom_primitives::scan_contains(lower, "opponent would gain life")
    {
        def.valid_player = Some(ReplacementPlayerScope::Opponent);
    } else if nom_primitives::scan_contains(lower, "would cause its controller to gain life")
        || nom_primitives::scan_contains(lower, "a player would gain life")
    {
        // CR 614.1a: "a spell or ability would cause its controller to gain
        // life" (Rain of Gore) and "a player would gain life" are global — the
        // replacement watches every player's life gain, not just the source
        // controller's.
        def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
    }
    // else: "you would gain life" → valid_player stays None (controller-only).
}

/// CR 614.1a: Apply `valid_player` scope to draw replacements from the
/// antecedent subject ("an opponent", "a player", or default controller-only).
fn apply_draw_player_scope(lower: &str, def: &mut ReplacementDefinition) {
    if nom_primitives::scan_contains(lower, "an opponent would draw")
        || nom_primitives::scan_contains(lower, "opponent would draw")
    {
        def.valid_player = Some(ReplacementPlayerScope::Opponent);
    } else if nom_primitives::scan_contains(lower, "a player would draw") {
        def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
    }
    // else: "you would draw" → valid_player stays None (controller-only).
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
        // Strip trailing "... instead" marker (e.g., "draw two cards instead.").
        let effect = effect
            .strip_suffix(" instead")
            .map_or(effect, |trimmed| trimmed.trim_end());
        // CR 614.1a: Strip leading "instead ..." marker (e.g., "instead you
        // draw two cards"). This form appears when the subject follows the
        // replacement word, as in Blood Scrivener: "..., instead you draw two
        // cards and you lose 1 life."
        let effect = effect
            .strip_prefix("instead ") // allow-noncombinator: TextPair structural cleanup on an already-extracted replacement effect fragment, mirroring the trailing "instead" strip above.
            .map_or(effect, |stripped| stripped.trim_start());
        if !effect.original.is_empty() {
            return Some(effect.original.to_string());
        }
    }
    None
}

/// CR 614.1a + CR 614.6: Strip a leading "you may instead " modal from the
/// effect text of an optional replacement and report whether it was present.
/// Returns `(true, remainder)` when the modal is stripped, `(false, original)`
/// otherwise. The modal must lead the effect text — a mid-clause "instead" is
/// the mandatory-replacement marker, not the optional one.
///
/// Uses a nom `tag` over the lowercased text for dispatch (no `starts_with`),
/// then peels the matched byte length off the original case-preserving slice
/// so downstream chain parsing sees the original capitalization.
fn strip_optional_instead_lead_in(effect_text: &str) -> (bool, &str) {
    let lower = effect_text.to_lowercase();
    let strip_result: nom::IResult<&str, (), OracleError<'_>> =
        preceded(tag("you may instead "), nom::combinator::success(())).parse(lower.as_str());
    let Ok((rest_lower, ())) = strip_result else {
        return (false, effect_text);
    };
    let offset = lower.len() - rest_lower.len();
    let rest_orig = effect_text[offset..].trim_start();
    (true, rest_orig)
}

#[derive(Clone, Copy)]
enum MillReplacementSubject {
    You,
    Opponent,
}

fn parse_mill_count_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    let ((subject, count), rest) = nom_on_lower(lower, lower, |input| {
        let (input, _) = tag("if ").parse(input)?;
        let (input, subject) = alt((
            value(MillReplacementSubject::Opponent, tag("an opponent")),
            value(MillReplacementSubject::Opponent, tag("opponent")),
            value(MillReplacementSubject::You, tag("you")),
        ))
        .parse(input)?;
        let (input, _) = tag(" would mill one or more cards, ").parse(input)?;
        let (input, _) = alt((tag("they mill "), tag("you mill "))).parse(input)?;
        let (input, count) = parse_mill_replacement_count.parse(input)?;
        let (input, _) = alt((tag(" cards instead"), tag(" instead"))).parse(input)?;
        let (input, _) = opt(char('.')).parse(input)?;
        Ok((input, (subject, count)))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let mut def = ReplacementDefinition::new(ReplacementEvent::Mill)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                count,
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        ))
        .description(original_text.to_string());

    if matches!(subject, MillReplacementSubject::Opponent) {
        def.valid_player = Some(ReplacementPlayerScope::Opponent);
    }

    Some(def)
}

fn parse_mill_replacement_count(input: &str) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    alt((
        value(
            QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            },
            tag("twice that many"),
        ),
        nom::combinator::map(
            preceded(tag("that many cards plus "), nom_primitives::parse_number),
            |value| QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
                offset: value as i32,
            },
        ),
        value(
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            tag("that many"),
        ),
    ))
    .parse(input)
}

/// CR 614.1a: Apply `valid_player` scope to proliferate replacements from the
/// antecedent subject ("an opponent", "a player", or default controller-only).
fn apply_proliferate_player_scope(lower: &str, def: &mut ReplacementDefinition) {
    if nom_primitives::scan_contains(lower, "an opponent would proliferate")
        || nom_primitives::scan_contains(lower, "opponent would proliferate")
    {
        def.valid_player = Some(ReplacementPlayerScope::Opponent);
    } else if nom_primitives::scan_contains(lower, "a player would proliferate") {
        def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
    } else if nom_primitives::scan_contains(lower, "you would proliferate") {
        def.valid_player = Some(ReplacementPlayerScope::You);
    }
}

fn parse_proliferate_replacement_count(
    input: &str,
) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    alt((
        value(
            QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            },
            tag("twice that many"),
        ),
        nom::combinator::map(nom_primitives::parse_number, |value| QuantityExpr::Fixed {
            value: value as i32,
        }),
        // CR 616.1: "proliferate twice" is a *multiplicative* replacement, not a
        // set-to-2. Modeling it as `Multiply` (double the in-flight count) instead
        // of `Fixed { value: 2 }` lets two doublers compound through the
        // replacement pipeline's re-evaluation: two Tekuthal, Inquiry Dominus
        // proliferate 1 -> 2 -> 4 times (per the MOM ruling), not a flat 2. The
        // single-doubler case is unchanged (1 * 2 == 2).
        value(
            QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            },
            tag("twice"),
        ),
        value(
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            tag("that many"),
        ),
    ))
    .parse(input)
}

/// CR 701.34a + CR 614.1a: Parse count-modifying proliferate replacements such
/// as Tekuthal, Inquiry Dominus ("proliferate twice instead").
fn parse_proliferate_count_replacement(
    lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let (count, rest) = nom_on_lower(lower, lower, |input| {
        let (input, _) = tag("if you would proliferate, proliferate ").parse(input)?;
        let (input, count) = parse_proliferate_replacement_count.parse(input)?;
        let (input, _) = alt((tag(" instead"), tag(" times instead"))).parse(input)?;
        let (input, _) = opt(char('.')).parse(input)?;
        Ok((input, count))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let repeat_for = match count {
        QuantityExpr::Fixed { value: 1 } => None,
        other => Some(other),
    };
    let mut execute = AbilityDefinition::new(AbilityKind::Spell, Effect::Proliferate);
    if let Some(repeat) = repeat_for {
        execute.repeat_for = Some(repeat);
    }

    let mut def = ReplacementDefinition::new(ReplacementEvent::Proliferate)
        .execute(execute)
        .description(original_text.to_string());
    def.valid_player = Some(ReplacementPlayerScope::You);
    Some(def)
}

fn parse_scry_count_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    let ((effect_kind, count), rest) = nom_on_lower(lower, lower, |input| {
        let (input, _) = tag("if you would scry ").parse(input)?;
        let (input, _) = tag("a number of cards, ").parse(input)?;
        let (input, effect_kind) = alt((
            value(ScryReplacementAction::Draw, tag("draw ")),
            value(ScryReplacementAction::Scry, tag("scry ")),
        ))
        .parse(input)?;
        let (input, count) = parse_scry_replacement_count.parse(input)?;
        let (input, _) = tag(" instead").parse(input)?;
        let (input, _) = opt(char('.')).parse(input)?;
        Ok((input, (effect_kind, count)))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let effect = match effect_kind {
        ScryReplacementAction::Draw => Effect::Draw {
            count,
            target: TargetFilter::Controller,
        },
        ScryReplacementAction::Scry => Effect::Scry {
            count,
            target: TargetFilter::Controller,
        },
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::Scry)
            .execute(AbilityDefinition::new(AbilityKind::Spell, effect))
            .description(original_text.to_string()),
    )
}

/// CR 701.44 + CR 614.1a: Parse explore replacement effects such as Twists and
/// Turns ("instead you scry 1, then that creature explores") and Topography
/// Tracker ("instead it explores, then it explores again").
fn parse_explore_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    if !nom_primitives::scan_contains(lower, "if a creature you control would explore") {
        return None;
    }
    let (_, execute_text) = split_once_on_lower(original_text, lower, "instead ")?;
    let execute_text = execute_text.trim().trim_end_matches('.');

    Some(
        ReplacementDefinition::new(ReplacementEvent::Explore)
            .valid_card(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .execute(parse_effect_chain(execute_text, AbilityKind::Spell))
            .description(original_text.to_string()),
    )
}

/// CR 502.3 + CR 502.4 + CR 614.1a: untap-step replacement —
/// "If [filter] would untap during [its controller's | your] untap step,
/// [effect] instead" (Freyalise's Winds, Edge of Malacol). The `valid_card`
/// filter scopes WHICH permanent (parsed generically via `parse_type_phrase`),
/// and `ReplacementCondition::DuringUntapStep` scopes WHEN (so effect-untaps at
/// other times are unaffected). The alternative effect appears BEFORE "instead"
/// ("remove all wind counters from it instead", "put two +1/+1 counters on it
/// instead").
/// CR 614.1e + CR 708.11: "As ~ is turned face up, [effect]"
/// is a replacement effect — the alternative action applies AS the permanent is
/// turned face up (no stack-response window), and the subject is always the
/// permanent itself. Scoped to effects that resolve against the permanent itself
/// (`SelfRef`): the self-counter class — Hooded Hydra "put five +1/+1 counters on
/// it", Bubble Smuggler "put four +1/+1 counters on it". Forms that need an
/// external target choice during the turn-up (Gift of Doom "you may attach it to
/// a creature") are gapped by `turn_face_up_effect_is_self_resolving` rather than
/// mis-resolved. `norm_lower` has self-references normalized to `~`.
fn parse_turned_face_up_replacement(norm_lower: &str, text: &str) -> Option<ReplacementDefinition> {
    // Anchored self-referential lead.
    tag::<_, _, OracleError<'_>>("as ~ is turned face up, ")
        .parse(norm_lower)
        .ok()?;
    // The effect is everything after the lead; pull it from the original line so
    // `parse_effect_chain` sees the printed casing.
    let lower = text.to_lowercase();
    let (_head, effect_text) = split_once_on_lower(text, &lower, " is turned face up, ")?;
    let effect_text = effect_text.trim().trim_end_matches('.').trim();
    if effect_text.is_empty() {
        return None;
    }

    // CR 708.11: the effect applies AS the permanent is turned face up — there is
    // no point at which the controller can use the targeting system. Only effects
    // that resolve against the permanent itself (`SelfRef`, e.g. Hooded Hydra's
    // "put five +1/+1 counters on it") can be faithfully modeled at this seam.
    // Effects that need an external target choice (Gift of Doom's "you may attach
    // it to a creature") would require a turn-up-time choice the apply path does
    // not provide; gap them rather than silently mis-resolve the host.
    let execute = parse_effect_chain(effect_text, AbilityKind::Spell);
    if !turn_face_up_effect_is_self_resolving(&execute) {
        return None;
    }

    Some(
        ReplacementDefinition::new(ReplacementEvent::TurnFaceUp)
            .valid_card(TargetFilter::SelfRef)
            .execute(execute)
            .description(text.to_string()),
    )
}

/// CR 708.11: True when every effect in an "As ~ is turned face up" chain resolves
/// against the permanent itself (`SelfRef`) or needs no target at all, so it can
/// be applied during the turn-up with no targeting window. An effect whose target
/// is an external filter (a creature/permanent/player chosen at resolution) needs
/// a choice the replacement-apply path does not model and must be gapped.
fn turn_face_up_effect_is_self_resolving(ability: &AbilityDefinition) -> bool {
    let mut current = Some(ability);
    while let Some(def) = current {
        match def.effect.target_filter() {
            None | Some(TargetFilter::SelfRef) => {}
            Some(_) => return false,
        }
        current = def.sub_ability.as_deref();
    }
    true
}

fn parse_untap_step_replacement(original_text: &str, lower: &str) -> Option<ReplacementDefinition> {
    if !nom_primitives::scan_contains(lower, "untap step")
        || !nom_primitives::scan_contains(lower, "instead")
    {
        return None;
    }

    // Subject filter: between "if " and " would untap during".
    let (head, after_would) = split_once_on_lower(original_text, lower, " would untap during ")?;
    // Self-reference untap clauses ("~ would untap") are handled elsewhere.
    if head.contains('~') {
        return None;
    }
    // CR 614.1a: consume the leading "if " with a `tag` combinator, then parse
    // the subject as a typed filter (lowercase, as `parse_type_phrase` expects).
    let head_lc = head.trim().to_ascii_lowercase();
    let (subject_lc, _) = tag::<_, _, OracleError<'_>>("if ")
        .parse(head_lc.as_str())
        .ok()?;
    let (filter, subject_rest) = parse_type_phrase(subject_lc.trim());
    if matches!(&filter, TargetFilter::Any) || !subject_rest.trim().is_empty() {
        return None;
    }

    // Skip past "[its controller's | your] untap step" to the alternative effect.
    let after_would_lc = after_would.to_ascii_lowercase();
    let (_step_owner, after_step) =
        split_once_on_lower(after_would, &after_would_lc, "untap step")?;
    let after_step = after_step.trim_start_matches([',', ' ']);

    // Effect is the text before " instead".
    let after_step_lc = after_step.to_ascii_lowercase();
    let (effect_text, _) = split_once_on_lower(after_step, &after_step_lc, "instead")?;
    let effect_text = effect_text.trim().trim_end_matches([',', ' ']);
    if effect_text.is_empty() {
        return None;
    }

    Some(
        ReplacementDefinition::new(ReplacementEvent::Untap)
            .valid_card(filter)
            .condition(ReplacementCondition::DuringUntapStep)
            .execute(parse_effect_chain(effect_text, AbilityKind::Spell))
            .description(original_text.to_string()),
    )
}

#[derive(Clone, Copy)]
enum ScryReplacementAction {
    Draw,
    Scry,
}

fn parse_scry_replacement_count(input: &str) -> nom::IResult<&str, QuantityExpr, OracleError<'_>> {
    alt((
        nom::combinator::map(
            preceded(tag("that many cards plus "), nom_primitives::parse_number),
            |value| QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
                offset: value as i32,
            },
        ),
        value(
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            tag("that many cards"),
        ),
    ))
    .parse(input)
}

/// Outcome of inspecting the `"...would <verb> while <condition>,"` antecedent
/// of a replacement line. The three states are deliberately distinct: a guard
/// that is *present but unparseable* must never be silently collapsed into
/// *absent*, or a conditional replacement degrades into one that fires on every
/// event — the Jace, Wielder of Mysteries / Laboratory Maniac spurious-win
/// class. Making `Unparsed` a first-class variant forces every caller to decide
/// what to do with an unrecognized guard rather than defaulting to "ungated".
enum WhileAntecedent {
    /// No `" while ...,"` guard is attached to the verb clause.
    Absent,
    /// A guard is present and understood as a typed `ReplacementCondition`.
    Parsed(ReplacementCondition),
    /// A guard is structurally present but could not be parsed into a typed
    /// condition. Callers MUST fail closed (skip the replacement) rather than
    /// emit it unconditionally.
    Unparsed,
}

/// CR 614.1a: Inspect the "while [condition]" gate clause that appears in the
/// antecedent of a "would [verb]" replacement (between the verb phrase and the
/// comma terminating the antecedent) and lift it to a typed
/// `ReplacementCondition::OnlyIfQuantity`. `verb_anchor` is the lowercase verb
/// phrase used to locate the antecedent (e.g. "would gain life").
///
/// Returns [`WhileAntecedent::Absent`] only when there is no "while" clause at
/// all. When a "while" clause *is* present but cannot be lifted to a quantity
/// comparison (unparseable body, trailing text, or a non-quantity condition the
/// typed surface can't carry), returns [`WhileAntecedent::Unparsed`] so the
/// caller fails closed instead of emitting an unconditional replacement.
///
/// Example: "If you would gain life while you have 5 or less life, you gain
/// twice that much life instead." → `Parsed(OnlyIfQuantity { lhs: LifeTotal,
/// comparator: LE, rhs: Fixed{5}, active_player_req: None })`.
fn parse_while_antecedent(lower: &str, verb_anchor: &str) -> WhileAntecedent {
    // Locate the antecedent's "while " clause: it appears between
    // " {verb_anchor} while " and the comma terminating the antecedent.
    // Single nom combinator chain — locate verb anchor, consume gate marker,
    // capture condition body in one pass.
    let Ok((after_verb, _)) = (
        take_until::<_, _, OracleError<'_>>(verb_anchor),
        tag::<_, _, OracleError<'_>>(verb_anchor),
    )
        .parse(lower)
    else {
        return WhileAntecedent::Absent;
    };
    let Ok((_, condition_text)) = nom::sequence::preceded(
        tag::<_, _, OracleError<'_>>(" while "),
        take_until::<_, _, OracleError<'_>>(","),
    )
    .parse(after_verb) else {
        return WhileAntecedent::Absent;
    };
    // A guard clause IS present from here on; every failure path below must fail
    // closed (`Unparsed`), never `Absent`.
    let Ok((rest, condition)) = parse_inner_condition(condition_text.trim()) else {
        return WhileAntecedent::Unparsed;
    };
    if !rest.trim().is_empty() {
        return WhileAntecedent::Unparsed;
    }
    // Only QuantityComparison conditions can be carried by OnlyIfQuantity. A
    // non-quantity guard is still a real guard, so it fails closed rather than
    // leaving the replacement ungated.
    let StaticCondition::QuantityComparison {
        lhs,
        comparator,
        rhs,
    } = condition
    else {
        return WhileAntecedent::Unparsed;
    };
    WhileAntecedent::Parsed(ReplacementCondition::OnlyIfQuantity {
        lhs,
        comparator,
        rhs,
        active_player_req: None,
    })
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

/// CR 121.1 + CR 504.1 + CR 614.6: Detect the "except the first one [you|they]
/// draw in each of [your|their] draw steps" exception clause used by
/// Alhammarret's Archive (and shared in shape with Orcish Bowmasters' trigger
/// suffix; see `oracle_trigger::has_except_first_draw_in_draw_step_clause`).
///
/// The clause is composed from independent dimensions (subject pronoun,
/// possessive pronoun) so we use a single nested `alt` over each dimension
/// rather than enumerating every "you/they" × "your/their" permutation.
/// The combinator scans the text rather than anchoring at the start, since
/// the exception phrase appears mid-sentence after the "you would draw a card"
/// prefix.
pub(super) fn has_except_first_draw_in_draw_step_clause(lower: &str) -> bool {
    fn parse_clause(input: &str) -> nom::IResult<&str, (), OracleError<'_>> {
        let (input, _) = tag("except the first one ").parse(input)?;
        let (input, _) = alt((tag("you "), tag("they "))).parse(input)?;
        let (input, _) = tag("draw in each of ").parse(input)?;
        let (input, _) = alt((tag("your "), tag("their "))).parse(input)?;
        let (input, _) = tag("draw steps").parse(input)?;
        Ok((input, ()))
    }
    // Scan word-by-word so the clause can appear anywhere in the line.
    let mut remaining = lower;
    while !remaining.is_empty() {
        if parse_clause(remaining).is_ok() {
            return true;
        }
        remaining = remaining
            .find(' ')
            .map_or("", |i| remaining[i + 1..].trim_start());
    }
    false
}

/// CR 707.10 + CR 614.1a: Parse a "copy an additional time" replacement —
/// "If you would copy a spell one or more times, instead copy it that many
/// times plus an additional time. You may choose new targets for the additional
/// copy." (Twinning Staff).
///
/// Modeled as a `CopySpell` replacement carrying a `QuantityModification`,
/// mirroring the token/counter doubling family (Doubling Season, Hardened
/// Scales). Generalizes to "plus N additional times" via `parse_number`. The
/// count change is consumed by `copy_spell::copy_count_with_replacements` at the
/// copy-count site — copies are produced by the `repeat_for` loop, not the
/// `ProposedEvent` pipeline, so this replacement is queried directly rather than
/// proposed. The additional copies always permit new targets (standard wording
/// for this class), satisfied by each copy's existing retarget step.
fn parse_copy_count_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    // Require the "plus [N] additional time(s)" tail so this only matches the
    // count-increasing class, not an unrelated one-shot "copy a spell" effect.
    // Composed from modular combinators along three independent axes — count
    // (`an` => 1, else a number), the fixed `additional` token, and the
    // singular/plural `time(s)` noun — rather than enumerating full-phrase tags,
    // so "plus an additional time" and "plus N additional times" both parse.
    let additional = nom_on_lower(lower, lower, |i| {
        let (i, _) = tag(
            "if you would copy a spell one or more times, instead copy it that many times plus ",
        )
        .parse(i)?;
        let (i, n) = alt((value(1u32, tag("an")), nom_primitives::parse_number)).parse(i)?;
        let (i, _) = tag(" additional ").parse(i)?;
        let (i, _) = alt((tag("times"), tag("time"))).parse(i)?;
        Ok((i, n))
    })
    .map(|(n, _)| n)?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::CopySpell)
            .quantity_modification(QuantityModification::Plus { value: additional })
            .description(original_text.to_string()),
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
        TokenReplacementShape::Half => {
            def = def.quantity_modification(QuantityModification::Half);
        }
        TokenReplacementShape::PlusSpec { spec } => {
            def = def.additional_token_spec(*spec);
        }
        TokenReplacementShape::Substitute { token } => {
            // CR 614.1a + CR 111.1: Full token substitution (Divine Visitation).
            // Approach (A) — no new ReplacementDefinition field: carry the
            // substitute characteristics as an Effect::Token in the existing
            // `execute` field (mirrors `mill_applier` reading `def.execute`).
            // `create_token_applier` resolves it to a TokenSpec and swaps it in,
            // preserving the event's count and owner.
            def = def.execute(AbilityDefinition::new(AbilityKind::Spell, *token));
            // CR 111.1: gate the substitution on the proposed token's core card
            // type ("if one or more CREATURE tokens would be created" — only
            // creature tokens become Angels).
            if let Some(core_type) = parse_token_core_type_gate(lower) {
                def = def.condition(ReplacementCondition::TokenCoreTypeMatches {
                    core_types: vec![core_type],
                });
            }
        }
    }

    // Scope: "under your control" → restrict to controller's tokens
    if nom_primitives::scan_contains(lower, "under your control") {
        def = def.token_owner_scope(ControllerRef::You);
    }
    // Halving Season class: "If an opponent would create …"
    if nom_primitives::scan_contains(lower, "an opponent would create")
        || nom_primitives::scan_contains(lower, "opponent would create")
    {
        def = def.token_owner_scope(ControllerRef::Opponent);
    }

    Some(def)
}

enum TokenReplacementShape {
    /// "twice that many tokens … are created instead" (Doubling Season).
    Double,
    /// "half that many … tokens … instead, rounded down" (Halving Season).
    Half,
    /// "those tokens plus [spec] are created instead" (Chatterfang, Donatello).
    PlusSpec {
        spec: Box<crate::types::proposed_event::TokenSpec>,
    },
    /// "that many [spec] are created instead" — full token substitution
    /// (Divine Visitation). The substitute characteristics are carried as an
    /// `Effect::Token` (resolved to a `TokenSpec` by the applier).
    Substitute { token: Box<Effect> },
}

/// CR 614.1a: Nom dispatch on the two token-replacement shapes. Uses
/// `nom_on_lower` for case-preserving parsing and delegates token-spec
/// extraction to the existing `parse_token_description` building block.
fn parse_token_replacement_shape(lower: &str) -> Option<TokenReplacementShape> {
    // "half that many" → Halving Season token-halving pattern.
    if nom_primitives::scan_contains(lower, "half that many") {
        return Some(TokenReplacementShape::Half);
    }

    // "twice that many" → Doubling Season pattern.
    if nom_on_lower(lower, lower, |i| {
        let (i, _) = take_until::<_, _, OracleError<'_>>("twice that many").parse(i)?;
        let (i, _) = tag("twice that many").parse(i)?;
        Ok((i, ()))
    })
    .is_some()
    {
        return Some(TokenReplacementShape::Double);
    }

    // "those tokens plus <spec> (is|are) created instead" → Chatterfang / Donatello.
    if let Some(spec) = parse_token_plus_spec_shape(lower) {
        return Some(TokenReplacementShape::PlusSpec {
            spec: Box::new(spec),
        });
    }

    // "that many <spec> (is|are) created instead" → full token SUBSTITUTION
    // (Divine Visitation). Disjoint prefix from PlusSpec ("those tokens plus")
    // and gated after Double ("twice that many"), so order is unambiguous.
    if let Some(token) = parse_token_substitute_shape(lower) {
        return Some(TokenReplacementShape::Substitute {
            token: Box::new(token),
        });
    }

    None
}

/// CR 614.1a + CR 111.1: Extract the "those tokens plus <spec> (is|are) created
/// instead" appended-token spec (Chatterfang / Donatello). Returns the parsed
/// `TokenSpec` for the appended token, or `None` if the phrase is absent.
fn parse_token_plus_spec_shape(lower: &str) -> Option<crate::types::proposed_event::TokenSpec> {
    // Extract the spec descriptor between "those tokens plus " and the trailing
    // "are/is created instead" clause using nom combinators.
    let ((descriptor_start, descriptor_len), _rest) = nom_on_lower(lower, lower, |i| {
        let (i, pre) = take_until::<_, _, OracleError<'_>>("those tokens plus ").parse(i)?;
        let start_offset = pre.len() + "those tokens plus ".len();
        let (i, _) = tag("those tokens plus ").parse(i)?;
        let (_, descriptor) = alt((
            take_until::<_, _, OracleError<'_>>(" are created instead"),
            take_until::<_, _, OracleError<'_>>(" is created instead"),
        ))
        .parse(i)?;
        Ok((i, (start_offset, descriptor.len())))
    })?;

    let descriptor = lower
        .get(descriptor_start..descriptor_start + descriptor_len)?
        .trim();
    let descriptor = normalize_additional_token_descriptor(descriptor)?;
    let token = super::oracle_effect::parse_token_description(&descriptor)?;
    token_description_to_spec(&token)
}

/// CR 614.1a + CR 111.1: Extract the "that many <spec> (is|are) created instead"
/// substitution spec (Divine Visitation). The "that many " count prefix is left
/// on the descriptor so `try_parse_token` binds it to `EventContextAmount`
/// (the substitution keeps the proposed event's count at apply time, so this
/// resolved count is ignored). Returns the substitute `Effect::Token`, or `None`
/// when the phrase is absent or doesn't parse to a token effect.
fn parse_token_substitute_shape(lower: &str) -> Option<Effect> {
    let ((descriptor_start, descriptor_len), _rest) = nom_on_lower(lower, lower, |i| {
        let (i, pre) = take_until::<_, _, OracleError<'_>>("that many ").parse(i)?;
        let start_offset = pre.len();
        let (i, _) = tag("that many ").parse(i)?;
        let (_, descriptor) = alt((
            take_until::<_, _, OracleError<'_>>(" are created instead"),
            take_until::<_, _, OracleError<'_>>(" is created instead"),
        ))
        .parse(i)?;
        Ok((i, (start_offset, "that many ".len() + descriptor.len())))
    })?;

    let descriptor = lower
        .get(descriptor_start..descriptor_start + descriptor_len)?
        .trim();
    let mut ctx = ParseContext::default();
    let effect = super::oracle_effect::try_parse_token(descriptor, descriptor, &mut ctx)?;
    matches!(effect, Effect::Token { .. }).then_some(effect)
}

/// CR 111.1: Extract the gated core card type from "if one or more <core type>
/// tokens would be created" (Divine Visitation gates on Creature). Returns
/// `None` when no typed gate is present (e.g. Doubling Season's untyped "one or
/// more tokens").
fn parse_token_core_type_gate(lower: &str) -> Option<crate::types::card_type::CoreType> {
    let (word, _) = nom_on_lower(lower, lower, |i| {
        let (i, _) = take_until::<_, _, OracleError<'_>>("one or more ").parse(i)?;
        let (i, _) = tag("one or more ").parse(i)?;
        let (i, word) = take_until::<_, _, OracleError<'_>>(" tokens would be created").parse(i)?;
        Ok((i, word.to_string()))
    })?;
    // The line is lowercased; `CoreType::from_str` expects title case ("Creature").
    crate::types::card_type::CoreType::from_str(&capitalize_first(word.trim())).ok()
}

/// CR 614.1a + CR 111.1: Normalize the optional "additional" modifier on
/// token descriptors before delegating to `parse_token_description`, whose token
/// grammar expects an article or numeric count prefix.
fn normalize_additional_token_descriptor(descriptor: &str) -> Option<String> {
    let (rest, stripped_additional) = opt(value(
        (),
        preceded(
            opt(alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))),
            tag("additional "),
        ),
    ))
    .parse(descriptor)
    .ok()?;
    let descriptor = rest.trim();
    if descriptor.is_empty() {
        return None;
    }
    if stripped_additional.is_some() {
        let (_, article) = peek(opt(alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))))
            .parse(descriptor)
            .ok()?;
        if article.is_none() {
            return Some(format!("a {descriptor}"));
        }
    }
    Some(descriptor.to_string())
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
        let (i, pre) = take_until::<_, _, OracleError<'_>>("would create one or more ").parse(i)?;
        let start_offset = pre.len() + "would create one or more ".len();
        let (i, _) = tag("would create one or more ").parse(i)?;
        let (_, subtype) = take_until::<_, _, OracleError<'_>>(" tokens,").parse(i)?;
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

    let spec = parse_instead_create_those_tokens_plus_spec(lower)?;

    // Capitalize the subtype to match the parser's existing convention
    // (TokenSpec.subtypes uses title-case: "Treasure", not "treasure").
    let canonical_subtype = canonicalize_subtype(subtype_phrase);

    Some(
        ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .condition(ReplacementCondition::TokenSubtypeMatches {
                subtypes: vec![canonical_subtype],
            })
            // CR 614.1a + CR 109.5: "If *you* would create..." scopes the
            // replacement to the source's controller — it must not fire for
            // tokens created by other players (issue #1967).
            .token_owner_scope(ControllerRef::You)
            .additional_token_spec(spec)
            .description(original_text.to_string()),
    )
}

/// CR 614.1a + CR 111.1: Tippy-Toe class — generic additional token without subtype gate.
fn parse_generic_additional_token_replacement(
    lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    if !nom_primitives::scan_contains(lower, "would create one or more tokens") {
        return None;
    }
    let spec = parse_instead_create_those_tokens_plus_spec(lower)?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::CreateToken)
            // CR 614.1a + CR 109.5: "If you would create..." scopes this
            // replacement to the source's controller, without Xorn's subtype gate.
            .token_owner_scope(ControllerRef::You)
            .additional_token_spec(spec)
            .description(original_text.to_string()),
    )
}

/// CR 614.1a + CR 111.1: Extract the appended token spec from the
/// "instead create those tokens plus ..." wording shared by Xorn- and
/// Tippy-Toe-class replacement effects.
fn parse_instead_create_those_tokens_plus_spec(
    lower: &str,
) -> Option<crate::types::proposed_event::TokenSpec> {
    let total_len = lower.len();
    let ((descriptor_start, descriptor_len), _) = nom_on_lower(lower, lower, |i| {
        let (i, _) =
            take_until::<_, _, OracleError<'_>>("instead create those tokens plus ").parse(i)?;
        let (i, _) = tag("instead create those tokens plus ").parse(i)?;
        let start_offset = total_len - i.len();
        let (i, descriptor) = alt((
            take_until::<_, _, OracleError<'_>>("."),
            nom::combinator::rest,
        ))
        .parse(i)?;
        Ok((i, (start_offset, descriptor.len())))
    })?;

    let descriptor = lower
        .get(descriptor_start..descriptor_start + descriptor_len)?
        .trim();
    let descriptor = normalize_additional_token_descriptor(descriptor)?;
    let token = super::oracle_effect::parse_token_description(&descriptor)?;
    token_description_to_spec(&token)
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
        let (i, _) = take_until::<_, _, OracleError<'_>>("would create a ").parse(i)?;
        let (i, _) = tag("would create a ").parse(i)?;
        let start_offset = total_len - i.len();
        let (i, list) = take_until::<_, _, OracleError<'_>>(" token,").parse(i)?;
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
            // CR 614.1a + CR 109.5: "If *you* would create..." scopes the
            // replacement to the source's controller — it must not fire for
            // tokens created by other players (issue #1967).
            .token_owner_scope(ControllerRef::You)
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

    fn entry(i: &str) -> IResult<&str, &str, OracleError<'_>> {
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
    token: &crate::parser::oracle_ir::ast::TokenDescription,
) -> Option<crate::types::proposed_event::TokenSpec> {
    use crate::types::ability::PtValue;
    use crate::types::card_type::CoreType;
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
        characteristics: crate::types::proposed_event::TokenCharacteristics {
            display_name: token.name.clone(),
            power,
            toughness,
            core_types,
            subtypes,
            // CR 205.4a: Carry parsed supertypes (legendary/snow) onto the
            // appended-token spec rather than dropping them.
            supertypes: token.supertypes.clone(),
            colors: token.colors.clone(),
            keywords: token.keywords.clone(),
        },
        script_name: token.name.clone(),
        static_abilities: token.static_abilities.clone(),
        enter_with_counters: Vec::new(),
        tapped: token.tapped,
        enters_attacking: false,
        sacrifice_at: None,
        // Placeholder: overwritten at apply time with the replacement source's identity.
        source_id: crate::types::identifiers::ObjectId(0),
        controller: crate::types::player::PlayerId(0),
        // Replacement-created tokens ("instead, create a token") are not the
        // "attached to" Aura/Role class; that path flows through `Effect::Token`.
        attach_to: None,
    })
}

/// CR 614.1a: Parse counter addition replacement effects.
/// Handles "twice that many ... counters" (Primal Vigor, Doubling Season),
/// "that many plus N ... counters" (Hardened Scales, Branching Evolution),
/// and "that many ... counters minus N" (Vizier of Remedies). The runtime
/// applier saturates at 0 because counters are markers per CR 122.1 — you
/// can't put a negative number of markers on a permanent — and the
/// -1/-1-specific P/T semantics live in CR 122.1a / CR 613.4c.
/// CR 107.14 + CR 614.1a: Izzet Generatorium — additional {E} on would-get events.
fn parse_energy_get_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    all_consuming(value(
        (),
        (
            tag::<_, _, OracleError<'_>>("if you would get one or more {e}, "),
            tag("you get an additional {e} instead."),
        ),
    ))
    .parse(lower)
    .ok()?;

    let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .quantity_modification(QuantityModification::Plus { value: 1 })
        .description(original_text.to_string());
    def.valid_player = Some(ReplacementPlayerScope::You);
    Some(def)
}
fn parse_counter_replacement(lower: &str, original_text: &str) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    let modification = if nom_primitives::scan_contains(lower, "half that many") {
        QuantityModification::Half
    } else if nom_primitives::scan_contains(lower, "twice that many") {
        QuantityModification::Double
    } else if let Some(rest) = strip_after(lower, "that many plus ") {
        // "that many plus one ... counters are put on it instead"
        // Delegate to nom_primitives::parse_number (input already lowercase)
        let (_rem, value) = nom_primitives::parse_number.parse(rest).ok()?;
        QuantityModification::Plus { value }
    } else if let Some(rest) = strip_after(lower, "that many minus ") {
        // "that many minus one ... counters are put on it instead"
        // Direct "minus" form — symmetric to the "plus" form above.
        let (_rem, value) = nom_primitives::parse_number.parse(rest).ok()?;
        QuantityModification::Minus { value }
    } else {
        // Vizier of Remedies form: "that many <type> counters minus one are put on it instead".
        // The "minus N" follows the counter-type token rather than preceding it.
        let rest = strip_after(lower, " counters minus ")?;
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
    if nom_primitives::scan_contains(lower, "an opponent would put")
        || nom_primitives::scan_contains(lower, "opponent would put")
    {
        def.valid_player = Some(ReplacementPlayerScope::Opponent);
    }

    // CR 122.1a + CR 614.1a: When the Oracle text names a specific counter type
    // ("+1/+1 counters", "-1/-1 counters", "loyalty counters", …), restrict the
    // replacement to that counter type so Hardened Scales doesn't fire on -1/-1
    // counter additions and Vizier of Remedies doesn't fire on +1/+1 counter
    // additions. Counter-agnostic wordings ("those counters" — Doubling Season)
    // leave `counter_match = None`, preserving the legacy any-counter behavior.
    if let Some(ct) = extract_replacement_counter_type(lower) {
        def = def.counter_match(CounterMatch::OfType(ct));
    }

    Some(def)
}

/// CR 614.17 + CR 614.6 + CR 122.1: Parse "Players can't get counters."
/// into a global player-counter prohibition. The runtime models the "can't"
/// event through `ReplacementEvent::AddCounter` with `valid_player` scope so
/// player-counter effects are suppressed before mutating player state.
fn parse_global_player_counter_prohibition(
    lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let mut combinator = all_consuming(terminated(
        tag::<_, _, OracleError<'_>>("players can't get counters"),
        opt(tag(".")),
    ));
    combinator.parse(lower.trim()).ok()?;

    let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .quantity_modification(QuantityModification::Prevent)
        .description(original_text.to_string());
    def.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
    Some(def)
}

/// CR 614.17 + CR 614.6 + CR 122.1: Parse global object-counter prohibitions
/// such as "Counters can't be put on artifacts, creatures, enchantments, or
/// lands." into an `AddCounter` prevention scoped to the named type list.
fn parse_global_object_counter_prohibition(
    lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let mut combinator = all_consuming(terminated(
        preceded(
            tag::<_, _, OracleError<'_>>("counters can't be put on "),
            separated_list1(
                parse_counter_prohibition_type_separator,
                parse_counter_prohibition_type,
            ),
        ),
        opt(tag(".")),
    ));
    let (_rest, type_filters) = combinator.parse(lower.trim()).ok()?;
    let type_filter = match type_filters.as_slice() {
        [single] => single.clone(),
        _ => TypeFilter::AnyOf(type_filters),
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(attach_zone_to_filter(
                TargetFilter::Typed(TypedFilter::new(type_filter)),
                Zone::Battlefield,
            ))
            .quantity_modification(QuantityModification::Prevent)
            .description(original_text.to_string()),
    )
}

/// CR 614.17 + CR 614.6 + CR 122.1: Parse inverted type-scoped counter
/// prohibitions such as "Creatures can't have counters put on them." Lowers to
/// the same `AddCounter` + `Prevent` replacement as Solemnity's object-counter
/// line, scoped to a single permanent type on the battlefield.
fn parse_inverted_typed_counter_prohibition(
    lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // Inverted surface form of `parse_global_object_counter_prohibition`: the
    // permanent type is the grammatical subject ("Creatures can't have counters
    // put on them") rather than the object ("Counters can't be put on
    // creatures"). Same replacement class, so it reuses the shared type-list
    // combinators and covers every permanent type (and comma/or-separated
    // lists) in one arm.
    let mut combinator = all_consuming(terminated(
        terminated(
            separated_list1(
                parse_counter_prohibition_type_separator,
                parse_counter_prohibition_type,
            ),
            tag::<_, _, OracleError<'_>>(" can't have counters put on them"),
        ),
        opt(tag(".")),
    ));
    let (_rest, type_filters) = combinator.parse(lower.trim()).ok()?;
    let type_filter = match type_filters.as_slice() {
        [single] => single.clone(),
        _ => TypeFilter::AnyOf(type_filters),
    };

    Some(
        ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(attach_zone_to_filter(
                TargetFilter::Typed(TypedFilter::new(type_filter)),
                Zone::Battlefield,
            ))
            .quantity_modification(QuantityModification::Prevent)
            .description(original_text.to_string()),
    )
}

fn parse_counter_prohibition_type_separator(input: &str) -> OracleResult<'_, &str> {
    alt((tag(", or "), tag(", "), tag(" or "))).parse(input)
}

fn parse_counter_prohibition_type(input: &str) -> OracleResult<'_, TypeFilter> {
    let (rest, type_filter) = parse_type_filter_word(input)?;
    match type_filter {
        TypeFilter::Artifact
        | TypeFilter::Creature
        | TypeFilter::Enchantment
        | TypeFilter::Land
        | TypeFilter::Planeswalker
        | TypeFilter::Battle => Ok((rest, type_filter)),
        _ => Err(oracle_err(input)),
    }
}

/// CR 122.1a + CR 614.1a: Extract the counter-type token named in a counter
/// replacement's Oracle text. Anchors on the "one or more <type> counter[s]"
/// phrase that scopes the replaced event to a specific counter type and
/// delegates the type token to `parse_counter_type_typed` (the single nom
/// authority for counter type recognition). Returns `None` for counter-
/// agnostic wordings such as Doubling Season's "if an effect would put one
/// or more counters on a permanent you control" — in that case the
/// replacement applies to every counter type, matching the printed behavior.
fn extract_replacement_counter_type(lower: &str) -> Option<CounterType> {
    // Compose nom end-to-end:
    //   <any prefix> "one or more " <counter-type-token> " counter"[s]
    // The leading `take_until("one or more ")` advances to the anchor without
    // delegating to `str::find` for parsing dispatch. The trailing-noun guard
    // (` counter` / ` counters`) prevents a counter-agnostic phrasing
    // ("one or more counters") from accidentally consuming a recognized
    // counter-type stem (e.g. the named-counter list contains "stun").
    let mut combinator = (
        take_until::<_, _, OracleError<'_>>("one or more "),
        tag("one or more "),
        nom_primitives::parse_counter_type_typed,
        alt((tag(" counters"), tag(" counter"))),
    )
        .map(|(_, _, ct, _): (&str, &str, CounterType, &str)| ct);
    combinator.parse(lower).ok().map(|(_rest, ct)| ct)
}

/// CR 113.6i + CR 614.17 + CR 614.6 + CR 614.7 + CR 122.1: Parse a self-targeted
/// counter-prohibition replacement effect.
///
/// CR 113.6i is the authorizing rule: "An object's ability that states
/// counters can't be put on that object functions as that object is entering
/// the battlefield in addition to functioning while that object is on the
/// battlefield." CR 614.17 is the "can't" effects framework — these aren't
/// strictly replacement effects, but follow similar rules — which is why we
/// model the prohibition through the replacement pipeline. CR 614.6/614.7
/// describe the resulting "event never happens" outcome; CR 122.1 names the
/// counter-placement event the prohibition suppresses.
///
/// Recognizes:
/// - `~ can't have counters put on it.` (Melira's Keepers — Human Scout)
/// - `~ can't have counters put on them.` (plural-pronoun variants)
///
/// The engine produces a `ReplacementDefinition` keyed on
/// `ReplacementEvent::AddCounter` with `valid_card: SelfRef` and
/// `quantity_modification: Some(QuantityModification::Prevent)`, so
/// `add_counter_applier` short-circuits to `ApplyResult::Prevented` (CR
/// 614.6: replaced events never happen) and `apply_counter_addition` is
/// never reached.
///
/// `norm_lower` is the lowercased, self-ref-normalized text (i.e. "this
/// creature" → "~"). `original_text` is the unmodified Oracle line used for
/// the `description` field.
///
/// The combinator is composed end-to-end with nom: a typed `alt` over the
/// two pronoun variants ("on it" / "on them") gated by the fixed
/// "~ can't have counters put " prefix, followed by an optional trailing
/// period. No `find()`/`split_once()`/`contains()` for dispatch — the
/// classifier-level `scan_contains` only routes the line to this parser;
/// the parser itself uses nom combinators throughout.
fn parse_no_counters_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    use crate::types::ability::QuantityModification;

    // The parser receives normalized text where "this creature" / "this
    // permanent" etc. have already been replaced by `~` (engine-internal
    // self-reference convention; CR 201.5 governs the underlying "object
    // refers to itself by name" semantics). `all_consuming` enforces that
    // the combinator matches the entire line as a single shape so adjacent
    // text (e.g., an "as long as ~ is tapped" prefix) is correctly
    // rejected — those compose via the outer dispatch in
    // `parse_replacement_line_inner`, not here. `terminated(.., opt(tag(".")))`
    // absorbs the optional trailing period inside the combinator, keeping
    // the entire dispatch in idiomatic nom.
    let mut combinator = all_consuming(terminated(
        (
            tag::<_, _, OracleError<'_>>("~ can't have counters put on "),
            alt((tag("it"), tag("them"))),
        ),
        opt(tag(".")),
    ));
    combinator.parse(norm_lower.trim()).ok()?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .valid_card(TargetFilter::SelfRef)
            .quantity_modification(QuantityModification::Prevent)
            .description(original_text.to_string()),
    )
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
            Some(damage_target_controller())
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
                .damage_target_filter(damage_target_controller())
                .redirect_target(TargetFilter::SelfRef)
                .description(original_text.to_string()),
        );
    }

    None
}

/// CR 614.1a + CR 120.6 + CR 510.2: Wolverine, Fierce Fighter — "If damage
/// would be dealt to ~, instead that damage is dealt, but all other damage
/// already dealt to him is healed."
///
/// Unlike the generic self-instead followup, the new damage instance is NOT
/// prevented (it IS dealt); only the receiver's PRIOR marked damage is cleared.
/// Emits a `DealtDamage` replacement with NO prevention shield, carrying an
/// `Effect::RemoveAllDamage { SelfRef }` in `execute`. `dealt_damage_applier`
/// runs the heal in Phase B (before delivery), so same-batch combat instances
/// (CR 510.2) are preserved while prior damage clears.
///
/// The receiver self-reference may surface as `~` (the source's own normalized
/// name) or an object pronoun (`him`/`her`/`it`/`them`), which `~`-normalization
/// does not rewrite — both are accepted via a shared `alt`.
fn parse_damage_heal_self_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    fn self_ref(i: &str) -> OracleResult<'_, &str> {
        alt((
            tag::<_, _, OracleError<'_>>("~"),
            tag("him"),
            tag("her"),
            tag("it"),
            tag("them"),
        ))
        .parse(i)
    }

    let mut combinator = all_consuming(terminated(
        (
            tag::<_, _, OracleError<'_>>("if damage would be dealt to "),
            self_ref,
            tag(", instead that damage is dealt, but all other damage already dealt to "),
            self_ref,
            tag(" is healed"),
        ),
        opt(char('.')),
    ));
    combinator.parse(norm_lower.trim()).ok()?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::DealtDamage)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::RemoveAllDamage {
                    target: TargetFilter::SelfRef,
                },
            ))
            .description(original_text.to_string()),
    )
}

fn parse_damage_to_self_instead_followup(
    norm_lower: &str,
    normalized: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let total_len = norm_lower.len();
    let ((effect_start, effect_len), rest) = nom_on_lower(normalized, norm_lower, |i| {
        let (i, _) = tag("if damage would be dealt to ").parse(i)?;
        let (i, _) = alt((tag("~"), tag("you"))).parse(i)?;
        let (i, _) = tag(", ").parse(i)?;
        let effect_start = total_len - i.len();
        let (i, effect) = take_until::<_, _, OracleError<'_>>(" instead").parse(i)?;
        let (i, _) = tag(" instead").parse(i)?;
        let (i, _) = opt(char('.')).parse(i)?;
        Ok((i, (effect_start, effect.len())))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let effect_text = normalized.get(effect_start..effect_start + effect_len)?;
    let mut ctx = ParseContext {
        subject: Some(TargetFilter::SelfRef),
        in_replacement: true,
        ..ParseContext::default()
    };
    let followup = parse_effect_chain_with_context(effect_text, AbilityKind::Spell, &mut ctx);

    Some(
        ReplacementDefinition::new(ReplacementEvent::DealtDamage)
            .prevention_shield(PreventionAmount::All)
            .execute(followup)
            .description(original_text.to_string()),
    )
}

fn parse_damage_to_player_instead_followup(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    let total_len = norm_lower.len();
    let ((effect_start, effect_len), rest) = nom_on_lower(original_text, norm_lower, |i| {
        let (i, _) = tag("if damage would be dealt to a player, ").parse(i)?;
        let effect_start = total_len - i.len();
        let (i, _) = alt((tag("that player "), tag("the player "))).parse(i)?;
        let (i, _) = take_until::<_, _, OracleError<'_>>(" instead").parse(i)?;
        let effect_end = total_len - i.len();
        let (i, _) = tag(" instead").parse(i)?;
        let (i, _) = opt(char('.')).parse(i)?;
        Ok((i, (effect_start, effect_end - effect_start)))
    })?;
    if !rest.trim().is_empty() {
        return None;
    }

    let effect_text = original_text.get(effect_start..effect_start + effect_len)?;
    let mut followup = parse_effect_chain(effect_text, AbilityKind::Spell);
    rewrite_damage_recipient_to_post_replacement_target(&mut followup);

    Some(
        ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::All)
            .damage_target_filter(damage_target_any_player())
            .execute(followup)
            .description(original_text.to_string()),
    )
}

/// CR 614.1a: Strip a leading "as long as <condition>, " gate from a damage
/// prevention replacement's normalized lowercase text and lift it to a typed
/// `ReplacementCondition`. Returns the trimmed slice plus the gate (or the
/// untouched input and `None` when no parseable gate is present).
///
/// Shares `replacement_condition_from_static` with `parse_source_state_external_entry`
/// so any condition shape the static-condition lifter supports — quantity
/// comparisons (party-size, opponents-count, life), `SourceIsTapped`,
/// `Not(SourceIsTapped)` — flows through unchanged.
///
/// When the prefix is present but the body fails to parse or doesn't lift to a
/// supported `ReplacementCondition`, the function returns the untouched input
/// and `None`. The caller continues with the original text rather than failing
/// — preserving prior coverage for prevention lines whose gate the typed
/// surface can't yet carry (still applies the description-based shield, same
/// as before this gate-extraction was added).
fn strip_as_long_as_prefix_for_prevention(
    norm_lower: &str,
) -> (&str, Option<ReplacementCondition>) {
    let parsed = (|| -> Option<(&str, ReplacementCondition)> {
        let (rest, _) = tag::<_, _, OracleError<'_>>("as long as ")
            .parse(norm_lower)
            .ok()?;
        let (rest, static_cond) = parse_inner_condition(rest).ok()?;
        let (rest, _) = tag::<_, _, OracleError<'_>>(", ").parse(rest).ok()?;
        let rc = replacement_condition_from_static(static_cond)?;
        Some((rest, rc))
    })();
    match parsed {
        Some((rest, rc)) => (rest, Some(rc)),
        None => (norm_lower, None),
    }
}

/// CR 615: Parse damage prevention replacement effects.
/// Handles:
/// - "prevent all combat damage that would be dealt [this turn]" (Fog, Moments Peace)
/// - "prevent all damage that would be dealt to you [this turn]" (Hallow)
/// - "prevent the next N damage that would be dealt to [target] this turn" (Mending Hands)
/// - "prevent all damage that would be dealt to and dealt by [creature]" (Stonehorn Dignitary)
/// - "prevent all damage that would be dealt to enchanted/equipped creature" — scoped via
///   `valid_card` with `EnchantedBy`/`EquippedBy` so only damage to the attached creature
///   is prevented (Inviolability, General's Kabuto, Magebane Armor, Artifact Ward, Multiclass Baldric).
/// - Optional leading "as long as <condition>, " gate (CR 614.1a) — Multiclass Baldric's
///   "As long as you have a full party, prevent all damage that would be dealt to equipped creature."
fn parse_damage_prevention_replacement(
    norm_lower: &str,
    original_text: &str,
) -> Option<ReplacementDefinition> {
    // CR 614.1a: An "as long as <cond>, " prefix on a prevention replacement gates
    // the shield itself, not its post-replacement followup. Strip the gate first
    // and lift it to a typed `ReplacementCondition` so the rest of the parser
    // operates on the bare prevention clause. Shares `replacement_condition_from_static`
    // with `parse_source_state_external_entry` and other "as long as" callers.
    let (working_lower, prefix_condition) = strip_as_long_as_prefix_for_prevention(norm_lower);

    // Must contain "prevent" and "damage" to be a prevention pattern
    if !nom_primitives::scan_contains(working_lower, "prevent")
        || !nom_primitives::scan_contains(working_lower, "damage")
    {
        return None;
    }

    // "damage can't be prevented" is NOT a prevention replacement -- it's a restriction.
    if nom_primitives::scan_contains(working_lower, "can't be prevented") {
        return None;
    }

    // CR 615: "sources of the color of your choice" requires interactive color choice —
    // handled as a Choose → PreventDamage spell effect chain, not a passive replacement.
    if nom_primitives::scan_contains(working_lower, "color of your choice") {
        return None;
    }

    // Redirection patterns ("prevent that damage. ~ deals that much damage to") are handled
    // by parse_damage_redirection_replacement — don't intercept them here.
    if nom_primitives::scan_contains(working_lower, "prevent that damage")
        && nom_primitives::scan_contains(working_lower, "deals that much damage")
    {
        return None;
    }
    // "is dealt to ~ instead" patterns are also redirections, not pure prevention
    if nom_primitives::scan_contains(working_lower, "is dealt to")
        && nom_primitives::scan_contains(working_lower, "instead")
    {
        return None;
    }

    // --- 1. Extract prevention amount ---
    // CR 615.7: "prevent the next N damage" → specific shield amount
    // CR 615.1a: "prevent all damage" → prevent everything
    let amount = if nom_primitives::scan_contains(working_lower, "prevent all") {
        PreventionAmount::All
    } else if let Some(rest) = strip_after(working_lower, "prevent the next ") {
        // Uses oracle_util::parse_number (not nom directly) because it handles "X" → 0
        // for cards like Temper, Acolyte's Reward, etc.
        let (n, _) = parse_number(rest)?;
        PreventionAmount::Next(n)
    } else if nom_primitives::scan_contains(working_lower, "prevent that damage") {
        // "prevent that damage" in redirection context — redirect handled separately
        PreventionAmount::All
    } else {
        return None;
    };

    // --- 2. Extract combat scope ---
    // CR 615: "combat damage" restricts to combat damage only.
    // Longest-match-first: "noncombat damage" before "combat damage" because
    // "noncombat" contains the substring "combat".
    let combat_scope = if nom_primitives::scan_contains(working_lower, "noncombat damage") {
        Some(CombatDamageScope::NoncombatOnly)
    } else if nom_primitives::scan_contains(working_lower, "combat damage") {
        Some(CombatDamageScope::CombatOnly)
    } else {
        None
    };

    // --- 3. Extract damage target filter ---
    // "to you" → player only, "to target creature" → creature only
    let damage_target_filter = if nom_primitives::scan_contains(working_lower, "dealt to you")
        || nom_primitives::scan_contains(working_lower, "deal to you")
    {
        Some(damage_target_controller())
    } else if nom_primitives::scan_contains(working_lower, "dealt to target creature") {
        Some(DamageTargetFilter::CreatureOnly)
    } else {
        // "prevent all combat damage" with no target → any target
        None
    };

    // CR 301.5 + CR 303.4 + CR 615.1a: Damage prevention scoped to the source's
    // attached creature ("equipped creature" / "enchanted creature"). The dedicated
    // `DamageTargetFilter` enum can't express attachment relationships (it covers
    // only player/creature type axes), so route through `valid_card`
    // — the runtime resolves `EquippedBy`/`EnchantedBy` against the source's own
    // `attached_to` (see `game/filter.rs` `FilterProp::EquippedBy`), correctly
    // scoping the shield to only the attached creature regardless of how many
    // other creatures are on the battlefield. Without this, the falls-through to
    // `damage_target_filter = None` caused the shield to prevent ALL damage to
    // any target, which was the Multiclass Baldric / Inviolability / Artifact Ward
    // class of bug.
    let valid_card_filter: Option<TargetFilter> = if nom_primitives::scan_contains(working_lower, "dealt to ~")
            || nom_primitives::scan_contains(working_lower, "dealt to and dealt by ~")
            // CR 615.1a: Subject-first self-recipient form — "If ~ would be dealt
            // damage, prevent that damage ..." (Unbreathing Horde — issue #2888).
            // `~` is the source card, so the shield is self-scoped; without
            // `SelfRef` `valid_card` stays None and the shield wrongly prevents
            // ALL damage (including damage dealt to players).
            || nom_primitives::scan_contains(working_lower, "~ would be dealt")
            || nom_primitives::scan_contains(working_lower, "this creature would be dealt")
    {
        // CR 615.1a: Self-scoped prevention ("If damage would be dealt to ~")
        // must gate on `valid_card: SelfRef`, not a broad creature damage filter.
        Some(TargetFilter::SelfRef)
    } else {
        nom_primitives::scan_at_word_boundaries(working_lower, |input| {
            preceded(
                tag::<_, _, OracleError<'_>>("dealt to "),
                terminated(
                    parse_attached_subject_target_filter,
                    alt((value((), eof), value((), multispace1), value((), tag(".")))),
                ),
            )
            .parse(input)
        })
        .or_else(|| parse_damage_recipient_valid_card_filter(working_lower))
    };

    // --- 4. Extract damage source filter ---
    let damage_source_filter = parse_damage_source_filter(working_lower);

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
    // Capture whether the recipient filter was event-driven (typed
    // `valid_card`) before moving it onto `def` — the follow-up rewrite
    // below uses this signal to distinguish the Vigor cohort (rewrite
    // `ParentTarget` → `PostReplacementDamageTarget`) from the spell-driven
    // cohort (keep `ParentTarget` for the real spell target).
    let recipient_is_event_filter = valid_card_filter.is_some();
    if let Some(vc) = valid_card_filter {
        def = def.valid_card(vc);
    }
    if let Some(cond) = prefix_condition {
        def = def.condition(cond);
    }

    // CR 615.5: A prevention effect may include an additional effect referring to
    // the prevented amount ("Put a -1/-1 counter on ~ for each 1 damage prevented
    // this way", "Create N tokens for each 1 damage prevented this way"). Parse
    // the trailing sentence and attach it as the replacement's `execute` ability,
    // which the runtime fires as a post-replacement follow-up after the shield
    // consumes the damage. Class members: Phyrexian Hydra, Vigor, Stormwild
    // Capridor, Hostility.
    if let Some(followup) = extract_prevention_followup(original_text) {
        // CR 608.2k: Static self-prevention replacements split into two
        // anaphor cohorts depending on what the rider counter/effect targets:
        //
        // 1. Rider targets the shield-bearing permanent itself (Anti-Venom,
        //    Phyrexian Hydra, Stormwild Capridor, Hostility). The rider's
        //    bare pronouns ("him"/"it"/"this creature"/"this enchantment"/
        //    "~") must bind to `SelfRef` so the counter lands on the source.
        //    Threading `subject: SelfRef` makes `resolve_pronoun_target`
        //    return `SelfRef` per its typed-subject carve-out.
        //
        // 2. Rider targets the prevented event's damage recipient (Vigor:
        //    "If damage would be dealt to another creature you control,
        //    prevent that damage. Put a +1/+1 counter on that creature ..."
        //    — "that creature" is the recipient, not the source). The rider
        //    parser lowers "that creature" to `TargetFilter::ParentTarget`
        //    by the generic CR 608.2c anaphor path, but there is no parent
        //    target slot in a passive replacement context, so the binding
        //    is dangling. Post-parse rewrite (below) remaps it to
        //    `PostReplacementDamageTarget`. Cohort 2 is detected by the
        //    presence of a typed `valid_card` recipient filter — that's the
        //    structural signal that the shield is event-driven (no spell
        //    target), so any `ParentTarget` in the rider can only refer to
        //    the event recipient.
        let mut followup_ctx = ParseContext {
            subject: Some(TargetFilter::SelfRef),
            in_replacement: true,
            ..ParseContext::default()
        };
        let mut followup_def =
            parse_effect_chain_with_context(&followup, AbilityKind::Spell, &mut followup_ctx);
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
        // CR 615.5 + CR 608.2c: Object-anaphor rewrite for cohort 2 (Vigor
        // class). When the shield is event-driven (signalled by a typed
        // `valid_card_filter`), `ParentTarget` in the rider can only refer
        // to the prevented event's damage recipient — there is no parent
        // target slot. Remap dangling `ParentTarget` to
        // `PostReplacementDamageTarget` so the runtime resolves it against
        // `state.post_replacement_event_target`. Spell-driven prevention
        // (Test of Faith — "prevent the next 3 damage that would be dealt to
        // target creature this turn") has `valid_card_filter = None` because
        // its all-consuming recipient terminator fails, so this rewrite
        // does not fire and `ParentTarget` correctly inherits the spell's
        // chosen target.
        if recipient_is_event_filter {
            rewrite_parent_target_to_post_replacement_damage_target(&mut followup_def);
        }
        def = def.execute(followup_def);
    }

    Some(def)
}

/// CR 614.1a: Extract the typed event-recipient filter from a damage-prevention
/// shield's "dealt to <filter>" clause. The clause may close at the end of the
/// sentence (`.`, `this turn`, `until end of turn`, or input end) or continue
/// into a sibling prevention imperative (`, prevent that damage. ...` — Vigor,
/// Phyrexian Hydra, Stormwild Capridor class of static prevention shields with
/// follow-up rider). The `peek(", prevent")` boundary keeps the filter scoped
/// to the recipient phrase without consuming the comma + imperative, leaving
/// the follow-up extractor (`extract_prevention_followup`) to claim it.
fn parse_damage_recipient_valid_card_filter(working_lower: &str) -> Option<TargetFilter> {
    nom_primitives::scan_at_word_boundaries(working_lower, |input| {
        let (after_to, _) = tag::<_, _, OracleError<'_>>("dealt to ").parse(input)?;
        let (filter, rest) = parse_type_phrase(after_to);
        if matches!(filter, TargetFilter::Any) {
            return Err(nom::Err::Error(OracleError::new(
                after_to,
                nom::error::ErrorKind::Verify,
            )));
        }

        let rest = rest.trim_start();
        let fully_consumed = all_consuming(alt((
            value((), eof::<&str, OracleError<'_>>),
            value((), tag::<_, _, OracleError<'_>>(".")),
            value(
                (),
                terminated(
                    tag::<_, _, OracleError<'_>>("this turn"),
                    opt(tag::<_, _, OracleError<'_>>(".")),
                ),
            ),
            value(
                (),
                terminated(
                    tag::<_, _, OracleError<'_>>("until end of turn"),
                    opt(tag::<_, _, OracleError<'_>>(".")),
                ),
            ),
        )))
        .parse(rest)
        .is_ok();
        // CR 614.1a + CR 615.5: A static prevention shield with a same-sentence
        // imperative ("if damage would be dealt to <filter>, prevent that damage")
        // closes the recipient phrase at the clause boundary `, prevent`, not at
        // sentence end. `peek` acknowledges the boundary without consuming so
        // the follow-up extractor still claims the imperative and its rider.
        let clause_boundary = peek(tag::<_, _, OracleError<'_>>(", prevent"))
            .parse(rest)
            .is_ok();
        if fully_consumed || clause_boundary {
            Ok((rest, filter))
        } else {
            Err(nom::Err::Error(OracleError::new(
                rest,
                nom::error::ErrorKind::Verify,
            )))
        }
    })
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

/// CR 615.5 + CR 608.2c: In a prevention follow-up whose shield is event-driven
/// (Vigor class: "If damage would be dealt to <typed filter>, prevent that
/// damage. Put a +1/+1 counter on that creature ..."), the rider's anaphor
/// "that creature" refers to the prevented event's damage recipient. The
/// ordinary `parse_target` path lowers "that <type phrase>" to
/// `TargetFilter::ParentTarget` per CR 608.2c, but in a passive replacement
/// there is no parent target slot to bind against. Rewrite each dangling
/// `ParentTarget` to `PostReplacementDamageTarget` so the runtime resolves
/// it against `state.post_replacement_event_target`.
///
/// Sibling of `rewrite_damage_recipient_to_post_replacement_target` which
/// handles the player-anaphor cohort ("that player draws cards ..."). Kept
/// separate so the player walker stays scoped to player refs and this walker
/// only fires when the caller has confirmed the shield is event-driven (via
/// a typed `valid_card_filter` signal) — spell-driven prevention with a real
/// `target creature` slot must keep its `ParentTarget` binding intact (Test
/// of Faith).
fn rewrite_parent_target_to_post_replacement_damage_target(def: &mut AbilityDefinition) {
    super::oracle_effect::each_target_filter_mut(&mut def.effect, &mut |f| {
        if matches!(f, TargetFilter::ParentTarget) {
            *f = TargetFilter::PostReplacementDamageTarget;
        }
    });
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_parent_target_to_post_replacement_damage_target(sub);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        rewrite_parent_target_to_post_replacement_damage_target(else_branch);
    }
}

/// CR 615.5: In a prevention follow-up attached to "damage would be dealt to a
/// player", the surface subject "that player" refers to the prevented event's
/// damage recipient. The ordinary effect parser has no active trigger event in
/// this replacement context, so it lowers a standalone non-trigger "that player"
/// subject to `TargetFilter::ParentTargetController` (the generic CR 608.2c
/// anaphor) — or, inside a trigger context, to `TargetFilter::TriggeringPlayer`.
/// Neither resolves correctly here (there is no parent target and no trigger
/// event), so rewrite the anaphoric recipient to `PostReplacementDamageTarget`
/// at the call site.
fn rewrite_damage_recipient_to_post_replacement_target(def: &mut AbilityDefinition) {
    super::oracle_effect::each_target_filter_mut(&mut def.effect, &mut |f| {
        if matches!(
            f,
            TargetFilter::Player
                | TargetFilter::TriggeringPlayer
                | TargetFilter::ParentTargetController
        ) {
            *f = TargetFilter::PostReplacementDamageTarget;
        }
    });
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_damage_recipient_to_post_replacement_target(sub);
    }
    if let Some(else_branch) = def.else_ability.as_mut() {
        rewrite_damage_recipient_to_post_replacement_target(else_branch);
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
/// `replacement.rs:2207` only stashes `post_replacement_continuation` when
/// prevention actually occurred — so the prelude is semantically a no-op and normalizes
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
    let (_, after) = split_once_on_lower(original_text, &lower, "prevent that damage. ")
        .or_else(|| {
            let (_, after) = split_once_on_lower(original_text, &lower, ". ")?;
            let after_lower = after.to_lowercase();
            if nom_primitives::scan_contains(&after_lower, "prevented this way") {
                Some(("", after))
            } else {
                None
            }
        })
        // CR 615.5: Same-sentence "prevent that damage and <followup>" form
        // (Anti-Venom, Ironscale Hydra, Jared Carthalion, Nine Lives). The
        // four cards in this class share the structural "[gate], prevent
        // that damage and put <count> <kind> counter[s] on <pronoun>" shape.
        // Rewrite the connector to a sentence boundary so the followup sub-
        // parser sees a fresh imperative chunk it can parse against.
        .or_else(|| {
            let (_, after_and) =
                split_once_on_lower(original_text, &lower, "prevent that damage and ")?;
            Some(("", after_and))
        })?;
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
                    tag::<_, _, OracleError<'_>>("when "),
                    tag::<_, _, OracleError<'_>>("if "),
                )),
                tag::<_, _, OracleError<'_>>("damage is prevented this way, "),
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
        && !nom_primitives::scan_contains(norm_lower, "tap a permanent for mana")
        && !nom_primitives::scan_contains(norm_lower, "tap a land for mana")
        && !nom_primitives::scan_contains(norm_lower, "tap a basic land for mana")
    {
        return None;
    }

    let def = ReplacementDefinition::new(ReplacementEvent::ProduceMana)
        .description(original_text.to_string());

    if let Ok((rest, (filter, factor))) = parse_mana_multiplier_replacement(norm_lower) {
        if rest.trim().is_empty() {
            return Some(
                def.mana_modification(ManaModification::Multiply { factor })
                    .mana_replacement_scope(ManaReplacementScope::TappedForMana)
                    .valid_card(filter),
            );
        }
    }

    let scope = if nom_primitives::scan_contains(norm_lower, "tapped for mana") {
        ManaReplacementScope::TappedForMana
    } else {
        ManaReplacementScope::Any
    };

    match scan_produces_replacement(norm_lower) {
        // CR 106.3: The mana source must be a land — scope the replacement so it
        // only fires on mana produced by lands (Contamination et al.). Applied
        // only when the payload is concretely known so pre-existing
        // color-choice / any-color replacements (not yet wired) retain their
        // parse-only behavior.
        Some(mana_type) => Some(
            def.mana_modification(ManaModification::ReplaceWith { mana_type })
                .mana_replacement_scope(scope)
                .valid_card(TargetFilter::Typed(TypedFilter::land())),
        ),
        None => Some(def.mana_replacement_scope(scope)),
    }
}

fn parse_mana_multiplier_replacement(
    input: &str,
) -> super::oracle_nom::error::OracleResult<'_, (TargetFilter, u32)> {
    let (input, _) = tag::<_, _, OracleError<'_>>("if you tap ").parse(input)?;
    let (input, filter) = alt((
        value(
            TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
            tag("a permanent"),
        ),
        value(
            TargetFilter::Typed(
                TypedFilter::land()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::HasSupertype {
                        value: Supertype::Basic,
                    }]),
            ),
            tag("a basic land"),
        ),
        value(
            TargetFilter::Typed(TypedFilter::land().controller(ControllerRef::You)),
            tag("a land"),
        ),
    ))
    .parse(input)?;
    let (input, _) = tag(" for mana, it produces ").parse(input)?;
    let (input, factor) = alt((
        value(2, tag("twice as much")),
        value(2, tag("two times as much")),
        value(3, tag("three times as much")),
    ))
    .parse(input)?;
    let (input, _) = tag(" of that mana instead").parse(input)?;
    let (input, _) = opt(char('.')).parse(input)?;
    Ok((input, (filter, factor)))
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
    let (rest, _) = tag::<_, _, OracleError<'_>>("produces ").parse(input)?;
    alt((parse_braced_mana_type, parse_colorless_mana)).parse(rest)
}

/// Parse a single colored-mana brace symbol into `ManaType`: `{W}`/`{U}`/`{B}`/`{R}`/`{G}`.
fn parse_braced_mana_type(input: &str) -> super::oracle_nom::error::OracleResult<'_, ManaType> {
    use nom::sequence::delimited;
    delimited(
        char::<_, OracleError<'_>>('{'),
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
        tag::<_, _, OracleError<'_>>("colorless mana"),
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
        tag::<_, _, OracleError<'_>>("or less life"),
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
            tag::<_, _, OracleError<'_>>(", or "),
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
    tag::<_, _, OracleError<'_>>("turn").parse(remaining).ok()?;
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

/// CR 614.1a: Parse "if you control a [filter], damage that would reduce
/// your life total to less than N reduces it to N instead." (Worship class).
///
/// Returns a `ReplacementDefinition` with:
/// - `event`: `DamageDone`
/// - `condition`: `IfControlsMatching { minimum: 1, filter }` (controller scope)
/// - `damage_modification`: `LifeFloor { minimum: N }`
/// - `damage_target_filter`: `DamageTargetFilter::Player(Controller)`
fn parse_life_floor_damage_replacement(norm_lower: &str) -> Option<ReplacementDefinition> {
    let (rest, _) = tag::<_, _, OracleError<'_>>("if you control ")
        .parse(norm_lower)
        .ok()?;
    let (rest, _) = alt((tag::<_, _, OracleError<'_>>("a "), tag("an ")))
        .parse(rest)
        .ok()?;

    let (after_threshold, filter_text) = terminated(
        take_until::<_, _, OracleError<'_>>(
            ", damage that would reduce your life total to less than ",
        ),
        tag::<_, _, OracleError<'_>>(", damage that would reduce your life total to less than "),
    )
    .parse(rest)
    .ok()?;

    let (tail, minimum) = nom_primitives::parse_number.parse(after_threshold).ok()?;
    let (tail, floor_val) = preceded(
        tag::<_, _, OracleError<'_>>(" reduces it to "),
        nom_primitives::parse_number,
    )
    .parse(tail)
    .ok()?;
    if floor_val != minimum {
        return None;
    }
    let (_, _) = all_consuming((
        tag::<_, _, OracleError<'_>>(" instead"),
        opt(tag::<_, _, OracleError<'_>>(".")),
    ))
    .parse(tail)
    .ok()?;

    // Build the controller-scoped filter (e.g., "creature you control").
    let (filter, leftover) = parse_type_phrase(filter_text);
    if filter == TargetFilter::Any || !leftover.trim().is_empty() {
        return None;
    }
    let condition_filter = inject_controller(filter, ControllerRef::You);

    Some(
        ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .condition(ReplacementCondition::IfControlsMatching {
                minimum: 1,
                filter: condition_filter,
            })
            .damage_modification(DamageModification::LifeFloor {
                minimum: minimum as i32,
            })
            .damage_target_filter(DamageTargetFilter::Player {
                player: DamageTargetPlayerScope::Controller,
            }),
    )
}

/// CR 614.1a: Parse the UNCONDITIONAL life-floor replacement:
/// - "damage that would reduce your life total to less than N reduces it to N instead"
///   (Fortune Thief, Sustaining Spirit)
/// - "damage that would reduce your life total to 0 reduces it to 1 instead"
///   (Ali from Cairo printed wording — lethal threshold "to 0", floor M)
///
/// Identical to [`parse_life_floor_damage_replacement`] but without the Worship-class
/// "if you control a [filter]," guard. Dispatched after the conditional arm.
fn parse_unconditional_life_floor_damage_replacement(
    norm_lower: &str,
) -> Option<ReplacementDefinition> {
    let floor_minimum = alt((
        parse_unconditional_life_floor_less_than_form,
        parse_unconditional_life_floor_to_zero_form,
    ))
    .parse(norm_lower)
    .ok()
    .map(|(_, minimum)| minimum)?;

    Some(
        ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .damage_modification(DamageModification::LifeFloor {
                minimum: floor_minimum,
            })
            .damage_target_filter(DamageTargetFilter::Player {
                player: DamageTargetPlayerScope::Controller,
            }),
    )
}

/// "damage that would reduce your life total to less than N reduces it to N instead."
fn parse_unconditional_life_floor_less_than_form(input: &str) -> OracleResult<'_, i32> {
    let (rest, minimum) = preceded(
        tag::<_, _, OracleError<'_>>("damage that would reduce your life total to less than "),
        nom_primitives::parse_number,
    )
    .parse(input)?;
    let (rest, floor_val) = preceded(
        tag::<_, _, OracleError<'_>>(" reduces it to "),
        nom_primitives::parse_number,
    )
    .parse(rest)?;
    if floor_val != minimum {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }
    let (rest, _) = all_consuming((
        tag::<_, _, OracleError<'_>>(" instead"),
        opt(tag::<_, _, OracleError<'_>>(".")),
    ))
    .parse(rest)?;
    Ok((rest, minimum as i32))
}

/// Ali from Cairo printed wording: "…to 0 reduces it to M instead."
fn parse_unconditional_life_floor_to_zero_form(input: &str) -> OracleResult<'_, i32> {
    let (rest, floor_val) = preceded(
        tag::<_, _, OracleError<'_>>(
            "damage that would reduce your life total to 0 reduces it to ",
        ),
        nom_primitives::parse_number,
    )
    .parse(input)?;
    let (rest, _) = all_consuming((
        tag::<_, _, OracleError<'_>>(" instead"),
        opt(tag::<_, _, OracleError<'_>>(".")),
    ))
    .parse(rest)?;
    Ok((rest, floor_val as i32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        Comparator, ControllerRef, CountScope, QuantityExpr, QuantityModification, QuantityRef,
        ReplacementCondition, ShieldKind, ZoneRef,
    };
    use crate::types::card_type::{CoreType, Supertype};
    use crate::types::keywords::Keyword;

    /// CR 615.1a + CR 615.5 + CR 122.1 + CR 608.2h: Protean Hydra class —
    /// "If damage would be dealt to ~, prevent that damage and remove that
    /// many +1/+1 counters from it." Building-block assertions:
    ///
    /// 1. The shield is self-scoped (`valid_card: SelfRef`) and prevents all
    ///    damage to the source — not a broad creature filter.
    /// 2. The rider parses to `Effect::RemoveCounter` (not `Unimplemented`),
    ///    so the four-card class (Protean Hydra, Ugin's Conjurant, Polukranos
    ///    Unchained, Underdark Beholder) is unlocked.
    /// 3. The rider's "that many" count resolves to `EventContextAmount` (the
    ///    prevented-damage amount), mirroring the Vigor `PutCounter` cohort.
    /// 4. "from it" binds to the shield-bearing permanent (`SelfRef`).
    #[test]
    fn protean_hydra_prevent_and_remove_that_many_counters() {
        let def = parse_replacement_line(
            "If damage would be dealt to ~, prevent that damage and remove that many +1/+1 counters from it.",
            "Protean Hydra",
        )
        .expect("Protean Hydra should parse as a damage prevention replacement");

        // (1) Self-scoped shield.
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(
            def.damage_target_filter.is_none(),
            "self-scoped prevention must not use a broad damage_target_filter"
        );

        // (2) + (3) + (4) Rider removes EventContextAmount counters from self.
        let execute = def.execute.as_ref().expect("execute follow-up present");
        match &*execute.effect {
            Effect::RemoveCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(*counter_type, Some(CounterType::Plus1Plus1));
                assert_eq!(*target, TargetFilter::SelfRef, "\"from it\" = the source");
                assert_eq!(
                    *count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    },
                    "\"that many\" must bind the prevented-damage amount"
                );
            }
            other => panic!("expected Effect::RemoveCounter, got {other:?}"),
        }
    }

    #[test]
    fn find_copy_verb_present_recognizes_copy_replacement() {
        // CR 707.9 / CR 614.1c: copy replacement verbs are recognized.
        assert!(find_copy_verb_present(
            "you may have ~ enter as a copy of any creature on the battlefield"
        ));
        assert!(find_copy_verb_present("become a copy of target creature"));
        // Static / prevent lines are NOT copy replacements.
        assert!(!find_copy_verb_present(
            "prevent all combat damage that would be dealt this turn"
        ));
        assert!(!find_copy_verb_present(
            "if a source you control would deal damage to a permanent or player"
        ));
        assert!(!find_copy_verb_present(
            "prevent all damage that would be dealt this turn unless its controller wins a clash"
        ));
    }

    /// CR 614.12 + CR 614.1a: Phial of Galadriel — "If you would gain life
    /// while you have 5 or less life, you gain twice that much life instead."
    /// The `while [condition]` clause in the antecedent must lift to a typed
    /// `ReplacementCondition::OnlyIfQuantity` so the doubler is suppressed
    /// while the controller has more than 5 life. Issue #317 follow-up:
    /// before this fix, the condition was silently dropped and the doubler
    /// fired unconditionally.
    #[test]
    fn phial_of_galadriel_while_life_threshold_emits_only_if_quantity() {
        let def = parse_replacement_line(
            "If you would gain life while you have 5 or less life, you gain twice that much life instead.",
            "Phial of Galadriel",
        )
        .expect("should parse as a replacement");
        let condition = def
            .condition
            .as_ref()
            .expect("while-life gate must lift to a typed ReplacementCondition");
        match condition {
            ReplacementCondition::OnlyIfQuantity {
                lhs,
                comparator,
                rhs,
                active_player_req,
            } => {
                assert_eq!(
                    *lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::LifeTotal {
                            player: crate::types::ability::PlayerScope::Controller,
                        },
                    }
                );
                assert_eq!(*comparator, Comparator::LE);
                assert_eq!(*rhs, QuantityExpr::Fixed { value: 5 });
                assert_eq!(*active_player_req, None);
            }
            other => panic!("expected OnlyIfQuantity, got {other:?}"),
        }
    }

    /// CR 614.1a + CR 614.12a: Karoo land — untyped multi-sacrifice cost
    /// (Lotus Vale, Scorched Ruins). Parses to a `MayCost` `Moved` replacement
    /// whose accept-cost is `Sacrifice { count: 2 }` and whose decline branch
    /// redirects the ETB to the owner's graveyard.
    #[test]
    fn karoo_land_sacrifice_count_replacement() {
        let def = parse_replacement_line(
            "If this land would enter, sacrifice two untapped lands instead. If you do, \
             put this land onto the battlefield. If you don't, put it into its owner's graveyard.",
            "Lotus Vale",
        )
        .expect("Karoo land should parse as a replacement");
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        match &def.mode {
            ReplacementMode::MayCost { cost, decline } => {
                assert!(
                    matches!(cost, AbilityCost::Sacrifice(ref c) if c.requirement.fixed_count() == Some(2)),
                    "expected Sacrifice count 2, got {cost:?}"
                );
                let decline = decline.as_ref().expect("Karoo decline branch");
                assert!(matches!(
                    &*decline.effect,
                    Effect::ChangeZone {
                        destination: Zone::Graveyard,
                        ..
                    }
                ));
            }
            other => panic!("expected MayCost, got {other:?}"),
        }
    }

    /// CR 614.1a: Karoo land — typed single-sacrifice cost (Heart of Yavimaya
    /// "sacrifice a Forest", Balduvian Trading Post "an untapped Mountain").
    #[test]
    fn karoo_land_typed_single_sacrifice_replacement() {
        let def = parse_replacement_line(
            "If this land would enter, sacrifice a Forest instead. If you do, put this \
             land onto the battlefield. If you don't, put it into its owner's graveyard.",
            "Heart of Yavimaya",
        )
        .expect("Karoo land should parse as a replacement");
        match &def.mode {
            ReplacementMode::MayCost { cost, .. } => {
                assert!(
                    matches!(cost, AbilityCost::Sacrifice(ref c) if c.requirement.fixed_count() == Some(1)),
                    "expected Sacrifice count 1, got {cost:?}"
                );
            }
            other => panic!("expected MayCost, got {other:?}"),
        }
    }

    /// CR 502.3 + CR 502.4 + CR 614.1a: untap-step replacement. Edge of Malacol
    /// "If a creature you control would untap during your untap step, put two
    /// +1/+1 counters on it instead." gates to the untap step (so effect-untaps
    /// elsewhere are unaffected) and keeps the alternative effect.
    #[test]
    fn untap_step_replacement_edge_of_malacol() {
        let def = parse_replacement_line(
            "If a creature you control would untap during your untap step, put two +1/+1 counters on it instead.",
            "Edge of Malacol",
        )
        .expect("untap-step replacement should parse");
        assert_eq!(def.event, ReplacementEvent::Untap);
        assert_eq!(def.condition, Some(ReplacementCondition::DuringUntapStep));
        assert!(
            def.execute.is_some(),
            "alternative effect (+1/+1 counters) must not be dropped"
        );
        assert!(
            def.valid_card.is_some(),
            "valid_card filter (a creature you control) should be set"
        );
    }

    /// CR 502.3 + CR 614.1a: untap-step replacement with a counter-filtered
    /// subject — Freyalise's Winds "If a permanent with a wind counter on it
    /// would untap during its controller's untap step, remove all wind counters
    /// from it instead."
    #[test]
    fn untap_step_replacement_freyalises_winds() {
        let def = parse_replacement_line(
            "If a permanent with a wind counter on it would untap during its controller's untap step, remove all wind counters from it instead.",
            "Freyalise's Winds",
        )
        .expect("counter-filtered untap-step replacement should parse");
        assert_eq!(def.event, ReplacementEvent::Untap);
        assert_eq!(def.condition, Some(ReplacementCondition::DuringUntapStep));
        assert!(def.execute.is_some());
    }

    #[test]
    fn turned_face_up_replacement_megamorph() {
        // CR 614.1e + CR 708.11: "As ~ is turned face up,
        // [effect]" is a TurnFaceUp REPLACEMENT (applies as the permanent is
        // turned up — no stack trigger), bound to the permanent itself.
        let def = parse_replacement_line(
            "As this creature is turned face up, put five +1/+1 counters on it.",
            "Hooded Hydra",
        )
        .expect("turn-face-up replacement should parse");
        assert_eq!(def.event, ReplacementEvent::TurnFaceUp);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        let execute = def.execute.expect("alternative effect must be parsed");
        assert!(
            matches!(&*execute.effect, Effect::PutCounter { .. }),
            "expected PutCounter, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn turned_face_up_replacement_gaps_external_target_choice() {
        // CR 708.11: an "As ~ is turned face up" effect applies during the
        // turn-up with no targeting window. Gift of Doom's "you may attach it to a
        // creature" needs an external host choice that cannot be made there, so it
        // must NOT be modeled as a TurnFaceUp replacement (gapped honestly rather
        // than mis-resolving the host) — only the self-resolving `SelfRef` class is.
        let def = parse_replacement_line(
            "As this permanent is turned face up, you may attach it to a creature.",
            "Gift of Doom",
        );
        assert!(
            !def.as_ref()
                .is_some_and(|d| d.event == ReplacementEvent::TurnFaceUp),
            "attach-to-external-creature must be gapped, got {def:?}"
        );
    }

    /// CR 614.12a: Karoo artifact — Mox Diamond's "you may discard ..." cost.
    /// The non-cost "you may " lead-in is stripped before `parse_single_cost`.
    #[test]
    fn karoo_artifact_discard_replacement() {
        let def = parse_replacement_line(
            "If this artifact would enter, you may discard a land card instead. If you do, \
             put this artifact onto the battlefield. If you don't, put it into its owner's graveyard.",
            "Mox Diamond",
        )
        .expect("Mox Diamond should parse as a replacement");
        assert_eq!(def.event, ReplacementEvent::Moved);
        match &def.mode {
            ReplacementMode::MayCost { cost, decline } => {
                assert!(
                    matches!(cost, AbilityCost::Discard { .. }),
                    "expected Discard cost, got {cost:?}"
                );
                assert!(decline.is_some(), "Mox Diamond needs a decline branch");
            }
            other => panic!("expected MayCost, got {other:?}"),
        }
    }

    /// CR 614.1a + CR 614.12: The Mimeoplasm — "As ~ enters, you may exile two
    /// creature cards from graveyards. If you do, it enters as a copy of one of
    /// those cards with a number of additional +1/+1 counters on it equal to the
    /// power of the other card."
    #[test]
    fn mimeoplasm_exile_from_graveyards_replacement() {
        let def = parse_replacement_line(
            "As ~ enters, you may exile two creature cards from graveyards. If you do, \
             it enters as a copy of one of those cards with a number of additional +1/+1 \
             counters on it equal to the power of the other card.",
            "The Mimeoplasm",
        )
        .expect("The Mimeoplasm should parse as a replacement");
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        match &def.mode {
            ReplacementMode::MayCost { cost, decline } => {
                assert!(
                    matches!(cost, AbilityCost::Exile { count, zone, filter } if *count == 2 && *zone == Some(Zone::Graveyard) && filter.is_some()),
                    "expected Exile count 2 from Graveyard, got {cost:?}"
                );
                assert!(decline.is_none(), "The Mimeoplasm has no decline branch");
            }
            other => panic!("expected MayCost, got {other:?}"),
        }
        // Verify the continuation effect is present in execute
        let execute = def.execute.as_ref().expect("execute must be present");
        // The continuation should be the copy + counter placement effect
        assert!(!matches!(&*execute.effect, Effect::Unimplemented { .. }));
    }

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
    fn extract_prevention_followup_accepts_turn_duration_counter_followup() {
        assert_eq!(
            extract_prevention_followup(
                "Prevent the next 3 damage that would be dealt to target creature this turn. \
                 For each 1 damage prevented this way, put a +1/+1 counter on that creature."
            )
            .as_deref(),
            Some("For each 1 damage prevented this way, put a +1/+1 counter on that creature.")
        );
    }

    #[test]
    fn extract_prevention_followup_accepts_turn_duration_life_followup() {
        assert_eq!(
            extract_prevention_followup(
                "Prevent all damage target spell would deal this turn. \
                 You gain life equal to the damage prevented this way."
            )
            .as_deref(),
            Some("You gain life equal to the damage prevented this way.")
        );
    }

    #[test]
    fn unbreathing_horde_self_damage_prevention_is_self_scoped() {
        // CR 615.1a + issue #2888: "If ~ would be dealt damage, prevent that
        // damage and remove a +1/+1 counter from it" must scope the shield to
        // the source itself (valid_card SelfRef), not prevent ALL damage
        // (including damage dealt to players).
        let def = parse_replacement_line(
            "If ~ would be dealt damage, prevent that damage and remove a +1/+1 counter from it.",
            "Unbreathing Horde",
        )
        .expect("Unbreathing Horde damage-prevention replacement should parse");
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::SelfRef),
            "self-damage prevention must be scoped to the source, got {:?}",
            def.valid_card
        );
    }

    #[test]
    fn gains_no_life_instead_lowers_to_prevent_not_unimplemented() {
        // CR 119.10 + CR 614.6 + issue #743: "If a player would gain life, that
        // player gains no life instead." must emit a structured `Prevent`
        // quantity modification (which `gain_life_applier` Branch 1 reads to
        // fully suppress the gain), NOT an `Unimplemented` no-op effect
        // (which the runtime silently passes through, letting the gain proceed).
        let def = parse_replacement_line(
            "If a player would gain life, that player gains no life instead.",
            "Sulfuric Vortex",
        )
        .expect("Sulfuric Vortex lifegain-negation replacement should parse");
        assert_eq!(def.event, ReplacementEvent::GainLife);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Prevent),
            "lifegain-negation must carry Prevent, got {:?}",
            def.quantity_modification
        );
        // No execute effect: a Prevent replacement carries no `Unimplemented`
        // (or any) effect, mirroring the counter-prohibition precedent.
        assert!(
            def.execute.is_none(),
            "Prevent replacement must not carry an execute effect, got {:?}",
            def.execute
        );
        // "a player would gain life" → global scope (CR 614.1a).
        assert_eq!(def.valid_player, Some(ReplacementPlayerScope::AnyPlayer));

        // Class coverage: the "you gain no life" sibling phrasing lowers the
        // same way (controller-only scope).
        let you_def = parse_replacement_line(
            "If you would gain life, you gain no life instead.",
            "Test Card",
        )
        .expect("'you gain no life' sibling should parse");
        assert_eq!(
            you_def.quantity_modification,
            Some(QuantityModification::Prevent)
        );
        assert_eq!(you_def.valid_player, None);
    }

    #[test]
    fn lifegain_doubler_still_doubles_not_prevented() {
        // Negative guard: "gain twice that much life" must NOT collapse into
        // Prevent — the negation detector only fires on "no life".
        let def = parse_replacement_line(
            "If you would gain life, you gain twice that much life instead.",
            "Boon Reflection",
        )
        .expect("doubler should parse");
        assert_eq!(def.event, ReplacementEvent::GainLife);
        assert_ne!(
            def.quantity_modification,
            Some(QuantityModification::Prevent),
            "doubler must not be turned into a Prevent"
        );
    }

    #[test]
    fn flames_durational_lifegain_negation_is_not_permanent_prevent() {
        // CR 611.2a + issue #743 scoping: Flames of the Blood Hand's clause is a
        // duration-scoped ("this turn") replacement created by a resolving
        // spell. It must NOT be lowered to a permanent `Prevent` (which would
        // suppress the player's lifegain forever). Deferred as a follow-up until
        // the durational replacement shape is supported.
        let def = parse_replacement_line(
            "If that player or that planeswalker's controller would gain life this turn, that player gains no life instead.",
            "Flames of the Blood Hand",
        );
        // Whether it parses to some other shape or None, it must never carry a
        // permanent Prevent.
        if let Some(def) = def {
            assert_ne!(
                def.quantity_modification,
                Some(QuantityModification::Prevent),
                "durational 'this turn' negation must not become a permanent Prevent"
            );
        }
    }

    #[test]
    fn library_of_leng_discard_to_library_top_replacement() {
        let def = parse_replacement_line(
            "If an effect causes you to discard a card, discard it, but you may put it on top of your library instead of into your graveyard.",
            "Library of Leng",
        )
        .expect("Library of Leng discard replacement should parse");
        assert_eq!(def.event, ReplacementEvent::Discard);
        assert!(
            matches!(def.mode, ReplacementMode::Optional { decline: None }),
            "optional top-of-library redirect must be Optional {{ decline: None }}; got {:?}",
            def.mode
        );
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            ))
        );
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::EffectCausedDiscard),
            "Library of Leng must gate on effect-caused discards only"
        );
        let execute = def.execute.as_ref().expect("execute present");
        assert!(
            matches!(*execute.effect, Effect::PutAtLibraryPosition { .. }),
            "expected PutAtLibraryPosition, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn discard_self_to_battlefield_replacement() {
        let def = parse_replacement_line(
            "If a spell or ability an opponent controls causes you to discard this card, put it onto the battlefield instead of putting it into your graveyard.",
            "Loxodon Smiter",
        )
        .expect("discard self replacement should parse");
        assert_eq!(def.event, ReplacementEvent::Discard);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::EventSourceControlledBy {
                controller: ControllerRef::Opponent
            })
        );
        let execute = def.execute.as_ref().expect("execute present");
        assert!(matches!(
            *execute.effect,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        ));
    }

    #[test]
    fn discard_self_to_battlefield_replacement_preserves_counters() {
        let def = parse_replacement_line(
            "If a spell or ability an opponent controls causes you to discard this card, put it onto the battlefield with two +1/+1 counters on it instead of putting it into your graveyard.",
            "Dodecapod",
        )
        .expect("discard self replacement should parse");
        let execute = def.execute.as_ref().expect("execute present");
        match &*execute.effect {
            Effect::ChangeZone {
                destination,
                enter_with_counters,
                ..
            } => {
                assert_eq!(*destination, Zone::Battlefield);
                assert_eq!(
                    enter_with_counters,
                    &vec![(CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 2 })]
                );
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    #[test]
    fn damage_to_self_puts_that_many_counters_instead() {
        let def = parse_replacement_line(
            "If damage would be dealt to this creature, put that many +1/+1 counters on it instead.",
            "Phytohydra",
        )
        .expect("damage-to-self counter replacement should parse");

        assert_eq!(def.event, ReplacementEvent::DealtDamage);
        assert_eq!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        );
        let execute = def.execute.as_ref().expect("execute present");
        assert!(matches!(
            *execute.effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Plus1Plus1
        ));
    }

    #[test]
    fn damage_to_you_puts_that_many_counters_on_source_instead() {
        let def = parse_replacement_line(
            "If damage would be dealt to you, put that many delay counters on this enchantment instead.",
            "Delaying Shield",
        )
        .expect("damage-to-controller counter replacement should parse");

        assert_eq!(def.event, ReplacementEvent::DealtDamage);
        let execute = def.execute.as_ref().expect("execute present");
        assert!(matches!(
            *execute.effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Generic("delay".to_string())
        ));
    }

    #[test]
    fn damage_to_player_exiles_that_many_cards_from_that_players_library_instead() {
        let def = parse_replacement_line(
            "If damage would be dealt to a player, that player exiles that many cards from the top of their library instead.",
            "Crumbling Sanctuary",
        )
        .expect("damage-to-player exile-top replacement should parse");

        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        );
        assert_eq!(def.damage_target_filter, Some(damage_target_any_player()));
        let execute = def.execute.as_ref().expect("execute present");
        assert!(
            matches!(
                *execute.effect,
                Effect::ExileTop {
                    player: TargetFilter::PostReplacementDamageTarget,
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    },
                    face_down: false,
                }
            ),
            "expected ExileTop against prevented damage recipient, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn damage_to_player_followup_rewrites_that_player_draw_target() {
        let def = parse_replacement_line(
            "If damage would be dealt to a player, that player draws that many cards instead.",
            "Damage Followup Test",
        )
        .expect("damage-to-player draw replacement should parse");

        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(def.damage_target_filter, Some(damage_target_any_player()));
        let execute = def.execute.as_ref().expect("execute present");
        assert!(matches!(
            *execute.effect,
            Effect::Draw {
                target: TargetFilter::PostReplacementDamageTarget,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
            }
        ));
    }

    #[test]
    fn prevention_counter_followup_uses_prevented_amount_repeat() {
        let def = parse_replacement_line(
            "Prevent the next 3 damage that would be dealt to target creature this turn. \
             For each 1 damage prevented this way, put a +1/+1 counter on that creature.",
            "Test of Faith",
        )
        .unwrap();

        let execute = def.execute.as_ref().expect("execute present");
        assert!(matches!(
            execute.repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            })
        ));
        assert!(matches!(
            *execute.effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::ParentTarget,
            } if *counter_type == CounterType::Plus1Plus1
        ));
    }

    /// Anti-Venom self-scoped prevention must use `valid_card: SelfRef`, not `CreatureOnly`.
    #[test]
    fn anti_venom_self_prevention_uses_valid_card_self_ref() {
        for text in [
            "If damage would be dealt to ~, prevent that damage and put that many +1/+1 counters on him.",
            "If damage would be dealt to and dealt by ~, prevent that damage and put that many +1/+1 counters on him.",
        ] {
            let def = parse_replacement_line(text, "Anti-Venom, Horrifying Healer")
                .expect("Anti-Venom prevention should parse");

            assert_eq!(
                def.valid_card,
                Some(TargetFilter::SelfRef),
                "self-scoped prevention must gate on SelfRef: {text}"
            );
            assert!(
                def.damage_target_filter.is_none(),
                "must not use broad CreatureOnly damage_target_filter: {text}"
            );
        }
    }

    /// CR 614.1a + CR 615.5 + CR 608.2c: Vigor — "If damage would be dealt to
    /// another creature you control, prevent that damage. Put a +1/+1 counter
    /// on that creature for each 1 damage prevented this way."
    ///
    /// Three building-block assertions:
    ///
    /// 1. The recipient phrase parses through `parse_damage_recipient_valid_card_filter`
    ///    even though it closes at `", prevent"` (the same-sentence clause
    ///    boundary), and the resulting typed filter retains `controller: You`
    ///    and `FilterProp::Another`. Previously the all-consuming terminator
    ///    rejected the comma + imperative, silently dropping `valid_card` and
    ///    causing the shield to fire on ANY creature (including opponents').
    ///
    /// 2. The rider's anaphor "that creature" (which `parse_target` lowers to
    ///    `TargetFilter::ParentTarget` per CR 608.2c) is rewritten at the
    ///    parser call site to `TargetFilter::PostReplacementDamageTarget` so
    ///    the +1/+1 counter lands on the prevented event's damage recipient
    ///    rather than dangling against a nonexistent parent target slot.
    ///
    /// 3. The rider count resolves to `QuantityRef::EventContextAmount` (the
    ///    prevented amount), via the existing `for each 1 damage prevented
    ///    this way` post-target suffix path.
    #[test]
    fn vigor_event_recipient_filter_and_counter_target_rewrite() {
        let def = parse_replacement_line(
            "If damage would be dealt to another creature you control, prevent that damage. \
             Put a +1/+1 counter on that creature for each 1 damage prevented this way.",
            "Vigor",
        )
        .expect("Vigor should parse as a damage prevention replacement");

        // (1) valid_card recipient filter — Typed Creature, controller=You, Another.
        let valid_card = def
            .valid_card
            .as_ref()
            .expect("Vigor's recipient filter must survive the parser");
        match valid_card {
            TargetFilter::Typed(tf) => {
                assert!(
                    tf.type_filters.contains(&TypeFilter::Creature),
                    "expected Creature type filter, got {:?}",
                    tf.type_filters
                );
                assert_eq!(
                    tf.controller,
                    Some(ControllerRef::You),
                    "expected controller=You, got {:?}",
                    tf.controller
                );
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "expected FilterProp::Another in {:?}",
                    tf.properties
                );
            }
            other => panic!("expected Typed recipient filter, got {other:?}"),
        }

        // (2) + (3) rider PutCounter targets the event recipient with
        // EventContextAmount on the `count` field.
        let execute = def.execute.as_ref().expect("execute present");
        match &*execute.effect {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(*counter_type, CounterType::Plus1Plus1);
                assert_eq!(*target, TargetFilter::PostReplacementDamageTarget);
                // The suffix-form for-each ("... for each 1 damage prevented
                // this way") lands the prevented amount on the PutCounter
                // `count` field via `try_parse_for_each_effect`, so pin the
                // exact field rather than accepting an either/or shape.
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount
                        }
                    ),
                    "expected count to be EventContextAmount; got count={count:?}, repeat_for={:?}",
                    execute.repeat_for
                );
            }
            other => panic!("expected Effect::PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn prevention_life_followup_uses_prevented_amount() {
        let def = parse_replacement_line(
            "Prevent all damage target spell would deal this turn. \
             You gain life equal to the damage prevented this way.",
            "Hallow",
        )
        .unwrap();

        let execute = def.execute.as_ref().expect("execute present");
        assert!(matches!(
            *execute.effect,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
                ..
            }
        ));
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
    }

    #[test]
    fn replacement_enters_prepared() {
        let def = parse_replacement_line("This creature enters prepared.", "Test Creature")
            .expect("enters prepared should parse as replacement");
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::BecomePrepared {
                target: TargetFilter::SelfRef
            }
        ));
    }

    #[test]
    fn oracle_enters_prepared_is_replacement_not_trigger() {
        let parsed = parse_oracle_text(
            "Lluwen enters prepared.",
            "Lluwen, Exchange Student",
            &[],
            &["Creature".to_string()],
            &[],
        );
        assert!(parsed.triggers.is_empty());
        assert_eq!(parsed.replacements.len(), 1);
        assert!(matches!(
            *parsed.replacements[0]
                .execute
                .as_ref()
                .expect("execute should be set")
                .effect,
            Effect::BecomePrepared {
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
        assert_eq!(def.damage_target_filter, Some(damage_target_controller()));
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
        assert_eq!(def.damage_target_filter, Some(damage_target_controller()));
    }

    #[test]
    fn replacement_prevent_all_damage_to_you_without_duration() {
        let def = parse_replacement_line(
            "Prevent all damage that would be dealt to you.",
            "Solitary Confinement",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert!(def.combat_scope.is_none());
        assert_eq!(def.damage_target_filter, Some(damage_target_controller()));
    }

    #[test]
    fn replacement_prevent_damage_to_equipped_creature_scopes_via_valid_card() {
        // General's Kabuto: prevention is scoped to the equipped creature, not "any creature".
        // Before the fix, the parser left `valid_card = None`, so the shield would prevent
        // damage to every creature on the battlefield once Kabuto was on the field.
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt to equipped creature.",
            "General's Kabuto",
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
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy])
            ))
        );
        // No type-based filter — the scoping comes from valid_card alone.
        assert!(def.damage_target_filter.is_none());
        assert!(def.condition.is_none());
    }

    #[test]
    fn replacement_prevent_noncombat_damage_to_equipped_creature() {
        // Magebane Armor: noncombat-only variant of the same scoping pattern.
        let def = parse_replacement_line(
            "Prevent all noncombat damage that would be dealt to equipped creature.",
            "Magebane Armor",
        )
        .unwrap();
        assert_eq!(def.combat_scope, Some(CombatDamageScope::NoncombatOnly));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy])
            ))
        );
    }

    #[test]
    fn replacement_prevent_damage_to_enchanted_creature_scopes_via_valid_card() {
        // Inviolability: aura variant of the same building block.
        let def = parse_replacement_line(
            "Prevent all damage that would be dealt to enchanted creature.",
            "Inviolability",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EnchantedBy])
            ))
        );
    }

    #[test]
    fn replacement_prevent_damage_to_enchanted_permanent_subjects_scope_via_valid_card() {
        for (text, type_filter) in [
            (
                "Prevent all damage that would be dealt to enchanted permanent.",
                TypeFilter::Permanent,
            ),
            (
                "Prevent all damage that would be dealt to enchanted artifact.",
                TypeFilter::Artifact,
            ),
            (
                "Prevent all damage that would be dealt to enchanted land.",
                TypeFilter::Land,
            ),
        ] {
            let def = parse_replacement_line(text, "Attachment Prevention").unwrap();
            assert_eq!(
                def.valid_card,
                Some(TargetFilter::Typed(
                    TypedFilter::new(type_filter).properties(vec![FilterProp::EnchantedBy])
                )),
                "expected attached-object scope for {text}"
            );
        }
    }

    #[test]
    fn replacement_prevent_damage_to_attacking_artifact_creatures_you_control_scopes_recipient() {
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt to attacking artifact creatures you control.",
            "Losheel, Clockwork Scholar",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(def.combat_scope, Some(CombatDamageScope::CombatOnly));
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Artifact)
                    .with_type(TypeFilter::Creature)
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::Attacking { defender: None }])
            ))
        );
        assert!(def.damage_target_filter.is_none());
    }

    #[test]
    fn replacement_multiclass_baldric_full_party_gates_equipped_prevention() {
        // CR 700.8c + CR 614.1a + CR 615: "As long as you have a full party,
        // prevent all damage that would be dealt to equipped creature."
        // Both the gate (full party) AND the target scope (equipped creature)
        // must be encoded so the shield only fires when both hold. Before the
        // fix, neither was — so the shield prevented all damage everywhere
        // whenever Multiclass Baldric was on the battlefield.
        let def = parse_replacement_line(
            "As long as you have a full party, prevent all damage that would be dealt to equipped creature.",
            "Multiclass Baldric",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert!(matches!(
            def.shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        // Gate: only applies while party size >= 4 (CR 700.8c full party).
        match def.condition {
            Some(ReplacementCondition::OnlyIfQuantity {
                ref lhs,
                comparator,
                ref rhs,
                active_player_req,
            }) => {
                assert_eq!(comparator, Comparator::GE);
                assert_eq!(active_player_req, None);
                assert!(matches!(
                    lhs,
                    QuantityExpr::Ref {
                        qty: QuantityRef::PartySize {
                            player: crate::types::ability::PlayerScope::Controller
                        }
                    }
                ));
                assert!(matches!(rhs, QuantityExpr::Fixed { value: 4 }));
            }
            other => panic!("expected OnlyIfQuantity gate, got {:?}", other),
        }
        // Target scope: only damage to the equipped creature is prevented.
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::EquippedBy])
            ))
        );
    }

    #[test]
    fn strip_as_long_as_prefix_returns_input_unchanged_when_absent() {
        // No "as long as" prefix: function leaves the slice untouched and reports no gate.
        let (rest, cond) = strip_as_long_as_prefix_for_prevention(
            "prevent all damage that would be dealt to equipped creature.",
        );
        assert_eq!(
            rest,
            "prevent all damage that would be dealt to equipped creature."
        );
        assert!(cond.is_none());
    }

    #[test]
    fn strip_as_long_as_prefix_leaves_input_intact_when_body_unparseable() {
        // Prefix is present but the body doesn't lift to a typed ReplacementCondition.
        // Function leaves the slice untouched so the rest of the parser can still
        // produce a description-only replacement (no regression vs. pre-fix behavior).
        let input = "as long as ~ has flying, prevent all damage that would be dealt to it.";
        let (rest, cond) = strip_as_long_as_prefix_for_prevention(input);
        assert_eq!(rest, input);
        assert!(cond.is_none());
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
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Double)
        );
        assert_eq!(def.valid_player, Some(ReplacementPlayerScope::Opponent));
    }

    #[test]
    fn replacement_lose_life_instead_preserves_generic_shape() {
        let def = parse_replacement_line(
            "If you would lose life, instead put one of your shields into your hand.",
            "Lich's Duel Mastery",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::LoseLife);
        assert_eq!(def.quantity_modification, None);
        assert_eq!(def.valid_player, None);
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
            assert_eq!(
                def.valid_card,
                Some(TargetFilter::SelfRef),
                "{card}: shuffle-back graveyard replacement must be self-scoped for stack resolution"
            );

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
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::SelfRef),
            "tilde subject must be self-scoped for stack resolution"
        );
    }

    /// CR 608.2n + CR 614.1a (issue #2897): card-name subjects normalize to `~`
    /// and must carry `valid_card: SelfRef`, not an absent filter that the
    /// stack-self-move gate would reject.
    #[test]
    fn graveyard_shuffle_back_card_name_subject_is_selfref() {
        let def = parse_replacement_line(
            "If Nexus of Fate would be put into a graveyard from anywhere, reveal Nexus of Fate \
             and shuffle it into its owner's library instead.",
            "Nexus of Fate",
        )
        .expect("Nexus of Fate shuffle-back must parse");
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    /// Board-wide graveyard replacements keep their external typed filter.
    #[test]
    fn graveyard_exile_card_subject_stays_external_nontoken() {
        use crate::types::ability::{FilterProp, TypedFilter};
        let def = parse_replacement_line(
            "If a card would be put into a graveyard from anywhere, exile it instead.",
            "Leyline of the Void",
        )
        .expect("Leyline-style exile must parse");
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::NonToken])
            ))
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
        assert!(matches!(
            def.mode,
            ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 }
                },
                ..
            }
        ));
        assert!(def.execute.is_none());
        // Decline branch: Tap { target: SelfRef }
        if let ReplacementMode::MayCost { decline, .. } = &def.mode {
            let decline = decline.as_ref().unwrap();
            assert!(matches!(
                *decline.effect,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
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
        assert!(matches!(
            def.mode,
            ReplacementMode::MayCost {
                cost: AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 }
                },
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

        assert!(matches!(def.mode, ReplacementMode::MayCost { .. }));
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                ..
            }
        ));
        assert!(execute.sub_ability.is_none());

        if let ReplacementMode::MayCost { decline, .. } = &def.mode {
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
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
        let cond = decline
            .condition
            .as_ref()
            .expect("Tarkir variant on_decline must carry a condition");
        match cond {
            crate::types::ability::AbilityCondition::Not { condition: inner } => {
                match inner.as_ref() {
                    crate::types::ability::AbilityCondition::ControllerControlsMatching {
                        filter: cond_filter,
                    } => match cond_filter {
                        TargetFilter::Typed(tf) => {
                            assert_eq!(tf.controller, Some(ControllerRef::You));
                            assert!(tf
                                .type_filters
                                .iter()
                                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Soldier")));
                        }
                        other => panic!("expected Typed Soldier condition filter, got {other:?}"),
                    },
                    other => panic!("expected Not(ControllerControlsMatching), got {other:?}"),
                }
            }
            other => panic!("expected Not(ControllerControlsMatching), got {other:?}"),
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
            Some(crate::types::ability::AbilityCondition::Not { condition: inner }) => {
                match inner.as_ref() {
                    crate::types::ability::AbilityCondition::ControllerControlsMatching {
                        filter,
                    } => match filter {
                        TargetFilter::Typed(tf) => {
                            assert_eq!(tf.controller, Some(ControllerRef::You));
                            assert!(tf
                                .type_filters
                                .iter()
                                .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dragon")));
                        }
                        other => panic!("expected Typed Dragon filter, got {other:?}"),
                    },
                    other => panic!("expected Not(ControllerControlsMatching), got {other:?}"),
                }
            }
            other => panic!("expected Not(ControllerControlsMatching), got {other:?}"),
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
                choice_type: ChoiceType::Color { ref excluded },
                persist: true,
                ..
            } if excluded.is_empty()
        ));
    }

    #[test]
    fn enters_tapped_then_choose_color_composes_tap_and_choice() {
        // CR 614.1c + CR 614.1d: Thriving land text ("This land enters
        // tapped. As it enters, choose a color other than green.") must compose
        // BOTH the enters-tapped modifier AND the colour choice into one Moved
        // replacement: Tap { SelfRef } (modifier) -> sub_ability(Choose).
        let def = parse_replacement_line(
            "This land enters tapped. As it enters, choose a color other than green.",
            "Thriving Grove",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        let execute = def.execute.as_ref().unwrap();
        // Primary effect is the enters-tapped event modifier.
        assert!(
            matches!(
                *execute.effect,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                }
            ),
            "primary effect must be Tap {{ SelfRef }} (enter_tapped modifier), got {:?}",
            execute.effect
        );
        // The colour choice rides as the sub-ability "real work".
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("enters-tapped choose-colour must carry the Choose as a sub-ability");
        assert!(
            matches!(
                *sub.effect,
                Effect::Choose {
                    choice_type: ChoiceType::Color { ref excluded },
                    persist: true,
                    ..
                } if excluded == &vec![ManaColor::Green]
            ),
            "sub-ability must be Choose color (excluding Green), got {:?}",
            sub.effect
        );
    }

    #[test]
    fn as_enters_choose_a_color_other_than_white() {
        let def = parse_replacement_line(
            "As this land enters, choose a color other than white.",
            "Citadel Gate",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::Color { ref excluded },
                persist: true,
                ..
            } if excluded == &vec![ManaColor::White]
        ));
    }

    #[test]
    fn as_enters_choose_two_colors() {
        let def = parse_replacement_line(
            "As this artifact enters, choose two colors.",
            "Tablet of the Guilds",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            *execute.effect,
            Effect::Choose {
                choice_type: ChoiceType::TwoColors,
                persist: true,
                ..
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
                ..
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
        assert!(matches!(def.mode, ReplacementMode::MayCost { .. }));
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
    }

    #[test]
    fn self_enters_tapped_with_counter_composes_modifiers() {
        let def = parse_replacement_line(
            "This creature enters tapped with a stun counter on it.",
            "Tonberry",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        let execute = def.execute.as_ref().expect("execute ability");
        assert!(matches!(
            *execute.effect,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
        let sub = execute.sub_ability.as_ref().expect("counter sub_ability");
        assert!(matches!(
            *sub.effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Stun
        ));
    }

    /// Issue #1988 — Slumbering Trudge. "This creature enters with a number of
    /// stun counters on it equal to three minus X. If X is 2 or less, it enters
    /// tapped." The "three minus X" arithmetic plus the trailing tapped sentence
    /// previously defeated every quantity parser, so `count` fell back to
    /// `Fixed { 1 }` (1 stun counter regardless of X). The count must be the
    /// `Offset { Multiply { -1, CostXPaid }, 3 }` expression so X=0 resolves to
    /// 3 stun counters (and X>3 clamps to 0 in the resolver).
    #[test]
    fn slumbering_trudge_enters_with_three_minus_x_stun_counters() {
        let def = parse_replacement_line(
            "This creature enters with a number of stun counters on it equal to \
             three minus X. If X is 2 or less, it enters tapped.",
            "Slumbering Trudge",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        let execute = def.execute.as_ref().expect("execute ability");
        // "it enters tapped" → Tap wrapper with the counter as its sub_ability.
        assert!(matches!(
            *execute.effect,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
        let sub = execute.sub_ability.as_ref().expect("counter sub_ability");
        match &*sub.effect {
            Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                assert_eq!(*counter_type, CounterType::Stun);
                assert_eq!(*target, TargetFilter::SelfRef);
                match count {
                    QuantityExpr::Offset { inner, offset } => {
                        assert_eq!(*offset, 3);
                        match inner.as_ref() {
                            QuantityExpr::Multiply { factor, inner } => {
                                assert_eq!(*factor, -1);
                                assert!(matches!(
                                    inner.as_ref(),
                                    QuantityExpr::Ref {
                                        qty: QuantityRef::CostXPaid
                                    }
                                ));
                            }
                            other => panic!("expected Multiply{{-1, CostXPaid}}, got {other:?}"),
                        }
                    }
                    other => panic!("expected Offset{{.., 3}}, got {other:?}"),
                }
            }
            other => panic!("expected PutCounter, got {other:?}"),
        }
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
            } if *counter_type == CounterType::Plus1Plus1
        ));
    }

    /// Issue #204 — Giada, Font of Hope. "Each other Angel you control enters
    /// with an additional +1/+1 counter on it for each Angel you already
    /// control." Defect #1: the subject `"Angel you control"` is a subtype-only
    /// phrase; the old `creature`/`permanent` keyword guard rejected it, so
    /// `valid_card` fell back to `SelfRef`. Defect #2: the `" already"` adverb
    /// defeated the `for each` count combinator, leaving `count` a `Fixed`.
    #[test]
    fn giada_other_angels_enter_with_for_each_angel_counter() {
        let def = parse_replacement_line(
            "Each other Angel you control enters with an additional +1/+1 counter \
             on it for each Angel you already control.",
            "Giada, Font of Hope",
        )
        .unwrap();

        // Defect #1: subtype-only subject accepted → external ChangeZone.
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Subtype("Angel".to_string())],
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::Another],
            })),
            "subtype-only subject must produce a Typed Angel filter with Another, not SelfRef"
        );

        // Defect #2: the count is a dynamic ObjectCount over Angels you control,
        // NOT the Fixed { value: 1 } fallback.
        match *def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                ref counter_type,
                count:
                    QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount { ref filter },
                    },
                ..
            } => {
                assert_eq!(*counter_type, CounterType::Plus1Plus1);
                assert_eq!(
                    *filter,
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![TypeFilter::Subtype("Angel".to_string())],
                        controller: Some(ControllerRef::You),
                        properties: Vec::new(),
                    })
                );
            }
            ref other => panic!("expected PutCounter with Ref ObjectCount count, got {other:?}"),
        }
    }

    /// Negative control for #204: a self-referential `~ enters with` line still
    /// resolves to a `SelfRef` valid_card and the `Moved` event — the subject
    /// acceptance check only fires after an explicit "each other" / "other".
    #[test]
    fn self_enters_with_counter_still_selfref() {
        let def = parse_replacement_line(
            "Giada, Font of Hope enters with a +1/+1 counter on it.",
            "Giada, Font of Hope",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    /// Dragonstorm Globe (#bug): "Each Dragon you control enters with an
    /// additional +1/+1 counter on it." The bare distributive "each " subject
    /// (no "other") must produce a typed Dragon filter WITHOUT `FilterProp::Another`
    /// — per CR 614.12 the general subset includes the source if it matches.
    /// Previously this fell through to `SelfRef`, so an Artifact source (which is
    /// never a Dragon) could never match an entering Dragon and the counter was
    /// never applied. External (non-SelfRef) → ChangeZone so token Dragons also
    /// receive the counter (CR 614.12).
    #[test]
    fn each_distributive_subject_no_another_changezone() {
        let def = parse_replacement_line(
            "Each Dragon you control enters with an additional +1/+1 counter on it.",
            "Dragonstorm Globe",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Subtype("Dragon".to_string())],
                controller: Some(ControllerRef::You),
                // NO FilterProp::Another for the bare "each" distributive form.
                properties: Vec::new(),
            })),
            "bare 'each [type]' must yield a typed filter WITHOUT Another (CR 614.12)"
        );
        match *def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } => assert_eq!(*counter_type, CounterType::Plus1Plus1),
            ref other => panic!("expected PutCounter Fixed(1) Plus1Plus1, got {other:?}"),
        }
    }

    /// Regression guard: the explicit "each other " form still injects
    /// `FilterProp::Another` (excludes the source) per CR 614.12.
    #[test]
    fn each_other_subject_keeps_another() {
        let def = parse_replacement_line(
            "Each other Angel you control enters with a +1/+1 counter on it.",
            "Angelic Overseer",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Subtype("Angel".to_string())],
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::Another],
            })),
            "'each other [type]' must keep FilterProp::Another (CR 614.12 excludes source)"
        );
    }

    /// Regression guard: a bare "each [non-type]" subject is rejected by the
    /// concrete-type `.filter()` guard (the word after "each " is not a card
    /// type), so it falls through to `SelfRef` rather than being mis-redirected
    /// to a typed distributive filter. This exercises the `Distributive`-scope
    /// rejection branch that the bare "each " prefix newly reaches.
    #[test]
    fn each_non_type_subject_falls_through_to_selfref() {
        // "each opponent" — "opponent" is not a `TypeFilter` variant, so
        // `parse_type_phrase` yields the `[Any]` fallback and the subject is
        // rejected, leaving the self-ETB `SelfRef`/`Moved` result.
        let def = parse_replacement_line(
            "Each opponent enters with a +1/+1 counter on it.",
            "Nonsense Source",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    /// Plain self-ETB ("~ enters with N counters on it") with no subject prefix
    /// stays `SelfRef`/`Moved` — `parse_distributive_subject` returns `None`.
    #[test]
    fn self_etb_no_subject_prefix_stays_selfref() {
        let def = parse_replacement_line(
            "This creature enters with two +1/+1 counters on it.",
            "Generic Creature",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
    }

    /// Building-block unit test: `parse_distributive_subject` must report the
    /// correct `SubjectScope` and strip the prefix, with `"each other "`
    /// winning over the shorter `"each "` (order-sensitivity contract).
    #[test]
    fn parse_distributive_subject_scopes_and_ordering() {
        assert_eq!(
            parse_distributive_subject("each other dragon you control enters with"),
            Some(("dragon you control enters with", SubjectScope::Other)),
            "'each other ' must win over the shorter 'each ' prefix"
        );
        assert_eq!(
            parse_distributive_subject("other dragon you control enters with"),
            Some(("dragon you control enters with", SubjectScope::Other)),
        );
        assert_eq!(
            parse_distributive_subject("each dragon you control enters with"),
            Some(("dragon you control enters with", SubjectScope::Distributive)),
        );
        // No distributive prefix → None (self-ETB falls through to SelfRef).
        assert_eq!(
            parse_distributive_subject("this creature enters with two counters on it"),
            None,
        );
    }

    #[test]
    fn enters_with_counters_if_event_condition() {
        let def = parse_replacement_line(
            "This creature enters with a +1/+1 counter on it if a creature died this turn.",
            "Cackling Slasher",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneChangeCountThisTurn {
                        from: Some(Zone::Battlefield),
                        to: Some(Zone::Graveyard),
                        ..
                    }
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
                active_player_req: None,
            })
        ));
    }

    #[test]
    fn enters_with_counter_for_each_creature_card_in_graveyard() {
        let def = parse_replacement_line(
            "This creature enters with a +1/+1 counter on it for each creature card in your graveyard.",
            "Golgari Grave-Troll",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ZoneCardCount {
                        zone: ZoneRef::Graveyard,
                        ref card_types,
                        scope: CountScope::Controller,
                        filter: None,
                    }
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Plus1Plus1
                && card_types.contains(&TypeFilter::Creature)
        ));
    }

    #[test]
    fn enters_with_counters_for_each_creature_that_convoked_it() {
        let def = parse_replacement_line(
            "This creature enters with two +1/+1 counters on it for each creature that convoked it.",
            "Ancient Imperiosaur",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Multiply {
                    factor: 2,
                    ref inner,
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Plus1Plus1
                && matches!(**inner, QuantityExpr::Ref { qty: QuantityRef::ConvokedCreatureCount })
        ));
    }

    #[test]
    fn enters_with_counters_for_each_color_of_mana_spent_preserves_multiplier() {
        let def = parse_replacement_line(
            "Converge — This creature enters with two +1/+1 counters on it for each color of mana spent to cast it.",
            "Glinting Creeper",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Multiply {
                    factor: 2,
                    ref inner,
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Plus1Plus1
                && matches!(**inner, QuantityExpr::Ref { qty: QuantityRef::ManaSpentToCast { scope: crate::types::ability::CastManaObjectScope::SelfObject, metric: crate::types::ability::CastManaSpentMetric::DistinctColors } })
        ));
    }

    #[test]
    fn enters_with_counters_for_each_mana_spent_uses_mana_spent_on_self() {
        let def = parse_replacement_line(
            "Verazol enters with a +1/+1 counter on it for each mana spent to cast it.",
            "Verazol, the Split Current",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast { scope: crate::types::ability::CastManaObjectScope::SelfObject, metric: crate::types::ability::CastManaSpentMetric::Total },
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Plus1Plus1
        ));
    }

    #[test]
    fn enters_with_number_of_counters_equal_to_amount_of_mana_spent() {
        let def = parse_replacement_line(
            "Gyrus enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.",
            "Gyrus, Waker of Corpses",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast { scope: crate::types::ability::CastManaObjectScope::SelfObject, metric: crate::types::ability::CastManaSpentMetric::Total },
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Plus1Plus1
        ));
    }

    #[test]
    fn enters_with_implicit_counter_count_equal_to_amount_of_mana_spent() {
        let def = parse_replacement_line(
            "The Spike Cactus enters the battlefield with +1/+1 counters on it equal to the amount of mana spent to cast it.",
            "The Spike Cactus",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::ManaSpentToCast { scope: crate::types::ability::CastManaObjectScope::SelfObject, metric: crate::types::ability::CastManaSpentMetric::Total },
                },
                target: TargetFilter::SelfRef,
            } if *counter_type == CounterType::Plus1Plus1
        ));
    }

    #[test]
    fn self_enters_with_multiple_counter_types() {
        let def = parse_replacement_line(
            "This artifact enters with a +1/+1 counter, a flying counter, a deathtouch counter, and a shield counter on it.",
            "Agent's Toolkit",
        )
        .unwrap();

        let mut cursor = def.execute.as_deref().expect("execute ability");
        let expected = [
            CounterType::Plus1Plus1,
            CounterType::Keyword(crate::types::keywords::KeywordKind::Flying),
            CounterType::Keyword(crate::types::keywords::KeywordKind::Deathtouch),
            // CR 122.1c: "shield" is now a first-class counter type (issue #1959),
            // no longer a Generic.
            CounterType::Shield,
        ];
        for counter in expected {
            assert!(matches!(
                *cursor.effect,
                Effect::PutCounter {
                    ref counter_type,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                } if *counter_type == counter
            ));
            if counter == CounterType::Shield {
                assert!(cursor.sub_ability.is_none());
            } else {
                cursor = cursor.sub_ability.as_deref().expect("next counter");
            }
        }
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
                assert_eq!(counter_type, &CounterType::Generic("charge".to_string()));
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
    fn enters_with_your_choice_of_counter_builds_selfref_choose_one_of() {
        use crate::types::keywords::KeywordKind;

        // CR 614.12a + CR 608.2d: "~ enters with your choice of <list> on it"
        // should parse to a Moved replacement whose execute is a ChooseOneOf of
        // self-targeted PutCounter branches — NOT a single Generic counter, and
        // NO ParentTarget/TargetOnly lift (the entering permanent is always the
        // recipient).
        let assert_choice = |text: &str, expected: &[CounterType]| {
            let def = parse_replacement_line(text, "Denry Klin, Editor in Chief")
                .unwrap_or_else(|| panic!("should parse: {text}"));
            assert_eq!(def.event, ReplacementEvent::Moved, "text: {text}");
            assert_eq!(
                def.valid_card,
                Some(TargetFilter::SelfRef),
                "valid_card should be SelfRef: {text}"
            );
            let Effect::ChooseOneOf { chooser, branches } = &*def.execute.as_ref().unwrap().effect
            else {
                panic!("expected ChooseOneOf execute, got {:?}", def.execute);
            };
            assert_eq!(*chooser, PlayerFilter::Controller);
            assert_eq!(branches.len(), expected.len(), "branch count: {text}");
            for (i, ct) in expected.iter().enumerate() {
                match &*branches[i].effect {
                    Effect::PutCounter {
                        counter_type,
                        target,
                        ..
                    } => {
                        assert_eq!(counter_type, ct, "branch {i} counter_type: {text}");
                        // CR 614.12a: every branch targets the entering permanent.
                        assert_eq!(
                            *target,
                            TargetFilter::SelfRef,
                            "branch {i} must be SelfRef (not ParentTarget/TargetOnly): {text}"
                        );
                    }
                    other => panic!("branch {i} expected PutCounter, got {other:?}"),
                }
            }
        };

        let expected = [
            CounterType::Plus1Plus1,
            CounterType::Keyword(KeywordKind::FirstStrike),
            CounterType::Keyword(KeywordKind::Vigilance),
        ];

        // SharedNoun shape (Denry Klin line 1).
        assert_choice(
            "Denry Klin enters with your choice of a +1/+1, first strike, or vigilance counter on it.",
            &expected,
        );
        // Distributed shape.
        assert_choice(
            "Denry Klin enters with your choice of a +1/+1 counter, a first strike counter, or a vigilance counter on it.",
            &expected,
        );
        // FromAmong shape (bare keywords).
        assert_choice(
            "Denry Klin enters with your choice of a counter from among first strike, vigilance, and lifelink on it.",
            &[
                CounterType::Keyword(KeywordKind::FirstStrike),
                CounterType::Keyword(KeywordKind::Vigilance),
                CounterType::Keyword(KeywordKind::Lifelink),
            ],
        );
    }

    #[test]
    fn enters_with_x_counters_where_x_is_life_lost_uses_quantity_binding() {
        let def = parse_replacement_line(
            "This creature enters with X +1/+1 counters on it, where X is the total life lost by your opponents this turn.",
            "Cryptborn Horror",
        )
        .unwrap();
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter {
                counter_type,
                count,
                ..
            } => {
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::LifeLostThisTurn { .. },
                        }
                    ),
                    "count should use LifeLostThisTurn, got {count:?}"
                );
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn enters_with_x_counters_where_x_partial_quantity_tail_stays_unimplemented() {
        assert!(
            parse_replacement_line(
                "This creature enters with X +1/+1 counters on it, where X is the total life lost by your opponents this turn and draw a card.",
                "Test Creature",
            )
            .is_none(),
            "malformed where-X suffix must not silently parse as CostXPaid"
        );
    }

    #[test]
    fn enters_with_x_counters_where_x_keeps_supported_arithmetic_quantity() {
        let def = parse_replacement_line(
            "This creature enters with X +1/+1 counters on it, where X is the total life lost by your opponents this turn plus one.",
            "Test Creature",
        )
        .unwrap();
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter { count, .. } => {
                assert!(
                    matches!(count, QuantityExpr::Offset { .. }),
                    "supported arithmetic where-X suffix should parse as Offset, got {count:?}"
                );
            }
            other => panic!("Expected PutCounter, got {other:?}"),
        }
    }

    #[test]
    fn enters_with_fixed_counters_remains_unchanged() {
        let def = parse_replacement_line(
            "This creature enters with three +1/+1 counters on it.",
            "Test Creature",
        )
        .unwrap();
        match &*def.execute.as_ref().unwrap().effect {
            Effect::PutCounter { count, .. } => {
                assert_eq!(count, &QuantityExpr::Fixed { value: 3 });
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
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
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
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
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
        // composes `DivideRounded { inner: CostXPaid, rounding: Up }`.
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
                assert_eq!(counter_type, &CounterType::Plus1Plus1);
                match count {
                    QuantityExpr::DivideRounded {
                        inner,
                        divisor,
                        rounding,
                    } => {
                        assert_eq!(*divisor, 2);
                        assert!(matches!(
                            inner.as_ref(),
                            QuantityExpr::Ref {
                                qty: QuantityRef::CostXPaid
                            }
                        ));
                        assert!(matches!(rounding, crate::types::ability::RoundingMode::Up));
                    }
                    other => panic!("expected DivideRounded, got {other:?}"),
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
                assert_eq!(
                    counter_type,
                    &CounterType::Plus1Plus1,
                    "counter type should be P1P1"
                );
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
            } if *counter_type == CounterType::Plus1Plus1
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
            } if *counter_type == CounterType::Plus1Plus1
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
            } if *counter_type == CounterType::Plus1Plus1
        ));
        assert_eq!(def.condition, Some(ReplacementCondition::CastViaEscape));
    }

    #[test]
    fn enters_with_counters_gated_on_web_slinging() {
        // CR 702.188a: Scarlet Spider's "Sensational Save" — the enters-with-X
        // replacement is gated by "If ~ was cast using web-slinging".
        let def = parse_replacement_line(
            "Sensational Save — If Scarlet Spider was cast using web-slinging, \
             he enters with X +1/+1 counters on him, where X is the mana value \
             of the returned creature.",
            "Scarlet Spider, Ben Reilly",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter { ref counter_type, .. }
                if *counter_type == CounterType::Plus1Plus1
        ));
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::CastVariantPaid {
                variant: CastVariantPaid::WebSlinging,
            }),
        );
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
            } if *counter_type == CounterType::Plus1Plus1
        ));
        assert_eq!(def.condition, Some(ReplacementCondition::CastViaEscape));
    }

    #[test]
    fn plural_subject_escape_with_counter_keeps_escape_condition() {
        // CR 702.138c: plural-subject "escape with" is still escape-gated, not
        // an unconditional battlefield-entry counter replacement.
        let def = parse_replacement_line(
            "Creatures you control escape with a +1/+1 counter on them.",
            "Escape Anthem",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Fixed { value: 1 },
                ..
            } if *counter_type == CounterType::Plus1Plus1
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
            } if *counter_type == CounterType::Plus1Plus1
        ));
        assert!(matches!(
            def.condition,
            Some(ReplacementCondition::CastViaKicker {
                variant: None,
                kicker_cost: None
            })
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
            } if *counter_type == CounterType::Plus1Plus1
        ));
        // CR 702.33d + CR 702.33f: per-variant resolution is deferred, but the
        // parser keeps typed cost metadata so synthesis can map it to the card's
        // positional `KickerVariant`.
        match &def.condition {
            Some(ReplacementCondition::CastViaKicker {
                variant: None,
                kicker_cost: Some(_),
            }) => {}
            other => panic!(
                "Expected CastViaKicker {{ variant: None, kicker_cost: Some(_) }}, got {other:?}"
            ),
        }
    }

    #[test]
    fn enters_with_counter_for_each_time_kicked_uses_kicker_count() {
        let def = parse_replacement_line(
            "This creature enters with a +1/+1 counter on it for each time it was kicked.",
            "Apex Hawks",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                ref counter_type,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::KickerCount
                },
                ..
            } if *counter_type == CounterType::Plus1Plus1
        ));
    }

    #[test]
    fn enters_with_two_counters_for_each_time_kicked_preserves_multiplier() {
        let def = parse_replacement_line(
            "This creature enters with two +1/+1 counters on it for each time it was kicked.",
            "Synthetic Multikicker",
        )
        .unwrap();
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::PutCounter {
                count: QuantityExpr::Multiply {
                    factor: 2,
                    ref inner,
                },
                ..
            } if matches!(**inner, QuantityExpr::Ref { qty: QuantityRef::KickerCount })
        ));
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
        // CR 730.3e: "a card or token" names tokens explicitly — token-inclusive,
        // so NO `NonToken` constraint and (with `Any` scope) no `valid_card` at all.
        assert!(def.valid_card.is_none()); // matches all objects, tokens included
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
        // valid_card should scope to opponent-owned cards AND exclude tokens:
        // CR 730.3e + CR 111.1 — "a card" (no "or token") is token-excluding, so a
        // dying token reaches the graveyard (dies-triggers fire — Blood Artist
        // class) instead of being wrongly exiled.
        match &def.valid_card {
            Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
                assert!(properties.contains(&FilterProp::Owned {
                    controller: ControllerRef::Opponent,
                }));
                assert!(
                    properties.contains(&FilterProp::NonToken),
                    "'a card' subject must exclude tokens (CR 730.3e)"
                );
            }
            other => panic!("Expected Typed filter with Owned + NonToken, got {other:?}"),
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

    /// CR 730.3e + CR 111.1: a card-only subject targeting ANY graveyard ("a
    /// card would be put into a graveyard") is token-EXCLUDING with no
    /// controller scope — `valid_card` is `NonToken` alone. This is the live
    /// Leyline-class bug fix: without the `NonToken` axis a dying token was
    /// wrongly redirected (exiled), suppressing dies-triggers.
    #[test]
    fn card_only_any_graveyard_excludes_tokens() {
        let def = parse_replacement_line(
            "If a card would be put into a graveyard from anywhere, exile it instead.",
            "Some Card-Scoped Hoser",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.destination_zone, Some(Zone::Graveyard));
        match &def.valid_card {
            Some(TargetFilter::Typed(TypedFilter { properties, .. })) => {
                assert!(
                    properties.contains(&FilterProp::NonToken),
                    "'a card' subject must exclude tokens (CR 730.3e)"
                );
                assert!(
                    !properties.contains(&FilterProp::Owned {
                        controller: ControllerRef::Opponent,
                    }),
                    "any-graveyard scope must not add an owner constraint"
                );
            }
            other => panic!("Expected Typed filter with NonToken, got {other:?}"),
        }
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

    #[test]
    fn creature_damaged_by_this_source_die_exile_replacement() {
        let def = parse_replacement_line(
            "If a creature dealt damage by this creature this turn would die, exile it instead.",
            "Frostwielder",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Destroy);
        assert_eq!(def.destination_zone, None);
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::DealtDamageThisTurnBySource {
                source: TargetFilter::SelfRef,
            })
        );
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
    fn creature_damaged_by_enchanted_source_die_exile_replacement() {
        let def = parse_replacement_line(
            "If a creature dealt damage by enchanted creature this turn would die, exile it instead.",
            "Kumano's Blessing",
        )
        .unwrap();
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::DealtDamageThisTurnBySource {
                source: TargetFilter::AttachedTo,
            })
        );
    }

    #[test]
    fn creature_damaged_by_spider_you_controlled_replacement_source_filter() {
        let (rest, source) =
            parse_damage_history_source("a spider you controlled would die").unwrap();
        assert_eq!(rest, " would die");
        assert_eq!(
            source,
            TargetFilter::Typed(
                TypedFilter::default()
                    .subtype("Spider".to_string())
                    .controller(ControllerRef::You)
            )
        );
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
                    &vec![(
                        CounterType::Generic("ice".to_string()),
                        QuantityExpr::Fixed { value: 1 },
                    )]
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
                    &vec![(
                        CounterType::Generic("egg".to_string()),
                        QuantityExpr::Fixed { value: 3 },
                    )]
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
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
    fn uphill_battle_played_by_opponents_enter_tapped() {
        let text = "Creatures played by your opponents enter the battlefield tapped.";
        assert!(
            parse_external_enters_tapped(&text.to_lowercase(), text).is_some(),
            "external entry parser must match Uphill Battle"
        );
        let def =
            parse_replacement_line(text, "Uphill Battle").expect("Uphill Battle played-by entry");
        assert_eq!(def.event, ReplacementEvent::ChangeZone);
        assert_eq!(def.destination_zone, Some(Zone::Battlefield));
        match &def.valid_card {
            Some(TargetFilter::Typed(tf)) => {
                assert!(tf.type_filters.contains(&TypeFilter::Creature));
                assert_eq!(tf.controller, Some(ControllerRef::Opponent));
                assert!(tf.properties.contains(&FilterProp::WasPlayed));
            }
            other => panic!("Expected Typed filter with WasPlayed, got {other:?}"),
        }
    }

    #[test]
    fn played_by_opponents_entry_covers_creature_and_land() {
        for (text, card, type_filter) in [
            (
                "Creatures played by your opponents enter the battlefield tapped.",
                "Uphill Battle",
                TypeFilter::Creature,
            ),
            (
                "Lands played by your opponents enter tapped.",
                "Contamination",
                TypeFilter::Land,
            ),
        ] {
            let def = parse_replacement_line(text, card)
                .unwrap_or_else(|| panic!("failed to parse {text}"));
            assert_eq!(def.event, ReplacementEvent::ChangeZone);
            match &def.valid_card {
                Some(TargetFilter::Typed(tf)) => {
                    assert!(tf.type_filters.contains(&type_filter));
                    assert!(tf.properties.contains(&FilterProp::WasPlayed));
                }
                other => panic!("expected Typed filter, got {other:?}"),
            }
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
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
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
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

    /// CR 614.1d: "If you control N or more other lands, this land enters tapped."
    /// Covers Lair of the Hydra, Hall of Storm Giants, Celestial Colonnade, etc.
    /// The replacement must apply (enter tapped) when the controller has ≥ N other lands.
    #[test]
    fn if_controls_two_or_more_other_lands_enters_tapped() {
        let def = parse_replacement_line(
            "If you control two or more other lands, this land enters tapped.",
            "Test Land",
        )
        .expect("creature-land conditional ETB must parse");
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert!(matches!(def.mode, ReplacementMode::Mandatory));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
        match &def.condition {
            Some(ReplacementCondition::IfControlsMatching { minimum, filter }) => {
                assert_eq!(*minimum, 2);
                let TargetFilter::Typed(tf) = filter else {
                    panic!("Expected Typed filter, got {filter:?}");
                };
                assert!(
                    tf.type_filters.contains(&TypeFilter::Land),
                    "filter must match lands"
                );
                assert_eq!(
                    tf.controller,
                    Some(ControllerRef::You),
                    "filter must be controller-scoped to You"
                );
                assert!(
                    tf.properties.contains(&FilterProp::Another),
                    "filter must require 'other' (Another property)"
                );
            }
            other => panic!("Expected IfControlsMatching, got {other:?}"),
        }
    }

    /// CR 614.1d: The "if you control" pattern must NOT fall through to the
    /// unconditional enters-tapped handler. Regression guard.
    #[test]
    fn if_controls_pattern_does_not_match_unconditional() {
        let def = parse_replacement_line(
            "If you control two or more other lands, this land enters tapped.",
            "Test Land",
        )
        .unwrap();
        // Must have a non-None condition — the unconditional handler would produce None.
        assert!(
            def.condition.is_some(),
            "conditional ETB must not produce unconditional replacement"
        );
    }

    /// CR 614.1d: Generality — three or more threshold.
    #[test]
    fn if_controls_three_or_more_other_lands() {
        let def = parse_replacement_line(
            "If you control three or more other lands, this land enters tapped.",
            "Hypothetical Land",
        )
        .expect("three-or-more variant must parse");
        match &def.condition {
            Some(ReplacementCondition::IfControlsMatching { minimum, .. }) => {
                assert_eq!(*minimum, 3);
            }
            other => panic!("Expected IfControlsMatching, got {other:?}"),
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
    fn uncivil_unrest_double_damage_parses_creature_source_filter() {
        let def = parse_replacement_line(
            "If a creature you control with a +1/+1 counter on it would deal damage to a permanent or player, it deals double that damage instead.",
            "Uncivil Unrest",
        )
        .expect("Uncivil Unrest replacement should parse");
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        let Some(TargetFilter::Typed(tf)) = def.damage_source_filter else {
            panic!(
                "expected typed damage source filter, got {:?}",
                def.damage_source_filter
            );
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Counters {
                    counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                    ..
                }
            )),
            "expected +1/+1 counter qualifier, got {:?}",
            tf.properties
        );
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
            Some(damage_target_opponent_or_permanents())
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
            Some(damage_target_opponent_or_permanents())
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
    fn damage_collective_inferno_double_all_chosen_type() {
        // Collective Inferno: "Double all damage that sources you control of the chosen type would deal"
        let def = parse_replacement_line(
            "Double all damage that sources you control of the chosen type would deal.",
            "Collective Inferno",
        )
        .expect("Collective Inferno static should parse");
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf.properties.contains(&FilterProp::IsChosenCreatureType));
            }
            other => panic!("Expected Typed filter with IsChosenCreatureType, got {other:?}"),
        }
    }

    #[test]
    fn damage_double_all_typed_subject_with_counters() {
        let def = parse_replacement_line(
            "Double all damage that creatures you control with counters on them would deal.",
            "Raphael, the Muscle",
        )
        .expect("typed no-instead damage doubler should parse");
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        let Some(TargetFilter::Typed(tf)) = def.damage_source_filter else {
            panic!(
                "expected typed damage source filter, got {:?}",
                def.damage_source_filter
            );
        };
        assert!(tf.type_filters.contains(&TypeFilter::Creature));
        assert_eq!(tf.controller, Some(ControllerRef::You));
        assert!(
            tf.properties.iter().any(|p| matches!(
                p,
                FilterProp::Counters {
                    counters: CounterMatch::Any,
                    comparator: Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }
            )),
            "expected any-counter qualifier, got {:?}",
            tf.properties
        );
    }

    #[test]
    fn damage_double_all_goblin_sources() {
        // Type-filtered variant
        let def = parse_replacement_line(
            "Double all damage that Goblin sources you control would deal.",
            "Goblin Doubler",
        )
        .expect("Goblin doubler should parse");
        assert_eq!(def.damage_modification, Some(DamageModification::Double));
        match def.damage_source_filter.unwrap() {
            TargetFilter::Typed(tf) => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert_eq!(tf.get_subtype(), Some("Goblin"));
            }
            other => panic!("Expected Typed filter with Goblin subtype, got {other:?}"),
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
        assert_eq!(def.damage_target_filter, Some(damage_target_any_player()));
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

    /// CR 707.9 + CR 614.1c: Mirror Image / Waxen Shapethief — "enter as a copy
    /// of a creature you control" with no zone phrase and no except clause. The
    /// controller-scoped filter must parse despite the sentence-final period
    /// (previously left as `parse_type_phrase` leftover, dropping the clone).
    #[test]
    fn clone_creature_you_control_no_zone_phrase() {
        let mirror = parse_replacement_line(
            "You may have this creature enter as a copy of a creature you control.",
            "Mirror Image",
        )
        .expect("Mirror Image clone should parse");
        assert_eq!(mirror.event, ReplacementEvent::Moved);
        match &*mirror.execute.as_ref().unwrap().effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Creature));
                    assert_eq!(
                        tf.controller,
                        Some(ControllerRef::You),
                        "filter must be scoped to creatures you control",
                    );
                }
                other => panic!("Expected Typed creature filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }

        // Same class with a union filter (Waxen Shapethief) must also parse.
        let waxen = parse_replacement_line(
            "You may have this creature enter as a copy of an artifact or creature you control.",
            "Waxen Shapethief",
        )
        .expect("Waxen Shapethief clone should parse");
        assert_eq!(waxen.event, ReplacementEvent::Moved);
        assert!(matches!(
            &*waxen.execute.as_ref().unwrap().effect,
            Effect::BecomeCopy { .. }
        ));
    }

    /// CR 707.9a + CR 702.3: Wall of Stolen Identity — clone except adds Wall
    /// subtype and defender via the "and has defender" shorthand.
    #[test]
    fn clone_wall_of_stolen_identity_except_defender() {
        let def = parse_replacement_line(
            "You may have this creature enter as a copy of any creature on the battlefield, \
             except it's a Wall in addition to its other types and has defender. \
             When you do, tap the copied creature and it doesn't untap during its controller's \
             untap step for as long as you control this creature.",
            "Wall of Stolen Identity",
        )
        .unwrap();
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        match &*execute.effect {
            Effect::BecomeCopy {
                additional_modifications,
                ..
            } => {
                use crate::types::keywords::Keyword;
                assert!(
                    !additional_modifications.is_empty(),
                    "expected except-clause modifications, got {additional_modifications:?}"
                );
                assert!(
                    additional_modifications.iter().any(|m| {
                        matches!(m, ContinuousModification::AddSubtype { subtype } if subtype == "Wall")
                    }),
                    "expected Wall subtype addition, got {additional_modifications:?}"
                );
                assert!(additional_modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Defender
                        }
                    )
                }));
            }
            other => panic!("expected BecomeCopy, got {other:?}"),
        }
        assert!(
            execute.sub_ability.is_some(),
            "When you do reflexive trigger should be sub_ability"
        );
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
    fn clone_enter_tapped_as_copy_vesuva() {
        // CR 614.1c + CR 707.9: "enter tapped as a copy" composes Tap { SelfRef }
        // as the top-level execute with BecomeCopy as its sub_ability. The replacement
        // pipeline walks the chain: event_modifiers_for_ability extracts EtbTapState::Tapped
        // from Tap, then first_non_modifier_ability finds BecomeCopy for CopyTargetChoice.
        let def = parse_replacement_line(
            "You may have Vesuva enter tapped as a copy of any land on the battlefield.",
            "Vesuva",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::Moved);
        assert!(matches!(
            def.mode,
            ReplacementMode::Optional { decline: None }
        ));
        let execute = def.execute.as_ref().unwrap();
        assert!(
            matches!(
                &*execute.effect,
                Effect::SetTapState {
                    target: TargetFilter::SelfRef,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                }
            ),
            "top-level execute must be Tap {{ SelfRef }}, got {:?}",
            execute.effect
        );
        let sub = execute
            .sub_ability
            .as_ref()
            .expect("sub_ability must carry BecomeCopy");
        match &*sub.effect {
            Effect::BecomeCopy { target, .. } => match target {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Land));
                }
                other => panic!("Expected Typed land filter, got {other:?}"),
            },
            other => panic!("Expected BecomeCopy in sub_ability, got {other:?}"),
        }
    }

    #[test]
    fn clone_enter_tapped_as_copy_echoing_deeps() {
        // CR 614.1c: Graveyard source zone + "except it's a Cave" modification
        let def = parse_replacement_line(
            "You may have this land enter tapped as a copy of any land card in a graveyard, except it's a Cave in addition to its other types.",
            "Echoing Deeps",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            &*execute.effect,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
        let sub = execute.sub_ability.as_ref().unwrap();
        match &*sub.effect {
            Effect::BecomeCopy {
                additional_modifications,
                ..
            } => {
                assert!(
                    additional_modifications.contains(&ContinuousModification::AddSubtype {
                        subtype: "Cave".to_string(),
                    })
                );
            }
            other => panic!("Expected BecomeCopy, got {other:?}"),
        }
    }

    #[test]
    fn clone_enter_tapped_as_copy_callidus_assassin_grants_etb_trigger() {
        let def = parse_replacement_line(
            "Polymorphine — You may have this creature enter tapped as a copy of any creature on the battlefield, except it has \"When this creature enters, destroy up to one other target creature with the same name as this creature.\"",
            "Callidus Assassin",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(matches!(
            &*execute.effect,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Tap,
            }
        ));
        let sub = execute.sub_ability.as_ref().unwrap();
        let Effect::BecomeCopy {
            additional_modifications,
            ..
        } = &*sub.effect
        else {
            panic!("Expected BecomeCopy, got {:?}", sub.effect);
        };
        let [ContinuousModification::GrantTrigger { trigger }] =
            additional_modifications.as_slice()
        else {
            panic!("expected one GrantTrigger, got {additional_modifications:?}");
        };
        let execute = trigger
            .execute
            .as_ref()
            .expect("granted trigger must execute");
        let Effect::Destroy { target, .. } = &*execute.effect else {
            panic!("expected Destroy effect, got {:?}", execute.effect);
        };
        let TargetFilter::Typed(filter) = target else {
            panic!("expected typed target, got {target:?}");
        };
        assert!(filter.type_filters.contains(&TypeFilter::Creature));
        assert!(filter.properties.contains(&FilterProp::Another));
        assert!(filter.properties.contains(&FilterProp::SameName));
    }

    #[test]
    fn clone_without_tapped_still_direct_become_copy() {
        // Non-tapped clone (Phantasmal Image class) must NOT compose through Tap
        let def = parse_replacement_line(
            "You may have Clone enter as a copy of any creature on the battlefield.",
            "Clone",
        )
        .unwrap();
        let execute = def.execute.as_ref().unwrap();
        assert!(
            matches!(&*execute.effect, Effect::BecomeCopy { .. }),
            "non-tapped clone must have BecomeCopy as top-level, got {:?}",
            execute.effect
        );
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
    fn solemnity_players_cant_get_counters_replacement() {
        let def = parse_replacement_line("Players can't get counters.", "Solemnity")
            .expect("Solemnity player-counter line must parse");
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Prevent)
        );
        assert_eq!(
            def.valid_player,
            Some(ReplacementPlayerScope::AnyPlayer),
            "Solemnity must apply to every player, not only its controller"
        );
    }

    #[test]
    fn solemnity_permanent_types_cant_get_counters_replacement() {
        let def = parse_replacement_line(
            "Counters can't be put on artifacts, creatures, enchantments, or lands.",
            "Solemnity",
        )
        .expect("Solemnity object-counter line must parse");
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Prevent)
        );
        assert!(matches!(
            def.valid_card,
            Some(TargetFilter::Typed(TypedFilter {
                type_filters,
                controller: None,
                properties,
                ..
            })) if type_filters == vec![TypeFilter::AnyOf(vec![
                TypeFilter::Artifact,
                TypeFilter::Creature,
                TypeFilter::Enchantment,
                TypeFilter::Land,
            ])] && properties == vec![FilterProp::InZone {
                zone: Zone::Battlefield
            }]
        ));
    }

    #[test]
    fn counter_agnostic_one_or_more_does_not_set_counter_match() {
        // CR 614.1a + CR 122.1: Sanity check — "if an effect would put one
        // or more counters on a permanent you control" (Doubling Season's
        // modern wording) must NOT be treated as type-specific. The
        // counter-agnostic wording leaves counter_match = None so the
        // replacement matches every counter type.
        let def = parse_replacement_line(
            "If an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.",
            "Doubling Season",
        )
        .unwrap();
        assert_eq!(def.counter_match, None);
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
        // CR 122.1a + CR 614.1a: Doubling Season's modern wording uses "those
        // counters" — counter-agnostic, so no `counter_match` is set.
        assert_eq!(def.counter_match, None);
    }

    #[test]
    fn counter_plus_one_replacement_hardened_scales() {
        // CR 614.1a + CR 122.1a: Hardened Scales — "+1/+1 counters" specifically.
        let def = parse_replacement_line(
            "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.",
            "Hardened Scales",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Plus { value: 1 })
        );
        assert_eq!(
            def.counter_match,
            Some(CounterMatch::OfType(CounterType::Plus1Plus1))
        );
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
    fn counter_minus_one_replacement_vizier_of_remedies() {
        // CR 614.1a + CR 122.1a: Vizier of Remedies — "-1/-1 counters"
        // specifically. The "minus one" follows the type token in this
        // wording (vs. Hardened Scales's "that many plus one"), so the
        // parser falls through to the " counters minus " branch.
        let def = parse_replacement_line(
            "If one or more -1/-1 counters would be put on a creature you control, that many -1/-1 counters minus one are put on it instead.",
            "Vizier of Remedies",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Minus { value: 1 })
        );
        assert_eq!(
            def.counter_match,
            Some(CounterMatch::OfType(CounterType::Minus1Minus1))
        );
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
    fn no_counters_replacement_melira_keepers() {
        // CR 614.6 + CR 614.7 + CR 122.1: Melira's Keepers — Human Scout that
        // can't be counter-targeted. The Oracle line is normalized to "~ can't
        // have counters put on it." before reaching the parser; the resulting
        // replacement is self-targeted (valid_card: SelfRef) and uses the
        // `Prevent` quantity-modification variant so the applier returns
        // ApplyResult::Prevented.
        use crate::types::ability::QuantityModification;
        let def = parse_replacement_line(
            "This creature can't have counters put on it.",
            "Melira's Keepers",
        )
        .expect("Melira's Keepers replacement must parse");
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Prevent)
        );
        // CR 122.1: counter-type-agnostic — applies to every counter type
        // (loyalty, charge, +1/+1, -1/-1, …).
        assert_eq!(def.counter_match, None);
    }

    #[test]
    fn no_counters_replacement_tilde_form() {
        // The parser receives self-ref-normalized text. Verify the typed form
        // ("~ can't have counters put on it.") parses identically — the
        // upstream normalization step is the single authority for the
        // "this creature" → "~" rewrite.
        use crate::types::ability::QuantityModification;
        let def = parse_replacement_line("~ can't have counters put on it.", "Some Creature")
            .expect("tilde-form must parse");
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(def.valid_card, Some(TargetFilter::SelfRef));
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Prevent)
        );
    }

    #[test]
    fn inverted_typed_counter_prohibition_covers_every_permanent_type() {
        // CR 614.6 + CR 122.1: "<type> can't have counters put on them" lowers to
        // the AddCounter+Prevent replacement scoped to that permanent type. The
        // single combinator covers every permanent type, so creatures (#3450),
        // planeswalkers (#3453), artifacts (#3455, #3502), and lands are all
        // handled by one arm — no per-type parallel tests needed.
        for (oracle_type, expected) in [
            ("Creatures", TypeFilter::Creature),
            ("Planeswalkers", TypeFilter::Planeswalker),
            ("Artifacts", TypeFilter::Artifact),
            ("Enchantments", TypeFilter::Enchantment),
            ("Lands", TypeFilter::Land),
        ] {
            let text = format!("{oracle_type} can't have counters put on them.");
            let def = parse_replacement_line(&text, "Test Card")
                .unwrap_or_else(|| panic!("{oracle_type} counter prohibition must parse"));
            assert_eq!(def.event, ReplacementEvent::AddCounter);
            assert_eq!(
                def.quantity_modification,
                Some(QuantityModification::Prevent)
            );
            assert!(
                matches!(
                    &def.valid_card,
                    Some(TargetFilter::Typed(tf))
                        if tf.type_filters == vec![expected.clone()]
                            && tf.controller.is_none()
                            && tf.properties.iter().any(|p| matches!(
                                p,
                                FilterProp::InZone { zone: Zone::Battlefield }
                            ))
                ),
                "{oracle_type} must scope to {expected:?} on the battlefield"
            );
        }
    }

    #[test]
    fn inverted_typed_counter_prohibition_handles_multiple_types() {
        // CR 614.6: comma/or-separated type lists reuse the shared type-list
        // combinator, so "Creatures or artifacts" lowers to a TypeFilter::AnyOf.
        let def = parse_replacement_line(
            "Creatures or artifacts can't have counters put on them.",
            "T",
        )
        .expect("multi-type counter prohibition must parse");
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert!(matches!(
            def.valid_card,
            Some(TargetFilter::Typed(tf))
                if tf.type_filters == vec![TypeFilter::AnyOf(vec![
                    TypeFilter::Creature,
                    TypeFilter::Artifact,
                ])]
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
        assert_eq!(def.damage_target_filter, Some(damage_target_controller()));
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
        assert_eq!(def.damage_target_filter, Some(damage_target_controller()));
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
                    qty: QuantityRef::HandSize {
                        player: crate::types::ability::PlayerScope::Controller
                    },
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
    fn draw_replacement_leading_instead_prefix_blood_scrivener() {
        // CR 614.1a: "instead you draw two cards" — leading "instead" form with
        // subject prefix. The replacement must wire up Draw {count:2} as the
        // execute effect and LoseLife as a sub_ability, gated on HandSize == 0.
        // Regression test for issue #3305: "instead" was not stripped from the
        // effect text, leaving the draw as Unimplemented and only the life loss
        // fired.
        let def = parse_replacement_line(
            "If you would draw a card while you have no cards in hand, instead you draw two cards and you lose 1 life.",
            "Blood Scrivener",
        )
        .unwrap();

        assert_eq!(def.event, ReplacementEvent::Draw);
        // Gate: HandSize EQ 0
        assert!(matches!(
            &def.condition,
            Some(ReplacementCondition::OnlyIfQuantity {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::HandSize {
                        player: crate::types::ability::PlayerScope::Controller
                    }
                },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
                active_player_req: None,
            })
        ));
        // Execute: Draw { count: 2 }
        assert!(matches!(
            def.execute.as_deref().map(|a| &*a.effect),
            Some(Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            })
        ));
        // Sub-ability: LoseLife { amount: 1 }
        assert!(matches!(
            def.execute
                .as_deref()
                .and_then(|a| a.sub_ability.as_deref())
                .map(|a| &*a.effect),
            Some(Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                ..
            })
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
        assert_eq!(
            def.mana_replacement_scope,
            ManaReplacementScope::TappedForMana
        );
    }

    #[test]
    fn mana_replacement_multiplies_tapped_permanent_mana() {
        // CR 106.12b + CR 614.1a: Nyxbloom Ancient multiplies the mana
        // production event for permanents you tap for mana.
        let def = parse_replacement_line(
            "If you tap a permanent for mana, it produces three times as much of that mana instead.",
            "Nyxbloom Ancient",
        )
        .unwrap();
        assert_eq!(def.event, ReplacementEvent::ProduceMana);
        assert_eq!(
            def.mana_modification,
            Some(ManaModification::Multiply { factor: 3 })
        );
        assert_eq!(
            def.mana_replacement_scope,
            ManaReplacementScope::TappedForMana
        );
        assert_eq!(
            def.valid_card,
            Some(TargetFilter::Typed(
                TypedFilter::permanent().controller(ControllerRef::You)
            ))
        );
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
    /// entering creature with a self-scoped spent-mana counter quantity.
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

        // execute: PutCounter { target: SelfRef, count: Ref(self spent-mana colors) }.
        let exec = def.execute.as_ref().expect("execute set");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*exec.effect
        else {
            panic!("expected PutCounter, got {:?}", exec.effect);
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert_eq!(target, &TargetFilter::SelfRef);
        assert_eq!(
            count,
            &QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: crate::types::ability::CastManaObjectScope::SelfObject,
                    metric: crate::types::ability::CastManaSpentMetric::DistinctColors
                }
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
        assert_eq!(spec.characteristics.power, Some(1));
        assert_eq!(spec.characteristics.toughness, Some(1));
        assert_eq!(spec.characteristics.core_types, vec![CoreType::Creature]);
        assert_eq!(spec.characteristics.subtypes, vec!["Squirrel".to_string()]);
        assert_eq!(spec.characteristics.colors, vec![ManaColor::Green]);
    }

    /// CR 614.1a + CR 111.1: Peregrin Took's "those tokens plus an additional
    /// Food token are created instead" replacement.
    #[test]
    fn parses_peregrin_took_additional_food_token() {
        let text = "If one or more tokens would be created under your control, those tokens plus an additional Food token are created instead.";
        let def = parse_replacement_line(text, "Peregrin Took").expect("should parse Peregrin");
        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert_eq!(def.token_owner_scope, Some(ControllerRef::You));
        let spec = def
            .additional_token_spec
            .as_ref()
            .expect("additional Food token spec");
        assert_eq!(spec.characteristics.subtypes, vec!["Food".to_string()]);
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
        // CR 614.1a + CR 109.5: "If you would create..." is scoped to the
        // source's controller, so the replacement must not fire for tokens
        // created by other players (issue #1967).
        assert_eq!(
            def.token_owner_scope,
            Some(ControllerRef::You),
            "Xorn 'if you would create' must scope to the controller's tokens"
        );
        let spec = def
            .additional_token_spec
            .as_ref()
            .expect("Xorn must populate additional_token_spec");
        assert!(
            spec.characteristics
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Treasure")),
            "appended spec must be a Treasure token, got {:?}",
            spec.characteristics.subtypes
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

        // CR 614.1a + CR 109.5: "If you would create..." is scoped to the
        // source's controller, so the replacement must not fire for tokens
        // created by other players (issue #1967).
        assert_eq!(
            def.token_owner_scope,
            Some(ControllerRef::You),
            "Manufactor 'if you would create' must scope to the controller's tokens"
        );

        let specs = def
            .ensure_token_specs
            .as_ref()
            .expect("Manufactor must populate ensure_token_specs");
        assert_eq!(specs.len(), 3);
        let subtypes_present: Vec<String> = specs
            .iter()
            .flat_map(|s| s.characteristics.subtypes.clone())
            .collect();
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

    /// CR 121.1 + CR 504.1 + CR 614.6 — the shared exception-clause detector
    /// must accept both `you/your` (Alhammarret's Archive) and `they/their`
    /// (Orcish Bowmasters) phrasings, scan past leading prefix text, and
    /// reject near-miss phrases that do not contain the exact clause.
    #[test]
    fn except_first_draw_in_draw_step_clause_recognizes_both_subjects() {
        // Alhammarret's Archive
        assert!(super::has_except_first_draw_in_draw_step_clause(
            "if you would draw a card except the first one you draw in each of your draw steps, draw two cards instead."
        ));
        // Orcish Bowmasters
        assert!(super::has_except_first_draw_in_draw_step_clause(
            "whenever an opponent draws a card except the first one they draw in each of their draw steps, ~ deals 1 damage to any target."
        ));
        // Bare clause (combinator must scan, not require any prefix).
        assert!(super::has_except_first_draw_in_draw_step_clause(
            "except the first one you draw in each of your draw steps"
        ));
        // Negative — no exception clause present.
        assert!(!super::has_except_first_draw_in_draw_step_clause(
            "if you would draw a card, draw two cards instead."
        ));
        // Negative — wrong phase ("upkeeps" instead of "draw steps").
        assert!(!super::has_except_first_draw_in_draw_step_clause(
            "except the first one you draw in each of your upkeeps"
        ));
    }

    #[test]
    fn tekuthal_proliferate_replacement_parses() {
        let def = parse_replacement_line(
            "If you would proliferate, proliferate twice instead.",
            "Tekuthal, Inquiry Dominus",
        )
        .expect("Tekuthal proliferate replacement");

        assert_eq!(def.event, ReplacementEvent::Proliferate);
        assert_eq!(
            def.valid_player,
            Some(ReplacementPlayerScope::You),
            "controller-scoped proliferate replacement"
        );
        let execute = def.execute.expect("execute ability");
        assert!(matches!(*execute.effect, Effect::Proliferate));
        assert_eq!(
            execute.repeat_for,
            Some(QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            }),
            "proliferate twice instead → repeat_for Multiply(2 × event count) so stacked doublers compound"
        );
    }

    #[test]
    fn max_speed_draw_replacement_gets_replacement_condition() {
        let def = parse_replacement_line(
            "Max speed \u{2014} If you would draw a card, draw two cards instead.",
            "Vnwxt, Verbose Host",
        )
        .expect("max speed draw replacement parses");

        assert_eq!(def.event, ReplacementEvent::Draw);
        assert_eq!(def.condition, Some(ReplacementCondition::HasMaxSpeed));
        assert!(matches!(
            *def.execute.as_ref().unwrap().effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ));
    }

    /// CR 614.1a + CR 121.1: Opponent draw replacements with the shared
    /// except-first-draw-in-draw-step clause (Notion Thief / Hullbreacher class).
    #[test]
    fn parses_opponent_draw_replacement_except_first_draw_in_step() {
        let notion_thief = parse_replacement_line(
            "If an opponent would draw a card except the first one they draw in each of their draw steps, instead that player skips that draw and you draw a card.",
            "Notion Thief",
        )
        .expect("Notion Thief draw replacement");
        assert_eq!(notion_thief.event, ReplacementEvent::Draw);
        assert_eq!(
            notion_thief.valid_player,
            Some(ReplacementPlayerScope::Opponent)
        );
        assert_eq!(
            notion_thief.condition,
            Some(ReplacementCondition::ExceptFirstDrawInDrawStep)
        );
        assert!(
            notion_thief.execute.is_some(),
            "replacement execute chain must be present"
        );

        let hullbreacher = parse_replacement_line(
            "If an opponent would draw a card except the first one they draw in each of their draw steps, instead you create a Treasure token.",
            "Hullbreacher",
        )
        .expect("Hullbreacher draw replacement");
        assert_eq!(hullbreacher.event, ReplacementEvent::Draw);
        assert_eq!(
            hullbreacher.valid_player,
            Some(ReplacementPlayerScope::Opponent)
        );
        assert_eq!(
            hullbreacher.condition,
            Some(ReplacementCondition::ExceptFirstDrawInDrawStep)
        );
    }

    /// CR 614.1a: Global-player draw replacement (Chains of Mephistopheles class).
    #[test]
    fn parses_any_player_draw_replacement_except_first_draw_in_step() {
        let def = parse_replacement_line(
            "If a player would draw a card except the first one they draw in each of their draw steps, that player discards a card instead.",
            "Chains of Mephistopheles",
        )
        .expect("Chains draw replacement antecedent");
        assert_eq!(def.event, ReplacementEvent::Draw);
        assert_eq!(def.valid_player, Some(ReplacementPlayerScope::AnyPlayer));
        assert_eq!(
            def.condition,
            Some(ReplacementCondition::ExceptFirstDrawInDrawStep)
        );
    }

    #[test]
    fn parses_opponent_mill_replacement_with_multiplier() {
        let text =
            "If an opponent would mill one or more cards, they mill twice that many cards instead.";
        let def = parse_replacement_line(text, "Bruvac the Grandiloquent")
            .expect("must parse mill replacement");

        assert_eq!(def.event, ReplacementEvent::Mill);
        assert_eq!(def.valid_player, Some(ReplacementPlayerScope::Opponent));
        let execute = def.execute.as_ref().expect("mill replacement must execute");
        match &*execute.effect {
            Effect::Mill {
                count,
                target,
                destination,
            } => {
                assert_eq!(target, &TargetFilter::Controller);
                assert_eq!(destination, &Zone::Graveyard);
                assert_eq!(
                    count,
                    &QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount
                        })
                    }
                );
            }
            other => panic!("expected Mill execute, got {other:?}"),
        }
    }

    #[test]
    fn parses_opponent_mill_replacement_with_offset() {
        let text = "If an opponent would mill one or more cards, they mill that many cards plus four instead.";
        let def =
            parse_replacement_line(text, "The Water Crystal").expect("must parse mill replacement");

        assert_eq!(def.event, ReplacementEvent::Mill);
        assert_eq!(def.valid_player, Some(ReplacementPlayerScope::Opponent));
        let execute = def.execute.as_ref().expect("mill replacement must execute");
        match &*execute.effect {
            Effect::Mill { count, .. } => assert_eq!(
                count,
                &QuantityExpr::Offset {
                    inner: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    }),
                    offset: 4
                }
            ),
            other => panic!("expected Mill execute, got {other:?}"),
        }
    }

    /// CR 614.1a: Rain of Gore — "If a spell or ability would cause its
    /// controller to gain life, that player loses that much life instead." The
    /// periphrastic "would cause its controller to gain life" subject has no
    /// "would gain life" substring; the widened entry gate must still route it
    /// to a `GainLife` replacement with `AnyPlayer` scope and a `LoseLife`
    /// execute of the replaced magnitude.
    #[test]
    fn parses_rain_of_gore_all_players_gain_life_replacement() {
        let def = parse_replacement_line(
            "If a spell or ability would cause its controller to gain life, \
             that player loses that much life instead.",
            "Rain of Gore",
        )
        .expect("Rain of Gore should parse as a replacement");
        assert_eq!(def.event, ReplacementEvent::GainLife);
        assert_eq!(
            def.valid_player,
            Some(ReplacementPlayerScope::AnyPlayer),
            "Rain of Gore watches every player's life gain"
        );
        let execute = def.execute.as_ref().expect("must have a LoseLife execute");
        assert!(
            matches!(&*execute.effect, Effect::LoseLife { .. }),
            "expected LoseLife execute, got {:?}",
            execute.effect
        );
    }

    #[test]
    fn parses_scry_replacement_with_draw_followup() {
        let text = "If you would scry a number of cards, draw that many cards instead.";
        let def = parse_replacement_line(text, "Eligeth, Crossroads Augur")
            .expect("must parse scry replacement");

        assert_eq!(def.event, ReplacementEvent::Scry);
        let execute = def.execute.as_ref().expect("scry replacement must execute");
        match &*execute.effect {
            Effect::Draw { count, target } => {
                assert_eq!(target, &TargetFilter::Controller);
                assert_eq!(
                    count,
                    &QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount
                    }
                );
            }
            other => panic!("expected Draw execute, got {other:?}"),
        }
    }

    #[test]
    fn parses_scry_replacement_with_scry_offset_followup() {
        let text = "If you would scry a number of cards, scry that many cards plus one instead.";
        let def = parse_replacement_line(text, "Kenessos, Priest of Thassa")
            .expect("must parse scry replacement");

        assert_eq!(def.event, ReplacementEvent::Scry);
        let execute = def.execute.as_ref().expect("scry replacement must execute");
        match &*execute.effect {
            Effect::Scry { count, target } => {
                assert_eq!(target, &TargetFilter::Controller);
                assert_eq!(
                    count,
                    &QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount
                        }),
                        offset: 1
                    }
                );
            }
            other => panic!("expected Scry execute, got {other:?}"),
        }
    }

    /// CR 614.1a: Worship — "If you control a creature, damage that would
    /// reduce your life total to less than 1 reduces it to 1 instead."
    /// Verifies: DamageDone event, IfControlsMatching(creature), LifeFloor(1),
    /// damage target = Controller.
    #[test]
    fn parses_worship_life_floor_replacement() {
        let def = parse_replacement_line(
            "If you control a creature, damage that would reduce your life total to less than 1 reduces it to 1 instead.",
            "Worship",
        )
        .expect("Worship should parse as a DamageDone replacement");

        assert_eq!(def.event, ReplacementEvent::DamageDone);

        match &def.condition {
            Some(ReplacementCondition::IfControlsMatching { minimum, filter }) => {
                assert_eq!(*minimum, 1, "Worship condition must have minimum = 1");
                let is_creature = match filter {
                    TargetFilter::Typed(tf) => tf.type_filters.contains(&TypeFilter::Creature),
                    TargetFilter::And { filters } => filters.iter().any(|f| {
                        matches!(f, TargetFilter::Typed(tf) if tf.type_filters.contains(&TypeFilter::Creature))
                    }),
                    _ => false,
                };
                assert!(
                    is_creature,
                    "condition filter should be Creature, got {:?}",
                    filter
                );
            }
            other => panic!("condition should be IfControlsMatching, got {:?}", other),
        }

        assert_eq!(
            def.damage_modification,
            Some(crate::types::ability::DamageModification::LifeFloor { minimum: 1 }),
            "damage modification should be LifeFloor(1)"
        );

        assert_eq!(
            def.damage_target_filter,
            Some(crate::types::ability::DamageTargetFilter::Player {
                player: crate::types::ability::DamageTargetPlayerScope::Controller
            }),
            "damage target should be Controller"
        );
    }

    /// CR 614.1a: the UNCONDITIONAL life-floor (Ali from Cairo, Fortune Thief,
    /// Sustaining Spirit) parses to the same `DamageDone` + `LifeFloor` +
    /// Controller-target replacement as Worship, but with NO condition — the
    /// previously-dropped `Effect:replacement_structure` gap.
    #[test]
    fn parses_unconditional_life_floor_replacement() {
        let def = parse_replacement_line(
            "Damage that would reduce your life total to 0 reduces it to 1 instead.",
            "Ali from Cairo",
        )
        .expect("Ali from Cairo printed 'to 0' wording should parse");
        assert_eq!(def.event, ReplacementEvent::DamageDone);
        assert_eq!(def.condition, None);
        assert_eq!(
            def.damage_modification,
            Some(crate::types::ability::DamageModification::LifeFloor { minimum: 1 })
        );

        for card in ["Ali from Cairo", "Fortune Thief", "Sustaining Spirit"] {
            let def = parse_replacement_line(
                "Damage that would reduce your life total to less than 1 reduces it to 1 instead.",
                card,
            )
            .unwrap_or_else(|| panic!("{card}: unconditional life-floor should parse"));

            assert_eq!(def.event, ReplacementEvent::DamageDone, "{card}");
            assert_eq!(
                def.condition, None,
                "{card}: unconditional form must carry NO condition (cf. Worship's IfControlsMatching)"
            );
            assert_eq!(
                def.damage_modification,
                Some(crate::types::ability::DamageModification::LifeFloor { minimum: 1 }),
                "{card}: damage modification should be LifeFloor(1)"
            );
            assert_eq!(
                def.damage_target_filter,
                Some(crate::types::ability::DamageTargetFilter::Player {
                    player: crate::types::ability::DamageTargetPlayerScope::Controller
                }),
                "{card}: damage target should be Controller"
            );
        }
    }

    /// Guard: the conditional Worship form still routes to the conditional arm
    /// (keeps its `IfControlsMatching` condition) — the unconditional arm must
    /// not swallow it.
    #[test]
    fn conditional_worship_life_floor_still_carries_condition() {
        let def = parse_replacement_line(
            "If you control a creature, damage that would reduce your life total to less than 1 reduces it to 1 instead.",
            "Worship",
        )
        .expect("Worship should still parse");
        assert!(
            matches!(
                def.condition,
                Some(ReplacementCondition::IfControlsMatching { .. })
            ),
            "Worship must keep its IfControlsMatching condition, got {:?}",
            def.condition
        );
    }

    // -----------------------------------------------------------------------
    // Taii Wakeen, Perfect Shot — "it deals that much damage plus X/N instead"
    // damage-modification scanning. The "plus X" form emits a `Plus { value: 0 }`
    // placeholder frozen at activation (CR 107.3a); a literal "plus N" carries N.
    // -----------------------------------------------------------------------

    /// CR 614.1a + CR 107.3a: "plus X" yields the `Plus { value: 0 }` placeholder
    /// (the announced X is frozen into it at activation time, not parse time).
    #[test]
    fn that_much_damage_plus_x_is_zero_placeholder() {
        assert_eq!(
            scan_damage_modification("it deals that much damage plus x instead"),
            Some(DamageModification::Plus { value: 0 }),
            "'plus X' must parse to the Plus(0) placeholder frozen at activation"
        );
    }

    /// A literal "plus 2" carries the constant directly.
    #[test]
    fn that_much_damage_plus_literal_carries_value() {
        assert_eq!(
            scan_damage_modification("it deals that much damage plus 2 instead"),
            Some(DamageModification::Plus { value: 2 })
        );
    }

    /// The "minus N" sibling stays intact through the nom conversion.
    #[test]
    fn that_much_damage_minus_literal_carries_value() {
        assert_eq!(
            scan_damage_modification("it deals that much damage minus 1 instead"),
            Some(DamageModification::Minus { value: 1 })
        );
    }

    #[test]
    fn parses_enchanted_land_destroy_sacrifice_indestructible() {
        let def = parse_replacement_line(
            "If enchanted land would be destroyed, instead sacrifice ~ and that land gains indestructible until end of turn.",
            "Harmonious Emergence",
        )
        .expect("enchanted land destroy");

        assert_eq!(def.event, ReplacementEvent::Destroy);
        assert_eq!(def.valid_card, Some(TargetFilter::AttachedTo));

        let execute = def.execute.as_ref().expect("replacement execute");
        assert!(matches!(
            &*execute.effect,
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                ..
            }
        ));

        let grant = execute.sub_ability.as_ref().expect("indestructible grant");
        match &*grant.effect {
            Effect::GenericEffect {
                static_abilities,
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            } => {
                assert!(static_abilities.iter().any(|static_ability| {
                    static_ability.affected == Some(TargetFilter::ParentTarget)
                        && static_ability.modifications.contains(
                            &ContinuousModification::AddKeyword {
                                keyword: Keyword::Indestructible,
                            },
                        )
                }));
            }
            other => panic!("expected indestructible grant to enchanted land, got {other:?}"),
        }
    }

    #[test]
    fn parses_generic_additional_food_token_replacement() {
        let def = parse_replacement_line(
            "If you would create one or more tokens, instead create those tokens plus an additional Food token.",
            "Tippy-Toe, Terrific Partner",
        )
        .expect("generic additional token");
        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert_eq!(def.token_owner_scope, Some(ControllerRef::You));
        assert!(
            def.condition.is_none(),
            "generic token wording must not inherit Xorn's subtype gate"
        );
        let spec = def
            .additional_token_spec
            .as_ref()
            .expect("additional Food token spec");
        assert_eq!(spec.characteristics.display_name, "Food");
        assert_eq!(spec.characteristics.core_types, vec![CoreType::Artifact]);
        assert_eq!(spec.characteristics.subtypes, vec!["Food".to_string()]);
        assert_eq!(spec.characteristics.power, None);
        assert_eq!(spec.characteristics.toughness, None);
    }

    #[test]
    fn parses_basic_land_triple_mana_replacement() {
        let def = parse_replacement_line(
            "If you tap a basic land for mana, it produces three times as much of that mana instead.",
            "Virtue of Strength",
        )
        .expect("basic land 3x mana");
        assert_eq!(
            def.mana_modification,
            Some(ManaModification::Multiply { factor: 3 })
        );
        let Some(TargetFilter::Typed(filter)) = def.valid_card else {
            panic!("basic land replacement should carry a typed source filter");
        };
        assert_eq!(filter.controller, Some(ControllerRef::You));
        assert!(filter.type_filters.contains(&TypeFilter::Land));
        assert!(filter.properties.contains(&FilterProp::HasSupertype {
            value: Supertype::Basic,
        }));
    }

    #[test]
    fn parses_energy_get_additional_replacement() {
        let def = parse_replacement_line(
            "If you would get one or more {E}, you get an additional {E} instead.",
            "Izzet Generatorium",
        )
        .expect("energy get replacement");
        assert_eq!(def.event, ReplacementEvent::AddCounter);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Plus { value: 1 })
        );
        assert_eq!(def.valid_player, Some(ReplacementPlayerScope::You));
    }

    #[test]
    fn parses_halving_season_opponent_counter_replacement() {
        let def = parse_replacement_line(
            "If an opponent would put one or more counters on a permanent or player, they put half that many of those counters on that permanent or player instead, rounded down.",
            "Halving Season",
        )
        .expect("halving season");
        assert_eq!(def.quantity_modification, Some(QuantityModification::Half));
        assert_eq!(def.valid_player, Some(ReplacementPlayerScope::Opponent));
    }

    #[test]
    fn parses_explore_replacement_scry_prelude() {
        let def = parse_replacement_line(
            "If a creature you control would explore, instead you scry 1, then that creature explores.",
            "Twists and Turns",
        )
        .expect("Twists and Turns explore replacement must parse");
        assert_eq!(def.event, ReplacementEvent::Explore);
        assert!(matches!(
            def.valid_card,
            Some(TargetFilter::Typed(tf))
                if tf.type_filters == vec![TypeFilter::Creature]
                    && tf.controller == Some(ControllerRef::You)
        ));
        assert!(def.execute.is_some());
    }

    #[test]
    fn parses_explore_replacement_double_explore() {
        let def = parse_replacement_line(
            "If a creature you control would explore, instead it explores, then it explores again.",
            "Topography Tracker",
        )
        .expect("Topography Tracker explore replacement must parse");
        assert_eq!(def.event, ReplacementEvent::Explore);
        assert!(def.execute.is_some());
    }

    #[test]
    fn parses_halving_season_opponent_token_replacement() {
        let def = parse_replacement_line(
            "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.",
            "Halving Season",
        )
        .expect("Halving Season token halving must parse");
        assert_eq!(def.event, ReplacementEvent::CreateToken);
        assert_eq!(def.quantity_modification, Some(QuantityModification::Half));
        assert_eq!(def.token_owner_scope, Some(ControllerRef::Opponent));
    }
}

/// Snapshot tests locking current replacement parser output before/after the IR split.
/// These verify behavioral parity: identical snapshots before and after the
/// `parse_replacement_line_ir` / `lower_replacement_ir` refactor.
#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn replacement_enters_tapped() {
        let def = parse_replacement_line("~ enters the battlefield tapped.", "Test Card").unwrap();
        insta::assert_json_snapshot!(def);
    }

    #[test]
    fn replacement_prevent_all_combat_damage() {
        let def = parse_replacement_line(
            "Prevent all combat damage that would be dealt to you.",
            "Test Card",
        )
        .unwrap();
        insta::assert_json_snapshot!(def);
    }

    #[test]
    fn replacement_would_die_exile() {
        let def = parse_replacement_line("If ~ would die, exile it instead.", "Test Card").unwrap();
        insta::assert_json_snapshot!(def);
    }

    #[test]
    fn replacement_enters_with_counters() {
        let def = parse_replacement_line(
            "~ enters the battlefield with two +1/+1 counters on it.",
            "Test Card",
        )
        .unwrap();
        insta::assert_json_snapshot!(def);
    }

    /// CR 104.2b + CR 104.3c: The "draw from empty library → win" class
    /// (Laboratory Maniac, Jace, Wielder of Mysteries) must gate its WinTheGame
    /// post-effect on the "while your library has no cards in it" antecedent.
    /// Without the gate the replacement fires on every draw — winning spuriously
    /// and leaking an un-drained post-replacement continuation across turns.
    #[test]
    fn draw_replacement_win_gated_on_empty_library() {
        let def = parse_replacement_line(
            "If you would draw a card while your library has no cards in it, you win the game instead.",
            "Laboratory Maniac",
        )
        .expect("must parse the empty-library win replacement");

        assert_eq!(def.event, ReplacementEvent::Draw);
        assert!(
            matches!(
                def.execute.as_deref().map(|a| &*a.effect),
                Some(crate::types::ability::Effect::WinTheGame { .. })
            ),
            "execute must be WinTheGame, got {:?}",
            def.execute
        );
        match def.condition {
            Some(ReplacementCondition::OnlyIfQuantity {
                lhs:
                    QuantityExpr::Ref {
                        qty:
                            QuantityRef::ZoneCardCount {
                                zone: crate::types::ability::ZoneRef::Library,
                                ref card_types,
                                scope: crate::types::ability::CountScope::Controller,
                                filter: None,
                            },
                    },
                comparator: Comparator::EQ,
                rhs: QuantityExpr::Fixed { value: 0 },
                ..
            }) => assert!(card_types.is_empty(), "library count must be unfiltered"),
            other => panic!(
                "expected OnlyIfQuantity(library == 0) gate, got {other:?}; \
                 the empty-library antecedent was dropped"
            ),
        }
    }

    /// Discipline guard: a draw replacement whose "while [condition]"
    /// antecedent is structurally present but unparseable must fail closed
    /// (produce no replacement) rather than emit an unconditional one. A
    /// silently-ungated win-on-draw is the regression `WhileAntecedent::Unparsed`
    /// exists to prevent. "while there is a full moon" has no typed condition.
    #[test]
    fn draw_replacement_with_unparseable_guard_fails_closed() {
        let def = parse_replacement_line(
            "If you would draw a card while there is a full moon, you win the game instead.",
            "Made Up Card",
        );
        assert!(
            def.is_none(),
            "unparseable while-guard must fail closed, not emit an unconditional \
             replacement; got {def:?}"
        );
    }

    // CR 614.1a + CR 614.9: building-block coverage for the one-shot
    // damage-replacement parser across the source × scope × recipient axes.
    #[test]
    fn oneshot_amount_double_from_chosen_source() {
        // Desperate Gambit win-branch.
        let effect = parse_oneshot_damage_replacement(
            "the next time that source would deal damage this turn, it deals double that damage instead",
        )
        .expect("must parse amount one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: Some(DamageModification::Double),
                redirect_to: None,
                source_filter: Some(TargetFilter::ChosenDamageSource),
                combat_scope: None,
                ..
            } => {}
            other => panic!("expected Double amount one-shot, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_redirect_to_target_creature_combat_from_self() {
        // Soltari Guerrillas.
        let effect = parse_oneshot_damage_replacement(
            "the next time ~ would deal combat damage to an opponent this turn, it deals that damage to target creature instead",
        )
        .expect("must parse redirection one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: None,
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: Some(CombatDamageScope::CombatOnly),
                target_filter: Some(DamageTargetFilter::Player { .. }),
                // CR 115.1: the "to target creature instead" redirect recipient
                // must surface a creature target filter so the targeting layer
                // offers the slot (Defect 1).
                redirect_object_filter: Some(_),
                recipient_object_filter: None,
            } => {}
            other => panic!("expected redirect-to-target-creature, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_en_kor_next_n_damage_to_self_redirect() {
        // The en-Kor cycle (Nomads / Lancers / Outrider / Shaman / Spirit /
        // Warrior en-Kor): passive "the next N damage that would be dealt to ~"
        // — the recipient is the source itself — redirected to a chosen creature
        // you control.
        let effect = parse_oneshot_damage_replacement(
            "the next 1 damage that would be dealt to ~ this turn is dealt to target creature you control instead",
        )
        .expect("must parse the en-Kor one-shot redirection");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: Some(PreventionAmount::Next(1)),
                // CR 614.9: the recipient is the source itself (`~`), encoded as
                // SelfRef so the resolver hosts the shield on the source.
                recipient_object_filter: Some(TargetFilter::SelfRef),
                // CR 115.1: "target creature you control" surfaces a redirect slot.
                redirect_object_filter: Some(TargetFilter::Typed(_)),
                source_filter: None,
                combat_scope: None,
                target_filter: None,
            } => {}
            other => panic!("expected en-Kor redirect-to-target, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_redirect_to_source_passive_phrasing() {
        // Beacon of Destiny — passive "that damage is dealt to ~ instead".
        let effect = parse_oneshot_damage_replacement(
            "the next time a source of your choice would deal damage to you this turn, that damage is dealt to ~ instead",
        )
        .expect("must parse passive redirection one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::SourceObject),
                redirect_amount: None,
                source_filter: Some(TargetFilter::ChosenDamageSource),
                ..
            } => {}
            other => panic!("expected redirect-to-source, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_redirect_to_controller_from_chosen_source() {
        // Jade Monolith.
        let effect = parse_oneshot_damage_replacement(
            "the next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead",
        )
        .expect("must parse redirect-to-you one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: None,
                source_filter: Some(TargetFilter::ChosenDamageSource),
                // CR 614.9: "would deal damage to target creature" — the
                // protected creature is a chosen original-recipient target, not
                // a broad scope (Defect 3). `target_filter` must stay None.
                recipient_object_filter: Some(_),
                target_filter: None,
                redirect_object_filter: None,
                ..
            } => {}
            other => panic!("expected redirect-to-controller, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_redirect_to_controller_combat_from_self() {
        // Goblin Psychopath.
        let effect = parse_oneshot_damage_replacement(
            "the next time it would deal combat damage this turn, it deals that damage to you instead",
        )
        .expect("must parse Goblin Psychopath one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: None,
                source_filter: Some(TargetFilter::SelfRef),
                combat_scope: Some(CombatDamageScope::CombatOnly),
                ..
            } => {}
            other => panic!("expected Goblin Psychopath redirect, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_prevention_sibling() {
        // Desperate Gambit lose-branch — routes to PreventDamage.
        let effect = parse_oneshot_damage_replacement(
            "the next time it would deal damage this turn, prevent that damage",
        )
        .expect("must parse prevention sibling");
        assert!(
            matches!(effect, Effect::PreventDamage { .. }),
            "expected PreventDamage, got {effect:?}"
        );
    }

    #[test]
    fn oneshot_rejects_unrelated_next_time_text() {
        // "the next time you draw" must not be hijacked by the damage parser.
        assert!(parse_oneshot_damage_replacement(
            "the next time you would draw a card this turn, draw two cards instead"
        )
        .is_none());
    }

    #[test]
    fn oneshot_next_n_damage_to_target_redirected_to_source() {
        // Daughter of Autumn — "{W}: The next 1 damage that would be dealt to
        // target white creature this turn is dealt to ~ instead." The ORIGINAL
        // recipient is a chosen target; the redirect destination is the source
        // itself (`~` → SourceObject), which needs no redirect slot.
        let effect = parse_oneshot_damage_replacement(
            "the next 1 damage that would be dealt to target white creature this turn is dealt to ~ instead",
        )
        .expect("must parse redirect-target-to-source one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::SourceObject),
                redirect_amount: Some(PreventionAmount::Next(1)),
                // CR 115.1: the protected creature is a chosen original-recipient
                // target (surfaces the recipient slot), not a broad scope.
                recipient_object_filter: Some(TargetFilter::Typed(_)),
                redirect_object_filter: None,
                source_filter: None,
                combat_scope: None,
                target_filter: None,
            } => {}
            other => panic!("expected redirect-target->source, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_next_n_damage_to_target_redirected_to_controller() {
        // Vassal's Duty — "{1}: The next 1 damage that would be dealt to target
        // legendary creature you control this turn is dealt to you instead." The
        // redirect destination is the controller (`you` → Controller).
        let effect = parse_oneshot_damage_replacement(
            "the next 1 damage that would be dealt to target legendary creature you control this turn is dealt to you instead",
        )
        .expect("must parse redirect-target-to-controller one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::Controller),
                redirect_amount: Some(PreventionAmount::Next(1)),
                recipient_object_filter: Some(TargetFilter::Typed(_)),
                redirect_object_filter: None,
                source_filter: None,
                combat_scope: None,
                target_filter: None,
            } => {}
            other => panic!("expected redirect-target->controller, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_target_redirect_does_not_steal_en_kor_self_recipient() {
        // The en-Kor self-recipient form ("...dealt to ~ this turn is dealt to
        // target creature...") must still parse via its own sibling (recipient =
        // SelfRef), NOT the new target-recipient arm.
        let effect = parse_oneshot_damage_replacement(
            "the next 1 damage that would be dealt to ~ this turn is dealt to target creature you control instead",
        )
        .expect("en-Kor self redirect must still parse");
        assert!(
            matches!(
                effect,
                Effect::CreateDamageReplacement {
                    recipient_object_filter: Some(TargetFilter::SelfRef),
                    redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                    ..
                }
            ),
            "en-Kor self-recipient form regressed: {effect:?}"
        );
    }

    #[test]
    fn oneshot_next_n_damage_to_target_redirected_to_chosen_target() {
        // Razia, Boros Archangel — "{T}: The next 3 damage that would be dealt to
        // target creature you control this turn is dealt to another target
        // creature instead." Both the original recipient AND the redirect
        // destination are chosen object targets (two slots).
        let effect = parse_oneshot_damage_replacement(
            "the next 3 damage that would be dealt to target creature you control this turn is dealt to another target creature instead",
        )
        .expect("must parse redirect-target-to-chosen-target one-shot");
        match effect {
            Effect::CreateDamageReplacement {
                modification: None,
                redirect_to: Some(DamageRedirectTarget::ChosenObjectTarget),
                redirect_amount: Some(PreventionAmount::Next(3)),
                recipient_object_filter: Some(TargetFilter::Typed(_)),
                // CR 115.1: the "another target creature" redirect surfaces its
                // own chosen-object slot.
                redirect_object_filter: Some(TargetFilter::Typed(_)),
                source_filter: None,
                combat_scope: None,
                target_filter: None,
            } => {}
            other => panic!("expected recipient+redirect both chosen targets, got {other:?}"),
        }
    }

    #[test]
    fn oneshot_redirect_to_any_target_fails_closed() {
        // CR 115.1: an "any target" redirect can resolve to a player, but the
        // CreateDamageReplacement resolver stores only OBJECT redirect targets, so
        // a player choice would silently drop the redirect. Both the `~`-recipient
        // (Zhalfirin Crusader) and chosen-target-recipient forms must therefore
        // fail closed on "any target" rather than mis-model it.
        assert!(
            parse_oneshot_damage_replacement(
                "the next 1 damage that would be dealt to ~ this turn is dealt to any target instead",
            )
            .is_none(),
            "en-Kor 'any target' redirect must fail closed (object-only resolver)"
        );
        assert!(
            parse_oneshot_damage_replacement(
                "the next 1 damage that would be dealt to target creature you control this turn is dealt to any target instead",
            )
            .is_none(),
            "chosen-recipient 'any target' redirect must fail closed (object-only resolver)"
        );
    }

    /// CR 614.1a + CR 614.6 + CR 121.6 + CR 701.20a: Abundance — the
    /// "you may instead" antecedent must lift the draw replacement to
    /// `ReplacementMode::Optional { decline: None }` (so the player is
    /// prompted to accept/decline and the original draw resolves on decline),
    /// and the effect chain must compose the existing `Effect::Choose`
    /// (`ChoiceType::Labeled["Land","Nonland"]`) and `Effect::RevealUntil`
    /// (filter: `FilterProp::IsChosenLandOrNonlandKind`, kept→Hand, rest→
    /// Library) building blocks with no `Unimplemented` node anywhere.
    #[test]
    fn abundance_parses_as_optional_choose_then_reveal_until_chosen_kind() {
        use crate::types::ability::{ChoiceType, FilterProp};
        let def = parse_replacement_line(
            "If you would draw a card, you may instead choose land or nonland and reveal cards \
             from the top of your library until you reveal a card of the chosen kind. Put that \
             card into your hand and put all other cards revealed this way on the bottom of \
             your library in any order.",
            "Abundance",
        )
        .expect("Abundance must parse as a Draw replacement");

        assert_eq!(def.event, ReplacementEvent::Draw);
        assert!(
            matches!(def.mode, ReplacementMode::Optional { decline: None }),
            "the \"you may instead\" antecedent must lift to Optional {{ decline: None }} \
             (CR 614.6: only the accept branch replaces the event); got {:?}",
            def.mode
        );

        let execute = def.execute.as_ref().expect("execute chain must be present");
        // Head clause: Choose(Labeled["Land","Nonland"]).
        let Effect::Choose {
            choice_type: ChoiceType::Labeled { options },
            ..
        } = &*execute.effect
        else {
            panic!(
                "expected head Effect::Choose(Labeled), got {:?}",
                execute.effect
            );
        };
        assert_eq!(
            options,
            &vec!["Land".to_string(), "Nonland".to_string()],
            "labeled choice options must be exactly [\"Land\",\"Nonland\"]"
        );

        // RevealUntil { filter: IsChosenLandOrNonlandKind, kept=Hand, rest=Library }
        // chained via the bare-and split (either as ContinuationStep or
        // SequentialSibling — both run sequentially under the chain resolver).
        let reveal = execute
            .sub_ability
            .as_ref()
            .expect("RevealUntil must follow Choose as a sequential sibling");
        let Effect::RevealUntil {
            filter: TargetFilter::Typed(tf),
            kept_destination,
            rest_destination,
            ..
        } = &*reveal.effect
        else {
            panic!(
                "expected sibling Effect::RevealUntil, got {:?}",
                reveal.effect
            );
        };
        assert!(
            tf.properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsChosenLandOrNonlandKind)),
            "RevealUntil filter must carry FilterProp::IsChosenLandOrNonlandKind so the \
             runtime resolves the kept card against the controller's earlier labeled choice"
        );
        assert_eq!(*kept_destination, Zone::Hand);
        assert_eq!(*rest_destination, Zone::Library);

        // No Unimplemented anywhere in the tree, and no stray PutAtLibraryPosition
        // sibling (the prior AST had a fallback chain that ended in
        // PutAtLibraryPosition — the chain must collapse to Choose → RevealUntil).
        let mut node: Option<&AbilityDefinition> = Some(execute.as_ref());
        while let Some(ability) = node {
            assert!(
                !matches!(*ability.effect, Effect::Unimplemented { .. }),
                "no Unimplemented node may remain in Abundance's parse tree; got {:?}",
                ability.effect
            );
            assert!(
                !matches!(*ability.effect, Effect::PutAtLibraryPosition { .. }),
                "no stray PutAtLibraryPosition sibling — the continuation must be absorbed \
                 by RevealUntilKept; got {:?}",
                ability.effect
            );
            node = ability.sub_ability.as_deref();
        }
    }

    /// CR 614.1a + CR 614.6: A "you may instead" lead-in on a draw
    /// replacement must lift to Optional mode but otherwise leave the
    /// effect-chain parse identical to the mandatory-instead form. The
    /// stripper must consume only the modal prefix.
    #[test]
    fn strip_optional_instead_lead_in_consumes_only_the_modal() {
        let (had_modal, rest) = super::strip_optional_instead_lead_in(
            "you may instead choose land or nonland and reveal cards",
        );
        assert!(had_modal, "lead-in modal must be detected");
        assert_eq!(rest, "choose land or nonland and reveal cards");

        let (no_modal, unchanged) = super::strip_optional_instead_lead_in("draw two cards");
        assert!(!no_modal, "mandatory effect text must not be misclassified");
        assert_eq!(unchanged, "draw two cards");
    }

    /// CR 707.10 + CR 614.1a: Twinning Staff's "If you would copy a spell one or
    /// more times, instead copy it that many times plus an additional time"
    /// parses to a `CopySpell` replacement carrying `Plus { value: 1 }`.
    #[test]
    fn copy_count_replacement_parses_twinning_staff() {
        use crate::types::ability::QuantityModification;
        use crate::types::replacements::ReplacementEvent;

        let def = super::parse_replacement_line(
            "If you would copy a spell one or more times, instead copy it that many times \
             plus an additional time. You may choose new targets for the additional copy.",
            "Twinning Staff",
        )
        .expect("Twinning Staff replacement must parse");

        assert_eq!(def.event, ReplacementEvent::CopySpell);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Plus { value: 1 })
        );
    }

    /// The "additional time(s)" tail is composed from modular combinators, so a
    /// numbered, pluralized variant ("plus 2 additional times") parses to the
    /// corresponding `Plus { value }` — sibling coverage beyond the single
    /// Twinning Staff wording.
    #[test]
    fn copy_count_replacement_parses_plural_numbered_variant() {
        use crate::types::ability::QuantityModification;
        use crate::types::replacements::ReplacementEvent;

        let def = super::parse_replacement_line(
            "If you would copy a spell one or more times, instead copy it that many times \
             plus 2 additional times.",
            "Hypothetical Double Staff",
        )
        .expect("plural numbered copy-count replacement must parse");

        assert_eq!(def.event, ReplacementEvent::CopySpell);
        assert_eq!(
            def.quantity_modification,
            Some(QuantityModification::Plus { value: 2 })
        );
    }

    #[test]
    fn copy_count_replacement_requires_full_copy_count_shape() {
        let text = "If you would copy a spell, instead copy target spell plus an additional time.";
        let lower = text.to_lowercase();

        assert!(
            super::parse_copy_count_replacement(&lower, text).is_none(),
            "copy-count replacement must not be gated by loose substring matching"
        );
    }

    /// CR 110.2a: "<this permanent> enters under the control of an opponent of
    /// your choice." parses to a self-ETB controller-override replacement —
    /// `Moved` / `valid_card = SelfRef` / `destination_zone = Battlefield` /
    /// `enters_under = Opponent`. Build-the-class across the four real corpus
    /// phrasings (self-name "~", "this artifact", "this enchantment").
    #[test]
    fn self_enters_under_opponent_parses_controller_override_replacement() {
        let cases = [
            // Xantcha, Sleeper Agent / Abby, Merciless Soldier (legendary short name → "~").
            (
                "Xantcha enters under the control of an opponent of your choice.",
                "Xantcha, Sleeper Agent",
            ),
            (
                "Abby enters under the control of an opponent of your choice.",
                "Abby, Merciless Soldier",
            ),
            // Pendant of Prosperity (card name absent; demonstrative subject).
            (
                "This artifact enters under the control of an opponent of your choice.",
                "Pendant of Prosperity",
            ),
            // Captive Audience.
            (
                "This enchantment enters under the control of an opponent of your choice.",
                "Captive Audience",
            ),
        ];

        for (text, card_name) in cases {
            let def = parse_replacement_line(text, card_name)
                .unwrap_or_else(|| panic!("{card_name}: should parse as a replacement"));
            assert_eq!(
                def.event,
                ReplacementEvent::Moved,
                "{card_name}: self-ETB replacement is a Moved event"
            );
            assert_eq!(
                def.valid_card,
                Some(TargetFilter::SelfRef),
                "{card_name}: applies only to the entering permanent itself"
            );
            assert_eq!(
                def.destination_zone,
                Some(Zone::Battlefield),
                "{card_name}: battlefield-entry-scoped (CR 614.1d)"
            );
            assert_eq!(
                def.enters_under,
                Some(ControllerRef::Opponent),
                "{card_name}: enters under an opponent's control (CR 110.2a)"
            );
        }
    }

    /// Regression for #3213: the controller-override line must route THROUGH the
    /// classifier (`REPLACEMENT_CONTAINS_PATTERNS`) to `parse_replacement_line`.
    /// The test above calls `parse_replacement_line` directly (bypassing the
    /// classifier), which is exactly why it passed while the real cards still
    /// gapped. This drives the full `parse_oracle_text` path: reverting the
    /// classifier entry makes the line fall through to the effect parser as
    /// `Unimplemented`, producing zero replacements — caught here.
    #[test]
    fn full_card_enters_under_opponent_routes_to_replacement() {
        let result = crate::parser::oracle::parse_oracle_text(
            "Xantcha enters under the control of an opponent of your choice.",
            "Xantcha, Sleeper Agent",
            &[],
            &["Creature".to_string()],
            &["Phyrexian".to_string(), "Minion".to_string()],
        );
        assert!(
            result.replacements.iter().any(|r| {
                r.event == ReplacementEvent::Moved
                    && r.enters_under == Some(ControllerRef::Opponent)
                    && r.valid_card == Some(TargetFilter::SelfRef)
            }),
            "the controller-override line must route to a replacement (not Unimplemented); \
             replacements = {:?}",
            result.replacements
        );
    }

    /// The control clause is NOT claimed when the subject is an external filter
    /// rather than the permanent itself — the self-subject gate must reject it.
    #[test]
    fn external_enters_under_opponent_is_not_a_self_replacement() {
        assert!(
            super::parse_self_enters_under_opponent(
                "creatures you control enter under the control of an opponent of your choice",
                "Whatever",
            )
            .is_none(),
            "external-subject entry must not match the self controller-override arm"
        );
    }
}
