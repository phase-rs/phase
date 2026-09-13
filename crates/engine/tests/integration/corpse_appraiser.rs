//! Corpse Appraiser — ETB exile from a graveyard, then dig if a card was
//! put into exile this way.
//!
//! Oracle: "When this creature enters, exile up to one target creature card
//! from a graveyard. If a card is put into exile this way, look at the top
//! three cards of your library, then put one of those cards into your hand
//! and the rest into your graveyard."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect, QuantityExpr, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const CORPSE_APPRAISER: &str = "When this creature enters, exile up to one target creature card from a graveyard. If a card is put into exile this way, look at the top three cards of your library, then put one of those cards into your hand and the rest into your graveyard.";

fn floating_ubr() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
    ]
}

/// SHAPE: the leading-if "put into exile this way" peels so the look-at +
/// put-one-keep-rest chain can assemble as a `Dig` gated by
/// `ZoneChangedThisWay { destination: Exile }`.
#[test]
fn corpse_appraiser_etb_parses_exile_then_conditional_dig() {
    let parsed = parse_oracle_text(CORPSE_APPRAISER, "Corpse Appraiser", &[], &[], &[]);
    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("ETB trigger");
    let execute = etb.execute.as_ref().expect("execute");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        ),
        "first instruction is exile from a graveyard, got {:?}",
        execute.effect
    );
    let dig = execute.sub_ability.as_ref().expect("dig sub_ability");
    let Effect::Dig {
        count,
        keep_count,
        destination,
        rest_destination,
        ..
    } = &*dig.effect
    else {
        panic!("expected Dig follow-up, got {:?}", dig.effect);
    };
    assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
    assert_eq!(*keep_count, Some(1));
    assert_eq!(*destination, Some(Zone::Hand));
    assert_eq!(*rest_destination, Some(Zone::Graveyard));
    let Some(AbilityCondition::ZoneChangedThisWay {
        filter,
        destination: this_way_dest,
    }) = &dig.condition
    else {
        panic!(
            "Dig must be gated by ZoneChangedThisWay, got {:?}",
            dig.condition
        );
    };
    assert_eq!(*this_way_dest, Some(Zone::Exile));
    match filter {
        engine::types::ability::TargetFilter::Typed(typed) => {
            assert_eq!(typed.type_filters, vec![TypeFilter::Card]);
        }
        other => panic!("expected Typed Card this-way filter, got {other:?}"),
    }
}

/// CR 608.2c + CR 701.13a + CR 701.20e: exiling a graveyard creature card
/// makes the "put into exile this way" gate true, so the look-at-three
/// instruction surfaces a DigChoice (keep one, rest to graveyard).
#[test]
fn corpse_appraiser_etb_exile_surfaces_dig_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let appraiser = scenario
        .add_creature_to_hand_from_oracle(P0, "Corpse Appraiser", 3, 3, CORPSE_APPRAISER)
        .id();
    let gy_creature = scenario
        .add_creature_to_graveyard(P1, "Doomed Bear", 2, 2)
        .id();
    let mill_two = scenario.add_card_to_library_top(P0, "Mill Two");
    let mill_one = scenario.add_card_to_library_top(P0, "Mill One");
    let keep = scenario.add_card_to_library_top(P0, "Keep Me");
    scenario.with_mana_pool(P0, floating_ubr());

    let mut runner = scenario.build();
    let outcome = runner.cast(appraiser).target_object(gy_creature).resolve();

    assert_eq!(
        outcome.zone_of(gy_creature),
        Zone::Exile,
        "the targeted graveyard creature card must be exiled"
    );

    let WaitingFor::DigChoice {
        cards,
        keep_count,
        rest_destination,
        kept_destination,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "expected DigChoice after a card is put into exile this way, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*keep_count, 1);
    assert_eq!(*kept_destination, Some(Zone::Hand));
    assert_eq!(*rest_destination, Some(Zone::Graveyard));
    assert_eq!(cards.len(), 3, "look at the top three cards");
    assert!(
        cards.contains(&keep) && cards.contains(&mill_one) && cards.contains(&mill_two),
        "DigChoice must offer the three looked-at library cards, got {cards:?}"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![keep] })
        .expect("DigChoice SelectCards must succeed");

    let state = runner.state();
    assert_eq!(
        state.objects[&keep].zone,
        Zone::Hand,
        "the chosen looked-at card goes to hand"
    );
    assert_eq!(
        state.objects[&mill_one].zone,
        Zone::Graveyard,
        "the rest of the looked-at cards go to the graveyard"
    );
    assert_eq!(
        state.objects[&mill_two].zone,
        Zone::Graveyard,
        "the rest of the looked-at cards go to the graveyard"
    );
}
