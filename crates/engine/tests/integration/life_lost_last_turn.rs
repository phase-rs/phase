//! EH2: the immediately preceding game turn's gross life loss, through real
//! casts, turn boundaries, targeting, state-based actions, and resolution.

use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AggregateFunction, Comparator, PlayerScope, QuantityExpr, QuantityRef, TargetRef,
    TriggerCondition, TurnWindow,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{ObjectId, PlayerId, TriggerMode};

const P2: PlayerId = PlayerId(2);

// Verbatim Scryfall Oracle text, including both of Paladin's abilities.
const FIRST_RESPONSE: &str = "At the beginning of each upkeep, if you lost life last turn, create a 1/1 white Soldier creature token.";
const PALADIN: &str = "At the beginning of each upkeep, if you lost life last turn, put a +1/+1 counter on this creature.\nWhen this creature dies, you gain life equal to its toughness.";
const FEAST: &str = "At the beginning of each upkeep, if an opponent lost life last turn, put a +1/+1 counter on target creature you control.";
const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const SALVE: &str = "Choose one —\n• Target player gains 3 life.\n• Prevent the next 3 damage that would be dealt to any target this turn.";
const STRIKE: &str = "Target creature gets +1/+0 until end of turn.\nDraw a card.";
const REPAY: &str = "Each player's life total becomes the lowest life total among all players.";

fn cost(generic: u32, color: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![color],
        generic,
    }
}

fn scenario(players: u8) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // CR 704.5b: regular draws must not eliminate any player in the fixture.
    for seat in 0..players {
        for _ in 0..8 {
            scenario.add_card_to_library_top(PlayerId(seat), "Filler");
        }
    }
    scenario
}

fn bolt(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Lightning Bolt", true, BOLT)
        .with_mana_cost(cost(0, ManaCostShard::Red))
        .id()
}

fn salve(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Healing Salve", true, SALVE)
        .with_mana_cost(cost(0, ManaCostShard::White))
        .id()
}

fn bear(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(cost(1, ManaCostShard::Green))
        .id()
}

fn assert_card<'a>(
    runner: &'a GameRunner,
    id: ObjectId,
    name: &str,
    kind: CoreType,
    mana: ManaCost,
) -> &'a GameObject {
    let card = &runner.state().objects[&id];
    assert_eq!(card.name, name);
    assert!(!card.is_token);
    assert_eq!(card.card_types.core_types, [kind]);
    assert!(card.card_types.supertypes.is_empty());
    assert_eq!(card.mana_cost, mana);
    card
}

fn assert_noncreature(card: &GameObject) {
    assert!(card.card_types.subtypes.is_empty());
    assert_eq!((card.power, card.toughness), (None, None));
}

// SHAPE (CR 603.4): assert the fully parsed, printed upkeep trigger, not
// just the guard fragment. The legacy condition would give the same result
// in free-for-all tests but would not use the team-aware opponent aggregate.
fn assert_last_turn_guard(card: &GameObject, player: PlayerScope) {
    let mut upkeeps = card.base_trigger_definitions.iter().filter(|trigger| {
        trigger.mode == TriggerMode::Phase && trigger.phase == Some(Phase::Upkeep)
    });
    let guard = upkeeps.next().expect("printed upkeep trigger");
    assert!(upkeeps.next().is_none(), "one printed upkeep trigger");
    assert_eq!(
        guard.condition,
        Some(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::LifeLost {
                    player,
                    window: TurnWindow::LastTurn,
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 1 },
        }),
        "{}",
        card.name,
    );
    assert!(!card.has_unimplemented_mechanics(), "{}", card.name);
}

fn assert_bear(runner: &GameRunner, id: ObjectId, controller: PlayerId) {
    let card = assert_card(
        runner,
        id,
        "Grizzly Bears",
        CoreType::Creature,
        cost(1, ManaCostShard::Green),
    );
    assert_eq!(card.card_types.subtypes, ["Bear"]);
    assert_eq!((card.power, card.toughness), (Some(2), Some(2)));
    assert_eq!(card.controller, controller);
    assert_eq!(card.zone, Zone::Battlefield);
}

fn assert_instant(runner: &GameRunner, id: ObjectId, name: &str, color: ManaCostShard) {
    let card = assert_card(runner, id, name, CoreType::Instant, cost(0, color));
    assert_noncreature(card);
    assert_eq!(card.zone, Zone::Hand);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == card.owner
    ));
}

fn assert_idle(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn damage_player(runner: &mut GameRunner, bolt: ObjectId, target: PlayerId) {
    assert_instant(runner, bolt, "Lightning Bolt", ManaCostShard::Red);
    let outcome = runner.cast(bolt).target_player(target).resolve();
    outcome.assert_zone(&[bolt], Zone::Graveyard);
    // CR 119.2: the damage really caused life loss for the chosen player.
    outcome.assert_life_delta(target, -3);
    assert_idle(runner);
}

fn heal_player(runner: &mut GameRunner, salve: ObjectId, target: PlayerId) {
    assert_instant(runner, salve, "Healing Salve", ManaCostShard::White);
    let outcome = runner
        .cast(salve)
        .modes(&[0])
        .target_player(target)
        .resolve();
    outcome.assert_zone(&[salve], Zone::Graveyard);
    // CR 119.3: Salve's gain mode restores the same player's life total.
    outcome.assert_life_delta(target, 3);
    assert_idle(runner);
}

// CR 117.4, CR 503.1, CR 508.1, CR 509.1: pass and decline combat until the
// next turn's upkeep opens. Stop before answering any of that upkeep's prompts.
fn next_upkeep(runner: &mut GameRunner, active: PlayerId) {
    assert_idle(runner);
    let turn = runner.state().turn_number;
    for _ in 0..100 {
        let state = runner.state();
        if state.phase == Phase::Upkeep && state.turn_number > turn {
            assert_eq!(state.turn_number, turn + 1);
            assert_eq!(state.active_player, active);
            return;
        }
        let action = if matches!(state.waiting_for, WaitingFor::DeclareAttackers { .. }) {
            GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }
        } else if matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. }) {
            GameAction::DeclareBlockers {
                assignments: vec![],
            }
        } else {
            assert_idle(runner);
            GameAction::PassPriority
        };
        runner.act(action).expect("advance the real turn structure");
    }
    panic!("the next upkeep did not begin");
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_soldiers(runner: &GameRunner, expected: usize) {
    // CR 111.1 and CR 111.3: these are exactly the tokens First Response created.
    let mut count = 0;
    for token in runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_token && obj.zone == Zone::Battlefield)
    {
        count += 1;
        assert_eq!((token.owner, token.controller), (P0, P0));
        assert_eq!(token.card_types.core_types, [CoreType::Creature]);
        assert_eq!(token.card_types.subtypes, ["Soldier"]);
        assert_eq!(token.color, [ManaColor::White]);
        assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
    }
    assert_eq!(count, expected);
}

fn assert_triggers(runner: &GameRunner, sources: &[ObjectId]) {
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), sources.len(), "exactly one trigger per source");
    for source in sources {
        assert!(stack.iter().any(|entry| {
            entry.source_id == *source
                && entry.controller == P0
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        }));
    }
}

#[test]
fn first_response_and_paladin_use_the_controllers_gross_loss_last_turn() {
    let mut scenario = scenario(2);
    for _ in 0..7 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_basic_land(P1, ManaColor::White);
    let first = scenario
        .add_enchantment_to_hand_from_oracle(P0, "First Response", FIRST_RESPONSE)
        .with_mana_cost(cost(3, ManaCostShard::White))
        .id();
    let paladin = scenario
        .add_creature_to_hand(P0, "Paladin of Atonement", 1, 1)
        .with_subtypes(vec!["Vampire", "Knight"])
        .with_mana_cost(cost(1, ManaCostShard::White))
        .from_oracle_text(PALADIN)
        .id();
    let self_loss = bolt(&mut scenario, P1);
    let opponent_loss = bolt(&mut scenario, P0);
    let lethal = bolt(&mut scenario, P1);
    let heal = salve(&mut scenario);
    let strike = scenario
        .add_spell_to_hand_from_oracle(P1, "Defiant Strike", true, STRIKE)
        .with_mana_cost(cost(0, ManaCostShard::White))
        .id();

    let mut runner = scenario.build();
    runner.act(GameAction::PassPriority).unwrap();
    damage_player(&mut runner, self_loss, P0);
    heal_player(&mut runner, heal, P0);
    assert_eq!(runner.life(P0), 20, "−3 then +3: no net loss");

    // The Scryfall rulings include loss before these permanents entered.
    let first_card = assert_card(
        &runner,
        first,
        "First Response",
        CoreType::Enchantment,
        cost(3, ManaCostShard::White),
    );
    assert_noncreature(first_card);
    assert_eq!(first_card.zone, Zone::Hand);
    let paladin_card = assert_card(
        &runner,
        paladin,
        "Paladin of Atonement",
        CoreType::Creature,
        cost(1, ManaCostShard::White),
    );
    assert_eq!(paladin_card.card_types.subtypes, ["Vampire", "Knight"]);
    assert_eq!(
        (paladin_card.power, paladin_card.toughness),
        (Some(1), Some(1))
    );
    assert_eq!(paladin_card.zone, Zone::Hand);
    for permanent in [first, paladin] {
        runner
            .cast(permanent)
            .resolve()
            .assert_zone(&[permanent], Zone::Battlefield);
        assert_eq!(runner.state().objects[&permanent].controller, P0);
        assert_idle(&runner);
    }
    assert_soldiers(&runner, 0);
    assert_eq!(plus_counters(&runner, paladin), 0);

    // CR 109.5 and CR 603.4: these check P0's life loss, even on P1's upkeep.
    next_upkeep(&mut runner, P1);
    if let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for {
        // CR 603.3b: order the two P0 triggers if their group needs a choice.
        assert_eq!(*player, P0);
        assert_eq!(triggers.len(), 2);
        runner
            .act(GameAction::OrderTriggers { order: vec![0, 1] })
            .unwrap();
    }
    assert_triggers(&runner, &[first, paladin]);
    runner.advance_until_stack_empty();
    assert_idle(&runner);
    assert_soldiers(&runner, 1);
    assert_eq!(plus_counters(&runner, paladin), 1);
    for id in [first, paladin] {
        assert_last_turn_guard(&runner.state().objects[&id], PlayerScope::Controller);
    }

    // CR 603.4: a turn with no loss must overwrite the older loss.
    next_upkeep(&mut runner, P0);
    assert_idle(&runner);
    assert_soldiers(&runner, 1);
    assert_eq!(plus_counters(&runner, paladin), 1);
    damage_player(&mut runner, opponent_loss, P1);
    next_upkeep(&mut runner, P1);
    assert_idle(&runner);
    assert_soldiers(&runner, 1);
    assert_eq!(plus_counters(&runner, paladin), 1);

    // CR 122.1a: an earned counter makes Paladin 2/2. Defiant Strike makes
    // power differ from toughness; the life gain must be 2, neither 3 nor 1.
    assert_instant(&runner, strike, "Defiant Strike", ManaCostShard::White);
    let outcome = runner.cast(strike).target_object(paladin).resolve();
    outcome.assert_zone(&[strike], Zone::Graveyard);
    outcome.assert_hand_drawn(P1, 1);
    assert_idle(&runner);
    let creature = &runner.state().objects[&paladin];
    assert_eq!((creature.power, creature.toughness), (Some(3), Some(2)));

    // CR 704.5g and CR 700.4: Bolt kills Paladin as a state-based action.
    // CR 608.2h: the untargeted dies trigger needs its last battlefield toughness.
    assert_instant(&runner, lethal, "Lightning Bolt", ManaCostShard::Red);
    let (p0_before, p1_before) = (runner.life(P0), runner.life(P1));
    let mut cast = runner.cast(lethal).target_object(paladin).commit();
    assert_eq!(cast.state().stack.len(), 1);
    for _ in 0..2 {
        cast.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().objects[&lethal].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&paladin].zone, Zone::Graveyard);
    assert_triggers(&runner, &[paladin]);
    assert_eq!(runner.life(P0), p0_before);
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), p0_before + 2);
    assert_eq!(runner.life(P1), p1_before);
    assert_idle(&runner);
}

// CR 603.3d and CR 601.2c: Feast's controller, not the upkeep player or the
// loser of life, announces exactly one target chosen from their own creatures.
fn resolve_feast(runner: &mut GameRunner, feast: ObjectId, target: ObjectId, other: ObjectId) {
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Feast's targeting prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, Some(feast));
    assert_eq!(target_slots.len(), 1);
    assert!(!target_slots[0].optional);
    let legal = &selection.current_legal_targets;
    assert_eq!(legal.len(), 2);
    assert!(legal.contains(&TargetRef::Object(target)));
    assert!(legal.contains(&TargetRef::Object(other)));
    let before = plus_counters(runner, target);
    let other_before = plus_counters(runner, other);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("announce Feast's legal creature target");
    assert_triggers(runner, &[feast]);
    runner.advance_until_stack_empty();
    assert_idle(runner);
    assert_eq!(plus_counters(runner, target), before + 1);
    assert_eq!(plus_counters(runner, other), other_before);
}

#[test]
fn feast_counts_either_opponent_but_not_its_controller_or_an_older_turn() {
    let mut scenario = scenario(3);
    let feast = scenario
        .add_enchantment_from_oracle(P0, "Feast on the Fallen", FEAST)
        .with_mana_cost(cost(2, ManaCostShard::Black))
        .id();
    let first = bear(&mut scenario, P0);
    let second = bear(&mut scenario, P0);
    let foreign = [bear(&mut scenario, P1), bear(&mut scenario, P2)];
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_basic_land(P2, ManaColor::Red);
    scenario.add_basic_land(P2, ManaColor::Red);
    let hit_p1 = bolt(&mut scenario, P1);
    let hit_p2 = bolt(&mut scenario, P0);
    let heal_p2 = salve(&mut scenario);
    let hit_p0 = bolt(&mut scenario, P1);
    let hit_both = [bolt(&mut scenario, P2), bolt(&mut scenario, P2)];

    let mut runner = scenario.build();
    let card = assert_card(
        &runner,
        feast,
        "Feast on the Fallen",
        CoreType::Enchantment,
        cost(2, ManaCostShard::Black),
    );
    assert_noncreature(card);
    assert_eq!((card.zone, card.controller), (Zone::Battlefield, P0));
    for (id, controller) in [
        (first, P0),
        (second, P0),
        (foreign[0], P1),
        (foreign[1], P2),
    ] {
        assert_bear(&runner, id, controller);
    }

    next_upkeep(&mut runner, P1);
    assert_idle(&runner); // No one has lost any life.
    damage_player(&mut runner, hit_p1, P1);
    next_upkeep(&mut runner, P2);
    resolve_feast(&mut runner, feast, first, second);
    assert_last_turn_guard(
        &runner.state().objects[&feast],
        PlayerScope::Opponent {
            aggregate: AggregateFunction::Sum,
        },
    );

    next_upkeep(&mut runner, P0);
    assert_idle(&runner); // P1's loss is now two turns old.
    damage_player(&mut runner, hit_p2, P2);
    heal_player(&mut runner, heal_p2, P2);
    assert_eq!(runner.life(P2), 20, "−3 then +3: no net loss");
    // P2 was neither the previous active player nor the player whose upkeep begins.
    next_upkeep(&mut runner, P1);
    resolve_feast(&mut runner, feast, second, first);

    damage_player(&mut runner, hit_p0, P0);
    next_upkeep(&mut runner, P2);
    assert_idle(&runner); // Only Feast's controller lost life.
    assert_eq!(
        (
            plus_counters(&runner, first),
            plus_counters(&runner, second)
        ),
        (1, 1)
    );

    damage_player(&mut runner, hit_both[0], P1);
    damage_player(&mut runner, hit_both[1], P2);
    next_upkeep(&mut runner, P0);
    resolve_feast(&mut runner, feast, first, second); // One trigger, not one per opponent.
    for id in foreign {
        assert_eq!(plus_counters(&runner, id), 0);
    }
}

#[test]
fn feast_counts_an_opponent_who_lost_life_and_left_last_turn() {
    let mut scenario = scenario(3);
    scenario.with_life(P1, 3);
    scenario.add_basic_land(P0, ManaColor::Red);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let lethal = bolt(&mut scenario, P0);
    let feast = scenario
        .add_enchantment_to_hand_from_oracle(P0, "Feast on the Fallen", FEAST)
        .with_mana_cost(cost(2, ManaCostShard::Black))
        .id();
    let first = bear(&mut scenario, P0);
    let second = bear(&mut scenario, P0);
    let foreign = bear(&mut scenario, P2);
    let mut runner = scenario.build();
    for (id, controller) in [(first, P0), (second, P0), (foreign, P2)] {
        assert_bear(&runner, id, controller);
    }

    // CR 704.5a and CR 800.4a: the actual damage eliminates P1 without ending the game.
    damage_player(&mut runner, lethal, P1);
    for player in &runner.state().players {
        assert_eq!(player.is_eliminated, player.id == P1);
    }
    let card = assert_card(
        &runner,
        feast,
        "Feast on the Fallen",
        CoreType::Enchantment,
        cost(2, ManaCostShard::Black),
    );
    assert_noncreature(card);
    assert_eq!(card.zone, Zone::Hand);
    runner
        .cast(feast)
        .resolve()
        .assert_zone(&[feast], Zone::Battlefield);
    assert_eq!(runner.state().objects[&feast].controller, P0);
    // CR 800.4i: the now-departed opponent's action history still counts.
    // Turn order advances directly to P2. This is the genuine next turn,
    // and Feast was not on the battlefield when the life loss happened.
    next_upkeep(&mut runner, P2);
    resolve_feast(&mut runner, feast, second, first);
    next_upkeep(&mut runner, P0);
    assert_idle(&runner);
    assert_eq!(plus_counters(&runner, second), 1);
    assert_eq!(plus_counters(&runner, foreign), 0);
}

#[test]
fn feast_triggers_when_the_opponents_combined_loss_exceeds_i32() {
    // Reachable after life doubling with Beacon of Immortality; the histories
    // start empty. Only Repay's actual resolution writes this fixture's loss.
    const HIGH_LIFE: i32 = 1_342_177_280;
    let mut scenario = scenario(3);
    scenario.with_life(P1, HIGH_LIFE);
    scenario.with_life(P2, HIGH_LIFE);
    for _ in 0..7 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let repay_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 5,
    };
    let repay = scenario
        .add_spell_to_hand_from_oracle(P0, "Repay in Kind", false, REPAY)
        .with_mana_cost(repay_cost.clone())
        .id();
    let feast = scenario
        .add_enchantment_from_oracle(P0, "Feast on the Fallen", FEAST)
        .with_mana_cost(cost(2, ManaCostShard::Black))
        .id();
    let first = bear(&mut scenario, P0);
    let second = bear(&mut scenario, P0);
    let mut runner = scenario.build();
    let card = assert_card(
        &runner,
        repay,
        "Repay in Kind",
        CoreType::Sorcery,
        repay_cost,
    );
    assert_noncreature(card);
    assert_eq!(card.zone, Zone::Hand);
    let card = assert_card(
        &runner,
        feast,
        "Feast on the Fallen",
        CoreType::Enchantment,
        cost(2, ManaCostShard::Black),
    );
    assert_noncreature(card);
    assert_eq!((card.zone, card.controller), (Zone::Battlefield, P0));
    assert_bear(&runner, first, P0);
    assert_bear(&runner, second, P0);

    // CR 119.5: setting a lower total loses the difference. Each player's
    // loss fits an i32, but their combined loss is 2,684,354,520.
    let outcome = runner.cast(repay).resolve();
    outcome.assert_zone(&[repay], Zone::Graveyard);
    outcome.assert_life_delta(P0, 0);
    outcome.assert_life_delta(P1, 20 - HIGH_LIFE);
    outcome.assert_life_delta(P2, 20 - HIGH_LIFE);
    assert_idle(&runner);
    next_upkeep(&mut runner, P1);
    resolve_feast(&mut runner, feast, first, second);
}
