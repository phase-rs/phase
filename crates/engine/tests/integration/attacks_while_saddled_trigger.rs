//! "Attacks while saddled" trigger-gate coverage — Alacrian Jaguar and its
//! 27-card class, driven through the real declare-attackers / trigger pipeline.
//!
//! CR 702.171a: Saddle is an activated ability (sorcery speed).
//! CR 702.171b: the saddled designation lasts until end of turn or the
//!   permanent leaves the battlefield; it is a marker spells/abilities identify.
//! CR 702.171c: the creatures that saddled the permanent.
//! CR 508.1: attackers are declared as a turn-based action.
//! CR 508.1m: abilities that trigger on attackers being declared trigger then;
//!   the "while saddled" gate is a declaration-time property of the declared
//!   attacker, folded into the attack trigger's `valid_card` and evaluated ONCE
//!   when attackers are declared.
//! Official ruling (2025-02-07): "attacks while saddled" fires only if the
//! creature is saddled when it's declared as an attacker.
//!
//! The gate folds into the attack trigger's `valid_card` as
//! `And { filters: [SelfRef, Typed([IsSaddled])] }`. It is NOT an intervening-if
//! (no printed "if" — CR 603.4 does not apply) and carries NO stored
//! `TriggerCondition`, so once the trigger is on the stack it resolves
//! unconditionally even if its source has since left the battlefield — no LKI
//! recheck is involved.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{FilterProp, TargetFilter, TargetRef, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

// Verbatim Oracle text (Scryfall / card-data.json), not paraphrases — a
// paraphrase could take a different parser branch and mask the real behavior.
const ALACRIAN_JAGUAR: &str = "Vigilance\n\
Whenever this creature attacks while saddled, it gets +2/+2 until end of turn.\n\
Saddle 1 (Tap any number of other creatures you control with total power 1 or more: This Mount becomes saddled until end of turn. Saddle only as a sorcery.)";

const ORNERY_TUMBLEWAGG: &str = "At the beginning of combat on your turn, put a +1/+1 counter on target creature.\n\
Whenever this creature attacks while saddled, double the number of +1/+1 counters on target creature.\n\
Saddle 2 (Tap any number of other creatures you control with total power 2 or more: This Mount becomes saddled until end of turn. Saddle only as a sorcery.)";

const REMOVAL_INSTANT: &str = "Destroy target creature.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Saddle `mount` with `riders` through the real `SaddleMount` announce+pay
/// pipeline (CR 702.171a-c), then resolve the Saddle stack entry.
fn saddle_mount(runner: &mut GameRunner, mount: ObjectId, riders: Vec<ObjectId>) {
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![],
        })
        .expect("entering SaddleMount should succeed at sorcery speed");
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: riders,
        })
        .expect("announcing the saddle should succeed");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&mount].is_saddled,
        "mount must be saddled after Saddle resolves"
    );
}

/// Advance from the current main-phase priority to `DeclareAttackers`, handling
/// any at-the-beginning-of-combat trigger by choosing `aux_target` (used by the
/// Ornery Tumblewagg fixture, whose combat trigger targets a creature).
fn advance_to_declare_attackers(
    runner: &mut GameRunner,
    attacker: PlayerId,
    aux_target: Option<ObjectId>,
) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering combat triggers should succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                let t = aux_target.expect("unexpected combat-trigger target selection");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(t)),
                    })
                    .expect("choosing combat-trigger target should succeed");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward declare attackers");
            }
            other => panic!("unexpected waiting_for advancing to declare attackers: {other:?}"),
        }
    }
    panic!("expected DeclareAttackers");
}

/// Handle target selection for the attacks-while-saddled trigger, choosing
/// `target`. Returns once priority reopens with the trigger on the stack.
fn choose_attack_trigger_target(runner: &mut GameRunner, target: ObjectId) {
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering attack triggers should succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing attack-trigger target should succeed");
                return;
            }
            _ => return,
        }
    }
    panic!("expected the attacks-while-saddled trigger to request a target");
}

/// Locate the triggered-ability stack entry for `source_id`. The OUTER `Option`
/// is `Some` when such an entry exists on the stack; the INNER `Option` is the
/// entry's stored `TriggerCondition` (`None` when it carries no condition). This
/// lets a caller distinguish "no trigger on the stack" from "trigger on the
/// stack with no stored condition" (`Some(None)`) — the latter is what the
/// declaration-time saddled fold produces.
fn stack_condition_for_source(
    runner: &GameRunner,
    source_id: ObjectId,
) -> Option<Option<TriggerCondition>> {
    runner.state().stack.iter().find_map(|entry| {
        if entry.source_id != source_id {
            return None;
        }
        match &entry.kind {
            StackEntryKind::TriggeredAbility { condition, .. } => Some(condition.clone()),
            _ => None,
        }
    })
}

/// Recursively collect leaf `TargetFilter`s under `And`/`Or`/`Not`, so structural
/// assertions survive `TargetFilter::normalized` flattening/reordering.
fn collect_leaf_filters<'a>(filter: &'a TargetFilter, out: &mut Vec<&'a TargetFilter>) {
    match filter {
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            for f in filters {
                collect_leaf_filters(f, out);
            }
        }
        TargetFilter::Not { filter } => collect_leaf_filters(filter, out),
        leaf => out.push(leaf),
    }
}

/// True if any `Typed` leaf anywhere under `filter` carries `FilterProp::IsSaddled`.
fn filter_mentions_is_saddled(filter: &TargetFilter) -> bool {
    let mut leaves = Vec::new();
    collect_leaf_filters(filter, &mut leaves);
    leaves.iter().any(
        |f| matches!(f, TargetFilter::Typed(tf) if tf.properties.contains(&FilterProp::IsSaddled)),
    )
}

/// True if any leaf under `filter` is a `SelfRef`.
fn filter_mentions_self_ref(filter: &TargetFilter) -> bool {
    let mut leaves = Vec::new();
    collect_leaf_filters(filter, &mut leaves);
    leaves.iter().any(|f| matches!(f, TargetFilter::SelfRef))
}

fn add_jaguar(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    // Synthetic 2/2 base so the +2/+2 lands at a clean 4/4 (the real card is 4/4);
    // the ability text is verbatim so the trigger takes the production branch.
    let mut b = scenario.add_creature(player, name, 2, 2);
    b.from_oracle_text_with_keywords(&["Vigilance", "Saddle"], ALACRIAN_JAGUAR);
    b.id()
}

/// Test 1 — saddled, attacks, the gate holds at trigger AND resolution, +2/+2.
#[test]
fn alacrian_jaguar_saddled_attack_pumps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jaguar = add_jaguar(&mut scenario, P0, "Alacrian Jaguar");
    let rider = scenario.add_creature(P0, "Rider", 1, 1).id();
    let mut runner = scenario.build();

    saddle_mount(&mut runner, jaguar, vec![rider]);
    assert_eq!(
        effective_pt(&mut runner, jaguar),
        (2, 2),
        "unpumped base P/T"
    );

    advance_to_declare_attackers(&mut runner, P0, None);
    runner
        .declare_attackers(&[(jaguar, AttackTarget::Player(P1))])
        .expect("saddled Mount should be a legal attacker");
    runner.advance_until_stack_empty();

    assert_eq!(
        effective_pt(&mut runner, jaguar),
        (4, 4),
        "saddled attacker must gain +2/+2 (CR 702.171b gate satisfied)"
    );
}

/// Test 2 — unsaddled: the trigger's subject-state gate lives in `valid_card`
/// (REVERT-FAILING reach-guard) but is false at declaration, so no trigger, no
/// pump.
#[test]
fn alacrian_jaguar_unsaddled_attack_no_pump() {
    // Reach-guard: the parsed attacks trigger MUST carry the saddled qualifier in
    // its `valid_card` (And{SelfRef, Typed([IsSaddled])}) and NO stored condition.
    // Without the fold the gate is dropped, making the "stays 2/2" assertion below
    // vacuous (an unconditional trigger that simply never fired for another
    // reason). These flip if the fix reverts.
    let parsed = engine::parser::oracle::parse_oracle_text(
        ALACRIAN_JAGUAR,
        "Alacrian Jaguar",
        &["Vigilance".to_string(), "Saddle".to_string()],
        &["Creature".to_string()],
        &["Cat".to_string()],
    );
    let attack_trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == engine::types::triggers::TriggerMode::Attacks)
        .expect("Alacrian Jaguar has an attacks trigger");
    assert!(
        attack_trigger.condition.is_none(),
        "attacks-while-saddled gate must NOT be a stored condition, got {:?}",
        attack_trigger.condition
    );
    let valid_card = attack_trigger
        .valid_card
        .as_ref()
        .expect("attacks trigger must carry a valid_card subject filter");
    assert!(
        filter_mentions_self_ref(valid_card),
        "valid_card must retain the SelfRef subject, got {valid_card:?}"
    );
    assert!(
        filter_mentions_is_saddled(valid_card),
        "valid_card must carry the IsSaddled qualifier, got {valid_card:?}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jaguar = add_jaguar(&mut scenario, P0, "Alacrian Jaguar");
    let mut runner = scenario.build();

    // No saddle activation — the Mount is NOT saddled.
    assert!(!runner.state().objects[&jaguar].is_saddled);

    advance_to_declare_attackers(&mut runner, P0, None);
    runner
        .declare_attackers(&[(jaguar, AttackTarget::Player(P1))])
        .expect("unsaddled Mount is still a legal attacker");
    runner.advance_until_stack_empty();

    assert_eq!(
        effective_pt(&mut runner, jaguar),
        (2, 2),
        "an unsaddled attacker must NOT gain +2/+2 — the gate is false at trigger time"
    );
}

/// Test 3 — two identical Mounts in one DeclareAttackers, only A saddled. The
/// gate is per-source, so only A pumps.
#[test]
fn only_saddled_mount_triggers_in_shared_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mount_a = add_jaguar(&mut scenario, P0, "Alacrian Jaguar A");
    let mount_b = add_jaguar(&mut scenario, P0, "Alacrian Jaguar B");
    let rider = scenario.add_creature(P0, "Rider", 1, 1).id();
    let mut runner = scenario.build();

    // Saddle only A.
    saddle_mount(&mut runner, mount_a, vec![rider]);
    assert!(!runner.state().objects[&mount_b].is_saddled);

    advance_to_declare_attackers(&mut runner, P0, None);
    runner
        .declare_attackers(&[
            (mount_a, AttackTarget::Player(P1)),
            (mount_b, AttackTarget::Player(P1)),
        ])
        .expect("both Mounts should be legal attackers");
    runner.advance_until_stack_empty();

    assert_eq!(
        effective_pt(&mut runner, mount_a),
        (4, 4),
        "the saddled Mount must gain +2/+2"
    );
    assert_eq!(
        effective_pt(&mut runner, mount_b),
        (2, 2),
        "the unsaddled Mount sharing the attack must NOT gain +2/+2"
    );
}

/// Test 5 — per-attacker identity. Two saddled Alacrian Jaguars both attack in
/// the same DeclareAttackers. Each attacks-while-saddled trigger pumps ITS OWN
/// source ("it gets +2/+2"), so each ends at exactly (4,4) — not (6,6). Guards
/// against a fold that mis-binds the subject or applies both pumps to one Mount:
/// the And{SelfRef, IsSaddled} `valid_card` matches per-source, so each Mount
/// fires exactly one trigger against itself.
#[test]
fn both_saddled_mounts_each_pump_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mount_a = add_jaguar(&mut scenario, P0, "Alacrian Jaguar A");
    let mount_b = add_jaguar(&mut scenario, P0, "Alacrian Jaguar B");
    let rider_a = scenario.add_creature(P0, "Rider A", 1, 1).id();
    let rider_b = scenario.add_creature(P0, "Rider B", 1, 1).id();
    let mut runner = scenario.build();

    // Saddle both Mounts (each with its own rider; the riders are tapped to pay,
    // the Mounts stay untapped and legal to attack).
    saddle_mount(&mut runner, mount_a, vec![rider_a]);
    saddle_mount(&mut runner, mount_b, vec![rider_b]);

    advance_to_declare_attackers(&mut runner, P0, None);
    runner
        .declare_attackers(&[
            (mount_a, AttackTarget::Player(P1)),
            (mount_b, AttackTarget::Player(P1)),
        ])
        .expect("both saddled Mounts should be legal attackers");
    runner.advance_until_stack_empty();

    assert_eq!(
        effective_pt(&mut runner, mount_a),
        (4, 4),
        "Mount A must gain exactly +2/+2 from its own trigger (not +4/+4)"
    );
    assert_eq!(
        effective_pt(&mut runner, mount_b),
        (4, 4),
        "Mount B must gain exactly +2/+2 from its own trigger (not +4/+4)"
    );
}

/// Test 4 — no-stored-condition / source-death immunity. A saddled Ornery
/// Tumblewagg attacks, its doubling trigger is placed on the stack targeting a
/// counter bearer, then the Mount is DESTROYED in response through the real cast
/// pipeline. Because the saddled gate folded into `valid_card` at declaration
/// (CR 508.1m) and is NOT a stored intervening-if condition, the on-stack
/// trigger carries no condition (`Some(None)`) and resolves unconditionally even
/// after its source has left the battlefield — no CR 603.4 recheck, no LKI.
#[test]
fn ornery_tumblewagg_dies_in_response_trigger_survives() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mount = {
        let mut b = scenario.add_creature(P0, "Ornery Tumblewagg", 2, 2);
        b.from_oracle_text_with_keywords(&["Saddle"], ORNERY_TUMBLEWAGG);
        b.id()
    };
    let rider = scenario.add_creature(P0, "Rider", 2, 2).id();
    // Target of the doubling trigger — starts with N=3 +1/+1 counters.
    let target = scenario.add_creature(P0, "Counter Bearer", 1, 1).id();
    scenario.with_counter(target, CounterType::Plus1Plus1, 3);
    // The removal instant that destroys the Mount in response.
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Bolt", true, REMOVAL_INSTANT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    saddle_mount(&mut runner, mount, vec![rider]);

    // Advance to combat; the beginning-of-combat trigger targets the Mount
    // itself (kept off `target` so N stays deterministic at 3).
    advance_to_declare_attackers(&mut runner, P0, Some(mount));

    let n = p1p1(&runner, target);
    assert_eq!(n, 3, "target must start with N=3 +1/+1 counters");

    runner
        .declare_attackers(&[(mount, AttackTarget::Player(P1))])
        .expect("saddled Mount should be a legal attacker");
    choose_attack_trigger_target(&mut runner, target);

    // On-stack reach-guard (REVERT-FAILING): the doubling trigger is on the
    // stack for `mount` (outer `Some`) and carries NO stored condition (inner
    // `None`) — the saddled gate was consumed at declaration into `valid_card`,
    // not stored for a resolution recheck. Reverting the fold repopulates the
    // inner condition and fails this `Some(None)` assertion.
    assert_eq!(
        stack_condition_for_source(&runner, mount),
        Some(None),
        "the doubling trigger must be on the stack with no stored condition, got {:?}",
        stack_condition_for_source(&runner, mount)
    );

    // Destroy the Mount in response — it leaves the battlefield BEFORE the
    // doubling trigger resolves.
    runner.cast(removal).target_object(mount).resolve();
    assert_eq!(
        runner.state().objects[&mount].zone,
        engine::types::zones::Zone::Graveyard,
        "the Mount must have been destroyed in response"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1(&runner, target),
        2 * n,
        "the doubling trigger must still resolve after its source left the \
         battlefield (no stored condition to recheck) — counters doubled from {n} to {}",
        2 * n
    );
}
