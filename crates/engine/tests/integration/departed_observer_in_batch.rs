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
use engine::types::ability::TargetRef;
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
#[ignore = "D6 blocker: the departure window admits events of the same simultaneous instruction"]
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

    runner.cast(wipe).resolve();

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
