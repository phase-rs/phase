//! R3-#1: the survivor pool and the Horde seat must not share one
//! shared-resource dedup key.
//!
//! Under one-vs-many the survivor side IS `TeamId(1)`, while a Horde seat
//! anchored at seat 1 also keyed as `TeamId(1)` — so `aggregate_over_teams`
//! dropped the Horde's pool and Repay in Kind read the survivors' 40 as the
//! minimum, raising the bosses instead of setting everyone to 5. The key now
//! separates shared sides from individual seats.
//!
//! Regression shape follows the Biorhythm precedent
//! (`biorhythm_sets_each_player_life_to_their_own_creature_count`): an
//! `AllPlayers`-scoped `SetLifeTotal` is not a targeted spell, so it cannot
//! go through the cast pipeline — the PARSED Repay in Kind effect is used
//! verbatim through the public resolver over a staged Horde game with the
//! Horde at seat 1.
//!
//! Revert guard: restoring the `TeamId`-for-both key drops the Horde pool
//! from the fold — the minimum reads 40, the survivors stay 40, and the
//! pool/life assertions flip.

use engine::game::effects::life::resolve_set_life_total;
use engine::game::players::team_life_total;
use engine::game::scenario::GameScenario;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, ResolvedAbility};
use engine::types::card_type::CoreType;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HORDE: PlayerId = PlayerId(1);
const SURVIVOR_A: PlayerId = PlayerId(0);
const SURVIVOR_B: PlayerId = PlayerId(2);

const REPAY_IN_KIND: &str =
    "Each player's life total becomes the lowest life total among all players.";

#[test]
fn repay_in_kind_reads_horde_seat_distinct_from_survivor_pool() {
    // Seats 0 and 2 survive (shared pool); the Horde anchors seat 1, the
    // same number the survivor side keys as — the collision under test.
    let mut config = FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset());
    config.archenemy_player = Some(HORDE);
    let mut scenario = GameScenario::new_with_format(config, 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    // A stocked library + battlefield creature keeps the Horde alive while
    // the survivors act (empty library + no creatures = defeat).
    for i in 0..10 {
        let card_id = CardId(runner.state().next_object_id);
        create_object(
            runner.state_mut(),
            card_id,
            HORDE,
            format!("Horde Lib {i}"),
            Zone::Library,
        );
    }
    let guard_card = CardId(runner.state().next_object_id);
    let guard = create_object(
        runner.state_mut(),
        guard_card,
        HORDE,
        "Horde Guard".to_string(),
        Zone::Battlefield,
    );
    let guard_obj = runner.state_mut().objects.get_mut(&guard).unwrap();
    guard_obj.card_types.core_types.push(CoreType::Creature);
    guard_obj.base_card_types = guard_obj.card_types.clone();
    for (seat, life) in [(SURVIVOR_A, 20), (SURVIVOR_B, 20), (HORDE, 5)] {
        if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == seat) {
            p.life = life;
        }
    }
    assert_eq!(
        team_life_total(runner.state(), SURVIVOR_A),
        40,
        "reach-guard: the survivor pool starts at 40"
    );

    // The parsed Repay in Kind effect verbatim through the public resolver.
    let def = parse_effect_chain(REPAY_IN_KIND, AbilityKind::Spell);
    let effect = (*def.effect).clone();
    let state = runner.state_mut();
    let ability = ResolvedAbility::new(effect, vec![], ObjectId(1), SURVIVOR_A);
    let mut events = Vec::new();
    resolve_set_life_total(state, &ability, &mut events).unwrap();

    let state = runner.state();
    assert_eq!(
        team_life_total(state, SURVIVOR_A),
        5,
        "the survivor pool must fall to the true minimum (5)"
    );
    assert_eq!(
        state.players.iter().find(|p| p.id == HORDE).unwrap().life,
        5,
        "the Horde seat must stay at the minimum (5), not rise to 40"
    );
}
