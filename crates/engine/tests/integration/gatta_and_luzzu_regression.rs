//! Regression for issue #328: Gatta and Luzzu's damage-prevention ETB was
//! reported as failing — the targeted creature died because (a) the shield
//! was being installed on Gatta itself instead of the chosen target, (b) the
//! prevention shield was depletion-based (`Next(1)`) so it absorbed only the
//! first damage event, and (c) the rider's "it" anaphor in
//! "put that many +1/+1 counters on it" was binding to `SelfRef` (Gatta)
//! rather than the parent's chosen target.
//!
//! Oracle text:
//!     "Flash
//!      When Gatta and Luzzu enters, choose target creature you control. If
//!      damage would be dealt to that creature this turn, prevent that damage
//!      and put that many +1/+1 counters on it."
//!
//! This test pins the full chained-trigger shape end-to-end:
//!   TargetOnly { Creature, You }
//!     → sub_ability: PreventDamage { All, ParentTarget, AllDamage }
//!         duration: UntilEndOfTurn
//!         → sub_ability: PutCounter { P1P1, target: ParentTarget }
//!             repeat_for: EventContextAmount
//!
//! And the runtime contracts:
//!   - The `Prevention { All }` shield persists across multiple damage events
//!     (CR 615.1a — only `Next(N)` shields are depletion-based per CR 615.7).
//!   - The shield is hosted on the *chosen* creature, not Gatta (CR 608.2c —
//!     `ParentTarget` aliases to the parent's selected target).
//!   - Each prevented damage event accumulates `+1/+1` counters on the chosen
//!     creature, one per 1 damage prevented (CR 615.5 — additional effect that
//!     refers to the prevented amount, fired immediately after each prevention).
//!
//! CR 608.2c: Later instructions may refer to a target chosen earlier in the
//!            same effect.
//! CR 615.1a: Effects that use the word "prevent" are prevention effects.
//! CR 615.5:  Prevention effects may include an additional effect that refers
//!            to the amount of damage prevented; the additional effect runs
//!            immediately after the prevention.
//! CR 615.7:  `Prevent the next N damage` is a depletion shield. (Distinct
//!            from this card's `Prevent that damage` formulation.)
//! CR 514.2:  "This turn" effects end at the cleanup step.
//!
//! ## PR #8849 — production-path coverage
//!
//! The two hand-built tests in this file pin the chained-ability shape and the runtime
//! prevention/counter contracts directly, bypassing `parse_oracle_text`,
//! `GameRunner::cast`, and the ETB target-selection prompt. Per code owner
//! feedback on PR #8849, `gatta_and_luzzu_prevents_through_the_real_cast_pipeline`
//! below drives Gatta's real parsed card through the production cast path
//! (`GameRunner::cast` → `WaitingFor::TriggerTargetSelection`, CR 603.3d —
//! the target is chosen when the triggered ability is put on the stack, not
//! hand-picked), then deals damage through a real cast Lightning Bolt so the
//! whole parser-to-runtime seam — including
//! `effects::bind_detached_continuation_to_parent` — is exercised end to end.

use engine::game::effects;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    Effect, PreventionAmount, PreventionScope, QuantityExpr, QuantityRef, ResolvedAbility,
    RestrictionExpiry, ShieldKind, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (verified against Scryfall AND card-data on
/// 2026-09-13) shared by the real-cast-pipeline test below.
const GATTA_AND_LUZZU_ORACLE: &str = "Flash\nWhen Gatta and Luzzu enters, choose target creature \
     you control. If damage would be dealt to that creature this turn, prevent that damage and \
     put that many +1/+1 counters on it.";

/// NOTE (#8777, PR #8849): this helper previously hand-filled the rider's `targets` with
/// `vec![TargetRef::Object(chosen)]`. The real parse path never does — sub-abilities start with
/// empty targets (`ability_utils::build_resolved_from_def_with_targets`) — so this fixture did
/// NOT exercise the `runtime_execute` parent-target binding and was green for a reason the card
/// does not enjoy. That binding is now performed by
/// `effects::bind_detached_continuation_to_parent` and is covered end to end by
/// `inkshield_prevented_this_way_token_rider.rs` and, for Gatta and Luzzu specifically, by
/// `gatta_and_luzzu_prevents_through_the_real_cast_pipeline` below — the real cast (flash
/// creature + ETB target choice through `WaitingFor::TriggerTargetSelection`) that this
/// hand-built helper does not exercise.
///
/// MEASURED CONSEQUENCE OF THE HAND-FILL: leaving the old hand-fill in place actively breaks
/// under the fix, not just masks it — `targeting::parent_chain_referents`' tier 2
/// (`parent_chain_targets_from_root` → `flatten_targets_in_chain`) concatenates EVERY node's
/// `targets` across the whole chain. With both `prevent.targets` and `counter_rider.targets` set
/// to `[chosen]`, flattening returns `[chosen, chosen]`, and the binding (correctly) installs
/// that duplicate, doubling every counter placement. The fix below — leaving `counter_rider`
/// with NO pre-set `targets` — is what makes the fixture representative of a real parse (where
/// only the resolving chain ROOT carries targets) instead of merely not-crashing.
///
/// Build the chained `PreventDamage → PutCounter` sub-ability that Gatta and
/// Luzzu's parser produces, parameterized on `chosen` so each test can wire
/// the parent target into the root `ability.targets` slot only — the sub-ability's `targets`
/// is left empty, matching what the real parse-and-build pipeline always produces, and is
/// populated at install time by `bind_detached_continuation_to_parent`.
fn build_gatta_prevention_chain(
    gatta: engine::types::identifiers::ObjectId,
    chosen: engine::types::identifiers::ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    let mut counter_rider = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ParentTarget,
        },
        Vec::new(),
        gatta,
        controller,
    );
    counter_rider.repeat_for = Some(QuantityExpr::Ref {
        qty: QuantityRef::EventContextAmount,
    });

    let mut prevent = ResolvedAbility::new(
        Effect::PreventDamage {
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::ParentTarget,
            scope: PreventionScope::AllDamage,
            damage_source_filter: None,
            prevention_duration: None,
        },
        vec![TargetRef::Object(chosen)],
        gatta,
        controller,
    )
    .sub_ability(counter_rider);
    prevent.duration = Some(engine::types::ability::Duration::UntilEndOfTurn);
    prevent
}

/// CR 608.2c + CR 615.1a + CR 615.5: End-to-end Gatta and Luzzu — choose a
/// creature, install a persistent prevention shield on it, fire three damage
/// events, and confirm zero damage marked + three sets of `+1/+1` counters
/// equal to the total damage that *would have been* dealt (12).
#[test]
fn gatta_and_luzzu_prevents_three_damage_events_and_accumulates_counters() {
    let mut state = GameState::new_two_player(42);

    let gatta = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Gatta and Luzzu".to_string(),
        Zone::Battlefield,
    );
    let chosen = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Bear".to_string(),
        Zone::Battlefield,
    );
    let attacker = create_object(
        &mut state,
        CardId(3),
        PlayerId(1),
        "Goblin".to_string(),
        Zone::Battlefield,
    );

    // Resolve the prevention sub-ability — installs the shield on the chosen
    // creature with EOT expiry and stashes the counter rider as the
    // post-replacement continuation.
    let prevent = build_gatta_prevention_chain(gatta, chosen, PlayerId(0));
    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &prevent, &mut events, 0).unwrap();

    // Shield must land on the chosen creature, not on Gatta.
    let chosen_obj = state.objects.get(&chosen).unwrap();
    assert_eq!(
        chosen_obj.replacement_definitions.len(),
        1,
        "shield must be hosted on the chosen target — got {:?}",
        chosen_obj.replacement_definitions
    );
    assert!(matches!(
        chosen_obj.replacement_definitions[0].shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        }
    ));
    // CR 514.2 cleanup contract: `turns::execute_cleanup` prunes on the typed
    // `expiry` field and ONLY on it. `shield_kind` classifies what the replacement
    // does and carries no lifetime meaning — CR 604.2 makes a printed static's
    // shield durable while holding this identical `ShieldKind` value. The duration
    // plumbing on the ability is therefore load-bearing, not advisory: this
    // ability's `Duration::UntilEndOfTurn` is what stamps the window here.
    assert_eq!(
        chosen_obj.replacement_definitions[0].expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "CR 514.2: the ability's stated 'this turn' window must be stamped on the shield"
    );

    let gatta_obj = state.objects.get(&gatta).unwrap();
    assert!(
        gatta_obj.replacement_definitions.is_empty(),
        "shield must NOT be installed on Gatta — got {:?}",
        gatta_obj.replacement_definitions
    );

    // Fire three damage events of varying sizes (4, 1, 7 — total 12) by
    // resolving an `Effect::DealDamage` ability whose source is the attacker.
    // Each prevention event must (a) absorb all damage, (b) re-fire the
    // rider adding counters equal to the prevented amount.
    let damage_amounts: [i32; 3] = [4, 1, 7];
    let mut expected_counters: u32 = 0;
    for dmg in damage_amounts {
        let damage_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: dmg },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(chosen)],
            attacker,
            PlayerId(1),
        );
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &damage_ability, &mut events, 0).unwrap();

        expected_counters += dmg as u32;
        let chosen_obj = state.objects.get(&chosen).unwrap();
        assert_eq!(
            chosen_obj.damage_marked, 0,
            "no damage should be marked after prevention (dmg={dmg})"
        );
        let counters = chosen_obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counters, expected_counters,
            "P1P1 counters must accumulate to {expected_counters} after dmg {dmg}; got {counters}"
        );
    }

    // Total: 12 damage prevented → 12 P1P1 counters on the chosen creature.
    let chosen_obj = state.objects.get(&chosen).unwrap();
    assert_eq!(
        chosen_obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        12,
        "total accumulated P1P1 counters must equal total prevented damage"
    );
    assert_eq!(chosen_obj.damage_marked, 0);

    // Shield must STILL be present after all three events — `Prevention { All }`
    // is duration-bound, not depletion-bound (CR 615.1a).
    let chosen_obj = state.objects.get(&chosen).unwrap();
    assert_eq!(
        chosen_obj.replacement_definitions.len(),
        1,
        "shield must persist across all three damage events"
    );
    assert!(
        !chosen_obj.replacement_definitions[0].is_consumed,
        "Prevention {{ All }} must not be consumed by use — got consumed shield"
    );
}

/// CR 615.1a + CR 615.5: Pinwheel test — confirm that 4 damage in one event
/// vs. 4 separate 1-damage events both produce 4 counters total. This locks
/// in the per-event accumulation model: counters scale with the prevented
/// amount per event, not per damage point.
#[test]
fn gatta_and_luzzu_pinwheel_one_event_vs_split_events_yield_same_total() {
    fn run_with_damage_profile(events_to_fire: &[i32]) -> u32 {
        let mut state = GameState::new_two_player(42);
        let gatta = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gatta and Luzzu".to_string(),
            Zone::Battlefield,
        );
        let chosen = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );

        let prevent = build_gatta_prevention_chain(gatta, chosen, PlayerId(0));
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &prevent, &mut events, 0).unwrap();

        for dmg in events_to_fire {
            let damage_ability = ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: *dmg },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
                vec![TargetRef::Object(chosen)],
                attacker,
                PlayerId(1),
            );
            let mut events = Vec::new();
            effects::resolve_ability_chain(&mut state, &damage_ability, &mut events, 0).unwrap();
        }
        state
            .objects
            .get(&chosen)
            .unwrap()
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    }

    let single_event = run_with_damage_profile(&[4]);
    let split_events = run_with_damage_profile(&[1, 1, 1, 1]);
    assert_eq!(
        single_event, 4,
        "one 4-damage event prevented must add 4 P1P1 counters"
    );
    assert_eq!(
        split_events, 4,
        "four 1-damage events prevented must add 4 P1P1 counters total"
    );
    assert_eq!(
        single_event, split_events,
        "per-event accumulation must yield the same total as one big event"
    );
}

/// #8777, PR #8849 — end-to-end production-path coverage: Gatta and Luzzu is
/// parsed from verbatim Oracle text, cast through `GameRunner::cast` (real
/// mana payment, real stack placement), its ETB trigger's target is chosen
/// through the real `WaitingFor::TriggerTargetSelection` prompt (CR 603.3d —
/// a triggered ability's target is chosen when the ability is put on the
/// stack), and a real cast Lightning Bolt then deals the damage that the
/// installed shield must prevent and convert into +1/+1 counters (CR 615.1a,
/// CR 615.5). This is the row the hand-built tests in this file cannot cover:
/// it is the only one that reaches
/// `effects::bind_detached_continuation_to_parent` (defined in
/// `crates/engine/src/game/effects/mod.rs`, called from
/// `crates/engine/src/game/effects/prevent_damage.rs`) by way of the actual
/// parser-to-runtime path — parse, cast, ETB target selection, damage,
/// counters — rather than from a hand-constructed `ResolvedAbility`.
#[test]
fn gatta_and_luzzu_prevents_through_the_real_cast_pipeline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gatta = {
        let mut builder = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Gatta and Luzzu",
            1,
            1,
            GATTA_AND_LUZZU_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 2,
        });
        builder.id()
    };
    // Two creatures P0 controls: `bear` is the target we will explicitly
    // choose; `decoy` is an equally legal alternative that we do NOT choose,
    // so the eventual choice proves a real selection rather than "the only
    // option available".
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let decoy = scenario.add_creature(P0, "Decoy Ox", 3, 3).id();
    // An opponent's creature: must NOT appear in the trigger's legal target
    // set, proving the parsed "creature you control" filter is doing real
    // work rather than accepting any creature.
    let hostile = scenario.add_creature(P1, "Hostile Goblin", 4, 4).id();
    let bolt = scenario.add_bolt_to_hand(P1);

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();

    // --- Cast Gatta and Luzzu through the production cast pipeline. ---
    let mut commit = runner.cast(gatta).commit();
    for _ in 0..20 {
        match commit.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => break,
            WaitingFor::OrderTriggers { triggers, .. } => {
                commit
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering Gatta and Luzzu's lone ETB trigger should succeed");
            }
            WaitingFor::Priority { .. } => {
                commit.act(GameAction::PassPriority).expect(
                    "priority passes should resolve Gatta and Luzzu and put its ETB \
                         trigger on the stack",
                );
            }
            other => panic!("unexpected state before Gatta and Luzzu's target prompt: {other:?}"),
        }
    }

    // Reach guard: the real TriggerTargetSelection prompt (CR 603.3d) was
    // actually surfaced, naming exactly one slot whose legal set is the
    // parsed "target creature you control" filter — both of P0's creatures,
    // and NOT the opponent's. Without this the ChooseTarget action below
    // would be answering a fabricated prompt, not Gatta's real one.
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        source_id,
        ..
    } = commit.state().waiting_for.clone()
    else {
        panic!(
            "expected Gatta and Luzzu's ETB trigger to reach TriggerTargetSelection, got {:?}",
            commit.state().waiting_for
        );
    };
    assert_eq!(
        source_id,
        Some(gatta),
        "the surfaced prompt must belong to Gatta and Luzzu's own ETB trigger"
    );
    assert_eq!(
        target_slots.len(),
        1,
        "the parsed trigger names exactly one target slot"
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(bear)),
        "Bear (a creature P0 controls) must be a legal target: {:?}",
        target_slots[0].legal_targets
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(decoy)),
        "Decoy Ox must also be legal — this is what makes the explicit choice below \
         non-vacuous: {:?}",
        target_slots[0].legal_targets
    );
    assert!(
        !target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(hostile)),
        "CR 603.3d + the parsed 'creature you control' filter: an opponent's creature must \
         not be a legal target: {:?}",
        target_slots[0].legal_targets
    );

    // Answer the real prompt through the runner with an explicit choice of
    // Bear over Decoy Ox — the production target-selection action, not a
    // hand-picked target.
    commit
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choosing Bear as Gatta and Luzzu's target should succeed");

    let cast_outcome = commit.resolve();
    assert!(
        matches!(
            cast_outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ),
        "Gatta and Luzzu's cast and ETB trigger must resolve to a clean priority window, got \
         {:?}",
        cast_outcome.final_waiting_for()
    );

    // Reach guard: the shield really did install on the CHOSEN creature —
    // not on Gatta and Luzzu itself, and not on the unchosen Decoy Ox.
    let bear_after_etb = &cast_outcome.state().objects[&bear];
    assert_eq!(
        bear_after_etb.replacement_definitions.len(),
        1,
        "the prevention shield must be installed on the chosen Bear: {:?}",
        bear_after_etb.replacement_definitions
    );
    assert!(
        matches!(
            bear_after_etb.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ),
        "the installed shield must be an All-damage prevention shield"
    );
    assert!(
        cast_outcome.state().objects[&gatta]
            .replacement_definitions
            .is_empty(),
        "the shield must NOT be installed on Gatta and Luzzu itself"
    );
    assert!(
        cast_outcome.state().objects[&decoy]
            .replacement_definitions
            .is_empty(),
        "the shield must NOT be installed on the unchosen Decoy Ox"
    );

    // --- Deal damage to Bear through a real cast Lightning Bolt. ---
    // Hand P1 priority so it can cast. This is a harness shortcut, not a legal
    // game transition — Bolt is an instant and would not need the active player
    // to change at all; the reassignment only saves passing a full turn. It
    // cannot affect what is under test: the shield's `UntilEndOfTurn` expiry is
    // keyed to the turn, which this does not advance (CR 514.2), and the rider
    // was already bound when the trigger resolved, above.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    let bolt_outcome = runner.cast(bolt).target_object(bear).resolve();

    assert_eq!(
        bolt_outcome.damage_marked(bear),
        0,
        "CR 615.1a: Gatta and Luzzu's shield must prevent all 3 damage from the real cast \
         Lightning Bolt"
    );
    bolt_outcome.assert_counters(bear, CounterType::Plus1Plus1, 3);
    // CR 615.5's "put that many +1/+1 counters on it" names the parent's
    // chosen target only — the unchosen Decoy Ox and Gatta and Luzzu itself
    // must gain none.
    bolt_outcome.assert_counters(decoy, CounterType::Plus1Plus1, 0);
    bolt_outcome.assert_counters(gatta, CounterType::Plus1Plus1, 0);
}
