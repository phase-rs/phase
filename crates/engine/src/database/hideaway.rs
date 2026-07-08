//! Hideaway (CR 702.75) — ETB look-and-exile-face-down keyword.
//!
//! CR 702.75a: "Hideaway N" is a triggered ability that means "When this
//! permanent enters, look at the top N cards of your library. Exile one of them
//! face down and put the rest on the bottom of your library in a random order.
//! The exiled card gains 'The player who controls the permanent that exiled this
//! card may look at this card in the exile zone.'"
//!
//! Older printings used "Hideaway" (Hideaway 4); since *Murders at Karlov Manor*
//! the keyword is parameterized ("Hideaway N"). Either way the ability is only
//! represented on the card as the `Keyword::Hideaway(N)` tag plus reminder text
//! (which the parser strips), so without synthesis it is a silent no-op — and
//! the card's companion "you may play the exiled card …" ability (already parsed
//! and handled in `game/casting.rs`) has nothing to reference. This module is
//! the prerequisite link: it establishes the face-down exiled card.
//!
//! Built from existing building blocks — no bespoke interactive choice:
//!
//! 1. `Effect::Dig` (look at top N, choose one to keep, rest to bottom of
//!    library) handles the look + interactive selection + "rest on the bottom"
//!    via the fully-wired `WaitingFor::DigChoice` flow. The kept card's
//!    destination is `Exile`; the `DigChoice` resolution binds the chosen
//!    (exiled) card onto the continuation's targets.
//! 2. `Effect::HideawayConceal` (chained as a `sub_ability`, CR 608.2c) takes
//!    that exiled card — `TargetFilter::ParentTarget` — and turns it face down
//!    while linking it to the source via the `exile_links` pool, so the
//!    companion ability (`TargetFilter::ExiledBySource`) can later play it and
//!    `visibility.rs` grants the controller the CR 702.75a look-permission.

use crate::types::ability::{
    AbilityDefinition, AbilityKind, DigSource, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition,
};
use crate::types::card::CardFace;
use crate::types::keywords::Keyword;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// CR 702.75a: Synthesize the ETB look-exile-face-down ability for every
/// `Keyword::Hideaway` printed on the face. Cards without the keyword are left
/// untouched. Idempotent: re-running `synthesize_all` does not stack duplicate
/// triggers (mirrors the other keyword synthesizers).
pub fn synthesize_hideaway(face: &mut CardFace) {
    let counts: Vec<u32> = face
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Hideaway(n) => Some(*n),
            _ => None,
        })
        .collect();

    // CR 113.2c: "If an object has multiple instances of the same ability, each
    // instance functions independently" — so synthesize one ETB trigger per
    // printed Hideaway instance (no real card prints two today, but the class is
    // handled). Idempotent across repeated `synthesize_all` calls: skip the
    // instances already represented by existing Hideaway triggers, so re-running
    // never stacks duplicates.
    let already_synthesized = face
        .triggers
        .iter()
        .filter(|t| is_hideaway_trigger(t))
        .count();
    for &n in counts.iter().skip(already_synthesized) {
        face.triggers.push(hideaway_trigger(n));
    }
}

/// CR 702.75a: "When this permanent enters, look at the top N cards of your
/// library. Exile one of them face down and put the rest on the bottom of your
/// library in a random order."
fn hideaway_trigger(n: u32) -> TriggerDefinition {
    // CR 701.20e + CR 608.2c: Dig handles look-at-top-N + interactive choose-one
    // + rest-to-bottom. The kept card is exiled (face up at this point); the
    // chained `HideawayConceal` step turns it face down and links it.
    let dig = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Dig {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: n as i32 },
            destination: Some(Zone::Exile),
            keep_count: Some(1),
            keep_count_expr: None,
            up_to: false,
            filter: TargetFilter::Any,
            // CR 702.75a: "put the rest on the bottom of your library."
            rest_destination: Some(Zone::Library),
            // CR 701.20e: the cards are looked at privately, not revealed.
            reveal: false,
            enter_tapped: false,
            source: DigSource::Library,
        },
    )
    // CR 608.2c: continuation — conceal the just-exiled card (ParentTarget).
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::HideawayConceal {
            target: TargetFilter::ParentTarget,
        },
    ));

    TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::SelfRef)
        .execute(dig)
        .description(format!(
            "CR 702.75a: Hideaway {n} — when this permanent enters, look at the top {n} cards of \
             your library, exile one face down, and put the rest on the bottom in a random order."
        ))
}

/// Idempotency probe: does this trigger already carry the Hideaway conceal step?
fn is_hideaway_trigger(trigger: &TriggerDefinition) -> bool {
    fn chain_has_conceal(ability: &AbilityDefinition) -> bool {
        matches!(ability.effect.as_ref(), Effect::HideawayConceal { .. })
            || ability
                .sub_ability
                .as_deref()
                .is_some_and(chain_has_conceal)
    }
    trigger
        .execute
        .as_ref()
        .is_some_and(|a| chain_has_conceal(a))
}
