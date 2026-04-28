//! Token-creation conversion: mtgish `CreatableToken` → engine `Effect::Token`.
//!
//! mtgish's `CreatableToken` enum has 60+ variants spanning predefined artifact
//! tokens (Treasure / Food / Blood / Clue / Powerstone / Map / Junk / Gold /
//! Shard / Lander / Mutagen / Vibranium / Walker / Octopus / Fish), Role tokens
//! (Cursed / Monster / Royal / Sorcerer / Virtuous / Wicked / Young Hero), the
//! generic `CreatureToken(PT, type, ColorList, subtypes)` shape, the
//! `NumberTokens(N, inner)` multiplier wrapper, and a long tail of token-copy
//! / replacement-context shapes.
//!
//! This module covers the high-frequency shapes (count ≥ 100 in the corpus):
//! `CreatureToken`, `NumberTokens`, and the predefined artifact tokens.
//! `CreatureTokenWithAbilities` strict-fails because faithful conversion would
//! require recursing through `Vec<Rule>` for the granted abilities, which is a
//! larger sub-system change.

use engine::types::ability::{Effect, PtValue, QuantityExpr, StaticDefinition};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::TargetFilter;

use crate::convert::filter;
use crate::convert::keyword as keyword_convert;
use crate::convert::quantity;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::{
    Color, ColorList, CreatableToken, CreatureTokenSubtypes, CreatureTokenType, GameNumber,
    PTXValue, Rule, TokenCopyEffects, PT,
};

/// Convert a single `CreatableToken` into an `Effect::Token`.
///
/// CR 111.1 + CR 111.5: Token creation. The engine's `Effect::Token` carries
/// every dimension we need (name, types, P/T, colors, count, owner,
/// supertypes), so the conversion is mostly a 1:1 mapping with subtype/PT
/// extraction.
pub fn convert(t: &CreatableToken) -> ConvResult<Effect> {
    Ok(match t {
        // CR 111.5: "Create N <token>" — multiplier over a single token spec.
        CreatableToken::NumberTokens(n, inner) => {
            let mut eff = convert(inner)?;
            apply_count(&mut eff, quantity::convert(n)?)?;
            eff
        }

        // CR 111.1: Generic creature token "Create a <colors> <subtypes>
        // creature token with power/toughness P/T".
        CreatableToken::CreatureToken(pt, ttype, colors, subs) => {
            build_creature_token(None, pt, ttype, colors, subs, Vec::new(), Vec::new())?
        }

        // CR 111.1 + CR 113.3d: Generic creature token whose printed face also
        // grants abilities. We recurse the inner `Vec<Rule>` and absorb any
        // pure-keyword (`Keyword::Flying`, etc.) or static-ability rules into
        // the engine's `Effect::Token { keywords, static_abilities, .. }`
        // slots. Triggered/activated abilities granted to the token are not
        // expressible in `Effect::Token` today (no `triggers` / `abilities`
        // slot on `TokenSpec`), so any non-keyword/non-static rule strict-
        // fails with an explicit engine prerequisite.
        CreatableToken::CreatureTokenWithAbilities(pt, ttype, colors, subs, rules) => {
            let (keywords, statics) = absorb_token_rules(rules)?;
            build_creature_token(None, pt, ttype, colors, subs, keywords, statics)?
        }

        // CR 111.4: Token whose printed name is supplied (e.g. "Beast", a
        // specific tribal name). Without abilities — straight build.
        CreatableToken::NamedCreatureToken(name, pt, ttype, colors, subs) => {
            build_creature_token(Some(name), pt, ttype, colors, subs, Vec::new(), Vec::new())?
        }

        // CR 111.4 + CR 113.3d: Named creature token with granted abilities.
        // Same absorb-or-fail discipline as `CreatureTokenWithAbilities`.
        CreatableToken::NamedCreatureTokenWithAbilities(name, pt, ttype, colors, subs, rules) => {
            let (keywords, statics) = absorb_token_rules(rules)?;
            build_creature_token(Some(name), pt, ttype, colors, subs, keywords, statics)?
        }

        // CR 205.4a + CR 111.4: Legendary named creature tokens. Engine
        // `Effect::Token { supertypes, .. }` already carries the supertype
        // slot, so we can stamp `Legendary` on the token spec directly.
        CreatableToken::LegendaryNamedCreatureToken(name, pt, ttype, colors, subs) => {
            build_creature_token_full(
                Some(name),
                pt,
                ttype,
                colors,
                subs,
                Vec::new(),
                Vec::new(),
                vec![engine::types::card_type::Supertype::Legendary],
            )?
        }
        CreatableToken::LegendaryNamedCreatureTokenWithAbilities(
            name,
            pt,
            ttype,
            colors,
            subs,
            rules,
        ) => {
            let (keywords, statics) = absorb_token_rules(rules)?;
            build_creature_token_full(
                Some(name),
                pt,
                ttype,
                colors,
                subs,
                keywords,
                statics,
                vec![engine::types::card_type::Supertype::Legendary],
            )?
        }

        // CR 707.2 / CR 707.5: "Create a token that's a copy of <permanent>".
        // The engine's `Effect::CopyTokenOf` carries the target filter,
        // count, and post-copy modifications. mtgish's `TokenCopyEffects` is
        // a list of "and the copy has X" / "and the copy is named Y" /
        // "the copy gains <ability>" overrides — only the empty form
        // (`NoTokenCopyEffects`) maps cleanly today; any non-empty list
        // requires translating the inner overrides to
        // `ContinuousModification` (and ability-additions to a slot
        // `CopyTokenOf` does not yet have), so we strict-fail with an
        // explicit gap when present.
        CreatableToken::TokenCopyOfPermanent(perm, copy_effects) => {
            require_no_copy_effects(copy_effects)?;
            Effect::CopyTokenOf {
                target: filter::convert_permanent(perm)?,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: Vec::new(),
                additional_modifications: Vec::new(),
            }
        }
        // CR 707.2 + CR 115.1: "Create a token that's a copy of target
        // <Permanents-filter>" — same shape but the filter resolves through
        // the `Permanents` (set) selector rather than a specific
        // permanent-context reference.
        CreatableToken::TokenCopyOfAPermanent(perms, copy_effects) => {
            require_no_copy_effects(copy_effects)?;
            Effect::CopyTokenOf {
                target: filter::convert(perms)?,
                enters_attacking: false,
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                extra_keywords: Vec::new(),
                additional_modifications: Vec::new(),
            }
        }

        // CR 111.1 + CR 111.4: Predefined artifact tokens. Names and type
        // lines mirror the canonical token printings (Wizards' "Treasure",
        // "Food", etc. each have a fixed Artifact -- <Name> identity).
        CreatableToken::TreasureToken => predefined_artifact_token("Treasure"),
        CreatableToken::FoodToken => predefined_artifact_token("Food"),
        CreatableToken::BloodToken => predefined_artifact_token("Blood"),
        CreatableToken::ClueToken => predefined_artifact_token("Clue"),
        CreatableToken::PowerstoneToken => predefined_artifact_token("Powerstone"),
        CreatableToken::MapToken => predefined_artifact_token("Map"),
        CreatableToken::JunkToken => predefined_artifact_token("Junk"),
        CreatableToken::GoldToken => predefined_artifact_token("Gold"),
        CreatableToken::ShardToken => predefined_artifact_token("Shard"),
        CreatableToken::LanderToken => predefined_artifact_token("Lander"),
        CreatableToken::MutagenToken => predefined_artifact_token("Mutagen"),

        // CR 303.7: Role tokens — each is an Enchantment -- Aura Role with a
        // canonical name. The native parser's `known_role_token_identity`
        // maps the same descriptors to `["Enchantment", "Aura", "Role"]`
        // type lines; we mirror that shape here so downstream synthesis
        // (which keys on the token name) generates the right granted
        // abilities. The created token is attached to the Role's host —
        // the attachment target is supplied by the surrounding Oracle
        // sentence and is not part of the `CreatableToken` payload itself,
        // so `attach_to: None` is the correct authoring-time default
        // (matches how the native parser leaves attachment unset when the
        // descriptor lacks an `attached to` clause).
        CreatableToken::CursedRoleToken => predefined_role_token("Cursed Role"),
        CreatableToken::MonsterRoleToken => predefined_role_token("Monster Role"),
        CreatableToken::RoyalRoleToken => predefined_role_token("Royal Role"),
        CreatableToken::SorcererRoleToken => predefined_role_token("Sorcerer Role"),
        CreatableToken::VirtuousRoleToken => predefined_role_token("Virtuous Role"),
        CreatableToken::WickedRoleToken => predefined_role_token("Wicked Role"),
        CreatableToken::YoungHeroRoleToken => predefined_role_token("Young Hero Role"),

        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "CreatableToken/convert",
                path: String::new(),
                detail: format!("unsupported CreatableToken: {}", token_tag(other)),
            });
        }
    })
}

fn build_creature_token(
    name: Option<&str>,
    pt: &PT,
    ttype: &CreatureTokenType,
    colors: &ColorList,
    subs: &CreatureTokenSubtypes,
    keywords: Vec<Keyword>,
    static_abilities: Vec<StaticDefinition>,
) -> ConvResult<Effect> {
    build_creature_token_full(
        name,
        pt,
        ttype,
        colors,
        subs,
        keywords,
        static_abilities,
        Vec::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_creature_token_full(
    name: Option<&str>,
    pt: &PT,
    ttype: &CreatureTokenType,
    colors: &ColorList,
    subs: &CreatureTokenSubtypes,
    keywords: Vec<Keyword>,
    static_abilities: Vec<StaticDefinition>,
    supertypes: Vec<engine::types::card_type::Supertype>,
) -> ConvResult<Effect> {
    let (power, toughness) = pt_to_values(pt)?;
    let subtypes = creature_subtypes(subs)?;
    let mut types = creature_token_core_types(ttype);
    for s in &subtypes {
        push_unique(&mut types, s.clone());
    }
    let resolved_name = match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ if subtypes.is_empty() => "Creature".to_string(),
        _ => subtypes.join(" "),
    };
    Ok(Effect::Token {
        name: resolved_name,
        power,
        toughness,
        types,
        colors: color_list_to_colors(colors)?,
        keywords,
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes,
        static_abilities,
        enter_with_counters: Vec::new(),
    })
}

fn predefined_artifact_token(name: &str) -> Effect {
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(0),
        toughness: PtValue::Fixed(0),
        types: vec!["Artifact".to_string(), name.to_string()],
        colors: Vec::new(),
        keywords: Vec::new(),
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: Vec::new(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
    }
}

/// CR 303.7: Build a Role token (Enchantment -- Aura Role). Mirrors the
/// native parser's `known_role_token_identity` shape (`["Enchantment",
/// "Aura", "Role"]` types, no power/toughness, no colors). Granted
/// abilities (e.g. Cursed Role's "−1/−1 to enchanted creature") are
/// synthesized at runtime from the canonical token name, not authored
/// here — the same way native parser output keys role behavior off the
/// `name` field.
fn predefined_role_token(name: &str) -> Effect {
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(0),
        toughness: PtValue::Fixed(0),
        types: vec![
            "Enchantment".to_string(),
            "Aura".to_string(),
            "Role".to_string(),
        ],
        colors: Vec::new(),
        keywords: Vec::new(),
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: Vec::new(),
        static_abilities: Vec::new(),
        enter_with_counters: Vec::new(),
    }
}

/// CR 113.3d + CR 702: Pull keyword and continuous-static rules out of an
/// inner `Vec<Rule>` for token-with-abilities specs. Triggered/activated
/// abilities granted to a token are not yet expressible in `Effect::Token`
/// (no `triggers` / `abilities` slot on the spec), so any rule that doesn't
/// reduce cleanly to a `Keyword` or a self-targeted `StaticDefinition`
/// strict-fails with an explicit engine prerequisite. This keeps the
/// huge "1/1 white Soldier creature token with flying" / "X/X Wurm with
/// trample" class working while surfacing the granted-trigger / granted-
/// activated gaps as a separate, future engine extension round.
fn absorb_token_rules(rules: &[Rule]) -> ConvResult<(Vec<Keyword>, Vec<StaticDefinition>)> {
    let mut keywords = Vec::new();
    let mut statics = Vec::new();
    for rule in rules {
        if let Some(kw) = keyword_convert::try_convert(rule, "CreatableToken/with-abilities")? {
            keywords.push(kw);
            continue;
        }
        // Static-only rules: self-referential `PermanentLayerEffect` (the
        // Oracle phrasing "this creature has X" / "this creature gets +N/+N"
        // on a token printing) maps to a single `StaticDefinition` whose
        // `affected` filter is the token itself (`TargetFilter::SelfRef`).
        // Other rule shapes (triggers, activated, replacements, "whenever"
        // clauses) require slots `Effect::Token` does not yet expose.
        if let Rule::PermanentLayerEffect(target, effects) = rule {
            let affected = filter::convert_permanent(target)?;
            let s = crate::convert::static_effect::build_static(affected, effects)?;
            statics.push(s);
            continue;
        }
        return Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "TokenSpec",
            needed_variant: format!(
                "abilities-slot-for-rule:{}",
                variant_tag_rule(rule).unwrap_or_else(|| "<untagged>".to_string())
            ),
        });
    }
    Ok((keywords, statics))
}

fn variant_tag_rule(rule: &Rule) -> Option<String> {
    serde_json::to_value(rule)
        .ok()
        .and_then(|v| v.get("_Rule").and_then(|t| t.as_str()).map(String::from))
}

/// CR 707.2: Token-copy override list. Only the empty form is a clean
/// reuse of the engine's `Effect::CopyTokenOf` — every non-empty list
/// requires translating per-override semantics (set name, set P/T, add
/// abilities, etc.) into the engine's `extra_keywords` /
/// `additional_modifications` slots, which don't yet cover ability-grants
/// or `SetName`. Strict-fail until that translation lands.
fn require_no_copy_effects(effects: &TokenCopyEffects) -> ConvResult<()> {
    match effects {
        TokenCopyEffects::NoTokenCopyEffects => Ok(()),
        TokenCopyEffects::TokenCopyEffects(list) if list.is_empty() => Ok(()),
        TokenCopyEffects::TokenCopyEffects(_) => Err(ConversionGap::EnginePrerequisiteMissing {
            engine_type: "Effect::CopyTokenOf",
            needed_variant: "token-copy-overrides".into(),
        }),
    }
}

/// Override the `count` slot on an `Effect::Token` from a `NumberTokens`
/// wrapper. Other Effect variants strict-fail — `NumberTokens` is the only
/// schema shape that emits one outside this module.
fn apply_count(e: &mut Effect, count: QuantityExpr) -> ConvResult<()> {
    if let Effect::Token { count: c, .. } = e {
        *c = count;
        Ok(())
    } else {
        Err(ConversionGap::MalformedIdiom {
            idiom: "CreatableToken/NumberTokens",
            path: String::new(),
            detail: "inner is not Effect::Token".into(),
        })
    }
}

fn creature_token_core_types(t: &CreatureTokenType) -> Vec<String> {
    use CreatureTokenType as C;
    match t {
        C::CreatureToken => vec!["Creature".to_string()],
        C::ArtifactCreatureToken => vec!["Artifact".to_string(), "Creature".to_string()],
        C::EnchantmentCreatureToken => vec!["Enchantment".to_string(), "Creature".to_string()],
        C::EnchantmentArtifactCreatureToken => vec![
            "Enchantment".to_string(),
            "Artifact".to_string(),
            "Creature".to_string(),
        ],
        C::LandCreatureToken => vec!["Land".to_string(), "Creature".to_string()],
    }
}

fn creature_subtypes(s: &CreatureTokenSubtypes) -> ConvResult<Vec<String>> {
    use CreatureTokenSubtypes as S;
    Ok(match s {
        S::CreatureTokenSubtypesList(list) => list.iter().map(|st| format!("{st:?}")).collect(),
        // CR 113.6: "<chosen creature type>" — runtime resolution; no static
        // subtype available at conversion time. Strict-fail for now.
        S::TheChosenCreatureType => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "CreatureTokenSubtypes/TheChosenCreatureType",
                path: String::new(),
                detail: "chosen creature type not yet resolvable in token spec".into(),
            });
        }
    })
}

fn color_list_to_colors(c: &ColorList) -> ConvResult<Vec<ManaColor>> {
    Ok(match c {
        ColorList::Colorless => Vec::new(),
        ColorList::AllColors => ManaColor::ALL.to_vec(),
        ColorList::Colors(list) => {
            let mut out = Vec::with_capacity(list.len());
            for color in list {
                if let Some(mc) = simple_color(color) {
                    out.push(mc);
                }
            }
            // Some `Colors` lists may include `Colorless` mixed with colored;
            // we filter to the colored set since `ManaColor` doesn't have a
            // colorless variant. An empty result for a non-empty input
            // (e.g., `[Colorless]`) collapses to "no colors", matching the
            // engine's representation.
            out
        }
        ColorList::TheChosenColor => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "ColorList/TheChosenColor",
                path: String::new(),
                detail: "chosen color not yet resolvable in token spec".into(),
            });
        }
    })
}

fn simple_color(c: &Color) -> Option<ManaColor> {
    Some(match c {
        Color::White => ManaColor::White,
        Color::Blue => ManaColor::Blue,
        Color::Black => ManaColor::Black,
        Color::Red => ManaColor::Red,
        Color::Green => ManaColor::Green,
        // Colorless/chosen-color are dropped by the caller above; runtime
        // refs strict-fail there.
        _ => return None,
    })
}

fn pt_to_values(pt: &PT) -> ConvResult<(PtValue, PtValue)> {
    use PT as P;
    Ok(match pt {
        P::PT(p, t) => (PtValue::Fixed(*p), PtValue::Fixed(*t)),
        P::ZeroPT => (PtValue::Fixed(0), PtValue::Fixed(0)),
        P::PTX(p, t, _n) => (ptx_to_value(p), ptx_to_value(t)),
        P::ManualPT(p, t) => (game_number_to_value(p)?, game_number_to_value(t)?),
        other => {
            return Err(ConversionGap::MalformedIdiom {
                idiom: "PT/pt_to_values",
                path: String::new(),
                detail: format!("unsupported PT: {other:?}"),
            });
        }
    })
}

fn ptx_to_value(v: &PTXValue) -> PtValue {
    match v {
        PTXValue::Integer(n) => PtValue::Fixed(*n),
        // CR 107.3 + CR 111.10: X-valued P/T on token specs resolves at
        // creation time; we mark X via Variable("X") consistent with the
        // crate's no-silent-zero rule.
        PTXValue::X => PtValue::Variable("X".to_string()),
    }
}

fn game_number_to_value(n: &GameNumber) -> ConvResult<PtValue> {
    match n {
        GameNumber::Integer(v) => Ok(PtValue::Fixed(*v)),
        GameNumber::ValueX => Ok(PtValue::Variable("X".to_string())),
        other => Err(ConversionGap::MalformedIdiom {
            idiom: "PT/ManualPT",
            path: String::new(),
            detail: format!("unsupported GameNumber for PT: {other:?}"),
        }),
    }
}

fn push_unique(v: &mut Vec<String>, s: String) {
    if !v.contains(&s) {
        v.push(s);
    }
}

fn token_tag(t: &CreatableToken) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| {
            v.get("_CreatableToken")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}
