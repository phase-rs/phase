//! Regression for issue #4513 (PR #4597) — Morkrut Necropod must never be
//! offered as a sacrifice to its own "sacrifice another creature or land"
//! trigger, even when a type-changing static makes it a Land.
//!
//! Morkrut Necropod reads: "Whenever Morkrut Necropod attacks or blocks,
//! sacrifice another creature or land." The parser builds the sacrifice target
//! as `Or { [Typed(creature), Typed(land)] }`. The word "another" is a runtime
//! IDENTITY exclusion (`FilterProp::Another` → `object_id != source.id`, see
//! `game/filter.rs`), NOT a type exclusion — it removes the *source permanent*
//! from the legal set regardless of what types that permanent currently has.
//!
//! The maintainer's (matthewevans) scenario: Ashaya, Soul of the Wild makes
//! "Nontoken creatures you control are Forest lands in addition to their other
//! types." Under Ashaya, Morkrut is simultaneously a Creature AND a Land, so it
//! matches the *land* leg of its own disjunctive trigger. An earlier fix put
//! `FilterProp::Another` on only the first (creature) leg of the `Or`; that
//! left the land leg without source-exclusion, so Morkrut-as-Land slipped
//! through and could be offered as a sacrifice to its own trigger. PR #4597
//! re-applies `Another` to EVERY leg of the `Or`, closing the hole.
//!
//! This test drives the real production path — declare Morkrut as an attacker,
//! let its attacks trigger resolve — and asserts that the Sacrifice
//! `EffectZoneChoice` excludes Morkrut (identity exclusion holds on the land
//! leg too) while still offering the other eligible permanent (so the
//! assertion is meaningful, not vacuously passing on an empty/absent choice).
//!
//! Why it would FAIL under the old single-leg behavior: with `Another` only on
//! the creature leg, Ashaya turns Morkrut into a Land, so it satisfies the
//! land leg — which carries no identity exclusion — and its id would appear in
//! the offered `cards`. The all-legs fix is exactly what removes it.
//!
//! CR 701.21a: To sacrifice a permanent, its controller moves it from the
//! battlefield to its owner's graveyard; a player can only sacrifice a
//! permanent they control. Morkrut's controller is the one choosing here, so
//! the legal set is its own permanents minus (by "another") Morkrut itself.
//!
//! https://github.com/phase-rs/phase/issues/4513

use engine::types::ability::EffectKind;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;

use super::rules::{AttackTarget, GameRunner, GameScenario, P0, P1};

/// Morkrut Necropod's combat trigger (Eldritch Moon). The "attacks or blocks"
/// trigger fires on declaration; "another creature or land" is the disjunctive
/// sacrifice target whose identity exclusion this test guards.
const MORKRUT_NECROPOD: &str =
    "Whenever Morkrut Necropod attacks or blocks, sacrifice another creature or land.";

/// Ashaya's "Nontoken creatures you control are Forest lands" static (shared
/// with `ashaya_nontoken_lands.rs`). The first line (a CDA) is included for
/// fidelity to the real card; only the type-changing line matters here.
const ASHAYA: &str = "Ashaya, Soul of the Wild's power and toughness are each \
equal to the number of lands you control.\nNontoken creatures you control are \
Forest lands in addition to their other types.";

/// Drive from `DeclareAttackers` (Morkrut already declared) through trigger
/// ordering and priority until the mandatory Sacrifice `EffectZoneChoice`
/// surfaces, then stop. Returns the offered `cards` set for inspection.
///
/// The sacrifice is mandatory and has more than one eligible permanent (the
/// other creature qualifies on both legs under Ashaya, and Morkrut is excluded
/// by "another"), so the resolver surfaces an `EffectZoneChoice` rather than
/// auto-sacrificing (see `game/effects/sacrifice.rs`).
fn drive_to_sacrifice_choice(runner: &mut GameRunner) -> Vec<engine::types::identifiers::ObjectId> {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::Sacrifice,
                cards,
                ..
            } => return cards,
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(engine::types::actions::GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(engine::types::actions::GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .ok();
            }
            _ => {
                if runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    panic!(
        "Morkrut's attack trigger never surfaced a Sacrifice EffectZoneChoice; \
         waiting_for = {:?}",
        runner.state().waiting_for
    );
}

/// Under Ashaya, Morkrut Necropod is itself a creature-Land and so matches the
/// land leg of its own "sacrifice another creature or land" trigger. PR #4597
/// applies the `another` identity exclusion to BOTH legs of the disjunction, so
/// Morkrut is not offered to its own trigger; the other eligible permanent is.
#[test]
fn morkrut_under_ashaya_is_not_offered_to_its_own_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // Morkrut Necropod: a fat attacker carrying the self-sacrifice trigger.
    let morkrut = scenario
        .add_creature_from_oracle(P0, "Morkrut Necropod", 4, 5, MORKRUT_NECROPOD)
        .id();

    // Ashaya makes every nontoken creature P0 controls a Forest land in
    // addition to its other types — including Morkrut and Grizzly Bears.
    scenario
        .add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA)
        .id();

    // A second nontoken creature so the mandatory sacrifice has a legal target
    // other than Morkrut. Under Ashaya it is a Creature AND a Land, so it
    // qualifies on both legs of the disjunction.
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // Pass priority so SBAs run and the layer system recomputes: Morkrut and
    // Grizzly Bears become creature-Lands under Ashaya.
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .ok();

    // Sanity: Ashaya's static actually made Morkrut a Land (the whole premise).
    // If this regressed, the land leg would not match Morkrut and the test
    // would pass vacuously — so we assert it explicitly.
    assert!(
        runner.state().objects[&morkrut]
            .card_types
            .core_types
            .contains(&CoreType::Land),
        "premise broken: Ashaya must make Morkrut a Land, got {:?}",
        runner.state().objects[&morkrut].card_types.core_types
    );
    assert!(
        runner.state().objects[&bears]
            .card_types
            .core_types
            .contains(&CoreType::Land),
        "premise broken: Ashaya must make Grizzly Bears a Land, got {:?}",
        runner.state().objects[&bears].card_types.core_types
    );

    // Production path: advance to the declare-attackers step (CR 508) and
    // declare Morkrut as an attacker so its "attacks" trigger fires.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(morkrut, AttackTarget::Player(P1))])
        .expect("Morkrut should be a legal attacker");

    // Resolve the attacks trigger until the Sacrifice choice surfaces.
    let offered = drive_to_sacrifice_choice(&mut runner);

    // CR 701.21a + `FilterProp::Another` (identity): Morkrut is the source of
    // its own trigger; "another" excludes it on BOTH the creature and land legs
    // even though Ashaya made it a Land. PR #4597 guards exactly this.
    assert!(
        !offered.contains(&morkrut),
        "Morkrut must NOT be offered as a sacrifice to its own trigger even as a \
         creature-Land under Ashaya (#4513); offered = {offered:?}"
    );

    // The choice must be genuinely non-empty: the other eligible permanent is
    // offered, so the exclusion above is a meaningful assertion, not a vacuous
    // pass on an empty/absent set.
    assert!(
        offered.contains(&bears),
        "the other eligible creature-Land must be offered (choice must be \
         non-empty); offered = {offered:?}"
    );
}
