//! #5802 review (matthewevans): the granted-as-enters-keyword virtual candidate
//! must generalize beyond Sunburst to Bloodthirst.
//!
//! Bloodlord of Vaasgoth's "Whenever you cast a Vampire creature spell, it gains
//! bloodthirst 3" grants Bloodthirst to a spell still ON THE STACK. Printed
//! Bloodthirst is synthesized into an object-carried ETB `ReplacementDefinition`
//! (`synthesize_bloodthirst`); a runtime grant adds only the keyword, so — exactly
//! like granted Sunburst (#5337) — the runtime must surface a VIRTUAL as-enters
//! counter replacement for the granted instance.
//!
//! Unlike Sunburst, fixed-N Bloodthirst is CONDITIONAL (CR 702.54a): the counters
//! are placed only if an opponent was dealt damage this turn. The virtual applier
//! honors that carried condition — the negative test below (no opponent damage →
//! ZERO counters) is the revert-canary for the condition handling.
//!
//! Oracle text is verbatim from Scryfall:
//! - Bloodlord of Vaasgoth: "Bloodthirst 3 (If an opponent was dealt damage this
//!   turn, this creature enters with three +1/+1 counters on it.)\nFlying\n
//!   Whenever you cast a Vampire creature spell, it gains bloodthirst 3."
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 702.54a: Bloodthirst N — if an opponent was dealt damage this turn, the
//!   creature enters with N +1/+1 counters.
//! - CR 702.54c: multiple instances of Bloodthirst each work separately.
//! - CR 613.1 + CR 400.7a: a keyword granted to a spell on the stack applies to
//!   the permanent that spell becomes.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::DamageRecord;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{BloodthirstValue, Keyword};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BLOODLORD_ORACLE: &str = "Bloodthirst 3 (If an opponent was dealt damage this turn, this creature enters with three +1/+1 counters on it.)\nFlying\nWhenever you cast a Vampire creature spell, it gains bloodthirst 3.";

/// Float `count` units of `ty` into P0's mana pool so the cast is funded.
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

/// Record noncombat damage to P0's opponent (P1) earlier this turn so the
/// Bloodthirst condition (CR 702.54a: "an opponent was dealt damage this turn")
/// is TRUE. CR 702.54a doesn't care about the source, so any source id works.
fn deal_damage_to_opponent(runner: &mut GameRunner) {
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(DamageRecord {
            source_id: ObjectId(999),
            source_controller: P0,
            target: TargetRef::Player(P1),
            target_controller: P1,
            amount: 1,
            is_combat: false,
            ..Default::default()
        });
}

/// Grant `bloodthirst 3` to `spell` the way Bloodlord's trigger would — a
/// transient continuous `AddKeyword` on the stack object, then rebuild the runner
/// so the layer pass materializes the grant onto the object's live keyword set.
/// This mirrors the `stack_object_keyword_grants` idiom (a `TriggeringSource`
/// keyword grant to a spell on the stack) and exercises the SAME virtual-candidate
/// surfacing the full Bloodlord trigger would reach at entry.
fn grant_bloodthirst_via_transient(runner: &mut GameRunner, spell: ObjectId, value: u32) {
    use engine::types::ability::{ContinuousModification, Duration, TargetFilter};

    runner.state_mut().add_transient_continuous_effect(
        spell,
        P0,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: spell },
        vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Bloodthirst(BloodthirstValue::Fixed(value)),
        }],
        None,
    );
}

/// Set up a Bloodlord on the battlefield (its trigger will grant Bloodthirst to a
/// cast Vampire creature) and a Vampire creature spell in hand (funded but not yet
/// cast). Returns `(runner, bloodlord, spell)`.
fn bloodlord_board() -> (GameRunner, ObjectId, ObjectId) {
    board_with(true)
}

/// Set up a Vampire creature spell in hand WITHOUT any Bloodlord on the
/// battlefield, so ONLY an explicit transient grant places counters (no
/// interfering real trigger). Returns `(runner, spell)`.
fn plain_vampire_board() -> (GameRunner, ObjectId) {
    let (runner, _sink, spell) = board_with(false);
    (runner, spell)
}

fn board_with(include_bloodlord: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // A vanilla permanent keeps `sink` populated when Bloodlord is absent so the
    // return shape is uniform; its plain-vanilla text grants nothing.
    let sink = if include_bloodlord {
        scenario
            .add_creature_from_oracle(P0, "Bloodlord of Vaasgoth", 4, 4, BLOODLORD_ORACLE)
            .id()
    } else {
        scenario.add_creature(P0, "Vanilla Bear", 2, 2).id()
    };

    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Vampire", 2, 2, "")
        .with_subtypes(vec!["Vampire"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    // The cast spell must be a Vampire CREATURE for Bloodlord's `valid_card`.
    {
        let obj = runner.state_mut().objects.get_mut(&spell).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        if !obj.card_types.subtypes.iter().any(|s| s == "Vampire") {
            obj.card_types.subtypes.push("Vampire".to_string());
        }
        obj.base_card_types = obj.card_types.clone();
    }
    (runner, sink, spell)
}

/// PRIMARY revert-canary for the Bloodthirst generalization: a spell GRANTED
/// bloodthirst 3 (transient, no interfering trigger), cast with an opponent
/// already dealt damage this turn, must enter with 3 +1/+1 counters via the
/// virtual granted-Bloodthirst candidate.
#[test]
fn granted_bloodthirst_with_opponent_damage_enters_with_three_p1p1() {
    let (mut runner, spell) = plain_vampire_board();

    // CR 702.54a: an opponent WAS dealt damage this turn — condition holds.
    deal_damage_to_opponent(&mut runner);
    grant_bloodthirst_via_transient(&mut runner, spell, 3);
    add_mana(&mut runner, ManaType::Black, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    // Reach-guard: the granted spell actually resolved onto the battlefield, so
    // the entry replacement pipeline WAS consulted for its ZoneChange.
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Battlefield,
        "the granted-bloodthirst spell must have resolved onto the battlefield"
    );
    // PRIMARY revert-failing assertion: reverting the granted-Bloodthirst virtual
    // candidate (or the shared applier) makes this 0.
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        3,
        "granted bloodthirst 3 with an opponent damaged this turn must place 3 +1/+1 counters"
    );
}

/// CONDITIONAL negative — the revert-canary for the applier's condition handling
/// (CR 702.54a). Same grant, but NO opponent was dealt damage this turn, so the
/// carried `condition` is unmet and ZERO counters are placed.
#[test]
fn granted_bloodthirst_without_opponent_damage_places_no_counters() {
    let (mut runner, spell) = plain_vampire_board();

    // CR 702.54a: NO opponent damage recorded — the condition is FALSE.
    assert!(
        runner.state().damage_dealt_this_turn.is_empty(),
        "precondition: no damage this turn"
    );
    grant_bloodthirst_via_transient(&mut runner, spell, 3);
    add_mana(&mut runner, ManaType::Black, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    // Reach-guard: the spell resolved (so the granted-Bloodthirst candidate WAS
    // consulted for its battlefield entry) — the zero below is the condition
    // gating, not a short-circuit before the candidate ran.
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Battlefield,
        "the spell must resolve so the granted-bloodthirst candidate is consulted"
    );
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        0,
        "granted bloodthirst 3 with NO opponent damaged this turn must place ZERO counters (CR 702.54a)"
    );
}

/// End-to-end through the REAL Bloodlord trigger: cast a Vampire creature spell
/// while Bloodlord is on the battlefield. Its "Whenever you cast a Vampire
/// creature spell, it gains bloodthirst 3" trigger grants Bloodthirst to the spell
/// on the stack; with an opponent damaged this turn it enters with 3 counters.
#[test]
fn bloodlord_trigger_grants_bloodthirst_end_to_end() {
    let (mut runner, bloodlord, spell) = bloodlord_board();
    let _ = bloodlord;

    deal_damage_to_opponent(&mut runner);
    add_mana(&mut runner, ManaType::Black, 1);

    let outcome = runner.cast(spell).resolve();
    let runner_after = GameRunner::from_state(outcome.state().clone());

    assert_eq!(
        outcome.zone_of(spell),
        Zone::Battlefield,
        "the cast Vampire creature must resolve onto the battlefield"
    );
    assert_eq!(
        counters_of(&runner_after, spell, &CounterType::Plus1Plus1),
        3,
        "Bloodlord's granted bloodthirst 3 (opponent damaged) must place 3 +1/+1 counters end-to-end"
    );
    let _ = PlayerId(0);
}
