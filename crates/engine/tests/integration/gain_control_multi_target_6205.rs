//! Issue #6205 — "gain control of up to N target …" dropped its target count.
//!
//! CR 115.1d + CR 601.2c: a spell or ability that says "up to N target …" offers up to N
//! target slots. `parse_targeted_action_ast`'s `gain control of ` branch called
//! `strip_optional_target_prefix` — which already parses the quantifier — but
//! bound the returned `MultiTargetSpec` to `_`, so the count never reached
//! `ParsedEffectClause.multi_target`. Every other "up to N target" shape kept
//! its count (Call of the Death-Dweller 2, Patch Up 3, The War in Heaven 3), so
//! the defect was specific to this one verb path. It spans 7 cards, of which
//! three were genuinely under-targeted:
//!
//!   * The Super Hero Civil War — "Gain control of up to two target creatures
//!     with total mana value 6 or less" parsed max = 1 (only one selectable).
//!   * Jace, Ingenious Mind-Mage — "Gain control of up to three target
//!     creatures" parsed no count at all.
//!   * Domineering Will — "target player gains control of up to three target
//!     nonattacking creatures"; `GiveControl` shares these parse paths, so it
//!     was capped at one as well. Parse-level only: the give-control path does
//!     not surface creature target slots at runtime for an unrelated reason,
//!     documented with the runtime section at the bottom of this file.
//!
//! The remaining four ("up to one target": Pyreswipe Hawk, Rangers of Ithilien,
//! Scroll of Isildur, Jon Irenicus) already selected correctly and only change
//! representation — from an implicit "optional (up to)" marker to an explicit
//! 0..=1 range. `up_to_one_stays_optional_with_an_explicit_upper_bound` pins
//! that their optionality (min 0) survives that move.
//!
//! The `TotalManaValue` target constraint was always parsed correctly; only the
//! count was lost, which is why the bug reads as "can only select one creature".
//!
//! Revert-proof: restoring the discarded binding drops `multi_target` back to
//! `None`/1 and the count assertions below fail.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::ability::{Effect, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, PersistedGameState, RetargetScope, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Parse `oracle` and return the `(min, max)` of the `multi_target` spec on the
/// first ability or trigger whose effect transfers control — `GainControl` (the
/// controller takes it) or `GiveControl` ("target player gains control of …").
/// Both lower through the same `gain control of ` parse paths, so both are part
/// of this class.
fn control_target_range(
    oracle: &str,
    name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> Option<(u32, Option<u32>)> {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);

    let is_control =
        |e: &Effect| matches!(e, Effect::GainControl { .. } | Effect::GiveControl { .. });
    let fixed = |q: &QuantityExpr| match q {
        QuantityExpr::Fixed { value } => Some(*value as u32),
        _ => None,
    };
    let range = |spec: &Option<engine::types::ability::MultiTargetSpec>| {
        spec.as_ref().map(|s| {
            (
                fixed(&s.min).unwrap_or(u32::MAX),
                s.max.as_ref().and_then(fixed),
            )
        })
    };

    for ability in &parsed.abilities {
        if is_control(&ability.effect) {
            return range(&ability.multi_target);
        }
    }
    for trigger in &parsed.triggers {
        if let Some(exec) = trigger.execute.as_ref() {
            if is_control(&exec.effect) {
                return range(&exec.multi_target);
            }
        }
    }
    None
}

/// Just the max, for the cases where optionality is not the point.
fn gain_control_max(oracle: &str, name: &str, types: &[&str], subtypes: &[&str]) -> Option<u32> {
    control_target_range(oracle, name, types, subtypes).and_then(|(_, max)| max)
}

#[test]
fn saga_chapter_gain_control_keeps_up_to_two() {
    // The Super Hero Civil War, chapter I. The trailing duration clause and the
    // `total mana value` constraint both survive today; only the count was lost.
    let max = gain_control_max(
        "I — Gain control of up to two target creatures with total mana value 6 or less \
         for as long as this Saga remains on the battlefield.",
        "The Super Hero Civil War",
        &["Enchantment"],
        &["Saga"],
    );
    assert_eq!(
        max,
        Some(2),
        "\"up to two target creatures\" must offer 2 slots, got {max:?}"
    );
}

#[test]
fn loyalty_gain_control_keeps_up_to_three() {
    // Jace, Ingenious Mind-Mage — the same verb path, a different count, and no
    // constraint clause: pins that the fix is the verb path, not the constraint.
    // Verbatim ultimate line (Scryfall Oracle), including the U+2212 minus.
    let max = gain_control_max(
        "−9: Gain control of up to three target creatures.",
        "Jace, Ingenious Mind-Mage",
        &["Planeswalker"],
        &["Jace"],
    );
    assert_eq!(
        max,
        Some(3),
        "\"up to three target creatures\" must offer 3 slots, got {max:?}"
    );
}

#[test]
fn single_target_gain_control_declares_no_count() {
    // Discriminating guard: the common single-target form must NOT acquire a
    // count, so the fix cannot be "always attach a multi_target".
    let max = gain_control_max(
        "Gain control of target creature until end of turn.",
        "Act of Treason",
        &["Sorcery"],
        &[],
    );
    assert_eq!(
        max, None,
        "single-target gain control must not declare a count, got {max:?}"
    );
}

#[test]
fn up_to_one_stays_optional_with_an_explicit_upper_bound() {
    // The largest affected subgroup (Pyreswipe Hawk, Rangers of Ithilien, Scroll
    // of Isildur). These already targeted correctly; the fix moves them from an
    // implicit "optional (up to)" marker to an EXPLICIT 0..=1 range, so the risk
    // here is losing optionality, not losing the count. CR 601.2c: "up to one"
    // permits choosing zero targets, so min must stay 0.
    let range = control_target_range(
        "Whenever you expend 6, gain control of up to one target artifact \
         for as long as you control this creature.",
        "Pyreswipe Hawk",
        &["Creature"],
        &["Bird"],
    );
    assert_eq!(
        range,
        Some((0, Some(1))),
        "\"up to one target\" must stay optional (min 0) with max 1, got {range:?}"
    );
}

#[test]
fn give_control_keeps_up_to_three() {
    // Domineering Will — "target player gains control of up to three target
    // nonattacking creatures". The same `gain control of ` parse paths serve
    // `GiveControl`, so this card was capped at one target too; it is a third
    // genuinely-broken member of the class, not just a representation change.
    let max = gain_control_max(
        "Target player gains control of up to three target nonattacking creatures \
         until end of turn. Untap those creatures. They block this turn if able.",
        "Domineering Will",
        &["Instant"],
        &[],
    );
    assert_eq!(
        max,
        Some(3),
        "\"up to three target\" give-control must offer 3 slots, got {max:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// RUNTIME regression — the parser tests above assert the AST shape; this one
// asserts the production consequence. `collect_target_slots`
// (`game/ability_utils.rs`) pushes ONE `TargetSelectionSlot` per unit of the
// ability's `multi_target` bound, so a dropped count means the pipeline offers
// exactly one slot and only one creature can ever change controller — which is
// the defect as reported (#6205). The witness selects DISTINCT legal targets
// through the ordinary `WaitingFor::TargetSelection` → `GameAction::ChooseTarget`
// flow (CR 601.2c, one target per slot in written order) and asserts every
// selected creature actually changed controller (CR 613.1b, Layer 2).
//
// Revert-proof: restore the discarded `multi_target` binding and only one slot
// is created, so the driver's second and third declared targets are never
// consumed and `assert_all_controlled_by` fails on creatures 2 and 3.
// ─────────────────────────────────────────────────────────────────────────────

/// Read an object's post-layer controller (CR 613.1b: control-changing effects
/// are applied in Layer 2, so the resolved `controller` field is the authority).
fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner
        .state()
        .objects
        .get(&id)
        .unwrap_or_else(|| panic!("object {id:?} must still exist"))
        .controller
}

/// Assert every listed creature is controlled by `expected`, naming the first
/// one that is not — the "only one target was selectable" failure lands here.
fn assert_all_controlled_by(
    runner: &GameRunner,
    creatures: &[ObjectId],
    expected: PlayerId,
    context: &str,
) {
    let actual: Vec<PlayerId> = creatures
        .iter()
        .map(|&c| controller_of(runner, c))
        .collect();
    assert!(
        actual.iter().all(|&c| c == expected),
        "{context}: all {} selected creatures must change controller to {expected:?}, got {actual:?}. \
         A creature still under its original controller means the target slot for it was never \
         offered — the dropped `multi_target` count (#6205).",
        creatures.len()
    );
}

#[test]
fn jace_ultimate_steals_three_creatures_through_target_selection() {
    // Jace, Ingenious Mind-Mage's "−9: Gain control of up to three target
    // creatures." — the `GainControl` runtime witness. A loyalty ability IS an
    // activated ability (CR 606.1 + CR 602.2b), so it goes on the stack and its
    // targets are chosen at announcement (CR 601.2c) exactly like a spell's.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Three distinct opponent creatures — distinct so a slot that is never
    // offered shows up as that specific creature staying with P1.
    let first = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let second = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let third = scenario.add_creature(P1, "Air Elemental", 4, 4).id();

    // Verbatim Oracle line, so the ability takes the real parse branch.
    let jace = scenario
        .add_creature_from_oracle(
            P0,
            "Jace, Ingenious Mind-Mage",
            0,
            0,
            "−9: Gain control of up to three target creatures.",
        )
        .id();

    let mut runner = scenario.build();
    {
        // Make Jace a real planeswalker with enough loyalty to pay −9, so the
        // LOYALTY activation path is the one under test (CR 606.3).
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&jace).expect("jace");
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.loyalty = Some(9);
        obj.counters.insert(CounterType::Loyalty, 9);
    }

    // Pre-state guard: all three start with the opponent, so the assertion
    // below cannot pass vacuously.
    assert_all_controlled_by(&runner, &[first, second, third], P1, "before activation");

    // The driver answers one slot per `ChooseTarget`, consuming one declared
    // object per slot — so it can only place all three if all three slots exist.
    runner
        .activate(jace, 0)
        .target_objects(&[first, second, third])
        .resolve();

    assert_all_controlled_by(
        &runner,
        &[first, second, third],
        P0,
        "CR 601.2c: \"up to three target creatures\" must offer three selectable slots",
    );
}

/// CR 601.2c + CR 603.3d: Rangers of Ithilien is a public ETB trigger, so
/// the production trigger-target prompt must expose exactly the lesser-power
/// creature and reject equal/greater candidates before the trigger resolves.
#[test]
fn rangers_of_ithilien_public_trigger_prompt_validates_lesser_power() {
    const RANGERS: &str = "Vigilance\nWhen this creature enters, gain control of up to one target creature with lesser power for as long as you control this creature. Then the Ring tempts you.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lesser = scenario.add_creature(P1, "Lesser", 2, 2).id();
    let equal = scenario.add_creature(P1, "Equal", 3, 3).id();
    let greater = scenario.add_creature(P1, "Greater", 4, 4).id();
    let rangers = scenario
        .add_creature_to_hand_from_oracle(P0, "Rangers of Ithilien", 3, 3, RANGERS)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&rangers].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rangers,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast real Rangers of Ithilien");

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                assert!(
                    legal.contains(&TargetRef::Object(lesser)),
                    "lesser target missing: {legal:?}"
                );
                assert!(
                    !legal.contains(&TargetRef::Object(equal)),
                    "equal target admitted: {legal:?}"
                );
                assert!(
                    !legal.contains(&TargetRef::Object(greater)),
                    "greater target admitted: {legal:?}"
                );
                assert!(
                    engine::ai_support::legal_actions(runner.state())
                        .iter()
                        .any(|action| matches!(
                            action,
                            GameAction::ChooseTarget {
                                target: Some(TargetRef::Object(id))
                            } if *id == lesser
                        )),
                    "AI candidates must include the same legal Rangers target"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(lesser)),
                    })
                    .expect("choose Rangers target");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Rangers trigger");
            }
            WaitingFor::ChooseRingBearer { candidates, .. } => {
                runner
                    .act(GameAction::ChooseRingBearer {
                        target: candidates[0],
                    })
                    .expect("choose a Rangers ring bearer");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Rangers trigger");
            }
            other => panic!("unexpected Rangers waiting state: {other:?}"),
        }
    }

    assert_eq!(runner.state().objects[&lesser].controller, P0);
    assert_eq!(runner.state().objects[&equal].controller, P1);
    assert_eq!(runner.state().objects[&greater].controller, P1);
}

const RANGERS_OF_ITHILIEN: &str = "Vigilance\nWhen this creature enters, gain control of up to one target creature with lesser power for as long as you control this creature. Then the Ring tempts you.";
const DEFLECTING_SWAT: &str = "If you control a commander, you may cast this spell without paying its mana cost.\nYou may choose new targets for target spell or ability.";
const STRIONIC_RESONATOR: &str = "{2}, {T}: Copy target triggered ability you control. You may choose new targets for the copy. (A triggered ability uses the words \"when,\" \"whenever,\" or \"at.\")";
const VEDALKEN_ORRERY: &str = "You may cast spells as though they had flash.";
const RENATA: &str = "Renata's power is equal to your devotion to green. (Each {G} in the mana costs of permanents you control counts toward your devotion to green.)\nEach other creature you control enters with an additional +1/+1 counter on it.";

struct RangersRetargetBoard {
    runner: GameRunner,
    rangers: ObjectId,
    rangers_countered: ObjectId,
    renata: ObjectId,
    initial: ObjectId,
    lesser: ObjectId,
    zero_power: ObjectId,
    equal: ObjectId,
    greater: ObjectId,
    swat: Option<ObjectId>,
    resonator: Option<ObjectId>,
}

fn rangers_retarget_board(response: &str) -> RangersRetargetBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let initial = scenario.add_creature(P1, "Initial lesser", 2, 2).id();
    let lesser = scenario.add_creature(P1, "Lesser", 1, 1).id();
    let zero_power = scenario.add_creature(P1, "Zero Power", 0, 1).id();
    let equal = scenario.add_creature(P1, "Equal", 3, 3).id();
    let greater = scenario.add_creature(P1, "Greater", 4, 4).id();
    let rangers = scenario
        .add_creature_to_hand_from_oracle(P0, "Rangers of Ithilien", 3, 3, RANGERS_OF_ITHILIEN)
        .with_mana_cost(ManaCost::zero())
        .id();
    let rangers_countered = scenario
        .add_creature_to_hand_from_oracle(P0, "Rangers of Ithilien", 3, 3, RANGERS_OF_ITHILIEN)
        .with_mana_cost(ManaCost::zero())
        .id();
    // Vedalken Orrery is a printed card that supplies the flash permission needed
    // to cast the second printed Rangers while the first trigger remains on the stack.
    scenario.add_artifact_from_oracle(P0, "Vedalken Orrery", VEDALKEN_ORRERY);
    let renata = scenario
        .add_creature_to_hand_from_oracle(P0, "Renata, Called to the Hunt", 0, 3, RENATA)
        .with_mana_cost(ManaCost::zero())
        .id();
    let swat = (response == "swat").then(|| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Deflecting Swat", true, DEFLECTING_SWAT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            })
            .id()
    });
    let resonator = (response == "resonator").then(|| {
        scenario
            .add_artifact_from_oracle(P0, "Strionic Resonator", STRIONIC_RESONATOR)
            .id()
    });
    let mana = if swat.is_some() { 3 } else { 2 };
    scenario.with_mana_pool(
        P0,
        (0..mana)
            .map(|index| {
                ManaUnit::new(
                    if swat.is_some() && index == 0 {
                        ManaType::Red
                    } else {
                        ManaType::Colorless
                    },
                    ObjectId(0),
                    false,
                    vec![],
                )
            })
            .collect(),
    );
    RangersRetargetBoard {
        runner: scenario.build(),
        rangers,
        rangers_countered,
        renata,
        initial,
        lesser,
        zero_power,
        equal,
        greater,
        swat,
        resonator,
    }
}

/// CR 603.3d: announce the real Rangers card and answer its ETB target with A,
/// leaving the trigger on the stack for a public retarget/copy action.
fn put_rangers_trigger_on_stack(board: &mut RangersRetargetBoard, rangers: ObjectId) -> ObjectId {
    let card_id = board.runner.state().objects[&rangers].card_id;
    board
        .runner
        .act(GameAction::CastSpell {
            object_id: rangers,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast real Rangers of Ithilien");
    for _ in 0..64 {
        if matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ) {
            if let Some(trigger) = board.runner.state().stack.iter().find(|entry| {
                entry.source_id == rangers
                    && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
            }) {
                return trigger.id;
            }
            board
                .runner
                .act(GameAction::PassPriority)
                .expect("resolve Rangers onto the battlefield");
            continue;
        }
        match board.runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                assert_eq!(selection.current_slot, 0);
                assert!(target_slots[0]
                    .legal_targets
                    .contains(&TargetRef::Object(board.initial)));
                board
                    .runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(board.initial)),
                    })
                    .expect("choose Rangers target A");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                board
                    .runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Rangers trigger");
            }
            other => panic!("unexpected Rangers setup state: {other:?}"),
        }
    }
    panic!("Rangers trigger did not reach a priority window");
}

fn assert_rangers_retarget_prompt(
    runner: &GameRunner,
    lesser: ObjectId,
    zero_power: ObjectId,
    equal: ObjectId,
    greater: ObjectId,
) {
    let WaitingFor::RetargetChoice {
        scope,
        legal_new_targets,
        slot_pools,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a retarget prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(matches!(scope, RetargetScope::All));
    assert!(legal_new_targets.contains(&TargetRef::Object(lesser)));
    assert!(slot_pools[0].contains(&TargetRef::Object(lesser)));
    assert!(slot_pools[0].contains(&TargetRef::Object(zero_power)));
    assert!(!slot_pools[0].contains(&TargetRef::Object(equal)));
    assert!(!slot_pools[0].contains(&TargetRef::Object(greater)));
}

/// CR 115.7d + CR 601.2c: Deflecting Swat's public retarget action must use
/// the Rangers trigger's source-relative lesser-power filter, not its saved
/// ambient target pool. Equal and greater candidates are rejected by the same
/// reducer that supplied the prompt; the AI exposes the accepted alternative.
#[test]
fn deflecting_swat_retargets_public_rangers_trigger_by_lesser_power() {
    let mut board = rangers_retarget_board("swat");
    let rangers = board.rangers;
    let trigger = put_rangers_trigger_on_stack(&mut board, rangers);
    let swat = board.swat.expect("Swat on the board");
    board.runner.cast(swat).target_object(trigger).commit();
    for _ in 0..32 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                board
                    .runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Deflecting Swat");
            }
            WaitingFor::RetargetChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Deflecting Swat");
            }
            other => panic!("unexpected Swat state: {other:?}"),
        }
    }
    assert_rangers_retarget_prompt(
        &board.runner,
        board.lesser,
        board.zero_power,
        board.equal,
        board.greater,
    );
    let ai_actions = engine::ai_support::candidate_actions(board.runner.state());
    assert!(
        ai_actions.iter().any(|candidate| {
            matches!(
                &candidate.action,
                GameAction::RetargetSpell { new_targets }
                    if new_targets == &vec![Some(TargetRef::Object(board.lesser))]
            )
        }),
        "AI must offer B at the Swat prompt: {ai_actions:?}"
    );
    for bad in [board.equal, board.greater] {
        assert!(board
            .runner
            .act(GameAction::RetargetSpell {
                new_targets: vec![Some(TargetRef::Object(bad))],
            })
            .is_err());
    }
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(board.lesser))],
        })
        .expect("choose lesser-power creature B");
    board.runner.advance_until_stack_empty();
    assert_eq!(board.runner.state().objects[&board.lesser].controller, P0);
    assert_eq!(board.runner.state().objects[&board.equal].controller, P1);
    assert_eq!(board.runner.state().objects[&board.greater].controller, P1);
}

/// CR 603.2 + CR 115.7e + CR 400.7: when two otherwise identical printed Rangers
/// triggers are on the stack, the retarget walk must bind to the selected
/// entry's event. The countered 4/4 trigger admits the 3/3 target; accidentally
/// reading the older 3/3 trigger would reject it.
#[test]
fn deflecting_swat_uses_the_selected_rangers_event_with_two_triggers() {
    let mut board = rangers_retarget_board("swat");
    let first_rangers = board.rangers;
    let first_trigger = put_rangers_trigger_on_stack(&mut board, first_rangers);
    board.runner.cast(board.renata).commit();
    board.runner.resolve_top();
    let countered_rangers = board.rangers_countered;
    let second_trigger = put_rangers_trigger_on_stack(&mut board, countered_rangers);
    assert_ne!(first_trigger, second_trigger);

    let swat = board.swat.expect("Swat on the board");
    board
        .runner
        .cast(swat)
        .target_object(second_trigger)
        .commit();
    for _ in 0..32 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                board
                    .runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Deflecting Swat");
            }
            WaitingFor::RetargetChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Deflecting Swat");
            }
            other => panic!("unexpected Swat state: {other:?}"),
        }
    }
    let WaitingFor::RetargetChoice { slot_pools, .. } = &board.runner.state().waiting_for else {
        panic!(
            "expected a retarget prompt, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert!(slot_pools[0].contains(&TargetRef::Object(board.equal)));
    assert!(!slot_pools[0].contains(&TargetRef::Object(board.greater)));
    let mut keep = GameRunner::from_state(board.runner.state().clone());
    keep.act(GameAction::RetargetSpell {
        new_targets: vec![None],
    })
    .expect("keep the selected countered Rangers trigger's original target");
    keep.advance_until_stack_empty();
    assert_eq!(keep.state().objects[&board.initial].controller, P0);
    board
        .runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(board.equal))],
        })
        .expect("countered Rangers trigger may retarget to the 3/3 creature");
    board.runner.advance_until_stack_empty();
    assert_eq!(board.runner.state().objects[&board.equal].controller, P0);
}

/// CR 603.2 + CR 115.7e + CR 400.7: a restored prompt that still carries an old
/// alternative must fail closed when its stack entry has lost the event that
/// supplies the Rangers source-relative power. The raw state round trip is the
/// same checked persistence boundary used by production restore; this test
/// deliberately removes the entry-owned event after the prompt was offered.
#[test]
fn restored_rangers_retarget_rejects_without_entry_event_provenance() {
    let mut board = rangers_retarget_board("swat");
    let rangers = board.rangers;
    let trigger = put_rangers_trigger_on_stack(&mut board, rangers);
    let swat = board.swat.expect("Swat on the board");
    board.runner.cast(swat).target_object(trigger).commit();
    for _ in 0..32 {
        match board.runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                board
                    .runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Deflecting Swat");
            }
            WaitingFor::RetargetChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                board
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("resolve Deflecting Swat");
            }
            other => panic!("unexpected Swat state: {other:?}"),
        }
    }
    let stack_entry_index = match &board.runner.state().waiting_for {
        WaitingFor::RetargetChoice {
            stack_entry_index, ..
        } => *stack_entry_index,
        other => panic!("expected retarget prompt, got {other:?}"),
    };
    let valid_restored = PersistedGameState::capture(board.runner.state().clone())
        .into_game_state()
        .expect("checked restore must preserve a valid Rangers retarget prompt");
    assert!(matches!(
        valid_restored.waiting_for,
        WaitingFor::RetargetChoice { .. }
    ));
    let WaitingFor::RetargetChoice { slot_pools, .. } = &valid_restored.waiting_for else {
        unreachable!();
    };
    assert!(slot_pools[0].contains(&TargetRef::Object(board.zero_power)));
    let mut substituted = valid_restored.clone();
    let entry_id = board.runner.state().stack[stack_entry_index].id;
    substituted.stack[stack_entry_index].id = ObjectId(entry_id.0.wrapping_add(10_000_000));
    assert!(
        PersistedGameState::capture(substituted)
            .into_game_state()
            .is_err(),
        "checked restore must reject a retarget prompt whose stack slot now holds a different entry"
    );

    let mut valid_runner = GameRunner::from_state(valid_restored);
    valid_runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(board.lesser))],
        })
        .expect("checked restore must preserve a legal Rangers retarget");

    let entry_id = board.runner.state().stack[stack_entry_index].id;
    let entry = &mut board.runner.state_mut().stack[stack_entry_index];
    let StackEntryKind::TriggeredAbility { trigger_event, .. } = &mut entry.kind else {
        panic!("Rangers retarget subject must be a triggered ability");
    };
    *trigger_event = None;
    board
        .runner
        .state_mut()
        .stack_trigger_event_batches
        .remove(&entry_id);
    let restored_state = PersistedGameState::capture(board.runner.state().clone())
        .into_game_state()
        .expect("checked restore must accept the fail-closed Rangers prompt");
    let mut restored = GameRunner::from_state(restored_state);
    assert!(restored
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(board.zero_power))],
        })
        .is_err());
    assert!(restored
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(board.lesser))],
        })
        .is_err());
}

/// CR 707.10c: Strionic Resonator's copy walk must apply the same public
/// source-relative target legality when it copies Rangers of Ithilien.
#[test]
fn strionic_resonator_copies_public_rangers_trigger_by_lesser_power() {
    let mut board = rangers_retarget_board("resonator");
    let rangers = board.rangers;
    let trigger = put_rangers_trigger_on_stack(&mut board, rangers);
    let resonator = board.resonator.expect("Resonator on the board");
    board
        .runner
        .activate(resonator, 0)
        .target_object(trigger)
        .accept_optional()
        .resolve();
    let WaitingFor::CopyRetarget { target_slots, .. } = &board.runner.state().waiting_for else {
        panic!(
            "expected copy retarget prompt, got {:?}",
            board.runner.state().waiting_for
        );
    };
    let slot = &target_slots[0];
    assert!(slot
        .legal_alternatives
        .contains(&TargetRef::Object(board.lesser)));
    assert!(!slot
        .legal_alternatives
        .contains(&TargetRef::Object(board.equal)));
    assert!(!slot
        .legal_alternatives
        .contains(&TargetRef::Object(board.greater)));
    for bad in [board.equal, board.greater] {
        assert!(board
            .runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bad)),
            })
            .is_err());
    }
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.lesser)),
        })
        .expect("choose lesser-power creature B for the copy");
    board.runner.advance_until_stack_empty();
    assert_eq!(board.runner.state().objects[&board.lesser].controller, P0);
    assert_eq!(board.runner.state().objects[&board.equal].controller, P1);
    assert_eq!(board.runner.state().objects[&board.greater].controller, P1);
}

/// CR 707.10c + CR 603.2: Resonator must copy the selected countered (4/4)
/// Rangers trigger's event when a different (3/3) Rangers trigger is also present.
/// The copied ability can therefore retarget to the 3/3 creature.
#[test]
fn strionic_resonator_copies_the_selected_rangers_event_with_two_triggers() {
    let mut board = rangers_retarget_board("resonator");
    let first_rangers = board.rangers;
    let first_trigger = put_rangers_trigger_on_stack(&mut board, first_rangers);
    board.runner.cast(board.renata).commit();
    board.runner.resolve_top();
    let countered_rangers = board.rangers_countered;
    let second_trigger = put_rangers_trigger_on_stack(&mut board, countered_rangers);
    assert_ne!(first_trigger, second_trigger);

    let resonator = board.resonator.expect("Resonator on the board");
    board
        .runner
        .activate(resonator, 0)
        .target_object(second_trigger)
        .accept_optional()
        .resolve();
    let WaitingFor::CopyRetarget { target_slots, .. } = &board.runner.state().waiting_for else {
        panic!(
            "expected copy retarget prompt, got {:?}",
            board.runner.state().waiting_for
        );
    };
    let slot = &target_slots[0];
    assert!(slot
        .legal_alternatives
        .contains(&TargetRef::Object(board.equal)));
    assert!(!slot
        .legal_alternatives
        .contains(&TargetRef::Object(board.greater)));
    let mut keep = GameRunner::from_state(board.runner.state().clone());
    keep.act(GameAction::ChooseTarget { target: None })
        .expect("keep the selected countered Rangers trigger's original target");
    keep.advance_until_stack_empty();
    assert_eq!(keep.state().objects[&board.initial].controller, P0);
    board
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(board.equal)),
        })
        .expect("choose the 3/3 creature for the copied countered trigger");
    board.runner.advance_until_stack_empty();
    assert_eq!(board.runner.state().objects[&board.equal].controller, P0);
}

/// CR 400.7 + CR 608.2h: an attack trigger's source power is read from the
/// recorded attacker incarnation, not a same-id permanent that blinked before
/// the trigger resolved. Reverend Mother Jessica attacks as a 3/3, blinks, and
/// returns as her printed 2/2; the opponent's 2/2 remains a legal lesser-power
/// target because the trigger keeps the attack-time 3 power.
#[test]
fn reverend_mother_jessica_uses_attack_incarnation_after_blink() {
    const REVEREND: &str = "Prowess, prowess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn twice.)\nWhenever Reverend Mother Jessica attacks, for each opponent, tap up to one target creature that player controls with lesser power and goad it. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)";
    const FLICKER: &str =
        "Exile target creature or enchantment, then return it to the battlefield under its owner's control.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature_from_oracle(P0, "Reverend Mother Jessica", 2, 2, REVEREND)
        .with_plus_counters(1)
        .id();
    let target = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P1, "Flicker of Fate", true, FLICKER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner.state_mut().add_mana_to_pool(
        P1,
        ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
    );
    runner.state_mut().add_mana_to_pool(
        P1,
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    );
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare Reverend Mother Jessica as an attacker");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("choose the opponent's lesser-power creature for Jessica's trigger");
    let old_incarnation = runner.state().objects[&attacker].incarnation;

    // P0 passes, then P1 casts and resolves a real blink while the attack
    // trigger is pending on the stack.
    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to the defending player");
    assert_eq!(runner.state().priority_player, P1);
    let p1 = runner
        .state()
        .players
        .iter()
        .find(|player| player.id == P1)
        .expect("P1 exists");
    assert_eq!(p1.mana_pool.units().count(), 2);
    let card_id = runner.state().objects[&flicker].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flicker,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Flicker of Fate in response");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(attacker)),
        })
        .expect("choose Jessica for Flicker of Fate");
    for _ in 0..8 {
        if runner.state().objects[&flicker].zone == engine::types::zones::Zone::Graveyard {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Flicker without resolving Jessica's trigger");
    }
    assert_ne!(
        runner.state().objects[&attacker].incarnation,
        old_incarnation,
        "Flicker must return Jessica as a new object"
    );
    assert_eq!(runner.state().objects[&attacker].power, Some(2));
    assert!(!runner.state().objects[&target].tapped);

    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&target].tapped,
        "the attack-time 3 power must keep the opponent's 2/2 legal after Jessica blinks"
    );
}

// NOTE — why there is no `GiveControl` RUNTIME witness here.
//
// `GiveControl`'s count is pinned at parse level (`give_control_keeps_up_to_three`)
// but cannot be pinned at runtime yet, and the reason is NOT this PR's defect: the
// RECIPIENT slot is missing. `ability_needs_companion_target_player_slot` defers to
// `effect_references_target_player`, which inspects only the effect's target FILTER
// (`effect_bound_filter_matches`) and never `GiveControl`'s separate `recipient`
// field. So "target player gains control of …" surfaces its creature slots but no
// player slot, the recipient never resolves, and `gain_control::resolve_give`
// installs no `ChangeController` effect at all.
//
// Measured, not assumed. Building the target slots for the same ability with and
// without `multi_target` gives 3 creature slots vs 1 creature slot — and NO player
// slot in either case. That proves two things: the missing recipient slot is
// pre-existing rather than introduced here, and the count this PR restores does
// reach real slot generation. A runtime witness fails identically with and without
// the "nonattacking" clause, which rules out the target filter as the cause.
//
// (Separately: "nonattacking" parses to `Subtype("Attacking")` — no such creature
// subtype exists, so Domineering Will has a second, independent filter gap on top
// of the recipient one. Also out of scope here.)
//
// It is deliberately not fixed here. `GiveControl`'s `recipient` is not always a
// chosen target — "an opponent gains control of that creature" and the
// `ScopedPlayer` / `SpecificPlayer` recipients resolve at resolution time via
// `unique_recipient_from_filter`. So teaching the companion-slot check to fire on
// any `GiveControl` would manufacture a spurious player slot for that whole
// non-targeted class, which is the same hazard the `Sacrifice`,
// `Bounce { AtResolution }` and `UnattachAll` carve-outs in
// `extract_target_filter_from_effect` exist to prevent. Doing it correctly means
// discriminating a TARGETED recipient from a resolution-time one — a separate
// design change, tracked as follow-up work rather than smuggled into a parser
// count fix.
