//! CR 115.6 + CR 601.2c + CR 701.14a: "up to one" on a fight frees exactly the
//! operand it is printed on. The printed operand decides which fighter is
//! optional; the other declared fighter stays required.
//!
//! - Two declared slots, fought creature optional: "target creature you control
//!   fights up to one target creature you don't control" (Favored Enemy, Smell
//!   Fear, Arni Slays the Troll I, Amok Time I).
//! - Two declared slots, fighter optional: "up to one target creature you
//!   control fights target creature …" (Gut, Fanatical Priestess; Earth Rumble).
//!   The fought creature can never be skipped, and an omitted fighter keeps its
//!   identity through announcement, validation and resolution (CR 608.2b).
//! - One declared slot, "it fights up to one target creature" after a targeted
//!   pump (Bridgeworks Battle, Primal Might): the fight may be declined.
//! - Restore: a pre-`up_to_one` save is migrated when its fight declares one
//!   slot; a fight that declares two keeps every slot required, as the older
//!   engine did. A restore is never refused.
//!
//! Review findings on https://github.com/phase-rs/phase/pull/8820 and
//! https://github.com/phase-rs/phase/pull/10714.

use std::collections::HashSet;
use std::path::Path;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    Effect, EffectKind, FightSlotRole, MultiTargetSpec, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, PersistedGameState, PersistedRestoreFinalization,
    TargetSelectionSlot, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Favored Enemy's enters trigger (its second line, verbatim). The first line's
/// "note the most prevalent creature type" is not needed here.
const FAVORED_ENEMY_ENTERS: &str = "When this enchantment enters, target creature you control fights up to one target creature you don't control.";

/// Bridgeworks Battle, front face (verbatim, reminder text dropped).
const BRIDGEWORKS_BATTLE: &str = "Target creature you control gets +2/+2 until end of turn. It fights up to one target creature you don't control.";

struct Board {
    runner: GameRunner,
    card: ObjectId,
    fighter: Option<ObjectId>,
    opponent: ObjectId,
}

/// A 3/3 for you (unless `with_fighter` is false), a 2/2 for the opponent, and
/// the card under test in your hand.
fn board(with_fighter: bool, name: &str, oracle: &str, enchantment: bool) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fighter = with_fighter.then(|| scenario.add_creature(P0, "Champion", 3, 3).id());
    let opponent = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let card = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(P0, name, false, oracle);
        if enchantment {
            builder.as_enchantment();
        }
        builder.id()
    };
    Board {
        runner: scenario.build(),
        card,
        fighter,
        opponent,
    }
}

fn favored_enemy(with_fighter: bool) -> Board {
    board(with_fighter, "Favored Enemy", FAVORED_ENEMY_ENTERS, true)
}

/// Cast the card under test and stop at its trigger's target prompt, returning
/// the slots. `None` when the stack empties without one surfacing.
fn cast_to_trigger_targets(board: &mut Board) -> Option<Vec<TargetSelectionSlot>> {
    let _commit = board.runner.cast(board.card).commit();
    pass_to_trigger_targets(&mut board.runner).0
}

/// Pass priority until a trigger's target prompt surfaces (returning its
/// slots) or the stack empties (`None`), collecting every event.
fn pass_to_trigger_targets(
    runner: &mut GameRunner,
) -> (Option<Vec<TargetSelectionSlot>>, Vec<GameEvent>) {
    let mut events = Vec::new();
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                return (Some(target_slots), events)
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return (None, events)
            }
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the trigger");
                events.extend(result.events);
            }
            other => panic!("unexpected wait while driving toward the trigger: {other:?}"),
        }
    }
    panic!("never reached a trigger target prompt or an empty stack");
}

fn choose(board: &mut Board, target: Option<ObjectId>) {
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: target.map(TargetRef::Object),
        })
        .expect("target choice accepted");
}

/// Pass priority until the stack is empty, returning every event.
fn resolve_stack(board: &mut Board) -> Vec<GameEvent> {
    drain_stack(&mut board.runner)
}

fn drain_stack(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..80 {
        if runner.state().stack.is_empty() {
            return events;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("pass to resolve the stack");
        events.extend(result.events);
    }
    panic!("stack did not resolve");
}

/// How many fights resolved (CR 701.14a), whether or not anyone fought.
fn fights_resolved(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Fight,
                    ..
                }
            )
        })
        .count()
}

/// The completion boundary: an empty stack and a priority window.
fn assert_completed(runner: &GameRunner) {
    let state = runner.state();
    assert!(state.stack.is_empty(), "stack: {:?}", state.stack);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "waiting for: {:?}",
        state.waiting_for
    );
}

/// CR 601.2c: one slot per "target", announced in order. The fighter slot is
/// required; only the "up to one" slot may be declined.
#[test]
fn two_slot_fighter_required_fought_optional() {
    let mut board = favored_enemy(true);
    let slots = cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    assert_eq!(slots.len(), 2, "fighter + fought creature: {slots:?}");
    assert!(
        !slots[0].optional,
        "CR 115.6: 'up to one' must not make the fighter optional"
    );
    assert!(slots[1].optional, "the 'up to one' creature is optional");
    let fighter = board.fighter.unwrap();
    assert!(slots[0].legal_targets.contains(&TargetRef::Object(fighter)));
}

/// CR 701.14a: the normal selected pair fights.
#[test]
fn two_slot_selected_pair_fights() {
    let mut board = favored_enemy(true);
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, Some(fighter));
    choose(&mut board, Some(opponent));
    resolve_stack(&mut board);

    let state = board.runner.state();
    assert_eq!(
        state.objects[&opponent].zone,
        Zone::Graveyard,
        "3/3 deals 3 to the 2/2"
    );
    assert_eq!(state.objects[&fighter].damage_marked, 2, "2/2 deals 2 back");
}

/// CR 115.6 + CR 701.14a + CR 609.3: a chosen fighter with the opponent declined resolves
/// as no fight — the lone fighter is not reread as "~ fights".
#[test]
fn two_slot_declined_opponent_is_no_fight() {
    let mut board = favored_enemy(true);
    let fighter = board.fighter.unwrap();
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, Some(fighter));
    choose(&mut board, None);
    let events = resolve_stack(&mut board);

    assert_eq!(
        fights_resolved(&events),
        1,
        "the ability resolved rather than fizzled: {events:?}"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&fighter].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].zone, Zone::Battlefield);
}

/// CR 115.6: declining the required fighter slot is refused.
#[test]
fn two_slot_declining_the_fighter_is_rejected() {
    let mut board = favored_enemy(true);
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    let declined = board.runner.act(GameAction::ChooseTarget { target: None });
    assert!(
        declined.is_err(),
        "the fighter slot is required, declining it must fail: {declined:?}"
    );
}

/// CR 603.3d: with no creature to fight with, the required fighter slot has no
/// legal target, so the trigger is removed from the stack and nothing fights.
#[test]
fn two_slot_no_legal_fighter_removes_the_trigger() {
    let mut board = favored_enemy(false);
    let slots = cast_to_trigger_targets(&mut board);
    assert!(
        slots.is_none(),
        "no creature you control: the trigger must not prompt, got {slots:?}"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&board.card].zone, Zone::Battlefield);
    assert_eq!(state.objects[&board.opponent].damage_marked, 0);
}

/// CR 115.6: the single-slot form may decline the fight; the pump still
/// applies.
#[test]
fn one_slot_declined_fight_still_pumps() {
    let mut board = board(true, "Bridgeworks Battle", BRIDGEWORKS_BATTLE, false);
    let fighter = board.fighter.unwrap();
    let outcome = board
        .runner
        .cast(board.card)
        .target_objects(&[fighter])
        .resolve();

    let state = outcome.state();
    assert_eq!(state.objects[&fighter].power, Some(5), "+2/+2 applied");
    assert_eq!(state.objects[&fighter].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].damage_marked, 0);
    assert_eq!(state.objects[&board.opponent].zone, Zone::Battlefield);
}

/// CR 701.14a: the single-slot form with a chosen opponent fights with the
/// pumped creature.
#[test]
fn one_slot_selected_fight() {
    let mut board = board(true, "Bridgeworks Battle", BRIDGEWORKS_BATTLE, false);
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    let outcome = board
        .runner
        .cast(board.card)
        .target_objects(&[fighter, opponent])
        .resolve();

    let state = outcome.state();
    assert_eq!(
        state.objects[&opponent].zone,
        Zone::Graveyard,
        "5/5 deals 5"
    );
    assert_eq!(state.objects[&fighter].damage_marked, 2, "2/2 deals 2 back");
}

/// Boxing Ring (verbatim, both lines). Its mana value is 2.
const BOXING_RING: &str = "Whenever a creature you control enters, it fights up to one target creature you don't control with the same mana value.\n{T}: Create a Treasure token. Activate only if you control a creature that fought this turn.";

/// A Boxing Ring board. `board.card` is the entering 3/3 in your hand (also
/// `board.fighter`), and `board.opponent` is the opponent's mana-value-3 2/2.
struct BoxingRingBoard {
    board: Board,
    opponent_mv2: ObjectId,
    opponent_mv4: ObjectId,
}

/// Boxing Ring (mana value 2) on your battlefield; the opponent controls 2/2s
/// with mana values 2, 3 and 4; a 3/3 with mana value `entering_mana_value` is
/// in your hand with enough Forests to cast it.
fn boxing_ring_board(entering_mana_value: u32) -> BoxingRingBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Boxing Ring", BOXING_RING)
        .with_mana_cost(ManaCost::generic(2));
    let opponent_mv2 = scenario
        .add_creature(P1, "Opponent Two", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let opponent_mv3 = scenario
        .add_creature(P1, "Opponent Three", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let opponent_mv4 = scenario
        .add_creature(P1, "Opponent Four", 2, 2)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    for _ in 0..entering_mana_value {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let entering = scenario
        .add_creature_to_hand(P0, "Entering Brawler", 3, 3)
        .with_mana_cost(ManaCost::generic(entering_mana_value))
        .id();
    BoxingRingBoard {
        board: Board {
            runner: scenario.build(),
            card: entering,
            fighter: Some(entering),
            opponent: opponent_mv3,
        },
        opponent_mv2,
        opponent_mv4,
    }
}

/// CR 202.3 + CR 608.2k: "with the same mana value" names no comparand, so it
/// compares with the object the trigger condition named — the entering
/// creature "it". Only the opponent creature whose mana value equals the
/// entering creature's (3) may be targeted; the one matching Boxing Ring's own
/// mana value (2) and the mana-value-4 one may not. CR 701.14a: the chosen
/// pair then fights.
#[test]
fn boxing_ring_offers_only_the_same_mana_value_creature() {
    let mut ring = boxing_ring_board(3);
    let entering = ring.board.card;
    let opponent_mv3 = ring.board.opponent;
    let slots = cast_to_trigger_targets(&mut ring.board).expect("trigger asks for a target");
    assert_eq!(
        slots.len(),
        1,
        "\"it\" is the entering creature, not a slot"
    );
    assert_eq!(
        slots[0].legal_targets,
        vec![TargetRef::Object(opponent_mv3)],
        "only the opponent creature with the entering creature's mana value is legal"
    );

    choose(&mut ring.board, Some(opponent_mv3));
    resolve_stack(&mut ring.board);

    let state = ring.board.runner.state();
    assert_eq!(state.objects[&opponent_mv3].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&entering].damage_marked, 2,
        "2/2 deals 2 back"
    );
    for untouched in [ring.opponent_mv2, ring.opponent_mv4] {
        assert_eq!(state.objects[&untouched].zone, Zone::Battlefield);
        assert_eq!(state.objects[&untouched].damage_marked, 0);
    }
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// Cast the Boxing Ring board's entering creature and drive toward its enters
/// trigger, collecting every event. Returns the trigger's target slots if a
/// prompt surfaces, `None` once the stack empties without one.
fn cast_boxing_ring_entrant(
    board: &mut Board,
) -> (Option<Vec<TargetSelectionSlot>>, Vec<GameEvent>) {
    let mut events = board.runner.cast(board.card).commit().events().to_vec();
    for _ in 0..80 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                return (Some(target_slots), events)
            }
            WaitingFor::Priority { .. } if board.runner.state().stack.is_empty() => {
                return (None, events)
            }
            WaitingFor::Priority { .. } => {
                let result = board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the enters trigger");
                events.extend(result.events);
            }
            other => panic!("unexpected wait while casting the entering creature: {other:?}"),
        }
    }
    panic!("the stack never emptied");
}

/// CR 115.6 + CR 701.14a: with no opponent creature of the entering creature's
/// mana value (5), the "up to one" target has no legal choice, so the trigger
/// is not prompted; it resolves with zero targets and nobody fights.
#[test]
fn boxing_ring_without_a_same_mana_value_creature_fights_nothing() {
    let mut ring = boxing_ring_board(5);
    let entering = ring.board.card;
    let (slots, events) = cast_boxing_ring_entrant(&mut ring.board);
    assert!(
        slots.is_none(),
        "no opponent creature has mana value 5, so there is nothing to choose: {slots:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Fight,
                    ..
                }
            ))
            .count(),
        1,
        "the trigger resolves once with zero targets: {events:?}"
    );

    let state = ring.board.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.objects[&entering].zone, Zone::Battlefield);
    assert_eq!(state.objects[&entering].damage_marked, 0);
    for untouched in [ring.opponent_mv2, ring.board.opponent, ring.opponent_mv4] {
        assert_eq!(state.objects[&untouched].zone, Zone::Battlefield);
        assert_eq!(state.objects[&untouched].damage_marked, 0);
    }
}

/// Exalted Angel (verbatim, all three lines, reminder text dropped). Its mana
/// value is 6; cast face down it is a 2/2 with mana value 0.
const EXALTED_ANGEL: &str =
    "Flying\nWhenever this creature deals damage, you gain that much life.\nMorph {2}{W}{W}";

/// Elesh Norn, Grand Cenobite (verbatim, all three lines).
const ELESH_NORN: &str = "Vigilance\nOther creatures you control get +2/+2.\nCreatures your opponents control get -2/-2.";

/// A Boxing Ring board whose entrant dies before its trigger is announced.
struct FaceDownEntrantBoard {
    runner: GameRunner,
    angel: ObjectId,
    elesh_norn: ObjectId,
    opponent_zero: ObjectId,
    opponent_six: ObjectId,
}

/// Boxing Ring and three Forests for you, with Exalted Angel in hand: the
/// Forests pay {3} but not {4}{W}{W}, so the cast goes face down. The opponent
/// controls Elesh Norn, Grand Cenobite (whose -2/-2 kills a face-down 2/2) and
/// two 2/2s with mana values 0 and 6.
fn face_down_entrant_board() -> FaceDownEntrantBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Boxing Ring", BOXING_RING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let angel = scenario
        .add_creature_to_hand(P0, "Exalted Angel", 4, 5)
        .from_oracle_text_with_keywords(&["Flying", "Morph"], EXALTED_ANGEL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 4,
        })
        .id();
    let elesh_norn = scenario
        .add_creature_from_oracle(P1, "Elesh Norn, Grand Cenobite", 4, 7, ELESH_NORN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 5,
        })
        .id();
    let opponent_zero = scenario
        .add_creature(P1, "Opponent Zero", 2, 2)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let opponent_six = scenario
        .add_creature(P1, "Opponent Six", 2, 2)
        .with_mana_cost(ManaCost::generic(6))
        .id();
    FaceDownEntrantBoard {
        runner: scenario.build(),
        angel,
        elesh_norn,
        opponent_zero,
        opponent_six,
    }
}

/// CR 400.7 + CR 608.2h + CR 708.2a: the entering face-down creature (mana value
/// 0) dies to Elesh Norn before the trigger's target is announced (CR 704.5f,
/// CR 117.5); "the same mana value" compares with its last known information,
/// not the revealed card in the graveyard. CR 701.14b: the departed fighter
/// fights nobody, and the chosen target stays legal at resolution (CR 608.2b).
#[test]
fn boxing_ring_compares_with_a_dead_face_down_entrants_last_known_mana_value() {
    let mut board = face_down_entrant_board();
    let angel = board.angel;
    let card_id = board.runner.state().objects[&angel].card_id;
    let mut events = board
        .runner
        .act(GameAction::CastSpell {
            object_id: angel,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Exalted Angel is cast")
        .events;
    {
        let cast = &board.runner.state().objects[&angel];
        assert!(
            cast.face_down && cast.zone == Zone::Stack,
            "reach guard: only {{3}} is payable, so the Angel is cast face down: face_down={}, zone={:?}",
            cast.face_down,
            cast.zone
        );
    }

    let (slots, more) = pass_to_trigger_targets(&mut board.runner);
    events.extend(more);
    let slots = slots.expect("Boxing Ring's trigger asks for a target");
    {
        let dead = &board.runner.state().objects[&angel];
        assert_eq!(
            dead.zone,
            Zone::Graveyard,
            "reach guard: the face-down 2/2 died to Elesh Norn before announcement"
        );
        assert!(
            !dead.face_down,
            "reach guard: revealed on leaving (CR 708.9)"
        );
        assert_eq!(
            dead.effective_mana_value(),
            6,
            "reach guard: the revealed card in the graveyard has mana value 6"
        );
    }
    assert_eq!(
        slots.len(),
        1,
        "\"it\" is the entrant, not a slot: {slots:?}"
    );
    assert_eq!(
        slots[0].legal_targets,
        vec![TargetRef::Object(board.opponent_zero)],
        "only the opponent creature with the entrant's last known mana value (0) is legal"
    );
    assert!(
        !slots[0]
            .legal_targets
            .contains(&TargetRef::Object(board.elesh_norn)),
        "Elesh Norn (mana value 7) is never offered"
    );

    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.opponent_zero)),
        })
        .expect("the mana-value-0 creature is chosen");
    events.extend(drain_stack(&mut board.runner));

    assert_eq!(
        fights_resolved(&events),
        1,
        "the chosen target stays legal at resolution, so the trigger resolves: {events:?}"
    );
    let state = board.runner.state();
    for untouched in [board.opponent_zero, board.opponent_six] {
        assert_eq!(state.objects[&untouched].zone, Zone::Battlefield);
        assert_eq!(state.objects[&untouched].damage_marked, 0);
    }
    assert_completed(&board.runner);
}

/// Gut, Fanatical Priestess's enters trigger (its second Oracle line,
/// verbatim). Its first line, "Specialize {5}", is a separate ability not under
/// test.
const GUT_ENTERS: &str = "When Gut, Fanatical Priestess enters, up to one target creature you control fights target creature you don't control. If the creature you don't control would die this turn, exile it instead.";

/// Earth Rumble (verbatim, reminder text dropped).
const EARTH_RUMBLE: &str =
    "Earthbend 2. When you do, up to one target creature you control fights target creature an opponent controls.";

/// Ray of Command (verbatim).
const RAY_OF_COMMAND: &str = "Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.";

/// Smell Fear (verbatim, both lines, reminder text dropped).
const SMELL_FEAR: &str =
    "Proliferate.\nTarget creature you control fights up to one target creature you don't control.";

/// A 3/3 "Champion" for you, a 2/4 for the opponent, which survives the
/// Champion's 3 damage so no assertion depends on Gut's unparsed exile-instead
/// rider (#10744), and Gut (4/3) in your hand. Returns the unbuilt scenario so
/// a row can add more cards, plus the Champion, the opponent's creature and
/// Gut.
fn gut_scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let champion = scenario.add_creature(P0, "Champion", 3, 3).id();
    let opponent = scenario.add_creature(P1, "Opponent Bear", 2, 4).id();
    let gut = scenario
        .add_creature_to_hand_from_oracle(P0, "Gut, Fanatical Priestess", 4, 3, GUT_ENTERS)
        .id();
    (scenario, champion, opponent, gut)
}

fn gut_board() -> Board {
    let (scenario, champion, opponent, gut) = gut_scenario();
    Board {
        runner: scenario.build(),
        card: gut,
        fighter: Some(champion),
        opponent,
    }
}

/// CR 115.6 + CR 601.2c: Gut's printed "up to one" is on the fighter, so the
/// fighter slot is optional and the fought creature's slot is required.
#[test]
fn gut_fighter_optional_fought_required() {
    let mut board = gut_board();
    let slots = cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    assert_eq!(slots.len(), 2, "fighter + fought creature: {slots:?}");
    assert!(slots[0].optional, "the 'up to one' fighter is optional");
    assert!(
        !slots[1].optional,
        "CR 601.2c: the fought creature is required"
    );
    for fighter in [board.fighter.unwrap(), board.card] {
        assert!(slots[0].legal_targets.contains(&TargetRef::Object(fighter)));
    }
    assert_eq!(
        slots[1].legal_targets,
        vec![TargetRef::Object(board.opponent)]
    );
}

/// CR 601.2c: after choosing a fighter, the required fought creature cannot
/// be skipped.
#[test]
fn gut_declining_the_fought_creature_is_rejected() {
    let mut board = gut_board();
    let fighter = board.fighter;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, fighter);
    let before = board.runner.state().waiting_for.clone();
    let declined = board.runner.act(GameAction::ChooseTarget { target: None });
    assert!(
        declined.is_err(),
        "the fought creature is required, declining it must fail: {declined:?}"
    );
    assert_eq!(board.runner.state().waiting_for, before, "prompt unchanged");
}

/// CR 115.6 + CR 608.2b: a declined fighter keeps its identity, so the chosen
/// fought creature is validated against its own role ("creature you don't
/// control") and the ability resolves. CR 701.14a + CR 609.3: a two-slot fight with only
/// one creature does not fight.
#[test]
fn gut_declined_fighter_resolves_without_a_fight() {
    let mut board = gut_board();
    let opponent = board.opponent;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, None);
    choose(&mut board, Some(opponent));
    let events = resolve_stack(&mut board);

    assert_eq!(
        fights_resolved(&events),
        1,
        "the trigger resolved rather than fizzled: {events:?}"
    );
    let state = board.runner.state();
    for creature in [opponent, board.fighter.unwrap(), board.card] {
        assert_eq!(state.objects[&creature].zone, Zone::Battlefield);
        assert_eq!(state.objects[&creature].damage_marked, 0);
    }
    assert_completed(&board.runner);
}

/// CR 701.14a: Gut's chosen pair fights; each deals damage equal to its power to
/// the other.
#[test]
fn gut_selected_pair_fights() {
    let mut board = gut_board();
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, Some(fighter));
    choose(&mut board, Some(opponent));
    resolve_stack(&mut board);

    let state = board.runner.state();
    assert_eq!(state.objects[&opponent].zone, Zone::Battlefield);
    assert_eq!(state.objects[&opponent].damage_marked, 3, "3/3 deals 3");
    assert_eq!(state.objects[&fighter].damage_marked, 2, "2/4 deals 2 back");
    assert_completed(&board.runner);
}

/// CR 608.2b: the fought creature changes controller before resolution. Its
/// only stored occurrence is checked against the role it was announced for,
/// "creature you don't control", which it now fails, so the ability does not
/// resolve. A positional reading would accept it as "creature you control".
#[test]
fn gut_fought_creature_stolen_before_resolution_stops_the_ability() {
    let (mut scenario, champion, opponent, gut) = gut_scenario();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P0, "Ray of Command", true, RAY_OF_COMMAND)
        .id();
    let mut board = Board {
        runner: scenario.build(),
        card: gut,
        fighter: Some(champion),
        opponent,
    };
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, None);
    choose(&mut board, Some(opponent));

    let mut events = board
        .runner
        .cast(ray)
        .target_object(opponent)
        .commit()
        .events()
        .to_vec();
    // Resolve Ray of Command (top of the stack); Gut's trigger stays below.
    while board.runner.state().stack.len() > 1 {
        let result = board
            .runner
            .act(GameAction::PassPriority)
            .expect("pass to resolve Ray of Command");
        events.extend(result.events);
    }
    assert_eq!(
        board.runner.state().objects[&opponent].controller,
        P0,
        "precondition: Ray of Command took the fought creature"
    );
    events.extend(resolve_stack(&mut board));

    assert_eq!(
        fights_resolved(&events),
        0,
        "the only target fails its own role's filter: {events:?}"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&opponent].damage_marked, 0);
    assert_eq!(state.objects[&champion].damage_marked, 0);
    assert_completed(&board.runner);
}

/// Earth Rumble in your hand, a Forest to earthbend and a 3/3 "Champion"; the
/// opponent's 2/2 is present only when `with_opponent`.
struct EarthRumbleBoard {
    runner: GameRunner,
    card: ObjectId,
    champion: ObjectId,
    forest: ObjectId,
    opponent: Option<ObjectId>,
}

fn earth_rumble_board(with_opponent: bool) -> EarthRumbleBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let champion = scenario.add_creature(P0, "Champion", 3, 3).id();
    let opponent = with_opponent.then(|| scenario.add_creature(P1, "Opponent Bear", 2, 2).id());
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Earth Rumble", false, EARTH_RUMBLE)
        .id();
    EarthRumbleBoard {
        runner: scenario.build(),
        card,
        champion,
        forest,
        opponent,
    }
}

impl EarthRumbleBoard {
    /// Cast Earth Rumble earthbending the Forest, then drive to the reflexive
    /// "when you do" prompt (its slots) or an empty stack (`None`), collecting
    /// every event.
    fn cast(&mut self) -> (Option<Vec<TargetSelectionSlot>>, Vec<GameEvent>) {
        let mut events = self
            .runner
            .cast(self.card)
            .target_object(self.forest)
            .commit()
            .events()
            .to_vec();
        let (slots, more) = pass_to_trigger_targets(&mut self.runner);
        events.extend(more);
        (slots, events)
    }

    fn choose(&mut self, target: Option<ObjectId>) -> Result<(), String> {
        self.runner
            .act(GameAction::ChooseTarget {
                target: target.map(TargetRef::Object),
            })
            .map(drop)
            .map_err(|error| format!("{error:?}"))
    }

    /// Reach guard: the earthbent Forest is a creature with two +1/+1 counters.
    fn assert_forest_earthbent(&self) {
        let forest = &self.runner.state().objects[&self.forest];
        assert!(
            forest.card_types.core_types.contains(&CoreType::Creature),
            "earthbend animated the Forest"
        );
        assert_eq!(forest.counters.get(&CounterType::Plus1Plus1), Some(&2));
    }
}

/// CR 115.6 + CR 601.2c: Earth Rumble's reflexive fight frees the fighter and
/// requires the fought creature.
#[test]
fn earth_rumble_fighter_optional_fought_required() {
    let mut rumble = earth_rumble_board(true);
    let (slots, _) = rumble.cast();
    let slots = slots.expect("the reflexive trigger asks for targets");
    rumble.assert_forest_earthbent();
    assert_eq!(slots.len(), 2, "fighter + fought creature: {slots:?}");
    assert!(slots[0].optional, "the 'up to one' fighter is optional");
    assert!(!slots[1].optional, "the fought creature is required");

    rumble
        .choose(Some(rumble.champion))
        .expect("the fighter is chosen");
    let declined = rumble.choose(None);
    assert!(
        declined.is_err(),
        "the fought creature is required, declining it must fail: {declined:?}"
    );
}

/// CR 115.6 + CR 701.14a + CR 609.3: declining Earth Rumble's fighter resolves the
/// reflexive trigger without a fight.
#[test]
fn earth_rumble_declined_fighter_resolves_without_a_fight() {
    let mut rumble = earth_rumble_board(true);
    let opponent = rumble.opponent.unwrap();
    let (slots, _) = rumble.cast();
    slots.expect("the reflexive trigger asks for targets");
    rumble.choose(None).expect("the fighter is declined");
    rumble
        .choose(Some(opponent))
        .expect("the fought creature is chosen");
    let events = drain_stack(&mut rumble.runner);

    assert_eq!(fights_resolved(&events), 1, "{events:?}");
    let state = rumble.runner.state();
    assert_eq!(state.objects[&opponent].damage_marked, 0);
    assert_eq!(state.objects[&rumble.champion].damage_marked, 0);
    assert_completed(&rumble.runner);
}

/// CR 603.3d: with no creature an opponent controls, the required fought
/// creature has no legal choice, so the reflexive trigger is removed — through
/// the failed-slot route, although the optional fighter has legal choices.
#[test]
fn earth_rumble_without_an_opponent_creature_removes_the_reflexive_trigger() {
    let mut rumble = earth_rumble_board(false);
    let (slots, events) = rumble.cast();
    assert!(
        slots.is_none(),
        "the fought creature has no legal choice: the trigger must not prompt, got {slots:?}"
    );
    rumble.assert_forest_earthbent();
    assert_eq!(fights_resolved(&events), 0, "{events:?}");
    assert_eq!(
        rumble.runner.state().objects[&rumble.champion].damage_marked,
        0
    );
    assert_completed(&rumble.runner);
}

/// CR 603.3d: Gut with no creature you don't control. The required fought
/// creature has no legal choice, so the enters trigger is removed although the
/// optional fighter has legal choices (Gut, Champion).
#[test]
fn gut_without_a_creature_you_dont_control_removes_the_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let champion = scenario.add_creature(P0, "Champion", 3, 3).id();
    let gut = scenario
        .add_creature_to_hand_from_oracle(P0, "Gut, Fanatical Priestess", 4, 3, GUT_ENTERS)
        .id();
    let mut runner = scenario.build();
    let mut events = runner.cast(gut).commit().events().to_vec();
    let (slots, more) = pass_to_trigger_targets(&mut runner);
    events.extend(more);

    assert!(
        slots.is_none(),
        "no creature you don't control: the trigger must not prompt, got {slots:?}"
    );
    assert_eq!(fights_resolved(&events), 0, "{events:?}");
    let state = runner.state();
    assert_eq!(state.objects[&gut].zone, Zone::Battlefield, "Gut resolved");
    assert_eq!(state.objects[&gut].damage_marked, 0);
    assert_eq!(state.objects[&champion].damage_marked, 0);
    assert_completed(&runner);
}

/// Submit a bulk `SelectTargets` answer.
fn select_targets(board: &mut Board, targets: &[ObjectId]) -> Result<(), String> {
    board
        .runner
        .act(GameAction::SelectTargets {
            targets: targets.iter().copied().map(TargetRef::Object).collect(),
        })
        .map(drop)
        .map_err(|error| format!("{error:?}"))
}

/// CR 601.2c + CR 115.6: a compact bulk list never names a declined slot, and
/// a required role is never omitted. Gut's one-entry list therefore names the
/// required fought creature: `[bear]` is legal (fighter declined), `[Champion]`
/// is not (Champion is no creature you don't control).
#[test]
fn gut_bulk_selection_reads_a_one_entry_list_as_the_fought_creature() {
    let mut board = gut_board();
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");

    let before = board.runner.state().waiting_for.clone();
    let refused = select_targets(&mut board, &[fighter]);
    assert!(
        refused.is_err(),
        "Champion cannot be the fought creature: {refused:?}"
    );
    assert_eq!(board.runner.state().waiting_for, before, "prompt unchanged");

    select_targets(&mut board, &[opponent]).expect("fighter declined, bear fought");
    let stacked = board
        .runner
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .expect("Gut's trigger is on the stack")
        .targets
        .clone();
    assert_eq!(stacked, vec![TargetRef::Object(opponent)]);
    let events = resolve_stack(&mut board);
    assert_eq!(fights_resolved(&events), 1, "{events:?}");
    let state = board.runner.state();
    assert_eq!(state.objects[&opponent].damage_marked, 0);
    assert_eq!(state.objects[&fighter].damage_marked, 0);
    assert_completed(&board.runner);
}

/// CR 701.14a: Gut's full bulk pair fights.
#[test]
fn gut_bulk_selection_of_both_fights() {
    let mut board = gut_board();
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    select_targets(&mut board, &[fighter, opponent]).expect("both fighters chosen");
    resolve_stack(&mut board);

    let state = board.runner.state();
    assert_eq!(state.objects[&opponent].zone, Zone::Battlefield);
    assert_eq!(state.objects[&opponent].damage_marked, 3, "3/3 deals 3");
    assert_eq!(state.objects[&fighter].damage_marked, 2, "2/4 deals 2 back");
    assert_completed(&board.runner);
}

/// CR 601.2c + CR 115.6: Favored Enemy's one-entry bulk list names the
/// required fighter: `[Champion]` declines the fought creature, `[bear]` is
/// refused (the bear is no creature you control).
#[test]
fn favored_enemy_bulk_selection_reads_a_one_entry_list_as_the_fighter() {
    let mut board = favored_enemy(true);
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");

    let before = board.runner.state().waiting_for.clone();
    let refused = select_targets(&mut board, &[opponent]);
    assert!(
        refused.is_err(),
        "the bear cannot be the fighter: {refused:?}"
    );
    assert_eq!(board.runner.state().waiting_for, before, "prompt unchanged");

    select_targets(&mut board, &[fighter]).expect("fighter chosen, fought declined");
    let events = resolve_stack(&mut board);
    assert_eq!(
        fights_resolved(&events),
        1,
        "the ability resolved rather than fizzled: {events:?}"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&opponent].zone, Zone::Battlefield);
    assert_eq!(state.objects[&opponent].damage_marked, 0);
    assert_eq!(state.objects[&fighter].damage_marked, 0);
    assert_completed(&board.runner);
}

/// A 3/3 for you, a 2/2 for the opponent, and Smell Fear cast with no targets
/// announced up front, stopped at its target prompt.
fn smell_fear_at_target_prompt() -> Board {
    let mut board = board(true, "Smell Fear", SMELL_FEAR, false);
    let card_id = board.runner.state().objects[&board.card].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: board.card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Smell Fear is cast");
    let WaitingFor::TargetSelection { target_slots, .. } = &board.runner.state().waiting_for else {
        panic!(
            "Smell Fear asks for targets: {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 2, "fighter + fought creature");
    assert!(!target_slots[0].optional, "the fighter is required");
    assert!(target_slots[1].optional, "the fought creature is up to one");
    board
}

/// CR 601.2c + CR 115.6: the spell-side bulk handler reads Smell Fear's compact
/// list through the same slots: `[Champion]` names the required fighter and
/// declines the fought creature, so nobody fights (CR 701.14a); `[bear]` is
/// refused.
#[test]
fn smell_fear_bulk_selection_reads_a_one_entry_list_as_the_fighter() {
    let mut board = smell_fear_at_target_prompt();
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;

    let before = board.runner.state().waiting_for.clone();
    let refused = select_targets(&mut board, &[opponent]);
    assert!(
        refused.is_err(),
        "the bear cannot be the fighter: {refused:?}"
    );
    assert_eq!(board.runner.state().waiting_for, before, "prompt unchanged");

    select_targets(&mut board, &[fighter]).expect("fighter chosen, fought declined");
    let events = resolve_stack(&mut board);
    assert_eq!(
        fights_resolved(&events),
        1,
        "the ability resolved rather than fizzled: {events:?}"
    );
    let state = board.runner.state();
    assert_eq!(state.objects[&opponent].zone, Zone::Battlefield);
    assert_eq!(state.objects[&opponent].damage_marked, 0);
    assert_eq!(state.objects[&fighter].damage_marked, 0);
    assert_completed(&board.runner);
}

/// CR 701.14a: Smell Fear's full bulk pair fights.
#[test]
fn smell_fear_bulk_selection_of_both_fights() {
    let mut board = smell_fear_at_target_prompt();
    let fighter = board.fighter.unwrap();
    let opponent = board.opponent;
    select_targets(&mut board, &[fighter, opponent]).expect("both fighters chosen");
    resolve_stack(&mut board);

    let state = board.runner.state();
    assert_eq!(state.objects[&opponent].zone, Zone::Graveyard);
    assert_eq!(state.objects[&fighter].damage_marked, 2);
    assert_completed(&board.runner);
}

/// Bolt Bend (verbatim).
const BOLT_BEND: &str = "This spell costs {3} less to cast if you control a creature with power 4 or greater.\nChange the target of target spell or ability with a single target.";

/// Put a free Bolt Bend in your hand.
fn add_bolt_bend(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Bend", true, BOLT_BEND)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

/// Cast Bolt Bend at the ability on top of the stack and pass until its
/// retarget prompt opens, returning that prompt's per-position pools.
fn bolt_bend_the_top_ability(runner: &mut GameRunner, bolt_bend: ObjectId) -> Vec<Vec<TargetRef>> {
    let ability = runner
        .state()
        .stack
        .back()
        .expect("the trigger is on the stack")
        .id;
    let _commit = runner.cast(bolt_bend).target_object(ability).commit();
    for _ in 0..80 {
        match &runner.state().waiting_for {
            WaitingFor::RetargetChoice { slot_pools, .. } => return slot_pools.clone(),
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward Bolt Bend's resolution");
            }
            other => panic!("unexpected wait before the retarget prompt: {other:?}"),
        }
    }
    panic!("Bolt Bend never asked for a new target");
}

fn retarget_to(runner: &mut GameRunner, target: ObjectId) -> Result<(), String> {
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(target))],
        })
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

/// CR 115.7a + CR 601.2c + CR 115.6: with Favored Enemy's fought creature
/// declined, its trigger's single target is the fighter, so Bolt Bend ("Change
/// the target of target spell or ability with a single target.") may move it
/// only to another creature you control — never to a creature you don't
/// control, which only the declined slot could hold.
#[test]
fn bolt_bend_moves_favored_enemys_lone_fighter_only_to_a_creature_you_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fighter = scenario.add_creature(P0, "Champion", 3, 3).id();
    let ally = scenario.add_creature(P0, "Ally Bear", 2, 2).id();
    let opponent = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let card = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Favored Enemy",
            false,
            FAVORED_ENEMY_ENTERS,
        );
        builder.as_enchantment();
        builder.id()
    };
    let bolt_bend = add_bolt_bend(&mut scenario);
    let mut board = Board {
        runner: scenario.build(),
        card,
        fighter: Some(fighter),
        opponent,
    };
    cast_to_trigger_targets(&mut board).expect("trigger asks for targets");
    choose(&mut board, Some(fighter));
    choose(&mut board, None);

    let pools = bolt_bend_the_top_ability(&mut board.runner, bolt_bend);
    assert_eq!(pools.len(), 1, "one stored target, one position: {pools:?}");
    assert!(
        pools[0].contains(&TargetRef::Object(ally)),
        "another creature you control is a legal new fighter: {pools:?}"
    );
    assert!(
        !pools[0].contains(&TargetRef::Object(opponent)),
        "a creature you don't control is not a legal fighter: {pools:?}"
    );

    let before = board.runner.state().waiting_for.clone();
    assert!(
        retarget_to(&mut board.runner, opponent).is_err(),
        "the opponent's creature is refused as the fighter"
    );
    assert_eq!(board.runner.state().waiting_for, before, "prompt unchanged");
    retarget_to(&mut board.runner, ally).expect("the ally is a legal fighter");

    let events = resolve_stack(&mut board);
    assert_eq!(
        fights_resolved(&events),
        1,
        "the trigger resolved with the ally as its lone fighter: {events:?}"
    );
    let state = board.runner.state();
    for creature in [fighter, ally, opponent] {
        assert_eq!(state.objects[&creature].damage_marked, 0);
    }
    assert_completed(&board.runner);
}

/// CR 115.7a + CR 603.3d + CR 202.3: Boxing Ring's "with the same mana value"
/// compares with the creature whose entry triggered it, for the retarget too.
/// With a mana-value-3 entrant, Bolt Bend may move the fight only to the other
/// mana-value-3 creature — never to the mana-value-0 ones, which a read
/// without the entry's trigger event would compare with 0. CR 701.14a: the
/// new pair then fights.
#[test]
fn bolt_bend_keeps_boxing_rings_fight_at_the_entrants_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_artifact_from_oracle(P0, "Boxing Ring", BOXING_RING)
        .with_mana_cost(ManaCost::generic(2));
    let first_three = scenario
        .add_creature(P1, "Opponent Three", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let second_three = scenario
        .add_creature(P1, "Other Three", 2, 2)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let zeros = [
        scenario
            .add_creature(P1, "Opponent Zero", 2, 2)
            .with_mana_cost(ManaCost::generic(0))
            .id(),
        scenario
            .add_creature(P1, "Other Zero", 2, 2)
            .with_mana_cost(ManaCost::generic(0))
            .id(),
    ];
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let entering = scenario
        .add_creature_to_hand(P0, "Entering Brawler", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let bolt_bend = add_bolt_bend(&mut scenario);
    let mut board = Board {
        runner: scenario.build(),
        card: entering,
        fighter: Some(entering),
        opponent: first_three,
    };
    let slots = cast_boxing_ring_entrant(&mut board)
        .0
        .expect("trigger asks for a target");
    assert_eq!(
        slots[0].legal_targets.len(),
        2,
        "reach guard: both mana-value-3 creatures are offered at announcement: {slots:?}"
    );
    choose(&mut board, Some(first_three));

    let pools = bolt_bend_the_top_ability(&mut board.runner, bolt_bend);
    assert_eq!(pools.len(), 1, "one stored target, one position: {pools:?}");
    assert!(
        pools[0].contains(&TargetRef::Object(second_three)),
        "the other mana-value-3 creature is a legal new target: {pools:?}"
    );
    for zero in zeros {
        assert!(
            !pools[0].contains(&TargetRef::Object(zero)),
            "a mana-value-0 creature is not: {pools:?}"
        );
    }

    let before = board.runner.state().waiting_for.clone();
    assert!(
        retarget_to(&mut board.runner, zeros[0]).is_err(),
        "a mana-value-0 creature is refused"
    );
    assert_eq!(board.runner.state().waiting_for, before, "prompt unchanged");
    retarget_to(&mut board.runner, second_three).expect("the other mana-value-3 creature");

    let events = resolve_stack(&mut board);
    assert_eq!(fights_resolved(&events), 1, "{events:?}");
    let state = board.runner.state();
    assert_eq!(
        state.objects[&second_three].zone,
        Zone::Graveyard,
        "the 3/3 deals 3 to the new 2/2"
    );
    assert_eq!(
        state.objects[&entering].damage_marked, 2,
        "2/2 deals 2 back"
    );
    for untouched in [first_three, zeros[0], zeros[1]] {
        assert_eq!(state.objects[&untouched].zone, Zone::Battlefield);
        assert_eq!(state.objects[&untouched].damage_marked, 0);
    }
    assert_completed(&board.runner);
}

/// Kogla, the Titan Ape (verbatim, all three lines).
const KOGLA: &str = "When Kogla enters, it fights up to one target creature you don't control.\nWhenever Kogla attacks, destroy target artifact or enchantment defending player controls.\n{1}{G}: Return target Human you control to its owner's hand. Kogla gains indestructible until end of turn.";

/// Kogla (7/6) in your hand; the opponent controls a 2/2 "Opponent Bear" and a
/// 3/3 "Opponent Wolf". With two legal targets an engine that required every
/// fight slot would also have prompted rather than auto-assigned. Returns the
/// runner, Kogla, the bear and the wolf.
fn kogla_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let wolf = scenario.add_creature(P1, "Opponent Wolf", 3, 3).id();
    let kogla = scenario
        .add_creature_to_hand_from_oracle(P0, "Kogla, the Titan Ape", 7, 6, KOGLA)
        .id();
    (scenario.build(), kogla, bear, wolf)
}

/// `board(true, name, oracle, enchantment)` plus a second opponent creature, a
/// 2/2 "Opponent Wolf", so that with every fight slot required the Champion ×
/// {bear, wolf} pairs are two legal combinations and the trigger prompts.
fn two_opponent_board(name: &str, oracle: &str, enchantment: bool) -> (Board, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fighter = scenario.add_creature(P0, "Champion", 3, 3).id();
    let opponent = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let card = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(P0, name, false, oracle);
        if enchantment {
            builder.as_enchantment();
        }
        builder.id()
    };
    let wolf = scenario.add_creature(P1, "Opponent Wolf", 2, 2).id();
    (
        Board {
            runner: scenario.build(),
            card,
            fighter: Some(fighter),
            opponent,
        },
        wolf,
    )
}

/// Save the runner's state as the trusted persistence writer does.
fn save(runner: &GameRunner) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("the state serializes")
}

/// Restore a save through the production persistence decode.
fn restore(save: serde_json::Value) -> Result<GameState, String> {
    serde_json::from_value::<PersistedGameState>(save)
        .map_err(|error| error.to_string())?
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .map_err(|error| error.to_string())?
        .finalize_after_rehydration(|_| Ok(()))
        .map_err(|error| error.to_string())
}

/// The ability-level spec an older writer stored for a fight's printed "up to
/// one".
fn legacy_up_to_one_spec() -> serde_json::Value {
    serde_json::to_value(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }))
        .expect("the spec serializes")
}

/// What an engine before `Effect::Fight::up_to_one` wrote: the printed 'up to
/// one' as the ability-level `multi_target` `{min 0, max 1}` and every fight
/// slot of a paused prompt required.
fn as_pre_operand_writer(save: &mut serde_json::Value) -> usize {
    match save {
        serde_json::Value::Array(values) => values.iter_mut().map(as_pre_operand_writer).sum(),
        serde_json::Value::Object(object) => {
            let mut rewritten = 0;
            let is_operand_fight = object.get("effect").is_some_and(|effect| {
                effect.get("type").and_then(serde_json::Value::as_str) == Some("Fight")
                    && effect.get("up_to_one").is_some()
            });
            if is_operand_fight {
                object
                    .get_mut("effect")
                    .and_then(serde_json::Value::as_object_mut)
                    .expect("the fight effect is an object")
                    .remove("up_to_one");
                object.insert("multi_target".to_string(), legacy_up_to_one_spec());
                object.insert(
                    "optional_targeting".to_string(),
                    serde_json::Value::Bool(false),
                );
                rewritten += 1;
            }
            let is_prompt = matches!(
                object.get("type").and_then(serde_json::Value::as_str),
                Some("TriggerTargetSelection" | "TargetSelection")
            );
            if is_prompt {
                let slots = object
                    .get_mut("data")
                    .and_then(|data| data.get_mut("target_slots"))
                    .and_then(serde_json::Value::as_array_mut);
                for slot in slots.into_iter().flatten() {
                    if slot.get("effect_kind").and_then(serde_json::Value::as_str) == Some("Fight")
                    {
                        slot["optional"] = serde_json::Value::Bool(false);
                    }
                }
            }
            rewritten
                + object
                    .values_mut()
                    .map(as_pre_operand_writer)
                    .sum::<usize>()
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => 0,
    }
}

/// Test-side twin of the restore migration's legacy-node predicate: a fight
/// with no `up_to_one` whose ability carries the up-to-one spec or
/// `optional_targeting`.
fn is_legacy_fight_node(node: &serde_json::Map<String, serde_json::Value>) -> bool {
    let Some(effect) = node.get("effect") else {
        return false;
    };
    effect.get("type").and_then(serde_json::Value::as_str) == Some("Fight")
        && effect
            .get("up_to_one")
            .is_none_or(serde_json::Value::is_null)
        && (node
            .get("multi_target")
            .and_then(|spec| serde_json::from_value::<MultiTargetSpec>(spec.clone()).ok())
            .is_some_and(|spec| spec.is_up_to_one())
            || node.get("optional_targeting") == Some(&serde_json::Value::Bool(true)))
}

fn legacy_fight_count(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => values.iter().map(legacy_fight_count).sum(),
        serde_json::Value::Object(object) => {
            usize::from(is_legacy_fight_node(object))
                + object.values().map(legacy_fight_count).sum::<usize>()
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => 0,
    }
}

/// Every `Effect::Fight` payload in `value`, as JSON text, sorted.
fn fight_effects(value: &serde_json::Value) -> Vec<String> {
    fn collect(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Array(values) => values.iter().for_each(|value| collect(value, out)),
            serde_json::Value::Object(object) => {
                if object.get("type").and_then(serde_json::Value::as_str) == Some("Fight") {
                    out.push(value.to_string());
                }
                object.values().for_each(|value| collect(value, out));
            }
            serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_) => {}
        }
    }
    let mut out = Vec::new();
    collect(value, &mut out);
    out.sort();
    out
}

/// The paused target prompt's slots.
fn prompt_slots(state: &GameState) -> Vec<TargetSelectionSlot> {
    match &state.waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. }
        | WaitingFor::TargetSelection { target_slots, .. } => target_slots.clone(),
        other => panic!("expected a target prompt, got {other:?}"),
    }
}

fn optionals(state: &GameState) -> Vec<bool> {
    prompt_slots(state)
        .iter()
        .map(|slot| slot.optional)
        .collect()
}

fn choose_target(
    runner: &mut GameRunner,
    target: Option<ObjectId>,
) -> Result<Vec<GameEvent>, String> {
    runner
        .act(GameAction::ChooseTarget {
            target: target.map(TargetRef::Object),
        })
        .map(|result| result.events)
        .map_err(|error| format!("{error:?}"))
}

/// The paused trigger's pending ability.
fn pending_trigger_ability(state: &GameState) -> &ResolvedAbility {
    &state
        .pending_trigger
        .as_ref()
        .expect("a trigger is pending its targets")
        .ability
}

/// CR 115.6 + CR 603.3d + CR 601.2c: a save written before a fight recorded its
/// printed operand restores with Kogla's 'up to one' on the fought creature,
/// and its paused prompt offers the decline; with no target chosen the fight
/// does nothing (CR 701.14a + CR 609.3).
#[test]
fn restored_pre_operand_kogla_prompt_offers_the_printed_decline() {
    let (mut runner, kogla, bear, wolf) = kogla_board();
    let _commit = runner.cast(kogla).commit();
    let (slots, _) = pass_to_trigger_targets(&mut runner);
    let slots = slots.expect("Kogla's trigger asks for a target");
    assert_eq!(slots.len(), 1, "reach guard: one fight slot: {slots:?}");
    assert_eq!(slots[0].effect_kind, EffectKind::Fight);
    assert!(
        slots[0].optional,
        "reach guard: the current slot is optional"
    );
    assert_eq!(
        slots[0]
            .legal_targets
            .iter()
            .cloned()
            .collect::<HashSet<_>>(),
        HashSet::from([TargetRef::Object(bear), TargetRef::Object(wolf)]),
    );
    assert!(
        matches!(
            &pending_trigger_ability(runner.state()).effect,
            Effect::Fight {
                subject: TargetFilter::SelfRef,
                up_to_one: Some(FightSlotRole::Opponent),
                ..
            }
        ),
        "reach guard: Kogla's fight is one SelfRef slot with its operand recorded: {:?}",
        pending_trigger_ability(runner.state()).effect
    );

    let mut legacy = save(&runner);
    assert!(
        as_pre_operand_writer(&mut legacy) >= 1,
        "transform reach: a fight node was rewritten"
    );
    assert_eq!(
        legacy_up_to_one_spec(),
        serde_json::json!({"min": 0, "max": {"type": "Fixed", "value": 1}}),
        "transform reach: the rewritten spec is the shape older engines exported"
    );

    let restored = restore(legacy.clone()).expect("the pre-operand save restores");
    assert_eq!(
        optionals(&restored),
        vec![true],
        "the restored prompt offers the printed decline"
    );
    {
        let ability = pending_trigger_ability(&restored);
        assert!(
            matches!(
                &ability.effect,
                Effect::Fight {
                    up_to_one: Some(FightSlotRole::Opponent),
                    ..
                }
            ),
            "the restored fight records Kogla's operand: {:?}",
            ability.effect
        );
        assert!(ability.multi_target.is_none(), "{:?}", ability.multi_target);
        assert!(!ability.optional_targeting);
    }
    let mut runner = GameRunner::from_state(restored);
    let mut events = choose_target(&mut runner, None).expect("Kogla's fight is declined");
    events.extend(drain_stack(&mut runner));
    assert_eq!(fights_resolved(&events), 1, "{events:?}");
    for creature in [bear, wolf, kogla] {
        assert_eq!(runner.state().objects[&creature].damage_marked, 0);
    }
    assert_completed(&runner);

    // Sibling: selection still works on the restored prompt.
    let mut runner = GameRunner::from_state(restore(legacy).expect("restores again"));
    choose_target(&mut runner, Some(bear)).expect("the bear is chosen");
    drain_stack(&mut runner);
    let state = runner.state();
    assert_eq!(state.objects[&bear].zone, Zone::Graveyard, "Kogla deals 7");
    assert_eq!(
        state.objects[&kogla].damage_marked, 2,
        "the bear deals 2 back"
    );
    assert_completed(&runner);
}

/// Primal Might (verbatim, reminder text dropped).
const PRIMAL_MIGHT: &str = "Target creature you control gets +X/+X until end of turn. Then it fights up to one target creature you don't control.";

/// CR 601.2c + CR 115.6: a save paused at Primal Might's target prompt restores
/// with only its fight slot freed; the pump's target stays required. With the
/// fight declined the pump still resolves and nobody fights (CR 701.14a +
/// CR 609.3).
///
/// Primal Might rather than Bridgeworks Battle (the same shape: a required pump
/// on the fighter, then a `ParentTarget` fight declaring one up-to-one slot):
/// a saved prompt holding a fixed-direction pump slot ("+2/+2") does not
/// restore at all, because `TargetEffectDetail` and `PtDirection` both use the
/// serde tag `type` and collide; that pre-existing defect is outside this
/// test's scope. Primal
/// Might's "+X/+X" pump slot carries no direction, so its prompt round-trips.
#[test]
fn restored_pre_operand_primal_might_prompt_frees_only_the_fight_slot() {
    let (mut board, wolf) = two_opponent_board("Primal Might", PRIMAL_MIGHT, false);
    let champion = board.fighter.unwrap();
    let card_id = board.runner.state().objects[&board.card].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: board.card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Primal Might is cast");
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: a spell target prompt: {:?}",
        board.runner.state().waiting_for
    );
    let slots = prompt_slots(board.runner.state());
    assert_eq!(slots.len(), 2, "reach guard: pump + fight: {slots:?}");
    assert_ne!(slots[0].effect_kind, EffectKind::Fight);
    assert!(
        !slots[0].optional,
        "reach guard: the pump target is required"
    );
    assert_eq!(slots[1].effect_kind, EffectKind::Fight);
    assert!(slots[1].optional, "reach guard: the fight is up to one");

    let mut legacy = save(&board.runner);
    assert!(as_pre_operand_writer(&mut legacy) >= 1, "transform reach");

    let restored = restore(legacy.clone()).expect("the pre-operand save restores");
    assert_eq!(
        optionals(&restored),
        vec![false, true],
        "only the fight slot is freed"
    );
    let mut runner = GameRunner::from_state(restored);
    let mut events = choose_target(&mut runner, Some(champion)).expect("the pump target is chosen");
    events.extend(choose_target(&mut runner, None).expect("the fight is declined"));
    events.extend(drain_stack(&mut runner));
    let state = runner.state();
    assert_eq!(
        state.objects[&champion].power,
        Some(3),
        "the pump resolved with X = 0 (the staged card has no mana cost)"
    );
    for creature in [champion, board.opponent, wolf] {
        assert_eq!(state.objects[&creature].damage_marked, 0);
    }
    assert_eq!(fights_resolved(&events), 1, "{events:?}");
    assert_completed(&runner);

    // Hostile: the pump slot is still required after restore.
    let mut runner = GameRunner::from_state(restore(legacy).expect("restores again"));
    let declined = choose_target(&mut runner, None);
    assert!(
        declined.is_err(),
        "the pump target is required: {declined:?}"
    );
}

/// Both declines are refused on fresh restores of `save`: slot 0 outright, and
/// slot 1 after `fighter` fills slot 0.
fn assert_every_fight_slot_required(save: &serde_json::Value, fighter: ObjectId) {
    let mut runner = GameRunner::from_state(restore(save.clone()).expect("restores"));
    let declined = choose_target(&mut runner, None).expect_err("slot 0 is required");
    assert!(
        declined.contains("Cannot skip a required target"),
        "{declined}"
    );
    let mut runner = GameRunner::from_state(restore(save.clone()).expect("restores"));
    choose_target(&mut runner, Some(fighter)).expect("slot 0 is filled");
    let declined = choose_target(&mut runner, None).expect_err("slot 1 is required");
    assert!(
        declined.contains("Cannot skip a required target"),
        "{declined}"
    );
}

/// The restored pending trigger is the legacy fight left as decoded.
fn assert_left_as_decoded(state: &GameState) {
    let ability = pending_trigger_ability(state);
    assert!(
        matches!(
            &ability.effect,
            Effect::Fight {
                up_to_one: None,
                ..
            }
        ),
        "{:?}",
        ability.effect
    );
    assert_eq!(
        ability.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }))
    );
    assert!(!ability.optional_targeting);
}

/// Restore decision ('Convert, never refuse'): a save written before a fight
/// recorded its printed operand, whose fight declares two slots, cannot say
/// which one its 'up to one' frees (CR 115.6 + CR 601.2c). It restores as saved
/// and keeps the older engine's reading, every declared slot required, in a
/// paused prompt and in an idle definition; the chosen pair still fights
/// (CR 701.14a).
#[test]
fn restored_pre_operand_two_slot_fights_keep_every_slot_required() {
    // (a) Favored Enemy paused at its trigger prompt.
    let (mut favored, _wolf) = two_opponent_board("Favored Enemy", FAVORED_ENEMY_ENTERS, true);
    let champion = favored.fighter.unwrap();
    let bear = favored.opponent;
    cast_to_trigger_targets(&mut favored).expect("Favored Enemy's trigger asks for targets");
    let current = save(&favored.runner);
    assert_eq!(
        optionals(&restore(current.clone()).expect("the current save restores")),
        vec![false, true],
        "positive reach: untransformed, the fought creature is optional"
    );
    let mut legacy = current;
    assert!(as_pre_operand_writer(&mut legacy) >= 1, "transform reach");
    let restored = restore(legacy.clone()).expect("the two-slot save restores");
    assert_left_as_decoded(&restored);
    assert_eq!(optionals(&restored), vec![false, false]);
    assert_every_fight_slot_required(&legacy, champion);
    let mut runner = GameRunner::from_state(restored);
    let mut events = choose_target(&mut runner, Some(champion)).expect("the fighter is chosen");
    events.extend(choose_target(&mut runner, Some(bear)).expect("the bear is chosen"));
    events.extend(drain_stack(&mut runner));
    let state = runner.state();
    assert_eq!(state.objects[&bear].zone, Zone::Graveyard, "3/3 deals 3");
    assert_eq!(
        state.objects[&champion].damage_marked, 2,
        "2/2 deals 2 back"
    );
    assert_eq!(fights_resolved(&events), 1, "{events:?}");
    assert_completed(&runner);

    // (b) Gut paused at its trigger prompt.
    let mut gut = gut_board();
    let champion = gut.fighter.unwrap();
    let opponent = gut.opponent;
    cast_to_trigger_targets(&mut gut).expect("Gut's trigger asks for targets");
    let current = save(&gut.runner);
    assert_eq!(
        optionals(&restore(current.clone()).expect("the current save restores")),
        vec![true, false],
        "positive reach: untransformed, the fighter is optional"
    );
    let mut legacy = current;
    assert!(as_pre_operand_writer(&mut legacy) >= 1, "transform reach");
    let restored = restore(legacy.clone()).expect("the two-slot save restores");
    assert_left_as_decoded(&restored);
    assert_eq!(optionals(&restored), vec![false, false]);
    assert_every_fight_slot_required(&legacy, champion);
    let mut runner = GameRunner::from_state(restored);
    choose_target(&mut runner, Some(champion)).expect("the fighter is chosen");
    choose_target(&mut runner, Some(opponent)).expect("the fought creature is chosen");
    drain_stack(&mut runner);
    let state = runner.state();
    assert_eq!(state.objects[&opponent].zone, Zone::Battlefield);
    assert_eq!(state.objects[&opponent].damage_marked, 3, "3/3 deals 3");
    assert_eq!(
        state.objects[&champion].damage_marked, 2,
        "2/4 deals 2 back"
    );
    assert_completed(&runner);

    // (c) Idle: Favored Enemy still in hand at the save, cast after restore.
    let (idle, _wolf) = two_opponent_board("Favored Enemy", FAVORED_ENEMY_ENTERS, true);
    let champion = idle.fighter.unwrap();
    let mut legacy = save(&idle.runner);
    assert!(as_pre_operand_writer(&mut legacy) >= 1, "transform reach");
    let restored = restore(legacy).expect("the idle two-slot save restores");
    assert!(
        legacy_fight_count(
            &serde_json::to_value(&restored.objects[&idle.card]).expect("the card serializes")
        ) >= 1,
        "the hand card keeps its legacy fight node"
    );
    let mut runner = GameRunner::from_state(restored);
    let _commit = runner.cast(idle.card).commit();
    let (slots, _) = pass_to_trigger_targets(&mut runner);
    let slots = slots.expect("the restored card's trigger prompts");
    assert_eq!(
        slots.iter().map(|slot| slot.optional).collect::<Vec<_>>(),
        vec![false, false]
    );
    assert_every_fight_slot_required(&save(&runner), champion);
}

/// CR 115.6 + CR 601.2c: current saves restore unchanged. Kogla's and Favored
/// Enemy's "up to one" frees the fought creature; Gut's frees the fighter.
#[test]
fn current_fight_saves_restore_unchanged() {
    let (mut kogla_runner, kogla, _, _) = kogla_board();
    let _commit = kogla_runner.cast(kogla).commit();
    pass_to_trigger_targets(&mut kogla_runner)
        .0
        .expect("Kogla's trigger asks for a target");
    let mut favored = favored_enemy(true);
    cast_to_trigger_targets(&mut favored).expect("Favored Enemy's trigger asks for targets");
    let mut gut = gut_board();
    cast_to_trigger_targets(&mut gut).expect("Gut's trigger asks for targets");

    for (runner, role) in [
        (&kogla_runner, FightSlotRole::Opponent),
        (&favored.runner, FightSlotRole::Opponent),
        (&gut.runner, FightSlotRole::Subject),
    ] {
        let current = save(runner);
        let restored = restore(current.clone()).expect("a current save restores");
        let ability = pending_trigger_ability(&restored);
        assert!(
            matches!(
                &ability.effect,
                Effect::Fight { up_to_one: Some(restored_role), .. } if *restored_role == role
            ),
            "{:?}",
            ability.effect
        );
        assert_eq!(optionals(&restored), optionals(runner.state()));
        assert_eq!(
            fight_effects(&save(&GameRunner::from_state(restored))),
            fight_effects(&current),
            "every fight payload restores unchanged"
        );
    }
}

/// The production decoder migrates every legacy fight in real committed dumps
/// written before the operand was recorded, including a granted trigger nested
/// in an effect payload and deck-pool faces.
#[test]
fn committed_pre_operand_dumps_migrate_every_legacy_fight() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for dump in [
        "engine/tests/fixtures/witherbloom_sprout_lumaret_4p.json.gz",
        "engine/tests/fixtures/witherbloom_sprout_lumaret_simple_4p.json.gz",
    ] {
        let path = root.join(dump);
        let raw = crate::loop_shortcut_drain_boards::committed_document(&path);
        assert!(
            legacy_fight_count(&raw) >= 1,
            "positive reach: {dump} holds a legacy fight node"
        );
        let restored = crate::committed_dump_walk::restore_committed(&path)
            .unwrap_or_else(|error| panic!("{dump} restores: {error}"));
        let restored = serde_json::to_value(&restored).expect("the state serializes");
        assert_eq!(
            legacy_fight_count(&restored),
            0,
            "{dump}: every legacy fight node is migrated"
        );
        assert!(
            fight_effects(&restored)
                .iter()
                .any(|effect| effect.contains("\"up_to_one\":\"Opponent\"")),
            "{dump}: a migrated fight records its operand"
        );
    }
}
