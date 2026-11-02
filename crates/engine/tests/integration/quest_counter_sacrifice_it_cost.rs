//! CR 701.21a + CR 601.2h + CR 602.2b + CR 113.7a: Ior Ruin Expedition — the
//! cost clause names the enchantment and then says "it", so the sacrifice leg
//! is paid with the enchantment itself and no other permanent is ever offered.
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
use engine::types::ability::{AbilityCost, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle text (CMR).
const IOR_RUIN: &str = "Landfall — Whenever a land you control enters, you may put a quest counter on this enchantment.\nRemove three quest counters from this enchantment and sacrifice it: Draw two cards.";

fn quest() -> CounterType {
    parse_counter_type("quest")
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

/// Verbatim Scryfall Oracle text (WHO).
const TRENZALORE: &str = "{T}: Add {U}. Put a time counter on Trenzalore Clocktower.\n{1}{U}, {T}, Remove twelve time counters from Trenzalore Clocktower and exile it: Shuffle your graveyard and hand into your library, then draw seven cards. Activate only if you control a Time Lord.";

/// The printed ability whose cost exiles the source (the artifact also has a
/// mana ability at index 0).
fn exile_cost_index(runner: &GameRunner, source: ObjectId) -> usize {
    fn has_self_exile(cost: Option<&AbilityCost>) -> bool {
        match cost {
            Some(AbilityCost::Exile { .. }) => true,
            Some(AbilityCost::Composite { costs }) => {
                costs.iter().any(|c| matches!(c, AbilityCost::Exile { .. }))
            }
            _ => false,
        }
    }
    activated_ability_definitions(runner.state(), source)
        .into_iter()
        .find(|(_, ability)| has_self_exile(ability.cost.as_ref()))
        .map(|(index, _)| index)
        .expect("Trenzalore Clocktower must carry an exile-cost activated ability")
}

/// The class's second printed verb at runtime: "… and exile it" is a
/// self-exile cost, not a lost cost. At base the leg lowers to
/// `EffectCost { ChangeZone { target: ParentTarget } }`, which
/// `supports_effect_cost_payment` refuses, so the activation is rejected
/// outright — the reach-guard below is the discriminator.
#[test]
fn activating_trenzalore_clocktower_exiles_itself_not_another_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let tower = scenario
        .add_artifact_from_oracle(P0, "Trenzalore Clocktower", TRENZALORE)
        .id();
    scenario.with_counter(tower, parse_counter_type("time"), 12);
    // The activation restriction needs a Time Lord; a synthetic one keeps the
    // card under test verbatim and the battlefield otherwise inert.
    let witness = scenario
        .add_creature(P0, "Time Lord Witness", 2, 2)
        .with_subtypes(vec!["Time Lord"])
        .id();
    let mut runner = scenario.build();
    let index = exile_cost_index(&runner, tower);

    let submitted = runner.act(GameAction::ActivateAbility {
        source_id: tower,
        ability_index: index,
        payment_mode: Default::default(),
    });
    // Reach-guard: the printed cost is payable at all (base rejects it).
    assert!(
        submitted.is_ok(),
        "the self-exile cost must be payable: {submitted:?}"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }),
        "a self-exile cost must not surface a PayCost prompt: {:?}",
        runner.state().waiting_for
    );
    // CR 701.13a + CR 601.2h: the cost exiled the SOURCE, and only it.
    assert_eq!(runner.state().objects[&tower].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&witness].zone, Zone::Battlefield);
}
