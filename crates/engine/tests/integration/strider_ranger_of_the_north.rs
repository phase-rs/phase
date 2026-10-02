//! "Then if that creature has power/toughness N or greater, …" — a fixed-N
//! power/toughness gate on the earlier instruction's target.
//!
//! CR 608.2c + CR 608.2h: "that creature" names the object the earlier
//! instruction targeted, and the gate reads that object's current power or
//! toughness as the gated instruction resolves — after the earlier pump or
//! counter has applied (CR 613.4c). CR 208.1: power and toughness are the
//! characteristics compared.
//!
//! Cards are staged from their verbatim Oracle text (MTGJSON `AtomicCards.json`):
//!
//! - Strider, Ranger of the North (4/4): "Landfall — Whenever a land you control
//!   enters, target creature gets +1/+1 until end of turn. Then if that creature
//!   has power 4 or greater, it gains first strike until end of turn."
//! - Dormant Grove // Gnarled Grovestrider: "At the beginning of combat on your
//!   turn, put a +1/+1 counter on target creature you control. Then if that
//!   creature has toughness 6 or greater, transform this enchantment."
//!
//! RED AT BASE: the gate falls through to the target-has keyword gate, which
//! fails it closed as `Unimplemented`, so first strike is never granted and the
//! Grove never transforms.

use engine::game::printed_cards::snapshot_object_face;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{ContinuousModification, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const STRIDER_ORACLE: &str = "Landfall — Whenever a land you control enters, target creature gets +1/+1 until end of turn. Then if that creature has power 4 or greater, it gains first strike until end of turn.";

const DORMANT_GROVE_ORACLE: &str = "At the beginning of combat on your turn, put a +1/+1 counter on target creature you control. Then if that creature has toughness 6 or greater, transform this enchantment.";

fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// A short, printable label for a `WaitingFor`, for the observed-sequence record.
fn waiting_label(waiting: &WaitingFor) -> String {
    format!("{waiting:?}").chars().take(96).collect()
}

/// The legal targets of the slot the runner is currently asking about.
fn current_slot_legal_targets(runner: &GameRunner) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        }
        | WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => target_slots
            .get(selection.current_slot)
            .map(|slot| slot.legal_targets.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Drive a real board until the stack is empty: order triggers, declare `want`
/// at the trigger's single target slot, and pass priority otherwise.
fn drive_board(runner: &mut GameRunner, want: TargetRef, what: &str) {
    let mut declared = false;
    let mut observed = Vec::new();
    for _ in 0..60 {
        let waiting = runner.state().waiting_for.clone();
        observed.push(waiting_label(&waiting));
        match waiting {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                assert!(
                    current_slot_legal_targets(runner).contains(&want),
                    "{what}: the wanted target must be legal; observed {observed:#?}"
                );
                declared = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(want.clone()),
                    })
                    .unwrap_or_else(|err| panic!("{what}: slot declaration rejected: {err:?}"));
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && declared => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance resolution");
            }
            other => panic!("{what}: unexpected {other:?}; observed {observed:#?}"),
        }
    }
    panic!("{what}: the stack did not empty in 60 steps; observed {observed:#?}");
}

/// Objects named directly by a transient continuous effect carrying a
/// modification `pred` accepts, in effect order.
fn tce_recipients_where(
    runner: &GameRunner,
    pred: impl Fn(&ContinuousModification) -> bool,
) -> Vec<ObjectId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|tce| tce.modifications.iter().any(&pred))
        .filter_map(|tce| match tce.affected {
            TargetFilter::SpecificObject { id } => Some(id),
            _ => None,
        })
        .collect()
}

fn grants_first_strike(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddKeyword {
            keyword: Keyword::FirstStrike
        }
    )
}

fn adds_power(modification: &ContinuousModification) -> bool {
    matches!(modification, ContinuousModification::AddPower { .. })
}

fn power(runner: &GameRunner, id: ObjectId) -> Option<i32> {
    runner.state().objects[&id].power
}

/// Strider on P0's battlefield, a Forest in P0's hand, and the extra creatures
/// `extra` (controller, name, power, toughness). Plays the Forest and declares
/// `extra[target_index]` for the landfall trigger, driving it to resolution.
fn strider_landfall(
    extra: &[(PlayerId, &str, i32, i32)],
    target_index: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let strider = scenario
        .add_creature_from_oracle(P0, "Strider, Ranger of the North", 4, 4, STRIDER_ORACLE)
        .with_subtypes(vec!["Human", "Ranger"])
        .id();
    let ids: Vec<ObjectId> = extra
        .iter()
        .map(|&(player, name, p, t)| scenario.add_creature(player, name, p, t).id())
        .collect();
    let forest = scenario
        .add_land_to_hand(P0, "Forest")
        .with_subtypes(vec!["Forest"])
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("the Forest must be playable");
    drive_board(
        &mut runner,
        TargetRef::Object(ids[target_index]),
        "Strider landfall",
    );
    (runner, strider, ids)
}

/// CR 608.2c + CR 608.2h: a 3/3 pumped to 4/4 meets "power 4 or greater" as
/// the rider resolves, so "it" — the pumped target — gains first strike.
/// Discriminates both the parse (at base the gate is `Unimplemented`, granting
/// nothing) and the timing (the pre-pump power 3 fails the gate).
#[test]
fn pumped_to_four_gains_first_strike() {
    let (runner, strider, ids) = strider_landfall(&[(P0, "Bear", 3, 3)], 0);
    let bear = ids[0];

    assert_eq!(
        power(&runner, bear),
        Some(4),
        "reach guard: the +1/+1 pump resolved on the declared creature"
    );
    assert_eq!(
        tce_recipients_where(&runner, grants_first_strike),
        vec![bear],
        "CR 608.2c: the pumped target has power 4, so it alone gains first strike"
    );
    assert!(
        !tce_recipients_where(&runner, grants_first_strike).contains(&strider),
        "the trigger's source gains nothing"
    );
}

/// CR 608.2c: a 2/2 pumped to 3/3 fails "power 4 or greater", so no first
/// strike is granted.
#[test]
fn pumped_to_three_does_not_gain_first_strike() {
    let (runner, _strider, ids) = strider_landfall(&[(P0, "Bear", 2, 2)], 0);
    let bear = ids[0];

    assert_eq!(
        power(&runner, bear),
        Some(3),
        "reach guard: the +1/+1 pump resolved on the declared creature"
    );
    assert_eq!(
        tce_recipients_where(&runner, adds_power),
        vec![bear],
        "reach guard: the pump's continuous effect names the declared creature"
    );
    assert_eq!(
        tce_recipients_where(&runner, grants_first_strike),
        Vec::<ObjectId>::new(),
        "CR 608.2c: power 3 fails the gate, so nothing gains first strike"
    );
}

/// CR 608.2c + CR 115.1: the gate reads the declared target, not the trigger's
/// source (Strider, power 4) or a power-5 bystander. The declared 1/1 is pumped
/// only to 2/2, so nothing gains first strike.
#[test]
fn gate_reads_the_target_not_the_source_or_bystander() {
    let (runner, _strider, ids) =
        strider_landfall(&[(P0, "Bystander", 5, 5), (P1, "Opp Pup", 1, 1)], 1);
    let pup = ids[1];

    assert_eq!(
        power(&runner, pup),
        Some(2),
        "reach guard: the +1/+1 pump resolved on the declared opposing creature"
    );
    assert_eq!(
        tce_recipients_where(&runner, grants_first_strike),
        Vec::<ObjectId>::new(),
        "CR 608.2c: only the declared target's power is compared; neither the \
         source's nor the bystander's power may satisfy the gate"
    );
}

/// Dormant Grove with its Gnarled Grovestrider back face installed, beside a P0
/// creature of the given size and a P0 toughness-6 bystander. Advances to P0's beginning of combat, declares
/// the creature for the counter trigger, and drives it to resolution.
fn dormant_grove_combat(power: i32, toughness: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grove = scenario
        .add_enchantment_from_oracle(P0, "Dormant Grove", DORMANT_GROVE_ORACLE)
        .id();
    // The donor supplies the back face (pattern: cr733_resolved_transform.rs).
    let donor = scenario
        .add_creature(P1, "Gnarled Grovestrider", 3, 6)
        .with_subtypes(vec!["Treefolk"])
        .id();
    let creature = scenario.add_creature(P0, "Tree", power, toughness).id();
    // A second legal recipient forces a real target declaration, and its
    // toughness 6 would satisfy the gate if it were (wrongly) the one read.
    scenario.add_creature(P0, "Bystander Tree", 6, 6);
    let mut runner = scenario.build();
    let back_face = snapshot_object_face(&runner.state().objects[&donor]);
    runner
        .state_mut()
        .objects
        .get_mut(&grove)
        .expect("Dormant Grove exists")
        .back_face = Some(back_face);

    runner.advance_to_phase(Phase::BeginCombat);
    drive_board(
        &mut runner,
        TargetRef::Object(creature),
        "Dormant Grove beginning of combat",
    );
    (runner, grove, creature)
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// CR 608.2c + CR 608.2h + CR 701.27a: a 5/5 that receives the +1/+1 counter has
/// toughness 6 as the rider resolves, so Dormant Grove transforms.
#[test]
fn dormant_grove_toughness_six_transforms() {
    let (runner, grove, creature) = dormant_grove_combat(5, 5);

    assert_eq!(
        plus_one_counters(&runner, creature),
        1,
        "reach guard: the counter landed on the declared creature"
    );
    assert_eq!(
        runner.state().objects[&creature].toughness,
        Some(6),
        "reach guard: the declared creature has toughness 6"
    );
    let grove_obj = &runner.state().objects[&grove];
    assert!(
        grove_obj.transformed,
        "CR 701.27a: toughness 6 meets the gate, so Dormant Grove transforms"
    );
    assert_eq!(grove_obj.name, "Gnarled Grovestrider");
}

/// CR 608.2c: a 4/4 that receives the counter has toughness 5, failing
/// "toughness 6 or greater", so Dormant Grove does not transform — the
/// toughness-6 bystander is not "that creature".
#[test]
fn dormant_grove_toughness_five_does_not_transform() {
    let (runner, grove, creature) = dormant_grove_combat(4, 4);

    assert_eq!(
        plus_one_counters(&runner, creature),
        1,
        "reach guard: the counter landed on the declared creature"
    );
    assert_eq!(
        runner.state().objects[&creature].toughness,
        Some(5),
        "reach guard: the declared creature has toughness 5"
    );
    let grove_obj = &runner.state().objects[&grove];
    assert!(
        !grove_obj.transformed,
        "CR 608.2c: toughness 5 fails the gate, so Dormant Grove stays untransformed"
    );
    assert_eq!(grove_obj.name, "Dormant Grove");
}
