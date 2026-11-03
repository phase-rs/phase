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
//! - Yavimaya Bloomsage // Channel (2/2, prepare layout): "At the beginning of
//!   your end step, put a +1/+1 counter on target creature you control. Then if
//!   that creature has power 7 or greater, this creature becomes prepared."
//!
//! RED AT BASE: the gate falls through to the target-has keyword gate, which
//! fails it closed as `Unimplemented`, so first strike is never granted, the
//! Grove never transforms, and the Bloomsage never becomes prepared.

use engine::game::printed_cards::snapshot_object_face;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{ContinuousModification, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const STRIDER_ORACLE: &str = "Landfall — Whenever a land you control enters, target creature gets +1/+1 until end of turn. Then if that creature has power 4 or greater, it gains first strike until end of turn.";

const DORMANT_GROVE_ORACLE: &str = "At the beginning of combat on your turn, put a +1/+1 counter on target creature you control. Then if that creature has toughness 6 or greater, transform this enchantment.";

const YAVIMAYA_BLOOMSAGE_ORACLE: &str = "At the beginning of your end step, put a +1/+1 counter on target creature you control. Then if that creature has power 7 or greater, this creature becomes prepared. (While it's prepared, you may cast a copy of its spell. Doing so unprepares it.)";

/// The prepare spell printed in Yavimaya Bloomsage's inset frame.
const CHANNEL_ORACLE: &str = "Until end of turn, any time you could activate a mana ability, you may pay 1 life. If you do, add {C}.";

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
    let declared = drive_board_declaring(runner, &[want], what);
    assert_eq!(
        declared, 1,
        "{what}: the trigger has exactly one target slot"
    );
}

/// [`drive_board`] for a trigger that may announce several targets: declares
/// `wants` in order, one per target slot the engine asks about, and returns how
/// many it declared. A `want` left over means the trigger asked for fewer slots.
fn drive_board_declaring(runner: &mut GameRunner, wants: &[TargetRef], what: &str) -> usize {
    let mut declared = 0;
    let mut observed = Vec::new();
    for _ in 0..60 {
        let waiting = runner.state().waiting_for.clone();
        observed.push(waiting_label(&waiting));
        match waiting {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let want = wants.get(declared).unwrap_or_else(|| {
                    panic!("{what}: more target slots than wanted targets; observed {observed:#?}")
                });
                assert!(
                    current_slot_legal_targets(runner).contains(want),
                    "{what}: the wanted target must be legal; observed {observed:#?}"
                );
                declared += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(want.clone()),
                    })
                    .unwrap_or_else(|err| panic!("{what}: slot declaration rejected: {err:?}"));
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && declared > 0 => {
                return declared;
            }
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

fn is_prepared(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].prepared.is_some()
}

/// Yavimaya Bloomsage on P0's battlefield beside a P0 recipient of the given
/// size and a P0 power-7 bystander. All three carry the Channel prepare spell
/// as a `LayoutKind::Prepare` back face (CR 722.2a), so each is eligible to
/// become prepared (CR 722.3a) and only the gate's resolution decides which one
/// does. Advances to P0's end step, declares the recipient for the counter
/// trigger, and drives it to resolution. Returns `(runner, bloomsage,
/// recipient, bystander)`.
fn bloomsage_end_step(power: i32, toughness: i32) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let bloomsage = scenario
        .add_creature_from_oracle(P0, "Yavimaya Bloomsage", 2, 2, YAVIMAYA_BLOOMSAGE_ORACLE)
        .with_subtypes(vec!["Dryad", "Druid"])
        .id();
    // The donor supplies the prepare-spell face (pattern: the Dormant Grove
    // back face above; `LayoutKind::Prepare` as in fra_bloodline_recollector.rs).
    let donor = scenario
        .add_spell_to_hand_from_oracle(P1, "Channel", false, CHANNEL_ORACLE)
        .id();
    let recipient = scenario
        .add_creature(P0, "Elder Oak", power, toughness)
        .id();
    // A second legal recipient forces a real target declaration, and its power
    // 7 would satisfy the gate if it were (wrongly) the one read.
    let bystander = scenario.add_creature(P0, "Bystander Oak", 7, 7).id();
    let mut runner = scenario.build();
    let mut prepare_face = snapshot_object_face(&runner.state().objects[&donor]);
    prepare_face.layout_kind = Some(LayoutKind::Prepare);
    for id in [bloomsage, recipient, bystander] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("prepare-faced creature exists")
            .back_face = Some(prepare_face.clone());
    }

    runner.advance_to_phase(Phase::End);
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach guard: the game reached P0's end step"
    );
    for id in [bloomsage, recipient, bystander] {
        assert!(
            !is_prepared(&runner, id),
            "no creature is prepared before the end-step trigger resolves"
        );
    }
    drive_board(
        &mut runner,
        TargetRef::Object(recipient),
        "Yavimaya Bloomsage end step",
    );
    (runner, bloomsage, recipient, bystander)
}

/// CR 608.2c + CR 608.2h + CR 722.3a: a 6/6 recipient that receives the +1/+1
/// counter has power 7 as the rider resolves, so "this creature" — the
/// Bloomsage, the trigger's source — becomes prepared. The recipient and the
/// bystander, though both have prepare spells, do not.
#[test]
fn bloomsage_power_seven_prepares_the_source() {
    let (runner, bloomsage, recipient, bystander) = bloomsage_end_step(6, 6);

    assert_eq!(
        plus_one_counters(&runner, recipient),
        1,
        "reach guard: the counter landed on the declared recipient"
    );
    assert_eq!(
        power(&runner, recipient),
        Some(7),
        "reach guard: the declared recipient has power 7"
    );
    assert!(
        is_prepared(&runner, bloomsage),
        "CR 722.3a: power 7 meets the gate, so the Bloomsage becomes prepared"
    );
    assert!(
        !is_prepared(&runner, recipient),
        "\"this creature\" is the source; the counter's recipient is not prepared"
    );
    assert!(
        !is_prepared(&runner, bystander),
        "the bystander is neither the source nor the recipient"
    );
}

/// CR 608.2c: a 5/5 recipient that receives the counter has power 6, failing
/// "power 7 or greater", so the Bloomsage stays unprepared — the power-7
/// bystander is not "that creature".
#[test]
fn bloomsage_power_six_leaves_the_source_unprepared() {
    let (runner, bloomsage, recipient, bystander) = bloomsage_end_step(5, 5);

    assert_eq!(
        plus_one_counters(&runner, recipient),
        1,
        "reach guard: the counter landed on the declared recipient"
    );
    assert_eq!(
        power(&runner, recipient),
        Some(6),
        "reach guard: the declared recipient has power 6"
    );
    assert!(
        !is_prepared(&runner, bloomsage),
        "CR 608.2c: power 6 fails the gate, so the Bloomsage stays unprepared"
    );
    assert!(
        !is_prepared(&runner, recipient),
        "the counter's recipient is not prepared"
    );
    assert!(
        !is_prepared(&runner, bystander),
        "the bystander is not prepared"
    );
}

/// A synthetic rider in the class Strider prints, but whose gated instruction
/// announces a target of its own. "That creature" is the counter's recipient;
/// the rider's "target creature" is a different object.
const OWN_TARGET_RIDER_ORACLE: &str = "Whenever a land you control enters, put a +1/+1 counter on target creature you control. Then if that creature has power 4 or greater, destroy target creature.";

/// A creature carrying [`OWN_TARGET_RIDER_ORACLE`] on P0's battlefield, a P0
/// counter recipient and a P1 destroy candidate of the given sizes, and a Forest
/// in P0's hand. Plays the Forest and offers the recipient then the candidate
/// for whatever target slots the landfall trigger announces, driving it to
/// resolution, and asserts exactly one slot was announced. Returns
/// `(runner, source, recipient, candidate)`.
fn own_target_rider_landfall(
    recipient_size: (i32, i32),
    candidate_size: (i32, i32),
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Test Warden", 2, 2, OWN_TARGET_RIDER_ORACLE)
        .id();
    let recipient = scenario
        .add_creature(P0, "Recipient", recipient_size.0, recipient_size.1)
        .id();
    let candidate = scenario
        .add_creature(P1, "Candidate", candidate_size.0, candidate_size.1)
        .id();
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
    let declared = drive_board_declaring(
        &mut runner,
        &[TargetRef::Object(recipient), TargetRef::Object(candidate)],
        "own-target rider landfall",
    );
    // The fail-closed rider announces no target: the trigger asks only for the
    // counter's recipient, so the candidate is never declared.
    assert_eq!(
        declared, 1,
        "own-target rider landfall: only the counter's target slot is announced"
    );
    (runner, source, recipient, candidate)
}

/// Asserts the counter landed on the recipient (reach guard) and that no
/// creature left the battlefield or reached a graveyard.
fn assert_rider_destroyed_nothing(
    runner: &GameRunner,
    source: ObjectId,
    recipient: ObjectId,
    candidate: ObjectId,
) {
    assert_eq!(
        plus_one_counters(runner, recipient),
        1,
        "reach guard: the first instruction put its counter on the recipient"
    );
    let state = runner.state();
    for id in [source, recipient, candidate] {
        assert!(
            state.battlefield.contains(&id),
            "{}: nothing is destroyed",
            state.objects[&id].name
        );
    }
    for player in &state.players {
        assert!(
            player.graveyard.is_empty(),
            "no creature reached a graveyard: {:?}",
            player.graveyard
        );
    }
}

/// CR 115.1 + CR 608.2c: the gate's "that creature" is the counter's recipient,
/// but its `TargetMatchesFilter { subject_slot: None }` is evaluated both
/// against the announcing instruction and against the rider's own first object
/// target, so without a guard it destroys only when BOTH objects qualify. The
/// rider therefore fails closed. Here the recipient (3/3 → 4/4) qualifies and
/// the candidate (2/2) does not: nothing is destroyed.
#[test]
fn own_target_rider_with_qualifying_recipient_destroys_nothing() {
    let (runner, source, recipient, candidate) = own_target_rider_landfall((3, 3), (2, 2));
    assert_eq!(
        power(&runner, recipient),
        Some(4),
        "reach guard: the recipient qualifies for the gate"
    );
    assert_rider_destroyed_nothing(&runner, source, recipient, candidate);
}

/// CR 115.1 + CR 608.2c: the opposite pair — the recipient (1/1 → 2/2) fails
/// "power 4 or greater" while the candidate (5/5) would pass it. The candidate's
/// power never decides the outcome: nothing is destroyed.
#[test]
fn own_target_rider_with_qualifying_candidate_destroys_nothing() {
    let (runner, source, recipient, candidate) = own_target_rider_landfall((1, 1), (5, 5));
    assert_eq!(
        power(&runner, recipient),
        Some(2),
        "reach guard: the recipient fails the gate"
    );
    assert_rider_destroyed_nothing(&runner, source, recipient, candidate);
}

/// CR 115.1 + CR 608.2c: both the recipient (3/3 → 4/4) and the candidate (5/5)
/// meet "power 4 or greater". Without the guard the misbound gate passes on
/// both reads and destroys the candidate; the fail-closed rider destroys
/// nothing, so this is the fixture that proves the guard is taken.
#[test]
fn own_target_rider_with_both_qualifying_destroys_nothing() {
    let (runner, source, recipient, candidate) = own_target_rider_landfall((3, 3), (5, 5));
    assert_eq!(
        power(&runner, recipient),
        Some(4),
        "reach guard: the recipient qualifies for the gate"
    );
    assert_rider_destroyed_nothing(&runner, source, recipient, candidate);
}
