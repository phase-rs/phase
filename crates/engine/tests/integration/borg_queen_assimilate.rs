//! Borg Queen, Perfection Manifest — the `assimilate` keyword action, driven
//! through the REAL cast pipeline.
//!
//! Oracle (verbatim, MTGJSON-verified):
//!   "Artifact creatures you control get +2/+0.
//!    When Borg Queen enters, assimilate target creature card from an opponent's
//!    graveyard. (Put it onto the battlefield under your control with a +1/+1
//!    counter. It's a Borg artifact creature and loses all other creature types.)"
//!
//! `assimilate` has NO CR 701.x number (the set is unreleased; zero matches in
//! `docs/MagicCompRules.txt`). Its definition lives only in reminder text, which
//! is stripped before the parser runs, so the parser encodes it as a two-step
//! chain: `ChangeZone` (CR 110.2a controller override + CR 122.1 entry counter)
//! with a `Duration::Permanent` `GenericEffect` continuation carrying ONE
//! `StaticDefinition` of four layer-4 modifications (CR 613.1d) implementing
//! CR 205.1b's "[creature type or types] artifact creature" semantics.
//!
//! These are RUNTIME tests: they cast through `GameRunner::cast(..).resolve()`
//! and read back EFFECTIVE post-`evaluate_layers` characteristics. The AST-shape
//! coverage lives in `parser/oracle_effect/tests.rs` — both the positive
//! lowering (`borg_queen_assimilate_lowers_to_reanimate_then_retype_chain`) and
//! the fail-closed negative
//! (`assimilate_without_a_graveyard_target_stays_unimplemented`).
//!
//! FOOT-GUN, load-bearing in every test here: `layers.rs`'s
//! `RemoveAllSubtypes { SubtypeSet::Creature }` arm retains any subtype NOT in
//! `state.all_creature_types`, and `GameScenario` leaves that vector EMPTY. With
//! it empty the subtype wipe is a silent no-op and every "does not contain
//! Human" assertion would pass vacuously. Each test seeds it explicitly.
//!
//! EFFECTIVE-P/T ARITHMETIC (verified, not assumed): Borg Queen's FIRST line is
//! a lord whose `affected` filter is `Typed { type_filters: [Creature, Artifact],
//! controller: You }`, and `type_filters` is CONJUNCTIVE (`filter.rs`
//! `filter_inner`: "Type filters check (all must match — conjunction)"). Because
//! assimilation adds BOTH `Artifact` and `Creature` and the victim enters under
//! Borg Queen's controller, the victim MATCHES that lord and gains +2/+0 in
//! layer 7c (CR 613.4c) on top of the +1/+1 counter. So while Borg Queen is on
//! the battlefield an assimilated victim is `printed + (1,1) + (2,0)`; once she
//! leaves it is `printed + (1,1)`. A layer-4-granted type earning a layer-7c
//! lord buff is exactly the in-tree "artifacts become creatures" precedent.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Borg Queen's verbatim printed Oracle text, reminder text included, so the
/// real reminder-stripping path runs (never a paraphrase).
const BORG_QUEEN: &str = "Artifact creatures you control get +2/+0.\n\
     When Borg Queen enters, assimilate target creature card from an opponent's graveyard. \
     (Put it onto the battlefield under your control with a +1/+1 counter. It's a Borg \
     artifact creature and loses all other creature types.)";

/// Pharika, God of Affliction's verbatim Oracle text. Her SECOND line is the
/// whole point of test 7: a static ability of the permanent ITSELF that removes
/// its `Creature` card type.
const PHARIKA: &str = "Indestructible\n\
     As long as your devotion to black and green is less than seven, Pharika isn't a creature.\n\
     {B}{G}: Exile target creature card from a graveyard. Its owner creates a 1/1 black and \
     green Snake enchantment creature token with deathtouch.";

// ---------------------------------------------------------------------------
// Read-back helpers (effective, post-layer characteristics)
// ---------------------------------------------------------------------------

/// CR 613.1: re-derive every characteristic from the printed base plus all live
/// continuous effects. Called before each read-back so nothing is stale.
fn relayer(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn has_subtype(runner: &GameRunner, id: ObjectId, subtype: &str) -> bool {
    runner.state().objects[&id]
        .card_types
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(subtype))
}

fn has_core_type(runner: &GameRunner, id: ObjectId, core_type: CoreType) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&core_type)
}

fn has_supertype(runner: &GameRunner, id: ObjectId, supertype: Supertype) -> bool {
    runner.state().objects[&id]
        .card_types
        .supertypes
        .contains(&supertype)
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn effective_pt(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Borg Queen in P0's hand at `ManaCost::zero()`.
///
/// The mana cost is deliberately zero: it is not under test, a pool-funded
/// zero-cost cast never surfaces a `ManaPayment` window, and it keeps P0's
/// devotion to black/green at 0 — which test 7's `Not(DevotionGE)` fixture
/// depends on. Her own type line is likewise not set: no assertion in this file
/// reads it, the lord's `affected` filter is evaluated against the OBJECTS it
/// modifies (not against its source's types), and leaving the `Legendary`
/// supertype off keeps CR 704.5j out of test 4's two-Queen fixture.
fn borg_queen_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Borg Queen, Perfection Manifest", 1, 4, BORG_QUEEN)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// The hostile victim for tests 2, 5 and 6: a `Legendary Artifact Enchantment
/// Creature — Human Wizard Equipment`, printed 2/2, in P1's graveyard.
///
/// Every axis of the CR 205.1b table is probed by ONE object: two creature
/// subtypes to replace, one ARTIFACT subtype (`Equipment`) to retain, a
/// non-`Artifact`/non-`Creature` card type (`Enchantment`) to retain, and a
/// supertype (`Legendary`) to retain. `as_artifact()`/`as_enchantment()` each
/// strip `Creature`, so `as_creature()` runs last to restore it.
fn hostile_victim_in_p1_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P1, "Assimilation Test Subject", 2, 2)
        .as_artifact()
        .as_enchantment()
        .as_creature()
        .as_legendary()
        .with_subtypes(vec!["Human", "Wizard", "Equipment"])
        .id()
}

/// Seeds `all_creature_types` for the hostile-victim fixture. `"Equipment"` is
/// deliberately ABSENT — that absence is what makes the non-creature-subtype
/// retention assertion meaningful (CR 205.1b).
fn seed_hostile_creature_types(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = vec![
        "Human".to_string(),
        "Wizard".to_string(),
        "Borg".to_string(),
        "Noble".to_string(),
    ];
}

// ---------------------------------------------------------------------------
// Test 2 — the full CR 205.1b table
// ---------------------------------------------------------------------------

/// CR 205.1b: assimilation ADDS `Artifact` + `Creature`, REPLACES the creature
/// subtype set with exactly `Borg`, and RETAINS every prior card type,
/// supertype, and non-creature subtype. Plus CR 110.2a's controller override,
/// CR 108.3's unchanged ownership, and CR 122.1's entry counter.
///
/// Revert-failing: with the assimilate lowering (PR #7096) reverted the ETB
/// trigger is `Effect::Unimplemented`, so the victim never leaves the graveyard
/// and reach-guard 2.1 fails first.
#[test]
fn assimilate_applies_the_full_cr_205_1b_type_change() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = hostile_victim_in_p1_graveyard(&mut scenario);
    let borg_queen = borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_hostile_creature_types(&mut runner);

    let outcome = runner.cast(borg_queen).target_objects(&[victim]).resolve();

    // 2.1 REACH-GUARD (CR 110.2a). Every negative assertion below is
    // non-vacuous only because this passes.
    outcome.assert_zone(&[victim], Zone::Battlefield);

    // 2.2 CR 110.2a + CR 108.3: controlled by us, still OWNED by the opponent.
    assert_eq!(
        runner.state().objects[&victim].controller,
        P0,
        "CR 110.2a: the assimilated permanent enters under the instruction's controller"
    );
    assert_eq!(
        runner.state().objects[&victim].owner,
        P1,
        "CR 108.3: ownership never changes"
    );

    relayer(&mut runner);

    // 2.3 CR 122.1 + CR 613.4c: exactly one +1/+1 counter, and the effective P/T
    // is printed 2/2 + the counter (1,1) + Borg Queen's own +2/+0 lord — which
    // the victim now matches BECAUSE assimilation made it an artifact creature
    // under her controller.
    assert_eq!(
        plus_one_counters(&runner, victim),
        1,
        "CR 122.1: the victim enters with exactly one +1/+1 counter"
    );
    assert_eq!(
        effective_pt(&runner, victim),
        (5, 3),
        "CR 613.4c: printed 2/2 + (1,1) counter + (2,0) from Borg Queen's artifact-creature lord"
    );

    // 2.4 CR 205.1a: the new creature type is present.
    assert!(
        has_subtype(&runner, victim, "Borg"),
        "CR 205.1a: the assimilated permanent gains the Borg creature type"
    );

    // 2.5 THE DISCRIMINATING ASSERTION (paired with 2.1 and 2.4): CR 205.1b
    // REPLACES the creature types rather than merging them.
    assert!(
        !has_subtype(&runner, victim, "Human"),
        "CR 205.1b: prior creature types are REPLACED, not merged"
    );
    assert!(
        !has_subtype(&runner, victim, "Wizard"),
        "CR 205.1b: prior creature types are REPLACED, not merged"
    );

    // 2.6 CR 205.1b: non-creature subtypes are retained. `Equipment` is an
    // ARTIFACT subtype, and the wipe is scoped to `SubtypeSet::Creature`.
    assert!(
        has_subtype(&runner, victim, "Equipment"),
        "CR 205.1b: non-creature subtypes (Equipment is an artifact subtype) are RETAINED"
    );

    // 2.7 Regression tripwire only. NOT discriminating on this fixture: the
    // victim is a PRINTED artifact AND a printed creature, so this leg passes
    // even with both `AddType`s removed. `AddType{Artifact}` is discriminated by
    // test 4's Goblin victim and by test 7; `AddType{Creature}` only by test 7.
    assert!(has_core_type(&runner, victim, CoreType::Artifact));
    assert!(has_core_type(&runner, victim, CoreType::Creature));

    // 2.8 CR 205.1b: prior card types are RETAINED. This is the assertion a
    // `SetCardTypes` (set-replacement) implementation fails.
    assert!(
        has_core_type(&runner, victim, CoreType::Enchantment),
        "CR 205.1b: prior card types are RETAINED — a SetCardTypes implementation fails here"
    );

    // 2.9 CR 205.1b: supertypes are RETAINED.
    assert!(
        has_supertype(&runner, victim, Supertype::Legendary),
        "CR 205.1b: supertypes are RETAINED"
    );

    // 2.10 CR 613.1: a second layer pass must reproduce the same answer. Every
    // pass resets `obj.card_types` to `base_card_types` and re-applies the live
    // continuous effects, so a one-shot mutation of `card_types` would be erased
    // here.
    relayer(&mut runner);
    assert!(has_subtype(&runner, victim, "Borg"));
    assert!(!has_subtype(&runner, victim, "Human"));
    assert!(!has_subtype(&runner, victim, "Wizard"));
    assert!(has_subtype(&runner, victim, "Equipment"));
    assert!(has_core_type(&runner, victim, CoreType::Artifact));
    assert!(has_core_type(&runner, victim, CoreType::Creature));
    assert!(has_core_type(&runner, victim, CoreType::Enchantment));
    assert!(
        has_supertype(&runner, victim, Supertype::Legendary),
        "CR 613.1: the override is a re-derived layer-4 continuous effect, not a one-shot mutation"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — fail-closed / negative-sibling coverage
// ---------------------------------------------------------------------------

/// 3a. CR 115.2: the filter's `Creature` type leg, POSITIVELY discriminated.
///
/// P1's graveyard holds BOTH an illegal land card and a legal creature card,
/// and the cast declares them IN THAT ORDER — illegal first. The three
/// reachable states are distinct, and the pair below is red in two of them:
///   * CORRECT filter -> the legal set is the singleton {creature}, so
///     `prepare_trigger_targets` (`game/triggers.rs`) auto-assigns it:
///     `ability_utils::auto_select_targets_for_ability` returns the sole
///     assignment and the trigger is `PreparedTriggerTargets::AutoAssigned`.
///     NO prompt is raised, so the declared objects are never consumed here —
///     declaring them is inert in this state, and safe, because nothing checks
///     for unconsumed declarations. The creature enters; the land stays put.
///   * `Creature` leg OVER-matches -> the legal set becomes {creature, land},
///     auto-selection declines (two assignments), a required slot IS created,
///     and `pick_slot_target` (`game/scenario.rs`) fills it with the FIRST
///     DECLARED legal object — the land. BOTH assertions below flip.
///   * `Creature` leg UNDER-matches to empty -> CR 603.3d removes the trigger
///     before any slot exists (`DroppedNoLegalRequiredTarget`), nothing moves,
///     and the POSITIVE leg below fails.
///
/// Declaration order is therefore a REGRESSION-ONLY instrument: it makes the
/// over-match case fail cleanly on assertions instead of on `pick_slot_target`'s
/// no-declared-target panic. There is no nondeterminism to design around — the
/// legal candidate is unique in the passing state, and pinned by declaration
/// order in the over-matching one.
///
/// ISOLATED AXIS: both candidates are cards in the SAME opponent's graveyard, so
/// `Owned { Opponent }` and `InZone { Graveyard }` are held constant and ONLY
/// `type_filters: [Creature]` varies between them. (The ownership leg is
/// isolated by 3b; `InZone { Graveyard }` is isolated by neither.)
///
/// Revert-failing: with the assimilate lowering (PR #7096) reverted the ETB
/// trigger is `Effect::Unimplemented`, nothing leaves the graveyard, and the
/// battlefield assertion fails first.
#[test]
fn assimilate_discriminates_a_creature_card_from_a_land_in_the_same_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Declared FIRST so an over-matching `Creature` leg is forced to consume it.
    let land = scenario.add_land_to_graveyard(P1, "Wastes").id();
    let creature = scenario
        .add_creature_to_graveyard(P1, "Graveyard Wizard", 2, 2)
        .with_subtypes(vec!["Human", "Wizard"])
        .id();
    let borg_queen = borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_hostile_creature_types(&mut runner);

    let outcome = runner
        .cast(borg_queen)
        .target_objects(&[land, creature])
        .resolve();

    // Positive reach-guard: the cast resolved.
    outcome.assert_zone(&[borg_queen], Zone::Battlefield);
    // POSITIVE leg (CR 115.2): the creature card IS a legal `target creature
    // card` and was taken, so the production demonstrably fired on this fixture.
    outcome.assert_zone(&[creature], Zone::Battlefield);
    // NEGATIVE leg, paired with the above: a land card is NOT a legal
    // `target creature card`, so it was skipped despite being declared first.
    outcome.assert_zone(&[land], Zone::Graveyard);
}

/// 3b. CR 108.3: the `Owned { controller: Opponent }` leg, POSITIVELY
/// discriminated, by the same three-state instrument as 3a (see 3a's comment
/// for the auto-assign / over-match / under-match breakdown).
///
/// ISOLATED AXIS: both candidates are CREATURE cards in a GRAVEYARD, so
/// `type_filters: [Creature]` and `InZone { Graveyard }` are held constant and
/// ONLY the owner varies (P0's own graveyard vs P1's). CR 109.4: a graveyard
/// card has no controller, so "an opponent's graveyard" rides as OWNERSHIP.
///
/// Revert-failing (CHANGED by this strengthening): the previous
/// single-candidate form also passed with the assimilate lowering (PR #7096)
/// reverted, because nothing moved at all, so it was only a forward guard. This
/// form asserts that the OPPONENT-owned card reaches the battlefield, so a
/// revert now fails it.
#[test]
fn assimilate_discriminates_an_opponents_graveyard_from_its_own_controllers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Declared FIRST so an over-matching ownership leg is forced to consume it.
    let own_card = scenario
        .add_creature_to_graveyard(P0, "Own Graveyard Wizard", 2, 2)
        .with_subtypes(vec!["Human", "Wizard"])
        .id();
    let opponent_card = scenario
        .add_creature_to_graveyard(P1, "Opponent Graveyard Wizard", 2, 2)
        .with_subtypes(vec!["Human", "Wizard"])
        .id();
    let borg_queen = borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_hostile_creature_types(&mut runner);

    let outcome = runner
        .cast(borg_queen)
        .target_objects(&[own_card, opponent_card])
        .resolve();

    // Positive reach-guard: the cast resolved.
    outcome.assert_zone(&[borg_queen], Zone::Battlefield);
    // POSITIVE leg (CR 108.3): the OPPONENT-owned card is legal and was taken.
    outcome.assert_zone(&[opponent_card], Zone::Battlefield);
    // NEGATIVE leg, paired with the above: P0's OWN card is not legal and was
    // skipped despite being declared first.
    outcome.assert_zone(&[own_card], Zone::Graveyard);
}

// ---------------------------------------------------------------------------
// Test 4 — multi-authority identity binding
// ---------------------------------------------------------------------------

/// CR 611.2c + CR 400.7: two independent assimilations install two transient
/// continuous effects, each frozen to its OWN `SpecificObject { id }`. Neither
/// broadcasts onto the other's victim, and killing one victim prunes only its
/// own effect.
///
/// Victim B is a plain `Goblin` with NO printed `Artifact` — that is what makes
/// 4.3's `Artifact` leg discriminate `AddType { core_type: Artifact }`. Do not
/// make victim B an artifact.
#[test]
fn two_assimilations_bind_to_distinct_objects_and_prune_independently() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim_a = scenario
        .add_creature_to_graveyard(P1, "Graveyard Wizard", 2, 2)
        .with_subtypes(vec!["Human", "Wizard"])
        .id();
    let victim_b = scenario
        .add_creature_to_graveyard(P1, "Graveyard Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let queen_one = borg_queen_in_hand(&mut scenario);
    let queen_two = borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Human".to_string(),
        "Wizard".to_string(),
        "Goblin".to_string(),
        "Borg".to_string(),
        "Noble".to_string(),
    ];

    let first = runner.cast(queen_one).target_objects(&[victim_a]).resolve();
    first.assert_zone(&[victim_a], Zone::Battlefield);
    let second = runner.cast(queen_two).target_objects(&[victim_b]).resolve();
    second.assert_zone(&[victim_b], Zone::Battlefield);

    relayer(&mut runner);

    // 4.1 Both under P0's control (CR 110.2a), each with exactly one counter
    // (CR 122.1).
    assert_eq!(runner.state().objects[&victim_a].controller, P0);
    assert_eq!(runner.state().objects[&victim_b].controller, P0);
    assert_eq!(plus_one_counters(&runner, victim_a), 1);
    assert_eq!(plus_one_counters(&runner, victim_b), 1);

    // 4.2 CR 611.2c: neither victim carries the other's pre-assimilation
    // creature types, so the two effects bound to distinct objects rather than
    // broadcasting.
    assert!(has_subtype(&runner, victim_a, "Borg"));
    assert!(!has_subtype(&runner, victim_a, "Human"));
    assert!(!has_subtype(&runner, victim_a, "Wizard"));
    assert!(
        !has_subtype(&runner, victim_a, "Goblin"),
        "CR 611.2c: victim A must not receive victim B's creature types"
    );
    assert!(has_subtype(&runner, victim_b, "Borg"));
    assert!(!has_subtype(&runner, victim_b, "Goblin"));
    assert!(
        !has_subtype(&runner, victim_b, "Human") && !has_subtype(&runner, victim_b, "Wizard"),
        "CR 611.2c: victim B must not receive victim A's creature types"
    );

    // 4.3 CR 400.7: victim A leaving the battlefield prunes ONLY its own effect.
    // Victim B's `Artifact` here is the DISCRIMINATING leg for
    // `AddType { Artifact }` — victim B is printed as a plain Goblin creature.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), victim_a, Zone::Graveyard, &mut events);
    relayer(&mut runner);
    assert!(
        has_subtype(&runner, victim_b, "Borg"),
        "CR 400.7: pruning victim A's effect must not touch victim B's"
    );
    assert!(
        has_core_type(&runner, victim_b, CoreType::Artifact),
        "AddType{{Artifact}} is the only reason a printed Goblin creature is an artifact"
    );

    // 4.4 CR 400.7: victim A returns as a NEW object with no memory of the
    // override, paired with the positive reach-guard that it really is back on
    // the battlefield.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), victim_a, Zone::Battlefield, &mut events);
    relayer(&mut runner);
    assert_eq!(
        runner.state().objects[&victim_a].zone,
        Zone::Battlefield,
        "reach-guard: victim A is back on the battlefield"
    );
    assert!(
        !has_subtype(&runner, victim_a, "Borg"),
        "CR 400.7: the returned object is a new object and is not Borg"
    );
    assert!(
        has_subtype(&runner, victim_a, "Human"),
        "CR 400.7: the returned object has its printed creature types back"
    );
}

// ---------------------------------------------------------------------------
// Test 5 — source independence
// ---------------------------------------------------------------------------

/// CR 611.2a: a continuous effect created by a resolving ability with no stated
/// duration lasts until the end of the game and is INDEPENDENT of its source.
/// Destroying Borg Queen must not end the type override.
///
/// The DISCRIMINATING legs are `Borg` present and `Human`/`Wizard` absent — the
/// `Artifact`/`Creature` legs would pass on this printed-artifact-creature
/// fixture even with both `AddType`s removed (see 2.7). This test also proves
/// independently that the TYPE override is source-independent while the lord
/// BUFF correctly is not: the +2/+0 disappears with her.
///
/// Guards against a future "simplification" of the duration to
/// `UntilHostLeavesPlay`, which `transient_effect_is_live` would then prune.
#[test]
fn assimilated_permanent_keeps_its_types_after_borg_queen_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = hostile_victim_in_p1_graveyard(&mut scenario);
    let borg_queen = borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_hostile_creature_types(&mut runner);

    let outcome = runner.cast(borg_queen).target_objects(&[victim]).resolve();
    outcome.assert_zone(&[victim], Zone::Battlefield);

    // Baseline WITH Borg Queen present: printed 2/2 + (1,1) counter + (2,0) lord.
    relayer(&mut runner);
    assert_eq!(effective_pt(&runner, victim), (5, 3));

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), borg_queen, Zone::Graveyard, &mut events);
    relayer(&mut runner);

    assert_eq!(
        runner.state().objects[&borg_queen].zone,
        Zone::Graveyard,
        "reach-guard: Borg Queen really left the battlefield"
    );

    // CR 611.2a: the type override survives its source.
    assert!(
        has_subtype(&runner, victim, "Borg"),
        "CR 611.2a: the override is independent of its source once created"
    );
    assert!(!has_subtype(&runner, victim, "Human"));
    assert!(!has_subtype(&runner, victim, "Wizard"));
    assert!(has_core_type(&runner, victim, CoreType::Artifact));
    assert!(has_core_type(&runner, victim, CoreType::Creature));

    // CR 611.3b (a continuous effect from a STATIC ability applies only while the
    // permanent generating it is on the battlefield): her LORD, by contrast, stops
    // applying — printed 2/2 + (1,1) counter only, with NO +2/+0.
    assert_eq!(
        effective_pt(&runner, victim),
        (3, 3),
        "Borg Queen's static lord ends with her; only the counter remains"
    );
}

// ---------------------------------------------------------------------------
// Test 6 — the override survives cleanup into the next turn
// ---------------------------------------------------------------------------

/// CR 611.2a: `Duration::Permanent`, exercised through the PRODUCTION end-of-turn
/// prune. This is the ONLY runtime test that can catch a missing
/// `Duration::Permanent`.
///
/// The failure mode: `game/effects/effect.rs`'s
/// `ability.duration.or(duration).unwrap_or(Duration::UntilEndOfTurn)` silently
/// degrades an unset duration to `UntilEndOfTurn`, and
/// `layers.rs::prune_end_of_turn_effects` (reached from `turns.rs`) DROPS exactly
/// those at cleanup. Tests 2-5 all run inside one turn, so none of them observes
/// the prune. The shape assertion in the parser SHAPE test passes by
/// construction and is not a substitute for this.
#[test]
fn assimilated_types_survive_cleanup_into_the_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = hostile_victim_in_p1_graveyard(&mut scenario);
    let borg_queen = borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_hostile_creature_types(&mut runner);

    let outcome = runner.cast(borg_queen).target_objects(&[victim]).resolve();
    // Confirmed BEFORE advancing — this is what makes the post-cleanup
    // assertions non-vacuous.
    outcome.assert_zone(&[victim], Zone::Battlefield);
    relayer(&mut runner);
    assert!(
        has_subtype(&runner, victim, "Borg"),
        "baseline: the override is in place before the turn advances"
    );

    let turn_before = runner.state().turn_number;

    // Cross the end step AND the cleanup step into the next turn, running the
    // real `prune_end_of_turn_effects` call site rather than invoking it by hand.
    runner.advance_to_upkeep();

    // 6.2 POSITIVE VACUITY GUARD: the turn really advanced past cleanup, so a
    // no-op advance cannot make the assertions below pass for free.
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "the advance must land in the next turn's upkeep step"
    );
    assert!(
        runner.state().turn_number > turn_before,
        "the turn counter must have advanced past cleanup, was {turn_before}"
    );

    relayer(&mut runner);

    // 6.1 REACH-GUARD: the victim did not die to an unrelated state-based action.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "CR 400.7: reach-guard — the victim is still on the battlefield"
    );

    // 6.3 / 6.4 CR 205.1a + CR 205.1b: these fail if — and among these tests only
    // if — the installed effect's duration degraded to `UntilEndOfTurn`.
    assert!(
        has_subtype(&runner, victim, "Borg"),
        "CR 611.2a: Duration::Permanent must survive the cleanup-step prune"
    );
    assert!(!has_subtype(&runner, victim, "Human"));
    assert!(!has_subtype(&runner, victim, "Wizard"));

    // 6.5 The discriminating legs here are `Enchantment` and `Legendary`
    // retention; the `Artifact`+`Creature` legs inherit 2.7's printed types.
    assert!(has_core_type(&runner, victim, CoreType::Artifact));
    assert!(has_core_type(&runner, victim, CoreType::Creature));
    assert!(has_core_type(&runner, victim, CoreType::Enchantment));
    assert!(has_supertype(&runner, victim, Supertype::Legendary));

    // 6.6 CR 122.1: a counter is not a duration-bound effect. This separates a
    // counter regression from a duration regression.
    assert_eq!(
        plus_one_counters(&runner, victim),
        1,
        "CR 122.1: the +1/+1 counter is not duration-bound"
    );
}

// ---------------------------------------------------------------------------
// Test 7 — the AddType modifications are load-bearing
// ---------------------------------------------------------------------------

/// CR 613.7n: when a resolving ability both puts a permanent onto the
/// battlefield and sets its characteristics (CR 611.2e's paradigm case), the
/// continuous effect from the permanent's OWN static ability receives an EARLIER
/// relative timestamp. So a victim whose own static removes its `Creature` card
/// type applies FIRST, and our later `AddType { Creature }` must restore it
/// (CR 613.7). The engine implements this by construction — the entry timestamp
/// is drawn before the resolution's transient effect.
///
/// This is the ONLY test that discriminates `AddType { Creature }`, and the
/// second that discriminates `AddType { Artifact }` (Pharika has no printed
/// `Artifact`).
///
/// PRE-COMMITMENT — do not resolve a failure here by weakening the test:
///   * If 7.1 and 7.2 pass but the `Creature` assertion FAILS, the expected value
///     is `Creature` PRESENT. On a faithful implementation that is a finding
///     about the engine's layer-4 ordering (CR 613.7n) — surface it. Never delete
///     the assertion, and never delete `AddType { Creature }` from the lowering.
///   * If 7.2 fails (the control Pharika IS a creature), the FIXTURE is at fault:
///     the devotion condition is not firing. Fix the fixture (fewer black/green
///     pips, or switch to `Erebos, God of the Dead` at devotion-to-black < 5),
///     never the guard. A test with 7.2 failing is VACUOUS.
///   * Effective P/T is 5/5 printed + (1,1) counter + (2,0) lord = 8/6. Observing
///     6/6 means an `AddType` did not apply (so the victim missed the
///     artifact-creature lord) — a bug in the implementation, NOT in the layer
///     engine.
#[test]
fn assimilate_restores_a_creature_type_its_victims_own_static_removes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Victim: Pharika in P1's graveyard. Identity setters FIRST, Oracle text
    // LAST — `from_oracle_text` overwrites abilities/statics but preserves
    // identity (name, P/T, card_types, mana cost). Deliberately NOT an artifact:
    // the absence of a printed `Artifact` is what makes the Artifact assertion
    // discriminating.
    let victim = scenario
        .add_creature_to_graveyard(P1, "Pharika, God of Affliction", 5, 5)
        .as_enchantment()
        .as_creature()
        .as_legendary()
        .with_subtypes(vec!["God"])
        .from_oracle_text(PHARIKA)
        .id();

    // CONTROL COPY (the vacuity guard): a second Pharika already on the
    // battlefield under P1. Legal alongside the assimilated copy because
    // CR 704.5j is PER-PLAYER — after resolution the two are controlled by
    // different players, so the legend rule does not fire. Built with the SAME
    // identity-first / Oracle-last order as the victim so the two copies cannot
    // parse differently.
    let control = scenario
        .add_creature(P1, "Pharika, God of Affliction", 5, 5)
        .as_enchantment()
        .as_creature()
        .as_legendary()
        .with_subtypes(vec!["God"])
        .from_oracle_text(PHARIKA)
        .id();

    let borg_queen = borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    // Same mandatory foot-gun as test 2: without "God" here the subtype wipe is a
    // no-op and 7.7 would pass vacuously.
    runner.state_mut().all_creature_types =
        vec!["God".to_string(), "Borg".to_string(), "Noble".to_string()];

    let outcome = runner.cast(borg_queen).target_objects(&[victim]).resolve();

    // 7.1 REACH-GUARD (CR 110.2a).
    outcome.assert_zone(&[victim], Zone::Battlefield);
    assert_eq!(runner.state().objects[&victim].controller, P0);

    relayer(&mut runner);

    // 7.2 VACUITY GUARD (CR 613.1d + CR 613.7a): the un-assimilated control
    // Pharika is NOT a creature, which proves the `Not(DevotionGE)` static is
    // live in this game state — so 7.3 below is a genuine override and not a
    // tautology.
    assert!(
        !has_core_type(&runner, control, CoreType::Creature),
        "VACUITY GUARD: the control Pharika's own static must be removing Creature \
         in this state; if it is not, fix the FIXTURE (devotion pips), not the assertion"
    );

    // 7.3 CR 205.1b + CR 613.7n + CR 613.7: the ONLY assertion that discriminates
    // `AddType { core_type: Creature }`.
    assert!(
        has_core_type(&runner, victim, CoreType::Creature),
        "CR 613.7n: the victim's own earlier-timestamped static removes Creature, so \
         AddType{{Creature}} at the later timestamp must restore it"
    );

    // 7.4 CR 205.1b: discriminates `AddType { core_type: Artifact }` — Pharika has
    // no printed `Artifact` card type.
    assert!(
        has_core_type(&runner, victim, CoreType::Artifact),
        "CR 205.1b: AddType{{Artifact}} is the only reason Pharika is an artifact"
    );

    // 7.5 CR 205.1b: prior card types retained on a two-card-type fixture — the
    // assertion a `SetCardTypes` implementation fails.
    assert!(
        has_core_type(&runner, victim, CoreType::Enchantment),
        "CR 205.1b: Enchantment is RETAINED"
    );

    // 7.6 CR 205.1b: supertype retained.
    assert!(has_supertype(&runner, victim, Supertype::Legendary));

    // 7.7 CR 205.1a + CR 205.1b: creature-type replacement.
    assert!(has_subtype(&runner, victim, "Borg"));
    assert!(
        !has_subtype(&runner, victim, "God"),
        "CR 205.1b: God is replaced, not merged"
    );

    // 7.8 CR 122.1 + CR 613.4c: 5/5 printed + (1,1) counter + (2,0) from Borg
    // Queen's artifact-creature lord. This is a STRONGER discriminator than the
    // bare counter arithmetic: reaching 8/6 requires BOTH `AddType`s to have
    // landed, because the lord's conjunctive `[Creature, Artifact]` filter only
    // matches once the victim is both. A 6/6 here means an `AddType` was dropped.
    assert_eq!(
        plus_one_counters(&runner, victim),
        1,
        "CR 122.1: exactly one +1/+1 counter"
    );
    assert_eq!(
        effective_pt(&runner, victim),
        (8, 6),
        "CR 613.4c: 5/5 + (1,1) counter + (2,0) lord — the lord applies only because \
         BOTH AddTypes landed"
    );
}
