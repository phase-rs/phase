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

const SOL_RING: &str = "{T}: Add {C}{C}.";
const ANGRATHS_RAMPAGE: &str = "Choose one —\n• Target player sacrifices an artifact of their choice.\n• Target player sacrifices a creature of their choice.\n• Target player sacrifices a planeswalker of their choice.";

/// Quark plus a Treasure token and (optionally) a nontoken Sol Ring; P0 then
/// casts Angrath's Rampage's artifact mode targeting themselves.
fn rampage_on_self(with_sol_ring: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Quark, Humble Proprietor", 2, 3, QUARK);
    let treasure = {
        let mut b = scenario.add_artifact_from_oracle(P0, "Treasure", TREASURE);
        b.with_subtypes(vec!["Treasure"]);
        b.id()
    };
    let sol_ring = with_sol_ring.then(|| {
        scenario
            .add_artifact_from_oracle(P0, "Sol Ring", SOL_RING)
            .id()
    });
    let rampage = scenario
        .add_spell_to_hand_from_oracle(P0, "Angrath's Rampage", false, ANGRATHS_RAMPAGE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&treasure)
        .expect("treasure")
        .is_token = true;
    runner.cast(rampage).modes(&[0]).target_player(P0).resolve();
    (runner, treasure, sol_ring)
}

/// CR 701.21a + CR 609.3: "sacrifice an artifact" offers only artifacts the
/// player can sacrifice. The Treasure token is not a legal choice, so the Sol
/// Ring is the one sacrificed and no prompt can pick the Treasure.
#[test]
fn sacrifice_an_artifact_effect_skips_treasure_token() {
    let (runner, treasure, sol_ring) = rampage_on_self(true);
    let sol_ring = sol_ring.expect("sol ring");
    assert!(
        !matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::EffectZoneChoice { .. }
        ),
        "only one artifact is a legal choice, so there is nothing to choose between"
    );
    assert!(!on_battlefield(&runner, sol_ring), "Sol Ring is sacrificed");
    assert!(
        on_battlefield(&runner, treasure),
        "the Treasure token is kept"
    );
}

/// CR 609.3: with only a Treasure token, the effect does as much as possible —
/// nothing — rather than "sacrificing" the token.
#[test]
fn sacrifice_an_artifact_effect_with_only_treasure_does_nothing() {
    let (runner, treasure, _) = rampage_on_self(false);
    assert!(on_battlefield(&runner, treasure));
}
