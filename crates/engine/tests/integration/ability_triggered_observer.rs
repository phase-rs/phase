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

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::visibility::{
    filter_events_for_viewer, filter_state_for_viewer, HIDDEN_CARD_NAME,
};
use engine::types::ability::{ControllerRef, TargetFilter, TriggerDefinition, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card::PrintedCardRef;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, TriggeredAbilityOrigin};
use engine::types::game_state::{
    AutoMayChoice, GameState, MayTriggerAutoChoiceKey, MayTriggerAutoChoiceSelector,
    MayTriggerOrigin, StackEntryKind, WaitingFor,
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
                cause: TriggerCause::EntersBattlefield {
                    core_types: vec![],
                    entrant: engine::types::statics::CausingObject::Any,
                },
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
