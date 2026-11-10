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
//!
//! Review findings on https://github.com/phase-rs/phase/pull/8820 and
//! https://github.com/phase-rs/phase/pull/10714.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
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
    resolve_stack(&mut board);

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

/// A 3/3 "Champion" for you, a 2/2 for the opponent, and Gut (4/3) in your
/// hand. Returns the unbuilt scenario so a row can add more cards, plus the
/// Champion, the opponent's creature and Gut.
fn gut_scenario() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let champion = scenario.add_creature(P0, "Champion", 3, 3).id();
    let opponent = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
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

/// CR 701.14a: Gut's chosen pair fights.
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
    assert_eq!(state.objects[&opponent].zone, Zone::Graveyard);
    assert_eq!(state.objects[&fighter].damage_marked, 2, "2/2 deals 2 back");
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
    assert_eq!(state.objects[&opponent].zone, Zone::Graveyard);
    assert_eq!(state.objects[&fighter].damage_marked, 2);
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
    resolve_stack(&mut board);
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
    resolve_stack(&mut board);
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
