//! Issue #9282 — which stack entry a counter or a targeted copy acts on.
//!
//! A spell's own cast trigger
//! (storm, cascade, "when you cast this spell") is a different stack object that
//! only shares the spell as its source, so countering the spell leaves it there.
//! The ward rows never assert the id a `SpellCountered` event carries.

use crate::support::shared_card_db;
use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    ActionResult, CastOfferKind, CastPaymentMode, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::log::{GameLogEntry, LogSegment};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FROST_TITAN: &str = "Whenever this creature becomes the target of a spell or ability an opponent controls, counter that spell or ability unless its controller pays {2}.\nWhenever this creature enters or attacks, tap target permanent. It doesn't untap during its controller's next untap step.";
const STRICT_PROCTOR: &str = "Flying\nWhenever a permanent entering causes a triggered ability to trigger, counter that ability unless its controller pays {2}.";
const HESITATION: &str =
    "When a player casts a spell, sacrifice this enchantment and counter that spell.";
const NETHER_VOID: &str =
    "Whenever a player casts a spell, counter it unless that player pays {3}.";
const UNSUBSTANTIATE: &str = "Return target spell or creature to its owner's hand.";
const PINGER: &str = "{T}: This creature deals 1 damage to any target.";
const MOGG_MOB: &str =
    "Sacrifice this creature: It deals 3 damage divided as you choose among one, two, or three targets.";
const ULAMOG: &str = "When you cast this spell, destroy target permanent.\nIndestructible\nAnnihilator 4 (Whenever this creature attacks, defending player sacrifices four permanents of their choice.)\nWhen Ulamog is put into a graveyard from anywhere, its owner shuffles their graveyard into their library.";
const BATTLEMAGE: &str = "Kicker {G} and/or {1}{U}\nWhen you cast this spell, if it was kicked with its {G} kicker, exile target artifact or enchantment an opponent controls.\nWhen you cast this spell, if it was kicked with its {1}{U} kicker, return target creature an opponent controls to its owner's hand.";

fn mana(count: usize, kind: ManaType) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn pool(units: &[(usize, ManaType)]) -> Vec<ManaUnit> {
    units
        .iter()
        .flat_map(|(count, kind)| mana(*count, *kind))
        .collect()
}

/// Everything the engine reported while a helper drove the game.
#[derive(Default)]
struct Trace {
    events: Vec<GameEvent>,
    log: Vec<GameLogEntry>,
}

impl Trace {
    fn absorb(&mut self, result: ActionResult) {
        self.events.extend(result.events);
        self.log.extend(result.log_entries);
    }

    /// The `SpellCountered` ids, in order.
    fn countered(&self) -> Vec<ObjectId> {
        self.events
            .iter()
            .filter_map(|event| match event {
                GameEvent::SpellCountered { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect()
    }

    fn spell_copies(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, GameEvent::SpellCopied { .. }))
            .count()
    }

    /// The card segments of every log line that reads "<a> counters <b>".
    fn counter_lines(&self) -> Vec<Vec<(String, ObjectId)>> {
        self.log
            .iter()
            .filter(|entry| {
                entry.segments.iter().any(
                    |segment| matches!(segment, LogSegment::Text(text) if text.contains(" counters ")),
                )
            })
            .map(|entry| card_segments(&entry.segments))
            .collect()
    }
}

fn card_segments(segments: &[LogSegment]) -> Vec<(String, ObjectId)> {
    segments
        .iter()
        .filter_map(|segment| match segment {
            LogSegment::CardName { name, object_id } => Some((name.clone(), *object_id)),
            _ => None,
        })
        .collect()
}

fn pass(runner: &mut GameRunner, trace: &mut Trace) {
    trace.absorb(runner.act(GameAction::PassPriority).expect("pass priority"));
}

/// Answer the prompts a resolution raises: trigger targets, "unless pays"
/// (always declined), trigger ordering, and copy retargeting (kept).
fn settle(runner: &mut GameRunner, trace: &mut Trace) {
    for _ in 0..20 {
        let result = match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => runner.choose_first_legal_target(),
            WaitingFor::UnlessPayment { .. } => {
                runner.act(GameAction::PayUnlessCost { pay: false })
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::CopyRetarget { .. } => runner.act(GameAction::KeepAllCopyTargets),
            _ => return,
        };
        trace.absorb(result.expect("answer prompt"));
    }
}

/// Pass priority until the entry that is on top now has left the stack.
fn resolve_top_entry(runner: &mut GameRunner) -> Trace {
    let top = runner.state().stack.back().expect("a stack entry").id;
    let mut trace = Trace::default();
    for _ in 0..6 {
        if !on_stack(runner, top) {
            break;
        }
        settle(runner, &mut trace);
        if !on_stack(runner, top) {
            break;
        }
        pass(runner, &mut trace);
    }
    settle(runner, &mut trace);
    trace
}

fn on_stack(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| entry.id == id)
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// The most recent triggered ability on the stack whose source is `source`.
fn trigger_of(runner: &GameRunner, source: ObjectId) -> ObjectId {
    runner
        .state()
        .stack
        .iter()
        .rev()
        .find(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|entry| entry.id)
        .expect("a triggered ability of that source is on the stack")
}

fn hand_size(runner: &GameRunner, player: usize) -> i64 {
    runner.state().players[player].hand.len() as i64
}

fn choose_object(runner: &mut GameRunner, target: ObjectId) {
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("choose the target");
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn build(scenario: GameScenario) -> GameRunner {
    let db = shared_card_db().expect("integration card fixture");
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
}

// ---------------------------------------------------------------------------

struct CascadeBoard {
    runner: GameRunner,
    elf: ObjectId,
    bears: ObjectId,
    answer: ObjectId,
    cascade: ObjectId,
}

/// P0 has cast Bloodbraid Elf; its cascade trigger waits above it and P0 has
/// passed priority. P1 holds `answer` with `blue` blue mana.
fn cascade_board(answer_name: &str, blue: usize) -> CascadeBoard {
    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    let elf = sc.add_real_card(P0, "Bloodbraid Elf", Zone::Hand, db);
    let bears = sc.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let answer = sc.add_real_card(P1, answer_name, Zone::Hand, db);
    sc.with_mana_pool(
        P0,
        pool(&[
            (2, ManaType::Colorless),
            (1, ManaType::Red),
            (1, ManaType::Green),
        ]),
    );
    sc.with_mana_pool(P1, mana(blue, ManaType::Blue));
    let mut runner = build(sc);
    runner.cast(elf).commit();
    let mut trace = Trace::default();
    settle(&mut runner, &mut trace);
    let cascade = trigger_of(&runner, elf);
    pass(&mut runner, &mut trace);
    CascadeBoard {
        runner,
        elf,
        bears,
        answer,
        cascade,
    }
}

fn assert_cascade_offered(runner: &GameRunner) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { .. },
                ..
            }
        ),
        "the cascade trigger still resolves and offers the cascade cast: {:?}",
        runner.state().waiting_for
    );
}

/// Counterspell counters Bloodbraid Elf; the cascade trigger above it stays and
/// resolves.
#[test]
fn counterspell_counters_a_cascade_spell_and_leaves_its_cascade_trigger() {
    let CascadeBoard {
        mut runner,
        elf,
        bears,
        answer,
        cascade,
    } = cascade_board("Counterspell", 2);
    runner.cast(answer).target_object(elf).commit();
    let stack = &runner.state().stack;
    assert_eq!(
        stack.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![elf, cascade, answer],
        "the cascade trigger waits between Bloodbraid Elf and Counterspell"
    );
    assert_eq!(
        stack
            .back()
            .and_then(|entry| entry.ability())
            .map(|a| a.targets.clone()),
        Some(vec![TargetRef::Object(elf)])
    );

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![elf]);
    assert_eq!(zone_of(&runner, elf), Zone::Graveyard);
    assert!(
        on_stack(&runner, cascade),
        "Counterspell removed the spell and not its cascade trigger"
    );

    let next = resolve_top_entry(&mut runner);
    assert!(next.countered().is_empty());
    assert_cascade_offered(&runner);
    assert_eq!(zone_of(&runner, bears), Zone::Exile);
}

/// Hesitation's "counter that spell" reads the spell it triggered for.
#[test]
fn hesitation_counters_the_spell_that_triggered_it_not_its_cascade_trigger() {
    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    sc.add_enchantment_from_oracle(P1, "Hesitation", HESITATION);
    let elf = sc.add_real_card(P0, "Bloodbraid Elf", Zone::Hand, db);
    sc.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    sc.with_mana_pool(
        P0,
        pool(&[
            (2, ManaType::Colorless),
            (1, ManaType::Red),
            (1, ManaType::Green),
        ]),
    );
    let mut runner = build(sc);
    runner.cast(elf).commit();
    let mut trace = Trace::default();
    settle(&mut runner, &mut trace);
    let cascade = trigger_of(&runner, elf);
    let hesitation_trigger = runner.state().stack.back().expect("top").id;
    assert_ne!(
        hesitation_trigger, cascade,
        "Hesitation's trigger waits above the cascade trigger"
    );

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![elf]);
    assert_eq!(zone_of(&runner, elf), Zone::Graveyard);
    assert!(on_stack(&runner, cascade));
}

/// Nether Void's "counter it unless that player pays {3}", declined, counters the
/// spell and leaves its cascade trigger.
#[test]
fn declined_nether_void_counters_the_spell_and_leaves_its_cascade_trigger() {
    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    sc.add_enchantment_from_oracle(P1, "Nether Void", NETHER_VOID);
    let elf = sc.add_real_card(P0, "Bloodbraid Elf", Zone::Hand, db);
    let bears = sc.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    sc.with_mana_pool(
        P0,
        pool(&[
            (2, ManaType::Colorless),
            (1, ManaType::Red),
            (1, ManaType::Green),
        ]),
    );
    let mut runner = build(sc);
    runner.cast(elf).commit();
    let mut trace = Trace::default();
    settle(&mut runner, &mut trace);
    let cascade = trigger_of(&runner, elf);
    assert!(
        runner
            .state()
            .stack
            .back()
            .is_some_and(|entry| entry.source_id != elf),
        "Nether Void's trigger waits above the cascade trigger"
    );

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![elf]);
    assert_eq!(zone_of(&runner, elf), Zone::Graveyard);
    assert!(on_stack(&runner, cascade));

    resolve_top_entry(&mut runner);
    assert_cascade_offered(&runner);
    assert_eq!(zone_of(&runner, bears), Zone::Exile);
}

/// Stifle counters exactly the trigger it targets.
#[test]
fn stifle_counters_the_trigger_it_targets_and_not_the_spell() {
    let CascadeBoard {
        mut runner,
        elf,
        answer,
        cascade,
        ..
    } = cascade_board("Stifle", 1);
    runner.cast(answer).target_object(cascade).commit();
    let stack = &runner.state().stack;
    assert_eq!(
        stack
            .back()
            .and_then(|entry| entry.ability())
            .map(|a| a.targets.clone()),
        Some(vec![TargetRef::Object(cascade)])
    );

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![cascade]);
    assert!(!on_stack(&runner, cascade));
    assert!(on_stack(&runner, elf));
}

/// Counterspell on a vanilla creature spell, and its log
/// line.
#[test]
fn counterspell_counters_a_vanilla_spell_and_logs_it() {
    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    let bears = sc.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let counterspell = sc.add_real_card(P1, "Counterspell", Zone::Hand, db);
    sc.with_mana_pool(P0, pool(&[(1, ManaType::Colorless), (1, ManaType::Green)]));
    sc.with_mana_pool(P1, mana(2, ManaType::Blue));
    let mut runner = build(sc);
    runner.cast(bears).commit();
    let mut trace = Trace::default();
    pass(&mut runner, &mut trace);
    runner.cast(counterspell).target_object(bears).commit();
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![bears, counterspell]
    );

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![bears]);
    assert_eq!(zone_of(&runner, bears), Zone::Graveyard);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        counter.counter_lines(),
        vec![vec![
            ("Counterspell".to_string(), counterspell),
            ("Grizzly Bears".to_string(), bears),
        ]]
    );
}

// ---------------------------------------------------------------------------
// Storm: countering the spell leaves the storm trigger, which still copies it.
// ---------------------------------------------------------------------------

struct StormBoard {
    runner: GameRunner,
    brain_freeze: ObjectId,
    storm: ObjectId,
    answer: ObjectId,
}

/// P0 has resolved Lightning Bolt and cast Brain Freeze; its storm trigger
/// waits above it and P0 has passed priority. P1 holds `answer`.
fn storm_board(answer_name: &str) -> StormBoard {
    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    let bolt = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let brain_freeze = sc.add_real_card(P0, "Brain Freeze", Zone::Hand, db);
    let answer = sc.add_real_card(P1, answer_name, Zone::Hand, db);
    for _ in 0..12 {
        sc.add_real_card(P1, "Grizzly Bears", Zone::Library, db);
        sc.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    }
    sc.with_mana_pool(
        P0,
        pool(&[
            (1, ManaType::Red),
            (1, ManaType::Colorless),
            (1, ManaType::Blue),
        ]),
    );
    sc.with_mana_pool(P1, mana(2, ManaType::Blue));
    let mut runner = build(sc);
    runner.cast(bolt).target_player(P1).commit();
    resolve_top_entry(&mut runner);
    runner.cast(brain_freeze).target_player(P1).commit();
    let mut trace = Trace::default();
    settle(&mut runner, &mut trace);
    let storm = trigger_of(&runner, brain_freeze);
    pass(&mut runner, &mut trace);
    StormBoard {
        runner,
        brain_freeze,
        storm,
        answer,
    }
}

/// Counterspell counters Brain Freeze; its storm trigger stays and copies it.
#[test]
fn counterspell_counters_a_storm_spell_and_its_storm_trigger_still_copies_it() {
    let StormBoard {
        mut runner,
        brain_freeze,
        storm,
        answer,
    } = storm_board("Counterspell");
    runner.cast(answer).target_object(brain_freeze).commit();
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![brain_freeze, storm, answer],
        "the storm trigger waits between Brain Freeze and Counterspell"
    );

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![brain_freeze]);
    assert_eq!(zone_of(&runner, brain_freeze), Zone::Graveyard);
    assert!(
        on_stack(&runner, storm),
        "Counterspell removed the spell and not its storm trigger"
    );

    let library_before = runner.state().players[1].library.len();
    let storm_resolution = resolve_top_entry(&mut runner);
    assert_eq!(storm_resolution.spell_copies(), 1);
    runner.advance_until_stack_empty();
    assert_eq!(
        library_before - runner.state().players[1].library.len(),
        3,
        "the storm copy mills three"
    );
}

/// Twincast copies Brain Freeze, not its storm trigger.
#[test]
fn twincast_copies_the_targeted_spell_and_not_its_storm_trigger() {
    let StormBoard {
        mut runner,
        brain_freeze,
        storm,
        answer,
    } = storm_board("Twincast");
    runner.cast(answer).target_object(brain_freeze).commit();
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![brain_freeze, storm, answer],
        "the storm trigger waits between Brain Freeze and Twincast"
    );

    resolve_top_entry(&mut runner);
    let top = runner.state().stack.back().expect("the copy");
    assert!(
        matches!(top.kind, StackEntryKind::Spell { .. }),
        "the copy is a spell, not a copy of the storm trigger: {:?}",
        top.kind
    );
    assert_ne!(top.id, brain_freeze);
    assert_eq!(
        top.ability().map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Player(P1)])
    );
    let storm_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == brain_freeze
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count();
    assert_eq!(storm_triggers, 1, "only the original storm trigger exists");
}

// ---------------------------------------------------------------------------
// "Counter that spell" on a cast trigger binds the spell as it was cast.
// ---------------------------------------------------------------------------

/// Decree of Silence's trigger for a spell that has since been bounced counters
/// nothing, and that spell's storm trigger survives and copies it.
#[test]
fn a_cast_trigger_counter_for_a_bounced_spell_counters_nothing() {
    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    sc.add_real_card(P1, "Decree of Silence", Zone::Battlefield, db);
    let bolt = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let brain_freeze = sc.add_real_card(P0, "Brain Freeze", Zone::Hand, db);
    let unsubstantiate = sc
        .add_spell_to_hand_from_oracle(P1, "Unsubstantiate", true, UNSUBSTANTIATE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    for _ in 0..12 {
        sc.add_real_card(P1, "Grizzly Bears", Zone::Library, db);
        sc.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    }
    sc.with_mana_pool(
        P0,
        pool(&[
            (1, ManaType::Red),
            (1, ManaType::Colorless),
            (1, ManaType::Blue),
        ]),
    );
    sc.with_mana_pool(P1, mana(2, ManaType::Colorless));
    let mut runner = build(sc);

    // Decree counters the first spell: the positive control for its trigger.
    runner.cast(bolt).target_player(P1).commit();
    let mut trace = Trace::default();
    settle(&mut runner, &mut trace);
    runner.advance_until_stack_empty();
    assert_eq!(zone_of(&runner, bolt), Zone::Graveyard);
    assert_eq!(runner.state().players[1].life, 20, "Decree countered Bolt");

    runner.cast(brain_freeze).target_player(P1).commit();
    settle(&mut runner, &mut trace);
    let storm = trigger_of(&runner, brain_freeze);
    pass(&mut runner, &mut trace);
    runner
        .cast(unsubstantiate)
        .target_object(brain_freeze)
        .commit();
    resolve_top_entry(&mut runner);
    assert_eq!(zone_of(&runner, brain_freeze), Zone::Hand, "bounced");

    let decree = resolve_top_entry(&mut runner);
    assert!(
        decree.countered().is_empty(),
        "the bounced spell is no longer 'that spell': {:?}",
        decree.countered()
    );
    assert!(on_stack(&runner, storm));

    let library_before = runner.state().players[1].library.len();
    let storm_resolution = resolve_top_entry(&mut runner);
    assert_eq!(storm_resolution.spell_copies(), 1);
    runner.advance_until_stack_empty();
    assert_eq!(library_before - runner.state().players[1].library.len(), 3);
}

// ---------------------------------------------------------------------------
// Strict Proctor counters the entering permanent's trigger, an ability.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Extra {
    Nothing,
    MultanisPresence,
    RhythmOfTheWild,
}

struct ProctorBoard {
    runner: GameRunner,
    visionary: ObjectId,
    enters: ObjectId,
    proctor_trigger: ObjectId,
}

/// P0 has resolved Elvish Visionary under P1's Strict Proctor. The Visionary's
/// enters trigger waits below Proctor's trigger.
fn proctor_board(extra: Extra) -> ProctorBoard {
    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    match extra {
        Extra::Nothing => {}
        Extra::MultanisPresence => {
            sc.add_real_card(P0, "Multani's Presence", Zone::Battlefield, db);
        }
        Extra::RhythmOfTheWild => {
            sc.add_real_card(P0, "Rhythm of the Wild", Zone::Battlefield, db);
        }
    }
    sc.add_creature_from_oracle(P1, "Strict Proctor", 1, 3, STRICT_PROCTOR);
    let visionary = sc.add_real_card(P0, "Elvish Visionary", Zone::Hand, db);
    for _ in 0..5 {
        sc.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    }
    sc.with_mana_pool(P0, pool(&[(1, ManaType::Colorless), (1, ManaType::Green)]));
    let mut runner = build(sc);
    runner.cast(visionary).commit();
    resolve_top_entry(&mut runner);
    let mut trace = Trace::default();
    for _ in 0..4 {
        if let WaitingFor::ReplacementChoice { .. } = &runner.state().waiting_for {
            // Riot, granted by Rhythm of the Wild: take the first offered choice.
            trace.absorb(
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("riot choice"),
            );
        }
        settle(&mut runner, &mut trace);
    }
    let enters = trigger_of(&runner, visionary);
    let proctor_trigger = runner.state().stack.back().expect("top").id;
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![enters, proctor_trigger],
        "Proctor's trigger waits above the Visionary's enters trigger"
    );
    ProctorBoard {
        runner,
        visionary,
        enters,
        proctor_trigger,
    }
}

/// Strict Proctor counters the enters trigger, so the Visionary draws nothing.
#[test]
fn strict_proctor_counters_the_enters_trigger_of_the_entering_permanent() {
    let ProctorBoard {
        mut runner,
        visionary,
        enters,
        ..
    } = proctor_board(Extra::Nothing);
    let hand_before = hand_size(&runner, 0);
    let counter = resolve_top_entry(&mut runner);
    assert_eq!(
        counter.countered(),
        vec![enters],
        "the countered object is the trigger's stack entry, not the permanent {visionary:?}"
    );
    assert!(!on_stack(&runner, enters));
    runner.advance_until_stack_empty();
    assert_eq!(hand_size(&runner, 0) - hand_before, 0);
}

/// "A spell you've cast is countered" (Multani's Presence) is not a trigger
/// that Strict Proctor's counter of an ability satisfies; a countered spell
/// still does.
#[test]
fn multanis_presence_draws_for_a_countered_spell_and_not_for_a_countered_trigger() {
    let ProctorBoard {
        mut runner, enters, ..
    } = proctor_board(Extra::MultanisPresence);
    let hand_before = hand_size(&runner, 0);
    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![enters]);
    runner.advance_until_stack_empty();
    assert_eq!(
        hand_size(&runner, 0) - hand_before,
        0,
        "an ability is not a spell you've cast"
    );

    let db = shared_card_db().expect("integration card fixture");
    let mut sc = scenario();
    sc.add_real_card(P0, "Multani's Presence", Zone::Battlefield, db);
    for _ in 0..5 {
        sc.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    }
    let bears = sc.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let counterspell = sc.add_real_card(P1, "Counterspell", Zone::Hand, db);
    sc.with_mana_pool(P0, pool(&[(1, ManaType::Colorless), (1, ManaType::Green)]));
    sc.with_mana_pool(P1, mana(2, ManaType::Blue));
    let mut runner = build(sc);
    runner.cast(bears).commit();
    let mut trace = Trace::default();
    pass(&mut runner, &mut trace);
    runner.cast(counterspell).target_object(bears).commit();
    let hand_before = hand_size(&runner, 0);
    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![bears]);
    runner.advance_until_stack_empty();
    assert_eq!(
        hand_size(&runner, 0) - hand_before,
        1,
        "Multani's Presence draws for a countered spell"
    );
}

/// Rhythm of the Wild ("Creature spells you control can't be countered.") does
/// not protect a creature's enters trigger, which is not a spell.
#[test]
fn a_creature_spell_protection_does_not_stop_strict_proctor_countering_its_trigger() {
    for extra in [Extra::Nothing, Extra::RhythmOfTheWild] {
        let ProctorBoard {
            mut runner, enters, ..
        } = proctor_board(extra);
        let hand_before = hand_size(&runner, 0);
        let counter = resolve_top_entry(&mut runner);
        assert_eq!(
            counter.countered(),
            vec![enters],
            "Rhythm of the Wild: {}",
            extra == Extra::RhythmOfTheWild
        );
        runner.advance_until_stack_empty();
        assert_eq!(hand_size(&runner, 0) - hand_before, 0);
    }
}

/// A creature whose own text says "This spell can't be countered." still has its
/// enters trigger countered: the text is about the spell.
#[test]
fn a_spell_that_cant_be_countered_does_not_protect_its_enters_trigger() {
    let mut sc = scenario();
    sc.add_creature_from_oracle(P1, "Strict Proctor", 1, 3, STRICT_PROCTOR);
    let herald = sc
        .add_creature_to_hand_from_oracle(
            P0,
            "Stubborn Herald",
            1,
            1,
            "This spell can't be countered.\nWhen this creature enters, you gain 3 life.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    sc.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut runner = build(sc);
    runner.cast(herald).commit();
    resolve_top_entry(&mut runner);
    let mut trace = Trace::default();
    settle(&mut runner, &mut trace);
    let enters = trigger_of(&runner, herald);
    let life_before = runner.state().players[0].life;

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![enters]);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].life - life_before, 0);
}

// ---------------------------------------------------------------------------
// Log lines cite an ability's source, not its stack entry.
// ---------------------------------------------------------------------------

#[test]
fn strict_proctors_log_line_names_the_trigger_source() {
    let ProctorBoard {
        mut runner,
        visionary,
        enters,
        proctor_trigger,
    } = proctor_board(Extra::Nothing);
    let proctor = runner.state().stack.back().expect("top").source_id;
    assert_ne!(proctor_trigger, enters);
    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![enters]);
    let names: Vec<Vec<(String, ObjectId)>> = vec![vec![
        ("Strict Proctor".to_string(), proctor),
        ("Elvish Visionary".to_string(), visionary),
    ]];
    assert_eq!(counter.counter_lines(), names);
}

#[test]
fn stifles_log_lines_name_the_countered_triggers_source() {
    let CascadeBoard {
        mut runner,
        elf,
        answer,
        cascade,
        ..
    } = cascade_board("Stifle", 1);
    let card_id = runner.state().objects[&answer].card_id;
    let mut cast = Trace::default();
    cast.absorb(
        runner
            .act(GameAction::CastSpell {
                object_id: answer,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("cast Stifle"),
    );
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        cast.absorb(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(cascade)),
                })
                .expect("target the cascade trigger"),
        );
    }
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .and_then(|entry| entry.ability())
            .map(|a| a.targets.clone()),
        Some(vec![TargetRef::Object(cascade)]),
        "Stifle is on the stack targeting the cascade trigger"
    );
    let targeted: Vec<Vec<(String, ObjectId)>> = cast
        .log
        .iter()
        .filter(|entry| {
            entry.segments.iter().any(
                |segment| matches!(segment, LogSegment::Text(text) if text.contains("is targeted by")),
            )
        })
        .map(|entry| card_segments(&entry.segments))
        .collect();
    assert_eq!(
        targeted,
        vec![vec![
            ("Bloodbraid Elf".to_string(), elf),
            ("Stifle".to_string(), answer),
        ]]
    );

    let counter = resolve_top_entry(&mut runner);
    assert_eq!(counter.countered(), vec![cascade]);
    assert_eq!(
        counter.counter_lines(),
        vec![vec![
            ("Stifle".to_string(), answer),
            ("Bloodbraid Elf".to_string(), elf),
        ]]
    );
}

// ---------------------------------------------------------------------------
// Frost Titan: a counter on a "becomes the target" trigger removes no entry
// that merely shares the targeted permanent as its source.
// ---------------------------------------------------------------------------

/// CR 701.6a + CR 113.7a: Frost Titan's "counter that spell or ability",
/// declined, does not counter Frost Titan's own enters trigger, which exists on
/// the stack independently of Frost Titan; that trigger still taps its target.
#[test]
fn frost_titans_counter_leaves_its_own_enters_trigger_on_the_stack() {
    let db = shared_card_db().expect("integration card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let titan = scenario
        .add_creature_to_hand_from_oracle(P0, "Frost Titan", 6, 6, FROST_TITAN)
        .with_mana_cost(ManaCost::generic(6))
        .id();
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P0, mana(6, ManaType::Colorless));
    let mut p1_pool = mana(1, ManaType::Red);
    p1_pool.extend(mana(2, ManaType::Colorless));
    scenario.with_mana_pool(P1, p1_pool);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    runner.cast(titan).commit();
    runner.resolve_top();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Frost Titan's enters trigger asks for its target: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears)),
        })
        .expect("target Grizzly Bears");
    let enters_trigger = runner
        .state()
        .stack
        .iter()
        .find(|entry| {
            entry.source_id == titan
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|entry| entry.id)
        .expect("Frost Titan's enters trigger is on the stack");

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner.cast(bolt).target_object(titan).commit();
    let becomes_target_triggers: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility {
                    trigger_event: Some(GameEvent::BecomesTarget { .. }),
                    ..
                }
            )
        })
        .map(|entry| entry.id)
        .collect();
    assert_eq!(
        becomes_target_triggers.len(),
        1,
        "Frost Titan's becomes-target trigger is on the stack"
    );

    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the becomes-target trigger");
    }
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines to pay {2}");
    let stack = &runner.state().stack;
    assert!(
        !stack
            .iter()
            .any(|entry| entry.id == becomes_target_triggers[0]),
        "the becomes-target trigger has resolved"
    );
    assert!(
        stack.iter().any(|entry| entry.id == enters_trigger),
        "Frost Titan's enters trigger is still on the stack"
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&bears].tapped,
        "Frost Titan's enters trigger tapped Grizzly Bears"
    );
}

// ---------------------------------------------------------------------------
// Ward.
// ---------------------------------------------------------------------------

fn warded(sc: &mut GameScenario) -> ObjectId {
    sc.add_creature(P1, "Warded", 3, 3)
        .with_keyword(Keyword::Ward(WardCost::Mana(ManaCost::generic(2))))
        .id()
}

/// Resolve every ward trigger of `warded` on the stack, declining each.
fn resolve_wards(runner: &mut GameRunner, warded: ObjectId) {
    for _ in 0..4 {
        if !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == warded)
        {
            break;
        }
        resolve_top_entry(runner);
    }
}

fn is_ward_trigger(kind: &StackEntryKind) -> bool {
    matches!(
        kind,
        StackEntryKind::TriggeredAbility {
            trigger_event: Some(GameEvent::BecomesTarget { .. }),
            ..
        }
    )
}

fn choose_targets_until_settled(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                choose_object(runner, target);
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            _ => break,
        }
    }
}

/// Ward counters an activated ability that targeted the warded creature.
#[test]
fn ward_counters_an_activated_ability_that_targeted_the_warded_creature() {
    let mut sc = scenario();
    let pinger = sc.add_creature_from_oracle(P0, "Pinger", 1, 1, PINGER).id();
    let warded = warded(&mut sc);
    let mut runner = build(sc);
    runner
        .act(GameAction::ActivateAbility {
            source_id: pinger,
            ability_index: 0,
        })
        .expect("activate");
    choose_targets_until_settled(&mut runner, warded);
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == pinger)
        .map(|entry| entry.id)
        .expect("the ability is on the stack");
    assert!(
        runner
            .state()
            .stack
            .back()
            .is_some_and(|e| is_ward_trigger(&e.kind)),
        "the ward trigger waits above the ability"
    );

    resolve_wards(&mut runner, warded);
    assert!(!on_stack(&runner, ability), "ward countered the ability");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&warded].damage_marked, 0);
}

/// Ward counters a triggered ability whose target was assigned automatically.
#[test]
fn ward_counters_a_triggered_ability_with_an_automatic_target() {
    let mut sc = scenario();
    let tapper = sc
        .add_creature_to_hand_from_oracle(
            P0,
            "Tap Herald",
            2,
            2,
            "When this creature enters, tap target creature an opponent controls.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let warded = warded(&mut sc);
    sc.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut runner = build(sc);
    runner.cast(tapper).commit();
    let mut trace = Trace::default();
    // Both players pass, so the creature spell resolves and its trigger is put on the stack.
    pass(&mut runner, &mut trace);
    pass(&mut runner, &mut trace);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the target was assigned without a prompt: {:?}",
        runner.state().waiting_for
    );
    let enters = trigger_of(&runner, tapper);
    assert!(
        runner
            .state()
            .stack
            .back()
            .is_some_and(|e| is_ward_trigger(&e.kind)),
        "the ward trigger waits above the enters trigger"
    );

    resolve_wards(&mut runner, warded);
    assert!(!on_stack(&runner, enters), "ward countered the trigger");
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&warded].tapped);
}

/// Ward counters a spell's cast trigger that targeted, and not the spell.
#[test]
fn ward_counters_a_cast_trigger_that_targeted_and_not_its_spell() {
    let mut sc = scenario();
    let ulamog = sc
        .add_creature_to_hand_from_oracle(P0, "Ulamog, the Infinite Gyre", 11, 11, ULAMOG)
        .with_mana_cost(ManaCost::generic(11))
        .id();
    let warded = warded(&mut sc);
    sc.with_mana_pool(P0, mana(11, ManaType::Colorless));
    let mut runner = build(sc);
    runner.cast(ulamog).commit();
    choose_targets_until_settled(&mut runner, warded);
    let cast_trigger = trigger_of(&runner, ulamog);
    assert!(
        runner
            .state()
            .stack
            .back()
            .is_some_and(|e| is_ward_trigger(&e.kind)),
        "the ward trigger waits above the cast trigger"
    );
    assert!(on_stack(&runner, ulamog));

    resolve_wards(&mut runner, warded);
    assert!(
        !on_stack(&runner, cast_trigger),
        "ward countered the trigger"
    );
    assert_eq!(zone_of(&runner, ulamog), Zone::Stack);
}

/// Each ward trigger counters the cast trigger that targeted the warded creature.
#[test]
fn each_ward_trigger_counters_a_kicked_spells_cast_trigger() {
    let mut sc = scenario();
    let battlemage = sc
        .add_creature_to_hand_from_oracle(P0, "Wastescape Battlemage", 2, 2, BATTLEMAGE)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Colorless],
        })
        .id();
    let warded = warded(&mut sc);
    sc.with_mana_pool(
        P0,
        pool(&[
            (3, ManaType::Colorless),
            (1, ManaType::Green),
            (1, ManaType::Blue),
        ]),
    );
    let mut runner = build(sc);
    // An artifact creature, so each kicker trigger has it as a legal target.
    let object = runner.state_mut().objects.get_mut(&warded).unwrap();
    object.card_types.core_types.push(CoreType::Artifact);
    object.base_card_types = object.card_types.clone();
    let card_id = runner.state().objects[&battlemage].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: battlemage,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Wastescape Battlemage");
    for _ in 0..16 {
        let result = match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => {
                runner.act(GameAction::DecideOptionalCost { pay: true })
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(warded)),
                })
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            _ => break,
        };
        result.expect("answer a Wastescape Battlemage prompt");
    }
    let cast_triggers: Vec<ObjectId> = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == battlemage
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|entry| entry.id)
        .collect();
    let ward_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == warded)
        .count();
    assert_eq!((cast_triggers.len(), ward_triggers), (2, 2));

    resolve_wards(&mut runner, warded);
    assert!(
        cast_triggers.iter().all(|id| !on_stack(&runner, *id)),
        "ward countered each cast trigger"
    );
    assert_eq!(zone_of(&runner, battlemage), Zone::Stack);
}

/// Ward counters Mogg Mob's divided-damage ability.
#[test]
fn ward_counters_a_divided_damage_ability() {
    let mut sc = scenario();
    let mob = sc
        .add_creature_from_oracle(P0, "Mogg Mob", 3, 3, MOGG_MOB)
        .id();
    let warded = warded(&mut sc);
    let mut runner = build(sc);
    runner
        .act(GameAction::ActivateAbility {
            source_id: mob,
            ability_index: 0,
        })
        .expect("activate");
    for _ in 0..8 {
        let result = match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => runner.act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(warded)],
            }),
            WaitingFor::DistributeAmong { .. } => runner.act(GameAction::DistributeAmong {
                distribution: vec![(TargetRef::Object(warded), 3)],
            }),
            _ => break,
        };
        result.expect("answer an activation prompt");
    }
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == mob)
        .map(|entry| entry.id)
        .expect("Mogg Mob's ability is on the stack");
    assert!(
        runner
            .state()
            .stack
            .back()
            .is_some_and(|e| is_ward_trigger(&e.kind)),
        "the ward trigger waits above the ability"
    );

    resolve_wards(&mut runner, warded);
    assert!(!on_stack(&runner, ability), "ward countered the ability");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&warded].damage_marked, 0);
}
