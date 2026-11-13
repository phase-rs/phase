//! CR 115.9a + CR 115.9b + CR 115.9c: an announced target's recorded fact
//! carries the target's public characteristics, the zone it was announced in and
//! the token bit it then held. It carries NO target relationships: the fact is an
//! `LKISnapshot`, and the synthesized `ZoneChangeRecord` it projects to has no
//! target list at all.
//!
//! Turn Aside ("Counter target spell that targets a permanent you control.")
//! announces a target whose slot filter is the bare stack kind PLUS a
//! `FilterProp::Targets` relationship. At manual mana payment the cast holds the
//! announced-target latch, and the live filter answers that relationship from the
//! stack entry's own target list. The restore guard has only the recorded fact,
//! so a projection that answers the relationship from the record walk's
//! fail-closed `false` reports a fact the save never stated as FALSE and refuses
//! an untouched save — even though the bare `StackSpell` leg is already UNKNOWN
//! on a captured stack zone.
//!
//! Swerve ("Change the target of target spell with a single target.") is the
//! sibling shape: `FilterProp::HasSingleTarget` counts the subject's targets,
//! which the record cannot state either.
//!
//! These tests pin both directions of the rule: a missing recorded fact is not a
//! failed predicate (the untouched save is admitted and resumes), while a
//! recorded axis that is genuinely contradicted (a captured zone that is not the
//! stack, for a slot that declared "target spell") is still refused.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, AnnouncedTargetZone, CastPaymentMode,
    PersistedGameState, PersistedRestoreError, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall): Turn Aside, an instant.
const TURN_ASIDE_ORACLE: &str = "Counter target spell that targets a permanent you control.";

/// Verbatim Oracle text (Scryfall): Lightning Bolt, an instant.
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

/// Verbatim Oracle text (Scryfall): Swerve, an instant.
const SWERVE_ORACLE: &str = "Change the target of target spell with a single target.";

/// Verbatim Oracle text (Scryfall): Stifle, an instant.
const STIFLE_ORACLE: &str = "Counter target activated or triggered ability.";

fn one_mana(color: ManaColor) -> ManaCost {
    ManaCost::Cost {
        shards: vec![match color {
            ManaColor::White => ManaCostShard::White,
            ManaColor::Blue => ManaCostShard::Blue,
            ManaColor::Black => ManaCostShard::Black,
            ManaColor::Red => ManaCostShard::Red,
            ManaColor::Green => ManaCostShard::Green,
        }],
        generic: 0,
    }
}

fn blue_red() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
        generic: 0,
    }
}

/// Save the parked cast through the authoritative capture and read it back.
fn save_and_restore(
    runner: &GameRunner,
) -> Result<engine::types::game_state::GameState, PersistedRestoreError> {
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the parked cast serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the parked cast deserializes");
    restored.into_game_state()
}

/// Does `filter` carry a `Targets` relationship anywhere in its tree?
fn filter_has_targets(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::Targets { .. })),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_has_targets)
        }
        TargetFilter::Not { filter } => filter_has_targets(filter),
        _ => false,
    }
}

/// Does `filter` carry a `HasSingleTarget` requirement anywhere in its tree?
fn filter_has_single_target(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .properties
            .iter()
            .any(|prop| matches!(prop, FilterProp::HasSingleTarget)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_has_single_target)
        }
        TargetFilter::Not { filter } => filter_has_single_target(filter),
        _ => false,
    }
}

/// The one object fact the parked cast's announcement latch carries.
fn parked_latch_fact(runner: &GameRunner) -> &ActivationTargetFact {
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the manual payment park keeps its pending cast");
    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach-guard: the announcement latch is populated at the payment park");
    assert_eq!(
        facts.len(),
        1,
        "reach-guard: this cast declares exactly one target"
    );
    &facts[0]
}

struct ParkedTurnAside {
    runner: GameRunner,
    turn_aside: ObjectId,
    bolt: ObjectId,
    victim: ObjectId,
    island: ObjectId,
}

/// P1 is the active player; it casts Lightning Bolt at P0's creature, and P0
/// answers with Turn Aside at the Bolt using `CastPaymentMode::Manual`, which
/// parks the cast at `WaitingFor::ManaPayment` holding its announced-target
/// latch.
fn park_turn_aside_at_manual_payment() -> ParkedTurnAside {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P0, "Villager", 2, 2).id();
    scenario.add_basic_land(P1, ManaColor::Red);
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
        .with_mana_cost(one_mana(ManaColor::Red))
        .id();
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    let turn_aside = scenario
        .add_spell_to_hand_from_oracle(P0, "Turn Aside", true, TURN_ASIDE_ORACLE)
        .with_mana_cost(one_mana(ManaColor::Blue))
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner.cast(bolt).target_objects(&[victim]).commit();
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Stack,
        "reach-guard: CR 405.1 puts the Bolt on the stack"
    );
    // CR 117.3d: the active player passes so the defending player can respond.
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach-guard: P0 holds priority with the Bolt on the stack"
    );

    let card_id = runner.state().objects[&turn_aside].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: turn_aside,
            card_id,
            targets: vec![bolt],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("CR 115.9b: the Bolt targets a permanent P0 controls, so Turn Aside may target it");
    assert!(
        matches!(cast.waiting_for, WaitingFor::ManaPayment { player: P0, .. }),
        "reach-guard: the cast parks at manual payment, got {:?}",
        cast.waiting_for
    );
    ParkedTurnAside {
        runner,
        turn_aside,
        bolt,
        victim,
        island,
    }
}

/// WITNESS (honest direction): an UNTOUCHED save of a Turn Aside announced at a
/// Bolt, taken during mana payment, restores and resumes — and the resumed cast
/// still counters the spell it announced.
///
/// Revert-probe: with `FilterProp::Targets` answered by the record walk's
/// fail-closed `false`, `into_game_state` returns
/// `ContradictoryAnnouncedTargetFacts` and this test fails at the
/// `save_and_restore` call.
#[test]
fn turn_aside_save_taken_during_mana_payment_restores_and_resumes() {
    let ParkedTurnAside {
        runner,
        turn_aside,
        bolt,
        victim,
        island,
    } = park_turn_aside_at_manual_payment();

    // THE DECLARING FILTER: the slot Turn Aside announced into asks a TARGET
    // RELATIONSHIP, so this witness exercises the predicate the announcement
    // never records.
    let declared = runner.state().objects[&turn_aside]
        .abilities
        .iter()
        .find_map(|ability| ability.effect.target_filter())
        .expect("reach-guard: Turn Aside declares a target slot");
    assert!(
        filter_has_targets(declared),
        "reach-guard: the declaring slot filter asks 'that targets a permanent you control', \
         got {declared:?}"
    );

    // The reach guard: the latch IS populated, and it states the announcement's
    // captured zone — the stack.
    match parked_latch_fact(&runner) {
        ActivationTargetFact::Object { id, zone, .. } => {
            assert_eq!(*id, bolt, "the latch names the announced Bolt");
            assert_eq!(
                *zone,
                AnnouncedTargetZone::Known(Zone::Stack),
                "CR 405.1: the announced target was on the stack"
            );
        }
        other => panic!("reach-guard: Turn Aside's declared target is an object, got {other:?}"),
    }
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "reach-guard: the Bolt's victim is a permanent P0 controls"
    );

    let restored = save_and_restore(&runner)
        .expect("CR 115.9b: an untouched Turn Aside save is admitted, not refused");
    let mut runner = GameRunner::from_state(restored);

    // CR 601.2c: the restore kept the stack target itself.
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("the restored park keeps its pending cast");
    assert_eq!(
        pending.ability.targets,
        vec![TargetRef::Object(bolt)],
        "the restored cast still names the spell it announced"
    );

    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&island)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the restored payment prompt offers the Island's own {T}: Add {U}");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the restored cast takes the mana it announced it would");
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
    assert_eq!(
        runner.state().objects[&turn_aside].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the counter resolved");
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Graveyard,
        "CR 701.5a: the resumed Turn Aside counters the Bolt it announced"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "the countered Bolt never deals its damage"
    );
}

/// THE DISCRIMINATING TWIN: the same save with the recorded announcement's ZONE
/// restated as a graveyard is refused. The captured zone IS recorded — it is the
/// announcement's own fact — so the slot's "target spell" requirement is
/// contradicted by it, and an unrecorded target relationship must not
/// blanket-accept that contradiction.
///
/// The first half re-proves the honest direction, so this test also fails when
/// the fix is reverted.
#[test]
fn turn_aside_save_whose_recorded_zone_contradicts_the_stack_slot_is_refused() {
    let ParkedTurnAside {
        runner,
        turn_aside,
        bolt,
        ..
    } = park_turn_aside_at_manual_payment();

    // Honest direction first: the untouched save is admitted.
    save_and_restore(&runner).expect("CR 115.9b: an untouched save is admitted");

    let mut state = runner.state().clone();
    {
        let pending = state
            .pending_cast
            .as_mut()
            .expect("reach-guard: the manual payment park keeps its pending cast");
        let facts = pending
            .committed_target_facts
            .as_deref_mut()
            .expect("reach-guard: the announcement latch is populated");
        match &mut facts[0] {
            ActivationTargetFact::Object { id, zone, .. } => {
                assert_eq!(*id, bolt, "reach-guard: the fact is the announced Bolt");
                assert_eq!(
                    *zone,
                    AnnouncedTargetZone::Known(Zone::Stack),
                    "reach-guard: the recorded zone says stack before the restatement"
                );
                *zone = AnnouncedTargetZone::Known(Zone::Graveyard);
            }
            other => panic!("reach-guard: the fact is an object fact, got {other:?}"),
        }
    }

    let error = PersistedGameState::capture(state)
        .into_game_state()
        .expect_err("a recorded graveyard zone cannot satisfy \"target spell\"");
    match error {
        PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason,
        } => {
            assert_eq!(
                spell_object_id, turn_aside,
                "the refusal names the cast it is about"
            );
            match reason {
                AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter { target } => {
                    assert_eq!(
                        target,
                        TargetRef::Object(bolt),
                        "and the target whose recorded announcement contradicts its own slot filter"
                    );
                }
                other => panic!(
                    "expected the declared-filter refusal that reads the recorded announcement, \
                     got {other:?}"
                ),
            }
        }
        other => panic!("expected a recorded-announcement refusal, got {other:?}"),
    }
}

struct ParkedSwerve {
    runner: GameRunner,
    swerve: ObjectId,
    bolt: ObjectId,
    first_victim: ObjectId,
    second_victim: ObjectId,
    island: ObjectId,
    mountain: ObjectId,
}

/// P1 is the active player; it casts Lightning Bolt at P0's first creature, and
/// P0 answers with Swerve at the Bolt using `CastPaymentMode::Manual`, parking
/// at `WaitingFor::ManaPayment` with a latch whose one fact names the Bolt.
fn park_swerve_at_manual_payment() -> ParkedSwerve {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first_victim = scenario.add_creature(P0, "First Villager", 2, 2).id();
    let second_victim = scenario.add_creature(P1, "Second Villager", 2, 2).id();
    scenario.add_basic_land(P1, ManaColor::Red);
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
        .with_mana_cost(one_mana(ManaColor::Red))
        .id();
    let island = scenario.add_basic_land(P0, ManaColor::Blue);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let swerve = scenario
        .add_spell_to_hand_from_oracle(P0, "Swerve", true, SWERVE_ORACLE)
        .with_mana_cost(blue_red())
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner.cast(bolt).target_objects(&[first_victim]).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach-guard: P0 holds priority with the Bolt on the stack"
    );

    let card_id = runner.state().objects[&swerve].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: swerve,
            card_id,
            targets: vec![bolt],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("CR 115.9a: the Bolt has a single target, so Swerve may target it");
    assert!(
        matches!(cast.waiting_for, WaitingFor::ManaPayment { player: P0, .. }),
        "reach-guard: the cast parks at manual payment, got {:?}",
        cast.waiting_for
    );
    ParkedSwerve {
        runner,
        swerve,
        bolt,
        first_victim,
        second_victim,
        island,
        mountain,
    }
}

/// WITNESS (sibling shape): Swerve's "target spell with a single target" asks
/// the subject's TARGET COUNT, another relationship the announcement never
/// records. An untouched save taken during mana payment restores, resumes, and
/// resolves into the retarget the card is for.
///
/// Revert-probe: with `FilterProp::HasSingleTarget` answered by the record
/// walk's fail-closed `false`, `into_game_state` refuses the save and this test
/// fails at the `save_and_restore` call.
#[test]
fn swerve_save_taken_during_mana_payment_restores_and_resumes_into_its_retarget() {
    let parked = park_swerve_at_manual_payment();

    let declared = parked.runner.state().objects[&parked.swerve]
        .abilities
        .iter()
        .find_map(|ability| ability.effect.target_filter())
        .expect("reach-guard: Swerve declares a target slot");
    assert!(
        filter_has_single_target(declared),
        "reach-guard: the declaring slot filter asks 'with a single target', got {declared:?}"
    );
    match parked_latch_fact(&parked.runner) {
        ActivationTargetFact::Object { id, zone, .. } => {
            assert_eq!(
                *id, parked.bolt,
                "the latch names the spell Swerve announced"
            );
            assert_eq!(
                *zone,
                AnnouncedTargetZone::Known(Zone::Stack),
                "CR 405.1: the announced target was on the stack"
            );
        }
        other => panic!("reach-guard: Swerve's declared target is an object, got {other:?}"),
    }

    let restored = save_and_restore(&parked.runner)
        .expect("CR 115.9a: an untouched Swerve save is admitted, not refused");
    let mut runner = GameRunner::from_state(restored);

    for land in [parked.island, parked.mountain] {
        let (_, _, grouped) = legal_actions_full(runner.state());
        let selection = grouped
            .get(&land)
            .into_iter()
            .flatten()
            .find_map(|action| match action {
                GameAction::TapLandForMana { selection } => Some(selection.clone()),
                _ => None,
            })
            .expect("the restored payment prompt offers this land's own mana");
        runner
            .act(GameAction::TapLandForMana { selection })
            .expect("the restored cast takes the mana it announced it would");
    }
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
    assert_eq!(
        runner.state().objects[&parked.swerve].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );

    // CR 115.7: "Change the target" opens the retarget prompt on resolution;
    // elect the Bolt's other legal target.
    runner.advance_until_stack_empty();
    let WaitingFor::RetargetChoice { .. } = runner.state().waiting_for else {
        panic!(
            "CR 115.7: expected the retarget prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(parked.second_victim))],
        })
        .expect("CR 115.7a: the new target is legal for the Bolt");

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the stack emptied");
    assert_eq!(
        runner.state().objects[&parked.first_victim].zone,
        Zone::Battlefield,
        "the retargeted Bolt no longer damages the announced victim"
    );
    assert_eq!(
        runner.state().objects[&parked.second_victim].zone,
        Zone::Graveyard,
        "CR 115.7: the Bolt deals its 3 damage to the elected target instead"
    );
}

/// The stack-ability capture gap: `casting::capture_target_facts` records an
/// object fact only for a target id that resolves in `state.objects`, and an
/// ACTIVATED or TRIGGERED ability's stack entry carries a freshly minted id that
/// has no object row (CR 405.1: an ability goes on the stack "without any card
/// associated with it"). The announced ability target is therefore DROPPED from
/// the latch.
///
/// The gap cannot refuse an honest save: the omission direction of
/// `reject_contradictory_announced_target_facts` exempts exactly a declared
/// target with no object row (`state.objects.contains_key(id)` is false), so the
/// missing fact reads as UNRECORDED — the same verdict the stack-relationship
/// leaves answer — and never as a contradiction. This witness pins that: the
/// latch is empty, and the untouched save restores and resumes into the counter.
///
/// This is a GAP witness, not a witness of the target-relationship filter fix:
/// it passes with and without that fix, because the fact it would need is never
/// captured at all. The empty-latch assertion is a pin that keeps the gap
/// visible: it fails the moment the capture starts recording such a fact, so
/// whoever closes the gap must update this record deliberately.
#[test]
fn stifle_at_an_ability_save_restores_even_though_the_ability_fact_is_dropped() {
    const PROBE_MAGE: &str = "{1}: ~ gets +1/+1 until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mage = scenario
        .add_creature_from_oracle(P0, "Probe Mage", 1, 1, PROBE_MAGE)
        .id();
    // CR 602.2b: the {1} activation cost needs a mana source P0 controls.
    scenario.add_basic_land(P0, ManaColor::Green);
    let island = scenario.add_basic_land(P1, ManaColor::Blue);
    let stifle = scenario
        .add_spell_to_hand_from_oracle(P1, "Stifle", true, STIFLE_ORACLE)
        .with_mana_cost(one_mana(ManaColor::Blue))
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let ability_index = runner.state().objects[&mage]
        .abilities
        .iter()
        .position(|ability| {
            matches!(ability.kind, engine::types::ability::AbilityKind::Activated)
                && ability.cost.is_some()
        })
        .expect("reach-guard: the Probe Mage has an activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: mage,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 activates the Mage's ability");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: CR 405.1 puts the ability on the stack"
    );
    let ability_entry = runner.state().stack[0].id;
    assert!(
        !runner.state().objects.contains_key(&ability_entry),
        "reach-guard: an ability stack entry has no object row — that is the gap"
    );
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "reach-guard: P1 holds priority with the ability on the stack"
    );

    let card_id = runner.state().objects[&stifle].card_id;
    let cast = runner
        .act(GameAction::CastSpell {
            object_id: stifle,
            card_id,
            targets: vec![ability_entry],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("CR 115.9b: Stifle may target the activated ability on the stack");
    assert!(
        matches!(cast.waiting_for, WaitingFor::ManaPayment { player: P1, .. }),
        "reach-guard: the cast parks at manual payment, got {:?}",
        cast.waiting_for
    );

    // THE GAP: the announced ability target produced no fact, because an ability
    // stack entry is not an object.
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the manual payment park keeps its pending cast");
    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach-guard: the announcement latch is populated at the payment park");
    assert!(
        facts.is_empty(),
        "the ability target has no object row, so the capture drops it; got {facts:?}"
    );

    let restored = save_and_restore(&runner)
        .expect("an unrecorded stack-ability target is not a contradiction");
    let mut runner = GameRunner::from_state(restored);

    // The committed target still rides the cast itself, so the counter has
    // something to counter.
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("the restored park keeps its pending cast");
    assert_eq!(
        pending.ability.targets,
        vec![TargetRef::Object(ability_entry)],
        "the restored cast still names the ability it announced"
    );

    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&island)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the restored payment prompt offers the Island's own {T}: Add {U}");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("the restored cast takes the mana it announced it would");
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the counter resolved");
    assert_eq!(
        runner.state().objects[&stifle].zone,
        Zone::Graveyard,
        "Stifle resolved and went to its owner's graveyard"
    );
    assert_eq!(
        runner.state().objects[&mage].power,
        Some(1),
        "CR 701.5a: the countered ability never resolved, so the Mage never grew"
    );
}
