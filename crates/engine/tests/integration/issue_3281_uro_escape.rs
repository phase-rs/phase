//! Regression for #3281 — Uro, Titan of Nature's Wrath escape from graveyard.
//!
//! https://github.com/phase-rs/phase/issues/3281

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::game_state::{CastPaymentMode, CastingVariant, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::{ExileCostSourceZone, Zone};
use engine::types::PtValue;

fn mana(color: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| {
            ManaUnit::new(
                color,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            )
        })
        .collect()
}

fn exported_uro_face() -> CardFace {
    let escape_kw: Keyword = serde_json::from_value(serde_json::json!({
        "Escape": {
            "type": "NonMana",
            "data": {
                "type": "Composite",
                "costs": [
                    {
                        "type": "Mana",
                        "cost": {
                            "type": "Cost",
                            "shards": ["Green", "Green", "Blue", "Blue"],
                            "generic": 0
                        }
                    },
                    {
                        "type": "Exile",
                        "count": 5,
                        "zone": "Graveyard",
                        "filter": {
                            "type": "Typed",
                            "type_filters": ["Card"],
                            "controller": "You",
                            "properties": [
                                {"type": "Another"},
                                {"type": "InZone", "zone": "Graveyard"}
                            ]
                        }
                    }
                ]
            }
        }
    }))
    .expect("card-data export escape keyword shape");

    CardFace {
        name: "Uro, Titan of Nature's Wrath".to_string(),
        mana_cost: ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        },
        card_type: CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Elder".to_string(), "Giant".to_string()],
        },
        power: Some(PtValue::Fixed(6)),
        toughness: Some(PtValue::Fixed(6)),
        keywords: vec![escape_kw],
        ..CardFace::default()
    }
}

fn add_exported_uro_to_graveyard(runner: &mut GameRunner) -> ObjectId {
    let id = create_object_from_card_face(runner.state_mut(), &exported_uro_face(), P0);
    remove_from_zone(runner.state_mut(), id, Zone::Library, P0);
    add_to_zone(runner.state_mut(), id, Zone::Graveyard, P0);
    runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
    id
}

#[test]
fn uro_escape_castable_from_graveyard_with_five_other_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for idx in 0..5 {
        scenario.add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1);
    }

    let mut runner = scenario.build();
    let uro_id = add_exported_uro_to_graveyard(&mut runner);
    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&uro_id),
        "Uro should be castable via escape with 5 other graveyard cards; castable={castable:?}"
    );
}

#[test]
fn uro_escape_not_castable_with_only_four_other_graveyard_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for idx in 0..4 {
        scenario.add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1);
    }

    let mut runner = scenario.build();
    let uro_id = add_exported_uro_to_graveyard(&mut runner);
    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        !castable.contains(&uro_id),
        "Uro must not be castable via escape with only four other graveyard cards"
    );
}

#[test]
fn uro_escape_full_cast_pauses_for_graveyard_exile_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(ManaType::Green, 2)
            .into_iter()
            .chain(mana(ManaType::Blue, 2))
            .collect(),
    );

    let filler: Vec<_> = (0..5)
        .map(|idx| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1)
                .id()
        })
        .collect();

    let mut runner = scenario.build();
    let uro_id = add_exported_uro_to_graveyard(&mut runner);
    let card_id = runner.state().objects[&uro_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: uro_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Uro escape cast should enter the pipeline");

    let WaitingFor::PayCost {
        kind:
            PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
        count,
        choices,
        ..
    } = result.waiting_for
    else {
        panic!(
            "Uro escape must pause to exile five other graveyard cards, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(count, 5);
    assert!(!choices.contains(&uro_id));
    assert!(filler.iter().all(|id| choices.contains(id)));

    let stack_variant = runner
        .state()
        .stack
        .get(0)
        .and_then(|entry| match &entry.kind {
            engine::types::game_state::StackEntryKind::Spell {
                casting_variant, ..
            } => Some(*casting_variant),
            _ => None,
        });
    assert_eq!(stack_variant, Some(CastingVariant::Escape));
}
