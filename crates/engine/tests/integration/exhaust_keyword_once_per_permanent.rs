//! Runtime coverage for the Exhaust keyword (CR 702.177).
//!
//! CR 702.177a: "Exhaust — [Cost]: [Effect]" means "[Cost]: [Effect]. Activate
//! only once." An exhaust ability is a special kind of activated ability whose
//! activation restriction is per-permanent for the whole game (CR 602.5b), not
//! per turn — once a permanent's exhaust ability has been activated, it can
//! never be activated again while that permanent remains on the battlefield.
//!
//! These tests drive the REAL activation pipeline (`GameAction::ActivateAbility`
//! through `apply()` via the `GameScenario`/`GameRunner` harness), not the parsed
//! AST. Each test:
//!   1. activates the exhaust ability once and asserts its effect actually
//!      resolved against game state (the positive case), then
//!   2. asserts a SECOND `ActivateAbility` on the same permanent is rejected by
//!      the engine (the can't-activate-twice discrimination).
//!
//! WHY THE NEGATIVE ASSERTION DISCRIMINATES: the second `ActivateAbility` action
//! reaches `casting::handle_activate_ability`, which calls
//! `restrictions::check_activation_restrictions` against the ability's
//! `[ActivationRestriction::OnlyOnce]`. That returns `Err` because
//! `record_ability_activation` (run during the first activation's production
//! path) left `activated_abilities_this_game[(source, idx)] == 1`. Remove the
//! `ActivationRestriction::OnlyOnce` push from the Exhaust parser branch (or the
//! `OnlyOnce` arm from `restrictions.rs`) and the second activation is accepted
//! instead of erroring — every `act(..).is_err()` assertion below flips.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Add `n` units of `ty` mana to P0's pool (deterministic payment without
/// modelling lands — the activation driver finalizes the pool via PassPriority).
fn add_mana(runner: &mut GameRunner, player: PlayerId, ty: ManaType, n: usize) {
    for _ in 0..n {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[usize::from(player.0)]
            .mana_pool
            .add(unit);
    }
}

/// Locate the runtime index of the permanent's Exhaust-tagged activated ability
/// (CR 702.177a `AbilityTag::Exhaust`). The Exhaust line is not always at index
/// 0 — a card may have an earlier static/other ability — so resolve it by tag
/// rather than hardcoding an index.
fn exhaust_ability_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| a.ability_tag == Some(AbilityTag::Exhaust))
        .expect("permanent must carry an Exhaust-tagged activated ability")
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Elvish Refueler's clean exhaust body: "Exhaust — {1}{G}: Put a +1/+1 counter
/// on this creature." (The card's first line — CR 702.177b "activate exhaust
/// abilities as though they haven't been activated" — is a separate, currently
/// unimplemented permission and is intentionally omitted here; this test
/// exercises the keyword, not that line.)
const ELVISH_REFUELER_EXHAUST: &str =
    "Exhaust — {1}{G}: Put a +1/+1 counter on this creature. (Activate each exhaust ability only once.)";

#[test]
fn exhaust_ability_resolves_its_effect_on_first_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_from_oracle(P0, "Elvish Refueler", 2, 2, ELVISH_REFUELER_EXHAUST)
        .id();
    let mut runner = scenario.build();

    assert_eq!(
        plus_one_counters(&runner, id),
        0,
        "precondition: no +1/+1 counter before the exhaust ability resolves"
    );

    let idx = exhaust_ability_index(&runner, id);
    add_mana(&mut runner, P0, ManaType::Green, 2); // {1}{G} -> one funds {G}, one funds the {1}
    runner.activate(id, idx).resolve();

    // CR 702.177a: the exhaust effect ("put a +1/+1 counter on this creature")
    // resolves on the (only) activation.
    assert_eq!(
        plus_one_counters(&runner, id),
        1,
        "exhaust ability's PutCounter effect must place exactly one +1/+1 counter"
    );
}

#[test]
fn exhaust_ability_cannot_be_activated_a_second_time() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_from_oracle(P0, "Elvish Refueler", 2, 2, ELVISH_REFUELER_EXHAUST)
        .id();
    let mut runner = scenario.build();

    let idx = exhaust_ability_index(&runner, id);

    // First activation: legal, resolves, records the per-game activation.
    add_mana(&mut runner, P0, ManaType::Green, 2);
    runner.activate(id, idx).resolve();
    assert_eq!(
        plus_one_counters(&runner, id),
        1,
        "first activation must resolve (one +1/+1 counter)"
    );
    runner.advance_until_stack_empty();

    // Fund the pool again so the rejection is attributable to the OnlyOnce
    // restriction, NOT to an unpayable cost.
    add_mana(&mut runner, P0, ManaType::Green, 2);

    // CR 702.177a + CR 602.5b: the SECOND activation must be rejected by the
    // engine. This is the discriminating assertion — it flips to Ok(_) if the
    // OnlyOnce restriction is dropped from the Exhaust ability.
    let second = runner.act(GameAction::ActivateAbility {
        source_id: id,
        ability_index: idx,
    });
    assert!(
        second.is_err(),
        "CR 702.177a: a permanent's exhaust ability can be activated only once \
         per game — the second ActivateAbility must be rejected, got {second:?}"
    );

    // And state must be unchanged by the rejected attempt: still one counter,
    // no new copy of the ability on the stack.
    assert_eq!(
        plus_one_counters(&runner, id),
        1,
        "rejected second activation must not add another +1/+1 counter"
    );
    assert!(
        runner.state().stack.is_empty(),
        "rejected second activation must not put the exhaust ability on the stack"
    );
}

/// Sita Varma, Masked Racer's exhaust ability has an X cost and an X-scaled
/// PutCounter. Only the PutCounter half parses to a real effect today (the
/// "have the base power and toughness of each other creature you control become
/// equal to ~'s power" half is an unimplemented gap); this test exercises the
/// keyword's once-per-permanent restriction on an X-cost exhaust ability and the
/// X-scaled counter placement that already works.
const SITA_VARMA_EXHAUST: &str = "Exhaust — {X}{G}{G}{U}: Put X +1/+1 counters on Sita Varma. \
     Then you may have the base power and toughness of each other creature you control become \
     equal to Sita Varma's power until end of turn. (Activate each exhaust ability only once.)";

#[test]
fn x_cost_exhaust_ability_is_also_once_per_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_from_oracle(P0, "Sita Varma, Masked Racer", 3, 3, SITA_VARMA_EXHAUST)
        .id();
    let mut runner = scenario.build();

    let idx = exhaust_ability_index(&runner, id);

    // {X}{G}{G}{U} with X=2 -> 2 generic (X) + GG + U. Fund generously: 2 green
    // cover {G}{G}, 1 blue covers {U}, 2 colorless cover X=2.
    add_mana(&mut runner, P0, ManaType::Green, 2);
    add_mana(&mut runner, P0, ManaType::Blue, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 2);
    runner.activate(id, idx).x(2).resolve();

    // CR 702.177a: the X-scaled PutCounter half resolves (X=2 counters).
    assert_eq!(
        plus_one_counters(&runner, id),
        2,
        "X-cost exhaust ability must place X=2 +1/+1 counters"
    );
    runner.advance_until_stack_empty();

    // Second activation rejected even though the cost would be payable.
    add_mana(&mut runner, P0, ManaType::Green, 2);
    add_mana(&mut runner, P0, ManaType::Blue, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 2);
    let second = runner.act(GameAction::ActivateAbility {
        source_id: id,
        ability_index: idx,
    });
    assert!(
        second.is_err(),
        "CR 702.177a: an X-cost exhaust ability is still once-per-permanent — \
         the second ActivateAbility must be rejected, got {second:?}"
    );
}
