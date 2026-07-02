use super::*;
use crate::types::ability::{
    Comparator, ControllerRef, FilterProp, TargetFilter, TypeFilter, TypedFilter,
};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// CR 303.4b + CR 603.6a: "Whenever a creature enchanted player controls
/// enters" — Trespasser's Curse. The subject is a creature filtered by
/// `ControllerRef::EnchantedPlayer`, destination is Battlefield.
#[test]
fn trigger_creature_enchanted_player_controls_enters() {
    let def = parse_trigger_line(
            "Whenever a creature enchanted player controls enters, that player loses 1 life and you gain 1 life.",
            "Trespasser's Curse",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::EnchantedPlayer)
        ))
    );
}

/// CR 303.4b + CR 603.6a: "Whenever a nontoken creature enchanted player
/// controls dies" — Curse of Clinging Webs. The subject is a nontoken
/// creature filtered by `ControllerRef::EnchantedPlayer`, destination is
/// Graveyard.
#[test]
fn trigger_nontoken_creature_enchanted_player_controls_dies() {
    let def = parse_trigger_line(
            "Whenever a nontoken creature enchanted player controls dies, exile it and you create a 1/2 green Spider creature token with reach.",
            "Curse of Clinging Webs",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Graveyard));
    assert_eq!(def.origin, Some(Zone::Battlefield));
    match &def.valid_card {
        Some(TargetFilter::Typed(tf)) => {
            assert_eq!(tf.controller, Some(ControllerRef::EnchantedPlayer));
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert!(tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::NonToken)));
        }
        other => panic!("expected Typed filter, got {:?}", other),
    }
}

/// CR 303.4b + CR 603.6a: "Whenever a land enchanted player controls
/// enters" — Curse of the Restless Dead. The subject is a land filtered by
/// `ControllerRef::EnchantedPlayer`, destination is Battlefield.
#[test]
fn trigger_land_enchanted_player_controls_enters() {
    let def = parse_trigger_line(
            "Whenever a land enchanted player controls enters, you create a 2/2 black Zombie creature token with decayed.",
            "Curse of the Restless Dead",
        );
    assert_eq!(def.mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    assert_eq!(
        def.valid_card,
        Some(TargetFilter::Typed(
            TypedFilter::land().controller(ControllerRef::EnchantedPlayer)
        ))
    );
}

/// CR 107.4 + CR 202.1 + CR 603.2 + CR 603.4 (issue #4370): Namor the
/// Sub-Mariner — "Whenever you cast a noncreature spell with one or more
/// blue mana symbols in its mana cost, create that many 1/1 blue Merfolk
/// creature tokens." (1) the trigger's `valid_card` must carry the
/// colored-pip constraint (`FilterProp::ManaSymbolCount { Blue, GE, 1 }`)
/// AND the noncreature type filter, so it does not over-fire on every
/// noncreature spell; (2) the "create that many" count must back-reference
/// the cast spell's blue-pip count (`ManaSymbolsInManaCost { EventSource,
/// Some(Blue) }`), not the generic `EventContextAmount` (which resolves to
/// 0 → zero tokens).
/// Collect every `Effect` reachable through the `sub_ability` chain.
fn collect_effects(def: &crate::types::ability::AbilityDefinition) -> Vec<&Effect> {
    let mut out = Vec::new();
    let mut node = Some(def);
    while let Some(d) = node {
        out.push(&*d.effect);
        node = d.sub_ability.as_deref();
    }
    out
}

/// Walk an `And`/`Typed` valid_card looking for a `ManaSymbolCount` prop.
fn valid_card_has_blue_pip(f: &TargetFilter) -> bool {
    use crate::types::mana::ManaColor;
    match f {
        TargetFilter::And { filters } => filters.iter().any(valid_card_has_blue_pip),
        TargetFilter::Typed(tf) => tf.properties.iter().any(|p| {
            matches!(
                p,
                FilterProp::ManaSymbolCount {
                    color: Some(ManaColor::Blue),
                    comparator: Comparator::GE,
                    value: 1,
                }
            )
        }),
        _ => false,
    }
}

#[test]
fn namor_blue_pip_cast_trigger_valid_card_and_token_count() {
    use crate::types::ability::{Effect, ObjectScope, QuantityExpr, QuantityRef};
    use crate::types::mana::ManaColor;

    let def = parse_trigger_line(
            "Whenever you cast a noncreature spell with one or more blue mana symbols in its mana cost, create that many 1/1 blue Merfolk creature tokens.",
            "Namor the Sub-Mariner",
        );

    // (1) valid_card carries the blue-pip constraint so the trigger does not
    // over-fire on every noncreature spell.
    let vc = def.valid_card.as_ref().expect("Namor trigger valid_card");
    assert!(
        valid_card_has_blue_pip(vc),
        "valid_card must contain ManaSymbolCount {{ Blue, GE, 1 }}, got {vc:?}"
    );

    // (2) token count back-references the blue-pip count of the cast spell.
    let execute = def.execute.as_deref().expect("Namor trigger execute body");
    let token_count = collect_effects(execute)
        .into_iter()
        .find_map(|e| match e {
            Effect::Token { count, .. } => Some(count.clone()),
            _ => None,
        })
        .expect("Namor trigger should create tokens");
    assert_eq!(
        token_count,
        QuantityExpr::Ref {
            qty: QuantityRef::ManaSymbolsInManaCost {
                scope: ObjectScope::EventSource,
                color: Some(ManaColor::Blue),
            },
        },
        "token count must back-reference the cast spell's blue-pip count"
    );
}

/// CR 603.2 (issue #4370): Leakage guard — a cast trigger WITHOUT a
/// colored-pip qualifier ("Whenever you cast a noncreature spell, create
/// that many ... tokens") must NOT pick up the Namor override: the count
/// stays `EventContextAmount` and the valid_card carries no
/// `ManaSymbolCount`. Confirms the override is gated on the staged color.
#[test]
fn cast_trigger_without_pip_qualifier_keeps_event_context_count() {
    use crate::types::ability::{Effect, QuantityExpr, QuantityRef};

    let def = parse_trigger_line(
        "Whenever you cast a noncreature spell, create that many 1/1 blue Merfolk creature tokens.",
        "Test Card",
    );

    // valid_card must NOT contain a ManaSymbolCount prop anywhere.
    fn has_pip(f: &TargetFilter) -> bool {
        match f {
            TargetFilter::And { filters } => filters.iter().any(has_pip),
            TargetFilter::Typed(tf) => tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::ManaSymbolCount { .. })),
            _ => false,
        }
    }
    if let Some(vc) = def.valid_card.as_ref() {
        assert!(!has_pip(vc), "no ManaSymbolCount expected, got {vc:?}");
    }

    if let Some(execute) = def.execute.as_deref() {
        if let Some(count) = collect_effects(execute).into_iter().find_map(|e| match e {
            Effect::Token { count, .. } => Some(count.clone()),
            _ => None,
        }) {
            assert_eq!(
                count,
                QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount
                },
                "without a pip qualifier the count must stay EventContextAmount"
            );
        }
    }
}

/// CR 102.2 + CR 603.2 + CR 608.2d (issue #4361): Heartwood Storyteller —
/// "Whenever a player casts a noncreature spell, each of that player's
/// opponents may draw a card." The recipient is fanned out via
/// `player_scope = OpponentOfTriggeringPlayer` (each opponent of the CASTER,
/// not the controller); the body draw stays `Controller` (rebound per
/// opponent). The "may" is per-recipient (execute.optional), so the
/// trigger-level `def.optional` stays false (the whole trigger is mandatory
/// to put on the stack; each opponent independently chooses).
#[test]
fn heartwood_storyteller_opponents_of_caster_may_draw() {
    use crate::types::ability::{Effect, PlayerFilter, QuantityExpr};

    let def = parse_trigger_line(
            "Whenever a player casts a noncreature spell, each of that player's opponents may draw a card.",
            "Heartwood Storyteller",
        );

    assert_eq!(def.mode, TriggerMode::SpellCast);

    // valid_card filters to noncreature spells (Non(Creature)).
    match def.valid_card.as_ref().expect("valid_card") {
        TargetFilter::Typed(tf) => assert!(
            tf.type_filters
                .contains(&TypeFilter::Non(Box::new(TypeFilter::Creature))),
            "expected Non(Creature) in valid_card, got {tf:?}"
        ),
        other => panic!("expected Typed valid_card, got {other:?}"),
    }

    // Trigger-level optional stays false (no leading "you may").
    assert!(
        !def.optional,
        "trigger-level optional must be false; the 'may' is per-recipient"
    );

    let execute = def.execute.as_deref().expect("execute body");
    // The per-recipient "may" lives on the execute body.
    assert!(
        execute.optional,
        "execute.optional must be true (per-opponent \"may draw\")"
    );
    // Recipient SET fans out via player_scope = OpponentOfTriggeringPlayer.
    assert_eq!(
        execute.player_scope,
        Some(PlayerFilter::OpponentOfTriggeringPlayer),
        "draw must fan out to each opponent of the casting player"
    );
    // The body draw is the per-opponent Controller (rebound per iteration).
    let draw = collect_effects(execute)
        .into_iter()
        .find_map(|e| match e {
            Effect::Draw { count, target } => Some((count.clone(), target.clone())),
            _ => None,
        })
        .expect("execute must contain a Draw effect");
    assert_eq!(draw.0, QuantityExpr::Fixed { value: 1 }, "draws one card");
    assert_eq!(
        draw.1,
        TargetFilter::Controller,
        "body draw target stays Controller; player_scope rebinds it per opponent"
    );
}

/// CR 102.2 (issue #4361): building-block coverage for the "each of that
/// player's opponents [may]" recipient + per-recipient optional, independent
/// of the card name. With "may" the execute body is optional; without it the
/// body is mandatory — both fan out via `OpponentOfTriggeringPlayer`.
#[test]
fn each_of_that_players_opponents_optional_building_block() {
    use crate::types::ability::PlayerFilter;

    let optional = parse_trigger_line(
            "Whenever a player casts a noncreature spell, each of that player's opponents may draw a card.",
            "Test Card",
        );
    let opt_exec = optional.execute.as_deref().expect("execute");
    assert!(
        opt_exec.optional,
        "\"may\" makes the per-opponent draw optional"
    );
    assert_eq!(
        opt_exec.player_scope,
        Some(PlayerFilter::OpponentOfTriggeringPlayer)
    );

    let mandatory = parse_trigger_line(
        "Whenever a player casts a noncreature spell, each of that player's opponents draw a card.",
        "Test Card",
    );
    let mand_exec = mandatory.execute.as_deref().expect("execute");
    assert!(
        !mand_exec.optional,
        "without \"may\" the per-opponent draw is mandatory"
    );
    assert_eq!(
        mand_exec.player_scope,
        Some(PlayerFilter::OpponentOfTriggeringPlayer)
    );
}
