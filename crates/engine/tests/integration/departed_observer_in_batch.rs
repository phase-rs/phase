//! CR 603.10 + CR 608.2c: a battlefield observer that a LATER instruction of
//! the same resolution moves off the battlefield still triggers on the events
//! of the earlier instructions — it existed immediately after each of them.
//! An observer that left SIMULTANEOUSLY with an event, or before it, does not.
//!
//! Printed cards (Oracle text verbatim, data/card-data.json):
//!   Soul Warden — "Whenever another creature enters, you gain 1 life."
//!   Martial Coup — "Create X 1/1 white Soldier creature tokens. If X is 5 or
//!     more, destroy all other creatures."
//!   Phyrexian Rebirth — "Destroy all creatures, then create an X/X colorless
//!     Phyrexian Horror artifact creature token, where X is the number of
//!     creatures destroyed this way."
//!   Kalitas, Traitor of Ghet — "Lifelink / If a nontoken creature an opponent
//!     controls would die, instead exile that card and create a 2/2 black
//!     Zombie creature token. / {2}{B}, Sacrifice another Vampire or Zombie:
//!     Put two +1/+1 counters on Kalitas."
//!   Day of Judgment — "Destroy all creatures."
//!   Smallpox — "Each player loses 1 life, discards a card, sacrifices a
//!     creature of their choice, then sacrifices a land of their choice."
//!   Bone Miser — "Whenever you discard a creature card, create a 2/2 black
//!     Zombie creature token. / Whenever you discard a land card, add {B}{B}.
//!     / Whenever you discard a noncreature, nonland card, draw a card."

use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, SubjectIncarnationScope, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Island", "Swamp", "Mountain"]);
    scenario.with_library_top(P1, &["Forest", "Island", "Swamp", "Mountain"]);
    scenario
}

fn add_mana(runner: &mut GameRunner, player: PlayerId, colored: &[ManaType], generic: usize) {
    let units = colored
        .iter()
        .copied()
        .chain(std::iter::repeat_n(ManaType::Colorless, generic));
    for color in units {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn tokens_named(runner: &GameRunner, controller: PlayerId, name: &str) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.controller == controller && object.name == name
        })
        .collect()
}

/// Martial Coup with X = 5: five Soldiers enter (instruction 1), then every
/// other creature is destroyed (instruction 2). Soul Warden saw each Soldier
/// enter before it was destroyed, so its controller gains 5.
#[test]
fn soul_warden_destroyed_by_martial_coup_saw_the_soldiers_enter() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let warden = scenario.add_real_card(P0, "Soul Warden", Zone::Battlefield, db);
    let coup = scenario.add_real_card(P0, "Martial Coup", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 5);

    runner.cast(coup).x(5).resolve();

    assert_eq!(
        tokens_named(&runner, P0, "Soldier").len(),
        5,
        "reach: instruction 1 created five Soldiers"
    );
    assert_eq!(
        runner.state().objects[&warden].zone,
        Zone::Graveyard,
        "reach: instruction 2 destroyed Soul Warden"
    );
    assert_eq!(
        runner.life(P0),
        25,
        "Soul Warden triggered once for each Soldier that entered before it died"
    );
}

/// Phyrexian Rebirth destroys every creature first and creates its Horror
/// second. Soul Warden had already left when the Horror entered, so it does
/// not trigger. Paired positive: the Martial Coup row above.
#[test]
fn soul_warden_destroyed_before_the_horror_enters_does_not_see_it() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let warden = scenario.add_real_card(P0, "Soul Warden", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let rebirth = scenario.add_real_card(P0, "Phyrexian Rebirth", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 4);

    runner.cast(rebirth).resolve();

    let horrors = tokens_named(&runner, P0, "Phyrexian Horror");
    assert_eq!(horrors.len(), 1, "reach: the Horror entered after the wipe");
    assert_eq!(
        runner.state().objects[&horrors[0]].power,
        Some(2),
        "reach: two creatures were destroyed this way"
    );
    assert_eq!(
        runner.state().objects[&warden].zone,
        Zone::Graveyard,
        "reach: Soul Warden was destroyed by the first instruction"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "Soul Warden had left before the Horror entered"
    );
}

/// D6 (CR 603.10): one "destroy all" instruction moves every creature
/// simultaneously. Kalitas's replacement turns each opposing nontoken death
/// into an exile plus a Zombie token, all part of that one event. P1's Soul
/// Warden is exiled by the same event, so it does not exist immediately
/// after the Zombies enter and does not trigger.
#[test]
fn soul_warden_exiled_by_the_same_wipe_does_not_see_its_zombies() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    // Kalitas last: the wipe walks the battlefield in order, so Kalitas is
    // still there to replace the earlier members' deaths.
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let wipe = scenario.add_real_card(P0, "Day of Judgment", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 2);

    perf_counters::reset();
    runner.cast(wipe).resolve();
    let counters = perf_counters::departure_window_snapshot();

    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Exile,
        "reach: Kalitas exiled the Bears instead"
    );
    assert_eq!(
        runner.state().objects[&warden].zone,
        Zone::Exile,
        "reach: Kalitas exiled Soul Warden instead"
    );
    assert_eq!(
        tokens_named(&runner, P0, "Zombie").len(),
        2,
        "reach: each replaced death created a Zombie"
    );
    assert!(
        counters.in_instruction_cuts > 0,
        "reach: the Bears' Zombie entry was cut from Soul Warden's window"
    );
    assert_eq!(counters.unstamped_departures, 0);
    assert_eq!(
        runner.life(P1),
        20,
        "Soul Warden left with the Zombies' entry, not before it"
    );
}

/// Census probe (residual callers): Smallpox's discard is a player choice, so
/// its completion collects the discard and the continuation (the sacrifices)
/// as separate slices. Bone Miser saw the creature-card discard before
/// Smallpox's next instruction sacrificed it, so it makes a Zombie.
#[test]
#[ignore = "census blocker: the discard-choice completion collects its slices without the later departure"]
fn bone_miser_sacrificed_by_smallpox_saw_the_discard() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let miser = scenario.add_real_card(P0, "Bone Miser", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    scenario.add_real_card(P0, "Forest", Zone::Hand, db);
    let smallpox = scenario.add_real_card(P0, "Smallpox", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Black], 0);

    runner.cast(smallpox).discard(&[bears]).resolve();

    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "reach: P0 discarded the creature card"
    );
    assert_eq!(
        runner.state().objects[&miser].zone,
        Zone::Graveyard,
        "reach: the next instruction sacrificed Bone Miser"
    );
    assert_eq!(
        tokens_named(&runner, P0, "Zombie").len(),
        1,
        "Bone Miser triggered on the discard before it was sacrificed"
    );
}

/// The same chain without a discard choice (one card in hand): every
/// instruction resolves in one action and the post-action collector sees the
/// whole chain.
#[test]
fn bone_miser_sacrificed_by_smallpox_saw_an_unprompted_discard() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let miser = scenario.add_real_card(P0, "Bone Miser", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let smallpox = scenario.add_real_card(P0, "Smallpox", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Black], 0);

    runner.cast(smallpox).discard(&[bears]).resolve();

    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&miser].zone, Zone::Graveyard);
    assert_eq!(
        tokens_named(&runner, P0, "Zombie").len(),
        1,
        "Bone Miser triggered on the discard before it was sacrificed"
    );
}

// ---------------------------------------------------------------------------
// Rows driven step by step, so the triggers are counted on the stack before
// they resolve.
//
// More printed cards (Oracle text verbatim, data/card-data.json):
//   Gix's Command — "Choose two — / • Put two +1/+1 counters on up to one
//     creature. It gains lifelink until end of turn. / • Destroy each creature
//     with power 2 or less. / • Return up to two creature cards from your
//     graveyard to your hand. / • Each opponent sacrifices a creature with the
//     greatest power among creatures they control."
//   Blood Artist — "Whenever this creature or another creature dies, target
//     player loses 1 life and you gain 1 life."
//   Seer of Stolen Sight — "Menace / Whenever one or more artifacts and/or
//     creatures you control are put into a graveyard from the battlefield,
//     surveil 1."
//   Syr Konrad, the Grim — "Whenever another creature dies, or a creature card
//     is put into a graveyard from anywhere other than the battlefield, or a
//     creature card leaves your graveyard, Syr Konrad deals 1 damage to each
//     opponent. / {1}{B}: Each player mills a card."
//   Fractured Identity — "Exile target nonland permanent. Each player other
//     than its controller creates a token that's a copy of it."
//   Aether Snap — "Remove all counters from all permanents and exile all
//     tokens."
//   Cloudsculpt Armorer — "When this creature enters, put a shield counter on
//     target artifact or creature. / Whenever one or more counters are removed
//     from a permanent you control, seek a nonland card."
//   Invasion of Kaladesh — "(As a Siege enters, choose an opponent to protect
//     it. You and others can attack it. When it's defeated, exile it, then cast
//     it transformed.) / When this Siege enters, create a 1/1 colorless Thopter
//     artifact creature token with flying."
//   Chandra, Fire Artisan — "Whenever one or more loyalty counters are removed
//     from Chandra, she deals that much damage to target opponent or
//     planeswalker. ..."
//   Jace Beleren — "[+2]: Each player draws a card. / [−1]: Target player
//     draws a card. / [−10]: Target player mills twenty cards."
//   Anguished Unmaking — "Exile target nonland permanent. You lose 3 life."
//   Into the Flood Maw — "Gift a tapped Fish (You may promise an opponent a
//     gift as you cast this spell. If you do, they create a tapped 1/1 blue
//     Fish creature token before its other effects.) / Return target creature
//     an opponent controls to its owner's hand. If the gift was promised,
//     instead return target nonland permanent an opponent controls to its
//     owner's hand."
// ---------------------------------------------------------------------------

/// Gix's Command mode indices (0-based, printed order).
const GIX_DESTROY_SMALL: usize = 1;
const GIX_OPPONENT_SACRIFICES: usize = 3;

/// Pass priority until `done` holds at a priority window, answering trigger target prompts with
/// `target_player` when it is legal (else the first legal target), placing
/// triggers in identity order and keeping every surveilled card on top.
fn drive(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    target_player: PlayerId,
    done: impl Fn(&GameRunner) -> bool,
) {
    for _ in 0..128 {
        // Triggers waiting to be ordered are not on the stack yet.
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        // Only a priority window is settled: a trigger may still be waiting
        // for its target.
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) && done(runner) {
            return;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                let wanted = TargetRef::Player(target_player);
                let target = if legal.contains(&wanted) {
                    wanted
                } else {
                    legal.first().cloned().expect("a legal trigger target")
                };
                GameAction::ChooseTarget {
                    target: Some(target),
                }
            }
            WaitingFor::SurveilChoice { cards, .. } => GameAction::SelectCards { cards },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("driver stopped at an unexpected prompt: {other:?}"),
        };
        let result = runner
            .act(action)
            .expect("the engine must accept the action");
        events.extend(result.events);
    }
    panic!("driver never reached its goal");
}

fn on_stack(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == id)
}

/// Triggered abilities from `source` on the stack, with their batched subject
/// count.
fn triggers_from(runner: &GameRunner, source: ObjectId) -> Vec<Option<u32>> {
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                subject_match_count,
                ..
            } if *source_id == source => Some(*subject_match_count),
            _ => None,
        })
        .collect()
}

/// CR 603.10 + CR 400.7: every stacked trigger of `source` carries a `Live`
/// source context — the departure-window scope never leaves matching.
fn stacked_trigger_sources_are_live(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.iter().all(|entry| match &entry.kind {
        StackEntryKind::TriggeredAbility {
            source_id, ability, ..
        } if *source_id == source => ability
            .trigger_source
            .as_ref()
            .is_some_and(|context| context.subject_scope == SubjectIncarnationScope::Live),
        _ => true,
    })
}

/// Cast Gix's Command (destroy power 2 or less, then each opponent sacrifices
/// its greatest-power creature) and pass until it has left the stack, leaving
/// its triggers on the stack.
fn cast_gix(runner: &mut GameRunner, gix: ObjectId) -> Vec<GameEvent> {
    add_mana(runner, P0, &[ManaType::Black, ManaType::Black], 3);
    let mut events = runner
        .cast(gix)
        .modes(&[GIX_DESTROY_SMALL, GIX_OPPONENT_SACRIFICES])
        .commit()
        .events()
        .to_vec();
    drive(runner, &mut events, P0, |r| !on_stack(r, gix));
    events
}

/// T5: Blood Artist (given three +1/+1 counters so it survives the first
/// mode) sees the Bears die to the first mode, then is sacrificed by the
/// second: one trigger for the Bears (departure window) and one for itself.
/// The reverse order (Blood Artist leaving first) is the existing
/// `dies_observer_does_not_observe_sequential_departure` lib test.
#[test]
fn blood_artist_sees_an_earlier_death_then_dies_to_a_later_mode() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let artist = scenario.add_real_card(P1, "Blood Artist", Zone::Battlefield, db);
    scenario.with_counter(artist, CounterType::Plus1Plus1, 3);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let gix = scenario.add_real_card(P0, "Gix's Command", Zone::Hand, db);
    let mut runner = scenario.build();

    cast_gix(&mut runner, gix);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "reach: the first mode destroyed the Bears"
    );
    assert_eq!(
        runner.state().objects[&artist].zone,
        Zone::Graveyard,
        "reach: the second mode sacrificed Blood Artist"
    );
    assert_eq!(
        triggers_from(&runner, artist).len(),
        2,
        "one trigger for the Bears' earlier death and one for its own"
    );
    assert!(stacked_trigger_sources_are_live(&runner, artist));
    let mut events = Vec::new();
    drive(&mut runner, &mut events, P0, |r| r.state().stack.is_empty());
    assert_eq!(runner.life(P0), 18);
    assert_eq!(runner.life(P1), 22);
}

/// T11 (CR 603.2c): Seer of Stolen Sight's "one or more" trigger fires once
/// for the two Bears the first mode destroys together (it is then sacrificed
/// by the second mode), and once more for its own trip to the graveyard —
/// two different events.
#[test]
fn seer_of_stolen_sight_batches_an_earlier_wipe_once_then_sees_itself() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let seer = scenario.add_real_card(P1, "Seer of Stolen Sight", Zone::Battlefield, db);
    scenario.with_counter(seer, CounterType::Plus1Plus1, 1);
    let bears = [
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db),
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db),
    ];
    let gix = scenario.add_real_card(P0, "Gix's Command", Zone::Hand, db);
    let mut runner = scenario.build();

    cast_gix(&mut runner, gix);
    for id in bears {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(
        runner.state().objects[&seer].zone,
        Zone::Graveyard,
        "reach: the second mode sacrificed the Seer"
    );
    let mut counts = triggers_from(&runner, seer);
    counts.sort();
    assert_eq!(
        counts.len(),
        2,
        "one batched trigger for the wipe, one for the Seer itself: {counts:?}"
    );
    assert!(stacked_trigger_sources_are_live(&runner, seer));
}

/// T12: Syr Konrad's multi-clause trigger is admitted for every event kind.
/// It sees the Bears die to the first mode, is then sacrificed by the second,
/// and deals 1 damage to P0 once (its own death is not "another creature").
#[test]
fn syr_konrad_sees_an_earlier_death_then_is_sacrificed() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let konrad = scenario.add_real_card(P1, "Syr Konrad, the Grim", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let gix = scenario.add_real_card(P0, "Gix's Command", Zone::Hand, db);
    let mut runner = scenario.build();

    cast_gix(&mut runner, gix);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&konrad].zone,
        Zone::Graveyard,
        "reach: the second mode sacrificed Syr Konrad"
    );
    assert_eq!(triggers_from(&runner, konrad).len(), 1);
    let mut events = Vec::new();
    drive(&mut runner, &mut events, P0, |r| r.state().stack.is_empty());
    assert_eq!(runner.life(P0), 19, "1 damage for the Bears' death");
}

fn token_named(runner: &GameRunner, controller: PlayerId, name: &str) -> ObjectId {
    *tokens_named(runner, controller, name)
        .first()
        .unwrap_or_else(|| panic!("{controller:?} controls no token named {name}"))
}

/// P0 casts Fractured Identity on `target` (declaring `extra_targets` for any
/// trigger the copy's entry raises, and `named_choice` for an "as it enters"
/// choice); returns P0's token copy.
fn fracture(
    runner: &mut GameRunner,
    identity: ObjectId,
    target: ObjectId,
    extra_targets: &[ObjectId],
    named_choice: Option<&str>,
) -> ObjectId {
    let name = runner.state().objects[&target].name.clone();
    add_mana(runner, P0, &[ManaType::White, ManaType::Blue], 3);
    let targets: Vec<ObjectId> = std::iter::once(target)
        .chain(extra_targets.iter().copied())
        .collect();
    let cast = runner.cast(identity).target_objects(&targets);
    match named_choice {
        Some(choice) => cast.choose_option(choice).resolve(),
        None => cast.resolve(),
    };
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Exile,
        "reach: Fractured Identity exiled its target"
    );
    token_named(runner, P0, &name)
}

/// Cast Aether Snap and pass until it has left the stack, leaving its
/// triggers on the stack.
fn snap(runner: &mut GameRunner, aether_snap: ObjectId) -> Vec<GameEvent> {
    add_mana(runner, P0, &[ManaType::Black, ManaType::Black], 3);
    let mut events = runner.cast(aether_snap).commit().events().to_vec();
    drive(runner, &mut events, P1, |r| !on_stack(r, aether_snap));
    events
}

fn counter_removed_from(events: &[GameEvent], id: ObjectId) -> bool {
    events.iter().any(
        |event| matches!(event, GameEvent::CounterRemoved { object_id, .. } if *object_id == id),
    )
}

fn left_battlefield_to_exile(events: &[GameEvent], id: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Exile,
                ..
            } if *object_id == id
        )
    })
}

fn counters_on(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

/// T2: P0's Cloudsculpt Armorer token watches "a permanent you control". Its
/// entry put a shield counter on P0's Bears; Aether Snap removes the Bears'
/// counters (instruction 1), then exiles the token (instruction 2). The token
/// seeks once for the one recipient.
#[test]
fn armorer_token_seeks_once_for_counters_removed_before_it_was_exiled() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let armorer = scenario.add_real_card(P1, "Cloudsculpt Armorer", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_counter(bears, CounterType::Plus1Plus1, 1);
    let identity = scenario.add_real_card(P0, "Fractured Identity", Zone::Hand, db);
    let aether_snap = scenario.add_real_card(P0, "Aether Snap", Zone::Hand, db);
    let mut runner = scenario.build();

    let token = fracture(&mut runner, identity, armorer, &[bears], None);
    assert_eq!(
        counters_on(&runner, bears, CounterType::Shield),
        1,
        "reach: the token's entry put a shield counter on the Bears"
    );
    let events = snap(&mut runner, aether_snap);
    assert!(counter_removed_from(&events, bears), "reach: instruction 1");
    assert!(
        left_battlefield_to_exile(&events, token),
        "reach: instruction 2"
    );
    assert_eq!(
        triggers_from(&runner, token).len(),
        1,
        "the departed token seeks once for its one recipient"
    );
    assert!(stacked_trigger_sources_are_live(&runner, token));
}

/// T2 negative: the token's controller does not control the permanent that
/// lost counters (its shield went on P1's Bears), so it does not seek.
#[test]
fn armorer_token_ignores_counters_removed_from_an_opponents_permanent() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let armorer = scenario.add_real_card(P1, "Cloudsculpt Armorer", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let identity = scenario.add_real_card(P0, "Fractured Identity", Zone::Hand, db);
    let aether_snap = scenario.add_real_card(P0, "Aether Snap", Zone::Hand, db);
    let mut runner = scenario.build();

    let token = fracture(&mut runner, identity, armorer, &[bears], None);
    assert_eq!(counters_on(&runner, bears, CounterType::Shield), 1);
    let events = snap(&mut runner, aether_snap);
    assert!(
        counter_removed_from(&events, bears),
        "reach: P1's Bears lost its shield counter"
    );
    assert!(left_battlefield_to_exile(&events, token));
    assert!(
        triggers_from(&runner, token).is_empty(),
        "P1's Bears is not a permanent P0 controls"
    );
}

/// T8 (a) CR 122.2: a planeswalker token without a removal trigger (Jace)
/// loses its loyalty and is exiled — nothing triggers from it — while the
/// live nontoken Chandra in the same board triggers exactly once.
#[test]
fn a_planeswalker_token_without_the_trigger_does_not_fire() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let chandra = scenario.add_real_card(P1, "Chandra, Fire Artisan", Zone::Battlefield, db);
    let jace = scenario.add_real_card(P1, "Jace Beleren", Zone::Battlefield, db);
    let identity = scenario.add_real_card(P0, "Fractured Identity", Zone::Hand, db);
    let aether_snap = scenario.add_real_card(P0, "Aether Snap", Zone::Hand, db);
    let mut runner = scenario.build();
    for (id, loyalty) in [(chandra, 4), (jace, 3)] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Loyalty, loyalty);
    }

    let token = fracture(&mut runner, identity, jace, &[], None);
    assert!(counters_on(&runner, token, CounterType::Loyalty) > 0);
    let events = snap(&mut runner, aether_snap);
    assert!(
        counter_removed_from(&events, token),
        "reach: Jace token lost loyalty"
    );
    assert!(left_battlefield_to_exile(&events, token));
    assert!(counter_removed_from(&events, chandra));
    assert!(triggers_from(&runner, token).is_empty());
    assert_eq!(
        triggers_from(&runner, chandra).len(),
        1,
        "the live Chandra triggers once"
    );
}

/// T8 (c) CR 122.2: exiling the Chandra token removes no counters (they
/// simply cease to exist), so it does not trigger. Paired positive: the
/// Aether Snap rows in `chandra_fire_artisan_loyalty_damage`.
#[test]
fn a_plainly_exiled_chandra_token_does_not_trigger() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let chandra = scenario.add_real_card(P1, "Chandra, Fire Artisan", Zone::Battlefield, db);
    let identity = scenario.add_real_card(P0, "Fractured Identity", Zone::Hand, db);
    let unmaking = scenario.add_real_card(P0, "Anguished Unmaking", Zone::Hand, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&chandra)
        .unwrap()
        .counters
        .insert(CounterType::Loyalty, 4);

    let token = fracture(&mut runner, identity, chandra, &[], None);
    assert_eq!(counters_on(&runner, token, CounterType::Loyalty), 4);
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::Black], 1);
    let mut events = runner
        .cast(unmaking)
        .target_object(token)
        .commit()
        .events()
        .to_vec();
    drive(&mut runner, &mut events, P1, |r| !on_stack(r, unmaking));
    assert!(
        left_battlefield_to_exile(&events, token),
        "reach: the token was exiled"
    );
    assert!(!counter_removed_from(&events, token));
    assert!(triggers_from(&runner, token).is_empty());
    assert_eq!(runner.life(P1), 20);
}

/// T9: the departure-window arm resolves no observer context for a wipe of
/// trigger-less creatures, nor for a wipe whose trigger-bearing members all
/// left simultaneously (their departures are the CR 603.10a arms' events).
/// Positive control: the Chandra token + Aether Snap chain resolves one.
#[test]
fn departure_window_work_is_zero_for_simultaneous_or_triggerless_wipes() {
    let Some(db) = shared_card_db() else {
        return;
    };
    // Trigger-less members.
    let mut scenario = main_phase();
    let plain = [
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db),
        scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db),
    ];
    let wipe = scenario.add_real_card(P0, "Day of Judgment", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 2);
    perf_counters::reset();
    runner.cast(wipe).resolve();
    for id in plain {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(
        perf_counters::departure_window_snapshot().context_resolutions,
        0
    );

    // Trigger-bearing members, all simultaneous.
    let mut scenario = main_phase();
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    let artist = scenario.add_real_card(P1, "Blood Artist", Zone::Battlefield, db);
    let wipe = scenario.add_real_card(P0, "Day of Judgment", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 2);
    perf_counters::reset();
    let mut events = runner.cast(wipe).commit().events().to_vec();
    drive(&mut runner, &mut events, P0, |r| !on_stack(r, wipe));
    assert_eq!(runner.state().objects[&warden].zone, Zone::Graveyard);
    assert_eq!(
        triggers_from(&runner, artist).len(),
        2,
        "reach: Blood Artist saw both simultaneous deaths (CR 603.10a)"
    );
    assert_eq!(
        perf_counters::departure_window_snapshot().context_resolutions,
        0
    );

    // Positive control.
    let mut scenario = main_phase();
    let chandra = scenario.add_real_card(P1, "Chandra, Fire Artisan", Zone::Battlefield, db);
    let identity = scenario.add_real_card(P0, "Fractured Identity", Zone::Hand, db);
    let aether_snap = scenario.add_real_card(P0, "Aether Snap", Zone::Hand, db);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&chandra)
        .unwrap()
        .counters
        .insert(CounterType::Loyalty, 4);
    let token = fracture(&mut runner, identity, chandra, &[], None);
    perf_counters::reset();
    snap(&mut runner, aether_snap);
    assert_eq!(triggers_from(&runner, token).len(), 1);
    assert!(perf_counters::departure_window_snapshot().context_resolutions > 0);
}

/// T10 + probe P4: P0's token copy of a Siege enters with its defense
/// counters; Aether Snap removes all of them, then exiles the token. The
/// "last defense counter removed" trigger fires once.
#[test]
#[ignore = "probe P4: a token copy of a battle enters without its defense counters (CR 310.4b)"]
fn siege_token_defeated_by_counter_removal_before_its_exile_triggers() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let siege = scenario.add_real_card(P1, "Invasion of Kaladesh", Zone::Battlefield, db);
    let identity = scenario.add_real_card(P0, "Fractured Identity", Zone::Hand, db);
    let aether_snap = scenario.add_real_card(P0, "Aether Snap", Zone::Hand, db);
    // Re-seat priority with the active player after placing the battle.
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let token = fracture(&mut runner, identity, siege, &[], Some("1"));
    assert_eq!(
        counters_on(&runner, token, CounterType::Defense),
        4,
        "probe P4: the copy entered with its four defense counters"
    );
    let events = snap(&mut runner, aether_snap);
    assert!(counter_removed_from(&events, token), "reach: instruction 1");
    assert!(
        left_battlefield_to_exile(&events, token),
        "reach: instruction 2"
    );
    assert_eq!(
        triggers_from(&runner, token).len(),
        1,
        "the last-defense-counter trigger fires once"
    );
    assert!(stacked_trigger_sources_are_live(&runner, token));
}

/// T15 (bounce): Into the Flood Maw with the gift promised. P1 creates its
/// tapped Fish first (CR 702.174j), which P1's Soul Warden sees, then the
/// spell returns Soul Warden to P1's hand.
#[test]
fn soul_warden_bounced_after_the_gift_fish_entered_saw_it() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    let maw = scenario.add_real_card(P0, "Into the Flood Maw", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Blue], 0);

    let out = runner
        .cast(maw)
        .accept_optional()
        .target_object(warden)
        .resolve();
    let _ = out;
    // The gift Fish is matched by name and controller.
    assert!(
        runner.state().battlefield.iter().any(|id| {
            let object = &runner.state().objects[id];
            object.name == "Fish" && object.controller == P1
        }),
        "reach: P1 received the gift Fish"
    );
    assert_eq!(
        runner.state().objects[&warden].zone,
        Zone::Hand,
        "reach: Soul Warden was returned"
    );
    assert_eq!(runner.life(P1), 21, "Soul Warden saw the Fish enter");
}

// ---------------------------------------------------------------------------
// D6: the departing instruction (CR 608.2c + CR 603.10). An observer that
// leaves the battlefield in an instruction does not exist immediately after
// that instruction's other events, so it sees only the look-back ones; the
// events of EARLIER instructions stay visible.
//
// More printed cards (Oracle text verbatim, data/card-data.json):
//   Reckless Spite — "Destroy two target nonblack creatures. You lose 5
//     life."
//   Diaochan, Artful Beauty — "{T}: Destroy target creature of your choice,
//     then destroy target creature of an opponent's choice. Activate only
//     during your turn, before attackers are declared."
//   Femeref Enchantress — "Whenever an enchantment is put into a graveyard
//     from the battlefield, draw a card."
//   Hyena Umbra — "Enchant creature / Enchanted creature gets +1/+1 and has
//     first strike. / Umbra armor (If enchanted creature would be destroyed,
//     instead remove all damage from it and destroy this Aura.)"
//   Gideon's Avenger — "Whenever a creature an opponent controls becomes
//     tapped, put a +1/+1 counter on this creature."
//   Lotleth Troll — "Trample / Discard a creature card: Put a +1/+1 counter
//     on this creature. / {B}: Regenerate this creature."
//   Evacuation — "Return all creatures to their owners' hands."
//   Extractor Demon — "Flying / Whenever another creature leaves the
//     battlefield, you may have target player mill two cards. / Unearth
//     {2}{B} (...)"
//   Raise the Alarm — "Create two 1/1 white Soldier creature tokens."
// ---------------------------------------------------------------------------

/// Cast `spell` and pass until it has left the stack, leaving its triggers on
/// the stack. Returns the departure-window counters of that span.
fn cast_and_settle(
    runner: &mut GameRunner,
    spell: ObjectId,
    targets: &[ObjectId],
) -> perf_counters::DepartureWindowCounters {
    perf_counters::reset();
    let mut events = runner
        .cast(spell)
        .target_objects(targets)
        .commit()
        .events()
        .to_vec();
    drive(runner, &mut events, P0, |r| !on_stack(r, spell));
    perf_counters::departure_window_snapshot()
}

/// R2 (CR 122.1c + CR 603.10): Day of Judgment would destroy P1's shielded
/// Bears and P1's Cloudsculpt Armorer together. The shield counter replaces
/// the Bears' destruction with its own removal inside that one event, and the
/// Armorer leaves in it; a counter removal does not look back in time, so the
/// Armorer does not seek.
#[test]
fn cloudsculpt_armorer_destroyed_with_a_shield_removal_in_the_same_wipe_does_not_seek() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    // The Bears first: the wipe walks the battlefield in order, so the shield
    // is removed before the Armorer leaves.
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    scenario.with_counter(bears, CounterType::Shield, 1);
    let armorer = scenario.add_real_card(P1, "Cloudsculpt Armorer", Zone::Battlefield, db);
    let wipe = scenario.add_real_card(P0, "Day of Judgment", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 2);

    let counters = cast_and_settle(&mut runner, wipe, &[]);

    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "reach: the shield counter saved the Bears"
    );
    assert_eq!(
        counters_on(&runner, bears, CounterType::Shield),
        0,
        "reach: the shield counter was removed"
    );
    assert_eq!(runner.state().objects[&armorer].zone, Zone::Graveyard);
    assert!(
        counters.in_instruction_cuts > 0,
        "reach: the Bears' counter removal was cut from the Armorer's window"
    );
    assert_eq!(counters.unstamped_departures, 0);
    assert!(
        triggers_from(&runner, armorer).is_empty(),
        "the Armorer left in the event that removed the counter"
    );
}

/// R2+ (paired positive): the Armorer shielded itself, so it survives the
/// wipe and sees its own shield counter removed. Nothing is cut.
#[test]
fn live_cloudsculpt_armorer_seeks_when_its_own_shield_is_removed_by_the_wipe() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let armorer = scenario.add_real_card(P1, "Cloudsculpt Armorer", Zone::Battlefield, db);
    scenario.with_counter(armorer, CounterType::Shield, 1);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let wipe = scenario.add_real_card(P0, "Day of Judgment", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 2);

    let counters = cast_and_settle(&mut runner, wipe, &[]);

    assert_eq!(runner.state().objects[&armorer].zone, Zone::Battlefield);
    assert_eq!(counters_on(&runner, armorer, CounterType::Shield), 0);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(counters.in_instruction_cuts, 0);
    assert_eq!(counters.unstamped_departures, 0);
    assert_eq!(
        triggers_from(&runner, armorer).len(),
        1,
        "the surviving Armorer seeks once"
    );
}

/// R3 (CR 608.2c): Reckless Spite destroys both targets in one instruction.
/// Kalitas exiles the Bears and makes a Zombie inside that event, and Soul
/// Warden leaves in the same event, so it does not see the Zombie. The life
/// loss is the next instruction.
#[test]
fn kalitas_replacement_inside_a_two_target_destroy_is_not_seen_by_the_other_victim() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let spite = scenario.add_real_card(P0, "Reckless Spite", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Black], 1);

    let counters = cast_and_settle(&mut runner, spite, &[bears, warden]);

    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&warden].zone, Zone::Exile);
    assert_eq!(tokens_named(&runner, P0, "Zombie").len(), 2);
    assert_eq!(runner.life(P0), 15, "reach: the next instruction resolved");
    assert!(counters.in_instruction_cuts > 0);
    assert_eq!(counters.unstamped_departures, 0);
    assert!(
        triggers_from(&runner, warden).is_empty(),
        "Soul Warden did not trigger"
    );
    let mut events = Vec::new();
    drive(&mut runner, &mut events, P0, |r| r.state().stack.is_empty());
    assert_eq!(
        runner.life(P1),
        20,
        "Soul Warden left in the event that created the first Zombie"
    );
}

/// Activate Diaochan, destroying `yours` first and then `theirs` (the
/// opponent's choice), and resolve it and every trigger it raises, aiming
/// trigger targets at P0. Returns the departure-window counters of that span.
fn diaochan(
    runner: &mut GameRunner,
    diaochan: ObjectId,
    yours: ObjectId,
    theirs: ObjectId,
) -> perf_counters::DepartureWindowCounters {
    perf_counters::reset();
    runner
        .activate(diaochan, 0)
        .target_objects(&[yours, theirs])
        .target_player(P0)
        .resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach: everything resolved"
    );
    perf_counters::departure_window_snapshot()
}

/// R-D1 (CR 608.2c, preservation): "..., then destroy ..." is two
/// instructions in order. The Bears are exiled and their Zombie enters while
/// Soul Warden is still on the battlefield, so it gains 1; the Warden is then
/// exiled by the second instruction and does not see its own Zombie.
#[test]
fn diaochan_then_chain_keeps_the_first_destruction_visible() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let beauty = scenario.add_real_card(P0, "Diaochan, Artful Beauty", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    let mut runner = scenario.build();

    let counters = diaochan(&mut runner, beauty, bears, warden);

    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&warden].zone, Zone::Exile);
    assert_eq!(tokens_named(&runner, P0, "Zombie").len(), 2);
    assert_eq!(
        counters.in_instruction_cuts, 0,
        "the Warden's own Zombie entered after it left"
    );
    assert_eq!(counters.unstamped_departures, 0);
    assert_eq!(
        runner.life(P1),
        21,
        "Soul Warden saw the Zombie of the earlier instruction"
    );
}

/// R-D2 (CR 608.2c, preservation): Blood Artist is destroyed by the first
/// instruction and the Bears by the second, so the Artist sees only its own
/// death: one trigger, so P1 gains 1 and P0 loses 1.
#[test]
fn diaochan_blood_artist_sees_only_its_own_death() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let beauty = scenario.add_real_card(P0, "Diaochan, Artful Beauty", Zone::Battlefield, db);
    let artist = scenario.add_real_card(P1, "Blood Artist", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();

    let counters = diaochan(&mut runner, beauty, artist, bears);

    assert_eq!(runner.state().objects[&artist].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(counters.unstamped_departures, 0);
    assert_eq!(runner.life(P0), 19, "one Blood Artist trigger resolved");
    assert_eq!(
        runner.life(P1),
        21,
        "Blood Artist had left before the Bears died"
    );
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

/// Day of Judgment on P1's Bears enchanted by Hyena Umbra and P1's Femeref
/// Enchantress, in the given battlefield order. Returns the Enchantress's
/// draws once every trigger has resolved.
fn femeref_umbra_wipe(enchantress_first: bool) -> Option<usize> {
    let db = shared_card_db()?;
    let mut scenario = main_phase();
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    // Placed while the Bears are the only creature, so it enchants them.
    let umbra = scenario.add_real_card(P1, "Hyena Umbra", Zone::Battlefield, db);
    let femeref = scenario.add_real_card(P1, "Femeref Enchantress", Zone::Battlefield, db);
    let wipe = scenario.add_real_card(P0, "Day of Judgment", Zone::Hand, db);
    let mut runner = scenario.build();
    if enchantress_first {
        // The wipe walks the battlefield in order: destroy the Enchantress
        // before the enchanted Bears.
        let battlefield = &mut runner.state_mut().battlefield;
        battlefield.retain(|id| *id != femeref);
        battlefield.push_front(femeref);
    }
    assert_eq!(
        runner.state().objects[&umbra]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(bears),
        "reach: Hyena Umbra enchants the Bears"
    );
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 2);
    let hand_before = hand_size(&runner, P1);

    let counters = cast_and_settle(&mut runner, wipe, &[]);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "reach: umbra armor saved the Bears"
    );
    assert_eq!(runner.state().objects[&umbra].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&femeref].zone, Zone::Graveyard);
    assert_eq!(counters.unstamped_departures, 0);
    let mut events = Vec::new();
    drive(&mut runner, &mut events, P0, |r| r.state().stack.is_empty());
    Some(hand_size(&runner, P1) - hand_before)
}

/// R5 (CR 603.10a + CR 702.89a): umbra armor destroys Hyena Umbra inside the
/// wipe that destroys Femeref Enchantress. A battlefield exit looks back in
/// time, so the Enchantress draws once.
#[test]
fn femeref_enchantress_sees_the_umbra_aura_destroyed_by_the_same_wipe() {
    let Some(draws) = femeref_umbra_wipe(false) else {
        return;
    };
    assert_eq!(draws, 1, "the Enchantress saw the Aura go to the graveyard");
}

/// R5b: the same with Femeref Enchantress BEFORE the enchanted Bears, so the
/// Aura leaves after the Enchantress within the one event (CR 603.10a: they
/// left simultaneously).
#[test]
fn femeref_enchantress_destroyed_first_sees_the_umbra_aura_of_the_same_wipe() {
    let Some(draws) = femeref_umbra_wipe(true) else {
        return;
    };
    assert_eq!(draws, 1, "the Enchantress saw the Aura go to the graveyard");
}

/// R7 (CR 701.19a, preservation): P0's Lotleth Troll regenerates inside the
/// wipe — regeneration taps it — and P1's Gideon's Avenger leaves in the same
/// event. A tap does not look back in time, so the Avenger does not trigger.
/// The engine taps a regenerated permanent without publishing a tap event
/// (`GameEvent::Regenerated` keys nothing), so no event reaches the
/// Avenger's window and nothing is cut; this row pins the outcome only.
#[test]
fn gideons_avenger_destroyed_with_a_regenerating_troll_does_not_see_the_tap() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    // The Troll first, so its regeneration happens before the Avenger leaves.
    let troll = scenario.add_real_card(P0, "Lotleth Troll", Zone::Battlefield, db);
    let avenger = scenario.add_real_card(P1, "Gideon's Avenger", Zone::Battlefield, db);
    let wipe = scenario.add_real_card(P0, "Day of Judgment", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Black], 0);
    let regenerate = runner.state().objects[&troll]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::Regenerate { .. }))
        .expect("Lotleth Troll has a Regenerate ability");
    runner.activate(troll, regenerate).resolve();
    add_mana(&mut runner, P0, &[ManaType::White, ManaType::White], 2);

    let counters = cast_and_settle(&mut runner, wipe, &[]);

    let troll_now = &runner.state().objects[&troll];
    assert_eq!(
        troll_now.zone,
        Zone::Battlefield,
        "reach: the Troll regenerated"
    );
    assert!(troll_now.tapped, "reach: regeneration tapped the Troll");
    assert_eq!(runner.state().objects[&avenger].zone, Zone::Graveyard);
    assert_eq!(counters.unstamped_departures, 0);
    assert!(
        triggers_from(&runner, avenger).is_empty(),
        "the Avenger left in the event that tapped the Troll"
    );
}

/// PH (a) (CR 603.10a): Evacuation returns the Bears and Extractor Demon
/// together; the Demon sees the Bears leave (a leaves-the-battlefield trigger
/// looks back in time) and the departure is collected exactly once.
#[test]
fn mass_bounce_with_a_departing_leaves_observer_collects_once() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let demon = scenario.add_real_card(P0, "Extractor Demon", Zone::Battlefield, db);
    let evacuation = scenario.add_real_card(P0, "Evacuation", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Blue, ManaType::Blue], 3);

    let counters = cast_and_settle(&mut runner, evacuation, &[]);

    assert_eq!(runner.state().objects[&bears].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&demon].zone, Zone::Hand);
    assert_eq!(counters.unstamped_departures, 0);
    assert_eq!(
        triggers_from(&runner, demon).len(),
        1,
        "one trigger for the Bears"
    );
}

/// PH (b) (CR 603.10a): Aether Snap exiles two creature tokens; the live
/// Extractor Demon triggers once for each.
#[test]
#[ignore = "pre-existing double collection: the live Demon triggers twice per exiled token (4); measured 4 at upstream main d7a224173 (follow-up C)"]
fn mass_exile_of_tokens_with_a_live_leaves_observer_collects_once() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let demon = scenario.add_real_card(P0, "Extractor Demon", Zone::Battlefield, db);
    let alarm = scenario.add_real_card(P0, "Raise the Alarm", Zone::Hand, db);
    let aether_snap = scenario.add_real_card(P0, "Aether Snap", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White], 1);
    runner.cast(alarm).resolve();
    let tokens = tokens_named(&runner, P0, "Soldier");
    assert_eq!(tokens.len(), 2, "reach: Raise the Alarm made two Soldiers");

    perf_counters::reset();
    let events = snap(&mut runner, aether_snap);

    for id in tokens {
        assert!(
            left_battlefield_to_exile(&events, id),
            "reach: token exiled"
        );
    }
    assert_eq!(runner.state().objects[&demon].zone, Zone::Battlefield);
    assert_eq!(
        perf_counters::departure_window_snapshot().unstamped_departures,
        0
    );
    assert_eq!(
        triggers_from(&runner, demon).len(),
        2,
        "one trigger per exiled token"
    );
}

// ---------------------------------------------------------------------------
// One instruction across a target list, and across a choice completion.
//
// More printed cards (Oracle text verbatim, data/card-data.json):
//   Decimate — "Destroy target artifact, target creature, target enchantment,
//     and target land. (You can't cast this spell unless you have legal
//     choices for all its targets.)"
//   Propaganda — "Creatures can't attack you unless their controller pays {2}
//     for each creature they control that's attacking you."
//   Sol Ring — "{T}: Add {C}{C}."
//   Dryad Arbor — a Land Creature — Forest Dryad.
//   Ornithopter — an Artifact Creature — Thopter with "Flying".
//   Barter in Blood — "Each player sacrifices two creatures of their choice."
// ---------------------------------------------------------------------------

/// R4 (CR 608.2c + CR 603.10): Decimate's one sentence is one instruction,
/// lowered as four chained `Destroy` list slots. Kalitas exiles the
/// Ornithopter (artifact slot) instead and makes a Zombie; Soul Warden
/// (creature slot) is exiled by the same instruction, so it does not exist
/// immediately after that Zombie enters and does not trigger.
#[test]
fn kalitas_inside_decimate_is_not_seen_by_the_creature_destroyed_in_the_same_sentence() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let ornithopter = scenario.add_real_card(P1, "Ornithopter", Zone::Battlefield, db);
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    let enchantment = scenario.add_real_card(P1, "Propaganda", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let decimate = scenario.add_real_card(P0, "Decimate", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Red, ManaType::Green], 2);

    let counters = cast_and_settle(
        &mut runner,
        decimate,
        &[ornithopter, warden, enchantment, forest],
    );

    assert_eq!(
        runner.state().objects[&ornithopter].zone,
        Zone::Exile,
        "reach: Kalitas exiled the Ornithopter instead"
    );
    assert_eq!(
        runner.state().objects[&warden].zone,
        Zone::Exile,
        "reach: Kalitas exiled Soul Warden instead"
    );
    assert_eq!(runner.state().objects[&enchantment].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&forest].zone, Zone::Graveyard);
    assert_eq!(
        tokens_named(&runner, P0, "Zombie").len(),
        2,
        "reach: each replaced death created a Zombie"
    );
    assert!(
        counters.in_instruction_cuts > 0,
        "reach: the Ornithopter's Zombie entry was cut from Soul Warden's window"
    );
    assert_eq!(counters.unstamped_departures, 0);
    assert!(
        triggers_from(&runner, warden).is_empty(),
        "Soul Warden did not trigger"
    );
    let mut events = Vec::new();
    drive(&mut runner, &mut events, P0, |r| r.state().stack.is_empty());
    assert_eq!(
        runner.life(P1),
        20,
        "Soul Warden left in the instruction that created the first Zombie"
    );
}

/// R4b (CR 603.10a): Blood Artist (creature slot) dies in the same instruction
/// as Dryad Arbor (land slot), so it sees that death as well as its own.
#[test]
fn decimate_blood_artist_sees_a_later_slot_death_in_the_same_sentence() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let artifact = scenario.add_real_card(P1, "Sol Ring", Zone::Battlefield, db);
    let artist = scenario.add_real_card(P1, "Blood Artist", Zone::Battlefield, db);
    let enchantment = scenario.add_real_card(P1, "Propaganda", Zone::Battlefield, db);
    let arbor = scenario.add_real_card(P1, "Dryad Arbor", Zone::Battlefield, db);
    let decimate = scenario.add_real_card(P0, "Decimate", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Red, ManaType::Green], 2);

    let counters = cast_and_settle(
        &mut runner,
        decimate,
        &[artifact, artist, enchantment, arbor],
    );

    for id in [artifact, artist, enchantment, arbor] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Graveyard,
            "reach: every slot was destroyed"
        );
    }
    assert_eq!(counters.unstamped_departures, 0);
    assert_eq!(
        triggers_from(&runner, artist).len(),
        2,
        "Blood Artist saw itself and Dryad Arbor die"
    );
}

/// Cast `spell` and pass until it has left the stack, answering each player's
/// sacrifice choice with `picks` and trigger targets with P0, leaving the
/// triggers on the stack. Returns the departure-window counters of that span.
fn settle_with_sacrifice_picks(
    runner: &mut GameRunner,
    spell: ObjectId,
    targets: &[ObjectId],
    picks: &[(PlayerId, Vec<ObjectId>)],
) -> perf_counters::DepartureWindowCounters {
    perf_counters::reset();
    let _ = runner.cast(spell).target_objects(targets).commit();
    for _ in 0..128 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && !on_stack(runner, spell)
        {
            return perf_counters::departure_window_snapshot();
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { player, .. } => GameAction::SelectCards {
                cards: picks
                    .iter()
                    .find(|(chooser, _)| *chooser == player)
                    .map(|(_, cards)| cards.clone())
                    .expect("a pick for the choosing player"),
            },
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                let wanted = TargetRef::Player(P0);
                GameAction::ChooseTarget {
                    target: Some(if legal.contains(&wanted) {
                        wanted
                    } else {
                        legal.first().cloned().expect("a legal trigger target")
                    }),
                }
            }
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("driver stopped at an unexpected prompt: {other:?}"),
        };
        runner
            .act(action)
            .expect("the engine must accept the action");
    }
    panic!("the spell never left the stack");
}

/// R8 (CR 701.21a + CR 603.10): Barter in Blood's sacrifices happen at once
/// after every player chose (its ruling: "Then all creatures are sacrificed
/// simultaneously"). Kalitas exiles P1's Bears and Soul Warden instead and
/// makes two Zombies inside that one event, so Soul Warden does not exist
/// immediately after either Zombie enters. The choice completion emits its
/// departures outside an instruction scope; the departure window fails closed
/// for them (counted as unstamped) instead of admitting the Zombies.
#[test]
fn barter_in_blood_choice_completion_is_one_instruction_for_kalitas_zombies() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let warden = scenario.add_real_card(P1, "Soul Warden", Zone::Battlefield, db);
    let ornithopter = scenario.add_real_card(P1, "Ornithopter", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let fodder_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let fodder_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let barter = scenario.add_real_card(P0, "Barter in Blood", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Black, ManaType::Black], 2);

    let counters = settle_with_sacrifice_picks(
        &mut runner,
        barter,
        &[],
        &[(P0, vec![fodder_a, fodder_b]), (P1, vec![bears, warden])],
    );

    assert_eq!(runner.state().objects[&fodder_a].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&fodder_b].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Exile,
        "reach: Kalitas exiled P1's Bears instead"
    );
    assert_eq!(
        runner.state().objects[&warden].zone,
        Zone::Exile,
        "reach: Kalitas exiled Soul Warden instead"
    );
    assert_eq!(
        runner.state().objects[&ornithopter].zone,
        Zone::Battlefield,
        "reach: P1 kept the creature it did not choose"
    );
    assert_eq!(
        tokens_named(&runner, P0, "Zombie").len(),
        2,
        "reach: each replaced death created a Zombie"
    );
    assert!(
        counters.unstamped_departures > 0,
        "reach: the choice completion's departures carry no instruction lead"
    );
    assert!(
        triggers_from(&runner, warden).is_empty(),
        "Soul Warden did not trigger"
    );
    let mut events = Vec::new();
    drive(&mut runner, &mut events, P0, |r| r.state().stack.is_empty());
    assert_eq!(
        runner.life(P1),
        20,
        "Soul Warden left in the event that created the Zombies"
    );
}

/// CR 608.2c + CR 603.10: Decimate's slots are one instruction even when a slot
/// pauses for a replacement choice (CR 616.1). Rest in Peace and Kalitas both
/// replace the Bears' death; P1 answers, Kalitas exiles it and a Zombie enters,
/// and Carnival of Souls ("Whenever a creature enters, you lose 1 life and add
/// {B}.") is destroyed by the same sentence, so it does not see the Zombie.
/// Preservation row: a list slot that cannot prove its sibling after a pause
/// fails closed (`InstructionWatch::Unwatched`) instead of opening a later
/// instruction start. MEASURED: this board passes with and without that
/// fallback (the resumed slots still find their siblings), so it guards the
/// end result and does not isolate the fallback; the reviewer's derivation of a
/// board that does was not reproduced.
#[test]
fn decimate_slot_after_a_replacement_choice_does_not_see_the_earlier_slots_zombie() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let sol_ring = scenario.add_real_card(P1, "Sol Ring", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let carnival = scenario.add_real_card(P1, "Carnival of Souls", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P1, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Kalitas, Traitor of Ghet", Zone::Battlefield, db);
    let decimate = scenario.add_real_card(P0, "Decimate", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Red, ManaType::Green], 2);

    settle_with_sacrifice_picks(
        &mut runner,
        decimate,
        &[sol_ring, bears, carnival, forest],
        &[],
    );

    assert_eq!(
        runner.state().objects[&carnival].zone,
        Zone::Exile,
        "reach: Rest in Peace exiled the destroyed enchantment"
    );
    assert_eq!(runner.state().objects[&forest].zone, Zone::Exile);
    assert_eq!(
        tokens_named(&runner, P0, "Zombie").len(),
        1,
        "reach: Kalitas replaced the Bears' death (P1 chose it at the prompt)"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "Carnival of Souls left in the event that created the Zombie"
    );
}
