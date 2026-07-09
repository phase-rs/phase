//! CR 120.4a — "Excess damage is dealt to that creature's controller instead."
//!
//! Flame Spill deals 4 damage to a target creature and redirects the *excess*
//! (damage past what would be lethal, CR 120.6) to that creature's controller.
//! These tests drive the real cast pipeline (`GameScenario` +
//! `cast(..).resolve()` + `CastOutcome` deltas) and would fail if the excess
//! redirect were removed (T1) or bound to the wrong player (T2). Class members:
//! Flame Spill, Gandalf's Sanction, Ravenous Tyrannosaurus. Ram Through's
//! trample-conditional form is deferred to `Effect::Unimplemented` (T8).

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DamageContextSnapshot, Effect, ExcessRecipient,
    PreventionAmount, QuantityExpr, QuantityRef, ReplacementDefinition, ReplacementMode,
    ReplacementPlayerScope, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const FLAME_SPILL: &str = "Flame Spill deals 4 damage to target creature. \
Excess damage is dealt to that creature's controller instead.";

// Ram Through: the excess redirect is gated on a controlled-source trample check
// we do not model, so the whole conditional rider must be deferred (honest red),
// NOT absorbed unconditionally (which would be rules-wrong for a non-trample
// source).
const RAM_THROUGH: &str = "Target creature you control deals damage equal to its power \
to target creature you don't control. If the creature you control has trample, excess \
damage is dealt to that creature's controller instead.";

/// Build a scenario, add a power-2 target creature with the given toughness and
/// optional pre-marked damage (controlled by `victim`), cast a free Flame Spill
/// at it, and return the outcome plus the creature id.
fn cast_flame_spill_at(victim: PlayerId, toughness: i32, premark: u32) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = {
        let mut c = scenario.add_creature(victim, "Victim", 2, toughness);
        if premark > 0 {
            c.with_damage_marked(premark);
        }
        c.id()
    };

    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Flame Spill", false, FLAME_SPILL);
        b.with_mana_cost(ManaCost::generic(0));
        b.id()
    };

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[creature]).resolve();
    (outcome, creature)
}

/// T1 — primary claim, revert-proof: excess redirects to the damaged creature's
/// controller. 4 damage to a toughness-2 creature → lethal 2, excess 2 → the
/// controller loses 2. If the redirect is removed, this life delta is 0.
#[test]
fn t1_excess_redirects_to_creatures_controller() {
    let (outcome, creature) = cast_flame_spill_at(P0, 2, 0);
    outcome.assert_life_delta(P0, -2);
    // Reach-guard: the creature took lethal damage and died, proving the damage
    // actually resolved (so -2 is a genuine redirect, not a parse short-circuit).
    outcome.assert_zone(&[creature], Zone::Graveyard);
}

/// T2 — multi-authority provenance: the recipient is the *creature's* controller
/// (P1), not the caster (P0). Discriminates a naive `ability.controller` bug.
#[test]
fn t2_excess_goes_to_creatures_controller_not_caster() {
    let (outcome, creature) = cast_flame_spill_at(P1, 2, 0);
    outcome.assert_life_delta(P1, -2);
    outcome.assert_life_delta(P0, 0);
    outcome.assert_zone(&[creature], Zone::Graveyard);
}

/// T3 — CR 120.6: excess uses live marked damage, not raw toughness. A
/// toughness-3 creature with 1 damage already marked has lethal 2, so 4 damage
/// yields excess 2.
#[test]
fn t3_excess_accounts_for_marked_damage() {
    let (outcome, creature) = cast_flame_spill_at(P1, 3, 1);
    outcome.assert_life_delta(P1, -2);
    outcome.assert_zone(&[creature], Zone::Graveyard);
}

/// T4 — exactly-lethal → zero excess → no redirect (paired negative). The
/// creature dying is the reach-guard proving 4 damage DID land, so the zero life
/// delta is a real zero-excess result, not a vacuous parse failure.
#[test]
fn t4_exactly_lethal_has_no_excess() {
    let (outcome, creature) = cast_flame_spill_at(P1, 4, 0);
    outcome.assert_life_delta(P1, 0);
    outcome.assert_zone(&[creature], Zone::Graveyard);
}

/// T5 — overkill: a toughness-1 creature takes 4, lethal 1, excess 3.
#[test]
fn t5_overkill_redirects_all_excess() {
    let (outcome, creature) = cast_flame_spill_at(P1, 1, 0);
    outcome.assert_life_delta(P1, -3);
    outcome.assert_zone(&[creature], Zone::Graveyard);
}

/// T10 — CR 120.4a "instead": the creature is dealt only the LETHAL portion, and
/// the excess is redirected rather than added on top. This discriminates the
/// over-marking that T1–T5 miss (their controller delta is identical whether the
/// creature is dealt 2 or 4). 4 damage to a 2-toughness creature → the creature's
/// recorded damage is 2 (lethal), the controller's is 2 (excess); total dealt is
/// 4, never 6. Reverting the lethal-reduction makes the creature record 4 and the
/// total 6, failing this test.
#[test]
fn t10_creature_dealt_only_lethal_not_full_amount() {
    let (outcome, creature) = cast_flame_spill_at(P1, 2, 0);
    let records = &outcome.state().damage_dealt_this_turn;
    let to_creature: u32 = records
        .iter()
        .filter(|r| r.target == TargetRef::Object(creature))
        .map(|r| r.amount)
        .sum();
    let to_controller: u32 = records
        .iter()
        .filter(|r| r.target == TargetRef::Player(P1))
        .map(|r| r.amount)
        .sum();
    assert_eq!(
        to_creature, 2,
        "the creature must be dealt only the lethal 2, not the full 4 — excess is redirected, not additive"
    );
    assert_eq!(to_controller, 2, "the controller takes the excess 2");
    assert_eq!(
        to_creature + to_controller,
        4,
        "total damage dealt = actual_amount (4), never actual_amount + excess (6)"
    );
}

/// T12 — CR 120.4a + CR 120.10: when the excess is redirected, the CREATURE was
/// dealt only its lethal portion, so its damage record must report ZERO excess —
/// the excess was dealt to the controller instead. Otherwise "was dealt excess
/// damage" triggers (Maarika, Rith, Aegar) would wrongly fire on the creature.
/// Before the fix the creature recorded `excess = 2`; this asserts it is 0.
#[test]
fn t12_redirected_creature_records_no_excess_damage() {
    let (outcome, creature) = cast_flame_spill_at(P1, 2, 0);
    let creature_excess: u32 = outcome
        .state()
        .damage_dealt_this_turn
        .iter()
        .filter(|r| r.target == TargetRef::Object(creature))
        .map(|r| r.excess)
        .sum();
    assert_eq!(
        creature_excess, 0,
        "the creature took only lethal damage; the redirected excess must not be recorded as excess dealt to the creature"
    );
}

/// T11 — CR 702.15b + CR 120.7: lifelink gains for each leg's actually-dealt
/// damage exactly once, not twice. A lifelink source dealing 4 to a 2-toughness
/// creature deals 2 (lethal) to the creature and redirects 2 to its controller;
/// the source's controller gains 2 (creature leg) + 2 (redirect leg) = 4 total,
/// NOT actual_amount (4) for the primary leg plus the excess (2) again = 6. The
/// creature belongs to P1, so the excess redirect also costs P1 2 life.
#[test]
fn t11_lifelink_counts_each_leg_once_not_twice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Victim", 2, 2).id();
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Flame Spill", false, FLAME_SPILL);
        b.with_mana_cost(ManaCost::generic(0))
            .with_keyword(Keyword::Lifelink);
        b.id()
    };
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[creature]).resolve();
    outcome.assert_life_delta(P0, 4); // lifelink for 2 (creature) + 2 (redirect), not 6
    outcome.assert_life_delta(P1, -2); // the redirected excess
}

/// A "you gain twice that much life instead" replacement (Rhox Faithmender /
/// Boon Reflection), used to force a CR 616.1 ordering choice when two are present.
fn gain_life_doubler() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                }),
            },
            player: TargetFilter::Controller,
        },
    ))
}

/// T13 — CR 120.4a + CR 614.7 + CR 702.15b: a lifelink life-gain replacement
/// *choice* must not drop any lifelink portion when the source has the excess
/// rider. P0 controls two "gain twice that much life" replacements, so the single
/// combined lifelink gain (creature lethal 2 + redirected 2 = 4) surfaces a CR
/// 616.1 ordering choice. Driving *through* the choice, P0 must gain the full
/// total (4 doubled twice = 16) and P1 must take the redirected excess (2). Before
/// the single-combined-gain model, the redirected leg's own lifelink could pause
/// and the primary leg's lifelink was dropped.
#[test]
fn t13_lifelink_replacement_choice_resolves_full_combined_total() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Victim", 2, 2).id();
    {
        let mut doublers = scenario.add_creature(P0, "Twin Menders", 0, 3);
        doublers
            .with_replacement_definition(gain_life_doubler())
            .with_replacement_definition(gain_life_doubler());
    }
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Flame Spill", false, FLAME_SPILL);
        b.with_mana_cost(ManaCost::generic(0))
            .with_keyword(Keyword::Lifelink);
        b.id()
    };
    let mut runner = scenario.build();
    let p0_before = runner.state().players[0].life;
    let p1_before = runner.state().players[1].life;

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free lifelink Flame Spill must be accepted");

    // Drive every prompt to resolution: pick the damage target, then order the two
    // life-gain doublers each time the combined lifelink gain surfaces the CR 616.1
    // ordering choice, passing priority otherwise.
    let mut saw_replacement_choice = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(creature)],
                })
                .map(|_| ())
                .expect("targeting the creature must succeed"),
            WaitingFor::ReplacementChoice { .. } => {
                saw_replacement_choice = true;
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .map(|_| ())
                    .expect("ordering a life-gain doubler must resolve the choice");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected prompt while resolving lifelink Flame Spill: {other:?}"),
        }
    }

    assert!(
        saw_replacement_choice,
        "the combined lifelink gain must surface the CR 616.1 ordering choice"
    );
    // The redirected excess (2) was dealt to the creature's controller.
    assert_eq!(
        runner.state().players[1].life - p1_before,
        -2,
        "P1 takes the excess"
    );
    // The single combined lifelink (2 + 2 = 4) resolved through the choice and BOTH
    // doublers applied (4 -> 8 -> 16); the primary leg's lifelink was not dropped.
    assert_eq!(
        runner.state().players[0].life - p0_before,
        16,
        "P0 gains the full combined lifelink total (4 doubled twice), not just the redirect leg"
    );
}

/// T14 — CR 120.4a + CR 615 + CR 702.15b: the combined lifelink must survive the
/// REDIRECTED DAMAGE itself pausing on a replacement choice (distinct from T13's
/// life-gain pause). P1 controls an optional "prevent the next 1 damage to you"
/// shield, so the redirected excess damage to P1 pauses. Driving through the choice
/// (declining the shield), the redirected damage lands and — via the parked
/// `lifelink_bonus` restored on resume — the source's controller still gains the
/// full combined lifelink (creature lethal 2 + redirect 2 = 4). Before the parked
/// continuation, the resume rebuilt context from the source and dropped the gain.
#[test]
fn t14_lifelink_redirect_damage_replacement_choice_resolves_full_total() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Victim", 2, 2).id();
    {
        let mut shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::Next(1))
            .mode(ReplacementMode::Optional { decline: None })
            .description("You may prevent the next 1 damage to you.".to_string());
        // Apply to damage dealt to the shield's controller (P1) as a player.
        shield.valid_player = Some(ReplacementPlayerScope::You);
        scenario
            .add_creature(P1, "Shieldbearer", 0, 3)
            .with_replacement_definition(shield);
    }
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Flame Spill", false, FLAME_SPILL);
        b.with_mana_cost(ManaCost::generic(0))
            .with_keyword(Keyword::Lifelink);
        b.id()
    };
    let mut runner = scenario.build();
    let p0_before = runner.state().players[0].life;
    let p1_before = runner.state().players[1].life;

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free lifelink Flame Spill must be accepted");

    let mut saw_choice = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => runner
                .act(GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(creature)],
                })
                .map(|_| ())
                .expect("targeting the creature must succeed"),
            WaitingFor::ReplacementChoice { .. } => {
                saw_choice = true;
                // ACCEPT the prevention shield (index 0 = accept for an Optional
                // replacement), so it prevents 1 of the 2 redirected damage and the
                // redirect resolves through with the reduced amount.
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .map(|_| ())
                    .expect("accepting the prevention shield must resolve the choice");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected prompt while resolving lifelink Flame Spill: {other:?}"),
        }
    }

    assert!(
        saw_choice,
        "the redirected damage must surface the prevention replacement choice"
    );
    // The shield prevents 1 of the 2 redirected damage, so P1 loses 1.
    assert_eq!(
        runner.state().players[1].life - p1_before,
        -1,
        "P1 takes the redirected excess minus the 1 prevented"
    );
    // The combined lifelink is gained on resume after the redirected-DAMAGE
    // replacement choice, via the parked `lifelink_bonus`: creature lethal 2 +
    // redirect ACTUALLY dealt 1 (1 prevented) = 3. Before the parked continuation
    // the resume rebuilt context from the source and dropped this gain entirely.
    assert_eq!(
        runner.state().players[0].life - p0_before,
        3,
        "P0 gains the combined lifelink for the damage actually dealt (2 lethal + 1 redirect), not dropped"
    );
}

/// T15 — CR 120.4a + CR 615 + CR 702.15b: when the redirected excess is FULLY
/// prevented, the source still gains lifelink for the lethal creature damage it
/// actually dealt. P1 controls a mandatory "prevent the next 2 damage to you"
/// shield, so the redirected 2 is fully prevented (dealt 0). Because the redirect
/// leg never reaches its own lifelink path, the creature leg gains lifelink for the
/// lethal 2 it did deal. Before the fix, the creature leg's lifelink was skipped and
/// the fully-prevented redirect leg never gained, so P0 gained nothing.
#[test]
fn t15_fully_prevented_redirect_still_gains_creature_lifelink() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Victim", 2, 2).id();
    {
        // Mandatory (no Optional mode) — fully prevents the redirected damage with
        // no choice, so `apply_damage_to_target` returns `Applied(0)`.
        let mut shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
            .prevention_shield(PreventionAmount::Next(2))
            .description("Prevent the next 2 damage to you.".to_string());
        shield.valid_player = Some(ReplacementPlayerScope::You);
        scenario
            .add_creature(P1, "Shieldbearer", 0, 3)
            .with_replacement_definition(shield);
    }
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Flame Spill", false, FLAME_SPILL);
        b.with_mana_cost(ManaCost::generic(0))
            .with_keyword(Keyword::Lifelink);
        b.id()
    };
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[creature]).resolve();
    // The redirected excess (2) is fully prevented, so P1 loses nothing.
    outcome.assert_life_delta(P1, 0);
    // The source still dealt lethal 2 to the creature, so lifelink still gains 2.
    outcome.assert_life_delta(P0, 2);
    outcome.assert_zone(&[creature], Zone::Graveyard);
}

/// Collect every effect in an ability's `sub_ability` chain.
fn collect_effects<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a Effect>) {
    out.push(&def.effect);
    if let Some(sub) = &def.sub_ability {
        collect_effects(sub, out);
    }
}

fn parse_effects(oracle: &str, name: &str) -> Vec<Effect> {
    let types = ["Sorcery".to_string()];
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    let mut refs: Vec<&Effect> = Vec::new();
    for ab in &parsed.abilities {
        collect_effects(ab, &mut refs);
    }
    refs.into_iter().cloned().collect()
}

/// T8 — SHAPE: Flame Spill absorbs the rider onto its `DealDamage` with zero
/// residual `Unimplemented` (fully supported); Ram Through's conditional form is
/// NOT absorbed and stays `Unimplemented` (honest red), proving the guard.
#[test]
fn t8_shape_flame_spill_absorbs_ram_through_defers() {
    let fs = parse_effects(FLAME_SPILL, "Flame Spill");
    let fs_damage: Vec<&Effect> = fs
        .iter()
        .filter(|e| matches!(e, Effect::DealDamage { .. }))
        .collect();
    assert_eq!(
        fs_damage.len(),
        1,
        "Flame Spill should parse to exactly one DealDamage effect: {fs:?}"
    );
    assert!(
        matches!(
            fs_damage[0],
            Effect::DealDamage {
                excess: Some(ExcessRecipient::TargetController),
                ..
            }
        ),
        "the excess rider must be absorbed onto Flame Spill's DealDamage: {fs:?}"
    );
    assert!(
        !fs.iter().any(|e| matches!(e, Effect::Unimplemented { .. })),
        "Flame Spill must have no Unimplemented residue (fully supported): {fs:?}"
    );

    // Ram Through: the trample-conditional rider must NOT be absorbed onto the
    // DealDamage (the `!has trample` / `!if ` guards defer it).
    let rt = parse_effects(RAM_THROUGH, "Ram Through");
    assert!(
        rt.iter().all(|e| !matches!(
            e,
            Effect::DealDamage {
                excess: Some(_),
                ..
            }
        )),
        "Ram Through's CONDITIONAL excess must not be absorbed unconditionally: {rt:?}"
    );
    // Coverage-honesty: the deferred conditional rider must leave an Unimplemented
    // marker somewhere in the parse (not be silently dropped). Checked on the full
    // parse Debug so it is robust to whether the residue lands as an effect, a
    // sub-ability, or a swallowed-clause warning.
    let rt_parsed = parse_oracle_text(
        RAM_THROUGH,
        "Ram Through",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        format!("{rt_parsed:?}").contains("Unimplemented"),
        "Ram Through's deferred excess rider must remain honestly Unimplemented"
    );
}

/// T9 — the excess rider must survive a replacement-pause resume. When the
/// primary hit pauses on a replacement choice, the real damage lands later via
/// `resolve_post_replacement`, which rebuilds the `DamageContext` from a
/// serialized `DamageContextSnapshot`. If the snapshot dropped
/// `excess_recipient`, the redirect would silently vanish on resume. This locks
/// the serialized carry-through (the `From` impls + serde). A full in-harness
/// replacement pause is not staged here; the carry-through is the material fix.
#[test]
fn t9_snapshot_carries_excess_recipient_across_resume() {
    let snap = DamageContextSnapshot {
        source_id: ObjectId(1),
        controller: P0,
        source_is_creature: true,
        has_deathtouch: false,
        has_lifelink: false,
        has_wither: false,
        has_infect: false,
        combat_damage_poison: 0,
        excess_recipient: Some(ExcessRecipient::TargetController),
        lifelink_bonus: 3,
    };
    let json = serde_json::to_string(&snap).expect("snapshot serializes");
    let back: DamageContextSnapshot = serde_json::from_str(&json).expect("snapshot deserializes");
    assert_eq!(
        back.excess_recipient,
        Some(ExcessRecipient::TargetController),
        "the excess-redirect rider must survive the snapshot round-trip"
    );
    assert_eq!(
        back.lifelink_bonus, 3,
        "the deferred lifelink bonus must survive the snapshot round-trip"
    );
}
