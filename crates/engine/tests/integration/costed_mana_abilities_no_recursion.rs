//! Two costed mana abilities (Signets) must not infinitely recurse when paying
//! one would auto-tap the other.
//!
//! Regression for the auto-tap mutual-recursion stack overflow: with two
//! Signets — each `{1}, {T}: Add <two colors>` — controlled by one player and
//! an empty pool, activating one Signet auto-taps the other to pay its `{1}`,
//! and paying that Signet's `{1}` auto-tapped the first right back. Pre-fix
//! this recursed forever and SIGABRTed the process, because every
//! `pay_mana_sub_cost` rebuilt the auto-tap exclusion set as `{source_id}`,
//! discarding the in-flight ancestor chain.
//!
//! The fix threads the in-flight exclusion chain (every ancestor mana-ability
//! activation suspended mid-payment on the call stack) through the nested
//! auto-tap, so the chain terminates: the degenerate, externally-unfundable
//! pair is rejected gracefully instead of overflowing, and a single Signet's
//! payment is never starved by an over-broad exclusion.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 605.3c: "Once a player begins to activate a mana ability, that
//!     ability can't be activated again until it has resolved." Each ancestor
//!     activation suspended mid-payment must stay excluded from the nested
//!     auto-tap — that is what makes the chain terminate.
//!   - CR 605.1b: non-land permanents (Signets) can have mana abilities.
//!   - CR 601.2g–h: a mana ability's mana sub-cost auto-taps the controller's
//!     mana sources.
//!
//! This is a *class* regression: the fix applies to any pair or chain of costed
//! mana abilities (Signets, filter lands, Pentad Prism). The two named Signets
//! are representative; the assertions are about the general nested-sub-cost
//! auto-tap mechanism, driven through the real activation pipeline
//! (`GameAction::ActivateAbility` → `apply()`).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DIMIR_SIGNET_ORACLE: &str = "{1}, {T}: Add {U}{B}.";
const GRUUL_SIGNET_ORACLE: &str = "{1}, {T}: Add {R}{G}.";

/// Signets are artifacts, not creatures. The scenario creature helper parses
/// Oracle text onto a battlefield permanent; convert it to a pure artifact and
/// clear P/T so the 0/0 stub is not destroyed as an SBA (CR 704.5f) before its
/// mana ability is activated.
fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Artifact];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
}

/// Index of the (single) mana ability on a Signet.
fn mana_ability_index(state: &engine::types::game_state::GameState, id: ObjectId) -> usize {
    let obj = state.objects.get(&id).expect("signet exists");
    obj.abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("signet has a costed mana ability")
}

#[test]
fn two_cross_paying_signets_terminate_without_recursion() {
    // Two Signets, empty pool, NO other mana source. Activating one forces the
    // engine to auto-tap the other to pay the `{1}`, whose own `{1}` would
    // auto-tap the first — the mutual recursion.
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let dimir = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();
    let gruul = scenario
        .add_creature_from_oracle(P0, "Gruul Signet", 0, 0, GRUUL_SIGNET_ORACLE)
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, dimir);
    make_artifact(&mut runner, gruul);
    let idx = mana_ability_index(runner.state(), gruul);

    // CR 605.3c: PRIMARY discriminating assertion. Pre-fix this call SIGABRTs
    // the nextest process via stack overflow (the two Signets re-tap each other
    // forever). Post-fix the in-flight exclusion chain terminates the recursion,
    // so the call returns. Because neither Signet can fund the other's `{1}`
    // once both are on the in-flight chain (CR 605.3c bars re-activating a
    // mana ability already being activated), the pair is correctly unfundable
    // and the activation is rejected gracefully rather than producing mana.
    let result = runner.act(GameAction::ActivateAbility {
        source_id: gruul,
        ability_index: idx,
    });
    assert!(
        result.is_err(),
        "the externally-unfundable Signet pair must be rejected, not partially activated"
    );

    // No partial state: neither Signet was left tapped and no mana was floated
    // (a re-entered chain would have tapped one or both before unwinding).
    assert!(
        !runner.state().objects.get(&dimir).unwrap().tapped,
        "Dimir Signet must not be tapped after a rejected activation"
    );
    assert!(
        !runner.state().objects.get(&gruul).unwrap().tapped,
        "Gruul Signet must not be tapped after a rejected activation"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "no mana floated by a rejected, recursion-free activation"
    );
}

#[test]
fn signet_activates_normally_with_a_sibling_costed_rock_present() {
    // A second costed mana rock is present, but a bootstrap land funds the
    // activated Signet's `{1}` — the auto-tap prefers the land over cross-paying
    // the sibling Signet. Guards against the exclusion set over-excluding and
    // starving a legitimate single activation when another costed rock exists.
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let dimir = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();
    let gruul = scenario
        .add_creature_from_oracle(P0, "Gruul Signet", 0, 0, GRUUL_SIGNET_ORACLE)
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, dimir);
    make_artifact(&mut runner, gruul);
    let idx = mana_ability_index(runner.state(), gruul);

    runner
        .act(GameAction::ActivateAbility {
            source_id: gruul,
            ability_index: idx,
        })
        .expect("Gruul Signet activates normally with a bootstrap land available");

    // Gruul tapped and produced {R}{G}; its `{1}` was paid by the land, so the
    // sibling Dimir Signet was never cross-tapped.
    assert!(
        runner.state().objects.get(&gruul).unwrap().tapped,
        "Gruul Signet activated and tapped"
    );
    assert!(
        !runner.state().objects.get(&dimir).unwrap().tapped,
        "the sibling Dimir Signet was not cross-tapped (the land funded the {{1}})"
    );
    // {R}{G} produced = 2 mana in the pool.
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        2,
        "Gruul Signet produced exactly {{R}}{{G}} — exclusion did not starve the payment"
    );
}

#[test]
fn single_signet_activates_normally() {
    // Baseline negative: a lone Signet with a bootstrap land activates normally.
    // Confirms the exclusion-chain threading does not regress the common case.
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::White);

    let dimir = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, dimir);
    let idx = mana_ability_index(runner.state(), dimir);

    runner
        .act(GameAction::ActivateAbility {
            source_id: dimir,
            ability_index: idx,
        })
        .expect("lone Signet activates normally");

    assert!(
        runner.state().objects.get(&dimir).unwrap().tapped,
        "Dimir Signet activated and tapped"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        2,
        "Dimir Signet produced exactly {{U}}{{B}}"
    );
}

#[test]
fn both_signets_and_lands_fund_a_spell_no_over_exclusion() {
    // POSITIVE multi-source-funded assertion locking in the "no over-exclusion"
    // guarantee. A normal board — two untapped Signets plus four basic Plains —
    // casts a `{U}{B}` sorcery through the real cast pipeline. Plains produce only
    // {W}, so the `{U}{B}` requirement can only be satisfied by the Dimir Signet,
    // whose own `{1}` is auto-tapped from a Plains during cost payment. The Gruul
    // Signet ({R}{G}) is irrelevant and must be left alone.
    //
    // CR 605.3c: the in-flight exclusion chain that terminates the pathological
    // Signet-funds-Signet recursion excludes ONLY ancestor mana-ability
    // activations, never lands. So when lands are present to fund a Signet's
    // sub-cost, a legitimate land-funded payment is never starved by
    // over-exclusion — the cast resolves and the cost is genuinely paid. This is
    // the permanent guard against the exclusion chain becoming too broad.
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Four untapped basic Plains: comfortably fund the activated Dimir Signet's
    // {1} sub-cost without any Signet-funds-Signet cross-payment.
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::White);
    }

    let dimir = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();
    let gruul = scenario
        .add_creature_from_oracle(P0, "Gruul Signet", 0, 0, GRUUL_SIGNET_ORACLE)
        .id();

    // A vanilla `{U}{B}` sorcery: the Plains cannot supply {U} or {B}, so the
    // Dimir Signet must be auto-tapped for the colored mana during cost payment,
    // and a Plains funds the Signet's own `{1}` sub-cost.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Two-Color Test Spell", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, dimir);
    make_artifact(&mut runner, gruul);

    // Drive the real cast pipeline. Auto-tap funds the cost from the pool +
    // mana-ability activations; if the exclusion chain over-excluded, the colored
    // requirement would be unfundable and the `CastSpell` announcement would be
    // rejected ("Cannot pay mana cost"), panicking `resolve()`'s `.expect`. A
    // clean resolution proves the payment succeeded.
    let outcome = runner.cast(spell).resolve();

    // PRIMARY positive assertion: the cost was genuinely payable — the spell was
    // announced, paid for, and resolved off the stack (CR 608.2m). If the fix
    // over-excluded, the cast never reaches the stack at all.
    let resolved_zone = outcome.zone_of(spell);
    assert!(
        !matches!(resolved_zone, Zone::Hand | Zone::Stack),
        "the {{U}}{{B}} spell must have been cast and resolved off the stack, \
         but it is still in {resolved_zone:?} — the land-funded payment was \
         starved by over-exclusion"
    );

    // The colored mana could only have come from the Dimir Signet, so it was
    // tapped — the legitimate, land-funded activation was NOT starved.
    assert!(
        outcome.state().objects.get(&dimir).unwrap().tapped,
        "Dimir Signet was tapped to fund the {{U}}{{B}} — the land-funded \
         activation was not over-excluded"
    );
    // The Gruul Signet produces the wrong colors and was never needed; the
    // exclusion fix neither forced a wasteful tap nor blocked the real one.
    assert!(
        !outcome.state().objects.get(&gruul).unwrap().tapped,
        "the irrelevant Gruul Signet was left untapped"
    );
}
