//! Integration tests for Consume Spirit legal targets and CR 115.4.
//!
//! CR 115.4 & CR Glossary: "any target" refers to a creature, player, planeswalker,
//! or battle. Other game objects (noncreature artifacts, noncreature enchantments,
//! lands, stack spells) cannot be chosen.
//!
//! Oracle:
//! "Spend only black mana on X.
//! Consume Spirit deals X damage to any target and you gain X life."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::targeting;
use engine::game::zones::create_object;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CONSUME_SPIRIT_ORACLE: &str =
    "Spend only black mana on X.\nConsume Spirit deals X damage to any target and you gain X life.";

fn black_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Black, ObjectId(9_999), false, vec![]); count]
}

fn add_permanent(
    state: &mut engine::types::game_state::GameState,
    cid: u32,
    controller: PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(cid.into()),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(core_type);
    id
}

#[test]
fn consume_spirit_legal_targets_enumeration() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let creature = add_permanent(state, 101, P1, "Grizzly Bears", CoreType::Creature);
    let planeswalker = add_permanent(state, 102, P1, "Jace Beleren", CoreType::Planeswalker);
    let battle = add_permanent(state, 103, P1, "Invasion of Gobakhan", CoreType::Battle);
    let land = add_permanent(state, 104, P1, "Island", CoreType::Land);
    let artifact = add_permanent(state, 105, P1, "Sol Ring", CoreType::Artifact);
    let enchantment = add_permanent(state, 106, P1, "Blood Moon", CoreType::Enchantment);

    let ability = &state.objects[&spell].abilities[0];
    let filter = ability
        .effect
        .target_filter()
        .expect("Consume Spirit must have a target filter");

    let legal_targets = targeting::find_legal_targets(state, filter, P0, spell);

    // CR 115.4: legal targets include creatures, players, planeswalkers, and battles.
    assert!(legal_targets.contains(&TargetRef::Player(P0)));
    assert!(legal_targets.contains(&TargetRef::Player(P1)));
    assert!(legal_targets.contains(&TargetRef::Object(creature)));
    assert!(legal_targets.contains(&TargetRef::Object(planeswalker)));
    assert!(legal_targets.contains(&TargetRef::Object(battle)));

    // Non-legal targets per CR 115.4: lands, artifacts, enchantments.
    assert!(
        !legal_targets.contains(&TargetRef::Object(land)),
        "Island (Land) must not be a legal target for Consume Spirit"
    );
    assert!(
        !legal_targets.contains(&TargetRef::Object(artifact)),
        "Sol Ring (Artifact) must not be a legal target for Consume Spirit"
    );
    assert!(
        !legal_targets.contains(&TargetRef::Object(enchantment)),
        "Blood Moon (Enchantment) must not be a legal target for Consume Spirit"
    );

    assert_eq!(legal_targets.len(), 5);
}

#[test]
fn consume_spirit_resolves_against_opponent_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_mana_pool(P0, black_pool(6));

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).x(3).target_player(P1).resolve();

    outcome.assert_life_delta(P1, -3);
    outcome.assert_life_delta(P0, 3);
}

#[test]
fn consume_spirit_resolves_against_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_mana_pool(P0, black_pool(5));

    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).x(2).target_object(bear).resolve();

    outcome.assert_life_delta(P0, 2);
    assert!(
        !outcome.state().battlefield.contains(&bear),
        "Target creature with 2 toughness should die after taking 2 damage"
    );
}

#[test]
fn consume_spirit_rejects_illegal_target_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black_pool(5));

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let land = add_permanent(runner.state_mut(), 104, P1, "Island", CoreType::Land);
    let card_id = runner.state().objects[&spell].card_id;

    let r1 = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Cast announcement should succeed");

    assert!(
        matches!(r1.waiting_for, WaitingFor::TargetSelection { .. }),
        "Expected TargetSelection, got {:?}",
        r1.waiting_for
    );

    let result = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(land)),
    });
    assert!(
        result.is_err(),
        "Targeting Island (Land) for Consume Spirit must be rejected as an illegal target; got {result:?}"
    );
}

#[test]
fn consume_spirit_rejects_illegal_target_artifact() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black_pool(5));

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let artifact = add_permanent(runner.state_mut(), 105, P1, "Sol Ring", CoreType::Artifact);
    let card_id = runner.state().objects[&spell].card_id;

    let r1 = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Cast announcement should succeed");

    assert!(
        matches!(r1.waiting_for, WaitingFor::TargetSelection { .. }),
        "Expected TargetSelection, got {:?}",
        r1.waiting_for
    );

    let result = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(artifact)),
    });
    assert!(
        result.is_err(),
        "Targeting Sol Ring (Artifact) for Consume Spirit must be rejected as an illegal target; got {result:?}"
    );
}
