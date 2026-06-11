//! Issue #1604: Lurking Predators — optional bottom branch and creature reveal path.
//!
//! https://github.com/phase-rs/phase/issues/1604
//!
//! Oracle: "Whenever an opponent casts a spell, reveal the top card of your library.
//! If it's a creature card, put it onto the battlefield. Otherwise, you may put that
//! card on the bottom of your library."

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LURKING_PREDATORS: &str = "Whenever an opponent casts a spell, reveal the top card of your library. If it's a creature card, put it onto the battlefield. Otherwise, you may put that card on the bottom of your library.";

fn load_test_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json");
        CardDatabase::from_mtgjson(&path).expect("test fixture database should load")
    })
}

fn zone_of(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object exists").zone
}

fn cast_creature_from_hand(runner: &mut engine::game::scenario::GameRunner, hand_card: ObjectId) {
    let card_id = runner
        .state()
        .objects
        .get(&hand_card)
        .expect("hand card exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: hand_card,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should succeed");
}

fn put_library_top(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let zone = runner.state().objects.get(&id).expect("object").zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner
            .state_mut()
            .players
            .get_mut(owner.0 as usize)
            .unwrap()
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

#[test]
fn lurking_predators_puts_creature_from_library_top_when_opponent_casts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Lurking Predators", 0, 0)
        .as_enchantment()
        .from_oracle_text(LURKING_PREDATORS);

    let library_creature = scenario.add_creature(P0, "Library Bear", 2, 2).id();
    let opponent_spell = scenario
        .add_creature_to_hand(P1, "Opponent Bear", 2, 2)
        .id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, library_creature);

    assert!(
        runner
            .state()
            .objects
            .get(&library_creature)
            .unwrap()
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "precondition: library top is a creature card"
    );

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // P1 casts a spell → P0's Lurking Predators reveals the top card. A creature
    // top takes the mandatory battlefield branch, so the resolution driver never
    // surfaces the optional bottom prompt.
    let outcome = runner.cast(opponent_spell).resolve();

    outcome.assert_zone(&[library_creature], Zone::Battlefield);
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "mandatory creature branch must not leave an optional prompt pending"
    );
}

#[test]
fn lurking_predators_optional_bottom_moves_noncreature_when_accepted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Lurking Predators", 0, 0)
        .as_enchantment()
        .from_oracle_text(LURKING_PREDATORS);

    let library_land = scenario
        .add_creature(P0, "Library Land", 0, 0)
        .as_enchantment()
        .id();
    let opponent_spell = scenario
        .add_creature_to_hand(P1, "Opponent Bear", 2, 2)
        .id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, library_land);

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    cast_creature_from_hand(&mut runner, opponent_spell);

    let mut guard = 0;
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        guard += 1;
        assert!(guard < 40, "stack did not reach optional prompt");
        runner.pass_both_players();
    }

    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => assert_eq!(*player, P0),
        other => panic!("expected OptionalEffectChoice for bottom branch, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional bottom");

    runner.advance_until_stack_empty();

    let lib = &runner.state().players[P0.0 as usize].library;
    assert_eq!(
        lib.last().copied(),
        Some(library_land),
        "accepted optional must put the revealed noncreature on the bottom"
    );
    assert_eq!(zone_of(&runner, library_land), Zone::Library);
}

/// Regression for mis-filed library objects (name only, no `CoreType::Creature`):
/// the creature branch must not fall through to the optional bottom prompt.
#[test]
fn lurking_predators_creature_branch_requires_card_types_on_library_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Lurking Predators", 0, 0)
        .as_enchantment()
        .from_oracle_text(LURKING_PREDATORS);

    let library_creature = scenario.add_card_to_library_top(P0, "Grizzly Bear");
    let opponent_spell = scenario
        .add_creature_to_hand(P1, "Opponent Bear", 2, 2)
        .id();

    let mut runner = scenario.build();
    assert!(
        !runner
            .state()
            .objects
            .get(&library_creature)
            .unwrap()
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "precondition: generic library placement has no creature type"
    );

    let db = load_test_db();
    let grizzly = db
        .get_face_by_name("Grizzly Bears")
        .expect("fixture has Grizzly Bears");
    runner.state_mut().card_face_registry = Arc::new(HashMap::from([(
        "grizzly bear".to_string(),
        grizzly.clone(),
    )]));

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let outcome = runner.cast(opponent_spell).resolve();

    outcome.assert_zone(&[library_creature], Zone::Battlefield);
}

#[test]
fn lurking_predators_parsed_trigger_chain_shape() {
    let parsed = parse_oracle_text(
        LURKING_PREDATORS,
        "Lurking Predators",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let trigger = parsed.triggers.first().expect("expected one trigger");
    let execute = trigger.execute.as_ref().expect("trigger must have execute");
    assert!(
        !execute.optional,
        "trigger head must not be optional; only the otherwise branch may be"
    );
    assert!(matches!(*execute.effect, Effect::RevealTop { .. }));
    let conditional = execute
        .sub_ability
        .as_ref()
        .expect("RevealTop must chain to conditional sub");
    assert!(matches!(
        conditional.condition.as_ref(),
        Some(AbilityCondition::RevealedHasCardType {
            card_types,
            ..
        }) if card_types.as_slice() == [CoreType::Creature]
    ));
    assert!(matches!(
        *conditional.effect,
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            ..
        }
    ));
    let else_branch = conditional
        .else_ability
        .as_ref()
        .expect("otherwise bottom branch");
    assert!(
        else_branch.optional,
        "otherwise 'you may put on bottom' must be optional"
    );
    assert!(matches!(
        *else_branch.effect,
        Effect::PutAtLibraryPosition {
            position: engine::types::ability::LibraryPosition::Bottom,
            ..
        }
    ));
}
