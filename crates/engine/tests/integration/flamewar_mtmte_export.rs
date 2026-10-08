//! Production-export regression for Flamewar's More Than Meets the Eye cast.

use crate::support::{shared_card_db, shared_card_export_json};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::AlternativeCastDecision;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Export census for the whole Pack Tactics grammar class, not one named card.
#[test]
fn production_export_has_canonical_pack_tactics_conditions_for_all_eight_cards() {
    let Some(export) = shared_card_export_json() else {
        return;
    };
    let pack_tactics = export
        .values()
        .filter(|face| {
            face.get("oracle_text")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| {
                    text.contains("you attacked with creatures with total power")
                        && text.contains("or greater this combat")
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(pack_tactics.len(), 8, "Pack Tactics export census changed");
    for face in pack_tactics {
        assert!(
            face.get("triggers")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|triggers| triggers.iter().any(|trigger| {
                    trigger
                        .get("condition")
                        .is_some_and(|condition| !condition.is_null())
                })),
            "{} must export a canonical trigger condition",
            face.get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        );
    }
}

/// CR 702.162a + CR 601.2b: the checked-in export hydrates Flamewar's real
/// transform pair and alternative cost; choosing it casts the converted spell
/// and it enters on the Streetwise Operative face.
#[test]
fn flamewar_mtmte_from_production_export_casts_back_face() {
    let db = shared_card_db().expect("integration card fixture must load");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flamewar = scenario.add_real_card(P0, "Flamewar, Brash Veteran", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let hand_object = runner
        .state()
        .objects
        .get(&flamewar)
        .expect("Flamewar in hand");
    assert_eq!(
        hand_object
            .back_face
            .as_ref()
            .map(|face| face.name.as_str()),
        Some("Flamewar, Streetwise Operative"),
        "real export must hydrate Flamewar's transform back face"
    );
    runner
        .cast(flamewar)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();

    let permanent = runner
        .state()
        .objects
        .get(&flamewar)
        .expect("Flamewar remains represented after resolution");
    assert!(permanent.transformed, "MTMTE cast enters transformed");
    assert_eq!(permanent.name, "Flamewar, Streetwise Operative");
}

const WEREWOLF_PACK_LEADER_ORACLE: &str =
    "Pack tactics — Whenever this creature attacks, if you attacked with creatures with total power 6 or greater this combat, draw a card.\n{3}{G}: Until end of turn, this creature has base power and toughness 5/3, gains trample, and isn't a Human.";

const HOBGOBLIN_CAPTAIN_ORACLE: &str =
    "Pack tactics — Whenever this creature attacks, if you attacked with creatures with total power 6 or greater this combat, this creature gains first strike until end of turn.";

/// CR 603.4 + CR 508.1a: real Pack Tactics Oracle text only fires when the
/// declaration snapshot's combined power reaches six.
#[test]
fn pack_tactics_uses_the_declared_attack_batch() {
    for (name, source_power, oracle) in [
        ("Werewolf Pack Leader", 3, WEREWOLF_PACK_LEADER_ORACLE),
        ("Hobgoblin Captain", 3, HOBGOBLIN_CAPTAIN_ORACLE),
    ] {
        for (other_power, should_trigger) in [(2, false), (3, true)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_creature_from_oracle(P0, name, source_power, 3, oracle)
                .id();
            let other = scenario
                .add_creature(P0, "Pack Tactics witness", other_power, 1)
                .id();
            let mut runner = scenario.build();
            runner.advance_to_combat();

            runner
                .declare_attackers(&[
                    (source, AttackTarget::Player(P1)),
                    (other, AttackTarget::Player(P1)),
                ])
                .expect("attack declaration accepted");
            assert_eq!(
                !runner.state().stack.is_empty(),
                should_trigger,
                "{name}: {source_power} + {other_power} declaration must {} trigger Pack Tactics",
                if should_trigger { "" } else { "not" }
            );
        }
    }
}

/// CR 603.4 + CR 508.1a: the resolution-time intervening-if recheck reads the
/// original declaration records after a co-attacker changes characteristics and
/// leaves the battlefield.
#[test]
fn pack_tactics_rechecks_declaration_snapshot_after_departure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Pack Tactics Draw"]);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Werewolf Pack Leader",
            3,
            3,
            WEREWOLF_PACK_LEADER_ORACLE,
        )
        .id();
    let other = scenario
        .add_creature(P0, "Departing Pack Tactics witness", 3, 1)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();

    runner
        .declare_attackers(&[
            (source, AttackTarget::Player(P1)),
            (other, AttackTarget::Player(P1)),
        ])
        .expect("six-power attack declaration accepted");
    assert!(
        !runner.state().stack.is_empty(),
        "Pack Tactics trigger queued"
    );
    runner.state_mut().objects.get_mut(&other).unwrap().power = Some(0);
    engine::game::zones::move_to_zone(runner.state_mut(), other, Zone::Graveyard, &mut Vec::new());
    runner.advance_until_stack_empty();

    assert!(
        runner.state().players[P0.0 as usize].hand.iter().any(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|object| object.name == "Pack Tactics Draw")
        }),
        "the snapshot-qualified trigger must draw after its co-attacker departs"
    );
}
