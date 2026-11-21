//! Issue #3669, "target opponents each A and B": Will of the Abzan gathers
//! sacrifice choices in APNAP order before moving any creatures (CR 608.2e,
//! CR 101.4), then applies the life loss to each targeted opponent (CR 119.3).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const WILL: &str = "Choose one. If you control a commander as you cast this spell, you may choose both instead.\n• Any number of target opponents each sacrifice a creature with the greatest power among creatures that player controls and lose 3 life.\n• Return target creature card from your graveyard to the battlefield.";

fn add_will(scenario: &mut GameScenario) -> ObjectId {
    scenario.at_phase(Phase::PreCombatMain).with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]); 4],
    );
    scenario
        .add_spell_to_hand_from_oracle(P0, "Will of the Abzan", false, WILL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 3,
        })
        .id()
}

fn announce(runner: &mut GameRunner, spell: ObjectId, modes: Vec<usize>, targets: Vec<TargetRef>) {
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Will of the Abzan");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::SelectModes { indices: modes })
        .expect("choose Will of the Abzan's modes");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { player: P0, .. }
        ),
        "expected target announcement, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets { targets })
        .expect("announce Will of the Abzan's targets");
    assert_eq!(runner.state().stack.len(), 1);
}

fn resolve(runner: &mut GameRunner) {
    for _ in 0..runner.state().players.len() {
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
}

fn sacrifice(runner: &mut GameRunner, chooser: PlayerId, pool: &[ObjectId], chosen: ObjectId) {
    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count: 1,
        effect_kind: EffectKind::Sacrifice,
        is_cost_payment: false,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected sacrifice prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, chooser);
    assert_eq!(cards.len(), pool.len());
    assert!(pool.iter().all(|id| cards.contains(id)), "{cards:?}");
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("select creature to sacrifice");
}

fn assert_battlefield(runner: &GameRunner, creatures: &[ObjectId]) {
    for id in creatures {
        assert_eq!(runner.state().objects[id].zone, Zone::Battlefield);
    }
}

fn assert_graveyard(runner: &GameRunner, player: PlayerId, expected: &[&str]) {
    let state = runner.state();
    let names: Vec<_> = state.players[player.0 as usize]
        .graveyard
        .iter()
        .map(|id| state.objects[id].name.as_str())
        .collect();
    assert_eq!(names, expected, "graveyard of {player:?}");
}

fn assert_complete(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn will_of_the_abzan_each_opponent_sacrifices_their_own_greatest_power() {
    let mut scenario = GameScenario::new_n_player(3, 1);
    let spell = add_will(&mut scenario);
    let caster = scenario.add_creature(P0, "Colossapede", 5, 5).id();
    let lesser = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let greatest = scenario.add_creature(P1, "Durkwood Boars", 4, 4).id();
    let tied_a = scenario.add_creature(P2, "Hill Giant", 3, 3).id();
    let tied_b = scenario.add_creature(P2, "Kalonian Tusker", 3, 3).id();
    let smaller = scenario
        .add_creature(P2, "Mons's Goblin Raiders", 1, 1)
        .id();
    let creatures = [caster, lesser, greatest, tied_a, tied_b, smaller];
    let mut runner = scenario.build();

    announce(
        &mut runner,
        spell,
        vec![0],
        vec![TargetRef::Player(P1), TargetRef::Player(P2)],
    );
    resolve(&mut runner);

    // CR 608.2e + CR 101.4: Both opponents' choices precede the simultaneous sacrifices.
    assert_battlefield(&runner, &creatures);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { player: P1, .. }
    ) {
        sacrifice(&mut runner, P1, &[greatest], greatest);
        assert_battlefield(&runner, &creatures);
    }
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
    sacrifice(&mut runner, P2, &[tied_a, tied_b], tied_b);

    // CR 701.21a: Each sacrificed creature moves to its owner's graveyard.
    assert_graveyard(&runner, P0, &["Will of the Abzan"]);
    assert_graveyard(&runner, P1, &["Durkwood Boars"]);
    assert_graveyard(&runner, P2, &["Kalonian Tusker"]);
    assert_battlefield(&runner, &[caster, lesser, tied_a, smaller]);
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 17);
    assert_eq!(runner.life(P2), 17);
    assert_complete(&runner);
}

#[test]
fn will_of_the_abzan_commander_enables_both_modes() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 1);
    let spell = add_will(&mut scenario);
    let commander = scenario
        .add_creature(P0, "Yargle, Glutton of Urborg", 9, 3)
        .with_color(vec![ManaColor::Black])
        .as_legendary()
        .commander()
        .id();
    let reanimate = scenario
        .add_creature_to_graveyard(P0, "Walking Corpse", 2, 2)
        .with_color(vec![ManaColor::Black])
        .id();
    let greatest = scenario.add_creature(P1, "Durkwood Boars", 4, 4).id();
    let smaller = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let reanimate_card = runner.state().objects[&reanimate].card_id;

    // CR 700.2a + CR 700.2c: Both chosen modes announce their own targets.
    announce(
        &mut runner,
        spell,
        vec![0, 1],
        vec![TargetRef::Player(P1), TargetRef::Object(reanimate)],
    );
    resolve(&mut runner);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ) {
        sacrifice(&mut runner, P1, &[greatest], greatest);
    }

    assert_graveyard(&runner, P0, &["Will of the Abzan"]);
    assert_graveyard(&runner, P1, &["Durkwood Boars"]);
    assert_battlefield(&runner, &[commander, smaller]);
    // CR 110.2a: The returned card enters under the caster's control.
    assert!(runner.state().battlefield.iter().any(|id| {
        let object = &runner.state().objects[id];
        object.card_id == reanimate_card && object.controller == P0
    }));
    assert_eq!(runner.life(P0), 40);
    assert_eq!(runner.life(P1), 37);
    assert_complete(&runner);
}

#[test]
fn will_of_the_abzan_allows_zero_opponent_targets() {
    let mut scenario = GameScenario::new_n_player(3, 1);
    let spell = add_will(&mut scenario);
    let creatures = [
        scenario.add_creature(P0, "Colossapede", 5, 5).id(),
        scenario.add_creature(P1, "Durkwood Boars", 4, 4).id(),
        scenario.add_creature(P2, "Hill Giant", 3, 3).id(),
    ];
    let mut runner = scenario.build();

    // CR 115.6: The target-selection window accepts an empty announcement.
    announce(&mut runner, spell, vec![0], vec![]);
    resolve(&mut runner);

    assert_battlefield(&runner, &creatures);
    assert_graveyard(&runner, P0, &["Will of the Abzan"]);
    for player in [P1, P2] {
        assert_graveyard(&runner, player, &[]);
    }
    for player in [P0, P1, P2] {
        assert_eq!(runner.life(player), 20);
    }
    assert_complete(&runner);
}
