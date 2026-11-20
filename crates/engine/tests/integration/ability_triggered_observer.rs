//! CR 603.2 + CR 603.3b: an ability-triggering observer ("Whenever [cause]
//! causes a triggered ability [of that creature] to trigger") triggers exactly
//! once for each attack- or enter-caused triggered occurrence, at its push or
//! at its removal during placement (CR 603.3c, CR 603.3d).
//!
//! The observers are the D1 shapes of the printed cards (phase 1 of the
//! Firebender Ascension class: the parser cannot produce them until phase 4).
//! Each is built from its verbatim Oracle text; only the trigger condition the
//! parser cannot yet produce is replaced, keeping today's parse of its effect.
//!
//! Oracle text (card data):
//! - Firebender Ascension: "When this enchantment enters, create a 2/2 red
//!   Soldier creature token with firebending 1. / Whenever a creature you
//!   control attacking causes a triggered ability of that creature to trigger,
//!   put a quest counter on this enchantment. Then if it has four or more quest
//!   counters on it, you may copy that ability. You may choose new targets for
//!   the copy."
//! - Strict Proctor: "Flying / Whenever a permanent entering causes a
//!   triggered ability to trigger, counter that ability unless its controller
//!   pays {2}."
//! - Aboleth Spawn: "Flash / Ward {2} / Probing Telepathy — Whenever a
//!   creature entering under an opponent's control causes a triggered ability
//!   of that creature to trigger, you may copy that ability. You may choose new
//!   targets for the copy."
//!
//! The CR 603.3b placement tests (the last section) put each first-part ability
//! of every player on the stack before any second-part ability (one whose
//! trigger condition is another ability triggering), and an ability that
//! triggered while a placement process was put on the stack after that whole
//! process, in the next repetition. Each board is built from its cards'
//! verbatim Oracle text.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::PendingTriggerContext;
use engine::game::visibility::{
    filter_events_for_viewer, filter_state_for_viewer, HIDDEN_CARD_NAME,
};
use engine::types::ability::{
    ControllerRef, StackAbilityKind, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::PrintedCardRef;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, TriggeredAbilityOrigin};
use engine::types::game_state::{
    ActionResult, AutoMayChoice, GameState, MayTriggerAutoChoiceKey, MayTriggerAutoChoiceSelector,
    MayTriggerOrigin, StackEntryKind, StackObjectClass, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::TriggerCause;
use engine::types::triggers::{CausedAbilitySource, TriggerEventKey, TriggerMode};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const FIREBENDER_ASCENSION: &str = "When this enchantment enters, create a 2/2 red Soldier creature token with firebending 1.\nWhenever a creature you control attacking causes a triggered ability of that creature to trigger, put a quest counter on this enchantment. Then if it has four or more quest counters on it, you may copy that ability. You may choose new targets for the copy.";
const STRICT_PROCTOR: &str = "Flying\nWhenever a permanent entering causes a triggered ability to trigger, counter that ability unless its controller pays {2}.";
const ABOLETH_SPAWN: &str = "Flash\nWard {2}\nProbing Telepathy — Whenever a creature entering under an opponent's control causes a triggered ability of that creature to trigger, you may copy that ability. You may choose new targets for the copy.";

const ZUKO: &str = "Firebending 1 (Whenever this creature attacks, add {R}. This mana lasts until end of combat.)\nWhenever you cast a noncreature spell, Zuko gains first strike until end of turn.\nWhenever Zuko deals combat damage to a player, put a +1/+1 counter on him.";
const AZULA: &str = "Firebending 2 (Whenever this creature attacks, add {R}{R}. This mana lasts until end of combat.)\nWhenever Azula attacks, you lose 1 life and create a Clue token. (It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")";
const IROH: &str = "Haste\nMentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\nAt the beginning of combat on your turn, each creature you control with a counter on it gains firebending 2 until end of turn. (Whenever it attacks, add {R}{R}. This mana lasts until end of combat.)";
const ROT_CURSE_RAKSHASA: &str = "Trample\nDecayed (This creature can't block. When it attacks, sacrifice it at end of combat.)\nRenew — {X}{B}{B}, Exile this card from your graveyard: Put a decayed counter on each of X target creatures. Activate only as a sorcery.";
const AKRASAN_SQUIRE: &str = "Exalted (Whenever a creature you control attacks alone, that creature gets +1/+1 until end of turn.)";
const RAGING_GOBLIN: &str =
    "Haste (This creature can attack and {T} as soon as it comes under your control.)";
const ELVISH_VISIONARY: &str = "When this creature enters, draw a card.";
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
const LLANOWAR_ELVES: &str = "{T}: Add {G}.";
const GRAVEDIGGER: &str = "When this creature enters, you may return target creature card from your graveyard to your hand.";
const GOBLIN_RACKETEER: &str = "Whenever this creature attacks, you may goad target creature defending player controls. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)";
const ISSHIN: &str = "If a creature attacking causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
const MONASTERY_SWIFTSPEAR: &str = "Haste\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)";
const SUTURE_PRIEST: &str = "Whenever another creature you control enters, you may gain 1 life.\nWhenever a creature an opponent controls enters, you may have that player lose 1 life.";
const FIEND_HUNTER: &str = "When this creature enters, you may exile another target creature.\nWhen this creature leaves the battlefield, return the exiled card to the battlefield under its owner's control.";
const ELITE_SCALEGUARD: &str = "When this creature enters, bolster 2. (Choose a creature with the least toughness among creatures you control and put two +1/+1 counters on it.)\nWhenever a creature you control with a +1/+1 counter on it attacks, tap target creature defending player controls.";
const SANCTUM_SEEKER: &str =
    "Whenever a Vampire you control attacks, each opponent loses 1 life and you gain 1 life.";
const VAMPIRE_INTERLOPER: &str = "Flying\nThis creature can't block.";
const RAID_BOMBARDMENT: &str = "Whenever a creature you control with power 2 or less attacks, this enchantment deals 1 damage to the player or planeswalker that creature is attacking.";
const SOUL_SUMMONS: &str = "Manifest the top card of your library. (Put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)";
const SIDISIS_FAITHFUL: &str = "Exploit (When this creature enters, you may sacrifice a creature.)\nWhen this creature exploits a creature, return target creature to its owner's hand.";

/// Which printed observer a D1 shape stands for (charter D1).
#[derive(Clone, Copy)]
enum Observer {
    /// `{CreatureAttacking, CausingObject}`, valid_card Creature/You.
    FirebenderAscension,
    /// `{EntersBattlefield{[]}, CausingObject}`, valid_card Creature/Opponent.
    AbolethSpawn,
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
            Observer::AbolethSpawn => TriggerMode::AbilityTriggered {
                cause: TriggerCause::EntersBattlefield { core_types: vec![] },
                source: CausedAbilitySource::CausingObject,
            },
            Observer::StrictProctor => TriggerMode::AbilityTriggered {
                cause: TriggerCause::EntersBattlefield { core_types: vec![] },
                source: CausedAbilitySource::Any,
            },
        }
    }

    fn valid_card(self) -> TargetFilter {
        match self {
            Observer::FirebenderAscension => {
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
            }
            Observer::AbolethSpawn => {
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent))
            }
            Observer::StrictProctor => TargetFilter::Typed(TypedFilter::permanent()),
        }
    }
}

/// Add the printed observer to `player`'s battlefield from its verbatim Oracle
/// text, then replace the trigger condition the parser cannot yet produce
/// (Ascension's `Unknown` line; Aboleth's and Proctor's misparsed `ChangesZone`
/// line) with its D1 shape, keeping the parsed effect half.
fn add_observer(scenario: &mut GameScenario, player: PlayerId, observer: Observer) -> ObjectId {
    let id = match observer {
        Observer::FirebenderAscension => scenario
            .add_enchantment_from_oracle(player, "Firebender Ascension", FIREBENDER_ASCENSION)
            .id(),
        Observer::AbolethSpawn => scenario
            .add_creature_from_oracle(player, "Aboleth Spawn", 2, 3, ABOLETH_SPAWN)
            .id(),
        Observer::StrictProctor => scenario
            .add_creature_from_oracle(player, "Strict Proctor", 1, 3, STRICT_PROCTOR)
            .id(),
    };
    id
}

/// Install the D1 shape on `observer` (see [`add_observer`]).
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

/// CR 603.2: every firing of `observer`: its triggered entries on the stack,
/// its contexts in `deferred_triggers`, in `pending_trigger_order` groups, and
/// a paused `pending_trigger`. Each item is the firing's trigger event.
fn observer_firings(state: &GameState, observer: ObjectId) -> Vec<GameEvent> {
    let ability_triggered = |event: &Option<GameEvent>| match event {
        Some(event @ GameEvent::AbilityTriggered { .. }) => Some(event.clone()),
        _ => None,
    };
    let mut out: Vec<GameEvent> = state
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event,
                ..
            } if *source_id == observer => ability_triggered(trigger_event),
            _ => None,
        })
        .collect();
    let contexts = state.deferred_triggers.iter().chain(
        state
            .pending_trigger_order
            .iter()
            .flat_map(|order| order.groups.iter())
            .flat_map(|group| group.triggers.iter()),
    );
    out.extend(
        contexts
            .filter(|ctx| ctx.pending.source_id == observer)
            .filter_map(|ctx| ability_triggered(&ctx.pending.trigger_event)),
    );
    if let Some(pending) = state.pending_trigger.as_ref() {
        if pending.source_id == observer {
            out.extend(ability_triggered(&pending.trigger_event));
        }
    }
    out
}

fn stack_entry_of(event: &GameEvent) -> Option<ObjectId> {
    match event {
        GameEvent::AbilityTriggered { stack_entry, .. } => *stack_entry,
        other => panic!("not an AbilityTriggered event: {other:?}"),
    }
}

fn source_of(event: &GameEvent) -> ObjectId {
    match event {
        GameEvent::AbilityTriggered { source_id, .. } => *source_id,
        other => panic!("not an AbilityTriggered event: {other:?}"),
    }
}

fn controller_of(event: &GameEvent) -> PlayerId {
    match event {
        GameEvent::AbilityTriggered { controller, .. } => *controller,
        other => panic!("not an AbilityTriggered event: {other:?}"),
    }
}

/// Triggered stack entries whose source is `source`.
fn triggered_entries_of(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == source)
        })
        .count()
}

/// Whether any carrier (stack, deferred queue, ordering groups, paused
/// construction) holds a non-observer trigger of `source`.
fn source_trigger_anywhere(state: &GameState, source: ObjectId) -> bool {
    triggered_entries_of(state, source) > 0
        || state
            .deferred_triggers
            .iter()
            .chain(
                state
                    .pending_trigger_order
                    .iter()
                    .flat_map(|order| order.groups.iter())
                    .flat_map(|group| group.triggers.iter()),
            )
            .any(|ctx| ctx.pending.source_id == source)
        || state
            .pending_trigger
            .as_ref()
            .is_some_and(|pending| pending.source_id == source)
}

/// Make `player` the active player with priority in their precombat main phase.
fn give_turn(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Cast `spell` from its owner's hand and pass priority until it has left the
/// stack (it resolved). Stops at the first non-priority prompt.
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

/// Declare `attackers` against P1 from P0's precombat main phase.
fn attack(runner: &mut GameRunner, attackers: &[ObjectId]) {
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
        .expect("attack declaration is accepted");
}

/// Answer every `OrderTriggers` prompt with the identity order.
fn order_identity(runner: &mut GameRunner) {
    for _ in 0..8 {
        let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
            return;
        };
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering is accepted");
    }
}

fn quest_counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Generic("quest".to_string()))
        .copied()
        .unwrap_or(0)
}

fn assert_no_optional_choice(runner: &GameRunner) {
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the copy clause (four or more quest counters) must not prompt below four counters"
    );
}

// ---------------------------------------------------------------------------
// Attack causes (Firebender Ascension)
// ---------------------------------------------------------------------------

/// V1 + N2: firebending is pushed and observed once (its hand-built cause
/// record names Zuko); the quest counter resolves; Zuko's combat-damage
/// trigger is not attack-caused and is never observed.
#[test]
fn v1_firebending_push_is_observed_once_and_combat_damage_is_not() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[zuko]);
    order_identity(&mut runner);

    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(
        firings.len(),
        1,
        "exactly one Ascension firing: {firings:?}"
    );
    assert_eq!(source_of(&firings[0]), zuko);
    assert!(matches!(
        firings[0],
        GameEvent::AbilityTriggered {
            origin: TriggeredAbilityOrigin::SourceAbility,
            ..
        }
    ));
    let entry = stack_entry_of(&firings[0]).expect("firebending was pushed");
    assert!(
        runner.state().stack.iter().any(|e| e.id == entry),
        "the observed firebending entry is on the stack"
    );

    // Resolve: one quest counter, and the copy clause never prompts.
    for _ in 0..40 {
        assert_no_optional_choice(&runner);
        if runner.state().stack.is_empty() {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(quest_counters(runner.state(), ascension), 1);

    // N2: Zuko's combat-damage trigger (P1 has no creature, so no block). Advance past
    // combat damage so the trigger resolves (`advance_to_phase` does not move combat
    // forward from the declare-attackers priority in this scenario).
    for _ in 0..12 {
        let counters = runner.state().objects[&zuko]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        if counters > 0 {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    // Reach guard: Zuko dealt combat damage and its trigger resolved (its +1/+1 counter),
    // so the negative below cannot pass merely because the trigger never fired.
    assert_eq!(
        runner.state().objects[&zuko]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "Zuko's combat-damage trigger must have fired and resolved"
    );
    // An observed firing would resolve into a second quest counter; the stack is empty
    // by now, so the counter total (not the live firing list) is the discriminating check.
    assert_eq!(
        quest_counters(runner.state(), ascension),
        1,
        "a combat-damage trigger is not caused by attacking: still only the firebending firing"
    );
}

/// V2: two triggers of one source (firebending and Azula's attack trigger)
/// are two occurrences, observed twice.
#[test]
fn v2_two_triggers_of_one_attacker_are_observed_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let azula = scenario
        .add_creature(P0, "Azula, On the Hunt", 4, 3)
        .from_oracle_text_with_keywords(&["firebending"], AZULA)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[azula]);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "two non-identical Azula triggers need ordering, got {:?}",
        runner.state().waiting_for
    );
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), azula), 2);
    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 2, "{firings:?}");
    let entries: Vec<_> = firings.iter().map(stack_entry_of).collect();
    assert!(entries.iter().all(Option::is_some));
    assert_ne!(entries[0], entries[1], "each firing names its own entry");
}

/// V3: the decayed attack trigger's hand-built record names the attacker.
#[test]
fn v3_decayed_attack_trigger_is_observed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let rakshasa = scenario
        .add_creature_from_oracle(P0, "Rot-Curse Rakshasa", 5, 5, ROT_CURSE_RAKSHASA)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[rakshasa]);
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), rakshasa), 1);
    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), rakshasa);
}

/// V4: exalted is caused by the lone attacker. Squire attacking alone: its
/// exalted is "of that creature"; another creature attacking alone: it is not.
#[test]
fn v4_exalted_counts_only_when_its_source_attacks_alone() {
    for squire_attacks in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
        let squire = scenario
            .add_creature(P0, "Akrasan Squire", 1, 1)
            .from_oracle_text_with_keywords(&["exalted"], AKRASAN_SQUIRE)
            .id();
        let goblin = scenario
            .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
            .id();
        let mut runner = scenario.build();
        install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

        attack(&mut runner, &[if squire_attacks { squire } else { goblin }]);
        order_identity(&mut runner);

        assert_eq!(
            triggered_entries_of(runner.state(), squire),
            1,
            "exalted triggered (squire attacks: {squire_attacks})"
        );
        let expected = usize::from(squire_attacks);
        assert_eq!(
            observer_firings(runner.state(), ascension).len(),
            expected,
            "squire attacks: {squire_attacks}"
        );
    }
}

/// V5: Iroh's mentor with no legal target is removed during placement
/// (CR 603.3d, D-15) and observed there with `stack_entry: None`.
#[test]
fn v5_mentor_removed_for_no_legal_target_is_observed_with_none() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let iroh = scenario
        .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[iroh]);
    order_identity(&mut runner);

    assert!(
        !source_trigger_anywhere(runner.state(), iroh),
        "the mentor trigger was removed (no attacking creature with lesser power)"
    );
    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), iroh);
    assert_eq!(stack_entry_of(&firings[0]), None);
}

/// V8: the collection refusal keeps a drained sibling's occurrence to one
/// collection. Scaleguard's tap trigger (subject Rakshasa) pauses for its
/// target; Rakshasa's decayed waits behind it and is drained by `SelectTargets`,
/// whose buffer the post-action pipeline rescans from 0.
#[test]
fn v8_drained_sibling_is_collected_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let _scaleguard = scenario
        .add_creature_from_oracle(P0, "Elite Scaleguard", 2, 3, ELITE_SCALEGUARD)
        .id();
    let rakshasa = scenario
        .add_creature_from_oracle(P0, "Rot-Curse Rakshasa", 5, 5, ROT_CURSE_RAKSHASA)
        .id();
    scenario.with_counter(rakshasa, CounterType::Plus1Plus1, 1);
    let crab_a = scenario.add_vanilla(P1, 1, 6);
    let _crab_b = scenario.add_vanilla(P1, 1, 6);
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[rakshasa]);
    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Scaleguard's and Rakshasa's triggers need ordering, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(triggers.len(), 2);
    // Dispatch Scaleguard's trigger first so it pauses for its target.
    let scaleguard_index = triggers
        .iter()
        .position(|t| t.source_id != rakshasa)
        .expect("Scaleguard's trigger is listed");
    let mut order = vec![scaleguard_index];
    order.extend((0..triggers.len()).filter(|i| *i != scaleguard_index));
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("ordering is accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Scaleguard's trigger pauses for its target, got {:?}",
        runner.state().waiting_for
    );

    let result = runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Object(crab_a)],
        })
        .expect("target is accepted");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        result.waiting_for
    );
    let drained = result
        .events
        .iter()
        .filter(|e| {
            matches!(e, GameEvent::AbilityTriggered { stack_entry: Some(_), source_id, .. } if *source_id == rakshasa)
        })
        .count();
    assert_eq!(drained, 1, "the drain ran into the action's buffer");

    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), rakshasa);
}

/// V8b: a target prompt's push is observed at the push (P-f).
#[test]
fn v8b_paused_push_is_observed_at_push() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let iroh = scenario
        .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
        .id();
    let goblin_a = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
        .id();
    let goblin_b = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[iroh, goblin_a, goblin_b]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "mentor has two legal targets, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Object(goblin_a)],
        })
        .expect("target is accepted");

    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), iroh);
    let entry = stack_entry_of(&firings[0]).expect("the mentor entry was pushed");
    assert!(runner.state().stack.iter().any(|e| e.id == entry));
}

/// V14 + D3 (Isshin): Isshin doubles Zuko's firebending (caused by attacking),
/// so two occurrences are observed; Ascension's own trigger is not doubled.
#[test]
fn v14_isshin_doubles_the_observed_trigger_not_the_observer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    scenario.add_creature_from_oracle(P0, "Isshin, Two Heavens as One", 3, 4, ISSHIN);
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[zuko]);
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), zuko), 2);
    assert_eq!(observer_firings(runner.state(), ascension).len(), 2);
    for _ in 0..40 {
        assert_no_optional_choice(&runner);
        if runner.state().stack.is_empty() {
            break;
        }
        runner.pass_both_players();
        order_identity(&mut runner);
    }
    assert_eq!(quest_counters(runner.state(), ascension), 2);
}

/// V15: per-attacker family. Sanctum Seeker's trigger for itself counts; the
/// one for the other Vampire is not "of that creature".
#[test]
fn v15_per_attacker_trigger_counts_only_for_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let seeker = scenario
        .add_creature_from_oracle(P0, "Sanctum Seeker", 3, 4, SANCTUM_SEEKER)
        .with_subtypes(vec!["Vampire", "Knight"])
        .id();
    let interloper = scenario
        .add_creature_from_oracle(P0, "Vampire Interloper", 2, 1, VAMPIRE_INTERLOPER)
        .with_subtypes(vec!["Vampire", "Scout"])
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[seeker, interloper]);
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), seeker), 2);
    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), seeker);
}

/// V16: an attack-caused trigger of a non-attacker is not "of that creature".
#[test]
fn v16_attack_caused_trigger_of_a_non_attacker_is_not_observed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let bombardment = scenario
        .add_enchantment_from_oracle(P0, "Raid Bombardment", RAID_BOMBARDMENT)
        .id();
    let goblin = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[goblin]);
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), bombardment), 1);
    assert!(observer_firings(runner.state(), ascension).is_empty());
}

// ---------------------------------------------------------------------------
// Enter causes (Aboleth Spawn, Strict Proctor)
// ---------------------------------------------------------------------------

/// V9: Aboleth observes the self ETB of an opponent's creature; not its
/// controller's own.
#[test]
fn v9_aboleth_observes_an_opponents_self_etb_only() {
    for opponent_casts in [true, false] {
        let caster = if opponent_casts { P1 } else { P0 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
        let visionary = scenario
            .add_creature_to_hand_from_oracle(caster, "Elvish Visionary", 1, 1, ELVISH_VISIONARY)
            .id();
        let mut runner = scenario.build();
        install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
        give_turn(&mut runner, caster);

        cast_and_resolve(&mut runner, visionary);
        order_identity(&mut runner);

        assert_eq!(triggered_entries_of(runner.state(), visionary), 1);
        assert_eq!(
            observer_firings(runner.state(), aboleth).len(),
            usize::from(opponent_casts),
            "opponent casts: {opponent_casts}"
        );
    }
}

/// V10: another object's ETB trigger is not "of that creature" (Aboleth,
/// ruling R4); Proctor's `Any` admits it.
#[test]
fn v10_other_object_etb_is_rejected_by_aboleth_and_admitted_by_proctor() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
    let proctor = add_observer(&mut scenario, P0, Observer::StrictProctor);
    let warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let elves = scenario
        .add_creature_to_hand_from_oracle(P1, "Llanowar Elves", 1, 1, LLANOWAR_ELVES)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
    install_observer(runner.state_mut(), proctor, Observer::StrictProctor);
    give_turn(&mut runner, P1);

    cast_and_resolve(&mut runner, elves);

    assert_eq!(triggered_entries_of(runner.state(), warden), 1);
    assert!(observer_firings(runner.state(), aboleth).is_empty());
    let firings = observer_firings(runner.state(), proctor);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), warden);
}

/// V12: exploit's hand-built record names the entering creature.
#[test]
fn v12_exploit_is_observed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
    let faithful = scenario
        .add_creature_to_hand_from_oracle(P1, "Sidisi's Faithful", 0, 4, SIDISIS_FAITHFUL)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
    give_turn(&mut runner, P1);

    cast_and_resolve(&mut runner, faithful);
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), faithful), 1);
    let firings = observer_firings(runner.state(), aboleth);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), faithful);
}

// ---------------------------------------------------------------------------
// Hidden information (CR 400.2, CR 708.5)
// ---------------------------------------------------------------------------

/// Soul Summons manifests a card under P1; P0's Soul Warden triggers on it and
/// P0's Proctor observes Soul Warden's trigger.
fn manifest_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proctor = add_observer(&mut scenario, P0, Observer::StrictProctor);
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    scenario.add_card_to_library_top(P1, "Grizzly Bears");
    let summons = scenario
        .add_spell_to_hand_from_oracle(P1, "Soul Summons", false, SOUL_SUMMONS)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), proctor, Observer::StrictProctor);
    give_turn(&mut runner, P1);
    cast_and_resolve(&mut runner, summons);
    order_identity(&mut runner);
    (runner, proctor, warden)
}

/// V21 (CR 400.2 + CR 708.5 + CR 701.40a): the manifested card's identity, nested
/// as a cause in Proctor's firing, is hidden from the viewer who may not look at
/// it (P0) and kept for its controller (P1).
#[test]
fn v21_nested_face_down_cause_is_hidden_from_the_opponent() {
    let (runner, proctor, _warden) = manifest_board();
    let cause_names = |state: &GameState| -> Vec<String> {
        observer_firings(state, proctor)
            .iter()
            .flat_map(|firing| match firing {
                GameEvent::AbilityTriggered { causes, .. } => causes.clone(),
                _ => Vec::new(),
            })
            .map(|cause| match cause {
                GameEvent::ZoneChanged {
                    from: Some(Zone::Library),
                    to: Zone::Battlefield,
                    record,
                    ..
                } => record.name.clone(),
                other => panic!("unexpected cause {other:?}"),
            })
            .collect()
    };
    // Reach guard: the authoritative firing records the manifested card itself.
    assert_eq!(
        cause_names(runner.state()),
        vec!["Grizzly Bears".to_string()]
    );
    assert_eq!(
        cause_names(&filter_state_for_viewer(runner.state(), P1)),
        vec!["Grizzly Bears".to_string()],
        "the manifest's controller may look at it (CR 708.5)"
    );
    assert_eq!(
        cause_names(&filter_state_for_viewer(runner.state(), P0)),
        vec![HIDDEN_CARD_NAME.to_string()],
        "an opponent may not learn the face-down card through the cause record"
    );
}

/// V21d (CR 400.2 + CR 708.5): a prompt that copies the resolving trigger's event
/// (here the unless-payment prompt of Proctor's "counter unless its controller pays")
/// carries the observer's `AbilityTriggered` event, so its nested face-down cause is
/// hidden from P0 like every other carrier.
#[test]
fn v21d_unless_payment_prompt_hides_the_face_down_cause() {
    let (runner, proctor, _warden) = manifest_board();
    let firing = observer_firings(runner.state(), proctor)
        .first()
        .cloned()
        .expect("reach guard: Proctor observed Soul Warden's trigger");
    let prompt_cause_names = |state: &GameState| -> Vec<String> {
        match &state.waiting_for {
            WaitingFor::UnlessPayment {
                trigger_event: Some(GameEvent::AbilityTriggered { causes, .. }),
                ..
            } => causes
                .iter()
                .map(|cause| match cause {
                    GameEvent::ZoneChanged { record, .. } => record.name.clone(),
                    other => panic!("unexpected cause {other:?}"),
                })
                .collect(),
            other => panic!("expected the unless-payment prompt, got {other:?}"),
        }
    };
    let mut state = runner.state().clone();
    state.waiting_for = WaitingFor::UnlessPayment {
        player: P0,
        repeat: None,
        cost: engine::types::ability::AbilityCost::Mana {
            cost: engine::types::mana::ManaCost::generic(2),
        },
        pending_effect: Box::new(engine::types::ability::ResolvedAbility::new(
            engine::types::ability::Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            proctor,
            P0,
        )),
        trigger_event: Some(firing),
        effect_description: None,
        remaining: Vec::new(),
        affected_object: None,
    };
    // Reach guard: the authoritative prompt names the manifested card.
    assert_eq!(
        prompt_cause_names(&state),
        vec!["Grizzly Bears".to_string()]
    );
    assert_eq!(
        prompt_cause_names(&filter_state_for_viewer(&state, P1)),
        vec!["Grizzly Bears".to_string()],
        "the manifest's controller may look at it (CR 708.5)"
    );
    assert_eq!(
        prompt_cause_names(&filter_state_for_viewer(&state, P0)),
        vec![HIDDEN_CARD_NAME.to_string()],
        "an opponent may not learn the face-down card through a prompt's trigger event"
    );
}

/// V21e (CR 400.2 + CR 708.5): P0 pays Proctor's unless-payment by activating a mana
/// ability that pauses at a color choice. The paused activation keeps the unless-payment
/// resume, and that resume keeps the observer's event, so the `ChooseManaColor` prompt
/// carries the nested face-down cause too. It is hidden from P0 like every other carrier.
#[test]
fn v21e_paused_mana_ability_resume_hides_the_face_down_cause() {
    use engine::types::game_state::{
        CastPaymentMode, ManaAbilityResume, ManaChoiceContext, ManaChoicePrompt, PendingManaAbility,
    };
    let (runner, proctor, _warden) = manifest_board();
    let firing = observer_firings(runner.state(), proctor)
        .first()
        .cloned()
        .expect("reach guard: Proctor observed Soul Warden's trigger");
    let prompt_cause_names = |state: &GameState| -> Vec<String> {
        match &state.waiting_for {
            WaitingFor::ChooseManaColor {
                context: ManaChoiceContext::ManaAbility(pending),
                ..
            } => match &pending.resume {
                ManaAbilityResume::UnlessPayment {
                    trigger_event: Some(GameEvent::AbilityTriggered { causes, .. }),
                    ..
                } => causes
                    .iter()
                    .map(|cause| match cause {
                        GameEvent::ZoneChanged { record, .. } => record.name.clone(),
                        other => panic!("unexpected cause {other:?}"),
                    })
                    .collect(),
                other => panic!("expected the unless-payment resume, got {other:?}"),
            },
            other => panic!("expected the paused mana-ability prompt, got {other:?}"),
        }
    };
    let mut state = runner.state().clone();
    state.waiting_for = WaitingFor::ChooseManaColor {
        player: P0,
        choice: ManaChoicePrompt::SingleColor {
            options: vec![engine::types::mana::ManaType::White],
        },
        context: ManaChoiceContext::ManaAbility(Box::new(PendingManaAbility {
            player: P0,
            source_id: proctor,
            ability_snapshot: None,
            announced_source_incarnation: None,
            announced_usage_key: None,
            ability_index: Some(0),
            rules_execution_node: None,
            color_override: None,
            resume: ManaAbilityResume::UnlessPayment {
                outer_player: None,
                cost: Box::new(engine::types::ability::AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::generic(2),
                }),
                pending_effect: Box::new(engine::types::ability::ResolvedAbility::new(
                    engine::types::ability::Effect::Draw {
                        count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                    Vec::new(),
                    proctor,
                    P0,
                )),
                trigger_event: Some(firing),
                effect_description: None,
                remaining: Vec::new(),
                affected_object: None,
                repeat: None,
            },
            cost_move_resume: None,
            selected_taps_for_mana: Vec::new(),
            chosen_tappers: None,
            chosen_discards: Vec::new(),
            chosen_mana_payment: None,
            payment_mode: CastPaymentMode::Auto,
            pinned_pool_units: Vec::new(),
            chosen_counter_counts: Vec::new(),
            chosen_x: None,
            collected_evidence: Vec::new(),
            chosen_exiled: Vec::new(),
            chosen_sacrificed_battlefield: Vec::new(),
            cost_paid_object: None,
            cost_removed_counters: None,
            batch_siblings: Vec::new(),
        })),
    };
    // Reach guard: the authoritative prompt names the manifested card.
    assert_eq!(
        prompt_cause_names(&state),
        vec!["Grizzly Bears".to_string()]
    );
    assert_eq!(
        prompt_cause_names(&filter_state_for_viewer(&state, P1)),
        vec!["Grizzly Bears".to_string()],
        "the manifest's controller may look at it (CR 708.5)"
    );
    assert_eq!(
        prompt_cause_names(&filter_state_for_viewer(&state, P0)),
        vec![HIDDEN_CARD_NAME.to_string()],
        "an opponent may not learn the face-down card through a paused mana ability's resume"
    );
}

/// V21f (CR 400.2 + CR 708.5): a discard-cost mana ability (Skirge Familiar) activated while
/// paying Proctor's unless-payment pauses at a `PayCost` discard prompt whose `resume` holds
/// the observer's event. The `PayCost` rebuild re-clones that authoritative resume for a
/// viewer without private access to the payer, so the nested face-down cause must be
/// redacted after the rebuild, not before it.
#[test]
fn v21f_pay_cost_discard_resume_hides_the_face_down_cause() {
    use engine::types::game_state::{
        CastPaymentMode, CostResume, ManaAbilityResume, PayCostKind, PendingManaAbility,
    };
    let (runner, proctor, _warden) = manifest_board();
    let firing = observer_firings(runner.state(), proctor)
        .first()
        .cloned()
        .expect("reach guard: Proctor observed Soul Warden's trigger");
    let prompt_cause_names = |state: &GameState| -> Vec<String> {
        match &state.waiting_for {
            WaitingFor::PayCost {
                resume: CostResume::ManaAbility { mana_ability },
                ..
            } => match &mana_ability.resume {
                ManaAbilityResume::UnlessPayment {
                    trigger_event: Some(GameEvent::AbilityTriggered { causes, .. }),
                    ..
                } => causes
                    .iter()
                    .map(|cause| match cause {
                        GameEvent::ZoneChanged { record, .. } => record.name.clone(),
                        other => panic!("unexpected cause {other:?}"),
                    })
                    .collect(),
                other => panic!("expected the unless-payment resume, got {other:?}"),
            },
            other => panic!("expected the discard-cost prompt, got {other:?}"),
        }
    };
    let mut state = runner.state().clone();
    state.waiting_for = WaitingFor::PayCost {
        player: P1,
        kind: PayCostKind::Discard,
        choices: Vec::new(),
        count: 1,
        min_count: 1,
        resume: CostResume::ManaAbility {
            mana_ability: Box::new(PendingManaAbility {
                player: P1,
                source_id: proctor,
                ability_snapshot: None,
                announced_source_incarnation: None,
                announced_usage_key: None,
                ability_index: Some(0),
                rules_execution_node: None,
                color_override: None,
                resume: ManaAbilityResume::UnlessPayment {
                    outer_player: None,
                    cost: Box::new(engine::types::ability::AbilityCost::Mana {
                        cost: engine::types::mana::ManaCost::generic(2),
                    }),
                    pending_effect: Box::new(engine::types::ability::ResolvedAbility::new(
                        engine::types::ability::Effect::Draw {
                            count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                        Vec::new(),
                        proctor,
                        P1,
                    )),
                    trigger_event: Some(firing),
                    effect_description: None,
                    remaining: Vec::new(),
                    affected_object: None,
                    repeat: None,
                },
                cost_move_resume: None,
                selected_taps_for_mana: Vec::new(),
                chosen_tappers: None,
                chosen_discards: Vec::new(),
                chosen_mana_payment: None,
                payment_mode: CastPaymentMode::Auto,
                pinned_pool_units: Vec::new(),
                chosen_counter_counts: Vec::new(),
                chosen_x: None,
                collected_evidence: Vec::new(),
                chosen_exiled: Vec::new(),
                chosen_sacrificed_battlefield: Vec::new(),
                cost_paid_object: None,
                cost_removed_counters: None,
                batch_siblings: Vec::new(),
            }),
        },
    };
    // Reach guard: the authoritative prompt names the manifested card.
    assert_eq!(
        prompt_cause_names(&state),
        vec!["Grizzly Bears".to_string()]
    );
    assert_eq!(
        prompt_cause_names(&filter_state_for_viewer(&state, P1)),
        vec!["Grizzly Bears".to_string()],
        "the manifest's controller may look at it (CR 708.5)"
    );
    assert_eq!(
        prompt_cause_names(&filter_state_for_viewer(&state, P0)),
        vec![HIDDEN_CARD_NAME.to_string()],
        "an opponent may not learn the face-down card through a PayCost resume"
    );
}

/// V21a: the client event stream never carries `causes`, for any viewer.
#[test]
fn v21a_event_stream_carries_no_causes() {
    let (runner, proctor, _) = manifest_board();
    let event = observer_firings(runner.state(), proctor)
        .pop()
        .expect("one Proctor firing");
    let GameEvent::AbilityTriggered { causes, .. } = &event else {
        unreachable!()
    };
    assert!(!causes.is_empty(), "the input event has a cause record");
    for viewer in [P0, P1] {
        let filtered =
            filter_events_for_viewer(std::slice::from_ref(&event), runner.state(), viewer);
        assert!(
            matches!(filtered.as_slice(), [GameEvent::AbilityTriggered { causes, .. }] if causes.is_empty()),
            "viewer {viewer:?}: {filtered:?}"
        );
    }
}

/// V21b (a): an ordering group's `causes` are private scheduling payload
/// (CR 603.3b + CR 400.2). P1 orders Azula's two triggers.
#[test]
fn v21b_ordering_group_causes_are_private() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let azula = scenario
        .add_creature(P1, "Azula, On the Hunt", 4, 3)
        .from_oracle_text_with_keywords(&["firebending"], AZULA)
        .id();
    let mut runner = scenario.build();
    give_turn(&mut runner, P1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(azula, AttackTarget::Player(P0))])
        .expect("attack is accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OrderTriggers { player, .. } if player == P1
    ));

    let causes_per_context = |state: &GameState| -> Vec<bool> {
        let value = serde_json::to_value(&state.pending_trigger_order).expect("serializes");
        value["groups"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| group["triggers"].as_array().cloned().unwrap_or_default())
            .map(|ctx| {
                ctx.get("causes")
                    .is_some_and(|c| !c.as_array().unwrap().is_empty())
            })
            .collect()
    };
    let own = causes_per_context(&filter_state_for_viewer(runner.state(), P1));
    assert_eq!(own, vec![true, true], "P1's own copy keeps both records");
    let other = causes_per_context(&filter_state_for_viewer(runner.state(), P0));
    assert!(
        !other.is_empty() && other.iter().all(|kept| !kept),
        "{other:?}"
    );
}

// ---------------------------------------------------------------------------
// Removal during placement with a remembered accept (Option A) and the pin
// ---------------------------------------------------------------------------

fn gravedigger_printed_ref() -> PrintedCardRef {
    PrintedCardRef {
        oracle_id: "gravedigger-test-oracle".to_string(),
        face_name: "Gravedigger".to_string(),
    }
}

/// Put Gravedigger in `caster`'s hand.
fn gravedigger_in_hand(scenario: &mut GameScenario, caster: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(caster, "Gravedigger", 2, 2, GRAVEDIGGER)
        .id()
}

fn remember_gravedigger_accept(runner: &mut GameRunner, gravedigger: ObjectId, caster: PlayerId) {
    runner
        .state_mut()
        .objects
        .get_mut(&gravedigger)
        .expect("Gravedigger exists")
        .base_printed_ref = Some(gravedigger_printed_ref());
    runner.state_mut().set_may_trigger_auto_choice_selector(
        MayTriggerAutoChoiceSelector::SameCard {
            player: caster,
            printed_ref: gravedigger_printed_ref(),
            printed_occurrence: 0,
        },
        AutoMayChoice::Accept,
    );
}

/// The live may-trigger key of `source`'s first trigger definition.
fn live_definition_key(
    state: &GameState,
    source: ObjectId,
    player: PlayerId,
) -> MayTriggerAutoChoiceKey {
    let obj = &state.objects[&source];
    let entry = &obj.trigger_definitions.as_slice()[0];
    MayTriggerAutoChoiceKey {
        player,
        source_id: source,
        origin: MayTriggerOrigin::Definition {
            definition_ref: obj.trigger_definition_ref(entry),
        },
    }
}

/// V24 (a) remembered accept and (b) none: Gravedigger's ETB with no legal
/// target is removed during placement (CR 603.3d) and observed once by each
/// observer, whatever the UI preference.
#[test]
fn v24_enter_removal_is_observed_whatever_the_preference() {
    for remembered_accept in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let proctor = add_observer(&mut scenario, P0, Observer::StrictProctor);
        let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
        let gravedigger = gravedigger_in_hand(&mut scenario, P1);
        let mut runner = scenario.build();
        install_observer(runner.state_mut(), proctor, Observer::StrictProctor);
        install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
        if remembered_accept {
            remember_gravedigger_accept(&mut runner, gravedigger, P1);
        }
        give_turn(&mut runner, P1);

        cast_and_resolve(&mut runner, gravedigger);

        assert_eq!(runner.state().objects[&gravedigger].zone, Zone::Battlefield);
        if remembered_accept {
            let key = live_definition_key(runner.state(), gravedigger, P1);
            assert_eq!(
                runner.state().may_trigger_auto_choice_for_live_prompt(&key),
                Some(AutoMayChoice::Accept),
                "the early drop's precondition held"
            );
        }
        assert!(
            !source_trigger_anywhere(runner.state(), gravedigger),
            "Gravedigger's trigger was removed (accept: {remembered_accept})"
        );
        assert!(!matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::OptionalEffectChoice { .. }
        ));
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
            "P0 orders its two observer firings, got {:?}",
            runner.state().waiting_for
        );
        for observer in [proctor, aboleth] {
            let firings = observer_firings(runner.state(), observer);
            assert_eq!(firings.len(), 1, "accept {remembered_accept}: {firings:?}");
            assert_eq!(stack_entry_of(&firings[0]), None);
            assert_eq!(source_of(&firings[0]), gravedigger);
            assert_eq!(controller_of(&firings[0]), P1);
        }
    }
}

/// V25 (a)/(b) + Isshin siblings: Goblin Racketeer attacks a player with no
/// creature; its goad trigger is removed during placement and observed, once,
/// or twice with Isshin (CR 603.2d doubling happens before the removal).
#[test]
fn v25_attack_removal_is_observed_and_doubled_by_isshin() {
    for (remembered_accept, isshin) in [(true, false), (false, false), (true, true), (false, true)]
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
        if isshin {
            scenario.add_creature_from_oracle(P0, "Isshin, Two Heavens as One", 3, 4, ISSHIN);
        }
        let racketeer = scenario
            .add_creature_from_oracle(P0, "Goblin Racketeer", 4, 2, GOBLIN_RACKETEER)
            .id();
        let mut runner = scenario.build();
        install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);
        let key = live_definition_key(runner.state(), racketeer, P0);
        if remembered_accept {
            runner
                .state_mut()
                .set_may_trigger_auto_choice(key.clone(), AutoMayChoice::Accept);
        }

        attack(&mut runner, &[racketeer]);
        order_identity(&mut runner);

        let label = format!("accept {remembered_accept}, isshin {isshin}");
        if remembered_accept {
            assert_eq!(
                runner.state().may_trigger_auto_choice_for_live_prompt(&key),
                Some(AutoMayChoice::Accept),
                "{label}"
            );
        }
        assert!(
            !source_trigger_anywhere(runner.state(), racketeer),
            "{label}"
        );
        let expected = if isshin { 2 } else { 1 };
        let firings = observer_firings(runner.state(), ascension);
        assert_eq!(firings.len(), expected, "{label}: {firings:?}");
        for firing in &firings {
            assert_eq!(stack_entry_of(firing), None, "{label}");
            assert_eq!(source_of(firing), racketeer, "{label}");
        }
        for _ in 0..40 {
            assert_no_optional_choice(&runner);
            if runner.state().stack.is_empty() {
                break;
            }
            runner.pass_both_players();
            order_identity(&mut runner);
        }
        assert_eq!(
            quest_counters(runner.state(), ascension),
            expected as u32,
            "{label}"
        );
    }
}

/// N1 (pin): with no observer anywhere, today's auto-inert drop is unchanged,
/// including on a board whose `unclassified` bucket is non-empty (Prowess).
#[test]
fn n1_no_observer_keeps_todays_drop_and_prompts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let swiftspear = scenario
        .add_creature_from_oracle(P1, "Monastery Swiftspear", 1, 2, MONASTERY_SWIFTSPEAR)
        .id();
    let gravedigger = gravedigger_in_hand(&mut scenario, P1);
    let mut runner = scenario.build();
    remember_gravedigger_accept(&mut runner, gravedigger, P1);
    give_turn(&mut runner, P1);
    // Reach guards on the board's index.
    engine::game::trigger_index::ensure_ready(runner.state_mut());
    assert!(runner
        .state()
        .trigger_index
        .unclassified
        .contains(&swiftspear));
    assert!(runner
        .state()
        .trigger_index
        .by_key
        .get(&TriggerEventKey::AbilityTriggered)
        .is_none_or(|bucket| bucket.is_empty()));

    cast_and_resolve(&mut runner, gravedigger);

    let key = live_definition_key(runner.state(), gravedigger, P1);
    assert_eq!(
        runner.state().may_trigger_auto_choice_for_live_prompt(&key),
        Some(AutoMayChoice::Accept)
    );
    assert_eq!(runner.state().objects[&gravedigger].zone, Zone::Battlefield);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "the dropped trigger must not join an ordering prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(triggered_entries_of(runner.state(), warden), 1);
    assert!(!source_trigger_anywhere(runner.state(), gravedigger));
}

// ---------------------------------------------------------------------------
// Fenced: CR 800.4d (a controller left the game before placement)
// ---------------------------------------------------------------------------

/// F3: three players; P1 concedes while ordering Soul Warden's and Suture
/// Priest's triggers for P2's Llanowar Elves.
#[test]
#[ignore = "known limitation (tracked in the PR description): a trigger whose controller left the game before placement is not observed (CR 800.4d + CR 603.2)"]
fn f3_conceded_ordering_group_is_observed() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let proctor = add_observer(&mut scenario, P0, Observer::StrictProctor);
    let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
    let warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let priest = scenario
        .add_creature_from_oracle(P1, "Suture Priest", 1, 1, SUTURE_PRIEST)
        .id();
    let elves = scenario
        .add_creature_to_hand_from_oracle(P2, "Llanowar Elves", 1, 1, LLANOWAR_ELVES)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), proctor, Observer::StrictProctor);
    install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
    give_turn(&mut runner, P2);

    cast_and_resolve(&mut runner, elves);
    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "P1 orders two triggers, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((player, triggers.len()), (P1, 2));
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("concession is accepted");
    assert!(runner
        .state()
        .players
        .iter()
        .any(|p| p.id == P1 && p.is_eliminated));
    assert!(!source_trigger_anywhere(runner.state(), warden));
    assert!(!source_trigger_anywhere(runner.state(), priest));

    let firings = observer_firings(runner.state(), proctor);
    assert_eq!(firings.len(), 2, "{firings:?}");
    assert!(firings
        .iter()
        .all(|f| stack_entry_of(f).is_none() && controller_of(f) == P1));
    assert!(observer_firings(runner.state(), aboleth).is_empty());
}

/// F4: three players; P2's Fiend Hunter pauses for its target while P1's Soul
/// Warden context waits in `deferred_triggers`; P1 concedes.
#[test]
#[ignore = "known limitation (tracked in the PR description): a trigger whose controller left the game before placement is not observed (CR 800.4d + CR 603.2)"]
fn f4_conceded_deferred_context_is_observed() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let proctor = add_observer(&mut scenario, P0, Observer::StrictProctor);
    let warden = scenario
        .add_creature_from_oracle(P1, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let hunter = scenario
        .add_creature_to_hand_from_oracle(P2, "Fiend Hunter", 1, 3, FIEND_HUNTER)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), proctor, Observer::StrictProctor);
    give_turn(&mut runner, P2);

    cast_and_resolve(&mut runner, hunter);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Fiend Hunter's trigger pauses, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner
        .state()
        .deferred_triggers
        .iter()
        .any(|ctx| ctx.pending.source_id == warden && ctx.pending.controller == P1));
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("concession is accepted");
    assert!(runner
        .state()
        .players
        .iter()
        .any(|p| p.id == P1 && p.is_eliminated));
    assert!(!runner
        .state()
        .deferred_triggers
        .iter()
        .any(|ctx| ctx.pending.source_id == warden));

    let firings = observer_firings(runner.state(), proctor);
    assert_eq!(firings.len(), 2, "{firings:?}");
    assert!(firings
        .iter()
        .any(|f| stack_entry_of(f).is_some() && source_of(f) == hunter && controller_of(f) == P2));
    assert!(firings
        .iter()
        .any(|f| stack_entry_of(f).is_none() && source_of(f) == warden && controller_of(f) == P1));
}

// ---------------------------------------------------------------------------
// Rows V6, V7, V11, V13, V17–V20, V21b(b)
// ---------------------------------------------------------------------------

const BREECHES: &str = "First strike\nWhenever a Pirate you control attacks, choose one that hasn't been chosen this turn —\n• Create a Treasure token.\n• Target creature can't block this turn.\n• Exile the top card of your library. You may play it this turn.";
const KITESAIL_CORSAIR: &str = "This creature has flying as long as it's attacking.";
const CULT_OF_SKARO: &str = "Whenever Cult of Skaro attacks, choose one at random —\n• Thay — Put a +1/+1 counter on each artifact creature you control.\n• Caan — Draw two cards.\n• Sec — Create a 3/3 black Dalek artifact creature token with menace.\n• Jast — Each opponent loses 4 life.";
const KINDRED_SUMMONS: &str = "Choose a creature type. Reveal cards from the top of your library until you reveal X creature cards of the chosen type, where X is the number of creatures you control of that type. Put those cards onto the battlefield, then shuffle the rest of the revealed cards into your library.";
const ELVISH_WARMASTER: &str = "Whenever one or more other Elves you control enter, create a 1/1 green Elf Warrior creature token. This ability triggers only once each turn.\n{5}{G}{G}: Elves you control get +2/+2 and gain deathtouch until end of turn.";
const ABERRANT: &str = "Ravenous (This creature enters with X +1/+1 counters on it. If X is 5 or more, draw a card when it enters.)\nTrample\nHeavy Power Hammer — Whenever this creature deals combat damage to a player, destroy target artifact or enchantment that player controls.";
const AVATAR_ROKU: &str = "Whenever a player attacks, add six {R}. Until end of combat, you don't lose this mana as steps end.\n{R}{R}{R}: Target creature gets +3/+0 until end of turn.";
const JACE_BELEREN: &str = "[+2]: Each player draws a card.\n[−1]: Target player draws a card.\n[−10]: Target player mills twenty cards.";
const FRONTIER_WARMONGER: &str = "Whenever one or more creatures attack one of your opponents or a planeswalker they control, those creatures gain menace until end of turn.";
const CAESAR: &str = "Whenever you attack, you may sacrifice another creature. When you do, choose two —\n• Create two 1/1 red and white Soldier creature tokens with haste that are tapped and attacking.\n• You draw a card and you lose 1 life.\n• Caesar deals damage equal to the number of creature tokens you control to target opponent.";
const NEYALI: &str = "Attacking tokens you control have double strike.\nWhenever one or more tokens you control attack a player, exile the top card of your library. During any turn you attacked with a token, you may play that card.";

/// The attacker set a pending context's `trigger_event` names.
fn attackers_of(event: Option<&GameEvent>) -> Vec<ObjectId> {
    match event {
        Some(GameEvent::AttackersDeclared { attacker_ids, .. }) => attacker_ids.clone(),
        _ => Vec::new(),
    }
}

/// V6 (D-3 and P-a): four Breeches triggers (one per attacking Pirate) choose
/// different modes; the fourth has no mode left and is removed before its push
/// (CR 603.3c + CR 700.2b). Only the Breeches-subject firing is "of that
/// creature". Dispatched last it is observed with `None`; dispatched first,
/// with its entry, and the dropped Corsair-subject firing yields nothing.
#[test]
fn v6_modal_push_and_no_legal_mode_removal_are_observed_once() {
    for breeches_last in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
        let breeches = scenario
            .add_creature_from_oracle(P0, "Breeches, Eager Pillager", 3, 3, BREECHES)
            .with_subtypes(vec!["Goblin", "Pirate"])
            .id();
        let corsairs: Vec<ObjectId> = (0..3)
            .map(|_| {
                scenario
                    .add_creature_from_oracle(P0, "Kitesail Corsair", 2, 1, KITESAIL_CORSAIR)
                    .with_subtypes(vec!["Human", "Pirate"])
                    .id()
            })
            .collect();
        let crab = scenario.add_vanilla(P1, 1, 6);
        scenario.add_card_to_library_top(P0, "Island");
        let mut runner = scenario.build();
        install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

        let mut attackers = vec![breeches];
        attackers.extend(&corsairs);
        attack(&mut runner, &attackers);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
            "got {:?}",
            runner.state().waiting_for
        );
        let group = runner
            .state()
            .pending_trigger_order
            .as_ref()
            .unwrap()
            .groups[0]
            .clone();
        assert_eq!(group.triggers.len(), 4);
        let own = group
            .triggers
            .iter()
            .position(|ctx| attackers_of(ctx.pending.trigger_event.as_ref()) == vec![breeches])
            .expect("the Breeches-subject firing");
        let others = (0..4).filter(|i| *i != own);
        let order: Vec<usize> = if breeches_last {
            others.chain(std::iter::once(own)).collect()
        } else {
            std::iter::once(own).chain(others).collect()
        };
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering is accepted");

        let mut modes = vec![0usize, 2, 1].into_iter();
        let mut mode_prompts = 0;
        for _ in 0..12 {
            match runner.state().waiting_for.clone() {
                WaitingFor::AbilityModeChoice { .. } => {
                    mode_prompts += 1;
                    let index = modes.next().expect("three modes");
                    runner
                        .act(GameAction::SelectModes {
                            indices: vec![index],
                        })
                        .expect("mode is accepted");
                }
                WaitingFor::TriggerTargetSelection { .. } => {
                    runner
                        .act(GameAction::SelectTargets {
                            targets: vec![engine::types::ability::TargetRef::Object(crab)],
                        })
                        .expect("target is accepted");
                }
                // The observer's firing joins the remaining batch (its placement
                // above the observed ability is phase 2); identity order.
                WaitingFor::OrderTriggers { .. } => order_identity(&mut runner),
                _ => break,
            }
        }
        assert_eq!(
            mode_prompts,
            3,
            "breeches last: {breeches_last}, waiting {:?}, stack {}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
        assert_eq!(
            triggered_entries_of(runner.state(), breeches),
            3,
            "the fourth firing was removed (breeches last: {breeches_last})"
        );
        let firings = observer_firings(runner.state(), ascension);
        assert_eq!(
            firings.len(),
            1,
            "breeches last: {breeches_last}: {firings:?}"
        );
        assert_eq!(source_of(&firings[0]), breeches);
        assert_eq!(
            stack_entry_of(&firings[0]).is_none(),
            breeches_last,
            "breeches last: {breeches_last}"
        );
    }
}

/// V7 (P-b, D-4): a random-modal attack trigger is pushed and observed once.
#[test]
fn v7_random_modal_push_is_observed_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let skaro = scenario
        .add_creature_from_oracle(P0, "Cult of Skaro", 4, 4, CULT_OF_SKARO)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[skaro]);
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), skaro), 1);
    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), skaro);
    assert!(stack_entry_of(&firings[0]).is_some());
}

/// V11: a batched ETB's record is its filter-matching members. P1's Kindred
/// Summons puts Elvish Warmaster and another Elf onto the battlefield together:
/// Warmaster's trigger is caused by the other Elf, so Aboleth does not copy it;
/// Proctor triggers once for it.
#[test]
fn v11_batched_etb_record_is_its_matching_members() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
    let proctor = add_observer(&mut scenario, P0, Observer::StrictProctor);
    // X = 2 Elves P1 controls.
    for name in ["Llanowar Elves", "Elvish Mystic"] {
        scenario
            .add_creature(P1, name, 1, 1)
            .with_subtypes(vec!["Elf"]);
    }
    let warmaster = scenario
        .add_creature_from_oracle(P1, "Elvish Warmaster", 2, 2, ELVISH_WARMASTER)
        .with_subtypes(vec!["Elf", "Warrior"])
        .id();
    let other_elf = scenario
        .add_creature(P1, "Fyndhorn Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let summons = scenario
        .add_spell_to_hand_from_oracle(P1, "Kindred Summons", false, KINDRED_SUMMONS)
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
    install_observer(runner.state_mut(), proctor, Observer::StrictProctor);
    // Stage the two Elves as P1's library top.
    for id in [other_elf, warmaster] {
        let state = runner.state_mut();
        let mut events = Vec::new();
        engine::game::zones::remove_from_zone(state, id, Zone::Battlefield, P1);
        state.objects.get_mut(&id).unwrap().zone = Zone::Library;
        state.players[P1.0 as usize].library.push_back(id);
        engine::game::zones::move_to_library_position(state, id, true, &mut events);
    }
    let top: Vec<ObjectId> = runner.state().players[P1.0 as usize]
        .library
        .iter()
        .take(2)
        .copied()
        .collect();
    assert_eq!(top, vec![warmaster, other_elf], "library top is staged");
    give_turn(&mut runner, P1);

    runner.cast(summons).commit();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => {
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Elf".to_string(),
                    })
                    .expect("creature type is accepted");
            }
            WaitingFor::Priority { .. }
                if runner.state().stack.iter().any(|entry| entry.id == summons) =>
            {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    order_identity(&mut runner);

    assert_eq!(runner.state().objects[&warmaster].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&other_elf].zone, Zone::Battlefield);
    assert_eq!(triggered_entries_of(runner.state(), warmaster), 1);
    assert!(observer_firings(runner.state(), aboleth).is_empty());
    let firings = observer_firings(runner.state(), proctor);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), warmaster);
}

/// V13: Ravenous's draw trigger is observed (X = 5); the counters are a
/// replacement effect (ruling R5), so X = 4 yields nothing.
#[test]
fn v13_ravenous_draw_trigger_is_observed_and_its_counters_are_not() {
    for x in [5u32, 4] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
        let cost: engine::types::mana::ManaCost =
            serde_json::from_str(r#"{"type":"Cost","shards":["X","Green"],"generic":1}"#)
                .expect("mana cost decodes");
        let aberrant = scenario
            .add_creature_to_hand(P1, "Aberrant", 0, 0)
            .from_oracle_text_with_keywords(&["ravenous", "trample"], ABERRANT)
            .with_mana_cost(cost)
            .id();
        scenario.with_mana_pool(
            P1,
            (0..(x + 2))
                .map(|_| {
                    engine::types::mana::ManaUnit::new(
                        engine::types::mana::ManaType::Green,
                        ObjectId(0),
                        false,
                        vec![],
                    )
                })
                .collect(),
        );
        let mut runner = scenario.build();
        install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
        give_turn(&mut runner, P1);

        runner.cast(aberrant).x(x).commit();
        for _ in 0..8 {
            let on_stack = runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.id == aberrant);
            if !on_stack || !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass");
        }
        order_identity(&mut runner);

        assert_eq!(runner.state().objects[&aberrant].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&aberrant]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            x,
            "Aberrant entered with X counters"
        );
        assert_eq!(
            triggered_entries_of(runner.state(), aberrant),
            usize::from(x == 5)
        );
        assert_eq!(
            observer_firings(runner.state(), aboleth).len(),
            usize::from(x == 5),
            "x = {x}"
        );
    }
}

/// V17: a player-scope single firing's record is every qualifying attacker
/// (CR 508.3d), though its narrowed context names the first one. Roku attacks
/// P1's Jace Beleren; a Raging Goblin attacks P1 and is declared first.
#[test]
fn v17_player_scope_record_names_every_qualifying_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let roku = scenario
        .add_creature_from_oracle(P0, "Avatar Roku, Firebender", 6, 6, AVATAR_ROKU)
        .id();
    let goblin = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
        .id();
    let jace = scenario
        .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, JACE_BELEREN)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (goblin, AttackTarget::Player(P1)),
            (roku, AttackTarget::Planeswalker(jace)),
        ])
        .expect("attack is accepted");
    order_identity(&mut runner);

    let roku_events: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event,
                ..
            } if *source_id == roku => Some(trigger_event.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(roku_events.len(), 1, "{roku_events:?}");
    assert_eq!(
        attackers_of(roku_events[0].as_ref()),
        vec![goblin],
        "the narrowed context names the first declared attacker"
    );
    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), roku);
}

/// V18: a player-scope batched trigger (opponent leg).
#[test]
fn v18_player_scope_batched_trigger_is_observed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let warmonger = scenario
        .add_creature_from_oracle(P0, "Frontier Warmonger", 4, 4, FRONTIER_WARMONGER)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[warmonger]);
    order_identity(&mut runner);

    assert_eq!(triggered_entries_of(runner.state(), warmonger), 1);
    let firings = observer_firings(runner.state(), ascension);
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(source_of(&firings[0]), warmonger);
}

/// V19: an unfiltered `YouAttack` trigger is caused by every declared
/// attacker: Caesar attacking counts; Caesar staying home does not.
#[test]
fn v19_you_attack_counts_only_when_its_source_attacked() {
    for caesar_attacks in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
        let caesar = scenario
            .add_creature_from_oracle(P0, "Caesar, Legion's Emperor", 4, 4, CAESAR)
            .id();
        let goblin = scenario
            .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
            .id();
        let mut runner = scenario.build();
        install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

        attack(&mut runner, &[if caesar_attacks { caesar } else { goblin }]);
        order_identity(&mut runner);

        assert_eq!(
            triggered_entries_of(runner.state(), caesar),
            1,
            "Caesar's trigger is on the stack (attacks: {caesar_attacks})"
        );
        assert_eq!(
            observer_firings(runner.state(), ascension).len(),
            usize::from(caesar_attacks),
            "caesar attacks: {caesar_attacks}"
        );
    }
}

/// V20: a filtered batched `YouAttack` trigger's record is its filter-matching
/// attackers only (the token), so Neyali's trigger is not "of" Neyali.
#[test]
fn v20_filtered_you_attack_record_excludes_its_non_matching_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let neyali = scenario
        .add_creature_from_oracle(P0, "Neyali, Suns' Vanguard", 3, 3, NEYALI)
        .id();
    // A Raise the Alarm Soldier token that has been under P0's control since
    // the turn began (CR 302.6).
    let token = scenario
        .add_creature(P0, "Soldier", 1, 1)
        .with_subtypes(vec!["Soldier"])
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[neyali, token]);
    order_identity(&mut runner);

    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.attackers.len() == 2),
        "both attackers were declared"
    );
    assert_eq!(triggered_entries_of(runner.state(), neyali), 1);
    assert!(observer_firings(runner.state(), ascension).is_empty());
}

/// V21b (b): a parked deferred context's `causes` are private scheduling
/// payload (CR 603.3b + CR 400.2). V8's board at Scaleguard's target prompt,
/// with Rakshasa's decayed context in `deferred_triggers`.
#[test]
fn v21b_deferred_context_causes_are_private() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Elite Scaleguard", 2, 3, ELITE_SCALEGUARD);
    let rakshasa = scenario
        .add_creature_from_oracle(P0, "Rot-Curse Rakshasa", 5, 5, ROT_CURSE_RAKSHASA)
        .id();
    scenario.with_counter(rakshasa, CounterType::Plus1Plus1, 1);
    scenario.add_vanilla(P1, 1, 6);
    scenario.add_vanilla(P1, 1, 6);
    let mut runner = scenario.build();

    attack(&mut runner, &[rakshasa]);
    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        panic!("got {:?}", runner.state().waiting_for);
    };
    let scaleguard_index = triggers
        .iter()
        .position(|t| t.source_id != rakshasa)
        .expect("Scaleguard's trigger is listed");
    let mut order = vec![scaleguard_index];
    order.extend((0..triggers.len()).filter(|i| *i != scaleguard_index));
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("ordering is accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));

    let parked_causes = |state: &GameState| -> Vec<bool> {
        let value = serde_json::to_value(&state.deferred_triggers).expect("serializes");
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter(|ctx| ctx["pending"]["source_id"] == serde_json::json!(rakshasa))
            .map(|ctx| {
                ctx.get("causes")
                    .is_some_and(|c| !c.as_array().unwrap().is_empty())
            })
            .collect()
    };
    assert_eq!(
        parked_causes(&filter_state_for_viewer(runner.state(), P0)),
        vec![true],
        "the controller's own copy keeps the record"
    );
    assert_eq!(
        parked_causes(&filter_state_for_viewer(runner.state(), P1)),
        vec![false],
        "another player's copy has no cause record"
    );
}

// ---------------------------------------------------------------------------
// CR 603.3b two-part placement and repetition
// ---------------------------------------------------------------------------
//
// CR 603.3b: "First, each player, in APNAP order, puts each triggered ability
// they control with a trigger condition that isn't another ability triggering
// on the stack in any order they choose. … Second, each player, in APNAP order,
// puts all remaining triggered abilities they control on the stack in any
// order they choose. Then … abilities that triggered during this process go on
// the stack. This process repeats …"

const ORIMS_PRAYER: &str =
    "Whenever one or more creatures attack you, you gain 1 life for each attacking creature.";
const HISTORIANS_BOON: &str = "Whenever this enchantment or another nontoken enchantment you control enters, create a 1/1 white Soldier creature token.\nWhenever the final chapter ability of a Saga you control triggers, create a 4/4 white Angel creature token with flying and vigilance.";
const HISTORY_OF_BENALIA: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II — Create a 2/2 white Knight creature token with vigilance.\nIII — Knights you control get +2/+1 until end of turn.";
const THE_ELDEST_REBORN: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Each opponent sacrifices a creature or planeswalker of their choice.\nII — Each opponent discards a card.\nIII — Put target creature or planeswalker card from a graveyard onto the battlefield under your control.";
const TAR_PIT_WARRIOR: &str =
    "When this creature becomes the target of a spell or ability, sacrifice it.";
const DEEPMUCK_DESPERADO: &str = "Whenever you commit a crime, each opponent mills three cards. This ability triggers only once each turn. (Targeting opponents, anything they control, and/or cards in their graveyards is a crime.)";
const HAMA_PASHAR: &str = "Room abilities of dungeons you own trigger an additional time.";
const FATES_REVERSAL: &str = "Return up to one target creature card from your graveyard to your hand. Venture into the dungeon. (Enter the first room or advance to the next room.)";
const PLANAR_VOID: &str =
    "Whenever another card is put into a graveyard from anywhere, exile that card.";
const DESECRATED_TOMB: &str = "Whenever one or more creature cards leave your graveyard, create a 1/1 black Bat creature token with flying.";
const DOOMED_DISSENTER: &str = "When this creature dies, create a 2/2 black Zombie creature token.";
const BOLG_OF_THE_NORTH: &str = "When Bolg enters, you may sacrifice another creature. When you do, Bolg deals damage equal to that creature's power to another target creature. If excess damage was dealt this way, amass Goblins X, where X is that excess damage. (Put X +1/+1 counters on an Army you control. It's also a Goblin. If you don't control an Army, create a 0/0 black Goblin Army creature token first.)";
const BACK_FOR_MORE: &str = "Return target creature card from your graveyard to the battlefield. When you do, it fights up to one target creature you don't control. (Each deals damage equal to its power to the other.)";
const DUNGEON_MAP: &str = "{T}: Add {C}.\n{3}, {T}: Venture into the dungeon. Activate only as a sorcery. (Enter the first room or advance to the next room.)";
const ACERERAK_THE_ARCHLICH: &str = "When Acererak enters, if you haven't completed Tomb of Annihilation, return Acererak to its owner's hand and venture into the dungeon.\nWhenever Acererak attacks, for each opponent, you create a 2/2 black Zombie creature token unless that player sacrifices a creature of their choice.";
const FLAMING_FIST_OFFICER: &str =
    "Whenever another creature you control leaves the battlefield, put a +1/+1 counter on this creature.";
const MORTICIAN_BEETLE: &str =
    "Whenever a player sacrifices a creature, you may put a +1/+1 counter on this creature.";
const PERILOUS_MYR: &str = "When this creature dies, it deals 2 damage to any target.";

/// One triggered stack entry or queued context, told apart by source, by the
/// ability it observes, or by its Oracle description.
#[derive(Clone, Copy, Debug)]
enum Pick {
    /// A trigger of this source that observes no ability.
    Of(ObjectId),
    /// A trigger of this source whose description contains the text.
    OfWith(ObjectId, &'static str),
    /// A trigger of this source whose description lacks the text.
    OfWithout(ObjectId, &'static str),
    /// An observer firing of the first source that observes the second.
    Observing(ObjectId, ObjectId),
}

fn observed_by(event: Option<&GameEvent>) -> Option<ObjectId> {
    match event {
        Some(GameEvent::AbilityTriggered { source_id, .. }) => Some(*source_id),
        _ => None,
    }
}

fn pick_matches(
    pick: Pick,
    source: ObjectId,
    event: Option<&GameEvent>,
    description: Option<&str>,
) -> bool {
    let observed = observed_by(event);
    match pick {
        Pick::Of(id) => source == id && observed.is_none(),
        Pick::OfWith(id, text) => {
            source == id && observed.is_none() && description.is_some_and(|d| d.contains(text))
        }
        Pick::OfWithout(id, text) => {
            source == id && observed.is_none() && !description.is_some_and(|d| d.contains(text))
        }
        Pick::Observing(observer, of) => source == observer && observed == Some(of),
    }
}

fn context_matches(ctx: &PendingTriggerContext, pick: Pick) -> bool {
    pick_matches(
        pick,
        ctx.pending.source_id,
        ctx.pending.trigger_event.as_ref(),
        ctx.pending.description.as_deref(),
    )
}

/// The triggered entries on the stack, bottom→top, with their trigger event
/// and description.
fn triggered_entries(state: &GameState) -> Vec<(ObjectId, Option<GameEvent>, Option<String>)> {
    state
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event,
                description,
                ..
            } => Some((*source_id, trigger_event.clone(), description.clone())),
            _ => None,
        })
        .collect()
}

/// The triggered stack sources, bottom→top.
fn triggered_stack_sources(state: &GameState) -> Vec<ObjectId> {
    triggered_entries(state)
        .into_iter()
        .map(|(source, _, _)| source)
        .collect()
}

fn object_name(state: &GameState, id: ObjectId) -> String {
    state
        .objects
        .get(&id)
        .map_or_else(|| format!("#{}", id.0), |obj| obj.name.clone())
}

fn target_names(state: &GameState, targets: &[TargetRef]) -> String {
    targets
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => object_name(state, *id),
            TargetRef::Player(player) => format!("P{}", player.0),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A readable bottom→top stack, for assertion messages.
fn stack_reading(state: &GameState) -> Vec<String> {
    state
        .stack
        .iter()
        .map(|entry| {
            let mut label = format!(
                "{}(P{})",
                object_name(state, entry.source_id),
                entry.controller.0
            );
            if let StackEntryKind::TriggeredAbility {
                trigger_event,
                description,
                ..
            } = &entry.kind
            {
                if let Some(of) = observed_by(trigger_event.as_ref()) {
                    label.push_str(&format!(" observing {}", object_name(state, of)));
                }
                if !state.objects.contains_key(&entry.source_id) {
                    label = description.clone().unwrap_or(label);
                    label.truncate(24);
                }
            } else {
                label = format!("spell {label}");
            }
            if let Some(ability) = entry.ability() {
                if !ability.targets.is_empty() {
                    label.push_str(&format!(" -> {}", target_names(state, &ability.targets)));
                }
            }
            label
        })
        .collect()
}

/// Assert the triggered stack entries, bottom→top, are exactly `expected`.
fn assert_triggered_stack(state: &GameState, expected: &[Pick], what: &str) {
    let entries = triggered_entries(state);
    let matches = entries.len() == expected.len()
        && entries
            .iter()
            .zip(expected)
            .all(|((source, event, description), pick)| {
                pick_matches(*pick, *source, event.as_ref(), description.as_deref())
            });
    assert!(
        matches,
        "{what}: expected {expected:?} bottom→top, got {:?}",
        stack_reading(state)
    );
}

/// Whether `items` and `expected` match one to one, in any order (each pick
/// consumes one item).
fn matches_in_any_order<T>(
    items: &[T],
    expected: &[Pick],
    matches: impl Fn(&T, Pick) -> bool,
) -> bool {
    let mut used = vec![false; items.len()];
    items.len() == expected.len()
        && expected.iter().all(|pick| {
            let found = (0..items.len()).find(|&i| !used[i] && matches(&items[i], *pick));
            found.map(|i| used[i] = true).is_some()
        })
}

/// CR 603.3b + CR 101.4: assert the triggered stack entries, bottom→top, are
/// `groups` in this order, the abilities of each group in any order. A group
/// is a set of abilities its controller may put on the stack in any order, so
/// only the order of the groups is asserted.
fn assert_triggered_stack_groups(state: &GameState, groups: &[&[Pick]], what: &str) {
    let entries = triggered_entries(state);
    let total: usize = groups.iter().map(|group| group.len()).sum();
    let mut rest = entries.as_slice();
    let matches = entries.len() == total
        && groups.iter().all(|group| {
            let (bottom, above) = rest.split_at(group.len());
            rest = above;
            matches_in_any_order(bottom, group, |(source, event, description), pick| {
                pick_matches(pick, *source, event.as_ref(), description.as_deref())
            })
        });
    assert!(
        matches,
        "{what}: expected the groups {groups:?} bottom→top, got {:?}",
        stack_reading(state)
    );
}

/// The contexts the open `OrderTriggers` prompt lists, in prompt order (the
/// first unordered group of `pending_trigger_order`).
fn prompt_contexts(state: &GameState) -> Vec<PendingTriggerContext> {
    state
        .pending_trigger_order
        .as_ref()
        .and_then(|order| order.groups.iter().find(|group| !group.ordered))
        .map(|group| group.triggers.clone())
        .unwrap_or_default()
}

/// Whether the open prompt lists exactly the contexts `expected` picks (in
/// any order).
fn prompt_lists_exactly(state: &GameState, expected: &[Pick]) -> bool {
    matches_in_any_order(&prompt_contexts(state), expected, |ctx, pick| {
        context_matches(ctx, pick)
    })
}

/// CR 603.3b: submit the order whose first listed contexts are `first` (each
/// pick consumes one listed context); the rest follow in prompt order. Returns
/// the action's result.
fn order_by_identity(runner: &mut GameRunner, first: &[Pick]) -> ActionResult {
    let contexts = prompt_contexts(runner.state());
    let mut used = vec![false; contexts.len()];
    let mut order = Vec::new();
    for pick in first {
        let index = (0..contexts.len())
            .find(|&i| !used[i] && context_matches(&contexts[i], *pick))
            .unwrap_or_else(|| panic!("{pick:?} is not listed by the order prompt"));
        used[index] = true;
        order.push(index);
    }
    order.extend((0..contexts.len()).filter(|&i| !used[i]));
    runner
        .act(GameAction::OrderTriggers { order })
        .expect("the engine-issued order is legal")
}

/// Answer every order prompt with the identity order; return how many were
/// answered.
fn count_identity_orders(runner: &mut GameRunner) -> usize {
    let mut answered = 0;
    while let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the identity order is legal");
        answered += 1;
        assert!(answered <= 8, "too many order prompts");
    }
    answered
}

/// CR 603.3b + CR 603.5: no construction prompt opens for an ability that is
/// placed in the second part (none of the class's second-part abilities has a
/// target, mode or division).
fn assert_no_second_part_construction(runner: &GameRunner, second_part_sources: &[ObjectId]) {
    let state = runner.state();
    let constructing = matches!(
        state.waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::AbilityModeChoice { .. }
            | WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::DistributeAmong { .. }
    );
    if constructing {
        if let Some(pending) = state.pending_trigger.as_ref() {
            assert!(
                !second_part_sources.contains(&pending.source_id),
                "a second-part ability opened a construction prompt: {:?}",
                state.waiting_for
            );
        }
    }
}

/// The completion boundary: `Priority`, nothing queued, no open order.
fn assert_completion_boundary(runner: &GameRunner, what: &str) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{what}: expected Priority, got {:?}",
        state.waiting_for
    );
    assert!(
        state.deferred_triggers.is_empty(),
        "{what}: the queue is empty at the boundary"
    );
    assert!(
        state.pending_trigger_order.is_none(),
        "{what}: no order is open at the boundary"
    );
}

/// CR 603.3b: the queued contexts that `pick` names.
fn queued_matching(state: &GameState, pick: Pick) -> usize {
    state
        .deferred_triggers
        .iter()
        .filter(|ctx| context_matches(ctx, pick))
        .count()
}

/// Round-trip the live state through its serialized form at a pause and keep
/// playing from the restored copy. Returns whether the serialized queue or
/// order groups carried a placement-process mark.
fn round_trip_state(runner: &mut GameRunner) -> bool {
    let json = serde_json::to_string(runner.state()).expect("the state serializes");
    let restored: GameState = serde_json::from_str(&json).expect("the state deserializes");
    *runner.state_mut() = restored;
    json.contains("\"placement_process\"")
}

fn tts_legal_targets(runner: &GameRunner) -> Vec<TargetRef> {
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for
    else {
        panic!(
            "expected a trigger target prompt, got {:?} (stack {:?})",
            runner.state().waiting_for,
            stack_reading(runner.state())
        );
    };
    target_slots[0].legal_targets.clone()
}

fn assert_tts(runner: &GameRunner, source: Option<ObjectId>, legal: &[TargetRef], what: &str) {
    let WaitingFor::TriggerTargetSelection { source_id, .. } = &runner.state().waiting_for else {
        panic!(
            "{what}: expected a trigger target prompt, got {:?} (stack {:?})",
            runner.state().waiting_for,
            stack_reading(runner.state())
        );
    };
    if let Some(source) = source {
        assert_eq!(*source_id, Some(source), "{what}: the prompt's source");
    }
    let mut got = tts_legal_targets(runner);
    let mut want = legal.to_vec();
    let key = |t: &TargetRef| match t {
        TargetRef::Object(id) => (0u8, id.0),
        TargetRef::Player(p) => (1u8, u64::from(p.0)),
    };
    got.sort_by_key(key);
    want.sort_by_key(key);
    assert_eq!(got, want, "{what}: the legal targets");
}

fn select(runner: &mut GameRunner, targets: Vec<TargetRef>) -> ActionResult {
    runner
        .act(GameAction::SelectTargets { targets })
        .expect("the engine-offered targets are legal")
}

fn object(id: ObjectId) -> TargetRef {
    TargetRef::Object(id)
}

/// Advance from P0's precombat main to P0's next precombat main, declaring no
/// attackers or blockers on the way.
fn advance_to_p0_next_precombat_main(runner: &mut GameRunner) {
    let start_turn = runner.state().turn_number;
    for _ in 0..80 {
        let state = runner.state();
        if state.turn_number > start_turn
            && state.active_player == P0
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        let action = match &state.waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: Vec::new(),
                bands: Vec::new(),
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            // A placement prompt on the way is the caller's to assert.
            _ => return,
        };
        runner.act(action).expect("the turn advances");
    }
    panic!("never reached P0's next precombat main");
}

/// Cast Azula at this turn and move to P0's next precombat main, so Azula's
/// entered-battlefield turn is later than every creature the scenario built.
fn azula_entered_this_turn(runner: &mut GameRunner, azula: ObjectId) {
    cast_and_resolve(runner, azula);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.pass_both_players();
    }
    advance_to_p0_next_precombat_main(runner);
    let state = runner.state();
    assert!(
        state.active_player == P0
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "P0's next precombat main: {:?}",
        state.waiting_for
    );
}

/// CR 603.3b + CR 101.4: an observer goes on the stack after every first-part
/// trigger of every player, even when the observed ability paused for its
/// target. Iroh's mentor (two legal targets) and Zuko's firebending are P0's;
/// Orim's Prayer is P1's.
#[test]
fn observer_goes_above_every_players_first_part_across_a_target_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let iroh = scenario
        .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
        .id();
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let goblin = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
        .id();
    let prayer = scenario
        .add_enchantment_from_oracle(P1, "Orim's Prayer", ORIMS_PRAYER)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);
    let second_part = [ascension];

    attack(&mut runner, &[iroh, zuko, goblin]);
    assert!(
        prompt_lists_exactly(runner.state(), &[Pick::Of(iroh), Pick::Of(zuko)]),
        "P0 orders the mentor and the firebending, got {:?}",
        runner.state().waiting_for
    );
    order_by_identity(&mut runner, &[Pick::Of(iroh)]);
    assert_no_second_part_construction(&runner, &second_part);
    assert_tts(
        &runner,
        Some(iroh),
        &[object(zuko), object(goblin)],
        "the mentor",
    );
    assert_not_first_ability_of_an_ability_resolution(&runner, "the mentor");
    select(&mut runner, vec![object(goblin)]);
    let answered = count_identity_orders(&mut runner);
    assert_eq!(answered, 1, "one order prompt for the two firings");

    assert_completion_boundary(&runner, "after the mentor's target");
    assert_triggered_stack(
        runner.state(),
        &[
            Pick::Of(iroh),
            Pick::Of(zuko),
            Pick::Of(prayer),
            Pick::Observing(ascension, iroh),
            Pick::Observing(ascension, zuko),
        ],
        "CR 603.3b: both parts, APNAP within the first",
    );
}

/// CR 603.3b: a chosen first-part order survives a target pause beside
/// observers, and the second part is ordered as one complete set. The order
/// submitted is against timestamp order (Azula entered after Zuko).
#[test]
fn chosen_first_part_order_survives_a_pause_and_the_second_part_is_ordered_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains", "Plains"]);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let iroh = scenario
        .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
        .id();
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let goblin = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
        .id();
    let azula = scenario
        .add_creature_to_hand(P0, "Azula, On the Hunt", 4, 3)
        .from_oracle_text_with_keywords(&["firebending"], AZULA)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);
    azula_entered_this_turn(&mut runner, azula);
    let entered = |id: ObjectId| runner.state().objects[&id].entered_battlefield_turn;
    assert!(
        entered(azula) > entered(zuko),
        "Azula entered after Zuko: {:?} vs {:?}",
        entered(azula),
        entered(zuko)
    );
    let second_part = [ascension];

    attack(&mut runner, &[iroh, zuko, azula, goblin]);
    let a1 = Pick::OfWithout(azula, "lose 1 life");
    let a2 = Pick::OfWith(azula, "lose 1 life");
    assert!(
        prompt_lists_exactly(runner.state(), &[Pick::Of(iroh), Pick::Of(zuko), a1, a2]),
        "P0 orders four first-part triggers, got {:?}",
        runner.state().waiting_for
    );
    order_by_identity(&mut runner, &[a1, Pick::Of(iroh), a2, Pick::Of(zuko)]);
    assert_no_second_part_construction(&runner, &second_part);
    assert_tts(
        &runner,
        Some(iroh),
        &[object(zuko), object(goblin)],
        "the mentor",
    );
    assert_not_first_ability_of_an_ability_resolution(&runner, "the mentor");
    let result = select(&mut runner, vec![object(goblin)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
        "the second part is ordered next, got {:?}",
        result.waiting_for
    );
    assert!(
        prompt_lists_exactly(
            runner.state(),
            &[
                Pick::Observing(ascension, azula),
                Pick::Observing(ascension, iroh),
                Pick::Observing(ascension, azula),
                Pick::Observing(ascension, zuko),
            ]
        ),
        "the prompt lists exactly the four firings"
    );
    assert_triggered_stack(
        runner.state(),
        &[a1, Pick::Of(iroh), a2, Pick::Of(zuko)],
        "CR 603.3b: the chosen first-part order",
    );
    count_identity_orders(&mut runner);

    assert_completion_boundary(&runner, "after the second part");
    let entries = triggered_entries(runner.state());
    assert_eq!(entries.len(), 8);
    assert!(
        entries[4..]
            .iter()
            .all(|(source, event, _)| *source == ascension && observed_by(event.as_ref()).is_some()),
        "the top four entries are the firings: {:?}",
        stack_reading(runner.state())
    );
}

/// CR 603.3b + CR 101.4: an entering causes part-2 triggers of two players;
/// both go above every first-part trigger, APNAP within the second part (P1 is
/// active).
#[test]
fn enter_observers_of_two_players_follow_every_first_part_trigger_in_apnap_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proctor_p1 = add_observer(&mut scenario, P1, Observer::StrictProctor);
    let hunter = scenario
        .add_creature_to_hand_from_oracle(P1, "Fiend Hunter", 1, 3, FIEND_HUNTER)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let proctor_p0 = add_observer(&mut scenario, P0, Observer::StrictProctor);
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), proctor_p1, Observer::StrictProctor);
    install_observer(runner.state_mut(), proctor_p0, Observer::StrictProctor);
    give_turn(&mut runner, P1);
    let second_part = [proctor_p0, proctor_p1];

    cast_and_resolve(&mut runner, hunter);
    assert_no_second_part_construction(&runner, &second_part);
    assert!(tts_legal_targets(&runner).len() >= 2);
    assert_not_first_ability_of_an_ability_resolution(&runner, "Fiend Hunter's ETB");
    select(&mut runner, vec![object(warden)]);
    let mut prompts = Vec::new();
    while let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() {
        prompts.push(player);
        assert_eq!(triggers.len(), 2);
        let contexts = prompt_contexts(runner.state());
        assert_eq!(
            contexts.len(),
            triggers.len(),
            "the open order group lists the prompt's two firings"
        );
        assert!(contexts
            .iter()
            .all(
                |ctx| observed_by(ctx.pending.trigger_event.as_ref()).is_some()
                    && ctx.pending.controller == player
            ));
        count_identity_orders_once(&mut runner);
    }
    assert_eq!(prompts, vec![P1, P0], "one prompt per controller, APNAP");

    assert_completion_boundary(&runner, "after both parts");
    let sources = triggered_stack_sources(runner.state());
    assert_eq!(
        sources,
        vec![hunter, warden, proctor_p1, proctor_p1, proctor_p0, proctor_p0],
        "bottom→top: {:?}",
        stack_reading(runner.state())
    );
}

fn count_identity_orders_once(runner: &mut GameRunner) {
    let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() else {
        panic!("expected an order prompt");
    };
    runner
        .act(GameAction::OrderTriggers {
            order: (0..triggers.len()).collect(),
        })
        .expect("the identity order is legal");
}

fn saga_on_battlefield(
    scenario: &mut GameScenario,
    name: &str,
    cost: engine::types::mana::ManaCost,
    oracle: &str,
) -> ObjectId {
    let id = scenario
        .add_creature(P0, name, 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .with_mana_cost(cost)
        .from_oracle_text(oracle)
        .id();
    // CR 714.3a: the scenario seeds the Saga with the lore counters it has.
    scenario.with_counter(id, CounterType::Lore, 2);
    id
}

fn lore(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0)
}

/// CR 603.3b + CR 714.2e: Historian's Boon's "the final chapter ability of a
/// Saga you control triggers" is a second-part trigger: it goes on the stack
/// after chapter III, whichever permanent was created first.
#[test]
fn final_chapter_observer_is_placed_after_the_chapter_ability() {
    for boon_first in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
        scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
        let benalia_cost = engine::types::mana::ManaCost::generic(3);
        let (boon, history) = if boon_first {
            let boon = scenario
                .add_enchantment_from_oracle(P0, "Historian's Boon", HISTORIANS_BOON)
                .id();
            let history = saga_on_battlefield(
                &mut scenario,
                "History of Benalia",
                benalia_cost,
                HISTORY_OF_BENALIA,
            );
            (boon, history)
        } else {
            let history = saga_on_battlefield(
                &mut scenario,
                "History of Benalia",
                benalia_cost,
                HISTORY_OF_BENALIA,
            );
            let boon = scenario
                .add_enchantment_from_oracle(P0, "Historian's Boon", HISTORIANS_BOON)
                .id();
            (boon, history)
        };
        let mut runner = scenario.build();

        advance_to_p0_next_precombat_main(&mut runner);
        assert_eq!(lore(&runner, history), 3, "boon first {boon_first}");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "boon first {boon_first}: no order prompt, got {:?}",
            runner.state().waiting_for
        );
        assert_triggered_stack(
            runner.state(),
            &[Pick::Of(history), Pick::Of(boon)],
            &format!("boon first {boon_first}"),
        );
    }
}

/// CR 603.3b + CR 700.13: a fresh batch's second part (Historian's Boon) goes
/// on the stack before a crime trigger that triggered while the batch's first
/// part (The Eldest Reborn's chapter III, auto-targeting P1's graveyard card)
/// was put on the stack.
#[test]
fn fresh_second_part_goes_before_what_its_first_part_triggered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains", "Plains"]);
    let boon = scenario
        .add_enchantment_from_oracle(P0, "Historian's Boon", HISTORIANS_BOON)
        .id();
    let reborn = saga_on_battlefield(
        &mut scenario,
        "The Eldest Reborn",
        engine::types::mana::ManaCost::generic(5),
        THE_ELDEST_REBORN,
    );
    let deepmuck = scenario
        .add_creature_from_oracle(P0, "Deepmuck Desperado", 2, 4, DEEPMUCK_DESPERADO)
        .id();
    let goblin_card = scenario
        .add_creature_to_graveyard(P1, "Raging Goblin", 1, 1)
        .id();
    let mut runner = scenario.build();

    advance_to_p0_next_precombat_main(&mut runner);
    assert_eq!(lore(&runner, reborn), 3);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no order or target prompt, got {:?}",
        runner.state().waiting_for
    );
    assert_triggered_stack(
        runner.state(),
        &[Pick::Of(reborn), Pick::Of(boon), Pick::Of(deepmuck)],
        "CR 603.3b: III, Boon, then the crime trigger",
    );
    let chapter = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == reborn)
        .and_then(|entry| entry.ability())
        .expect("chapter III is on the stack");
    assert_eq!(chapter.targets, vec![object(goblin_card)]);
}

/// CR 603.3b: an ability that triggers while an ability is put on the stack
/// (Tar Pit Warrior becoming the auto-chosen mentor target) goes above that
/// ability's observer.
#[test]
fn becomes_target_trigger_goes_above_the_observer_of_its_targeting_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ascension = add_observer(&mut scenario, P0, Observer::FirebenderAscension);
    let iroh = scenario
        .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
        .id();
    let tar_pit = scenario
        .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);

    attack(&mut runner, &[iroh, tar_pit]);
    assert_completion_boundary(&runner, "no order prompt");
    assert_triggered_stack(
        runner.state(),
        &[
            Pick::Of(iroh),
            Pick::Observing(ascension, iroh),
            Pick::Of(tar_pit),
        ],
        "CR 603.3b: mentor, its observer, then Tar Pit",
    );
}

/// CR 603.3b + CR 702.21a: Aboleth Spawn's ward triggers while Fiend Hunter's
/// ETB (P1's) is put on the stack targeting it; it goes above Aboleth's own
/// observer firing.
#[test]
fn ward_of_an_observer_goes_above_its_firing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aboleth = add_observer(&mut scenario, P0, Observer::AbolethSpawn);
    let hunter = scenario
        .add_creature_to_hand_from_oracle(P1, "Fiend Hunter", 1, 3, FIEND_HUNTER)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), aboleth, Observer::AbolethSpawn);
    give_turn(&mut runner, P1);

    cast_and_resolve(&mut runner, hunter);
    assert_completion_boundary(&runner, "no order prompt");
    assert_triggered_stack(
        runner.state(),
        &[
            Pick::Of(hunter),
            Pick::Observing(aboleth, hunter),
            Pick::Of(aboleth),
        ],
        "CR 603.3b: ETB, Aboleth's firing, ward",
    );
}

/// The board shared by the chosen-remainder tests: P0's Iroh, Zuko, Raging
/// Goblin, Goblin Racketeer and Deepmuck Desperado (built), Azula cast this
/// turn, and Firebender Ascension when asked.
struct RemainderBoard {
    runner: GameRunner,
    ascension: Option<ObjectId>,
    iroh: ObjectId,
    zuko: ObjectId,
    azula: ObjectId,
    goblin: ObjectId,
    racketeer: ObjectId,
    deepmuck: ObjectId,
    theirs: Vec<ObjectId>,
}

fn remainder_board(with_ascension: bool, p1_bears: usize) -> RemainderBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains", "Plains"]);
    let ascension =
        with_ascension.then(|| add_observer(&mut scenario, P0, Observer::FirebenderAscension));
    let iroh = scenario
        .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
        .id();
    let zuko = scenario
        .add_creature(P0, "Zuko, Seeking Honor", 2, 2)
        .from_oracle_text_with_keywords(&["firebending"], ZUKO)
        .id();
    let goblin = scenario
        .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
        .id();
    let racketeer = scenario
        .add_creature_from_oracle(P0, "Goblin Racketeer", 4, 2, GOBLIN_RACKETEER)
        .id();
    let deepmuck = scenario
        .add_creature_from_oracle(P0, "Deepmuck Desperado", 2, 4, DEEPMUCK_DESPERADO)
        .id();
    let azula = scenario
        .add_creature_to_hand(P0, "Azula, On the Hunt", 4, 3)
        .from_oracle_text_with_keywords(&["firebending"], AZULA)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let theirs = if p1_bears == 0 {
        vec![scenario
            .add_creature_from_oracle(P1, "Raging Goblin", 1, 1, RAGING_GOBLIN)
            .id()]
    } else {
        (0..p1_bears)
            .map(|_| scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
            .collect()
    };
    let mut runner = scenario.build();
    if let Some(ascension) = ascension {
        install_observer(runner.state_mut(), ascension, Observer::FirebenderAscension);
    }
    azula_entered_this_turn(&mut runner, azula);
    let entered = |id: ObjectId| runner.state().objects[&id].entered_battlefield_turn;
    assert!(entered(azula) > entered(zuko), "Azula entered after Zuko");
    RemainderBoard {
        runner,
        ascension,
        iroh,
        zuko,
        azula,
        goblin,
        racketeer,
        deepmuck,
        theirs,
    }
}

/// CR 603.3b + CR 700.13: a chosen first-part order keeps its order beside a
/// crime trigger collected while the auto-targeted goad was put on the stack,
/// and that trigger goes above the whole process (both parts).
#[test]
fn chosen_remainder_keeps_its_order_beside_a_window_collected_crime_trigger() {
    for with_ascension in [false, true] {
        let RemainderBoard {
            mut runner,
            ascension,
            iroh,
            zuko,
            azula,
            goblin,
            racketeer,
            deepmuck,
            theirs,
        } = remainder_board(with_ascension, 0);
        let second_part: Vec<ObjectId> = ascension.into_iter().collect();
        let what = format!("with ascension {with_ascension}");

        attack(&mut runner, &[iroh, zuko, azula, goblin, racketeer]);
        let (g, m, z) = (Pick::Of(racketeer), Pick::Of(iroh), Pick::Of(zuko));
        let a1 = Pick::OfWithout(azula, "lose 1 life");
        let a2 = Pick::OfWith(azula, "lose 1 life");
        assert!(
            prompt_lists_exactly(runner.state(), &[g, m, z, a1, a2]),
            "{what}: five first-part triggers, got {:?}",
            runner.state().waiting_for
        );
        order_by_identity(&mut runner, &[g, m, a1, a2, z]);
        assert_no_second_part_construction(&runner, &second_part);
        assert_tts(&runner, Some(iroh), &[object(zuko), object(goblin)], &what);
        assert_not_first_ability_of_an_ability_resolution(&runner, &what);
        let goad = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == racketeer)
            .and_then(|entry| entry.ability())
            .expect("the goad was put on the stack");
        assert_eq!(goad.targets, vec![object(theirs[0])], "{what}");
        select(&mut runner, vec![object(goblin)]);
        let answered = count_identity_orders(&mut runner);
        assert_eq!(answered, usize::from(with_ascension), "{what}: prompts");

        assert_completion_boundary(&runner, &what);
        let mut expected = vec![g, m, a1, a2, z];
        if let Some(ascension) = ascension {
            expected.extend([
                Pick::Observing(ascension, racketeer),
                Pick::Observing(ascension, iroh),
                Pick::Observing(ascension, azula),
                Pick::Observing(ascension, azula),
                Pick::Observing(ascension, zuko),
            ]);
        }
        expected.push(Pick::Of(deepmuck));
        let entries = triggered_entries(runner.state());
        assert_eq!(
            entries.len(),
            expected.len(),
            "{what}: {:?}",
            stack_reading(runner.state())
        );
        // The firings are one controller's ordered set; check them as a set.
        let (fixed_bottom, rest) = expected.split_at(5);
        assert_triggered_stack_prefix(runner.state(), fixed_bottom, &what);
        assert!(
            pick_matches(
                Pick::Of(deepmuck),
                entries.last().unwrap().0,
                entries.last().unwrap().1.as_ref(),
                None
            ),
            "{what}: the crime trigger is on top: {:?}",
            stack_reading(runner.state())
        );
        let firings = &entries[5..entries.len() - 1];
        assert_eq!(firings.len(), rest.len() - 1, "{what}");
        assert!(firings
            .iter()
            .all(|(source, event, _)| Some(*source) == ascension
                && observed_by(event.as_ref()).is_some()));
    }
}

/// Assert the bottom of the triggered stack is exactly `expected`.
fn assert_triggered_stack_prefix(state: &GameState, expected: &[Pick], what: &str) {
    let entries = triggered_entries(state);
    let matches = entries.len() >= expected.len()
        && entries
            .iter()
            .zip(expected)
            .all(|((source, event, description), pick)| {
                pick_matches(*pick, *source, event.as_ref(), description.as_deref())
            });
    assert!(
        matches,
        "{what}: expected bottom {expected:?}, got {:?}",
        stack_reading(state)
    );
}

/// CR 603.3b + CR 601.2c via CR 603.3d + CR 603.2e: Tar Pit Warrior (P0)
/// becomes the target of Fiend Hunter's ETB (P1's) as that ETB is put on the
/// stack after a target choice; its trigger is collected once and goes on
/// after the whole process, both parts, the second of which P1 orders.
#[test]
fn finisher_collected_trigger_goes_after_both_parts_at_an_order_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proctor = add_observer(&mut scenario, P1, Observer::StrictProctor);
    let hunter = scenario
        .add_creature_to_hand_from_oracle(P1, "Fiend Hunter", 1, 3, FIEND_HUNTER)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .id();
    let tar_pit = scenario
        .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
        .id();
    let mut runner = scenario.build();
    install_observer(runner.state_mut(), proctor, Observer::StrictProctor);
    give_turn(&mut runner, P1);
    let second_part = [proctor];

    cast_and_resolve(&mut runner, hunter);
    assert_no_second_part_construction(&runner, &second_part);
    assert_tts(
        &runner,
        Some(hunter),
        &[object(proctor), object(warden), object(tar_pit)],
        "Fiend Hunter's ETB",
    );
    assert!(
        runner.state().resolving_stack_entry.is_none(),
        "no resolution is under way"
    );
    assert_not_first_ability_of_an_ability_resolution(&runner, "Fiend Hunter's ETB");
    let result = select(&mut runner, vec![object(tar_pit)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P1),
        "P1 orders the two firings, got {:?} (stack {:?})",
        result.waiting_for,
        stack_reading(runner.state())
    );
    assert!(prompt_lists_exactly(
        runner.state(),
        &[
            Pick::Observing(proctor, hunter),
            Pick::Observing(proctor, warden)
        ]
    ));
    assert_eq!(
        queued_matching(runner.state(), Pick::Of(tar_pit)),
        1,
        "Tar Pit's trigger waits in the queue"
    );
    assert_eq!(runner.state().deferred_triggers.len(), 1);
    assert!(triggered_stack_sources(runner.state()).contains(&warden));
    let marked = round_trip_state(&mut runner);
    order_by_identity(&mut runner, &[Pick::Observing(proctor, warden)]);

    assert_completion_boundary(&runner, "after the order");
    assert_triggered_stack(
        runner.state(),
        &[
            Pick::Of(hunter),
            Pick::Of(warden),
            Pick::Observing(proctor, warden),
            Pick::Observing(proctor, hunter),
            Pick::Of(tar_pit),
        ],
        "CR 603.3b: part 1, part 2 in P1's order, then Tar Pit",
    );
    assert!(
        marked,
        "the serialized order groups carried the placement-process mark"
    );
}

/// CR 603.3b + CR 700.13: the goad (two legal targets) commits a crime as it
/// is put on the stack after its target choice; Deepmuck Desperado's trigger
/// waits beside the chosen remainder (mentor, Azula's two, Zuko), which keeps
/// its order, and goes above the whole process, once.
#[test]
fn finisher_collected_crime_waits_beside_a_chosen_remainder() {
    for with_ascension in [false, true] {
        let RemainderBoard {
            mut runner,
            ascension,
            iroh,
            zuko,
            azula,
            goblin,
            racketeer,
            deepmuck,
            theirs,
        } = remainder_board(with_ascension, 2);
        let second_part: Vec<ObjectId> = ascension.into_iter().collect();
        let what = format!("with ascension {with_ascension}");

        attack(&mut runner, &[iroh, zuko, azula, goblin, racketeer]);
        let (g, m, z) = (Pick::Of(racketeer), Pick::Of(iroh), Pick::Of(zuko));
        let a1 = Pick::OfWithout(azula, "lose 1 life");
        let a2 = Pick::OfWith(azula, "lose 1 life");
        assert!(
            prompt_lists_exactly(runner.state(), &[g, m, z, a1, a2]),
            "{what}: five first-part triggers, got {:?}",
            runner.state().waiting_for
        );
        order_by_identity(&mut runner, &[g, m, a1, a2, z]);
        assert_no_second_part_construction(&runner, &second_part);
        assert_tts(
            &runner,
            Some(racketeer),
            &[object(theirs[0]), object(theirs[1])],
            &what,
        );
        assert!(
            runner.state().resolving_stack_entry.is_none(),
            "{what}: no resolution is under way"
        );
        assert_not_first_ability_of_an_ability_resolution(&runner, &what);
        select(&mut runner, vec![object(theirs[0])]);
        assert_no_second_part_construction(&runner, &second_part);
        assert_tts(&runner, Some(iroh), &[object(zuko), object(goblin)], &what);
        assert!(
            runner.state().resolving_stack_entry.is_none(),
            "{what}: no resolution is under way"
        );
        assert_not_first_ability_of_an_ability_resolution(&runner, &what);
        assert_eq!(
            queued_matching(runner.state(), Pick::Of(deepmuck)),
            1,
            "{what}: the crime trigger is queued"
        );
        if let Some(ascension) = ascension {
            assert_eq!(
                queued_matching(runner.state(), Pick::Observing(ascension, racketeer)),
                1,
                "{what}: the goad's firing is queued"
            );
        }
        let marked = round_trip_state(&mut runner);
        let result = select(&mut runner, vec![object(goblin)]);
        if let Some(ascension) = ascension {
            assert!(
                prompt_lists_exactly(
                    runner.state(),
                    &[
                        Pick::Observing(ascension, racketeer),
                        Pick::Observing(ascension, iroh),
                        Pick::Observing(ascension, azula),
                        Pick::Observing(ascension, azula),
                        Pick::Observing(ascension, zuko),
                    ]
                ),
                "{what}: the second part is one prompt, got {:?}",
                result.waiting_for
            );
            count_identity_orders_once(&mut runner);
        }

        assert_completion_boundary(&runner, &what);
        assert_triggered_stack_prefix(runner.state(), &[g, m, a1, a2, z], &what);
        let entries = triggered_entries(runner.state());
        let expected_len = 6 + if with_ascension { 5 } else { 0 };
        assert_eq!(
            entries.len(),
            expected_len,
            "{what}: {:?}",
            stack_reading(runner.state())
        );
        let (top, _, _) = entries.last().unwrap();
        assert_eq!(*top, deepmuck, "{what}: the crime trigger is on top");
        assert_eq!(
            triggered_stack_sources(runner.state())
                .iter()
                .filter(|s| **s == deepmuck)
                .count(),
            1,
            "{what}: one crime trigger"
        );
        assert!(
            marked,
            "{what}: the serialized queue carried the placement-process mark"
        );
    }
}

/// CR 603.3b + CR 101.4: abilities that triggered during one process, one
/// collected while the auto-targeted goad was put on the stack and one when
/// the mentor's target was chosen, go on the stack together in the next
/// repetition, each player's in APNAP order.
#[test]
fn triggers_of_one_process_from_two_controllers_are_placed_in_apnap_order() {
    // P1's Tar Pit Warrior is the goad's only target.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let iroh = scenario
            .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
            .id();
        let p0_tar_pit = scenario
            .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
            .id();
        let goblin = scenario
            .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
            .id();
        let racketeer = scenario
            .add_creature_from_oracle(P0, "Goblin Racketeer", 4, 2, GOBLIN_RACKETEER)
            .id();
        let p1_tar_pit = scenario
            .add_creature_from_oracle(P1, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
            .id();
        let mut runner = scenario.build();

        attack(&mut runner, &[iroh, p0_tar_pit, goblin, racketeer]);
        assert!(
            prompt_lists_exactly(runner.state(), &[Pick::Of(iroh), Pick::Of(racketeer)]),
            "got {:?}",
            runner.state().waiting_for
        );
        order_by_identity(&mut runner, &[Pick::Of(racketeer), Pick::Of(iroh)]);
        let goad = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == racketeer)
            .and_then(|entry| entry.ability())
            .expect("the goad is on the stack");
        assert_eq!(goad.targets, vec![object(p1_tar_pit)]);
        assert_eq!(queued_matching(runner.state(), Pick::Of(p1_tar_pit)), 1);
        assert_tts(
            &runner,
            Some(iroh),
            &[object(p0_tar_pit), object(goblin)],
            "the mentor",
        );
        assert!(
            runner.state().resolving_stack_entry.is_none(),
            "no resolution is under way"
        );
        assert_not_first_ability_of_an_ability_resolution(&runner, "the mentor");
        select(&mut runner, vec![object(p0_tar_pit)]);

        assert_completion_boundary(&runner, "cross-controller");
        assert_triggered_stack(
            runner.state(),
            &[
                Pick::Of(racketeer),
                Pick::Of(iroh),
                Pick::Of(p0_tar_pit),
                Pick::Of(p1_tar_pit),
            ],
            "CR 603.3b + CR 101.4: P0's Tar Pit, then P1's",
        );
    }
}

/// CR 603.3b + CR 700.13: the same, with both abilities P0's: the goad's
/// auto-target is a crime (Deepmuck Desperado) and the mentor targets P0's Tar
/// Pit Warrior; P0 orders the two.
#[test]
fn triggers_of_one_process_from_one_controller_are_ordered_together() {
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
        let iroh = scenario
            .add_creature_from_oracle(P0, "Iroh, Dragon of the West", 4, 4, IROH)
            .id();
        let tar_pit = scenario
            .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
            .id();
        let goblin = scenario
            .add_creature_from_oracle(P0, "Raging Goblin", 1, 1, RAGING_GOBLIN)
            .id();
        let racketeer = scenario
            .add_creature_from_oracle(P0, "Goblin Racketeer", 4, 2, GOBLIN_RACKETEER)
            .id();
        let deepmuck = scenario
            .add_creature_from_oracle(P0, "Deepmuck Desperado", 2, 4, DEEPMUCK_DESPERADO)
            .id();
        let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();

        attack(&mut runner, &[iroh, tar_pit, goblin, racketeer]);
        order_by_identity(&mut runner, &[Pick::Of(racketeer), Pick::Of(iroh)]);
        let goad = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == racketeer)
            .and_then(|entry| entry.ability())
            .expect("the goad is on the stack");
        assert_eq!(goad.targets, vec![object(bears)]);
        assert_tts(
            &runner,
            Some(iroh),
            &[object(tar_pit), object(goblin)],
            "the mentor",
        );
        assert!(
            runner.state().resolving_stack_entry.is_none(),
            "no resolution is under way"
        );
        assert_not_first_ability_of_an_ability_resolution(&runner, "the mentor");
        let result = select(&mut runner, vec![object(tar_pit)]);
        assert!(
            matches!(result.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
            "same-controller: P0 orders them, got {:?} (stack {:?})",
            result.waiting_for,
            stack_reading(runner.state())
        );
        assert!(prompt_lists_exactly(
            runner.state(),
            &[Pick::Of(deepmuck), Pick::Of(tar_pit)]
        ));
        order_by_identity(&mut runner, &[Pick::Of(tar_pit)]);

        assert_completion_boundary(&runner, "same-controller");
        assert_triggered_stack(
            runner.state(),
            &[
                Pick::Of(racketeer),
                Pick::Of(iroh),
                Pick::Of(tar_pit),
                Pick::Of(deepmuck),
            ],
            "CR 603.3b: P0's chosen order",
        );
    }
}

/// CR 603.3b + CR 603.12 + CR 309.4c: whether the construction prompt open now
/// is for the first ability that the ability resolution under way put on the
/// stack: the resolving object is an ability and the pending entry is the only
/// ability on the stack put there after it.
fn first_ability_of_an_ability_resolution(state: &GameState) -> bool {
    let Some(carrier) = state
        .resolving_stack_entry
        .as_ref()
        .filter(|entry| matches!(entry.kind.class(), StackObjectClass::Ability(_)))
    else {
        return false;
    };
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind.class(), StackObjectClass::Ability(_)) && entry.id > carrier.id
        })
        .count()
        == 1
}

/// The kind of the object whose resolution is under way, if any.
fn resolving_class(state: &GameState) -> Option<StackObjectClass> {
    state
        .resolving_stack_entry
        .as_ref()
        .map(|entry| entry.kind.class())
}

/// The prompt open now is not the first ability an ability's resolution put on
/// the stack.
fn assert_not_first_ability_of_an_ability_resolution(runner: &GameRunner, what: &str) {
    assert!(
        !first_ability_of_an_ability_resolution(runner.state()),
        "{what}: not the first ability of an ability's resolution (resolving {:?}, stack {:?})",
        resolving_class(runner.state()),
        stack_reading(runner.state())
    );
}

/// Whether a queued context is marked as a member of the placement process
/// under way (read from its serialized form).
fn queued_under_way(ctx: &PendingTriggerContext) -> bool {
    serde_json::to_value(ctx)
        .expect("a queued context serializes")
        .get("placement_process")
        == Some(&serde_json::json!("UnderWay"))
}

/// The targets of each triggered stack entry, bottom→top.
fn triggered_targets(state: &GameState) -> Vec<Vec<TargetRef>> {
    state
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .map(|entry| {
            entry
                .ability()
                .map(|ability| ability.targets.clone())
                .unwrap_or_default()
        })
        .collect()
}

/// The targets of each triggered stack entry of `source`, bottom→top.
fn triggered_targets_of(state: &GameState, source: ObjectId) -> Vec<Vec<TargetRef>> {
    triggered_stack_sources(state)
        .into_iter()
        .zip(triggered_targets(state))
        .filter(|(entry_source, _)| *entry_source == source)
        .map(|(_, targets)| targets)
        .collect()
}

/// The bottom→top index, among the triggered stack entries, of the only entry
/// of `source` with exactly `targets`.
fn triggered_index(state: &GameState, source: ObjectId, targets: &[TargetRef]) -> usize {
    let found: Vec<usize> = triggered_stack_sources(state)
        .into_iter()
        .zip(triggered_targets(state))
        .enumerate()
        .filter(|(_, (entry_source, entry_targets))| {
            *entry_source == source && entry_targets.as_slice() == targets
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "one entry of #{} targeting {targets:?}: {:?}",
        source.0,
        stack_reading(state)
    );
    found[0]
}

fn set_dungeon(runner: &mut GameRunner, room: u8) {
    let progress = runner.state_mut().dungeon_progress.entry(P0).or_default();
    progress.current_dungeon = Some(engine::game::dungeon::DungeonId::LostMineOfPhandelver);
    progress.current_room = room;
}

fn colorless_pool(scenario: &mut GameScenario, amount: u64) {
    scenario.with_mana_pool(
        P0,
        (0..amount)
            .map(|i| {
                engine::types::mana::ManaUnit::new(
                    engine::types::mana::ManaType::Colorless,
                    ObjectId(9_900 + i),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
}

/// Pass priority until the resolution under way asks for a choice.
fn pass_until_resolution_choice(runner: &mut GameRunner) {
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass is accepted");
    }
    panic!("no resolution choice opened");
}

/// CR 309.4c: move the venture marker into Storeroom (Lost Mine of
/// Phandelver's room 3) at the open room choice, which the resolution of
/// `carrier`'s kind asks for.
fn choose_storeroom(runner: &mut GameRunner, carrier: StackObjectClass) -> ObjectId {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseDungeonRoom { .. }
        ),
        "reach guard: the venture asks for the room, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        resolving_class(runner.state()),
        Some(carrier),
        "the room is chosen while the venture resolves"
    );
    runner
        .act(GameAction::ChooseDungeonRoom { room_index: 3 })
        .expect("Storeroom follows Goblin Lair");
    let WaitingFor::TriggerTargetSelection { source_id, .. } = runner.state().waiting_for else {
        panic!(
            "Storeroom's ability asks for its target, got {:?}",
            runner.state().waiting_for
        );
    };
    source_id.expect("the room ability has a source")
}

/// The board of the Dungeon Map tests: P0's Hama Pashar, Ruin Seeker, Tar Pit
/// Warrior and Dungeon Map with three colorless mana, P1's Tar Pit Warrior when
/// asked, and Lost Mine of Phandelver at Goblin Lair. Dungeon Map's venture is
/// activated and resolving.
fn dungeon_map_venture(p1_tar_pit: bool) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hama = scenario
        .add_creature_from_oracle(P0, "Hama Pashar, Ruin Seeker", 2, 3, HAMA_PASHAR)
        .id();
    let tar_pit = scenario
        .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
        .id();
    let theirs = p1_tar_pit.then(|| {
        scenario
            .add_creature_from_oracle(P1, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
            .id()
    });
    let map = scenario
        .add_artifact_from_oracle(P0, "Dungeon Map", DUNGEON_MAP)
        .id();
    colorless_pool(&mut scenario, 3);
    let mut runner = scenario.build();
    set_dungeon(&mut runner, 1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: map,
            ability_index: 1,
            payment_mode: Default::default(),
        })
        .expect("Dungeon Map's venture is activated as a sorcery");
    pass_until_resolution_choice(&mut runner);
    assert!(
        runner
            .state()
            .resolving_stack_entry
            .as_ref()
            .is_some_and(|entry| entry.source_id == map),
        "Dungeon Map's ability is resolving"
    );
    (runner, hama, tar_pit, theirs)
}

/// CR 603.3b + CR 603.2d + CR 309.4c + CR 603.3d: Hama Pashar makes Storeroom's
/// ability trigger twice as Dungeon Map's ability resolves; both instances are
/// one placement process, each choosing its own target. Tar Pit Warrior's
/// trigger, which triggered as the first instance was put on the stack
/// targeting it, goes on after both, with no order to choose.
#[test]
fn doubled_room_instances_are_placed_before_what_the_first_instance_triggered() {
    let (mut runner, hama, tar_pit, _) = dungeon_map_venture(false);
    let room = choose_storeroom(
        &mut runner,
        StackObjectClass::Ability(StackAbilityKind::Activated),
    );
    assert_tts(
        &runner,
        Some(room),
        &[object(hama), object(tar_pit)],
        "the first instance",
    );
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    let queued = &runner.state().deferred_triggers;
    assert_eq!(queued.len(), 1, "the second instance is queued");
    assert_eq!(queued[0].pending.source_id, room);
    assert!(!queued_under_way(&queued[0]), "queued as a fresh context");
    round_trip_state(&mut runner);
    let result = select(&mut runner, vec![object(tar_pit)]);
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the second instance asks for its target, got {:?}",
        result.waiting_for
    );
    assert_tts(
        &runner,
        Some(room),
        &[object(hama), object(tar_pit)],
        "the second instance",
    );
    assert_not_first_ability_of_an_ability_resolution(&runner, "the second instance");
    assert_eq!(queued_matching(runner.state(), Pick::Of(tar_pit)), 1);
    select(&mut runner, vec![object(hama)]);

    assert_completion_boundary(&runner, "after the second instance");
    assert_triggered_stack_groups(
        runner.state(),
        &[&[Pick::Of(room), Pick::Of(room)], &[Pick::Of(tar_pit)]],
        "CR 603.3b: both instances, then Tar Pit",
    );
    // CR 603.3b: P0 may order the two instances; each keeps its own target.
    let room_targets = triggered_targets_of(runner.state(), room);
    assert_eq!(room_targets.len(), 2, "{room_targets:?}");
    assert!(room_targets.contains(&vec![object(tar_pit)]));
    assert!(room_targets.contains(&vec![object(hama)]));
    assert_eq!(
        triggered_targets_of(runner.state(), tar_pit),
        vec![Vec::<TargetRef>::new()]
    );
}

/// CR 603.3b + CR 101.4 + CR 603.2d + CR 603.3d: each of the two Storeroom
/// instances targets a Tar Pit Warrior of a different player. Both Tar Pit
/// triggers triggered while the instances were put on the stack, so they go on
/// together after both instances, the active player's first.
#[test]
fn second_room_instance_places_both_becomes_target_triggers_in_apnap_order() {
    let (mut runner, hama, ours, theirs) = dungeon_map_venture(true);
    let theirs = theirs.expect("P1 controls a Tar Pit Warrior");
    let room = choose_storeroom(
        &mut runner,
        StackObjectClass::Ability(StackAbilityKind::Activated),
    );
    let legal = [object(hama), object(ours), object(theirs)];
    assert_tts(&runner, Some(room), &legal, "the first instance");
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    assert_eq!(runner.state().deferred_triggers.len(), 1);
    assert!(!queued_under_way(&runner.state().deferred_triggers[0]));
    let result = select(&mut runner, vec![object(theirs)]);
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the second instance asks for its target, got {:?}",
        result.waiting_for
    );
    assert_tts(&runner, Some(room), &legal, "the second instance");
    let queued = &runner.state().deferred_triggers;
    assert_eq!(queued.len(), 1, "only P1's Tar Pit trigger is queued");
    assert_eq!(queued[0].pending.source_id, theirs);
    assert!(!queued_under_way(&queued[0]), "it stays a fresh context");
    assert_eq!(triggered_stack_sources(runner.state()), vec![room, room]);
    assert!(
        runner.state().resolving_stack_entry.is_some(),
        "Dungeon Map's ability is still resolving"
    );
    assert_not_first_ability_of_an_ability_resolution(&runner, "the second instance");
    round_trip_state(&mut runner);
    select(&mut runner, vec![object(ours)]);

    assert_completion_boundary(&runner, "after the second instance");
    assert_triggered_stack_groups(
        runner.state(),
        &[
            &[Pick::Of(room), Pick::Of(room)],
            &[Pick::Of(ours)],
            &[Pick::Of(theirs)],
        ],
        "CR 603.3b + CR 101.4: both instances, then P0's Tar Pit, then P1's",
    );
    // CR 603.3b: P0 may order the two instances; each keeps its own target.
    let room_targets = triggered_targets_of(runner.state(), room);
    assert_eq!(room_targets.len(), 2, "{room_targets:?}");
    assert!(room_targets.contains(&vec![object(theirs)]));
    assert!(room_targets.contains(&vec![object(ours)]));
}

/// The board of the Acererak tests: P0's Flaming Fist Officer and Tar Pit
/// Warrior, Hama Pashar, Ruin Seeker when asked, Acererak the Archlich cast at
/// no cost, Lost Mine of Phandelver at Goblin Lair (Tomb of Annihilation not
/// completed). Acererak's ETB returns it to its owner's hand and ventures.
fn acererak_venture(hama: bool) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let officer = scenario
        .add_creature_from_oracle(P0, "Flaming Fist Officer", 2, 2, FLAMING_FIST_OFFICER)
        .id();
    let tar_pit = scenario
        .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
        .id();
    let hama = hama.then(|| {
        scenario
            .add_creature_from_oracle(P0, "Hama Pashar, Ruin Seeker", 2, 3, HAMA_PASHAR)
            .id()
    });
    let acererak = scenario
        .add_creature_to_hand_from_oracle(P0, "Acererak the Archlich", 5, 5, ACERERAK_THE_ARCHLICH)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    set_dungeon(&mut runner, 1);
    runner.cast(acererak).commit();
    pass_until_resolution_choice(&mut runner);
    (runner, officer, tar_pit, hama)
}

/// CR 603.3b + CR 309.4c + CR 603.3d: Acererak's ETB returns it to hand
/// (Flaming Fist Officer triggers) and then ventures into Storeroom. The room
/// ability is placed with the Officer's trigger, and Tar Pit Warrior's trigger,
/// which triggered as the room ability was put on the stack targeting it, goes
/// on after both.
#[test]
fn room_ability_is_placed_with_an_earlier_trigger_of_its_resolution() {
    let (mut runner, officer, tar_pit, _) = acererak_venture(false);
    let room = choose_storeroom(
        &mut runner,
        StackObjectClass::Ability(StackAbilityKind::Triggered),
    );
    assert_tts(
        &runner,
        Some(room),
        &[object(officer), object(tar_pit)],
        "Storeroom",
    );
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    assert_eq!(runner.state().deferred_triggers.len(), 1);
    assert_eq!(queued_matching(runner.state(), Pick::Of(officer)), 1);
    select(&mut runner, vec![object(tar_pit)]);

    assert_completion_boundary(&runner, "after Storeroom's target");
    assert_triggered_stack_groups(
        runner.state(),
        &[&[Pick::Of(room), Pick::Of(officer)], &[Pick::Of(tar_pit)]],
        "CR 603.3b: the room and the Officer, then Tar Pit",
    );
}

/// CR 603.3b + CR 603.2d + CR 309.4c: with Hama Pashar, the second Storeroom
/// instance and Flaming Fist Officer's trigger are placed together, in the
/// order P0 chooses; Tar Pit Warrior's trigger goes on after all three.
#[test]
fn doubled_room_and_an_earlier_trigger_of_its_resolution_are_ordered_together() {
    let (mut runner, officer, tar_pit, hama) = acererak_venture(true);
    let hama = hama.expect("Hama Pashar is on the battlefield");
    let room = choose_storeroom(
        &mut runner,
        StackObjectClass::Ability(StackAbilityKind::Triggered),
    );
    let legal = [object(officer), object(tar_pit), object(hama)];
    assert_tts(&runner, Some(room), &legal, "the first instance");
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    assert_eq!(runner.state().deferred_triggers.len(), 2);
    assert_eq!(queued_matching(runner.state(), Pick::Of(officer)), 1);
    assert_eq!(queued_matching(runner.state(), Pick::Of(room)), 1);
    let result = select(&mut runner, vec![object(tar_pit)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
        "P0 orders the second instance and the Officer, got {:?}",
        result.waiting_for
    );
    assert!(prompt_lists_exactly(
        runner.state(),
        &[Pick::Of(officer), Pick::Of(room)]
    ));
    order_by_identity(&mut runner, &[Pick::Of(officer)]);
    assert_tts(&runner, Some(room), &legal, "the second instance");
    assert_not_first_ability_of_an_ability_resolution(&runner, "the second instance");
    assert_eq!(queued_matching(runner.state(), Pick::Of(tar_pit)), 1);
    select(&mut runner, vec![object(hama)]);

    assert_completion_boundary(&runner, "after the second instance");
    assert_triggered_stack_groups(
        runner.state(),
        &[
            &[Pick::Of(room), Pick::Of(officer), Pick::Of(room)],
            &[Pick::Of(tar_pit)],
        ],
        "CR 603.3b: both instances and the Officer, then Tar Pit",
    );
    let state = runner.state();
    triggered_index(state, room, &[object(tar_pit)]);
    assert!(
        triggered_index(state, officer, &[]) < triggered_index(state, room, &[object(hama)]),
        "CR 603.3b: P0's chosen order puts the Officer below the second instance: {:?}",
        stack_reading(state)
    );
}

/// Cast Bolg of the North at no cost, accept its sacrifice and sacrifice
/// `victim` (the legal choices are exactly `choices`); Bolg's reflexive
/// ability then asks for its target.
fn bolg_sacrificing(
    runner: &mut GameRunner,
    bolg: ObjectId,
    victim: ObjectId,
    choices: &[ObjectId],
) {
    runner.cast(bolg).commit();
    pass_until_resolution_choice(runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "Bolg's ETB offers the sacrifice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the sacrifice is accepted");
    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the sacrifice is chosen, got {:?}",
            runner.state().waiting_for
        );
    };
    let mut cards = cards;
    let mut want = choices.to_vec();
    cards.sort();
    want.sort();
    assert_eq!(cards, want, "the creatures Bolg may sacrifice");
    runner
        .act(GameAction::SelectCards {
            cards: vec![victim],
        })
        .expect("the sacrifice is legal");
    assert_eq!(
        resolving_class(runner.state()),
        Some(StackObjectClass::Ability(StackAbilityKind::Triggered)),
        "Bolg's ETB is resolving"
    );
}

/// The board of the Bolg tests: P0's Doomed Dissenter and Tar Pit Warrior, P1's
/// Grizzly Bears and, when asked, Mortician Beetle.
fn bolg_board(
    beetle: bool,
) -> (
    GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
    Option<ObjectId>,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dissenter = scenario
        .add_creature_from_oracle(P0, "Doomed Dissenter", 1, 1, DOOMED_DISSENTER)
        .id();
    let tar_pit = scenario
        .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let beetle = beetle.then(|| {
        scenario
            .add_creature_from_oracle(P1, "Mortician Beetle", 1, 1, MORTICIAN_BEETLE)
            .id()
    });
    let bolg = scenario
        .add_creature_to_hand_from_oracle(P0, "Bolg of the North", 5, 5, BOLG_OF_THE_NORTH)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    (scenario.build(), dissenter, tar_pit, bears, beetle, bolg)
}

/// CR 603.3b + CR 603.12 + CR 603.3d: Bolg's ETB sacrifices Doomed Dissenter;
/// Bolg's reflexive ability is placed with the Dissenter's dies trigger, and
/// Tar Pit Warrior's trigger, which triggered as the reflexive ability was put
/// on the stack targeting it, goes on after both, with no order to choose.
#[test]
fn reflexive_ability_is_placed_with_an_earlier_trigger_of_its_resolution() {
    let (mut runner, dissenter, tar_pit, bears, _, bolg) = bolg_board(false);
    bolg_sacrificing(&mut runner, bolg, dissenter, &[dissenter, tar_pit]);
    assert_tts(
        &runner,
        Some(bolg),
        &[object(tar_pit), object(bears)],
        "Bolg's reflexive ability",
    );
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    assert_eq!(runner.state().deferred_triggers.len(), 1);
    assert_eq!(queued_matching(runner.state(), Pick::Of(dissenter)), 1);
    select(&mut runner, vec![object(tar_pit)]);

    assert_completion_boundary(&runner, "after the reflexive target");
    assert_triggered_stack_groups(
        runner.state(),
        &[&[Pick::Of(bolg), Pick::Of(dissenter)], &[Pick::Of(tar_pit)]],
        "CR 603.3b: the reflexive ability and the Dissenter, then Tar Pit",
    );
}

/// CR 603.3b + CR 603.12 + CR 101.4: the same with P1's Mortician Beetle, which
/// triggered on the sacrifice: both players' earlier triggers, APNAP, go before
/// Tar Pit Warrior's trigger.
#[test]
fn reflexive_ability_and_earlier_triggers_of_both_players_precede_what_it_triggered() {
    let (mut runner, dissenter, tar_pit, bears, beetle, bolg) = bolg_board(true);
    let beetle = beetle.expect("P1 controls Mortician Beetle");
    bolg_sacrificing(&mut runner, bolg, dissenter, &[dissenter, tar_pit]);
    assert_tts(
        &runner,
        Some(bolg),
        &[object(tar_pit), object(bears), object(beetle)],
        "Bolg's reflexive ability",
    );
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    assert_eq!(runner.state().deferred_triggers.len(), 2);
    assert_eq!(queued_matching(runner.state(), Pick::Of(dissenter)), 1);
    assert_eq!(queued_matching(runner.state(), Pick::Of(beetle)), 1);
    select(&mut runner, vec![object(tar_pit)]);

    assert_completion_boundary(&runner, "after the reflexive target");
    assert_triggered_stack_groups(
        runner.state(),
        &[
            &[Pick::Of(bolg), Pick::Of(dissenter)],
            &[Pick::Of(beetle)],
            &[Pick::Of(tar_pit)],
        ],
        "CR 603.3b + CR 101.4: P0's two, then P1's, then Tar Pit",
    );
}

/// CR 603.3b + CR 101.4 + CR 603.12 + CR 603.3d: Bolg sacrifices Perilous Myr,
/// whose dies trigger is itself targeted. Bolg's reflexive ability targets P1's
/// Tar Pit Warrior and the Myr's trigger P0's: both Tar Pit triggers triggered
/// while that batch was put on the stack, so they go on together after it, the
/// active player's first.
#[test]
fn earlier_targeted_trigger_of_a_resolution_places_both_becomes_target_triggers_in_apnap_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let myr = scenario
        .add_creature_from_oracle(P0, "Perilous Myr", 1, 1, PERILOUS_MYR)
        .as_artifact_creature()
        .id();
    let ours = scenario
        .add_creature_from_oracle(P0, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
        .id();
    let theirs = scenario
        .add_creature_from_oracle(P1, "Tar Pit Warrior", 3, 4, TAR_PIT_WARRIOR)
        .id();
    let bolg = scenario
        .add_creature_to_hand_from_oracle(P0, "Bolg of the North", 5, 5, BOLG_OF_THE_NORTH)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    bolg_sacrificing(&mut runner, bolg, myr, &[myr, ours]);
    assert_tts(
        &runner,
        Some(bolg),
        &[object(ours), object(theirs)],
        "Bolg's reflexive ability",
    );
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    let queued = &runner.state().deferred_triggers;
    assert_eq!(queued.len(), 1, "the Myr's dies trigger is queued");
    assert_eq!(queued[0].pending.source_id, myr);
    let result = select(&mut runner, vec![object(theirs)]);
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "the Myr's trigger asks for its target, got {:?}",
        result.waiting_for
    );
    assert_tts(
        &runner,
        Some(myr),
        &[
            TargetRef::Player(P0),
            TargetRef::Player(P1),
            object(bolg),
            object(ours),
            object(theirs),
        ],
        "Perilous Myr's trigger",
    );
    let queued = &runner.state().deferred_triggers;
    assert_eq!(queued.len(), 1, "only P1's Tar Pit trigger is queued");
    assert_eq!(queued[0].pending.source_id, theirs);
    assert!(!queued_under_way(&queued[0]), "it stays a fresh context");
    let mut placed = triggered_stack_sources(runner.state());
    placed.sort();
    let mut batch = vec![bolg, myr];
    batch.sort();
    assert_eq!(placed, batch, "the reflexive ability and the Myr's trigger");
    assert!(
        runner.state().resolving_stack_entry.is_some(),
        "Bolg's ETB is still resolving"
    );
    assert_not_first_ability_of_an_ability_resolution(&runner, "the Myr's trigger");
    round_trip_state(&mut runner);
    select(&mut runner, vec![object(ours)]);

    assert_completion_boundary(&runner, "after the Myr's target");
    assert_triggered_stack_groups(
        runner.state(),
        &[
            &[Pick::Of(bolg), Pick::Of(myr)],
            &[Pick::Of(ours)],
            &[Pick::Of(theirs)],
        ],
        "CR 603.3b + CR 101.4: the reflexive ability and the Myr, then P0's Tar Pit, then P1's",
    );
    assert_eq!(
        triggered_targets_of(runner.state(), bolg),
        vec![vec![object(theirs)]]
    );
    assert_eq!(
        triggered_targets_of(runner.state(), myr),
        vec![vec![object(ours)]]
    );
}

/// The board of the Caesar tests: P0's Caesar, Legion's Emperor, Doomed
/// Dissenter and Deepmuck Desperado (one card in P0's library), P1's Mortician
/// Beetle when asked. Caesar attacks alone and "Whenever you attack" resolves;
/// P0 sacrifices Doomed Dissenter and the reflexive ability asks for its modes.
fn caesar_sacrificing_dissenter(
    beetle: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Plains");
    scenario.with_library_top(P1, &["Plains", "Plains", "Plains"]);
    let caesar = scenario
        .add_creature_from_oracle(P0, "Caesar, Legion's Emperor", 4, 4, CAESAR)
        .id();
    let dissenter = scenario
        .add_creature_from_oracle(P0, "Doomed Dissenter", 1, 1, DOOMED_DISSENTER)
        .id();
    let deepmuck = scenario
        .add_creature_from_oracle(P0, "Deepmuck Desperado", 2, 4, DEEPMUCK_DESPERADO)
        .id();
    let beetle = beetle.then(|| {
        scenario
            .add_creature_from_oracle(P1, "Mortician Beetle", 1, 1, MORTICIAN_BEETLE)
            .id()
    });
    let mut runner = scenario.build();
    attack(&mut runner, &[caesar]);
    pass_until_resolution_choice(&mut runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the sacrifice is accepted");
    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the sacrifice is chosen, got {:?}",
            runner.state().waiting_for
        );
    };
    let mut cards = cards;
    let mut want = vec![dissenter, deepmuck];
    cards.sort();
    want.sort();
    assert_eq!(cards, want, "the creatures Caesar's ability may sacrifice");
    runner
        .act(GameAction::SelectCards {
            cards: vec![dissenter],
        })
        .expect("the sacrifice is legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "the reflexive ability asks for its modes, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        resolving_class(runner.state()),
        Some(StackObjectClass::Ability(StackAbilityKind::Triggered)),
        "Caesar's attack trigger is resolving"
    );
    assert!(first_ability_of_an_ability_resolution(runner.state()));
    (runner, caesar, dissenter, deepmuck, beetle)
}

/// Choose Caesar's draw mode and damage mode; the damage mode's only legal
/// target is P1 (a crime, CR 700.13).
fn choose_draw_and_damage(runner: &mut GameRunner) {
    runner
        .act(GameAction::SelectModes {
            indices: vec![1, 2],
        })
        .expect("two modes are chosen");
}

/// CR 603.3b + CR 603.12 + CR 700.2b + CR 700.13: Caesar's reflexive ability
/// (modes chosen as it is put on the stack, its damage mode targeting P1) is
/// placed with Doomed Dissenter's dies trigger; Deepmuck Desperado's crime
/// trigger goes on after both, with no order to choose.
#[test]
fn modal_reflexive_ability_is_placed_with_an_earlier_trigger_of_its_resolution() {
    let (mut runner, caesar, dissenter, deepmuck, _) = caesar_sacrificing_dissenter(false);
    assert_eq!(runner.state().deferred_triggers.len(), 1);
    assert_eq!(queued_matching(runner.state(), Pick::Of(dissenter)), 1);
    choose_draw_and_damage(&mut runner);

    assert_completion_boundary(&runner, "after the modes");
    assert_triggered_stack_groups(
        runner.state(),
        &[
            &[Pick::Of(caesar), Pick::Of(dissenter)],
            &[Pick::Of(deepmuck)],
        ],
        "CR 603.3b: the reflexive ability and the Dissenter, then the crime trigger",
    );
}

/// CR 603.3b + CR 603.12 + CR 101.4 + CR 700.13: the same with P1's Mortician
/// Beetle: both players' earlier triggers go before the crime trigger.
#[test]
fn modal_reflexive_ability_and_earlier_triggers_of_both_players_precede_its_crime_trigger() {
    let (mut runner, caesar, dissenter, deepmuck, beetle) = caesar_sacrificing_dissenter(true);
    let beetle = beetle.expect("P1 controls Mortician Beetle");
    assert_eq!(runner.state().deferred_triggers.len(), 2);
    assert_eq!(queued_matching(runner.state(), Pick::Of(dissenter)), 1);
    assert_eq!(queued_matching(runner.state(), Pick::Of(beetle)), 1);
    choose_draw_and_damage(&mut runner);

    assert_completion_boundary(&runner, "after the modes");
    assert_triggered_stack_groups(
        runner.state(),
        &[
            &[Pick::Of(caesar), Pick::Of(dissenter)],
            &[Pick::Of(beetle)],
            &[Pick::Of(deepmuck)],
        ],
        "CR 603.3b + CR 101.4: P0's two, then P1's, then the crime trigger",
    );
}

/// The board of the Fates' Reversal tests: P0's Hama Pashar, Ruin Seeker and
/// Grizzly Bears, Planar Void or Desecrated Tomb (with a Grizzly Bears card in
/// P0's graveyard, the spell's target), Lost Mine of Phandelver at Goblin Lair.
/// Fates' Reversal is cast at no cost and its venture asks for the room.
fn fates_reversal_venture(void: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hama = scenario
        .add_creature_from_oracle(P0, "Hama Pashar, Ruin Seeker", 2, 3, HAMA_PASHAR)
        .id();
    let other = if void {
        scenario
            .add_enchantment_from_oracle(P0, "Planar Void", PLANAR_VOID)
            .id()
    } else {
        scenario
            .add_artifact_from_oracle(P0, "Desecrated Tomb", DESECRATED_TOMB)
            .id()
    };
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let graveyard_bears = (!void).then(|| {
        scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .id()
    });
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fates' Reversal", false, FATES_REVERSAL)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    set_dungeon(&mut runner, 1);
    match graveyard_bears {
        Some(card) => runner.cast(spell).target_objects(&[card]).commit(),
        None => runner.cast(spell).commit(),
    };
    pass_until_resolution_choice(&mut runner);
    (runner, hama, other, bears)
}

/// CR 603.3b + CR 608.2n + CR 603.2d: while Fates' Reversal (a sorcery)
/// resolves, Hama Pashar doubles Storeroom's ability; Planar Void triggers on
/// the sorcery's own move to the graveyard, the final part of its resolution.
/// The second instance and Planar Void's trigger are ordered together.
#[test]
fn spell_resolution_room_instance_is_ordered_with_a_trigger_of_the_spell_going_to_the_graveyard() {
    let (mut runner, hama, void, bears) = fates_reversal_venture(true);
    let room = choose_storeroom(&mut runner, StackObjectClass::Spell);
    assert_tts(
        &runner,
        Some(room),
        &[object(hama), object(bears)],
        "the first instance",
    );
    assert!(!first_ability_of_an_ability_resolution(runner.state()));
    let result = select(&mut runner, vec![object(bears)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
        "P0 orders the second instance and Planar Void, got {:?}",
        result.waiting_for
    );
    assert!(prompt_lists_exactly(
        runner.state(),
        &[Pick::Of(room), Pick::Of(void)]
    ));
    order_by_identity(&mut runner, &[Pick::Of(void)]);
    assert_tts(
        &runner,
        Some(room),
        &[object(hama), object(bears)],
        "the second instance",
    );
    select(&mut runner, vec![object(hama)]);

    assert_completion_boundary(&runner, "after the second instance");
    assert_triggered_stack_groups(
        runner.state(),
        &[&[Pick::Of(room), Pick::Of(void), Pick::Of(room)]],
        "CR 603.3b: both instances and Planar Void",
    );
    let state = runner.state();
    triggered_index(state, room, &[object(bears)]);
    assert!(
        triggered_index(state, void, &[]) < triggered_index(state, room, &[object(hama)]),
        "CR 603.3b: P0's chosen order puts Planar Void below the second instance: {:?}",
        stack_reading(state)
    );
}

/// CR 603.3b + CR 603.2d: while Fates' Reversal resolves, returning the
/// graveyard Grizzly Bears triggers Desecrated Tomb; the second Storeroom
/// instance and the Tomb's trigger are ordered together.
#[test]
fn spell_resolution_room_instance_is_ordered_with_an_earlier_trigger_of_the_spell() {
    let (mut runner, hama, tomb, bears) = fates_reversal_venture(false);
    let room = choose_storeroom(&mut runner, StackObjectClass::Spell);
    assert_tts(
        &runner,
        Some(room),
        &[object(hama), object(bears)],
        "the first instance",
    );
    assert!(!first_ability_of_an_ability_resolution(runner.state()));
    assert_eq!(queued_matching(runner.state(), Pick::Of(tomb)), 1);
    let result = select(&mut runner, vec![object(bears)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
        "P0 orders the second instance and the Tomb, got {:?}",
        result.waiting_for
    );
    assert!(prompt_lists_exactly(
        runner.state(),
        &[Pick::Of(room), Pick::Of(tomb)]
    ));
    order_by_identity(&mut runner, &[Pick::Of(tomb)]);
    assert_tts(
        &runner,
        Some(room),
        &[object(hama), object(bears)],
        "the second instance",
    );
    select(&mut runner, vec![object(hama)]);

    assert_completion_boundary(&runner, "after the second instance");
    assert_triggered_stack_groups(
        runner.state(),
        &[&[Pick::Of(room), Pick::Of(tomb), Pick::Of(room)]],
        "CR 603.3b: both instances and the Tomb",
    );
    let state = runner.state();
    triggered_index(state, room, &[object(bears)]);
    assert!(
        triggered_index(state, tomb, &[]) < triggered_index(state, room, &[object(hama)]),
        "CR 603.3b: P0's chosen order puts the Tomb below the second instance: {:?}",
        stack_reading(state)
    );
}

/// CR 603.3b + CR 603.12 + CR 608.2n: while Back for More (an instant) resolves,
/// returning the graveyard Grizzly Bears triggers Desecrated Tomb, and Planar
/// Void triggers on the instant's own move to the graveyard; the two are
/// ordered together above the reflexive fight ability.
#[test]
fn spell_resolution_reflexive_batch_orders_an_earlier_and_a_later_trigger_together() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomb = scenario
        .add_artifact_from_oracle(P0, "Desecrated Tomb", DESECRATED_TOMB)
        .id();
    let void = scenario
        .add_enchantment_from_oracle(P0, "Planar Void", PLANAR_VOID)
        .id();
    let card = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let bear_1 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let bear_2 = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Back for More", true, BACK_FOR_MORE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[card]).commit();
    pass_until_resolution_choice(&mut runner);
    assert_tts(
        &runner,
        Some(spell),
        &[object(bear_1), object(bear_2)],
        "the reflexive fight",
    );
    assert_eq!(
        resolving_class(runner.state()),
        Some(StackObjectClass::Spell)
    );
    assert!(!first_ability_of_an_ability_resolution(runner.state()));
    assert_eq!(queued_matching(runner.state(), Pick::Of(tomb)), 1);
    let result = select(&mut runner, vec![object(bear_1)]);
    assert!(
        matches!(result.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
        "P0 orders the Tomb and Planar Void, got {:?}",
        result.waiting_for
    );
    assert!(prompt_lists_exactly(
        runner.state(),
        &[Pick::Of(tomb), Pick::Of(void)]
    ));
    order_by_identity(&mut runner, &[Pick::Of(tomb)]);

    assert_completion_boundary(&runner, "after the order");
    assert_triggered_stack_groups(
        runner.state(),
        &[&[Pick::Of(spell), Pick::Of(tomb), Pick::Of(void)]],
        "CR 603.3b: the reflexive fight, the Tomb and Planar Void",
    );
    let state = runner.state();
    triggered_index(state, spell, &[object(bear_1)]);
    assert!(
        triggered_index(state, tomb, &[]) < triggered_index(state, void, &[]),
        "CR 603.3b: P0's chosen order puts the Tomb below Planar Void: {:?}",
        stack_reading(state)
    );
}
