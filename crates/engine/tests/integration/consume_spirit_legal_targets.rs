//! Integration tests for Consume Spirit legal targets (CR 115.4) and payment restrictions (CR 601.2b, CR 601.2h).
//!
//! CR 115.4 & CR Glossary: "any target" refers to a creature, player, planeswalker,
//! or battle. Other game objects (noncreature artifacts, noncreature enchantments,
//! lands, stack spells) cannot be chosen.
//!
//! CR 601.2b + CR 601.2h: "Spend only black mana on X." Mana spent to pay the {X}
//! portion of the cost must be black mana. Non-black mana can pay the generic portion
//! of the printed cost ({1}), but cannot be spent on X.
//!
//! Oracle:
//! "Spend only black mana on X.
//! Consume Spirit deals X damage to any target and you gain X life."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::targeting;
use engine::game::zones::create_object;
use engine::types::ability::{
    Effect, FilterProp, QuantityExpr, StaticCondition, StaticDefinition, TargetFilter, TargetRef,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, CastingVariant, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{FlashbackCost, Keyword};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;

const CONSUME_SPIRIT_ORACLE: &str =
    "Spend only black mana on X.\nConsume Spirit deals X damage to any target and you gain X life.";

fn black_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Black, ObjectId(9_999), false, vec![]); count]
}

fn red_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(9_998), false, vec![]); count]
}

fn green_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Green, ObjectId(9_996), false, vec![]); count]
}

fn colorless_pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_997), false, vec![]); count]
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

    // 1. Generic TargetFilter::Any enumeration (targeting::find_legal_targets) includes all battlefield objects + players.
    let generic_targets =
        targeting::find_legal_targets(state, &engine::types::ability::TargetFilter::Any, P0, spell);
    assert!(generic_targets.contains(&TargetRef::Player(P0)));
    assert!(generic_targets.contains(&TargetRef::Player(P1)));
    assert!(generic_targets.contains(&TargetRef::Object(creature)));
    assert!(generic_targets.contains(&TargetRef::Object(planeswalker)));
    assert!(generic_targets.contains(&TargetRef::Object(battle)));
    assert!(generic_targets.contains(&TargetRef::Object(land)));
    assert!(generic_targets.contains(&TargetRef::Object(artifact)));
    assert!(generic_targets.contains(&TargetRef::Object(enchantment)));

    // 2. Consume Spirit target slot building (build_target_slots) narrows "any target" damage slot to CR 115.4 domain.
    let ability = &state.objects[&spell].abilities[0];
    let resolved =
        engine::types::ability::ResolvedAbility::new(*ability.effect.clone(), vec![], spell, P0);
    let slots = engine::game::ability_utils::build_target_slots(state, &resolved)
        .expect("Consume Spirit target slots must build");
    let legal_targets = &slots[0].legal_targets;

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

    let r2 = if matches!(r1.waiting_for, WaitingFor::ChooseXValue { .. }) {
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("ChooseX should succeed")
    } else {
        r1
    };

    assert!(
        matches!(r2.waiting_for, WaitingFor::TargetSelection { .. }),
        "Expected TargetSelection, got {:?}",
        r2.waiting_for
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

    let r2 = if matches!(r1.waiting_for, WaitingFor::ChooseXValue { .. }) {
        runner
            .act(GameAction::ChooseX { value: 2 })
            .expect("ChooseX should succeed")
    } else {
        r1
    };

    assert!(
        matches!(r2.waiting_for, WaitingFor::TargetSelection { .. }),
        "Expected TargetSelection, got {:?}",
        r2.waiting_for
    );

    let result = runner.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(artifact)),
    });
    assert!(
        result.is_err(),
        "Targeting Sol Ring (Artifact) for Consume Spirit must be rejected as an illegal target; got {result:?}"
    );
}

#[test]
fn consume_spirit_rejects_paying_x_with_non_black_mana() {
    // CR 601.2b / CR 601.2h: "Spend only black mana on X."
    // Consume Spirit cost is {X}{1}{B}.
    // With X=3, total cost is 5 mana: 1 generic, 1 {B}, 3 {B} for X (total 4 Black + 1 generic).
    // If player has 3 Black mana and 2 Red mana (total 5 mana):
    // 1 Black pays {B}, leaving only 2 Black for X (which needs 3 Black).
    // The Red mana cannot pay for X, so payment must be rejected.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = black_pool(3);
    pool.extend(red_pool(2));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();

    let result = runner.cast(spell).x(3).target_player(P1).try_resolve();
    assert!(
        result.is_err(),
        "Paying for X with non-black mana must be rejected"
    );
}

#[test]
fn consume_spirit_allows_paying_generic_portion_with_non_black_mana() {
    // CR 601.2b / CR 601.2h: "Spend only black mana on X."
    // Consume Spirit cost is {X}{1}{B}.
    // With X=3, total cost is 4 Black + 1 generic.
    // If player has 4 Black mana and 1 Colorless/Red mana:
    // 4 Black pays {B} + 3 {B} on X, and 1 Colorless pays {1} generic.
    // This must succeed.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let mut pool = black_pool(4);
    pool.extend(colorless_pool(1));
    scenario.with_mana_pool(P0, pool);

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
fn consume_spirit_with_generic_cost_reduction() {
    // CR 601.2b, CR 601.2f, CR 601.2h:
    // Consume Spirit cost is {X}{1}{B}. With X=2, base cost is {2}{1}{B} = {3}{B}.
    // A {2} generic reduction reduces the total cost to {1}{B}.
    // 1 generic is unrestricted, and 1 {B} is black.
    // The player should be able to cast with 1 Black mana and 1 Colorless mana.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    scenario
        .add_creature(P0, "Helm of Awakening", 0, 1)
        .with_static_definition(StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::generic(2),
            spell_filter: None,
            dynamic_count: None,
            reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
        }));

    let mut pool = black_pool(1);
    pool.extend(colorless_pool(1));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).x(2).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -2);
    outcome.assert_life_delta(P0, 2);
}

#[test]
fn consume_spirit_with_colored_cost_reduction() {
    // CR 601.2b, CR 601.2f, CR 601.2h:
    // Consume Spirit cost is {X}{1}{B}. With X=2, base cost is {2}{1}{B} = {3}{B}.
    // A {B} colored reduction reduces the {B} pip, leaving {3} generic (2 restricted to Black for X, 1 unrestricted).
    // Player with 2 Black and 1 Colorless can pay the cost.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    scenario
        .add_creature(P0, "Jet Medallion", 0, 1)
        .with_static_definition(StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            },
            spell_filter: None,
            dynamic_count: None,
            reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
        }));

    let mut pool = black_pool(2);
    pool.extend(colorless_pool(1));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).x(2).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -2);
    outcome.assert_life_delta(P0, 2);
}

#[test]
fn soul_burn_accepts_black_and_red_mana_for_x_and_rejects_green() {
    // CR 601.2b, CR 601.2h: "Spend only black and/or red mana on X."
    // Soul Burn cost is {X}{2}{R}.
    // With X=2, total cost is 4 generic + 1 Red (2 generic restricted to Black/Red for X, 2 generic unrestricted).
    const SOUL_BURN_ORACLE: &str =
        "Spend only black and/or red mana on X.\nSoul Burn deals X damage to any target and you gain X life.";

    // 1. Paying with 2 Black on X + 2 Colorless generic + 1 Red pip -> Success
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 20);
        let mut pool = black_pool(2);
        pool.extend(colorless_pool(2));
        pool.extend(red_pool(1));
        scenario.with_mana_pool(P0, pool);

        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Soul Burn", false, SOUL_BURN_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 2,
            })
            .id();

        let mut runner = scenario.build();
        let outcome = runner.cast(spell).x(2).target_player(P1).resolve();
        outcome.assert_life_delta(P1, -2);
        outcome.assert_life_delta(P0, 2);
    }

    // 2. Paying with 2 Red on X + 2 Colorless generic + 1 Red pip (3 Red + 2 Colorless) -> Success
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 20);
        let mut pool = red_pool(3);
        pool.extend(colorless_pool(2));
        scenario.with_mana_pool(P0, pool);

        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Soul Burn", false, SOUL_BURN_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 2,
            })
            .id();

        let mut runner = scenario.build();
        let outcome = runner.cast(spell).x(2).target_player(P1).resolve();
        outcome.assert_life_delta(P1, -2);
        outcome.assert_life_delta(P0, 2);
    }

    // 3. Paying with 1 Black + 1 Red on X + 2 Colorless generic + 1 Red pip -> Success
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        scenario.with_life(P1, 20);
        let mut pool = black_pool(1);
        pool.extend(red_pool(2));
        pool.extend(colorless_pool(2));
        scenario.with_mana_pool(P0, pool);

        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Soul Burn", false, SOUL_BURN_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 2,
            })
            .id();

        let mut runner = scenario.build();
        let outcome = runner.cast(spell).x(2).target_player(P1).resolve();
        outcome.assert_life_delta(P1, -2);
        outcome.assert_life_delta(P0, 2);
    }

    // 4. Trying to pay for X with 2 Green mana + 2 Colorless generic + 1 Red pip -> Rejected
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut pool = green_pool(2);
        pool.extend(colorless_pool(2));
        pool.extend(red_pool(1));
        scenario.with_mana_pool(P0, pool);

        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Soul Burn", false, SOUL_BURN_ORACLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 2,
            })
            .id();

        let mut runner = scenario.build();
        let result = runner.cast(spell).x(2).target_player(P1).try_resolve();
        assert!(
            result.is_err(),
            "Paying for X in Soul Burn with green mana must be rejected"
        );
    }
}

#[test]
fn pipeline_non_damage_any_target_allows_artifacts_and_lands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless_pool(2));

    let aura1 = scenario
        .add_spell_to_hand(P0, "Enchant Land", false)
        .with_mana_cost(ManaCost::generic(1))
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Any))
        .id();

    let aura2 = scenario
        .add_spell_to_hand(P0, "Enchant Artifact", false)
        .with_mana_cost(ManaCost::generic(1))
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Any))
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let land = add_permanent(state, 104, P1, "Island", CoreType::Land);
    let artifact = add_permanent(state, 105, P1, "Sol Ring", CoreType::Artifact);

    let outcome1 = runner.cast(aura1).target_object(land).resolve();
    assert!(
        outcome1.state().battlefield.contains(&aura1),
        "Aura must resolve onto battlefield"
    );
    assert_eq!(
        outcome1.state().objects[&aura1].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(land)),
        "Aura must be attached to land"
    );

    let outcome2 = runner.cast(aura2).target_object(artifact).resolve();
    assert!(
        outcome2.state().battlefield.contains(&aura2),
        "Aura must resolve onto battlefield"
    );
    assert_eq!(
        outcome2.state().objects[&aura2].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(artifact)),
        "Aura must be attached to artifact"
    );
}

#[test]
fn pipeline_damage_any_target_narrows_while_generic_any_remains_broad() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless_pool(10));
    scenario.with_life(P1, 20);

    let spell = scenario
        .add_spell_to_hand(P0, "Direct Bolt", false)
        .with_mana_cost(ManaCost::generic(1))
        .with_ability(Effect::DealDamage {
            target: TargetFilter::Any,
            amount: QuantityExpr::Fixed { value: 3 },
            damage_source: None,
            excess: None,
        })
        .id();

    let mut runner = scenario.build();
    let land = add_permanent(runner.state_mut(), 104, P1, "Island", CoreType::Land);
    let artifact = add_permanent(runner.state_mut(), 105, P1, "Sol Ring", CoreType::Artifact);

    // 1. Assert damage Any target excludes land and artifact per CR 115.4
    let slots = engine::game::casting::legal_target_slots_for_castable_spell(runner.state(), spell);
    assert_eq!(slots.len(), 1);
    assert!(
        !slots[0].legal_targets.contains(&TargetRef::Object(land)),
        "Targeting land with damage Any target must be rejected per CR 115.4"
    );
    assert!(
        !slots[0]
            .legal_targets
            .contains(&TargetRef::Object(artifact)),
        "Targeting artifact with damage Any target must be rejected per CR 115.4"
    );
    assert!(
        slots[0].legal_targets.contains(&TargetRef::Player(P1)),
        "Targeting player with damage Any target must be legal per CR 115.4"
    );

    // 2. Targeting player succeeds and deals damage
    let outcome = runner.cast(spell).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -3);
}

#[test]
fn pipeline_damage_bare_another_target_narrows_to_cr_115_4() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless_pool(10));
    scenario.with_life(P1, 20);
    let creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let bare_another =
        TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Another]));
    let spell = scenario
        .add_spell_to_hand(P0, "Damage Another", false)
        .with_mana_cost(ManaCost::generic(1))
        .with_ability(Effect::DealDamage {
            target: bare_another,
            amount: QuantityExpr::Fixed { value: 2 },
            damage_source: None,
            excess: None,
        })
        .id();

    let mut runner = scenario.build();
    let land = add_permanent(runner.state_mut(), 104, P1, "Island", CoreType::Land);
    let artifact = add_permanent(runner.state_mut(), 105, P1, "Sol Ring", CoreType::Artifact);

    // 1. Assert damage bare Another target excludes land and artifact per CR 115.4
    let slots = engine::game::casting::legal_target_slots_for_castable_spell(runner.state(), spell);
    assert_eq!(slots.len(), 1);
    assert!(
        !slots[0].legal_targets.contains(&TargetRef::Object(land)),
        "Targeting land with damage bare Another target must be rejected per CR 115.4"
    );
    assert!(
        !slots[0]
            .legal_targets
            .contains(&TargetRef::Object(artifact)),
        "Targeting artifact with damage bare Another target must be rejected per CR 115.4"
    );
    assert!(
        slots[0]
            .legal_targets
            .contains(&TargetRef::Object(creature)),
        "Targeting creature with damage bare Another target must be legal per CR 115.4"
    );

    // 2. Targeting creature succeeds
    let outcome = runner.cast(spell).target_object(creature).resolve();
    assert!(
        !outcome.state().battlefield.contains(&creature),
        "Target creature must die from 2 damage"
    );
}

#[test]
fn consume_spirit_with_colored_reduction_spillover() {
    // CR 601.2b, CR 601.2f, CR 601.2h, CR 118.7b/c/d:
    // Consume Spirit cost is {X}{1}{B}. With X=2, base cost is {2}{1}{B} = {3}{B}.
    // A {B}{B} colored reduction reduces the single {B} shard, and the second {B}
    // spills over to reduce generic mana by 1.
    // Total cost becomes 2 generic (0 Black pips).
    // The 1 spillover generic reduction reduces the X requirement by 1:
    // So 1 generic is restricted to Black for X, and 1 generic is unrestricted base generic.
    // The player can pay with 1 Black mana and 1 Colorless mana.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    scenario
        .add_creature(P0, "Double Black Reducer", 0, 1)
        .with_static_definition(StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost {
                shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                generic: 0,
            },
            spell_filter: None,
            dynamic_count: None,
            reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
        }));

    let mut pool = black_pool(1);
    pool.extend(colorless_pool(1));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).x(2).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -2);
    outcome.assert_life_delta(P0, 2);
}

#[test]
fn consume_spirit_with_variant_gated_reduction() {
    // CR 601.2b, CR 601.2f, CR 601.2h:
    // Consume Spirit cast from graveyard via Flashback with X=2.
    // Base cost {X}{1}{B} with X=2 is {3}{B}.
    // A reducer gated on StaticCondition::CastingAsVariant { variant: Flashback } reduces generic by {2}.
    // When cast as Flashback, generic reduction of 2 reduces X by 2, leaving cost {1}{B} (1 unrestricted generic, 1 {B}).
    // Player can pay with 1 Black mana and 1 Colorless mana.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    scenario
        .add_creature(P0, "Flashback Reducer", 0, 1)
        .with_static_definition(
            StaticDefinition::new(StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::generic(2),
                spell_filter: None,
                dynamic_count: None,
                reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
            })
            .condition(StaticCondition::CastingAsVariant {
                variant: CastingVariant::Flashback,
            }),
        );

    let mut pool = black_pool(1);
    pool.extend(colorless_pool(1));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_graveyard(P0, "Consume Spirit", false)
        .from_oracle_text(CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .with_keyword(Keyword::Flashback(FlashbackCost::Mana(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })))
        .id();

    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .x(2)
        .target_player(P1)
        .resolve();
    outcome.assert_life_delta(P1, -2);
    outcome.assert_life_delta(P0, 2);
}

#[test]
fn consume_spirit_with_cost_increase_tax_payable_with_unrestricted_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // Thalia-style tax (+{1} cost increase):
    // Consume Spirit base {1}{B} + X=1 {1} + Tax {1} = {3}{B} (4 mana total).
    // Restricted X count = 1 (must be Black).
    // Unrestricted generic = 2 (1 base + 1 tax, can be Colorless).
    // Mana pool: 2 Black (1 for {B}, 1 for X) + 2 Colorless (for {2} unrestricted).
    let mut pool = black_pool(2);
    pool.extend(colorless_pool(2));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let taxer = add_permanent(state, 301, P1, "Thalia Guardian", CoreType::Creature);
    state
        .objects
        .get_mut(&taxer)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Raise,
            amount: ManaCost::generic(1),
            spell_filter: None,
            dynamic_count: None,
            reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
        }));

    let outcome = runner.cast(spell).x(1).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -1);
    outcome.assert_life_delta(P0, 1);
}

#[test]
fn consume_spirit_with_cost_floor_min_3_payable_with_unrestricted_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // Trinisphere floor (min 3 mana):
    // Consume Spirit with X=0: base {1}{B} (2 mana) is floored to {2}{B} (3 mana total).
    // Restricted X count = 0.
    // Unrestricted generic = 2 (1 base + 1 floor, can be Colorless).
    // Mana pool: 1 Black (for {B}) + 2 Colorless (for {2} unrestricted).
    let mut pool = black_pool(1);
    pool.extend(colorless_pool(2));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let trini = add_permanent(state, 302, P1, "Trinisphere", CoreType::Artifact);
    state
        .objects
        .get_mut(&trini)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Minimum,
            amount: ManaCost::generic(3),
            spell_filter: None,
            dynamic_count: None,
            reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
        }));

    let outcome = runner.cast(spell).x(0).target_player(P1).resolve();
    outcome.assert_life_delta(P1, 0);
    outcome.assert_life_delta(P0, 0);
}

#[test]
fn consume_spirit_with_cost_floor_min_3_with_x1() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    // Trinisphere floor (min 3 mana):
    // Consume Spirit with X=1: base {1}{B} + X=1 {1} = {2}{B} (3 mana total, meets floor).
    // Restricted X count = 1 (must be Black).
    // Unrestricted generic = 1 (1 base, can be Colorless).
    // Mana pool: 2 Black (1 for {B}, 1 for X) + 1 Colorless (for {1} unrestricted).
    let mut pool = black_pool(2);
    pool.extend(colorless_pool(1));
    scenario.with_mana_pool(P0, pool);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consume Spirit", false, CONSUME_SPIRIT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    let trini = add_permanent(state, 302, P1, "Trinisphere", CoreType::Artifact);
    state
        .objects
        .get_mut(&trini)
        .unwrap()
        .static_definitions
        .push(StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Minimum,
            amount: ManaCost::generic(3),
            spell_filter: None,
            dynamic_count: None,
            reach: engine::types::statics::CostReductionReach::SpillsToGeneric,
        }));

    let outcome = runner.cast(spell).x(1).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -1);
    outcome.assert_life_delta(P0, 1);
}
