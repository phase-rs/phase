//! Regression for issue #2909 — Blast-Furnace Hellkite Artifact offering and
//! "creatures attacking your opponents have double strike."
//!
//! https://github.com/phase-rs/phase/issues/2909

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{keyword_display_name, parse_oracle_text};
use engine::types::ability::{
    ContinuousModification, ControllerRef, FilterProp, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const BLAST_FURNACE_ORACLE: &str = "\
Artifact offering (You may cast this spell as though it had flash by sacrificing an artifact and paying the difference in mana costs between this and the sacrificed artifact. Mana cost includes color.)\n\
Flying, double strike\n\
Creatures attacking your opponents have double strike.";

fn parse_blast_furnace() -> engine::parser::oracle::ParsedAbilities {
    let keywords = [Keyword::Flying, Keyword::DoubleStrike];
    let keyword_names: Vec<String> = keywords.iter().map(keyword_display_name).collect();
    parse_oracle_text(
        BLAST_FURNACE_ORACLE,
        "Blast-Furnace Hellkite",
        &keyword_names,
        &["Creature".to_string()],
        &["Dragon".to_string()],
    )
}

#[test]
fn blast_furnace_hellkite_parses_artifact_offering_and_attacking_opponents_static() {
    let parsed = parse_blast_furnace();

    assert!(
        parsed
            .extracted_keywords
            .iter()
            .any(|k| matches!(k, Keyword::Offering(q) if q == "Artifact")),
        "expected Artifact offering keyword, got {:?}",
        parsed.extracted_keywords
    );

    let static_def = parsed
        .statics
        .iter()
        .find(|s| {
            matches!(s.mode, StaticMode::Continuous)
                && s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::DoubleStrike
                        }
                    )
                })
        })
        .expect("expected continuous double strike static");

    assert_eq!(
        static_def.affected,
        Some(TargetFilter::Typed(TypedFilter::creature().properties(
            vec![FilterProp::Attacking {
                defender: Some(ControllerRef::Opponent)
            }]
        ),))
    );

    assert!(
        !parsed.abilities.iter().any(|a| {
            matches!(
                a.effect.as_ref(),
                engine::types::ability::Effect::Unimplemented { name, .. }
                    if name == "static_structure"
            )
        }),
        "static line must not leak Unimplemented, got abilities: {:?}",
        parsed.abilities
    );
}

#[test]
fn blast_furnace_hellkite_grants_double_strike_to_creature_attacking_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Blast-Furnace Hellkite", 5, 5, BLAST_FURNACE_ORACLE);
    let attacker = scenario.add_creature(P0, "Raider", 2, 2).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attacker at opponent");

    evaluate_layers(runner.state_mut());

    let raider = runner.state().objects.get(&attacker).expect("attacker");
    assert!(
        raider
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::DoubleStrike)),
        "creature attacking an opponent should gain double strike from Hellkite, got {:?}",
        raider.keywords
    );
}

#[test]
fn blast_furnace_hellkite_offering_prompts_artifact_sacrifice() {
    let _parsed = parse_blast_furnace();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for _ in 0..9 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }

    let artifact_id = scenario.add_creature(P0, "Sacrifice Me", 0, 0).id();

    let hellkite = scenario
        .add_creature_to_hand(P0, "Blast-Furnace Hellkite", 5, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 7,
        })
        .with_keyword(Keyword::Offering("Artifact".to_string()))
        .id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&artifact_id).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes.clear();
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
    }
    let card_id = runner.state().objects[&hellkite].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: hellkite,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin Hellkite cast");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "Artifact offering should prompt optional sacrifice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept Artifact offering");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "expected sacrifice target selection for offering, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![artifact_id],
        })
        .expect("sacrifice artifact for offering");

    assert_ne!(
        runner.state().objects[&hellkite].zone,
        Zone::Graveyard,
        "offering sacrifice alone must not fizzle the cast"
    );
}
