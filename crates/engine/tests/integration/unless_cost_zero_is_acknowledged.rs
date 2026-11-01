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
const POWER_SINK: &str = "Counter target spell unless its controller pays {X}. If that player doesn't, they tap all lands with mana abilities they control and lose all unspent mana.";
const CONDESCEND: &str = "Counter target spell unless its controller pays {X}. Scry 2. (Look at the top two cards of your library, then put any number of them on the bottom and the rest on top in any order.)";
const DONT_MAKE_A_SOUND: &str = "Counter target spell unless its controller pays {2}. If they do, surveil 2. (Look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)";
const FLASH: &str = "You may put a creature card from your hand onto the battlefield. If you do, sacrifice it unless you pay its mana cost reduced by {2}.";

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
    let spell = push_stack_spell(&mut runner, P0);
    (runner, spell)
}

/// An instant controlled by `controller`, on the stack.
fn push_stack_spell(runner: &mut GameRunner, controller: PlayerId) -> ObjectId {
    let spell = create_object(
        runner.state_mut(),
        CardId(901),
        controller,
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
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(901),
            ability: None,
            casting_variant: engine::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
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

struct PrintedCounter {
    runner: GameRunner,
    bear: ObjectId,
    spare: ObjectId,
    payment_lands: Vec<ObjectId>,
    looked: Vec<ObjectId>,
}

/// Real response-cast pipeline. The payer's casting, payment and rider-witness
/// lands are distinct; the responder has exactly the casting resources needed.
fn printed_counter_prompt(name: &str, oracle: &str, x: Option<u32>) -> PrintedCounter {
    printed_counter_prompt_with_redirects(name, oracle, x, false)
}

fn printed_counter_prompt_with_redirects(
    name: &str,
    oracle: &str,
    x: Option<u32>,
    redirects: bool,
) -> PrintedCounter {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let casting_lands = [
        scenario.add_basic_land(P0, ManaColor::Green),
        scenario.add_basic_land(P0, ManaColor::Green),
    ];
    let payment_lands = (0..2)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect::<Vec<_>>();
    let spare = scenario.add_basic_land(P0, ManaColor::Green);
    if redirects {
        use engine::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use engine::types::replacements::ReplacementEvent;
        for destination in [Zone::Exile, Zone::Hand] {
            let redirect = ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .valid_card(TargetFilter::SpecificObject { id: bear })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination,
                        target: TargetFilter::SelfRef,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        enters_modified_if: None,
                        face_down_profile: None,
                    },
                ));
            scenario
                .add_creature(P1, &format!("Counter destination {destination:?}"), 1, 1)
                .with_replacement_definition(redirect);
        }
    }
    let cost = ManaCost::Cost {
        shards: if x.is_some() {
            vec![ManaCostShard::X, ManaCostShard::Blue]
        } else {
            vec![ManaCostShard::Blue]
        },
        generic: if x.is_some() { 0 } else { 1 },
    };
    let counter = scenario
        .add_spell_to_hand_from_oracle(P1, name, true, oracle)
        .with_mana_cost(cost)
        .id();
    let caster_lands = (0..(x.unwrap_or(1) + 1))
        .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
        .collect::<Vec<_>>();
    let second = scenario.add_card_to_library_top(P1, "Second known card");
    let first = scenario.add_card_to_library_top(P1, "First known card");
    let mut runner = scenario.build();
    let mut bear_cast = runner.cast(bear).commit();
    assert!(matches!(
        bear_cast.state().stack.back().unwrap().kind,
        StackEntryKind::Spell {
            actual_mana_spent: 2,
            ..
        }
    ));
    assert!(casting_lands
        .iter()
        .all(|land| bear_cast.state().objects[land].tapped));
    bear_cast
        .act(GameAction::PassPriority)
        .expect("Bears caster passes to responder");
    let response = bear_cast.cast(counter).target_object(bear);
    let response = match x {
        Some(x) => response.x(x),
        None => response,
    };
    let committed = response.commit();
    let entry = committed.state().stack.back().expect("response on stack");
    match &entry.kind {
        StackEntryKind::Spell {
            ability: Some(ability),
            actual_mana_spent,
            ..
        } => {
            assert_eq!(*actual_mana_spent, x.unwrap_or(1) + 1);
            if let Some(x) = x {
                assert_eq!(ability.chosen_x, Some(x));
            }
        }
        other => panic!("real parsed response must be paid and committed: {other:?}"),
    }
    let outcome = committed.resolve();
    outcome.assert_zone(&[bear], Zone::Stack);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::UnlessPayment { player: P0, .. }
    ));
    assert!(caster_lands
        .iter()
        .all(|land| outcome.state().objects[land].tapped));
    assert!(outcome.state().players[1].mana_pool.is_empty());
    assert!(payment_lands
        .iter()
        .all(|land| !outcome.state().objects[land].tapped));
    assert!(!outcome.state().objects[&spare].tapped);
    assert!(
        !outcome.state().objects[&spare].abilities.is_empty(),
        "spare Forest has a working mana ability"
    );
    drop(bear_cast);
    PrintedCounter {
        runner,
        bear,
        spare,
        payment_lands,
        looked: vec![first, second],
    }
}

#[test]
fn power_sink_acknowledged_and_declined_zero_and_nonzero() {
    for x in [0, 1] {
        for pay in [false, true] {
            let mut board = printed_counter_prompt("Power Sink", POWER_SINK, Some(x));
            assert_eq!(
                unless_cost(&board.runner),
                Some((
                    P0,
                    AbilityCost::Mana {
                        cost: ManaCost::generic(x)
                    }
                ))
            );
            if pay && x == 1 {
                let (_, _, grouped) = engine::ai_support::legal_actions_full(board.runner.state());
                let selection = grouped
                    .get(&board.payment_lands[0])
                    .into_iter()
                    .flatten()
                    .find_map(|action| {
                        if let GameAction::TapLandForMana { selection } = action {
                            Some(selection.clone())
                        } else {
                            None
                        }
                    })
                    .expect("engine-authored payment-land capability");
                board
                    .runner
                    .act(GameAction::TapLandForMana { selection })
                    .expect("payer activates the named payment Forest");
                assert!(board.runner.state().objects[&board.payment_lands[0]].tapped);
                assert!(!board.runner.state().objects[&board.spare].tapped);
            }
            let answer = board
                .runner
                .act(GameAction::PayUnlessCost { pay })
                .expect("answer the actual X payment");
            if pay {
                assert_eq!(zone(&board.runner, board.bear), Zone::Stack);
                assert!(
                    !board.runner.state().objects[&board.spare].tapped,
                    "paid X={x} suppresses the unpaid rider"
                );
                if x == 0 {
                    assert!(board
                        .payment_lands
                        .iter()
                        .all(|land| !board.runner.state().objects[land].tapped));
                }
                assert!(board.runner.state().players[0].mana_pool.is_empty());
                board.runner.resolve_top();
                assert_eq!(zone(&board.runner, board.bear), Zone::Battlefield);
            } else {
                assert_eq!(zone(&board.runner, board.bear), Zone::Graveyard);
                assert!(
                    board.runner.state().objects[&board.spare].tapped,
                    "unpaid X={x} must deliver its tap rider even after Counter happened"
                );
                assert_eq!(answer.events.iter().filter(|event| matches!(event, engine::types::events::GameEvent::SpellCountered { object_id, .. } if *object_id == board.bear)).count(), 1);
            }
        }
    }
}

#[test]
fn power_sink_unpaid_outcome_survives_counter_destination_replacement_pause() {
    use engine::types::events::GameEvent;
    for pay in [false, true] {
        let mut board =
            printed_counter_prompt_with_redirects("Power Sink", POWER_SINK, Some(0), true);
        assert_eq!(unless_cost(&board.runner), Some((P0, zero_cost())));
        let answer = board
            .runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("real zero payment with counter destination replacements");
        if pay {
            assert_eq!(zone(&board.runner, board.bear), Zone::Stack);
            assert!(!board.runner.state().objects[&board.spare].tapped);
            assert!(matches!(
                board.runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(!answer.events.iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == board.bear)));
            board.runner.resolve_top();
            assert_eq!(zone(&board.runner, board.bear), Zone::Battlefield);
            continue;
        }
        match &board.runner.state().waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => assert_eq!(candidates.len(), 2),
            other => {
                panic!("competing counter-destination replacements must really pause: {other:?}")
            }
        }
        assert!(answer.events.iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == board.bear)));
        assert!(!board
            .runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == board.bear));
        assert!(
            !board.runner.state().objects[&board.spare].tapped,
            "rider waits for the counter's destination move"
        );
        let resumed = board
            .runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("choose actual destination replacement");
        assert!(matches!(
            zone(&board.runner, board.bear),
            Zone::Exile | Zone::Hand
        ));
        assert!(board.runner.state().objects[&board.spare].tapped);
        assert_eq!(answer.events.iter().chain(resumed.events.iter()).filter(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == board.spare)).count(), 1);
        assert!(matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn paid_outcome_preserves_affirmative_and_independent_printed_riders() {
    for pay in [false, true] {
        let mut board = printed_counter_prompt("Don't Make a Sound", DONT_MAKE_A_SOUND, None);
        assert_eq!(
            unless_cost(&board.runner),
            Some((
                P0,
                AbilityCost::Mana {
                    cost: ManaCost::generic(2)
                }
            ))
        );
        board
            .runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("sound payment");
        if pay {
            assert_eq!(zone(&board.runner, board.bear), Zone::Stack);
            match &board.runner.state().waiting_for {
                WaitingFor::SurveilChoice { player, cards, .. } => {
                    assert_eq!(*player, P1);
                    assert_eq!(*cards, board.looked);
                }
                other => panic!("paid caster surveils exactly the known two cards: {other:?}"),
            }
            board
                .runner
                .act(GameAction::SelectCards {
                    cards: board.looked.clone(),
                })
                .expect("keep surveilled cards");
        } else {
            assert_eq!(zone(&board.runner, board.bear), Zone::Graveyard);
            assert!(matches!(
                board.runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
        for x in [0, 1] {
            let mut board = printed_counter_prompt("Condescend", CONDESCEND, Some(x));
            assert_eq!(
                unless_cost(&board.runner),
                Some((
                    P0,
                    AbilityCost::Mana {
                        cost: ManaCost::generic(x)
                    }
                ))
            );
            board
                .runner
                .act(GameAction::PayUnlessCost { pay })
                .expect("Condescend payment");
            assert_eq!(
                zone(&board.runner, board.bear),
                if pay { Zone::Stack } else { Zone::Graveyard }
            );
            match &board.runner.state().waiting_for {
                WaitingFor::ScryChoice { player, cards, .. } => {
                    assert_eq!(*player, P1);
                    assert_eq!(*cards, board.looked);
                }
                other => panic!("independent Scry 2 occurs for X={x}, pay={pay}: {other:?}"),
            }
            board
                .runner
                .act(GameAction::SelectCards {
                    cards: board.looked.clone(),
                })
                .expect("keep scry cards");
        }
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bolt = scenario.add_spell_to_hand_from_oracle(P0, "Rhystic Lightning", true,
            "Rhystic Lightning deals 4 damage to any target unless that permanent's controller or that player pays {2}. If they do, Rhystic Lightning deals 2 damage to the permanent or player.")
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Red], generic: 2 }).id();
        let casting = (0..3)
            .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
            .collect::<Vec<_>>();
        let payment = (0..2)
            .map(|_| scenario.add_basic_land(P1, ManaColor::Green))
            .collect::<Vec<_>>();
        let mut runner = scenario.build();
        let outcome = runner.cast(bolt).target_player(P1).resolve();
        outcome.assert_life_delta(P1, 0);
        assert!(casting
            .iter()
            .all(|land| outcome.state().objects[land].tapped));
        assert_eq!(
            unless_cost(&runner),
            Some((
                P1,
                AbilityCost::Mana {
                    cost: ManaCost::generic(2)
                }
            ))
        );
        runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("damage payment");
        assert_eq!(runner.state().players[1].life, if pay { 18 } else { 16 });
        assert_eq!(runner.state().players[0].life, 20);
        assert_eq!(
            payment
                .iter()
                .filter(|land| runner.state().objects[land].tapped)
                .count(),
            if pay { 2 } else { 0 }
        );
    }
}

#[test]
fn flash_preserves_preceding_optional_outcome_through_unless_payment() {
    for (put, pay) in [(true, true), (true, false), (false, false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let flash = scenario
            .add_spell_to_hand_from_oracle(P0, "Flash", true, FLASH)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            })
            .id();
        let memnite = scenario
            .add_creature_to_hand(P0, "Memnite", 1, 1)
            .as_artifact_creature()
            .with_mana_cost(ManaCost::zero())
            .id();
        let decoy = scenario
            .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            })
            .id();
        let lands = [
            scenario.add_basic_land(P0, ManaColor::Blue),
            scenario.add_basic_land(P0, ManaColor::Blue),
        ];
        let mut runner = scenario.build();
        let mut cast = runner.cast(flash).commit();
        for _ in 0..4 {
            if !matches!(cast.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            cast.act(GameAction::PassPriority)
                .expect("resolve Flash to its actual optional put");
        }
        assert!(matches!(
            cast.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        assert!(lands.iter().all(|land| cast.state().objects[land].tapped));
        cast.act(GameAction::DecideOptionalEffect { accept: put })
            .expect("answer initial optional put");
        if put {
            match &cast.state().waiting_for {
                WaitingFor::EffectZoneChoice { cards, .. } => {
                    assert!(cards.contains(&memnite));
                    assert!(cards.contains(&decoy));
                }
                other => panic!("choose the real creature from hand: {other:?}"),
            }
            cast.act(GameAction::SelectCards {
                cards: vec![memnite],
            })
            .expect("select Memnite, not the decoy");
        }
        let outcome = cast.resolve();
        outcome.assert_zone(&[decoy], Zone::Hand);
        if !put {
            outcome.assert_zone(&[memnite], Zone::Hand);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            continue;
        }
        outcome.assert_zone(&[memnite], Zone::Battlefield);
        match outcome.final_waiting_for() {
            WaitingFor::UnlessPayment {
                player,
                cost,
                pending_effect,
                ..
            } => {
                assert_eq!(*player, P0);
                assert_eq!(*cost, zero_cost());
                assert!(matches!(pending_effect.effect, Effect::Sacrifice { .. }));
                assert!(
                    pending_effect.context.optional_effect_performed,
                    "preceding put admits this root"
                );
                assert_eq!(
                    pending_effect.targets,
                    vec![TargetRef::Object(memnite)],
                    "the pending instruction is bound to the exact selected Memnite"
                );
            }
            other => panic!("real reduced-cost payment: {other:?}"),
        }
        let result = runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("answer separate payment");
        assert_eq!(
            zone(&runner, memnite),
            if pay {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(zone(&runner, decoy), Zone::Hand);
        assert!(runner.state().players[0].mana_pool.is_empty());
        assert_eq!(result.events.iter().filter(|event| matches!(event, engine::types::events::GameEvent::PermanentSacrificed { object_id, .. } if *object_id == memnite)).count(), usize::from(!pay));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn payment_outcome_independent_sibling_follows_counter_or_paid_suppression() {
    use engine::types::ability::SubAbilityLink;
    use engine::types::events::GameEvent;
    for pay in [false, true] {
        let (mut runner, spell) = direct_board();
        let mut root = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
            vec![TargetRef::Object(spell)],
            ObjectId(9000),
            P1,
        );
        root.unless_pay = Some(UnlessPayModifier {
            cost: zero_cost(),
            payer: TargetFilter::ParentTargetController,
        });
        let mut sibling = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 7 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(9000),
            P1,
        );
        sibling.sub_link = SubAbilityLink::SequentialSibling;
        root.sub_ability = Some(Box::new(sibling));
        resolve_ability_chain(runner.state_mut(), &root, &mut Vec::new(), 0)
            .expect("mechanism root produces payment");
        assert_eq!(unless_cost(&runner), Some((P0, zero_cost())));
        assert_eq!(zone(&runner, spell), Zone::Stack);
        let result = runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("apply actual payment action");
        assert_eq!(
            zone(&runner, spell),
            if pay { Zone::Stack } else { Zone::Graveyard }
        );
        assert_eq!(
            runner.state().players[1].life,
            27,
            "independent sibling runs exactly once in both outcomes"
        );
        assert_eq!(result.events.iter().filter(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == spell)).count(), usize::from(!pay));
    }
}

/// CR 118.12a + CR 608.2c, synthetic mechanism chain (no printed card has this
/// shape; card data has no unless head whose dependent step is followed by an
/// independent instruction): a Counter head with an unless cost, a dependent
/// resolution step of that head (`ContinuationStep`, gain 3 life), then the
/// next independent instruction (`SequentialSibling`, gain 7 life). Declining
/// lets the head and its step happen, then the sibling. Paying suppresses the
/// head and its step, and the walk resumes at the sibling, which still happens
/// once.
#[test]
fn paid_suppression_skips_a_dependent_step_to_the_next_independent_instruction() {
    use engine::types::ability::SubAbilityLink;
    use engine::types::events::GameEvent;
    for pay in [false, true] {
        let (mut runner, spell) = direct_board();
        let mut root = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: None,
            },
            vec![TargetRef::Object(spell)],
            ObjectId(9000),
            P1,
        );
        root.unless_pay = Some(UnlessPayModifier {
            cost: zero_cost(),
            payer: TargetFilter::ParentTargetController,
        });
        let mut step = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(9000),
            P1,
        );
        assert_eq!(step.sub_link, SubAbilityLink::ContinuationStep);
        let mut sibling = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 7 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(9000),
            P1,
        );
        sibling.sub_link = SubAbilityLink::SequentialSibling;
        step.sub_ability = Some(Box::new(sibling));
        root.sub_ability = Some(Box::new(step));
        resolve_ability_chain(runner.state_mut(), &root, &mut Vec::new(), 0)
            .expect("mechanism root produces payment");
        // Reach guards: the payer is prompted for {0}, and nothing has
        // resolved yet.
        assert_eq!(unless_cost(&runner), Some((P0, zero_cost())));
        assert_eq!(zone(&runner, spell), Zone::Stack);
        assert_eq!(runner.state().players[1].life, 20);
        let result = runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("apply actual payment action");
        assert_eq!(
            zone(&runner, spell),
            if pay { Zone::Stack } else { Zone::Graveyard }
        );
        assert_eq!(result.events.iter().filter(|event| matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == spell)).count(), usize::from(!pay));
        assert_eq!(
            runner.state().players[1].life,
            if pay { 27 } else { 30 },
            "pay={pay}: the dependent step follows the head; the sibling runs once either way"
        );
        assert_eq!(runner.state().players[0].life, 20);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// CR 117.3b + CR 800.4j: the active player left the game earlier in its turn
/// (three players, turn order P0 -> P1 -> P2), and the turn continues. P1's
/// unless-Counter asks P2, the controller of the target spell. Whether P2 pays
/// or declines, the answered prompt hands the resolution back where the active
/// player would receive priority, so the next player in turn order still in the
/// game, P1, receives it, not the departed seat.
#[test]
fn answered_unless_prompt_after_the_active_player_left_gives_priority_to_the_next_living_seat() {
    let p2 = PlayerId(2);
    for pay in [false, true] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(runner.state().seat_order, [P0, P1, p2]);
        assert_eq!(
            runner.state().turn_direction,
            engine::types::phase::TurnDirection::Normal
        );
        // CR 800.4a: P0 leaves through the engine's elimination authority.
        engine::game::elimination::eliminate_player(runner.state_mut(), P0, &mut Vec::new());
        assert!(runner.state().players[0].is_eliminated);
        assert!(runner.state().game_end.is_none());
        assert_eq!(runner.state().active_player, P0);

        let spell = push_stack_spell(&mut runner, p2);
        run_counter_chain(
            &mut runner,
            spell,
            zero_cost(),
            TargetFilter::ParentTargetController,
        );
        // Reach guards: the target spell's controller is asked for {0}, and the
        // spell is still on the stack.
        assert_eq!(unless_cost(&runner), Some((p2, zero_cost())));
        assert_eq!(zone(&runner, spell), Zone::Stack);

        runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("P2 answers the unless prompt");
        assert_eq!(
            zone(&runner, spell),
            if pay { Zone::Stack } else { Zone::Graveyard }
        );
        assert_eq!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 },
            "pay={pay}"
        );
        assert_eq!(runner.state().priority_player, P1);
        assert_eq!(runner.state().active_player, P0);
    }
}
