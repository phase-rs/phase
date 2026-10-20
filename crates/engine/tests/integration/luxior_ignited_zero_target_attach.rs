//! CR 115.6 zero-target Attach: the host frame's "up to one target creature"
//! and the declined-host no-op.
//!
//! The prepared cards below are the verbatim corpus Oracle text (MTGJSON
//! `AtomicCards` strings), driven through `GameScenario`/`GameRunner` so every
//! assertion is a resolution delta, not a re-read of the parse.
//!
//! Base behavior that flips:
//! * `Luxior, Ignited` `[+1]` prints "Attach this Equipment to up to one target
//!   creature you control." — before the fix the clause carried no zero-target
//!   authority, so activating it with no creature on the battlefield was
//!   REJECTED at announcement (CR 601.2c: a mandatory target set with no legal
//!   target cannot be announced).
//! * `The Aetherspark` `[+1]` and `Sting, Bilbo's Sword`'s enters trigger have the
//!   same printed host; `Sting` additionally showed the lost counter rider.
//! * `Assassin Gauntlet` reaches the resolver with a declined host (printed
//!   "up to one target") and previously errored with `MissingParam`; the
//!   declined host is now a resolution no-op (CR 115.6 + CR 608.2c).
//! * `Hunter's Bow` and `Novel Nunchaku` print a MANDATORY host; with no legal
//!   creature the enters trigger is removed (CR 603.3d), so no Attach resolves.
//!   (The trigger-wide `has_up_to` stamp no longer marks an Attach host optional.)
//!
//! D4 (pre-existing, labelled, NOT fixed here): `Serene Remembrance`'s "from a
//! single graveyard" constraint parses as a target-set constraint on the
//! SelfRef head, but the announced multi-target set is on the sub-ability, and a
//! mixed-graveyard pair is accepted at announcement. The probe was measured and
//! failed, so only the same-graveyard positive is kept.

use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const LUXIOR_ORACLE: &str = "Equipped creature gets +1/+1 for each counter on Luxior, Ignited.\n[+1]: Attach this Equipment to up to one target creature you control.\n[\u{2212}2]: Equipped creature gets +2/+2 and gains double strike until end of turn.";

const AETHERSPARK_ORACLE: &str = "As long as The Aetherspark is attached to a creature, The Aetherspark can't be attacked and has \"Whenever equipped creature deals combat damage during your turn, put that many loyalty counters on The Aetherspark.\"\n[+1]: Attach The Aetherspark to up to one target creature you control. Put a +1/+1 counter on that creature.\n[\u{2212}5]: Draw two cards.\n[\u{2212}10]: Add ten mana of any one color.";

const STING_ORACLE: &str = "Flash\nWhen Sting enters, put a hone counter on Sting for each creature target opponent controls. Attach Sting to up to one target creature you control. (Each hone counter on an Equipment grants +1/+0 to equipped creature.)\nEquip {3}";

const ASSASSIN_GAUNTLET_ORACLE: &str = "When this Equipment enters, attach it to up to one target creature you control. Tap all creatures target opponent controls.\nEquipped creature gets +1/+1 and has \"Whenever this creature deals combat damage to a player, draw a card, then discard a card.\"\nEquip {2}";

const HUNTERS_BOW_ORACLE: &str = "When this Equipment enters, attach it to target creature you control. That creature deals damage equal to its power to up to one target creature you don't control.\nEquipped creature has reach and ward {2}.\nEquip {1}";

const NOVEL_NUNCHAKU_ORACLE: &str = "When this Equipment enters, attach it to target creature you control. When you do, equipped creature fights up to one target creature an opponent controls. (Each deals damage equal to its power to the other.)\nEquipped creature gets +1/+1 and has trample.\nEquip {3} ({3}: Attach to target creature you control. Equip only as a sorcery.)";

const SERENE_REMEMBRANCE_ORACLE: &str = "Shuffle Serene Remembrance and up to three target cards from a single graveyard into their owners' libraries.";

const STOLEN_GOODIES_ORACLE: &str =
    "Distribute three +1/+1 counters among any number of target creatures you control.";

/// Enter the precombat main phase with the active player holding priority.
fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn counters_of(outcome: &Outcome, id: ObjectId, counter: &CounterType) -> u32 {
    outcome
        .state()
        .objects
        .get(&id)
        .and_then(|object| object.counters.get(counter))
        .copied()
        .unwrap_or(0)
}

fn attached_object(outcome: &Outcome, id: ObjectId) -> Option<ObjectId> {
    outcome
        .state()
        .objects
        .get(&id)
        .and_then(|object| object.attached_to.as_ref())
        .and_then(|target| target.as_object())
}

fn attach_effect_resolved_in(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Attach,
                ..
            }
        )
    })
}

fn attach_effect_resolved(outcome: &Outcome) -> bool {
    attach_effect_resolved_in(outcome.events())
}

/// Build the Luxior battlefield state: the Equipment Planeswalker with loyalty
/// counters (its `[+1]` is ability index 0).
fn luxior_runner(creatures: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let luxior = scenario
        .add_planeswalker_from_oracle(P0, "Luxior, Ignited", "Luxior", 4, LUXIOR_ORACLE)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut mine = Vec::new();
    for index in 0..creatures {
        mine.push(
            scenario
                .add_creature(P0, &format!("Trained {index}"), 1, 1)
                .id(),
        );
    }
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, luxior, mine)
}

// ---------------------------------------------------------------------------
// Luxior, Ignited
// ---------------------------------------------------------------------------

#[test]
fn luxior_plus_one_activates_and_resolves_with_no_creatures() {
    let (mut runner, luxior, _) = luxior_runner(0);
    let outcome = runner.activate(luxior, 0).resolve();

    // CR 115.6: zero targets is a legal announcement, and the ability still
    // resolves; CR 606.4 + CR 601.2c: the loyalty cost is paid either way.
    assert_eq!(
        counters_of(&outcome, luxior, &CounterType::Loyalty),
        5,
        "CR 606.4: the [+1] cost puts one loyalty counter on"
    );
    assert_eq!(
        attached_object(&outcome, luxior),
        None,
        "with no chosen host the Equipment stays unattached"
    );
    assert!(
        attach_effect_resolved(&outcome),
        "the Attach instruction must resolve as a no-op (EffectResolved): {:?}",
        outcome.events()
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn luxior_plus_one_declined_host_leaves_it_unattached() {
    // Reach-guard (same test): the identical state WITH a declared host attaches.
    let (mut chosen_runner, chosen_luxior, chosen_creatures) = luxior_runner(1);
    let chosen = chosen_runner
        .activate(chosen_luxior, 0)
        .target_object(chosen_creatures[0])
        .resolve();
    assert_eq!(
        attached_object(&chosen, chosen_luxior),
        Some(chosen_creatures[0]),
        "positive reach-guard: a chosen host is attached"
    );

    // Declined: a legal host exists but the controller chooses none.
    let (mut runner, luxior, _) = luxior_runner(1);
    let declined = runner.activate(luxior, 0).resolve();
    assert_eq!(
        attached_object(&declined, luxior),
        None,
        "declining the optional host resolves the Attach unattached"
    );
    assert!(attach_effect_resolved(&declined));
    assert_eq!(counters_of(&declined, luxior, &CounterType::Loyalty), 5);
}

#[test]
fn luxior_plus_one_attaches_when_a_creature_is_chosen() {
    let (mut runner, luxior, creatures) = luxior_runner(1);
    let outcome = runner
        .activate(luxior, 0)
        .target_object(creatures[0])
        .resolve();
    assert_eq!(attached_object(&outcome, luxior), Some(creatures[0]));
    assert_eq!(counters_of(&outcome, luxior, &CounterType::Loyalty), 5);
}

// ---------------------------------------------------------------------------
// The Aetherspark — optional host plus its counter rider
// ---------------------------------------------------------------------------

#[test]
fn aetherspark_plus_one_with_no_creature_puts_no_counter() {
    // Reach-guard (same test): a chosen host receives the +1/+1 counter.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spark = scenario
        .add_planeswalker_from_oracle(P0, "The Aetherspark", "Aetherspark", 4, AETHERSPARK_ORACLE)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let mine = scenario.add_creature(P0, "Chosen Host", 2, 2).id();
    let mut chosen_runner = scenario.build();
    grant_priority(&mut chosen_runner, P0);
    let chosen = chosen_runner
        .activate(spark, 0)
        .target_object(mine)
        .resolve();
    assert_eq!(attached_object(&chosen, spark), Some(mine));
    assert_eq!(
        counters_of(&chosen, mine, &CounterType::Plus1Plus1),
        1,
        "the chosen host gets the counter rider"
    );

    // Declined: the only creature is the opponent's, so the host slot is empty
    // and the rider has no object to affect.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spark = scenario
        .add_planeswalker_from_oracle(P0, "The Aetherspark", "Aetherspark", 4, AETHERSPARK_ORACLE)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let theirs = scenario.add_creature(P1, "Bystander", 1, 1).id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let declined = runner.activate(spark, 0).resolve();
    assert_eq!(attached_object(&declined, spark), None);
    assert_eq!(
        counters_of(&declined, theirs, &CounterType::Plus1Plus1),
        0,
        "an ineligible creature receives no counter"
    );
    assert!(attach_effect_resolved(&declined));
}

// ---------------------------------------------------------------------------
// Sting, Bilbo's Sword — ETB trigger whose SUB-clause host is optional
// ---------------------------------------------------------------------------

#[test]
fn sting_enters_with_no_legal_creature_and_attaches_to_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sting = scenario
        .add_artifact_to_hand_from_oracle(P0, "Sting, Bilbo's Sword", STING_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    // The trigger's "for each creature target opponent controls" instruction
    // declares its opponent as a target even though this run declines the host.
    let outcome = runner.cast(sting).target_player(P1).resolve();

    // CR 603.3d + CR 115.6: the trigger's optional host is declined and the
    // trigger still resolves. The hone-counter instruction is a separate
    // mandatory step, so its EffectResolved must appear BEFORE the Attach one
    // (CR 608.2c: instructions are followed in order).
    let first_effect_resolution = |kind: EffectKind| {
        outcome.events().iter().position(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: resolved,
                    ..
                } if *resolved == kind
            )
        })
    };
    let counter_step =
        first_effect_resolution(EffectKind::PutCounter).expect("hone-counter step must resolve");
    let attach_step = first_effect_resolution(EffectKind::Attach)
        .expect("the declined Attach must still resolve as a no-op");
    assert!(
        counter_step < attach_step,
        "CR 608.2c: the hone-counter step ({counter_step}) must precede the declined Attach \
         ({attach_step}): {:?}",
        outcome.events()
    );
    assert_eq!(attached_object(&outcome, sting), None);
    assert!(attach_effect_resolved(&outcome));

    // D5 (pre-existing, labelled, NOT fixed here): Sting's Attach is the SECOND
    // sentence of the trigger, so it resolves as a chain child. The parent->child
    // hand-off (`effects::apply_parent_chain_context`) replaces the child's
    // context wholesale and with it the node-local `attach_target_bindings`, so a
    // CHOSEN host is lost before resolution. A chosen-host positive therefore
    // cannot attach even at base (it errored `MissingParam`); the class reach
    // guard for "a chosen host does attach" is the trigger-root Attach row
    // (`assassin_gauntlet_etb_declined_host_stays_unattached`'s positive half),
    // not this card. The discriminating assertion above is the trigger
    // RESOLUTION: at base a mandatory host slot with a legal host was announced,
    // but the declined set removed the trigger, so no PutCounter ever resolved.
}

#[test]
fn sting_enters_and_attaches_to_a_chosen_host() {
    // A chosen host is announced as an explicit object target; the resolver
    // consumes that slot, so the Attach lands (the node-local host BINDING is not
    // required for this path — see the D5 note in the report).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sting = scenario
        .add_artifact_to_hand_from_oracle(P0, "Sting, Bilbo's Sword", STING_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mine = scenario.add_creature(P0, "Willing Host", 2, 2).id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let chosen = runner
        .cast(sting)
        .target_object(mine)
        .target_player(P1)
        .resolve();
    assert_eq!(attached_object(&chosen, sting), Some(mine));
    assert!(attach_effect_resolved(&chosen));
}

// ---------------------------------------------------------------------------
// Both operands printed "target": the chosen ATTACHMENT is not host evidence
// ---------------------------------------------------------------------------
//
// CR 115.1a/c/d + CR 115.6 + CR 601.2c + CR 602.2b + CR 603.3d + CR 608.2c: "attach target Equipment you
// control to up to one target creature you control" prints "target" for BOTH
// roles, so the chosen Equipment sits in the announced target list beside the
// (declinable) host. The declined-host no-op must key on HOST-slot occupancy:
// matching the host filter, not "any announced object target", or choosing the
// Equipment and declining the host would fall through to a battlefield scan and
// auto-attach to an unchosen creature.

const IRON_HILLS_STALWART_ORACLE: &str = "Reach, trample\nWhen this creature enters, attach target Equipment you control to up to one target creature you control.";

const BLACKSMITHS_TALENT_ORACLE: &str = "(Gain the next level as a sorcery to add its ability.)\nWhen this Class enters, create a colorless Equipment artifact token named Sword with \"Equipped creature gets +1/+1\" and equip {2}.\n{2}{R}: Level 2\nAt the beginning of combat on your turn, attach target Equipment you control to up to one target creature you control.\n{3}{R}: Level 3\nDuring your turn, equipped creatures you control have double strike and haste.";

/// An Equipment permanent under P0's control, already on the battlefield.
fn battlefield_equipment(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, name, "Equipped creature gets +1/+0.")
        .with_subtypes(vec!["Equipment"])
        .id()
}

/// Drive to the beginning-of-combat Attach trigger's announcement prompt,
/// accumulating the events emitted on the way.
fn advance_to_attach_trigger_prompt(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("priority must be passable")
                        .events,
                );
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                events.extend(
                    runner
                        .act(GameAction::OrderTriggers {
                            order: (0..triggers.len()).collect(),
                        })
                        .expect("trigger ordering must be accepted")
                        .events,
                );
            }
            other => panic!("unexpected window before the Attach target prompt: {other:?}"),
        }
    }
    panic!("the Attach trigger's announcement prompt was not reached");
}

/// Answer the trigger's two printed Attach roles at announcement: the mandatory
/// Equipment slot first, then the optional host slot (chosen or declined).
fn answer_attach_trigger_prompt(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    equipment: ObjectId,
    host: Option<ObjectId>,
) {
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("expected the Attach announcement prompt");
    };
    let attachment_slot = &target_slots[selection.current_slot];
    assert!(
        attachment_slot
            .legal_targets
            .contains(&TargetRef::Object(equipment)),
        "the printed Equipment target must open the announcement, got {:?}",
        attachment_slot.legal_targets
    );
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(equipment)),
            })
            .expect("the Equipment target must be accepted")
            .events,
    );

    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("expected the host slot after the Equipment target");
    };
    let host_slot = &target_slots[selection.current_slot];
    assert!(
        host_slot.optional,
        "the host is \"up to one target creature\", so its slot must be declinable"
    );
    if let Some(host) = host {
        assert!(
            host_slot.legal_targets.contains(&TargetRef::Object(host)),
            "the chosen host must be a legal target, got {:?}",
            host_slot.legal_targets
        );
    }
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: host.map(TargetRef::Object),
            })
            .expect("the host slot must accept the chosen host or a decline")
            .events,
    );
}

/// Pass priority until the pending Attach trigger has fully resolved.
fn finish_pending_trigger(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..40 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("priority must be passable")
                        .events,
                );
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                events.extend(
                    runner
                        .act(GameAction::OrderTriggers {
                            order: (0..triggers.len()).collect(),
                        })
                        .expect("trigger ordering must be accepted")
                        .events,
                );
            }
            other => panic!("unexpected window while the Attach trigger resolves: {other:?}"),
        }
    }
    panic!("the Attach trigger did not finish resolving");
}

#[test]
fn iron_hills_stalwart_etb_declined_host_keeps_the_chosen_equipment_unattached() {
    // Reach-guard (same test): declaring BOTH printed targets attaches to the
    // chosen host and leaves the creature's own body alone.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stalwart = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Iron Hills Stalwart",
            4,
            5,
            IRON_HILLS_STALWART_ORACLE,
        )
        .id();
    let equipment = battlefield_equipment(&mut scenario, "Rusty Sword");
    let host = scenario.add_creature(P0, "Willing Host", 2, 2).id();
    let mut chosen_runner = scenario.build();
    grant_priority(&mut chosen_runner, P0);
    let chosen = chosen_runner
        .cast(stalwart)
        .target_object(equipment)
        .target_object(host)
        .resolve();
    assert_eq!(
        attached_object(&chosen, equipment),
        Some(host),
        "positive reach-guard: with both printed targets declared the Equipment attaches to the chosen host"
    );

    // Declined: the Equipment is the mandatory printed target; the host is
    // "up to one target creature" and the controller chooses none.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stalwart = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Iron Hills Stalwart",
            4,
            5,
            IRON_HILLS_STALWART_ORACLE,
        )
        .id();
    let equipment = battlefield_equipment(&mut scenario, "Rusty Sword");
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let declined = runner.cast(stalwart).target_object(equipment).resolve();
    assert_eq!(
        attached_object(&declined, equipment),
        None,
        "choosing the Equipment is not host evidence: a declined host must leave it unattached \
         instead of auto-attaching to the first creature the battlefield scan finds"
    );
    assert!(
        attach_effect_resolved(&declined),
        "the Attach instruction still resolves as a no-op: {:?}",
        declined.events()
    );
    assert!(matches!(
        declined.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// Blacksmith's Talent at level 2: the beginning-of-combat trigger prints both
/// roles as targets. The trigger fires at BeginCombat and announces through the
/// `TriggerTargetSelection` prompt, which is driven directly here.
fn blacksmiths_talent_at_level_two() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let talent = scenario
        .add_enchantment_from_oracle(P0, "Blacksmith's Talent", BLACKSMITHS_TALENT_ORACLE)
        .with_subtypes(vec!["Class"])
        .id();
    let equipment = battlefield_equipment(&mut scenario, "Sword");
    let host = scenario.add_creature(P0, "Willing Host", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&talent)
        .expect("the Class permanent exists")
        .class_level = Some(2);
    (runner, talent, equipment, host)
}

#[test]
fn blacksmiths_talent_level_two_declined_host_keeps_the_chosen_equipment_unattached() {
    // Reach-guard (same test): choosing both printed targets attaches.
    let (mut chosen_runner, _, chosen_equipment, chosen_host) = blacksmiths_talent_at_level_two();
    let mut chosen_events = Vec::new();
    chosen_runner.advance_to_phase(Phase::BeginCombat);
    advance_to_attach_trigger_prompt(&mut chosen_runner, &mut chosen_events);
    answer_attach_trigger_prompt(
        &mut chosen_runner,
        &mut chosen_events,
        chosen_equipment,
        Some(chosen_host),
    );
    finish_pending_trigger(&mut chosen_runner, &mut chosen_events);
    assert_eq!(
        attached_object_by_state(&chosen_runner, chosen_equipment),
        Some(chosen_host),
        "positive reach-guard: the chosen host receives the chosen Equipment"
    );

    // Declined: the Equipment is chosen, the host is not.
    let (mut runner, _, equipment, host) = blacksmiths_talent_at_level_two();
    let mut events = Vec::new();
    runner.advance_to_phase(Phase::BeginCombat);
    advance_to_attach_trigger_prompt(&mut runner, &mut events);
    answer_attach_trigger_prompt(&mut runner, &mut events, equipment, None);
    finish_pending_trigger(&mut runner, &mut events);
    assert_eq!(
        attached_object_by_state(&runner, equipment),
        None,
        "declining the optional host must not auto-attach the chosen Equipment to the \
         battlefield scan's first legal creature (here the reach-guard's own host)"
    );
    assert!(
        attach_effect_resolved_in(&events),
        "the declined Attach must still resolve as a no-op: {events:?}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the trigger must finish at a priority window, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().objects[&host].attachments.is_empty(),
        "no creature may receive the declined attachment, got {:?}",
        runner.state().objects[&host].attachments
    );
}

fn attached_object_by_state(runner: &GameRunner, id: ObjectId) -> Option<ObjectId> {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|object| object.attached_to.as_ref())
        .and_then(|target| target.as_object())
}

// ---------------------------------------------------------------------------
// No-legal-host and declined-host regressions for the Attach host class
// ---------------------------------------------------------------------------

/// Cast an Equipment from hand and drive its enters trigger with no host on the
/// board (an optional host is declined; a mandatory host removes the trigger per
/// CR 603.3d).
fn cast_equipment_declined(name: &str, oracle: &str) -> Outcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let equipment = scenario
        .add_artifact_to_hand_from_oracle(P0, name, oracle)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(equipment).resolve()
}

#[test]
fn assassin_gauntlet_etb_declined_host_stays_unattached() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gauntlet = scenario
        .add_artifact_to_hand_from_oracle(P0, "Assassin Gauntlet", ASSASSIN_GAUNTLET_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    // A legal host exists; the controller still chooses none (CR 115.6).
    let _legal_host = scenario.add_creature(P0, "Declined Host", 2, 2).id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    // The trigger's second printed sentence still needs its own target opponent,
    // declared separately from the declined object target.
    let declined = runner.cast(gauntlet).target_player(P1).resolve();
    assert_eq!(attached_object(&declined, gauntlet), None);
    assert!(
        attach_effect_resolved(&declined),
        "the declined Attach must complete, not error: {:?}",
        declined.events()
    );

    // Reach-guard: the same cast with a declared host attaches.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gauntlet = scenario
        .add_artifact_to_hand_from_oracle(P0, "Assassin Gauntlet", ASSASSIN_GAUNTLET_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mine = scenario.add_creature(P0, "Chosen Host", 2, 2).id();
    let mut chosen_runner = scenario.build();
    grant_priority(&mut chosen_runner, P0);
    let chosen = chosen_runner
        .cast(gauntlet)
        .target_object(mine)
        .target_player(P1)
        .resolve();
    assert_eq!(attached_object(&chosen, gauntlet), Some(mine));
}

#[test]
fn hunters_bow_without_a_legal_host_removes_the_trigger() {
    // CR 115.6 + CR 601.2c + CR 603.3d: the host is mandatory, and this board has
    // no creature it could target, so the enters trigger is removed rather than
    // resolving as a declined/no-op Attach.
    let outcome = cast_equipment_declined("Hunter's Bow", HUNTERS_BOW_ORACLE);
    let unequipped = outcome
        .state()
        .objects
        .values()
        .filter(|object| object.name == "Hunter's Bow")
        .all(|object| object.attached_to.is_none());
    assert!(
        unequipped,
        "the Equipment must stay unattached when the trigger is removed"
    );
    assert!(
        !attach_effect_resolved(&outcome),
        "no Attach instruction may resolve: {:?}",
        outcome.events()
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));

    // Reach-guard: with a declared host the equipment attaches.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bow = scenario
        .add_artifact_to_hand_from_oracle(P0, "Hunter's Bow", HUNTERS_BOW_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mine = scenario.add_creature(P0, "Chosen Host", 3, 3).id();
    let mut chosen_runner = scenario.build();
    grant_priority(&mut chosen_runner, P0);
    let chosen = chosen_runner.cast(bow).target_object(mine).resolve();
    assert_eq!(attached_object(&chosen, bow), Some(mine));
}
#[test]
fn novel_nunchaku_without_a_legal_host_removes_the_trigger() {
    // CR 115.6 + CR 601.2c + CR 603.3d: mandatory host, no legal creature on the
    // board, so the enters trigger is removed; no Attach resolves and the "When
    // you do" reflexive fight never triggers.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nunchaku = scenario
        .add_artifact_to_hand_from_oracle(P0, "Novel Nunchaku", NOVEL_NUNCHAKU_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let declined = runner.cast(nunchaku).resolve();
    assert_eq!(attached_object(&declined, nunchaku), None);
    assert!(
        !attach_effect_resolved(&declined),
        "no Attach instruction may resolve: {:?}",
        declined.events()
    );
    assert!(
        !declined.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Fight,
                ..
            }
        )),
        "no attach means no reflexive fight: {:?}",
        declined.events()
    );
    assert!(matches!(
        declined.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));

    // Reach-guard: with a declared host the equipment attaches and the reflexive
    // trigger is on the stack (it may still be declined for want of a target).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nunchaku = scenario
        .add_artifact_to_hand_from_oracle(P0, "Novel Nunchaku", NOVEL_NUNCHAKU_ORACLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mine = scenario.add_creature(P0, "Chosen Host", 3, 3).id();
    let mut chosen_runner = scenario.build();
    grant_priority(&mut chosen_runner, P0);
    let chosen = chosen_runner.cast(nunchaku).target_object(mine).resolve();
    assert_eq!(attached_object(&chosen, nunchaku), Some(mine));
}
// ---------------------------------------------------------------------------
// Serene Remembrance — the compound-shuffle second subject
// ---------------------------------------------------------------------------

/// D4 (pre-existing, labelled): the "from a single graveyard" constraint is
/// parsed (the semantic row asserts `SameZoneOwner`) but not enforced on the
/// sub-ability's announced target set — a mixed-graveyard pair is accepted, so
/// this row asserts only what the engine actually does for the legal set.
#[test]
fn serene_remembrance_shuffles_two_cards_from_one_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_to_graveyard(P0, "Grave Ghoul", 1, 1)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Grave Spirit", 1, 1)
        .id();
    let serene = scenario
        .add_spell_to_hand_from_oracle(P0, "Serene Remembrance", false, SERENE_REMEMBRANCE_ORACLE)
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let outcome = runner
        .cast(serene)
        .target_objects(&[first, second])
        .resolve();
    // CR 115.6 + CR 701.24a: the second subject's "up to three target cards"
    // includes both chosen cards; both are shuffled into their owner's library.
    assert_eq!(outcome.zone_of(first), Zone::Library);
    assert_eq!(outcome.zone_of(second), Zone::Library);
}

// ---------------------------------------------------------------------------
// Stolen Goodies — the distribution pin (finite {0,3} spec survives assembly)
// ---------------------------------------------------------------------------

#[test]
fn stolen_goodies_accepts_at_most_three_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creatures: Vec<ObjectId> = (0..4)
        .map(|index| {
            scenario
                .add_creature(P0, &format!("Hungry {index}"), 1, 1)
                .id()
        })
        .collect();
    let goodies = scenario
        .add_spell_to_hand_from_oracle(P0, "Stolen Goodies", false, STOLEN_GOODIES_ORACLE)
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    // Four legal creatures are declared; the printed {0,3} spec caps the
    // ANNOUNCED set at three (CR 601.2c), so the committed stack entry may carry
    // only three targets.
    let commit = runner.cast(goodies).target_objects(&creatures).commit();
    let ability = commit
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .expect("the committed spell carries its ability");
    assert_eq!(
        ability.targets.len(),
        3,
        "the {{0,3}} spec caps the announced set at three: {:?}",
        ability.targets
    );
    let outcome = commit.resolve();
    let counters: Vec<u32> = creatures
        .iter()
        .map(|creature| counters_of(&outcome, *creature, &CounterType::Plus1Plus1))
        .collect();
    assert_eq!(
        counters.iter().sum::<u32>(),
        3,
        "all three counters are distributed: {counters:?}"
    );
    assert!(
        counters.iter().filter(|count| **count > 0).count() <= 3,
        "at most three targets may receive counters: {counters:?}"
    );
}
