//! Runtime cast-pipeline coverage for the `"and with "<ability>""` entry rider
//! printed alongside an enters-with-counters clause (CR 614.1c), the shape five
//! Invasion/Apocalypse kicker lines use: "If this creature was kicked with its
//! {B} kicker, it enters with a +1/+1 counter on it and with \"Pay 3 life:
//! Regenerate this creature.\"" (Anavolver).
//!
//! Before the fix, `parse_enters_with_counters` composed only the counters and
//! the bare-keyword riders, so the quoted granted ability was never read at
//! all — five cards entered with their counters and silently without their
//! granted ability. The pinned parser test documented that counters-only claim;
//! these tests prove the grant now reaches the battlefield and functions.
//!
//! CR 611.2a is why the grant is modeled as a continuous grant with
//! `Duration::UntilHostLeavesPlay` rather than a one-shot effect: unlike
//! tapping, the granted ability is a characteristic the permanent must KEEP
//! while it remains on the battlefield.
//!
//! Built via the `/card-test` recipe: `GameScenario` + full verbatim Oracle
//! text + real kicker payments through `DecideOptionalCost`, so each entering
//! object's `kickers_paid` is populated authentically rather than seeded. Each
//! test asserts the +1/+1 counters alongside the grant so that a spell which
//! never resolved cannot satisfy either assertion vacuously. Degavolver and
//! Necravolver each decline the sibling kicker to isolate the quoted half and
//! prove variant gating both ways.
//!
//! REVERT DISCRIMINATOR: `degavolver_first_kicker_grants_regenerate`. Remove
//! the `compose_enters_with_entry_grants` quoted composition from
//! `parse_enters_with_counters` and the regenerate grant disappears while the
//! counters still land, so the grant assertion fails and the counter
//! reach-guard still passes.

use engine::game::combat::{get_valid_attacker_ids, validate_attackers, AttackTarget};
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, Effect, KickerVariant, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Degavolver {1}{W} 1/1 — full verbatim Oracle text. The {1}{B} (first)
/// kicker carries the quoted regenerate grant; the {R} (second) kicker carries
/// bare first strike.
const DEGAVOLVER: &str = "Kicker {1}{B} and/or {R} (You may pay an additional {1}{B} and/or {R} as you cast this spell.)\nIf this creature was kicked with its {1}{B} kicker, it enters with two +1/+1 counters on it and with \"Pay 3 life: Regenerate this creature.\"\nIf this creature was kicked with its {R} kicker, it enters with a +1/+1 counter on it and with first strike.";

/// Necravolver {2}{B} 2/2 — full verbatim Oracle text. The {W} (second) kicker
/// carries the quoted damage-lifegain trigger; the {1}{G} (first) kicker
/// carries bare trample.
const NECRAVOLVER: &str = "Kicker {1}{G} and/or {W} (You may pay an additional {1}{G} and/or {W} as you cast this spell.)\nIf this creature was kicked with its {1}{G} kicker, it enters with two +1/+1 counters on it and with trample.\nIf this creature was kicked with its {W} kicker, it enters with a +1/+1 counter on it and with \"Whenever this creature deals damage, you gain that much life.\"";

/// Prison Barricade {1}{W} 1/3 — full verbatim Oracle text. The single kicker
/// carries the quoted can-attack static.
const PRISON_BARRICADE: &str = "Defender (This creature can't attack.)\nKicker {1}{W} (You may pay an additional {1}{W} as you cast this spell.)\nIf this creature was kicked, it enters with a +1/+1 counter on it and with \"This creature can attack as though it didn't have defender.\"";

fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|color| ManaUnit::new(*color, ObjectId(0), false, vec![]))
        .collect()
}

/// Cast `id` through the real optional-cost flow, deciding each offered kicker
/// in order, and resolve it to the battlefield.
fn cast_with_kickers(runner: &mut GameRunner, id: ObjectId, decisions: &[bool]) {
    let card = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id: card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the kicker creature");
    for (i, pay) in decisions.iter().enumerate() {
        assert!(
            matches!(
                &runner.state().waiting_for,
                WaitingFor::OptionalCostChoice { .. }
            ),
            "the engine must offer kicker decision {i}, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalCost { pay: *pay })
            .expect("P0 decides the kicker");
    }
    while runner.state().objects.get(&id).map(|o| o.zone) != Some(Zone::Battlefield) {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the creature");
            }
            other => panic!("unexpected waiting state while resolving: {other:?}"),
        }
    }
}

fn counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn has_regeneration_shield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .replacement_definitions
        .as_slice()
        .iter()
        .any(|r| r.shield_kind.is_shield())
}

/// The index of `source`'s first activated ability whose effect matches `pick`.
fn ability_index(runner: &GameRunner, source: ObjectId, pick: fn(&Effect) -> bool) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|a| pick(&a.effect))
        .expect("the ability is present")
}

fn cast_degavolver(decisions: &[bool]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let degavolver = scenario
        .add_creature_to_hand_from_oracle(P0, "Degavolver", 1, 1, DEGAVOLVER)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::White,
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    cast_with_kickers(&mut runner, degavolver, decisions);
    (runner, degavolver)
}

/// PRIMARY REVERT DISCRIMINATOR. First kicker paid, so the quoted half of the
/// one replacement applies: two +1/+1 counters AND the granted "Pay 3 life:
/// Regenerate" ability — and the grant actually activates.
#[test]
fn degavolver_first_kicker_grants_regenerate() {
    let (mut runner, degavolver) = cast_degavolver(&[true, false]);
    assert_eq!(
        runner.state().objects[&degavolver].kickers_paid,
        vec![KickerVariant::First]
    );
    assert_eq!(
        counters(&runner, degavolver),
        2,
        "reach-guard: the kicked replacement must place two +1/+1 counters — if this is 0 \
         the replacement never applied and the grant assertions below would be vacuous"
    );
    // Variant isolation: the declined {R} half must not fire.
    assert!(
        !runner.state().objects[&degavolver].has_keyword(&Keyword::FirstStrike),
        "the declined second kicker must not grant first strike"
    );

    // The granted ability is present with its printed cost and effect.
    let index = ability_index(&runner, degavolver, |e| {
        matches!(e, Effect::Regenerate { .. })
    });
    let granted = &runner.state().objects[&degavolver].abilities[index];
    assert!(
        matches!(
            &granted.cost,
            Some(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 }
            })
        ),
        "the granted regenerate must cost 3 life, got {:?}",
        granted.cost
    );

    // And it activates: P0 pays 3 life, Degavolver gains a regen shield.
    let outcome = runner.activate(degavolver, index).resolve();
    assert_eq!(outcome.life_delta(P0), -3);
    assert!(
        has_regeneration_shield(&runner, degavolver),
        "CR 614.1c: the quoted rider is part of the kicked replacement, so the \
         granted regenerate must shield Degavolver"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "activation must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Opposite polarity: unkicked, so NEITHER half applies — no counters and no
/// granted regenerate. Pairs with the test above so the grant there is shown
/// to be a real gate decision rather than an unconditional grant.
#[test]
fn degavolver_unkicked_gets_neither_counters_nor_grant() {
    let (runner, degavolver) = cast_degavolver(&[false, false]);
    assert_eq!(counters(&runner, degavolver), 0);
    assert!(
        runner.state().objects[&degavolver]
            .replacement_definitions
            .as_slice()
            .len()
            == 2,
        "reach-guard: both kicker lines must parse to replacements, so the \
         missing grant below is a gate decision, not a parse failure"
    );
    assert!(
        !runner.state().objects[&degavolver]
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Regenerate { .. })),
        "the regenerate rider is gated on kicker (CR 702.33d), so an unkicked \
         Degavolver must NOT have it"
    );
}

fn cast_necravolver(decisions: &[bool]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let necravolver = scenario
        .add_creature_to_hand_from_oracle(P0, "Necravolver", 2, 2, NECRAVOLVER)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Black,
            ManaType::White,
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    cast_with_kickers(&mut runner, necravolver, decisions);
    (runner, necravolver)
}

fn attack_unblocked(runner: &mut GameRunner, attacker: ObjectId) {
    // A later turn's combat: the just-resolved creature shakes summoning
    // sickness so the attack declaration tests the grant, not CR 302.6.
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .unwrap()
        .summoning_sick = false;
    runner.advance_to_combat();
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the attacker");
    // Post-declaration priority (CR 508.2) into the declare-blockers step
    // (CR 509.1). Priority is passed ONLY while still in the DeclareAttackers
    // step: with no possible blockers the engine may skip straight past, and
    // passing on would resolve combat damage before `combat_damage` snapshots
    // its life baseline. DeclareBlockers is answered when it appears rather
    // than asserted unconditionally.
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("P1 blocks nothing");
                break;
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::DeclareAttackers => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass post-declaration priority");
            }
            _ => break,
        }
    }
}

/// Second kicker paid: one +1/+1 counter AND the granted damage-lifegain
/// trigger — and the trigger fires on combat damage.
#[test]
fn necravolver_second_kicker_gains_life_on_damage() {
    let (mut runner, necravolver) = cast_necravolver(&[false, true]);
    assert_eq!(
        runner.state().objects[&necravolver].kickers_paid,
        vec![KickerVariant::Second]
    );
    assert_eq!(
        counters(&runner, necravolver),
        1,
        "reach-guard: the kicked replacement must place one +1/+1 counter"
    );
    // Variant isolation: the declined {1}{G} half must not fire.
    assert!(
        !runner.state().objects[&necravolver].has_keyword(&Keyword::Trample),
        "the declined first kicker must not grant trample"
    );
    assert!(
        runner.state().objects[&necravolver]
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|t| t.definition.mode == TriggerMode::DamageDone),
        "CR 614.1c: the quoted rider is part of the kicked replacement, so a \
         kicked Necravolver must carry the damage trigger"
    );

    // 2 base power + 1 counter = 3 combat damage unblocked; the trigger gains 3.
    attack_unblocked(&mut runner, necravolver);
    let outcome = runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P1),
        -3,
        "reach-guard: 3 combat damage must land, so the lifegain below reads a real event amount"
    );
    assert_eq!(
        outcome.life_delta(P0),
        3,
        "the granted \"Whenever this creature deals damage, you gain that much \
         life\" must fire for the 3 damage dealt"
    );
}

/// Opposite polarity: unkicked combat deals damage but gains no life.
#[test]
fn necravolver_unkicked_deals_damage_without_lifegain() {
    let (mut runner, necravolver) = cast_necravolver(&[false, false]);
    assert_eq!(counters(&runner, necravolver), 0);
    assert!(
        !runner.state().objects[&necravolver]
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|t| t.definition.mode == TriggerMode::DamageDone),
        "an unkicked Necravolver must NOT carry the damage trigger"
    );
    attack_unblocked(&mut runner, necravolver);
    let outcome = runner.combat_damage();
    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "reach-guard: base-power combat damage must land, so the zero lifegain \
         below is a missing trigger, not a missing attack"
    );
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "without the granted trigger, combat damage gains no life"
    );
}

fn cast_prison_barricade(kicked: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The Defender line carries reminder text, so the keyword needs its
    // MTGJSON-style hint (the `/card-test` keyword rule).
    let mut barricade_builder = scenario.add_creature_to_hand(P0, "Prison Barricade", 1, 3);
    barricade_builder.from_oracle_text_with_keywords(&["Defender"], PRISON_BARRICADE);
    barricade_builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::White],
    });
    let barricade = barricade_builder.id();
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::White,
            ManaType::White,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    cast_with_kickers(&mut runner, barricade, &[kicked]);
    // A later turn's combat legality: shake summoning sickness so the attack
    // checks below test defender and the grant, not CR 302.6.
    runner
        .state_mut()
        .objects
        .get_mut(&barricade)
        .unwrap()
        .summoning_sick = false;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    (runner, barricade)
}

/// Kicked: one +1/+1 counter AND the quoted can-attack static — the Wall keeps
/// defender but attacks anyway.
#[test]
fn prison_barricade_kicked_can_attack_despite_defender() {
    let (runner, barricade) = cast_prison_barricade(true);
    assert_eq!(
        counters(&runner, barricade),
        1,
        "reach-guard: the kicked replacement must place one +1/+1 counter"
    );
    let obj = &runner.state().objects[&barricade];
    assert!(
        has_keyword(obj, &Keyword::Defender),
        "reach-guard: the Barricade still HAS defender — the grant permits \
         attacking \"as though\", it does not remove the keyword"
    );
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&barricade),
        "CR 614.1c: the quoted rider is part of the kicked replacement, so a \
         kicked Prison Barricade must be a legal attacker: {valid:?}"
    );
    assert!(
        validate_attackers(runner.state(), &[barricade]).is_ok(),
        "declaring the kicked Barricade as an attacker must be legal"
    );
}

/// Opposite polarity: unkicked, the defender restriction holds.
#[test]
fn prison_barricade_unkicked_cannot_attack() {
    let (runner, barricade) = cast_prison_barricade(false);
    assert_eq!(counters(&runner, barricade), 0);
    let obj = &runner.state().objects[&barricade];
    assert!(
        has_keyword(obj, &Keyword::Defender),
        "reach-guard: the unkicked Barricade has defender, so its exclusion \
         below is FOR defender, not a missing keyword"
    );
    let valid = get_valid_attacker_ids(runner.state());
    assert!(
        !valid.contains(&barricade),
        "CR 702.3: an unkicked Prison Barricade has defender and no grant, so \
         it must be excluded from valid attackers: {valid:?}"
    );
    assert!(
        validate_attackers(runner.state(), &[barricade]).is_err(),
        "declaring the unkicked Barricade as an attacker must be illegal"
    );
}
