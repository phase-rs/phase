//! CR 111.3: a token has the abilities the spell or ability that created it defines, and no
//! others. Every board is real cards driven through `apply()`.

use engine::game::combat::AttackTarget;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn mana(source: ObjectId, color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, source, false, vec![]))
        .collect()
}

/// P0's tokens with the Spawn subtype, oldest first.
fn spawns(state: &GameState) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token
                && object.controller == P0
                && object.card_types.subtypes.iter().any(|s| s == "Spawn")
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// CR 605.1a: the token's whole ability list is one mana ability.
fn assert_one_mana_ability(state: &GameState, token: ObjectId) {
    let abilities = &state.objects[&token].abilities;
    assert_eq!(abilities.len(), 1, "{abilities:?}");
    assert!(is_mana_ability(&abilities[0]), "{abilities:?}");
}

/// CR 111.3: the one ability is the static its creating effect defined for the token, not an
/// ability attached from outside that effect.
fn assert_ability_defined_by_the_effect(state: &GameState, token: ObjectId) {
    assert_one_mana_ability(state, token);
    let object = &state.objects[&token];
    assert_eq!(object.base_static_definitions.len(), 1, "{object:?}");
    assert!(object.base_abilities.is_empty(), "{object:?}");
}

/// CR 605.3b: sacrifices `token` for mana; it leaves and P0 has one more {C}.
fn sacrifice_for_colorless(runner: &mut GameRunner, token: ObjectId) {
    let before = runner.state().players[0]
        .mana_pool
        .count_color(ManaType::Colorless);
    runner
        .act(GameAction::ActivateAbility {
            source_id: token,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the token's mana ability");
    let state = runner.state();
    assert!(!state.battlefield.contains(&token));
    assert_eq!(
        state.players[0].mana_pool.count_color(ManaType::Colorless),
        before + 1
    );
}

/// `attacker` attacks P1 alone and its combat damage and the triggers it causes resolve.
fn connect(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    runner.combat_damage();
    runner.advance_until_stack_empty();
}

/// Basking Broodscale: "{1}{G}: Adapt 1. Whenever one or more +1/+1 counters are put on this
/// creature, you may create a 0/1 colorless Eldrazi Spawn creature token with "Sacrifice this
/// token: Add {C}.""
#[test]
fn a_spawn_created_with_a_quoted_ability_has_it_once() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let broodscale = scenario.add_real_card(P0, "Basking Broodscale", Zone::Battlefield, db);
    let mut pool = mana(broodscale, ManaType::Green, 1);
    pool.extend(mana(broodscale, ManaType::Colorless, 1));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner.activate(broodscale, 0).accept_optional().resolve();
    runner.advance_until_stack_empty();

    let state = runner.state();
    let made = spawns(state);
    assert_eq!(made.len(), 1);
    assert_eq!(state.objects[&made[0]].name, "Eldrazi Spawn");
    assert_one_mana_ability(state, made[0]);
}

/// Essence Feed: "Target player loses 3 life. You gain 3 life and create three 0/1 colorless
/// Eldrazi Spawn creature tokens. They have "Sacrifice this token: Add {C}.""
#[test]
fn spawns_given_an_ability_by_a_following_sentence_have_it_once() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let feed = scenario.add_real_card(P0, "Essence Feed", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(feed, ManaType::Black, 6));
    let mut runner = scenario.build();
    runner.cast(feed).target_player(P1).resolve();

    let state = runner.state();
    assert_eq!((state.players[0].life, state.players[1].life), (23, 17));
    let made = spawns(state);
    assert_eq!(made.len(), 3);
    for token in &made {
        assert_one_mana_ability(state, *token);
    }
    sacrifice_for_colorless(&mut runner, made[0]);
}

/// Spawning Pit: "Sacrifice a creature: Put a charge counter on this artifact." / "{1}, Remove
/// two charge counters from this artifact: Create a 2/2 colorless Spawn artifact creature token."
#[test]
fn a_spawn_whose_creating_effect_gives_no_ability_has_none() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pit = scenario.add_real_card(P0, "Spawning Pit", Zone::Battlefield, db);
    let fodder =
        [(); 2].map(|()| scenario.add_real_card(P0, "Goblin Instigator", Zone::Battlefield, db));
    scenario.with_mana_pool(P0, mana(pit, ManaType::Colorless, 1));
    let mut runner = scenario.build();
    for creature in fodder {
        runner.activate(pit, 0).pay_with(&[creature]).resolve();
    }
    runner.activate(pit, 1).resolve();

    let state = runner.state();
    for creature in fodder {
        assert_eq!(state.objects[&creature].zone, Zone::Graveyard);
    }
    assert!(state.objects[&pit].counters.values().all(|n| *n == 0));
    let made = spawns(state);
    assert_eq!(made.len(), 1);
    let token = &state.objects[&made[0]];
    assert_eq!((token.power, token.toughness), (Some(2), Some(2)));
    for core in [CoreType::Artifact, CoreType::Creature] {
        assert!(token.card_types.core_types.contains(&core), "{core:?}");
    }
    assert!(token.abilities.is_empty(), "{:?}", token.abilities);
}

/// CR 707.2: a token copy of a Spawn has the Spawn's one ability once. Cackling Counterpart:
/// "Create a token that's a copy of target creature you control."
#[test]
fn a_token_copy_of_a_spawn_has_its_ability_once() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let feed = scenario.add_real_card(P0, "Essence Feed", Zone::Hand, db);
    let counterpart = scenario.add_real_card(P0, "Cackling Counterpart", Zone::Hand, db);
    let mut pool = mana(feed, ManaType::Black, 6);
    pool.extend(mana(feed, ManaType::Blue, 3));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner.cast(feed).target_player(P1).resolve();
    let originals = spawns(runner.state());
    assert_eq!(originals.len(), 3);
    runner
        .cast(counterpart)
        .target_object(originals[0])
        .resolve();

    let state = runner.state();
    let all = spawns(state);
    assert_eq!(all.len(), 4);
    let copy = *all
        .iter()
        .find(|id| !originals.contains(id))
        .expect("the copy");
    assert_eq!(state.objects[&copy].name, "Eldrazi Spawn");
    assert_one_mana_ability(state, copy);
}

/// Rapacious One: "Trample" / "Whenever this creature deals combat damage to a player, create
/// that many 0/1 colorless Eldrazi Spawn creature tokens. They have "Sacrifice this token: Add
/// {C}.""
#[test]
fn a_separate_they_have_sentence_defines_the_spawns_it_follows() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let one = scenario.add_real_card(P0, "Rapacious One", Zone::Battlefield, db);
    let mut runner = scenario.build();
    connect(&mut runner, one);

    let state = runner.state();
    assert_eq!(state.players[1].life, 15);
    let made = spawns(state);
    assert_eq!(made.len(), 5);
    for token in made {
        assert_ability_defined_by_the_effect(state, token);
    }
}

/// CR 707.2: a creature that is a copy of Rapacious One has its trigger, and that trigger's
/// tokens have the ability the trigger gives them. Mirrorweave: "Each other creature becomes a
/// copy of target nonlegendary creature until end of turn."
#[test]
fn a_copy_of_the_creator_makes_spawns_with_the_ability_its_effect_defines() {
    let db = shared_card_db().expect("the card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let one = scenario.add_real_card(P0, "Rapacious One", Zone::Battlefield, db);
    let instigator = scenario.add_real_card(P0, "Goblin Instigator", Zone::Battlefield, db);
    let weave = scenario.add_real_card(P0, "Mirrorweave", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(weave, ManaType::White, 4));
    let mut runner = scenario.build();
    runner.cast(weave).target_object(one).resolve();
    let copy = &runner.state().objects[&instigator];
    assert_eq!((copy.name.as_str(), copy.power), ("Rapacious One", Some(5)));
    connect(&mut runner, instigator);

    let state = runner.state();
    assert_eq!(state.players[1].life, 15);
    let made = spawns(state);
    assert_eq!(made.len(), 5);
    for token in &made {
        assert_ability_defined_by_the_effect(state, *token);
    }
    sacrifice_for_colorless(&mut runner, made[0]);
}
