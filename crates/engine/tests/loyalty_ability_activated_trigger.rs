//! Runtime coverage for the "Whenever you activate a loyalty ability of
//! [planeswalker]" trigger class (CR 606.2) — the GAP proofs from the
//! reviewed implementation plan.
//!
//! The discriminator design adds `ActivatedAbilityKind::{Normal,Loyalty}` to the
//! single `GameEvent::AbilityActivated` event (no new event), and a
//! `TriggerMode::LoyaltyAbilityActivated` matcher gated on `kind == Loyalty`.
//! The seam most likely to silently no-fire is the **targeted** loyalty
//! activation finalize (`game/casting_targets.rs`), where the kind is derived
//! from `pending.activation_cost`. These tests drive a *targeted* minus-loyalty
//! ability (two legal creature targets force the `TargetSelection` window and so
//! route through that finalize seam) and assert the trigger fires there.
//!
//! CARD TEXT (verified against MTGJSON AtomicCards.json):
//!   * Keral Keep Disciples — "Whenever you activate a loyalty ability of a
//!     Chandra planeswalker, this creature deals 1 damage to each opponent."
//!   * Elspeth's Talent — "... of enchanted planeswalker, creatures you control
//!     get +2/+2 and gain vigilance until end of turn."
//!
//! The loyalty ability itself is a generic targeted minus-loyalty ability built
//! from shipped building blocks (CR 606.1 loyalty cost + `Effect::DealDamage` to
//! `target creature`); the cards under test only need *some* loyalty ability of
//! the right planeswalker to be activated through the targeted finalize seam.

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TargetRef,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const KERAL_ORACLE: &str =
    "Whenever you activate a loyalty ability of a Chandra planeswalker, this creature deals 1 damage to each opponent.";
const ELSPETH_TALENT_ORACLE: &str =
    "Whenever you activate a loyalty ability of enchanted planeswalker, creatures you control get +2/+2 and gain vigilance until end of turn.";

/// A targeted minus-loyalty ability: CR 606.1 loyalty cost (`[−2]`, an
/// `AbilityCost::Loyalty`) + `Effect::DealDamage 2` to `target creature`. Two
/// legal creatures force a `TargetSelection` window, routing activation through
/// the targeted finalize seam (`casting_targets.rs`).
fn targeted_minus_loyalty_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            damage_source: None,
        },
    )
    .cost(AbilityCost::Loyalty { amount: -2 })
    .sorcery_speed()
}

/// A non-loyalty targeted activated ability (no loyalty cost) dealing 2 damage
/// to target creature — produces `AbilityActivated { kind: Normal }`.
fn targeted_normal_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            damage_source: None,
        },
    )
}

/// Imperatively place a planeswalker with the given subtype and loyalty under
/// `owner`, carrying `ability` at index 0. Returns its `ObjectId`.
fn place_planeswalker(
    runner: &mut GameRunner,
    owner: PlayerId,
    name: &str,
    subtype: &str,
    loyalty: u32,
    ability: AbilityDefinition,
) -> ObjectId {
    let state = runner.state_mut();
    let id = ObjectId(state.next_object_id);
    create_object(
        state,
        CardId(id.0),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Planeswalker);
    obj.card_types.subtypes.push(subtype.to_string());
    obj.base_card_types = obj.card_types.clone();
    // CR 306.5b: loyalty IS the loyalty-counter count; seed both in sync.
    obj.loyalty = Some(loyalty);
    obj.counters.insert(CounterType::Loyalty, loyalty);
    obj.abilities = Arc::new(vec![ability.clone()]);
    obj.base_abilities = Arc::new(vec![ability]);
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
    obj.summoning_sick = false;
    id
}

/// Build a scenario at PreCombatMain with P0 active, two P0 creatures (so a
/// `target creature` loyalty ability surfaces a `TargetSelection` window), and a
/// trigger-source permanent parsed from `oracle` via the real parser.
fn scenario_with_trigger_source(
    source_name: &str,
    oracle: &str,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let _c2 = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let source = scenario
        .add_creature(P0, source_name, 2, 2)
        .from_oracle_text(oracle)
        .id();
    (scenario.build(), c1, source)
}

/// Activate `pw`'s loyalty ability at index 0, announcing the `[−X]` counter
/// removal (X = 2) and answering the `target creature` slot with `target`,
/// halting at the post-commit `Priority` window so the caller can inspect the
/// stack (where any fired trigger now sits).
fn activate_and_target(runner: &mut GameRunner, pw: ObjectId, target: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: pw,
            ability_index: 0,
        })
        .expect("activation announced");

    // CR 602.2b / CR 601.2c: answer the announcement-through-target windows in
    // order — `ChooseXValue` (only for the `[−X]` loyalty cost), then the
    // `target creature` slot — stopping once the ability commits to the stack.
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: 2 })
                    .expect("announce loyalty [-X]");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose ability target");
            }
            _ => break,
        }
    }
}

/// GAP-1 PROOF: a TARGETED loyalty ability of a Chandra planeswalker, activated
/// through the `TargetSelection` finalize seam, fires Keral Keep Disciples'
/// `LoyaltyAbilityActivated` trigger. Asserted by inspecting the stack right
/// after the activation commits (the trigger's source is on the stack above the
/// loyalty ability). If the kind at the targeted finalize seam were hardcoded
/// `Normal`, the matcher would reject the event and the trigger source would NOT
/// appear on the stack — the revert assertion this test guards.
#[test]
fn gap1_targeted_loyalty_fires_keral_keep_trigger() {
    let (mut runner, c1, _keral) =
        scenario_with_trigger_source("Keral Keep Disciples", KERAL_ORACLE);
    let chandra = place_planeswalker(
        &mut runner,
        P0,
        "Chandra, Acolyte of Flame",
        "Chandra",
        5,
        targeted_minus_loyalty_ability(),
    );

    activate_and_target(&mut runner, chandra, c1);

    let names = runner.stack_names();
    assert!(
        names.iter().any(|n| n == "Keral Keep Disciples"),
        "expected Keral Keep Disciples' loyalty trigger on the stack, got {names:?}"
    );
}

/// A NON-Chandra planeswalker's loyalty activation does NOT fire the
/// "a Chandra planeswalker" trigger.
#[test]
fn non_chandra_loyalty_does_not_fire() {
    let (mut runner, c1, _keral) =
        scenario_with_trigger_source("Keral Keep Disciples", KERAL_ORACLE);
    let jace = place_planeswalker(
        &mut runner,
        P0,
        "Jace, the Mind Sculptor",
        "Jace",
        5,
        targeted_minus_loyalty_ability(),
    );

    activate_and_target(&mut runner, jace, c1);

    let names = runner.stack_names();
    assert!(
        !names.iter().any(|n| n == "Keral Keep Disciples"),
        "non-Chandra loyalty activation must NOT fire the trigger, got {names:?}"
    );
}

/// A NON-loyalty activated ability (kind == Normal) does NOT fire the loyalty
/// trigger, even on a Chandra planeswalker.
#[test]
fn non_loyalty_ability_does_not_fire() {
    let (mut runner, c1, _keral) =
        scenario_with_trigger_source("Keral Keep Disciples", KERAL_ORACLE);
    let chandra = place_planeswalker(
        &mut runner,
        P0,
        "Chandra, Acolyte of Flame",
        "Chandra",
        5,
        targeted_normal_ability(),
    );

    activate_and_target(&mut runner, chandra, c1);

    let names = runner.stack_names();
    assert!(
        !names.iter().any(|n| n == "Keral Keep Disciples"),
        "a Normal-kind activated ability must NOT fire the loyalty trigger, got {names:?}"
    );
}

/// Enchanted-host (Elspeth's / Rowan's Talent class): the loyalty ability of the
/// ENCHANTED planeswalker fires; a different planeswalker does not. The Talent's
/// `valid_card == AttachedTo` resolves against the aura's host.
#[test]
fn enchanted_host_loyalty_fires_other_does_not() {
    let (mut runner, c1, talent) =
        scenario_with_trigger_source("Elspeth's Talent", ELSPETH_TALENT_ORACLE);

    let host = place_planeswalker(
        &mut runner,
        P0,
        "Host Walker",
        "Elspeth",
        5,
        targeted_minus_loyalty_ability(),
    );
    let other = place_planeswalker(
        &mut runner,
        P0,
        "Other Walker",
        "Jace",
        5,
        targeted_minus_loyalty_ability(),
    );

    // Make the Talent an Aura attached to the host planeswalker.
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&talent).unwrap();
        obj.card_types.subtypes.push("Aura".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.attached_to = Some(AttachTarget::Object(host));
    }

    // Activating the host's loyalty ability fires the Talent trigger.
    activate_and_target(&mut runner, host, c1);
    assert!(
        runner.stack_names().iter().any(|n| n == "Elspeth's Talent"),
        "enchanted-host loyalty activation should fire the Talent trigger, got {:?}",
        runner.stack_names()
    );

    // Drain the stack so the next activation starts from a clean Priority window.
    for _ in 0..32 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // Activating the OTHER (non-host) planeswalker's loyalty ability does not.
    activate_and_target(&mut runner, other, c1);
    let after = runner.stack_names();
    assert!(
        !after.iter().any(|n| n == "Elspeth's Talent"),
        "non-host loyalty activation must NOT fire the Talent trigger, got {after:?}"
    );
}
