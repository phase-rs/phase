//! Discriminating regression test for **issue #656**: Shilgengar, Sire of
//! Famine's second ability —
//!
//! > {W/B}{W/B}{W/B}, Sacrifice six Blood tokens: Return each creature card
//! > from your graveyard to the battlefield with a finality counter on it.
//! > Those creatures are Vampires in addition to their other types.
//!
//! The "return each creature card ... to the battlefield" clause is a MASS
//! return and must lower to `Effect::ChangeZoneAll`, returning every creature
//! card in the controller's graveyard. Before the fix the parser collapsed it
//! to a single-target `Effect::ChangeZone` because the mass branch was gated on
//! `enter_with_counters` being empty — the finality counter disqualified the
//! mass path, so only ONE creature would return.
//!
//! Setup: three creature CARDS in P0's graveyard. Resolving the parsed return
//! clause under P0 must:
//!   1. Return ALL THREE to the battlefield under P0's control (the
//!      `ChangeZoneAll` discriminator — single `ChangeZone` would return one).
//!   2. Give EACH a finality counter (proves the counters are threaded through
//!      the mass path, not dropped).
//!
//! With the fix reverted, the battlefield count would be 1 (or 0) and the
//! assertion on `== 3` fails.
//!
//! CR 400.7: a zone change creates a new object on the battlefield.
//! CR 122.1h: one or more finality counters exile the permanent instead of it
//! going to the graveyard.
//! CR 701.21: Sacrifice (the activation cost — out of scope for this parser
//! regression, exercised by the cost resolver elsewhere).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHILGENGAR_RETURN: &str =
    "Return each creature card from your graveyard to the battlefield with a finality counter on it.";

#[test]
fn shilgengar_returns_each_creature_card_with_finality_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Shilgengar on the battlefield as the ability source.
    let shilgengar = scenario
        .add_creature(P0, "Shilgengar, Sire of Famine", 4, 4)
        .id();

    // Three creature CARDS in P0's graveyard — all should return.
    scenario.add_creature_to_graveyard(P0, "Graveyard Creature A", 1, 1);
    scenario.add_creature_to_graveyard(P0, "Graveyard Creature B", 2, 2);
    scenario.add_creature_to_graveyard(P0, "Graveyard Creature C", 3, 3);

    let mut runner = scenario.build();

    // Same parser path the real card uses: the clause lowers to
    // `Effect::ChangeZoneAll`, which is the mass-return discriminator.
    let def = parse_effect_chain(SHILGENGAR_RETURN, AbilityKind::Activated);
    assert!(
        matches!(&*def.effect, Effect::ChangeZoneAll { .. }),
        "mass return must lower to ChangeZoneAll, got {:?}",
        def.effect
    );

    let ability = build_resolved_from_def(&def, shilgengar, P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("mass return must resolve");

    // DISCRIMINATOR #1 (ChangeZoneAll vs ChangeZone): all THREE creature cards
    // are now on the battlefield under P0's control. A single `ChangeZone` would
    // have returned only one.
    let returned: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.id != shilgengar
                && obj.zone == Zone::Battlefield
                && obj.controller == P0
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .collect();
    assert_eq!(
        returned.len(),
        3,
        "all three graveyard creatures must return to the battlefield (mass \
         ChangeZoneAll); single ChangeZone would return only 1"
    );

    // DISCRIMINATOR #2 (counters threaded through the mass path): every returned
    // creature carries exactly one finality counter (CR 122.1h).
    for obj in &returned {
        let finality = obj
            .counters
            .get(&CounterType::Finality)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            finality, 1,
            "returned creature {:?} must enter with one finality counter; the \
             mass path must thread enter_with_counters through ChangeZoneAll",
            obj.name
        );
    }
}

/// Regression guard: a SINGLE-target return ("return target creature card from
/// your graveyard to the battlefield") must still lower to single-target
/// `Effect::ChangeZone`, NOT the mass `ChangeZoneAll`. The mass path is gated on
/// the "each"/"all" quantifier alone — dropping the counter guard must not
/// promote single-target returns to mass.
#[test]
fn single_target_return_to_battlefield_stays_change_zone() {
    let def = parse_effect_chain(
        "Return target creature card from your graveyard to the battlefield.",
        AbilityKind::Activated,
    );
    assert!(
        matches!(&*def.effect, Effect::ChangeZone { .. }),
        "single-target return must stay ChangeZone, got {:?}",
        def.effect
    );
}
