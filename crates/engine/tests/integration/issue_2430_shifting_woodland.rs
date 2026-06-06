//! Regression for issue #2430: Shifting Woodland must tap for {G} and its
//! Delirium copy effect must revert at end of turn.
//!
//! https://github.com/phase-rs/phase/issues/2430

use engine::game::mana_sources::activatable_land_mana_options;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Duration, Effect};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHIFTING_WOODLAND_ORACLE: &str = concat!(
    "This land enters tapped unless you control a Forest.\n",
    "{T}: Add {G}.\n",
    "Delirium — {2}{G}{G}: This land becomes a copy of target permanent card in your graveyard until end of turn. ",
    "Activate only if there are four or more card types among cards in your graveyard."
);

#[test]
fn shifting_woodland_delirium_parses_become_copy_until_end_of_turn() {
    let parsed = parse_oracle_text(
        SHIFTING_WOODLAND_ORACLE,
        "Shifting Woodland",
        &[],
        &["Land".to_string()],
        &[],
    );

    let mana_abilities: Vec<_> = parsed
        .abilities
        .iter()
        .filter(|a| matches!(&*a.effect, Effect::Mana { .. }))
        .collect();
    assert_eq!(
        mana_abilities.len(),
        1,
        "expected one mana ability, got abilities: {:?}",
        parsed.abilities
    );

    let copy_ability = parsed
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::BecomeCopy { .. }))
        .expect("expected Delirium BecomeCopy activated ability");

    match &*copy_ability.effect {
        Effect::BecomeCopy { duration, .. } => {
            assert_eq!(
                duration,
                &Some(Duration::UntilEndOfTurn),
                "copy must expire at end of turn, not persist permanently"
            );
        }
        other => panic!("expected BecomeCopy, got {other:?}"),
    }
}

#[test]
fn shifting_woodland_taps_for_green_mana_when_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let woodland = scenario
        .add_land_to_hand(P0, "Shifting Woodland")
        .from_oracle_text(SHIFTING_WOODLAND_ORACLE)
        .id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&woodland).unwrap();
        obj.zone = Zone::Battlefield;
        obj.entered_battlefield_turn = Some(0);
        obj.summoning_sick = false;
    }
    let options = activatable_land_mana_options(runner.state(), woodland, P0);
    assert!(
        !options.is_empty(),
        "untapped Shifting Woodland must offer green mana"
    );
    assert!(
        options.iter().any(|o| o.mana_type == ManaType::Green),
        "expected green mana option, got {options:?}"
    );
}
