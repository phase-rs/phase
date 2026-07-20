//! Stolen Goodies may be cast with no targets: its three +1/+1 counters are
//! simply not distributed. A fixed positive pool must not make "any number"
//! require a target or leave the cast stuck at an empty distribution prompt.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{CastingPermission, MultiTargetSpec};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const STOLEN_GOODIES: &str =
    "Distribute three +1/+1 counters among any number of target creatures you control.";

#[test]
fn stolen_goodies_can_be_cast_with_no_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let parsed_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Stolen Goodies", false, STOLEN_GOODIES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        })
        .id();
    let picnic = scenario
        .add_creature_to_hand(P0, "Picnic Ruiner", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();

    assert_eq!(
        runner.state().objects[&parsed_spell].abilities[0].multi_target,
        Some(MultiTargetSpec::fixed(0, 3)),
        "'any number' permits zero targets and cannot exceed the three-counter pool"
    );

    let adventure_abilities = runner.state().objects[&parsed_spell]
        .abilities
        .as_ref()
        .clone();
    runner
        .state_mut()
        .objects
        .get_mut(&picnic)
        .unwrap()
        .back_face = Some(BackFaceData {
        name: "Stolen Goodies".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        card_types: CardType {
            core_types: vec![CoreType::Sorcery],
            subtypes: vec!["Adventure".to_string()],
            ..CardType::default()
        },
        mana_cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 3,
        },
        keywords: vec![],
        abilities: adventure_abilities,
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Green],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    });

    let pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for _ in 0..4 {
        pool.add(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    }
    let card_id = runner.state().objects[&picnic].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: picnic,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Picnic Ruiner must offer its Adventure face");
    assert!(matches!(
        result.waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Adventure { .. },
            ..
        }
    ));

    let result = runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("Stolen Goodies must be castable without a creature target");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "an empty target set has nothing to distribute, got {:?}",
        result.waiting_for
    );
    runner.resolve_top();
    let picnic = &runner.state().objects[&picnic];
    assert_eq!(picnic.zone, Zone::Exile);
    assert!(
        picnic
            .casting_permissions
            .contains(&CastingPermission::AdventureCreature),
        "a targetless Adventure still permits the creature face to be cast later"
    );
}
