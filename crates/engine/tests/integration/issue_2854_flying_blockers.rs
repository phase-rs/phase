//! Regression for issue #2854: flying attackers must not be blockable by
//! creatures without flying or reach (Wren's Run Packmaster wolf tokens vs
//! flying insect tokens).
//!
//! https://github.com/phase-rs/phase/issues/2854

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityKind, Effect, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BROODSPINNER_ORACLE: &str = "{4}{B}{G}, {T}, Sacrifice this creature: Create a number of 1/1 black and green Insect creature tokens with flying equal to the number of card types among cards in your graveyard.";

fn issue_2854_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/issue_2854_cards.json");
        engine::database::card_db::CardDatabase::from_export(&path)
            .expect("issue_2854_cards.json fixture must load")
    })
}

fn parsed_broodspinner_token_effect() -> Effect {
    let parsed = parse_oracle_text(
        BROODSPINNER_ORACLE,
        "Broodspinner",
        &[],
        &["Creature".to_string()],
        &["Spider".to_string()],
    );
    let token_effect = parsed
        .abilities
        .into_iter()
        .find(|ability| {
            matches!(ability.kind, AbilityKind::Activated)
                && matches!(ability.effect.as_ref(), Effect::Token { .. })
        })
        .expect("Broodspinner activated token ability must parse")
        .effect;

    let mut effect = *token_effect;
    let Effect::Token {
        keywords, count, ..
    } = &mut effect
    else {
        unreachable!("matched Token effect above");
    };
    assert!(
        keywords.contains(&Keyword::Flying),
        "parsed Broodspinner token effect must carry Flying, got {keywords:?}"
    );
    *count = QuantityExpr::Fixed { value: 1 };
    effect
}

fn create_flying_insect(runner: &mut engine::game::scenario::GameRunner) -> ObjectId {
    let source = ObjectId(9001);
    runner.state_mut().objects.insert(
        source,
        engine::game::game_object::GameObject::new(
            source,
            engine::types::identifiers::CardId(9001),
            P0,
            "Token Source".to_string(),
            engine::types::zones::Zone::Battlefield,
        ),
    );
    let ability = engine::types::ability::ResolvedAbility::new(
        parsed_broodspinner_token_effect(),
        vec![],
        source,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::token::resolve(runner.state_mut(), &ability, &mut events).unwrap();
    let insect = *runner
        .state()
        .battlefield
        .back()
        .expect("insect token created");
    runner
        .state_mut()
        .objects
        .get_mut(&insect)
        .expect("insect object exists")
        .summoning_sick = false;
    insect
}

#[test]
fn issue_2854_flying_insect_token_cannot_be_blocked_by_wolf_without_reach() {
    let db = issue_2854_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _packmaster = scenario.add_real_card(P0, "Wren's Run Packmaster", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let insect = create_flying_insect(&mut runner);
    assert!(
        runner.state().objects[&insect].has_keyword(&Keyword::Flying),
        "structured Token effect must stamp Flying on the insect token"
    );

    // Create a deathtouch wolf via Packmaster's activated ability.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));

    let packmaster = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Wren's Run Packmaster")
        .copied()
        .expect("packmaster on battlefield");
    runner.activate(packmaster, 0).resolve();

    let wolf = runner
        .state()
        .battlefield
        .iter()
        .find(|id| {
            runner.state().objects[id].is_token
                && runner.state().objects[*id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("Wolf"))
        })
        .copied()
        .expect("wolf token created");

    assert!(
        runner.state().objects[&wolf].has_keyword(&Keyword::Deathtouch),
        "Wren's Run Packmaster grants deathtouch to wolves"
    );
    assert!(
        !runner.state().objects[&wolf].has_keyword(&Keyword::Reach),
        "wolf token must not have reach"
    );

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(insect, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare flying insect attacker");

    let block_result = runner.act(GameAction::DeclareBlockers {
        assignments: vec![(wolf, insect)],
    });
    assert!(
        block_result.is_err(),
        "CR 702.9b: a wolf without flying or reach must not block a flying attacker; got ok with waiting_for={:?}",
        runner.state().waiting_for
    );
}
