//! Issue #5337 — GRANTED sunburst must place as-enters counters.
//!
//! Two coordinated gaps are exercised end-to-end through the real cast /
//! activation / trigger / replacement pipeline (no shape assertions):
//!
//! Gap 1 (parser): Solar Array's "When you next cast an artifact spell this
//! turn, THAT SPELL gains sunburst" is a `WhenNextEvent` delayed grant. The
//! subject-position "that spell" anaphor must bind `TriggeringSource` (the newly
//! cast spell / event source) rather than `ParentTarget` — a delayed trigger has
//! no parent target, so a `ParentTarget` grant registers against the empty
//! chain-tracked set and silently never lands (CR 608.2k).
//!
//! Gap 2 (runtime): a spell GRANTED sunburst carries the keyword but no
//! object-carried ETB replacement (only PRINTED sunburst is synthesized into
//! `replacement_definitions`). The runtime must surface a virtual as-enters
//! counter replacement for the granted instance so the permanent enters with a
//! counter per color of mana spent (CR 702.44a/b/d).
//!
//! Oracle texts are verbatim from Scryfall:
//! - Solar Array: "{T}: Add one mana of any color. When you next cast an
//!   artifact spell this turn, that spell gains sunburst. (...)"
//! - Lux Artillery: "Whenever you cast an artifact creature spell, it gains
//!   sunburst. (...)\n..."
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 702.44a: sunburst — enters with a +1/+1 (creature) or charge (otherwise)
//!   counter for each color of mana spent to cast it.
//! - CR 702.44b: counts colors of mana spent, from the stack as a resolving spell.
//! - CR 702.44d: multiple instances of sunburst each work separately.
//! - CR 608.2k: a "that spell" anaphor in a delayed trigger names the event source.
//! - CR 616.1: replacement-effect ordering (Doubling Season interplay).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SOLAR_ARRAY_ORACLE: &str = "{T}: Add one mana of any color. When you next cast an artifact spell this turn, that spell gains sunburst. (If it's a creature, it enters with a +1/+1 counter on it for each color of mana spent to cast it. Otherwise, it enters with that many charge counters on it.)";

const LUX_ARTILLERY_ORACLE: &str = "Whenever you cast an artifact creature spell, it gains sunburst. (It enters with a +1/+1 counter on it for each color of mana spent to cast it.)";

/// Turn a scenario "creature" permanent into a pure noncreature artifact and
/// clear its P/T so the 0/0 stub isn't destroyed as an SBA before use.
fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Artifact];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
}

/// Float `count` units of `ty` into P0's mana pool.
fn add_mana(runner: &mut GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn counters_of(runner: &GameRunner, id: ObjectId, ct: &CounterType) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(ct))
        .copied()
        .unwrap_or(0)
}

fn charge() -> CounterType {
    CounterType::Generic("charge".to_string())
}

/// Index of Solar Array's `{T}` mana ability.
fn mana_ability_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner
        .state()
        .objects
        .get(&id)
        .unwrap()
        .abilities
        .iter()
        .position(engine::game::mana_abilities::is_mana_ability)
        .expect("Solar Array has a mana ability")
}

/// Activate Solar Array's mana ability so the `WhenNextEvent` delayed grant is
/// created. Drives the "{T}: Add one mana of any color" color prompt manually
/// (`WaitingFor::ChooseManaColor` → `GameAction::ChooseManaColor`, the
/// brigid_mana_ability idiom), then CLEARS the pool so the produced unit cannot
/// leak into the cast — each test funds the cast with an explicit floated pool
/// so the colors-of-mana-spent mix stays test-controlled (CR 702.44b).
fn arm_solar_array(runner: &mut GameRunner, solar: ObjectId) {
    use engine::types::actions::GameAction;
    use engine::types::game_state::{ManaChoice, WaitingFor};

    let idx = mana_ability_index(runner, solar);
    runner
        .act(GameAction::ActivateAbility {
            source_id: solar,
            ability_index: idx,
        })
        .expect("activating Solar Array's mana ability must succeed");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ) {
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::White),
                count: 1,
            })
            .expect("submitting the any-color choice must succeed");
    }
    runner.state_mut().players[0].mana_pool.clear();
}

/// PRIMARY end-to-end revert-canary for BOTH gaps: Solar Array grants sunburst
/// to a cast artifact CREATURE spell; paying three distinct colors, it must
/// enter with three +1/+1 counters.
#[test]
fn solar_array_grants_sunburst_creature_three_colors_enters_with_three_p1p1() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let solar = scenario
        .add_creature_from_oracle(P0, "Solar Array", 0, 0, SOLAR_ARRAY_ORACLE)
        .id();

    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Golem", 0, 0, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, solar);
    // The cast spell must be an artifact creature.
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
    }

    arm_solar_array(&mut runner, solar);
    add_mana(&mut runner, ManaType::White, 1);
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Black, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    // PRIMARY revert-failing assertion: reverting EITHER gap makes this 0.
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        3,
        "granted-sunburst artifact creature cast for 3 colors must enter with 3 +1/+1 counters"
    );
    // Reach-guard: the spell actually resolved onto the battlefield.
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Battlefield,
        "the granted spell must have resolved onto the battlefield"
    );
}

/// Solar Array grants sunburst to a NONCREATURE artifact → charge counters.
#[test]
fn solar_array_grants_sunburst_noncreature_two_colors_enters_with_two_charge() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let solar = scenario
        .add_creature_from_oracle(P0, "Solar Array", 0, 0, SOLAR_ARRAY_ORACLE)
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Relic", false, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, solar);
    // The cast spell is a noncreature artifact.
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
    }

    arm_solar_array(&mut runner, solar);
    add_mana(&mut runner, ManaType::White, 1);
    add_mana(&mut runner, ManaType::Green, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    assert_eq!(
        counters_of(&runner_after, spell, &charge()),
        2,
        "granted-sunburst noncreature artifact cast for 2 colors must enter with 2 charge counters"
    );
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        0,
        "a noncreature granted-sunburst permanent must not place +1/+1 counters (CR 702.44a)"
    );
    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
}

/// CR 702.44a revert-canary for the PRINTED-vs-LIVE core-type branch.
///
/// Sunburst reads "if this object is entering as a creature, IGNORING ANY
/// TYPE-CHANGING EFFECTS that would affect it". This spell is a PRINTED
/// noncreature artifact whose LIVE card types include Creature while it is on the
/// stack — exactly the state a type-changing effect leaves behind (Layer-6 type
/// effects do reach off-battlefield objects via `remote_type_layer_recipients`,
/// and the layer pass re-seeds live characteristics only for battlefield objects,
/// so the divergence survives to the entry-replacement pipeline).
///
/// Branching on the LIVE types yields +1/+1 counters; the rule mandates the
/// PRINTED types, so charge counters are correct. Every other fixture in this
/// file sets `base_card_types == card_types`, so this is the only test that
/// exercises the divergent arm — without it the branch is unverified.
#[test]
fn granted_sunburst_ignores_type_changing_effect_and_branches_on_printed_types() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let solar = scenario
        .add_creature_from_oracle(P0, "Solar Array", 0, 0, SOLAR_ARRAY_ORACLE)
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Relic", false, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, solar);
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        // PRINTED (characteristic-defining): a noncreature artifact.
        obj.base_card_types.core_types = vec![CoreType::Artifact];
        // LIVE: a type-changing effect has made it an artifact creature. CR 702.44a
        // orders sunburst to ignore precisely this.
        obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
    }

    // Non-vacuity guard: the printed/live divergence this test turns on is really
    // present at cast time. If a future change re-seeds stack objects from their
    // printed types, this fires instead of the test silently going green.
    {
        let obj = runner.state().objects.get(&spell).unwrap();
        assert!(
            obj.card_types.core_types.contains(&CoreType::Creature),
            "fixture precondition: the LIVE types must include Creature"
        );
        assert!(
            !obj.base_card_types.core_types.contains(&CoreType::Creature),
            "fixture precondition: the PRINTED types must NOT include Creature"
        );
    }

    arm_solar_array(&mut runner, solar);
    add_mana(&mut runner, ManaType::White, 1);
    add_mana(&mut runner, ManaType::Green, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    // Reach-guard: the spell actually entered, so the counter assertions below
    // cannot pass vacuously on a spell that never resolved.
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Battlefield,
        "the granted spell must have resolved onto the battlefield"
    );
    // REVERT-FAILING: branching on the live `card_types` makes this 0 (and the
    // +1/+1 assertion below 2).
    assert_eq!(
        counters_of(&runner_after, spell, &charge()),
        2,
        "CR 702.44a: sunburst ignores type-changing effects, so a PRINTED noncreature \
         artifact must enter with charge counters even while a type-changing effect \
         makes it a creature"
    );
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        0,
        "CR 702.44a: the LIVE creature type must not redirect sunburst to +1/+1 counters"
    );
}

/// Lux Artillery grants sunburst via a NON-delayed trigger ("it gains
/// sunburst"). Revert-canary for gap 2 alone (its trigger already lowers to
/// `TriggeringSource`, so gap 1's parser lift is not exercised).
#[test]
fn lux_artillery_grants_sunburst_two_colors_enters_with_two_p1p1() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let lux = scenario
        .add_creature_from_oracle(P0, "Lux Artillery", 0, 0, LUX_ARTILLERY_ORACLE)
        .id();

    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Automaton", 0, 0, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, lux);
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
    }

    add_mana(&mut runner, ManaType::Red, 1);
    add_mana(&mut runner, ManaType::Green, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        2,
        "Lux Artillery's granted sunburst on a 2-color cast must place 2 +1/+1 counters"
    );
    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
}

/// Negative (CR 702.44b): a granted-sunburst artifact cast paying ZERO colored
/// mana (all generic/colorless) must enter with NO counters.
#[test]
fn solar_array_granted_sunburst_zero_colors_places_no_counters() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let solar = scenario
        .add_creature_from_oracle(P0, "Solar Array", 0, 0, SOLAR_ARRAY_ORACLE)
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Colorless Relic", false, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, solar);
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
    }

    arm_solar_array(&mut runner, solar);
    // Two colorless units fund the {2} generic cost — no colored mana spent.
    add_mana(&mut runner, ManaType::Colorless, 2);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    // Reach-guard: the spell resolved (so the replacement pipeline WAS consulted
    // for its battlefield entry), yet zero colors were spent.
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Battlefield,
        "the colorless-cast granted-sunburst permanent must have resolved"
    );
    assert_eq!(
        counters_of(&runner_after, spell, &charge()),
        0,
        "zero colors of mana spent means zero charge counters (CR 702.44b)"
    );
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        0,
        "zero colors of mana spent means zero +1/+1 counters"
    );
    let _ = ManaColor::White; // keep the ManaColor import honest across cfgs
}

/// Printed-sunburst control (CR 702.44a/b): the pre-synthesized object-carried
/// replacement path is untouched by the granted-instance virtual candidate —
/// a PRINTED-sunburst noncreature artifact cast for three colors still enters
/// with three charge counters.
#[test]
fn printed_sunburst_control_three_colors_enters_with_three_charge() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Explicit keyword hint: the bare "Sunburst (reminder)" line needs the
    // MTGJSON-style keyword name for the scenario's keyword-line detection,
    // which feeds `synthesize_all` (the printed ETB-replacement synthesis).
    let spell = {
        let mut b = scenario.add_spell_to_hand(P0, "Printed Relic", false);
        b.from_oracle_text_with_keywords(
            &["Sunburst"],
            "Sunburst (This enters with a charge counter on it for each color of mana spent to cast it.)",
        );
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
            generic: 0,
        })
        .id()
    };

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
    }

    add_mana(&mut runner, ManaType::White, 1);
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Black, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
    assert_eq!(
        counters_of(&runner_after, spell, &charge()),
        3,
        "printed sunburst cast for 3 colors must enter with 3 charge counters (control)"
    );
}

/// CR 702.44d: "If an object has multiple instances of sunburst, each one works
/// separately." A PRINTED-sunburst artifact that is ALSO granted sunburst by
/// Solar Array must place its as-enters counters TWICE — once via the
/// object-carried printed replacement, once via the granted-instance virtual
/// candidate (which counts only the granted surplus, so nothing is lost or
/// double-counted on either side).
#[test]
fn printed_plus_granted_sunburst_each_apply_separately() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let solar = scenario
        .add_creature_from_oracle(P0, "Solar Array", 0, 0, SOLAR_ARRAY_ORACLE)
        .id();
    // Explicit keyword hint — see the printed-control test above.
    let spell = {
        let mut b = scenario.add_spell_to_hand(P0, "Printed Relic", false);
        b.from_oracle_text_with_keywords(
            &["Sunburst"],
            "Sunburst (This enters with a charge counter on it for each color of mana spent to cast it.)",
        );
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Green],
            generic: 0,
        })
        .id()
    };

    let mut runner = scenario.build();
    make_artifact(&mut runner, solar);
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
    }

    arm_solar_array(&mut runner, solar);
    add_mana(&mut runner, ManaType::White, 1);
    add_mana(&mut runner, ManaType::Green, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
    assert_eq!(
        counters_of(&runner_after, spell, &charge()),
        4,
        "printed + granted sunburst on a 2-color cast must each place 2 charge counters (CR 702.44d: 2+2=4)"
    );
}

/// CR 616.1: the granted-instance virtual candidate participates in the normal
/// replacement-ordering pipeline — a Doubling Season-class AddCounter doubler
/// doubles the granted sunburst's placement (2 colors → 2 counters → 4).
#[test]
fn granted_sunburst_participates_in_counter_doubling() {
    use engine::types::ability::QuantityModification;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::ReplacementDefinition;

    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    let solar = scenario
        .add_creature_from_oracle(P0, "Solar Array", 0, 0, SOLAR_ARRAY_ORACLE)
        .id();
    let doubler = scenario.add_creature(P0, "Doubling Season", 0, 3).id();
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Golem", 0, 0, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Green],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, solar);
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
    }
    // CR 614.1a: a Doubling Season-class counter-doubling replacement.
    {
        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::DOUBLE);
        runner
            .state_mut()
            .objects
            .get_mut(&doubler)
            .unwrap()
            .replacement_definitions
            .push(repl);
    }

    arm_solar_array(&mut runner, solar);
    add_mana(&mut runner, ManaType::Red, 1);
    add_mana(&mut runner, ManaType::Green, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    assert_eq!(outcome.zone_of(spell), Zone::Battlefield);
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        4,
        "granted sunburst (2 colors) under a counter doubler must enter with 4 +1/+1 counters (CR 616.1)"
    );
}

/// #5802 review (CR 616.1e): the granted-sunburst virtual candidate is a
/// counter-payload WRITE (`Writes { Count, Additive }`), not `Disjoint` — when a
/// same-event Count writer co-fires on the entering spell's ZoneChange, the
/// affected controller's ordering choice MUST surface. Reverting the
/// classification to `Disjoint` suppresses the prompt and this test fails.
///
/// Both legal orderings are driven end-to-end. NOTE on outcomes: in the current
/// engine the two orders converge (2 counters each) because a bare
/// `quantity_modification` on a `Moved`-keyed definition has no ZoneChange
/// counter-payload applier yet — the functioning Doubling Season path scales the
/// downstream AddCounter placement instead (covered by
/// `granted_sunburst_participates_in_counter_doubling`, which asserts 4). The
/// assertions below pin (a) the prompt surfacing with both candidates, (b) both
/// orders being drivable to a clean entry, and (c) the granted payload surviving
/// either order — so if a ZoneChange payload applier lands later, only the
/// counter totals need updating (4 for sunburst-first, 2 for writer-first), not
/// the ordering machinery.
fn drive_sunburst_vs_count_writer_ordering(pick_sunburst_first: bool) -> u32 {
    use engine::types::ability::QuantityModification;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::replacements::ReplacementEvent;
    use engine::types::ReplacementDefinition;

    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let solar = scenario
        .add_creature_from_oracle(P0, "Solar Array", 0, 0, SOLAR_ARRAY_ORACLE)
        .id();
    let writer = scenario.add_creature(P0, "Entry Count Writer", 0, 3).id();
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Golem", 0, 0, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    make_artifact(&mut runner, solar);
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
    }
    // A same-event Count writer on the entering spell's ZoneChange (the
    // Moved-keyed quantity-modification shape from the reclassified pair).
    {
        let repl = ReplacementDefinition::new(ReplacementEvent::Moved)
            .quantity_modification(QuantityModification::DOUBLE);
        runner
            .state_mut()
            .objects
            .get_mut(&writer)
            .unwrap()
            .replacement_definitions
            .push(repl);
    }
    arm_solar_array(&mut runner, solar);
    add_mana(&mut runner, ManaType::Red, 1);
    add_mana(&mut runner, ManaType::Green, 1);

    let commit = runner.cast(spell).commit();
    let mut r2 = GameRunner::from_state(commit.state().clone());
    let mut saw_ordering_prompt = false;
    for _ in 0..30 {
        match r2.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if r2.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                // CR 616.1e revert-canary: BOTH candidates must be offered
                // together — a `Disjoint` classification auto-applies the
                // sunburst appender and never surfaces this prompt.
                if candidates.len() == 2 {
                    saw_ordering_prompt = true;
                    let sunburst_idx = candidates
                        .iter()
                        .position(|c| c.description.contains("Sunburst"))
                        .expect("sunburst candidate must be listed");
                    let writer_idx = 1 - sunburst_idx;
                    let idx = if pick_sunburst_first {
                        sunburst_idx
                    } else {
                        writer_idx
                    };
                    r2.act(GameAction::ChooseReplacement { index: idx })
                        .expect("ordering choice must be accepted");
                } else {
                    r2.act(GameAction::ChooseReplacement { index: 0 })
                        .expect("remaining replacement choice must be accepted");
                }
            }
            other => panic!("unexpected waiting state during entry: {other:?}"),
        }
        let done = {
            let st = r2.state();
            st.objects
                .get(&spell)
                .is_some_and(|o| o.zone == Zone::Battlefield)
        };
        if done {
            break;
        }
    }
    assert!(
        saw_ordering_prompt,
        "the CR 616.1e ordering prompt must surface for the co-firing counter-payload writers"
    );
    let o = r2.state().objects.get(&spell).unwrap();
    assert_eq!(o.zone, Zone::Battlefield, "the spell must finish entering");
    o.counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn granted_sunburst_ordering_choice_sunburst_first() {
    let counters = drive_sunburst_vs_count_writer_ordering(true);
    assert_eq!(
        counters, 2,
        "sunburst-first: the granted payload (2 colors) must survive the ordering pass"
    );
}

#[test]
fn granted_sunburst_ordering_choice_count_writer_first() {
    let counters = drive_sunburst_vs_count_writer_ordering(false);
    assert_eq!(
        counters, 2,
        "writer-first: the granted payload must still be appended after the writer applies"
    );
}
