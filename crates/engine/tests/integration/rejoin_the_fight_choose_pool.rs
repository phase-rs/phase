//! Rejoin the Fight (FIC) — `ChooseFromZone` candidate pool and zone owner.
//!
//! Verbatim Oracle text, confirmed via Scryfall `cards/named`:
//!
//! > Mill three cards. Then starting with the next opponent in turn order, each
//! > opponent chooses a creature card in your graveyard that hasn't been chosen.
//! > Return each card chosen this way to the battlefield under your control.
//!
//! The clause NAMES its own zone, so its candidate pool is that zone (CR 608.2c:
//! instructions are followed in the order written; CR 608.2d: the choice is made
//! while applying the effect). Two defects met here:
//!
//! * **Candidate pool.** `ChooseImperativeAst::FromZone` lowered with
//!   `ZoneChoiceCandidateSource::Legacy`, which prefers the resolution chain's
//!   tracked set whenever one exists. The preceding `Mill` publishes one, so the
//!   spell offered only the three cards it had just milled — and, because every
//!   selection republishes that set, each later opponent was offered only the
//!   previous opponent's pick.
//!
//! * **Zone owner.** CR 109.5: "your" means the spell's controller. The
//!   `player_scope` fan-out rebinds `ability.controller` to the iterated player
//!   and preserves the printed controller in `original_controller`
//!   (`game/effects/mod.rs:14389-14397`, `:13039-13041`), so
//!   `resolve_zone_owner` reading `controller` scanned each iterated OPPONENT's
//!   graveyard.
//!
//! Deferred, deliberately, and visible in the expectations below: `"that hasn't
//! been chosen"` still produces no constraint, so the opponents may re-pick one
//! another's cards. The `starting with the next opponent in turn order` clause
//! remains a declared `Unimplemented`.
//!
//! Revert-to-red:
//! * restore `candidate_source: Legacy` in the `FromZone` lowering arm ⇒
//!   `pool_is_the_casters_whole_graveyard_not_just_the_milled_cards` fails with
//!   only the milled cards offered.
//! * restore `ZoneOwner::Controller => Ok(ability.controller)` in
//!   `resolve_zone_owner` ⇒ `your_graveyard_is_the_casters_not_each_iterated_
//!   opponents` fails with each opponent offered their own decoy.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const REJOIN_THE_FIGHT: &str = "Mill three cards. Then starting with the next opponent in turn order, each opponent chooses a creature card in your graveyard that hasn't been chosen. Return each card chosen this way to the battlefield under your control.";

/// Every pool the resolution offered, in prompt order, with the chooser.
fn drive(
    runner: &mut GameRunner,
    spell: engine::types::identifiers::ObjectId,
) -> Vec<(PlayerId, Vec<String>)> {
    let mut commit = runner.cast(spell).free_cast().commit();
    let mut offers: Vec<(PlayerId, Vec<String>)> = Vec::new();

    for _ in 0..80 {
        let waiting = commit.state().waiting_for.clone();
        match waiting {
            WaitingFor::Priority { .. } => {
                if commit.state().stack.is_empty() {
                    break;
                }
                commit
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the spell resolves");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let count = triggers.len();
                commit
                    .act(GameAction::OrderTriggers {
                        order: (0..count).collect(),
                    })
                    .expect("order triggers");
            }
            WaitingFor::ChooseFromZoneChoice {
                player, ref cards, ..
            } => {
                let names: Vec<String> = cards
                    .iter()
                    .map(|id| {
                        commit
                            .state()
                            .objects
                            .get(id)
                            .map(|obj| obj.name.clone())
                            .unwrap_or_default()
                    })
                    .collect();
                offers.push((player, names));
                commit
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("submit the zone choice");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }
    offers
}

/// Four-player board. The caster's graveyard is pre-seeded with creature cards
/// that were NOT milled by this spell, and the library top is stocked so the
/// mill has something to move.
fn board() -> (GameRunner, engine::types::identifiers::ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rejoin the Fight", false, REJOIN_THE_FIGHT)
        .id();

    for name in ["Buried Alpha", "Buried Beta", "Buried Gamma"] {
        scenario.add_creature_to_graveyard(P0, name, 2, 2);
    }
    // Decoys: each opponent's OWN graveyard. CR 109.5 makes these unreachable.
    for (player, name) in [
        (PlayerId(1), "Decoy P1"),
        (PlayerId(2), "Decoy P2"),
        (PlayerId(3), "Decoy P3"),
    ] {
        scenario.add_creature_to_graveyard(player, name, 2, 2);
    }
    scenario.with_library_top(
        P0,
        &["Milled One", "Milled Two", "Milled Three", "Milled Four"],
    );

    let mut runner = scenario.build();
    // The milled cards are creature cards, so "creature card in your graveyard"
    // cannot exclude them for want of a type — which is what makes the
    // pool assertions below discriminating rather than type-filtered.
    let library: Vec<_> = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("caster exists")
        .library
        .iter()
        .copied()
        .collect();
    for id in library {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("library card");
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
    }

    (runner, spell)
}

/// CR 608.2c + CR 608.2d: the pool is the named zone, not the preceding mill's
/// tracked set.
#[test]
fn pool_is_the_casters_whole_graveyard_not_just_the_milled_cards() {
    let (mut runner, spell) = board();
    let offers = drive(&mut runner, spell);

    // Positive reach guard: the choice really was presented.
    assert!(
        !offers.is_empty(),
        "no zone choice was ever presented — the assertions below would be vacuous"
    );
    let (_, first_pool) = &offers[0];
    assert!(
        !first_pool.is_empty(),
        "the first candidate pool must be non-empty"
    );

    // The pre-seeded graveyard creatures must be reachable. Pre-fix the pool was
    // exactly the three milled cards and these were never offered.
    for buried in ["Buried Alpha", "Buried Beta", "Buried Gamma"] {
        assert!(
            first_pool.contains(&buried.to_string()),
            "{buried} is a creature card in the caster's graveyard and must be offered; \
             pool was {first_pool:?}"
        );
    }
}

/// CR 608.2c: each opponent chooses from the named zone, so a later chooser is
/// not restricted to an earlier chooser's pick.
#[test]
fn later_opponents_are_not_restricted_to_the_previous_opponents_pick() {
    let (mut runner, spell) = board();
    let offers = drive(&mut runner, spell);

    assert!(
        offers.len() >= 2,
        "expected one prompt per opponent, got {offers:?}"
    );
    let (_, second_pool) = &offers[1];
    assert!(
        second_pool.len() > 1,
        "the second opponent must still see a real pool, not just the first \
         opponent's pick; got {second_pool:?}"
    );
}

/// CR 109.5: "your graveyard" is the caster's, never the iterated opponent's.
///
/// This is the guard on the zone-owner binding specifically. With the candidate
/// pool fixed, reverting ONLY `resolve_zone_owner` re-points each iteration at
/// its own opponent's graveyard, so each opponent is offered their own decoy.
#[test]
fn your_graveyard_is_the_casters_not_each_iterated_opponents() {
    let (mut runner, spell) = board();
    let offers = drive(&mut runner, spell);

    assert!(
        !offers.is_empty(),
        "no zone choice was presented — assertion would be vacuous"
    );

    for (chooser, pool) in &offers {
        assert!(
            !pool.is_empty(),
            "chooser p{} was offered an empty pool",
            chooser.0
        );
        assert!(
            !pool.iter().any(|name| name.starts_with("Decoy ")),
            "p{} was offered a card from an opponent's own graveyard: {pool:?}",
            chooser.0
        );
    }

    let battlefield: Vec<String> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .map(|obj| obj.name.clone())
        .collect();
    assert!(
        !battlefield.iter().any(|name| name.starts_with("Decoy ")),
        "the spell must not reanimate an opponent's own creature; battlefield {battlefield:?}"
    );
}
