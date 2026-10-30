//! Compound "Activate only <A> and only <B> …" restriction lists (CR 602.1b +
//! CR 602.5): every listed restriction must hold for the ability to be
//! activated. Parser-only fix — the runtime already enforces each emitted
//! `ActivationRestriction` — so these are the production-path tests: legality
//! through `can_activate_ability_now`, refusal through
//! `GameAction::ActivateAbility`, and success through the activation driver.
//!
//! Also covers the "<count> creature(s) is/are blocking <source>" condition
//! leaf (CR 509.1g) through both its consumers: Grizzled Wolverine's
//! activation restriction and Rampaging Cyclops' static (CR 611.3a).

use engine::game::casting::can_activate_ability_now;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, ActivationRestriction, Comparator, Effect, FilterProp, ParsedCondition,
    QuantityExpr, QuantityRef, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle text.
const DEFENDERS_OF_HUMANITY: &str = "When this enchantment enters, create X 2/2 white Astartes Warrior creature tokens with vigilance.\n{X}{2}{W}, Exile this enchantment: Create X 2/2 white Astartes Warrior creature tokens with vigilance. Activate only if you control no creatures and only during your turn.";

/// Verbatim Scryfall Oracle text.
const GRIZZLED_WOLVERINE: &str = "{R}: This creature gets +2/+0 until end of turn. Activate only during the declare blockers step, only if at least one creature is blocking this creature, and only once each turn.";

/// Verbatim Scryfall Oracle text.
const RAMPAGING_CYCLOPS: &str =
    "This creature gets -2/-0 as long as two or more creatures are blocking it.";

fn add_mana(runner: &mut GameRunner, mana_type: ManaType, count: usize) {
    for _ in 0..count {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            mana_type,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

fn has_unimplemented(definition: &AbilityDefinition) -> bool {
    matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })
        || definition
            .sub_ability
            .as_deref()
            .is_some_and(has_unimplemented)
}

fn ability_index(state: &GameState, source: ObjectId, is_ability: fn(&Effect) -> bool) -> usize {
    state.objects[&source]
        .abilities
        .iter()
        .position(|ability| is_ability(ability.effect.as_ref()))
        .expect("activated ability present")
}

fn refuse_activation(runner: &mut GameRunner, source: ObjectId, ability_index: usize) {
    let result = runner.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        result.is_err(),
        "CR 602.5: the activation must be refused, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// Defenders of Humanity
// ---------------------------------------------------------------------------

struct DefendersBoard {
    p0_creature: bool,
    p0_turn: bool,
}

/// P0 controls Defenders and has {4}{W} floating (X = 2); P1 controls a 2/2.
fn defenders_board(board: DefendersBoard) -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let defenders = scenario
        .add_enchantment_from_oracle(P0, "Defenders of Humanity", DEFENDERS_OF_HUMANITY)
        .id();
    scenario.add_creature(P1, "Opponent Bear", 2, 2);
    if board.p0_creature {
        scenario.add_vanilla(P0, 1, 1);
    }
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 4);
    add_mana(&mut runner, ManaType::White, 1);
    if !board.p0_turn {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let idx = ability_index(runner.state(), defenders, |effect| {
        matches!(effect, Effect::Token { .. })
    });
    assert_defenders_ast(runner.state(), defenders, idx);
    (runner, defenders, idx)
}

/// Reach-guard: both restrictions are on the ability and the sentence is not
/// Unimplemented, so the refusals below cannot pass on an empty restriction
/// list.
fn assert_defenders_ast(state: &GameState, defenders: ObjectId, idx: usize) {
    let ability = &state.objects[&defenders].abilities[idx];
    assert!(
        !has_unimplemented(ability),
        "the restriction sentence must not stay Unimplemented: {ability:#?}"
    );
    // CR 602.1b: activation instructions are not part of the effect.
    assert!(ability.condition.is_none());
    let restrictions = &ability.activation_restrictions;
    assert_eq!(restrictions.len(), 2, "got {restrictions:?}");
    assert!(
        restrictions.contains(&ActivationRestriction::DuringYourTurn),
        "got {restrictions:?}"
    );
    assert!(
        restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::Not { condition })
            } if matches!(
                condition.as_ref(),
                ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter { type_filters, .. }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                } if type_filters.as_slice() == [TypeFilter::Creature]
            )
        )),
        "'you control no creatures' gate: {restrictions:?}"
    );
}

fn astartes_tokens(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.is_token
                && obj.name == "Astartes Warrior"
        })
        .count()
}

/// CR 602.5 + CR 107.3a: on its controller's turn with no creatures (an
/// opponent's creature does not count), Defenders activates with X = 2.
#[test]
fn defenders_of_humanity_activates_on_own_turn_with_no_creatures() {
    let (mut runner, defenders, idx) = defenders_board(DefendersBoard {
        p0_creature: false,
        p0_turn: true,
    });
    assert!(can_activate_ability_now(runner.state(), P0, defenders, idx));
    let outcome = runner.activate(defenders, idx).x(2).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation must complete, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&defenders].zone, Zone::Exile);
    // CR 111.1: X = 2 creates two tokens.
    assert_eq!(astartes_tokens(runner.state()), 2);
}

/// CR 602.5: "only during your turn" refuses activation on an opponent's turn.
#[test]
fn defenders_of_humanity_refused_during_opponents_turn() {
    let (legal, defenders, idx) = defenders_board(DefendersBoard {
        p0_creature: false,
        p0_turn: true,
    });
    assert!(can_activate_ability_now(legal.state(), P0, defenders, idx));

    let (mut runner, defenders, idx) = defenders_board(DefendersBoard {
        p0_creature: false,
        p0_turn: false,
    });
    assert!(
        !can_activate_ability_now(runner.state(), P0, defenders, idx),
        "DuringYourTurn must refuse P1's turn"
    );
    refuse_activation(&mut runner, defenders, idx);
    assert_eq!(runner.state().objects[&defenders].zone, Zone::Battlefield);
    assert_eq!(astartes_tokens(runner.state()), 0);
}

/// CR 602.5: "only if you control no creatures" refuses activation while the
/// activator controls a creature.
#[test]
fn defenders_of_humanity_refused_while_controlling_a_creature() {
    let (legal, defenders, idx) = defenders_board(DefendersBoard {
        p0_creature: false,
        p0_turn: true,
    });
    assert!(can_activate_ability_now(legal.state(), P0, defenders, idx));

    let (mut runner, defenders, idx) = defenders_board(DefendersBoard {
        p0_creature: true,
        p0_turn: true,
    });
    assert!(
        !can_activate_ability_now(runner.state(), P0, defenders, idx),
        "a creature P0 controls must refuse the activation"
    );
    refuse_activation(&mut runner, defenders, idx);
    assert_eq!(runner.state().objects[&defenders].zone, Zone::Battlefield);
    assert_eq!(astartes_tokens(runner.state()), 0);
}

// ---------------------------------------------------------------------------
// Grizzled Wolverine / Rampaging Cyclops (combat)
// ---------------------------------------------------------------------------

/// Pass priority / declare `blocks` until P0 holds priority in the declare
/// blockers step (CR 509).
fn advance_to_declare_blockers_priority(runner: &mut GameRunner, blocks: &[(ObjectId, ObjectId)]) {
    for _ in 0..20 {
        let waiting = runner.state().waiting_for.clone();
        if runner.state().phase == Phase::DeclareBlockers
            && matches!(waiting, WaitingFor::Priority { .. })
        {
            assert!(
                matches!(waiting, WaitingFor::Priority { player: P0 }),
                "the active player holds priority first, got {waiting:?}"
            );
            return;
        }
        let acted = match waiting {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: blocks.to_vec(),
            }),
            other => panic!("unexpected waiting state before declare blockers: {other:?}"),
        };
        acted.expect("combat advance action");
    }
    panic!("did not reach the declare blockers step");
}

struct WolverineCombat {
    runner: GameRunner,
    wolverine: ObjectId,
    idx: usize,
}

#[derive(Clone, Copy)]
enum WolverineBlock {
    /// P1's 0/4 blocks Wolverine.
    Wolverine,
    /// Nothing blocks.
    None,
    /// P1's 0/4 blocks a second P0 attacker; Wolverine is unblocked.
    OtherAttacker,
}

/// P0 attacks P1 with Wolverine (and a second attacker); P1 blocks per `block`.
/// Ends with P0 holding priority in the declare blockers step.
fn wolverine_combat(block: WolverineBlock) -> WolverineCombat {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wolverine = scenario
        .add_creature_from_oracle(P0, "Grizzled Wolverine", 2, 2, GRIZZLED_WOLVERINE)
        .id();
    let other_attacker = scenario.add_creature(P0, "Other Attacker", 1, 1).id();
    let wall = scenario.add_creature(P1, "Opponent Wall", 0, 4).id();
    let mut runner = scenario.build();
    let idx = ability_index(runner.state(), wolverine, |effect| {
        matches!(effect, Effect::Pump { .. })
    });
    assert_wolverine_ast(runner.state(), wolverine, idx);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (wolverine, AttackTarget::Player(P1)),
            (other_attacker, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    let blocks = match block {
        WolverineBlock::Wolverine => vec![(wall, wolverine)],
        WolverineBlock::None => vec![],
        WolverineBlock::OtherAttacker => vec![(wall, other_attacker)],
    };
    advance_to_declare_blockers_priority(&mut runner, &blocks);
    // Costs are payable, so every refusal below is the restriction's.
    add_mana(&mut runner, ManaType::Red, 2);
    WolverineCombat {
        runner,
        wolverine,
        idx,
    }
}

/// Reach-guard: all three restrictions are on the ability.
fn assert_wolverine_ast(state: &GameState, wolverine: ObjectId, idx: usize) {
    let ability = &state.objects[&wolverine].abilities[idx];
    assert!(
        !has_unimplemented(ability),
        "no restriction leg may stay Unimplemented: {ability:#?}"
    );
    assert!(ability.condition.is_none());
    let restrictions = &ability.activation_restrictions;
    assert_eq!(restrictions.len(), 3, "got {restrictions:?}");
    assert!(restrictions.contains(&ActivationRestriction::OnlyOnceEachTurn));
    assert!(
        restrictions.contains(&ActivationRestriction::RequiresCondition {
            condition: Some(ParsedCondition::CurrentPhaseIs {
                phases: vec![Phase::DeclareBlockers],
            }),
        }),
        "declare blockers step gate: {restrictions:?}"
    );
    assert!(
        restrictions.iter().any(|restriction| matches!(
            restriction,
            ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter { properties, .. }),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                }),
            } if properties.as_slice() == [FilterProp::BlockingSource]
        )),
        "blocking-source count gate: {restrictions:?}"
    );
}

/// CR 602.5 + CR 509.1g: blocked, in the declare blockers step, Wolverine
/// pumps once; the once-each-turn cadence then refuses a second activation.
#[test]
fn grizzled_wolverine_pumps_when_blocked_in_declare_blockers_step() {
    let WolverineCombat {
        mut runner,
        wolverine,
        idx,
    } = wolverine_combat(WolverineBlock::Wolverine);
    assert!(can_activate_ability_now(runner.state(), P0, wolverine, idx));
    let outcome = runner.activate(wolverine, idx).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation must complete, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&wolverine].power, Some(4));
    assert_eq!(runner.state().phase, Phase::DeclareBlockers);
    // CR 602.5b: only once each turn.
    assert!(
        !can_activate_ability_now(runner.state(), P0, wolverine, idx),
        "the once-each-turn cadence must refuse a second activation"
    );
}

/// CR 509.1g: the blocking referent is Wolverine itself — unblocked (no blocks,
/// or a blocker on another attacker) refuses activation.
#[test]
fn grizzled_wolverine_refused_when_unblocked() {
    let legal = wolverine_combat(WolverineBlock::Wolverine);
    assert!(can_activate_ability_now(
        legal.runner.state(),
        P0,
        legal.wolverine,
        legal.idx
    ));

    for block in [WolverineBlock::None, WolverineBlock::OtherAttacker] {
        let WolverineCombat {
            mut runner,
            wolverine,
            idx,
        } = wolverine_combat(block);
        assert_eq!(runner.state().phase, Phase::DeclareBlockers);
        assert!(
            !can_activate_ability_now(runner.state(), P0, wolverine, idx),
            "an unblocked Wolverine must refuse the activation"
        );
        refuse_activation(&mut runner, wolverine, idx);
        assert_eq!(runner.state().objects[&wolverine].power, Some(2));
    }
}

/// CR 602.5 + CR 506.1: the step gate — still blocked, but in the combat
/// damage step (or before combat) the activation is refused.
#[test]
fn grizzled_wolverine_refused_outside_declare_blockers_step() {
    let WolverineCombat {
        mut runner,
        wolverine,
        idx,
    } = wolverine_combat(WolverineBlock::Wolverine);
    assert!(can_activate_ability_now(runner.state(), P0, wolverine, idx));
    runner.pass_both_players();
    let phase = runner.state().phase;
    assert!(
        matches!(phase, Phase::CombatDamage | Phase::EndCombat),
        "expected a later combat step, got {phase:?}"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "P0 must hold priority, got {:?}",
        runner.state().waiting_for
    );
    // The blocking leg still holds; only the step gate fails.
    assert!(
        runner.state().combat.as_ref().is_some_and(|combat| combat
            .blocker_to_attacker
            .values()
            .any(|attackers| attackers.contains(&wolverine))),
        "Wolverine must still be blocked"
    );
    add_mana(&mut runner, ManaType::Red, 1);
    assert!(
        !can_activate_ability_now(runner.state(), P0, wolverine, idx),
        "the declare blockers step gate must refuse {phase:?}"
    );
    refuse_activation(&mut runner, wolverine, idx);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wolverine = scenario
        .add_creature_from_oracle(P0, "Grizzled Wolverine", 2, 2, GRIZZLED_WOLVERINE)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 1);
    let idx = ability_index(runner.state(), wolverine, |effect| {
        matches!(effect, Effect::Pump { .. })
    });
    assert_wolverine_ast(runner.state(), wolverine, idx);
    assert!(
        !can_activate_ability_now(runner.state(), P0, wolverine, idx),
        "precombat main must refuse the activation"
    );
}

/// CR 611.3a + CR 509.1g: Rampaging Cyclops gets -2/-0 only while two or more
/// creatures block it.
#[test]
fn rampaging_cyclops_weakened_by_two_blockers() {
    for (blockers, expected_power) in [(1, 4), (2, 2)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cyclops = scenario
            .add_creature_from_oracle(P0, "Rampaging Cyclops", 4, 4, RAMPAGING_CYCLOPS)
            .id();
        let walls: Vec<ObjectId> = (0..blockers)
            .map(|i| {
                scenario
                    .add_creature(P1, &format!("Opponent Blocker {i}"), 1, 1)
                    .id()
            })
            .collect();
        let mut runner = scenario.build();
        assert_eq!(
            runner.state().objects[&cyclops].power,
            Some(4),
            "unblocked outside combat"
        );
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(cyclops, AttackTarget::Player(P1))])
            .expect("declare attackers");
        let blocks: Vec<(ObjectId, ObjectId)> = walls.iter().map(|&w| (w, cyclops)).collect();
        advance_to_declare_blockers_priority(&mut runner, &blocks);
        assert_eq!(
            runner.state().objects[&cyclops].power,
            Some(expected_power),
            "{blockers} blocker(s)"
        );
    }
}
