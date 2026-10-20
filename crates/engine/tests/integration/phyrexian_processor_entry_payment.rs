//! Phyrexian Processor class: "As ~ enters, pay any amount of life" entry
//! payments with a remembered X (issue #10152, PR #10008).
//!
//! Matrix rows V1b/V2/V2b/V2c/V2d/V3–V8 (plan-r3). Every test drives the
//! production cast/entry path and asserts the required prompt unconditionally.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{
    AbilityDefinition, ChosenAttribute, Effect, QuantityExpr, QuantityModification,
    ReplacementDefinition, ReplacementMode, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{PayableResource, PendingReplacement, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Verbatim Oracle texts (Scryfall; see plan-r3 Step 0).
const PROCESSOR_ORACLE: &str = "As this artifact enters, pay any amount of life.\n{4}, {T}: Create an X/X black Phyrexian Minion creature token, where X is the life paid as this artifact entered.";
const MINION_ORACLE: &str = "Trample\nAs this creature enters, pay any amount of life.\nMinion of the Wastes's power and toughness are each equal to the life paid as it entered.";
const NAMELESS_ORACLE: &str = "Trample\nAs this creature enters, pay any amount of life. The amount you pay can't be more than the total number of white nontoken permanents your opponents control plus the total number of white cards in their graveyards.\nNameless Race's power and toughness are each equal to the life paid as it entered.";
const BLIND_OBEDIENCE_ORACLE: &str = "Extort (Whenever you cast a spell, you may pay {W/B}. If you do, each opponent loses 1 life and you gain that much life.)\nArtifacts and creatures your opponents control enter tapped.";
const CLONE_ORACLE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const CANCEL_ORACLE: &str = "Counter target spell.";
const FLICKER_ORACLE: &str =
    "Exile target nontoken permanent, then return it to the battlefield under its owner's control.";
const GLORIOUS_ANTHEM_ORACLE: &str = "Creatures you control get +1/+1.";
const PLATINUM_EMPERION_ORACLE: &str =
    "Your life total can't change. (You can't gain or lose life. You can't pay any amount of life except 0.)";

fn add_mana_for(runner: &mut GameRunner, player: PlayerId, ty: ManaType, count: usize) {
    let idx = runner
        .state()
        .players
        .iter()
        .position(|p| p.id == player)
        .expect("player must exist");
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[idx].mana_pool.add(unit);
    }
}

fn cost_with_shards(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Pass priority until `player` holds it (the stack must not resolve).
fn give_priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..runner.state().players.len() {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        let depth = runner.state().stack.len();
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is accepted");
        assert_eq!(
            runner.state().stack.len(),
            depth,
            "one pass hands priority over without resolving the stack"
        );
    }
    panic!("priority never reached {player:?}");
}

/// Assert a pre-entry life-payment prompt with exact fields. Unconditional:
/// any other waiting state fails the test.
fn assert_pay_prompt(waiting: &WaitingFor, player: PlayerId, min: u32, max: u32, source: ObjectId) {
    match waiting {
        WaitingFor::PayAmountChoice {
            player: p,
            resource,
            min: lo,
            max: hi,
            accumulated,
            source_id,
            pending_mana_ability,
        } => {
            assert_eq!(*p, player, "prompt actor");
            assert!(
                matches!(resource, PayableResource::Life),
                "prompt resource is Life, got {resource:?}"
            );
            assert_eq!((*lo, *hi), (min, max), "prompt range");
            assert_eq!(*accumulated, 0, "prompt starts unaccumulated");
            assert_eq!(*source_id, source, "prompt source is the entrant");
            assert!(
                pending_mana_ability.is_none(),
                "entry payment carries no mana-ability rider"
            );
        }
        other => panic!("expected a PayAmountChoice prompt, got {other:?}"),
    }
}

fn chosen_number_of(runner: &GameRunner, object: ObjectId) -> Option<u32> {
    runner
        .state()
        .objects
        .get(&object)
        .expect("object must exist")
        .chosen_attributes
        .iter()
        .find_map(|attr| match attr {
            ChosenAttribute::Number(n) => Some(*n),
            _ => None,
        })
}

fn battlefield_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects.get(id).is_some_and(|o| o.is_token))
        .collect()
}

/// Roll P0's turn into P1's pre-combat main with priority. Both libraries must
/// be stocked (draw steps). Bounded; panics on any unexpected prompt.
fn roll_to_p1_precombat_main(runner: &mut GameRunner) {
    for _ in 0..400 {
        let done = runner.state().active_player == P1
            && runner.state().phase == Phase::PreCombatMain
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. });
        if done {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while rolling");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("decline attacks while rolling");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("decline blocks while rolling");
            }
            other => panic!("unexpected {other:?} while rolling to P1 main"),
        }
    }
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
}

/// V2 + V1b: casting Processor raises exactly one pre-entry life-payment
/// prompt; answering pays once, latches once, and completes the entry with no
/// second prompt (the V1b load-bearing revert-red: dropping the
/// post-replacement suppression re-fires a post-entry prompt, so the final
/// state would be `PayAmountChoice`, not `Priority`).
#[test]
fn processor_entry_prompts_for_life_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 4);

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);

    runner
        .act(GameAction::SubmitPayAmount { amount: 7 })
        .expect("answer the entry payment");
    runner.advance_until_stack_empty();

    // Paid once, latched once, entered, done.
    assert_eq!(runner.life(P0), 13, "20 - 7 paid exactly once");
    assert_eq!(chosen_number_of(&runner, proc), Some(7));
    assert_eq!(
        outcome.zone_of(proc),
        Zone::Stack,
        "snapshot point: prompt is up while the spell is still on the stack"
    );
    assert_eq!(
        runner.state().objects.get(&proc).unwrap().zone,
        Zone::Battlefield
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no second prompt after the answer, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack empty at completion");
    assert_eq!(
        runner.state().last_effect_count,
        None,
        "entry payment stamps no resolution amount (CR 614.1)"
    );
}

/// V2 hostile: two sequential entries prompt sequentially; the second prompt's
/// max reflects the first payment.
#[test]
fn two_processors_sequential_prompts_reflect_prior_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let second = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 8);

    let outcome = runner.cast(first).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, first);
    runner
        .act(GameAction::SubmitPayAmount { amount: 5 })
        .expect("answer the first payment");
    runner.advance_until_stack_empty();
    assert_eq!(chosen_number_of(&runner, first), Some(5));

    let outcome = runner.cast(second).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 15, second);
    runner
        .act(GameAction::SubmitPayAmount { amount: 6 })
        .expect("answer the second payment");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 9);
    assert_eq!(chosen_number_of(&runner, second), Some(6));
    // Object-axis binding: the first latch is untouched by the second entry.
    assert_eq!(chosen_number_of(&runner, first), Some(5));
}

/// Identity contract: two opponents each control a Processor; A's latch reads
/// only A's payment and B's only B's (object-axis binding, not global).
#[test]
fn opponent_processors_keep_independent_latches() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains", "Plains"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island", "Island"]);
    let a = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let b = scenario
        .add_artifact_to_hand_from_oracle(P1, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 8);

    let outcome = runner.cast(a).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, a);
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("P0 answers");
    runner.advance_until_stack_empty();
    // A's activation mints a 3/3 before the turn rolls (mana empties).
    let outcome = runner.activate(a, 0).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
    let tokens = battlefield_tokens(&runner);
    assert_eq!(tokens.len(), 1, "one Minion token for A");
    let token = runner.state().objects.get(&tokens[0]).unwrap();
    assert_eq!((token.power, token.toughness), (Some(3), Some(3)));

    roll_to_p1_precombat_main(&mut runner);
    add_mana_for(&mut runner, P1, ManaType::Colorless, 8);
    let outcome = runner.cast(b).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P1, 0, 20, b);
    runner
        .act(GameAction::SubmitPayAmount { amount: 7 })
        .expect("P1 answers");
    runner.advance_until_stack_empty();
    let outcome = runner.activate(b, 0).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "B activation resolves, got {:?}",
        outcome.final_waiting_for()
    );

    // Each activation read its own object's latch.
    let tokens = battlefield_tokens(&runner);
    assert_eq!(tokens.len(), 2, "one Minion token per Processor");
    let mut pts: Vec<(Option<i32>, Option<i32>)> = tokens
        .iter()
        .map(|id| {
            let o = runner.state().objects.get(id).unwrap();
            (o.power, o.toughness)
        })
        .collect();
    pts.sort();
    assert_eq!(pts, vec![(Some(3), Some(3)), (Some(7), Some(7))]);
    assert_eq!(chosen_number_of(&runner, a), Some(3));
    assert_eq!(chosen_number_of(&runner, b), Some(7));
}

/// V2b: an EnterTapped co-candidate (Blind Obedience) auto-resolves against
/// the payment's Disjoint classification — the FIRST prompt is the payment,
/// never an ordering prompt — and both apply (tapped + latched).
#[test]
fn tapped_entry_co_candidate_auto_resolves_then_pays() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P1, "Blind Obedience", BLIND_OBEDIENCE_ORACLE);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 4);

    // The payment parks at the auto-resolve site: no ReplacementChoice first.
    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 4 })
        .expect("answer the entry payment");
    runner.advance_until_stack_empty();

    let obj = runner.state().objects.get(&proc).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.tapped, "Blind Obedience tapped the entry");
    assert_eq!(chosen_number_of(&runner, proc), Some(4));
    assert_eq!(runner.life(P0), 16);
}

/// V2c (copy): Clone copying Minion of the Wastes pays on the COPY — the
/// payment replacement rides `CopiableValues.replacement_definitions`, parks
/// after the copy applies, and latches onto Clone without touching Minion.
///
/// Test-evidence delta (justified): the plan names Clone-as-Processor, but
/// Clone's "any creature" filter cannot legally target the artifact Processor;
/// Minion is the in-class creature member, so the copy-interplay row uses it.
/// The :12712 ordering path the plan attributed to copy cannot arise from
/// copy at all (copy-then-pay is sequential — the copied payment does not
/// exist until the copy applies, per the Spark Double precedent), so it is
/// covered separately by `payment_orders_against_material_override_then_parks`.
#[test]
fn clone_copying_minion_pays_on_the_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let minion = scenario
        .add_creature_to_hand_from_oracle(P0, "Minion of the Wastes", 0, 0, MINION_ORACLE)
        .with_mana_cost(cost_with_shards(
            vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
            3,
        ))
        .id();
    let clone = scenario
        .add_creature_to_hand_from_oracle(P0, "Clone", 0, 0, CLONE_ORACLE)
        .with_mana_cost(cost_with_shards(vec![ManaCostShard::Blue], 3))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 6);
    add_mana_for(&mut runner, P0, ManaType::Black, 3);
    add_mana_for(&mut runner, P0, ManaType::Blue, 1);

    // Original Minion enters first and latches 3.
    let outcome = runner.cast(minion).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, minion);
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("Minion answers");
    runner.advance_until_stack_empty();
    let obj = runner.state().objects.get(&minion).unwrap();
    assert_eq!((obj.power, obj.toughness), (Some(3), Some(3)));

    // Clone: accept the copy, choose Minion, then the copied payment parks.
    let outcome = runner.cast(clone).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::ReplacementChoice {
            candidate_count: 2, ..
        } => {}
        other => panic!("expected the copy accept/decline prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the copy");
    let WaitingFor::CopyTargetChoice { valid_targets, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected CopyTargetChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        valid_targets.contains(&minion),
        "the copy choice must offer the Minion"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(minion)),
        })
        .expect("choose Minion");
    // The surviving legal target should be Minion (the only creature).
    assert_pay_prompt(&runner.state().waiting_for.clone(), P0, 0, 17, clone);
    runner
        .act(GameAction::SubmitPayAmount { amount: 7 })
        .expect("Clone answers the copied payment");
    runner.advance_until_stack_empty();

    // Clone is a Minion-copy with its own latch; the original is untouched.
    let clone_obj = runner.state().objects.get(&clone).unwrap();
    assert_eq!(clone_obj.zone, Zone::Battlefield);
    assert_eq!(clone_obj.name, "Minion of the Wastes");
    assert_eq!((clone_obj.power, clone_obj.toughness), (Some(7), Some(7)));
    assert_eq!(chosen_number_of(&runner, clone), Some(7));
    assert_eq!(chosen_number_of(&runner, minion), Some(3));
    let minion_obj = runner.state().objects.get(&minion).unwrap();
    assert_eq!((minion_obj.power, minion_obj.toughness), (Some(3), Some(3)));
}

/// V2c (ordering): a payment plus a simultaneous MATERIAL co-candidate offers
/// an ordering prompt; choosing the payment parks it at the ordering-continue
/// site, and the co-candidate applies after the answer.
///
/// The co-candidate is test-constructed: a Moved replacement whose execute
/// the materiality walk does not classify (draw-zero), so it defaults to
/// `Unconditional` and orders against the `Disjoint` payment in the same CR
/// 616.1e tier. (An `enters_under` override or copy co-candidate would NOT
/// work: CR 616.1b/c precedence filters them into an earlier tier, so they
/// apply sequentially ahead of the payment instead of co-ordering with it.)
/// No printed card pairs a simultaneous material replacement with an entry
/// payment on one entrant.
#[test]
fn payment_orders_against_material_co_candidate_then_parks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 4);
    // Stage the material co-candidate on the entering object itself (CR 614.12:
    // the entering object is scanned).
    let draw0 = ReplacementDefinition::new(ReplacementEvent::Moved)
        .execute(AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Battlefield)
        .description("test: unclassified draw-zero co-candidate".to_string());
    // APPEND: the parsed entry payment must stay onboard beside the rig.
    let obj = runner.state_mut().objects.get_mut(&proc).unwrap();
    assert_eq!(
        obj.replacement_definitions.len(),
        1,
        "the parsed entry payment is staged"
    );
    obj.replacement_definitions.push(draw0);

    let outcome = runner.cast(proc).resolve();
    let pay_index = match outcome.final_waiting_for() {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            assert_eq!(candidates.len(), 2, "payment + draw order materially");
            candidates
                .iter()
                .position(|c| !c.description.contains("draw-zero"))
                .expect("payment candidate present")
        }
        other => panic!("expected an ordering prompt, got {other:?}"),
    };
    // Choose the payment: the ordering-continue site parks it.
    runner
        .act(GameAction::ChooseReplacement { index: pay_index })
        .expect("order the payment first");
    assert_pay_prompt(&runner.state().waiting_for.clone(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 5 })
        .expect("answer the entry payment");
    runner.advance_until_stack_empty();

    // Both applied: entered + latched; the draw-zero drew nothing.
    let obj = runner.state().objects.get(&proc).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(chosen_number_of(&runner, proc), Some(5));
    assert_eq!(runner.life(P0), 15);
    let hand_now = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len();
    assert_eq!(hand_now, 0, "the 1-card hand was cast; draw-zero drew 0");
}

/// V2d (Paid path): the completed entry leaves no stranded pause behind — in
/// NEITHER pause slot (`capture_paused_zone_change_delivery` checks the
/// change-zone frame first, then the batch delivery).
#[test]
fn paid_path_leaves_no_stranded_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 4);

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 9 })
        .expect("answer the entry payment");
    runner.advance_until_stack_empty();

    // Load-bearing: battlefield + latch + clean completion boundary.
    let obj = runner.state().objects.get(&proc).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(chosen_number_of(&runner, proc), Some(9));
    assert_eq!(runner.life(P0), 11);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completed to Priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    // No stranded pause in either slot.
    let frame_paused = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .and_then(|outer| outer.paused_current.as_ref());
    assert!(
        frame_paused.is_none(),
        "change-zone frame carries no paused delivery"
    );
    assert!(
        runner.state().active_batch_delivery().is_none(),
        "no parked batch delivery remains"
    );
}

/// V2d (Deferred path) + V8 (main): the inner life loss hits a material
/// ordering (double × plus-one); the outer entry is pinned before the payment,
/// stashed through the inner prompt, and completes with the SUBMITTED latch
/// (not the modified loss).
#[test]
fn deferred_path_pins_boundary_then_completes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host_a = scenario.add_creature(P0, "Doubler Host", 2, 2).id();
    let host_b = scenario.add_creature(P0, "Plus Host", 2, 2).id();
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 4);
    // Two live LoseLife replacements: Times{2} (Multiplicative) vs Plus{1}
    // (Additive) — NonCommuting, so the inner loss offers an ordering prompt.
    let double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Times { factor: 2 })
        .description("test: double life loss".to_string());
    let plus = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Plus { value: 1 })
        .description("test: plus one life loss".to_string());
    runner
        .state_mut()
        .objects
        .get_mut(&host_a)
        .unwrap()
        .replacement_definitions = vec![double].into();
    runner
        .state_mut()
        .objects
        .get_mut(&host_b)
        .unwrap()
        .replacement_definitions = vec![plus].into();

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    // Clone the pre-answer outer proposal for the boundary comparison.
    let pre_answer: ProposedEvent = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("outer entry is parked")
        .proposed
        .clone();
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("answer the entry payment");

    // The inner ordering prompt is up; the outer entry rides the resume marker.
    let stashed: PendingReplacement = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            assert_eq!(candidates.len(), 2, "inner ordering prompt");
            match runner.state().pending_deferred_life_cost_resume.clone() {
                Some(engine::types::game_state::DeferredLifeCostResume::PayAmount {
                    pending_entry: Some(pending),
                    total,
                    ..
                }) => {
                    assert_eq!(total, 3, "resume total is the submitted amount");
                    *pending
                }
                other => panic!("expected a stashed entry resume, got {other:?}"),
            }
        }
        other => panic!("expected the inner ordering prompt, got {other:?}"),
    };
    // V2d boundary assertions: pinned pre-payment; the latched proposal differs.
    let boundary = stashed
        .delivery_boundary
        .as_ref()
        .expect("boundary was pinned");
    assert_eq!(
        boundary.as_ref(),
        &pre_answer,
        "delivery boundary is the pre-payment proposal"
    );
    assert!(
        stashed.parks_delivery(&pre_answer),
        "parks_delivery holds for the pinned proposal"
    );
    let changed = stashed.proposed.clone();
    assert_ne!(
        &changed, &pre_answer,
        "the latched proposal differs (enter_with_choices)"
    );
    assert!(
        !stashed.parks_delivery(&changed),
        "parks_delivery fails for the changed proposal"
    );

    // Order the double first: (3 × 2) + 1 = 7 lost; latch stays 3.
    let double_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.description.contains("double"))
            .expect("double candidate present"),
        other => panic!("ordering prompt still up, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement {
            index: double_index,
        })
        .expect("order the double first");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 13, "20 - ((3 × 2) + 1)");
    assert_eq!(
        chosen_number_of(&runner, proc),
        Some(3),
        "latch is the submitted amount (CR 118.11), not the modified loss"
    );
    assert_eq!(
        runner.state().objects.get(&proc).unwrap().zone,
        Zone::Battlefield
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "entry completed, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().last_effect_count,
        None,
        "no resolution stamp"
    );
}

/// V8 hostile: a lone OPTIONAL inner shield, declined — the full submitted
/// amount is lost, the latch holds, and the entry completes.
#[test]
fn lone_optional_inner_decline_loses_full_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Shield Host", 2, 2).id();
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 4);
    // A lone optional inner: decline skips it, so the loss is unmodified.
    // `Prevent` (not `Times{0}`): the decline-bypass gate covers only
    // `Prevent`/damage-shield outcomes — a declined Times modifier would
    // still apply (no corpus card declines a Times modifier).
    let shield = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .mode(ReplacementMode::Optional { decline: None })
        .quantity_modification(QuantityModification::Prevent)
        .description("test: optional shield".to_string());
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .replacement_definitions = vec![shield].into();

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 6 })
        .expect("answer the entry payment");
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the inner accept/decline prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(candidates.len(), 2, "accept + decline");
    let decline = candidates
        .iter()
        .position(|c| c.description == "Decline")
        .expect("a Decline option must be offered");
    runner
        .act(GameAction::ChooseReplacement { index: decline })
        .expect("decline the shield");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 14, "full 6 lost on decline");
    assert_eq!(chosen_number_of(&runner, proc), Some(6));
    assert_eq!(
        runner.state().objects.get(&proc).unwrap().zone,
        Zone::Battlefield
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "entry completed, got {:?}",
        runner.state().waiting_for
    );
}

/// V8 hostile: a lone MANDATORY inner whose substitute pauses (Scry) —
/// `pay_life_as_cost` returns `PaidWithDeferredSubstitution`, the entry is
/// stashed, and answering the substitute completes the entry with the latch.
///
/// (The plan's "lone-optional decline" rig cannot produce that variant: a
/// decline resolves synchronously. The mandatory-plus-pausing-substitute rig
/// is what exercises the stash arm; the decline case above covers decline.)
#[test]
fn mandatory_pausing_substitute_defers_then_completes() {
    use engine::types::ability::{AbilityKind, TargetFilter as TF};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    let host = scenario.add_creature(P0, "Scry Shield Host", 2, 2).id();
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 4);
    // Mandatory LoseLife whose substitute (Scry 2) pauses mid-resolution.
    let scry_instead = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 2 },
                target: TF::Controller,
            },
        ))
        .description("test: scry instead of losing life".to_string());
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .unwrap()
        .replacement_definitions = vec![scry_instead].into();

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 5 })
        .expect("answer the entry payment");
    // The substitute paused: answer the Scry by keeping everything on top.
    let keep = match &runner.state().waiting_for {
        WaitingFor::ScryChoice { cards, .. } => cards.clone(),
        other => panic!("expected the substitute Scry prompt, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards { cards: keep })
        .expect("answer the Scry");
    runner.advance_until_stack_empty();

    // The loss was substituted away, but the latch holds the submitted 5.
    assert_eq!(
        runner.life(P0),
        20,
        "substituted loss leaves life untouched"
    );
    assert_eq!(chosen_number_of(&runner, proc), Some(5));
    assert_eq!(
        runner.state().objects.get(&proc).unwrap().zone,
        Zone::Battlefield
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "entry completed, got {:?}",
        runner.state().waiting_for
    );
}

/// V8 reach-guard: a RESOLUTION-time pay-any prompt with a deferring inner
/// still drains through `finish_pay_amount_choice` (stamps!) — the entry fork
/// keys on `pending_entry: Some` and does not overreach.
///
/// The inner is the V8 double/plus ORDERING pair. The asserted contract is
/// the STAMP (`last_effect_count == submitted`) plus the modified loss, not
/// the downstream draw: any resolution-time deferral (ordering or pausing
/// substitute) drops the outer spell's downstream continuation (upstream gap
/// isolated by inspection — none of the entry diffs touch the resolution
/// submit arm, the drain's `None` branch, or the inner answer machinery; the
/// fork provably routes `None` to the unchanged `finish_pay_amount_choice`).
#[test]
fn resolution_time_deferral_still_stamps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &["Plains", "Plains", "Plains", "Plains", "Plains", "Plains"],
    );
    let host_a = scenario.add_creature(P0, "Doubler Host", 2, 2).id();
    let host_b = scenario.add_creature(P0, "Plus Host", 2, 2).id();
    // Necro-style synthetic sorcery: pay any, draw that many.
    let sorcery = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Grasp Insight",
            false,
            "Pay any amount of life. Draw that many cards.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 1);
    // Same NonCommuting inner pair as the V8 entry row: Times{2} vs Plus{1}.
    let double = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Times { factor: 2 })
        .description("test: double life loss".to_string());
    let plus = ReplacementDefinition::new(ReplacementEvent::LoseLife)
        .quantity_modification(QuantityModification::Plus { value: 1 })
        .description("test: plus one life loss".to_string());
    runner
        .state_mut()
        .objects
        .get_mut(&host_a)
        .unwrap()
        .replacement_definitions = vec![double].into();
    runner
        .state_mut()
        .objects
        .get_mut(&host_b)
        .unwrap()
        .replacement_definitions = vec![plus].into();

    let outcome = runner.cast(sorcery).resolve();
    // Resolution-time prompt (source is the resolving spell, not an entrant).
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, sorcery);
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("answer the resolution prompt");
    // Order the double first: (3 × 2) + 1 = 7 lost; the STAMP stays 3.
    let double_index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.description.contains("double"))
            .expect("double candidate present"),
        other => panic!("expected the inner ordering prompt, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement {
            index: double_index,
        })
        .expect("order the double first");
    runner.advance_until_stack_empty();

    // The modified loss landed (7) while the stamp stayed submitted (3),
    // proving the drain routed to `finish_pay_amount_choice` (the entry tail
    // never stamps). The outer spell's downstream draw is NOT asserted: a
    // resolution-time deferral drops the outer continuation (upstream gap —
    // the stamp, the fork's contract, survives).
    assert_eq!(runner.life(P0), 13, "20 - ((3 × 2) + 1)");
    assert_eq!(
        runner.state().last_effect_count,
        Some(3),
        "finish_pay_amount_choice stamped the submitted 3 (fork did not overreach)"
    );
    assert_eq!(
        chosen_number_of(&runner, sorcery),
        None,
        "no entry latch on a resolution prompt"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completed, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

/// V3: an entry nested in an outer resolution whose next step reads
/// `EventContextAmount` — the nested payment must not clobber the outer
/// amount. Outer pays 5, entry pays 3, gain reads 5: net -3 (a clobber would
/// gain 3 for net -5). The latch pins that the payment ran.
#[test]
fn outer_resolution_amount_survives_nested_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // Synthetic mechanism spell (not a card): outer pay → battlefield-put →
    // amount read. The middle step's zone change runs the entry pipeline.
    let outer = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Pious Assembly",
            false,
            "Pay any amount of life. Put target artifact card from your hand onto the battlefield. You gain that much life.",
        )
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 2);

    let outcome = runner.cast(outer).target_objects(&[proc]).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, outer);
    runner
        .act(GameAction::SubmitPayAmount { amount: 5 })
        .expect("answer the outer prompt");
    // The nested entry prompt follows inside the outer resolution.
    assert_pay_prompt(&runner.state().waiting_for.clone(), P0, 0, 15, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("answer the nested entry payment");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        17,
        "20 - 5 - 3 + 5 (gain read the OUTER 5)"
    );
    assert_eq!(chosen_number_of(&runner, proc), Some(3));
    assert_eq!(
        runner.state().objects.get(&proc).unwrap().zone,
        Zone::Battlefield
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "outer resolution completed, got {:?}",
        runner.state().waiting_for
    );
}

/// V4: the activated ability mints an N/N token from the remembered latch.
#[test]
fn processor_activation_mints_remembered_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 8);

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 6 })
        .expect("answer the entry payment");
    runner.advance_until_stack_empty();

    let outcome = runner.activate(proc, 0).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
    let tokens = battlefield_tokens(&runner);
    assert_eq!(tokens.len(), 1, "one Minion token minted");
    let token = runner.state().objects.get(&tokens[0]).unwrap();
    assert_eq!((token.power, token.toughness), (Some(6), Some(6)));
    assert!(
        token.name.contains("Minion"),
        "token is the Phyrexian Minion, got {}",
        token.name
    );
    // The latch survives the activation (revert-red: latch revert breaks X).
    assert_eq!(chosen_number_of(&runner, proc), Some(6));
}

/// V4 (X=0): paying 0 mints a 0/0 token; under Glorious Anthem it survives as
/// a 1/1 — proving the base was exactly 0/0 (a broken latch would mint a
/// different base and the assertion would miss).
#[test]
fn zero_payment_mints_zero_zero_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM_ORACLE);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 8);

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 0 })
        .expect("pay nothing");
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 20);
    assert_eq!(chosen_number_of(&runner, proc), Some(0));

    let outcome = runner.activate(proc, 0).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
    let tokens = battlefield_tokens(&runner);
    assert_eq!(tokens.len(), 1, "0/0 token survived via the anthem");
    let token = runner.state().objects.get(&tokens[0]).unwrap();
    assert_eq!(
        (token.power, token.toughness),
        (Some(1), Some(1)),
        "0/0 base + anthem"
    );
}

/// V4 (flicker, CR 400.7): exile-and-return re-enters as a new object — the
/// entry prompts again and the new latch REPLACES the old (no accumulation).
#[test]
fn flickered_processor_pays_again_fresh_latch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Flicker", true, FLICKER_ORACLE)
        .with_mana_cost(cost_with_shards(vec![ManaCostShard::White], 1))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 5);
    add_mana_for(&mut runner, P0, ManaType::White, 1);

    let outcome = runner.cast(proc).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 8 })
        .expect("first entry answers 8");
    runner.advance_until_stack_empty();
    assert_eq!(chosen_number_of(&runner, proc), Some(8));

    // Flicker exiles and returns it; the re-entry prompts again.
    let outcome = runner.cast(flicker).target_objects(&[proc]).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 12, proc);
    runner
        .act(GameAction::SubmitPayAmount { amount: 2 })
        .expect("re-entry answers 2");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 10, "20 - 8 - 2");
    assert_eq!(
        chosen_number_of(&runner, proc),
        Some(2),
        "new latch replaces the old (CR 400.7)"
    );
    assert_eq!(
        runner.state().objects.get(&proc).unwrap().zone,
        Zone::Battlefield
    );
}

/// V5: Minion of the Wastes pays on entry and its CDA reads the latch.
#[test]
fn minion_entry_sets_power_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let minion = scenario
        .add_creature_to_hand_from_oracle(P0, "Minion of the Wastes", 0, 0, MINION_ORACLE)
        .with_mana_cost(cost_with_shards(
            vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
            3,
        ))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 3);
    add_mana_for(&mut runner, P0, ManaType::Black, 3);

    let outcome = runner.cast(minion).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 20, minion);
    runner
        .act(GameAction::SubmitPayAmount { amount: 11 })
        .expect("answer the entry payment");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 9);
    assert_eq!(chosen_number_of(&runner, minion), Some(11));
    let obj = runner.state().objects.get(&minion).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(11), Some(11)),
        "CDA reads the latch"
    );
}

/// V5: a countered Minion never enters — no prompt, no payment.
#[test]
fn countered_minion_prompts_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let minion = scenario
        .add_creature_to_hand_from_oracle(P0, "Minion of the Wastes", 0, 0, MINION_ORACLE)
        .with_mana_cost(cost_with_shards(
            vec![
                ManaCostShard::Black,
                ManaCostShard::Black,
                ManaCostShard::Black,
            ],
            3,
        ))
        .id();
    let cancel = scenario
        .add_spell_to_hand_from_oracle(P1, "Cancel", true, CANCEL_ORACLE)
        .with_mana_cost(cost_with_shards(
            vec![ManaCostShard::Blue, ManaCostShard::Blue],
            1,
        ))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 3);
    add_mana_for(&mut runner, P0, ManaType::Black, 3);
    add_mana_for(&mut runner, P1, ManaType::Colorless, 1);
    add_mana_for(&mut runner, P1, ManaType::Blue, 2);

    // Commit Minion to the stack without resolving, then counter it.
    runner.cast(minion).commit();
    give_priority_to(&mut runner, P1);
    let outcome = runner.cast(cancel).target_objects(&[minion]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "counter resolves, got {:?}",
        outcome.final_waiting_for()
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects.get(&minion).unwrap().zone,
        Zone::Graveyard,
        "countered Minion is in the graveyard"
    );
    assert_eq!(runner.life(P0), 20, "nothing paid");
    assert_eq!(chosen_number_of(&runner, minion), None, "nothing latched");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no prompt was ever raised, got {:?}",
        runner.state().waiting_for
    );
}

/// V6: Nameless Race's cap clamps the prompt max (2 white nontoken perms + 1
/// white yard card = 3, with a white TOKEN excluded); over-cap submits fail
/// closed and the CDA reads the paid amount.
#[test]
fn nameless_cap_clamps_prompt_and_rejects_overcap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Opponent's board: 2 white nontoken creatures + 1 white token (excluded
    // by the cap's NonToken branch).
    scenario
        .add_creature(P1, "White Bear A", 2, 2)
        .with_color(vec![ManaColor::White]);
    scenario
        .add_creature(P1, "White Bear B", 2, 2)
        .with_color(vec![ManaColor::White]);
    let white_token = scenario
        .add_creature(P1, "White Soldier Token", 1, 1)
        .with_color(vec![ManaColor::White])
        .id();
    // Opponent's graveyard: 1 white card.
    scenario
        .add_creature_to_graveyard(P1, "White Remains", 2, 2)
        .with_color(vec![ManaColor::White]);
    let nameless = scenario
        .add_creature_to_hand_from_oracle(P0, "Nameless Race", 0, 0, NAMELESS_ORACLE)
        .with_mana_cost(cost_with_shards(vec![ManaCostShard::Black], 3))
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&white_token)
        .unwrap()
        .is_token = true;
    add_mana_for(&mut runner, P0, ManaType::Colorless, 3);
    add_mana_for(&mut runner, P0, ManaType::Black, 1);

    // Cap = 2 (perms) + 1 (yard) = 3; the white token is excluded.
    let outcome = runner.cast(nameless).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 3, nameless);
    let over = runner.act(GameAction::SubmitPayAmount { amount: 4 });
    assert!(
        over.is_err(),
        "over-cap submission fails closed at the shared guard"
    );
    // The prompt is still up after the rejected submit.
    assert_pay_prompt(&runner.state().waiting_for.clone(), P0, 0, 3, nameless);
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("answer at the cap");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 17);
    assert_eq!(chosen_number_of(&runner, nameless), Some(3));
    let obj = runner.state().objects.get(&nameless).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(3), Some(3)),
        "CDA reads the capped latch"
    );
}

/// V6 (cap-0): with no white anywhere for the opponents, the payment is
/// determined-0 — no prompt, inline latch, entry completes.
#[test]
fn nameless_cap_zero_auto_commits() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nameless = scenario
        .add_creature_to_hand_from_oracle(P0, "Nameless Race", 0, 0, NAMELESS_ORACLE)
        .with_mana_cost(cost_with_shards(vec![ManaCostShard::Black], 3))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 3);
    add_mana_for(&mut runner, P0, ManaType::Black, 1);

    // No prompt at all: the entry auto-commits through to Priority.
    let outcome = runner.cast(nameless).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "cap-0 auto-commits with no prompt, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(runner.life(P0), 20, "nothing paid");
    assert_eq!(chosen_number_of(&runner, nameless), Some(0));
    // A 0/0 correctly dies to SBA after entering: the latch rides
    // `enter_with_choices`, which is installed only at battlefield delivery,
    // so graveyard-with-latch proves entered-then-died (not never-entered).
    assert_eq!(
        runner.state().objects.get(&nameless).unwrap().zone,
        Zone::Graveyard,
        "0/0 enters, then dies to SBA"
    );
}

/// V7: under a CantLoseLife lock (Platinum Emperion) the entry auto-commits
/// 0 with NO prompt — while a Necro-style RESOLUTION prompt in the same game
/// still prompts (max 0), proving the lock is live and contrasting the
/// determined-choice entry path.
#[test]
fn cant_lose_life_auto_commits_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Platinum Emperion", 8, 8, PLATINUM_EMPERION_ORACLE);
    scenario.add_enchantment_from_oracle(P0, "Glorious Anthem", GLORIOUS_ANTHEM_ORACLE);
    let proc = scenario
        .add_artifact_to_hand_from_oracle(P0, "Phyrexian Processor", PROCESSOR_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // Necro-style synthetic sorcery for the lock-liveness contrast.
    let sorcery = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Grasp Insight",
            false,
            "Pay any amount of life. Draw that many cards.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();
    add_mana_for(&mut runner, P0, ManaType::Colorless, 9);

    // The lock is live: the resolution prompt appears WITH max 0.
    let outcome = runner.cast(sorcery).resolve();
    assert_pay_prompt(outcome.final_waiting_for(), P0, 0, 0, sorcery);
    runner
        .act(GameAction::SubmitPayAmount { amount: 0 })
        .expect("answer 0 under the lock");
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 20);

    // The entry, by contrast, never prompts: determined-0 auto-commit.
    let outcome = runner.cast(proc).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "locked entry auto-commits with no prompt, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(runner.life(P0), 20);
    assert_eq!(chosen_number_of(&runner, proc), Some(0));
    assert_eq!(
        runner.state().objects.get(&proc).unwrap().zone,
        Zone::Battlefield
    );

    // Latch-0 activation mints a 0/0, surviving via the anthem as a 1/1.
    let outcome = runner.activate(proc, 0).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation resolves, got {:?}",
        outcome.final_waiting_for()
    );
    let tokens = battlefield_tokens(&runner);
    assert_eq!(tokens.len(), 1);
    let token = runner.state().objects.get(&tokens[0]).unwrap();
    assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
}
