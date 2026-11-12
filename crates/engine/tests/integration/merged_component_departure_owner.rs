//! CR 730.3 + CR 603.2c + CR 616.1: a merged permanent's departure belongs to
//! the logical owner that announced it. When a component's own replacement
//! choice parks the split, that owner — the batch of a library placement or the
//! ChangeZone iteration of an owner shuffle — still collects the departure once,
//! records its terminal outcome once, and resumes its own tail, whether the
//! choice is answered or its chooser concedes.
//!
//! Oracle text (card data):
//!   Circuit Mender: "When this creature enters, you gain 2 life. / When this
//!    creature leaves the battlefield, draw a card."
//!   Brokkos, Apex of Forever: "Mutate {2}{U/B}{G}{G} (...) / Trample / You may
//!    cast this card from your graveyard using its mutate ability."
//!   Totally Lost: "Put target nonland permanent on top of its owner's library."
//!   Chaos Warp: "The owner of target permanent shuffles it into their library,
//!    then reveals the top card of their library. If it's a permanent card, they
//!    put it onto the battlefield."
//!   Rebuking Ceremony: "Put two target artifacts on top of their owners'
//!    libraries."
//!   Kederekt Leviathan: "When this creature enters, return all other nonland
//!    permanents to their owners' hands. / Unearth {6}{U} (...)"
//!   Dryad Arbor: a 1/1 Land Creature — Forest Dryad.
//!   Lord of Extinction: "Lord of Extinction's power and toughness are each
//!    equal to the number of cards in all graveyards."
//!   Platinum Angel: "Flying / You can't lose the game and your opponents can't
//!    win the game."
//!   Super Shredder: "Menace / Whenever another permanent leaves the
//!    battlefield, put a +1/+1 counter on Super Shredder."
//!   Benalish Marshal: "Other creatures you control get +1/+1."
//!   Unsummon: "Return target creature to its owner's hand."
//!   Collision of Realms: "Each player shuffles all creatures they own into
//!    their library. Each player who shuffled a nontoken creature into their
//!    library this way reveals cards from the top of their library until they
//!    reveal a creature card, then puts that card onto the battlefield and the
//!    rest on the bottom of their library in a random order." (The parser does not
//!    support the reveal and bottom clauses, so only the shuffle half is
//!    asserted, and the spell's own final zone is not.)
//!   Kor Skyfisher: "Flying / When this creature enters, return a permanent you
//!    control to its owner's hand."
//!   Ninth Bridge Patrol: "Whenever another creature you control leaves the
//!    battlefield, put a +1/+1 counter on this creature."
//!   Karn, Silver Golem and Traxos, Scourge of Kroog: legendary artifact
//!    creatures, each its owner's commander in the two-commander row.
//!
//! The mutate itself is staged with `merge_object_onto` (mutate casting is not
//! under test). Known defect, not under test and not rules-correct: when
//! Circuit Mender is on top of Brokkos as the survivor, the merged permanent has
//! Circuit Mender's abilities (CR 730.2a) and its leaves-the-battlefield ability
//! should look back and trigger (CR 603.10a), but it does not trigger even with
//! nothing parked. For that pile the rows compare the parked run with the
//! unparked one and use Ninth Bridge Patrol to observe the departure. Brokkos is a legendary creature and the designated commander
//! (CR 903.3); CR 903.9b offers its owner the command zone when it would be put
//! into a library.

use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::format::FormatConfig;
use engine::types::game_state::{ActionResult, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);

/// Which card the merged permanent keeps as its survivor (the object that stays
/// on the battlefield, CR 730.2c). Components are delivered topmost first.
#[derive(Clone, Copy, Debug)]
enum Pile {
    /// Circuit Mender is the survivor and the topmost component; Brokkos was
    /// mutated under it. Circuit Mender is delivered, then Brokkos's prompt opens.
    BrokkosUnderMender,
    /// Brokkos is the survivor with Circuit Mender mutated on top. Circuit
    /// Mender is delivered, then the prompt opens on the survivor's own
    /// incarnation — the same object the original owner announced.
    MenderOverBrokkos,
}

struct MergedCommander {
    survivor: ObjectId,
    mender: ObjectId,
    brokkos: ObjectId,
}

fn mana(color: ManaType, amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn add_pile(scenario: &mut GameScenario, owner: PlayerId) -> (ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mender = scenario.add_real_card(owner, "Circuit Mender", Zone::Battlefield, db);
    let brokkos = scenario.add_real_card(owner, "Brokkos, Apex of Forever", Zone::Battlefield, db);
    (mender, brokkos)
}

/// CR 702.140c + CR 730.2: stage the mutate and the commander designation.
fn merge_pile(
    runner: &mut GameRunner,
    pile: Pile,
    mender: ObjectId,
    brokkos: ObjectId,
    commander: bool,
) -> MergedCommander {
    let state = runner.state_mut();
    let mut events = Vec::new();
    let (rider, survivor, side) = match pile {
        Pile::BrokkosUnderMender => (brokkos, mender, MergeSide::Bottom),
        Pile::MenderOverBrokkos => (mender, brokkos, MergeSide::Top),
    };
    merge_object_onto(state, rider, survivor, side, &mut events);
    state.battlefield.retain(|&id| id != rider);
    state
        .objects
        .get_mut(&brokkos)
        .expect("Brokkos exists")
        .is_commander = commander;
    assert_eq!(
        state.objects[&survivor].merged_components,
        vec![mender, brokkos],
        "CR 730.2a: Circuit Mender is the topmost component"
    );
    MergedCommander {
        survivor,
        mender,
        brokkos,
    }
}

/// Commit `spell` and pass priority until it resolves or its resolution asks a
/// question.
fn cast_until_resolved_or_asked(
    runner: &mut GameRunner,
    spell: ObjectId,
    targets: &[ObjectId],
) -> Vec<GameEvent> {
    let mut events = runner
        .cast(spell)
        .target_objects(targets)
        .commit()
        .events()
        .to_vec();
    for _ in 0..6 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || !runner.state().stack.iter().any(|entry| entry.id == spell)
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("priority passes while the spell resolves")
                .events,
        );
    }
    events
}

/// The CR 903.9b prompt opened by Brokkos's own component delivery.
fn assert_commander_prompt(state: &GameState, chooser: PlayerId, brokkos: ObjectId) {
    assert!(
        matches!(
            &state.waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if *player == chooser
        ),
        "{:?}",
        state.waiting_for
    );
    assert_eq!(
        state
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.proposed.affected_object_id()),
        Some(brokkos)
    );
}

fn answer(runner: &mut GameRunner, index: usize) -> Vec<GameEvent> {
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the parked replacement choice is answered")
        .events
}

/// Serialize and restore the whole game at the parked prompt.
fn save_and_restore(runner: &mut GameRunner) {
    let wire = serde_json::to_value(runner.state()).expect("parked prompt serializes");
    let mut restored: GameState = serde_json::from_value(wire).expect("parked prompt restores");
    restored.rehydrate_rng();
    *runner.state_mut() = restored;
}

/// The triggered abilities waiting to be put on the stack, as (controller,
/// Oracle description): those already on the stack plus every controller's
/// group in the APNAP ordering pass (CR 603.3b).
fn waiting_triggers(state: &GameState) -> Vec<(PlayerId, String)> {
    let mut waiting: Vec<(PlayerId, String)> = state
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                description: Some(text),
                ..
            } => Some((entry.controller, text.clone())),
            _ => None,
        })
        .collect();
    if let Some(order) = &state.pending_trigger_order {
        waiting.extend(order.groups.iter().flat_map(|group| {
            group.triggers.iter().filter_map(|trigger| {
                trigger
                    .pending
                    .description
                    .clone()
                    .map(|text| (group.controller, text))
            })
        }));
    }
    waiting
}

/// The controllers of Circuit Mender's leaves-the-battlefield triggers.
fn mender_departure_triggers(state: &GameState) -> Vec<PlayerId> {
    waiting_triggers(state)
        .into_iter()
        .filter(|(_, text)| text.contains("leaves the battlefield, draw a card"))
        .map(|(controller, _)| controller)
        .collect()
}

/// Super Shredder's leave triggers waiting to be put on the stack: one per
/// other permanent that left the battlefield.
fn shredder_triggers(state: &GameState) -> usize {
    waiting_triggers(state)
        .into_iter()
        .filter(|(_, text)| text.contains("another permanent leaves"))
        .count()
}

/// The controllers of Ninth Bridge Patrol triggers: each one observed one
/// creature its controller controlled leaving the battlefield.
fn patrol_triggers(state: &GameState) -> Vec<PlayerId> {
    waiting_triggers(state)
        .into_iter()
        .filter(|(_, text)| text.contains("another creature you control leaves"))
        .map(|(controller, _)| controller)
        .collect()
}

/// The resolution is over: priority, or the ordering of the abilities it
/// triggered (CR 603.3b), with nothing of it left parked.
fn resolution_finished(state: &GameState) {
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
        ),
        "{:?}",
        state.waiting_for
    );
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "no parked zone-change owner or component child survives the resolution"
    );
}

/// CR 730.3 + CR 603.2c: Totally Lost puts the merged permanent on top of its
/// owner's library through one batch. Circuit Mender's ability triggers once
/// for the one permanent that left — whether the split runs straight through
/// (Brokkos is not a commander) or Brokkos's CR 903.9b prompt parks it and is
/// answered either way.
#[test]
fn totally_lost_collects_the_merged_departure_once_for_each_commander_answer() {
    for answer_index in [None, Some(0), Some(1)] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P0);
        let lost = scenario.add_real_card(P0, "Totally Lost", Zone::Hand, db);
        scenario.with_library_top(P0, &["Library Card A", "Library Card B"]);
        scenario.with_mana_pool(P0, mana(ManaType::Blue, 5));
        let mut runner = scenario.build();
        let pile = merge_pile(
            &mut runner,
            Pile::BrokkosUnderMender,
            mender,
            brokkos,
            answer_index.is_some(),
        );

        cast_until_resolved_or_asked(&mut runner, lost, &[pile.survivor]);
        let brokkos_zone = match answer_index {
            None => Zone::Library,
            Some(index) => {
                assert_commander_prompt(runner.state(), P0, pile.brokkos);
                answer(&mut runner, index);
                if index == 0 {
                    Zone::Command
                } else {
                    Zone::Library
                }
            }
        };

        let label = format!("answer {answer_index:?}");
        resolution_finished(runner.state());
        assert_eq!(
            mender_departure_triggers(runner.state()),
            vec![P0],
            "{label}: one merged permanent left, so its ability triggers once"
        );
        assert_eq!(runner.state().objects[&pile.mender].zone, Zone::Library);
        assert_eq!(runner.state().objects[&pile.brokkos].zone, brokkos_zone);
    }
}

/// The Circuit Mender departure triggers `spell` produces on the same pile when
/// Brokkos is not a commander, so nothing parks the split: the synchronous
/// control the parked runs must match.
fn unparked_control_triggers(spell_name: &str, pile_shape: Pile) -> Vec<PlayerId> {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let (mender, brokkos) = add_pile(&mut scenario, P1);
    scenario.add_real_card(P1, "Ninth Bridge Patrol", Zone::Battlefield, db);
    let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);
    scenario.with_library_top(P1, &["Library Card A", "Library Card B"]);
    let mut pool = mana(ManaType::Blue, 5);
    pool.extend(mana(ManaType::Red, 3));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, false);
    cast_until_resolved_or_asked(&mut runner, spell, &[pile.survivor]);
    resolution_finished(runner.state());
    assert_eq!(
        patrol_triggers(runner.state()),
        vec![P1],
        "control: Ninth Bridge Patrol observes the one merged departure once"
    );
    mender_departure_triggers(runner.state())
}

/// The unparked control. With Circuit Mender as the survivor its own trigger
/// fires once. With Brokkos as the survivor it is not pinned: it is missing
/// because of the known look-back defect in the module note, and the rows only
/// require the parked run to match it.
fn assert_control(spell_name: &str, pile_shape: Pile, control: &[PlayerId]) {
    if matches!(pile_shape, Pile::BrokkosUnderMender) {
        assert_eq!(control, [P1], "{spell_name} {pile_shape:?} control");
    }
}

/// CR 730.3 + CR 701.24a + CR 608.2c: Chaos Warp's owner shuffle runs through a
/// ChangeZone iteration of the owner. Brokkos's CR 903.9b prompt parks the
/// split; the iteration keeps the original permanent's boundary, so after either
/// answer (and a save/restore at the prompt) the owner records the departure,
/// collects it exactly as often as the unparked control does, and only then
/// shuffles and reveals. This holds both when the prompt opens on a
/// non-survivor component and when it opens on the survivor's own incarnation.
#[test]
fn chaos_warp_hands_a_parked_component_back_to_the_owner_iteration() {
    for pile_shape in [Pile::BrokkosUnderMender, Pile::MenderOverBrokkos] {
        let control = unparked_control_triggers("Chaos Warp", pile_shape);
        assert_control("Chaos Warp", pile_shape, &control);
        for answer_index in [0, 1] {
            let db = shared_card_db().expect("integration card fixture must load");
            let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 7);
            scenario.at_phase(Phase::PreCombatMain);
            let (mender, brokkos) = add_pile(&mut scenario, P1);
            scenario.add_real_card(P1, "Ninth Bridge Patrol", Zone::Battlefield, db);
            let warp = scenario.add_real_card(P0, "Chaos Warp", Zone::Hand, db);
            scenario.with_library_top(P1, &["Library Card A", "Library Card B"]);
            scenario.with_mana_pool(P0, mana(ManaType::Red, 3));
            let mut runner = scenario.build();
            let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
            let label = format!("{pile_shape:?}, answer {answer_index}");

            let mut events = cast_until_resolved_or_asked(&mut runner, warp, &[pile.survivor]);
            assert_commander_prompt(runner.state(), P1, pile.brokkos);
            save_and_restore(&mut runner);
            events.extend(answer(&mut runner, answer_index));

            resolution_finished(runner.state());
            let shuffle = events
                .iter()
                .position(|event| {
                    matches!(
                        event,
                        GameEvent::PlayerPerformedAction {
                            player_id: P1,
                            action: PlayerActionKind::ShuffledLibrary,
                            ..
                        }
                    )
                })
                .unwrap_or_else(|| panic!("{label}: the owner shuffles"));
            let reveal = events
                .iter()
                .position(|event| matches!(event, GameEvent::CardsRevealed { player: P1, .. }))
                .unwrap_or_else(|| panic!("{label}: the owner reveals"));
            assert!(shuffle < reveal, "{label}: shuffle, then reveal");
            let brokkos_destination = if answer_index == 0 {
                Zone::Command
            } else {
                Zone::Library
            };
            for (component, destination) in [
                (pile.mender, Zone::Library),
                (pile.brokkos, brokkos_destination),
            ] {
                let arrival = events
                    .iter()
                    .position(|event| {
                        matches!(
                            event,
                            GameEvent::ZoneChanged { object_id, to, .. }
                                if *object_id == component && *to == destination
                        )
                    })
                    .unwrap_or_else(|| panic!("{label}: {component:?} reaches {destination:?}"));
                assert!(
                    arrival < shuffle,
                    "{label}: the owner records the departure before it shuffles"
                );
            }
            let revealed = events
                .iter()
                .find_map(|event| match event {
                    GameEvent::CardsRevealed {
                        player: P1,
                        card_ids,
                        ..
                    } => card_ids.first().copied(),
                    _ => None,
                })
                .expect("one card is revealed");
            // The revealed card is the shuffled library's random top card:
            // a permanent card (either component) is put onto the battlefield,
            // a sentinel (no card type) stays on top of the library.
            let expected_zone = if revealed == pile.mender || revealed == pile.brokkos {
                Zone::Battlefield
            } else {
                Zone::Library
            };
            assert_eq!(
                runner.state().objects[&revealed].zone,
                expected_zone,
                "{label}: the revealed card goes where Chaos Warp sends it"
            );
            assert_eq!(
                mender_departure_triggers(runner.state()),
                control,
                "{label}: the parked departure is collected exactly as the unparked one"
            );
            assert_eq!(
                patrol_triggers(runner.state()),
                vec![P1],
                "{label}: Ninth Bridge Patrol observes the one merged departure once"
            );
        }
    }
}

/// CR 800.4a + CR 117.2e + CR 730.3: Rebuking Ceremony targets a surviving
/// artifact and P1's merged artifact commander; the merged one is placed first.
/// Brokkos's CR 903.9b prompt parks the split, and P1 — the commander's owner
/// and the prompt's chooser — concedes. P1's objects leave the game, including
/// the parked component, and within that concession the ceremony finishes: the
/// surviving artifact goes on top of its owner's library before any player has
/// priority. Both pile shapes; in `MenderOverBrokkos` the departure also cancels
/// the original permanent's own pending member.
#[test]
fn rebuking_ceremony_concession_during_the_prompt_places_the_surviving_artifact_first() {
    for pile_shape in [Pile::BrokkosUnderMender, Pile::MenderOverBrokkos] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        let surviving = scenario.add_real_card(P2, "Circuit Mender", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
        let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
        scenario.with_library_top(P2, &["Library Card A"]);
        scenario.with_library_top(P1, &["Library Card B"]);
        scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
        let label = format!("{pile_shape:?}");

        // Rebuking Ceremony's top-of-library placement delivers its targets
        // last first, so the merged permanent (second target) moves before the
        // surviving artifact.
        cast_until_resolved_or_asked(&mut runner, ceremony, &[surviving, pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        assert_eq!(runner.state().objects[&surviving].zone, Zone::Battlefield);
        save_and_restore(&mut runner);

        let conceded = engine::game::engine::apply(
            runner.state_mut(),
            P1,
            GameAction::Concede { player_id: P1 },
        )
        .expect("a player may concede at any time");

        assert!(
            matches!(conceded.waiting_for, WaitingFor::Priority { player: P0 }),
            "{label}: {:?}",
            conceded.waiting_for
        );
        resolution_finished(runner.state());
        assert_eq!(
            runner.state().players[2].library.front(),
            Some(&surviving),
            "{label}: the surviving artifact is on top of its owner's library"
        );
        assert!(
            conceded.events.iter().any(|event| matches!(
                *event,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Library, .. }
                    if object_id == surviving
            )),
            "{label}: the ceremony's own placement moved it in the concession"
        );
        assert_eq!(runner.state().objects[&ceremony].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().objects[&pile.brokkos].zone,
            Zone::Exile,
            "{label}: CR 800.4a: the parked component leaves the game with its owner"
        );
        assert_eq!(
            mender_departure_triggers(runner.state()),
            vec![P2],
            "{label}: only the surviving artifact's ability waits on the stack"
        );
        assert_eq!(
            shredder_triggers(runner.state()),
            2,
            "{label}: Super Shredder saw the merged permanent and the surviving artifact leave, once each"
        );
    }
}

fn concede(runner: &mut GameRunner, player: PlayerId) -> ActionResult {
    engine::game::engine::apply(
        runner.state_mut(),
        player,
        GameAction::Concede { player_id: player },
    )
    .expect("a player may concede at any time")
}

/// CR 800.4a + CR 117.2e + CR 903.9b: the same concession with no merged
/// permanent. Rebuking Ceremony targets Traxos, Scourge of Kroog (P2's
/// commander) and Karn, Silver Golem (P1's commander); Karn moves first and its
/// owner's command-zone choice parks the batch. P1 concedes: Karn leaves the
/// game, and the resumed batch reaches Traxos, whose owner's choice parks it
/// again — the concession hands that prompt to P2 and keeps its own departure
/// events, and P0's Super Shredder trigger from Karn's departure waits for the
/// resolution. Answering it finishes the ceremony and puts each leave trigger on
/// the stack once.
#[test]
fn concession_resume_that_parks_again_opens_the_next_commander_prompt() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let karn = scenario.add_real_card(P1, "Karn, Silver Golem", Zone::Battlefield, db);
    let traxos = scenario.add_real_card(P2, "Traxos, Scourge of Kroog", Zone::Battlefield, db);
    let shredder = scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
    let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
    scenario.with_library_top(P2, &["Library Card A"]);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();
    for commander in [karn, traxos] {
        runner
            .state_mut()
            .objects
            .get_mut(&commander)
            .expect("commander exists")
            .is_commander = true;
    }

    cast_until_resolved_or_asked(&mut runner, ceremony, &[traxos, karn]);
    assert_commander_prompt(runner.state(), P1, karn);

    let conceded = concede(&mut runner, P1);
    assert!(
        matches!(
            conceded.waiting_for,
            WaitingFor::ReplacementChoice { player: P2, .. }
        ),
        "{:?}",
        conceded.waiting_for
    );
    assert_commander_prompt(runner.state(), P2, traxos);
    assert_eq!(runner.state().objects[&karn].zone, Zone::Exile);
    assert!(
        conceded.events.iter().any(|event| matches!(
            *event,
            GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. } if object_id == karn
        )),
        "the concession keeps its own departure events"
    );

    assert!(
        runner.state().stack.is_empty(),
        "the departure's trigger waits until the resolution finishes"
    );

    answer(&mut runner, 1);
    resolution_finished(runner.state());
    let shredder_triggers = waiting_triggers(runner.state())
        .into_iter()
        .filter(|(_, text)| text.contains("another permanent leaves"))
        .count();
    assert_eq!(
        shredder_triggers, 2,
        "Super Shredder saw Karn leave the game and Traxos go to its library, once each"
    );
    assert_eq!(runner.state().objects[&shredder].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[2].library.front(), Some(&traxos));
    assert_eq!(runner.state().objects[&ceremony].zone, Zone::Graveyard);
}

/// CR 117.5 + CR 704.3 + CR 704.5g: a concession whose resume finishes the
/// resolution of a triggered ability (which owes no spell final part, so no
/// later boundary checks them) performs state-based actions before any player
/// receives priority. Kederekt Leviathan's
/// enters trigger returns all other nonland permanents; Karn, Silver Golem (P1's
/// commander) moves first and parks on P1's command-zone choice, and P1
/// concedes. The resumed trigger returns Benalish Marshal, and P2's Dryad Arbor
/// (a land, so it stays) is left a 1/1 with 1 damage and is destroyed before
/// the concession hands out priority.
#[test]
fn concession_resume_of_a_triggered_ability_performs_state_based_actions() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let karn = scenario.add_real_card(P1, "Karn, Silver Golem", Zone::Battlefield, db);
    let marshal = scenario.add_real_card(P2, "Benalish Marshal", Zone::Battlefield, db);
    let arbor = scenario.add_real_card(P2, "Dryad Arbor", Zone::Battlefield, db);
    let leviathan = scenario.add_real_card(P0, "Kederekt Leviathan", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(ManaType::Blue, 8));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&karn)
        .expect("Karn exists")
        .is_commander = true;
    runner
        .state_mut()
        .objects
        .get_mut(&arbor)
        .expect("Dryad Arbor exists")
        .damage_marked = 1;

    cast_until_resolved_or_asked(&mut runner, leviathan, &[]);
    for _ in 0..6 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes while the trigger resolves");
    }
    assert_commander_prompt(runner.state(), P1, karn);
    assert!(
        runner.state().stack.is_empty(),
        "the enters trigger is resolving, not waiting on the stack"
    );
    assert_eq!(runner.state().objects[&marshal].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&arbor].toughness, Some(2));

    let conceded = concede(&mut runner, P1);
    assert!(
        matches!(conceded.waiting_for, WaitingFor::Priority { player: P0 }),
        "{:?}",
        conceded.waiting_for
    );
    resolution_finished(runner.state());
    assert_eq!(runner.state().objects[&marshal].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&arbor].zone,
        Zone::Graveyard,
        "CR 704.5g: the lethally damaged creature is destroyed before priority"
    );
}

/// CR 608.2n + CR 704.3: when the resumed resolution is an instant or sorcery,
/// its final part comes before the state-based actions. Lord of Extinction
/// counts the cards in all graveyards; the only one is P1's. Rebuking Ceremony
/// targets P2's Circuit Mender and Karn, Silver Golem (P1's commander); Karn
/// parks on P1's command-zone choice and P1 concedes, taking that card with
/// them. The resumed ceremony finishes and goes to P0's graveyard before the
/// state-based actions look at the Lord, so it survives as a 1/1.
#[test]
fn concession_resume_puts_the_spell_into_the_graveyard_before_state_based_actions() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let karn = scenario.add_real_card(P1, "Karn, Silver Golem", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Circuit Mender", Zone::Graveyard, db);
    let mender = scenario.add_real_card(P2, "Circuit Mender", Zone::Battlefield, db);
    let lord = scenario.add_real_card(P0, "Lord of Extinction", Zone::Battlefield, db);
    let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&karn)
        .expect("Karn exists")
        .is_commander = true;

    cast_until_resolved_or_asked(&mut runner, ceremony, &[mender, karn]);
    assert_commander_prompt(runner.state(), P1, karn);
    assert_eq!(runner.state().objects[&lord].toughness, Some(1));

    let conceded = concede(&mut runner, P1);
    assert!(
        matches!(conceded.waiting_for, WaitingFor::Priority { player: P0 }),
        "{:?}",
        conceded.waiting_for
    );
    resolution_finished(runner.state());
    assert_eq!(runner.state().objects[&ceremony].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[2].library.front(), Some(&mender));
    assert_eq!(
        runner.state().objects[&lord].zone,
        Zone::Battlefield,
        "CR 608.2n: the ceremony was in a graveyard when the state-based actions ran"
    );
    assert_eq!(runner.state().objects[&lord].toughness, Some(1));
}

/// CR 104.2a + CR 104.1 + CR 704.5a: a concession whose resumed resolution
/// ends the game. P2 is at 0 life and controls Platinum Angel; P0 controls
/// Super Shredder. Rebuking Ceremony targets the Angel and Karn, Silver Golem
/// (P1's commander); Karn parks and P1 concedes. The resumed ceremony puts the
/// Angel on top of P2's library, P2 loses to the state-based action and P0 wins.
/// The finished game owes nothing: no frame, no carrier, nothing waiting to be
/// put on the stack.
#[test]
fn concession_resume_that_ends_the_game_leaves_nothing_owed() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let karn = scenario.add_real_card(P1, "Karn, Silver Golem", Zone::Battlefield, db);
    let angel = scenario.add_real_card(P2, "Platinum Angel", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
    let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
    scenario.with_life(P2, 0);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&karn)
        .expect("Karn exists")
        .is_commander = true;

    cast_until_resolved_or_asked(&mut runner, ceremony, &[angel, karn]);
    assert_commander_prompt(runner.state(), P1, karn);
    assert!(
        !runner.state().players[2].is_eliminated,
        "Platinum Angel keeps P2 in the game at 0 life"
    );

    let conceded = concede(&mut runner, P1);
    assert!(
        matches!(
            conceded.waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "{:?}",
        conceded.waiting_for
    );
    assert_eq!(runner.state().objects[&angel].zone, Zone::Exile);
    assert!(runner.state().deferred_triggers.is_empty());
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_replacement.is_none());
}

/// CR 704.5a + CR 608.2n + CR 704.4 + CR 603.6c: the chooser leaves through
/// the player-loss state-based action instead of conceding. The engine's
/// player-loss safety net (#962) applies that loss at the prompt rather than at
/// the next priority (CR 104.3b), so the resolution it interrupts must still be
/// finished before anyone receives priority, and the departure's own leave
/// triggers must still be collected. P1 is at 0 life
/// with Platinum Angel and owns the merged commander pile; P1's graveyard holds
/// the only graveyard card, and P0 controls Lord of Extinction. Rebuking
/// Ceremony targets the pile and the Angel, and P1 arranges them (CR 401.4);
/// the Angel moves first, Circuit Mender follows and Brokkos's CR 903.9b prompt
/// parks the split. At that action's boundary P1 loses, which cancels the
/// prompt; the ceremony is
/// finished and put into P0's graveyard before the remaining state-based
/// actions, so the Lord survives and nothing is parked when priority is given.
#[test]
fn player_loss_during_the_prompt_finishes_the_resolution_before_priority() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let (mender, brokkos) = add_pile(&mut scenario, P1);
    let angel = scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Circuit Mender", Zone::Graveyard, db);
    let lord = scenario.add_real_card(P0, "Lord of Extinction", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
    let patrol = scenario.add_real_card(P1, "Ninth Bridge Patrol", Zone::Battlefield, db);
    let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
    scenario.with_life(P1, 0);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();
    let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
    assert_eq!(runner.state().objects[&lord].toughness, Some(1));

    cast_until_resolved_or_asked(&mut runner, ceremony, &[pile.survivor, angel]);
    // CR 401.4: both cards go on top of P1's library, so P1 arranges them.
    let WaitingFor::EffectZoneChoice {
        player: P1, cards, ..
    } = &runner.state().waiting_for
    else {
        panic!("{:?}", runner.state().waiting_for);
    };
    assert_eq!(cards.len(), 2);
    runner
        .act(GameAction::SelectCards {
            cards: vec![pile.survivor, angel],
        })
        .expect("P1 arranges the two cards");

    assert!(
        runner.state().players[1].is_eliminated,
        "CR 704.5a: without Platinum Angel, P1 at 0 life loses"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    resolution_finished(runner.state());
    assert_eq!(runner.state().objects[&ceremony].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&pile.brokkos].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&lord].zone,
        Zone::Battlefield,
        "CR 608.2n + CR 704.4: the ceremony was in a graveyard when the state-based actions ran"
    );
    assert_eq!(runner.state().objects[&lord].toughness, Some(1));
    assert_eq!(runner.state().objects[&patrol].zone, Zone::Exile);
    assert_eq!(
        shredder_triggers(runner.state()),
        3,
        "CR 603.6c: Super Shredder saw the Angel, the merged permanent and the departed player's Patrol leave, once each"
    );
}

/// CR 800.4a + CR 730.3 + CR 117.2e: a move no frame announced. Unsummon
/// returns P1's merged commander pile through a single targeted move, so the
/// component batch is its own owner. Brokkos's CR 903.9b prompt parks it and
/// P1 concedes: the component the batch still holds leaves the game with P1,
/// and Unsummon finishes before priority.
#[test]
fn unsummon_concession_during_the_prompt_exiles_the_held_component() {
    for pile_shape in [Pile::BrokkosUnderMender, Pile::MenderOverBrokkos] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
        let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
        scenario.with_mana_pool(P0, mana(ManaType::Blue, 1));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
        let label = format!("{pile_shape:?}");

        cast_until_resolved_or_asked(&mut runner, unsummon, &[pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        let conceded = concede(&mut runner, P1);

        assert!(
            matches!(conceded.waiting_for, WaitingFor::Priority { player: P0 }),
            "{label}: {:?}",
            conceded.waiting_for
        );
        resolution_finished(runner.state());
        assert_eq!(
            runner.state().objects[&unsummon].zone,
            Zone::Graveyard,
            "{label}"
        );
        for component in [pile.mender, pile.brokkos] {
            assert_eq!(
                runner.state().objects[&component].zone,
                Zone::Exile,
                "{label}: CR 800.4a: {component:?} leaves the game with its owner"
            );
        }
        assert_eq!(
            shredder_triggers(runner.state()),
            1,
            "{label}: Super Shredder saw the one merged permanent leave once"
        );
    }
}

/// CR 800.4a + CR 117.2e + CR 730.3: Chaos Warp's owner iteration parks on
/// Brokkos's CR 903.9b prompt and the owner — the prompt's chooser — concedes.
/// Every component leaves the game with P1, and the owner iteration and its
/// continuation finish within the concession: nothing is parked when priority
/// is given, and the departed owner's shuffle, reveal and put-onto-battlefield
/// do nothing.
#[test]
fn chaos_warp_owner_concession_during_the_prompt_finishes_before_priority() {
    for pile_shape in [Pile::BrokkosUnderMender, Pile::MenderOverBrokkos] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        let warp = scenario.add_real_card(P0, "Chaos Warp", Zone::Hand, db);
        scenario.with_library_top(P1, &["Library Card A"]);
        scenario.with_mana_pool(P0, mana(ManaType::Red, 3));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
        let label = format!("{pile_shape:?}");

        cast_until_resolved_or_asked(&mut runner, warp, &[pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        let conceded = concede(&mut runner, P1);

        assert!(
            matches!(conceded.waiting_for, WaitingFor::Priority { player: P0 }),
            "{label}: {:?}",
            conceded.waiting_for
        );
        resolution_finished(runner.state());
        assert_eq!(
            runner.state().objects[&warp].zone,
            Zone::Graveyard,
            "{label}"
        );
        assert!(
            !conceded.events.iter().any(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P1,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } | GameEvent::CardsRevealed { player: P1, .. }
            )),
            "{label}: CR 800.4a: the departed owner neither shuffles nor reveals"
        );
        assert!(
            runner
                .state()
                .battlefield
                .iter()
                .all(|id| runner.state().objects[id].owner != P1),
            "{label}: nothing of the departed owner's is put onto the battlefield"
        );
        for component in [pile.mender, pile.brokkos] {
            assert_eq!(
                runner.state().objects[&component].zone,
                Zone::Exile,
                "{label}: CR 800.4a: {component:?} leaves the game with its owner"
            );
        }
    }
}

/// CR 800.4a + CR 730.3: another seat concedes while the commander's owner is
/// choosing. P2's surviving artifact leaves the game with P2 and drops out of
/// the ceremony; the parked component and its owner are untouched, the prompt
/// stays with P1, and either answer finishes the ceremony with the merged
/// departure collected once.
#[test]
fn another_seat_concession_leaves_the_commander_prompt_and_its_owner_intact() {
    for answer_index in [0, 1] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        let surviving = scenario.add_real_card(P2, "Circuit Mender", Zone::Battlefield, db);
        let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
        scenario.with_library_top(P1, &["Library Card A"]);
        scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
        let label = format!("answer {answer_index}");

        cast_until_resolved_or_asked(&mut runner, ceremony, &[surviving, pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        concede(&mut runner, P2);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        assert_eq!(
            runner.state().objects[&surviving].zone,
            Zone::Exile,
            "{label}"
        );

        answer(&mut runner, answer_index);
        resolution_finished(runner.state());
        assert_eq!(
            runner.state().objects[&ceremony].zone,
            Zone::Graveyard,
            "{label}"
        );
        assert_eq!(
            runner.state().objects[&pile.mender].zone,
            Zone::Library,
            "{label}"
        );
        assert_eq!(
            mender_departure_triggers(runner.state()),
            vec![P1],
            "{label}: one merged permanent left, so its ability triggers once"
        );
    }
}

/// CR 730.3 + CR 603.2c + CR 903.9b: Kor Skyfisher's enters trigger returns a
/// permanent its controller chooses at resolution; the merged permanent is
/// chosen from several, so the move runs through the selected-card loop. The
/// commander's prompt parks the split — on Brokkos, or on the survivor's own
/// incarnation — and after either answer that loop, the announcing owner,
/// records the departure once.
#[test]
fn kor_skyfisher_selection_hands_a_parked_component_back_to_its_loop() {
    for (pile_shape, answer_index) in [
        (Pile::BrokkosUnderMender, 0),
        (Pile::BrokkosUnderMender, 1),
        (Pile::MenderOverBrokkos, 0),
        (Pile::MenderOverBrokkos, 1),
    ] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P0);
        scenario.add_real_card(P0, "Ninth Bridge Patrol", Zone::Battlefield, db);
        let skyfisher = scenario.add_real_card(P0, "Kor Skyfisher", Zone::Hand, db);
        scenario.with_mana_pool(P0, mana(ManaType::White, 2));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
        let label = format!("{pile_shape:?}, answer {answer_index}");

        cast_until_resolved_or_asked(&mut runner, skyfisher, &[]);
        for _ in 0..6 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            runner
                .act(GameAction::PassPriority)
                .expect("priority passes while the trigger resolves");
        }
        let WaitingFor::EffectZoneChoice { cards, .. } = &runner.state().waiting_for else {
            panic!("{label}: {:?}", runner.state().waiting_for);
        };
        assert!(
            cards.contains(&pile.survivor) && cards.contains(&skyfisher),
            "{label}: the merged permanent is one of several permanents to choose from"
        );
        runner
            .act(GameAction::SelectCards {
                cards: vec![pile.survivor],
            })
            .expect("the controller chooses the merged permanent");
        assert_commander_prompt(runner.state(), P0, pile.brokkos);
        save_and_restore(&mut runner);
        answer(&mut runner, answer_index);

        resolution_finished(runner.state());
        assert_eq!(
            runner.state().objects[&pile.mender].zone,
            Zone::Hand,
            "{label}"
        );
        assert_eq!(
            runner.state().objects[&pile.brokkos].zone,
            if answer_index == 0 {
                Zone::Command
            } else {
                Zone::Hand
            },
            "{label}"
        );
        assert_eq!(runner.state().objects[&skyfisher].zone, Zone::Battlefield);
        assert_eq!(
            patrol_triggers(runner.state()),
            vec![P0],
            "{label}: Ninth Bridge Patrol observes the one merged departure once"
        );
        if matches!(pile_shape, Pile::BrokkosUnderMender) {
            assert_eq!(
                mender_departure_triggers(runner.state()),
                vec![P0],
                "{label}: one merged permanent left, so its ability triggers once"
            );
        }
    }
}

/// CR 800.4a + CR 730.3: another seat concedes while Chaos Warp's owner
/// iteration is parked on Brokkos's prompt. The prompt, the component child and
/// the owner iteration beneath it are untouched; either answer finishes the
/// owner's shuffle and reveal with the merged departure observed once.
#[test]
fn another_seat_concession_leaves_a_parked_owner_iteration_intact() {
    for answer_index in [0, 1] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        scenario.add_real_card(P1, "Ninth Bridge Patrol", Zone::Battlefield, db);
        let bystander = scenario.add_real_card(P2, "Circuit Mender", Zone::Battlefield, db);
        let warp = scenario.add_real_card(P0, "Chaos Warp", Zone::Hand, db);
        scenario.with_library_top(P1, &["Library Card A", "Library Card B"]);
        scenario.with_mana_pool(P0, mana(ManaType::Red, 3));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
        let label = format!("answer {answer_index}");

        cast_until_resolved_or_asked(&mut runner, warp, &[pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        let conceded = concede(&mut runner, P2);
        assert!(
            matches!(
                conceded.waiting_for,
                WaitingFor::ReplacementChoice { player: P1, .. }
            ),
            "{label}: {:?}",
            conceded.waiting_for
        );
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        assert_eq!(runner.state().objects[&bystander].zone, Zone::Exile);

        let events = answer(&mut runner, answer_index);
        resolution_finished(runner.state());
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id: P1,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                }
            )),
            "{label}: the owner still shuffles"
        );
        assert_eq!(
            patrol_triggers(runner.state()),
            vec![P1],
            "{label}: Ninth Bridge Patrol observes the one merged departure once"
        );
        assert_eq!(
            mender_departure_triggers(runner.state()),
            vec![P1],
            "{label}: one merged permanent left, so its ability triggers once"
        );
    }
}

/// CR 730.3 + CR 903.9b + CR 616.1: a batch owner that is resumed and then
/// reaches a merged member whose component prompts. Rebuking Ceremony targets
/// P2's merged commander pile and Karn, Silver Golem (P1's commander); Karn
/// moves first and parks on P1's choice. After that answer, the resumed batch
/// delivers the pile and Brokkos's prompt parks it again, and after either
/// answer the batch records the merged departure once and finishes. Super
/// Shredder sees each departure once. Covered with a save/restore at both
/// prompts, both pile shapes, and both answers at each prompt.
#[test]
fn resumed_batch_owner_hands_a_later_merged_member_to_its_component_batch() {
    for pile_shape in [Pile::BrokkosUnderMender, Pile::MenderOverBrokkos] {
        for (karn_answer, brokkos_answer) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
            let db = shared_card_db().expect("integration card fixture must load");
            let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
            scenario.at_phase(Phase::PreCombatMain);
            let karn = scenario.add_real_card(P1, "Karn, Silver Golem", Zone::Battlefield, db);
            let (mender, brokkos) = add_pile(&mut scenario, P2);
            scenario.add_real_card(P2, "Ninth Bridge Patrol", Zone::Battlefield, db);
            scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
            let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
            scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
            let mut runner = scenario.build();
            runner
                .state_mut()
                .objects
                .get_mut(&karn)
                .expect("Karn exists")
                .is_commander = true;
            let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
            let label = format!("{pile_shape:?}, Karn {karn_answer}, Brokkos {brokkos_answer}");

            cast_until_resolved_or_asked(&mut runner, ceremony, &[pile.survivor, karn]);
            assert_commander_prompt(runner.state(), P1, karn);
            save_and_restore(&mut runner);
            answer(&mut runner, karn_answer);
            assert_commander_prompt(runner.state(), P2, pile.brokkos);
            save_and_restore(&mut runner);
            answer(&mut runner, brokkos_answer);

            resolution_finished(runner.state());
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { player: P0 }
                ) || matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
                "{label}: {:?}",
                runner.state().waiting_for
            );
            assert_eq!(
                runner.state().objects[&ceremony].zone,
                Zone::Graveyard,
                "{label}"
            );
            assert_eq!(
                patrol_triggers(runner.state()),
                vec![P2],
                "{label}: Ninth Bridge Patrol observes the one merged departure once"
            );
            assert_eq!(
                shredder_triggers(runner.state()),
                2,
                "{label}: Super Shredder saw Karn and the merged permanent leave, once each"
            );
        }
    }
}

/// CR 800.4a + CR 117.2e: the same resumed batch, reached through a concession.
/// P1 concedes at Karn's prompt; the resumed ceremony reaches P2's merged pile,
/// whose Brokkos prompt parks it again within the concession, and P2's answer
/// finishes it.
#[test]
fn concession_resume_reaching_a_merged_member_parks_on_its_component() {
    for pile_shape in [Pile::BrokkosUnderMender, Pile::MenderOverBrokkos] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let karn = scenario.add_real_card(P1, "Karn, Silver Golem", Zone::Battlefield, db);
        let (mender, brokkos) = add_pile(&mut scenario, P2);
        scenario.add_real_card(P2, "Ninth Bridge Patrol", Zone::Battlefield, db);
        let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
        scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&karn)
            .expect("Karn exists")
            .is_commander = true;
        let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
        let label = format!("{pile_shape:?}");

        cast_until_resolved_or_asked(&mut runner, ceremony, &[pile.survivor, karn]);
        assert_commander_prompt(runner.state(), P1, karn);
        let conceded = concede(&mut runner, P1);
        assert!(
            matches!(
                conceded.waiting_for,
                WaitingFor::ReplacementChoice { player: P2, .. }
            ),
            "{label}: {:?}",
            conceded.waiting_for
        );
        assert_commander_prompt(runner.state(), P2, pile.brokkos);
        answer(&mut runner, 1);
        resolution_finished(runner.state());
        assert_eq!(
            runner.state().objects[&ceremony].zone,
            Zone::Graveyard,
            "{label}"
        );
        assert_eq!(
            patrol_triggers(runner.state()),
            vec![P2],
            "{label}: Ninth Bridge Patrol observes the one merged departure once"
        );
    }
}

/// CR 800.4a + CR 608.2m + CR 730.3: Collision of Realms' per-player pass for
/// the owner of a merged commander pile parks on Brokkos's prompt, and that
/// owner concedes. The pass the iteration was running leaves with its player, and
/// the resolution goes on with the pass of the player still in the game: P2's
/// Circuit Mender is shuffled into P2's library before priority. (The same
/// concession over a plain, unmerged permanent abandons the whole carrier,
/// including P2's pass, exactly as at the merge base; that behaviour is not
/// touched by this change and is measured identical at base and head.)
#[test]
fn concession_during_a_merged_permanents_owner_pass_goes_on_with_the_next_pass() {
    for pile_shape in [Pile::BrokkosUnderMender, Pile::MenderOverBrokkos] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        let bystander = scenario.add_real_card(P2, "Circuit Mender", Zone::Battlefield, db);
        let collision = scenario.add_real_card(P0, "Collision of Realms", Zone::Hand, db);
        scenario.with_mana_pool(P0, mana(ManaType::Red, 7));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
        let label = format!("{pile_shape:?}");

        cast_until_resolved_or_asked(&mut runner, collision, &[]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        let conceded = concede(&mut runner, P1);

        assert!(
            matches!(
                conceded.waiting_for,
                WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
            ),
            "{label}: {:?}",
            conceded.waiting_for
        );
        resolution_finished(runner.state());
        assert_eq!(
            runner.state().objects[&bystander].zone,
            Zone::Library,
            "{label}: P2's pass still shuffles P2's creature"
        );
        for component in [pile.mender, pile.brokkos] {
            assert_eq!(
                runner.state().objects[&component].zone,
                Zone::Exile,
                "{label}: CR 800.4a: {component:?} leaves the game with its owner"
            );
        }
    }
}

/// CR 616.1 + CR 730.3: a synthetic member-level replacement first parks the
/// original permanent's own move. The nearest printed analogue is an optional
/// move replacement such as Rest in Peace's graveyard redirect; no printed card
/// replaces a move into a library, so this one is staged directly. Answering it
/// resumes the original member, whose split then either delivers every
/// component at once (Brokkos not a commander), delivers the survivor and parks
/// on a later component (Brokkos under Circuit Mender), or parks again on the
/// survivor's own component (Brokkos is the survivor and the commander). Both owners —
/// Totally Lost's batch and Chaos Warp's owner iteration — finish and collect
/// the departure exactly as the unparked control does.
#[test]
fn resumed_merged_member_settles_once_whether_its_components_park_or_not() {
    for spell_name in ["Totally Lost", "Chaos Warp"] {
        for (pile_shape, component_answer) in [
            (Pile::BrokkosUnderMender, None),
            (Pile::BrokkosUnderMender, Some(0)),
            (Pile::BrokkosUnderMender, Some(1)),
            (Pile::MenderOverBrokkos, Some(0)),
            (Pile::MenderOverBrokkos, Some(1)),
        ] {
            let control = unparked_control_triggers(spell_name, pile_shape);
            assert_control(spell_name, pile_shape, &control);
            let db = shared_card_db().expect("integration card fixture must load");
            let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 7);
            scenario.at_phase(Phase::PreCombatMain);
            let (mender, brokkos) = add_pile(&mut scenario, P1);
            scenario.add_real_card(P1, "Ninth Bridge Patrol", Zone::Battlefield, db);
            let survivor = match pile_shape {
                Pile::BrokkosUnderMender => mender,
                Pile::MenderOverBrokkos => brokkos,
            };
            let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);
            scenario.with_library_top(P1, &["Library Card A", "Library Card B"]);
            let mut pool = mana(ManaType::Blue, 5);
            pool.extend(mana(ManaType::Red, 3));
            scenario.with_mana_pool(P0, pool);
            scenario
                .add_creature(P0, "Synthetic member-level library redirect", 0, 0)
                .as_enchantment()
                .with_replacement_definition(member_level_library_redirect(survivor));
            let mut runner = scenario.build();
            let pile = merge_pile(
                &mut runner,
                pile_shape,
                mender,
                brokkos,
                component_answer.is_some(),
            );
            let label = format!("{spell_name}, {pile_shape:?}, {component_answer:?}");

            cast_until_resolved_or_asked(&mut runner, spell, &[pile.survivor]);
            assert_eq!(
                runner
                    .state()
                    .pending_replacement
                    .as_ref()
                    .and_then(|pending| pending.proposed.affected_object_id()),
                Some(pile.survivor),
                "{label}: the original permanent's own move parks first"
            );
            // Decline the optional redirect: the permanent still goes to the library.
            answer(&mut runner, 1);
            if let Some(index) = component_answer {
                assert_commander_prompt(runner.state(), P1, pile.brokkos);
                save_and_restore(&mut runner);
                answer(&mut runner, index);
            }

            resolution_finished(runner.state());
            assert_eq!(
                mender_departure_triggers(runner.state()),
                control,
                "{label}: the resumed departure is collected exactly as the unparked one"
            );
            assert_eq!(
                patrol_triggers(runner.state()),
                vec![P1],
                "{label}: Ninth Bridge Patrol observes the one merged departure once"
            );
            if component_answer == Some(0) {
                assert_eq!(
                    runner.state().objects[&pile.brokkos].zone,
                    Zone::Command,
                    "{label}"
                );
            }
        }
    }
}

/// Synthetic: "If the merged permanent would be put into a library, you may
/// exile it instead." Scoped to the original permanent; the split inherits the
/// applied record, so its components are not offered it again (CR 730.3d).
fn member_level_library_redirect(survivor: ObjectId) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .mode(ReplacementMode::Optional { decline: None })
        .destination_zone(Zone::Library)
        .valid_card(TargetFilter::SpecificObject { id: survivor })
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                library_shuffle: Default::default(),
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}
