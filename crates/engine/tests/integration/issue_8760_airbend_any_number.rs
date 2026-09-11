//! Issue #8760 — Appa, Steadfast Guardian airbends any number of other
//! target nonland permanents you control (including zero).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, FilterProp, MultiTargetSpec, TargetFilter, TypeFilter,
};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const APPA_ORACLE: &str = "Flash\nFlying\nWhen Appa enters, airbend any number of other target nonland permanents you control. (Exile them. While each one is exiled, its owner may cast it for {2} rather than its mana cost.)\nWhenever you cast a spell from exile, create a 1/1 white Ally creature token.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut engine::game::scenario::GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn ability_contains_unimplemented(definition: &AbilityDefinition) -> bool {
    matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })
        || definition
            .sub_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
        || definition
            .else_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
}

fn appa_board() -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let mine_a = scenario.add_creature(P0, "Grizzly Bears A", 2, 2).id();
    let mine_b = scenario.add_creature(P0, "Grizzly Bears B", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let appa = scenario
        .add_creature_to_hand_from_oracle(P0, "Appa, Steadfast Guardian", 3, 4, APPA_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::White));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, land, mine_a, mine_b, theirs, appa)
}

/// SHAPE: Appa's ETB airbend is `MultiTargetSpec::unlimited(0)`, not
/// `ChangeZone.up_to`. Both triggers lower with zero Unimplemented.
#[test]
fn appa_etb_airbend_any_number_shape() {
    let parsed = parse_oracle_text(
        APPA_ORACLE,
        "Appa, Steadfast Guardian",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "Appa must parse ETB + spell-cast triggers, got {:?}",
        parsed.triggers
    );

    let etb = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::ChangesZone)
        .expect("Appa must parse an ETB trigger");
    let execute = etb
        .execute
        .as_deref()
        .expect("ETB trigger must carry an executed ability");
    assert_eq!(
        execute.multi_target,
        Some(MultiTargetSpec::unlimited(0)),
        "ETB airbend optionality lives on MultiTargetSpec"
    );
    match execute.effect.as_ref() {
        Effect::ChangeZone {
            origin,
            target: TargetFilter::Typed(tf),
            up_to,
            ..
        } => {
            assert_eq!(origin, &None);
            assert!(
                !*up_to,
                "optionality must not be encoded as ChangeZone.up_to"
            );
            assert!(
                tf.type_filters.contains(&TypeFilter::Permanent),
                "expected Permanent, got {:?}",
                tf.type_filters
            );
            assert!(
                tf.type_filters
                    .iter()
                    .any(|ty| matches!(ty, TypeFilter::Non(inner) if **inner == TypeFilter::Land)),
                "expected Non(Land), got {:?}",
                tf.type_filters
            );
            assert_eq!(tf.controller, Some(ControllerRef::You));
            assert!(
                tf.properties.contains(&FilterProp::Another),
                "expected Another, got {:?}",
                tf.properties
            );
        }
        other => panic!("expected ChangeZone with typed nonland permanent, got {other:?}"),
    }

    let spell_cast = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::SpellCast)
        .expect("Appa must parse a SpellCast trigger");
    let spell_execute = spell_cast
        .execute
        .as_deref()
        .expect("SpellCast trigger must carry an executed ability");
    assert!(
        matches!(spell_execute.effect.as_ref(), Effect::Token { .. }),
        "second trigger must still be SpellCast→Token, got {:?}",
        spell_execute.effect
    );

    for trigger in &parsed.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            assert!(
                !ability_contains_unimplemented(execute),
                "both triggers must lower with zero Unimplemented, got {execute:#?}"
            );
        }
    }
}

/// CR 107.1c + CR 115.6 + CR 603.3d: choosing zero targets is legal; the ETB
/// resolves (does not fizzle) and Appa remains on the battlefield.
#[test]
fn appa_etb_airbend_zero_selected() {
    let (mut runner, land, mine_a, mine_b, theirs, appa) = appa_board();

    runner.cast(appa).resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "zero-target airbend must return to priority, got {:?}",
        runner.state().waiting_for
    );
    for (id, name) in [
        (mine_a, "mine_a"),
        (mine_b, "mine_b"),
        (land, "land"),
        (theirs, "theirs"),
        (appa, "appa"),
    ] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Battlefield,
            "{name} must remain on the battlefield when zero targets are chosen"
        );
    }
}

/// CR 701.65a: airbending chosen permanents exiles them; unchosen objects stay.
#[test]
fn appa_etb_airbend_multiple_selected() {
    let (mut runner, land, mine_a, mine_b, theirs, appa) = appa_board();

    runner
        .cast(appa)
        .target_objects(&[mine_a, mine_b])
        .resolve();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "multi-target airbend must return to priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&mine_a].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&mine_b].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&appa].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Battlefield);
}
