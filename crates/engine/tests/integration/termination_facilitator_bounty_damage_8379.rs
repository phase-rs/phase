//! Issue #8379: Termination Facilitator must destroy the creature that WAS
//! DEALT damage, not the source that dealt it.
//!
//! CR 120.1: "An object that deals damage is the source of that damage" — the
//! recipient is the object that *receives* it. A PASSIVE-voice trigger
//! condition ("whenever a creature ... is dealt damage") makes its grammatical
//! subject the recipient, so the effect body's untargeted "it" (CR 608.2k)
//! names the damaged permanent. Binding it to the damage SOURCE inverts the
//! roles and destroys the wrong object.
//!
//! The reporter saw both halves of that single inversion in one game: the
//! bountied creature survived AND the Outpost Siege that dealt the damage was
//! destroyed instead. Both follow from one mis-bound anaphor.
//!
//! `valid_card` (which creature may trigger) was already correct; only the
//! destroy target was wrong, which is why the card reported as `fully_parsed`.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const TERMINATION_FACILITATOR_ORACLE: &str = "{T}: Put a bounty counter on target creature or planeswalker. Activate only as a sorcery.\n\
Whenever a creature or planeswalker an opponent controls with a bounty counter on it is dealt damage, destroy it.";

/// The reporter's board: the damage came from the opponent's own permanent
/// (Outpost Siege), so BOTH candidate objects are controlled by P1. A wrong
/// binding therefore cannot be masked by controller filtering.
fn bounty_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.add_creature_from_oracle(
        P0,
        "Termination Facilitator",
        1,
        3,
        TERMINATION_FACILITATOR_ORACLE,
    );
    let bountied = scenario.add_creature(P1, "Bountied Bear", 2, 2).id();
    let dealer = scenario.add_creature(P1, "Siege Engine", 1, 4).id();
    scenario.with_counter(bountied, CounterType::Generic("bounty".to_string()), 1);
    (scenario.build(), bountied, dealer)
}

/// Where did this object end up? A destroyed permanent may have left the object
/// map entirely, which is itself "in the graveyard" for this test's purposes —
/// the same idiom `the_black_arrow_dragon_gated_destroy` uses.
fn zone_of(runner: &GameRunner, object: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&object)
        .map_or(Zone::Graveyard, |o| o.zone)
}

fn ping(source_id: ObjectId, target: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Object(target)],
        source_id,
        P1,
    )
}

/// THE RUNTIME HALF. A green AST assertion is not evidence that the right
/// creature dies, so this drives real damage through `deal_damage::resolve` +
/// `process_triggers` and asserts on final zones.
///
/// Pre-fix this fails on BOTH assertions at once — Siege Engine is in the
/// graveyard and Bountied Bear is still on the battlefield — which is the
/// single-defect claim stated as a test.
#[test]
fn bountied_creature_dies_and_the_damage_source_survives() {
    let (mut runner, bountied, dealer) = bounty_board();

    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &ping(dealer, bountied), &mut events)
        .expect("damage to the bountied creature resolves");
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, bountied),
        Zone::Graveyard,
        "the creature that WAS DEALT damage carries the bounty counter and must be destroyed"
    );
    assert_eq!(
        zone_of(&runner, dealer),
        Zone::Battlefield,
        "the source that DEALT the damage must survive — destroying it was symptom (B) of #8379"
    );
}

/// The trigger must not fire at all when the damaged permanent has no bounty
/// counter. Without this, the test above would still pass if the fix
/// over-fired and destroyed every damaged creature.
#[test]
fn an_unbountied_creature_dealt_damage_is_not_destroyed() {
    let mut scenario = GameScenario::new();
    scenario.add_creature_from_oracle(
        P0,
        "Termination Facilitator",
        1,
        3,
        TERMINATION_FACILITATOR_ORACLE,
    );
    let plain = scenario.add_creature(P1, "Plain Bear", 2, 2).id();
    let dealer = scenario.add_creature(P1, "Siege Engine", 1, 4).id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &ping(dealer, plain), &mut events)
        .expect("damage resolves");
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, plain),
        Zone::Battlefield,
        "no bounty counter ⇒ the trigger's valid_card must not match"
    );
    assert_eq!(zone_of(&runner, dealer), Zone::Battlefield);
}

/// Extract the trigger's destroy/counter target filter from freshly parsed
/// Oracle text.
fn passive_damage_trigger_target(oracle: &str, card_name: &str) -> TargetFilter {
    let parsed = engine::parser::oracle::parse_oracle_text(
        oracle,
        card_name,
        &[],
        &["Creature".to_string()],
        &[],
    );
    parsed
        .triggers
        .iter()
        .find_map(|t| {
            let effect = t.execute.as_deref()?.effect.as_ref();
            match effect {
                Effect::Destroy { target, .. } | Effect::PutCounter { target, .. } => {
                    Some((t.mode.clone(), target.clone()))
                }
                _ => None,
            }
        })
        .map(|(mode, target)| {
            assert_eq!(
                mode,
                TriggerMode::DamageReceived,
                "{card_name} must parse as a passive damage trigger"
            );
            target
        })
        .unwrap_or_else(|| panic!("{card_name} must parse a damage-received trigger"))
}

/// THE CLASS, not the card. Every printed shape of "whenever <non-self
/// subject> is dealt damage, <verb> it" must bind the recipient. The
/// `AttachedTo` rows matter most: they are 7 of the 10 affected cards, and
/// their subject is the enchanted/equipped creature, so the anaphor can never
/// mean the Aura or Equipment.
#[test]
fn passive_damage_anaphor_binds_the_recipient_across_the_class() {
    for (name, oracle) in [
        (
            "Termination Facilitator",
            "Whenever a creature or planeswalker an opponent controls with a bounty counter on it is dealt damage, destroy it.",
        ),
        // CR 120.2a damage-class axis + `AttachedTo` subject (Hot Soup).
        (
            "Hot Soup",
            "Whenever equipped creature is dealt damage, destroy it.",
        ),
        (
            "Cracked Skull",
            "When enchanted creature is dealt damage, destroy it.",
        ),
        // Unrestricted subject (Death Pits of Rath).
        (
            "Death Pits of Rath",
            "Whenever a creature is dealt damage, destroy it.",
        ),
        // A non-Destroy body proves the binding is anaphor-scoped, not
        // Destroy-scoped (Rite of Passage).
        (
            "Rite of Passage",
            "Whenever a creature you control is dealt damage, put a +1/+1 counter on it.",
        ),
    ] {
        assert_eq!(
            passive_damage_trigger_target(oracle, name),
            TargetFilter::EventTarget,
            "{name}: the passive-voice anaphor must bind the damage RECIPIENT"
        );
    }
}

/// NEGATIVE CONTROL 1 — the ACTIVE-voice sibling must be untouched. Here the
/// grammatical subject IS the damage source, so `TriggeringSource` stays
/// correct. 40 cards in card-data.json depend on this staying put; if the fix
/// keyed on "dealt damage" alone rather than on voice, this row goes red.
#[test]
fn active_voice_damage_anaphor_still_binds_the_dealer() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        "Whenever a creature deals damage to you, destroy it.",
        "Ashnod",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let (mode, target) = parsed
        .triggers
        .iter()
        .find_map(|t| match t.execute.as_deref()?.effect.as_ref() {
            Effect::Destroy { target, .. } => Some((t.mode.clone(), target.clone())),
            _ => None,
        })
        .expect("active-voice damage trigger must parse");
    assert_eq!(mode, TriggerMode::DamageDone);
    assert_eq!(
        target,
        TargetFilter::TriggeringSource,
        "active voice: the subject IS the dealer, so TriggeringSource must survive the fix"
    );
}

/// NEGATIVE CONTROL 2 — a SELF-scoped passive trigger (the enrage class) keeps
/// its `SelfRef` binding. The recipient and the source object coincide there,
/// and `SelfRef` resolves without consulting the trigger event at all, so
/// widening the new axis to self-referential subjects would be a strict
/// robustness regression across a large class.
#[test]
fn self_scoped_passive_damage_trigger_keeps_selfref() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        "Whenever this creature is dealt damage, put a +1/+1 counter on it.",
        "Enrage Fixture",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let target = parsed
        .triggers
        .iter()
        .find_map(|t| match t.execute.as_deref()?.effect.as_ref() {
            Effect::PutCounter { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("self-scoped enrage trigger must parse");
    assert_eq!(
        target,
        TargetFilter::SelfRef,
        "a self-scoped passive trigger must not be widened to EventTarget"
    );
}

/// THE DISCRIMINATOR: symptoms (A) and (B) are one defect, demonstrated by
/// holding the board fixed and varying ONLY the destroy target's binding.
///
/// The reporter saw the bountied creature survive *and* the damage source die.
/// Those are not two bugs — they are the two halves of one inverted reference.
/// Driving the same `DamageReceived` trigger with each binding in turn shows the
/// binding alone selects which object dies:
///   `TriggeringSource` (the pre-fix parse) → the DEALER dies      = symptom (B)
///   `EventTarget`      (the fixed parse)   → the RECIPIENT dies   = correct
///
/// This also pins the handler fix (`destroy::resolve` resolving an untargeted
/// event-context referent). Before it, BOTH bindings destroyed nothing, so this
/// test could not have distinguished them at all.
#[test]
fn the_destroy_binding_alone_selects_which_object_dies() {
    for (binding, expect_recipient_dead, label) in [
        (
            TargetFilter::TriggeringSource,
            false,
            "TriggeringSource destroys the DEALER",
        ),
        (
            TargetFilter::EventTarget,
            true,
            "EventTarget destroys the RECIPIENT",
        ),
    ] {
        let mut scenario = GameScenario::new();
        let recipient = scenario.add_creature(P1, "Recipient", 2, 2).id();
        let dealer = scenario.add_creature(P1, "Dealer", 1, 4).id();
        let mut runner = scenario.build();

        let mut events = Vec::new();
        deal_damage::resolve(runner.state_mut(), &ping(dealer, recipient), &mut events)
            .expect("damage resolves");

        // One ability, one event, one varying field: the destroy target.
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: binding,
                cant_regenerate: false,
            },
            vec![],
            recipient,
            P0,
        );
        let damage_event = events
            .iter()
            .find(|e| matches!(e, engine::types::events::GameEvent::DamageDealt { .. }))
            .cloned()
            .expect("the damage event must be recorded");
        let state = runner.state_mut();
        // CR 603.2: the referent both bindings read comes from this one event.
        state.current_trigger_event = Some(damage_event);
        let mut out = Vec::new();
        engine::game::effects::destroy::resolve(state, &ability, &mut out)
            .expect("destroy resolves");

        assert_eq!(
            zone_of(&runner, recipient) == Zone::Graveyard,
            expect_recipient_dead,
            "{label}: recipient"
        );
        assert_eq!(
            zone_of(&runner, dealer) == Zone::Graveyard,
            !expect_recipient_dead,
            "{label}: dealer"
        );
    }
}
