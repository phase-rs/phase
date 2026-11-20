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
//!
//! Sawback Manticore exercises a separate source-state gate: the source
//! itself must be attacking or blocking, independent of its target or cadence.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::restrictions::ability_usage_key;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, ActivationRestriction, Comparator, Effect, FilterProp, ParsedCondition,
    QuantityExpr, QuantityRef, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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

// Verbatim Scryfall Oracle.
const SAWBACK_MANTICORE: &str = "{4}: This creature gains flying until end of turn.\n{1}: This creature deals 2 damage to target attacking or blocking creature. Activate only if this creature is attacking or blocking and only once each turn.";
const RECONNAISSANCE: &str = "{0}: Remove target attacking creature you control from combat and untap it. (If you activate during end of combat, the creature will untap after it deals combat damage.)";
const QUICKSILVER_ELEMENTAL: &str = "{U}: This creature gains all activated abilities of target creature until end of turn. (If any of the abilities use that creature's name, use this creature's name instead.)\nYou may spend blue mana as though it were mana of any color to pay the activation costs of this creature's abilities.";
const SEAT_OF_THE_SYNOD: &str = "{T}: Add {U}.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

fn add_manticore(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature(player, "Sawback Manticore", 2, 4)
        .with_subtypes(vec!["Manticore"])
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{R}{G}"))
        .from_oracle_text(SAWBACK_MANTICORE)
        .id()
}

fn assert_printed_creature(
    runner: &GameRunner,
    id: ObjectId,
    subtype: &str,
    stats: (i32, i32),
    cost: &str,
) {
    let card = &runner.state().objects[&id];
    assert_eq!(card.zone, Zone::Battlefield);
    assert_eq!(card.card_types.core_types, [CoreType::Creature]);
    assert_eq!(card.card_types.subtypes, [subtype]);
    assert!(card.card_types.supertypes.is_empty());
    assert_eq!((card.power, card.toughness), (Some(stats.0), Some(stats.1)));
    assert_eq!(card.mana_cost, parse_mtgjson_mana_cost(cost));
}

fn damage_index(runner: &GameRunner, source: ObjectId) -> usize {
    ability_index(runner.state(), source, |effect| {
        matches!(
            effect,
            Effect::DealDamage {
                amount: _,
                target: _,
                damage_source: _,
                excess: _,
            }
        )
    })
}

fn activation_action(source_id: ObjectId, ability_index: usize) -> GameAction {
    GameAction::ActivateAbility {
        source_id,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    }
}

fn damage_offered(runner: &GameRunner, source: ObjectId, index: usize) -> bool {
    legal_actions(runner.state()).iter().any(|action| {
        matches!(action, GameAction::ActivateAbility { source_id, ability_index, payment_mode: _ }
            if (*source_id, *ability_index) == (source, index))
    })
}

fn add_blue_sources<const N: usize>(
    scenario: &mut GameScenario,
    player: PlayerId,
) -> [ObjectId; N] {
    std::array::from_fn(|_| {
        scenario
            .add_land_from_oracle(player, "Seat of the Synod", SEAT_OF_THE_SYNOD)
            .as_artifact_land()
            .id()
    })
}

fn tap_blue_sources(runner: &mut GameRunner, lands: &[ObjectId]) {
    for &land in lands {
        let card = &runner.state().objects[&land];
        assert_eq!(card.card_types.core_types.len(), 2);
        assert!(card.card_types.core_types.contains(&CoreType::Artifact));
        assert!(card.card_types.core_types.contains(&CoreType::Land));
        assert!(card.card_types.supertypes.is_empty() && card.card_types.subtypes.is_empty());
        assert_eq!((card.power, card.toughness), (None, None));
        runner
            .act(activation_action(land, 0))
            .expect("tap printed mana source");
        assert!(runner.state().objects[&land].tapped);
    }
}

fn activation_snapshot(
    runner: &GameRunner,
    player: PlayerId,
    source: ObjectId,
    index: usize,
) -> (usize, u32, u32, usize, usize, bool) {
    let state = runner.state();
    let key = ability_usage_key(state, source, index).expect("live ability usage key");
    (
        state
            .players
            .iter()
            .find(|seat| seat.id == player)
            .unwrap()
            .mana_pool
            .total(),
        state
            .activated_abilities_this_turn
            .get(&key)
            .copied()
            .unwrap_or(0),
        state
            .activated_abilities_this_game
            .get(&key)
            .copied()
            .unwrap_or(0),
        state
            .abilities_activated_this_turn_by_player
            .get(&player)
            .map_or(0, |rows| rows.len()),
        state.stack.len(),
        state.pending_cast.is_some(),
    )
}

fn refuse_manticore_damage(
    runner: &mut GameRunner,
    player: PlayerId,
    source: ObjectId,
    restriction: &str,
) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player)
    );
    let index = damage_index(runner, source);
    let before = activation_snapshot(runner, player, source, index);
    assert!(before.0 > 0, "payment must not mask restriction");
    let offered = damage_offered(runner, source, index);
    let result = runner.act(activation_action(source, index));
    let after = activation_snapshot(runner, player, source, index);
    let waiting = runner.state().waiting_for.variant_name();
    println!(
        "{:?}: {restriction}; offered={offered}; submission={:?}; before={before:?}; after={after:?}; waiting={waiting}",
        runner.state().phase,
        result.as_ref().map(|_| ())
    );
    assert!(
        matches!(result, Err(EngineError::ActionNotAllowed(message))
            if message.contains(restriction)),
        "expected refusal by {restriction}, not lack of a target or another prerequisite"
    );
    assert!(!offered);
    assert_eq!(after, before);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player)
    );
}

fn activate_manticore_damage(
    runner: &mut GameRunner,
    player: PlayerId,
    source: ObjectId,
    target: ObjectId,
    other_target: ObjectId,
) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player)
    );
    assert_eq!(runner.state().objects[&source].controller, player);
    let index = damage_index(runner, source);
    let before = activation_snapshot(runner, player, source, index);
    assert!(damage_offered(runner, source, index));
    runner.act(activation_action(source, index)).unwrap();
    let WaitingFor::TargetSelection {
        player: chooser,
        pending_cast: _,
        target_slots: _,
        mode_labels: _,
        selection,
    } = &runner.state().waiting_for
    else {
        panic!("several legal creatures must open target choice");
    };
    assert_eq!(*chooser, player);
    assert!(selection
        .current_legal_targets
        .contains(&TargetRef::Object(target)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap();
    let entry = runner
        .state()
        .stack
        .back()
        .expect("targeted damage on stack");
    assert_eq!((entry.source_id, entry.controller), (source, player));
    let StackEntryKind::ActivatedAbility { source_id, ability } = &entry.kind else {
        panic!("expected activated ability, got {:?}", entry.kind);
    };
    assert_eq!(*source_id, source);
    assert_eq!(ability.targets, [TargetRef::Object(target)]);
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: _ }
    ));
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&target].damage_marked, 2);
    assert_eq!(runner.state().objects[&other_target].damage_marked, 0);
    let after = activation_snapshot(runner, player, source, index);
    assert_eq!(after, (before.0 - 1, 1, 1, before.3 + 1, 0, false));
    println!(
        "{:?}: {source:?} dealt 2 damage to selected {target:?}, paid one and committed one use",
        runner.state().phase
    );
}

fn attack(runner: &mut GameRunner, attackers: &[ObjectId]) {
    runner.advance_to_combat();
    let declarations: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    runner.declare_attackers(&declarations).unwrap();
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 508.1k / CR 602.5: Attacking satisfies the gate, but not the second use.
#[test]
fn manticore_attacking_can_deal_damage_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = add_manticore(&mut scenario, P0);
    let target = scenario.add_vanilla(P0, 2, 5);
    let lands = add_blue_sources::<2>(&mut scenario, P0);
    let mut runner = scenario.build();
    assert_printed_creature(&runner, source, "Manticore", (2, 4), "{3}{R}{G}");
    attack(&mut runner, &[source, target]);
    tap_blue_sources(&mut runner, &lands);
    activate_manticore_damage(&mut runner, P0, source, target, source);
    refuse_manticore_damage(&mut runner, P0, source, "OnlyOnceEachTurn");
}

/// CR 509.1g / CR 602.5: Blocking satisfies the same gate on the other turn.
#[test]
fn manticore_blocking_can_deal_damage_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = add_manticore(&mut scenario, P1);
    let blocked = scenario.add_vanilla(P0, 2, 5);
    let other_attacker = scenario.add_vanilla(P0, 2, 5);
    let target = scenario.add_vanilla(P1, 0, 5);
    let lands = add_blue_sources::<2>(&mut scenario, P1);
    let mut runner = scenario.build();
    attack(&mut runner, &[blocked, other_attacker]);
    advance_to_declare_blockers_priority(
        &mut runner,
        &[(source, blocked), (target, other_attacker)],
    );
    runner.act(GameAction::PassPriority).unwrap();
    tap_blue_sources(&mut runner, &lands);
    activate_manticore_damage(&mut runner, P1, source, target, blocked);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    refuse_manticore_damage(&mut runner, P1, source, "OnlyOnceEachTurn");
}

#[test]
fn manticore_idle_during_combat_cannot_use_another_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = add_manticore(&mut scenario, P0);
    let targets = [
        scenario.add_vanilla(P0, 2, 5),
        scenario.add_vanilla(P0, 2, 5),
    ];
    let lands = add_blue_sources::<1>(&mut scenario, P0);
    let mut runner = scenario.build();
    attack(&mut runner, &targets);
    tap_blue_sources(&mut runner, &lands);
    refuse_manticore_damage(&mut runner, P0, source, "SourceIsAttackingOrBlocking");
}

/// CR 506.4: Live participation ends when an effect removes the source.
#[test]
fn manticore_removed_from_combat_loses_permission_immediately() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = add_manticore(&mut scenario, P0);
    let targets = [
        scenario.add_vanilla(P0, 2, 5),
        scenario.add_vanilla(P0, 2, 5),
    ];
    let enchantment = scenario
        .add_enchantment_from_oracle(P0, "Reconnaissance", RECONNAISSANCE)
        .with_mana_cost(parse_mtgjson_mana_cost("{W}"))
        .id();
    let lands = add_blue_sources::<1>(&mut scenario, P0);
    let mut runner = scenario.build();
    let card = &runner.state().objects[&enchantment];
    assert_eq!(card.card_types.core_types, [CoreType::Enchantment]);
    assert!(card.card_types.supertypes.is_empty() && card.card_types.subtypes.is_empty());
    assert_eq!((card.power, card.toughness), (None, None));
    assert_eq!(card.mana_cost, parse_mtgjson_mana_cost("{W}"));
    attack(&mut runner, &[source, targets[0], targets[1]]);
    tap_blue_sources(&mut runner, &lands);
    assert!(damage_offered(
        &runner,
        source,
        damage_index(&runner, source)
    ));
    runner
        .activate(enchantment, 0)
        .target_object(source)
        .resolve();
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat is still live");
    assert!(!combat.attackers.iter().any(|a| a.object_id == source));
    assert!(targets
        .iter()
        .all(|target| combat.attackers.iter().any(|a| a.object_id == *target)));
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert!(!runner.state().objects[&source].tapped);
    refuse_manticore_damage(&mut runner, P0, source, "SourceIsAttackingOrBlocking");
}

/// CR 602.5: Check the source gate even when there are also
/// no legal damage targets, and leave the separate flying activation alone.
#[test]
fn manticore_damage_refused_before_combat_without_restricting_flying() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = add_manticore(&mut scenario, P0);
    let lands = add_blue_sources::<4>(&mut scenario, P0);
    let mut runner = scenario.build();
    tap_blue_sources(&mut runner, &lands);
    refuse_manticore_damage(&mut runner, P0, source, "SourceIsAttackingOrBlocking");
    let flying = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .description
                .as_deref()
                .is_some_and(|text| text.starts_with("{4}:"))
        })
        .expect("printed flying ability");
    assert!(can_activate_ability_now(runner.state(), P0, source, flying));
    runner.activate(source, flying).resolve();
    assert!(runner.state().objects[&source].has_keyword(&Keyword::Flying));
    assert_eq!(activation_snapshot(&runner, P0, source, flying).0, 0);
}

/// CR 511.3: Leaving combat ends live participation.
#[test]
fn manticore_damage_refused_after_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = add_manticore(&mut scenario, P0);
    scenario.add_vanilla(P1, 0, 5);
    let lands = add_blue_sources::<1>(&mut scenario, P0);
    let mut runner = scenario.build();
    attack(&mut runner, &[source]);
    advance_to_declare_blockers_priority(&mut runner, &[]);
    for _ in 0..12 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass through combat");
    }
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert!(runner.state().combat.is_none());
    assert_eq!(runner.life(P1), 18);
    tap_blue_sources(&mut runner, &lands);
    refuse_manticore_damage(&mut runner, P0, source, "SourceIsAttackingOrBlocking");
}

/// CR 113.7 + CR 700.7: “This creature” identifies the new source, not the
/// donor. CR 602.5c: The restriction follows the acquired ability.
#[test]
fn manticore_gained_damage_checks_quicksilver_not_opposing_donor() {
    for source_blocks in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let donor = add_manticore(&mut scenario, P0);
        let source = scenario
            .add_creature(P1, "Quicksilver Elemental", 3, 4)
            .with_subtypes(vec!["Elemental"])
            .with_mana_cost(parse_mtgjson_mana_cost("{3}{U}{U}"))
            .from_oracle_text(QUICKSILVER_ELEMENTAL)
            .id();
        let unsummon = scenario
            .add_spell_to_hand_from_oracle(P1, "Unsummon", true, UNSUMMON)
            .with_mana_cost(parse_mtgjson_mana_cost("{U}"))
            .id();
        let blocked = scenario.add_vanilla(P0, 2, 5);
        let target = scenario.add_vanilla(P0, 2, 5);
        let other_blocker = scenario.add_vanilla(P1, 0, 5);
        let lands = add_blue_sources::<3>(&mut scenario, P1);
        let mut runner = scenario.build();
        assert_printed_creature(&runner, source, "Elemental", (3, 4), "{3}{U}{U}");
        let spell = &runner.state().objects[&unsummon];
        assert_eq!(spell.card_types.core_types, [CoreType::Instant]);
        assert!(spell.card_types.supertypes.is_empty() && spell.card_types.subtypes.is_empty());
        assert_eq!((spell.power, spell.toughness), (None, None));
        assert_eq!(spell.mana_cost, parse_mtgjson_mana_cost("{U}"));
        let mut attackers = vec![blocked, target];
        if !source_blocks {
            attackers.push(donor);
        }
        attack(&mut runner, &attackers);
        let blocker = if source_blocks { source } else { other_blocker };
        advance_to_declare_blockers_priority(&mut runner, &[(blocker, blocked)]);
        let combat = runner.state().combat.as_ref().expect("live combat");
        assert_eq!(combat.blocker_to_attacker[&blocker], [blocked]);
        runner.act(GameAction::PassPriority).unwrap();
        tap_blue_sources(&mut runner, &lands);
        let gain = ability_index(runner.state(), source, |effect| {
            matches!(
                effect,
                Effect::GainActivatedAbilitiesOfTarget {
                    target: _,
                    recipient: _,
                    scope: _,
                    duration: _,
                }
            )
        });
        runner.activate(source, gain).target_object(donor).resolve();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ) {
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().objects[&donor].controller, P0);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        if source_blocks {
            runner.cast(unsummon).target_object(donor).resolve();
            assert_eq!(runner.state().objects[&donor].zone, Zone::Hand);
            if matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ) {
                runner.act(GameAction::PassPriority).unwrap();
            }
            activate_manticore_damage(&mut runner, P1, source, target, blocked);
        } else {
            refuse_manticore_damage(&mut runner, P1, source, "SourceIsAttackingOrBlocking");
        }
    }
}
