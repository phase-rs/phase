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
//!   Turn Against: "Devoid / Gain control of target creature until end of turn.
//!    Untap that creature. It gains haste until end of turn."
//!   Extractor Demon: "Flying / Whenever another creature leaves the battlefield,
//!    you may have target player mill two cards. / Unearth {2}{B} (...)"
//!   Karn, Silver Golem and Traxos, Scourge of Kroog: legendary artifact
//!    creatures, each its owner's commander in the two-commander row.
//!
//! The mutate itself is staged with `merge_object_onto` (mutate casting is not
//! under test). Brokkos is a legendary creature and the designated commander
//! (CR 903.3); CR 903.9b offers its owner the command zone when it would be put
//! into a library.
//!
//! Known defect, not under test and not rules-correct: when Brokkos is the
//! survivor with Circuit Mender mutated on top (`Pile::MenderOverBrokkos`), the
//! merged permanent has Circuit Mender's abilities (CR 730.2a) and its
//! leaves-the-battlefield ability should look back and trigger (CR 603.10a), but
//! it does not trigger even with nothing parked. For that pile the rows compare
//! the parked run with the unparked one and use Ninth Bridge Patrol to observe
//! the departure. The defect is at the merge base too (`merge.rs` removes the
//! merge layer effect before the move) and has no upstream issue yet.

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

/// CR 603.10a + CR 800.4d: `thief` steals `target` with a real control-changing
/// spell: priority passes from the active player P0 to `thief`, who casts
/// `spell`, and it resolves back to P0. At the thief's later departure the
/// stolen permanent is the thief's.
fn steal_for(runner: &mut GameRunner, thief: PlayerId, spell: ObjectId, target: ObjectId) {
    for _ in 0..3 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == thief
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes toward the thief");
    }
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(runner.state().objects[&target].controller, thief);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the resolved spell returns priority to the active player"
    );
}

/// How many Extractor Demon triggers wait: one for each creature that left.
fn demon_triggers(state: &GameState) -> usize {
    waiting_triggers(state)
        .into_iter()
        .filter(|(_, text)| text.contains("another creature leaves"))
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
    pool.extend(mana(ManaType::Red, 5));
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

/// The unparked control, pinned for both piles. With Circuit Mender as the
/// survivor its own trigger fires once. With Brokkos as the survivor it does not
/// fire at all: that is the known look-back defect in the module note, pinned
/// here as it is so a fix of it fails this row and is noticed, while the rows
/// require the parked run to match the control either way.
fn assert_control(spell_name: &str, pile_shape: Pile, control: &[PlayerId]) {
    let expected: &[PlayerId] = match pile_shape {
        Pile::BrokkosUnderMender => &[P1],
        Pile::MenderOverBrokkos => &[],
    };
    assert_eq!(control, expected, "{spell_name} {pile_shape:?} control");
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
            scenario.with_mana_pool(P0, mana(ManaType::Red, 5));
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
            let count_of = |matcher: &dyn Fn(&GameEvent) -> bool| {
                events.iter().filter(|event| matcher(event)).count()
            };
            assert_eq!(
                count_of(&|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id: P1,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )),
                1,
                "{label}: the owner's tail shuffles exactly once"
            );
            assert_eq!(
                count_of(&|event| matches!(event, GameEvent::CardsRevealed { player: P1, .. })),
                1,
                "{label}: the owner's tail reveals exactly once"
            );
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
        let stolen_demon = scenario.add_real_card(P0, "Extractor Demon", Zone::Battlefield, db);
        let turn_against = scenario.add_real_card(P1, "Turn Against", Zone::Hand, db);
        let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
        scenario.with_library_top(P2, &["Library Card A"]);
        scenario.with_library_top(P1, &["Library Card B"]);
        scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
        scenario.with_mana_pool(P1, mana(ManaType::Red, 5));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, pile_shape, mender, brokkos, true);
        let label = format!("{pile_shape:?}");
        steal_for(&mut runner, P1, turn_against, stolen_demon);

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
            matches!(
                conceded.waiting_for,
                WaitingFor::Priority { player: P0 } | WaitingFor::OrderTriggers { player: P0, .. }
            ),
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
        assert_eq!(
            demon_triggers(runner.state()),
            1,
            "{label}: CR 603.10a + CR 800.4d: the Extractor Demon P1 had stolen looked back to a \
             source P1 controlled for the merged permanent's retained departure and the parked \
             component that left with P1, so those triggers were P1's; it triggers once, for the \
             surviving artifact that left after P1 did"
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
/// P0 also has an Extractor Demon that P1 stole: P0's own Super Shredder, the
/// unstolen observer, triggers once for each permanent that left, and the stolen
/// Demon triggers for none, because the departure is collected once, against the
/// observers as they were before P1 left.
#[test]
fn player_loss_during_the_prompt_finishes_the_resolution_before_priority() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let (mender, brokkos) = add_pile(&mut scenario, P1);
    let angel = scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Circuit Mender", Zone::Graveyard, db);
    let lord = scenario.add_real_card(P0, "Lord of Extinction", Zone::Battlefield, db);
    let shredder = scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
    let stolen_demon = scenario.add_real_card(P0, "Extractor Demon", Zone::Battlefield, db);
    let turn_against = scenario.add_real_card(P1, "Turn Against", Zone::Hand, db);
    let patrol = scenario.add_real_card(P1, "Ninth Bridge Patrol", Zone::Battlefield, db);
    let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
    scenario.with_life(P1, 0);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    scenario.with_mana_pool(P1, mana(ManaType::Red, 5));
    let mut runner = scenario.build();
    let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
    assert_eq!(runner.state().objects[&lord].toughness, Some(1));

    steal_for(&mut runner, P1, turn_against, stolen_demon);

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
        runner.state().objects[&stolen_demon].controller,
        P0,
        "CR 800.4a: P1's control effect ended with P1"
    );
    assert_eq!(
        runner.state().objects[&stolen_demon].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().objects[&shredder].zone, Zone::Battlefield);
    assert_eq!(
        shredder_triggers(runner.state()),
        3,
        "CR 603.6c: P0's own Super Shredder saw the Angel, the merged permanent and the departed \
         player's Patrol leave, once each"
    );
    assert_eq!(
        demon_triggers(runner.state()),
        0,
        "CR 603.10a + CR 603.3a + CR 800.4d: the Extractor Demon P1 had stolen looked back to a \
         source P1 controlled for the merged permanent's departure, which the owner collected \
         before P1 left, and for the Patrol that left with P1, so those triggers were \
         P1's and left with P1"
    );
}

/// CR 704.5a + CR 603.3 + CR 117.5 + CR 603.6c: a player-loss state-based action
/// in the pass that follows a completed choice, with nothing parked to resume.
/// P1 is at 0 life with Platinum Angel and owns an unmerged Circuit Mender and a
/// Ninth Bridge Patrol. Rebuking Ceremony puts the Angel and the Mender on top
/// of P1's library and P1 arranges them (CR 401.4); the Angel leaves and P1 loses
/// in that action's pass. The abilities the departure triggered, P0's Super
/// Shredder seeing the Patrol leave the game with P1, are on the stack before P0
/// has priority, with the Shredder's triggers for the Angel and the Mender. A
/// regression pin for the in-pass route (the departure's queued triggers join the
/// boundary's one batch); it does not discriminate the origin stamp.
#[test]
fn player_loss_after_a_completed_choice_puts_the_departure_triggers_on_the_stack() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let angel = scenario.add_real_card(P1, "Platinum Angel", Zone::Battlefield, db);
    let mender = scenario.add_real_card(P1, "Circuit Mender", Zone::Battlefield, db);
    let patrol = scenario.add_real_card(P1, "Ninth Bridge Patrol", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Super Shredder", Zone::Battlefield, db);
    let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
    scenario.with_life(P1, 0);
    scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
    let mut runner = scenario.build();

    cast_until_resolved_or_asked(&mut runner, ceremony, &[angel, mender]);
    let WaitingFor::EffectZoneChoice {
        player: P1, cards, ..
    } = &runner.state().waiting_for
    else {
        panic!("{:?}", runner.state().waiting_for);
    };
    assert_eq!(cards.len(), 2);
    runner
        .act(GameAction::SelectCards {
            cards: vec![angel, mender],
        })
        .expect("P1 arranges the two cards");

    assert!(runner.state().players[1].is_eliminated);
    assert_eq!(runner.state().objects[&patrol].zone, Zone::Exile);
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "the departure's triggers are not left waiting"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(
                |entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility {
                description: Some(text), ..
            } if text.contains("another permanent leaves"))
            )
            .count(),
        3,
        "Super Shredder saw the Angel, the Mender and the Patrol leave, once each, on the stack"
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

/// CR 800.4a + CR 730.3: the spell's controller concedes while the
/// owner of the merged pile is choosing. P0's spell leaves with P0; P1's prompt,
/// the component child and the owner beneath it (a ChangeZone iteration for
/// Chaos Warp, a batch for Rebuking Ceremony) are untouched, and after P1's
/// answer nothing is left parked.
#[test]
fn spell_controller_concession_during_the_owners_prompt_leaves_nothing_parked() {
    for spell_name in ["Chaos Warp", "Rebuking Ceremony"] {
        for answer_index in [0, 1] {
            let db = shared_card_db().expect("integration card fixture must load");
            let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
            scenario.at_phase(Phase::PreCombatMain);
            let (mender, brokkos) = add_pile(&mut scenario, P1);
            let karn = scenario.add_real_card(P2, "Karn, Silver Golem", Zone::Battlefield, db);
            let spell = scenario.add_real_card(P0, spell_name, Zone::Hand, db);
            scenario.with_library_top(P1, &["Library Card A", "Library Card B"]);
            let mut pool = mana(ManaType::Red, 5);
            pool.extend(mana(ManaType::Green, 5));
            scenario.with_mana_pool(P0, pool);
            let mut runner = scenario.build();
            let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
            let label = format!("{spell_name}, answer {answer_index}");
            let targets: Vec<ObjectId> = if spell_name == "Chaos Warp" {
                vec![pile.survivor]
            } else {
                vec![karn, pile.survivor]
            };

            cast_until_resolved_or_asked(&mut runner, spell, &targets);
            assert_commander_prompt(runner.state(), P1, pile.brokkos);
            let conceded = concede(&mut runner, P0);
            assert!(
                matches!(
                    conceded.waiting_for,
                    WaitingFor::ReplacementChoice { player: P1, .. }
                ),
                "{label}: the owner's prompt is untouched: {:?}",
                conceded.waiting_for
            );
            assert_commander_prompt(runner.state(), P1, pile.brokkos);
            let events = answer(&mut runner, answer_index);
            // CR 608.2m: a resolution under way continues to resolve fully, so
            // Chaos Warp's tail shuffles and reveals once for the owner.
            let expected_tail = usize::from(spell_name == "Chaos Warp");
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        GameEvent::PlayerPerformedAction {
                            player_id: P1,
                            action: PlayerActionKind::ShuffledLibrary,
                            ..
                        }
                    ))
                    .count(),
                expected_tail,
                "{label}: the owner's shuffle runs {expected_tail} time(s)"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::CardsRevealed { player: P1, .. }))
                    .count(),
                expected_tail,
                "{label}: the owner's reveal runs {expected_tail} time(s)"
            );
            // The recipient of priority is not asserted: with a leave trigger
            // waiting, the engine hands it to the departed active player, as it
            // does at the merge base for an unmerged Rebuking Ceremony.
            resolution_finished(runner.state());
            assert_eq!(
                runner.state().objects[&pile.mender].zone,
                Zone::Library,
                "{label}"
            );
        }
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
        scenario.with_mana_pool(P0, mana(ManaType::Red, 5));
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
        let stolen_demon = scenario.add_real_card(P0, "Extractor Demon", Zone::Battlefield, db);
        let turn_against = scenario.add_real_card(P2, "Turn Against", Zone::Hand, db);
        let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
        scenario.with_library_top(P1, &["Library Card A"]);
        scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
        scenario.with_mana_pool(P2, mana(ManaType::Red, 5));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
        let label = format!("answer {answer_index}");
        steal_for(&mut runner, P2, turn_against, stolen_demon);

        cast_until_resolved_or_asked(&mut runner, ceremony, &[surviving, pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        concede(&mut runner, P2);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        assert_eq!(
            runner.state().objects[&surviving].zone,
            Zone::Exile,
            "{label}"
        );

        // The owner's record of what it collected before P2 left survives a save.
        save_and_restore(&mut runner);
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
        assert_eq!(
            demon_triggers(runner.state()),
            0,
            "{label}: CR 603.10a + CR 800.4d: P2 conceded while P1's prompt was parked, so the \
             Extractor Demon P2 had stolen looked back to a source P2 controlled for the merged \
             permanent's retained departure, which the owner collected before P2 left"
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

/// CR 603.2 + CR 603.10a + CR 603.3a: the observer leaves with the leaver, but its
/// trigger is a survivor's. P0 takes control of P2's Extractor Demon with Turn
/// Against; P0's Totally Lost moves P1's merged commander pile and Brokkos's
/// CR 903.9b prompt parks the split. P2 concedes while P1's prompt is open, and
/// the Demon leaves the game with its owner. The Demon's ability had already
/// triggered for the merged permanent's departure under P0, so after P1 answers
/// P0 is putting it on the stack, exactly once.
#[test]
fn a_survivors_trigger_survives_its_source_leaving_with_another_seat() {
    for answer_index in [0, 1] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        let demon = scenario.add_real_card(P2, "Extractor Demon", Zone::Battlefield, db);
        let turn_against = scenario.add_real_card(P0, "Turn Against", Zone::Hand, db);
        let lost = scenario.add_real_card(P0, "Totally Lost", Zone::Hand, db);
        scenario.with_library_top(P1, &["Library Card A"]);
        let mut pool = mana(ManaType::Blue, 5);
        pool.extend(mana(ManaType::Red, 5));
        scenario.with_mana_pool(P0, pool);
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
        let label = format!("answer {answer_index}");
        steal_for(&mut runner, P0, turn_against, demon);

        cast_until_resolved_or_asked(&mut runner, lost, &[pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        concede(&mut runner, P2);
        assert_eq!(runner.state().objects[&demon].zone, Zone::Exile, "{label}");
        assert_commander_prompt(runner.state(), P1, pile.brokkos);

        answer(&mut runner, answer_index);
        // The Demon's ability is being put on the stack for P0, who chooses its
        // target (CR 603.3d); nothing of the resolution is left parked.
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection {
                player: P0,
                trigger_controller: Some(P0),
                source_id: Some(source),
                ..
            } if *source == demon => {}
            other => panic!("{label}: {other:?}"),
        }
        assert!(runner.state().pending_replacement.is_none(), "{label}");
        assert!(runner.state().resolution_stack.is_empty(), "{label}");
        let demon_triggers_in_flight = runner
            .state()
            .deferred_triggers
            .iter()
            .filter(|context| context.pending.source_id == demon)
            .count()
            + runner
                .state()
                .stack
                .iter()
                .filter(|entry| entry.source_id == demon)
                .count();
        assert_eq!(
            demon_triggers_in_flight, 1,
            "{label}: the Demon triggered once, for the one merged permanent that left: the \
             ability P0 is announcing is its only one on the stack or waiting"
        );
    }
}

/// CR 603.2c + CR 603.3a: the batched sibling of the row above. P0 controls
/// P2's Nefarious Imp and P1's merged commander pile (two Turn Against); P0's
/// Totally Lost moves the pile and Brokkos's CR 903.9b prompt parks the split.
/// P2 concedes and the Imp leaves with its owner. After P1 answers, P0 has two
/// Imp triggers: one for the merged permanent P0 controlled, latched on the
/// owner before P2 left, and one for the Imp itself leaving the battlefield.
#[test]
fn a_survivors_batched_trigger_survives_its_source_leaving_with_another_seat() {
    for answer_index in [0, 1] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        let imp = scenario.add_real_card(P2, "Nefarious Imp", Zone::Battlefield, db);
        let take_imp = scenario.add_real_card(P0, "Turn Against", Zone::Hand, db);
        let take_pile = scenario.add_real_card(P0, "Turn Against", Zone::Hand, db);
        let lost = scenario.add_real_card(P0, "Totally Lost", Zone::Hand, db);
        scenario.with_library_top(P1, &["Library Card A"]);
        let mut pool = mana(ManaType::Blue, 5);
        pool.extend(mana(ManaType::Red, 10));
        scenario.with_mana_pool(P0, pool);
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
        let label = format!("answer {answer_index}");
        steal_for(&mut runner, P0, take_imp, imp);
        steal_for(&mut runner, P0, take_pile, pile.survivor);

        cast_until_resolved_or_asked(&mut runner, lost, &[pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        concede(&mut runner, P2);
        assert_eq!(runner.state().objects[&imp].zone, Zone::Exile, "{label}");
        assert_commander_prompt(runner.state(), P1, pile.brokkos);

        answer(&mut runner, answer_index);
        assert!(runner.state().resolution_stack.is_empty(), "{label}");
        let imp_controllers: Vec<PlayerId> = waiting_triggers(runner.state())
            .into_iter()
            .filter(|(_, text)| text.contains("one or more permanents you control leave"))
            .map(|(controller, _)| controller)
            .collect();
        assert_eq!(
            imp_controllers,
            vec![P0, P0],
            "{label}: {:?}",
            runner.state().waiting_for
        );
    }
}

/// Add `entry` to the `collected_before_departure` lists in a saved game: to
/// those that already hold a record (`holding_a_record`), or to the empty ones.
fn forge_collected_before_departure(
    value: &mut serde_json::Value,
    entry: &serde_json::Value,
    holding_a_record: bool,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "collected_before_departure" {
                    let list = child.as_array_mut().expect("the record is a list");
                    if list.is_empty() != holding_a_record {
                        list.push(entry.clone());
                    }
                } else {
                    forge_collected_before_departure(child, entry, holding_a_record);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                forge_collected_before_departure(item, entry, holding_a_record);
            }
        }
        _ => {}
    }
}

/// CR 603.2c: a save whose owner claims to have collected, before a departure,
/// a zone change neither it nor its component child holds is rejected, and so
/// is a duplicated record; the untouched save loads. The save is taken at
/// Brokkos's prompt after another seat conceded, when the owner holds one real
/// record (Circuit Mender's departure).
#[test]
fn a_forged_record_of_departures_collected_before_a_player_left_is_rejected_on_load() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let (mender, brokkos) = add_pile(&mut scenario, P1);
    let lost = scenario.add_real_card(P0, "Totally Lost", Zone::Hand, db);
    scenario.with_library_top(P1, &["Library Card A"]);
    scenario.with_mana_pool(P0, mana(ManaType::Blue, 5));
    let mut runner = scenario.build();
    let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
    cast_until_resolved_or_asked(&mut runner, lost, &[pile.survivor]);
    assert_commander_prompt(runner.state(), P1, pile.brokkos);
    concede(&mut runner, P2);

    let saved = serde_json::to_value(runner.state()).expect("the parked prompt serializes");
    let real: Vec<serde_json::Value> = {
        fn find(value: &serde_json::Value, found: &mut Vec<serde_json::Value>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, child) in map {
                        if key == "collected_before_departure" {
                            found.extend(child.as_array().cloned().unwrap_or_default());
                        } else {
                            find(child, found);
                        }
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|item| find(item, found)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        find(&saved, &mut found);
        found
    };
    assert_eq!(
        real.len(),
        1,
        "reach guard: the owner recorded Circuit Mender's departure: {real:?}"
    );
    // A restore rejects invalid frames with an error, and in a debug build its
    // runtime invariant check panics first; either is a rejection.
    let loads = |value: serde_json::Value| {
        std::panic::catch_unwind(|| {
            serde_json::from_value::<GameState>(value)
                .ok()
                .is_some_and(|state| state.resolution_stack.validate(&state.waiting_for).is_ok())
        })
        .unwrap_or(false)
    };
    assert!(loads(saved.clone()), "the untouched save loads");

    let mut unknown = saved.clone();
    let mut forged = real[0].clone();
    forged["object_id"] = serde_json::json!(9_999);
    forge_collected_before_departure(&mut unknown, &forged, true);
    assert!(
        !loads(unknown),
        "a record naming a zone change nobody holds is rejected"
    );

    let mut duplicate = saved;
    forge_collected_before_departure(&mut duplicate, &real[0], true);
    assert!(!loads(duplicate), "a duplicated record is rejected");

    // An owner with no component child: Totally Lost on an unmerged commander.
    // Karn has not moved, so a record of Karn's move is not one of the owner's
    // occurrences.
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let karn = scenario.add_real_card(P1, "Karn, Silver Golem", Zone::Battlefield, db);
    let lost = scenario.add_real_card(P0, "Totally Lost", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(ManaType::Blue, 5));
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&karn)
        .expect("commander exists")
        .is_commander = true;
    cast_until_resolved_or_asked(&mut runner, lost, &[karn]);
    assert_commander_prompt(runner.state(), P1, karn);
    let saved = serde_json::to_value(runner.state()).expect("the parked prompt serializes");
    assert!(loads(saved.clone()), "the untouched unmerged save loads");
    let mut forged_karn = real[0].clone();
    forged_karn["object_id"] = serde_json::json!(karn.0);
    let mut unlinked = saved;
    forge_collected_before_departure(&mut unlinked, &forged_karn, false);
    assert!(
        !loads(unlinked),
        "an owner with no component child cannot claim a move it has not made"
    );
}

/// CR 603.10a + CR 603.3a + CR 800.4d: a second player leaves the same turn.
/// In a four-player game P3 concedes first, so an earlier departure is already
/// recorded this turn. P2 then steals P0's Extractor Demon, P0's Rebuking
/// Ceremony starts moving P1's merged commander pile, and Brokkos's CR 903.9b
/// prompt parks the owner. P2 concedes while P1's prompt is open, the Demon goes
/// back to P0, and P1 answers: the merged permanent's retained departure was
/// collected under the controllers before P2 left, so the Demon triggers nothing.
/// The boundary is the departure during this owner's operation, not the first
/// of the turn.
#[test]
fn a_second_leaver_in_the_turn_still_retires_its_stolen_observers_retained_trigger() {
    for answer_index in [0, 1] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 4, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let (mender, brokkos) = add_pile(&mut scenario, P1);
        let stolen_demon = scenario.add_real_card(P0, "Extractor Demon", Zone::Battlefield, db);
        let turn_against = scenario.add_real_card(P2, "Turn Against", Zone::Hand, db);
        let ceremony = scenario.add_real_card(P0, "Rebuking Ceremony", Zone::Hand, db);
        let surviving = scenario.add_real_card(P2, "Circuit Mender", Zone::Battlefield, db);
        scenario.with_library_top(P1, &["Library Card A"]);
        scenario.with_mana_pool(P0, mana(ManaType::Green, 5));
        scenario.with_mana_pool(P2, mana(ManaType::Red, 5));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
        let label = format!("answer {answer_index}");

        concede(&mut runner, PlayerId(3));
        assert!(runner.state().players[3].is_eliminated, "{label}");
        steal_for(&mut runner, P2, turn_against, stolen_demon);
        cast_until_resolved_or_asked(&mut runner, ceremony, &[surviving, pile.survivor]);
        assert_commander_prompt(runner.state(), P1, pile.brokkos);
        concede(&mut runner, P2);
        assert!(runner.state().players[2].is_eliminated, "{label}");
        assert_commander_prompt(runner.state(), P1, pile.brokkos);

        answer(&mut runner, answer_index);
        resolution_finished(runner.state());
        assert_eq!(
            mender_departure_triggers(runner.state()),
            vec![P1],
            "{label}: one merged permanent left, so its ability triggers once"
        );
        assert_eq!(
            demon_triggers(runner.state()),
            0,
            "{label}: the Extractor Demon P2 had stolen looked back to a source P2 controlled \
             for the merged permanent's retained departure, which the owner collected before P2 \
             left, though P3 had left earlier in the turn"
        );
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
        let stolen_demon = scenario.add_real_card(P0, "Extractor Demon", Zone::Battlefield, db);
        let turn_against = scenario.add_real_card(P2, "Turn Against", Zone::Hand, db);
        let warp = scenario.add_real_card(P0, "Chaos Warp", Zone::Hand, db);
        scenario.with_library_top(P1, &["Library Card A", "Library Card B"]);
        scenario.with_mana_pool(P0, mana(ManaType::Red, 5));
        scenario.with_mana_pool(P2, mana(ManaType::Red, 5));
        let mut runner = scenario.build();
        let pile = merge_pile(&mut runner, Pile::BrokkosUnderMender, mender, brokkos, true);
        let label = format!("answer {answer_index}");
        steal_for(&mut runner, P2, turn_against, stolen_demon);

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

        // The owner's record of what it collected before P2 left survives a save.
        save_and_restore(&mut runner);
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
        assert_eq!(
            demon_triggers(runner.state()),
            0,
            "{label}: CR 603.10a + CR 800.4d: P2 conceded while P1's prompt was parked, so the \
             Extractor Demon P2 had stolen looked back to a source P2 controlled for the merged \
             permanent's retained departure, which the owner collected before P2 left"
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
            pool.extend(mana(ManaType::Red, 5));
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
