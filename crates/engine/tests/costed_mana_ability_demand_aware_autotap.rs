//! Demand-aware nested auto-tap for costed mana abilities.
//!
//! Regression for the false "Cannot pay mana cost" when funding a costed mana
//! ability's generic sub-cost consumed already-floated colored mana that the
//! OUTER cost still needed.
//!
//! Repro: cast a `{U}{B}{R}{G}` sorcery with a Dimir Signet (`{1},{T}: Add
//! {U}{B}`), a Gruul Signet (`{1},{T}: Add {R}{G}`), and two Plains. The outer
//! cost reserves both Signets for its four colored shards. Activating Dimir
//! floats `{U}{B}` and a Plains pays its `{1}`. Pre-fix, funding Gruul's `{1}`
//! consumed the floated `{U}` (a tie-break pick) instead of tapping the second
//! Plains, leaving the outer `{U}{B}{R}{G}` short of `{U}` — the spell was
//! wrongly reported unpayable.
//!
//! The fix is three coordinated layers plus a CR 118.10 source-exclusion fix:
//!   - Layer A (planner): when the nested sub-cost's outer-cost colored demand
//!     is known, a generic pip is counted covered ONLY by a non-demanded scratch
//!     unit, so the second Plains is planned to tap.
//!   - Layer B (real spend): the `{1}` payment softly deprioritizes a demanded
//!     color, so it pays from `{W}` (Plains) not the reserved `{U}`.
//!   - Layer C (exclusion): the nested sub-cost auto-tap excludes every source
//!     the outer plan reserved (CR 118.10), so it can't grab a reserved source.
//!
//! All threading is `Option<&ColorDemand>` and `None` on every top-level / cast /
//! affordability path, so non-nested behavior is byte-identical.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 107.4b: Generic mana in costs can be paid with any type of mana.
//!   - CR 118.10: Each payment of a cost applies to only one spell or ability.
//!   - CR 601.2h: Partial payments aren't allowed and unpayable costs can't be
//!     paid; conversely a payable cost must never be reported unpayable.
//!   - CR 605.1b / CR 605.3a / CR 605.3c: Signets are mana abilities; their mana
//!     sub-cost may itself activate further mana abilities, bounded by the
//!     in-flight exclusion chain.
//!
//! This is a *class* regression covering any multicolor cost funded by a mix of
//! costed colored mana rocks and plain mana sources. Driven through the real
//! cast / activation pipeline, not shape assertions.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
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

/// Float `count` units of `ty` into player 0's mana pool (no source modeled;
/// mirrors the `add_mana` helper in `chord_of_calling.rs`).
fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
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
fn wubrg_spell_funded_by_two_signets_and_two_plains_resolves() {
    // PRIMARY discriminating test. A `{U}{B}{R}{G}` sorcery, two Signets that
    // together supply all four colors, and exactly two Plains to fund the two
    // `{1}` Signet sub-costs. Pre-fix, funding the second Signet's `{1}` from a
    // floated color the outer cost still needed made `CastSpell` reject with
    // "Cannot pay mana cost" — `resolve()`'s `.expect` panics. Post-fix the
    // legal line (Plains -> Dimir, Plains -> Gruul, pool {U}{B}{R}{G}) resolves.
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Exactly two Plains: one per Signet `{1}` sub-cost. No spare colorless or
    // extra colored source — if a Signet's `{1}` consumes a reserved floated
    // color, the outer cost is genuinely short and the cast fails.
    let plains1 = scenario.add_basic_land(P0, ManaColor::White);
    let plains2 = scenario.add_basic_land(P0, ManaColor::White);

    let dimir = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();
    let gruul = scenario
        .add_creature_from_oracle(P0, "Gruul Signet", 0, 0, GRUUL_SIGNET_ORACLE)
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "WUBRG Test Spell", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Black,
                ManaCostShard::Red,
                ManaCostShard::Green,
            ],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, dimir);
    make_artifact(&mut runner, gruul);

    // Drive the real cast pipeline. Auto-tap funds {U}{B}{R}{G} by activating
    // both Signets, each `{1}` paid from a Plains. If the fix regressed, the
    // colored requirement is unfundable and the cast never reaches the stack.
    let outcome = runner.cast(spell).resolve();

    // PRIMARY revert-failing assertion: the cost was genuinely payable and the
    // spell resolved off the stack (CR 608.2m). Reverting any of the three
    // layers makes funding the second Signet steal a reserved color, the cost
    // is short, and this assertion flips (the spell stays in Hand/Stack and the
    // cast `.expect` panics first).
    let resolved_zone = outcome.zone_of(spell);
    assert!(
        !matches!(resolved_zone, Zone::Hand | Zone::Stack),
        "the {{U}}{{B}}{{R}}{{G}} spell must have cast and resolved, but it is \
         still in {resolved_zone:?} — a Signet `{{1}}` consumed a reserved color"
    );

    // Both Signets supplied colors the Plains cannot, so both were tapped.
    assert!(
        outcome.state().objects.get(&dimir).unwrap().tapped,
        "Dimir Signet was tapped for {{U}}{{B}}"
    );
    assert!(
        outcome.state().objects.get(&gruul).unwrap().tapped,
        "Gruul Signet was tapped for {{R}}{{G}}"
    );

    // Both Plains were tapped to fund the two `{1}` sub-costs: Layer A planned
    // the second Plains instead of consuming the reserved floated color, and
    // Layer B paid each `{1}` from {W}, not from a color the outer cost needs.
    assert!(
        outcome.state().objects.get(&plains1).unwrap().tapped,
        "the first Plains was tapped to fund a Signet `{{1}}`"
    );
    assert!(
        outcome.state().objects.get(&plains2).unwrap().tapped,
        "the second Plains was tapped — Layer A planned it instead of consuming \
         the floated {{U}} the outer cost reserved"
    );

    // Every produced unit was spent paying the spell: the pool is empty.
    assert_eq!(
        outcome.state().players[0].mana_pool.total(),
        0,
        "the full {{U}}{{B}}{{R}}{{G}} was spent — no leftover floated mana"
    );
}

#[test]
fn signet_one_cost_paid_from_only_remaining_float_when_no_spare_source() {
    // CR 601.2h float-only fallback. A Dimir Signet activated with NO untapped
    // mana source: its `{1}` can only be paid from mana already floating in the
    // pool. We pre-float a single {U}. Even though {U} is a color the Signet
    // itself will produce (so a naive demand rule might refuse to spend it), the
    // soft spend must STILL pay the `{1}` from the only available unit — never
    // hard-block a payable cost.
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let dimir = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, dimir);

    // Pre-float exactly one {U} into the player's pool — the only mana available
    // to pay the Signet's `{1}` (no untapped land or rock exists).
    add_mana(&mut runner, ManaType::Blue, 1);

    let idx = mana_ability_index(runner.state(), dimir);
    runner
        .act(GameAction::ActivateAbility {
            source_id: dimir,
            ability_index: idx,
        })
        .expect("the Signet `{1}` must be payable from the only floated unit (CR 601.2h)");

    // Revert-failing assertion: the Signet activated and produced {U}{B}. If the
    // soft spend had hard-refused the demanded {U}, activation would error and
    // the pool would still hold the single pre-floated {U}.
    assert!(
        runner.state().objects.get(&dimir).unwrap().tapped,
        "Dimir Signet activated and tapped"
    );
    // Pool: started with 1 ({U}), paid the `{1}` (-1), produced {U}{B} (+2) = 2.
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        2,
        "Signet produced {{U}}{{B}} after paying its `{{1}}` from the only float"
    );
}

#[test]
fn reserved_sibling_rock_not_cross_tapped_for_nested_sub_cost() {
    // Layer C reservation (CR 118.10). A `{U}{B}{R}{G}` sorcery funded by two
    // Signets (Dimir {U}{B}, Gruul {R}{G}) and three Plains — one spare. The
    // outer plan reserves BOTH Signets for the four colored shards. Phase 3 taps
    // them sequentially: while Dimir resolves, the Gruul Signet is reserved for
    // the outer cost but not yet tapped. Pre-Layer-C, Dimir's `{1}` nested
    // auto-tap re-scanned the battlefield and could grab the still-untapped Gruul
    // (a source the outer cost still needs) instead of a Plains, double-spending
    // a reserved source. Layer C excludes every outer-reserved source from the
    // nested sub-cost auto-tap, so a Plains funds the `{1}` and Gruul stays
    // available for the outer {R}{G}. The spare third Plains proves the
    // exclusion does not strand the payment.
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let plains: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();

    let dimir = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();
    let gruul = scenario
        .add_creature_from_oracle(P0, "Gruul Signet", 0, 0, GRUUL_SIGNET_ORACLE)
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "WUBRG-minus-W Test Spell", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Black,
                ManaCostShard::Red,
                ManaCostShard::Green,
            ],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, dimir);
    make_artifact(&mut runner, gruul);

    let outcome = runner.cast(spell).resolve();

    // Revert-failing assertion: the cost resolved without a false-unpayable. If
    // Layer C let the nested sub-cost grab the reserved Gruul, the outer {R}{G}
    // would be short and the cast would be rejected.
    let resolved_zone = outcome.zone_of(spell);
    assert!(
        !matches!(resolved_zone, Zone::Hand | Zone::Stack),
        "the four-color spell must have resolved, but it is still in \
         {resolved_zone:?} — a reserved sibling rock was cross-tapped"
    );

    // Both Signets supplied colors the Plains cannot, so both were tapped for the
    // OUTER cost (not consumed by a nested sub-cost).
    for (name, id) in [("Dimir", dimir), ("Gruul", gruul)] {
        assert!(
            outcome.state().objects.get(&id).unwrap().tapped,
            "{name} Signet was tapped to supply its colors for the outer cost"
        );
    }
    // Exactly two of the three Plains funded the two `{1}` sub-costs; one is
    // spare and stays untapped (the exclusion did not over-tap).
    let tapped_plains = plains
        .iter()
        .filter(|id| outcome.state().objects.get(id).unwrap().tapped)
        .count();
    assert_eq!(
        tapped_plains, 2,
        "exactly two Plains funded the two Signet `{{1}}` sub-costs; one is spare"
    );
    assert_eq!(
        outcome.state().players[0].mana_pool.total(),
        0,
        "the full {{U}}{{B}}{{R}}{{G}} was spent — no stranded leftover mana"
    );
}
