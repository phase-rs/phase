//! Encore (CR 702.141) — graveyard-activated, per-opponent token-copy keyword.
//!
//! CR 702.141a: "Encore [cost]" is an activated ability that functions only
//! while the card with encore is in a graveyard:
//!
//! > "[Cost], Exile this card from your graveyard: For each opponent, create a
//! > token that's a copy of this card that attacks that opponent this turn if
//! > able. The tokens gain haste. Sacrifice them at the beginning of the next
//! > end step. Activate only as a sorcery."
//!
//! Like Embalm / Eternalize (`database/embalm_eternalize.rs`) and Unearth
//! (`database/unearth.rs`), Encore is an activated ability that functions only
//! while the card is in a graveyard, so the synthesized ability carries
//! `activation_zone = Some(Zone::Graveyard)` and `sorcery_speed()`, and exiles
//! the card from the graveyard as part of its activation cost (CR 602.1a).
//!
//! The per-opponent copy generation, the "attacks that opponent this turn"
//! requirement, the haste grant, and the next-end-step sacrifice are not
//! expressible as a composition of existing leaf effects (the generic
//! `Effect::ForceAttack` cannot resolve "the opponent" from a context ref), so
//! the body lives in a single dedicated `Effect::Encore` resolver
//! (`game/effects/encore.rs`), mirroring its sibling `Effect::Myriad`. This
//! module only builds the activation shell around it.

use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect};
use crate::types::card::CardFace;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

/// CR 702.141a: Synthesize the graveyard-activated Encore ability for every
/// `Keyword::Encore` printed on the face. Cards without the keyword are left
/// untouched. Per CR 113.2c each `Keyword::Encore` yields its own ability.
pub fn synthesize_encore(face: &mut CardFace) {
    let abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(encore_ability_for_keyword)
        .collect();
    face.abilities.extend(abilities);
}

/// CR 702.141a + CR 604.1: Standalone keyword→ability builder so the Encore
/// activated ability can be synthesized both at card-build time
/// (`synthesize_encore`) and on the fly for runtime-granted Encore (the
/// graveyard activated-ability gather in `casting.rs`). Mirrors the
/// `cycling_ability_for_keyword` precedent. Returns `None` for non-Encore
/// keywords.
pub(crate) fn encore_ability_for_keyword(keyword: &Keyword) -> Option<AbilityDefinition> {
    match keyword {
        Keyword::Encore(cost) => Some(encore_ability(cost.clone())),
        _ => None,
    }
}

/// CR 702.141a + CR 602.1a: Build the activated ability
/// "[cost], Exile this card from your graveyard: For each opponent, create a
/// token that's a copy of this card that attacks that opponent this turn if
/// able. The tokens gain haste. Sacrifice them at the beginning of the next end
/// step. Activate only as a sorcery."
fn encore_ability(mana_cost: ManaCost) -> AbilityDefinition {
    // CR 602.1a: The activation cost is everything before the colon — the
    // keyword's mana cost plus exiling this card from the graveyard. The SelfRef
    // graveyard exile is auto-paid by the cost resolver (no player choice); the
    // explicit `Zone::Graveyard` validates the source's location when paid.
    let cost = AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana { cost: mana_cost },
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: Some(crate::types::ability::TargetFilter::SelfRef),
            },
        ],
    };

    let mut def = AbilityDefinition::new(AbilityKind::Activated, Effect::Encore)
        .cost(cost)
        // CR 702.141a: "Activate only as a sorcery."
        .sorcery_speed();
    // CR 702.141a: the ability "functions while the card with encore is in a
    // graveyard" — only legal to activate from the graveyard.
    def.activation_zone = Some(Zone::Graveyard);
    def
}
