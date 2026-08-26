//! CR 604.2 + CR 611.3b vs CR 611.2a + CR 514.2 — the lifetime of a
//! damage-prevention shield is decided by its PROVENANCE, and the engine records
//! that provenance exactly once, at creation, in `ReplacementDefinition::expiry`.
//!
//! A printed static ability's prevention effect (Solitary Confinement, Nine
//! Lives, Fog Bank, Pariah, ...) is active "as long as the permanent with the
//! ability remains on the battlefield" (CR 604.2) — it has no turn window and
//! carries `expiry: None`. A prevention effect created by the RESOLUTION of a
//! spell or ability lasts "as long as stated by the spell or ability creating
//! it" (CR 611.2a), and its creator stamps that window.
//!
//! `turns::execute_cleanup` previously keyed its CR 514.2 prune on
//! `ShieldKind::is_shield()`, which is TRUE for both classes — so every printed
//! prevention card lost its shield at the first cleanup step and was dead for the
//! rest of the game. Every test in this file therefore CROSSES A TURN BOUNDARY: a
//! same-turn test passes with the bug present and proves nothing.
//!
//! Positive half (must survive): T1, T5.
//! Negative half (must still expire): T2 (ability-duration carrier), T4 (one-shot),
//! T7 (no window on either carrier — the engine's turn default), T8's step 5.
//! T8 is the "longer than a turn, but not forever" middle case.
//!
//! Oracle text is verbatim from Scryfall; the harness rules this file obeys
//! (stocked libraries, `active_player` reach-guards after every crossing, the
//! alternating End/Upkeep crossing idiom, and `#[must_use]` combat reach-guards)
//! are documented at their helpers below.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityKind, CombatDamageScope, PreventionAmount, ReplacementDefinition, RestrictionExpiry,
    ShieldKind, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Verbatim Solitary Confinement — the reported bug. Printed static, no window.
const SOLITARY_CONFINEMENT_TEXT: &str = "Skip your draw step.\nAt the beginning of your upkeep, sacrifice Solitary Confinement unless you discard a card.\nPrevent all damage that would be dealt to you.";

/// Verbatim Fog — a resolution-created shield whose window rides on the
/// ability's own `duration`.
const FOG_TEXT: &str = "Prevent all combat damage that would be dealt this turn.";

/// Verbatim Fog Bank — a printed static contributing TWO definitions
/// ("dealt to" and "dealt by").
const FOG_BANK_TEXT: &str = "Defender (This creature can't attack.)\nFlying\nPrevent all combat damage that would be dealt to and dealt by this creature.";

/// Verbatim Reverse Damage — the duration-less resolution class: no window on
/// `prevention_duration` AND none on `ability.duration`.
const REVERSE_DAMAGE_TEXT: &str = "The next time a source of your choice would deal damage to you this turn, prevent that damage. You gain life equal to the damage prevented this way.";

/// Verbatim Morningtide's Light — the ability-duration carrier ("Until your next
/// turn"), on a Sorcery that exiles itself so its shield lands on the
/// layer-stable pending registry.
const MORNINGTIDES_LIGHT_TEXT: &str = "Exile any number of target creatures. At the beginning of the next end step, return those cards to the battlefield tapped under their owners' control.\nUntil your next turn, prevent all damage that would be dealt to you.\nExile Morningtide's Light.";

/// Verbatim Sewers of Estark — the corpus's only `prevention_duration:
/// UntilEndOfCombat` producer, and structurally combat-gated.
const SEWERS_OF_ESTARK_TEXT: &str = "Choose target creature. If it's attacking, it can't be blocked this turn. If it's blocking, prevent all combat damage that would be dealt this combat by it and each creature it's blocking.";

/// Verbatim Awe Strike — a one-shot prevention shield, deliberately never
/// consumed in T4.
const AWE_STRIKE_TEXT: &str = "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

/// A permanent-type seed MUST be applied before the Oracle text: the ability
/// parse runs inside `from_oracle_text`, and `parse_oracle_text` given
/// `types: ["Sorcery"]` returns zero replacements for a static prevention line.
/// Copied from `statecraft_damage_prevention.rs`.
fn add_enchantment_spell_to_hand(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    oracle_text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand(player, name, false)
        .as_enchantment()
        .from_oracle_text(oracle_text)
        .with_mana_cost(free_cost())
        .id()
}

/// Stock both libraries. Without this a player decks out on the far side of a
/// boundary, the game ends in `WaitingFor::GameOver`, combat never runs, and
/// EVERY life assertion in the test passes vacuously.
fn stock_libraries(scenario: &mut GameScenario) {
    scenario.with_library_top(
        P0,
        &["F0a", "F0b", "F0c", "F0d", "F0e", "F0f", "F0g", "F0h"],
    );
    scenario.with_library_top(
        P1,
        &["F1a", "F1b", "F1c", "F1d", "F1e", "F1f", "F1g", "F1h"],
    );
}

/// Cross exactly one turn boundary.
///
/// Two measured harness hazards make this a named helper rather than an inline
/// call: `advance_to_phase(Phase::Upkeep)` twice in a row does NOT advance a
/// second turn, and `advance_to_phase` STALLS SILENTLY (no panic, no error) when
/// a `WaitingFor` needs an action. Every caller therefore asserts
/// `active_player` afterwards — that assertion is the stall guard.
fn cross_boundary(runner: &mut GameRunner) {
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::Upkeep);
}

/// Drive combat from the current state through end of combat, declaring
/// `attacker` for `attacker_player` against `defend_player` and, if given,
/// `blocker` for the defender. Copied verbatim in shape from
/// `statecraft_damage_prevention.rs::run_combat`.
///
/// The return value is the combat reach-guard: every prevention assertion in
/// this file reads "life unchanged", which is also what "combat never happened"
/// looks like. Callers MUST assert it `== true`.
#[must_use = "combat must be asserted to have actually run — see doc comment"]
fn run_combat(
    runner: &mut GameRunner,
    attacker_player: PlayerId,
    attacker: ObjectId,
    defend_player: PlayerId,
    blocker: Option<ObjectId>,
) -> bool {
    let mut attacked = false;
    let mut blocked = false;
    let mut reached_end_of_combat = false;

    for _ in 0..400 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            reached_end_of_combat = true;
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. }
                if player == attacker_player && !attacked =>
            {
                attacked = true;
                runner
                    .declare_attackers(&[(attacker, AttackTarget::Player(defend_player))])
                    .expect("declaring the intended attacker must succeed");
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { player, .. } if player == defend_player && !blocked => {
                blocked = true;
                let blocks = if let Some(blk) = blocker {
                    vec![(blk, attacker)]
                } else {
                    vec![]
                };
                runner
                    .declare_blockers(&blocks)
                    .expect("declaring the intended blocker must succeed");
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    attacked && (blocker.is_none() || blocked) && reached_end_of_combat
}

// ---------------------------------------------------------------------------
// T1 / T1b — the headline positive, and its unshielded control.
// ---------------------------------------------------------------------------

/// **T1.** CR 604.2 + CR 611.3b: Solitary Confinement's prevention effect is
/// created by a printed STATIC ability, so it is active for as long as the
/// enchantment remains on the battlefield. CR 514.2 ends "until end of turn" and
/// "this turn" effects — this is neither, so the cleanup step must not touch it.
///
/// Reverting `turns::execute_cleanup`'s predicate to read
/// `ShieldKind::is_shield()` flips BOTH the structural assertion
/// (`replacement_definitions.len() == 1` becomes `0`) and the behavioral one
/// (life 20 becomes 17).
#[test]
fn printed_prevention_survives_turn_boundary_and_prevents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let sc = add_enchantment_spell_to_hand(
        &mut scenario,
        P0,
        "Solitary Confinement",
        SOLITARY_CONFINEMENT_TEXT,
    );
    let bear = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(sc).resolve();
    outcome.assert_zone(&[sc], Zone::Battlefield);

    // Reach-guard: the printed shield really was installed by the cast pipeline.
    let defs = &runner.state().objects[&sc].replacement_definitions;
    assert_eq!(defs.len(), 1, "printed shield must be installed on cast");
    assert_eq!(
        defs[0].shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        }
    );
    assert_eq!(
        defs[0].expiry, None,
        "CR 604.2: a printed static's shield states no window"
    );

    let life_before = runner.state().players[0].life;
    cross_boundary(&mut runner);
    assert_eq!(
        runner.state().active_player,
        P1,
        "scenario must have advanced into P1's turn"
    );

    // The defect: today the enchantment is still on the battlefield but its
    // shield has been deleted from both the live and base definition lists.
    assert_eq!(
        runner.state().objects[&sc].zone,
        Zone::Battlefield,
        "the enchantment itself never left"
    );
    assert_eq!(
        runner.state().objects[&sc].replacement_definitions.len(),
        1,
        "CR 604.2: the printed shield must survive the cleanup step"
    );
    assert_eq!(
        runner.state().objects[&sc].replacement_definitions[0].expiry,
        None
    );

    assert!(
        run_combat(&mut runner, P1, bear, P0, None),
        "combat reach-guard: the attack must actually have happened"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "CR 604.2 + CR 611.3b: the printed shield must still prevent next turn"
    );
}

/// **T1b.** The paired control for T1: with no enchantment at all, the identical
/// cross-boundary attack DOES deal its 3 damage. Without this, T1's "life
/// unchanged" could pass because combat silently failed to run.
#[test]
fn unblocked_attacker_damages_an_unshielded_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let bear = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    let life_before = runner.state().players[0].life;
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P1);

    assert!(
        run_combat(&mut runner, P1, bear, P0, None),
        "combat reach-guard"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before - 3,
        "the harness can deal cross-boundary combat damage"
    );
}

// ---------------------------------------------------------------------------
// T2 — the anti-over-fix guard.
// ---------------------------------------------------------------------------

/// **T2.** CR 611.2a + CR 514.2: Fog's window rides on its ability's own
/// `duration` ("this turn"), so its shield MUST still die at cleanup. This is the
/// test that makes the naive one-line "just delete the `is_shield()` disjunct"
/// fix unshippable: with no creation-seam stamp at all, Fog's shield survives the
/// boundary, P0 takes 0, and this test goes red.
///
/// Honest scope: Fog is satisfied by EITHER the `.or_else` ability-duration
/// carrier or the engine's turn default, so it discriminates neither
/// individually. T7 and T8 are the discriminating tests for those.
#[test]
fn fog_prevention_shield_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let fog = scenario
        .add_spell_to_hand_from_oracle(P0, "Fog", true, FOG_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let p0_bear = scenario.add_creature(P0, "Grizzly Bears", 3, 3).id();
    let p1_bear = scenario.add_creature(P1, "Runeclaw Bear", 3, 3).id();
    let mut runner = scenario.build();

    runner.cast(fog).resolve();

    // Reach-guard: the parse reached the resolver and a shield was created.
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "Fog's shield must land on the pending registry"
    );

    // Positive half, same turn: the shield is live and doing work.
    let p1_life_before = runner.state().players[1].life;
    assert!(
        run_combat(&mut runner, P0, p0_bear, P1, None),
        "combat reach-guard (same turn)"
    );
    assert_eq!(
        runner.state().players[1].life,
        p1_life_before,
        "Fog must prevent combat damage during its own turn"
    );

    let p0_life_before = runner.state().players[0].life;
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P1);

    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "CR 514.2: Fog's 'this turn' shield must be pruned at cleanup"
    );
    assert!(
        run_combat(&mut runner, P1, p1_bear, P0, None),
        "combat reach-guard (next turn)"
    );
    assert_eq!(
        runner.state().players[0].life,
        p0_life_before - 3,
        "CR 514.2: damage must land once Fog's window has ended"
    );
}

// ---------------------------------------------------------------------------
// T4 — one-shot prevention, never consumed.
// ---------------------------------------------------------------------------

/// **T4 — a REGRESSION GUARD, not a discriminating test.** Awe Strike already
/// carries `prevention_duration: UntilEndOfTurn`, so its shield is stamped
/// `Some(EndOfTurn)` by the existing effect-level carrier with or without
/// `ReplacementDefinition::prevention_oneshot_shield`'s builder stamp — deleting
/// that stamp leaves this test green. Stated up front so no reader over-reads it.
///
/// CR 615.3 + CR 615.8 + CR 514.2: a "the next time ... this turn" shield that is
/// never used up still ends at cleanup.
#[test]
fn oneshot_prevention_shield_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let awe = scenario
        .add_spell_to_hand_from_oracle(P0, "Awe Strike", true, AWE_STRIKE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    runner.cast(awe).target_object(bear).resolve();

    // Reach-guard. The shield's host surface is the PENDING REGISTRY, not the
    // targeted creature — asserting against `bear` here would be vacuous.
    let pending = &runner.state().pending_damage_replacements;
    assert_eq!(
        pending.len(),
        1,
        "Awe Strike's shield must exist pre-boundary"
    );
    assert_eq!(pending[0].shield_kind, ShieldKind::PreventionOneShot);
    assert!(
        pending[0].consume_on_apply,
        "CR 615.8: 'the next time' is consumed on apply"
    );
    assert_eq!(
        pending[0].expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "CR 615.3 + CR 514.2: the one-shot's window is stamped at creation"
    );

    let life_before = runner.state().players[0].life;
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P1);

    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "CR 514.2: an unconsumed one-shot shield still expires at cleanup"
    );
    assert!(
        run_combat(&mut runner, P1, bear, P0, None),
        "combat reach-guard"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before - 3,
        "the shielded creature's damage must land next turn"
    );
}

// ---------------------------------------------------------------------------
// T3b — an `EndOfCombat` prevention shield can be produced at all, and works.
// ---------------------------------------------------------------------------

/// Declare `attacker` and `blocker`, then STOP at the first priority window of
/// the declare-blockers step so the caller can cast at instant speed into it.
///
/// Returns whether both declarations actually happened — the same
/// reach-guard contract as `run_combat`.
#[must_use = "the declarations must be asserted to have actually happened"]
fn declare_attack_and_block(
    runner: &mut GameRunner,
    attacker_player: PlayerId,
    attacker: ObjectId,
    defend_player: PlayerId,
    blocker: ObjectId,
) -> bool {
    let mut attacked = false;
    let mut blocked = false;
    for _ in 0..400 {
        if attacked && blocked {
            return matches!(runner.state().waiting_for, WaitingFor::Priority { .. });
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. }
                if player == attacker_player && !attacked =>
            {
                attacked = true;
                runner
                    .declare_attackers(&[(attacker, AttackTarget::Player(defend_player))])
                    .expect("declaring the intended attacker must succeed");
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { player, .. } if player == defend_player && !blocked => {
                blocked = true;
                runner
                    .declare_blockers(&[(blocker, attacker)])
                    .expect("declaring the intended blocker must succeed");
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    false
}

/// Pass priority from mid-combat through the end of the combat phase.
#[must_use = "combat must be asserted to have actually completed"]
fn finish_combat(runner: &mut GameRunner) -> bool {
    for _ in 0..400 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            return true;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
    false
}

/// **T3b — the production reach-guard for the `EndOfCombat` value.** Sewers of
/// Estark is the corpus's only card whose prevention window rides on
/// `prevention_duration: UntilEndOfCombat`, and its "If it's blocking" gate makes
/// it structurally impossible to create outside combat. This test proves an
/// `EndOfCombat` prevention shield can exist at all AND does work, so
/// `turns.rs::cleanup_expires_end_of_combat_prevention_shield` is not guarding an
/// impossible value.
///
/// Deliberately asserts NO player's life: the attacker is blocked, so both
/// players read 20 with and without the shield. The discriminating observable is
/// the BLOCKER's survival — Sewers prevents damage dealt to and by the blocking
/// creature, so the 2/2 lives through the 3/3. See the paired control below.
#[test]
fn sewers_of_estark_stamps_end_of_combat_on_the_blocking_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let sewers = scenario
        .add_spell_to_hand_from_oracle(P0, "Sewers of Estark", true, SEWERS_OF_ESTARK_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let attacker = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    assert!(
        declare_attack_and_block(&mut runner, P0, attacker, P1, blocker),
        "reach-guard: attacker and blocker must both have been declared"
    );

    runner.cast(sewers).target_object(blocker).resolve();

    // Structural: the shield is installed on the BLOCKER's own definitions.
    let defs = &runner.state().objects[&blocker].replacement_definitions;
    assert_eq!(
        defs.len(),
        1,
        "the shield must land on the blocking creature"
    );
    assert_eq!(
        defs[0].expiry,
        Some(RestrictionExpiry::EndOfCombat),
        "CR 511.2: an 'until end of combat' window maps to RestrictionExpiry::EndOfCombat"
    );
    assert_eq!(defs[0].valid_card, Some(TargetFilter::SelfRef));
    assert_eq!(defs[0].combat_scope, Some(CombatDamageScope::CombatOnly));

    assert!(finish_combat(&mut runner), "combat reach-guard");

    // Behavioral: the shielded 2/2 survives the 3/3 it blocked.
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "the shielded blocker must survive combat damage"
    );

    // Pre-existing behavior: the window ends with the combat phase.
    assert!(
        runner.state().objects[&blocker]
            .replacement_definitions
            .as_slice()
            .is_empty(),
        "CR 511.2: effects that last 'until end of combat' expire at the end of the combat phase"
    );
}

/// The paired control for T3b: without the Sewers cast, the identical 2/2 blocker
/// dies to the identical 3/3. This is what makes T3b's survival assertion
/// non-vacuous.
#[test]
fn blocked_creature_dies_without_the_sewers_shield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let attacker = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    assert!(
        run_combat(&mut runner, P0, attacker, P1, Some(blocker)),
        "combat reach-guard"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Graveyard,
        "an unshielded 2/2 dies to a 3/3 it blocked"
    );
}

/// The condition-gate sibling: cast at a NON-blocking creature in the precombat
/// main phase, Sewers of Estark creates no shield at all. This is why the
/// `EndOfCombat` cleanup arm has no reachable production path today and is tested
/// at unit level in `turns.rs` instead.
#[test]
fn sewers_of_estark_creates_no_shield_outside_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let sewers = scenario
        .add_spell_to_hand_from_oracle(P0, "Sewers of Estark", true, SEWERS_OF_ESTARK_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(sewers).target_object(bear).resolve();
    // Reach-guard: the spell genuinely resolved rather than being stuck on the
    // stack or fizzling on an illegal target.
    outcome.assert_zone(&[sewers], Zone::Graveyard);
    assert!(
        runner.state().objects[&bear]
            .replacement_definitions
            .as_slice()
            .is_empty(),
        "the 'if it's blocking' condition gate must refuse outside combat"
    );
    assert!(runner.state().pending_damage_replacements.is_empty());
}

// ---------------------------------------------------------------------------
// T5 — the multi-authority hostile fixture.
// ---------------------------------------------------------------------------

/// **T5.** One host object carrying TWO printed `expiry: None` definitions (Fog
/// Bank's single sentence compiles to "dealt to" AND "dealt by") plus a staged
/// resolution-shaped `Some(EndOfTurn)` definition of the IDENTICAL
/// `ShieldKind::Prevention { All }` value. They are indistinguishable by kind and
/// distinguishable only by the latched `expiry`, so this proves the binding is
/// per-definition and latched at creation rather than per-object or re-derived at
/// prune time.
#[test]
fn printed_and_turn_bound_shields_on_one_host_part_ways_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let fog_bank = scenario
        .add_creature_from_oracle(P0, "Fog Bank", 0, 2, FOG_BANK_TEXT)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .valid_card(TargetFilter::SelfRef)
                .prevention_shield(PreventionAmount::All)
                .expiry(RestrictionExpiry::EndOfTurn),
        )
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    // Reach-guard: 2 printed + 1 staged. Fog Bank contributes TWO definitions.
    assert_eq!(
        runner.state().objects[&fog_bank]
            .replacement_definitions
            .len(),
        3,
        "two printed Fog Bank definitions plus the staged turn-bound one"
    );

    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P1);

    let defs = &runner.state().objects[&fog_bank].replacement_definitions;
    assert_eq!(
        defs.len(),
        2,
        "CR 604.2 vs CR 514.2: exactly the two printed definitions survive"
    );
    assert!(
        defs.as_slice().iter().all(|d| d.expiry.is_none()),
        "the survivors are the ones with no stated window"
    );

    // Fog Bank still does its job in the new turn: it blocks the 3/3 and takes
    // no damage, and deals none.
    assert!(
        run_combat(&mut runner, P1, bear, P0, Some(fog_bank)),
        "combat reach-guard (blocked)"
    );
    assert_eq!(
        runner.state().objects[&fog_bank].zone,
        Zone::Battlefield,
        "CR 604.2: Fog Bank's printed prevention must still work next turn"
    );
    assert_eq!(
        runner.state().objects[&fog_bank].damage_marked,
        0,
        "no combat damage may be marked on Fog Bank"
    );
}

// ---------------------------------------------------------------------------
// T7 — the duration-less resolution class (the engine's turn default).
// ---------------------------------------------------------------------------

/// **T7.** Reverse Damage reaches `prevent_damage::resolve` with NO window on
/// either carrier — `prevention_duration: None` and `ability.duration: None` —
/// because the parser drops its printed "this turn". It is the discriminating
/// test for `ReplacementDefinition::with_resolution_shield_expiry`'s engine
/// default: delete that one line and this class of shields becomes immortal while
/// every other test in this file stays green (Fog and Morningtide's Light both
/// carry a window on `ability.duration`; Awe Strike carries one on
/// `prevention_duration`).
///
/// The shield is deliberately hosted on the layer-stable pending registry (the
/// spell exiles itself to the graveyard), and the card is chosen over Circle of
/// Protection: Red, whose object-hosted shield is destroyed by the next layer
/// pass long before cleanup runs.
#[test]
fn reverse_damage_shield_expires_at_cleanup_with_no_duration_on_either_carrier() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let rd = scenario
        .add_spell_to_hand_from_oracle(P0, "Reverse Damage", true, REVERSE_DAMAGE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    // No colour requirement: Reverse Damage's prompt is "a source of your
    // choice" with no restriction. A plain vanilla creature is the right fixture.
    let bear = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    // Reach-guard, pre-cast.
    assert!(runner.state().pending_damage_replacements.is_empty());

    // Cast at instant speed on P1's turn so the shield's own turn is P1's.
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P1);
    // No mana re-seed is needed: the fixture's Reverse Damage carries a free
    // mana cost, so the empty post-boundary pool cannot block the cast.
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 must hold priority to cast at instant speed on P1's turn"
    );
    runner.cast(rd).resolve();

    // Resolution parks on the CR 609.7a source choice — drive the round-trip.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ),
        "reach-guard: the ChosenDamageSource branch under test must be reached"
    );
    runner
        .act(GameAction::ChooseDamageSource { source: bear })
        .expect("choosing the damage source must succeed");

    let pending = &runner.state().pending_damage_replacements;
    assert_eq!(pending.len(), 1, "the shield must exist after the choice");
    assert_eq!(
        pending[0].shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        }
    );
    assert_eq!(
        pending[0].damage_source_filter,
        Some(TargetFilter::SpecificObject { id: bear }),
        "reach-guard: the shield is genuinely scoped to the chosen source"
    );
    // THE REVERT-FAILING ASSERTION. Measured `None` before the fix.
    assert_eq!(
        pending[0].expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "the engine turn default must stamp a shield with no window on either carrier"
    );

    // Behavioral positive half, SAME turn — this is what makes the negative half
    // below non-vacuous.
    let life_before = runner.state().players[0].life;
    assert!(
        run_combat(&mut runner, P1, bear, P0, None),
        "combat reach-guard (same turn)"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "the shield must prevent the chosen source's damage in its own turn"
    );

    // Cross two boundaries back to P1's next turn.
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P0);
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P1);
    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "CR 514.2: the duration-less resolution shield must be pruned at cleanup"
    );

    let life_before = runner.state().players[0].life;
    assert!(
        run_combat(&mut runner, P1, bear, P0, None),
        "combat reach-guard (later turn)"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before - 3,
        "damage must land once the engine's turn window has ended"
    );
}

// ---------------------------------------------------------------------------
// T8 — the ability-duration carrier.
// ---------------------------------------------------------------------------

/// **T8.** CR 611.2a names BOTH duration carriers: "as stated by the spell OR
/// ABILITY creating it". Morningtide's Light states "Until your next turn" on the
/// ability, not on the prevention effect, so its window can only be read through
/// `prevent_damage::resolve`'s `.or_else(expiry_from_duration(ability.duration))`
/// fallback. Cut that fallback and step 2 fails immediately.
///
/// The negative half (step 5) is what stops the fallback from being an
/// immortality bug: the `UntilPlayerNextTurn` prune at P0's untap step must
/// remove it.
#[test]
fn ability_duration_prevention_shield_survives_to_controllers_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let ml = scenario
        .add_spell_to_hand_from_oracle(P0, "Morningtide's Light", false, MORNINGTIDES_LIGHT_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    // "Exile any number of target creatures" — cast with zero targets.
    runner.cast(ml).target_objects(&[]).resolve();

    // Reach-guard: the shield lands on the pending registry (the Sorcery exiles
    // itself), correctly scoped to its controller.
    let pending = &runner.state().pending_damage_replacements;
    assert_eq!(pending.len(), 1, "the prevention clause must have resolved");
    assert_eq!(
        pending[0].shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        }
    );
    // THE REVERT-FAILING ASSERTION. Measured `None` before the fix.
    assert_eq!(
        pending[0].expiry,
        Some(RestrictionExpiry::UntilPlayerNextTurn { player: P0 }),
        "CR 611.2a: the ability's stated 'Until your next turn' must be the window"
    );

    // It survives a cleanup step it dies at today.
    let life_before = runner.state().players[0].life;
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "CR 611.2a: an 'until your next turn' window outlives its own turn's cleanup"
    );

    assert!(
        run_combat(&mut runner, P1, bear, P0, None),
        "combat reach-guard"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "the card's actual promise: damage to P0 is prevented during P1's turn"
    );

    // The negative half: the window really does end at P0's next turn.
    cross_boundary(&mut runner);
    assert_eq!(runner.state().active_player, P0);
    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "the UntilPlayerNextTurn prune must fire — the shield is not immortal"
    );
}

// ---------------------------------------------------------------------------
// T9 — the complement of T1: a PRINTED def that carries `expiry: None` today
// but whose own clause states a turn window.
// ---------------------------------------------------------------------------

/// Verbatim Urza's Science Fair Project (MTGJSON `text`). A `{2}` activated die
/// roll whose six results are printed as an em-dash results table; row 2 states
/// its own turn window and is the corpus's only turn-windowed printed shield
/// hosted on a PERMANENT.
const URZAS_SCIENCE_FAIR_PROJECT_TEXT: &str = "{2}: Roll a six-sided die. This creature gets the indicated result.\n1 \u{2014} It gets -2/-2 until end of turn.\n2 \u{2014} Prevent all combat damage it would deal this turn.\n3 \u{2014} It gains vigilance until end of turn.\n4 \u{2014} It gains first strike until end of turn.\n5 \u{2014} It gains flying until end of turn.\n6 \u{2014} It gets +2/+2 until end of turn.";

/// **T9.** CR 611.2a + CR 514.2, and the counterpart hazard to T1: making
/// `expiry` the single lifetime authority is only safe if every definition that
/// reaches the battlefield with `expiry: None` is genuinely a CR 604.2 printed
/// static that states no window. This card is the corpus's one counterexample.
///
/// Its row "Prevent all combat damage it would deal this turn." lowers to a
/// printed `DamageDone` shield on the permanent itself, UNSCOPED in both
/// directions (`valid_card: None`, `damage_target_filter: None`). Under the old
/// `is_shield()` blanket the mis-lowering self-limited to one turn; keyed on
/// `expiry` alone and left unstamped it would be immortal — a game-wide "no
/// combat damage is ever dealt, by or to anyone" lock. The parser now records
/// the window the clause states, via the positional `strip_trailing_duration`
/// authority, so cleanup catches it on the very evidence the clause provides.
///
/// Reverting `parse_damage_prevention_replacement`'s `stated_clause_expiry`
/// stamp flips BOTH the structural assertion (`expiry` becomes `None`, and the
/// shield count after the boundary becomes `(1, 1)`) and the behavioral one
/// (life 20 - 3 = 17 becomes 20). Measured at the pre-fix candidate: `(1, 1)`
/// and life 20.
///
/// The card's printed type line is Artifact Creature; the scenario stages it as
/// a plain creature because the artifact half is not load-bearing. What makes
/// this the counterexample — and the eight Instant/Sorcery hosts of the same
/// shape harmless — is only that it is a PERMANENT, so its definitions clear
/// `object_replacement_candidate_applies`' `[Battlefield, Command]` zone gate.
#[test]
fn turn_windowed_printed_shield_is_stamped_and_does_not_survive_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let project = scenario
        .add_creature_from_oracle(
            P0,
            "Urza's Science Fair Project",
            4,
            4,
            URZAS_SCIENCE_FAIR_PROJECT_TEXT,
        )
        // CR 302.6: it entered this turn. Without this the 4/4 is a legal
        // attacker in P0's own turn and `cross_boundary`'s
        // `advance_to_phase(Phase::End)` STALLS SILENTLY at DeclareAttackers —
        // the stall this file's helper doc warns about. Orthogonal to the
        // replacement under test.
        .with_summoning_sickness()
        .id();
    let bear = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    // Reach-guard: the card really is a battlefield permanent with its Oracle
    // text applied, so everything below is about a live object.
    assert_eq!(runner.state().objects[&project].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&project]
            .abilities
            .iter()
            .any(|a| a.kind == AbilityKind::Activated),
        "reach-guard: the {{2}} die-roll activated ability parsed, so the Oracle \
         text was really applied to the object"
    );

    let shields_on = |runner: &GameRunner| -> (usize, usize) {
        let obj = &runner.state().objects[&project];
        (
            obj.replacement_definitions
                .iter_unchecked()
                .filter(|r| r.shield_kind.is_shield())
                .count(),
            obj.base_replacement_definitions
                .iter()
                .filter(|r| r.shield_kind.is_shield())
                .count(),
        )
    };

    // Reach-guard + THE REVERT-FAILING STRUCTURAL ASSERTION. The mis-lowered
    // shield really is installed on both surfaces (so the prune below is not
    // vacuous), and it now carries the window its own clause states. Measured
    // `expiry: None` before the fix.
    assert_eq!(
        shields_on(&runner),
        (1, 1),
        "reach-guard: the printed shield is installed on both surfaces"
    );
    let installed = runner.state().objects[&project]
        .base_replacement_definitions
        .iter()
        .find(|r| r.shield_kind.is_shield())
        .expect("reach-guard: the shield is on the base surface");
    assert_eq!(
        installed.shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::All
        }
    );
    assert_eq!(
        installed.expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "CR 611.2a + CR 514.2: the clause's own 'this turn' must be recorded as \
         the definition's window"
    );

    let life_before = runner.state().players[0].life;
    cross_boundary(&mut runner);
    assert_eq!(
        runner.state().active_player,
        P1,
        "scenario must have advanced into P1's turn"
    );
    assert_eq!(
        runner.state().objects[&project].zone,
        Zone::Battlefield,
        "the permanent itself never left — an unpruned shield of its would still apply"
    );
    // The finding's exact framing: a printed shield whose clause states a turn
    // window must not be alive after the cleanup step, on EITHER surface.
    assert_eq!(
        shields_on(&runner),
        (0, 0),
        "CR 514.2: a printed shield stating 'this turn' must not survive cleanup"
    );

    // P1 attacks P0 with an unblocked 3/3. The shield was unscoped in both
    // directions, so while alive it prevented this damage too.
    assert!(
        run_combat(&mut runner, P1, bear, P0, None),
        "combat reach-guard: the attack must actually have happened"
    );
    // THE REVERT-FAILING BEHAVIORAL ASSERTION. Measured 20 -> 20 before the fix.
    assert_eq!(
        runner.state().players[0].life,
        life_before - 3,
        "CR 514.2: with the turn-windowed shield gone, ordinary combat damage lands"
    );
}
