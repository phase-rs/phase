//! Runtime regression for #2879 — Escape to the Wilds must grant both the
//! tracked exile play permission and the extra land drop through the real spell
//! resolution pipeline.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::static_abilities::additional_land_drops;
use engine::types::ability::{CastingPermission, Duration, PlayerScope};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ESCAPE_TO_THE_WILDS: &str = "Exile the top five cards of your library. \
You may play cards exiled this way until the end of your next turn. \
You may play an additional land this turn.";

#[test]
fn escape_to_the_wilds_runtime_grants_exile_play_and_extra_land() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let exiled_cards: Vec<_> = ["Alpha", "Bravo", "Charlie", "Delta", "Echo"]
        .into_iter()
        .map(|name| scenario.add_card_to_library_top(P0, name))
        .collect();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Escape to the Wilds", false, ESCAPE_TO_THE_WILDS)
        .with_mana_cost(ManaCost::zero())
        .id();

    let outcome = scenario.build().cast(spell).resolve();

    for id in &exiled_cards {
        assert_eq!(
            outcome.zone_of(*id),
            Zone::Exile,
            "Escape to the Wilds must exile the top five library cards"
        );

        let object = &outcome.state().objects[id];
        assert!(
            object.casting_permissions.iter().any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    granted_to: P0,
                    duration: Duration::UntilEndOfNextTurnOf {
                        player: PlayerScope::Controller
                    },
                    ..
                }
            )),
            "exiled card must carry the controller's until-next-turn PlayFromExile permission, got {:?}",
            object.casting_permissions
        );
    }

    assert_eq!(
        additional_land_drops(outcome.state(), P0),
        1,
        "the parser-produced GenericEffect must register a controller-scoped transient extra land drop; TCEs: {:?}",
        outcome.state().transient_continuous_effects
    );
    assert_eq!(
        additional_land_drops(outcome.state(), P1),
        0,
        "the transient extra land drop must not fan out to opponents"
    );
}
