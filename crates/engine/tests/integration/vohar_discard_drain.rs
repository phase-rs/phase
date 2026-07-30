//! Vohar drains only when its loot ability discards an instant or sorcery.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::{AbilityCondition, TargetFilter, TypeFilter};

const VOHAR: &str = "{T}: Draw a card, then discard a card. If you discarded an instant or \
sorcery card this way, each opponent loses 1 life and you gain 1 life.\n\
{2}, Sacrifice Vohar: You may cast target instant or sorcery card from your graveyard this \
turn. If that spell would be put into your graveyard, exile it instead. Activate only as a \
sorcery.";

#[test]
fn vohar_parses_effect_discard_condition() {
    let parsed = parse_oracle_text(
        VOHAR,
        "Vohar, Vodalian Desecrator",
        &["Legendary".into()],
        &["Creature".into()],
        &["Phyrexian".into(), "Merfolk".into(), "Wizard".into()],
    );
    let drain = parsed.abilities[0]
        .sub_ability
        .as_ref()
        .and_then(|discard| discard.sub_ability.as_ref())
        .expect("Vohar's loot ability must contain its drain rider");
    let Some(AbilityCondition::ZoneChangedThisWay { filter }) = &drain.condition else {
        panic!(
            "Vohar must check the card discarded by the effect: {:?}",
            drain.condition
        );
    };
    let TargetFilter::Typed(filter) = filter else {
        panic!("Vohar's discard condition must use a typed filter: {filter:?}");
    };
    assert_eq!(
        filter.type_filters,
        vec![TypeFilter::AnyOf(vec![
            TypeFilter::Instant,
            TypeFilter::Sorcery
        ])],
        "Vohar must accept either an instant or a sorcery"
    );
    assert!(
        !parsed.parse_warnings.iter().any(|warning| matches!(
            warning,
            OracleDiagnostic::SwallowedClause { detector, .. } if detector == "Condition_If"
        )),
        "Vohar's represented discard condition must not be reported as swallowed: {:?}",
        parsed.parse_warnings
    );
}

fn activate_vohar(discard_instant: bool) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    if discard_instant {
        scenario.add_spell_to_library_top(P0, "Drawn Instant", true);
    } else {
        scenario.add_card_to_library_top(P0, "Drawn Land");
    }
    let vohar = scenario
        .add_creature_from_oracle(P0, "Vohar, Vodalian Desecrator", 1, 2, VOHAR)
        .id();
    let mut runner = scenario.build();
    runner.activate(vohar, 0).resolve();
    (
        runner.state().players[P0.0 as usize].life,
        runner.state().players[P1.0 as usize].life,
    )
}

#[test]
fn vohar_drains_after_discarding_an_instant() {
    assert_eq!(
        activate_vohar(true),
        (21, 19),
        "discarding an instant must make the opponent lose 1 life and Vohar's controller gain 1"
    );
}

#[test]
fn vohar_does_not_drain_after_discarding_a_land() {
    assert_eq!(
        activate_vohar(false),
        (20, 20),
        "discarding a land must not trigger Vohar's drain rider"
    );
}
