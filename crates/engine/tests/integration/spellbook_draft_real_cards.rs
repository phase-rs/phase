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
use engine::types::ability::{
    CardPlayMode, CastingPermission, ChoiceType, ChosenAttribute, CopyTargetPurpose, Duration,
    EffectKind, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaChoice, PendingLiminalEntryResume, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
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
    scenario_with_seed(phase, db, SEED)
}

fn scenario_with_seed(phase: Phase, db: &CardDatabase, seed: u64) -> GameScenario {
    let mut scenario = GameScenario::new_with_format(FormatConfig::historic(), 2, seed);
    scenario.at_phase(phase);
    for player in [P0, P1] {
        for _ in 0..LIBRARY_PAD {
            scenario.add_real_card(player, "Island", Zone::Library, db);
        }
    }
    scenario
}

/// MTGJSON AtomicCards (5.3.0+20261124) omits these digital faces from the
/// committed integration subset. Parse their printed records at test time.
fn battlefield_spellbook_database() -> &'static CardDatabase {
    use engine::database::{mtgjson::AtomicCard, synthesis::build_oracle_face};
    use std::sync::OnceLock;

    static DATABASE: OnceLock<CardDatabase> = OnceLock::new();
    DATABASE.get_or_init(|| {
        let names = [
            "Forest",
            "Island",
            "Swamp",
            "Spelunking",
            "Lotus Cobra",
            "Containment Priest",
            "Elemental Bond",
            "Evendo, Waking Haven",
            "Uthros, Titanic Godcore",
            "Barkhide Troll",
            "Adaptive Automaton",
            "Anointed Peacekeeper",
            "Consider",
            "Glorious Anthem",
            "Llanowar Elves",
            "Mystic Reflection",
            "Opt",
            "Ornithopter",
            "Plains",
            "Hardened Scales",
            "Doubling Season",
            "Murder",
            "Cling to Dust",
            "Rhythm of the Wild",
            "Stonebound Mentor",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect();
        let json = shared_card_db()
            .expect("the committed card fixture")
            .export_subset_json(&names);
        let mut records: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&json).expect("the fixture exports named faces");
        let atomic_cards: Vec<AtomicCard> =
            serde_json::from_str(BATTLEFIELD_SPELLBOOK_ATOMIC).expect("pinned MTGJSON records");
        for card in &atomic_cards {
            let mut face = build_oracle_face(card, card.identifiers.scryfall_oracle_id.clone());
            face.metadata.spellbook = card.related_cards.spellbook.clone();
            records.insert(
                card.name.to_lowercase(),
                serde_json::to_value(face).unwrap(),
            );
        }
        CardDatabase::from_json_str(&serde_json::Value::Object(records).to_string()).unwrap()
    })
}

const PLANETS: [&str; 5] = [
    "Adagia, Windswept Bastion",
    "Evendo, Waking Haven",
    "Kavaron, Memorial World",
    "Susur Secundi, Void Altar",
    "Uthros, Titanic Godcore",
];
const SUSUR: &str = "Susur Secundi, Void Altar";

fn brood_draft(
    spelunking_owner: Option<PlayerId>,
    cobra: bool,
) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let db = battlefield_spellbook_database();
    for seed in 0..32 {
        let mut game = scenario_with_seed(Phase::PreCombatMain, db, seed);
        let brood = game.add_real_card(P0, "Brood Astronomer", Zone::Hand, db);
        let sacrifice = game.add_real_card(P0, "Island", Zone::Battlefield, db);
        for _ in 0..3 {
            game.add_real_card(P0, "Forest", Zone::Battlefield, db);
        }
        let spelunking = spelunking_owner
            .map(|owner| game.add_real_card(owner, "Spelunking", Zone::Battlefield, db));
        if cobra {
            game.add_real_card(P0, "Lotus Cobra", Zone::Battlefield, db);
        }
        let mut runner = build(game, db);
        assert_real_spellbook(&runner, brood, &PLANETS);
        let outcome = runner.cast(brood).commit().stop_at_optional().resolve();
        assert!(
            matches!(
                outcome.final_waiting_for(),
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "expected Brood Astronomer's optional ETB, got {:?}",
            outcome.final_waiting_for(),
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let WaitingFor::EffectZoneChoice {
            cards,
            effect_kind: EffectKind::Sacrifice,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected Brood Astronomer's sacrifice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert!(cards.contains(&sacrifice));
        runner
            .act(GameAction::SelectCards {
                cards: vec![sacrifice],
            })
            .unwrap();
        assert_eq!(runner.state().objects[&sacrifice].zone, Zone::Graveyard);
        let (player, source, offered) = pending_draft(runner.state());
        assert_eq!((player, source), (P0, brood));
        assert_three_card_offer(&offered, &PLANETS);
        if offered.iter().any(|card| card == SUSUR) {
            return (runner, brood, spelunking);
        }
    }
    panic!("the real draft never offered Susur");
}

fn outside_game_entry_receipts(events: &[GameEvent], id: ObjectId, destination: Zone) {
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event, GameEvent::ObjectConjured { object_id, .. } if *object_id == id
            ))
            .count(),
        1
    );
    assert_eq!(events.iter().filter(|event| matches!(
        event, GameEvent::ZoneChanged { object_id, from: None, to, record }
            if *object_id == id && *to == destination && record.entered_incarnation.is_some()
    )).count(), 1);
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::TokenCreated { .. })));
}

fn saved_game_runner(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(runner.state()).expect("entry state serializes");
    GameRunner::from_state(serde_json::from_str(&json).expect("entry state restores"))
}

#[test]
fn brood_astronomer_orders_printed_tap_and_untap_replacements() {
    for spelunking_first in [false, true] {
        let (mut runner, brood, spelunking) = brood_draft(Some(P0), false);
        let spelunking = spelunking.unwrap();
        assert!(runner
            .act(GameAction::SubmitSpellbookDraft {
                card: "Forest".into()
            })
            .is_err());
        assert!(at_draft(runner.state()));
        let events = submit(&mut runner, SUSUR);
        assert!(events
            .iter()
            .all(|event| !matches!(event, GameEvent::ObjectConjured { .. })));
        let state = runner.state();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ));
        let pending = state.pending_replacement.as_ref().unwrap();
        let entry_ref = pending.proposed.affected_object_id().unwrap();
        assert!(state.liminal_entries.contains_key(&entry_ref));
        assert!(!state.objects.contains_key(&entry_ref));
        assert!(!state.battlefield.contains(&entry_ref));
        assert_eq!(
            pending
                .candidates
                .iter()
                .map(|candidate| candidate.source)
                .collect::<HashSet<_>>(),
            HashSet::from([entry_ref, spelunking]),
        );
        let first = if spelunking_first {
            spelunking
        } else {
            entry_ref
        };
        let index = pending
            .candidates
            .iter()
            .position(|candidate| candidate.source == first)
            .unwrap();
        let result = runner.act(GameAction::ChooseReplacement { index }).unwrap();
        outside_game_entry_receipts(&result.events, entry_ref, Zone::Battlefield);
        assert_eq!(runner.state().objects[&entry_ref].tapped, spelunking_first);
        assert!(!runner.state().objects[&brood].tapped);
        assert_eq!(runner.state().last_created_token_ids, vec![entry_ref]);
        assert!(settled(runner.state()), "{:?}", runner.state().waiting_for);
    }
}

#[test]
fn brood_astronomer_without_a_friendly_spelunking_enters_the_planet_tapped() {
    for spelunking_owner in [None, Some(P1)] {
        let (mut runner, brood, _) = brood_draft(spelunking_owner, false);
        let events = submit(&mut runner, SUSUR);
        let entry_ref = object_named(runner.state(), SUSUR);
        outside_game_entry_receipts(&events, entry_ref, Zone::Battlefield);
        assert!(runner.state().objects[&entry_ref].tapped);
        assert!(!runner.state().objects[&brood].tapped);
    }
}

#[test]
fn brood_astronomer_restores_ordering_and_notifies_landfall_once() {
    let (mut runner, _, _) = brood_draft(Some(P0), true);
    submit(&mut runner, SUSUR);
    let opponent = filter_state_for_viewer(runner.state(), P1);
    assert!(opponent.liminal_entries.is_empty());
    assert!(opponent.pending_replacement.is_none());
    assert!(matches!(
        &opponent.waiting_for,
        WaitingFor::ReplacementChoice { candidates, .. } if candidates.iter().all(|option| option.source_name != SUSUR)
    ));
    let mut runner = saved_game_runner(&runner);
    let pending = runner.state().pending_replacement.as_ref().unwrap();
    let entry_ref = pending.proposed.affected_object_id().unwrap();
    let index = pending
        .candidates
        .iter()
        .position(|candidate| candidate.source == entry_ref)
        .unwrap();
    let result = runner.act(GameAction::ChooseReplacement { index }).unwrap();
    outside_game_entry_receipts(&result.events, entry_ref, Zone::Battlefield);
    let cobra = object_named(runner.state(), "Lotus Cobra");
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == cobra)
            .count(),
        1
    );
    assert!(!runner.state().objects[&entry_ref].tapped);
    drive_until(&mut runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::ChooseManaColor { player: P0, .. }
        )
    });
    let before = runner.state().players[0]
        .mana_pool
        .count_color(ManaType::Green);
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .unwrap();
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        before + 1
    );
    assert!(settled(runner.state()));
}

const GARRUK: &str = "Garruk, Wrath of the Wilds";
const TROLL: &str = "Barkhide Troll";
const AUTOMATON: &str = "Adaptive Automaton";
const PEACEKEEPER: &str = "Anointed Peacekeeper";
const MIRROR_IMAGE: &str = "Mirror Image";

fn garruk_draft(extra: &[&str], donor: Option<&str>) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let db = battlefield_spellbook_database();
    for seed in 0..64 {
        let mut game = scenario_with_seed(Phase::PreCombatMain, db, seed);
        let garruk = game.add_real_card(P0, GARRUK, Zone::Battlefield, db);
        for name in extra {
            game.add_real_card(P0, name, Zone::Battlefield, db);
        }
        let spells = donor.map(|donor| {
            let original = game.add_real_card(P0, donor, Zone::Hand, db);
            if donor == MIRROR_IMAGE {
                game.add_real_card(P0, AUTOMATON, Zone::Hand, db);
                game.add_real_card(P0, "Glorious Anthem", Zone::Battlefield, db);
            }
            if donor == PEACEKEEPER {
                game.add_real_card(P1, "Consider", Zone::Hand, db);
                game.add_real_card(P1, "Opt", Zone::Hand, db);
                game.add_real_card(P1, "Island", Zone::Battlefield, db);
                game.add_real_card(P0, "Plains", Zone::Battlefield, db);
            }
            let reflection = game.add_real_card(P0, "Mystic Reflection", Zone::Hand, db);
            game.add_real_card(P0, "Murder", Zone::Hand, db);
            game.add_real_card(P0, "Ornithopter", Zone::Hand, db);
            for _ in 0..9 {
                game.add_real_card(P0, "Island", Zone::Battlefield, db);
            }
            for _ in 0..3 {
                game.add_real_card(P0, "Swamp", Zone::Battlefield, db);
            }
            (original, reflection)
        });
        let mut runner = build(game, db);
        if let (Some(donor), Some((original, reflection))) = (donor, spells) {
            runner.state_mut().all_creature_types = db.creature_type_vocabulary().to_vec();
            match donor {
                PEACEKEEPER => {
                    runner.cast(original).resolve();
                    choose_named(&mut runner, &ChoiceType::opponent(), &P1.0.to_string());
                    choose_card_name(&mut runner, "Consider");
                }
                MIRROR_IMAGE => {
                    let automaton = object_named(runner.state(), AUTOMATON);
                    runner.cast(automaton).resolve();
                    choose_construct(&mut runner);
                    runner.cast(original).replacement_choice(1).resolve();
                    assert_eq!(runner.state().objects[&original].name, MIRROR_IMAGE);
                    assert_eq!(runner.state().objects[&original].zone, Zone::Battlefield);
                }
                AUTOMATON => {
                    runner.cast(original).resolve();
                    choose_construct(&mut runner);
                }
                _ => unreachable!("unsupported reflection witness"),
            }
            assert!(settled(runner.state()));
            runner.cast(reflection).target_object(original).resolve();
            assert!(settled(runner.state()), "{:?}", runner.state().waiting_for);
        }
        runner.activate(garruk, 1).resolve();
        let (player, source, offered) = pending_draft(runner.state());
        assert_eq!((player, source), (P0, garruk));
        if offered.iter().any(|name| name == TROLL) {
            return (runner, garruk, spells.map(|(id, _)| id));
        }
    }
    panic!("the real Garruk draft never offered Barkhide Troll");
}

fn choose_named(runner: &mut GameRunner, expected: &ChoiceType, choice: &str) -> Vec<GameEvent> {
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::NamedChoice {
            choice_type, player: P0, ..
        } if std::mem::discriminant(choice_type) == std::mem::discriminant(expected)),
        "expected named choice {expected:?}, got {:?}",
        runner.state().waiting_for,
    );
    runner
        .act(GameAction::ChooseOption {
            choice: choice.to_owned(),
        })
        .unwrap()
        .events
}

fn choose_construct(runner: &mut GameRunner) {
    choose_named(
        runner,
        &ChoiceType::CreatureType {
            options: Vec::new(),
        },
        "Construct",
    );
}

fn choose_card_name(runner: &mut GameRunner, name: &str) -> Vec<GameEvent> {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CardName { .. },
                player: P0,
                ..
            }
        ),
        "expected a card-name choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseOption {
            choice: name.to_owned(),
        })
        .unwrap()
        .events
}

fn pending_spellbook_entry_child(runner: &GameRunner) -> ObjectId {
    let state = runner.state();
    let Some(PendingLiminalEntryResume::OutsideGameCard {
        source_id,
        player: P0,
        event,
        ..
    }) = &state.pending_liminal_entry_resume
    else {
        panic!(
            "expected an outside-game child resume over {:?}",
            state.waiting_for
        );
    };
    assert!(
        matches!(event, ProposedEvent::OutsideGameCardEntry { entry_ref, .. }
        if entry_ref == source_id)
    );
    assert!(state.liminal_entries.contains_key(source_id));
    assert!(!state.objects.contains_key(source_id));
    assert!(!state.battlefield.contains(source_id));
    *source_id
}

fn assert_entry_unpublished(events: &[GameEvent], id: ObjectId) {
    assert!(events.iter().all(|event| !matches!(event,
        GameEvent::ObjectConjured { object_id, .. } | GameEvent::ZoneChanged { object_id, .. }
        if *object_id == id
    )));
}

fn assert_reflection_spent(runner: &mut GameRunner) {
    let creature = object_named(runner.state(), "Ornithopter");
    runner.cast(creature).resolve();
    let card = &runner.state().objects[&creature];
    assert_eq!(card.name, "Ornithopter");
    assert_eq!(card.zone, Zone::Battlefield);
    assert!(settled(runner.state()));
}

#[test]
fn spellbook_creature_is_redirected_to_exile_by_containment_priest() {
    let (mut runner, _, _) = garruk_draft(&["Containment Priest"], None);
    let priest = object_named(runner.state(), "Containment Priest");
    let mut events = submit(&mut runner, TROLL);
    if let Some(pending) = &runner.state().pending_replacement {
        // The Priest's redirect competes with Barkhide Troll's own counter.
        let index = pending
            .candidates
            .iter()
            .position(|candidate| candidate.source == priest)
            .unwrap();
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index })
                .unwrap()
                .events,
        );
    }
    let id = object_named(runner.state(), TROLL);
    assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    assert!(!runner.state().objects[&id].is_token);
    outside_game_entry_receipts(&events, id, Zone::Exile);
    assert!(!runner.state().battlefield.contains(&id));
    assert!(settled(runner.state()));
}

#[test]
fn spellbook_entry_copy_restores_its_own_name_choice_and_keeps_the_drafted_base() {
    let (mut runner, _, original) = garruk_draft(&[], Some(AUTOMATON));
    let original = original.unwrap();
    let events = submit(&mut runner, TROLL);
    let WaitingFor::NamedChoice {
        source: Some(source),
        player: P0,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Adaptive Automaton's entry choice, got {:?}",
            runner.state().waiting_for
        );
    };
    let id = source.prompt.identity.reference.object_id;
    assert_ne!(original, id);
    assert!(runner.state().liminal_entries.contains_key(&id));
    assert!(!runner.state().objects.contains_key(&id));
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::ObjectConjured { .. })));
    let mut runner = saved_game_runner(&runner);
    let result = runner
        .act(GameAction::ChooseOption {
            choice: "Elf".into(),
        })
        .unwrap();
    outside_game_entry_receipts(&result.events, id, Zone::Battlefield);
    let state = runner.state();
    let copy = &state.objects[&id];
    assert_eq!(copy.name, "Adaptive Automaton");
    assert!(!copy.is_token);
    assert_ne!(
        copy.chosen_attributes,
        state.objects[&original].chosen_attributes
    );
    assert!(format!("{:?}", copy.chosen_attributes).contains("Elf"));
    assert!(format!("{:?}", state.objects[&original].chosen_attributes).contains("Construct"));
    assert!(state.pending_liminal_entry_resume.is_none());
    assert!(state.liminal_entries.is_empty());
    assert!(settled(state));
    assert_reflection_spent(&mut runner);

    // CR 400.7 + CR 707.2: a real move ends the current copy modification.
    let murder = object_named(runner.state(), "Murder");
    runner.cast(murder).target_object(id).resolve();
    let moved = &runner.state().objects[&id];
    assert_eq!(moved.zone, Zone::Graveyard);
    assert_eq!(moved.name, TROLL);
}

#[test]
fn spellbook_entry_restores_the_copied_peacekeepers_own_name_and_cast_tax() {
    let (mut runner, _, original) = garruk_draft(&[], Some(PEACEKEEPER));
    let original = original.unwrap();
    let opt = object_named(runner.state(), "Opt");
    let consider = object_named(runner.state(), "Consider");
    let cost = |runner: &GameRunner, spell, generic| {
        assert_eq!(
            engine::game::casting::effective_spell_cost(runner.state(), P1, spell),
            Some(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic
            }),
        );
    };
    assert_eq!(
        runner.state().objects[&original].chosen_card_name(),
        Some("Consider")
    );
    cost(&runner, consider, 2);
    cost(&runner, opt, 0);

    let events = submit(&mut runner, TROLL);
    let id = pending_spellbook_entry_child(&runner);
    assert_entry_unpublished(&events, id);
    let events = choose_named(&mut runner, &ChoiceType::opponent(), &P1.0.to_string());
    assert_entry_unpublished(&events, id);
    assert_eq!(pending_spellbook_entry_child(&runner), id);
    let mut runner = saved_game_runner(&runner);
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), battlefield_spellbook_database());
    assert_eq!(pending_spellbook_entry_child(&runner), id);
    let events = choose_card_name(&mut runner, "Opt");
    outside_game_entry_receipts(&events, id, Zone::Battlefield);
    let copy = &runner.state().objects[&id];
    assert_eq!(copy.name, PEACEKEEPER);
    assert_eq!(copy.chosen_card_name(), Some("Opt"));
    assert_eq!(
        copy.chosen_attributes,
        [
            ChosenAttribute::Player(P1),
            ChosenAttribute::CardName("Opt".into()),
        ],
    );
    assert!(!copy.is_token);
    assert_eq!(
        runner.state().objects[&original].chosen_card_name(),
        Some("Consider")
    );
    assert!(runner.state().pending_liminal_entry_resume.is_none());
    assert!(runner.state().liminal_entries.is_empty());
    assert!(settled(runner.state()));
    cost(&runner, consider, 2);
    cost(&runner, opt, 2);

    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let card_id = runner.state().objects[&opt].card_id;
    let error = runner
        .act(GameAction::CastSpell {
            object_id: opt,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect_err("the opponent's lone Island cannot pay the copied tax");
    assert!(
        matches!(error, engine::game::engine::EngineError::ActionNotAllowed(message)
        if message == "Cannot pay mana cost")
    );
    assert_eq!(runner.state().objects[&opt].zone, Zone::Hand);
}

#[test]
fn spellbook_entry_restores_copy_target_choice_before_the_new_copied_choice() {
    let (mut runner, _, original) = garruk_draft(&[], Some(MIRROR_IMAGE));
    let original = original.unwrap();
    let automaton = object_named(runner.state(), AUTOMATON);
    let before = submit(&mut runner, TROLL);
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "expected Mirror Image's optional entry replacement, got {:?}",
            runner.state().waiting_for
        );
    };
    let entry_ref = runner
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .proposed
        .affected_object_id()
        .unwrap();
    assert_entry_unpublished(&before, entry_ref);
    assert_eq!(candidates[0].source_id, entry_ref);
    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert_entry_unpublished(&result.events, entry_ref);
    assert_eq!(pending_spellbook_entry_child(&runner), entry_ref);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::CopyTargetChoice {
        player: P0, source_id, valid_targets, purpose: CopyTargetPurpose::BecomeCopy, ..
    } if *source_id == entry_ref && valid_targets.contains(&automaton))
    );
    let mut runner = saved_game_runner(&runner);
    assert_eq!(pending_spellbook_entry_child(&runner), entry_ref);
    let result = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(automaton)),
        })
        .unwrap();
    assert_entry_unpublished(&result.events, entry_ref);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::NamedChoice {
        source: Some(source), choice_type: ChoiceType::CreatureType { .. }, ..
    } if source.prompt.identity.reference.object_id == entry_ref)
    );
    assert!(!runner.state().objects.contains_key(&entry_ref));
    let events = choose_named(
        &mut runner,
        &ChoiceType::CreatureType {
            options: Vec::new(),
        },
        "Elf",
    );
    outside_game_entry_receipts(&events, entry_ref, Zone::Battlefield);
    let state = runner.state();
    assert_eq!(state.objects[&entry_ref].name, AUTOMATON);
    assert_eq!(
        state.objects[&entry_ref].chosen_creature_type(),
        Some("Elf")
    );
    assert!(!state.objects[&entry_ref].is_token);
    assert_eq!(
        state.objects[&automaton].chosen_creature_type(),
        Some("Construct")
    );
    assert_eq!(state.objects[&original].name, MIRROR_IMAGE);
    assert!(state.pending_liminal_entry_resume.is_none());
    assert!(state.liminal_entries.is_empty());
    assert!(settled(state));
    assert_reflection_spent(&mut runner);

    let (mut control, _, _) = garruk_draft(&[], None);
    let events = submit(&mut control, TROLL);
    let troll = object_named(control.state(), TROLL);
    outside_game_entry_receipts(&events, troll, Zone::Battlefield);
    assert!(settled(control.state()));
}

#[test]
fn spellbook_counter_ordering_finishes_after_restore_before_the_entry_receipt() {
    for double_first in [false, true] {
        let (mut runner, _, _) = garruk_draft(
            &["Hardened Scales", "Doubling Season", "Elemental Bond"],
            None,
        );
        let bond = object_named(runner.state(), "Elemental Bond");
        let events = submit(&mut runner, TROLL);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::ObjectConjured { .. })));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player: P0, .. }
        ));
        let mut runner = saved_game_runner(&runner);
        let id = object_named(runner.state(), TROLL);
        let first = object_named(
            runner.state(),
            if double_first {
                "Doubling Season"
            } else {
                "Hardened Scales"
            },
        );
        let pending = runner.state().pending_replacement.as_ref().unwrap();
        assert_eq!(pending.proposed.affected_object_id(), Some(id));
        let index = pending
            .candidates
            .iter()
            .position(|candidate| candidate.source == first)
            .unwrap();
        let result = runner.act(GameAction::ChooseReplacement { index }).unwrap();
        outside_game_entry_receipts(&result.events, id, Zone::Battlefield);
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, from: None, record, .. }
                if *object_id == id && record.power == Some(if double_first { 5 } else { 6 })
        )));
        assert_eq!(
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            if double_first { 3 } else { 4 },
        );
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| entry.source_id == bond)
                .count(),
            1
        );
        let hand_before = runner.state().players[0].hand.len();
        drive_until(&mut runner, settled);
        assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
        assert!(settled(runner.state()));
    }
}

#[test]
fn blood_age_muster_random_entry_keeps_the_written_tail_behind_riot() {
    for (rhythm_owner, counter) in [
        (None, false),
        (Some(P1), false),
        (Some(P0), true),
        (Some(P0), false),
    ] {
        let db = battlefield_spellbook_database();
        let mut game = scenario(Phase::PreCombatMain, db);
        let muster = game.add_real_card(P0, "Blood Age Muster", Zone::Battlefield, db);
        let exiled = game.add_real_card(P0, "Island", Zone::Graveyard, db);
        let cling = game.add_real_card(P0, "Cling to Dust", Zone::Hand, db);
        game.add_real_card(P0, "Swamp", Zone::Battlefield, db);
        if let Some(owner) = rhythm_owner {
            game.add_real_card(owner, "Rhythm of the Wild", Zone::Battlefield, db);
        }
        let mut runner = build(game, db);
        let mut observed = runner
            .cast(cling)
            .target_object(exiled)
            .resolve()
            .events()
            .to_vec();
        observed.extend(
            drive_until(&mut runner, |state| {
                matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) || settled(state)
            })
            .events,
        );
        assert_eq!(runner.state().objects[&exiled].zone, Zone::Exile);
        let events = if rhythm_owner == Some(P0) {
            let WaitingFor::ReplacementChoice {
                player: P0,
                candidates,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "expected the drafted creature's riot prompt, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(candidates.len(), 2);
            assert!(runner.state().pending_replacement.is_some());
            assert!(runner.state().active_ability_continuation().is_some());
            assert!(!observed.iter().any(|event| matches!(
                event, GameEvent::EffectResolved { source_id, kind: EffectKind::ApplyPerpetual, .. } if *source_id == muster
            )));
            let index = candidates
                .iter()
                .position(|candidate| {
                    if counter {
                        candidate.description.contains("+1/+1")
                    } else {
                        candidate.description == "It gains haste"
                    }
                })
                .unwrap();
            runner
                .act(GameAction::ChooseReplacement { index })
                .unwrap()
                .events
        } else {
            observed
        };
        let id = *runner
            .state()
            .last_created_token_ids
            .first()
            .expect("the draft completed");
        outside_game_entry_receipts(&events, id, Zone::Battlefield);
        if rhythm_owner == Some(P0) {
            assert_eq!(
                runner.state().objects[&id]
                    .counters
                    .get(&CounterType::Plus1Plus1)
                    .copied()
                    .unwrap_or(0),
                u32::from(counter),
            );
        }
        let card = &runner.state().objects[&id];
        let printed_haste = db
            .get_face_by_name(&card.name)
            .unwrap()
            .keywords
            .contains(&engine::types::keywords::Keyword::Haste);
        assert_eq!(
            card.has_keyword(&engine::types::keywords::Keyword::Haste),
            printed_haste || (rhythm_owner == Some(P0) && !counter),
        );
        assert!(runner.state().pending_liminal_entry_resume.is_none());
        assert!(runner.state().liminal_entries.is_empty());
        assert!(runner.state().active_ability_continuation().is_none());
        assert!(settled(runner.state()));
    }
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

const BATTLEFIELD_SPELLBOOK_ATOMIC: &str = r#"[
{"name":"Mirror Image","type":"Creature — Shapeshifter","types":["Creature"],"subtypes":["Shapeshifter"],"supertypes":[],"layout":"normal","text":"You may have this creature enter as a copy of a creature you control.","manaCost":"{2}{U}","manaValue":3.0,"power":"0","toughness":"0","colors":["U"],"colorIdentity":["U"],"identifiers":{"scryfallOracleId":"7275fb8c-571a-4c4c-889a-829419332e51"},"legalities":{"historic":"Legal"}},
{"name":"Brood Astronomer","type":"Creature — Insect Scientist","types":["Creature"],"subtypes":["Insect","Scientist"],"supertypes":[],"layout":"normal","text":"When this creature enters, you may sacrifice a land. If you do, draft a card from the Planets Spellbook and put it onto the battlefield tapped.\n{T}: Add one mana of any color. If you control a Planet with twelve or more charge counters on it, add three mana of any one color instead.","manaCost":"{1}{G}","manaValue":2.0,"power":"2","toughness":"2","colors":["G"],"colorIdentity":["G"],"keywords":["Draft from a spellbook"],"relatedCards":{"spellbook":["Adagia, Windswept Bastion","Evendo, Waking Haven","Kavaron, Memorial World","Susur Secundi, Void Altar","Uthros, Titanic Godcore"]},"identifiers":{"scryfallOracleId":"7a56bdd4-f70e-4abf-9cb7-1624be3288ce"},"legalities":{"historic":"Legal"}},
{"name":"Garruk, Wrath of the Wilds","type":"Legendary Planeswalker — Garruk","types":["Planeswalker"],"subtypes":["Garruk"],"supertypes":["Legendary"],"layout":"normal","text":"[+1]: Choose a creature card in your hand. It perpetually gets +1/+1 and perpetually gains \"This spell costs {1} less to cast.\"\n[−1]: Draft a card from Garruk, Wrath of the Wilds's spellbook and put it onto the battlefield.\n[−6]: Until end of turn, creatures you control get +3/+3 and gain trample.","manaCost":"{2}{G}{G}","manaValue":4.0,"loyalty":"4","colors":["G"],"colorIdentity":["G"],"keywords":["Draft from a spellbook"],"relatedCards":{"spellbook":["Barkhide Troll","Dawntreader Elk","Dire Wolf Prowler","Ferocious Pup","Mosscoat Goriak","Murasa Rootgrazer","Nessian Hornbeetle","Pestilent Wolf","Scurrid Colony","Sylvan Brushstrider","Territorial Scythecat","Trufflesnout","Underdark Basilisk","Wary Okapi"]},"identifiers":{"scryfallOracleId":"5967ced4-fd2b-4434-8747-7335c2bec201"},"legalities":{"historic":"Legal"}},
{"name":"Blood Age Muster","type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"layout":"normal","text":"Whenever one or more cards leave your graveyard, conjure a random card from Blood Age Muster's spellbook onto the battlefield. Its base power and toughness perpetually become 2/2. This ability triggers only once each turn.","manaCost":"{R}{W}","manaValue":2.0,"colors":["R","W"],"colorIdentity":["R","W"],"keywords":["Conjure"],"relatedCards":{"spellbook":["Blood Age General","Charging Strifeknight","Fuming Effigy","Pillardrop Warden","Spirit Mascot","Stonebinder's Familiar","Stonebound Mentor","Stone Docent","Stonerise Spirit","Summoned Dromedary"]},"identifiers":{"scryfallOracleId":"1d7c3f12-3c8f-4e54-beb2-481247a6a750"},"legalities":{"historic":"Legal"}},
{"name":"Adagia, Windswept Bastion","type":"Land — Planet","types":["Land"],"subtypes":["Planet"],"supertypes":[],"layout":"normal","text":"This land enters tapped.\n{T}: Add {W}.\nStation (Tap another creature you control: Put charge counters equal to its power on this Planet. Station only as a sorcery.)\n12+ | {3}{W}, {T}: Create a token that's a copy of target artifact or enchantment you control, except it's legendary. Activate only as a sorcery.","manaValue":0.0,"colors":[],"colorIdentity":["W"],"keywords":["Station"],"relatedCards":{"tokens":["3ae3c4af-2b85-5943-bab1-760e1b7da047"]},"identifiers":{"scryfallOracleId":"70d35dbd-1d91-4a2a-a643-6870d168f4f5"},"legalities":{"historic":"Legal"}},
{"name":"Kavaron, Memorial World","type":"Land — Planet","types":["Land"],"subtypes":["Planet"],"supertypes":[],"layout":"normal","text":"This land enters tapped.\n{T}: Add {R}.\nStation (Tap another creature you control: Put charge counters equal to its power on this Planet. Station only as a sorcery.)\n12+ | {1}{R}, {T}, Sacrifice a land: Create a 2/2 colorless Robot artifact creature token, then creatures you control get +1/+0 and gain haste until end of turn.","manaValue":0.0,"colors":[],"colorIdentity":["R"],"keywords":["Station"],"relatedCards":{"tokens":["100c8870-61aa-5c98-af92-dd7fbc63ce35"]},"identifiers":{"scryfallOracleId":"4fa826ca-d361-4391-ad0d-989ebcfa4a91"},"legalities":{"historic":"Legal"}},
{"name":"Susur Secundi, Void Altar","type":"Land — Planet","types":["Land"],"subtypes":["Planet"],"supertypes":[],"layout":"normal","text":"This land enters tapped.\n{T}: Add {B}.\nStation (Tap another creature you control: Put charge counters equal to its power on this Planet. Station only as a sorcery.)\n12+ | {1}{B}, {T}, Pay 2 life, Sacrifice a creature: Draw cards equal to the sacrificed creature's power. Activate only as a sorcery.","manaValue":0.0,"colors":[],"colorIdentity":["B"],"keywords":["Station"],"identifiers":{"scryfallOracleId":"50d6cadc-07e4-479e-90f4-e3a20f769bab"},"legalities":{"historic":"Legal"}},
{"name":"Blood Age General","type":"Creature — Spirit Warrior","types":["Creature"],"subtypes":["Spirit","Warrior"],"supertypes":[],"layout":"normal","text":"{T}: Attacking Spirits get +1/+0 until end of turn.","manaCost":"{1}{R}","manaValue":2.0,"power":"2","toughness":"2","colors":["R"],"colorIdentity":["R"],"identifiers":{"scryfallOracleId":"10ea73c8-7cf8-4a48-a0cc-072bb541880f"},"legalities":{"historic":"Legal"}},
{"name":"Charging Strifeknight","type":"Creature — Spirit Knight","types":["Creature"],"subtypes":["Spirit","Knight"],"supertypes":[],"layout":"normal","text":"Haste\n{T}, Discard a card: Draw a card.","manaCost":"{2}{R}","manaValue":3.0,"power":"3","toughness":"3","colors":["R"],"colorIdentity":["R"],"keywords":["Haste"],"identifiers":{"scryfallOracleId":"b0b9a7c1-f515-4627-a050-b595d6641611"},"legalities":{"historic":"Legal"}},
{"name":"Fuming Effigy","type":"Creature — Spirit","types":["Creature"],"subtypes":["Spirit"],"supertypes":[],"layout":"normal","text":"Whenever one or more cards leave your graveyard, this creature deals 1 damage to each opponent.","manaCost":"{3}{R}","manaValue":4.0,"power":"4","toughness":"3","colors":["R"],"colorIdentity":["R"],"identifiers":{"scryfallOracleId":"37d48f9b-0286-4c5b-b549-baccb473aea3"},"legalities":{"historic":"Legal"}},
{"name":"Pillardrop Warden","type":"Creature — Spirit Dwarf","types":["Creature"],"subtypes":["Spirit","Dwarf"],"supertypes":[],"layout":"normal","text":"Reach\n{2}, {T}, Sacrifice this creature: Return target instant or sorcery card from your graveyard to your hand. Activate only as a sorcery.","manaCost":"{3}{R}","manaValue":4.0,"power":"1","toughness":"5","colors":["R"],"colorIdentity":["R"],"keywords":["Reach"],"identifiers":{"scryfallOracleId":"6ee2a7ba-a676-4d05-b214-7db52e41afe7"},"legalities":{"historic":"Legal"}},
{"name":"Spirit Mascot","type":"Creature — Spirit Ox","types":["Creature"],"subtypes":["Spirit","Ox"],"supertypes":[],"layout":"normal","text":"Whenever one or more cards leave your graveyard, put a +1/+1 counter on this creature.","manaCost":"{R}{W}","manaValue":2.0,"power":"2","toughness":"2","colors":["R","W"],"colorIdentity":["R","W"],"identifiers":{"scryfallOracleId":"34c572b2-dccf-43d9-a9c4-846653ad22da"},"legalities":{"historic":"Legal"}},
{"name":"Stonebinder's Familiar","type":"Creature — Spirit Dog","types":["Creature"],"subtypes":["Spirit","Dog"],"supertypes":[],"layout":"normal","text":"Whenever one or more cards are put into exile during your turn, put a +1/+1 counter on this creature. This ability triggers only once each turn.","manaCost":"{W}","manaValue":1.0,"power":"1","toughness":"1","colors":["W"],"colorIdentity":["W"],"identifiers":{"scryfallOracleId":"87266dec-ac03-4f21-b235-67c14c3024db"},"legalities":{"historic":"Legal"}},
{"name":"Stone Docent","type":"Creature — Spirit Chimera","types":["Creature"],"subtypes":["Spirit","Chimera"],"supertypes":[],"layout":"normal","text":"{W}, Exile this card from your graveyard: You gain 2 life. Surveil 1. Activate only as a sorcery. (Look at the top card of your library. You may put it into your graveyard.)","manaCost":"{1}{W}","manaValue":2.0,"power":"3","toughness":"1","colors":["W"],"colorIdentity":["W"],"keywords":["Surveil"],"identifiers":{"scryfallOracleId":"dbb1690a-f6f5-459c-9d7f-cb6541fb6688"},"legalities":{"historic":"Legal"}},
{"name":"Stonerise Spirit","type":"Creature — Spirit Bird","types":["Creature"],"subtypes":["Spirit","Bird"],"supertypes":[],"layout":"normal","text":"Flying\n{4}, Exile a card from your graveyard: Target creature gains flying until end of turn.","manaCost":"{1}{W}","manaValue":2.0,"power":"1","toughness":"2","colors":["W"],"colorIdentity":["W"],"keywords":["Flying"],"identifiers":{"scryfallOracleId":"9fbab5bc-c651-46dc-89b6-0bcfeadb3891"},"legalities":{"historic":"Legal"}},
{"name":"Summoned Dromedary","type":"Creature — Spirit Camel","types":["Creature"],"subtypes":["Spirit","Camel"],"supertypes":[],"layout":"normal","text":"Vigilance\n{1}{W}: Return this card from your graveyard to your hand. Activate only as a sorcery.","manaCost":"{3}{W}","manaValue":4.0,"power":"4","toughness":"3","colors":["W"],"colorIdentity":["W"],"keywords":["Vigilance"],"identifiers":{"scryfallOracleId":"697b14c6-fe6a-4880-9cf2-b6602a52ca0b"},"legalities":{"historic":"Legal"}}
]"#;
