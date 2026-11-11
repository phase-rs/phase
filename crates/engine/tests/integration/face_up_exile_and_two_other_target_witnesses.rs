//! Runtime witnesses for cards this PR's parser changes newly support: the
//! exact target count before "other target" (Ioreth of the Healing House,
//! Nimbleclaw Adept) and the "face-up exiled" target adjectives (Pull from
//! Eternity, Time Reaper). Printed Oracle text is quoted from the card data.
//!
//! Infrastructure exceptions: permanents are tapped and exiled cards are turned
//! face down by setting the object fields directly, isolating the target
//! declaration and resolution under test.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IORETH: &str =
    "{T}: Untap another target permanent.\n{T}: Untap two other target legendary creatures.";
const NIMBLECLAW_ADEPT: &str = "Bigby's Hand — {T}: Untap two other target permanents. Activate only as a sorcery and only once each turn.";
const PULL_FROM_ETERNITY: &str = "Put target face-up exiled card into its owner's graveyard.";
const TIME_REAPER: &str = "Flying, haste\nConsume Anomaly — Whenever this creature deals combat damage to a player, put target face-up card they own in exile on the bottom of their library. If you do, you gain 3 life.";

fn tap(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

fn face_down(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().face_down = true;
}

/// CR 115.1 + CR 602.2b + CR 115.10a: "Untap two other target legendary
/// creatures" declares exactly two targets, neither of them Ioreth; both
/// untap, and an untargeted tapped creature stays tapped.
#[test]
fn ioreth_untaps_two_other_target_legendary_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ioreth = scenario
        .add_creature_from_oracle(P0, "Ioreth of the Healing House", 1, 4, IORETH)
        .as_legendary()
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let first = scenario
        .add_creature(P0, "Legendary Bear", 2, 2)
        .as_legendary()
        .id();
    let second = scenario
        .add_creature(P1, "Legendary Elk", 2, 2)
        .as_legendary()
        .id();
    let bystander = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    for id in [first, second, bystander] {
        tap(&mut runner, id);
    }

    runner
        .activate(ioreth, 1)
        .target_objects(&[first, second])
        .resolve();
    assert!(tapped(&runner, ioreth), "{{T}} is paid");
    assert!(!tapped(&runner, first));
    assert!(!tapped(&runner, second));
    assert!(tapped(&runner, bystander), "only the two targets untap");
}

/// CR 115.1 + CR 602.2b + CR 602.5d: Nimbleclaw Adept's sorcery-speed ability
/// declares exactly two other target permanents and untaps both.
#[test]
fn nimbleclaw_adept_untaps_two_other_target_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let adept = scenario
        .add_creature_from_oracle(P0, "Nimbleclaw Adept", 2, 3, NIMBLECLAW_ADEPT)
        .with_subtypes(vec!["Dragon", "Wizard"])
        .id();
    let first = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let second = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let bystander = scenario.add_creature(P1, "Elvish Mystic", 1, 1).id();
    let mut runner = scenario.build();
    for id in [first, second, bystander] {
        tap(&mut runner, id);
    }

    runner
        .activate(adept, 0)
        .target_objects(&[first, second])
        .resolve();
    assert!(tapped(&runner, adept), "{{T}} is paid");
    assert!(!tapped(&runner, first));
    assert!(!tapped(&runner, second));
    assert!(tapped(&runner, bystander), "only the two targets untap");
}

/// CR 406.3 + CR 115.1: Pull from Eternity may target only a face-up exiled
/// card; the chosen one goes to its owner's graveyard.
#[test]
fn pull_from_eternity_targets_only_face_up_exiled_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let face_up = scenario.add_creature_to_exile(P1, "Exiled Bear", 2, 2).id();
    let other_face_up = scenario
        .add_creature_to_exile(P0, "My Exiled Bear", 2, 2)
        .id();
    let hidden = scenario.add_creature_to_exile(P1, "Hidden Card", 2, 2).id();
    let battlefield = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let pull = scenario
        .add_spell_to_hand_from_oracle(P0, "Pull from Eternity", true, PULL_FROM_ETERNITY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    face_down(&mut runner, hidden);

    let card_id = runner.state().objects[&pull].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: pull,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Pull from Eternity is cast");
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("expected its target, got {:?}", runner.state().waiting_for);
    };
    let mut legal = target_slots[selection.current_slot].legal_targets.clone();
    legal.sort_by_key(|target| format!("{target:?}"));
    let mut expected = vec![TargetRef::Object(face_up), TargetRef::Object(other_face_up)];
    expected.sort_by_key(|target| format!("{target:?}"));
    assert_eq!(
        legal, expected,
        "exactly the face-up exiled cards: not face-down {hidden:?}, not the battlefield creature {battlefield:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(face_up)),
        })
        .expect("the face-up exiled card is chosen");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&face_up].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&face_up].owner, P1);
    assert_eq!(runner.state().objects[&hidden].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&other_face_up].zone, Zone::Exile);
}

/// CR 510.2 + CR 603.2 + CR 406.3: Time Reaper's combat damage trigger targets
/// a face-up card the damaged player owns in exile (not a face-down one, not
/// one its controller owns), puts it on the bottom of that library, and its
/// controller gains 3 life.
#[test]
fn time_reaper_returns_the_damaged_players_face_up_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 library card"]);
    let reaper = scenario
        .add_creature_from_oracle(P0, "Time Reaper", 4, 4, TIME_REAPER)
        .with_subtypes(vec!["Alien", "Horror"])
        .id();
    let theirs = scenario.add_creature_to_exile(P1, "Exiled Bear", 2, 2).id();
    let their_other = scenario.add_creature_to_exile(P1, "Exiled Elk", 2, 2).id();
    let hidden = scenario.add_creature_to_exile(P1, "Hidden Card", 2, 2).id();
    let mine = scenario
        .add_creature_to_exile(P0, "My Exiled Bear", 2, 2)
        .id();
    let mut runner = scenario.build();
    face_down(&mut runner, hidden);
    let life_before = runner.life(P0);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(reaper, AttackTarget::Player(P1))])
        .expect("Time Reaper attacks");
    runner.combat_damage();
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Consume Anomaly's target, got {:?}",
            runner.state().waiting_for
        );
    };
    let mut legal = target_slots[selection.current_slot].legal_targets.clone();
    legal.sort_by_key(|target| format!("{target:?}"));
    let mut expected = vec![TargetRef::Object(theirs), TargetRef::Object(their_other)];
    expected.sort_by_key(|target| format!("{target:?}"));
    assert_eq!(
        legal, expected,
        "the damaged player's face-up exiled cards only: not face-down {hidden:?}, not P0's {mine:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(theirs)),
        })
        .expect("the damaged player's face-up exiled card is chosen");
    runner.advance_until_stack_empty();
    let library: Vec<ObjectId> = runner.state().players[1].library.iter().copied().collect();
    assert_eq!(
        library.last(),
        Some(&theirs),
        "on the bottom of its owner's library"
    );
    assert_eq!(runner.state().objects[&hidden].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&mine].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&their_other].zone, Zone::Exile);
    assert_eq!(runner.life(P0), life_before + 3);
}
