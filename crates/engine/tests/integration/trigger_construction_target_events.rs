//! CR 601.2c + CR 603.3d + CR 700.13 + CR 603.2c: the events a triggered
//! ability's own construction produces — its targets becoming targets
//! (`BecomesTarget`) and the crime it commits (`CrimeCommitted`) — trigger the
//! abilities that watch for them exactly once, whichever context emitted them.
//!
//! The class: the abilities that trigger on becoming the target of an ability
//! (objects and players), on a crime, and ward. In MTGJSON AtomicCards
//! 5.3.0+20261101 that is 193, 21 and 325 cards. They are reached here through
//! the shared matcher, so each test covers its whole family.
//!
//! Each emission context has exactly one owner:
//! - a dispatch window collects its own range (the window collector);
//! - an action-seam construction finisher (`SelectTargets`, `ChooseTarget`,
//!   `SelectModes`) matches its own span at emission, claims it, and queues the
//!   matched abilities after its own drain (`ConstructionSpanOwner::Finisher`);
//! - a finisher that runs inside a dispatch window (the random-mode re-entry)
//!   leaves the span to the window and does not drain
//!   (`ConstructionSpanOwner::EnclosingWindow`);
//! - a drain that runs dispatch windows on the reducer action's own buffer
//!   claims the ranges its windows collected, so the post-action pipeline does
//!   not collect them a second time (`triggers::drain_on_action_buffer`);
//! - a windowless synthetic dispatch (a dungeon room, CR 309.4c) is collected by
//!   the `ChooseDungeonRoom` arm's range collection.
//!
//! In-class observers whose own trigger has a target slot (why a released or
//! nested context can emit again). Comma-form predicate
//! `(commits? a crime|becomes? the targets? of [^.]*abilit[^.]*), [^.]*\btarget\b`:
//! Mouse Trapper, Black Bolt, Inhuman King, Scalelord Reckoner, Pawpatch
//! Recruit, Beta Ray Bill, Rattleback Apothecary, Kaervek, the Punisher.
//! (At Knifepoint also matches; its "target" is inside a token's ability.)
//! Next-sentence predicate
//! `(commits? a crime|becomes? the targets? of [^.]*abilit[^.]*)[^.]*\.[^.]*\btarget\b`
//! (over the same MTGJSON face texts) adds no observer whose own trigger has a
//! target slot: every hit's "target" belongs to a different ability or to
//! reminder text (Agrus Kos, Altanak, Cathedral Acolyte, Cephalid Illusionist,
//! Crystalline Nautilus, Dwarven Mattock, Elder Owyn Lyons, Falcon's Wing
//! Harness, Frost Titan, Frostfist Strider, Hoverstone Pilgrim, Invasion of
//! Moag, Malfunctioning Holodeck, Mirrorshell Crab, Nettle Guard, Nightkin
//! Ambusher, Parnesse, Patrolling Peacemaker, Seal from Existence, Squishy
//! Sphinx Ninja, Thran Power Suit, Trapped in the Screen, Zul Ashur). Agrus
//! Kos's "could target" names the copies its resolution makes, not a target of
//! its own trigger.
//!
//! Drain sites not wrapped in `drain_on_action_buffer`, and why: no later
//! ordinary scan reads the range they append. A combat-damage window cannot be
//! rescanned (a window emission comes with a stack push, so the `CombatDamage`
//! pass skips its trigger scan, and the damage-assignment arms process triggers
//! inline). A combat declaration's batch and a resolution choice that returns
//! `WaitingForWithInlineTriggers` mark the action's triggers processed inline.
//! A priority boundary's batch (`place_priority_batch`) is placed after that
//! pass's scan. `process_triggers`, `dispatch_collected_triggers` and the
//! delayed-trigger dispatch run on a local buffer. The elimination drain is
//! wrapped inside `drain_or_clear_deferred_triggers_after_elimination`, so
//! every route to it (state-based-action losses, a concession, the
//! departure-settled safety net, the sandbox) claims the drained range; where
//! no later scan of that buffer follows, the claim is inert (the ledger is
//! cleared at the next action).

use engine::game::combat::AttackTarget;
use engine::game::dungeon::DungeonId;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::ability::{
    ControllerRef, Effect, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    ActionResult, DistributionScope, GameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::TriggerCause;
use engine::types::triggers::{CausedAbilitySource, TriggerMode};

// Verbatim Oracle text (MTGJSON AtomicCards 5.3.0+20261101).
const TAR_PIT_WARRIOR: &str =
    "When this creature becomes the target of a spell or ability, sacrifice it.";
const IROH: &str = "Haste\nMentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\nAt the beginning of combat on your turn, each creature you control with a counter on it gains firebending 2 until end of turn. (Whenever it attacks, add {R}{R}. This mana lasts until end of combat.)";
const ZUKO: &str = "Firebending 1 (Whenever this creature attacks, add {R}. This mana lasts until end of combat.)\nWhenever you cast a noncreature spell, Zuko gains first strike until end of turn.\nWhenever Zuko deals combat damage to a player, put a +1/+1 counter on him.";
const FIREBENDER_ASCENSION: &str = "When this enchantment enters, create a 2/2 red Soldier creature token with firebending 1.\nWhenever a creature you control attacking causes a triggered ability of that creature to trigger, put a quest counter on this enchantment. Then if it has four or more quest counters on it, you may copy that ability. You may choose new targets for the copy.";
const RAGING_GOBLIN: &str =
    "Haste (This creature can attack and {T} as soon as it comes under your control.)";
const GOBLIN_RACKETEER: &str = "Whenever this creature attacks, you may goad target creature defending player controls. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)";
const DEEPMUCK_DESPERADO: &str = "Whenever you commit a crime, each opponent mills three cards. This ability triggers only once each turn. (Targeting opponents, anything they control, and/or cards in their graveyards is a crime.)";
const RIMESHIELD_FROST_GIANT: &str = "Ward {3} (Whenever this creature becomes the target of a spell or ability an opponent controls, counter it unless that player pays {3}.)";
const HEARTFIRE_HERO: &str = "Valiant — Whenever this creature becomes the target of a spell or ability you control for the first time each turn, put a +1/+1 counter on it.\nWhen this creature dies, it deals damage equal to its power to each opponent.";
const MOUSE_TRAPPER: &str = "Flash\nValiant — Whenever this creature becomes the target of a spell or ability you control for the first time each turn, tap target creature an opponent controls.";
const CULT_OF_SKARO: &str = "Whenever Cult of Skaro attacks, choose one at random —\n• Thay — Put a +1/+1 counter on each artifact creature you control.\n• Caan — Draw two cards.\n• Sec — Create a 3/3 black Dalek artifact creature token with menace.\n• Jast — Each opponent loses 4 life.";
const STRICT_PROCTOR: &str = "Flying\nWhenever a permanent entering causes a triggered ability to trigger, counter that ability unless its controller pays {2}.";
const FIEND_HUNTER: &str = "When this creature enters, you may exile another target creature.\nWhen this creature leaves the battlefield, return the exiled card to the battlefield under its owner's control.";
const INFERNO_TITAN: &str = "{R}: This creature gets +1/+0 until end of turn.\nWhenever this creature enters or attacks, it deals 3 damage divided as you choose among one, two, or three targets.";
const CHARMING_PRINCE: &str = "When this creature enters, choose one —\n• Scry 2.\n• You gain 3 life.\n• Exile another target creature you own. Return it to the battlefield under your control at the beginning of the next end step.";
const WARSTORM_SURGE: &str =
    "Whenever a creature you control enters, it deals damage equal to its power to any target.";
const MAGUS_SISTERS: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II, III — Choose one at random —\n• Combine Powers! — Put three +1/+1 counters on target creature.\n• Defense! — Put a shield counter on target creature. You gain 3 life.\n• Fight! — This creature fights up to one target creature an opponent controls.\nHaste";
const COWARDICE: &str = "Whenever a creature becomes the target of a spell or ability, return that creature to its owner's hand. (It won't be affected by the spell or ability.)";
const BONECRUSHER_GIANT: &str = "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.";
const NOWHERE_TO_RUN: &str = "Flash\nWhen this enchantment enters, target creature an opponent controls gets -3/-3 until end of turn.\nCreatures your opponents control can be the targets of spells and abilities as though they didn't have hexproof. Ward abilities of those creatures don't trigger.";
const IVORY_MASK: &str = "You have shroud. (You can't be the target of spells or abilities.)";
const FATES_REVERSAL: &str = "Return up to one target creature card from your graveyard to your hand. Venture into the dungeon. (Enter the first room or advance to the next room.)";

// ---------------------------------------------------------------------------
// Typed observers (copied from `ability_triggered_observer.rs`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Observer {
    /// `{CreatureAttacking, CausingObject}`, valid_card Creature/You.
    FirebenderAscension,
    /// `{EntersBattlefield{[]}, Any}`, valid_card Permanent.
    StrictProctor,
}

impl Observer {
    fn mode(self) -> TriggerMode {
        match self {
            Observer::FirebenderAscension => TriggerMode::AbilityTriggered {
                cause: TriggerCause::CreatureAttacking,
                source: CausedAbilitySource::CausingObject,
            },
            Observer::StrictProctor => TriggerMode::AbilityTriggered {
                cause: TriggerCause::EntersBattlefield {
                    core_types: vec![],
                    entrant: engine::types::statics::CausingObject::Any,
                },
                source: CausedAbilitySource::Any,
            },
        }
    }

    fn valid_card(self) -> TargetFilter {
        match self {
            Observer::FirebenderAscension => {
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
            }
            Observer::StrictProctor => TargetFilter::Typed(TypedFilter::permanent()),
        }
    }
}

fn add_observer(scenario: &mut GameScenario, player: PlayerId, observer: Observer) -> ObjectId {
    match observer {
        Observer::FirebenderAscension => scenario
            .add_enchantment_from_oracle(player, "Firebender Ascension", FIREBENDER_ASCENSION)
            .id(),
        Observer::StrictProctor => scenario
            .add_creature_from_oracle(player, "Strict Proctor", 1, 3, STRICT_PROCTOR)
            .id(),
    }
}

/// Replace the observer line the parser cannot yet produce with its typed shape,
/// keeping the parsed effect half (as in `ability_triggered_observer.rs`).
fn install_observer(state: &mut GameState, id: ObjectId, observer: Observer) {
    let obj = state.objects.get_mut(&id).expect("observer exists");
    let defs = std::sync::Arc::make_mut(&mut obj.base_trigger_definitions);
    let slot = defs
        .iter()
        .position(|def| {
            def.description
                .as_deref()
                .is_some_and(|text| text.contains("causes a triggered ability"))
        })
        .expect("the observer line is present in the parse");
    let parsed = defs[slot].clone();
    let mut d1 = TriggerDefinition::new(observer.mode()).valid_card(observer.valid_card());
    d1.execute = parsed.execute;
    d1.optional = parsed.optional;
    d1.description = parsed.description;
    defs[slot] = d1;
    obj.materialize_base_trigger_definitions();
    reindex_object_triggers(state, id);
}

/// Every firing of an `AbilityTriggered` observer still on a carrier.
fn observer_firings(state: &GameState, observer: ObjectId) -> usize {
    let is_firing =
        |event: &Option<GameEvent>| matches!(event, Some(GameEvent::AbilityTriggered { .. }));
    let on_stack = state
        .stack
        .iter()
        .filter(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event,
                ..
            } => *source_id == observer && is_firing(trigger_event),
            _ => false,
        })
        .count();
    let queued = state
        .deferred_triggers
        .iter()
        .chain(
            state
                .pending_trigger_order
                .iter()
                .flat_map(|order| order.groups.iter())
                .flat_map(|group| group.triggers.iter()),
        )
        .filter(|ctx| ctx.pending.source_id == observer && is_firing(&ctx.pending.trigger_event))
        .count();
    on_stack + queued
}

// ---------------------------------------------------------------------------
// Board and driving helpers
// ---------------------------------------------------------------------------

fn creature(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
    oracle: &str,
) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, name, power, toughness, oracle)
        .id()
}

/// Stack indices (0 = bottom) of the triggered entries whose source is `source`.
fn trigger_positions(state: &GameState, source: ObjectId) -> Vec<usize> {
    state
        .stack
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Count(X): the triggered stack entries whose source is `source`.
fn count_triggers_from(state: &GameState, source: ObjectId) -> usize {
    trigger_positions(state, source).len()
}

/// The single triggered stack entry of `source`, with its stack index.
fn only_trigger_of(state: &GameState, source: ObjectId) -> (usize, Vec<TargetRef>) {
    let positions = trigger_positions(state, source);
    assert_eq!(
        positions.len(),
        1,
        "exactly one triggered entry of {source:?} expected, stack {:?}",
        stack_summary(state)
    );
    let entry = &state.stack[positions[0]];
    (
        positions[0],
        entry
            .ability()
            .expect("a triggered ability")
            .targets
            .clone(),
    )
}

fn stack_summary(state: &GameState) -> Vec<(ObjectId, String)> {
    state
        .stack
        .iter()
        .map(|entry| {
            let name = state
                .objects
                .get(&entry.source_id)
                .map(|obj| obj.name.clone())
                .unwrap_or_default();
            (entry.source_id, name)
        })
        .collect()
}

fn attack(runner: &mut GameRunner, attackers: &[ObjectId]) -> ActionResult {
    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "expected the declare-attackers prompt, got {:?}",
        runner.state().waiting_for
    );
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("attack declaration is accepted")
}

/// The sources listed by the open `OrderTriggers` prompt.
fn order_prompt_sources(runner: &GameRunner) -> Vec<ObjectId> {
    let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a CR 603.3b order prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    triggers.iter().map(|trigger| trigger.source_id).collect()
}

/// CR 603.3b: answer the open order prompt so the triggers of `first` (in that
/// sequence, one listed trigger per entry) are constructed first; the rest
/// follow in prompt order.
fn order_sources_first(runner: &mut GameRunner, first: &[ObjectId]) -> ActionResult {
    let sources = order_prompt_sources(runner);
    let mut used = vec![false; sources.len()];
    let mut order = Vec::new();
    for wanted in first {
        let index = (0..sources.len())
            .find(|&i| !used[i] && sources[i] == *wanted)
            .unwrap_or_else(|| panic!("{wanted:?} is not in the order prompt {sources:?}"));
        used[index] = true;
        order.push(index);
    }
    order.extend((0..sources.len()).filter(|&i| !used[i]));
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("the engine-issued order is legal")
}

fn legal_targets(runner: &GameRunner) -> Vec<TargetRef> {
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for
    else {
        panic!(
            "expected a trigger target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    target_slots[0].legal_targets.clone()
}

fn select_targets(runner: &mut GameRunner, targets: Vec<TargetRef>) -> ActionResult {
    runner
        .act(GameAction::SelectTargets { targets })
        .expect("the engine-offered targets are legal")
}

/// Answer every CR 603.3b order prompt with the identity order until the game
/// reaches `Priority`; any other prompt fails the test.
fn order_identity_until_priority(runner: &mut GameRunner) {
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("the identity order is legal");
            }
            other => panic!("unexpected prompt before Priority: {other:?}"),
        }
    }
    panic!("never reached Priority: {:?}", runner.state().waiting_for);
}

fn obj(id: ObjectId) -> TargetRef {
    TargetRef::Object(id)
}

fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).commit();
    for _ in 0..8 {
        let on_stack = runner.state().stack.iter().any(|entry| entry.id == spell);
        if !on_stack || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass is accepted");
    }
    panic!("spell never resolved");
}

fn assert_priority(result: &ActionResult, what: &str) {
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "{what}: expected Priority, got {:?}",
        result.waiting_for
    );
}

// ---------------------------------------------------------------------------
// Action-seam finisher at a prompt end
// ---------------------------------------------------------------------------

/// F-1: CR 601.2c + CR 603.3d + CR 603.2: the mentor's target is chosen by
/// `SelectTargets`; its drain ends on the order prompt for the two Ascension
/// firings. Tar Pit Warrior becoming the mentor's target triggers it once, and
/// its entry is above the mentor's (CR 603.3).
#[test]
fn f1_order_triggers_end_collects_the_finisher_span_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let tar_pit = creature(&mut scenario, P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let goblin = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[iroh, zuko, tar_pit, goblin]);
    let mut listed = order_prompt_sources(&runner);
    listed.sort();
    let mut expected = vec![iroh, zuko];
    expected.sort();
    assert_eq!(
        listed, expected,
        "the order prompt lists mentor and firebending"
    );
    order_sources_first(&mut runner, &[zuko, iroh]);

    let legal = legal_targets(&runner);
    for target in [zuko, tar_pit, goblin] {
        assert!(
            legal.contains(&obj(target)),
            "{target:?} is a mentor target"
        );
    }
    let result = select_targets(&mut runner, vec![obj(tar_pit)]);
    let WaitingFor::OrderTriggers { triggers, .. } = &result.waiting_for else {
        panic!(
            "reach guard: the drain prompts the two Ascension firings, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(triggers.len(), 2);
    assert!(triggers.iter().all(|t| t.source_id == ascension));
    order_identity_until_priority(&mut runner);

    let state = runner.state();
    let (mentor_index, mentor_targets) = only_trigger_of(state, iroh);
    assert_eq!(mentor_targets, vec![obj(tar_pit)]);
    let tar_pit_count = count_triggers_from(state, tar_pit);
    assert_eq!(tar_pit_count, 1, "Count(Tar Pit) = {tar_pit_count}");
    assert!(
        trigger_positions(state, tar_pit)[0] > mentor_index,
        "CR 603.3: the Tar Pit trigger is above the mentor entry"
    );
}

/// F-2: the finisher's drain ends on a second `TriggerTargetSelection` (the goad).
#[test]
fn f2_second_target_prompt_end_collects_the_finisher_span_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let tar_pit = creature(&mut scenario, P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let goblin = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let racketeer = creature(
        &mut scenario,
        P0,
        "Goblin Racketeer",
        4,
        2,
        GOBLIN_RACKETEER,
    );
    let bear_1 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let _bear_2 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    attack(&mut runner, &[iroh, tar_pit, goblin, racketeer]);
    order_sources_first(&mut runner, &[iroh, racketeer]);
    let legal = legal_targets(&runner);
    assert!(legal.contains(&obj(tar_pit)) && legal.contains(&obj(goblin)));
    assert!(!legal.contains(&obj(racketeer)), "4 is not lesser power");

    let result = select_targets(&mut runner, vec![obj(tar_pit)]);
    let WaitingFor::TriggerTargetSelection { source_id, .. } = &result.waiting_for else {
        panic!(
            "reach guard: the drain prompts the goad's targets, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(*source_id, Some(racketeer));
    let result = select_targets(&mut runner, vec![obj(bear_1)]);
    assert_priority(&result, "after the goad's target");

    let state = runner.state();
    let (mentor_index, _) = only_trigger_of(state, iroh);
    let (_, goad_targets) = only_trigger_of(state, racketeer);
    assert_eq!(goad_targets, vec![obj(bear_1)]);
    let tar_pit_count = count_triggers_from(state, tar_pit);
    assert_eq!(tar_pit_count, 1, "Count(Tar Pit) = {tar_pit_count}");
    assert!(trigger_positions(state, tar_pit)[0] > mentor_index);
}

/// F-3: CR 700.13: the goad's target is P1's creature, a crime, committed by a
/// finisher whose drain ends on the mentor's prompt. The mentor's own-creature
/// target commits none (paired with the goad's crime in the same sequence).
#[test]
fn f3_crime_of_a_finisher_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let goblin_1 = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let goblin_2 = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let racketeer = creature(
        &mut scenario,
        P0,
        "Goblin Racketeer",
        4,
        2,
        GOBLIN_RACKETEER,
    );
    let deepmuck = creature(
        &mut scenario,
        P0,
        "Deepmuck Desperado",
        2,
        4,
        DEEPMUCK_DESPERADO,
    );
    let bear_1 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let _bear_2 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    attack(&mut runner, &[iroh, goblin_1, goblin_2, racketeer]);
    order_sources_first(&mut runner, &[racketeer, iroh]);
    let result = select_targets(&mut runner, vec![obj(bear_1)]);
    let WaitingFor::TriggerTargetSelection { source_id, .. } = &result.waiting_for else {
        panic!(
            "reach guard: the drain prompts the mentor's targets, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(*source_id, Some(iroh));
    let result = select_targets(&mut runner, vec![obj(goblin_1)]);
    assert_priority(&result, "after the mentor's target");

    let deepmuck_count = count_triggers_from(runner.state(), deepmuck);
    assert_eq!(deepmuck_count, 1, "Count(Deepmuck) = {deepmuck_count}");
}

/// F-4: CR 702.21a: ward triggers on the goad's targeting of the Giant.
#[test]
fn f4_ward_of_a_finisher_target_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let goblin = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let racketeer = creature(
        &mut scenario,
        P0,
        "Goblin Racketeer",
        4,
        2,
        GOBLIN_RACKETEER,
    );
    let giant = creature(
        &mut scenario,
        P1,
        "Rimeshield Frost Giant",
        4,
        5,
        RIMESHIELD_FROST_GIANT,
    );
    let _bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&giant]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Ward(_))),
        "reach guard: the Giant has ward"
    );

    attack(&mut runner, &[iroh, goblin, zuko, racketeer]);
    order_sources_first(&mut runner, &[racketeer, iroh]);
    let result = select_targets(&mut runner, vec![obj(giant)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::TriggerTargetSelection { source_id, .. } if source_id == Some(iroh)),
        "reach guard: the drain prompts the mentor, got {:?}",
        result.waiting_for
    );
    let result = select_targets(&mut runner, vec![obj(goblin)]);
    assert_priority(&result, "after the mentor's target");

    let state = runner.state();
    let ward = trigger_positions(state, giant);
    assert_eq!(ward.len(), 1, "Count(ward) = {}", ward.len());
    assert_eq!(state.stack[ward[0]].controller, P1);
    let (goad_index, _) = only_trigger_of(state, racketeer);
    assert!(ward[0] > goad_index, "the ward trigger is above the goad");
}

/// F-5: valiant (the "you control" sibling) on the mentor's target.
#[test]
fn f5_valiant_of_a_finisher_target_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let heartfire = creature(&mut scenario, P0, "Heartfire Hero", 1, 1, HEARTFIRE_HERO);
    let goblin = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let racketeer = creature(
        &mut scenario,
        P0,
        "Goblin Racketeer",
        4,
        2,
        GOBLIN_RACKETEER,
    );
    let bear_1 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let _bear_2 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    attack(&mut runner, &[iroh, heartfire, goblin, racketeer]);
    order_sources_first(&mut runner, &[iroh, racketeer]);
    let result = select_targets(&mut runner, vec![obj(heartfire)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::TriggerTargetSelection { source_id, .. } if source_id == Some(racketeer)),
        "reach guard: the drain prompts the goad, got {:?}",
        result.waiting_for
    );
    let result = select_targets(&mut runner, vec![obj(bear_1)]);
    assert_priority(&result, "after the goad's target");

    let count = count_triggers_from(runner.state(), heartfire);
    assert_eq!(count, 1, "Count(Heartfire valiant) = {count}");
}

// ---------------------------------------------------------------------------
// Priority end: placement above the drained sibling, exact count
// ---------------------------------------------------------------------------

/// F-6: CR 603.3b: Fiend Hunter's ETB targets Tar Pit Warrior; the drain places
/// Proctor's firing and the action returns `Priority`. Tar Pit's trigger goes on
/// last: bottom→top Fiend Hunter ETB, Proctor firing, Tar Pit.
#[test]
fn f6_priority_end_places_the_span_once_above_the_drained_sibling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proctor = add_observer(&mut scenario, P0, Observer::StrictProctor);
    let tar_pit = creature(&mut scenario, P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let hunter = scenario
        .add_creature_to_hand_from_oracle(P0, "Fiend Hunter", 1, 3, FIEND_HUNTER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), proctor, Observer::StrictProctor);

    cast_and_resolve(&mut runner, hunter);
    let legal = legal_targets(&runner);
    assert!(legal.contains(&obj(proctor)) && legal.contains(&obj(tar_pit)));
    let result = select_targets(&mut runner, vec![obj(tar_pit)]);
    assert_priority(&result, "F-6 returns Priority directly");

    let state = runner.state();
    assert_eq!(
        observer_firings(state, proctor),
        1,
        "exactly one Proctor firing"
    );
    let sources: Vec<ObjectId> = state.stack.iter().map(|e| e.source_id).collect();
    assert_eq!(
        sources,
        vec![hunter, proctor, tar_pit],
        "bottom→top: Fiend Hunter ETB, Proctor firing, Tar Pit ({:?})",
        stack_summary(state)
    );
}

/// F-6c: the crime sibling at a `Priority` end.
#[test]
fn f6c_priority_end_crime_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let deepmuck = creature(
        &mut scenario,
        P0,
        "Deepmuck Desperado",
        2,
        4,
        DEEPMUCK_DESPERADO,
    );
    let bear_1 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let _bear_2 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let hunter = scenario
        .add_creature_to_hand_from_oracle(P0, "Fiend Hunter", 1, 3, FIEND_HUNTER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, hunter);
    assert_eq!(legal_targets(&runner).len(), 3);
    let result = select_targets(&mut runner, vec![obj(bear_1)]);
    assert_priority(&result, "F-6c");

    let state = runner.state();
    let (hunter_index, hunter_targets) = only_trigger_of(state, hunter);
    assert_eq!(hunter_targets, vec![obj(bear_1)]);
    let positions = trigger_positions(state, deepmuck);
    assert_eq!(positions.len(), 1, "Count(Deepmuck) = {}", positions.len());
    assert!(positions[0] > hunter_index);
}

// ---------------------------------------------------------------------------
// DistributeAmong end
// ---------------------------------------------------------------------------

fn titan_divided_board(
    p1_creatures: &[(&str, i32, i32, Option<&str>)],
    with_nowhere: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let titan = scenario
        .add_creature_from_oracle(P0, "Inferno Titan", 6, 6, INFERNO_TITAN)
        .with_subtypes(vec!["Giant"])
        .id();
    if with_nowhere {
        scenario.add_enchantment_from_oracle(P0, "Nowhere to Run", NOWHERE_TO_RUN);
    }
    let theirs = p1_creatures
        .iter()
        .map(|(name, power, toughness, oracle)| match oracle {
            Some(oracle) => creature(&mut scenario, P1, name, *power, *toughness, oracle),
            None => scenario.add_creature(P1, name, *power, *toughness).id(),
        })
        .collect();
    let mut runner = scenario.build();
    attack(&mut runner, &[titan]);
    (runner, titan, theirs)
}

fn divide(runner: &mut GameRunner, distribution: Vec<(TargetRef, u32)>) -> ActionResult {
    runner
        .act(GameAction::DistributeAmong { distribution })
        .expect("the division is legal")
}

/// F-7: CR 601.2d: finalize returns `DistributeAmong` before any drain.
#[test]
fn f7_distribute_among_end_collects_the_finisher_span_once() {
    let (mut runner, titan, theirs) = titan_divided_board(
        &[
            ("Tar Pit Warrior", 3, 4, Some(TAR_PIT_WARRIOR)),
            ("Grizzly Bears", 2, 2, None),
        ],
        false,
    );
    let tar_pit = theirs[0];
    let result = select_targets(&mut runner, vec![obj(tar_pit), TargetRef::Player(P1)]);
    let WaitingFor::DistributeAmong {
        targets,
        scope,
        total,
        ..
    } = &result.waiting_for
    else {
        panic!(
            "reach guard: two targets divide, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(*scope, DistributionScope::AnnouncedTargets);
    assert_eq!(*total, 3);
    assert_eq!(targets.len(), 2);
    let result = divide(
        &mut runner,
        vec![(obj(tar_pit), 1), (TargetRef::Player(P1), 2)],
    );
    assert_priority(&result, "after the division");

    let state = runner.state();
    let (titan_index, _) = only_trigger_of(state, titan);
    let positions = trigger_positions(state, tar_pit);
    assert_eq!(positions.len(), 1, "Count(Tar Pit) = {}", positions.len());
    assert!(positions[0] > titan_index);
}

/// F-7h: CR 603.2 + CR 702.21a: in one finalize, Tar Pit triggers once,
/// Bonecrusher Giant (its trigger event is becoming the target of a spell) never,
/// and the ward triggers unless Nowhere to Run suppresses it (read at emission).
#[test]
fn f7h_hostile_siblings_in_one_divided_finalize() {
    for with_nowhere in [false, true] {
        let (mut runner, _titan, theirs) = titan_divided_board(
            &[
                ("Tar Pit Warrior", 3, 4, Some(TAR_PIT_WARRIOR)),
                ("Rimeshield Frost Giant", 4, 5, Some(RIMESHIELD_FROST_GIANT)),
                ("Bonecrusher Giant", 4, 3, Some(BONECRUSHER_GIANT)),
            ],
            with_nowhere,
        );
        let (tar_pit, giant, bonecrusher) = (theirs[0], theirs[1], theirs[2]);
        // Reach guard (CR 603.2): the zero-count below only means something if
        // Bonecrusher Giant carries its parsed, executable spell-only observer.
        let giant_triggers = &runner.state().objects[&bonecrusher].base_trigger_definitions;
        assert!(
            giant_triggers.iter().any(|def| {
                def.mode == TriggerMode::BecomesTarget
                    && def.valid_source == Some(TargetFilter::StackSpell)
                    && def
                        .execute
                        .as_ref()
                        .is_some_and(|ability| matches!(*ability.effect, Effect::DealDamage { .. }))
            }),
            "Bonecrusher Giant parses a BecomesTarget(StackSpell) trigger that deals damage: {giant_triggers:?}"
        );
        let result = select_targets(
            &mut runner,
            vec![obj(tar_pit), obj(giant), obj(bonecrusher)],
        );
        let WaitingFor::DistributeAmong { targets, .. } = &result.waiting_for else {
            panic!(
                "reach guard: three targets divide, got {:?}",
                result.waiting_for
            );
        };
        assert_eq!(targets.len(), 3);
        divide(
            &mut runner,
            vec![(obj(tar_pit), 1), (obj(giant), 1), (obj(bonecrusher), 1)],
        );
        // CR 603.3b: without Nowhere to Run, P1 controls two abilities that
        // triggered at once (Tar Pit's and the ward) and orders them.
        if with_nowhere {
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "nowhere: one trigger, no order prompt: {:?}",
                runner.state().waiting_for
            );
        } else {
            let mut sources = order_prompt_sources(&runner);
            sources.sort();
            let mut expected = vec![tar_pit, giant];
            expected.sort();
            assert_eq!(sources, expected, "P1 orders exactly Tar Pit and the ward");
            order_identity_until_priority(&mut runner);
        }

        let state = runner.state();
        let tar_pit_count = count_triggers_from(state, tar_pit);
        assert_eq!(
            tar_pit_count, 1,
            "nowhere {with_nowhere}: Count(Tar Pit) = {tar_pit_count}"
        );
        assert_eq!(
            count_triggers_from(state, bonecrusher),
            0,
            "Bonecrusher: a spell only"
        );
        let ward = count_triggers_from(state, giant);
        assert_eq!(
            ward,
            usize::from(!with_nowhere),
            "nowhere {with_nowhere}: Count(ward) = {ward}"
        );
    }
}

// ---------------------------------------------------------------------------
// SelectModes action seam
// ---------------------------------------------------------------------------

/// F-8: CR 603.3c + CR 700.2b: Charming Prince's exile mode is chosen by
/// `SelectModes`; its only legal target is auto-assigned, and the drain ends on
/// Warstorm Surge's target prompt.
#[test]
fn f8_select_modes_finisher_collects_its_span_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let surge = scenario
        .add_enchantment_from_oracle(P0, "Warstorm Surge", WARSTORM_SURGE)
        .id();
    let tar_pit = creature(&mut scenario, P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let prince = scenario
        .add_creature_to_hand_from_oracle(P0, "Charming Prince", 2, 2, CHARMING_PRINCE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    cast_and_resolve(&mut runner, prince);
    order_sources_first(&mut runner, &[prince, surge]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "the Prince's ETB asks for its mode, got {:?}",
        runner.state().waiting_for
    );
    let result = runner
        .act(GameAction::SelectModes { indices: vec![2] })
        .expect("the exile mode is legal");
    assert!(
        matches!(result.waiting_for, WaitingFor::TriggerTargetSelection { source_id, .. } if source_id == Some(surge)),
        "reach guard: the drain prompts Warstorm Surge, got {:?}",
        result.waiting_for
    );
    let result = select_targets(&mut runner, vec![TargetRef::Player(P1)]);
    assert_priority(&result, "after the Surge's target");

    let state = runner.state();
    let (prince_index, prince_targets) = only_trigger_of(state, prince);
    assert_eq!(
        prince_targets,
        vec![obj(tar_pit)],
        "the exile mode targets Tar Pit"
    );
    let positions = trigger_positions(state, tar_pit);
    assert_eq!(positions.len(), 1, "Count(Tar Pit) = {}", positions.len());
    assert!(positions[0] > prince_index);
}

// ---------------------------------------------------------------------------
// Windowless synthetic dispatch (dungeon rooms)
// ---------------------------------------------------------------------------

fn fates_reversal_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Fates' Reversal", false, FATES_REVERSAL)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn set_dungeon(runner: &mut GameRunner, dungeon: DungeonId, room: u8) {
    let progress = runner.state_mut().dungeon_progress.entry(P0).or_default();
    progress.current_dungeon = Some(dungeon);
    progress.current_room = room;
}

/// Cast Fates' Reversal with no target and pass priority until its venture asks
/// for the room; answer `room_index`; reach `Priority`.
fn venture_through_fates_reversal(runner: &mut GameRunner, spell: ObjectId, room_index: u8) {
    cast_and_resolve(runner, spell);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseDungeonRoom { .. }
        ),
        "reach guard: the venture asks for the room, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .resolving_stack_entry
            .as_ref()
            .is_some_and(|entry| entry.id == spell),
        "the room is chosen while Fates' Reversal resolves"
    );
    let result = runner
        .act(GameAction::ChooseDungeonRoom { room_index })
        .expect("the room is legal");
    assert_priority(&result, "after the room choice");
}

/// F-9: CR 309.4c: Storeroom's room ability is P0's and auto-targets Heartfire
/// Hero; the `ChooseDungeonRoom` arm collects valiant once.
#[test]
fn f9_room_ability_target_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let heartfire = creature(&mut scenario, P0, "Heartfire Hero", 1, 1, HEARTFIRE_HERO);
    let spell = fates_reversal_in_hand(&mut scenario);
    let mut runner = scenario.build();
    // Lost Mine of Phandelver, marker on Goblin Lair (room 1 → Storeroom 3, Dark Pool 4).
    set_dungeon(&mut runner, DungeonId::LostMineOfPhandelver, 1);

    venture_through_fates_reversal(&mut runner, spell, 3);

    let state = runner.state();
    let storeroom = state
        .stack
        .iter()
        .position(|entry| {
            entry
                .ability()
                .is_some_and(|ability| ability.targets == vec![obj(heartfire)])
                && entry.source_id != heartfire
        })
        .unwrap_or_else(|| {
            panic!(
                "the Storeroom entry targets Heartfire: {:?}",
                stack_summary(state)
            )
        });
    let positions = trigger_positions(state, heartfire);
    assert_eq!(positions.len(), 1, "Count(valiant) = {}", positions.len());
    assert!(positions[0] > storeroom);
}

/// F-9c: CR 700.13: Trap! targets P1 (P0 has shroud), a crime committed at the
/// synthetic dispatch's push; Fates' Reversal itself targets nothing.
#[test]
fn f9c_room_ability_crime_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Ivory Mask", IVORY_MASK);
    let deepmuck = creature(
        &mut scenario,
        P0,
        "Deepmuck Desperado",
        2,
        4,
        DEEPMUCK_DESPERADO,
    );
    let spell = fates_reversal_in_hand(&mut scenario);
    let mut runner = scenario.build();
    // Undercity, marker on Forge (room 1 → Trap! 3, Arena 4).
    set_dungeon(&mut runner, DungeonId::Undercity, 1);

    venture_through_fates_reversal(&mut runner, spell, 3);

    let state = runner.state();
    assert!(
        state.stack.iter().any(|entry| entry
            .ability()
            .is_some_and(|a| a.targets == vec![TargetRef::Player(P1)])),
        "the Trap! entry targets P1: {:?}",
        stack_summary(state)
    );
    let count = count_triggers_from(state, deepmuck);
    assert_eq!(count, 1, "Count(Deepmuck) = {count}");
}

// ---------------------------------------------------------------------------
// In-window finisher (random modes)
// ---------------------------------------------------------------------------

fn magus_on_battlefield(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Summon: Magus Sisters", 5, 5)
        .as_enchantment()
        .as_creature()
        .with_subtypes(vec!["Saga", "Faerie"])
        .from_oracle_text(MAGUS_SISTERS)
        .id()
}

fn chapter_is_combine_powers(state: &GameState, magus: ObjectId) -> bool {
    trigger_positions(state, magus).iter().any(|&index| {
        state.stack[index].ability().is_some_and(|ability| {
            matches!(
                ability.effect,
                Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    ..
                }
            )
        })
    })
}

/// M-1 board in a game seeded with `seed`: chapter I fires as P0's precombat
/// main phase begins (CR 714.3c; the chapter trigger is CR 714.2b).
fn m1_fire_chapter_one(seed: u64) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    let magus = magus_on_battlefield(&mut scenario);
    let cowardice = scenario
        .add_enchantment_from_oracle(P0, "Cowardice", COWARDICE)
        .id();
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    (runner, magus, cowardice)
}

/// M-1: CR 603.2c: chapter I's random mode is chosen inside its dispatch window
/// and auto-targets Magus Sisters; Cowardice triggers once.
#[test]
fn m1_in_window_finisher_span_is_collected_once() {
    let (runner, magus, cowardice) = m1_fire_chapter_one(0);
    let state = runner.state();
    assert!(
        chapter_is_combine_powers(state, magus),
        "reach guard: seed 0 draws Combine Powers! targeting Magus Sisters: {:?} waiting {:?}",
        stack_summary(state),
        state.waiting_for
    );
    let (_, targets) = only_trigger_of(state, magus);
    assert_eq!(targets, vec![obj(magus)]);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "completion boundary: waiting {:?}",
        state.waiting_for
    );
    assert!(
        state.deferred_triggers.is_empty(),
        "completion boundary: deferred queue not drained"
    );
    assert!(
        state.pending_trigger_order.is_none(),
        "completion boundary: trigger ordering still pending"
    );
    let count = count_triggers_from(state, cowardice);
    assert_eq!(count, 1, "Count(Cowardice) = {count}");
}

/// The N-1 board: Iroh, Mouse Trapper and Cult of Skaro attack; P1 controls
/// exactly Tar Pit Warrior. Mentor's only legal target is Mouse Trapper.
fn skaro_board(with_ascension: bool) -> (GameRunner, Option<ObjectId>, [ObjectId; 4]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension =
        with_ascension.then(|| add_observer(&mut scenario, P0, Observer::FirebenderAscension));
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let trapper = creature(&mut scenario, P0, "Mouse Trapper", 3, 2, MOUSE_TRAPPER);
    let skaro = scenario
        .add_creature_from_oracle(P0, "Cult of Skaro", 4, 4, CULT_OF_SKARO)
        .as_artifact_creature()
        .id();
    let tar_pit = creature(&mut scenario, P1, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let mut runner = scenario.build();
    if let Some(ascension) = ascension {
        install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);
    }
    attack(&mut runner, &[iroh, trapper, skaro]);
    (runner, ascension, [iroh, trapper, skaro, tar_pit])
}

/// N-1: the nested finalize with the typed observer beside the target-lock observer. The
/// batch's end-of-batch drain prompts valiant and both Ascension firings
/// together (CR 603.3b).
#[test]
fn n1_in_window_random_mode_does_not_drain_mid_batch() {
    let (mut runner, ascension, [iroh, trapper, skaro, tar_pit]) = skaro_board(true);
    let ascension = ascension.expect("board has Ascension");
    let mut listed = order_prompt_sources(&runner);
    listed.sort();
    let mut expected = vec![iroh, skaro];
    expected.sort();
    assert_eq!(listed, expected);
    let result = order_sources_first(&mut runner, &[iroh, skaro]);
    let WaitingFor::OrderTriggers { triggers, .. } = &result.waiting_for else {
        panic!(
            "the batch ends on an order prompt, got {:?}",
            result.waiting_for
        );
    };
    let sources: Vec<ObjectId> = triggers.iter().map(|t| t.source_id).collect();
    assert_eq!(
        sources.len(),
        3,
        "the second order prompt lists valiant and both Ascension firings: {sources:?}"
    );
    assert_eq!(sources.iter().filter(|s| **s == trapper).count(), 1);
    assert_eq!(sources.iter().filter(|s| **s == ascension).count(), 2);
    order_sources_first(&mut runner, &[trapper, ascension, ascension]);
    order_identity_until_priority(&mut runner);

    let state = runner.state();
    let (_, mentor_targets) = only_trigger_of(state, iroh);
    assert_eq!(mentor_targets, vec![obj(trapper)]);
    let (valiant_index, valiant_targets) = only_trigger_of(state, trapper);
    assert_eq!(valiant_targets, vec![obj(tar_pit)]);
    assert_eq!(observer_firings(state, ascension), 2);
    let positions = trigger_positions(state, tar_pit);
    assert_eq!(positions.len(), 1, "Count(Tar Pit) = {}", positions.len());
    assert!(positions[0] > valiant_index);
}

/// N-1b: the nested finalize's count discriminator (no Ascension).
#[test]
fn n1b_in_window_random_mode_collects_the_nested_target_once() {
    let (mut runner, _, [iroh, trapper, skaro, tar_pit]) = skaro_board(false);
    let result = order_sources_first(&mut runner, &[iroh, skaro]);
    assert_priority(&result, "N-1b");

    let state = runner.state();
    let (_, mentor_targets) = only_trigger_of(state, iroh);
    assert_eq!(mentor_targets, vec![obj(trapper)]);
    assert_eq!(
        count_triggers_from(state, skaro),
        1,
        "Cult's entry is on the stack"
    );
    let (_, valiant_targets) = only_trigger_of(state, trapper);
    assert_eq!(valiant_targets, vec![obj(tar_pit)]);
    let count = count_triggers_from(state, tar_pit);
    assert_eq!(count, 1, "Count(Tar Pit) = {count}");
}

// ---------------------------------------------------------------------------
// Drain windows on the action buffer (`triggers::drain_on_action_buffer`)
// ---------------------------------------------------------------------------

/// W-1: CR 603.2c: the finisher's drain dispatches the goad in a window on the
/// action buffer; it auto-targets Tar Pit Warrior (a crime) and the action ends
/// at `Priority`. Each occurrence is collected once.
#[test]
fn w1_drain_window_on_the_action_buffer_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let goblin_1 = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let goblin_2 = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let racketeer = creature(
        &mut scenario,
        P0,
        "Goblin Racketeer",
        4,
        2,
        GOBLIN_RACKETEER,
    );
    let deepmuck = creature(
        &mut scenario,
        P0,
        "Deepmuck Desperado",
        2,
        4,
        DEEPMUCK_DESPERADO,
    );
    let tar_pit = creature(&mut scenario, P1, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let mut runner = scenario.build();

    attack(&mut runner, &[iroh, goblin_1, goblin_2, racketeer]);
    order_sources_first(&mut runner, &[iroh, racketeer]);
    let result = select_targets(&mut runner, vec![obj(goblin_1)]);
    assert_priority(&result, "W-1");

    let state = runner.state();
    let (_, goad_targets) = only_trigger_of(state, racketeer);
    assert_eq!(goad_targets, vec![obj(tar_pit)]);
    let tar_pit_count = count_triggers_from(state, tar_pit);
    let deepmuck_count = count_triggers_from(state, deepmuck);
    assert_eq!(
        (tar_pit_count, deepmuck_count),
        (1, 1),
        "Count(Tar Pit), Count(Deepmuck)"
    );
}

/// W-2: a held context with its own target slot (Mouse Trapper's valiant) is
/// released and dispatched in a window on the next action's buffer.
#[test]
fn w2_released_context_window_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let iroh = creature(&mut scenario, P0, "Iroh, Dragon of the West", 4, 4, IROH);
    let trapper = creature(&mut scenario, P0, "Mouse Trapper", 3, 2, MOUSE_TRAPPER);
    let goblin = creature(&mut scenario, P0, "Raging Goblin", 1, 1, RAGING_GOBLIN);
    let titan = scenario
        .add_creature_from_oracle(P0, "Inferno Titan", 6, 6, INFERNO_TITAN)
        .with_subtypes(vec!["Giant"])
        .id();
    let tar_pit = creature(&mut scenario, P1, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let mut runner = scenario.build();

    attack(&mut runner, &[iroh, trapper, goblin, titan]);
    order_sources_first(&mut runner, &[iroh, titan]);
    let result = select_targets(&mut runner, vec![obj(trapper)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::TriggerTargetSelection { source_id, .. } if source_id == Some(titan)),
        "reach guard: the drain prompts the Titan, got {:?}",
        result.waiting_for
    );
    let result = select_targets(&mut runner, vec![TargetRef::Player(P1)]);
    assert_priority(&result, "W-2");

    let state = runner.state();
    let (_, valiant_targets) = only_trigger_of(state, trapper);
    assert_eq!(valiant_targets, vec![obj(tar_pit)]);
    let count = count_triggers_from(state, tar_pit);
    assert_eq!(count, 1, "Count(Tar Pit) = {count}");
}

/// W-4: CR 603.2c (each ability that targets it is its own trigger event): two
/// abilities each make Tar Pit a target, so there are two trigger events. The
/// `DistributeAmong` arm's drain dispatches the goad in a window on the action
/// buffer.
#[test]
fn w4_distribute_among_arm_drain_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let titan = scenario
        .add_creature_from_oracle(P0, "Inferno Titan", 6, 6, INFERNO_TITAN)
        .with_subtypes(vec!["Giant"])
        .id();
    let racketeer = creature(
        &mut scenario,
        P0,
        "Goblin Racketeer",
        4,
        2,
        GOBLIN_RACKETEER,
    );
    let tar_pit = creature(&mut scenario, P1, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR);
    let mut runner = scenario.build();

    attack(&mut runner, &[titan, racketeer]);
    order_sources_first(&mut runner, &[titan, racketeer]);
    let result = select_targets(&mut runner, vec![obj(tar_pit), TargetRef::Player(P1)]);
    let WaitingFor::DistributeAmong { targets, .. } = &result.waiting_for else {
        panic!(
            "reach guard: two targets divide, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(targets.len(), 2);
    let result = divide(
        &mut runner,
        vec![(obj(tar_pit), 1), (TargetRef::Player(P1), 2)],
    );
    assert_priority(&result, "W-4");

    let state = runner.state();
    let (titan_index, _) = only_trigger_of(state, titan);
    let (_, goad_targets) = only_trigger_of(state, racketeer);
    assert_eq!(goad_targets, vec![obj(tar_pit)]);
    let positions = trigger_positions(state, tar_pit);
    assert_eq!(positions.len(), 2, "Count(Tar Pit) = {}", positions.len());
    assert!(positions.iter().all(|&index| index > titan_index));
}
