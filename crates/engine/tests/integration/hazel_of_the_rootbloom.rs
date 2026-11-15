//! CR 608.2c — a demonstrative anaphor on a DIVERGENT noun ("that token", "that
//! artifact", "that land") binds to the enclosing ability's DECLARED OBJECT
//! TARGET under a present-tense copula, not to a zone-change event object.
//!
//! The defect: two hand-maintained noun vocabularies had silently drifted apart.
//! `parse_target_demonstrative_subject` accepted three nouns
//! (creature/permanent/card); `parse_zone_change_object_type_text` accepted nine
//! (those plus enchantment/artifact/equipment/aura/land/token). Every noun in
//! the gap therefore fell through to `ZoneChangeObjectMatchesFilter`, which reads
//! `state.current_trigger_event` — `None` on a `Phase` trigger — so the gated
//! branch could never fire.
//!
//! Witness (Oracle text fetched from Scryfall, verbatim below):
//!   Hazel of the Rootbloom — "At the beginning of your end step, create a token
//!   that's a copy of target token you control. If that token is a Squirrel,
//!   instead create two tokens that are copies of it."
//!
//! `Jackknight` and `Thieving Skydiver` print the SAME noun ("that artifact") on
//! the SAME trigger mode (`ChangesZone`) with OPPOSITE anaphora — Jackknight's
//! names the entering object, Skydiver's names the ability's chosen target — so
//! no edit to either noun list can be the correct general rule. The
//! discriminator has to be the enclosing ability's declared shape, which is what
//! `ParseContext::chain_declared_object_target` + `slot_matches_anaphor` supply.
//!
//! REVERT DISCRIMINATORS, per test:
//!   * `hazel_end_step_copies_squirrel_token_twice` — the primary. Restore the
//!     `token` noun's refusal (or move the chunk-loop seeding back inside the
//!     `strip_leading_general_conditional` window, where the instead path never
//!     sees it) and the gate re-emits `ZoneChangeObjectMatchesFilter`, which is
//!     false on a `Phase` trigger, so 2 Squirrels appear instead of 3.
//!   * `hazel_end_step_copies_non_squirrel_token_once` — the discrimination
//!     probe: a condition made unconditional yields 3 here instead of 2.
//!   * `overgrowth_elemental_dies_rider_still_gates_on_subtype` — the OVERLAP
//!     nouns must keep their unconditional target-route precedence.
//!   * `emeria_shepherd_land_rider_survives_double_authority` — the agreement
//!     predicate's `type_ok` + zone conjunct on a double-authority ability.
//!   * `jackknight_contraption_rider_still_reads_the_entering_artifact` — the
//!     zone-change route's non-regression, via the walk's fail-closed arm.
//!   * `thieving_skydiver_equipment_rider_condition_becomes_live` — the same
//!     rule on a different noun, effect and reach (the leading-conditional
//!     cascade rather than the instead path).

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::{AbilityCondition, Effect, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Hazel of the Rootbloom, `{2}{B}{G}` Legendary Creature — Squirrel Druid 3/5.
/// Verbatim from Scryfall (`api.scryfall.com/cards/named?exact=Hazel+of+the+Rootbloom`),
/// byte-compared against `client/public/card-data.json`.
const HAZEL: &str = "{T}, Pay 2 life, Tap X untapped tokens you control: Add X mana in any combination of colors.\nAt the beginning of your end step, create a token that's a copy of target token you control. If that token is a Squirrel, instead create two tokens that are copies of it.";

/// Overgrowth Elemental — verbatim. The rider's noun is the OVERLAP noun
/// "creature" under a PAST-tense copula, so it must keep its existing
/// unconditional target-route claim.
const OVERGROWTH_ELEMENTAL: &str = "When this creature enters, put a +1/+1 counter on another target Elemental you control.\nWhenever another creature you control dies, you gain 1 life. If that creature was an Elemental, put a +1/+1 counter on this creature.";

/// Emeria Shepherd — verbatim. The double-authority fixture: one ability with a
/// zone-change event object (the entering land) AND a declared object target
/// (the graveyard card).
const EMERIA_SHEPHERD: &str = "Flying\nLandfall — Whenever a land you control enters, you may return target nonland permanent card from your graveyard to your hand. If that land is a Plains, you may return that nonland permanent card to the battlefield instead.";

/// Jackknight — verbatim. Same noun as Thieving Skydiver, OPPOSITE anaphora:
/// "that artifact" is the ENTERING object, so the zone-change route must keep it.
const JACKKNIGHT: &str = "Whenever another artifact you control enters, put a +1/+1 counter on this creature. If that artifact is a Contraption, this creature gains lifelink until end of turn.";

/// Thieving Skydiver — verbatim, kicker reminder text included (byte-verified
/// against `client/public/card-data.json`).
const THIEVING_SKYDIVER: &str = "Kicker {X}. X can't be 0. (You may pay an additional {X} as you cast this spell.)\nFlying\nWhen this creature enters, if it was kicked, gain control of target artifact with mana value X or less. If that artifact is an Equipment, attach it to this creature.";

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Count P0's battlefield TOKENS carrying `subtype`. Deliberately restricted to
/// tokens: Hazel herself is a Squirrel *card*, not a token (CR 111.1).
fn token_count_with_subtype(state: &GameState, subtype: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.is_token
                    && o.controller == P0
                    && o.card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(subtype))
            })
        })
        .count()
}

/// Count ALL of P0's battlefield tokens, regardless of subtype.
fn token_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.controller == P0)
        })
        .count()
}

/// Turn an already-created creature into a real token with the given subtype,
/// untapped and not summoning-sick.
fn make_token(runner: &mut GameRunner, id: ObjectId, subtype: &str) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.is_token = true;
    obj.tapped = false;
    obj.summoning_sick = false;
    obj.card_types.subtypes = vec![subtype.to_string()];
    obj.base_card_types = obj.card_types.clone();
}

/// Drive every trigger/targeting prompt to completion, auto-selecting the first
/// legal target and accepting every optional prompt, and return the accumulated
/// event log. Returns when priority is held with an empty stack.
fn resolve_all(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..120 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::TriggerTargetSelection { target_slots, .. }
            | WaitingFor::TargetSelection { target_slots, .. } => {
                let target = target_slots
                    .first()
                    .and_then(|slot| slot.legal_targets.first())
                    .cloned();
                if target.is_none() {
                    break;
                }
                match runner.act(GameAction::ChooseTarget { target }) {
                    Ok(result) => events.extend(result.events),
                    Err(_) => break,
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                match runner.act(GameAction::DecideOptionalEffect { accept: true }) {
                    Ok(result) => events.extend(result.events),
                    Err(_) => break,
                }
            }
            _ => match runner.act(GameAction::PassPriority) {
                Ok(result) => events.extend(result.events),
                Err(_) => break,
            },
        }
    }
    events
}

/// Reach guard shared by every fixture: the card parsed with zero
/// `Effect::Unimplemented`, so no negative assertion below can be satisfied
/// vacuously by an upstream parse failure.
fn assert_parses_cleanly(runner: &GameRunner, id: ObjectId, name: &str) {
    let obj = &runner.state().objects[&id];
    assert!(
        !obj.abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "{name} must parse with zero Effect::Unimplemented, got {:?}",
        obj.abilities
    );
    for entry in obj.trigger_definitions.iter_unchecked() {
        if let Some(execute) = entry.definition().execute.as_ref() {
            assert!(
                !matches!(&*execute.effect, Effect::Unimplemented { .. }),
                "{name}'s trigger body must parse, got {:?}",
                execute.effect
            );
        }
    }
}

/// Mark `victim` with lethal damage, run SBAs so it dies, dispatch the resulting
/// triggers, then resolve the stack.
fn kill_and_resolve(runner: &mut GameRunner, victim: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .expect("victim exists")
        .damage_marked = 99;
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    resolve_all(runner);
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// T1 / T2 — Hazel of the Rootbloom, the primary fix
// ---------------------------------------------------------------------------

/// Set Hazel up with exactly one token of `subtype` under P0 and advance to P0's
/// end step, resolving the trigger. Returns `(runner, hazel_id, token_id)`.
fn hazel_end_step(subtype: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hazel = scenario
        .add_creature_from_oracle(P0, "Hazel of the Rootbloom", 3, 5, HAZEL)
        .id();
    let token = scenario.add_creature(P0, subtype, 1, 1).id();
    let mut runner = scenario.build();
    make_token(&mut runner, token, subtype);
    assert_parses_cleanly(&runner, hazel, "Hazel of the Rootbloom");

    // CR 508.1: Hazel and the token can attack, so the declare-attackers
    // turn-based action surfaces a prompt `advance_to_phase` cannot auto-pass —
    // it would stall in combat and leave the end step unreached. Cross combat
    // explicitly rather than letting the phase helper stop there.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach guard: the scenario must actually reach P0's end step, or the \
         token-count assertions below pass vacuously by never triggering"
    );
    // Reach guard: the trigger is genuinely on the stack (or awaiting its
    // target) before anything is resolved.
    assert!(
        !runner.state().stack.is_empty()
            || matches!(
                runner.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. } | WaitingFor::OrderTriggers { .. }
            ),
        "reach guard: Hazel's end-step trigger must be pending, got stack {:?} / {:?}",
        runner.state().stack,
        runner.state().waiting_for
    );

    resolve_all(&mut runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the trigger must resolve to a clean priority window, got {:?}",
        runner.state().waiting_for
    );
    (runner, hazel, token)
}

/// **T1 — PRIMARY.** CR 707.1/707.2 + CR 111.6 + CR 614.1a: the target token IS
/// a Squirrel, so the "instead create two tokens" replacement applies and the
/// battlefield ends with 1 original + 2 copies.
///
/// Revert probe: with the `token` noun refused by
/// `parse_target_type_membership_condition`, the gate is a
/// `ZoneChangeObjectMatchesFilter { destination: Battlefield }`, which reads
/// `state.current_trigger_event` — `None` on this `Phase` trigger — so the
/// instead branch never fires and this count is 2, not 3.
#[test]
fn hazel_end_step_copies_squirrel_token_twice() {
    let (runner, _hazel, token) = hazel_end_step("Squirrel");
    assert!(
        runner.state().objects[&token].zone == Zone::Battlefield,
        "reach guard: the copied token must still be on the battlefield"
    );
    assert_eq!(
        token_count_with_subtype(runner.state(), "Squirrel"),
        3,
        "CR 614.1a: the target token IS a Squirrel, so TWO copies are created \
         instead of one — 1 original + 2 copies == 3"
    );
}

/// **T2 — the DISCRIMINATION probe.** The target token is NOT a Squirrel, so the
/// replacement does not apply and exactly ONE copy is created.
///
/// Revert probe: a condition made unconditional (filter dropped, or lowered as
/// `TargetMatchesFilter { Any }`) yields 3 here; an anaphor bound to the wrong
/// object fails too. The paired positives below stop a total parse failure from
/// satisfying "exactly one copy" vacuously.
#[test]
fn hazel_end_step_copies_non_squirrel_token_once() {
    let (runner, hazel, _token) = hazel_end_step("Soldier");

    // (b) The rider's condition is present and is the target anaphor wrapped in
    // the "instead" replacement (CR 614.1a + CR 608.2c) — asserted on BOTH the
    // wrapper and its inner variant, because `ConditionInstead` is Hazel's
    // actual outer variant and an assertion naming `TargetMatchesFilter` alone
    // would fail on a correct fix.
    let trigger = runner.state().objects[&hazel]
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| entry.definition())
        .find(|t| t.execute.is_some())
        .expect("Hazel's end-step trigger must be published");
    let sub = trigger
        .execute
        .as_ref()
        .unwrap()
        .sub_ability
        .as_ref()
        .expect("the 'instead create two tokens' rider must be a sub-ability");
    let condition = sub
        .condition
        .as_ref()
        .expect("the rider must carry its printed CR 608.2c condition");
    let AbilityCondition::ConditionInstead { inner } = condition else {
        panic!("expected ConditionInstead, got {condition:?}");
    };
    let AbilityCondition::TargetMatchesFilter {
        filter, use_lki, ..
    } = inner.as_ref()
    else {
        panic!("expected TargetMatchesFilter inside ConditionInstead, got {inner:?}");
    };
    assert!(!use_lki, "present-tense 'is' must read current state");
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected a Typed subtype filter, got {filter:?}");
    };
    assert_eq!(
        tf.type_filters,
        vec![TypeFilter::Subtype("Squirrel".to_string())]
    );

    // (c) Exactly one copy was created.
    assert_eq!(
        token_count(runner.state()),
        2,
        "a non-Squirrel target must yield exactly ONE copy — 1 original + 1 copy"
    );
    assert_eq!(
        token_count_with_subtype(runner.state(), "Soldier"),
        2,
        "the copy must be a copy of the Soldier token (CR 707.2)"
    );
    assert_eq!(
        token_count_with_subtype(runner.state(), "Squirrel"),
        0,
        "no Squirrel TOKEN may appear — Hazel is a Squirrel card, not a token"
    );
}

// ---------------------------------------------------------------------------
// T6 — the OVERLAP-noun negative boundary
// ---------------------------------------------------------------------------

/// **T6.** The overlap nouns (creature/permanent/card) keep their unconditional
/// target-route precedence, so Overgrowth Elemental's past-tense dies rider
/// still gates on the subtype.
///
/// Revert probe: give `creature` the divergent route and the membership arm's
/// decline leaves the PAST-tense condition with no owner at all — the
/// zone-change route has no ` was ` arm — so the rider either fires
/// unconditionally or lowers as `Unimplemented`. Both cases are asserted here,
/// so T6 fails whichever it is.
#[test]
fn overgrowth_elemental_dies_rider_still_gates_on_subtype() {
    for (victim_subtype, expect_counter) in [("Elemental", true), ("Goblin", false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let elemental = scenario
            .add_creature_from_oracle(P0, "Overgrowth Elemental", 3, 4, OVERGROWTH_ELEMENTAL)
            .id();
        let victim = scenario.add_creature(P0, victim_subtype, 2, 2).id();
        let mut runner = scenario.build();
        {
            let obj = runner.state_mut().objects.get_mut(&victim).unwrap();
            obj.card_types.subtypes = vec![victim_subtype.to_string()];
            obj.base_card_types = obj.card_types.clone();
        }
        assert_parses_cleanly(&runner, elemental, "Overgrowth Elemental");

        let life_before = runner.life(P0);
        let counters_before = plus_counters(&runner, elemental);
        kill_and_resolve(&mut runner, victim);

        // Positive in BOTH cases: the trigger resolved and the ability is not
        // `Unimplemented`.
        assert_eq!(
            runner.life(P0) - life_before,
            1,
            "the dies trigger must gain 1 life in the {victim_subtype} case, \
             proving it resolved"
        );
        let gained = plus_counters(&runner, elemental) - counters_before;
        if expect_counter {
            assert_eq!(
                gained, 1,
                "an ELEMENTAL death must add a +1/+1 counter (CR 400.7 LKI)"
            );
        } else {
            assert_eq!(
                gained, 0,
                "a non-Elemental death must NOT add a +1/+1 counter"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// T7 — the multi-authority fixture, and the only test that reaches
//      `slot_matches_anaphor`
// ---------------------------------------------------------------------------

/// **T7.** Emeria Shepherd carries a zone-change event object (the entering
/// land) AND a declared object target (the graveyard card) at the same time.
/// "That land" must stay bound to the EVENT object, so a Plains returns the card
/// to the battlefield and any other land returns it to hand.
///
/// Revert probe: make `slot_matches_anaphor` return `true` for a type-mismatched
/// slot (or drop the agreement conjunct) and the gate becomes a
/// `TargetMatchesFilter`, which resolves against the graveyard card — never a
/// Plains — and is false in BOTH cases, so Case A wrongly leaves the card in
/// hand. First production branch reached: `type_ok`
/// (`TypeFilter::Land` ∉ `[Permanent, Non(Land)]`), then the zone conjunct
/// (`InZone(Graveyard)` ⇒ `CardInNonBattlefieldZone` ≠ `BattlefieldPermanent`).
#[test]
fn emeria_shepherd_land_rider_survives_double_authority() {
    for (land_name, expected) in [("Plains", Zone::Battlefield), ("Forest", Zone::Hand)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let shepherd = scenario
            .add_creature_from_oracle(P0, "Emeria Shepherd", 4, 4, EMERIA_SHEPHERD)
            .id();
        let buried = scenario
            .add_creature_to_graveyard(P0, "Buried Bear", 2, 2)
            .id();
        let land = scenario
            .add_land_to_hand(P0, land_name)
            .with_subtypes(vec![land_name])
            .id();
        let mut runner = scenario.build();
        assert_parses_cleanly(&runner, shepherd, "Emeria Shepherd");
        assert_eq!(
            runner.state().objects[&buried].zone,
            Zone::Graveyard,
            "reach guard: the nonland permanent card must start in the graveyard"
        );

        let card_id = runner.state().objects[&land].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .expect("P0 plays the land, firing landfall");
        resolve_all(&mut runner);

        // Paired positive in BOTH cases: the card LEFT the graveyard, so the
        // "hand, not battlefield" case is not satisfied by a no-op resolution.
        assert_ne!(
            runner.state().objects[&buried].zone,
            Zone::Graveyard,
            "the landfall trigger must return the card out of the graveyard \
             ({land_name} case)"
        );
        assert_eq!(
            runner.state().objects[&buried].zone,
            expected,
            "with {land_name} entering, the returned card must end in {expected:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// T9 — the zone-change route's non-regression, same noun as T8
// ---------------------------------------------------------------------------

/// **T9.** Jackknight prints "that artifact" with the OPPOSITE anaphora to
/// Thieving Skydiver: it names the ENTERING object, so the zone-change route
/// must keep it.
///
/// First production branch reached: `chain_declared_object_target`'s fail-closed
/// arm — the prior clause is `PutCounter { target: SelfRef }`, a non-`Typed`
/// declared target, so the walk returns `None` and the agreement test declines.
/// (This test does NOT discriminate fail-closed from skip: Jackknight's chain has
/// exactly one prior clause, so both designs return `None`. The discriminating
/// fixture is `chain_declared_object_target_blocks_at_a_non_typed_nearer_clause`
/// in `parser::oracle_effect`.)
#[test]
fn jackknight_contraption_rider_still_reads_the_entering_artifact() {
    for (subtype, expect_lifelink) in [("Contraption", true), ("Equipment", false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let jackknight = scenario
            .add_creature_from_oracle(P0, "Jackknight", 3, 3, JACKKNIGHT)
            .id();
        let entering = scenario
            .add_creature_to_hand(P0, "Entering Artifact", 1, 1)
            .as_artifact()
            .with_subtypes(vec![subtype])
            .id();
        let mut runner = scenario.build();
        assert_parses_cleanly(&runner, jackknight, "Jackknight");
        {
            let obj = runner.state_mut().objects.get_mut(&jackknight).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
            obj.base_card_types = obj.card_types.clone();
        }

        let counters_before = plus_counters(&runner, jackknight);
        let card_id = runner.state().objects[&entering].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: entering,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("P0 casts the entering artifact");
        resolve_all(&mut runner);
        assert_eq!(
            runner.state().objects[&entering].zone,
            Zone::Battlefield,
            "reach guard: the artifact must actually enter ({subtype} case)"
        );

        // Positive in BOTH cases: the trigger resolved.
        assert_eq!(
            plus_counters(&runner, jackknight) - counters_before,
            1,
            "the enters trigger must add its +1/+1 counter in the {subtype} case"
        );
        let has_lifelink = runner.state().objects[&jackknight]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Lifelink));
        assert_eq!(
            has_lifelink, expect_lifelink,
            "the Contraption rider must gate on the ENTERING artifact's subtype \
             ({subtype} case)"
        );
    }
}

// ---------------------------------------------------------------------------
// T8 — the same rule on a different noun, effect and reach
// ---------------------------------------------------------------------------

/// **T8.** Thieving Skydiver reaches the gate through the
/// `strip_leading_general_conditional` cascade rather than Hazel's instead path,
/// with a different noun (`artifact`), a different effect (`GainControl`) and
/// `valid_card: SelfRef` — so it proves this is a CLASS fix, not a Hazel
/// special case.
///
/// Asserted at the strength Unit 1 owns: that the CONDITION went live. Case A
/// asserts the rider's `Attach` was REACHED; Case B asserts it was not.
/// `attached_to == skydiver` is deliberately NOT asserted — Skydiver's rider
/// body carries a SEPARATE, pre-existing defect (an equal-operand
/// `Attach { attachment: ParentTarget, target: ParentTarget }` self-attach the
/// engine rejects before any edit, documented at
/// `parser/oracle_effect/lower.rs`), which this change neither fixes nor widens.
///
/// Revert probe: restore the `artifact` noun's refusal and the gate becomes a
/// `ZoneChangeObjectMatchesFilter` evaluating `Subtype(Equipment)` against the
/// ENTERING Skydiver — a Human Rogue creature — so it is always false and Case
/// A's `Attach` is never reached.
///
/// This test is also the reach-guard for the open question of whether the
/// trigger's "if it was kicked" intervening-if lands as a CLAUSE condition: if
/// it did, the fail-closed walk would return `None` and Case A would fail.
#[test]
fn thieving_skydiver_equipment_rider_condition_becomes_live() {
    for (subtype, expect_attach) in [("Equipment", true), ("Clue", false)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let skydiver = scenario
            .add_creature_to_hand_from_oracle(P0, "Thieving Skydiver", 1, 1, THIEVING_SKYDIVER)
            .id();
        let loot = scenario
            .add_creature(engine::game::scenario::P1, "Opposing Artifact", 1, 1)
            .id();
        for _ in 0..10 {
            scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
        }
        let mut runner = scenario.build();
        {
            let obj = runner.state_mut().objects.get_mut(&loot).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.card_types.subtypes = vec![subtype.to_string()];
            obj.base_card_types = obj.card_types.clone();
            obj.power = None;
            obj.toughness = None;
            obj.base_power = None;
            obj.base_toughness = None;
        }

        let card_id = runner.state().objects[&skydiver].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: skydiver,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("P0 casts Thieving Skydiver");
        // Kicker {X}: announce X = 1 (X can't be 0), then pay it.
        let mut events = Vec::new();
        for _ in 0..12 {
            match &runner.state().waiting_for {
                WaitingFor::OptionalCostChoice { .. } => {
                    events.extend(
                        runner
                            .act(GameAction::DecideOptionalCost { pay: true })
                            .expect("P0 pays the kicker")
                            .events,
                    );
                }
                WaitingFor::ChooseXValue { .. } => {
                    events.extend(
                        runner
                            .act(GameAction::ChooseX { value: 1 })
                            .expect("CR 702.33: X can't be 0, so announce X = 1")
                            .events,
                    );
                }
                _ => break,
            }
        }
        events.extend(resolve_all(&mut runner));
        assert_eq!(
            runner.state().objects[&skydiver].zone,
            Zone::Battlefield,
            "reach guard: Skydiver must actually enter ({subtype} case)"
        );

        // Positive in BOTH cases: the ETB resolved and control changed.
        assert_eq!(
            runner.state().objects[&loot].controller,
            P0,
            "the kicked ETB must gain control of the target artifact ({subtype} case)"
        );

        let attach_reached = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Attach,
                    ..
                }
            )
        });
        assert_eq!(
            attach_reached, expect_attach,
            "the rider's condition must be LIVE and gate on the gained \
             artifact's Equipment subtype ({subtype} case)"
        );
    }
}
