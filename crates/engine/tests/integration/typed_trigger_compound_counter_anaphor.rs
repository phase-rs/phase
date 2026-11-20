//! "Tap target creature … and put a stun counter on it" under a typed trigger
//! subject: the counter goes on the announced target, not on the ability's
//! source or on the object the trigger event names.
//!
//! Grappling Kraken:
//!   Landfall — Whenever a land you control enters, tap target creature an
//!   opponent controls and put a stun counter on it.
//! Fear of Sleep Paralysis:
//!   Eerie — Whenever this creature or another enchantment you control enters
//!   and whenever you fully unlock a Room, tap up to one target creature and put
//!   a stun counter on it.
//! Mjölnir, Storm Hammer:
//!   Whenever equipped creature attacks, tap target creature defending player
//!   controls and put a stun counter on it. Then Mjölnir deals damage to each
//!   opponent equal to the number of tapped creatures that opponent controls.
//!
//! CR 608.2c: read the whole instruction — the only object named before "it" is
//! the announced target. CR 115.1d: "target creature …" makes the triggered
//! ability targeted; CR 603.3d: its target is chosen as it goes on the stack.
//! CR 122.1d: the stun counter is the replacement that stops untapping.
//!
//! Red at base: the typed trigger subject skipped the continuation's anaphor
//! rewrite, so the stun counter landed on the ability's source.

use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const GRAPPLING_KRAKEN: &str = "Landfall — Whenever a land you control enters, tap target creature an opponent controls and put a stun counter on it. (If a permanent with a stun counter would become untapped, remove one from it instead.)";
const FEAR_OF_SLEEP_PARALYSIS: &str = "Flying\nEerie — Whenever this creature or another enchantment you control enters and whenever you fully unlock a Room, tap up to one target creature and put a stun counter on it.\nStun counters can't be removed from permanents your opponents control. (They won't untap if they have stun counters.)";
const MJOLNIR_STORM_HAMMER: &str = "When Mjölnir enters, attach it to target legendary creature you control.\nWhenever equipped creature attacks, tap target creature defending player controls and put a stun counter on it. Then Mjölnir deals damage to each opponent equal to the number of tapped creatures that opponent controls.\nEquip {4}";

fn stun(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Stun)
        .copied()
        .unwrap_or(0)
}

fn tapped(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id].tapped
}

/// Completion boundary: priority returned with nothing left on the stack.
fn settled(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty()
}

/// The pending trigger's single target slot. Panics unless the engine is
/// waiting on `TriggerTargetSelection` — the prompt is asserted unconditionally.
fn trigger_target_slot(runner: &GameRunner) -> TargetSelectionSlot {
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for
    else {
        panic!(
            "expected TriggerTargetSelection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "one target slot: {target_slots:?}");
    target_slots[0].clone()
}

fn legal_objects(slot: &TargetSelectionSlot) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = slot
        .legal_targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    ids.sort();
    ids
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// Answer the pending trigger target prompt with `target`, then pass priority
/// until the stack is empty. Any other prompt fails the test. Returns every
/// event emitted from the answer through completion.
fn choose_and_settle(runner: &mut GameRunner, target: Option<ObjectId>) -> Vec<GameEvent> {
    let mut events = runner
        .act(GameAction::ChooseTarget {
            target: target.map(TargetRef::Object),
        })
        .expect("target choice accepted")
        .events;
    for _ in 0..32 {
        if settled(runner.state()) {
            return events;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt while resolving: {other:?}"),
        };
        events.extend(runner.act(action).expect("priority pass accepted").events);
    }
    panic!(
        "trigger did not resolve; waiting on {:?}",
        runner.state().waiting_for
    );
}

fn play_land(runner: &mut GameRunner, land: ObjectId) {
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("land play accepted");
}

struct KrakenBoard {
    runner: GameRunner,
    kraken: ObjectId,
    a: ObjectId,
    b: ObjectId,
    land: ObjectId,
}

fn kraken_board() -> KrakenBoard {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let kraken = s
        .add_creature(P0, "Grappling Kraken", 5, 6)
        .from_oracle_text_with_keywords(&["Landfall"], GRAPPLING_KRAKEN)
        .id();
    let a = s.add_creature(P1, "Opponent Bear", 2, 2).id();
    let b = s.add_creature(P1, "Opponent Ogre", 3, 3).id();
    let land = s.add_land_to_hand(P0, "Island").id();
    KrakenBoard {
        runner: s.build(),
        kraken,
        a,
        b,
        land,
    }
}

/// R-1: the land entering (the trigger event's object, CR 603.2), the Kraken
/// (the source) and the chosen creature are three different objects; only the
/// chosen one — the second of two candidates — receives the counter.
#[test]
fn grappling_kraken_landfall_stuns_the_chosen_target_not_itself() {
    let KrakenBoard {
        mut runner,
        kraken,
        a,
        b,
        land,
    } = kraken_board();
    play_land(&mut runner, land);
    let slot = trigger_target_slot(&runner);
    assert_eq!(
        legal_objects(&slot),
        sorted(vec![a, b]),
        "only creatures an opponent controls are legal"
    );
    choose_and_settle(&mut runner, Some(b));
    let state = runner.state();
    assert!(
        tapped(state, b),
        "the tap head resolved on the chosen target"
    );
    assert_eq!(
        stun(state, b),
        1,
        "the stun counter goes on the tapped target"
    );
    assert_eq!(stun(state, kraken), 0, "the source gets no stun counter");
    assert!(!tapped(state, a), "the other candidate is untouched");
    assert_eq!(stun(state, a), 0);
    assert_eq!(
        stun(state, land),
        0,
        "the entering land gets no stun counter"
    );
    assert!(settled(state));
}

/// R-2: Grappling Kraken ruling — "You may target a creature that's already
/// tapped with Grappling Kraken's ability. In that case, you'll just put a stun
/// counter on that creature." Tapping an already-tapped permanent does nothing
/// (CR 701.26a), and the continuation still resolves.
#[test]
fn grappling_kraken_stuns_an_already_tapped_target() {
    let KrakenBoard {
        mut runner,
        kraken,
        a,
        b,
        land,
    } = kraken_board();
    runner.state_mut().objects.get_mut(&b).unwrap().tapped = true;
    play_land(&mut runner, land);
    let slot = trigger_target_slot(&runner);
    assert_eq!(legal_objects(&slot), sorted(vec![a, b]));
    choose_and_settle(&mut runner, Some(b));
    let state = runner.state();
    assert!(tapped(state, b));
    assert_eq!(
        stun(state, b),
        1,
        "an already-tapped target still gets the counter"
    );
    assert_eq!(stun(state, kraken), 0, "the source gets no stun counter");
    assert!(settled(state));
}

struct FearBoard {
    runner: GameRunner,
    fear: ObjectId,
    a: ObjectId,
    c: ObjectId,
    filler: ObjectId,
}

/// A textless land that is also an enchantment (an object can have several
/// card types, CR 205.2b): playing it is "another enchantment you control"
/// entering, with no casting machinery between the entry and the trigger. One
/// entering enchantment, so exactly one eerie trigger goes on the stack.
fn fear_board() -> FearBoard {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let fear = s
        .add_creature(P0, "Fear of Sleep Paralysis", 6, 6)
        .from_oracle_text_with_keywords(&["Eerie", "Flying"], FEAR_OF_SLEEP_PARALYSIS)
        .id();
    let a = s.add_creature(P1, "Opponent Bear", 2, 2).id();
    let c = s.add_creature(P0, "Own Bear", 2, 2).id();
    let mut filler = s.add_land_to_hand(P0, "Filler Enchantment Land");
    filler.as_enchantment();
    let filler = filler.id();
    FearBoard {
        runner: s.build(),
        fear,
        a,
        c,
        filler,
    }
}

/// R-3: an `Or{SelfRef, Typed}` trigger subject; the counter goes on the chosen
/// creature, not on Fear (the source) or the entering enchantment.
#[test]
fn fear_of_sleep_paralysis_eerie_stuns_the_chosen_creature() {
    let FearBoard {
        mut runner,
        fear,
        a,
        c,
        filler,
    } = fear_board();
    play_land(&mut runner, filler);
    let slot = trigger_target_slot(&runner);
    assert!(slot.optional, "\"up to one target\" is an optional slot");
    let legal = legal_objects(&slot);
    assert!(
        legal.contains(&a) && legal.contains(&c),
        "any creature is legal, got {legal:?}"
    );
    choose_and_settle(&mut runner, Some(a));
    let state = runner.state();
    assert!(
        tapped(state, a),
        "the tap head resolved on the chosen target"
    );
    assert_eq!(
        stun(state, a),
        1,
        "the stun counter goes on the tapped target"
    );
    assert_eq!(stun(state, fear), 0, "the source gets no stun counter");
    assert_eq!(stun(state, filler), 0, "the entering enchantment gets none");
    assert!(!tapped(state, c));
    assert_eq!(stun(state, c), 0);
    assert!(settled(state));
}

/// R-4: CR 115.6 + CR 601.2c — "up to one" lets zero targets be chosen; the
/// number is announced before the targets. With no target there is nothing for
/// "it" to name, so no permanent gets a stun counter.
#[test]
fn fear_of_sleep_paralysis_with_no_target_places_no_stun_counter() {
    let FearBoard {
        mut runner,
        fear,
        a,
        c,
        filler,
    } = fear_board();
    play_land(&mut runner, filler);
    let slot = trigger_target_slot(&runner);
    assert!(slot.optional, "\"up to one target\" is an optional slot");
    let events = choose_and_settle(&mut runner, None);
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Tap,
                source_id,
                ..
            } if *source_id == fear
        )),
        "reach guard: Fear's trigger resolved its tap head, got {events:?}"
    );
    let state = runner.state();
    for (name, id) in [("Fear", fear), ("filler", filler), ("A", a), ("C", c)] {
        assert_eq!(stun(state, id), 0, "{name} must get no stun counter");
    }
    assert!(settled(state));
}

/// R-5: "Whenever equipped creature attacks" (an `AttachedTo` trigger subject).
/// The attacking equipped creature, the Equipment (the source) and the chosen
/// defending creature are distinct; only the chosen creature gets the counter.
/// The trailing "Then Mjölnir deals damage …" is not asserted here.
#[test]
fn mjolnir_equipped_attack_stuns_the_chosen_defending_creature() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let hammer = s
        .add_artifact_from_oracle(P0, "Mjölnir, Storm Hammer", MJOLNIR_STORM_HAMMER)
        .as_legendary()
        .with_subtypes(vec!["Equipment"])
        .id();
    let host = s.add_creature(P0, "Hero", 3, 3).id();
    let a = s.add_creature(P1, "Opponent Bear", 2, 2).id();
    let b = s.add_creature(P1, "Opponent Ogre", 3, 3).id();
    let mut runner = s.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&hammer).unwrap().attached_to = Some(AttachTarget::Object(host));
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .attachments
            .push(hammer);
        state.layers_dirty.mark_full();
    }
    runner.advance_to_phase(Phase::DeclareAttackers);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(host, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("attack declared");
    let slot = trigger_target_slot(&runner);
    assert_eq!(
        legal_objects(&slot),
        sorted(vec![a, b]),
        "only creatures the defending player controls are legal"
    );
    choose_and_settle(&mut runner, Some(b));
    let state = runner.state();
    assert!(
        tapped(state, b),
        "the tap head resolved on the chosen target"
    );
    assert_eq!(
        stun(state, b),
        1,
        "the stun counter goes on the tapped target"
    );
    assert_eq!(stun(state, hammer), 0, "the Equipment gets no stun counter");
    assert_eq!(
        stun(state, host),
        0,
        "the attacking equipped creature gets none"
    );
    assert_eq!(stun(state, a), 0);
    assert!(settled(state));
}
