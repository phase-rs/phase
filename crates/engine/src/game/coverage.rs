use crate::database::legality::LegalityFormat;
use crate::database::CardDatabase;
use crate::game::game_object::GameObject;
use crate::game::static_abilities::{build_static_registry, StaticAbilityHandler};
use crate::game::triggers::build_trigger_registry;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, ControllerRef, DamageAmount,
    Duration, Effect, FilterProp, PtValue, QuantityExpr, ReplacementDefinition, ReplacementMode,
    StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
};
use crate::types::card::CardFace;
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// A lightweight node in the parse tree for a single card, representing one
/// parsed item (keyword, ability, trigger, static, or replacement) with its
/// support status and any nested children (sub-abilities, modal modes, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedItem {
    /// Category of the parsed item.
    pub category: ParseCategory,
    /// Human-readable label (e.g. "DealDamage", "Flying", "ChangesZone").
    pub label: String,
    /// Original Oracle text fragment that produced this item, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_text: Option<String>,
    /// Whether this specific item is supported by the engine.
    pub supported: bool,
    /// Key-value pairs of parsed parameters (e.g., target, amount, zone).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub details: Vec<(String, String)>,
    /// Nested items (sub-abilities, modal choices, composite costs).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<ParsedItem>,
}

/// The category of a parsed item in the coverage tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseCategory {
    Keyword,
    Ability,
    Trigger,
    Static,
    Replacement,
    Cost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardCoverageResult {
    pub card_name: String,
    pub set_code: String,
    pub supported: bool,
    pub missing_handlers: Vec<String>,
    /// Original Oracle text for the card face.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle_text: Option<String>,
    /// Hierarchical parse tree showing what each piece of Oracle text was parsed into.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parse_details: Vec<ParsedItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub total_cards: usize,
    pub supported_cards: usize,
    pub coverage_pct: f64,
    #[serde(default)]
    pub coverage_by_format: BTreeMap<String, FormatCoverageSummary>,
    pub cards: Vec<CardCoverageResult>,
    pub missing_handler_frequency: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FormatCoverageSummary {
    pub total_cards: usize,
    pub supported_cards: usize,
    pub coverage_pct: f64,
}

/// Extract the effect variant name (e.g. "DealDamage", "Draw", "Unimplemented")
/// by serializing to JSON and reading the serde `type` tag.
fn effect_type_name(effect: &Effect) -> String {
    serde_json::to_value(effect)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_else(|| "Unknown".to_string())
}

// ---------------------------------------------------------------------------
// Detail formatters — extract human-readable parameter summaries
// ---------------------------------------------------------------------------

fn fmt_target(filter: &TargetFilter) -> String {
    match filter {
        TargetFilter::None => "none".into(),
        TargetFilter::Any => "any target".into(),
        TargetFilter::Player => "player".into(),
        TargetFilter::Controller => "controller".into(),
        TargetFilter::SelfRef => "self".into(),
        TargetFilter::StackAbility => "ability on stack".into(),
        TargetFilter::AttachedTo => "attached permanent".into(),
        TargetFilter::LastCreated => "last created".into(),
        TargetFilter::TriggeringSpellController => "triggering spell's controller".into(),
        TargetFilter::TriggeringSpellOwner => "triggering spell's owner".into(),
        TargetFilter::TriggeringPlayer => "triggering player".into(),
        TargetFilter::TriggeringSource => "triggering source".into(),
        TargetFilter::SpecificObject { id } => format!("object #{}", id.0),
        TargetFilter::TrackedSet { id } => format!("tracked set #{}", id.0),
        TargetFilter::Not { filter } => format!("not {}", fmt_target(filter)),
        TargetFilter::Or { filters } => filters
            .iter()
            .map(fmt_target)
            .collect::<Vec<_>>()
            .join(" or "),
        TargetFilter::And { filters } => filters
            .iter()
            .map(fmt_target)
            .collect::<Vec<_>>()
            .join(" + "),
        TargetFilter::Typed(tf) => fmt_typed_filter(tf),
    }
}

fn fmt_typed_filter(tf: &TypedFilter) -> String {
    let mut parts = Vec::new();
    for prop in &tf.properties {
        match prop {
            FilterProp::Token => parts.push("token".into()),
            FilterProp::Attacking => parts.push("attacking".into()),
            FilterProp::Tapped => parts.push("tapped".into()),
            FilterProp::NonType { value } => parts.push(format!("non-{value}")),
            FilterProp::WithKeyword { value } => parts.push(format!("with {value}")),
            FilterProp::CountersGE {
                counter_type,
                count,
            } => parts.push(format!("{count}+ {counter_type} counters")),
            FilterProp::CmcGE { value } => parts.push(format!("mv {value}+")),
            FilterProp::CmcLE { value } => parts.push(format!("mv {value}-")),
            FilterProp::InZone { zone } => parts.push(format!("in {zone:?}")),
            FilterProp::Owned { controller } => parts.push(fmt_controller(controller)),
            FilterProp::EnchantedBy => parts.push("enchanted by self".into()),
            FilterProp::EquippedBy => parts.push("equipped by self".into()),
            FilterProp::Another => parts.push("another".into()),
            FilterProp::HasColor { color } => parts.push(color.clone()),
            FilterProp::PowerLE { value } => parts.push(format!("power ≤{value}")),
            FilterProp::PowerGE { value } => parts.push(format!("power ≥{value}")),
            FilterProp::Multicolored => parts.push("multicolored".into()),
            FilterProp::HasSupertype { value } => parts.push(value.to_lowercase()),
            FilterProp::IsChosenCreatureType => parts.push("chosen type".into()),
            FilterProp::Suspected => parts.push("suspected".into()),
            FilterProp::Other { value } => parts.push(value.clone()),
        }
    }
    if let Some(ctrl) = &tf.controller {
        parts.push(fmt_controller(ctrl));
    }
    if let Some(sub) = &tf.subtype {
        parts.push(sub.clone());
    }
    let type_str = tf
        .card_type
        .as_ref()
        .map(fmt_type_filter)
        .unwrap_or_default();
    if parts.is_empty() {
        if type_str.is_empty() {
            "any".into()
        } else {
            type_str
        }
    } else {
        let props = parts.join(" ");
        if type_str.is_empty() {
            props
        } else {
            format!("{props} {type_str}")
        }
    }
}

fn fmt_type_filter(tf: &TypeFilter) -> String {
    match tf {
        TypeFilter::Creature => "creature",
        TypeFilter::Land => "land",
        TypeFilter::Artifact => "artifact",
        TypeFilter::Enchantment => "enchantment",
        TypeFilter::Instant => "instant",
        TypeFilter::Sorcery => "sorcery",
        TypeFilter::Planeswalker => "planeswalker",
        TypeFilter::Permanent => "permanent",
        TypeFilter::Card => "card",
        TypeFilter::Any => "any",
    }
    .into()
}

fn fmt_controller(ctrl: &ControllerRef) -> String {
    match ctrl {
        ControllerRef::You => "you control",
        ControllerRef::Opponent => "opponent controls",
    }
    .into()
}

fn fmt_pt(p: &PtValue) -> String {
    match p {
        PtValue::Fixed(n) => format!("{n:+}"),
        PtValue::Variable(s) => format!("+{s}"),
    }
}

fn fmt_damage_amount(a: &DamageAmount) -> String {
    match a {
        DamageAmount::Fixed(n) => n.to_string(),
        DamageAmount::Variable(s) => s.clone(),
    }
}

fn fmt_quantity(q: &QuantityExpr) -> String {
    match q {
        QuantityExpr::Fixed { value } => value.to_string(),
        QuantityExpr::Ref { qty } => format!("{qty:?}"),
    }
}

fn fmt_duration(d: &Duration) -> String {
    match d {
        Duration::UntilEndOfTurn => "until end of turn",
        Duration::UntilYourNextTurn => "until your next turn",
        Duration::UntilHostLeavesPlay => "while on battlefield",
        Duration::Permanent => "permanent",
    }
    .into()
}

fn fmt_zone(z: &Zone) -> String {
    format!("{z:?}")
}

/// Extract key-value detail pairs from an `Effect`'s parameters.
fn effect_details(effect: &Effect) -> Vec<(String, String)> {
    let mut d = Vec::new();
    match effect {
        Effect::DealDamage { amount, target } => {
            d.push(("amount".into(), fmt_quantity(amount)));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Draw { count } => {
            if !matches!(count, QuantityExpr::Fixed { value: 1 }) {
                d.push(("count".into(), fmt_quantity(count)));
            }
        }
        Effect::Pump {
            power,
            toughness,
            target,
        } => {
            d.push((
                "p/t".into(),
                format!("{}/{}", fmt_pt(power), fmt_pt(toughness)),
            ));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::PumpAll {
            power,
            toughness,
            target,
        } => {
            d.push((
                "p/t".into(),
                format!("{}/{}", fmt_pt(power), fmt_pt(toughness)),
            ));
            if !matches!(target, TargetFilter::None) {
                d.push(("filter".into(), fmt_target(target)));
            }
        }
        Effect::Destroy { target }
        | Effect::Tap { target }
        | Effect::Untap { target }
        | Effect::Sacrifice { target }
        | Effect::GainControl { target }
        | Effect::Attach { target }
        | Effect::Fight { target }
        | Effect::CopySpell { target }
        | Effect::Suspect { target }
        | Effect::Transform { target }
        | Effect::Shuffle { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::DestroyAll { target } | Effect::DamageAll { amount: _, target } => {
            if !matches!(target, TargetFilter::None) {
                d.push(("filter".into(), fmt_target(target)));
            }
            if let Effect::DamageAll { amount, .. } = effect {
                d.push(("amount".into(), fmt_damage_amount(amount)));
            }
        }
        Effect::Counter {
            target,
            source_static,
            ..
        } => {
            d.push(("target".into(), fmt_target(target)));
            if source_static.is_some() {
                d.push(("+ static".into(), "on source".into()));
            }
        }
        Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            keywords,
            count,
            tapped,
        } => {
            let mut desc = String::new();
            match count {
                crate::types::ability::CountValue::Fixed(n) if *n != 1 => {
                    desc.push_str(&format!("{n}× "));
                }
                crate::types::ability::CountValue::Variable(v) => {
                    desc.push_str(&format!("{v}× "));
                }
                _ => {}
            }
            desc.push_str(&format!("{}/{} ", fmt_pt(power), fmt_pt(toughness)));
            if !colors.is_empty() {
                let c: Vec<_> = colors.iter().map(|c| format!("{c:?}")).collect();
                desc.push_str(&c.join("/"));
                desc.push(' ');
            }
            desc.push_str(name);
            if !types.is_empty() {
                desc.push_str(&format!(" ({})", types.join(" ")));
            }
            if !keywords.is_empty() {
                let kws: Vec<_> = keywords.iter().map(|k| keyword_label(k)).collect();
                desc.push_str(&format!(" with {}", kws.join(", ")));
            }
            if *tapped {
                desc.push_str(" tapped");
            }
            d.push(("token".into(), desc));
        }
        Effect::AddCounter {
            counter_type,
            count,
            target,
        }
        | Effect::RemoveCounter {
            counter_type,
            count,
            target,
        } => {
            d.push(("counter".into(), format!("{count} {counter_type}")));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::PutCounter {
            counter_type,
            count,
            target,
        } => {
            d.push(("counter".into(), format!("{count} {counter_type}")));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::MultiplyCounter {
            counter_type,
            multiplier,
            target,
        } => {
            d.push(("counter".into(), format!("{counter_type} ×{multiplier}")));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::DiscardCard { count, target } | Effect::Discard { count, target } => {
            if *count != 1 {
                d.push(("count".into(), count.to_string()));
            }
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Mill { count, target } => {
            d.push(("count".into(), fmt_quantity(count)));
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Scry { count } | Effect::Surveil { count } => {
            d.push(("count".into(), count.to_string()));
        }
        Effect::GainLife { amount, player } => {
            d.push(("amount".into(), fmt_quantity(amount)));
            if !matches!(player, crate::types::ability::GainLifePlayer::Controller) {
                d.push(("player".into(), format!("{player:?}")));
            }
        }
        Effect::LoseLife { amount } => {
            d.push(("amount".into(), fmt_quantity(amount)));
        }
        Effect::ChangeZone {
            origin,
            destination,
            target,
        }
        | Effect::ChangeZoneAll {
            origin,
            destination,
            target,
        } => {
            if let Some(o) = origin {
                d.push(("from".into(), fmt_zone(o)));
            }
            d.push(("to".into(), fmt_zone(destination)));
            if !matches!(target, TargetFilter::None) {
                d.push(("target".into(), fmt_target(target)));
            }
        }
        Effect::Dig { count, destination } => {
            d.push(("count".into(), count.to_string()));
            if let Some(dest) = destination {
                d.push(("to".into(), fmt_zone(dest)));
            }
        }
        Effect::Bounce {
            target,
            destination,
        } => {
            d.push(("target".into(), fmt_target(target)));
            if let Some(dest) = destination {
                d.push(("to".into(), fmt_zone(dest)));
            }
        }
        Effect::SearchLibrary {
            filter,
            count,
            reveal,
        } => {
            d.push(("find".into(), fmt_target(filter)));
            if *count != 1 {
                d.push(("count".into(), count.to_string()));
            }
            if *reveal {
                d.push(("reveal".into(), "yes".into()));
            }
        }
        Effect::Animate {
            power,
            toughness,
            types,
            target,
        } => {
            if let (Some(p), Some(t)) = (power, toughness) {
                d.push(("p/t".into(), format!("{p}/{t}")));
            }
            if !types.is_empty() {
                d.push(("types".into(), types.join(" ")));
            }
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::Choose {
            choice_type,
            persist,
        } => {
            d.push(("choice".into(), format!("{choice_type:?}")));
            if *persist {
                d.push(("persist".into(), "yes".into()));
            }
        }
        Effect::Mana { produced, .. } => {
            d.push(("mana".into(), format!("{produced:?}")));
        }
        Effect::RevealHand {
            target,
            card_filter,
        } => {
            d.push(("player".into(), fmt_target(target)));
            if !matches!(card_filter, TargetFilter::Any) {
                d.push(("card filter".into(), fmt_target(card_filter)));
            }
        }
        Effect::TargetOnly { target } => {
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::ChooseCard { choices, target } => {
            if !choices.is_empty() {
                d.push(("choices".into(), choices.join(", ")));
            }
            d.push(("target".into(), fmt_target(target)));
        }
        Effect::CreateDelayedTrigger {
            condition,
            uses_tracked_set,
            ..
        } => {
            d.push(("when".into(), format!("{condition:?}")));
            if *uses_tracked_set {
                d.push(("tracked".into(), "yes".into()));
            }
        }
        Effect::GenericEffect {
            duration, target, ..
        } => {
            if let Some(dur) = duration {
                d.push(("duration".into(), fmt_duration(dur)));
            }
            if let Some(t) = target {
                d.push(("target".into(), fmt_target(t)));
            }
        }
        Effect::Unimplemented { .. }
        | Effect::Explore
        | Effect::Proliferate
        | Effect::SolveCase
        | Effect::Cleanup { .. }
        | Effect::AddRestriction { .. }
        | Effect::CreateEmblem { .. } => {}
    }
    d
}

/// Extract detail pairs from an `AbilityDefinition` (non-effect fields).
fn ability_details(def: &AbilityDefinition) -> Vec<(String, String)> {
    let mut d = Vec::new();
    if def.kind != AbilityKind::Spell {
        d.push(("kind".into(), format!("{:?}", def.kind)));
    }
    if let Some(dur) = &def.duration {
        d.push(("duration".into(), fmt_duration(dur)));
    }
    if def.optional_targeting {
        d.push(("targeting".into(), "optional (up to)".into()));
    }
    if let Some(mt) = &def.multi_target {
        d.push(("targets".into(), match mt.max {
            Some(max) => format!("{}-{}", mt.min, max),
            None => format!("{}+", mt.min),
        }));
    }
    if def.condition.is_some() {
        d.push(("conditional".into(), "yes".into()));
    }
    if def.sorcery_speed {
        d.push(("timing".into(), "sorcery speed".into()));
    }
    if let Some(modal) = &def.modal {
        d.push((
            "modal".into(),
            format!(
                "choose {}-{} of {}",
                modal.min_choices, modal.max_choices, modal.mode_count
            ),
        ));
    }
    d
}

/// Extract detail pairs from a `TriggerDefinition` (non-effect fields).
fn trigger_details(trig: &TriggerDefinition) -> Vec<(String, String)> {
    let mut d = Vec::new();
    if let Some(vc) = &trig.valid_card {
        d.push(("watches".into(), fmt_target(vc)));
    }
    if let Some(origin) = &trig.origin {
        d.push(("from".into(), fmt_zone(origin)));
    }
    if let Some(dest) = &trig.destination {
        d.push(("to".into(), fmt_zone(dest)));
    }
    if !trig.trigger_zones.is_empty() {
        let zones: Vec<_> = trig.trigger_zones.iter().map(fmt_zone).collect();
        d.push(("active in".into(), zones.join(", ")));
    }
    if let Some(phase) = &trig.phase {
        d.push(("phase".into(), format!("{phase:?}")));
    }
    if trig.optional {
        d.push(("optional".into(), "yes".into()));
    }
    if trig.combat_damage {
        d.push(("combat damage".into(), "yes".into()));
    }
    if let Some(vt) = &trig.valid_target {
        d.push(("valid target".into(), fmt_target(vt)));
    }
    if let Some(vs) = &trig.valid_source {
        d.push(("valid source".into(), fmt_target(vs)));
    }
    if trig.constraint.is_some() {
        d.push(("constraint".into(), "yes".into()));
    }
    if trig.condition.is_some() {
        d.push(("condition".into(), "yes".into()));
    }
    d
}

/// Extract detail pairs from a `StaticDefinition`.
fn static_details(stat: &StaticDefinition) -> Vec<(String, String)> {
    let mut d = Vec::new();
    if let Some(affected) = &stat.affected {
        d.push(("affects".into(), fmt_target(affected)));
    }
    if !stat.modifications.is_empty() {
        d.push(("modifications".into(), stat.modifications.len().to_string()));
    }
    if stat.condition.is_some() {
        d.push(("conditional".into(), "yes".into()));
    }
    if stat.characteristic_defining {
        d.push(("CDA".into(), "yes".into()));
    }
    if let Some(zone) = &stat.affected_zone {
        d.push(("zone".into(), fmt_zone(zone)));
    }
    d
}

/// Extract a human-readable label for a keyword.
fn keyword_label(kw: &Keyword) -> String {
    serde_json::to_value(kw)
        .ok()
        .and_then(|v| match &v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(map) => map.keys().next().cloned(),
            _ => None,
        })
        .unwrap_or_else(|| format!("{kw:?}"))
}

/// Build a hierarchical parse tree from a `CardFace`, checking each item against
/// the engine's trigger and static registries for support status.
pub fn build_parse_details(
    face: &CardFace,
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
    static_registry: &HashMap<StaticMode, StaticAbilityHandler>,
) -> Vec<ParsedItem> {
    let mut items = Vec::new();

    // Keywords
    for kw in &face.keywords {
        items.push(ParsedItem {
            category: ParseCategory::Keyword,
            label: keyword_label(kw),
            source_text: None,
            supported: !matches!(kw, Keyword::Unknown(_)),
            details: vec![],
            children: vec![],
        });
    }

    // Activated/spell abilities
    for def in &face.abilities {
        items.push(build_ability_item(def));
    }

    // Triggers
    for trig in &face.triggers {
        let mode_supported = !matches!(&trig.mode, TriggerMode::Unknown(_))
            && trigger_registry.contains_key(&trig.mode);
        let mut children = Vec::new();
        if let Some(execute) = &trig.execute {
            children.push(build_ability_item(execute));
        }
        items.push(ParsedItem {
            category: ParseCategory::Trigger,
            label: format!("{}", trig.mode),
            source_text: trig.description.clone(),
            supported: mode_supported && children.iter().all(|c| c.is_fully_supported()),
            details: trigger_details(trig),
            children,
        });
    }

    // Static abilities
    for stat in &face.static_abilities {
        items.push(ParsedItem {
            category: ParseCategory::Static,
            label: format!("{}", stat.mode),
            source_text: stat.description.clone(),
            supported: static_registry.contains_key(&stat.mode),
            details: static_details(stat),
            children: vec![],
        });
    }

    // Replacement effects
    for repl in &face.replacements {
        let mut children = Vec::new();
        let mut execute_supported = true;
        if let Some(execute) = &repl.execute {
            let item = build_ability_item(execute);
            execute_supported = item.is_fully_supported();
            children.push(item);
        }
        if let ReplacementMode::Optional {
            decline: Some(decline),
        } = &repl.mode
        {
            let item = build_ability_item(decline);
            if !item.is_fully_supported() {
                execute_supported = false;
            }
            children.push(item);
        }
        items.push(ParsedItem {
            category: ParseCategory::Replacement,
            label: format!("{}", repl.event),
            source_text: repl.description.clone(),
            supported: execute_supported,
            details: vec![],
            children,
        });
    }

    // Additional cost
    if let Some(additional_cost) = &face.additional_cost {
        build_additional_cost_items(additional_cost, &mut items);
    }

    items
}

/// Build a `ParsedItem` for a single `AbilityDefinition`, recursing into
/// sub-abilities and modal abilities.
fn build_ability_item(def: &AbilityDefinition) -> ParsedItem {
    let label = effect_type_name(&def.effect);
    let supported = !matches!(&def.effect, Effect::Unimplemented { .. });
    let source_text = def.description.clone().or_else(|| match &def.effect {
        Effect::Unimplemented { description, .. } => description.clone(),
        _ => None,
    });

    let mut details = effect_details(&def.effect);
    details.extend(ability_details(def));

    let mut children = Vec::new();

    // Cost
    if let Some(cost) = &def.cost {
        build_cost_item(cost, &mut children);
    }

    // Sub-ability chain
    if let Some(sub) = &def.sub_ability {
        children.push(build_ability_item(sub));
    }

    // Modal abilities
    for mode_ability in &def.mode_abilities {
        children.push(build_ability_item(mode_ability));
    }

    ParsedItem {
        category: ParseCategory::Ability,
        label,
        source_text,
        supported,
        details,
        children,
    }
}

/// Build `ParsedItem` nodes for ability costs, only emitting items for
/// composite or unimplemented costs (simple costs are not interesting).
fn build_cost_item(cost: &AbilityCost, items: &mut Vec<ParsedItem>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for nested in costs {
                build_cost_item(nested, items);
            }
        }
        AbilityCost::Unimplemented { description } => {
            items.push(ParsedItem {
                category: ParseCategory::Cost,
                label: description.clone(),
                source_text: Some(description.clone()),
                supported: false,
                details: vec![],
                children: vec![],
            });
        }
        _ => {}
    }
}

/// Build `ParsedItem` nodes for additional costs (kicker, etc.).
fn build_additional_cost_items(additional_cost: &AdditionalCost, items: &mut Vec<ParsedItem>) {
    match additional_cost {
        AdditionalCost::Optional(cost) => build_cost_item(cost, items),
        AdditionalCost::Choice(first, second) => {
            build_cost_item(first, items);
            build_cost_item(second, items);
        }
    }
}

impl ParsedItem {
    /// Returns true if this item and all its children are supported.
    pub fn is_fully_supported(&self) -> bool {
        self.supported && self.children.iter().all(ParsedItem::is_fully_supported)
    }
}

/// Check whether a game object has any mechanics the engine cannot handle.
///
/// Checks keywords (Unknown variant = unrecognized), abilities (api_type
/// not in effect registry), triggers (mode not in trigger registry), and
/// static abilities (mode not in static registry).
pub fn unimplemented_mechanics(obj: &GameObject) -> Vec<String> {
    let mut missing = Vec::new();

    // 1. Any Unknown keyword means the parser didn't recognize it
    for kw in &obj.keywords {
        if let Keyword::Unknown(s) = kw {
            missing.push(format!("Keyword: {s}"));
        }
    }

    // 2. Check abilities against known effect types
    for def in &obj.abilities {
        if let Effect::Unimplemented { name, .. } = &def.effect {
            missing.push(format!("Effect: {name}"));
        }
    }

    // 3. Check trigger modes against trigger registry
    let trigger_registry = build_trigger_registry();
    for trig in &obj.trigger_definitions {
        if matches!(&trig.mode, TriggerMode::Unknown(_))
            || !trigger_registry.contains_key(&trig.mode)
        {
            missing.push(format!("Trigger: {}", trig.mode));
        }
    }

    // 4. Check static ability modes against static registry
    let static_registry = build_static_registry();
    for stat in &obj.static_definitions {
        if !static_registry.contains_key(&stat.mode) {
            missing.push(format!("Static: {}", stat.mode));
        }
    }

    missing
}

/// Analyze card coverage by checking which cards have all their abilities,
/// triggers, keywords, and static abilities supported by the engine's registries.
pub fn analyze_coverage(card_db: &CardDatabase) -> CoverageSummary {
    let trigger_registry = build_trigger_registry();
    let static_registry = build_static_registry();

    let mut cards = Vec::new();
    let mut freq: HashMap<String, usize> = HashMap::new();
    let mut coverage_by_format_accumulators: BTreeMap<String, (usize, usize)> = LegalityFormat::ALL
        .into_iter()
        .map(|format| (format.as_key().to_string(), (0, 0)))
        .collect();

    for (key, face) in card_db.face_iter() {
        let mut missing = Vec::new();

        // Check abilities
        check_abilities(&face.abilities, &mut missing);

        // Check additional cost
        check_additional_cost(&face.additional_cost, &mut missing);

        // Check triggers
        check_triggers(&face.triggers, &trigger_registry, &mut missing);

        // Check keywords
        check_keywords(&face.keywords, &mut missing);

        // Check static abilities
        check_statics(&face.static_abilities, &static_registry, &mut missing);

        // Check replacements
        check_replacements(&face.replacements, &mut missing);

        let supported = missing.is_empty();

        for m in &missing {
            *freq.entry(m.clone()).or_default() += 1;
        }

        for format in LegalityFormat::ALL {
            if card_db
                .legality_status(key, format)
                .is_some_and(|status| status.is_legal())
            {
                let entry = coverage_by_format_accumulators
                    .get_mut(format.as_key())
                    .expect("all legality formats must be pre-seeded");
                entry.0 += 1;
                if supported {
                    entry.1 += 1;
                }
            }
        }

        let parse_details = build_parse_details(face, &trigger_registry, &static_registry);

        cards.push(CardCoverageResult {
            card_name: face.name.clone(),
            set_code: String::new(),
            supported,
            missing_handlers: missing,
            oracle_text: face.oracle_text.clone(),
            parse_details,
        });
    }

    let total_cards = cards.len();
    let supported_cards = cards.iter().filter(|c| c.supported).count();
    let coverage_pct = if total_cards > 0 {
        (supported_cards as f64 / total_cards as f64) * 100.0
    } else {
        0.0
    };

    let mut missing_handler_frequency: Vec<(String, usize)> = freq.into_iter().collect();
    missing_handler_frequency.sort_by(|a, b| b.1.cmp(&a.1));

    let coverage_by_format = coverage_by_format_accumulators
        .into_iter()
        .map(|(format, (total_cards, supported_cards))| {
            let coverage_pct = if total_cards > 0 {
                (supported_cards as f64 / total_cards as f64) * 100.0
            } else {
                0.0
            };
            (
                format,
                FormatCoverageSummary {
                    total_cards,
                    supported_cards,
                    coverage_pct,
                },
            )
        })
        .collect();

    CoverageSummary {
        total_cards,
        supported_cards,
        coverage_pct,
        coverage_by_format,
        cards,
        missing_handler_frequency,
    }
}

pub fn card_face_has_unimplemented_parts(face: &CardFace) -> bool {
    ability_definitions_have_unimplemented_parts(&face.abilities)
        || face
            .additional_cost
            .as_ref()
            .is_some_and(additional_cost_has_unimplemented_parts)
        || face.triggers.iter().any(trigger_has_unimplemented_parts)
        || face
            .replacements
            .iter()
            .any(replacement_has_unimplemented_parts)
}

fn check_abilities(abilities: &[AbilityDefinition], missing: &mut Vec<String>) {
    for def in abilities {
        collect_ability_missing_parts(def, missing);
    }
}

fn check_triggers(
    triggers: &[TriggerDefinition],
    trigger_registry: &HashMap<TriggerMode, crate::game::triggers::TriggerMatcher>,
    missing: &mut Vec<String>,
) {
    for def in triggers {
        if let Some(execute) = &def.execute {
            collect_ability_missing_parts(execute, missing);
        }
        if matches!(&def.mode, TriggerMode::Unknown(_)) || !trigger_registry.contains_key(&def.mode)
        {
            let label = format!("Trigger:{}", def.mode);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_keywords(keywords: &[Keyword], missing: &mut Vec<String>) {
    for kw in keywords {
        if let Keyword::Unknown(s) = kw {
            let label = format!("Keyword:{}", s);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_additional_cost(additional_cost: &Option<AdditionalCost>, missing: &mut Vec<String>) {
    if let Some(additional_cost) = additional_cost {
        collect_additional_cost_missing_parts(additional_cost, missing);
    }
}

fn check_statics(
    statics: &[StaticDefinition],
    static_registry: &HashMap<StaticMode, StaticAbilityHandler>,
    missing: &mut Vec<String>,
) {
    for def in statics {
        if !static_registry.contains_key(&def.mode) {
            let label = format!("Static:{}", def.mode);
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
    }
}

fn check_replacements(replacements: &[ReplacementDefinition], missing: &mut Vec<String>) {
    for def in replacements {
        if let Some(execute) = &def.execute {
            collect_ability_missing_parts(execute, missing);
        }

        if let ReplacementMode::Optional {
            decline: Some(decline),
        } = &def.mode
        {
            collect_ability_missing_parts(decline, missing);
        }
    }
}

fn ability_definitions_have_unimplemented_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities
        .iter()
        .any(ability_definition_has_unimplemented_parts)
}

fn trigger_has_unimplemented_parts(trigger: &TriggerDefinition) -> bool {
    trigger
        .execute
        .as_ref()
        .is_some_and(|execute| ability_definition_has_unimplemented_parts(execute))
}

fn replacement_has_unimplemented_parts(replacement: &ReplacementDefinition) -> bool {
    replacement
        .execute
        .as_ref()
        .is_some_and(|execute| ability_definition_has_unimplemented_parts(execute))
        || matches!(
            &replacement.mode,
            ReplacementMode::Optional {
                decline: Some(decline),
            } if ability_definition_has_unimplemented_parts(decline)
        )
}

fn ability_definition_has_unimplemented_parts(def: &AbilityDefinition) -> bool {
    matches!(def.effect, Effect::Unimplemented { .. })
        || def
            .cost
            .as_ref()
            .is_some_and(ability_cost_has_unimplemented_parts)
        || def
            .sub_ability
            .as_ref()
            .is_some_and(|sub| ability_definition_has_unimplemented_parts(sub))
        || def
            .mode_abilities
            .iter()
            .any(ability_definition_has_unimplemented_parts)
}

fn additional_cost_has_unimplemented_parts(additional_cost: &AdditionalCost) -> bool {
    match additional_cost {
        AdditionalCost::Optional(cost) => ability_cost_has_unimplemented_parts(cost),
        AdditionalCost::Choice(first, second) => {
            ability_cost_has_unimplemented_parts(first)
                || ability_cost_has_unimplemented_parts(second)
        }
    }
}

fn ability_cost_has_unimplemented_parts(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Composite { costs } => costs.iter().any(ability_cost_has_unimplemented_parts),
        AbilityCost::Unimplemented { .. } => true,
        _ => false,
    }
}

fn collect_ability_missing_parts(def: &AbilityDefinition, missing: &mut Vec<String>) {
    if let Effect::Unimplemented { name, .. } = &def.effect {
        let label = format!("Effect:{name}");
        if !missing.contains(&label) {
            missing.push(label);
        }
    }

    if let Some(cost) = &def.cost {
        collect_ability_cost_missing_parts(cost, missing);
    }

    if let Some(sub_ability) = &def.sub_ability {
        collect_ability_missing_parts(sub_ability, missing);
    }

    for mode_ability in &def.mode_abilities {
        collect_ability_missing_parts(mode_ability, missing);
    }
}

fn collect_additional_cost_missing_parts(
    additional_cost: &AdditionalCost,
    missing: &mut Vec<String>,
) {
    match additional_cost {
        AdditionalCost::Optional(cost) => collect_ability_cost_missing_parts(cost, missing),
        AdditionalCost::Choice(first, second) => {
            collect_ability_cost_missing_parts(first, missing);
            collect_ability_cost_missing_parts(second, missing);
        }
    }
}

fn collect_ability_cost_missing_parts(cost: &AbilityCost, missing: &mut Vec<String>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for nested_cost in costs {
                collect_ability_cost_missing_parts(nested_cost, missing);
            }
        }
        AbilityCost::Unimplemented { description } => {
            let label = format!("Cost:{description}");
            if !missing.contains(&label) {
                missing.push(label);
            }
        }
        _ => {}
    }
}

/// Returns `true` if the coverage summary shows 100% support (CI pass).
/// Returns `false` if there are any unsupported cards (CI fail).
pub fn is_fully_covered(summary: &CoverageSummary) -> bool {
    summary.total_cards > 0 && summary.supported_cards == summary.total_cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::legality::{legalities_to_export_map, LegalityStatus};
    use crate::types::ability::{AbilityKind, DamageAmount, Effect, TargetFilter};
    use crate::types::card_type::CardType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn make_obj() -> GameObject {
        GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test Card".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn vanilla_object_has_no_unimplemented_mechanics() {
        let obj = make_obj();
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_known_keyword_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_unknown_keyword_has_unimplemented() {
        let mut obj = make_obj();
        obj.keywords
            .push(Keyword::Unknown("FutureKeyword".to_string()));
        assert!(!unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_registered_ability_has_no_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: TargetFilter::Any,
                },
            ));
        assert!(unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn object_with_unregistered_ability_has_unimplemented() {
        let mut obj = make_obj();
        obj.abilities
            .push(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Fateseal".to_string(),
                    description: None,
                },
            ));
        assert!(!unimplemented_mechanics(&obj).is_empty());
    }

    #[test]
    fn has_unimplemented_via_game_object_method() {
        let mut obj = make_obj();
        assert!(!obj.has_unimplemented_mechanics());
        obj.keywords.push(Keyword::Unknown("Bogus".to_string()));
        assert!(obj.has_unimplemented_mechanics());
    }

    fn make_face() -> CardFace {
        CardFace {
            name: "Test Card".to_string(),
            mana_cost: Default::default(),
            card_type: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
        }
    }

    #[test]
    fn card_face_with_nested_mode_unimplemented_is_detected() {
        let mut face = make_face();
        face.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "modal".to_string(),
                    description: None,
                },
            )
            .with_modal(
                crate::types::ability::ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 1,
                    mode_descriptions: vec!["Mode".to_string()],
                    allow_repeat_modes: false,
                    constraints: vec![],
                },
                vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Unimplemented {
                        name: "nested".to_string(),
                        description: None,
                    },
                )],
            ),
        );

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn card_face_with_unimplemented_additional_cost_is_detected() {
        let mut face = make_face();
        face.additional_cost = Some(AdditionalCost::Optional(AbilityCost::Unimplemented {
            description: "mystery cost".to_string(),
        }));

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn card_face_with_replacement_decline_unimplemented_is_detected() {
        let mut face = make_face();
        face.replacements
            .push(ReplacementDefinition::new(ReplacementEvent::Draw).mode(
                ReplacementMode::Optional {
                    decline: Some(Box::new(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Unimplemented {
                            name: "decline".to_string(),
                            description: None,
                        },
                    ))),
                },
            ));

        assert!(card_face_has_unimplemented_parts(&face));
    }

    #[test]
    fn analyze_coverage_reports_legality_based_format_totals() {
        let supported = serde_json::json!({
            "alpha": {
                "name": "Alpha",
                "mana_cost": "NoCost",
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                    (LegalityFormat::Modern, LegalityStatus::Legal),
                ])),
            },
            "beta": {
                "name": "Beta",
                "mana_cost": "NoCost",
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [{
                    "kind": "Spell",
                    "effect": { "type": "Unimplemented", "name": "beta_gap", "description": null },
                    "cost": null,
                    "sub_ability": null,
                    "duration": null,
                    "description": null,
                    "target_prompt": null,
                    "sorcery_speed": false,
                    "condition": null,
                    "optional_targeting": false
                }],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": legalities_to_export_map(&HashMap::from([
                    (LegalityFormat::Standard, LegalityStatus::Legal),
                    (LegalityFormat::Commander, LegalityStatus::Legal),
                ])),
            }
        })
        .to_string();

        let db = CardDatabase::from_json_str(&supported).expect("test export should deserialize");
        let summary = analyze_coverage(&db);

        assert_eq!(summary.total_cards, 2);
        assert_eq!(summary.supported_cards, 1);
        assert_eq!(
            summary.coverage_by_format.get("standard"),
            Some(&FormatCoverageSummary {
                total_cards: 2,
                supported_cards: 1,
                coverage_pct: 50.0,
            })
        );
        assert_eq!(
            summary.coverage_by_format.get("modern"),
            Some(&FormatCoverageSummary {
                total_cards: 1,
                supported_cards: 1,
                coverage_pct: 100.0,
            })
        );
        assert_eq!(
            summary.coverage_by_format.get("commander"),
            Some(&FormatCoverageSummary {
                total_cards: 1,
                supported_cards: 0,
                coverage_pct: 0.0,
            })
        );
    }
}
