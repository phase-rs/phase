//! Regression test for issue #433 — Dalkovan Encampment's delayed
//! "Whenever you attack this turn" trigger does nothing.
//!
//! Dalkovan Encampment's activated ability is:
//!   `{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red Warrior
//!    creature tokens that are tapped and attacking. ...`
//!
//! The inner clause is a delayed triggered ability (CR 603.7c) whose condition
//! is prefix-stripped to the bare string `"you attack"`. Before the #433 fix the
//! trigger parser only recognized the prefixed forms `"whenever you attack"` /
//! `"when you attack"`, so the bare condition fell through to
//! `TriggerMode::Unknown` and the delayed trigger never fired — the Warrior
//! tokens were never created.
//!
//! This test drives the full pipeline through `apply`: activate the ability,
//! declare an attacker, and assert two tapped+attacking Warrior tokens appear.
//! There is no synthetic `process_triggers` call.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

/// CR 508.1 + CR 603.7c: activating Dalkovan Encampment's ability creates a
/// "Whenever you attack this turn" delayed trigger; declaring an attacker fires
/// it and creates two 1/1 red Warrior tokens that are tapped and attacking.
#[test]
fn dalkovan_encampment_delayed_trigger_creates_warrior_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Dalkovan Encampment's third ability only. `add_creature_from_oracle`
    // parses the activated ability from Oracle text; card type is irrelevant
    // to activated-ability resolution.
    let encampment = scenario
        .add_creature_from_oracle(
            P0,
            "Dalkovan Encampment",
            0,
            1,
            "{2}{W}, {T}: Whenever you attack this turn, create two 1/1 red \
             Warrior creature tokens that are tapped and attacking. Sacrifice \
             them at the beginning of the next end step.",
        )
        .id();

    // A separate creature to attack with.
    let attacker = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    // Pay {2}{W} from the pool so activation does not require land taps.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    // Locate the activated ability index whose effect is the delayed trigger.
    let ability_index = runner
        .state()
        .objects
        .get(&encampment)
        .expect("encampment exists")
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::CreateDelayedTrigger { .. }))
        .expect("Dalkovan Encampment must have a CreateDelayedTrigger ability");

    // Activate the {2}{W},{T} ability through the activation pipeline, paying the
    // mana cost from the funded pool; resolution creates the "Whenever you attack
    // this turn" delayed trigger (CR 603.7c).
    runner.activate(encampment, ability_index).resolve();

    // Declare the attacker — this is the event the delayed trigger watches.
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    // Two 1/1 red Warrior tokens, tapped and attacking, must now exist on P0's
    // battlefield.
    let warriors: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P0
                && o.zone == Zone::Battlefield
                && o.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("warrior"))
        })
        .map(|o| o.id)
        .collect();

    assert_eq!(
        warriors.len(),
        2,
        "the delayed 'you attack' trigger must create exactly two Warrior tokens"
    );

    for &w in &warriors {
        let obj = runner.state().objects.get(&w).expect("token exists");
        assert!(obj.tapped, "Warrior tokens must enter tapped (CR 508.4)");
    }

    // CR 508.4: the tokens enter attacking — they must appear in the combat
    // attacker list (not declared as attackers, but attacking creatures).
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat state present during DeclareAttackers");
    for &w in &warriors {
        let info = combat
            .attackers
            .iter()
            .find(|a| a.object_id == w)
            .expect("Warrior token must be an attacking creature");
        assert_eq!(
            info.defending_player, P1,
            "Warrior token must attack the opponent declared this combat, not its controller"
        );
    }
}
