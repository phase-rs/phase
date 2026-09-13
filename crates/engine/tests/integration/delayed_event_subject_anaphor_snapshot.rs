//! CR 603.7c + CR 608.2k: a PHASE-delayed trigger that names an object carried
//! by its CREATION event must snapshot that object at creation time.
//!
//! "Whenever <source> deals combat damage to a creature, destroy that creature
//! AT END OF COMBAT" lowers to
//! `CreateDelayedTrigger { condition: AtNextPhase(EndCombat), effect: Destroy {
//! target: EventTarget } }`. `EventTarget` resolves out of
//! `state.current_trigger_event`, which at the end-of-combat step is the phase
//! change — it carries no object, so the referent resolved to nothing and the
//! destroy silently did nothing (issue #4229, Ohran Viper).
//!
//! `delayed_trigger::resolve` already creation-time-snapshots the OTHER
//! event-subject anaphor, `TriggeringSource`, for exactly this reason.
//! `EventTarget` is its CR 120.3 recipient counterpart and was simply never a
//! member of that snapshot pass.
//!
//! These are building-block tests: the snapshot is keyed on the anaphor, not on
//! a card, so the coverage below spans the self-referential source (Ohran
//! Viper), a source filter that is NOT the damage dealer (the Sliver class,
//! where the trigger source watches a third object deal the damage), and the
//! granted-ability form (Simic Basilisk), whose trigger source is the creature
//! that received the grant rather than the granter.

use super::rules::{GameScenario, Phase, P0, P1};
use engine::game::combat::AttackTarget;
use engine::game::scenario::GameRunner;
use engine::types::ability::{
    AbilityDefinition, Effect, TargetFilter, TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::zones::Zone;

const OHRAN_VIPER: &str =
    "Whenever this creature deals combat damage to a creature, destroy that creature at end of combat.";
const DELAYED_SLIVER: &str =
    "Whenever a Sliver deals combat damage to a creature, destroy that creature at end of combat.";
const SIMIC_BASILISK_GRANT: &str = "{1}{G}: Until end of turn, target creature with a +1/+1 counter on it gains \"Whenever this creature deals combat damage to a creature, destroy that creature at end of combat.\"";
/// Verbatim Oracle text (Scryfall). An instant, so it can be cast in the
/// combat-damage step's priority window — which is the only place a blink can
/// land BETWEEN the delayed trigger's creation and its end-of-combat firing.
/// "Creature you control" is satisfiable here because the damage RECIPIENT is
/// by construction controlled by the trigger controller's opponent.
const EPHEMERATE: &str =
    "Exile target creature you control, then return it to the battlefield under its owner's control.";

/// CR 400.1: the zone an object currently occupies, read straight off the live
/// runner (`Outcome::zone_of` only sees the snapshot taken at its own call).
fn zone_of(runner: &GameRunner, object: ObjectId) -> Zone {
    runner.state().objects[&object].zone
}

/// One unit of untapped, unrestricted mana of `color`.
fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

/// Drive the declare-blockers step: pass priority until the engine surfaces it.
fn pass_into_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    panic!("never reached the declare-blockers step");
}

/// CR 511.1 + CR 603.7c: the reported board state (issue #4229). Ohran Viper
/// attacks, a 0/6 Wall blocks. The Viper deals 1 combat damage to the Wall — not
/// lethal, so only the delayed destroy can remove it — and takes 0 back. At the
/// END OF COMBAT step the delayed trigger fires and must destroy the Wall it
/// damaged.
#[test]
fn ohran_viper_destroys_the_damaged_creature_at_end_of_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let viper = {
        let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
        b.from_oracle_text(OHRAN_VIPER);
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_into_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, viper)])
        .expect("declare blockers");

    let damage = runner.combat_damage();
    assert_eq!(
        damage.zone_of(wall),
        Zone::Battlefield,
        "CR 704.5g: 1 damage is not lethal to a 0/6 — the Wall may only die to the \
         delayed destroy, so this test would pass vacuously if it died here"
    );

    // CR 511.1: the delayed trigger fires at the beginning of the end-of-combat
    // step; drive past it so the trigger resolves.
    runner.advance_to_phase(Phase::PostCombatMain);

    assert_eq!(
        zone_of(&runner, wall),
        Zone::Graveyard,
        "CR 603.7c: the delayed destroy must affect the creature the trigger's \
         CREATION event damaged, snapshotted at creation time"
    );
    assert_eq!(
        zone_of(&runner, viper),
        Zone::Battlefield,
        "CR 120.1: the damage DEALER is not its own referent"
    );
}

/// CR 120.3 + CR 603.7c: the snapshot must bind the event's damage RECIPIENT,
/// not the trigger source and not the ability's source object. Here the trigger
/// source (a 3/3 Sliver lord that never fights) watches a DIFFERENT Sliver deal
/// the combat damage, so a snapshot keyed on `ability.source_id` or on
/// `TriggeringSource` would destroy the wrong creature — or nothing at all.
#[test]
fn delayed_sliver_trigger_destroys_the_recipient_not_the_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let watcher = {
        let mut b = scenario.add_creature(P0, "Toxin Sliver", 3, 3);
        b.with_subtypes(vec!["Sliver"]);
        b.from_oracle_text(DELAYED_SLIVER);
        b.id()
    };
    let dealer = {
        let mut b = scenario.add_creature(P0, "Sidewinder Sliver", 1, 1);
        b.with_subtypes(vec!["Sliver"]);
        b.id()
    };
    let blocker = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(dealer, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_into_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, dealer)])
        .expect("declare blockers");

    let damage = runner.combat_damage();
    assert_eq!(
        damage.zone_of(blocker),
        Zone::Battlefield,
        "1 damage is not lethal to a 0/6 — guards against a vacuous pass"
    );

    runner.advance_to_phase(Phase::PostCombatMain);

    assert_eq!(
        zone_of(&runner, blocker),
        Zone::Graveyard,
        "CR 120.3: the delayed destroy names the damage RECIPIENT"
    );
    assert_eq!(
        zone_of(&runner, dealer),
        Zone::Battlefield,
        "CR 120.1: the damage dealer must survive — `TriggeringSource` is the wrong anaphor"
    );
    assert_eq!(
        zone_of(&runner, watcher),
        Zone::Battlefield,
        "the trigger source is not its own referent"
    );
}

/// CR 603.7c + CR 113.3 + CR 611.2c: the GRANTED form (Simic Basilisk). The
/// trigger is printed on the grantor's activated ability and granted to a
/// DIFFERENT creature, so the delayed trigger created at combat damage belongs
/// to the GRANTEE. The creation-time snapshot must read the grantee's own
/// creation event — anything keyed on the grantor (which never fought) resolves
/// to nothing.
#[test]
fn granted_delayed_damage_trigger_snapshots_its_own_creation_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, vec![mana(ManaType::Green), mana(ManaType::Green)]);

    // The grantor stays home: it never attacks and never deals damage.
    let grantor = {
        let mut b = scenario.add_creature(P0, "Simic Basilisk", 2, 2);
        b.from_oracle_text(SIMIC_BASILISK_GRANT);
        b.id()
    };
    // CR 122.1: the grant targets "creature with a +1/+1 counter on it".
    let grantee = {
        let mut b = scenario.add_creature(P0, "Grafted Creature", 1, 1);
        b.with_plus_counters(1);
        b.id()
    };
    let blocker = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();

    let mut runner = scenario.build();
    runner.activate(grantor, 0).target_object(grantee).resolve();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(grantee, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_into_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, grantee)])
        .expect("declare blockers");

    let damage = runner.combat_damage();
    assert_eq!(
        damage.zone_of(blocker),
        Zone::Battlefield,
        "2 damage is not lethal to a 0/6 — guards against a vacuous pass"
    );

    runner.advance_to_phase(Phase::PostCombatMain);

    assert_eq!(
        zone_of(&runner, blocker),
        Zone::Graveyard,
        "CR 603.7c: a granted delayed trigger snapshots the recipient from its own \
         creation event"
    );
    assert_eq!(
        zone_of(&runner, grantor),
        Zone::Battlefield,
        "the grantor never fought and is not its own referent"
    );
}

/// CR 400.7 + CR 603.7c: the PIN, exercised through the production pipeline.
///
/// "If that object leaves the battlefield and returns, it becomes a new object
/// and the ability no longer affects it." The creature is damaged (which creates
/// and snapshots the delayed trigger), then BLINKED in the combat-damage step's
/// priority window, and returns as a new incarnation before the end-of-combat
/// step fires the delayed destroy. The returned permanent must survive.
///
/// Two arms. Arm 1 is a mandatory reach-guard: without it, "the creature lived"
/// in arm 2 would also pass on a trigger that never fired, or on a snapshot pass
/// that silently bound nothing — the exact bug this file exists to catch.
///
/// The blink is cast by the RECIPIENT's controller, which is always the trigger
/// controller's opponent, so Ephemerate's "creature you control" is satisfied.
#[test]
fn delayed_destroy_does_not_affect_a_blinked_and_returned_recipient() {
    /// Drive combat until the damage trigger has RESOLVED and installed its
    /// delayed trigger, stopping in the combat-damage step's priority window
    /// rather than running on to end of combat.
    ///
    /// Stopping merely at "damage is marked" is too early: the damage trigger is
    /// still on the stack at that point and `CreateDelayedTrigger` has not run.
    fn deal_combat_damage_and_hold_priority(
        runner: &mut GameRunner,
        viper: ObjectId,
        wall: ObjectId,
    ) {
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(viper, AttackTarget::Player(P1))])
            .expect("declare attackers");
        pass_into_declare_blockers(runner);
        runner
            .declare_blockers(&[(wall, viper)])
            .expect("declare blockers");
        for _ in 0..16 {
            if !runner.state().delayed_triggers.is_empty() {
                return;
            }
            assert_ne!(
                runner.state().phase,
                Phase::EndCombat,
                "reached end of combat before the damage trigger installed its \
                 delayed trigger — the blink would have nothing to race"
            );
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority toward the combat damage step");
        }
        panic!(
            "combat damage never installed a delayed trigger (damage marked: {})",
            runner.state().objects[&wall].damage_marked
        );
    }

    // ---- Arm 1 (reach-guard): no blink, the delayed destroy DOES land. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let viper = {
        let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
        b.from_oracle_text(OHRAN_VIPER);
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();

    let mut runner = scenario.build();
    deal_combat_damage_and_hold_priority(&mut runner, viper, wall);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: combat damage must install exactly one delayed trigger"
    );
    runner.advance_to_phase(Phase::PostCombatMain);
    assert_eq!(
        zone_of(&runner, wall),
        Zone::Graveyard,
        "arm 1 reach-guard: with no blink the delayed destroy must kill the \
         damaged creature — otherwise arm 2 proves nothing"
    );

    // ---- Arm 2 (the detector): blink the recipient, it must SURVIVE. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, vec![mana(ManaType::White), mana(ManaType::White)]);
    let viper = {
        let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
        b.from_oracle_text(OHRAN_VIPER);
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P1, "Ephemerate", true, EPHEMERATE)
        .id();

    let mut runner = scenario.build();
    deal_combat_damage_and_hold_priority(&mut runner, viper, wall);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: the blink arm must install the same delayed trigger"
    );

    // The recipient's controller (P1) holds priority after the active player
    // passes; cast the blink there.
    for _ in 0..4 {
        if runner.state().waiting_for.acting_players().first().copied() == Some(P1) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass active-player priority so the blinker can respond");
    }
    let blinked = runner.cast(ephemerate).target_object(wall).resolve();
    assert_eq!(
        blinked.zone_of(wall),
        Zone::Battlefield,
        "reach-guard: Ephemerate must return the creature to the battlefield"
    );

    runner.advance_to_phase(Phase::PostCombatMain);

    assert_eq!(
        zone_of(&runner, wall),
        Zone::Battlefield,
        "CR 400.7: the blinked recipient came back as a NEW object, so the \
         pinned delayed destroy must not affect it"
    );
}

/// CR 120.1 + CR 120.3 + CR 608.2k: a delayed chain naming BOTH event subjects
/// binds each clause to its own object, END TO END through the delayed-trigger
/// pipeline.
///
/// `ResolvedAbility::targets` is ONE shared slot. Without
/// `delayed_trigger::bind_event_subject_nodes` both clauses read that slot, so
/// they hit the same object: one of the two creatures wrongly survives. CR 120.1
/// makes the event's subject the damage DEALER and CR 120.3 makes its object
/// slot the RECIPIENT, and on a blocked attack those are never the same object.
///
/// No printed card reaches this shape (a `card-data.json` scan finds 6 delayed
/// `EventTarget` cards, 76 delayed `TriggeringSource` cards, and zero naming
/// both), so the chain is built by taking the REAL parsed Ohran Viper delayed
/// payload and cloning its clause with the other anaphor substituted. That keeps
/// every field the parser produces and changes only the axis under test — the
/// printed reading is "destroy that creature and this creature at end of
/// combat", a coherent mutual-destruction basilisk.
///
/// Run in BOTH orderings, because the chain-wide answer is whichever anaphor
/// comes first in `EVENT_SUBJECT_ANAPHORS` rather than whichever the root clause
/// uses: a rebind keyed on "differs from the chain-wide pick" passes one
/// ordering and fails the other.
#[test]
fn a_mixed_event_subject_delayed_chain_destroys_both_dealer_and_recipient() {
    /// Ohran Viper's parsed trigger, with a second delayed clause appended that
    /// names the opposite anaphor. `root_anaphor` becomes the delayed payload's
    /// ROOT clause and the other becomes its sub-clause.
    fn mixed_trigger(root_anaphor: TargetFilter, sub_anaphor: TargetFilter) -> TriggerDefinition {
        let abilities =
            engine::parser::oracle::parse_oracle_text(OHRAN_VIPER, "Ohran Viper", &[], &[], &[]);
        let mut trigger = abilities
            .triggers
            .first()
            .expect("Ohran Viper must parse a damage trigger")
            .clone();

        fn set_destroy_target(def: &mut AbilityDefinition, filter: TargetFilter) {
            match &mut *def.effect {
                Effect::Destroy { target, .. } => *target = filter,
                other => panic!("expected a Destroy payload, got {other:?}"),
            }
        }

        let execute = trigger
            .execute
            .as_deref_mut()
            .expect("trigger must have a body");
        let Effect::CreateDelayedTrigger {
            effect: delayed, ..
        } = &mut *execute.effect
        else {
            panic!("Ohran Viper's body must be a CreateDelayedTrigger");
        };

        // The second clause: same shape, opposite anaphor, no further chain.
        let mut sub = delayed.as_ref().clone();
        sub.sub_ability = None;
        sub.else_ability = None;
        set_destroy_target(&mut sub, sub_anaphor);

        set_destroy_target(delayed, root_anaphor);
        delayed.sub_ability = Some(Box::new(sub));

        trigger.clone()
    }

    // Both orderings of the same chain must behave identically.
    for (label, root, sub) in [
        (
            "root=EventTarget sub=TriggeringSource",
            TargetFilter::EventTarget,
            TargetFilter::TriggeringSource,
        ),
        (
            "root=TriggeringSource sub=EventTarget",
            TargetFilter::TriggeringSource,
            TargetFilter::EventTarget,
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // 1/2 dealer vs 0/6 blocker: neither kills the other in combat, so any
        // death below is attributable ONLY to a delayed destroy clause.
        let dealer = {
            let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
            b.with_trigger_definition(mixed_trigger(root.clone(), sub.clone()));
            b.id()
        };
        let recipient = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();

        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(dealer, AttackTarget::Player(P1))])
            .expect("declare attackers");
        pass_into_declare_blockers(&mut runner);
        runner
            .declare_blockers(&[(recipient, dealer)])
            .expect("declare blockers");

        let damage = runner.combat_damage();
        assert_eq!(
            damage.zone_of(recipient),
            Zone::Battlefield,
            "{label}: 1 damage is not lethal to a 0/6 — guards against a vacuous pass"
        );
        assert_eq!(
            damage.zone_of(dealer),
            Zone::Battlefield,
            "{label}: a 0/6 deals no damage — the dealer must survive combat itself"
        );

        runner.advance_to_phase(Phase::PostCombatMain);

        assert_eq!(
            zone_of(&runner, recipient),
            Zone::Graveyard,
            "{label}: CR 120.3 — the EventTarget clause must destroy the damage RECIPIENT"
        );
        assert_eq!(
            zone_of(&runner, dealer),
            Zone::Graveyard,
            "{label}: CR 120.1 — the TriggeringSource clause must destroy the damage \
             DEALER. Both in the graveyard is the whole point: one shared target slot \
             would send both clauses at the same object and leave the other alive"
        );
    }
}

/// CR 608.2k: the root snapshot answers for the ROOT clause only.
///
/// Swooping Pteranodon is the ONE card in the shipped corpus whose delayed chain
/// names an event subject in a DESCENDANT but not in its root:
///
///   root: TargetOnly { Typed(Land) }                      <- the chosen land
///   sub : DealDamage { 3, target: TriggeringSource, .. }  <- "that creature"
///
/// It is therefore the only shipped card whose behavior the root-local snapshot
/// could move, and it had NO coverage before this test.
///
/// This is a CHARACTERIZATION test, not a revert-detector, and the distinction
/// is deliberate: the binding asserted below is identical under the old
/// chain-wide root snapshot and the new root-local one — verified by running it
/// both ways. That is precisely the point. It pins the one card at risk and
/// records that this change does not move it, which is the evidence that
/// switching the root question is safe. Do not read a passing run here as proof
/// that root-local is in effect; `a_mixed_event_subject_delayed_chain_*` above
/// carries that proof.
#[test]
fn swooping_pteranodon_root_clause_keeps_its_own_target_slot() {
    const SWOOPING_PTERANODON: &str = "Whenever this creature or another Dinosaur you control with flying enters, gain control of target creature an opponent controls until end of turn. Untap that creature. It gains flying and haste until end of turn. At the beginning of the next end step, target land deals 3 damage to that creature.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, vec![mana(ManaType::Green); 6]);

    let pteranodon = scenario
        .add_spell_to_hand_from_oracle(P0, "Swooping Pteranodon", false, SWOOPING_PTERANODON)
        .as_creature()
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.add_land_from_oracle(P0, "Mountain", "{T}: Add {R}.");

    let mut runner = scenario.build();
    runner.cast(pteranodon).target_object(victim).resolve();

    // Reach-guard: the ETB resolved and installed the delayed trigger. Without
    // this, the binding assertions below would pass vacuously on an empty list.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach-guard: Pteranodon's ETB must install exactly one delayed trigger"
    );

    // The root clause is `TargetOnly` over a LAND; the creature referent belongs
    // to the damage sub-clause. Asserting the root slot does not hold the
    // creature records the separation of the two clauses' bindings.
    let delayed = &runner.state().delayed_triggers[0];
    assert!(
        !delayed.ability.targets.contains(&TargetRef::Object(victim)),
        "the root TargetOnly(Land) clause must not hold the damage clause's \
         creature referent; got {:?}",
        delayed.ability.targets
    );
}

/// CR 608.2k + CR 603.7c: a delayed MASS zone move keeps its creation event's
/// referent, end to end.
///
/// `Effect::ChangeZoneAll` answers `None` from `target_filter()` — the whole
/// mass-population family does — so its `target` is a HIDDEN slot that
/// `effect_parent_ref_slots` must surface explicitly. While that arm was gated
/// on `filter_refs_parent_target` alone, a mass move naming `EventTarget` was
/// invisible to the event-subject detector: no creation-time snapshot was taken,
/// and at the end-of-combat step (whose phase event carries no event subject)
/// it resolved against nothing and moved no cards.
///
/// The parser can produce this shape — its trigger rebind converts a context
/// filter to `EventTarget` across all nine mass effects — but no printed card
/// pairs it with a delayed suffix today, so the payload is built by substituting
/// a `ChangeZoneAll` into the real parsed Ohran Viper delayed trigger. The
/// printed reading is "put that creature into its owner's graveyard at end of
/// combat".
#[test]
fn a_delayed_mass_zone_move_keeps_its_creation_event_referent() {
    /// Ohran Viper's parsed trigger with its delayed payload replaced by a mass
    /// zone move over the damage recipient.
    fn mass_move_trigger() -> TriggerDefinition {
        let abilities =
            engine::parser::oracle::parse_oracle_text(OHRAN_VIPER, "Ohran Viper", &[], &[], &[]);
        let mut trigger = abilities
            .triggers
            .first()
            .expect("Ohran Viper must parse a damage trigger")
            .clone();

        let execute = trigger
            .execute
            .as_deref_mut()
            .expect("trigger must have a body");
        let Effect::CreateDelayedTrigger {
            effect: delayed, ..
        } = &mut *execute.effect
        else {
            panic!("Ohran Viper's body must be a CreateDelayedTrigger");
        };

        *delayed.effect = Effect::ChangeZoneAll {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            target: TargetFilter::EventTarget,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            library_shuffle: Default::default(),
            random_order: false,
        };
        delayed.sub_ability = None;
        delayed.else_ability = None;

        trigger.clone()
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let viper = {
        let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
        b.with_trigger_definition(mass_move_trigger());
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();
    // A bystander the mass move must NOT sweep: `EventTarget` names ONE object,
    // so a filter that degraded to "all creatures" would take this too.
    let bystander = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_into_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, viper)])
        .expect("declare blockers");

    let damage = runner.combat_damage();
    assert_eq!(
        damage.zone_of(wall),
        Zone::Battlefield,
        "1 damage is not lethal to a 0/6 — the wall may only leave via the \
         delayed mass move, so this test would pass vacuously if it died here"
    );

    runner.advance_to_phase(Phase::PostCombatMain);

    assert_eq!(
        zone_of(&runner, wall),
        Zone::Graveyard,
        "CR 603.7c: the delayed mass move must affect the creature its CREATION \
         event damaged, snapshotted before the phase event erased that context"
    );
    assert_eq!(
        zone_of(&runner, bystander),
        Zone::Battlefield,
        "EventTarget names exactly one object — an uninvolved creature must not \
         be swept up by the mass move"
    );
}

/// CR 608.2k + CR 603.7c: a delayed mass filter that NESTS the event subject
/// binds it too, and the enclosing structure keeps its meaning.
///
/// `Not(EventTarget)` is the sharp case, and it fails in the most dangerous
/// direction: if the inner reference is detected but never bound, at the phase
/// event it resolves to NO object, and `Not(nothing)` inverts into "everything".
/// A delayed "destroy each OTHER creature" would then wipe the board AND take
/// the one creature it was written to spare.
///
/// So this asserts both halves: the referent survives (the exclusion still
/// excludes) and a bystander dies (the mass effect still applies to the rest).
/// Asserting only one half would pass on a filter that had collapsed.
#[test]
fn a_delayed_mass_move_binds_a_nested_event_subject_reference() {
    fn nested_mass_trigger() -> TriggerDefinition {
        let abilities =
            engine::parser::oracle::parse_oracle_text(OHRAN_VIPER, "Ohran Viper", &[], &[], &[]);
        let mut trigger = abilities
            .triggers
            .first()
            .expect("Ohran Viper must parse a damage trigger")
            .clone();
        let execute = trigger
            .execute
            .as_deref_mut()
            .expect("trigger must have a body");
        let Effect::CreateDelayedTrigger {
            effect: delayed, ..
        } = &mut *execute.effect
        else {
            panic!("Ohran Viper's body must be a CreateDelayedTrigger");
        };

        // "Destroy each creature OTHER THAN that creature at end of combat."
        *delayed.effect = Effect::DestroyAll {
            target: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(engine::types::ability::TypedFilter::creature()),
                    TargetFilter::Not {
                        filter: Box::new(TargetFilter::EventTarget),
                    },
                ],
            },
            cant_regenerate: false,
        };
        delayed.sub_ability = None;
        delayed.else_ability = None;
        trigger.clone()
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let viper = {
        let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
        b.with_trigger_definition(nested_mass_trigger());
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();
    let bystander = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_into_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, viper)])
        .expect("declare blockers");

    let damage = runner.combat_damage();
    assert_eq!(
        damage.zone_of(wall),
        Zone::Battlefield,
        "1 damage is not lethal to a 0/6 — guards against a vacuous pass"
    );

    runner.advance_to_phase(Phase::PostCombatMain);

    assert_eq!(
        zone_of(&runner, wall),
        Zone::Battlefield,
        "CR 608.2k: the nested Not(EventTarget) must still EXCLUDE the damaged \
         creature. An unbound inner reference resolves to nothing, and \
         Not(nothing) sweeps everything — including the object it must spare"
    );
    assert_eq!(
        zone_of(&runner, bystander),
        Zone::Graveyard,
        "the mass destroy must still apply to every OTHER creature — asserting \
         only the exclusion would pass on a filter that matched nobody at all"
    );
}

/// CR 400.7 + CR 603.7c: a blinked-and-returned referent is not affected by a
/// delayed MASS move either.
///
/// CHARACTERIZATION test, and the label is load-bearing: unlike its
/// single-target sibling above, this one does **not** detect a missing pin.
/// Verified by revert-probe — disabling a `target_pin_is_current` gate on the
/// `ChangeZoneAll` scan leaves it green, so whatever spares the returned object
/// on the mass path is not that gate. The mass resolvers (`change_zone`,
/// `destroy_all`, …) each scan and filter independently and `DestroyAll` carries
/// no pin check at all, so the mechanism here is still unidentified.
///
/// It is kept because the BEHAVIOR is correct and worth locking in: if a future
/// change starts sweeping blinked referents on the mass path, this goes red.
/// Do not read it as proof that the mass path enforces CR 400.7 — establishing
/// that needs a repro this test does not yet provide.
#[test]
fn a_delayed_mass_move_does_not_affect_a_blinked_and_returned_referent() {
    fn mass_move_trigger() -> TriggerDefinition {
        let abilities =
            engine::parser::oracle::parse_oracle_text(OHRAN_VIPER, "Ohran Viper", &[], &[], &[]);
        let mut trigger = abilities
            .triggers
            .first()
            .expect("Ohran Viper must parse a damage trigger")
            .clone();
        let execute = trigger
            .execute
            .as_deref_mut()
            .expect("trigger must have a body");
        let Effect::CreateDelayedTrigger {
            effect: delayed, ..
        } = &mut *execute.effect
        else {
            panic!("Ohran Viper's body must be a CreateDelayedTrigger");
        };
        *delayed.effect = Effect::ChangeZoneAll {
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            target: TargetFilter::EventTarget,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            library_shuffle: Default::default(),
            random_order: false,
        };
        delayed.sub_ability = None;
        delayed.else_ability = None;
        trigger.clone()
    }

    fn deal_damage_and_hold(runner: &mut GameRunner, viper: ObjectId, wall: ObjectId) {
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(viper, AttackTarget::Player(P1))])
            .expect("declare attackers");
        pass_into_declare_blockers(runner);
        runner
            .declare_blockers(&[(wall, viper)])
            .expect("declare blockers");
        for _ in 0..16 {
            if !runner.state().delayed_triggers.is_empty() {
                return;
            }
            assert_ne!(
                runner.state().phase,
                Phase::EndCombat,
                "reached end of combat before the delayed trigger was installed"
            );
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority toward the combat damage step");
        }
        panic!("combat damage never installed a delayed trigger");
    }

    // ---- Arm 1 (reach-guard): no blink, the mass destroy DOES land. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let viper = {
        let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
        b.with_trigger_definition(mass_move_trigger());
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();
    let mut runner = scenario.build();
    deal_damage_and_hold(&mut runner, viper, wall);
    runner.advance_to_phase(Phase::PostCombatMain);
    assert_eq!(
        zone_of(&runner, wall),
        Zone::Graveyard,
        "arm 1 reach-guard: without the blink the delayed mass destroy must kill \
         the damaged creature — otherwise arm 2 proves nothing"
    );

    // ---- Arm 2 (the detector): blink the referent, it must SURVIVE. ----
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P1, vec![mana(ManaType::White), mana(ManaType::White)]);
    let viper = {
        let mut b = scenario.add_creature(P0, "Ohran Viper", 1, 2);
        b.with_trigger_definition(mass_move_trigger());
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Stone", 0, 6).id();
    let ephemerate = scenario
        .add_spell_to_hand_from_oracle(P1, "Ephemerate", true, EPHEMERATE)
        .id();

    let mut runner = scenario.build();
    deal_damage_and_hold(&mut runner, viper, wall);
    for _ in 0..4 {
        if runner.state().waiting_for.acting_players().first().copied() == Some(P1) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass active-player priority so the blinker can respond");
    }
    let blinked = runner.cast(ephemerate).target_object(wall).resolve();
    assert_eq!(
        blinked.zone_of(wall),
        Zone::Battlefield,
        "reach-guard: Ephemerate must return the creature to the battlefield"
    );

    runner.advance_to_phase(Phase::PostCombatMain);

    assert_eq!(
        zone_of(&runner, wall),
        Zone::Battlefield,
        "CR 400.7: the blinked referent is a NEW object, so the pinned delayed \
         MASS destroy must not affect it — the mass path filters per object and \
         never reads ability.targets, so it needs its own pin check"
    );
}
