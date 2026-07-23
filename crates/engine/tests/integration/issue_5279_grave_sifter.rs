//! Issue #5279: Grave Sifter (Discord report title "Grace Sifter") — each player
//! chooses a creature type and returns any number of matching cards from their
//! graveyard to their hand.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ChoiceType, Effect, FilterProp, PlayerFilter, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GRAVE_SIFTER_ORACLE: &str =
    "When this creature enters, each player chooses a creature type and returns any number of cards of that type from their graveyard to their hand.";

fn parsed_grave_sifter() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        GRAVE_SIFTER_ORACLE,
        "Grave Sifter",
        &[],
        &["Creature".to_string()],
        &["Elemental".to_string(), "Beast".to_string()],
    )
}

fn setup_grave_sifter_cast() -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let hill_giant = scenario
        .add_creature_to_graveyard(P1, "Hill Giant", 3, 3)
        .with_subtypes(vec!["Giant"])
        .id();

    let grave_sifter = scenario
        .add_creature_to_hand_from_oracle(P0, "Grave Sifter", 5, 7, GRAVE_SIFTER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![ManaCostShard::Green],
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(9_990), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_991), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_992), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_993), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_994), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(9_995), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Bear".to_string(),
        "Giant".to_string(),
        "Elemental".to_string(),
        "Beast".to_string(),
    ];

    (runner, grave_sifter, bears, hill_giant)
}

#[test]
fn grave_sifter_etb_parses_choose_creature_type_per_player() {
    let parsed = parsed_grave_sifter();
    let trigger = parsed.triggers.first().expect("ETB trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::Choose {
                choice_type,
                persist: true,
                ..
            } if matches!(choice_type, ChoiceType::CreatureType { .. })
        ),
        "top effect must be persisting CreatureType Choose, got {:?}",
        execute.effect
    );
    assert_eq!(
        execute.player_scope,
        Some(PlayerFilter::All),
        "each player chooses must set player_scope: All"
    );
    let sub = execute.sub_ability.as_ref().expect("return sub");
    if let Effect::ChangeZone {
        target,
        origin,
        up_to,
        ..
    } = sub.effect.as_ref()
    {
        assert_eq!(
            *origin,
            Some(Zone::Graveyard),
            "graveyard return must set origin"
        );
        assert!(*up_to, "any number of cards must set up_to on ChangeZone");
        if let TargetFilter::Typed(tf) = target {
            assert!(
                tf.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::IsChosenCreatureType)),
                "creature-type choice must filter with IsChosenCreatureType, got {:?}",
                tf.properties
            );
        } else {
            panic!("expected Typed target on graveyard return, got {target:?}");
        }
    } else {
        panic!(
            "sub must return graveyard cards to hand, got {:?}",
            sub.effect
        );
    }
}

#[test]
fn grave_sifter_etb_prompts_creature_type_choice() {
    let (mut runner, grave_sifter, _bears, _hill_giant) = setup_grave_sifter_cast();

    runner.cast(grave_sifter).resolve();

    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(
                *player, P0,
                "APNAP: active player chooses creature type first"
            );
            assert!(
                matches!(choice_type, ChoiceType::CreatureType { .. }),
                "prompt must be creature type choice, got {choice_type:?}"
            );
            assert!(
                options.iter().any(|o| o.eq_ignore_ascii_case("Bear")),
                "Bear must be a legal creature type option: {options:?}"
            );
        }
        other => {
            panic!("Grave Sifter ETB must surface NamedChoice for creature type, got {other:?}")
        }
    }
}

#[test]
fn grave_sifter_each_player_creature_type_return_from_graveyard() {
    let (mut runner, grave_sifter, bears, hill_giant) = setup_grave_sifter_cast();

    runner.cast(grave_sifter).resolve();

    runner
        .act(GameAction::ChooseOption {
            choice: "Bear".to_string(),
        })
        .expect("P0 chooses Bear");

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            up_to,
            ..
        } => {
            assert_eq!(*player, P0, "P0 must choose cards from their graveyard");
            assert!(cards.contains(&bears), "Grizzly Bears must be eligible");
            assert!(*up_to, "any number of cards must allow up-to selection");
        }
        other => panic!(
            "after P0 chooses Bear, expected EffectZoneChoice for graveyard return, got {other:?}"
        ),
    }

    runner
        .act(GameAction::SelectCards { cards: vec![bears] })
        .expect("P0 returns Grizzly Bears");

    match &runner.state().waiting_for {
        WaitingFor::NamedChoice { player, .. } => {
            assert_eq!(*player, P1, "APNAP: opponent chooses creature type next");
        }
        other => panic!("expected P1 NamedChoice after P0 return, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseOption {
            choice: "Giant".to_string(),
        })
        .expect("P1 chooses Giant");

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "P1 resolves their own return instruction");
            assert_eq!(
                cards,
                &vec![hill_giant],
                "P1's Giant choice must not reuse P0's Bear choice"
            );
        }
        other => panic!("expected P1's Giant return choice, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![hill_giant],
        })
        .expect("P1 returns Hill Giant");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Hand,
        "Grizzly Bears must return to P0's hand"
    );
    assert_eq!(
        runner.state().objects[&hill_giant].zone,
        Zone::Hand,
        "Hill Giant must return to P1's hand"
    );
}
