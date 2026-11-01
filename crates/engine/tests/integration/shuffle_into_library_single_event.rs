//! CR 701.24a: "To shuffle a library or a face-down pile of cards, randomize the
//! cards within it so that no player knows their order."
//!
//! A "[put it into its owner's library, then] shuffle" clause is ONE shuffle
//! instruction. The parser lowers it to a move into the library followed by a
//! terminal `Shuffle`; the zone pipeline also randomizes a placement-less library
//! delivery. The two must not both fire: every affected library is shuffled exactly
//! once per instruction (CR 701.24a), because each shuffle is an observable event —
//! CR 701.24f has shuffle watchers (Cosi's Trickster) trigger once per simultaneous
//! shuffle — and is journaled.
//!
//! Every test drives the real cast / trigger pipeline with printed cards and pairs
//! its event-count assertion with a reach-guard proving the object actually arrived
//! in (or, for CR 701.24c/d, stayed out of) the library — a count of one is vacuous
//! if nothing moved.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn card_db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

fn shuffles(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == player
            )
        })
        .count()
}

/// Library shuffles recorded by the resolved-rules journal for `player`.
fn journaled_shuffles(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                &entry.command,
                Some(ResolvedRulesCommand::LibraryShuffle(command)) if command.player == player
            )
        })
        .count()
}

/// Pass priority until the stack is empty, collecting every emitted event. A
/// "you may" prompt (Cosi's Trickster) is accepted.
fn drain_stack(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                let result = runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional effect");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
            other => panic!("unexpected waiting state while draining the stack: {other:?}"),
        }
    }
    panic!("stack did not drain");
}

/// Enough mana of every color for any spell below.
fn pool() -> Vec<ManaUnit> {
    [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ]
    .into_iter()
    .flat_map(|color| std::iter::repeat_n(color, 3))
    .chain(std::iter::repeat_n(ManaType::Colorless, 4))
    .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
    .collect()
}

fn in_library(runner: &GameRunner, owner: PlayerId, id: ObjectId) -> bool {
    runner.state().objects[&id].zone == Zone::Library
        && runner.state().players[owner.0 as usize]
            .library
            .contains(&id)
}

fn watcher_counters(runner: &GameRunner, watcher: ObjectId) -> u32 {
    runner.state().objects[&watcher]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// P1's Worldspine Wurm, P0's Cosi's Trickster (a shuffle-event observer) and a P0
/// Murder aimed at the Wurm.
struct WurmBoard {
    runner: GameRunner,
    wurm: ObjectId,
    murder: ObjectId,
    trickster: ObjectId,
}

fn wurm_board() -> WurmBoard {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    let wurm = scenario.add_real_card(P1, "Worldspine Wurm", Zone::Battlefield, db);
    let trickster = scenario.add_real_card(P0, "Cosi's Trickster", Zone::Battlefield, db);
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    scenario.add_card_to_library_top(P0, "P0 Library Card");
    scenario.add_card_to_library_top(P1, "P1 Library Card A");
    scenario.add_card_to_library_top(P1, "P1 Library Card B");
    WurmBoard {
        runner: scenario.build(),
        wurm,
        murder,
        trickster,
    }
}

/// The issue's card: one resolution of "When Worldspine Wurm is put into a graveyard
/// from anywhere, shuffle it into its owner's library." is one shuffle of the OWNER's
/// library — one `ShuffledLibrary` event, one journaled shuffle, and one trigger of an
/// opponent's "whenever an opponent shuffles their library" observer.
#[test]
fn worldspine_wurm_trigger_shuffles_its_owner_once() {
    let WurmBoard {
        mut runner,
        wurm,
        murder,
        trickster,
    } = wurm_board();

    // Cosi's Trickster's "you may" is accepted so the observer's count is visible.
    let outcome = runner
        .cast(murder)
        .target_objects(&[wurm])
        .accept_optional()
        .resolve();
    let mut events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut events);

    // Reach guard: the Wurm really left the graveyard for its owner's library.
    assert!(in_library(&runner, P1, wurm));

    assert_eq!(shuffles(&events, P1), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P0), 0, "events: {events:?}");
    assert_eq!(journaled_shuffles(&runner, P1), 1);
    assert_eq!(
        watcher_counters(&runner, trickster),
        1,
        "one shuffle instruction triggers a shuffle-watcher once (CR 701.24a, 701.24f)"
    );
}

/// CR 701.24c: "If an effect would cause a player to shuffle one or more specific
/// objects into a library, that library is shuffled even if none of those objects are
/// in the zone they're expected to be in" — the rule's own example is a card put into a
/// graveyard whose "shuffle it into its owner's library" trigger is answered by
/// exiling it. The terminal shuffle must still run: deferring the move's own shuffle to
/// it must not swallow the instruction's.
#[test]
fn shuffle_into_library_still_shuffles_when_the_object_was_moved_in_response() {
    let WurmBoard {
        mut runner,
        wurm,
        murder,
        trickster,
    } = wurm_board();

    // Cast Murder and pass priority until the Wurm dies and its shuffle trigger waits
    // on the stack, so the Wurm can be exiled "in response".
    let mut events = runner
        .cast(murder)
        .target_objects(&[wurm])
        .commit()
        .events()
        .to_vec();
    let shuffle_trigger_waiting = |runner: &GameRunner| {
        runner.state().stack.iter().any(|entry| {
            entry.source_id == wurm
                && matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { description: Some(text), .. }
                        if text.contains("shuffle it into its owner's library")
                )
        })
    };
    for _ in 0..10 {
        if shuffle_trigger_waiting(&runner) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
        }
    }
    assert!(
        shuffle_trigger_waiting(&runner),
        "the shuffle trigger must be waiting on the stack"
    );
    assert_eq!(runner.state().objects[&wurm].zone, Zone::Graveyard);
    engine::game::zones::move_to_zone(runner.state_mut(), wurm, Zone::Exile, &mut events);
    assert_eq!(runner.state().objects[&wurm].zone, Zone::Exile);
    let before = events.len();
    drain_stack(&mut runner, &mut events);

    // Reach guard: the Wurm stayed in exile, so the one shuffle is the instruction's
    // own, not a side effect of moving the card.
    assert_eq!(runner.state().objects[&wurm].zone, Zone::Exile);
    assert_eq!(
        shuffles(&events[before..], P1),
        1,
        "events: {:?}",
        &events[before..]
    );
    assert_eq!(watcher_counters(&runner, trickster), 1);
}

/// Casts printed `spell` from P0's hand against freshly staged targets, resolves the
/// whole stack, and returns the events plus the spell and target ids.
fn cast_real_spell(
    spell: &str,
    targets: &[(PlayerId, Zone)],
) -> (GameRunner, Vec<GameEvent>, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    let spell_id = scenario.add_real_card(P0, spell, Zone::Hand, card_db());
    let ids: Vec<ObjectId> = targets
        .iter()
        .enumerate()
        .map(|(index, (player, zone))| match zone {
            Zone::Graveyard => scenario
                .add_creature_to_graveyard(*player, &format!("Target {index}"), 2, 2)
                .id(),
            _ => scenario
                .add_creature(*player, &format!("Target {index}"), 2, 2)
                .id(),
        })
        .collect();
    scenario.add_card_to_library_top(P0, "P0 Library Card");
    scenario.add_card_to_library_top(P1, "P1 Library Card");
    let mut runner = scenario.build();
    let outcome = runner.cast(spell_id).target_objects(&ids).resolve();
    let mut events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut events);
    (runner, events, spell_id, ids)
}

/// Turn the Earth: "The owners of those cards shuffle them into their libraries" with
/// targets owned by two different players — each owner's library is shuffled exactly
/// once, not once per move plus once for the instruction (nor one owner twice and the
/// other once).
#[test]
fn shuffle_them_into_their_owners_libraries_shuffles_each_owner_once() {
    let (runner, events, _, ids) = cast_real_spell(
        "Turn the Earth",
        &[(P0, Zone::Graveyard), (P1, Zone::Graveyard)],
    );
    assert!(in_library(&runner, P0, ids[0]));
    assert!(in_library(&runner, P1, ids[1]));
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P1), 1, "events: {events:?}");
}

/// Serene Remembrance: the spell (P0's library) and two targets owned by the same other
/// player (P1's library) are two libraries, each shuffled once.
#[test]
fn shuffle_self_and_targets_into_their_owners_libraries_shuffles_each_library_once() {
    let (runner, events, spell, ids) = cast_real_spell(
        "Serene Remembrance",
        &[(P1, Zone::Graveyard), (P1, Zone::Graveyard)],
    );
    assert!(in_library(&runner, P0, spell));
    assert!(in_library(&runner, P1, ids[0]));
    assert!(in_library(&runner, P1, ids[1]));
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P1), 1, "events: {events:?}");
}

/// Chaos Warp: "The owner of target permanent shuffles it into their library, then
/// reveals the top card of their library" — one shuffle, and the follow-up still reads
/// the target owner's library.
#[test]
fn owner_shuffles_target_permanent_into_their_library_once() {
    let (runner, events, _, ids) = cast_real_spell("Chaos Warp", &[(P1, Zone::Battlefield)]);
    // The reveal may put a permanent card back onto the battlefield, so the reach guard
    // is the zone-change event into the owner's library.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Library, .. } if *object_id == ids[0]
        )),
        "the target must have been put into its owner's library: {events:?}"
    );
    assert!(runner.state().objects.contains_key(&ids[0]));
    assert_eq!(shuffles(&events, P1), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P0), 0, "events: {events:?}");
}

/// Beacon of Destruction: "deals 5 damage to any target. Shuffle ~ into its owner's
/// library." The shuffled library is the SPELL OWNER's, not the damage target's owner's
/// — the terminal shuffle names the library that actually received the card.
#[test]
fn shuffle_self_into_owners_library_shuffles_the_spells_owner_not_the_damage_target() {
    let (runner, events, spell, ids) =
        cast_real_spell("Beacon of Destruction", &[(P1, Zone::Battlefield)]);
    assert!(in_library(&runner, P0, spell));
    assert_eq!(runner.state().objects[&ids[0]].zone, Zone::Graveyard);
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P1), 0, "events: {events:?}");
}

/// Nexus of Fate: "If ~ would be put into a graveyard from anywhere, reveal ~ and
/// shuffle it into its owner's library instead." — the replacement outcome shuffles the
/// owner once.
#[test]
fn shuffle_back_replacement_shuffles_the_owner_once() {
    let (runner, events, spell, _) = cast_real_spell("Nexus of Fate", &[]);
    assert!(in_library(&runner, P0, spell));
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P1), 0, "events: {events:?}");
}

/// CR 701.24d: "If an effect would cause a player to shuffle a set of objects into a
/// library, that library is shuffled even if there are no objects in that set" (the
/// rule's Loaming Shaman example). Memory's Journey with no cards chosen still shuffles
/// the TARGETED player's library — once — and not the caster's.
#[test]
fn shuffling_no_cards_into_a_library_still_shuffles_the_target_players_library_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    let journey = scenario.add_real_card(P0, "Memory's Journey", Zone::Hand, card_db());
    let unchosen = scenario
        .add_creature_to_graveyard(P1, "Unchosen Graveyard Card", 2, 2)
        .id();
    scenario.add_card_to_library_top(P0, "P0 Library Card");
    scenario.add_card_to_library_top(P1, "P1 Library Card");
    let mut runner = scenario.build();

    let outcome = runner.cast(journey).target_players(&[P1]).resolve();
    let mut events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut events);

    // Reach guard: the spell resolved and no card was chosen, so nothing moved.
    assert_eq!(runner.state().objects[&journey].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&unchosen].zone, Zone::Graveyard);
    assert_eq!(shuffles(&events, P1), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P0), 0, "events: {events:?}");
}

/// Lat-Nam's Legacy: "Shuffle a card from your hand into your library" picks its card at
/// resolution — automatically when one card qualifies, through an `EffectZoneChoice`
/// when several do. Both routes defer the move's own shuffle to the instruction's.
fn lat_nams_legacy_with_hand(extra_cards: usize) -> (GameRunner, Vec<GameEvent>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    let legacy = scenario.add_real_card(P0, "Lat-Nam's Legacy", Zone::Hand, card_db());
    let hand: Vec<ObjectId> = (0..=extra_cards)
        .map(|index| {
            scenario
                .add_creature_to_hand(P0, &format!("Hand Card {index}"), 2, 2)
                .id()
        })
        .collect();
    scenario.add_card_to_library_top(P0, "P0 Library Card");
    let mut runner = scenario.build();
    // Choose the first hand card whenever a choice is offered.
    let outcome = runner.cast(legacy).effect_zone(&hand[..1]).resolve();
    let mut events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut events);
    (runner, events, hand[0])
}

#[test]
fn shuffle_a_card_from_hand_into_library_shuffles_once_when_the_pick_is_automatic() {
    let (runner, events, card) = lat_nams_legacy_with_hand(0);
    assert!(in_library(&runner, P0, card));
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
}

#[test]
fn shuffle_a_card_from_hand_into_library_shuffles_once_after_an_interactive_choice() {
    let (runner, events, card) = lat_nams_legacy_with_hand(2);
    assert!(in_library(&runner, P0, card));
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
}

/// Gomazoa: "{T}: Put this creature and each creature it's blocking on top of their
/// owners' libraries, then those players shuffle." The move and the shuffle arrive as
/// two separately parsed clauses but are one shuffle instruction — its owner's library
/// is shuffled once, not once for the move and once for the clause. (The current lowering
/// moves only Gomazoa itself and drops "each creature it's blocking"; that pre-existing
/// gap is not exercised here — this pins the shuffle count of the shape it does lower.)
#[test]
fn put_on_top_of_owners_libraries_then_those_players_shuffle_shuffles_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gomazoa = scenario
        .add_creature_from_oracle(
            P0,
            "Gomazoa Stand-In",
            0,
            3,
            "{T}: Put this creature and each creature it's blocking on top of their owners' libraries, then those players shuffle.",
        )
        .id();
    scenario.add_card_to_library_top(P0, "P0 Library Card A");
    scenario.add_card_to_library_top(P0, "P0 Library Card B");
    let mut runner = scenario.build();

    let outcome = runner.activate(gomazoa, 0).resolve();
    let mut events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut events);

    // Reach guard: Gomazoa left the battlefield for its owner's library.
    assert!(in_library(&runner, P0, gomazoa));
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P1), 0, "events: {events:?}");
}

/// Cathartic Parting (printed text): two shuffle-into-library instructions in one chain —
/// the opponent's artifact or enchantment goes into the OPPONENT's library, then up to four
/// of your graveyard cards go into YOURS. Each owner is shuffled once; the second
/// instruction must not re-shuffle the first one's owner.
#[test]
fn two_shuffle_instructions_in_one_spell_shuffle_each_owner_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    let parting = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Parting Stand-In",
            false,
            "The owner of target artifact or enchantment an opponent controls shuffles it into their library. You may shuffle up to four target cards from your graveyard into your library.",
        )
        .id();
    let enchantment = scenario
        .add_enchantment_from_oracle(P1, "Opposing Enchantment", "")
        .id();
    let graveyard_card = scenario
        .add_creature_to_graveyard(P0, "My Graveyard Card", 2, 2)
        .id();
    scenario.add_card_to_library_top(P0, "P0 Library Card");
    scenario.add_card_to_library_top(P1, "P1 Library Card");
    let mut runner = scenario.build();

    let outcome = runner
        .cast(parting)
        .target_objects(&[enchantment, graveyard_card])
        .accept_optional()
        .resolve();
    let mut events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut events);

    // Reach guards: both moves happened, each into its owner's library.
    assert!(in_library(&runner, P1, enchantment));
    assert!(in_library(&runner, P0, graveyard_card));
    assert_eq!(shuffles(&events, P1), 1, "events: {events:?}");
    assert_eq!(shuffles(&events, P0), 1, "events: {events:?}");
}

/// Guan Yu, Sainted Warrior: "When Guan Yu is put into your graveyard from the battlefield,
/// you may shuffle Guan Yu into your library." Declining shuffles nothing (the terminal
/// shuffle belongs to the declined instruction); accepting shuffles once. The decline leg's
/// reach guard is that the trigger resolved and Guan Yu died.
#[test]
fn optional_shuffle_into_library_shuffles_only_when_accepted() {
    for accept in [false, true] {
        let db = card_db();
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, pool());
        let guan_yu = scenario.add_real_card(P1, "Guan Yu, Sainted Warrior", Zone::Battlefield, db);
        let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
        scenario.add_card_to_library_top(P1, "P1 Library Card");
        let mut runner = scenario.build();

        let cast = runner.cast(murder).target_objects(&[guan_yu]);
        let outcome = if accept {
            cast.accept_optional().resolve()
        } else {
            cast.decline_optional().resolve()
        };
        let mut events = outcome.events().to_vec();
        drain_stack(&mut runner, &mut events);

        if accept {
            assert!(in_library(&runner, P1, guan_yu), "accept leg: reach guard");
            assert_eq!(shuffles(&events, P1), 1, "events: {events:?}");
        } else {
            assert_eq!(
                runner.state().objects[&guan_yu].zone,
                Zone::Graveyard,
                "decline leg: Guan Yu died and stayed in the graveyard"
            );
            // Reach guard: Murder AND Guan Yu's trigger both resolved (it fired and was declined).
            let resolved = events
                .iter()
                .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                .count();
            assert_eq!(resolved, 2, "events: {events:?}");
            assert_eq!(shuffles(&events, P1), 0, "events: {events:?}");
        }
    }
}

/// A shuffle-into-library chain must read only the owners it published itself. After a
/// Worldspine Wurm resolution shuffled P1, a later Nexus of Fate (whose redirect publishes
/// nothing) must not shuffle P1 again off the Wurm's leftover population.
#[test]
fn a_later_shuffle_instruction_does_not_reuse_an_earlier_resolutions_owners() {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    let wurm = scenario.add_real_card(P1, "Worldspine Wurm", Zone::Battlefield, db);
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    let nexus = scenario.add_real_card(P0, "Nexus of Fate", Zone::Hand, db);
    scenario.add_card_to_library_top(P0, "P0 Library Card");
    scenario.add_card_to_library_top(P1, "P1 Library Card");
    let mut runner = scenario.build();

    let outcome = runner.cast(murder).target_objects(&[wurm]).resolve();
    let mut wurm_events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut wurm_events);
    // Reach guard: the Wurm resolution shuffled P1 once and left its population behind.
    assert!(in_library(&runner, P1, wurm));
    assert_eq!(shuffles(&wurm_events, P1), 1, "events: {wurm_events:?}");

    let outcome = runner.cast(nexus).resolve();
    let mut nexus_events = outcome.events().to_vec();
    drain_stack(&mut runner, &mut nexus_events);
    // Reach guard: Nexus of Fate went back into its owner's (P0's) library.
    assert!(in_library(&runner, P0, nexus));
    assert_eq!(shuffles(&nexus_events, P0), 1, "events: {nexus_events:?}");
    assert_eq!(shuffles(&nexus_events, P1), 0, "events: {nexus_events:?}");
}
