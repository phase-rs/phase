//! Issue #6012 — Nissa, Ascended Animist's +1 must create the Phyrexian Horror
//! token whose power and toughness equal Nissa's loyalty.
//!
//! Oracle (+1 loyalty ability):
//!   "Create an X/X green Phyrexian Horror creature token, where X is Nissa's
//!    loyalty."
//!
//! After card-name normalization the parser sees "where X is ~'s loyalty".
//! Before the fix, the self-possessive loyalty quantity was unrecognized, so
//! `parse_cda_quantity` returned `None`, the whole token clause failed to lower
//! (`try_parse_token` returns `None` when the P/T expression is unrepresentable),
//! and the ability degraded to `Effect::Unimplemented` — activating +1 created
//! no token at all.
//!
//! The fix adds `parse_self_loyalty_ref`, mapping "~'s loyalty" to
//! `QuantityRef::CountersOn { scope: Source, counter_type: Some(Loyalty) }`
//! (CR 306.5c: the loyalty of a planeswalker on the battlefield is the number of
//! loyalty counters on it). This runtime regression activates the parsed +1 from
//! a Nissa with five loyalty counters, then asserts the post-cost 6/6 Phyrexian
//! Horror token is created; if the quantity ever silently drops again the token
//! is absent and the assertions flip.

use std::sync::Arc;

use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::AbilityCost;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

// Verbatim +1 text; parse_oracle_text normalizes the card name to `~` before the
// effect parser sees it (CR 201.5 name normalization).
const NISSA_ORACLE: &str =
    "+1: Create an X/X green Phyrexian Horror creature token, where X is Nissa's loyalty.";

const LOYALTY: u32 = 5;

#[test]
fn issue_6012_nissa_plus_one_creates_horror_token_sized_to_loyalty() {
    let parsed = parse_oracle_text(
        NISSA_ORACLE,
        "Nissa, Ascended Animist",
        &[],
        &["Legendary".to_string()],
        &["Nissa".to_string()],
    );
    let plus_one_index = parsed
        .abilities
        .iter()
        .position(|ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: 1 })))
        .expect("Nissa's +1 must parse as a loyalty ability");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nissa = scenario
        .add_creature(P0, "Nissa, Ascended Animist", 0, 0)
        .id();
    let mut runner = scenario.build();

    // Nissa on the battlefield as a planeswalker holding five loyalty counters.
    // CR 306.5c: loyalty on the battlefield IS the loyalty-counter count, which
    // is the quantity the token's "where X is ~'s loyalty" clause reads.
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&nissa)
            .expect("nissa exists");
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.loyalty = Some(LOYALTY);
        obj.counters.insert(CounterType::Loyalty, LOYALTY);
        obj.abilities = Arc::new(parsed.abilities.clone());
        obj.base_abilities = Arc::new(parsed.abilities);
    }

    // The real loyalty activation pipeline pays +1 before resolving the effect.
    // This proves both that the normalized full ability lowers to a token and that
    // its dynamic P/T reads Nissa's post-cost loyalty, rather than a test-only
    // manually-built resolved ability.
    runner.activate(nissa, plus_one_index).resolve();
    let expected_loyalty = LOYALTY + 1;
    assert_eq!(
        runner.state().objects[&nissa].loyalty,
        Some(expected_loyalty),
        "activating Nissa's +1 must add a loyalty counter before resolution"
    );

    // Observable state: a brand-new token creature (distinct from Nissa) with
    // the Horror subtype, on the controller's battlefield, sized to loyalty.
    let token = runner
        .state()
        .objects
        .values()
        .find(|obj| {
            obj.id != nissa
                && obj.is_token
                && obj.zone == engine::types::zones::Zone::Battlefield
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .expect("Nissa's +1 must create a token creature");

    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Horror")),
        "token must be a Phyrexian Horror, got subtypes {:?}",
        token.card_types.subtypes
    );
    assert_eq!(
        token.power,
        Some(expected_loyalty as i32),
        "token power must equal Nissa's post-activation loyalty ({expected_loyalty})"
    );
    assert_eq!(
        token.toughness,
        Some(expected_loyalty as i32),
        "token toughness must equal Nissa's post-activation loyalty ({expected_loyalty})"
    );
}
