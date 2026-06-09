//! Unearth (CR 702.84) — graveyard-activated temporary reanimation keyword.
//!
//! CR 702.84a: "Unearth [cost]" means "[Cost]: Return this card from your
//! graveyard to the battlefield. It gains haste. Exile it at the beginning of
//! the next end step. If it would leave the battlefield, exile it instead of
//! putting it anywhere else. Activate only as a sorcery."
//!
//! Like Embalm / Eternalize (`database/embalm_eternalize.rs`), Unearth is an
//! activated ability that functions only while the card is in a graveyard, so
//! the synthesized ability carries `activation_zone = Some(Zone::Graveyard)` and
//! `sorcery_speed()`. Unlike those keywords it returns the *actual card* (not a
//! token copy), so the resolution is a four-part chain built entirely from
//! existing engine building blocks — this module adds **no new resolver**:
//!
//! 1. `Effect::ChangeZone` graveyard → battlefield (the reanimation itself),
//! 2. an `Effect::GenericEffect` continuous "gains haste" grant (Layer 6),
//! 3. an `Effect::CreateDelayedTrigger` that exiles the card at the next end
//!    step (CR 702.84a), and
//! 4. an `Effect::AddTargetReplacement` installing the "if it would leave the
//!    battlefield, exile it instead" replacement (CR 702.84a / CR 614.1a).
//!
//! Steps 2–4 are chained as `sub_ability` continuation steps (CR 608.2c) of the
//! primary `ChangeZone`, so they resolve as one action.
//!
//! Self-reference binding: every step targets `TargetFilter::SelfRef`. The card
//! keeps its `ObjectId` across the graveyard → battlefield move (`ObjectId` is
//! storage identity in `game/zones.rs::move_to_zone`, persistent across the
//! CR 400.7 "new object" boundary), so `SelfRef` — which resolves to the
//! ability's `source_id` — keeps pointing at the returned permanent for the
//! haste grant, the delayed-exile target, and the replacement host. This is
//! exactly why Unearth's *self*-return is simpler than the targeted
//! temporary-reanimation template (Goryo's Vengeance), where the reanimated
//! creature is a *different* object than the spell and the riders must instead
//! bind through `LastCreated` / `ParentTarget`.

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, DelayedTriggerCondition,
    Duration, Effect, ReplacementDefinition, StaticDefinition, TargetFilter,
};
use crate::types::card::CardFace;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// CR 702.84a: Synthesize the graveyard-activated reanimation ability for every
/// `Keyword::Unearth` printed on the face. Cards without the keyword are left
/// untouched. Per CR 113.2c each `Keyword::Unearth` yields its own ability, so a
/// (hypothetical) face with two Unearth costs offers both — matching the Embalm
/// / Eternalize "one ability per keyword" shape.
pub fn synthesize_unearth(face: &mut CardFace) {
    let abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|keyword| match keyword {
            Keyword::Unearth(cost) => Some(unearth_ability(cost.clone())),
            _ => None,
        })
        .collect();
    face.abilities.extend(abilities);
}

/// CR 702.84a: Build the full activated ability
/// "[cost]: Return this card from your graveyard to the battlefield. It gains
/// haste. Exile it at the beginning of the next end step. If it would leave the
/// battlefield, exile it instead of putting it anywhere else. Activate only as a
/// sorcery."
fn unearth_ability(mana_cost: ManaCost) -> AbilityDefinition {
    // CR 608.2c: the haste grant, the delayed exile, and the leaves-battlefield
    // replacement are continuation steps of the reanimation, chained innermost
    // first so they read in Oracle order once linked onto the primary effect.
    let chain = grant_haste_step()
        .sub_ability(delayed_exile_step().sub_ability(leaves_battlefield_exile_step()));

    // CR 602.1a: the activation cost (everything before the colon) is just the
    // keyword's mana cost — Unearth does not exile the card as a cost; it
    // returns it as the effect.
    let mut def = AbilityDefinition::new(AbilityKind::Activated, return_to_battlefield_effect())
        .cost(AbilityCost::Mana { cost: mana_cost })
        // CR 702.84a: "Activate only as a sorcery."
        .sorcery_speed()
        .sub_ability(chain);
    // CR 702.84a: Unearth "functions while the card with unearth is in a
    // graveyard" — the ability is only legal to activate from the graveyard.
    def.activation_zone = Some(Zone::Graveyard);
    def
}

/// CR 702.84a: "Return this card from your graveyard to the battlefield." The
/// card returns under its owner's control (CR 110.2a — the activating player is
/// the owner of a card in their own graveyard), so `enters_under` stays `None`.
fn return_to_battlefield_effect() -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Battlefield,
        target: TargetFilter::SelfRef,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        face_down_profile: None,
    }
}

/// CR 702.84a: "It gains haste." A continuous Layer 6 keyword grant on the
/// returned permanent (`SelfRef`). The `Permanent` duration ends naturally when
/// the object leaves the battlefield, which Unearth forces by the end of the
/// turn anyway. Mirrors the Riot "gains haste" grant in `synthesis.rs`.
fn grant_haste_step() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Haste,
                }])],
            duration: Some(Duration::Permanent),
            target: None,
        },
    )
    .duration(Duration::Permanent)
    .description("It gains haste.".to_string())
}

/// CR 702.84a: "Exile it at the beginning of the next end step." A one-shot
/// delayed trigger (CR 603.7d) whose effect exiles the returned permanent. The
/// delayed `ResolvedAbility` carries the ability's `source_id`, so `SelfRef` in
/// its effect resolves to the returned permanent when the trigger fires.
fn delayed_exile_step() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::CreateDelayedTrigger {
            // CR 513: "the beginning of the next end step."
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                exile_self_from_battlefield_effect(),
            )),
            uses_tracked_set: false,
        },
    )
    .description("Exile it at the beginning of the next end step.".to_string())
}

/// CR 702.84a / CR 614.1a: "If it would leave the battlefield, exile it instead
/// of putting it anywhere else." Installs a `Moved` replacement on the returned
/// permanent (`target: SelfRef`); `valid_card: SelfRef` binds the replacement to
/// its own host so it fires only for that object, redirecting any
/// battlefield-exit to exile.
fn leaves_battlefield_exile_step() -> AbilityDefinition {
    let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            exile_self_from_battlefield_effect(),
        ));

    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::AddTargetReplacement {
            replacement: Box::new(replacement),
            target: TargetFilter::SelfRef,
        },
    )
    .description(
        "If it would leave the battlefield, exile it instead of putting it anywhere else."
            .to_string(),
    )
}

/// CR 702.84a: the shared "exile this permanent from the battlefield" move,
/// used both by the delayed end-step trigger and by the leaves-battlefield
/// replacement's redirect. Targets `SelfRef` (the returned permanent).
fn exile_self_from_battlefield_effect() -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Battlefield),
        destination: Zone::Exile,
        target: TargetFilter::SelfRef,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        face_down_profile: None,
    }
}
