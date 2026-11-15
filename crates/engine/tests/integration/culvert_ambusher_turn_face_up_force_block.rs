//! RUNTIME witnesses for the disjunctive trigger-event head family, via Culvert Ambusher.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, TargetFilter, TargetRef};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const CULVERT_AMBUSHER: &str = "When this creature enters or is turned face up, target creature \
                                blocks this turn if able.\nDisguise {4}{G} (You may cast this \
                                card face down for {3} as a 2/2 creature with ward {2}. Turn it \
                                face up any time for its disguise cost.)";

fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn must_block_targets(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            tce.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::MustBlock,
                    }
                )
            })
        })
        // PANIC rather than discard on an unexpected `affected` shape. Silently
        // dropping it would make every assertion below vacuous in the one case that
        // matters: a MustBlock effect that really did apply, to a filter this helper
        // does not understand, would read as "no MustBlock at all" and the negative
        // rows would pass for the wrong reason.
        .map(|tce| match tce.affected {
            TargetFilter::SpecificObject { id } => id,
            ref other => panic!(
                "ForceBlock is expected to apply to a specific object (CR 509.1c); \
                 got affected={other:?} — this helper's assertions would be vacuous"
            ),
        })
        .collect()
}

/// Drain trigger target selection, preferring `want` when it is legal.
/// A bear-selecting drain, NOT `drain_trigger_targets`: the face-up Ambusher is
/// itself a legal "target creature", so a first-legal drain could pick it.
/// Returns true if a `TriggerTargetSelection` was ever observed (CR 603.3d).
fn drain_choosing(runner: &mut GameRunner, want: ObjectId) -> bool {
    let mut saw = false;
    for _ in 0..32 {
        let pick = match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                saw = true;
                let slot = &target_slots[selection.current_slot];
                slot.legal_targets
                    .iter()
                    .find(|t| matches!(t, TargetRef::Object(id) if *id == want))
                    .or_else(|| slot.legal_targets.first())
                    .cloned()
            }
            _ => break,
        };
        runner
            .act(GameAction::ChooseTarget { target: pick })
            .expect("choose a legal target for the trigger");
    }
    saw
}

#[test]
fn disguise_flip_fires_force_block_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let ambusher = scenario
        .add_creature_to_hand_from_oracle(P0, "Culvert Ambusher", 4, 5, CULVERT_AMBUSHER)
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&ambusher].card_id;
    add_mana(&mut runner, ManaType::Green, 12);
    runner
        .act(GameAction::PlayFaceDown {
            object_id: ambusher,
            card_id,
        })
        .expect("cast face down for {3}");
    assert!(
        runner.state().objects[&ambusher].face_down,
        "reach-guard: must actually be face down on the battlefield"
    );
    assert_eq!(runner.state().objects[&ambusher].zone, Zone::Battlefield);
    // CR 708.2 + CR 708.3: a face-down permanent has no abilities, so nothing fired yet.
    assert!(
        must_block_targets(&runner).is_empty(),
        "face-down entry must not force a block"
    );

    runner
        .act(GameAction::TurnFaceUp {
            object_id: ambusher,
            x: 0,
        })
        .expect("turn face up for its disguise cost");
    assert!(
        !runner.state().objects[&ambusher].face_down,
        "reach-guard: must actually be face up"
    );

    let saw = drain_choosing(&mut runner, bear);
    assert!(
        saw,
        "reach-guard: a turn-face-up trigger must have gone on the stack and asked for a target"
    );
    runner.advance_until_stack_empty();

    assert!(
        must_block_targets(&runner).contains(&bear),
        "CR 509.1c: the chosen creature must carry MustBlock after the flip, got {:?}",
        must_block_targets(&runner)
    );
}

#[test]
fn entering_face_up_still_forces_block() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let ambusher = scenario
        .add_creature_to_hand_from_oracle(P0, "Culvert Ambusher", 4, 5, CULVERT_AMBUSHER)
        .id();
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Green, 12);
    // CR 702.168a: disguise offers an alternative cast; take the printed cost.
    // (Without this the SpellCast driver panics at WaitingFor::AlternativeCastChoice.)
    runner
        .cast(ambusher)
        .alternative_cast(AlternativeCastDecision::Normal)
        .target_object(bear)
        .resolve();
    // The cast driver may already have submitted the ETB trigger's target; drain any
    // remainder without asserting on it (the reach-guard below is the resolution witness).
    drain_choosing(&mut runner, bear);
    runner.advance_until_stack_empty();

    // Reach-guard: the spell must actually have resolved onto the battlefield face up,
    // otherwise "MustBlock present" below could pass or fail for the wrong reason.
    assert_eq!(runner.state().objects[&ambusher].zone, Zone::Battlefield);
    assert!(!runner.state().objects[&ambusher].face_down);

    assert!(
        must_block_targets(&runner).contains(&bear),
        "the ETB half must still work after the split, got {:?}",
        must_block_targets(&runner)
    );
    // CR 603.2e + CR 708.8: the split puts TWO arms on this card, so the ETB
    // direction must also prove the turn-face-up arm did not co-fire. Entering the
    // battlefield face up is not a turn-face-up event. Mirrors the opposite-direction
    // count in `turn_face_up_fires_exactly_one_force_block`.
    assert_eq!(
        must_block_targets(&runner).len(),
        1,
        "exactly one arm may fire on entering face up, got {:?}",
        must_block_targets(&runner)
    );
}

#[test]
fn turn_face_up_fires_exactly_one_force_block() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let ambusher = scenario
        .add_creature_to_hand_from_oracle(P0, "Culvert Ambusher", 4, 5, CULVERT_AMBUSHER)
        .id();
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&ambusher].card_id;
    add_mana(&mut runner, ManaType::Green, 12);
    runner
        .act(GameAction::PlayFaceDown {
            object_id: ambusher,
            card_id,
        })
        .expect("cast face down");
    runner
        .act(GameAction::TurnFaceUp {
            object_id: ambusher,
            x: 0,
        })
        .expect("flip");
    assert!(drain_choosing(&mut runner, bear));
    runner.advance_until_stack_empty();

    // CR 708.8 + CR 702.168d: turning face up is not an enters event, so only the
    // TurnFaceUp half fires — exactly one MustBlock, not two.
    assert_eq!(
        must_block_targets(&runner).len(),
        1,
        "exactly one arm may fire on one turn-face-up event, got {:?}",
        must_block_targets(&runner)
    );
}

/// CR 603.2e: an ability that triggers when a permanent "becomes tapped" does NOT
/// trigger if the permanent ENTERS the battlefield tapped. The split puts an ETB
/// arm and a `Taps` arm on ONE card (Champions of the Shoal class), so this is the
/// one new rules interaction it introduces. The engine is safe because the ETB tap
/// state is applied inside the zone-change pipeline — the CR 614.1 enter-tapped arm
/// of `zone_pipeline::deliver_replaced_zone_change`
/// routes it through `object_state::resolve_and_apply_object_edit(.., Tapped, true)`
/// and pushes NO event — while `GameEvent::PermanentTapped` is emitted only by tap
/// ACTIONS (combat.rs, mana_sources.rs, casting_costs.rs, effects/tap_untap.rs,
/// restrictions.rs, engine_replacement.rs's `ProposedEvent::Tap` arm).
/// Measured non-vacuous: injecting a `PermanentTapped` push at that site reds this
/// test (the second arm demands a target the fixture never declares).
#[test]
fn entering_tapped_does_not_fire_the_becomes_tapped_arm() {
    // Champions-of-the-Shoal-shaped trigger head, with an enters-tapped rider and
    // the force-block payload so the arms are individually countable.
    const ENTERS_TAPPED_AMBUSHER: &str = "This creature enters tapped.\nWhenever this creature \
                                          enters or becomes tapped, target creature blocks this \
                                          turn if able.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let subject = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Shoal Ambusher Fixture",
            4,
            5,
            ENTERS_TAPPED_AMBUSHER,
        )
        .id();
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Green, 12);
    runner.cast(subject).target_object(bear).resolve();
    drain_choosing(&mut runner, bear);
    runner.advance_until_stack_empty();

    // Reach-guards: it must actually be on the battlefield AND actually be tapped,
    // otherwise CR 603.2e is not even engaged and the count below is vacuous.
    assert_eq!(runner.state().objects[&subject].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&subject].tapped,
        "reach-guard: the fixture must have entered the battlefield TAPPED"
    );
    // Positive half: the ETB arm did fire, so the count is a real discriminator.
    assert!(
        must_block_targets(&runner).contains(&bear),
        "the enters arm must fire, got {:?}",
        must_block_targets(&runner)
    );
    // CR 603.2e: the "becomes tapped" arm must NOT also have fired.
    assert_eq!(
        must_block_targets(&runner).len(),
        1,
        "entering tapped must not fire the becomes-tapped arm, got {:?}",
        must_block_targets(&runner)
    );
}
