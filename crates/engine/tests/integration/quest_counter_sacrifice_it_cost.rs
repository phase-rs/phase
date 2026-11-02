//! CR 201.5 + CR 113.7 + CR 701.21a + CR 601.2h + CR 602.2b: Ior Ruin
//! Expedition — the cost's bare "it" names the ability's own source, so the
//! sacrifice leg is paid with the enchantment itself and no other permanent is
//! ever offered.
//!
//! The class this pins is named by predicate, not by count: printed cost legs
//! whose object phrase is the bare pronoun `it` (`\b(sacrifice|exile)\s+it\b`
//! in the pre-colon cost clause). At base the sacrifice leg parses as an
//! unrestricted `Typed{}` filter, `find_eligible_sacrifice_targets` returns
//! EVERY permanent the payer controls, and `AbilityActivation::try_resolve`
//! panics on the surfaced `WaitingFor::PayCost { kind: Sacrifice, .. }` — that
//! prompt is the defect this module pins.

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle text (CMR).
const IOR_RUIN: &str = "Landfall — Whenever a land you control enters, you may put a quest counter on this enchantment.\nRemove three quest counters from this enchantment and sacrifice it: Draw two cards.";

fn quest() -> CounterType {
    CounterType::Generic("quest".to_string())
}

/// Stage the enchantment with `counters` quest counters plus one unrelated
/// permanent the defect would let the payer sacrifice instead.
fn stage(counters: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Island", "Island", "Island"]);
    let ruin = scenario
        .add_enchantment_from_oracle(P0, "Ior Ruin Expedition", IOR_RUIN)
        .id();
    scenario.with_counter(ruin, quest(), counters);
    let bystander = scenario.add_vanilla(P0, 2, 2);
    (scenario.build(), ruin, bystander)
}

/// The printed ability index of "… and sacrifice it: Draw two cards." (the
/// Landfall triggered ability also lives in the printed list).
fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    activated_ability_definitions(runner.state(), source)
        .into_iter()
        .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::Draw { .. }))
        .map(|(index, _)| index)
        .expect("Ior Ruin Expedition must carry a Draw activated ability")
}

/// The discriminating claim: activation never asks WHICH permanent to sacrifice.
#[test]
fn ior_ruin_expedition_sacrifice_cost_never_offers_another_permanent() {
    let (mut runner, ruin, bystander) = stage(3);
    let index = activated_index(&runner, ruin);
    let submitted = runner.act(GameAction::ActivateAbility {
        source_id: ruin,
        ability_index: index,
        payment_mode: Default::default(),
    });
    // Reach-guard: the activation was accepted (the instrument fires at all).
    assert!(
        submitted.is_ok(),
        "activation must be accepted: {submitted:?}"
    );
    // CR 601.2h: the source pays its own sacrifice cost — no choice is owed.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "a self-sacrifice cost must not surface a PayCost prompt: {:?}",
        runner.state().waiting_for
    );
    // CR 701.21a: the cost moved the SOURCE to its owner's graveyard...
    assert_eq!(runner.state().objects[&ruin].zone, Zone::Graveyard);
    // ...and the unrelated permanent was never the payer.
    assert_eq!(runner.state().objects[&bystander].zone, Zone::Battlefield);
}

/// Completion boundary: the ability resolves independently of the sacrificed source.
#[test]
fn ior_ruin_expedition_sacrifices_itself_and_draws_two() {
    let (mut runner, ruin, bystander) = stage(3);
    let index = activated_index(&runner, ruin);
    let outcome = runner.activate(ruin, index).resolve();
    outcome.assert_zone(&[ruin], Zone::Graveyard); // CR 701.21a
    outcome.assert_counters(ruin, quest(), 0); // CR 122.2
    outcome.assert_zone(&[bystander], Zone::Battlefield); // the negative's positive twin
    outcome.assert_hand_drawn(P0, 2); // CR 113.7a + the effect
    outcome.assert_stack_size(0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 118.3: the counter leg keeps reading the SOURCE's counters (the sibling
/// that must not regress) — 2 counters is unpayable, 3 is payable. Both halves
/// submit the raw `ActivateAbility`, which reports rejection instead of panicking.
#[test]
fn ior_ruin_expedition_counter_cost_still_binds_the_source() {
    let (mut short, short_ruin, _) = stage(2);
    let index = activated_index(&short, short_ruin);
    let rejected = short.act(GameAction::ActivateAbility {
        source_id: short_ruin,
        ability_index: index,
        payment_mode: Default::default(),
    });
    assert!(
        rejected.is_err(),
        "two quest counters cannot pay a three-counter cost: {rejected:?}"
    );

    let (mut full, ruin, _) = stage(3);
    let index = activated_index(&full, ruin);
    let accepted = full.act(GameAction::ActivateAbility {
        source_id: ruin,
        ability_index: index,
        payment_mode: Default::default(),
    });
    assert!(
        accepted.is_ok(),
        "three quest counters pay the counter leg: {accepted:?}"
    );
}
