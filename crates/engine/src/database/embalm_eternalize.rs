//! Embalm (CR 702.128) and Eternalize (CR 702.129) — graveyard-activated
//! token-copy keywords.
//!
//! Both keywords are activated abilities that function only while the card is in
//! a graveyard (CR 702.128a / CR 702.129a). Each means:
//!
//! > "[cost], Exile this card from your graveyard: Create a token that's a copy
//! > of this card, except <overrides>. Activate only as a sorcery."
//!
//! The two keywords differ only in the per-token *override set* applied on top
//! of the copy, so this module factors the shared shape into a single builder
//! (`token_copy_ability`) parameterized by the keyword's activation cost (mana
//! or a composite mana + non-mana cost, e.g. the Champion of Wits "{3}{U}{U},
//! Discard a card") and its `ContinuousModification` overrides — the "build for
//! the class" pattern. The
//! overrides reuse the existing copy-exception building block
//! (`Effect::CopyTokenOf.additional_modifications`), which the token-copy
//! resolver bakes into the synthesized token at creation
//! (see `game/effects/token_copy.rs`).
//!
//! Self-exile mechanics: the card is exiled as a cost via
//! `AbilityCost::Exile { filter: SelfRef, .. }` — CR 602.1a (the activation cost
//! is everything before the colon) — which the engine auto-pays without a player
//! choice. The card keeps its `ObjectId` when it moves graveyard → exile
//! (`game/zones.rs::move_to_zone` mutates the zone in place), so
//! `Effect::CopyTokenOf { target: SelfRef }` still resolves to the exiled card
//! and `compute_current_copiable_values` reads its printed copiable values from
//! the exile zone.
//!
//! Scope note — CR 702.128b ("a token is *embalmed* if it's created by a
//! resolving embalm ability"): this token-status flag is not tracked here
//! because no implemented card's *runtime* depends on it. The only card that
//! references it is Vizier of Many Faces ("…except if ~ was embalmed…"), whose
//! self-referential clone ETB is a separate, already-incomplete parser concern
//! (its "if embalmed" condition is not currently captured), so tracking the flag
//! would be dead infrastructure for a single card — see "build for the class".

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, Effect, QuantityExpr,
    TargetFilter,
};
use crate::types::card::CardFace;
use crate::types::keywords::{EmbalmCost, EternalizeCost, Keyword};
use crate::types::mana::ManaColor;
use crate::types::zones::Zone;

/// CR 702.128a + CR 702.129a: Synthesize the graveyard-activated token-copy
/// ability for every Embalm / Eternalize keyword printed on the face. Other
/// keywords are ignored, so a card with neither keyword is left untouched.
pub fn synthesize_embalm_eternalize(face: &mut CardFace) {
    let abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|keyword| match keyword {
            Keyword::Embalm(cost) => Some(token_copy_ability(
                embalm_cost_part(cost),
                embalm_overrides(),
            )),
            Keyword::Eternalize(cost) => Some(token_copy_ability(
                eternalize_cost_part(cost),
                eternalize_overrides(),
            )),
            _ => None,
        })
        .collect();
    face.abilities.extend(abilities);
}

/// CR 702.128a / CR 702.129a + CR 604.1: Synthesize the graveyard-activated
/// token-copy ability for a single Embalm / Eternalize keyword **granted at
/// runtime** (e.g. Cursecloth Wrappings' "target creature card in your graveyard
/// gains embalm until end of turn. The embalm cost is equal to its mana cost.").
/// Mirrors `database::synthesis::graveyard_activated_ability_for_keyword` for the
/// Encore / Scavenge family: the `AddKeyword` continuous-effect seam installs
/// only the keyword, so the activatable ability must be synthesized on demand.
/// Returns `None` for any non-Embalm/Eternalize keyword.
///
/// The caller is responsible for resolving a `ManaCost::SelfManaCost` payload to
/// the recipient card's concrete mana cost BEFORE calling — the activated-ability
/// payment path treats `SelfManaCost` as a free cost, so a self-cost grant must
/// arrive here already concretized.
pub fn embalm_eternalize_ability_for_keyword(keyword: &Keyword) -> Option<AbilityDefinition> {
    match keyword {
        Keyword::Embalm(cost) => Some(token_copy_ability(
            embalm_cost_part(cost),
            embalm_overrides(),
        )),
        Keyword::Eternalize(cost) => Some(token_copy_ability(
            eternalize_cost_part(cost),
            eternalize_overrides(),
        )),
        _ => None,
    }
}

/// CR 702.128a: Embalm's copy exceptions — "it's white, it has no mana cost,
/// and it's a Zombie in addition to its other types."
fn embalm_overrides() -> Vec<ContinuousModification> {
    vec![
        ContinuousModification::SetColor {
            colors: vec![ManaColor::White],
        },
        ContinuousModification::RemoveManaCost,
        // CR 702.128a: "Zombie in addition to its other types" — AddSubtype
        // keeps the copied subtypes (unlike RemoveAllSubtypes).
        ContinuousModification::AddSubtype {
            subtype: "Zombie".to_string(),
        },
    ]
}

/// CR 702.129a: Eternalize's copy exceptions — "it's black, it's 4/4, it has no
/// mana cost, and it's a Zombie in addition to its other types."
fn eternalize_overrides() -> Vec<ContinuousModification> {
    vec![
        ContinuousModification::SetColor {
            colors: vec![ManaColor::Black],
        },
        ContinuousModification::SetPower { value: 4 },
        ContinuousModification::SetToughness { value: 4 },
        ContinuousModification::RemoveManaCost,
        ContinuousModification::AddSubtype {
            subtype: "Zombie".to_string(),
        },
    ]
}

/// CR 602.1a: Unwrap the per-keyword `EmbalmCost` to the single keyword-cost
/// `AbilityCost` (mana or composite/non-mana) that precedes the self-exile.
/// Cost composition itself stays in `token_copy_ability` (single authority).
fn embalm_cost_part(cost: &EmbalmCost) -> AbilityCost {
    match cost {
        EmbalmCost::Mana(m) => AbilityCost::Mana { cost: m.clone() },
        EmbalmCost::NonMana(ac) => ac.clone(),
    }
}

/// CR 602.1a: `EternalizeCost` analogue of `embalm_cost_part`.
fn eternalize_cost_part(cost: &EternalizeCost) -> AbilityCost {
    match cost {
        EternalizeCost::Mana(m) => AbilityCost::Mana { cost: m.clone() },
        EternalizeCost::NonMana(ac) => ac.clone(),
    }
}

/// CR 702.128a / CR 702.129a + CR 707.2: Build the activated ability
/// "[cost], Exile this card from your graveyard: Create a token that's a copy
/// of this card, except <overrides>. Activate only as a sorcery."
fn token_copy_ability(
    keyword_cost: AbilityCost,
    overrides: Vec<ContinuousModification>,
) -> AbilityDefinition {
    // CR 602.1a: The activation cost is everything before the colon — here a
    // composite of the keyword's cost (mana, or a composite mana + non-mana such
    // as the Champion of Wits "{3}{U}{U}, Discard a card") plus exiling this card
    // from the graveyard. The SelfRef graveyard exile is auto-paid by
    // `pay_ability_cost` (no player choice). An explicit `Zone::Graveyard`
    // validates the source's location when the cost is paid.
    let exile_self = AbilityCost::Exile {
        count: 1,
        zone: Some(Zone::Graveyard),
        filter: Some(TargetFilter::SelfRef),
    };
    // Flatten an already-Composite keyword cost so the self-exile joins the
    // existing sub-costs instead of nesting (mirrors `synthesize_cycling`).
    let cost = match keyword_cost {
        AbilityCost::Composite { costs } => {
            let mut flat = costs;
            flat.push(exile_self);
            AbilityCost::Composite { costs: flat }
        }
        other => AbilityCost::Composite {
            costs: vec![other, exile_self],
        },
    };

    // CR 707.2: Create a token that's a copy of this card. `SelfRef` resolves
    // to the exiled card (it keeps its ObjectId across the graveyard → exile
    // cost), and the keyword-specific copy exceptions ride along in
    // `additional_modifications`. The token is created under the ability's
    // controller (CR 109.4 / CR 111.2 default `owner: Controller`).
    let effect = Effect::CopyTokenOf {
        target: TargetFilter::SelfRef,
        owner: TargetFilter::Controller,
        source_filter: None,
        enters_attacking: false,
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        extra_keywords: vec![],
        additional_modifications: overrides,
    };

    let mut def = AbilityDefinition::new(AbilityKind::Activated, effect)
        .cost(cost)
        // CR 702.128a / CR 702.129a: "Activate only as a sorcery."
        .sorcery_speed();
    // CR 702.128a / CR 702.129a: the ability "functions only while the card with
    // [the keyword] is in a graveyard."
    def.activation_zone = Some(Zone::Graveyard);
    def
}
