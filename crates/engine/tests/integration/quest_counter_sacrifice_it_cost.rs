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
//!
//! In a sacrifice or exile leg the pronoun binds the source only on the
//! source-cost channel (the cost the source's own text imposes). A
//! resolution-time optional payment such as Gorbag of Minas Morgul's "you may
//! sacrifice it. When you do, choose one —" names an object the trigger
//! introduced, so it is pinned below as never bound to the printed source.

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, Effect, SacrificeCost, TargetFilter,
};
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

/// The printed ability whose cost exiles the source (the land also has a mana
/// ability at index 0).
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
/// outright. At base this test fails earlier, in `exile_cost_index`, because
/// base lowers the cost to an `EffectCost` rather than an `Exile` leg.
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
        .add_land_from_oracle(P0, "Trenzalore Clocktower", TRENZALORE)
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

/// Verbatim Scryfall Oracle text (LTR).
const GORBAG: &str = "Whenever a Goblin or Orc you control deals combat damage to a player, you may sacrifice it. When you do, choose one \u{2014}\n\u{2022} Draw a card.\n\u{2022} Create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";

/// True when `cost` (recursing `Composite` / `OneOf`) pays by sacrificing or
/// exiling the source itself.
fn cost_binds_source(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Sacrifice(SacrificeCost {
            target: TargetFilter::SelfRef,
            ..
        }) => true,
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            ..
        } => true,
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().any(cost_binds_source)
        }
        _ => false,
    }
}

/// Collect every node of `ability` (own effect, `sub_ability`, `else_ability`,
/// modal `mode_abilities`) whose effect binds the printed source.
fn source_bound_nodes(ability: &AbilityDefinition, found: &mut Vec<String>) {
    match ability.effect.as_ref() {
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            ..
        } => found.push(format!("{:?}", ability.effect)),
        Effect::PayCost { cost, .. } if cost_binds_source(cost) => {
            found.push(format!("{:?}", ability.effect));
        }
        _ => {}
    }
    for child in ability
        .sub_ability
        .iter()
        .chain(ability.else_ability.iter())
    {
        source_bound_nodes(child, found);
    }
    for mode in &ability.mode_abilities {
        source_bound_nodes(mode, found);
    }
}

/// CR 608.2c + CR 603.12 + CR 701.21a: Gorbag of Minas Morgul's "you may
/// sacrifice it. When you do, choose one —" is a resolution-time optional
/// payment, not a printed source cost. The cost grammar cannot know the
/// antecedent of "it" on that channel: it names the object the trigger
/// introduced — whichever Goblin or Orc dealt the damage, Gorbag himself or
/// another — so the parse must never bind it to the printed source.
///
/// Before the pronoun arm was bound to the source-cost channel (the reflexive
/// `MayPay` caller parsing its cost like a printed source cost), the reflexive
/// marker was lowered to `PayCost { Sacrifice { SelfRef } }`, which is exactly
/// what the walker flags.
#[test]
fn gorbag_of_minas_morgul_may_sacrifice_it_never_binds_the_source() {
    let mut scenario = GameScenario::new();
    let gorbag = scenario
        .add_creature_from_oracle(P0, "Gorbag of Minas Morgul", 2, 2, GORBAG)
        .id();
    let runner = scenario.build();
    let object = &runner.state().objects[&gorbag];

    // Reach-guards: the production parser produced the optional sacrifice, the
    // reflexive "When you do" body, and the two-mode choice.
    assert_eq!(
        object.trigger_definitions.len(),
        1,
        "Gorbag must parse exactly one trigger"
    );
    let trigger = object
        .trigger_definitions
        .first()
        .expect("Gorbag trigger exists");
    let execute = trigger
        .definition
        .execute
        .as_deref()
        .expect("Gorbag trigger must carry an execute ability");
    assert!(
        matches!(execute.effect.as_ref(), Effect::Sacrifice { .. }) && execute.optional,
        "the trigger must open with the optional sacrifice: {execute:?}"
    );
    let reflexive = execute
        .sub_ability
        .as_deref()
        .expect("the optional sacrifice must carry its reflexive sub_ability");
    assert_eq!(
        reflexive.condition,
        Some(AbilityCondition::WhenYouDo),
        "the reflexive body must be gated on WhenYouDo"
    );
    assert_eq!(
        reflexive.modal.as_ref().map(|modal| modal.mode_count),
        Some(2),
        "the reflexive body must be the two-mode choice: {reflexive:?}"
    );

    // Discriminator: no node of the parse sacrifices/exiles the printed source.
    let mut bound = Vec::new();
    source_bound_nodes(execute, &mut bound);
    assert!(
        bound.is_empty(),
        "the resolution-time pronoun must not be bound to the printed source: {bound:?}"
    );
}
