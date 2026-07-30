//! Regression for GitHub issue #6504 — Jeweled Amulet's first ability places a
//! charge counter but must also note the mana type spent to pay its own {1}
//! activation cost (CR 106.1b), and its second (mana) ability must read that
//! noted type back to produce matching mana (CR 106.5: no noted type, no
//! mana). Before the fix, both clauses fell through to `Effect::Unimplemented`
//! and the second ability produced no mana at all regardless of what was
//! noted.

use engine::game::effects::bounce;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{BounceSelection, Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const JEWELED_AMULET_ORACLE: &str = "{1}, {T}: Put a charge counter on this artifact. \
Note the type of mana spent to pay this activation cost. Activate only if there are no \
charge counters on this artifact.\n\
{T}, Remove a charge counter from this artifact: Add one mana of this artifact's last \
noted type.";

const CHARGE: fn() -> CounterType = || CounterType::Generic("charge".to_string());

fn charge_counters(runner: &engine::game::scenario::GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CHARGE()).copied())
        .unwrap_or(0)
}

fn pool_color(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
    color: ManaType,
) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.count_color(color))
        .unwrap_or(0)
}

fn pool_total(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0)
}

/// Activates ability 0 ({1}, {T}: put a counter, note the paid type) funded by
/// exactly one mana unit of `color`, then untaps the amulet so ability 1's own
/// {T} cost isn't blocked by ability 0's tap (they are unrelated costs on the
/// same permanent).
fn activate_note_ability(
    runner: &mut engine::game::scenario::GameRunner,
    amulet: ObjectId,
    color: ManaType,
) {
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(color, ObjectId(0), false, vec![]));
    runner.activate(amulet, 0).resolve();
    runner
        .state_mut()
        .objects
        .get_mut(&amulet)
        .expect("amulet must exist")
        .tapped = false;
}

#[test]
fn jeweled_amulet_notes_and_produces_matching_mana_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let amulet = scenario
        .add_creature_from_oracle(P0, "Jeweled Amulet", 0, 0, JEWELED_AMULET_ORACLE)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    activate_note_ability(&mut runner, amulet, ManaType::Red);
    assert_eq!(
        charge_counters(&runner, amulet),
        1,
        "first ability must place exactly one charge counter"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 1,
        })
        .expect("activate the mana ability (CR 605.3b: resolves immediately, no stack)");

    assert_eq!(
        pool_color(&runner, P0, ManaType::Red),
        1,
        "the noted type (red) must be produced, not zero mana"
    );
    assert_eq!(
        charge_counters(&runner, amulet),
        0,
        "the mana ability must remove the charge counter it noted the type from"
    );
}

/// Same round trip with a different paid color, proving the produced type
/// tracks whatever was actually spent rather than being hardcoded to one
/// color (CR 106.1b names white/blue/black/red/green/colorless — the noted
/// value must be read back verbatim, not defaulted).
#[test]
fn jeweled_amulet_tracks_a_different_noted_color() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let amulet = scenario
        .add_creature_from_oracle(P0, "Jeweled Amulet", 0, 0, JEWELED_AMULET_ORACLE)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    activate_note_ability(&mut runner, amulet, ManaType::Green);

    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 1,
        })
        .expect("activate the mana ability");

    assert_eq!(
        pool_color(&runner, P0, ManaType::Green),
        1,
        "green was spent to pay the first ability, so green must be produced"
    );
    assert_eq!(
        pool_color(&runner, P0, ManaType::Red),
        0,
        "no red should appear when green was the noted type"
    );
}

/// CR 106.5: an ability that would produce mana of an undefined type produces
/// no mana instead. A freshly entered amulet (or one whose first ability was
/// never activated) has nothing noted, so the second ability must add zero
/// mana — never silently defaulting to some fixed color.
#[test]
fn jeweled_amulet_produces_no_mana_with_nothing_noted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let amulet = scenario
        .add_creature_from_oracle(P0, "Jeweled Amulet", 0, 0, JEWELED_AMULET_ORACLE)
        .as_artifact()
        .id();
    // A charge counter is placed directly (bypassing ability 0 entirely) so
    // ability 1's "remove a charge counter" cost is payable while nothing has
    // ever been noted on this incarnation of the object.
    scenario.with_counter(amulet, CHARGE(), 1);
    let mut runner = scenario.build();

    assert_eq!(
        pool_total(&runner, P0),
        0,
        "reach-guard: pool must start empty"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 1,
        })
        .expect("activate the mana ability");

    assert_eq!(
        pool_total(&runner, P0),
        0,
        "CR 106.5: no noted type must produce no mana, not a default color"
    );
    assert_eq!(
        charge_counters(&runner, amulet),
        0,
        "the charge counter is still removed even though no mana was produced"
    );
}

/// CR 400.7: a source that leaves and returns while its OWN activation is
/// still unresolved on the stack becomes a new object at the same storage id
/// (see `GameObject::incarnation`). Bouncing Jeweled Amulet in response to
/// its own first ability — before that ability resolves — must NOT let the
/// note land on the new incarnation: that incarnation never paid anything
/// itself. Without the incarnation guard in `Effect::NoteManaSpent`, the
/// stale `mana_spent_to_activate` latch from the departed incarnation would
/// be promoted onto the returned card's `chosen_attributes` anyway.
#[test]
fn jeweled_amulet_bounced_mid_stack_does_not_note_on_new_incarnation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let amulet = scenario
        .add_creature_from_oracle(P0, "Jeweled Amulet", 0, 0, JEWELED_AMULET_ORACLE)
        .as_artifact()
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();

    // (1) Activate ability 0. The {1} cost auto-pays from the single floating
    // red unit with no ambiguity, so one action step lands the ability on the
    // stack, unresolved, at the post-announcement Priority window.
    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 0,
        })
        .expect("activating ability 0 must succeed");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: the note ability must be sitting on the stack, unresolved"
    );
    let incarnation_before = runner.state().objects[&amulet].incarnation;

    // (2) Interject: bounce the amulet through the real bounce resolver
    // (not a raw zone-field flip) before its own ability resolves.
    let bounce_ability = ResolvedAbility::new(
        Effect::Bounce {
            target: TargetFilter::Any,
            destination: None,
            selection: BounceSelection::Targeted,
        },
        vec![TargetRef::Object(amulet)],
        ObjectId(999),
        P0,
    );
    let mut events = Vec::new();
    bounce::resolve(runner.state_mut(), &bounce_ability, &mut events)
        .expect("bouncing the amulet must succeed");
    assert_eq!(
        runner.state().objects[&amulet].zone,
        Zone::Hand,
        "reach-guard: the amulet must actually be in hand now"
    );
    assert!(
        runner.state().objects[&amulet].incarnation > incarnation_before,
        "reach-guard: the zone change must have bumped the object's incarnation"
    );

    // (3) CR 112.7a: the ability is independent of its departed source and
    // still resolves.
    runner.resolve_top();
    assert!(
        runner.state().stack.is_empty(),
        "the note ability must have resolved off the stack"
    );

    // (4) The new incarnation, sitting in hand, must have nothing noted.
    assert!(
        runner.state().objects[&amulet].noted_mana_spent().is_none(),
        "a bounced-and-returned amulet must not inherit the departed \
         incarnation's payment as a noted type"
    );
}
