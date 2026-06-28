//! Last Night Together (cluster 61) — end-to-end runtime proof.
//!
//! Oracle:
//! > Choose two target creatures. Untap them. Put two +1/+1 counters on each of
//! > them. They gain vigilance, indestructible, and haste until end of turn.
//! > After this main phase, there is an additional combat phase. Only the chosen
//! > creatures can attack during that combat phase.
//!
//! Before the fix the sixth sentence parsed to `Effect::Unimplemented{name:"only"}`.
//! After the fix it folds onto the scheduled `Effect::AdditionalPhase` as an
//! `attacker_restriction`, the resolver snapshots the spell's chosen targets into
//! a fixed tracked set (CR 608.2h), and combat enforcement (CR 508.1c / CR 611.2c)
//! restricts the scheduled combat phase to those creatures.
//!
//! This test drives the real cast pipeline: cast LNT choosing two of three
//! controlled creatures, resolve, advance into the scheduled extra combat, and
//! assert that only the two chosen creatures may be declared as attackers — which
//! also proves parent→sub-ability target propagation reaches the AdditionalPhase
//! resolver. It then runs the combat out and asserts the restriction is cleared.

use engine::game::combat::{get_valid_attacker_ids, validate_attackers, AttackTarget};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const LNT_ORACLE: &str = "Choose two target creatures. Untap them. Put two +1/+1 counters on \
each of them. They gain vigilance, indestructible, and haste until end of turn. After this \
main phase, there is an additional combat phase. Only the chosen creatures can attack during \
that combat phase.";

/// CR 508.1c + CR 611.2c: only the chosen creatures may attack during the
/// scheduled additional combat phase; the unchosen creature is excluded even
/// though it is otherwise a legal attacker.
#[test]
fn last_night_together_restricts_additional_combat_to_chosen_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Three otherwise-legal attackers controlled by the active player. The
    // builder marks them not-summoning-sick, so only the spell's restriction
    // can keep the unchosen one out of combat.
    let chosen_a = scenario.add_creature(P0, "Chosen A", 2, 2).id();
    let chosen_b = scenario.add_creature(P0, "Chosen B", 2, 2).id();
    let unchosen_c = scenario.add_creature(P0, "Unchosen C", 2, 2).id();

    // Mana cost is irrelevant to the restriction; use a simple generic cost the
    // pool auto-pays so the cast commits without a colored-pip funding dance.
    let lnt = scenario
        .add_spell_to_hand_from_oracle(P0, "Last Night Together", false, LNT_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    // Cast choosing A and B; resolve the whole chain.
    let outcome = runner
        .cast(lnt)
        .target_objects(&[chosen_a, chosen_b])
        .resolve();
    // Sanity: the spell resolved (it scheduled the extra phase / put counters).
    assert!(
        runner
            .state()
            .extra_phases
            .iter()
            .any(|ep| ep.phase == Phase::BeginCombat),
        "an additional combat phase must be scheduled; outcome stack empty = {}",
        outcome.state().stack.is_empty()
    );

    // CR 122.1 + CR 608.2c: "Put two +1/+1 counters on each of them" places the
    // counters on the two CHOSEN creatures (the `ParentTarget` anaphor) — not on
    // every battlefield permanent. Before the fix the bare "each of them" pronoun
    // parsed to `PutCounterAll { target: Typed{} }` (an empty filter matching all
    // permanents), so the unchosen creature wrongly received counters too.
    assert_eq!(
        outcome.counters(chosen_a, CounterType::Plus1Plus1),
        2,
        "chosen A must receive two +1/+1 counters"
    );
    assert_eq!(
        outcome.counters(chosen_b, CounterType::Plus1Plus1),
        2,
        "chosen B must receive two +1/+1 counters"
    );
    assert_eq!(
        outcome.counters(unchosen_c, CounterType::Plus1Plus1),
        0,
        "the unchosen creature must NOT receive counters — \"each of them\" is the chosen set"
    );

    // Advance into the scheduled (restricted) combat's declare-attackers step.
    runner.advance_to_combat();
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    assert!(
        runner.state().current_combat_attacker_restriction.is_some(),
        "the scheduled combat phase must carry the attacker restriction"
    );

    // CR 508.1c: only the two chosen creatures are legal attackers.
    let valid = get_valid_attacker_ids(runner.state());
    assert!(valid.contains(&chosen_a), "chosen A may attack");
    assert!(valid.contains(&chosen_b), "chosen B may attack");
    assert!(
        !valid.contains(&unchosen_c),
        "unchosen C must be excluded by the restriction"
    );
    assert!(
        validate_attackers(runner.state(), &[unchosen_c]).is_err(),
        "declaring the unchosen creature must be illegal"
    );
    assert!(
        validate_attackers(runner.state(), &[chosen_a, chosen_b]).is_ok(),
        "declaring the two chosen creatures must be legal"
    );

    // Declare the legal attack and run combat to its end.
    runner
        .declare_attackers(&[
            (chosen_a, AttackTarget::Player(P1)),
            (chosen_b, AttackTarget::Player(P1)),
        ])
        .expect("declaring the chosen attackers must be accepted");
    let _ = runner.combat_damage();

    // CR 511.3: once that combat phase is over, its restriction ends.
    assert!(
        runner.state().current_combat_attacker_restriction.is_none(),
        "the restriction must clear once the scheduled combat phase ends"
    );
}
