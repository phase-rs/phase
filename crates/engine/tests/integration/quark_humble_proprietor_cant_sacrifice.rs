//! Quark, Humble Proprietor (TRK):
//!
//! "You can't sacrifice Treasure tokens.
//!  At the beginning of each upkeep, create a Treasure token."
//!
//! CR 701.21a + CR 101.2: a player-scoped "can't sacrifice <filter>" static
//! stops the sacrifice whether it is a cost (CR 118.3) or an effect, and only
//! for permanents matching the filter and players inside the scope.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const QUARK: &str = "You can't sacrifice Treasure tokens.\nAt the beginning of each upkeep, create a Treasure token.";
const TREASURE: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";

fn contains_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

fn board(token: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let quark = scenario
        .add_creature_from_oracle(P0, "Quark, Humble Proprietor", 2, 3, QUARK)
        .id();
    let treasure = {
        let mut b = scenario.add_artifact_from_oracle(P0, "Treasure", TREASURE);
        b.with_subtypes(vec!["Treasure"]);
        b.id()
    };
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&treasure)
        .expect("treasure")
        .is_token = token;
    // Reach guard: Quark parsed to a real prohibition, with no residue.
    let obj = &runner.state().objects[&quark];
    assert!(
        obj.static_definitions
            .iter_all()
            .any(|d| matches!(d.mode, StaticMode::CantSacrifice { .. })),
        "Quark must parse a CantSacrifice static, got {:?}",
        obj.static_definitions
    );
    assert!(!obj.abilities.iter().any(contains_unimplemented));
    (runner, quark, treasure)
}

fn on_battlefield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().battlefield.contains(&id)
}

fn mana_available(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

fn activate(
    runner: &mut GameRunner,
    source: ObjectId,
) -> Result<(), engine::game::engine::EngineError> {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|_| ())
}

#[test]
fn treasure_token_cannot_be_sacrificed_for_mana() {
    let (mut runner, _quark, treasure) = board(true);
    assert!(
        activate(&mut runner, treasure).is_err(),
        "sacrificing a Treasure token as a cost is prohibited while Quark is out"
    );
    assert!(on_battlefield(&runner, treasure));
    assert_eq!(mana_available(&runner), 0);
}

#[test]
fn nontoken_treasure_can_still_be_sacrificed() {
    let (mut runner, _quark, treasure) = board(false);
    activate(&mut runner, treasure)
        .expect("a nontoken Treasure is outside the 'Treasure tokens' filter");
    assert!(!on_battlefield(&runner, treasure), "it was sacrificed");
}
