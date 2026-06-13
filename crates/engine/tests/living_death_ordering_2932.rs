//! Living Death (#2932) — exile/sacrifice/return ordering must keep the
//! "exiled this way" set distinct from the creatures sacrificed during the
//! same resolution.
//!
//! Oracle: "Each player exiles all creature cards from their graveyard, then
//! sacrifices all creatures they control, then puts all cards they exiled this
//! way onto the battlefield."
//!
//! CR 608.2c / CR 608.2f: the spell resolves in three ordered steps — (1) every
//! player exiles all creature cards from their graveyard, (2) every player
//! sacrifices all creatures they control (those go to the graveyard), (3) every
//! player returns ONLY the cards they exiled in step 1. The creatures sacrificed
//! in step 2 must NOT join the returned set; the "exiled this way" tracked set
//! is fixed by step 1, before the sacrifice adds new creature cards to
//! graveyards.
//!
//! Pre-fix bug: the sacrifice step extended the chain's tracked object set with
//! the just-sacrificed permanents, so step 3 returned both the graveyard cards
//! AND the creatures sacrificed this way — "wrong creatures end up on the
//! battlefield and in the graveyard".

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Each player exiles all creature cards from their graveyard, then sacrifices all creatures they control, then puts all cards they exiled this way onto the battlefield.";

#[test]
fn living_death_returns_graveyard_cards_not_sacrificed_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Battlefield creatures (will be SACRIFICED this way → graveyard, NOT returned).
    let p0_bf = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let p1_bf = scenario.add_creature(P1, "Gray Wolf", 2, 2).id();

    // Graveyard creature CARDS (will be EXILED this way → returned to battlefield).
    let p0_gy = scenario
        .add_creature_to_graveyard(P0, "Walking Corpse", 2, 2)
        .id();
    let p1_gy = scenario
        .add_creature_to_graveyard(P1, "Bone Sentry", 2, 2)
        .id();

    // A non-creature source so the spell's source is never part of any creature
    // filter (Living Death is a sorcery; in this driver it has no real object).
    let source = scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();

    let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Living Death must resolve");

    let zone_of =
        |runner: &engine::game::scenario::GameRunner, id| runner.state().objects[&id].zone;

    // Step 3: the cards exiled from each graveyard return to that player's
    // battlefield.
    assert_eq!(
        zone_of(&runner, p0_gy),
        Zone::Battlefield,
        "P0's graveyard creature card must be returned to the battlefield"
    );
    assert_eq!(
        zone_of(&runner, p1_gy),
        Zone::Battlefield,
        "P1's graveyard creature card must be returned to the battlefield"
    );

    // Step 2: the creatures that were on the battlefield are sacrificed and must
    // remain in the graveyard — they were NOT exiled this way, so step 3 must
    // not return them.
    assert_eq!(
        zone_of(&runner, p0_bf),
        Zone::Graveyard,
        "P0's sacrificed battlefield creature must stay in the graveyard, not be returned"
    );
    assert_eq!(
        zone_of(&runner, p1_bf),
        Zone::Graveyard,
        "P1's sacrificed battlefield creature must stay in the graveyard, not be returned"
    );

    // Ownership / control stays with each player's own set (no cross-player leak).
    assert_eq!(runner.state().objects[&p0_gy].controller, P0);
    assert_eq!(runner.state().objects[&p1_gy].controller, P1);

    // Exactly the two graveyard cards return; the two sacrificed creatures do not.
    let returned_creatures: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner.state().objects[id]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature)
        })
        .collect();
    assert_eq!(
        returned_creatures.len(),
        2,
        "exactly the two exiled-this-way graveyard cards return, got {returned_creatures:?}"
    );
    assert!(returned_creatures.contains(&p0_gy));
    assert!(returned_creatures.contains(&p1_gy));
    assert!(!returned_creatures.contains(&p0_bf));
    assert!(!returned_creatures.contains(&p1_bf));
}
