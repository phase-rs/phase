//! Regression for GitHub issue #4253 — Sanar, Innovative First-Year's Vivid
//! ability revealed nonlands into hand before the per-color exile loop, so
//! `ForEachCategoryExile` saw an empty Library pool.
//!
//! CR 701.20b: revealed cards stay in the library until an effect moves them.
//! Sanar's "for each of those colors, you may exile a card of that color from
//! among the revealed cards" requires the reveal-until step to leave the pile
//! in the library.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, Chooser, ControllerRef, Effect, IterationCategory, QuantityExpr, QuantityRef,
    ResolvedAbility, RevealUntilDisposition, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::{EtbTapState, Zone};

const SANAR_VIVID_ORACLE: &str = "Reveal cards from the top of your library until you reveal X \
nonland cards, where X is the number of colors among permanents you control. For each of those \
colors, you may exile a card of that color from among the revealed cards. Then shuffle. You may \
cast the exiled cards this turn.";

fn distinct_colors_count() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::DistinctColorsAmongPermanents {
            filter: TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
        },
    }
}

fn sanar_vivid_chain(source: ObjectId) -> ResolvedAbility {
    let mut ability = ResolvedAbility::new(
        Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Land))],
                controller: None,
                properties: vec![],
            }),
            count: distinct_colors_count(),
            matched_disposition: RevealUntilDisposition::RevealOnly,
            kept_destination: Zone::Library,
            rest_destination: Zone::Library,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            kept_optional_to: None,
            enters_under: None,
        },
        vec![],
        source,
        P0,
    );
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::ForEachCategoryExile {
            category: IterationCategory::Color,
            zone: Zone::Library,
            chooser: Chooser::Controller,
            up_to: true,
        },
        vec![],
        source,
        P0,
    )));
    ability
}

#[test]
fn sanar_vivid_parses_library_reveal_then_per_color_exile() {
    let def = parse_effect_chain(SANAR_VIVID_ORACLE, AbilityKind::Spell);
    assert!(
        matches!(
            &*def.effect,
            Effect::RevealUntil {
                matched_disposition: RevealUntilDisposition::RevealOnly,
                kept_destination: Zone::Library,
                rest_destination: Zone::Library,
                ..
            }
        ),
        "reveal-until must keep the pile in the library, got {:?}",
        def.effect
    );
    let exile = def
        .sub_ability
        .as_ref()
        .expect("Vivid must chain into per-color exile");
    assert!(
        matches!(
            exile.effect.as_ref(),
            Effect::ForEachCategoryExile {
                category: IterationCategory::Color,
                zone: Zone::Library,
                up_to: true,
                ..
            }
        ),
        "expected ForEachCategoryExile, got {:?}",
        exile.effect
    );
}

#[test]
fn sanar_vivid_per_color_exile_offers_revealed_library_cards() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let white_perm = scenario.add_creature(P0, "White Bear", 2, 2).id();
    let red_perm = scenario.add_creature(P0, "Red Bear", 2, 2).id();

    // Library top → bottom: land, red sorcery, land, white sorcery.
    let _bottom = scenario.add_card_to_library_top(P0, "Bottom Marker");
    let white_spell = scenario.add_card_to_library_top(P0, "White Bolt");
    let _land2 = scenario.add_card_to_library_top(P0, "Land Two");
    let red_spell = scenario.add_card_to_library_top(P0, "Red Bolt");
    let _land1 = scenario.add_card_to_library_top(P0, "Land One");

    let source = scenario.add_creature(P0, "Sanar Source", 1, 1).id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&white_perm)
        .unwrap()
        .color = vec![ManaColor::White];
    runner.state_mut().objects.get_mut(&red_perm).unwrap().color = vec![ManaColor::Red];

    for (id, core) in [
        (red_spell, CoreType::Sorcery),
        (white_spell, CoreType::Sorcery),
        (_land1, CoreType::Land),
        (_land2, CoreType::Land),
    ] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types = vec![core];
    }
    runner
        .state_mut()
        .objects
        .get_mut(&red_spell)
        .unwrap()
        .color = vec![ManaColor::Red];
    runner
        .state_mut()
        .objects
        .get_mut(&white_spell)
        .unwrap()
        .color = vec![ManaColor::White];

    let ability = sanar_vivid_chain(source);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Sanar Vivid chain must resolve through per-color exile");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { cards, up_to, .. } => {
            assert!(*up_to, "you may exile is optional per color");
            assert_eq!(
                cards,
                &vec![white_spell],
                "WUBRG iteration offers the white sorcery first while it remains in the library"
            );
        }
        other => panic!("expected ChooseFromZoneChoice for the first color member, got {other:?}"),
    }

    assert_eq!(
        runner.state().objects[&red_spell].zone,
        Zone::Library,
        "revealed cards must stay in the library until exiled"
    );
    assert_eq!(
        runner.state().objects[&white_spell].zone,
        Zone::Library,
        "revealed cards must stay in the library until exiled"
    );
}
