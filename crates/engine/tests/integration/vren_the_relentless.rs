//! Vren, the Relentless: the end-step Rat-token trigger counts creatures exiled under
//! opponents' control this turn (Oracle text in `VREN_ORACLE`).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VREN_ORACLE: &str = "Ward {2}\n\
If a creature an opponent controls would die, exile it instead.\n\
At the beginning of each end step, create X 1/1 black Rat creature tokens with \"This token gets +1/+1 for each other Rat you control,\" where X is the number of creatures that were exiled under your opponents' control this turn.";
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";
const SWORDS_ORACLE: &str = "Exile target creature. Its controller gains life equal to its power.";

fn colored_cost(shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic: 0,
    }
}

/// Vren as printed: {2}{U}{B} Legendary Creature — Rat Rogue 3/4.
fn add_vren(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Vren, the Relentless", 3, 4, VREN_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Rat", "Rogue"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 2,
        })
        .id()
}

/// Shock as printed: {R} Instant.
fn add_shock(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .with_mana_cost(colored_cost(ManaCostShard::Red))
        .id()
}

/// Swords to Plowshares as printed: {W} Instant.
fn add_swords(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_ORACLE)
        .with_mana_cost(colored_cost(ManaCostShard::White))
        .id()
}

fn mana_pool(scenario: &mut GameScenario, colors: &[ManaType]) {
    let source = ObjectId(9_999);
    scenario.with_mana_pool(
        P0,
        colors
            .iter()
            .map(|color| ManaUnit::new(*color, source, false, vec![]))
            .collect(),
    );
}

fn rat_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| {
            object.zone == Zone::Battlefield
                && object.is_token
                && object.name.eq_ignore_ascii_case("Rat")
        })
        .map(|(id, _)| *id)
        .collect()
}

/// Walk the real turn structure to the end step and let Vren's trigger resolve.
fn advance_through_end_step_trigger(runner: &mut GameRunner) {
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().phase, Phase::End);
    assert!(
        runner.state().stack.is_empty(),
        "Vren's end-step trigger must finish resolving"
    );
}

#[test]
fn vren_counts_only_creatures_exiled_under_opponents_control_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vren = add_vren(&mut scenario);

    let killed_by_shock = scenario
        .add_creature(P1, "Opponent creature killed", 1, 1)
        .id();
    let exiled_by_spell = scenario
        .add_creature(P1, "Opponent creature exiled", 2, 2)
        .id();
    let mut stolen = scenario.add_creature(P1, "Opponent-owned stolen creature", 2, 2);
    stolen.controlled_by(P0);
    let stolen = stolen.id();

    let shock = add_shock(&mut scenario);
    let swords_one = add_swords(&mut scenario);
    let swords_two = add_swords(&mut scenario);
    mana_pool(
        &mut scenario,
        &[ManaType::Red, ManaType::White, ManaType::White],
    );
    let mut runner = scenario.build();

    runner.cast(shock).target_object(killed_by_shock).resolve();
    runner
        .cast(swords_one)
        .target_object(exiled_by_spell)
        .resolve();
    runner.cast(swords_two).target_object(stolen).resolve();
    for creature in [killed_by_shock, exiled_by_spell, stolen] {
        assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);
    }

    advance_through_end_step_trigger(&mut runner);

    let tokens = rat_tokens(&runner);
    assert_eq!(
        tokens.len(),
        2,
        "count opponent-controlled creature exiles, not the creature owned by P1 but controlled by P0"
    );
    // CR 613.4c: Vren is a Rat, so each token sees two other Rats (Vren and the other
    // token): 1/1 + 2 = 3/3.
    for token in tokens {
        let token = &runner.state().objects[&token];
        assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    }
    let vren = &runner.state().objects[&vren];
    assert_eq!((vren.power, vren.toughness), (Some(3), Some(4)));
}

#[test]
fn vren_rat_tokens_get_dynamic_bonus_from_other_rats_not_non_rats() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_vren(&mut scenario);
    let non_rat = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let first_opponent_creature = scenario
        .add_creature(P1, "First opponent creature", 1, 1)
        .id();
    let second_opponent_creature = scenario
        .add_creature(P1, "Second opponent creature", 1, 1)
        .id();
    let first_shock = add_shock(&mut scenario);
    let second_shock = add_shock(&mut scenario);
    mana_pool(&mut scenario, &[ManaType::Red, ManaType::Red]);
    let mut runner = scenario.build();

    runner
        .cast(first_shock)
        .target_object(first_opponent_creature)
        .resolve();
    runner
        .cast(second_shock)
        .target_object(second_opponent_creature)
        .resolve();
    for creature in [first_opponent_creature, second_opponent_creature] {
        assert_eq!(runner.state().objects[&creature].zone, Zone::Exile);
    }

    advance_through_end_step_trigger(&mut runner);

    let tokens = rat_tokens(&runner);
    assert_eq!(tokens.len(), 2);
    // Vren + 2 tokens are Rats: each token counts the 2 other Rats -> 3/3.
    for token in tokens {
        let token = &runner.state().objects[&token];
        assert_eq!((token.power, token.toughness), (Some(3), Some(3)));
    }
    let non_rat = &runner.state().objects[&non_rat];
    assert_eq!((non_rat.power, non_rat.toughness), (Some(2), Some(2)));
}

const BEAR_FORCE_PILOT_ORACLE: &str = "When The Bear Force Pilot enters the battlefield, create a 0/0 colorless artifact Vehicle token with flying, \"This creature gets +1/+1 for each Bear you control,\" and crew 2.";

/// The Bear Force Pilot: the token's `crew 2` follows its quoted static and must not be
/// dropped (CR 702.122a), alongside flying and the dynamic +1/+1 (CR 613.4c).
#[test]
fn bear_force_pilot_token_keeps_flying_and_crew_two_after_quoted_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pilot = scenario
        .add_creature_to_hand_from_oracle(P0, "The Bear Force Pilot", 2, 2, BEAR_FORCE_PILOT_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Bear", "Pilot"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    mana_pool(&mut scenario, &[ManaType::Green, ManaType::Green]);
    let mut runner = scenario.build();

    runner.cast(pilot).resolve();
    runner.advance_until_stack_empty();

    let vehicles: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, o)| o.zone == Zone::Battlefield && o.is_token)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        vehicles.len(),
        1,
        "the ETB trigger creates one Vehicle token"
    );
    let token = &runner.state().objects[&vehicles[0]];
    assert!(token.card_types.subtypes.iter().any(|s| s == "Vehicle"));
    assert!(
        token.keywords.contains(&Keyword::Flying),
        "{:?}",
        token.keywords
    );
    assert!(
        token.keywords.contains(&Keyword::Crew {
            power: 2,
            once_per_turn: None
        }),
        "token must have crew 2: {:?}",
        token.keywords
    );
    // The quoted static is attached to the token; its per-Bear bonus is asserted by the
    // parser class test, since an uncrewed Vehicle is not a creature.
    assert!(!token.static_definitions.is_empty());
}
