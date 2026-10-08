//! Ironsoul Enforcer — "Whenever this creature or a commander you control
//! attacks alone, return target artifact card from your graveyard to the
//! battlefield."
//!
//! The building block under test is a *disjunctive* attacks-alone trigger
//! subject: a self-reference OR'd with a non-self class (here CR 903.3's
//! commander designation). Two axes must hold together:
//!
//!   * CR 506.5 — "attacks alone" is evaluated against the creature that was
//!     declared as an attacker, not against the ability's source. When the
//!     commander attacks alone the Enforcer is not in combat at all, so the
//!     co-attacker tally must exclude the *commander*, not the Enforcer.
//!   * CR 903.3 — "commander" is a deck-construction designation carried by
//!     `is_commander`, not a creature subtype. A creature that is not flagged
//!     must not satisfy the second disjunct.
//!
//! Covers the whole `<self> or <class> attacks alone` class (Ironsoul Enforcer
//! and any future card whose attacks-alone subject is a disjunction), not just
//! this card.

use engine::types::phase::Phase;

use super::rules::{run_combat, GameScenario, P0};

const ORACLE: &str = "Whenever this creature or a commander you control attacks alone, return target artifact card from your graveyard to the battlefield.";

/// Which creatures P0 declares as attackers in the fixture.
#[derive(Clone, Copy)]
enum Attack {
    /// The commander attacks by itself; the Enforcer stays home.
    CommanderAlone,
    /// The Enforcer attacks by itself; the commander stays home.
    EnforcerAlone,
    /// Both attack — no creature attacks alone.
    Both,
    /// A non-commander bear attacks by itself — neither disjunct matches.
    NonCommanderAlone,
}

/// Returns whether the graveyard artifact ("Ornithopter") reached the
/// battlefield, i.e. whether the trigger fired and resolved.
fn artifact_returned(attack: Attack) -> bool {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let enforcer = scenario
        .add_creature_from_oracle(P0, "Ironsoul Enforcer", 4, 4, ORACLE)
        .id();
    let commander = scenario
        .add_creature(P0, "Kediss, Emberclaw Familiar", 2, 2)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // The reanimation target: an artifact card in P0's graveyard.
    let ornithopter = scenario
        .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
        .as_artifact()
        .id();

    let mut runner = scenario.build();

    // CR 903.3: the commander designation is a per-object flag, not a subtype.
    // Mirrors `make_commander` in `rules/layers.rs` — owner == controller == P0
    // with the object on the battlefield, so `FilterProp::IsCommander` matches.
    runner
        .state_mut()
        .objects
        .get_mut(&commander)
        .expect("commander object exists")
        .is_commander = true;

    let attackers = match attack {
        Attack::CommanderAlone => vec![commander],
        Attack::EnforcerAlone => vec![enforcer],
        Attack::Both => vec![commander, enforcer],
        Attack::NonCommanderAlone => vec![bear],
    };

    run_combat(&mut runner, attackers, vec![]);
    runner.advance_until_stack_empty();

    runner.state().battlefield.contains(&ornithopter)
}

#[test]
fn commander_attacking_alone_fires_while_the_enforcer_stays_home() {
    // CR 506.5: the commander is the only declared attacker, so it attacks
    // alone even though the trigger's source is not in combat.
    assert!(
        artifact_returned(Attack::CommanderAlone),
        "CR 506.5 + CR 903.3: a lone commander attacker must satisfy the second \
         disjunct and return the graveyard artifact"
    );
}

#[test]
fn enforcer_attacking_alone_fires_on_the_self_disjunct() {
    assert!(
        artifact_returned(Attack::EnforcerAlone),
        "CR 506.5: the self-reference disjunct must still fire when the source \
         itself is the lone attacker"
    );
}

#[test]
fn two_attackers_do_not_attack_alone() {
    assert!(
        !artifact_returned(Attack::Both),
        "CR 506.5: with two declared attackers neither creature attacks alone"
    );
}

#[test]
fn non_commander_attacking_alone_does_not_fire() {
    assert!(
        !artifact_returned(Attack::NonCommanderAlone),
        "CR 903.3: an unflagged creature is not a commander, so a lone \
         non-commander attacker matches neither disjunct"
    );
}
