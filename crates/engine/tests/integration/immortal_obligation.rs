//! Immortal Obligation ({1}{W} Instant), end to end through the cast pipeline.
//!
//! Oracle text (MTGJSON 5.3.0+20261016, verbatim): "Return target creature card
//! from an opponent's graveyard to the battlefield under their control with a
//! duty counter on it. For as long as that creature has a duty counter on it,
//! it is goaded, can't attack you or a permanent you control, and can't block
//! creatures you control."
//!
//! Clause by clause:
//! - CR 608.2c: the return is a one-shot instruction; its target is a creature
//!   card in an opponent's graveyard.
//! - CR 108.3 + CR 110.2a: "under their control" is the card's owner.
//! - CR 122.1 + CR 122.6: it enters with one duty counter.
//! - CR 400.7 + CR 608.2c + CR 611.2b: "that creature" is the permanent the card
//!   became, and the duration ends when it has no duty counter.
//! - CR 701.15b + CR 611.2a: it is goaded by the spell's controller for that
//!   duration.
//! - CR 109.5 + CR 508.1b + CR 508.1c: it can't attack the spell's controller
//!   or a planeswalker or battle that player controls ("you" fixed at
//!   resolution, CR 608.2c).
//! - CR 509.1b: it can't block creatures the spell's controller controls.
//!
//! Bearing rulings (Promise of Loyalty, the same counter-bound shape): "The
//! creature you put a vow counter on will remember its promise if another
//! player gains control of it." (the control-change row); "If the vow counter is
//! moved to another creature, it won't prevent the second creature from
//! attacking normally." (the moved-counter row); "If the vow counter is removed
//! from the creature, it can attack as normal." (the removal row).
//!
//! Window facts:
//! - Attack windows are reached by production priority passing only
//!   (`next_turn`). The returned creature is summoning sick on entry, and only
//!   its controller's turn start clears it (CR 302.6); no test writes
//!   `summoning_sick`.
//! - From the active player's declaration, three priority passes (one per
//!   player, CR 508.2 + CR 117.3d) reach the defending player's
//!   `DeclareBlockers` prompt.
//! - The defending player always controls a second creature that may block, so
//!   the prompt is real.
//! - A block window whose attacker is not the active player's is set up with
//!   `arm_combat` on a clone; its block decision is still the production
//!   `declare_blockers`.

use crate::support::shared_card_db;
use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    ChosenAttribute, ContinuousModification, DefinitionOrigin, Duration, FilterProp,
    StaticCondition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::{CounterMatch, CounterType};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Immortal Obligation, verbatim.
const IMMORTAL_OBLIGATION: &str = "Return target creature card from an opponent's graveyard to the battlefield under their control with a duty counter on it. For as long as that creature has a duty counter on it, it is goaded, can't attack you or a permanent you control, and can't block creatures you control.";
/// Ajani, Caller of the Pride, verbatim.
const AJANI_CALLER_OF_THE_PRIDE: &str = "[+1]: Put a +1/+1 counter on up to one target creature.\n[\u{2212}3]: Target creature gains flying and double strike until end of turn.\n[\u{2212}8]: Create X 2/2 white Cat creature tokens, where X is your life total.";
/// Fate Transfer, verbatim.
const FATE_TRANSFER: &str = "Move all counters from target creature onto another target creature.";

/// `validate_declaration_core`'s per-target restriction loop (CR 508.1c).
const ATTACK_RESTRICTION: &str = "(CR 508.1c/d attack restriction)";
/// The CR 508.1d requirement bar.
const ATTACK_REQUIREMENT: &str = "attack requirement(s) but";
/// `validate_blockers_core`'s `BlockRestriction` loop (CR 509.1b).
const BLOCK_RESTRICTION: &str = "can block only creatures matching the block restriction";

fn duty() -> CounterType {
    CounterType::Generic("duty".to_string())
}

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    /// The creature card in P1's graveyard; the engine keeps its id across the
    /// return, so after resolution it names the returned permanent.
    card: ObjectId,
    /// P0's creature (the caster's).
    c0: ObjectId,
    /// P1's creature (the owner's second legal blocker).
    p1c: ObjectId,
    /// P2's creature (the third player's).
    c2: ObjectId,
    ajani: Option<ObjectId>,
    fate_transfers: Vec<ObjectId>,
}

/// Three players (so goad has a legal player other than the caster), P0's
/// precombat main. Grizzly Bears is printed with no rules text.
fn board(with_ajani: bool, fate_transfers: usize) -> Board {
    board_with_setup(with_ajani, fate_transfers, |_| ()).0
}

fn board_with_setup<T>(
    with_ajani: bool,
    fate_transfers: usize,
    setup: impl FnOnce(&mut GameScenario) -> T,
) -> (Board, T) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..3 {
        for _ in 0..10 {
            scenario.add_card_to_library_top(PlayerId(seat), "Filler");
        }
    }
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::White);
    let c0 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let p1c = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let c2 = scenario.add_creature(P2, "Grizzly Bears", 2, 2).id();
    let card = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let ajani = with_ajani.then(|| {
        scenario
            .add_planeswalker_from_oracle(
                P0,
                "Ajani, Caller of the Pride",
                "Ajani",
                4,
                AJANI_CALLER_OF_THE_PRIDE,
            )
            .id()
    });
    let fate_transfers = (0..fate_transfers)
        .map(|_| {
            scenario.add_basic_land(P0, ManaColor::Blue);
            scenario.add_basic_land(P0, ManaColor::Blue);
            scenario
                .add_spell_to_hand_from_oracle(P0, "Fate Transfer", true, FATE_TRANSFER)
                .id()
        })
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Immortal Obligation", true, IMMORTAL_OBLIGATION)
        .id();
    let extra = setup(&mut scenario);
    let board = Board {
        runner: scenario.build(),
        spell,
        card,
        c0,
        p1c,
        c2,
        ajani,
        fate_transfers,
    };
    (board, extra)
}

/// Cast Immortal Obligation at the card in P1's graveyard and resolve it to the
/// completion boundary (empty stack, priority).
fn cast(board: &mut Board) -> ObjectId {
    board
        .runner
        .cast(board.spell)
        .target_object(board.card)
        .resolve();
    assert_settled(&board.runner);
    board.card
}

fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "the stack must be empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected priority, got {:?}",
        runner.state().waiting_for.variant_name()
    );
}

/// Re-evaluate layers after a hand edit of counters or control.
fn refresh(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

fn set_duty(runner: &mut GameRunner, id: ObjectId, count: Option<u32>) {
    let counters = &mut runner.state_mut().objects.get_mut(&id).unwrap().counters;
    match count {
        Some(n) => {
            counters.insert(duty(), n);
        }
        None => {
            counters.remove(&duty());
        }
    }
    refresh(runner);
}

fn duty_count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&duty())
        .copied()
        .unwrap_or(0)
}

/// CR 701.15b: the live goad designation, read through the engine's filter.
fn is_goaded(runner: &GameRunner, id: ObjectId) -> bool {
    matches_target_filter(
        runner.state(),
        id,
        &TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded])),
        &FilterContext::neutral(),
    )
}

fn effects_on(
    runner: &GameRunner,
    id: ObjectId,
) -> Vec<&engine::types::game_state::TransientContinuousEffect> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| effect.affected == TargetFilter::SpecificObject { id })
        .collect()
}

/// The controllers of the Goaded effects on `id` (the goaders, CR 701.15b).
fn goad_tce_controllers(runner: &GameRunner, id: ObjectId) -> Vec<PlayerId> {
    effects_on(runner, id)
        .into_iter()
        .filter(|effect| {
            effect
                .modifications
                .contains(&ContinuousModification::AddStaticMode {
                    mode: StaticMode::Goaded,
                })
        })
        .map(|effect| effect.controller)
        .collect()
}

fn granted_cant_attack_controllers(runner: &GameRunner, id: ObjectId) -> Vec<Option<PlayerId>> {
    runner.state().objects[&id]
        .static_definitions
        .iter_all()
        .filter(|def| def.mode == StaticMode::CantAttack)
        .map(|def| def.source_controller)
        .collect()
}

/// Production priority passing, one pass at a time, to `expected`'s precombat
/// main. Seats not under test declare no attackers and no blockers.
fn next_turn(runner: &mut GameRunner, expected: PlayerId) {
    for _ in 0..400 {
        let state = runner.state();
        if state.active_player == expected
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            other => panic!("next_turn: unexpected {:?}", other.variant_name()),
        }
    }
    panic!("{expected:?}'s turn never arrived");
}

/// Pass priority to the active player's declare-attackers prompt and return its
/// legal attackers.
fn attack_prompt(runner: &mut GameRunner) -> Vec<ObjectId> {
    for _ in 0..50 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                ..
            } => {
                assert_eq!(*player, runner.state().active_player);
                return valid_attacker_ids.clone();
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("attack_prompt: unexpected {:?}", other.variant_name()),
        }
    }
    panic!("no declare-attackers prompt");
}

/// Set up a declare-attackers prompt for a seat that is not active (local copy
/// of `dack_fayden_helping_hand.rs::arm_combat`).
fn arm_combat(runner: &mut GameRunner, active: PlayerId) {
    let state = runner.state_mut();
    state.active_player = active;
    state.priority_player = active;
    state.phase = Phase::DeclareAttackers;
    state.layers_dirty.mark_full();
    evaluate_layers(state);
    state.waiting_for = build_declare_attackers_waiting_for(state);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
}

/// Try an attack declaration on a clone.
fn declare(runner: &GameRunner, attacks: &[(ObjectId, AttackTarget)]) -> Result<(), String> {
    GameRunner::from_state(runner.state().clone())
        .declare_attackers(attacks)
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

fn at(player: PlayerId) -> AttackTarget {
    AttackTarget::Player(player)
}

fn assert_refused(result: Result<(), String>, reason: &str) {
    match result {
        Err(error) => assert!(error.contains(reason), "expected {reason:?}, got {error}"),
        Ok(()) => panic!("expected a refusal containing {reason:?}"),
    }
}

/// On a clone: reach a declare-attackers prompt (production on the active
/// player's turn, or `arm_combat` for `arm`'s seat), declare `attacker` at
/// `defender`, and pass priority to `defender`'s `DeclareBlockers` prompt.
fn blockers_window(
    runner: &GameRunner,
    arm: Option<PlayerId>,
    attacker: ObjectId,
    defender: PlayerId,
) -> (GameRunner, Vec<ObjectId>) {
    let mut window = GameRunner::from_state(runner.state().clone());
    match arm {
        Some(seat) => arm_combat(&mut window, seat),
        None => {
            attack_prompt(&mut window);
        }
    }
    window
        .declare_attackers(&[(attacker, at(defender))])
        .expect("the attack is legal");
    let mut passes = 0;
    loop {
        match &window.state().waiting_for {
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                ..
            } => {
                assert_eq!(*player, defender);
                // CR 508.2 + CR 117.3d: one pass per player.
                assert_eq!(passes, 3, "priority passes to the blockers prompt");
                let valid = valid_blocker_ids.clone();
                return (window, valid);
            }
            WaitingFor::Priority { .. } => {
                assert!(passes < 3, "no blockers prompt after three passes");
                window.act(GameAction::PassPriority).expect("pass priority");
                passes += 1;
            }
            other => panic!("blockers_window: unexpected {:?}", other.variant_name()),
        }
    }
}

/// Try a block declaration on a clone.
fn block(window: &GameRunner, blocker: ObjectId, attacker: ObjectId) -> Result<(), String> {
    GameRunner::from_state(window.state().clone())
        .declare_blockers(&[(blocker, attacker)])
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

fn duty_duration() -> Duration {
    Duration::ForAsLongAs {
        condition: StaticCondition::RecipientHasCounters {
            counters: CounterMatch::OfType(duty()),
            minimum: 1,
            maximum: None,
        },
    }
}

/// CR 110.2a + CR 122.6: the card returns under its owner's control with one
/// duty counter, and the three parts are effects of the caster on that object.
#[test]
fn returns_under_its_owners_control_with_a_duty_counter() {
    let mut board = board(false, 0);
    let ret = cast(&mut board);
    let runner = &board.runner;
    let object = &runner.state().objects[&ret];
    assert_eq!(object.zone, Zone::Battlefield);
    assert!(runner.state().battlefield.contains(&ret));
    assert_eq!(object.controller, P1);
    assert_eq!(object.owner, P1);
    assert_eq!(duty_count(runner, ret), 1);
    let effects = effects_on(runner, ret);
    assert_eq!(effects.len(), 3, "{effects:#?}");
    for effect in effects {
        assert_eq!(effect.controller, P0);
        assert_eq!(effect.duration, duty_duration());
    }
}

/// CR 701.15b + CR 508.1c + CR 508.1d: goaded by the caster, it must attack, and
/// it can't attack the caster.
#[test]
fn returned_creature_is_goaded_by_the_caster() {
    let mut board = board(false, 0);
    let ret = cast(&mut board);
    let p1c = board.p1c;
    let runner = &mut board.runner;
    assert!(is_goaded(runner, ret));
    assert_eq!(goad_tce_controllers(runner, ret), vec![P0]);
    assert!(runner.state().objects[&ret].goaded_by.is_empty());
    assert!(runner.state().objects[&ret].summoning_sick);
    // CR 109.5 + CR 608.2c: the attack prohibition's "you" is latched.
    assert_eq!(granted_cant_attack_controllers(runner, ret), vec![Some(P0)]);

    next_turn(runner, P1);
    assert!(!runner.state().objects[&ret].summoning_sick);
    let valid = attack_prompt(runner);
    assert!(valid.contains(&ret), "CR 302.6 reach-guard: {valid:?}");
    assert_refused(declare(runner, &[]), ATTACK_REQUIREMENT);
    assert_refused(declare(runner, &[(ret, at(P0))]), ATTACK_RESTRICTION);
    assert_eq!(declare(runner, &[(ret, at(P2))]), Ok(()));
    // Paired positives: P1's unmarked creature may attack the caster, as long
    // as the goaded creature attacks someone.
    assert_eq!(declare(runner, &[(ret, at(P2)), (p1c, at(P0))]), Ok(()));
    assert_refused(declare(runner, &[(p1c, at(P0))]), ATTACK_REQUIREMENT);
}

/// CR 508.1b + CR 508.1c: "a permanent you control" includes the caster's
/// planeswalker.
#[test]
fn returned_creature_cannot_attack_the_casters_planeswalker() {
    let mut board = board(true, 0);
    let ret = cast(&mut board);
    let ajani = board.ajani.unwrap();
    let p1c = board.p1c;
    let runner = &mut board.runner;
    next_turn(runner, P1);
    let valid = attack_prompt(runner);
    assert!(valid.contains(&ret), "CR 302.6 reach-guard: {valid:?}");
    assert_refused(
        declare(runner, &[(ret, AttackTarget::Planeswalker(ajani))]),
        ATTACK_RESTRICTION,
    );
    assert_eq!(
        declare(
            runner,
            &[(ret, at(P2)), (p1c, AttackTarget::Planeswalker(ajani))]
        ),
        Ok(())
    );
}

/// CR 310.9b: P1 can attack the Siege protected by P2.
/// CR 508.1c: Immortal Obligation forbids the returned creature from attacking
/// that Siege because the caster controls it.
#[test]
fn returned_creature_cannot_attack_the_casters_battle() {
    let (mut board, siege) = board_with_setup(false, 0, |scenario| {
        let db = shared_card_db().expect("fixture database");
        let siege = scenario.add_real_card(P0, "Invasion of Gobakhan", Zone::Battlefield, db);
        // Settle the fixture's as-enters setup before casting Immortal Obligation.
        scenario.at_phase(Phase::PreCombatMain);
        siege
    });
    let battle = board.runner.state_mut().objects.get_mut(&siege).unwrap();
    battle.chosen_attributes.push(ChosenAttribute::Player(P2));
    battle.counters.insert(CounterType::Defense, 3);

    let ret = cast(&mut board);
    let p1c = board.p1c;
    let runner = &mut board.runner;
    next_turn(runner, P1);
    let valid = attack_prompt(runner);
    assert!(valid.contains(&ret), "{valid:?}");
    let battle = &runner.state().objects[&siege];
    assert_eq!(battle.controller, P0);
    assert_eq!(battle.protector(), Some(P2));
    assert_refused(
        declare(runner, &[(ret, AttackTarget::Battle(siege))]),
        ATTACK_RESTRICTION,
    );
    assert_eq!(
        declare(runner, &[(ret, at(P2)), (p1c, AttackTarget::Battle(siege))]),
        Ok(())
    );
}

/// CR 509.1b + CR 109.5 + CR 608.2c: it can't block the caster's creatures and
/// may block other players' creatures.
#[test]
fn returned_creature_cannot_block_the_casters_creatures() {
    let mut board = board(false, 0);
    let ret = cast(&mut board);
    let runner = &board.runner;

    let (window, valid) = blockers_window(runner, None, board.c0, P1);
    assert!(!valid.contains(&ret), "{valid:?}");
    assert_refused(block(&window, ret, board.c0), BLOCK_RESTRICTION);
    assert_eq!(block(&window, board.p1c, board.c0), Ok(()));

    let (window, valid) = blockers_window(runner, Some(P2), board.c2, P1);
    assert!(valid.contains(&ret), "{valid:?}");
    assert_eq!(block(&window, ret, board.c2), Ok(()));
}

#[test]
fn duty_counter_restrictions_survive_dress_down() {
    let (mut board, dress_down) = board_with_setup(true, 1, |scenario| {
        let db = shared_card_db().expect("fixture database");
        scenario.add_basic_land(P0, ManaColor::Blue);
        scenario.add_basic_land(P0, ManaColor::Blue);
        scenario.add_real_card(P0, "Dress Down", Zone::Hand, db)
    });
    let ret = cast(&mut board);
    let mut blocking = GameRunner::from_state(board.runner.state().clone());
    blocking.cast(dress_down).resolve();
    assert_settled(&blocking);
    assert_eq!(
        blocking.state().objects[&dress_down].zone,
        Zone::Battlefield
    );
    assert_eq!(duty_count(&blocking, ret), 1);

    // CR 611.2c + CR 613.1f: The rule effects survive ability removal.
    assert_eq!(
        blocking.state().objects[&ret].granted_abilities_from,
        Some(0)
    );
    let statics = &blocking.state().objects[&ret].static_definitions;
    let attack = statics
        .iter_all()
        .find(|def| def.mode == StaticMode::CantAttack)
        .expect("live attack restriction after Dress Down");
    assert_eq!(attack.origin, DefinitionOrigin::Resolution);
    let block_restriction = statics
        .iter_all()
        .find(|def| matches!(def.mode, StaticMode::BlockRestriction { .. }))
        .expect("live block restriction after Dress Down");
    assert_eq!(block_restriction.origin, DefinitionOrigin::Resolution);

    // CR 509.1b: P1's other creature can block the same attacker.
    let (window, valid) = blockers_window(&blocking, None, board.c0, P1);
    assert!(!valid.contains(&ret), "{valid:?}");
    assert_refused(block(&window, ret, board.c0), BLOCK_RESTRICTION);
    assert_eq!(block(&window, board.p1c, board.c0), Ok(()));

    // CR 611.2b: Moving the last duty counter ends the restrictions.
    blocking
        .cast(board.fate_transfers[0])
        .target_objects(&[ret, board.p1c])
        .resolve();
    assert_settled(&blocking);
    assert_eq!(
        blocking.state().objects[&dress_down].zone,
        Zone::Battlefield
    );
    assert_eq!(duty_count(&blocking, ret), 0);
    assert_eq!(duty_count(&blocking, board.p1c), 1);
    assert!(!is_goaded(&blocking, ret));
    let (window, _) = blockers_window(&blocking, None, board.c0, P1);
    assert_eq!(block(&window, ret, board.c0), Ok(()));

    // Resolve Dress Down on P1's turn too, so it is present for the attack.
    let attacking = &mut board.runner;
    next_turn(attacking, P1);
    for _ in 0..2 {
        if attacking.state().priority_player == P0 {
            break;
        }
        attacking
            .act(GameAction::PassPriority)
            .expect("pass to Dress Down's caster");
    }
    assert_eq!(attacking.state().priority_player, P0);
    attacking.cast(dress_down).resolve();
    assert_settled(attacking);
    assert_eq!(
        attacking.state().objects[&dress_down].zone,
        Zone::Battlefield
    );
    assert_eq!(duty_count(attacking, ret), 1);
    assert!(is_goaded(attacking, ret));
    let mut without_duty = GameRunner::from_state(attacking.state().clone());
    set_duty(&mut without_duty, ret, None);

    // CR 508.1c + CR 508.1d + CR 701.15b: It must attack and has a legal defender.
    let valid = attack_prompt(attacking);
    assert!(valid.contains(&ret), "{valid:?}");
    assert_refused(declare(attacking, &[]), ATTACK_REQUIREMENT);
    assert_refused(declare(attacking, &[(ret, at(P0))]), ATTACK_RESTRICTION);
    assert_refused(
        declare(
            attacking,
            &[(ret, AttackTarget::Planeswalker(board.ajani.unwrap()))],
        ),
        ATTACK_RESTRICTION,
    );
    assert_eq!(declare(attacking, &[(ret, at(P2))]), Ok(()));

    // CR 611.2b: The attack restriction and goad requirement expire together.
    let valid = attack_prompt(&mut without_duty);
    assert!(valid.contains(&ret), "{valid:?}");
    assert!(!is_goaded(&without_duty, ret));
    assert_eq!(declare(&without_duty, &[]), Ok(()));
    assert_eq!(declare(&without_duty, &[(ret, at(P0))]), Ok(()));
}

/// CR 611.2b: removing the duty counter ends the goad, attack and block parts.
#[test]
fn removing_the_duty_counter_lifts_everything() {
    let mut board = board(false, 0);
    let ret = cast(&mut board);
    let runner = &mut board.runner;
    assert!(
        is_goaded(runner, ret),
        "reach-guard: goaded after resolution"
    );

    set_duty(runner, ret, None);
    assert!(!is_goaded(runner, ret));
    assert!(effects_on(runner, ret).is_empty());
    assert!(granted_cant_attack_controllers(runner, ret).is_empty());
    assert!(!runner.state().objects[&ret]
        .static_definitions
        .iter_all()
        .any(|def| matches!(def.mode, StaticMode::BlockRestriction { .. })));

    let (window, valid) = blockers_window(runner, None, board.c0, P1);
    assert!(valid.contains(&ret), "{valid:?}");
    assert_eq!(block(&window, ret, board.c0), Ok(()));

    next_turn(runner, P1);
    let valid = attack_prompt(runner);
    assert!(valid.contains(&ret), "{valid:?}");
    assert_eq!(declare(runner, &[]), Ok(()));
    assert_eq!(declare(runner, &[(ret, at(P0))]), Ok(()));
}

/// CR 611.2b: the duration lasts while any duty counter remains.
#[test]
fn removing_one_of_two_duty_counters_keeps_everything() {
    let mut board = board(false, 0);
    let ret = cast(&mut board);
    let runner = &mut board.runner;
    set_duty(runner, ret, Some(2));
    set_duty(runner, ret, Some(1));
    next_turn(runner, P1);
    assert!(is_goaded(runner, ret));
    let valid = attack_prompt(runner);
    assert!(valid.contains(&ret), "{valid:?}");
    assert_refused(declare(runner, &[(ret, at(P0))]), ATTACK_RESTRICTION);
}

/// Promise of Loyalty ruling: the restrictions still name the caster after
/// another player gains control (CR 109.5 + CR 608.2c), and the goader is
/// still the caster.
#[test]
fn restrictions_still_name_the_caster_after_control_change() {
    let mut board = board(false, 0);
    let ret = cast(&mut board);
    let runner = &mut board.runner;
    {
        let object = runner.state_mut().objects.get_mut(&ret).unwrap();
        object.base_controller = Some(P2);
        object.controller = P2;
    }
    refresh(runner);
    assert_eq!(runner.state().objects[&ret].controller, P2);
    assert!(runner.state().objects[&ret].summoning_sick);
    assert!(is_goaded(runner, ret));
    assert_eq!(goad_tce_controllers(runner, ret), vec![P0]);

    let (window, valid) = blockers_window(runner, None, board.c0, P2);
    assert!(!valid.contains(&ret), "{valid:?}");
    assert_refused(block(&window, ret, board.c0), BLOCK_RESTRICTION);
    assert_eq!(block(&window, board.c2, board.c0), Ok(()));

    let (window, valid) = blockers_window(runner, Some(P1), board.p1c, P2);
    assert!(valid.contains(&ret), "{valid:?}");
    assert_eq!(block(&window, ret, board.p1c), Ok(()));

    next_turn(runner, P1);
    next_turn(runner, P2);
    assert!(!runner.state().objects[&ret].summoning_sick);
    let valid = attack_prompt(runner);
    assert!(valid.contains(&ret), "CR 302.6 reach-guard: {valid:?}");
    assert_refused(declare(runner, &[]), ATTACK_REQUIREMENT);
    assert_refused(declare(runner, &[(ret, at(P0))]), ATTACK_RESTRICTION);
    assert_eq!(declare(runner, &[(ret, at(P1))]), Ok(()));
    assert_eq!(goad_tce_controllers(runner, ret), vec![P0]);
}

/// CR 400.7 + CR 608.2c: the effect names the returned creature only, so a
/// duty counter put on another creature later binds nothing.
#[test]
fn a_later_duty_counter_binds_nothing() {
    let mut board = board(false, 0);
    let ret = cast(&mut board);
    let p1c = board.p1c;
    let runner = &mut board.runner;
    set_duty(runner, p1c, Some(1));
    assert!(!is_goaded(runner, p1c));
    assert!(is_goaded(runner, ret));

    let (window, _) = blockers_window(runner, None, board.c0, P1);
    assert_eq!(block(&window, p1c, board.c0), Ok(()));
    assert_refused(block(&window, ret, board.c0), BLOCK_RESTRICTION);

    next_turn(runner, P1);
    attack_prompt(runner);
    assert_eq!(declare(runner, &[(ret, at(P2)), (p1c, at(P0))]), Ok(()));
    assert_refused(declare(runner, &[(ret, at(P0))]), ATTACK_RESTRICTION);
}

/// Promise of Loyalty ruling ("If the vow counter is moved to another creature,
/// it won't prevent the second creature from attacking normally."), driven by
/// Fate Transfer: the moved counter frees the returned creature and binds
/// nothing, and moving it back does not resume the effect (CR 611.2b).
#[test]
fn a_moved_duty_counter_binds_nothing_and_does_not_resume() {
    let mut board = board(false, 2);
    let ret = cast(&mut board);
    let (p1c, c0) = (board.p1c, board.c0);
    let (there, back) = (board.fate_transfers[0], board.fate_transfers[1]);
    let runner = &mut board.runner;
    assert!(
        is_goaded(runner, ret),
        "reach-guard: goaded after resolution"
    );

    runner.cast(there).target_objects(&[ret, p1c]).resolve();
    assert_settled(runner);
    assert_eq!(duty_count(runner, ret), 0);
    assert_eq!(duty_count(runner, p1c), 1);
    assert!(!is_goaded(runner, ret));
    assert!(!is_goaded(runner, p1c));
    assert!(effects_on(runner, ret).is_empty());

    let (window, _) = blockers_window(runner, None, c0, P1);
    assert_eq!(block(&window, ret, c0), Ok(()));
    assert_eq!(block(&window, p1c, c0), Ok(()));

    runner.cast(back).target_objects(&[p1c, ret]).resolve();
    assert_settled(runner);
    assert_eq!(duty_count(runner, ret), 1, "the counter is back on it");
    assert!(
        !is_goaded(runner, ret),
        "CR 611.2b: the ended effect stays ended"
    );
    assert!(effects_on(runner, ret).is_empty());

    next_turn(runner, P1);
    let valid = attack_prompt(runner);
    assert!(valid.contains(&ret), "{valid:?}");
    assert_eq!(declare(runner, &[]), Ok(()));
    assert_eq!(declare(runner, &[(ret, at(P0)), (p1c, at(P0))]), Ok(()));
}

/// CR 608.2b: with its only target gone, the spell does nothing.
#[test]
fn does_nothing_when_the_target_left_the_graveyard() {
    let mut board = board(false, 0);
    let card = board.card;
    let mut commit = board.runner.cast(board.spell).target_object(card).commit();
    let mut events = Vec::new();
    move_to_zone(commit.state_mut(), card, Zone::Exile, &mut events);
    commit.resolve();
    let runner = &board.runner;
    assert_settled(runner);
    assert!(runner.state().transient_continuous_effects.is_empty());
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    assert!(!runner.state().battlefield.contains(&card));
}
