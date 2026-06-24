//! Regression (issue #2894): Spymaster's Vault connive count must equal the
//! number of creatures that died this turn (including tokens), not hardcode 1.
//!
//! Oracle: `{B}, {T}: Target creature you control connives X, where X is the
//! number of creatures that died this turn.`
//!
//! CR 701.50d + CR 700.4 + CR 107.3i: X is a dynamic zone-change count over
//! battlefield→graveyard transitions recorded in `zone_changes_this_turn`.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetRef};
use engine::types::counter::CounterType;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VAULT_ORACLE: &str = "This land enters tapped unless you control a Swamp.\n\
{T}: Add {B}.\n\
{B}, {T}: Target creature you control connives X, where X is the number of creatures that died this turn.";

#[test]
fn spymasters_vault_connive_parses_creatures_died_count() {
    let parsed = parse_oracle_text(
        VAULT_ORACLE,
        "Spymaster's Vault",
        &[],
        &["Land".to_string()],
        &[],
    );
    let connive = parsed
        .abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::Connive { count, .. } => Some(count.clone()),
            _ => None,
        })
        .expect("Spymaster's Vault must parse a Connive activated ability");
    assert!(
        matches!(
            connive,
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn {
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Graveyard),
                    ..
                },
            }
        ),
        "connive count must be creatures-died-this-turn, got {connive:?}"
    );
}

/// CR 701.50a/d: With two creatures having died this turn (one nontoken, one
/// token), connive draws and discards two cards, placing one +1/+1 counter per
/// nonland discarded.
#[test]
fn spymasters_vault_connive_uses_creature_death_count_including_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let vault = scenario
        .add_creature_from_oracle(P0, "Spymaster's Vault", 0, 0, VAULT_ORACLE)
        .id();

    // Two creatures that will die this turn — one nontoken, one token.
    let nontoken = scenario.add_creature(P0, "Bear", 2, 2).id();
    let token = scenario.add_creature(P0, "Token Soldier", 1, 1).id();

    let conniver = scenario.add_creature(P0, "Conniver", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;

    // Populate deaths through the production zone-change recorder.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), nontoken, Zone::Graveyard, &mut events);
    move_to_zone(runner.state_mut(), token, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().zone_changes_this_turn.len(),
        2,
        "sanity: both deaths must be recorded"
    );

    // Stock the library with two nonland cards so connive X=2 can draw and
    // auto-discard them (empty hand → draw 2 → discard all 2).
    for i in 0..2 {
        create_object(
            runner.state_mut(),
            CardId(10_000 + i),
            P0,
            format!("Spell {i}"),
            Zone::Library,
        );
    }

    let connive_def = runner
        .state()
        .objects
        .get(&vault)
        .expect("vault on battlefield")
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::Connive { .. }))
        .expect("connive activated ability")
        .clone();

    let ability = ResolvedAbility {
        targets: vec![TargetRef::Object(conniver)],
        ..build_resolved_from_def(&connive_def, vault, P0)
    };

    events.clear();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("connive must resolve");

    let counters = runner
        .state()
        .objects
        .get(&conniver)
        .expect("conniver still on battlefield")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 2,
        "connive X=2 with two nonland discards must place two +1/+1 counters, got {counters}"
    );
}
