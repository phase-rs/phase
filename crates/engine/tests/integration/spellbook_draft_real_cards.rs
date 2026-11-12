//! Real-card runtime coverage for Alchemy spellbooks (digital-only; the
//! Comprehensive Rules have no entry for spellbooks, drafting or conjuring).
//!
//! Every card here is a printed card loaded from the card-data fixture, and
//! every spellbook is the card's real MTGJSON `relatedCards.spellbook` list —
//! no test seeds a spellbook. Tests choose from the presented `options`; none
//! hardcodes which entries a draft offers.
//!
//! Cards (verbatim Oracle text, MTGJSON AtomicCards):
//!
//! * Arms Scavenger — "At the beginning of your upkeep, draft a card from this
//!   creature's spellbook, then exile it. Until end of turn, you may play that
//!   card.\nEquip abilities you activate cost {1} less to activate."
//! * Tibalt, Wicked Tormentor — "[+1]: Add {R}{R}. Draft a card from Tibalt,
//!   Wicked Tormentor's spellbook, then exile it. Until end of turn, you may
//!   cast that card.\n[+1]: Tibalt, Wicked Tormentor deals 4 damage to target
//!   creature or planeswalker unless its controller has Tibalt deal 4 damage
//!   to them. If they do, you may discard a card. If you do, draw a
//!   card.\n[−X]: Create X 1/1 red Devil creature tokens with "When this
//!   creature dies, it deals 1 damage to any target.""
//! * Kayla's Kindling — "When Kayla's Kindling enters, it deals 2 damage to
//!   any target.\nAt the beginning of your upkeep, draft a card from Kayla's
//!   Kindling's spellbook and exile it. Until end of turn, you may cast that
//!   card."
//! * The War Doctor — "Whenever one or more other permanents phase out and
//!   whenever one or more other cards are put into exile from anywhere, put a
//!   time counter on The War Doctor.\nWhenever The War Doctor attacks, it deals
//!   damage equal to the number of time counters on it to any target. If a
//!   creature dealt damage this way would die this turn, exile it instead."
//! * Follow the Tracks — "Conjure a card of your choice from Follow the
//!   Tracks's spellbook onto the battlefield."
//! * Goblin Influx Array — "Goblin spells you cast cost {R} less to cast. (If
//!   it doesn't require {R}, it costs {1} less to cast.)\nAt the beginning of
//!   your end step, conjure a random card from Goblin Influx Array's spellbook
//!   into your hand."
//! * Quasiduplicate — "Create a token that's a copy of target creature you
//!   control.\nJump-start (You may cast this card from your graveyard by
//!   discarding a card in addition to paying its other costs. Then exile this
//!   card.)"
//! * Shock — "Shock deals 2 damage to any target."
//! * Basic lands (Forest, Island, Mountain) for mana and library padding.
//!
//! A draft ("draft a card from X's spellbook") offers three distinct entries
//! of the book chosen at random and the controller picks one; "a card of your
//! choice" offers the whole book; "a random card" picks with no prompt. The
//! exile riders are read literally (CR 608.2c, instructions in the order
//! written): the card is drafted into hand, then exiled from hand.

use std::collections::{HashMap, HashSet};

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{CardPlayMode, CastingPermission, Duration, EffectKind};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

type CardDatabase = engine::database::CardDatabase;

/// Fixed seed: the game RNG is deterministic, so every draft offer below is
/// reproducible.
const SEED: u64 = 7;

/// Arms Scavenger's spellbook (MTGJSON `relatedCards.spellbook`).
const ARMS_SCAVENGER_SPELLBOOK: [&str; 15] = [
    "Boots of Speed",
    "Ceremonial Knife",
    "Cliffhaven Kitesail",
    "Colossus Hammer",
    "Dueling Rapier",
    "Goldvein Pick",
    "Jousting Lance",
    "Mask of Immolation",
    "Mirror Shield",
    "Relic Axe",
    "Rogue's Gloves",
    "Scavenged Blade",
    "Shield of the Realm",
    "Spare Dagger",
    "Tormentor's Helm",
];

/// Tibalt, Wicked Tormentor's spellbook (MTGJSON `relatedCards.spellbook`).
const TIBALT_WICKED_TORMENTOR_SPELLBOOK: [&str; 15] = [
    "Brimstone Vandal",
    "Chained Brute",
    "Charmbreaker Devils",
    "Devil's Play",
    "Festival Crasher",
    "Forge Devil",
    "Frenzied Devils",
    "Havoc Jester",
    "Hellrider",
    "Hobblefiend",
    "Pitchburn Devils",
    "Sin Prodder",
    "Spiteful Prankster",
    "Tibalt's Rager",
    "Torch Fiend",
];

/// Kayla's Kindling's spellbook (MTGJSON `relatedCards.spellbook`).
const KAYLAS_KINDLING_SPELLBOOK: [&str; 15] = [
    "Abrade",
    "Banefire",
    "Cleansing Wildfire",
    "Crackle with Power",
    "Dualcaster Mage",
    "Electrodominance",
    "Explosive Singularity",
    "Guttersnipe",
    "Lightning Bolt",
    "Seasoned Pyromancer",
    "Siege-Gang Commander",
    "Terror of the Peaks",
    "Unexpected Windfall",
    "Volcanic Fallout",
    "Young Pyromancer",
];

/// Follow the Tracks's spellbook (MTGJSON `relatedCards.spellbook`).
const FOLLOW_THE_TRACKS_SPELLBOOK: [&str; 5] = [
    "Gate of the Black Dragon",
    "Gate to Manorborn",
    "Gate to Seatower",
    "Gate to the Citadel",
    "Gate to Tumbledown",
];

/// Goblin Influx Array's spellbook (MTGJSON `relatedCards.spellbook`).
const GOBLIN_INFLUX_ARRAY_SPELLBOOK: [&str; 15] = [
    "Battle Cry Goblin",
    "Beetleback Chief",
    "Brash Taunter",
    "Ember Hauler",
    "Fanatical Firebrand",
    "Goblin Arsonist",
    "Goblin Chieftain",
    "Goblin Instigator",
    "Goblin Trashmaster",
    "Goblin Warchief",
    "Legion Warboss",
    "Reckless Ringleader",
    "Relic Robber",
    "Skirk Prospector",
    "Wily Goblin",
];

/// Tibalt's spellbook entries castable with the {R}{R} his +1 adds that also
/// survive on a creature-less board: none has an enters trigger, a cast
/// condition or a target. Forge Devil is excluded — its enters trigger must
/// target itself on such a board and it dies (CR 704.5g).
const TIBALT_RR_CASTABLE: [&str; 5] = [
    "Chained Brute",
    "Festival Crasher",
    "Hobblefiend",
    "Tibalt's Rager",
    "Torch Fiend",
];

/// Library padding per player, so no draw step decks a player.
const LIBRARY_PAD: usize = 8;

/// Upper bound on single actions a drive may take; a turn cycle needs far fewer.
const DRIVE_CAP: usize = 200;

/// A Historic (Arena) game at `phase` of P0's turn with both libraries padded
/// with real basic lands.
fn scenario(phase: Phase, db: &CardDatabase) -> GameScenario {
    let mut scenario = GameScenario::new_with_format(FormatConfig::historic(), 2, SEED);
    scenario.at_phase(phase);
    for player in [P0, P1] {
        for _ in 0..LIBRARY_PAD {
            scenario.add_real_card(player, "Island", Zone::Library, db);
        }
    }
    scenario
}

/// Builds the scenario and rehydrates it from the card database, which seeds
/// the outside-the-game registry the drafted cards are conjured from.
fn build(scenario: GameScenario, db: &CardDatabase) -> GameRunner {
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
}

fn names(book: &[&str]) -> Vec<String> {
    book.iter().map(|name| name.to_string()).collect()
}

/// Reach-guard: the real data pipeline delivered the printed spellbook.
fn assert_real_spellbook(runner: &GameRunner, source: ObjectId, book: &[&str]) {
    assert_eq!(
        runner.state().objects[&source].spellbook,
        names(book),
        "the source must carry its printed MTGJSON spellbook"
    );
}

/// What a drive saw on the way to its stop condition.
#[derive(Default)]
struct DriveLog {
    events: Vec<GameEvent>,
    order_prompts: usize,
}

/// Drives the game through priority passes, trigger ordering (identity order,
/// through `GameAction::OrderTriggers`) and empty attack declarations until
/// `stop` holds. Any other prompt is a test-harness failure.
fn drive_until(runner: &mut GameRunner, mut stop: impl FnMut(&GameState) -> bool) -> DriveLog {
    let mut log = DriveLog::default();
    for _ in 0..DRIVE_CAP {
        if stop(runner.state()) {
            return log;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                log.order_prompts += 1;
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            other => panic!("drive_until: unexpected prompt {other:?}"),
        };
        let result = runner
            .act(action)
            .expect("the drive's action must be accepted");
        log.events.extend(result.events);
    }
    panic!(
        "drive_until: the stop condition never held; waiting for {:?}",
        runner.state().waiting_for
    );
}

fn at_draft(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::SpellbookDraft { .. })
}

fn settled(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty()
}

/// The pending draft's `(player, source, options)`.
fn pending_draft(state: &GameState) -> (PlayerId, ObjectId, Vec<String>) {
    match &state.waiting_for {
        WaitingFor::SpellbookDraft {
            player,
            source_id,
            options,
            ..
        } => (*player, *source_id, options.clone()),
        other => panic!("expected a spellbook draft, got {other:?}"),
    }
}

/// Asserts `options` is a three-card draft offer of distinct members of `book`.
fn assert_three_card_offer(options: &[String], book: &[&str]) {
    assert_eq!(options.len(), 3, "a draft shows three cards: {options:?}");
    let distinct: HashSet<&str> = options.iter().map(String::as_str).collect();
    assert_eq!(
        distinct.len(),
        3,
        "the offered cards are distinct: {options:?}"
    );
    assert!(
        distinct.iter().all(|name| book.contains(name)),
        "every offered card is a member of the source's spellbook: {options:?}"
    );
}

fn submit(runner: &mut GameRunner, card: &str) -> Vec<GameEvent> {
    runner
        .act(GameAction::SubmitSpellbookDraft {
            card: card.to_string(),
        })
        .expect("an offered card must be accepted")
        .events
}

/// The single object named `name`.
fn object_named(state: &GameState, name: &str) -> ObjectId {
    let ids: Vec<ObjectId> = state
        .objects
        .values()
        .filter(|object| object.name == name)
        .map(|object| object.id)
        .collect();
    assert_eq!(ids.len(), 1, "exactly one object named {name}: {ids:?}");
    ids[0]
}

/// The `PlayFromExile` permissions on `object`.
fn exile_grants(state: &GameState, object: ObjectId) -> Vec<&CastingPermission> {
    state.objects[&object]
        .casting_permissions
        .iter()
        .filter(|permission| matches!(permission, CastingPermission::PlayFromExile { .. }))
        .collect()
}

/// Asserts `object` holds exactly one until-end-of-turn exile grant with
/// `mode`, stamped with `source`.
fn assert_single_grant(state: &GameState, object: ObjectId, mode: CardPlayMode, source: ObjectId) {
    let grants = exile_grants(state, object);
    assert_eq!(grants.len(), 1, "exactly one exile grant: {grants:?}");
    match grants[0] {
        CastingPermission::PlayFromExile {
            duration,
            mode: granted,
            source_id,
            ..
        } => {
            // CR 611.2a + CR 514.2: an "until end of turn" effect lasts as stated and ends in the cleanup step.
            assert_eq!(*duration, Duration::UntilEndOfTurn);
            assert_eq!(*granted, mode);
            assert_eq!(*source_id, Some(source), "the grant names its own source");
        }
        other => panic!("expected PlayFromExile, got {other:?}"),
    }
}

/// Asserts `events` move `object` from hand to exile.
fn assert_exiled_from_hand(events: &[GameEvent], object: ObjectId) {
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Hand),
                to: Zone::Exile,
                ..
            } if *object_id == object
        )),
        "CR 608.2c + CR 701.13a: the drafted card goes to hand, then is exiled from there"
    );
}

/// Arms Scavenger (plus `extra` permanents) on P0's battlefield at untap,
/// driven to its upkeep draft (CR 503.1a). Returns the runner and Arms's id.
fn arms_upkeep_draft(db: &CardDatabase, extra: &[&str]) -> (GameRunner, ObjectId) {
    let mut scenario = scenario(Phase::Untap, db);
    let arms = scenario.add_real_card(P0, "Arms Scavenger", Zone::Battlefield, db);
    for name in extra {
        scenario.add_real_card(P0, name, Zone::Battlefield, db);
    }
    let mut runner = build(scenario, db);
    assert_real_spellbook(&runner, arms, &ARMS_SCAVENGER_SPELLBOOK);
    runner.advance_to_upkeep();
    drive_until(&mut runner, at_draft);
    (runner, arms)
}

/// R1: Arms Scavenger's draft offers three distinct members of its real
/// 15-card spellbook, and a real member outside the offer is not a legal pick.
#[test]
fn arms_scavenger_draft_offers_three_distinct_members_of_its_spellbook() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let (mut runner, arms) = arms_upkeep_draft(db, &[]);
    let (player, source, options) = pending_draft(runner.state());
    assert_eq!(player, P0);
    assert_eq!(source, arms);
    assert_three_card_offer(&options, &ARMS_SCAVENGER_SPELLBOOK);

    // A member of the book that was not offered is rejected, and the offer is
    // neither resampled nor dropped.
    let outside = ARMS_SCAVENGER_SPELLBOOK
        .iter()
        .find(|name| !options.iter().any(|offered| offered == *name))
        .expect("a 15-card book has members outside a 3-card offer");
    assert!(
        runner
            .act(GameAction::SubmitSpellbookDraft {
                card: outside.to_string(),
            })
            .is_err(),
        "{outside} was not offered, so it cannot be drafted"
    );
    assert_eq!(pending_draft(runner.state()).2, options, "the offer stands");
    assert!(
        runner.state().objects.values().all(|o| o.name != *outside),
        "the rejected card was not created"
    );

    // Reach: an offered card is accepted.
    submit(&mut runner, &options[0]);
    object_named(runner.state(), &options[0]);
}

/// R2: Follow the Tracks ("a card of your choice") keeps the whole-book choice.
#[test]
fn follow_the_tracks_offers_its_whole_spellbook() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let mut scenario = scenario(Phase::PreCombatMain, db);
    let spell = scenario.add_real_card(P0, "Follow the Tracks", Zone::Hand, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    }
    let mut runner = build(scenario, db);
    assert_real_spellbook(&runner, spell, &FOLLOW_THE_TRACKS_SPELLBOOK);

    let outcome = runner.cast(spell).resolve();
    let options = match outcome.final_waiting_for() {
        WaitingFor::SpellbookDraft { options, .. } => options.clone(),
        other => panic!("expected the spellbook choice, got {other:?}"),
    };
    let offered: HashSet<&str> = options.iter().map(String::as_str).collect();
    assert_eq!(
        offered,
        HashSet::from(FOLLOW_THE_TRACKS_SPELLBOOK),
        "\"a card of your choice\" offers the whole spellbook"
    );

    submit(&mut runner, &options[0]);
    let gate = object_named(runner.state(), &options[0]);
    assert_eq!(runner.state().objects[&gate].zone, Zone::Battlefield);
    assert!(settled(runner.state()), "{:?}", runner.state().waiting_for);
}

/// R3: Goblin Influx Array ("a random card") conjures one book entry into hand
/// with no prompt.
#[test]
fn goblin_influx_array_conjures_a_random_book_entry_without_a_prompt() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let mut scenario = scenario(Phase::PreCombatMain, db);
    let array = scenario.add_real_card(P0, "Goblin Influx Array", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    assert_real_spellbook(&runner, array, &GOBLIN_INFLUX_ARRAY_SPELLBOOK);

    drive_until(&mut runner, |state| state.phase == Phase::End);
    let hand_before: HashSet<ObjectId> = runner.state().players[0].hand.iter().copied().collect();
    drive_until(&mut runner, |state| {
        assert!(!at_draft(state), "a random conjure never prompts");
        settled(state)
    });
    let state = runner.state();
    let new_cards: Vec<&str> = state.players[0]
        .hand
        .iter()
        .filter(|id| !hand_before.contains(id))
        .map(|id| state.objects[id].name.as_str())
        .collect();
    assert_eq!(new_cards.len(), 1, "one card is conjured: {new_cards:?}");
    assert!(
        GOBLIN_INFLUX_ARRAY_SPELLBOOK.contains(&new_cards[0]),
        "the conjured card is a member of the spellbook: {new_cards:?}"
    );
}

/// R4: the AI's legal picks are exactly the offer.
#[test]
fn ai_legal_draft_picks_are_exactly_the_offer() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let (runner, _) = arms_upkeep_draft(db, &[]);
    let (_, _, options) = pending_draft(runner.state());
    let picks: HashSet<String> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::SubmitSpellbookDraft { card } => Some(card),
            _ => None,
        })
        .collect();
    assert_eq!(picks, options.iter().cloned().collect::<HashSet<_>>());
    assert!(
        ARMS_SCAVENGER_SPELLBOOK
            .iter()
            .filter(|name| !options.iter().any(|offered| offered == *name))
            .all(|name| !picks.contains(*name)),
        "the twelve unoffered members are not legal picks"
    );
}

/// R5: the offer is private to the drafting player.
#[test]
fn the_draft_offer_is_hidden_from_other_viewers() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let (runner, _) = arms_upkeep_draft(db, &[]);
    let (_, _, options) = pending_draft(runner.state());
    assert_eq!(
        pending_draft(&filter_state_for_viewer(runner.state(), P0)).2,
        options,
        "the drafting player sees the offer"
    );
    let (player, _, hidden) = pending_draft(&filter_state_for_viewer(runner.state(), P1));
    assert_eq!(player, P0, "the opponent still sees who is drafting");
    assert!(
        hidden.is_empty(),
        "the opponent does not see the offer: {hidden:?}"
    );
}

/// R6 + R7: Arms Scavenger drafts into hand, the exile instruction moves the
/// drafted card from hand to exile (CR 608.2c + CR 701.13a), and "that card"
/// is the card in exile (CR 400.7j), playable until end of turn (CR 611.2a).
#[test]
fn arms_scavenger_drafts_into_hand_then_exiles_it_with_a_play_grant() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let (mut runner, arms) = arms_upkeep_draft(db, &[]);
    let (_, _, options) = pending_draft(runner.state());
    let events = submit(&mut runner, &options[0]);

    let state = runner.state();
    let drafted = object_named(state, &options[0]);
    assert_exiled_from_hand(&events, drafted);
    assert_eq!(state.objects[&drafted].zone, Zone::Exile);
    assert!(!state.players[0].hand.contains(&drafted));
    assert_single_grant(state, drafted, CardPlayMode::Play, arms);
    assert!(settled(state), "{:?}", state.waiting_for);
}

/// R8: a printed observer of cards put into exile (The War Doctor) sees the
/// drafted card's hand-to-exile move.
#[test]
fn the_war_doctor_counts_the_drafted_cards_exile() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let (mut runner, _) = arms_upkeep_draft(db, &["The War Doctor"]);
    let doctor = object_named(runner.state(), "The War Doctor");
    assert!(
        runner.state().objects[&doctor]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| entry.definition.mode == TriggerMode::ChangesZone
                && entry.definition.destination == Some(Zone::Exile)),
        "reach-guard: The War Doctor observes cards put into exile"
    );
    let (_, _, options) = pending_draft(runner.state());
    submit(&mut runner, &options[0]);
    drive_until(&mut runner, settled);
    assert_eq!(
        runner.state().objects[&doctor]
            .counters
            .get(&CounterType::Time)
            .copied()
            .unwrap_or(0),
        1,
        "the drafted card was put into exile from hand"
    );
}

/// R9: Kayla's Kindling (" and exile it") takes the same literal
/// decomposition, with a cast grant.
#[test]
fn kaylas_kindling_drafts_into_hand_then_exiles_it_with_a_cast_grant() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let mut scenario = scenario(Phase::Untap, db);
    let kayla = scenario.add_real_card(P0, "Kayla's Kindling", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    assert_real_spellbook(&runner, kayla, &KAYLAS_KINDLING_SPELLBOOK);
    runner.advance_to_upkeep();
    drive_until(&mut runner, at_draft);
    let (_, source, options) = pending_draft(runner.state());
    assert_eq!(source, kayla);
    assert_three_card_offer(&options, &KAYLAS_KINDLING_SPELLBOOK);

    let events = submit(&mut runner, &options[0]);
    let state = runner.state();
    let drafted = object_named(state, &options[0]);
    assert_exiled_from_hand(&events, drafted);
    assert_eq!(state.objects[&drafted].zone, Zone::Exile);
    assert_single_grant(state, drafted, CardPlayMode::Cast, kayla);
}

/// R10: Tibalt's first +1 end to end — loyalty (CR 606.4), the {R}{R}
/// (CR 106.4; a loyalty ability is not a mana ability, CR 605.1a), the draft,
/// and casting the drafted card from exile with that mana. Activated at
/// sorcery speed in P0's main phase (CR 606.3).
#[test]
fn tibalt_plus_one_drafts_exiles_and_casts_the_card() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let mut scenario = scenario(Phase::PreCombatMain, db);
    let tibalt = scenario.add_real_card(P0, "Tibalt, Wicked Tormentor", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    assert_real_spellbook(&runner, tibalt, &TIBALT_WICKED_TORMENTOR_SPELLBOOK);
    assert_eq!(runner.state().objects[&tibalt].loyalty, Some(3));

    let outcome = runner.activate(tibalt, 0).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::SpellbookDraft { .. }
        ),
        "the +1 halts at the draft, got {:?}",
        outcome.final_waiting_for()
    );
    let (_, source, options) = pending_draft(runner.state());
    assert_eq!(source, tibalt);
    assert_three_card_offer(&options, &TIBALT_WICKED_TORMENTOR_SPELLBOOK);
    // Reach-guard (deterministic under SEED): the offer holds a card the {R}{R}
    // can cast on this creature-less board.
    let pick = options
        .iter()
        .find(|name| TIBALT_RR_CASTABLE.contains(&name.as_str()))
        .unwrap_or_else(|| panic!("SEED must offer a castable card: {options:?}"))
        .clone();

    let events = submit(&mut runner, &pick);
    let state = runner.state();
    assert!(settled(state), "{:?}", state.waiting_for);
    assert_eq!(
        state.objects[&tibalt].loyalty,
        Some(4),
        "CR 606.4: +1 loyalty"
    );
    assert_eq!(
        state.players[0].mana_pool.count_color(ManaType::Red),
        2,
        "the +1 added {{R}}{{R}}"
    );
    let drafted = object_named(state, &pick);
    assert_exiled_from_hand(&events, drafted);
    assert_single_grant(state, drafted, CardPlayMode::Cast, tibalt);

    let cast = runner.cast(drafted).resolve();
    assert_eq!(
        cast.zone_of(drafted),
        Zone::Battlefield,
        "the drafted card is cast from exile and resolves"
    );
}

/// R11: a later resolution's grant cannot reach an earlier pick. Pick A's
/// grant ends at cleanup (CR 514.2); next upkeep's grant names only pick B.
#[test]
fn a_later_draft_does_not_regrant_an_earlier_pick() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let (mut runner, arms) = arms_upkeep_draft(db, &[]);
    let first_turn = runner.state().turn_number;
    let (_, _, first_offer) = pending_draft(runner.state());
    submit(&mut runner, &first_offer[0]);
    let pick_a = object_named(runner.state(), &first_offer[0]);
    assert_single_grant(runner.state(), pick_a, CardPlayMode::Play, arms);

    drive_until(&mut runner, |state| state.active_player == P1);
    assert!(
        exile_grants(runner.state(), pick_a).is_empty(),
        "CR 514.2: pick A's until-end-of-turn grant ended at cleanup"
    );

    drive_until(&mut runner, at_draft);
    assert_eq!(runner.state().active_player, P0);
    assert!(runner.state().turn_number > first_turn);
    let (_, source, second_offer) = pending_draft(runner.state());
    assert_eq!(source, arms);
    // Choose a card other than A's name so the two picks are distinct objects
    // by name (the offers sample independently).
    let pick_name = second_offer
        .iter()
        .find(|name| **name != first_offer[0])
        .expect("a three-card offer holds a card other than pick A")
        .clone();
    submit(&mut runner, &pick_name);
    let state = runner.state();
    let pick_b = object_named(state, &pick_name);
    assert_single_grant(state, pick_b, CardPlayMode::Play, arms);
    assert_eq!(
        state.objects[&pick_a].zone,
        Zone::Exile,
        "pick A is still in exile"
    );
    assert!(
        exile_grants(state, pick_a).is_empty(),
        "this resolution's grant names only its own pick"
    );
}

/// R12: two printed sources in one upkeep — each grant binds only its own
/// source's pick. The two triggers differ, so P0 orders them (CR 603.3b).
#[test]
fn two_sources_in_one_upkeep_each_grant_only_their_own_pick() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let mut scenario = scenario(Phase::Untap, db);
    let arms = scenario.add_real_card(P0, "Arms Scavenger", Zone::Battlefield, db);
    let kayla = scenario.add_real_card(P0, "Kayla's Kindling", Zone::Battlefield, db);
    let mut runner = build(scenario, db);
    assert_real_spellbook(&runner, arms, &ARMS_SCAVENGER_SPELLBOOK);
    assert_real_spellbook(&runner, kayla, &KAYLAS_KINDLING_SPELLBOOK);
    runner.advance_to_upkeep();

    let mut order_prompts = 0;
    let mut picks: HashMap<ObjectId, ObjectId> = HashMap::new();
    for _ in 0..2 {
        order_prompts += drive_until(&mut runner, at_draft).order_prompts;
        let (_, source, options) = pending_draft(runner.state());
        let book: &[&str] = if source == arms {
            &ARMS_SCAVENGER_SPELLBOOK
        } else {
            assert_eq!(source, kayla);
            &KAYLAS_KINDLING_SPELLBOOK
        };
        assert_three_card_offer(&options, book);
        submit(&mut runner, &options[0]);
        picks.insert(source, object_named(runner.state(), &options[0]));
    }
    drive_until(&mut runner, settled);
    assert!(
        order_prompts >= 1,
        "reach-guard: P0 ordered the two upkeep triggers"
    );

    let state = runner.state();
    assert_single_grant(state, picks[&arms], CardPlayMode::Play, arms);
    assert_single_grant(state, picks[&kayla], CardPlayMode::Cast, kayla);
}

/// R13: "then exile it" reaches only what its own draft delivered — never an
/// object an earlier instruction created, nor an earlier resolution's pick.
///
/// Board: Quasiduplicate's token copy of Arms Scavenger. A copy acquires the
/// copiable values of the printed card (CR 707.2) — including the draft
/// instruction, whose "this creature's spellbook" names the copy itself
/// (CR 201.5) — but the engine does not carry a spellbook through copies yet
/// (F11), so the copy's draft delivers nothing today. Shock removes the
/// original (CR 704.5g) so only the copy's upkeep trigger fires.
///
/// Outcome assertions (a)–(d) hold under both copy-path readings: whether the
/// copy's draft delivers nothing or a new card that existed nowhere before
/// this upkeep. Reach-guards (1)–(4) describe the board while copies carry no
/// spellbook; reach-guard (1) is expected to fail when the copy fix lands, and
/// this row then becomes the positive "a copy drafts from the copied card's
/// book" row.
#[test]
fn an_empty_draft_exiles_and_grants_nothing() {
    let db = shared_card_db().expect("spellbook draft tests require the integration card database");
    let mut scenario = scenario(Phase::Untap, db);
    let arms = scenario.add_real_card(P0, "Arms Scavenger", Zone::Battlefield, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    }
    scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    let quasiduplicate = scenario.add_real_card(P0, "Quasiduplicate", Zone::Hand, db);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let mut runner = build(scenario, db);
    assert_real_spellbook(&runner, arms, &ARMS_SCAVENGER_SPELLBOOK);

    // Turn N: Arms drafts pick A.
    runner.advance_to_upkeep();
    drive_until(&mut runner, at_draft);
    let first_turn = runner.state().turn_number;
    let (_, _, offer) = pending_draft(runner.state());
    submit(&mut runner, &offer[0]);
    let pick_a = object_named(runner.state(), &offer[0]);
    assert_single_grant(runner.state(), pick_a, CardPlayMode::Play, arms);

    // Main phase: copy Arms, then Shock the original.
    drive_until(&mut runner, |state| {
        state.phase == Phase::PreCombatMain && settled(state)
    });
    runner.cast(quasiduplicate).target_object(arms).resolve();
    let token = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let object = &runner.state().objects[id];
            object.is_token && object.name == "Arms Scavenger"
        })
        .expect("Quasiduplicate creates a token copy of Arms Scavenger");
    runner.cast(shock).target_object(arms).resolve();

    // Advance through P1's turn to P0's next upkeep, the copy's trigger waiting.
    drive_until(&mut runner, |state| {
        state.active_player == P0
            && state.turn_number > first_turn
            && state.phase == Phase::Upkeep
            && !state.stack.is_empty()
    });

    // Reach-guard (1), conditional on F11: the copy has Arms's draft trigger
    // and no spellbook.
    let token_object = &runner.state().objects[&token];
    assert!(
        token_object
            .trigger_definitions
            .iter_unchecked()
            .any(
                |entry| entry.definition.execute.as_deref().is_some_and(|execute| {
                    matches!(
                        &*execute.effect,
                        engine::types::ability::Effect::DraftFromSpellbook { .. }
                    )
                })
            ),
        "reach-guard (1): the token copy has Arms Scavenger's upkeep draft"
    );
    assert!(
        token_object.spellbook.is_empty(),
        "reach-guard (1): copies carry no spellbook yet (F11)"
    );
    // Reach-guard (2): the hazard is live — the copy is the last created object.
    assert_eq!(runner.state().last_created_token_ids, vec![token]);
    // Reach-guard (4): only the copy triggers.
    assert!(
        runner.state().players[0]
            .graveyard
            .iter()
            .any(|id| runner.state().objects[id].name == "Arms Scavenger"),
        "reach-guard (4): the original Arms Scavenger died to Shock"
    );

    let permissions_before: HashMap<ObjectId, Vec<CastingPermission>> = runner
        .state()
        .objects
        .values()
        .map(|object| (object.id, object.casting_permissions.clone()))
        .collect();
    let upkeep = drive_until(&mut runner, settled);

    // Reach-guard (3): the copy's draft resolved (the empty-book arm).
    assert!(
        upkeep.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::DraftFromSpellbook,
                source_id,
                ..
            } if *source_id == token
        )),
        "reach-guard (3): the token copy's draft resolved"
    );

    let state = runner.state();
    // (a) The copy is not exiled.
    assert_eq!(
        state.objects.get(&token).map(|object| object.zone),
        Some(Zone::Battlefield),
        "(a) the token copy stays on the battlefield"
    );
    // (b) No permission names the copy itself.
    assert!(
        state.objects[&token].casting_permissions.is_empty(),
        "(b) the token copy holds no casting permission"
    );
    // (c) The earlier pick is not re-granted.
    assert!(
        exile_grants(state, pick_a).is_empty(),
        "(c) pick A gets no grant from this resolution"
    );
    // (d) Nothing that existed before this upkeep moved to exile or gained a
    // permission.
    assert!(
        !upkeep.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Exile, .. }
                if permissions_before.contains_key(object_id)
        )),
        "(d) no pre-upkeep object was exiled"
    );
    for (id, before) in &permissions_before {
        if let Some(object) = state.objects.get(id) {
            assert_eq!(
                &object.casting_permissions, before,
                "(d) {} gained no permission",
                object.name
            );
        }
    }
}
