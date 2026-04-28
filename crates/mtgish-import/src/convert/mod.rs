//! Top-level dispatch: `mtgish::OracleCard` → engine card form.
//!
//! Phase 2 skeleton. Every `Rule` currently fails with
//! `ConversionGap::UnknownVariant`, surfacing the variant tag (via serde
//! introspection on the `_Rule` discriminator) so the report ranks the
//! whole work queue without us writing a 475-arm `match` up front. Per-arm
//! conversion lands in later phases (4–11).
//!
//! Strict-failure discipline: the first failed rule short-circuits the
//! entire card. The card produces no output entry; the gap is recorded in
//! the report.

pub mod action;
pub mod cast_effect;
pub mod companion;
pub mod condition;
pub mod cost;
pub mod deferred;
pub mod filter;
pub mod keyword;
pub mod mana;
pub mod player_effect;
pub mod quantity;
pub mod replacement;
pub mod result;
pub mod saga;
pub mod static_effect;
pub mod token;
pub mod trigger;

use engine::types::ability::{
    AbilityDefinition, AbilityKind, ActivationRestriction, Effect, ReplacementDefinition,
    StaticDefinition,
};
use engine::types::{Keyword, TriggerDefinition};

use crate::provenance::{CardProvenance, ProvenanceEntry, ProvenanceSlot};
use crate::report::Ctx;
use crate::schema::types::{Card, DoorInfo, FlipInfo, OracleCard, Rule};
use result::{ConvResult, ConversionGap};

/// Per-face conversion accumulator. Mirrors the relevant subset of
/// `engine::CardFace`. The casting-metadata fields (`additional_cost`,
/// `casting_options`, `casting_restrictions`, `strive_cost`) carry data
/// produced by `Rule::CastEffect` arms and feed `CardFace` slots of the
/// same name when the downstream consumer is wired up.
#[derive(Default, Debug, serde::Serialize)]
pub struct EngineFaceStub {
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<AbilityDefinition>,
    pub triggers: Vec<TriggerDefinition>,
    pub statics: Vec<StaticDefinition>,
    pub replacements: Vec<ReplacementDefinition>,
    /// CR 601.2f / CR 608.2c: "As an additional cost to cast this spell, ..."
    pub additional_cost: Option<engine::types::ability::AdditionalCost>,
    /// CR 601.2b + CR 609.4b: "You may cast this spell as though it had flash",
    /// "You may pay [alt] rather than this spell's mana cost", etc.
    pub casting_options: Vec<engine::types::ability::SpellCastingOption>,
    /// CR 601.2c: "Cast this spell only [during X / if Y]."
    pub casting_restrictions: Vec<engine::types::ability::CastingRestriction>,
    /// CR 207.2c + CR 601.2f: Strive — "this spell costs {X} more to cast for
    /// each target beyond the first."
    pub strive_cost: Option<engine::types::ManaCost>,
}

/// Faces extracted from a top-level `OracleCard` for uniform iteration.
/// Each entry is `(face_label, rules_slice)` where `face_label` is the
/// provenance breadcrumb component used in `(card_name, face_idx, ...)`.
struct Face<'a> {
    label: &'static str,
    rules: &'a [Rule],
}

/// Dispatch entry point. Walks every face's rules and converts each in
/// order. Per-rule failures are recorded to `ctx.unsupported` (so the
/// report's frequency table reflects every blocker, not just the first
/// per card) and then the first error is propagated so the card is
/// dropped from output. This is intentional: full reporting matters for
/// the work-queue distribution; strict-failure semantics still hold for
/// the output (any rule failure ⇒ no card).
pub fn convert_card(card: &OracleCard, ctx: &mut Ctx) -> ConvResult<Vec<EngineFaceStub>> {
    convert_card_with_provenance(card, ctx, None)
}

/// Same as [`convert_card`], with an optional side-channel that captures
/// a provenance breadcrumb for every successfully-produced item. The
/// provenance recording is a strict no-op for the conversion path —
/// failed rules are still routed through `ctx.unsupported` and propagate
/// as `Err`, never appearing in the provenance side-channel.
///
/// Slot indices are derived by snapshotting the per-stub `Vec` lengths
/// before and after each `convert_rule` call, then enumerating the
/// newly-added entries in each slot. This keeps `convert_rule` itself
/// untouched (no new arguments, no per-arm bookkeeping) while still
/// producing one breadcrumb per produced item.
pub fn convert_card_with_provenance(
    card: &OracleCard,
    ctx: &mut Ctx,
    mut provenance: Option<&mut CardProvenance>,
) -> ConvResult<Vec<EngineFaceStub>> {
    let mut faces_out = Vec::new();
    let mut first_err: Option<ConversionGap> = None;
    for (face_idx, face) in collect_faces(card).into_iter().enumerate() {
        let mut stub = EngineFaceStub::default();
        for (idx, rule) in face.rules.iter().enumerate() {
            let snapshot = StubLengths::of(&stub);
            match convert_rule(rule, face.label, idx, &mut stub, ctx) {
                Err(e) => {
                    let path = enrich_gap_path(&e, face.label, idx, rule);
                    ctx.unsupported(&path);
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Ok(()) => {
                    if let Some(prov) = provenance.as_deref_mut() {
                        let tag = variant_tag(rule).unwrap_or_else(|| "<untagged>".to_string());
                        let path = format!("{}/Rules[{}]/Rule::{}", face.label, idx, tag);
                        snapshot.record_diff(&stub, face_idx, &path, prov);
                    }
                }
            }
        }
        faces_out.push(stub);
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(faces_out),
    }
}

/// Snapshot of `EngineFaceStub` slot lengths used to derive provenance
/// `slot_idx` values for newly-added items.
#[derive(Clone, Copy)]
struct StubLengths {
    abilities: usize,
    triggers: usize,
    statics: usize,
    replacements: usize,
    keywords: usize,
}

impl StubLengths {
    fn of(stub: &EngineFaceStub) -> Self {
        Self {
            abilities: stub.abilities.len(),
            triggers: stub.triggers.len(),
            statics: stub.statics.len(),
            replacements: stub.replacements.len(),
            keywords: stub.keywords.len(),
        }
    }

    fn record_diff(
        &self,
        after: &EngineFaceStub,
        face_idx: usize,
        path: &str,
        prov: &mut CardProvenance,
    ) {
        let push =
            |prov: &mut CardProvenance, slot: ProvenanceSlot, before: usize, after: usize| {
                for slot_idx in before..after {
                    prov.record(ProvenanceEntry {
                        face_idx,
                        slot,
                        slot_idx,
                        path: path.to_string(),
                    });
                }
            };
        push(
            prov,
            ProvenanceSlot::Ability,
            self.abilities,
            after.abilities.len(),
        );
        push(
            prov,
            ProvenanceSlot::Trigger,
            self.triggers,
            after.triggers.len(),
        );
        push(
            prov,
            ProvenanceSlot::Static,
            self.statics,
            after.statics.len(),
        );
        push(
            prov,
            ProvenanceSlot::Replacement,
            self.replacements,
            after.replacements.len(),
        );
        push(
            prov,
            ProvenanceSlot::Keyword,
            self.keywords,
            after.keywords.len(),
        );
    }
}

/// Build the report key for a sub-converter `Err`. Always anchored on
/// the dispatched-rule context (`face/Rules[idx]/Rule::<tag>`) so
/// distinct dispatch sites can be told apart in the frequency table,
/// even when the inner gap's `path` is empty (which is most sub-
/// converters today). When the gap carries its own enriched detail
/// (`MalformedIdiom`/`EnginePrerequisite`/non-empty path), append it
/// after a `" :: "` separator.
fn enrich_gap_path(gap: &ConversionGap, face: &str, idx: usize, rule: &Rule) -> String {
    let tag = variant_tag(rule).unwrap_or_else(|| "<untagged>".to_string());
    let dispatch_key = format!("{face}/Rules[{idx}]/Rule::{tag}");
    let inner = gap.report_path();
    if inner.is_empty() {
        dispatch_key
    } else {
        format!("{dispatch_key} :: {inner}")
    }
}

/// Per-rule dispatcher. Tries each phase's converter in turn; the first
/// successful match consumes the rule. If every converter declines,
/// returns `Err(ConversionGap::UnknownVariant)`; the wrapping
/// `convert_card` records all sub-converter failures uniformly via
/// `enrich_gap_path` so the report's frequency table reflects the true
/// distribution of blockers (not just the top-level Rule path). The
/// `ctx` parameter is threaded through to recursive helpers
/// (`recurse_rules`, `graveyard_effect_to_static`) but no longer
/// records gaps directly.
fn convert_rule(
    rule: &Rule,
    face: &str,
    idx: usize,
    stub: &mut EngineFaceStub,
    ctx: &mut Ctx,
) -> ConvResult<()> {
    let tag = variant_tag(rule).unwrap_or_else(|| "<untagged>".to_string());
    let path = format!("{face}/Rules[{idx}]/Rule::{tag}");

    // Phase 4: keyword conversion (most specific — single-variant matches).
    if let Some(kw) = keyword::try_convert(rule, &path)? {
        stub.keywords.push(kw);
        return Ok(());
    }

    // Phase 5–7: structural rule arms — triggered/activated/spell abilities.
    match rule {
        // CR 603: Triggered abilities. `TriggerA` = simple "trigger then actions".
        Rule::TriggerA(trig, actions) => {
            let tds = trigger::convert_many(trig)?;
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            push_triggers(&mut stub.triggers, tds, &body, None);
            return Ok(());
        }
        // CR 603.4 + CR 603.6 + CR 603.10: Triggered ability with intervening-if
        // (`TriggerI`). Uses the ETB-aware converter so that snapshot-derivable
        // `EnteringPermanentPassesFilter` predicates with no `TriggerCondition`
        // analog are routed into each trigger's `valid_card` filter instead of
        // strict-failing.
        Rule::TriggerI(trig, cond, actions) => {
            let tds = trigger::convert_many(trig)?;
            let ext = condition::convert_trigger_with_etb_filter(cond)?;
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            push_triggers_with_valid_card(
                &mut stub.triggers,
                tds,
                &body,
                ext.condition,
                ext.valid_card,
            );
            return Ok(());
        }
        // CR 603.4: Frequency-limited triggered abilities — TriggerA + constraint.
        Rule::TriggerOnce(trig, actions) => {
            let mut td = trigger::convert(trig)?;
            td.constraint = Some(engine::types::ability::TriggerConstraint::OncePerGame);
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            stub.triggers.push(td);
            return Ok(());
        }
        Rule::TriggerOnceEachTurn(trig, actions) => {
            let mut td = trigger::convert(trig)?;
            td.constraint = Some(engine::types::ability::TriggerConstraint::OncePerTurn);
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            stub.triggers.push(td);
            return Ok(());
        }
        Rule::TriggerTwiceEachTurn(trig, actions) => {
            let mut td = trigger::convert(trig)?;
            td.constraint =
                Some(engine::types::ability::TriggerConstraint::MaxTimesPerTurn { max: 2 });
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            stub.triggers.push(td);
            return Ok(());
        }
        Rule::TriggerOnceEachTurnI(trig, cond, actions)
        | Rule::TriggerIOnceEachTurn(trig, cond, actions) => {
            let mut td = trigger::convert(trig)?;
            // CR 603.6 + CR 603.10: ETB-aware condition conversion (see
            // `Rule::TriggerI`). `valid_card` merges with any pre-existing
            // filter set by the trigger builder.
            let ext = condition::convert_trigger_with_etb_filter(cond)?;
            td.condition = ext.condition;
            if let Some(vc) = ext.valid_card {
                td.valid_card = Some(condition::merge_valid_card(td.valid_card.take(), vc));
            }
            td.constraint = Some(engine::types::ability::TriggerConstraint::OncePerTurn);
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            stub.triggers.push(td);
            return Ok(());
        }
        Rule::TriggerIOnce(trig, cond, actions) => {
            let mut td = trigger::convert(trig)?;
            let ext = condition::convert_trigger_with_etb_filter(cond)?;
            td.condition = ext.condition;
            if let Some(vc) = ext.valid_card {
                td.valid_card = Some(condition::merge_valid_card(td.valid_card.take(), vc));
            }
            td.constraint = Some(engine::types::ability::TriggerConstraint::OncePerGame);
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            stub.triggers.push(td);
            return Ok(());
        }
        Rule::TriggerMayOnceEachTurn(trig, actions) => {
            let mut td = trigger::convert(trig)?;
            td.constraint = Some(engine::types::ability::TriggerConstraint::OncePerTurn);
            td.optional = true;
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            stub.triggers.push(td);
            return Ok(());
        }
        Rule::TriggerMayOnceEachTurnI(trig, cond, actions) => {
            let mut td = trigger::convert(trig)?;
            let ext = condition::convert_trigger_with_etb_filter(cond)?;
            td.condition = ext.condition;
            if let Some(vc) = ext.valid_card {
                td.valid_card = Some(condition::merge_valid_card(td.valid_card.take(), vc));
            }
            td.constraint = Some(engine::types::ability::TriggerConstraint::OncePerTurn);
            td.optional = true;
            let conv = action::convert_actions(actions)?;
            let body = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            td.execute = Some(Box::new(body));
            stub.triggers.push(td);
            return Ok(());
        }
        // CR 602: Activated abilities — `Activated(Cost, Actions)`.
        Rule::Activated(cost_box, actions) => {
            let cost = cost::convert(cost_box)?;
            let conv = action::convert_actions(actions)?;
            let ability = build_ability_from_actions(AbilityKind::Activated, Some(cost), conv)?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 602.5: Activated with timing/limit modifiers — same shape plus restrictions.
        Rule::ActivatedWithModifiers(cost_box, actions, modifier) => {
            let cost = cost::convert(cost_box)?;
            let conv = action::convert_actions(actions)?;
            let mut ability = build_ability_from_actions(AbilityKind::Activated, Some(cost), conv)?;
            let restrictions = convert_activate_modifier(modifier)?;
            ability = ability.activation_restrictions(restrictions);
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 601: Spell-effect rules on instants/sorceries — `SpellActions(Actions)`.
        Rule::SpellActions(actions) => {
            let conv = action::convert_actions(actions)?;
            let ability = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 702.33d: Kicker — two-branch spell. `was_kicked` actions
        // override the parent (`wasnt_kicked`) when the kicker cost was
        // paid (`AdditionalCostPaidInstead`). The engine synthesizes
        // `additional_cost` from `Keyword::Kicker(ManaCost)` at card-face
        // assembly time, so all we need on the stub is the keyword + the
        // two-branch ability.
        Rule::SpellActions_Kicker(cost, was_kicked, wasnt_kicked) => {
            let mana = require_pure_mana(cost, "Rule::SpellActions_Kicker")?;
            stub.keywords.push(Keyword::Kicker(mana));
            let was = unwrap_actions(was_kicked, "Rule::SpellActions_Kicker/was")?;
            let wasnt = unwrap_actions(wasnt_kicked, "Rule::SpellActions_Kicker/wasnt")?;
            let ability =
                build_two_branch_spell(action::convert_list(&wasnt)?, action::convert_list(&was)?)?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 702.96a: Overload — same two-branch shape as Kicker.
        Rule::SpellActions_Overload(cost, paid, not_paid) => {
            let mana = require_pure_mana(cost, "Rule::SpellActions_Overload")?;
            stub.keywords.push(Keyword::Overload(mana));
            let p = unwrap_actions(paid, "Rule::SpellActions_Overload/paid")?;
            let np = unwrap_actions(not_paid, "Rule::SpellActions_Overload/not_paid")?;
            let ability =
                build_two_branch_spell(action::convert_list(&np)?, action::convert_list(&p)?)?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 702.99a: Cleave — same two-branch shape; cleave-paid removes
        // bracketed text, but mtgish encodes the resolved branches directly.
        Rule::SpellActions_Cleave(cost, paid, not_paid) => {
            let mana = require_pure_mana(cost, "Rule::SpellActions_Cleave")?;
            stub.keywords.push(Keyword::Cleave(mana));
            let p = unwrap_actions(paid, "Rule::SpellActions_Cleave/paid")?;
            let np = unwrap_actions(not_paid, "Rule::SpellActions_Cleave/not_paid")?;
            let ability =
                build_two_branch_spell(action::convert_list(&np)?, action::convert_list(&p)?)?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 702.34a: Madness — alternative cost from exile after discard.
        // mtgish gives us paid/unpaid variants for X-cost madness (Asylum
        // Visitor pattern); both branches produce full effect lists.
        Rule::SpellActions_MadnessX(cost, paid, not_paid) => {
            let mana = require_pure_mana(cost, "Rule::SpellActions_MadnessX")?;
            stub.keywords.push(Keyword::Madness(mana));
            let p = unwrap_actions(paid, "Rule::SpellActions_MadnessX/paid")?;
            let np = unwrap_actions(not_paid, "Rule::SpellActions_MadnessX/not_paid")?;
            let ability =
                build_two_branch_spell(action::convert_list(&np)?, action::convert_list(&p)?)?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 702.113a: Awaken N—{cost} — alternative-cost spell with a
        // paired land-buff effect. Same two-branch shape as Kicker:
        // paid/unpaid branches both produce full effect lists; the paid
        // (`WasAwakened`) branch becomes the `AdditionalCostPaidInstead`
        // override.
        Rule::SpellActions_Awaken(cost, was, wasnt) => {
            let mana = require_pure_mana(cost, "Rule::SpellActions_Awaken")?;
            stub.keywords.push(Keyword::Awaken(mana));
            let was_acts = unwrap_actions(was, "Rule::SpellActions_Awaken/was")?;
            let wasnt_acts = unwrap_actions(wasnt, "Rule::SpellActions_Awaken/wasnt")?;
            let ability = build_two_branch_spell(
                action::convert_list(&wasnt_acts)?,
                action::convert_list(&was_acts)?,
            )?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 101.2 + CR 702.18 + CR 702.60: Spell-time spell-effect modifiers
        // (CantBeCountered, CantBeCopied, Cascade, SplitSecond, ...). Each
        // inner SpellEffect maps to either a face-level keyword or a static
        // ability with `StaticMode::CantBe*`. Strict-fail variants we don't
        // recognize so the report tracks them.
        Rule::ThisSpellEffect(effects) => {
            for eff in effects {
                apply_this_spell_effect(eff, stub)?;
            }
            return Ok(());
        }
        // CR 601.2: Cast-time spell modifiers — additional/alternate cost,
        // free-cast permissions, conditional flash, casting restrictions,
        // and per-target strive surcharges. Each variant emits exactly one
        // entry into the matching CardFace slot via `cast_effect::apply`.
        Rule::CastEffect(eff) => {
            cast_effect::apply(eff, stub)?;
            return Ok(());
        }
        // CR 101.2 + CR 117.7 + CR 702.7: "Spells matching [filter] have
        // [property]." Maps each inner SpellEffect to a `StaticDefinition`
        // whose `affected` filter is the converted `Spells` predicate. Only
        // the boolean-static SpellEffects (CantBeCountered, CantBeCopied)
        // map cleanly today; cost-modifier and ability-grant inners
        // strict-fail pending dedicated mappings.
        Rule::StackSpellsEffect(spells, effects) => {
            let affected = filter::spells_to_filter(spells)?;
            for eff in effects {
                stub.statics
                    .push(stack_spell_effect_to_static(eff, &affected)?);
            }
            return Ok(());
        }
        // CR 702.152a: Gift — opponent receives a gift if "promised" while
        // casting. mtgish encodes the gift action separately from the two
        // branches; we map the action to a `GiftKind` and emit the
        // promised-branch effect as the override sub via the two-branch
        // spell helper (Instead semantics).
        Rule::SpellActions_Gift(gift_action, was_promised, wasnt_promised) => {
            let kind = gift_action_to_kind(gift_action)?;
            stub.keywords.push(Keyword::Gift(kind));
            let was = unwrap_actions(was_promised, "Rule::SpellActions_Gift/was_promised")?;
            let wasnt = unwrap_actions(wasnt_promised, "Rule::SpellActions_Gift/wasnt_promised")?;
            let ability =
                build_two_branch_spell(action::convert_list(&wasnt)?, action::convert_list(&was)?)?;
            stub.abilities.push(ability);
            return Ok(());
        }
        // CR 613.6 + CR 305.2 + CR 402.2: Static player effects — "you may
        // look at top of library", "you have no maximum hand size", "you
        // skip your draw step", etc. Maps each inner PlayerEffect to a
        // `StaticDefinition` whose affected filter resolves to the right
        // controller (You / Opponent / TargetPlayer).
        Rule::PlayerEffect(player, effects) => {
            player_effect::apply_for_player(player, effects, &mut stub.statics)?;
            return Ok(());
        }
        Rule::EachPlayerEffect(players, effects) => {
            player_effect::apply_for_players(players, effects, &mut stub.statics)?;
            return Ok(());
        }
        // CR 702.124 + CR 903.4: Deck-construction metadata. Partner family
        // becomes a `Keyword::Partner(PartnerType)`; pure deck-build metadata
        // with no in-game effect (CanBeYourCommander, CanHave*OfThisCard,
        // banlist flags) is silently consumed — the engine derives commander
        // legality from type-line + Oracle text at card-face assembly, and
        // the limit-N deckbuilding rule is enforced at deck-validation time.
        Rule::DeckConstruction(dc) => {
            apply_deck_construction(dc, stub)?;
            return Ok(());
        }
        // CR 702.139: Companion — deckbuilding-time keyword. Each of the
        // 10 companion cards has a unique `CompanionCondition` enumerated
        // in the engine; mtgish encodes the same condition as a structural
        // filter expression. `companion::convert` pattern-matches the
        // shape and emits the matching engine variant.
        Rule::Companion(c) => {
            stub.keywords
                .push(Keyword::Companion(companion::convert(c)?));
            return Ok(());
        }
        // CR 613: Continuous static effects on a single permanent (usually self).
        Rule::PermanentLayerEffect(target, effects) => {
            let affected = filter::convert_permanent(target)?;
            let s = static_effect::build_static(affected, effects)?;
            stub.statics.push(s);
            return Ok(());
        }
        // CR 613: Continuous static effects on each permanent matching a filter.
        // "Creatures you control get +1/+1" / "Other Goblins have haste".
        Rule::EachPermanentLayerEffect(filter_box, effects) => {
            let affected = filter::convert(filter_box)?;
            let s = static_effect::build_static(affected, effects)?;
            stub.statics.push(s);
            return Ok(());
        }
        // CR 113.6: Rules-modifying continuous effects on a single permanent
        // ("This creature can't attack").
        Rule::PermanentRuleEffect(target, rules) => {
            let affected = filter::convert_permanent(target)?;
            for r in rules {
                stub.statics
                    .push(static_effect::convert_permanent_rule(r, affected.clone())?);
            }
            return Ok(());
        }
        // Same on each permanent matching a filter ("Creatures your opponents control can't attack").
        Rule::EachPermanentRuleEffect(filter_box, rules) => {
            let affected = filter::convert(filter_box)?;
            for r in rules {
                stub.statics
                    .push(static_effect::convert_permanent_rule(r, affected.clone())?);
            }
            return Ok(());
        }
        // CR 113.6 + CR 401.1: Continuous effects that grant abilities /
        // keywords to each card in a graveyard matching a filter (Karador
        // family). The static's `affected` filter selects the matching
        // graveyard cards; `affected_zone = Graveyard` scopes the effect
        // to the graveyard zone. Each `GraveyardCardEffect::AddAbility`
        // entry recurses its `Vec<Rule>` through `recurse_rules` and
        // promotes any produced keywords into `AddKeyword` modifications,
        // any produced abilities into `GrantAbility`, and any produced
        // triggers into `GrantTrigger`. Statics/replacements in the
        // graveyard-grant body are non-canonical and strict-fail.
        //
        // The `Player` argument (whose graveyard) is dropped today: every
        // mtgish corpus occurrence uses `Player::You`, and the filter
        // shape `controller` slot would be the right home if other
        // values appeared. We emit no `controller` constraint until
        // a non-You owner shows up.
        Rule::EachCardInGraveyardEffect(cards, _whose_graveyard, effects) => {
            let affected = filter::cards_in_graveyard_to_filter(cards)?;
            for eff in effects {
                stub.statics
                    .push(graveyard_effect_to_static(eff, &affected, face, idx, ctx)?);
            }
            return Ok(());
        }
        // CR 113.6 + CR 401.1: Hand-zoned static. Same shape as
        // `EachCardInGraveyardEffect` — `HandEffect::AddAbility(Vec<Rule>)`
        // recurses through `recurse_rules` and produces a
        // `StaticDefinition` with `affected_zone = Some(Zone::Hand)` and
        // ContinuousModification entries for each produced inner item.
        Rule::EachCardInPlayersHandEffect(cards, _whose_hand, effects) => {
            let affected = filter::cards_to_filter(cards)?;
            for eff in effects {
                stub.statics
                    .push(hand_effect_to_static(eff, &affected, face, idx, ctx)?);
            }
            return Ok(());
        }
        // CR 602.5 + CR 605.1a: "Activated abilities of [filter] can't be
        // activated" — Pithing Needle / Phyrexian Revoker / Sorcerous
        // Spyglass / Karn family. Maps onto engine
        // `StaticMode::CantBeActivated { who, source_filter, exemption }`.
        // The schema's `ActivatedAbilities` filter that scopes which
        // abilities are prohibited becomes the engine's `source_filter`
        // (filter on the source object whose abilities are blocked).
        // The `NonManaAbility` schema variant is the Pithing Needle
        // exemption (CR 605.1a — "unless they're mana abilities").
        Rule::ActivatedAbilityEffect(abilities_filter, eff) => {
            stub.statics
                .push(activated_ability_effect_to_static(abilities_filter, eff)?);
            return Ok(());
        }
        // CR 614.6: "If [this card] would be put into a graveyard from
        // anywhere, [redirect] instead." Rest in Peace / Necropotence
        // family — keyed on self, no origin restriction. Mirrors the
        // existing `ReplaceWouldPutIntoGraveyard` converter (round 5)
        // but with `valid_card = Some(SelfRef)` and origin = None.
        Rule::AsPutIntoAGraveyardFromAnywhere(_card_self_ref, actions) => {
            let mut reps = replacement::convert_as_put_into_graveyard_from_anywhere(actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 601.3c: "This spell has flash if [condition]." Dropping the
        // inner Condition turns conditional flash into unconditional flash;
        // strict-fail until the mtgish::Condition → engine::ParsedCondition
        // bridge ships. Same family as CastEffect::MayCastAsThoughItHadFlashIf.
        Rule::FlashForCasters(_condition) => {
            return Err(
                crate::convert::result::ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "ParsedCondition",
                    needed_variant: "Rule/FlashForCasters".into(),
                },
            );
        }
        // CR 604.3: Characteristic-defining abilities — defines a static
        // characteristic of the source itself (power, toughness, types, color).
        // Encoded in the engine via StaticDefinition with `characteristic_defining`.
        Rule::CDA_Power(g) => {
            let qty = quantity::convert(g)?;
            let m = match qty {
                engine::types::ability::QuantityExpr::Fixed { value } => {
                    engine::types::ability::ContinuousModification::SetPower { value }
                }
                dynamic => engine::types::ability::ContinuousModification::SetDynamicPower {
                    value: dynamic,
                },
            };
            stub.statics.push(
                engine::types::ability::StaticDefinition::new(
                    engine::types::statics::StaticMode::Continuous,
                )
                .affected(engine::types::ability::TargetFilter::SelfRef)
                .modifications(vec![m])
                .cda(),
            );
            return Ok(());
        }
        // CR 604.3: CDA setting the source's color(s).
        Rule::CDA_Color(c) => {
            let mods = convert_settable_color_to_mods(c)?;
            stub.statics.push(
                engine::types::ability::StaticDefinition::new(
                    engine::types::statics::StaticMode::Continuous,
                )
                .affected(engine::types::ability::TargetFilter::SelfRef)
                .modifications(mods)
                .cda(),
            );
            return Ok(());
        }
        // CR 604.3 + CR 205.3: CDA setting the source's types/subtypes
        // ("creature types include all", Changeling, "X is also a Y", etc.).
        Rule::CDA_Types(t) => {
            let mods = convert_cda_types_to_mods(t)?;
            stub.statics.push(
                engine::types::ability::StaticDefinition::new(
                    engine::types::statics::StaticMode::Continuous,
                )
                .affected(engine::types::ability::TargetFilter::SelfRef)
                .modifications(mods)
                .cda(),
            );
            return Ok(());
        }
        Rule::CDA_Toughness(g) => {
            let qty = quantity::convert(g)?;
            let m = match qty {
                engine::types::ability::QuantityExpr::Fixed { value } => {
                    engine::types::ability::ContinuousModification::SetToughness { value }
                }
                dynamic => engine::types::ability::ContinuousModification::SetDynamicToughness {
                    value: dynamic,
                },
            };
            stub.statics.push(
                engine::types::ability::StaticDefinition::new(
                    engine::types::statics::StaticMode::Continuous,
                )
                .affected(engine::types::ability::TargetFilter::SelfRef)
                .modifications(vec![m])
                .cda(),
            );
            return Ok(());
        }

        // CR 614.12: ETB replacement effects — "this enters tapped",
        // "this enters with a +1/+1 counter", etc.
        Rule::AsPermanentEnters(target, actions) => {
            let mut reps = replacement::convert_as_enters(target, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 702.138: Escape — fires only when this card enters via the
        // Escape alternative cost. Structurally identical to ETB but
        // requires Escape-cast gating the engine doesn't expose;
        // strict-fail with EnginePrerequisiteMissing.
        Rule::AsPermanentEscapes(target, actions) => {
            let mut reps = replacement::convert_as_escapes(target, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 614.1a + CR 111.1: "If a player would create one or more
        // tokens, …" — token-count replacement scoped to a player. The
        // engine has `additional_token_spec` and `ensure_token_specs`
        // for token-creation modifiers, but the action shape here is
        // `CreateTokensInstead(Vec<CreatableToken>)` which substitutes
        // the entire token set rather than appending — no current slot.
        Rule::ReplaceAnyNumberOfTokensWouldBeCreated(_event, _actions) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "token-set substitution (CreateTokensInstead)".into(),
            });
        }
        // CR 614.1a + CR 111.1: "If an effect would create one or more
        // tokens, …" — effect-scoped variant of the above. Same engine
        // gap (no replacement-side token-set substitution slot).
        Rule::ReplaceAnEffectWouldCreateAnyNumberOfTokens(_event, _actions) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ReplacementDefinition",
                needed_variant: "token-set substitution (CreateTokensInstead, effect-scoped)"
                    .into(),
            });
        }
        // CR 614.12: Generalized "permanent would enter" replacement —
        // applies to any permanent matching the event filter, not just self.
        Rule::ReplaceWouldEnter(event, actions) => {
            let mut reps = replacement::convert_replace_would_enter(event, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 614.2 + CR 615.1: Damage replacement effects. Maps the event
        // shape to `damage_*_filter` / `combat_scope` slots and the action
        // (PreventThatDamage / PreventSomeOfThatDamage / CancelThatDamage)
        // to `damage_modification` (Minus { u32::MAX } / Minus). Other actions
        // strict-fail until further engine extensions.
        Rule::ReplaceWouldDealDamage(event, actions) => {
            let mut reps = replacement::convert_replace_would_deal_damage(event, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 614.11: Draw replacement effects. The execute body's
        // `Effect::Draw { count }` is read by the engine's `draw_applier`
        // and substituted for the original event count. Covers DrawACard /
        // DrawNumberCards (modify count) and SkipThatDraw (count = 0).
        Rule::ReplaceWouldDraw(event, actions) => {
            let mut reps = replacement::convert_replace_would_draw(event, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 614.6 + CR 614.12: Zone-redirect on death. Maps the
        // `instead` actions (ExileItInstead / hand / library) onto a
        // `ReplacementEvent::Moved` with `destination_zone =
        // Some(Zone::Graveyard)` and an execute body that rewrites the
        // destination via `Effect::ChangeZone`.
        Rule::ReplaceWouldPutIntoGraveyard(event, actions) => {
            let mut reps = replacement::convert_replace_would_put_into_graveyard(event, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 614.1a: Counter-quantity replacements. Maps the
        // Hardened-Scales (+1) and Doubling-Season (×2) families onto
        // `quantity_modification: Some(QuantityModification::Plus|Double)`.
        Rule::ReplaceWouldPutCounters(event, actions) => {
            let mut reps = replacement::convert_replace_would_put_counters(event, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 614.1a: Life-gain replacements. Maps Boon Reflection
        // (Twice) and Hardened-Heart (+N) families onto
        // `quantity_modification: Some(Double|Plus)`. The engine's
        // `gain_life_applier` was extended in this round to consume
        // `quantity_modification` in addition to its legacy
        // `Effect::GainLife { amount: Fixed }` execute-body delta.
        Rule::ReplaceWouldGainLife(event, actions) => {
            let mut reps = replacement::convert_replace_would_gain_life(event, actions)?;
            stub.replacements.append(&mut reps);
            return Ok(());
        }
        // CR 701.52a + CR 702.159a: Visit — Attraction trigger that fires
        // when its controller rolls to visit and the result hits a number
        // lit on this Attraction. Engine has the trigger mode but no
        // runtime hook; this converter slot ships structurally correct
        // data so Phase 14 doesn't have to know about Attractions.
        Rule::Visit(actions) => {
            let effects = action::convert_list(actions)?;
            let exec = build_ability_chain(AbilityKind::Spell, None, effects)?;
            let trigger =
                TriggerDefinition::new(engine::types::triggers::TriggerMode::VisitAttraction)
                    .valid_card(engine::types::ability::TargetFilter::SelfRef)
                    .execute(exec);
            stub.triggers.push(trigger);
            return Ok(());
        }
        // CR 701.52a + CR 702.159a: VisitAndPrize — Visit trigger that
        // additionally claims a prize. Engine encodes prize as part of
        // the same trigger body; the converter chains the prize actions
        // after the visit actions in the same execute body.
        Rule::VisitAndPrize(visit_actions, prize_actions) => {
            let mut effects = action::convert_list(visit_actions)?;
            effects.extend(action::convert_list(prize_actions)?);
            let exec = build_ability_chain(AbilityKind::Spell, None, effects)?;
            let trigger =
                TriggerDefinition::new(engine::types::triggers::TriggerMode::VisitAttraction)
                    .valid_card(engine::types::ability::TargetFilter::SelfRef)
                    .execute(exec);
            stub.triggers.push(trigger);
            return Ok(());
        }
        // CR 714.2: Saga chapter triggers — one CounterAdded trigger per
        // chapter ordinal, keyed on the lore counter type. The Saga ETB
        // lore counter is supplied by the engine's Saga handling pipeline.
        Rule::SagaChapters(chapters) => {
            let mut trigs = saga::convert(chapters)?;
            stub.triggers.append(&mut trigs);
            return Ok(());
        }
        // Pre-game / draft metadata with no in-game effect at runtime.
        // Mirrors the round-2 `DeckConstruction::CanBeYourCommander` and
        // `RemoveFromDeckIfNotPlayingForAnte` treatment: silently consumed
        // because the engine has no Vanguard / Conspiracy-draft runtime.
        // The card emits no engine artifact for these rules but converts
        // cleanly so the rest of its rules can register.
        //
        // - `StartingIntensity(GameNumber)`: Vanguard starting-loyalty
        //   metadata; only appears on `OracleCard::Vanguard`.
        // - `AsSelfDraft(Vec<DraftAction>)`: Conspiracy draft-time effects
        //   (DraftFaceUp / RevealThisDraftedCard / etc.); applies before
        //   the game starts.
        // - CR 702.106a: `HiddenAgenda` — Conspiracy "as you put this
        //   conspiracy card into the command zone, turn it face down and
        //   secretly choose a card name." The chosen name is consumed by
        //   linked abilities elsewhere on the card (CR 702.106d), but the
        //   choosing itself is pre-game / outside-the-game.
        // - `DoubleAgenda`: Conspiracy variant of HiddenAgenda that
        //   chooses two names instead of one. Same pre-game scope.
        // - `FaceUpDraftEffect(FaceUpDraftEffect)`: Conspiracy draft-time
        //   effect that fires when the card is drafted face-up.
        Rule::StartingIntensity(_)
        | Rule::AsSelfDraft(_)
        | Rule::HiddenAgenda
        | Rule::DoubleAgenda
        | Rule::FaceUpDraftEffect(_) => {
            return Ok(());
        }
        // CR 608.2c + CR 613: Conditional wrappers. We recurse the inner
        // body into a fresh stub; if the only items produced are statics
        // (the dominant shape — "if [source] is tapped, [layer effect]"),
        // we decorate each one with the condition. Mixed bodies (triggers /
        // abilities / replacements) require a context-specific decorator
        // we haven't built yet and strict-fail so the report tracks them.
        Rule::If(cond, body) => {
            let inner = recurse_rules(body, face, idx, ctx)?;
            apply_condition(rule, cond, false, inner, stub)?;
            return Ok(());
        }
        Rule::Unless(cond, body) => {
            let inner = recurse_rules(body, face, idx, ctx)?;
            apply_condition(rule, cond, true, inner, stub)?;
            return Ok(());
        }
        // CR 608.2c + CR 613: IfElse — apply the positive condition to the
        // first body and the negated condition to the second. Each branch's
        // produced items are independently decorated and merged into the
        // parent stub. This works only for static-only branches today; mixed
        // branches strict-fail via the helper.
        Rule::IfElse(cond, then_body, else_body) => {
            let then_stub = recurse_rules(then_body, face, idx, ctx)?;
            let else_stub = recurse_rules(else_body, face, idx, ctx)?;
            apply_condition(rule, cond, false, then_stub, stub)?;
            apply_condition(rule, cond, true, else_stub, stub)?;
            return Ok(());
        }
        // CR 702.178a: "max speed" wrapper — body abilities/statics function
        // only while the controller has max speed. Same recursive-decorator
        // pattern as If/Unless, with `StaticCondition::HasMaxSpeed` baked in.
        Rule::MaxSpeed(body) => {
            let inner = recurse_rules(body, face, idx, ctx)?;
            apply_static_condition_typed(
                rule,
                engine::types::ability::StaticCondition::HasMaxSpeed,
                inner,
                stub,
            )?;
            return Ok(());
        }

        // Zone-scoped wrappers — for now we ignore the zone (TODO: thread it
        // into AbilityDefinition.activation_zone) and just dispatch the inner
        // rule. Loses fidelity, but unlocks many flashback/graveyard activated
        // shapes whose inner Activated rule already converts.
        Rule::FromGraveyard(inner)
        | Rule::FromExile(inner)
        | Rule::FromExileOrBattlefield(inner)
        | Rule::FromHand(inner)
        | Rule::FromAnyZone(inner)
        | Rule::FromCommandZone(inner)
        | Rule::FromCommandZoneOrBattlefield(inner)
        | Rule::FromGraveyardOrBattlefield(inner)
        | Rule::FromStack(inner) => {
            return convert_rule(inner, face, idx, stub, ctx);
        }
        _ => {}
    }

    // Recording is now performed by the wrapping `convert_card` for ALL
    // sub-converter failures uniformly (including this top-level
    // UnknownVariant). The dispatcher just returns the typed gap.
    Err(ConversionGap::UnknownVariant {
        path: path.clone(),
        repr: truncated_repr(rule),
    })
}

/// CR 603: Push one or more `TriggerDefinition`s onto the stub, sharing
/// a single body via cloning. When `Trigger::Or` fans out into N
/// definitions each must carry its own clone of the execute body and
/// (if any) intervening-if condition. Single-trigger lists are the
/// degenerate case (one push, no extra clones).
fn push_triggers(
    out: &mut Vec<TriggerDefinition>,
    tds: Vec<TriggerDefinition>,
    body: &AbilityDefinition,
    condition: Option<engine::types::ability::TriggerCondition>,
) {
    push_triggers_with_valid_card(out, tds, body, condition, None);
}

/// CR 603.6 + CR 603.10: Variant of `push_triggers` that additionally merges
/// an extra ETB-derived `TargetFilter` into each trigger's `valid_card` (via
/// `condition::merge_valid_card`). Used when a `Condition::EnteringPermanentPassesFilter`
/// predicate is snapshot-derivable and routed through `valid_card` instead of
/// `TriggerCondition`.
fn push_triggers_with_valid_card(
    out: &mut Vec<TriggerDefinition>,
    tds: Vec<TriggerDefinition>,
    body: &AbilityDefinition,
    condition: Option<engine::types::ability::TriggerCondition>,
    extra_valid_card: Option<engine::types::ability::TargetFilter>,
) {
    for mut td in tds {
        td.execute = Some(Box::new(body.clone()));
        if let Some(c) = &condition {
            td.condition = Some(c.clone());
        }
        if let Some(vc) = &extra_valid_card {
            td.valid_card = Some(condition::merge_valid_card(
                td.valid_card.take(),
                vc.clone(),
            ));
        }
        out.push(td);
    }
}

/// Build an `AbilityDefinition` whose head is the first effect, with the
/// remainder chained via `sub_ability`. Empty effects → strict-failure
/// (no card prints "do nothing").
pub(crate) fn build_ability_chain(
    kind: AbilityKind,
    cost: Option<engine::types::ability::AbilityCost>,
    effects: Vec<Effect>,
) -> ConvResult<AbilityDefinition> {
    let mut iter = effects.into_iter();
    let head = iter.next().ok_or(ConversionGap::MalformedIdiom {
        idiom: "Rule/empty_action_list",
        path: String::new(),
        detail: "ActionList contained no actions".into(),
    })?;
    let mut ability = AbilityDefinition::new(kind, head);
    if let Some(c) = cost {
        ability = ability.cost(c);
    }
    // Chain remaining effects via sub_ability (each a Spell-kind continuation).
    let tail: Vec<_> = iter.collect();
    if !tail.is_empty() {
        let mut chain: Option<AbilityDefinition> = None;
        for eff in tail.into_iter().rev() {
            let mut next = AbilityDefinition::new(AbilityKind::Spell, eff);
            if let Some(child) = chain.take() {
                next = next.sub_ability(child);
            }
            chain = Some(next);
        }
        if let Some(sub) = chain {
            ability = ability.sub_ability(sub);
        }
    }
    Ok(ability)
}

/// CR 608.2c + CR 700.4: Build an `AbilityDefinition` chain from a sequence
/// of `ChainSegment`s. Each segment becomes one AD link in the `sub_ability`
/// chain, with its head AD carrying `condition` (and `else_ability` for
/// IfElse segments). Within a segment, multiple effects chain
/// unconditionally beneath the segment's head.
///
/// The first segment is the chain root; `kind`/`cost` apply to it. All
/// subsequent segments are `AbilityKind::Spell` continuations with no cost.
fn build_ability_segment_chain(
    kind: AbilityKind,
    cost: Option<engine::types::ability::AbilityCost>,
    segments: Vec<action::ChainSegment>,
) -> ConvResult<AbilityDefinition> {
    let mut iter = segments.into_iter();
    let first = iter.next().ok_or(ConversionGap::MalformedIdiom {
        idiom: "ChainSegment/empty",
        path: String::new(),
        detail: "no segments to build".into(),
    })?;

    // Build the head AD from the first segment, applying the caller-supplied
    // kind/cost. The head's `optional` slot is folded in from the first
    // segment via `apply_segment_optionality` so a leading
    // `OptionalWithCost` segment composes correctly with the dispatch-site
    // cost (or strict-fails when both are present, mirroring the
    // `ActionsConversion::OptionalWithCost` rule at the sole-action layer).
    let mut head = build_ability_chain(kind, cost.clone(), first.effects)?;
    head.condition = first.condition;
    if let Some(else_eff) = first.else_effects {
        let else_ability = build_ability_chain(AbilityKind::Spell, None, else_eff)?;
        head.else_ability = Some(Box::new(else_ability));
    }
    apply_segment_optionality(&mut head, first.optional, cost.is_some())?;
    // CR 119.1 + CR 119.3 + CR 608.2c: A scoped segment runs its effects
    // per matching player. Mirrors `ActionsConversion::Scoped` materialization.
    if let Some(scope) = first.player_scope {
        head.player_scope = Some(scope);
    }

    // Each remaining segment becomes a sub-AD; we attach it at the deepest
    // empty `sub_ability` slot of the running chain so the order matches the
    // original Action sequence.
    for seg in iter {
        let mut sub = build_ability_chain(AbilityKind::Spell, None, seg.effects)?;
        sub.condition = seg.condition;
        if let Some(else_eff) = seg.else_effects {
            let else_ability = build_ability_chain(AbilityKind::Spell, None, else_eff)?;
            sub.else_ability = Some(Box::new(else_ability));
        }
        // Sub-segments never carry a dispatch-site cost, so their
        // `OptionalWithCost` cost slot is always free to fill.
        apply_segment_optionality(&mut sub, seg.optional, false)?;
        // CR 119.1 + CR 119.3 + CR 608.2c: Per-segment player scope —
        // mid-list `Action::EachPlayerAction(s)` / non-You `PlayerAction`.
        if let Some(scope) = seg.player_scope {
            sub.player_scope = Some(scope);
        }
        attach_at_chain_tail(&mut head, sub);
    }
    Ok(head)
}

/// CR 117.5 + CR 117.6 + CR 605.1c: Fold a `SegmentOptional` onto a built
/// `AbilityDefinition`. Mirrors the materialization of
/// `ActionsConversion::Optional` / `OptionalWithCost` so segment-level
/// optionality reuses the same `optional` flag and `cost` slot the
/// sole-action layer uses. When `host_has_cost` is `true` (the head segment
/// of a chain whose dispatch site already supplied a cost), an
/// `OptionalWithCost` segment strict-fails — additive cost composition is
/// not yet expressible.
fn apply_segment_optionality(
    ability: &mut AbilityDefinition,
    optional: action::SegmentOptional,
    host_has_cost: bool,
) -> ConvResult<()> {
    match optional {
        action::SegmentOptional::Mandatory => {}
        action::SegmentOptional::Optional => {
            ability.optional = true;
        }
        action::SegmentOptional::OptionalWithCost(extra_cost) => {
            if host_has_cost {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "AbilityCost",
                    needed_variant:
                        "additive cost composition (head-segment cost + MayCost extra cost)".into(),
                });
            }
            ability.cost = Some(extra_cost);
            ability.optional = true;
        }
    }
    Ok(())
}

/// Walk to the deepest `sub_ability` in `root` and attach `tail` there.
/// Used by `build_ability_segment_chain` so successive segments append
/// (rather than overwrite) the previously-built chain.
fn attach_at_chain_tail(root: &mut AbilityDefinition, tail: AbilityDefinition) {
    let mut cursor: &mut AbilityDefinition = root;
    while cursor.sub_ability.is_some() {
        cursor = cursor.sub_ability.as_mut().expect("checked Some").as_mut();
    }
    cursor.sub_ability = Some(Box::new(tail));
}

/// CR 700.2 / CR 117.5 / CR 117.6 / CR 700.4: Materialize an
/// `ActionsConversion` onto a base `AbilityDefinition`. The base is built
/// from `kind` + `cost` (no effect chain yet); `apply_actions_to_ability`
/// fills in `effect`, `sub_ability`, `else_ability`, `modal`,
/// `mode_abilities`, `optional`, and `condition` according to the
/// converted shape. This is the single seam through which trigger /
/// activated / spell dispatch sites consume an `Actions` body.
pub(crate) fn build_ability_from_actions(
    kind: AbilityKind,
    cost: Option<engine::types::ability::AbilityCost>,
    conv: action::ActionsConversion,
) -> ConvResult<AbilityDefinition> {
    use action::ActionsConversion as A;
    use action::ChooseSpec;
    match conv {
        // CR 608.2c: Linear effect chain — legacy path.
        A::Linear { effects } => build_ability_chain(kind, cost, effects),

        // CR 608.2c + CR 700.4: Linear chain with mid-list conditional gates.
        // Each segment becomes one or more AbilityDefinition links; the head
        // AD of each segment carries the segment's `condition` (and
        // `else_ability` for IfElse segments).
        A::LinearChain { segments } => build_ability_segment_chain(kind, cost, segments),

        // CR 700.2 / CR 700.2d: Modal — parent is a `GenericEffect` no-op
        // marker; modes live on `mode_abilities`. `ModalChoice` carries
        // `min_choices`/`max_choices` derived from the typed `ChooseSpec`.
        A::Modal {
            modes,
            choose,
            constraints,
            entwine_cost,
            allow_repeat_modes,
        } => {
            if modes.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "ActionsConversion::Modal/empty",
                    path: String::new(),
                    detail: "no modes after conversion".into(),
                });
            }
            let mode_count = modes.len();
            let (min_choices, max_choices) = match choose {
                ChooseSpec::One => (1, 1),
                ChooseSpec::Exactly { n } => (n, n),
                ChooseSpec::UpToN { n } => (0, n.min(mode_count)),
                // CR 700.2: "Choose one or more —" — at least 1, up to all modes.
                ChooseSpec::OneOrMore => (1, mode_count),
                // CR 700.2: "Choose any number —" — 0..=mode_count.
                ChooseSpec::AnyNumber => (0, mode_count),
            };
            let modal = engine::types::ability::ModalChoice {
                min_choices,
                max_choices,
                mode_count,
                mode_descriptions: Vec::new(),
                allow_repeat_modes,
                constraints,
                mode_costs: Vec::new(),
                entwine_cost,
            };
            // Each mode body becomes its own `AbilityDefinition` chain.
            let mut mode_abilities = Vec::with_capacity(modes.len());
            for mode_effects in modes {
                mode_abilities.push(build_ability_chain(AbilityKind::Spell, None, mode_effects)?);
            }
            let parent_effect = Effect::GenericEffect {
                static_abilities: vec![],
                duration: None,
                target: None,
            };
            let mut ability =
                AbilityDefinition::new(kind, parent_effect).with_modal(modal, mode_abilities);
            if let Some(c) = cost {
                ability = ability.cost(c);
            }
            Ok(ability)
        }

        // CR 117.5: "You may [do X]" — optional flag on the produced ability.
        A::Optional { effects } => {
            let mut ability = build_ability_chain(kind, cost, effects)?;
            ability.optional = true;
            Ok(ability)
        }

        // CR 117.6 + CR 605.1c: "You may [pay cost] to [do X]." — combine the
        // optional flag with an additional ability cost. When the dispatch
        // site already supplied a cost (e.g., an Activated rule), surface as
        // engine prerequisite — combining two costs requires a bespoke
        // additive-cost shape we haven't built yet.
        A::OptionalWithCost {
            cost: extra_cost,
            effects,
        } => {
            if cost.is_some() {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "AbilityCost",
                    needed_variant:
                        "additive cost composition (Activated cost + MayCost extra cost)".into(),
                });
            }
            let mut ability = build_ability_chain(kind, Some(extra_cost), effects)?;
            ability.optional = true;
            Ok(ability)
        }

        // CR 117.6 + CR 605.1c + CR 603.12: Reflexive form of "you may pay X.
        // When you do, [body]." Parent's effect is `Effect::PayCost` (the
        // resolution-time cost choice); sub_ability carries the body gated on
        // `AbilityCondition::WhenYouDo`. Mirrors the native parser's
        // reflexive-trigger lowering (oracle.rs:4272-4290).
        A::OptionalWithCostReflexive {
            cost: extra_cost,
            inner,
        } => {
            if cost.is_some() {
                return Err(ConversionGap::EnginePrerequisiteMissing {
                    engine_type: "AbilityCost",
                    needed_variant:
                        "additive cost composition (Activated cost + reflexive MayCost)".into(),
                });
            }
            let payment_cost = ability_cost_to_payment_cost(&extra_cost)?;
            let mut sub = build_ability_from_actions(AbilityKind::Spell, None, *inner)?;
            sub.condition = Some(engine::types::ability::AbilityCondition::WhenYouDo);
            let parent_effect = engine::types::ability::Effect::PayCost { cost: payment_cost };
            // CR 117.6: `optional = true` on the parent gates `Effect::PayCost`
            // through `WaitingFor::OptionalEffectChoice` (effects/mod.rs:1205+
            // / 2454) — this is the "you may" prompt. Without this flag the
            // engine treats the cost as mandatory-if-able (the "must pay"
            // semantics). Confirmed by `resolve_ability_chain_condition_blocks_optional_prompt`
            // at effects/mod.rs:2429.
            let mut parent = AbilityDefinition::new(kind, parent_effect).sub_ability(sub);
            parent.optional = true;
            Ok(parent)
        }

        // CR 700.4 + CR 608.2c: "If [cond], [do X]." Single-condition gate.
        A::Conditional { condition, effects } => {
            let mut ability = build_ability_chain(kind, cost, effects)?;
            ability.condition = Some(condition);
            Ok(ability)
        }

        // CR 700.4 + CR 608.2c: "If [cond], [do A]. Otherwise, [do B]."
        // Maps onto `condition` + `else_ability`.
        A::Branched {
            condition,
            then_effects,
            else_effects,
        } => {
            let mut ability = build_ability_chain(kind, cost, then_effects)?;
            ability.condition = Some(condition);
            let else_ability = build_ability_chain(AbilityKind::Spell, None, else_effects)?;
            ability.else_ability = Some(Box::new(else_ability));
            Ok(ability)
        }
        // CR 608.2c: Player-scoped wrapper — materialize the inner
        // conversion, then attach `player_scope` so the engine iterates
        // the effect over the matching players (each becomes the acting
        // controller).
        A::Scoped {
            inner,
            player_scope,
        } => {
            let mut ability = build_ability_from_actions(kind, cost, *inner)?;
            ability.player_scope = Some(player_scope);
            Ok(ability)
        }
    }
}

/// Convert mtgish `ActivateModifier` into engine `ActivationRestriction`s.
/// CR 602.5: Activation timing / frequency limits. Strict-failure on
/// idioms not yet mapped (RequiresCondition / threshold gates land later).
fn convert_activate_modifier(
    m: &crate::schema::types::ActivateModifier,
) -> ConvResult<Vec<ActivationRestriction>> {
    use crate::schema::types::ActivateModifier as M;
    Ok(match m {
        M::And(parts) => {
            let mut all = Vec::new();
            for p in parts {
                all.extend(convert_activate_modifier(p)?);
            }
            all
        }
        M::ActivateOnlyAsASorcery => vec![ActivationRestriction::AsSorcery],
        M::ActivateOnlyAsAnInstant => vec![ActivationRestriction::AsInstant],
        M::ActivateOnlyDuringTheirTurn => vec![ActivationRestriction::DuringYourTurn],
        M::ActivateOnlyOnce => vec![ActivationRestriction::OnlyOnce],
        M::ActivateOnlyOnceEachTurn => vec![ActivationRestriction::OnlyOnceEachTurn],
        M::ActivateNoMoreThanNumberTimesEachTurn(n) => {
            let qty = quantity::convert(n)?;
            match qty {
                engine::types::ability::QuantityExpr::Fixed { value }
                    if (1..=255).contains(&value) =>
                {
                    vec![ActivationRestriction::MaxTimesEachTurn { count: value as u8 }]
                }
                _ => {
                    return Err(ConversionGap::MalformedIdiom {
                        idiom: "ActivateModifier/max_times",
                        path: String::new(),
                        detail: "non-fixed activation count".into(),
                    });
                }
            }
        }
        // CR 602.5 + CR 602.5b: "Activate only if [condition]" gates the
        // begin-to-activate step (CR 602.1a), NOT resolution. Bridge the
        // inner Condition through `convert_parsed` and emit
        // `ActivationRestriction::RequiresCondition { condition: Some(_) }`.
        // Engine evaluates `None` as always-pass via `is_none_or` in
        // restrictions.rs:494, so a translation gap propagates as the rule's
        // strict-fail rather than silently lifting the gate.
        M::ActivateOnlyIf(condition) => {
            let parsed = crate::convert::condition::convert_parsed(condition)?;
            vec![ActivationRestriction::RequiresCondition {
                condition: Some(parsed),
            }]
        }
        _ => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: serde_json::to_value(m)
                    .ok()
                    .and_then(|v| {
                        v.get("_ActivateModifier")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "<unknown>".into()),
            });
        }
    })
}

/// Extract the inner `Actions` from a tuple newtype struct (`WasKicked`,
/// `WasntKicked`, `OverloadPaid`, etc.) whose field is module-private.
/// `Actions` round-trips through JSON cleanly because the vendored type is
/// `Serialize + Deserialize`. Avoids touching the vendored schema file.
fn unwrap_actions<T: serde::Serialize>(
    branch: &T,
    idiom: &'static str,
) -> ConvResult<crate::schema::types::Actions> {
    let value = serde_json::to_value(branch).map_err(|e| ConversionGap::MalformedIdiom {
        idiom,
        path: String::new(),
        detail: format!("serialize branch: {e}"),
    })?;
    // Tuple newtype structs serialize as the inner value directly when the
    // field count is 1.
    serde_json::from_value(value).map_err(|e| ConversionGap::MalformedIdiom {
        idiom,
        path: String::new(),
        detail: format!("re-deserialize branch as Actions: {e}"),
    })
}

/// CR 702.124 + CR 903: Apply a `DeckConstruction` rule. Partner family
/// pushes a `Keyword::Partner(PartnerType)` onto the face. Pure deck-build
/// metadata that the engine derives or enforces elsewhere is silently
/// consumed (no engine artifact emitted, but `Ok(())` returned so the
/// card converts cleanly).
fn apply_deck_construction(
    dc: &crate::schema::types::DeckConstruction,
    stub: &mut EngineFaceStub,
) -> ConvResult<()> {
    use crate::schema::types::DeckConstruction as D;
    use engine::types::keywords::PartnerType;
    match dc {
        // CR 702.124a: Generic Partner.
        D::Partner => stub.keywords.push(Keyword::Partner(PartnerType::Generic)),
        // CR 702.124c: Partner with [Name].
        D::PartnerWith(name) => stub
            .keywords
            .push(Keyword::Partner(PartnerType::With(name.clone()))),
        // CR 702.124f: Friends forever.
        D::PartnerFriendsForever => stub
            .keywords
            .push(Keyword::Partner(PartnerType::FriendsForever)),
        // CR 702.124: "Partner — Character select".
        D::PartnerCharacterSelect => stub
            .keywords
            .push(Keyword::Partner(PartnerType::CharacterSelect)),
        // CR 702.124: Doctor's companion.
        D::DoctorsCompanion => stub
            .keywords
            .push(Keyword::Partner(PartnerType::DoctorsCompanion)),
        // CR 702.124: "Choose a Background".
        D::ChooseABackground => stub
            .keywords
            .push(Keyword::Partner(PartnerType::ChooseABackground)),
        // CR 903.4: "[This card] can be your commander." — engine derives
        // commander legality from type-line + Oracle text at card-face
        // assembly. Pure metadata, no in-game effect; silently consume.
        D::CanBeYourCommander => {}
        // CR 100.2a: Singleton-rule overrides ("a deck can have any number
        // of cards named ~"). Engine enforces at deck-validation time, not
        // in-game; silently consume.
        D::CanHaveAnyNumberOfThisCard | D::CanHaveUptoNumberOfThisCard(_) => {}
        // Ante and banlist annotations are tournament-policy metadata, not
        // in-game rules. Silently consume.
        D::ThisCardIsBanned | D::RemoveFromDeckIfNotPlayingForAnte => {}
        // Partner-Survivors is rare and absent from engine `PartnerType`;
        // surface as a strict prerequisite gap.
        D::PartnerSurvivors => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "PartnerType",
                needed_variant: "Survivors".into(),
            });
        }
        D::PartnerFatherAndSon => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "PartnerType",
                needed_variant: "FatherAndSon".into(),
            });
        }
    }
    Ok(())
}

/// CR 101.2: Apply a single `SpellEffect` (inner of `Rule::ThisSpellEffect`)
/// onto the face stub. The recognized subset covers spell-time statics
/// (`CantBeCountered`, `CantBeCopied`) emitted as a `StaticDefinition`
/// targeting the source via `StaticMode::CantBe*`, plus face-level
/// keywords (`Cascade`, `SplitSecond`). Anything outside this subset
/// strict-fails so the report tracks it.
fn apply_this_spell_effect(
    eff: &crate::schema::types::SpellEffect,
    stub: &mut EngineFaceStub,
) -> ConvResult<()> {
    use crate::schema::types::SpellEffect as S;
    use engine::types::ability::TargetFilter;
    use engine::types::statics::StaticMode;
    match eff {
        // CR 101.2: This spell can't be countered.
        S::CantBeCountered => {
            stub.statics.push(
                engine::types::ability::StaticDefinition::new(StaticMode::CantBeCountered)
                    .affected(TargetFilter::SelfRef),
            );
        }
        // CR 101.2 + CR 707.10: This spell can't be copied.
        S::CantBeCopied => {
            stub.statics.push(
                engine::types::ability::StaticDefinition::new(StaticMode::CantBeCopied)
                    .affected(TargetFilter::SelfRef),
            );
        }
        // CR 702.84a: Cascade — face-level keyword.
        S::Cascade => stub.keywords.push(Keyword::Cascade),
        // CR 702.60a: Split second — face-level keyword.
        S::SplitSecond => stub.keywords.push(Keyword::SplitSecond),
        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: serde_json::to_value(other)
                    .ok()
                    .and_then(|v| {
                        v.get("_SpellEffect")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "<unknown>".into()),
            });
        }
    }
    Ok(())
}

/// CR 101.2 + CR 117.7: Apply a single `SpellEffect` (inner of
/// `Rule::StackSpellsEffect`) against a precomputed spell-target filter.
/// Mirrors `apply_this_spell_effect` but emits the static with `affected
/// = <spells filter>` rather than `SelfRef`, so the static applies to
/// every spell on the stack matching the filter, not just the source.
///
/// CR 113.6 + CR 401.1: Build a single graveyard-zone static from one
/// `GraveyardCardEffect`. The dominant shape (29/30 corpus occurrences)
/// is `AddAbility(Vec<Rule>)` — the body is a single keyword Rule
/// (Flashback, Unearth, Escape, etc.) that grants the corresponding
/// ability to the graveyard cards.
///
/// Strategy: recurse the inner `Vec<Rule>` through `recurse_rules`,
/// then promote each produced item into a `ContinuousModification`:
///
/// - keyword → `AddKeyword { keyword }`
/// - ability → `GrantAbility { definition }`
/// - trigger → `GrantTrigger { trigger }`
///
/// Statics / replacements inside an ability-grant body are non-canonical
/// and strict-fail.
///
/// Other GraveyardCardEffect variants (LosesAllAbilities,
/// CantBeTheTargetOfSpellsOrAbilities, AddCreatureTypeVariable) require
/// their own ContinuousModification mappings and strict-fail today.
/// CR 602.5 + CR 605.1a: Build a `CantBeActivated` static from
/// `Rule::ActivatedAbilityEffect(filter, CantBeActivated)`. The schema
/// `ActivatedAbilities` filter selects which source objects' abilities
/// are blocked; `NonManaAbility` flips the engine's `ActivationExemption`
/// to `ManaAbilities` (CR 605.1a — "unless they're mana abilities").
///
/// Other ActivatedAbilityEffect variants (IncreaseManaCost,
/// AdditionalCost, ReduceManaCostNotLessThanOne) need engine slots
/// that don't exist today (the engine has `ReduceAbilityCost` keyed on
/// keyword name, not on a generic ManaCost delta) and strict-fail.
fn activated_ability_effect_to_static(
    abilities: &crate::schema::types::ActivatedAbilities,
    eff: &crate::schema::types::ActivatedAbilityEffect,
) -> ConvResult<StaticDefinition> {
    use crate::schema::types::ActivatedAbilityEffect as E;
    use engine::types::statics::{ProhibitionScope, StaticMode};
    let (source_filter, exemption) = activated_abilities_to_filter(abilities)?;
    let mode = match eff {
        E::CantBeActivated => StaticMode::CantBeActivated {
            who: ProhibitionScope::AllPlayers,
            source_filter,
            exemption,
        },
        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: serde_json::to_value(other)
                    .ok()
                    .and_then(|v| {
                        v.get("_ActivatedAbilityEffect")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "<unknown>".to_string()),
            });
        }
    };
    Ok(StaticDefinition::new(mode))
}

/// CR 602.5: Decompose an `ActivatedAbilities` filter into the
/// `(source_filter, exemption)` pair that engine `CantBeActivated`
/// expects. `AbilityOfAPermanent`/`AbilityOfPermanent` carry the source
/// filter; `NonManaAbility` is the canonical Pithing Needle shape with
/// any-source + mana-ability exemption.
fn activated_abilities_to_filter(
    abilities: &crate::schema::types::ActivatedAbilities,
) -> ConvResult<(
    engine::types::ability::TargetFilter,
    engine::types::statics::ActivationExemption,
)> {
    use crate::schema::types::ActivatedAbilities as A;
    use engine::types::ability::{TargetFilter, TypedFilter};
    use engine::types::statics::ActivationExemption;
    Ok(match abilities {
        A::AbilityOfAPermanent(perms) => (filter::convert(perms)?, ActivationExemption::None),
        A::AbilityOfPermanent(perm) => {
            (filter::convert_permanent(perm)?, ActivationExemption::None)
        }
        // CR 605.1a: Pithing Needle exemption — "non-mana ability" means
        // any source, every activated ability except mana abilities.
        A::NonManaAbility => (
            TargetFilter::Typed(TypedFilter::default()),
            ActivationExemption::ManaAbilities,
        ),
        // CR 602.5: Bare "any activated ability" — any source, no exemption.
        A::AnyAbility => (
            TargetFilter::Typed(TypedFilter::default()),
            ActivationExemption::None,
        ),
        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: serde_json::to_value(other)
                    .ok()
                    .and_then(|v| {
                        v.get("_ActivatedAbilities")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "<unknown>".to_string()),
            });
        }
    })
}

fn graveyard_effect_to_static(
    eff: &crate::schema::types::GraveyardCardEffect,
    affected: &engine::types::ability::TargetFilter,
    face: &str,
    idx: usize,
    ctx: &mut Ctx,
) -> ConvResult<StaticDefinition> {
    use crate::schema::types::GraveyardCardEffect as G;
    use engine::types::ability::ContinuousModification as M;
    use engine::types::statics::StaticMode;
    let mods: Vec<M> = match eff {
        G::AddAbility(body) => {
            let inner = recurse_rules(body, face, idx, ctx)?;
            // Statics or replacements inside an ability-grant body are
            // non-canonical (CR 113.6 + CR 401.1: granted abilities are
            // standalone game-object abilities, not cross-zone statics).
            if !inner.statics.is_empty() || !inner.replacements.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "GraveyardCardEffect::AddAbility/static-or-replacement-in-body",
                    path: String::new(),
                    detail: format!(
                        "expected keyword/ability/trigger inner rules; got statics={} replacements={}",
                        inner.statics.len(),
                        inner.replacements.len()
                    ),
                });
            }
            let mut mods = Vec::new();
            for kw in inner.keywords {
                mods.push(M::AddKeyword { keyword: kw });
            }
            for ab in inner.abilities {
                mods.push(M::GrantAbility {
                    definition: Box::new(ab),
                });
            }
            for tr in inner.triggers {
                mods.push(M::GrantTrigger {
                    trigger: Box::new(tr),
                });
            }
            if mods.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "GraveyardCardEffect::AddAbility/empty-body",
                    path: String::new(),
                    detail: "ability-grant body produced no modifications".into(),
                });
            }
            mods
        }
        G::LosesAllAbilities => vec![M::RemoveAllAbilities],
        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: serde_json::to_value(other)
                    .ok()
                    .and_then(|v| {
                        v.get("_GraveyardCardEffect")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "<unknown>".to_string()),
            });
        }
    };
    Ok(StaticDefinition::new(StaticMode::Continuous)
        .affected(affected.clone())
        .affected_zone(engine::types::zones::Zone::Graveyard)
        .modifications(mods))
}

/// CR 113.6 + CR 401.1: Hand-zoned static analogue of
/// `graveyard_effect_to_static`. Uses `Zone::Hand` for `affected_zone`
/// and walks `HandEffect::AddAbility(Vec<Rule>)` through `recurse_rules`
/// to build the same `ContinuousModification` set (AddKeyword /
/// GrantAbility / GrantTrigger).
fn hand_effect_to_static(
    eff: &crate::schema::types::HandEffect,
    affected: &engine::types::ability::TargetFilter,
    face: &str,
    idx: usize,
    ctx: &mut Ctx,
) -> ConvResult<StaticDefinition> {
    use crate::schema::types::HandEffect as H;
    use engine::types::ability::ContinuousModification as M;
    use engine::types::statics::StaticMode;
    let mods: Vec<M> = match eff {
        H::AddAbility(body) => {
            let inner = recurse_rules(body, face, idx, ctx)?;
            if !inner.statics.is_empty() || !inner.replacements.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "HandEffect::AddAbility/static-or-replacement-in-body",
                    path: String::new(),
                    detail: format!(
                        "expected keyword/ability/trigger inner rules; got statics={} replacements={}",
                        inner.statics.len(),
                        inner.replacements.len()
                    ),
                });
            }
            let mut mods = Vec::new();
            for kw in inner.keywords {
                mods.push(M::AddKeyword { keyword: kw });
            }
            for ab in inner.abilities {
                mods.push(M::GrantAbility {
                    definition: Box::new(ab),
                });
            }
            for tr in inner.triggers {
                mods.push(M::GrantTrigger {
                    trigger: Box::new(tr),
                });
            }
            if mods.is_empty() {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "HandEffect::AddAbility/empty-body",
                    path: String::new(),
                    detail: "ability-grant body produced no modifications".into(),
                });
            }
            mods
        }
    };
    Ok(StaticDefinition::new(StaticMode::Continuous)
        .affected(affected.clone())
        .affected_zone(engine::types::zones::Zone::Hand)
        .modifications(mods))
}

/// Strict-failure on inner SpellEffects without a clean StaticMode
/// equivalent (e.g., cost modifiers, ability grants, mana-spent rules);
/// those need dedicated mappings.
fn stack_spell_effect_to_static(
    eff: &crate::schema::types::SpellEffect,
    affected: &engine::types::ability::TargetFilter,
) -> ConvResult<StaticDefinition> {
    use crate::schema::types::SpellEffect as S;
    use engine::types::statics::StaticMode;
    let mode = match eff {
        // CR 101.2: matched spells can't be countered.
        S::CantBeCountered => StaticMode::CantBeCountered,
        // CR 101.2 + CR 707.10: matched spells can't be copied.
        S::CantBeCopied => StaticMode::CantBeCopied,
        other => {
            return Err(ConversionGap::UnknownVariant {
                path: String::new(),
                repr: serde_json::to_value(other)
                    .ok()
                    .and_then(|v| {
                        v.get("_SpellEffect")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| "<unknown>".into()),
            });
        }
    };
    Ok(StaticDefinition::new(mode).affected(affected.clone()))
}

/// CR 601 + CR 608.2e: Build a two-branch spell ability. The base body
/// uses `base_effects` as the parent ability chain; the `paid_effects`
/// chain is attached as a sub_ability gated on
/// `AbilityCondition::AdditionalCostPaidInstead`, which the resolver swaps
/// in place of the parent at resolution time when the additional cost was
/// paid. This is the canonical encoding for Kicker / Overload / Madness /
/// Cleave / Gift two-branch spells.
fn build_two_branch_spell(
    base_effects: Vec<Effect>,
    paid_effects: Vec<Effect>,
) -> ConvResult<AbilityDefinition> {
    let parent = build_ability_chain(AbilityKind::Spell, None, base_effects)?;
    let mut paid = build_ability_chain(AbilityKind::Spell, None, paid_effects)?;
    paid.condition = Some(engine::types::ability::AbilityCondition::AdditionalCostPaidInstead);
    Ok(parent.sub_ability(paid))
}

/// CR 118.1: Map an activation-time `AbilityCost` to a resolution-time
/// `PaymentCost`. Reflexive triggers ("you may pay X. when you do, Y")
/// materialize the cost choice as `Effect::PayCost`, which only accepts
/// the resolution-time payment shapes (Mana / Life / Speed / Energy).
/// Non-payment costs (Tap / Sacrifice / Discard / Exile / etc.) cannot be
/// deferred to resolution — they belong on the parent's activation cost
/// slot only and strict-fail here.
fn ability_cost_to_payment_cost(
    cost: &engine::types::ability::AbilityCost,
) -> ConvResult<engine::types::ability::PaymentCost> {
    use engine::types::ability::AbilityCost as AC;
    use engine::types::ability::PaymentCost as PC;
    use engine::types::ability::QuantityExpr;
    Ok(match cost {
        AC::Mana { cost } => PC::Mana { cost: cost.clone() },
        AC::PayLife { amount } => PC::Life {
            amount: amount.clone(),
        },
        AC::PayEnergy { amount } => PC::Energy {
            amount: QuantityExpr::Fixed {
                value: *amount as i32,
            },
        },
        AC::PaySpeed { amount } => PC::Speed {
            amount: amount.clone(),
        },
        _ => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "PaymentCost",
                needed_variant: format!(
                    "AbilityCost not mappable to resolution-time PaymentCost: {cost:?}"
                ),
            });
        }
    })
}

/// Convert an mtgish `Cost` to a pure `ManaCost`, strict-failing for
/// non-mana costs (sacrifice, life payment, discard, etc. — these need
/// engine work beyond the keyword cost slot).
fn require_pure_mana(
    cost: &crate::schema::types::Cost,
    idiom: &'static str,
) -> ConvResult<engine::types::ManaCost> {
    match crate::convert::cost::as_pure_mana(cost)? {
        Some(mc) => Ok(mc),
        None => Err(ConversionGap::MalformedIdiom {
            idiom,
            path: String::new(),
            detail: "non-mana keyword cost — engine slot is ManaCost-only".into(),
        }),
    }
}

/// CR 702.152a: Map a mtgish Gift `Action` to the engine's `GiftKind`. The
/// Gift action is encoded as `Action::PlayerAction(TheGiftedPlayer, inner)`;
/// the inner action determines the kind. Unknown shapes strict-fail.
fn gift_action_to_kind(
    action: &crate::schema::types::Action,
) -> ConvResult<engine::types::keywords::GiftKind> {
    use crate::schema::types::{Action, CreatableToken};
    use engine::types::keywords::GiftKind;
    let inner = match action {
        Action::PlayerAction(_, inner) => inner.as_ref(),
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Rule::SpellActions_Gift/action_shape",
                path: String::new(),
                detail: format!(
                    "expected PlayerAction(TheGiftedPlayer, _), got {}",
                    serde_json::to_value(other)
                        .ok()
                        .and_then(|v| v.get("_Action").and_then(|t| t.as_str()).map(String::from))
                        .unwrap_or_else(|| "<unknown>".into())
                ),
            });
        }
    };
    Ok(match inner {
        Action::DrawACard => GiftKind::Card,
        Action::CreateTokens(tokens) if tokens.len() == 1 => match &tokens[0] {
            CreatableToken::TreasureToken => GiftKind::Treasure,
            CreatableToken::FoodToken => GiftKind::Food,
            CreatableToken::FishToken => GiftKind::TappedFish,
            other => {
                return Err(ConversionGap::MalformedIdiom {
                    idiom: "Rule::SpellActions_Gift/token_kind",
                    path: String::new(),
                    detail: format!("unsupported gift token: {other:?}"),
                });
            }
        },
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "Rule::SpellActions_Gift/inner_action",
                path: String::new(),
                detail: format!(
                    "unsupported gift inner action: {}",
                    serde_json::to_value(other)
                        .ok()
                        .and_then(|v| v.get("_Action").and_then(|t| t.as_str()).map(String::from))
                        .unwrap_or_else(|| "<unknown>".into())
                ),
            });
        }
    })
}

/// Pull the `_Rule` tag from a serialized variant. Cheap and avoids a
/// 475-arm match. Returns `None` only on serializer failure (shouldn't
/// happen for the vendored types).
fn variant_tag(rule: &Rule) -> Option<String> {
    let v = serde_json::to_value(rule).ok()?;
    v.get("_Rule")?.as_str().map(str::to_string)
}

/// Short JSON repr for the report. Keeps the report human-skimmable
/// without inlining megabytes of nested AST per gap.
fn truncated_repr(rule: &Rule) -> String {
    let s = serde_json::to_string(rule).unwrap_or_default();
    const MAX: usize = 240;
    if s.len() <= MAX {
        s
    } else {
        let mut t = s;
        t.truncate(MAX);
        t.push('…');
        t
    }
}

/// Map each of the 17 `OracleCard` variants to one or more faces. Faces
/// without a `rules` field (Sticker sheets) become an empty list and
/// trivially convert. Multi-face cards (Split, ModalDFC, Transforming,
/// Adventurer, Preparer, Flip, Room, Ominous) produce one face per part.
fn collect_faces(card: &OracleCard) -> Vec<Face<'_>> {
    match card {
        OracleCard::Card { rules, .. } => single("main", rules.as_deref()),
        OracleCard::MeldPiece { rules, .. } => slice("main", rules),
        OracleCard::Melded { rules, .. } => slice("melded", rules),
        OracleCard::Adventurer {
            rules, adventure, ..
        } => {
            let mut v = single("main", rules.as_deref());
            v.extend(card_face("adventure", adventure));
            v
        }
        OracleCard::Preparer {
            rules, prepared, ..
        } => {
            let mut v = single("main", rules.as_deref());
            v.extend(card_face("prepared", prepared));
            v
        }
        OracleCard::Ominous { rules, omen, .. } => {
            let mut v = slice("main", rules);
            v.extend(card_face("omen", omen));
            v
        }
        OracleCard::ModalDFC {
            front_face,
            back_face,
        } => {
            let mut v = card_face("front", front_face);
            v.extend(card_face("back", back_face));
            v
        }
        OracleCard::Transforming {
            front_face,
            back_face,
        } => {
            let mut v = card_face("front", front_face);
            v.extend(card_face("back", back_face));
            v
        }
        OracleCard::Flip {
            unflipped, flipped, ..
        } => {
            let mut v = flip_face("unflipped", unflipped);
            v.extend(flip_face("flipped", flipped));
            v
        }
        OracleCard::Room {
            left_door,
            right_door,
            ..
        } => {
            let mut v = door_face("left_door", left_door);
            v.extend(door_face("right_door", right_door));
            v
        }
        OracleCard::Split { cards } => cards
            .iter()
            .enumerate()
            .flat_map(|(i, c)| {
                let label: &'static str = match i {
                    0 => "split[0]",
                    1 => "split[1]",
                    2 => "split[2]",
                    _ => "split[n]",
                };
                card_face(label, c)
            })
            .collect(),
        OracleCard::Planar { rules, .. } => slice("main", rules),
        OracleCard::Conspiracy { rules, .. } => slice("main", rules),
        OracleCard::Scheme { rules, .. } => slice("main", rules),
        OracleCard::Dungeon { rules, .. } => slice("main", rules),
        OracleCard::Vanguard { rules, .. } => slice("main", rules),
        OracleCard::StickerSheet { .. } => Vec::new(),
    }
}

fn single<'a>(label: &'static str, rules: Option<&'a [Rule]>) -> Vec<Face<'a>> {
    rules
        .map(|r| vec![Face { label, rules: r }])
        .unwrap_or_default()
}

fn slice<'a>(label: &'static str, rules: &'a [Rule]) -> Vec<Face<'a>> {
    vec![Face { label, rules }]
}

fn card_face<'a>(label: &'static str, c: &'a Card) -> Vec<Face<'a>> {
    single(label, c.rules.as_deref())
}

fn flip_face<'a>(label: &'static str, f: &'a FlipInfo) -> Vec<Face<'a>> {
    slice(label, &f.rules)
}

fn door_face<'a>(label: &'static str, d: &'a DoorInfo) -> Vec<Face<'a>> {
    slice(label, &d.rules)
}

/// Recurse a `Vec<Rule>` body into a fresh `EngineFaceStub`. Used by the
/// `If/Unless/IfElse` wrappers so we can post-decorate the produced
/// items with the wrapper's condition.
fn recurse_rules(
    body: &[Rule],
    face: &str,
    parent_idx: usize,
    ctx: &mut Ctx,
) -> ConvResult<EngineFaceStub> {
    let mut inner = EngineFaceStub::default();
    for (j, r) in body.iter().enumerate() {
        // Use the parent index as the breadcrumb; nested If bodies share
        // their parent's slot in the report path. j only disambiguates
        // within the body when multiple rules sit under the same wrapper.
        convert_rule(r, face, parent_idx + j, &mut inner, ctx)?;
    }
    Ok(inner)
}

/// Decorate the produced items with the wrapper's condition. Dispatches
/// on the inner body's shape:
///
/// - all statics → attach a `StaticCondition` to each (composed via And
///   with any existing condition from a nested wrapper).
/// - all triggers → attach a `TriggerCondition` to each (composed via And).
///
/// Mixed bodies and ability/replacement/keyword bodies strict-fail until
/// per-context decorators land.
///
/// `negated` flips the condition (used for `Unless`).
fn apply_condition(
    wrapper: &Rule,
    cond: &crate::schema::types::Condition,
    negated: bool,
    inner: EngineFaceStub,
    stub: &mut EngineFaceStub,
) -> ConvResult<()> {
    if !inner.statics.is_empty()
        && inner.triggers.is_empty()
        && inner.abilities.is_empty()
        && inner.replacements.is_empty()
        && inner.keywords.is_empty()
    {
        let mut sc = condition::convert_static(cond)?;
        if negated {
            sc = engine::types::ability::StaticCondition::Not {
                condition: Box::new(sc),
            };
        }
        return apply_static_condition_typed(wrapper, sc, inner, stub);
    }
    if !inner.triggers.is_empty()
        && inner.statics.is_empty()
        && inner.abilities.is_empty()
        && inner.replacements.is_empty()
        && inner.keywords.is_empty()
    {
        // CR 603.4 + CR 603.6 + CR 603.10: Triggered ability with intervening-if.
        // The wrapper condition becomes (or composes with) each trigger's
        // `condition`; ETB-snapshot-derivable predicates merge into `valid_card`.
        if negated {
            // Engine TriggerCondition has no Not; fail fast.
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "TriggerCondition",
                needed_variant: "Not (negated wrapper around trigger body)".into(),
            });
        }
        let ext = condition::convert_trigger_with_etb_filter(cond)?;
        return apply_trigger_condition_typed(wrapper, ext.condition, ext.valid_card, inner, stub);
    }
    Err(ConversionGap::MalformedIdiom {
        idiom: "Rule::If/mixed-or-unsupported-body",
        path: String::new(),
        detail: format!(
            "wrapper {} body produced (statics={}, triggers={}, abilities={}, replacements={}, keywords={}) — only pure-static or pure-trigger bodies handled today",
            variant_tag(wrapper).unwrap_or_else(|| "<untagged>".into()),
            inner.statics.len(),
            inner.triggers.len(),
            inner.abilities.len(),
            inner.replacements.len(),
            inner.keywords.len(),
        ),
    })
}

fn apply_static_condition_typed(
    wrapper: &Rule,
    cond: engine::types::ability::StaticCondition,
    inner: EngineFaceStub,
    stub: &mut EngineFaceStub,
) -> ConvResult<()> {
    if inner.statics.is_empty() {
        return Err(ConversionGap::MalformedIdiom {
            idiom: "Rule::If/empty-body",
            path: String::new(),
            detail: format!(
                "wrapper {} produced no items",
                variant_tag(wrapper).unwrap_or_else(|| "<untagged>".into())
            ),
        });
    }
    for mut s in inner.statics {
        let new_cond = match s.condition.take() {
            Some(existing) => engine::types::ability::StaticCondition::And {
                conditions: vec![cond.clone(), existing],
            },
            None => cond.clone(),
        };
        s.condition = Some(new_cond);
        stub.statics.push(s);
    }
    Ok(())
}

fn apply_trigger_condition_typed(
    _wrapper: &Rule,
    cond: Option<engine::types::ability::TriggerCondition>,
    extra_valid_card: Option<engine::types::ability::TargetFilter>,
    inner: EngineFaceStub,
    stub: &mut EngineFaceStub,
) -> ConvResult<()> {
    for mut t in inner.triggers {
        if let Some(c) = &cond {
            let new_cond = match t.condition.take() {
                Some(existing) => engine::types::ability::TriggerCondition::And {
                    conditions: vec![c.clone(), existing],
                },
                None => c.clone(),
            };
            t.condition = Some(new_cond);
        }
        if let Some(vc) = &extra_valid_card {
            t.valid_card = Some(condition::merge_valid_card(t.valid_card.take(), vc.clone()));
        }
        stub.triggers.push(t);
    }
    Ok(())
}

/// CR 105.2 + CR 604.3: Map `SettableColor` (CDA_Color payload) to layer-5
/// continuous modifications. `AllColors`, `Colorless`, and a five-color
/// `SimpleColorList` collapse to `SetColor`. `Devoid` (CR 702.114a) sets the
/// object to colorless. `TheChosen{Color,Colors}` defer to the source's
/// chosen attributes.
fn convert_settable_color_to_mods(
    c: &crate::schema::types::SettableColor,
) -> ConvResult<Vec<engine::types::ability::ContinuousModification>> {
    use crate::schema::types::SettableColor as S;
    use engine::types::ability::ContinuousModification as M;
    Ok(match c {
        // CR 105.2: "is all colors" — set the object's color to all five.
        S::AllColors => vec![M::SetColor {
            colors: engine::types::ManaColor::ALL.to_vec(),
        }],
        // CR 702.114a: Devoid — the object is colorless.
        // CR 105.2c: "is colorless" — colorless is represented as an empty color list.
        S::Devoid | S::Colorless => vec![M::SetColor { colors: vec![] }],
        // CR 105.1 + CR 105.2: Static color list ("is white and blue").
        S::SimpleColorList(list) => {
            let colors = list
                .iter()
                .map(simple_color_to_mana_color)
                .collect::<Vec<_>>();
            vec![M::SetColor { colors }]
        }
        // CR 105.3 + CR 700.7: Chosen-color CDAs read from `chosen_attributes`.
        S::TheChosenColor | S::TheChosenColors => vec![M::AddChosenColor],
        // CR 700.7: "the mana color chosen this way" — engine has no
        // distinct primitive yet; surface as engine prerequisite.
        S::TheManaColorChosenThisWay => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ContinuousModification",
                needed_variant: "AddChosenColor (TheManaColorChosenThisWay variant)".into(),
            });
        }
    })
}

/// CR 205.3 + CR 604.3: Map `CDA_Types` to layer-4 type-defining
/// modifications. Static subtype lists become a `Vec<AddSubtype>`;
/// Changeling and `HasAllCreatureTypes` become `AddAllCreatureTypes`;
/// `AddCreatureTypeVariable(TheChosen…)` defers to `chosen_attributes`.
fn convert_cda_types_to_mods(
    t: &crate::schema::types::CDA_Types,
) -> ConvResult<Vec<engine::types::ability::ContinuousModification>> {
    use crate::schema::types::{CDA_Types as T, CreatureTypeVariable as V};
    use engine::types::ability::{ChosenSubtypeKind, ContinuousModification as M};
    Ok(match t {
        // CR 702.73a + CR 205.3: Changeling — has every creature type.
        T::Changeling | T::HasAllCreatureTypes => vec![M::AddAllCreatureTypes],
        // CR 205.3: "is also a [type1] [type2]" — one AddSubtype per listed type.
        T::AddCreatureTypes(types) => types
            .iter()
            .map(|ct| M::AddSubtype {
                subtype: creature_type_name(ct),
            })
            .collect(),
        // CR 700.7 + CR 205.3: chosen-creature-type CDA.
        T::AddCreatureTypeVariable(V::TheChosenCreatureType)
        | T::AddCreatureTypeVariable(V::TheChosenCreatureTypes) => {
            vec![M::AddChosenSubtype {
                kind: ChosenSubtypeKind::CreatureType,
            }]
        }
        // Other variable kinds (CreatureTypesOfExiled, TheNotedCreatureType)
        // require additional engine plumbing; surface as a gap.
        T::AddCreatureTypeVariable(other) => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ContinuousModification",
                needed_variant: format!("AddCreatureTypeVariable({other:?})"),
            });
        }
        // "Has all nonbasic land types" — no current engine primitive.
        T::HasAllNonbasicLandTypes => {
            return Err(ConversionGap::EnginePrerequisiteMissing {
                engine_type: "ContinuousModification",
                needed_variant: "AddAllNonbasicLandTypes".into(),
            });
        }
    })
}

fn simple_color_to_mana_color(c: &crate::schema::types::SimpleColor) -> engine::types::ManaColor {
    use crate::schema::types::SimpleColor as C;
    use engine::types::ManaColor as MC;
    match c {
        C::White => MC::White,
        C::Blue => MC::Blue,
        C::Black => MC::Black,
        C::Red => MC::Red,
        C::Green => MC::Green,
    }
}

/// `CreatureType` is an externally-tagged enum where the variant name is the
/// canonical subtype string ("Goblin", "Wizard", ...). Use serde to extract it.
fn creature_type_name(ct: &crate::schema::types::CreatureType) -> String {
    serde_json::to_value(ct)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{ct:?}"))
}
