//! Regression for GitHub issue #6504 — Jeweled Amulet's first ability places a
//! charge counter but must also note the mana type spent to pay its own {1}
//! activation cost (CR 106.1b), and its second (mana) ability must read that
//! noted type back to produce matching mana (CR 106.5: no noted type, no
//! mana). Before the fix, both clauses fell through to `Effect::Unimplemented`
//! and the second ability produced no mana at all regardless of what was
//! noted.

use engine::game::effects::{bounce, copy_spell};
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    BounceSelection, CopyRetargetPermission, Effect, ResolvedAbility, TargetFilter, TargetRef,
};
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

/// CR 608.2c: an ability's instructions are followed only when it RESOLVES,
/// not when it's activated/paid for. Activating ability 0 and leaving it
/// sitting unresolved on the stack must not produce a durable
/// `ChosenAttribute::NotedManaSpent` yet — the write happens inside
/// `Effect::NoteManaSpent`'s resolution, never at payment time. (A countered
/// ability never reaches this resolution step at all, so this is also the
/// discriminating half of the "a countered ability never notes anything"
/// claim in `note_mana_spent.rs`'s doc comment.)
#[test]
fn jeweled_amulet_notes_nothing_before_resolution() {
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

    assert!(
        runner.state().objects[&amulet].noted_mana_spent().is_none(),
        "an unresolved activation must not have written a durable note yet"
    );
}

/// "The last noted type" (singular) must REPLACE, not append: two note
/// cycles on the SAME object, each fully resolved before the next begins,
/// must leave exactly one `ChosenAttribute::NotedManaSpent` reflecting only
/// the most recent payment.
#[test]
fn jeweled_amulet_second_note_replaces_not_appends() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let amulet = scenario
        .add_creature_from_oracle(P0, "Jeweled Amulet", 0, 0, JEWELED_AMULET_ORACLE)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    // First note cycle: pay red, resolve fully.
    activate_note_ability(&mut runner, amulet, ManaType::Red);
    assert_eq!(
        runner.state().objects[&amulet].noted_mana_spent(),
        Some([ManaType::Red].as_slice()),
        "reach-guard: the first cycle must have noted red"
    );

    // Remove the charge counter (ability 1) so ability 0 is activatable
    // again, and drain the mana it produces so it doesn't interfere with
    // the pool assertions below.
    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 1,
        })
        .expect("removing the charge counter must succeed");
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .mana
        .clear();
    // Ability 1 taps the amulet as part of its own {T} cost; untap it so the
    // second `activate_note_ability` call can pay ability 0's own {T} cost.
    runner
        .state_mut()
        .objects
        .get_mut(&amulet)
        .expect("amulet must exist")
        .tapped = false;
    assert_eq!(
        charge_counters(&runner, amulet),
        0,
        "reach-guard: ability 0 must be activatable again"
    );

    // Second note cycle: pay green, resolve fully.
    activate_note_ability(&mut runner, amulet, ManaType::Green);

    let noted = runner.state().objects[&amulet].noted_mana_spent();
    assert_eq!(
        noted,
        Some([ManaType::Green].as_slice()),
        "the second note must REPLACE the first, not append to it \
         (got {noted:?})"
    );
}

/// Issue #6504 review: the payment latch this effect reads must be captured
/// PER ACTIVATION, not as a single mutable per-object field a later
/// activation can overwrite. Sequence (all legal under CR 602.5: "Activate
/// only if there are no charge counters" is checked at ACTIVATION time, and
/// no charge counter exists yet while the first note ability is still
/// unresolved on the stack):
///
///   1. Activate ability 0 paying RED. It goes on the stack, unresolved.
///   2. Something untaps the amulet (simulated directly — CR-correctness of
///      the untap effect itself is not what this test is about).
///   3. Activate ability 0 AGAIN, paying GREEN. LIFO: this sits ABOVE the
///      first activation.
///   4. Resolve the top (green activation) first — notes green, first
///      charge counter placed.
///   5. Resolve the remaining (red activation) second — must note RED, its
///      OWN payment, not a bled-through read of the green activation's
///      (later, and by then long-overwritten) payment.
///
/// Revert-probe: reading a shared `GameObject`-level mutable latch at
/// resolution time (rather than a per-activation `ResolvedAbility` snapshot)
/// makes step 5 observe green instead of red, since both activations' cost
/// payments write through the SAME field and the red activation resolves
/// after the field was already overwritten by the green activation's
/// payment.
#[test]
fn jeweled_amulet_lifo_stacked_activations_each_note_their_own_payment() {
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

    // (1) Activate paying red. On the stack, unresolved.
    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 0,
        })
        .expect("first activation (red) must succeed");
    assert_eq!(runner.state().stack.len(), 1);

    // (2) Simulate an external untap effect (e.g. Puppet Strings) resolving
    // in response — CR-correctness of untapping itself is out of scope here.
    runner
        .state_mut()
        .objects
        .get_mut(&amulet)
        .expect("amulet must exist")
        .tapped = false;

    // (3) Activate again, paying green. LIFO: stacks ABOVE the red one.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 0,
        })
        .expect("second activation (green), still no charge counter yet, must succeed");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach-guard: both note activations must be on the stack at once"
    );

    // (4) Resolve the top (green) first.
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner.state().objects[&amulet].noted_mana_spent(),
        Some([ManaType::Green].as_slice()),
        "the green activation must note its own (green) payment"
    );
    assert_eq!(
        charge_counters(&runner, amulet),
        1,
        "reach-guard: the green activation's PutCounter must have resolved"
    );

    // (5) Resolve the remaining (red) activation second.
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&amulet].noted_mana_spent(),
        Some([ManaType::Red].as_slice()),
        "the red activation must note ITS OWN (red) payment, not the \
         green activation's — a shared mutable per-object latch would leak \
         green into this resolution instead"
    );
}

/// CR 707.10: a copy of an activated ability is not itself activated, so it
/// never paid a mana cost. Copies Jeweled Amulet's first ability (through the
/// real `copy_spell::resolve` pipeline — same stack-entry lookup and LIFO
/// resolution a real "copy target activated or triggered ability" card like
/// Lithoform Engine drives) after the ORIGINAL paid red, and proves the copy
/// cannot note red (or anything) even though its `ResolvedAbility` chain was
/// cloned from an original that carried a live `noted_mana_payment`
/// snapshot. Both PutCounter placements still fire unconditionally (CR
/// 602.5/608.2c: "Activate only if..." gates ACTIVATION, never re-checked at
/// resolution, and a copy was never gated by it in the first place), so the
/// counter count alone can't distinguish correct from buggy behavior here —
/// only `noted_mana_spent()` can.
#[test]
fn jeweled_amulet_copied_activation_does_not_note_original_payment() {
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

    // (1) Activate ability 0 paying red. On the stack, unresolved.
    runner
        .act(GameAction::ActivateAbility {
            source_id: amulet,
            ability_index: 0,
        })
        .expect("activating ability 0 must succeed");
    let original_entry_id = runner
        .state()
        .stack
        .back()
        .expect("reach-guard: the activation must be on the stack")
        .id;

    // (2) Copy it — mirrors what Lithoform Engine's "{2}, {T}: Copy target
    // activated or triggered ability you control" drives through the real
    // engine pipeline, minus the copying permanent's own stack presence
    // (the mechanism under test, `copy_spell::resolve` and its
    // `preserve_ability_copy_source_recursive` call, is identical either
    // way — this is the same direct-resolver-call idiom already used above
    // for `bounce::resolve`).
    let copy_ability = ResolvedAbility::new(
        Effect::CopySpell {
            target: TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            },
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        },
        vec![TargetRef::Object(original_entry_id)],
        ObjectId(999),
        P0,
    );
    let mut events = Vec::new();
    copy_spell::resolve(runner.state_mut(), &copy_ability, &mut events)
        .expect("copying the activation must succeed");
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach-guard: original + copy must both be on the stack"
    );

    // (3) LIFO: the copy resolves first.
    runner.resolve_top();
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: exactly the original must remain"
    );
    assert_eq!(
        charge_counters(&runner, amulet),
        1,
        "the copy's PutCounter still fires unconditionally"
    );
    assert!(
        runner.state().objects[&amulet].noted_mana_spent().is_none(),
        "CR 707.10: the copy never paid a mana cost, so its NoteManaSpent \
         must not note the original's (red) payment"
    );

    // (4) The original resolves second — it DID pay, so it must still note
    // correctly. This is the paired positive reach-guard proving the fix
    // clears only the COPY's snapshot, not the original's.
    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        charge_counters(&runner, amulet),
        2,
        "the original's PutCounter also fires (a second charge counter — \
         CR 602.5: the no-counters restriction gates activation, not \
         resolution, and was never re-checked for the copy either)"
    );
    assert_eq!(
        runner.state().objects[&amulet].noted_mana_spent(),
        Some([ManaType::Red].as_slice()),
        "the original activation must still note its own (red) payment"
    );
}
