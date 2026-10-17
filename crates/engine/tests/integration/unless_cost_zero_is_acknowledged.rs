//! A payable {0} "unless" cost of a counterspell is asked, not auto-paid.
//!
//! CR 118.12a: "[Do something] unless [a player does something else]" means
//! "[A player may do something else]. If [that player] doesn't, [do something]".
//! CR 118.5: "Some costs are represented by {0}, or are reduced to {0}. The
//! action necessary for a player to pay such a cost is the player's
//! acknowledgment that they are paying it. Even though such a cost requires no
//! resources, it's not automatically paid." So the payer is prompted and may
//! decline, in which case the spell is countered. An UNPAYABLE cost (CR 118.6,
//! `ManaCost::NoCost`) is a different shape: nothing is offered and the effect
//! happens.
//!
//! Oracle text (Scryfall, 2026-10-13):
//!   Override: "Counter target spell unless its controller pays {1} for each
//!   artifact you control."
//!   Clash of Wills: "Counter target spell unless its controller pays {X}."
//!
//! Test spells are NOT named "Counter": a leading oracle word "Counter" that
//! matches the card's own name is normalised to the self-reference `~`.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, UnlessPayModifier,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const OVERRIDE: &str =
    "Counter target spell unless its controller pays {1} for each artifact you control.";
const CLASH_OF_WILLS: &str = "Counter target spell unless its controller pays {X}.";

struct Board {
    runner: GameRunner,
    bear: ObjectId,
    spare_land: ObjectId,
    artifacts: Vec<ObjectId>,
}

fn override_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    }
}

/// P0 casts a 2/2 Bear with a spare untapped Forest left over and passes; P1
/// (three Islands, `artifacts` artifacts) casts `text` at the Bear spell. The
/// counter is left on the stack.
fn counter_at_bear(
    name: &str,
    text: &str,
    cost: ManaCost,
    x: Option<u32>,
    artifacts: usize,
) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let spare_land = scenario.add_basic_land(P0, ManaColor::Green);
    let counter = scenario
        .add_spell_to_hand_from_oracle(P1, name, true, text)
        .with_mana_cost(cost)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    let artifact_ids = (0..artifacts)
        .map(|i| {
            scenario
                .add_artifact_from_oracle(P1, &format!("Trinket {i}"), "")
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 3;
        state.active_player = P0;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.cast(bear).commit();
    let bear_spell = runner.state().stack.back().expect("bear spell").id;
    engine::game::engine::apply_as_current_for_simulation(
        runner.state_mut(),
        GameAction::PassPriority,
    )
    .expect("P0 passes");
    let cast = runner.cast(counter);
    let cast = match x {
        Some(x) => cast.x(x),
        None => cast,
    };
    cast.target_object(bear_spell).commit();
    Board {
        runner,
        bear: bear_spell,
        spare_land,
        artifacts: artifact_ids,
    }
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object").zone
}

fn unless_cost(runner: &GameRunner) -> Option<(PlayerId, AbilityCost)> {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => Some((*player, cost.clone())),
        _ => None,
    }
}

fn zero_cost() -> AbilityCost {
    AbilityCost::Mana {
        cost: ManaCost::zero(),
    }
}

/// Row 1: the "for each" route (Override with no artifacts) asks for {0}.
#[test]
fn override_with_no_artifacts_asks_for_zero_then_decline_counters() {
    // Reach-guard: with two artifacts the same harness reaches the interceptor
    // and asks for {2}.
    let mut guard = counter_at_bear("Override", OVERRIDE, override_cost(), None, 2);
    assert_eq!(guard.artifacts.len(), 2);
    guard.runner.resolve_top();
    assert_eq!(
        unless_cost(&guard.runner),
        Some((
            P0,
            AbilityCost::Mana {
                cost: ManaCost::generic(2)
            }
        )),
        "reach guard: Override with two artifacts asks P0 for {{2}}"
    );

    let mut board = counter_at_bear("Override", OVERRIDE, override_cost(), None, 0);
    board.runner.resolve_top();
    assert_eq!(
        unless_cost(&board.runner),
        Some((P0, zero_cost())),
        "CR 118.5: a {{0}} cost is asked, not auto-paid; got {:?}",
        board.runner.state().waiting_for
    );
    assert_eq!(zone(&board.runner, board.bear), Zone::Stack);
    board
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P0 declines");
    assert_eq!(
        zone(&board.runner, board.bear),
        Zone::Graveyard,
        "CR 118.12a: declining the {{0}} lets the counter happen"
    );
}

/// Row 1 accept half: accepting pays nothing and the spell resolves.
#[test]
fn override_with_no_artifacts_accepting_pays_nothing_and_the_spell_resolves() {
    let mut board = counter_at_bear("Override", OVERRIDE, override_cost(), None, 0);
    board.runner.resolve_top();
    assert_eq!(
        unless_cost(&board.runner),
        Some((P0, zero_cost())),
        "reach guard: the {{0}} prompt is raised"
    );
    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P0 acknowledges the {0}");
    assert_eq!(zone(&board.runner, board.bear), Zone::Stack);
    assert!(
        !board.runner.state().objects[&board.spare_land].tapped,
        "nothing is tapped to pay {{0}}"
    );
    assert!(board.runner.state().players[P0.0 as usize]
        .mana_pool
        .is_empty());
    board.runner.resolve_top();
    assert_eq!(zone(&board.runner, board.bear), Zone::Battlefield);
}

/// Row 2: the X route (Clash of Wills) asks at X = 0; X = 1 is the control.
#[test]
fn clash_of_wills_asks_for_zero_at_x_zero_and_for_one_at_x_one() {
    for (x, expected) in [(0u32, ManaCost::zero()), (1, ManaCost::generic(1))] {
        let mut board = counter_at_bear(
            "Clash of Wills",
            CLASH_OF_WILLS,
            ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Blue],
                generic: 0,
            },
            Some(x),
            0,
        );
        board.runner.resolve_top();
        assert_eq!(
            unless_cost(&board.runner),
            Some((P0, AbilityCost::Mana { cost: expected })),
            "X = {x}"
        );
        board
            .runner
            .act(GameAction::PayUnlessCost { pay: false })
            .expect("P0 declines");
        assert_eq!(zone(&board.runner, board.bear), Zone::Graveyard, "X = {x}");
    }
}

/// Row 6 (hostile, live cost): the artifact count is read at resolution.
#[test]
fn override_reads_the_artifact_count_at_resolution() {
    let mut board = counter_at_bear("Override", OVERRIDE, override_cost(), None, 1);
    let artifact = board.artifacts[0];
    assert_eq!(
        zone(&board.runner, artifact),
        Zone::Battlefield,
        "reach guard: the artifact is on the battlefield while Override is on the stack"
    );
    assert!(
        board
            .runner
            .state()
            .stack
            .iter()
            .any(|e| e.id != board.bear && matches!(e.kind, StackEntryKind::Spell { .. })),
        "reach guard: the Override entry is on the stack"
    );
    engine::game::zones::move_to_zone(
        board.runner.state_mut(),
        artifact,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    board.runner.resolve_top();
    assert_eq!(
        unless_cost(&board.runner),
        Some((P0, zero_cost())),
        "the cost is {{1}} x the artifacts P1 controls as Override resolves"
    );
}

/// Building-block harness for rows 3, 5, 7, 8: a counter chain controlled by P1
/// at P0's spell, resolved straight through `resolve_ability_chain`.
fn direct_board() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let spell = create_object(
        runner.state_mut(),
        CardId(901),
        P0,
        "Shock".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("spell")
        .card_types
        .core_types = vec![CoreType::Instant];
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(901),
            ability: None,
            casting_variant: engine::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    (runner, spell)
}

fn run_counter_chain(
    runner: &mut GameRunner,
    spell: ObjectId,
    cost: AbilityCost,
    payer: TargetFilter,
) {
    let mut ability = ResolvedAbility::new(
        Effect::Counter {
            target: TargetFilter::Any,
            source_rider: None,
            countered_spell_zone: None,
        },
        vec![TargetRef::Object(spell)],
        ObjectId(9000),
        P1,
    );
    ability.unless_pay = Some(UnlessPayModifier { cost, payer });
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the counter chain resolves");
}

/// Row 3: a fixed {0} cost prompts the spell's controller; {1} is the control.
#[test]
fn fixed_zero_cost_prompts_the_spells_controller() {
    for (cost, label) in [(ManaCost::generic(1), "{1}"), (ManaCost::zero(), "{0}")] {
        let (mut runner, spell) = direct_board();
        run_counter_chain(
            &mut runner,
            spell,
            AbilityCost::Mana { cost: cost.clone() },
            TargetFilter::ParentTargetController,
        );
        assert_eq!(
            unless_cost(&runner),
            Some((P0, AbilityCost::Mana { cost })),
            "{label}"
        );
        assert_eq!(
            zone(&runner, spell),
            Zone::Stack,
            "{label}: not yet countered"
        );
        runner
            .act(GameAction::PayUnlessCost { pay: false })
            .expect("decline");
        assert_eq!(zone(&runner, spell), Zone::Graveyard, "{label}: declined");
    }
    let (mut runner, spell) = direct_board();
    run_counter_chain(
        &mut runner,
        spell,
        zero_cost(),
        TargetFilter::ParentTargetController,
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("accept");
    assert_eq!(
        zone(&runner, spell),
        Zone::Stack,
        "accepted {{0}} leaves the spell"
    );
}

/// Row 5: an `AllPlayers` payer polls for {0} too; {1} arms the same shape.
#[test]
fn all_players_zero_cost_arms_the_poll() {
    for cost in [ManaCost::generic(1), ManaCost::zero()] {
        let (mut runner, spell) = direct_board();
        run_counter_chain(
            &mut runner,
            spell,
            AbilityCost::Mana { cost: cost.clone() },
            TargetFilter::AllPlayers,
        );
        match &runner.state().waiting_for {
            WaitingFor::UnlessPayment {
                player,
                cost: prompted,
                remaining,
                ..
            } => {
                assert_eq!(*player, P0, "{cost:?}");
                assert_eq!(remaining, &vec![P1], "{cost:?}");
                assert_eq!(prompted, &AbilityCost::Mana { cost: cost.clone() });
            }
            other => panic!("expected the poll for {cost:?}, got {other:?}"),
        }
    }
}

/// Row 7 (preservation): an UNPAYABLE cost (CR 118.6) still counters outright.
#[test]
fn unpayable_cost_still_counters_without_a_prompt() {
    let (mut guard, guard_spell) = direct_board();
    run_counter_chain(
        &mut guard,
        guard_spell,
        zero_cost(),
        TargetFilter::ParentTargetController,
    );
    assert!(
        unless_cost(&guard).is_some(),
        "reach guard: the same harness prompts for a payable {{0}}"
    );

    let (mut runner, spell) = direct_board();
    run_counter_chain(
        &mut runner,
        spell,
        AbilityCost::Mana {
            cost: ManaCost::NoCost,
        },
        TargetFilter::ParentTargetController,
    );
    assert!(unless_cost(&runner).is_none(), "no prompt for NoCost");
    assert_eq!(zone(&runner, spell), Zone::Graveyard);
}

/// Row 8 (preservation): a {0} in a non-Mana shape already prompted.
#[test]
fn zero_pay_life_still_prompts() {
    let (mut runner, spell) = direct_board();
    let cost = AbilityCost::PayLife {
        amount: QuantityExpr::Fixed { value: 0 },
    };
    run_counter_chain(
        &mut runner,
        spell,
        cost.clone(),
        TargetFilter::ParentTargetController,
    );
    assert_eq!(unless_cost(&runner), Some((P0, cost)));
}

/// Row 4: a {0} ward (class shape; no printed card) asks the spell's controller.
#[test]
fn ward_zero_asks_the_targeting_player() {
    for pay in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let creature = scenario
            .add_creature_from_oracle(P1, "Warded Bear", 2, 2, "Ward {0}")
            .id();
        let murder = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 2,
            })
            .id();
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        let mut runner = scenario.build();
        runner.cast(murder).target_objects(&[creature]).commit();
        runner.advance_until_stack_empty();
        assert_eq!(
            unless_cost(&runner),
            Some((P0, zero_cost())),
            "pay = {pay}: the ward trigger asks P0 for {{0}}; got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("answer the ward prompt");
        runner.advance_until_stack_empty();
        if pay {
            assert_eq!(
                zone(&runner, creature),
                Zone::Graveyard,
                "paid ward: Murder resolves"
            );
        } else {
            assert_eq!(
                zone(&runner, creature),
                Zone::Battlefield,
                "declined ward: Murder is countered"
            );
            assert_eq!(zone(&runner, murder), Zone::Graveyard);
        }
    }
}
